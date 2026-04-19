pub mod ffmpeg;

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

use crate::config::ClientConfig;
use crate::flicker::frame::encode_timestamp_frame;

fn now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

pub fn run(cfg: ClientConfig) -> Result<()> {
    eprintln!(
        "[flicker/client] video: {}x{}@{}fps; grid: {}x{} cells of {}px ({} total, {} bits of ts); update every {} frames",
        cfg.grid.width,
        cfg.grid.height,
        cfg.grid.fps,
        cfg.grid.cols,
        cfg.grid.rows,
        cfg.grid.cell,
        cfg.grid.total_cells,
        cfg.grid.total_cells.min(64),
        cfg.grid.update_every,
    );
    eprintln!("[flicker/client] rtmp target: {}", cfg.rtmp_url);

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
            .context("failed to set Ctrl+C handler")?;
    }

    let args = ffmpeg::publish_args(&cfg.rtmp_url, &cfg.grid);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg — is it on PATH?")?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on ffmpeg"))?;

    let mut frame = vec![0u8; cfg.grid.frame_bytes()];
    let frame_period = Duration::from_nanos(1_000_000_000 / cfg.grid.fps as u64);
    let start = Instant::now();
    let mut frame_idx: u64 = 0;

    while running.load(Ordering::SeqCst) {
        if frame_idx % cfg.grid.update_every == 0 {
            encode_timestamp_frame(&mut frame, now_ns(), &cfg.grid);
        }

        if let Err(e) = stdin.write_all(&frame) {
            eprintln!("[flicker/client] ffmpeg stdin closed: {e}");
            break;
        }

        frame_idx += 1;
        let target = start + frame_period * frame_idx as u32;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
    }

    drop(stdin);
    let status = child.wait().context("failed to wait for ffmpeg")?;
    if !status.success() {
        eprintln!("[flicker/client] ffmpeg exited with {status}");
    }
    Ok(())
}
