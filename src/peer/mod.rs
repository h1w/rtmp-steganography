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

use anyhow::{Context, Result};

use crate::config::{self, PeerConfig};
use crate::flicker::fragment::{Fragment, Reassembler, FRAGMENT_HEADER_BYTES};
use crate::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use crate::flicker::grid::FlickerParams;
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

pub fn run_peer_tunnel(
    cfg: PeerConfig,
    dir: Direction,
    socks_bind: std::net::SocketAddr,
    with_bench_support: bool,
) -> Result<()> {
    // Tunnel requires bidirectional channels — caller already enforced this,
    // but double-check here as a defensive guard.
    if !(dir.tx && dir.rx) {
        return Err(anyhow::anyhow!("run_peer_tunnel requires bidirectional peer"));
    }
    config::validate_tx(&cfg)?;
    config::validate_rx(&cfg)?;

    let running = Arc::new(AtomicBool::new(true));
    let running_signal = Arc::clone(&running);
    ctrlc::set_handler(move || {
        running_signal.store(false, Ordering::SeqCst);
    }).context("ctrlc handler")?;

    let (app_out_tx, app_out_rx) = mpsc::channel::<OutboundMessage>();
    let (app_in_tx, app_in_rx)   = mpsc::channel::<InboundMessage>();

    let mut handles = Vec::new();
    let cfg_tx = cfg.clone();
    let run_tx = Arc::clone(&running);
    handles.push(thread::spawn(move || {
        if let Err(e) = tx_thread(cfg_tx, app_out_rx, run_tx) {
            eprintln!("[peer/tx] error: {e}");
        }
    }));

    let cfg_rx = cfg.clone();
    let run_rx = Arc::clone(&running);
    handles.push(thread::spawn(move || {
        if let Err(e) = rx_thread(cfg_rx, app_in_tx, run_rx) {
            eprintln!("[peer/rx] error: {e}");
        }
    }));

    // App (tunnel) runs on the main thread.
    app::run_tunnel(app_out_tx, app_in_rx, Arc::clone(&running), socks_bind, with_bench_support);

    for h in handles { let _ = h.join(); }
    Ok(())
}

/// Default warm-up delay before the rx thread starts looking for the other
/// peer's stream. Both peers' publishes need a few seconds to register with
/// VK before HLS becomes available. Overridable via env `peer_rx_warmup_ms`.
pub const DEFAULT_RX_WARMUP_MS: u64 = 10_000;
const RETRY_INITIAL_MS: u64 = 2_000;
const RETRY_MAX_MS: u64 = 16_000;

fn sleep_backoff(prev_ms: u64) -> u64 {
    let next = (prev_ms.saturating_mul(2)).min(RETRY_MAX_MS);
    thread::sleep(Duration::from_millis(prev_ms));
    next
}

fn tx_thread(cfg: PeerConfig, outbound: Receiver<OutboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let rtmp_url = format!("{}/{}", cfg.my_rtmp_url.trim_end_matches('/'), cfg.my_stream_key);
    let params = FlickerParams::default_256x144_24();
    if let Err(e) = params.validate() { return Err(anyhow::anyhow!("flicker params: {e}")); }
    let fps = cfg.flicker_fps.max(1);
    let mut backoff_ms = RETRY_INITIAL_MS;
    let mut encoder = FrameEncoder {
        params,
        mode: cfg.modulation_mode,
        channel_id: 1,
        frame_counter: 0,
    };
    let mut frame_buf = vec![0u8; params.frame_bytes_rgb24()];
    let frame_interval = Duration::from_nanos(1_000_000_000 / fps as u64);
    let mut next_msg_id: u32 = 0;

    while running.load(Ordering::SeqCst) {
        let args = ffmpeg_publish::publish_args(&ffmpeg_publish::PublishOpts {
            rtmp_url: &rtmp_url,
            flicker_width: params.width,
            flicker_height: params.height,
            stream_width: cfg.stream_width,
            stream_height: cfg.stream_height,
            fps,
        });
        let spawn_result = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn();
        let mut child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[peer/tx] spawn failed: {e:#} — retrying in {}ms", backoff_ms);
                backoff_ms = sleep_backoff(backoff_ms);
                continue;
            }
        };
        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                eprintln!("[peer/tx] no stdin handle — killing and retrying");
                let _ = child.kill();
                let _ = child.wait();
                backoff_ms = sleep_backoff(backoff_ms);
                continue;
            }
        };

        // Successful spawn — reset backoff.
        backoff_ms = RETRY_INITIAL_MS;
        let mut next_deadline = std::time::Instant::now();

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
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
            }
            if let Err(e) = encoder.encode(&mut frame_buf, &fragments) {
                eprintln!("[peer/tx] encode error: {e:#} — restarting ffmpeg");
                break;
            }
            if stdin.write_all(&frame_buf).is_err() {
                eprintln!("[peer/tx] ffmpeg stdin closed — will respawn");
                break;
            }
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
        if running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(RETRY_INITIAL_MS));
        }
    }
    Ok(())
}

