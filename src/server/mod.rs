pub mod decoder;
pub mod ingest;
pub mod vk_live;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::ServerConfig;

const BACKOFF_SEQ_SECS: &[u64] = &[1, 2, 5, 10];

struct ResolvedSource {
    url: String,
    is_hls: bool,
}

fn resolve(page_url: &str) -> Result<ResolvedSource> {
    let r = vk_live::resolve_page(page_url)?;
    let url = vk_live::pick_playback_url(&r)
        .context("no live_hls / live_dash on page")?;
    let url = sanitize_url(&url);
    let is_hls = looks_like_hls(&url);
    eprintln!(
        "[flicker/server] resolved -> {} ({})",
        url.chars().take(96).collect::<String>(),
        if is_hls { "hls" } else { "dash" }
    );
    Ok(ResolvedSource { url, is_hls })
}

fn looks_like_hls(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains(".m3u8") || u.contains("/hls")
}

/// Strip okcdn CMAF ultra-low-latency flag from the query; stock ffmpeg
/// can't decode ULL fragments, so this forces regular segments.
fn sanitize_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let original_parts = query.split('&').count();
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            let lower = kv.to_ascii_lowercase();
            !(lower == "low-latency=yes"
                || lower == "low-latency=1"
                || lower == "low-latency=true")
        })
        .collect();
    if kept.len() == original_parts {
        return url.to_string();
    }
    if kept.is_empty() {
        return base.to_string();
    }
    format!("{base}?{}", kept.join("&"))
}

pub fn run(cfg: ServerConfig) -> Result<()> {
    eprintln!(
        "[flicker/server] grid: {}x{} cells of {}px ({} bits of ts)",
        cfg.grid.cols,
        cfg.grid.rows,
        cfg.grid.cell,
        cfg.grid.total_cells.min(64),
    );
    eprintln!("[flicker/server] page: {}", cfg.page_url);
    if cfg.log_every_frame {
        eprintln!("[flicker/server] logging every frame (stream_log_every_frame=1)");
    } else {
        eprintln!(
            "[flicker/server] logging on ts change (set stream_log_every_frame=1 for per-frame)"
        );
    }

    let running = Arc::new(AtomicBool::new(true));
    let child_slot: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
    {
        let r = running.clone();
        let slot = child_slot.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
            if let Ok(mut g) = slot.lock() {
                if let Some(c) = g.as_mut() {
                    let _ = c.kill();
                }
            }
        })
        .context("failed to set Ctrl+C handler")?;
    }

    let mut attempt: usize = 0;
    while running.load(Ordering::SeqCst) {
        let resolved = match resolve(&cfg.page_url) {
            Ok(r) => r,
            Err(e) => {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                eprintln!("[flicker/server] resolve failed: {e:#}");
                backoff(attempt, &running);
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        let args = ingest::read_args(&resolved.url, &cfg.page_url, resolved.is_hls);
        let mut child = match ingest::spawn(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[flicker/server] spawn ffmpeg failed: {e:#}");
                backoff(attempt, &running);
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout on ffmpeg"))?;

        {
            let mut g = child_slot
                .lock()
                .map_err(|e| anyhow!("child mutex poisoned: {e}"))?;
            *g = Some(child);
        }

        attempt = 0;

        let decode_result =
            decoder::run_owned_stdout(stdout, &cfg.grid, &running, cfg.log_every_frame);

        let wait_status = {
            let mut g = child_slot
                .lock()
                .map_err(|e| anyhow!("child mutex poisoned: {e}"))?;
            if let Some(mut c) = g.take() {
                c.wait().ok()
            } else {
                None
            }
        };

        if let Err(e) = decode_result {
            eprintln!("[flicker/server] decoder error: {e:#}");
        }
        if let Some(s) = wait_status {
            if !s.success() {
                eprintln!("[flicker/server] ffmpeg exited with {s}");
            }
        }

        if !running.load(Ordering::SeqCst) {
            break;
        }

        eprintln!("[flicker/server] reconnecting…");
        attempt = attempt.saturating_add(1);
        backoff(attempt, &running);
    }
    Ok(())
}

fn backoff(attempt: usize, running: &Arc<AtomicBool>) {
    let secs = BACKOFF_SEQ_SECS
        .get(attempt.min(BACKOFF_SEQ_SECS.len().saturating_sub(1)))
        .copied()
        .unwrap_or(10);
    let total = Duration::from_secs(secs);
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < total && running.load(Ordering::SeqCst) {
        thread::sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_url;

    #[test]
    fn strips_low_latency_yes() {
        let u = "https://vsd208.okcdn.ru/cmaf/14/sig/x/urls/1/t704368.v.m4s?low-latency=yes";
        assert_eq!(
            sanitize_url(u),
            "https://vsd208.okcdn.ru/cmaf/14/sig/x/urls/1/t704368.v.m4s"
        );
    }

    #[test]
    fn keeps_other_query_params() {
        let u = "https://x.ru/m.mpd?foo=bar&low-latency=yes&baz=1";
        assert_eq!(sanitize_url(u), "https://x.ru/m.mpd?foo=bar&baz=1");
    }

    #[test]
    fn leaves_unrelated_urls_alone() {
        let u = "https://x.ru/m.mpd?foo=bar";
        assert_eq!(sanitize_url(u), u);
    }
}
