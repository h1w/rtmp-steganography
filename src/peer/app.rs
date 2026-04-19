//! Default application running on top of flicker: heartbeats out, logs in.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::flicker::{InboundMessage, OutboundMessage};

pub const MSG_TYPE_TIME_SYNC: u8 = 0x01;

pub fn run_default(
    outbound_tx: Option<Sender<OutboundMessage>>,
    inbound_rx: Option<Receiver<InboundMessage>>,
    running: Arc<AtomicBool>,
) {
    if let Some(tx) = outbound_tx {
        let running_c = Arc::clone(&running);
        thread::spawn(move || heartbeat_loop(tx, running_c));
    }
    if let Some(rx) = inbound_rx {
        let running_c = Arc::clone(&running);
        thread::spawn(move || log_loop(rx, running_c));
    }
    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
    }
}

fn heartbeat_loop(tx: Sender<OutboundMessage>, running: Arc<AtomicBool>) {
    let mut next_tick = Instant::now();
    while running.load(Ordering::SeqCst) {
        if Instant::now() >= next_tick {
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
            let payload = now_ns.to_be_bytes().to_vec();
            eprintln!("[app] time_sync ts={now_ns} sent");
            if tx.send(OutboundMessage { msg_type: MSG_TYPE_TIME_SYNC, payload }).is_err() {
                break;
            }
            next_tick += Duration::from_secs(1);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn log_loop(rx: Receiver<InboundMessage>, running: Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(msg) => {
                if msg.msg_type == MSG_TYPE_TIME_SYNC && msg.payload.len() == 8 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&msg.payload);
                    let ts_ns = u64::from_be_bytes(bytes);
                    let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
                    let delta_ms = (now_ns as i128 - ts_ns as i128) / 1_000_000;
                    eprintln!("[app] time_sync ts={ts_ns} Δ={delta_ms}ms");
                } else {
                    eprintln!("[app] msg type=0x{:02x} len={}", msg.msg_type, msg.payload.len());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
}

pub fn run_tunnel(
    outbound_tx: std::sync::mpsc::Sender<crate::flicker::OutboundMessage>,
    inbound_rx: std::sync::mpsc::Receiver<crate::flicker::InboundMessage>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    socks_bind: std::net::SocketAddr,
    with_bench_support: bool,
) {
    use crate::flicker::frame::block_count_for;
    use crate::flicker::fec::RS_BLOCK_K;
    use crate::flicker::fragment::FRAGMENT_HEADER_BYTES;
    use crate::flicker::grid::FlickerParams;
    use crate::tunnel::{adapter::FlickerChannel, kcp::Profile, metrics::{EventEmitter, new_run_id}, Tunnel};
    use std::sync::Arc;

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[peer/tunnel] runtime init failed: {e}");
            return;
        }
    };
    rt.block_on(async move {
        let run_id = new_run_id();
        let base = std::env::var("METRICS_DIR").unwrap_or_else(|_| "./metrics".into());
        let dir = std::path::PathBuf::from(base).join(&run_id);
        let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
        let em = match EventEmitter::new(&dir, peer_id) {
            Ok(e) => Arc::new(e),
            Err(e) => {
                eprintln!("[peer/tunnel] metrics dir setup failed: {e}");
                return;
            }
        };
        eprintln!("[peer/tunnel] run_id={} metrics_dir={}", run_id, dir.display());

        // Runtime-computed tunnel MTU: full payload capacity of the current
        // FlickerParams + modulation mode, minus fragment header and CRC32.
        // At 256x144 mode B this is ~227 bytes; at 640x360 mode B it is ~2387.
        let cfg = crate::config::load_peer().unwrap_or_else(|_| crate::config::PeerConfig {
            my_rtmp_url: String::new(), my_stream_key: String::new(),
            their_vk_channel: String::new(), their_stream_name: String::new(),
            modulation_mode: crate::flicker::ModulationMode::B,
            frag_timeout_ms: 2000, rx_warmup_ms: 0, log_every_frame: false,
            flicker_fps: 24, stream_width: 256, stream_height: 144,
        });
        let params = FlickerParams::new(cfg.stream_width, cfg.stream_height, cfg.flicker_fps.max(1));
        let block_count = block_count_for(&params, cfg.modulation_mode);
        let frame_capacity = block_count * RS_BLOCK_K;
        let max_payload = frame_capacity.saturating_sub(FRAGMENT_HEADER_BYTES).saturating_sub(4);
        eprintln!("[peer/tunnel] flicker={}x{}@{} mode={:?} block_count={} frame_capacity={} max_payload={}",
            params.width, params.height, params.fps, cfg.modulation_mode, block_count, frame_capacity, max_payload);

        let ch: Arc<dyn crate::tunnel::adapter::DatagramChannel> =
            Arc::new(FlickerChannel::new(outbound_tx, inbound_rx, max_payload));
        let profile = Profile::from_env();
        eprintln!("[peer/tunnel] profile={:?} socks_bind={}", profile, socks_bind);

        let _t = match Tunnel::start(ch, profile, socks_bind, em).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[peer/tunnel] Tunnel::start failed: {e}");
                return;
            }
        };

        if with_bench_support {
            let running_async = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let echo_bind: std::net::SocketAddr = "127.0.0.1:18080".parse().unwrap();
            let dns_bind: std::net::SocketAddr = "127.0.0.1:18053".parse().unwrap();
            let fixtures = std::path::PathBuf::from("./fixtures/dns.txt");
            if let Err(e) = crate::bench::support::spawn_http_echo(echo_bind, std::sync::Arc::clone(&running_async)).await {
                eprintln!("[peer/tunnel] bench-support http_echo bind failed: {e}");
            } else {
                eprintln!("[peer/tunnel] bench-support http_echo listening on {echo_bind}");
            }
            if let Err(e) = crate::bench::support::spawn_tcp_dns(dns_bind, &fixtures, std::sync::Arc::clone(&running_async)).await {
                eprintln!("[peer/tunnel] bench-support tcp_dns bind failed: {e}");
            } else {
                eprintln!("[peer/tunnel] bench-support tcp_dns listening on {dns_bind}");
            }
            let raw_echo_bind: std::net::SocketAddr = "127.0.0.1:18090".parse().unwrap();
            if let Err(e) = crate::bench::support::spawn_raw_echo(raw_echo_bind, std::sync::Arc::clone(&running_async)).await {
                eprintln!("[peer/tunnel] bench-support raw_echo bind failed: {e}");
            } else {
                eprintln!("[peer/tunnel] bench-support raw_echo listening on {raw_echo_bind}");
            }
        }

        while running.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
}
