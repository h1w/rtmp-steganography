use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::flicker::frame::decode_timestamp_frame;
use crate::flicker::{GridConfig, FRAME_BYTES};

fn local_now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

/// Single-slot channel: reader overwrites, decoder always grabs the newest.
struct FrameSlot {
    data: Mutex<Option<Vec<u8>>>,
    cv: Condvar,
    eof: AtomicBool,
}

impl FrameSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(None),
            cv: Condvar::new(),
            eof: AtomicBool::new(false),
        })
    }

    fn put(&self, frame: Vec<u8>) {
        let mut g = self.data.lock().expect("frame-slot mutex poisoned");
        *g = Some(frame);
        drop(g);
        self.cv.notify_one();
    }

    fn mark_eof(&self) {
        self.eof.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }

    /// Blocks until a new frame is available, EOF is signalled, or `running`
    /// flips to false. Returns `None` on EOF or shutdown.
    fn take_newest(&self, running: &Arc<AtomicBool>) -> Option<Vec<u8>> {
        let mut g = self.data.lock().expect("frame-slot mutex poisoned");
        loop {
            if let Some(f) = g.take() {
                return Some(f);
            }
            if self.eof.load(Ordering::SeqCst) || !running.load(Ordering::SeqCst) {
                return None;
            }
            let (g2, _timeout) = self
                .cv
                .wait_timeout(g, Duration::from_millis(100))
                .expect("frame-slot cv wait");
            g = g2;
        }
    }
}

pub fn run_owned_stdout(
    stdout: std::process::ChildStdout,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    let slot = FrameSlot::new();

    // Reader thread: pulls raw frames from ffmpeg stdout as fast as possible.
    // Because it overwrites the slot each time, any backlog (e.g. the HLS
    // warm-up ramp that used to take ~5 s) is effectively skipped — the
    // decoder side always grabs the latest frame the reader has read.
    let reader_slot = slot.clone();
    let reader = thread::spawn(move || reader_loop(stdout, reader_slot));

    let decode_result = decoder_loop(&slot, cfg, running, log_every_frame);

    // Reader will exit once ffmpeg stdout EOFs (caller kills child on shutdown
    // or reconnect). Join to avoid dangling threads.
    let _ = reader.join();

    decode_result.context("decoder loop")
}

fn reader_loop(mut stdout: std::process::ChildStdout, slot: Arc<FrameSlot>) {
    let mut buf = vec![0u8; FRAME_BYTES];
    loop {
        match stdout.read_exact(&mut buf) {
            Ok(()) => {
                // Clone on put so the reader's buffer stays reusable; the
                // allocation is one Vec per frame which is cheap next to
                // ffmpeg's I/O cost.
                slot.put(buf.clone());
            }
            Err(e) => {
                eprintln!("[flicker/server] reader: stream ended / short read: {e}");
                slot.mark_eof();
                return;
            }
        }
    }
}

fn decoder_loop(
    slot: &Arc<FrameSlot>,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    let mut frame_idx: u64 = 0;
    let mut last_ts: Option<u64> = None;
    let mut dropped_since_log: u64 = 0;

    while running.load(Ordering::SeqCst) {
        let Some(frame) = slot.take_newest(running) else {
            return Ok(());
        };

        let ts_ns = decode_timestamp_frame(&frame, cfg);
        let now_ns = local_now_ns();
        let delta_ms = (now_ns as i128 - ts_ns as i128) / 1_000_000;

        if log_every_frame || last_ts != Some(ts_ns) {
            eprintln!(
                "[flicker] frame={:>8} ts_ns={} Δ={:>6} ms  dropped_stale={}",
                frame_idx, ts_ns, delta_ms, dropped_since_log
            );
            dropped_since_log = 0;
        } else {
            dropped_since_log = dropped_since_log.saturating_add(1);
        }
        last_ts = Some(ts_ns);
        frame_idx = frame_idx.saturating_add(1);
    }
    Ok(())
}
