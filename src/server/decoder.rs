use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::flicker::frame::decode_timestamp_frame;
use crate::flicker::{GridConfig, FRAME_BYTES};

fn local_now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

pub fn run<R: Read>(
    mut stdout: R,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    let mut frame = vec![0u8; FRAME_BYTES];
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
        let delta_ns = now_ns as i128 - ts_ns as i128;
        let delta_ms = delta_ns / 1_000_000;

        if log_every_frame || last_ts != Some(ts_ns) {
            eprintln!(
                "[flicker] frame={:>8} ts_ns={} Δ={:>6} ms",
                frame_idx, ts_ns, delta_ms
            );
        }
        last_ts = Some(ts_ns);
        frame_idx += 1;
    }
    Ok(())
}

pub fn run_owned_stdout(
    stdout: std::process::ChildStdout,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    run(stdout, cfg, running, log_every_frame).context("decoder loop")
}