fn rx_thread(cfg: PeerConfig, inbound: Sender<InboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let page_url = format!("https://live.vkvideo.ru/{}/stream/{}", cfg.their_vk_channel, cfg.their_stream_name);
    let warmup_ms = cfg.rx_warmup_ms;
    let params = FlickerParams::default_256x144_24();
    let fps = cfg.flicker_fps.max(1);
    let mut backoff_ms = RETRY_INITIAL_MS;
    let mut buf = vec![0u8; params.frame_bytes_rgb24()];
    let dec = FrameDecoder { params };
    let mut reassembler = Reassembler::new(cfg.frag_timeout_ms);
    let mut frame_idx: u64 = 0;

    // Warm-up: give both peers time to register with VK before we try to
    // resolve the counterpart's stream. Without this, a cold start hits
    // a "no playerUrls in HTML" response since VK hasn't indexed the
    // publish endpoint yet.
    if warmup_ms > 0 {
        eprintln!("[peer/rx] warm-up {}ms before first resolve", warmup_ms);
        let total = Duration::from_millis(warmup_ms);
        let step = Duration::from_millis(200);
        let start = std::time::Instant::now();
        while running.load(Ordering::SeqCst) && start.elapsed() < total {
            thread::sleep(step);
        }
    }

    // Outer loop: re-resolve + respawn ffmpeg on any error. Needed because
    //   (a) peer startup races with the other peer's publish going live,
    //   (b) a fresh VK HLS playlist may be empty/unparseable for a second or two,
    //   (c) signed playback URLs can expire mid-session.
    while running.load(Ordering::SeqCst) {
        let (stream_url, is_hls) = match vk_live::resolve(&cfg.their_vk_channel, &cfg.their_stream_name) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[peer/rx] vk resolve failed ({e:#}) — retrying in {}ms", backoff_ms);
                backoff_ms = sleep_backoff(backoff_ms);
                continue;
            }
        };
        eprintln!("[peer/rx] stream resolved: is_hls={is_hls} url_head={}", &stream_url.chars().take(80).collect::<String>());
        let args = ffmpeg_read::read_args(&ffmpeg_read::ReadOpts {
            input_url: &stream_url,
            page_url: &page_url,
            input_is_hls: is_hls,
            flicker_width: params.width,
            flicker_height: params.height,
            fps,
        });
        let mut child = match ffmpeg_read::spawn(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[peer/rx] ffmpeg spawn failed: {e:#} — retrying in {}ms", backoff_ms);
                backoff_ms = sleep_backoff(backoff_ms);
                continue;
            }
        };
        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                eprintln!("[peer/rx] no ffmpeg stdout — killing and retrying");
                let _ = child.kill();
                let _ = child.wait();
                backoff_ms = sleep_backoff(backoff_ms);
                continue;
            }
        };

        // Got past spawn — reset backoff.
        backoff_ms = RETRY_INITIAL_MS;

        while running.load(Ordering::SeqCst) {
            if stdout.read_exact(&mut buf).is_err() {
                eprintln!("[peer/rx] ffmpeg stdout ended — will re-resolve");
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
                            if inbound.send(msg).is_err() {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Ok(());
                            }
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
        if running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(RETRY_INITIAL_MS));
        }
    }
    Ok(())
}
