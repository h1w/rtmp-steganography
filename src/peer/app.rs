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
