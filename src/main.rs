use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

const WIDTH: usize = 256;
const HEIGHT: usize = 144;
const FPS: u32 = 30;
const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;
const DEFAULT_CELL: usize = 16;
const DEFAULT_UPDATE_EVERY: u64 = 5;

struct GridConfig {
    cell: usize,
    cols: usize,
    rows: usize,
    total_cells: usize,
    update_every: u64,
}

fn load_grid_config() -> Result<GridConfig> {
    let cell = match std::env::var("cell_size") {
        Ok(v) => v
            .trim()
            .parse::<usize>()
            .with_context(|| format!("cell_size is not a valid positive integer: {v:?}"))?,
        Err(_) => DEFAULT_CELL,
    };
    if cell == 0 {
        return Err(anyhow!("cell_size must be > 0"));
    }
    if WIDTH % cell != 0 || HEIGHT % cell != 0 {
        return Err(anyhow!(
            "cell_size={cell} must divide both WIDTH={WIDTH} and HEIGHT={HEIGHT} evenly \
             (valid values: 1, 2, 4, 8, 16)"
        ));
    }
    let update_every = match std::env::var("update_every_frames") {
        Ok(v) => v
            .trim()
            .parse::<u64>()
            .with_context(|| format!("update_every_frames is not a valid positive integer: {v:?}"))?,
        Err(_) => DEFAULT_UPDATE_EVERY,
    };
    if update_every == 0 {
        return Err(anyhow!("update_every_frames must be >= 1"));
    }
    let cols = WIDTH / cell;
    let rows = HEIGHT / cell;
    Ok(GridConfig {
        cell,
        cols,
        rows,
        total_cells: cols * rows,
        update_every,
    })
}

fn encode_timestamp_frame(buf: &mut [u8], ts_ns: u64, cfg: &GridConfig) {
    buf.fill(0);
    let bits_to_encode = cfg.total_cells.min(64);
    for bit_idx in 0..cfg.total_cells {
        let bit = if bit_idx < bits_to_encode {
            ((ts_ns >> (bits_to_encode - 1 - bit_idx)) & 1) as u8
        } else {
            0
        };
        if bit == 0 {
            continue;
        }
        let cx = bit_idx % cfg.cols;
        let cy = bit_idx / cfg.cols;
        let x0 = cx * cfg.cell;
        let y0 = cy * cfg.cell;
        for y in y0..y0 + cfg.cell {
            let row_start = (y * WIDTH + x0) * 3;
            let row_end = row_start + cfg.cell * 3;
            buf[row_start..row_end].fill(255);
        }
    }
}

fn now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let key = std::env::var("stream_key").context("stream_key not set in .env")?;
    let server = std::env::var("rtmp_server").context("rtmp_server not set in .env")?;
    let url = format!("{}/{}", server.trim_end_matches('/'), key);

    let cfg = load_grid_config()?;
    eprintln!(
        "grid: {}x{} cells of {}px ({} total, {} bits for timestamp); update every {} frames",
        cfg.cols,
        cfg.rows,
        cfg.cell,
        cfg.total_cells,
        cfg.total_cells.min(64),
        cfg.update_every,
    );

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
            .context("failed to set Ctrl+C handler")?;
    }

    let size_arg = format!("{}x{}", WIDTH, HEIGHT);
    let rate_arg = format!("{}", FPS);
    let gop_arg = format!("{}", FPS * 2);

    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel", "info",
            "-y",
            "-use_wallclock_as_timestamps", "1",
            "-thread_queue_size", "1024",
            "-f", "rawvideo",
            "-pix_fmt", "rgb24",
            "-s", &size_arg,
            "-r", &rate_arg,
            "-i", "-",
            "-f", "lavfi",
            "-i", "anullsrc=channel_layout=stereo:sample_rate=44100",
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-tune", "zerolatency",
            "-profile:v", "baseline",
            "-level", "3.0",
            "-pix_fmt", "yuv420p",
            "-b:v", "300k",
            "-maxrate", "300k",
            "-bufsize", "600k",
            "-g", &gop_arg,
            "-keyint_min", &rate_arg,
            "-c:a", "aac",
            "-b:a", "64k",
            "-ar", "44100",
            "-ac", "2",
            "-shortest",
            "-flvflags", "no_duration_filesize",
            "-f", "flv",
            &url,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg — is it on PATH?")?;

    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin on ffmpeg"))?;

    let mut frame = vec![0u8; FRAME_BYTES];

    let frame_period = Duration::from_nanos(1_000_000_000 / FPS as u64);
    let start = Instant::now();
    let mut frame_idx: u64 = 0;

    while running.load(Ordering::SeqCst) {
        if frame_idx % cfg.update_every == 0 {
            encode_timestamp_frame(&mut frame, now_ns(), &cfg);
        }

        if let Err(e) = stdin.write_all(&frame) {
            eprintln!("ffmpeg stdin closed: {e}");
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
        eprintln!("ffmpeg exited with {status}");
    }
    Ok(())
}
