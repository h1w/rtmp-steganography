pub mod app;
pub mod ffmpeg_publish;
pub mod ffmpeg_read;
pub mod vk_live;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{self, PeerConfig};
use crate::flicker::fragment::{Fragment, Reassembler, FRAGMENT_HEADER_BYTES};
use crate::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use crate::flicker::grid::FRAME_BYTES_RGB24;
use crate::flicker::{InboundMessage, OutboundMessage};

#[derive(Copy, Clone, Debug)]
pub struct Direction {
    pub tx: bool,
    pub rx: bool,
}

pub fn run_peer(cfg: PeerConfig, dir: Direction) -> Result<()> {
    if dir.tx { config::validate_tx(&cfg)?; }
    if dir.rx { config::validate_rx(&cfg)?; }

    let running = Arc::new(AtomicBool::new(true));
    let running_signal = Arc::clone(&running);
    ctrlc::set_handler(move || {
        running_signal.store(false, Ordering::SeqCst);
    }).context("ctrlc handler")?;

    let (app_out_tx, app_out_rx) = mpsc::channel::<OutboundMessage>();
    let (app_in_tx, app_in_rx) = mpsc::channel::<InboundMessage>();

    let mut handles = Vec::new();
    if dir.tx {
        let cfg_c = cfg.clone();
        let run_c = Arc::clone(&running);
        handles.push(thread::spawn(move || {
            if let Err(e) = tx_thread(cfg_c, app_out_rx, run_c) {
                eprintln!("[peer/tx] error: {e}");
            }
        }));
    } else {
        drop(app_out_rx);
    }

    if dir.rx {
        let cfg_c = cfg.clone();
        let run_c = Arc::clone(&running);
        handles.push(thread::spawn(move || {
            if let Err(e) = rx_thread(cfg_c, app_in_tx, run_c) {
                eprintln!("[peer/rx] error: {e}");
            }
        }));
    } else {
        drop(app_in_tx);
    }

    // App thread runs in main.
    let out_tx = if dir.tx { Some(app_out_tx) } else { None };
    let in_rx = if dir.rx { Some(app_in_rx) } else { None };
    app::run_default(out_tx, in_rx, Arc::clone(&running));

    for h in handles { let _ = h.join(); }
    Ok(())
}

fn tx_thread(cfg: PeerConfig, outbound: Receiver<OutboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let rtmp_url = format!("{}/{}", cfg.my_rtmp_url.trim_end_matches('/'), cfg.my_stream_key);
    let args = ffmpeg_publish::publish_args(&rtmp_url);
    let mut child = std::process::Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg publish")?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;

    let mut encoder = FrameEncoder {
        mode: cfg.modulation_mode,
        channel_id: 1,
        frame_counter: 0,
    };
    let mut frame_buf = vec![0u8; FRAME_BYTES_RGB24];
    let frame_interval = Duration::from_nanos(1_000_000_000 / crate::flicker::grid::FPS as u64);
    let mut next_deadline = std::time::Instant::now();
    let mut next_msg_id: u32 = 0;

    while running.load(Ordering::SeqCst) {
        // Gather one message worth of fragments for this frame.
        let mut fragments: Vec<Fragment> = Vec::new();
        let capacity = encoder.payload_bytes_per_frame();
        let max_payload = capacity.saturating_sub(FRAGMENT_HEADER_BYTES);
        match outbound.recv_timeout(Duration::from_millis(10)) {
            Ok(msg) => {
                if msg.payload.len() <= max_payload {
                    next_msg_id = next_msg_id.wrapping_add(1);
                    fragments.push(Fragment {
                        msg_type: msg.msg_type,
                        message_id: next_msg_id,
                        fragment_idx: 0,
                        fragment_total: 1,
                        payload: msg.payload,
                    });
                } else {
                    // Split across multiple frames — emit first fragment now.
                    let total = ((msg.payload.len() + max_payload - 1) / max_payload) as u16;
                    next_msg_id = next_msg_id.wrapping_add(1);
                    let chunk = msg.payload[..max_payload].to_vec();
                    fragments.push(Fragment {
                        msg_type: msg.msg_type,
                        message_id: next_msg_id,
                        fragment_idx: 0,
                        fragment_total: total,
                        payload: chunk,
                    });
                    // TODO: queue remaining fragments across upcoming frames (v2.1).
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        encoder.encode(&mut frame_buf, &fragments)?;
        if stdin.write_all(&frame_buf).is_err() { break; }
        // Pace to fps.
        next_deadline += frame_interval;
        let now = std::time::Instant::now();
        if next_deadline > now {
            thread::sleep(next_deadline - now);
        } else {
            next_deadline = now;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn rx_thread(cfg: PeerConfig, inbound: Sender<InboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let page_url = format!("https://live.vkvideo.ru/{}/stream/{}", cfg.their_vk_channel, cfg.their_stream_name);
    // Resolve VK stream URL.
    let (stream_url, is_hls) = vk_live::resolve(&cfg.their_vk_channel, &cfg.their_stream_name)
        .context("vk resolve")?;
    let args = ffmpeg_read::read_args(&stream_url, &page_url, is_hls);
    let mut child = ffmpeg_read::spawn(&args)?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no ffmpeg stdout"))?;

    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let dec = FrameDecoder;
    let mut reassembler = Reassembler::new(cfg.frag_timeout_ms);
    let mut frame_idx: u64 = 0;

    while running.load(Ordering::SeqCst) {
        if stdout.read_exact(&mut buf).is_err() {
            eprintln!("[flicker] rx ffmpeg stdout ended");
            break;
        }
        match dec.decode(&buf) {
            DecodeOutcome::Ok { header, fragments, pilot_success } => {
                if cfg.log_every_frame {
                    eprintln!("[flicker] rx frame={frame_idx} ch={} mode={:?} payload_len={} pilots={:.2}",
                        header.channel_id, header.modulation_mode, header.payload_len, pilot_success);
                }
                for f in fragments {
                    if let Some(msg) = reassembler.accept(f) {
                        if inbound.send(msg).is_err() { return Ok(()); }
                    }
                }
            }
            DecodeOutcome::Dropped { reason } => {
                if cfg.log_every_frame {
                    eprintln!("[flicker] rx frame={frame_idx} dropped: {reason:?}");
                }
            }
        }
        frame_idx += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}
