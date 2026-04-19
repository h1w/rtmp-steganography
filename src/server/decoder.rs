use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::flicker::frame::decode_timestamp_frame;
use crate::flicker::GridConfig;

fn local_now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

/// Sequential, lossless read loop. Every frame emitted by ffmpeg is decoded —
/// the flicker protocol is a data channel, so frame skipping is not allowed.
/// Latency is whatever HLS/DASH gives us plus the decode time of the queued
/// burst; there is no catch-up mechanism that silently drops frames.
pub fn run_owned_stdout(
    stdout: std::process::ChildStdout,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    run_loop(stdout, cfg, running, log_every_frame).context("decoder loop")
}

fn run_loop<R: Read>(
    mut stdout: R,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    let mut frame = vec![0u8; cfg.frame_bytes()];
    let mut frame_idx: u64 = 0;
    let mut last_ts: Option<u64> = None;

    while running.load(Ordering::SeqCst) {
        match stdout.read_exact(&mut frame) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[flicker/server] stream ended / short read: {e}");
                return Ok(());
            }
        }

        let ts_ns = decode_timestamp_frame(&frame, cfg);
        let now_ns = local_now_ns();
        let delta_ms = (now_ns as i128 - ts_ns as i128) / 1_000_000;

        if log_every_frame || last_ts != Some(ts_ns) {
            eprintln!(
                "[flicker] frame={:>6} ts_ns={} Δ={:>5} ms",
                frame_idx, ts_ns, delta_ms
            );
        }
        last_ts = Some(ts_ns);
        frame_idx = frame_idx.saturating_add(1);
    }
    Ok(())
}
