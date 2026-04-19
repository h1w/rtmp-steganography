pub mod decoder;
pub mod ingest;
pub mod vk_live;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{HttpConfig, ServerConfig, SourceConfig};

const BACKOFF_SEQ_SECS: &[u64] = &[1, 2, 5, 10];

struct ResolvedSource {
    url: String,
    http: HttpConfig,
    is_hls: bool,
}

fn resolve(source: &SourceConfig, base_http: &HttpConfig) -> Result<ResolvedSource> {
    match source {
        SourceConfig::DirectUrl(u) => {
            let url = sanitize_url(u);
            let is_hls = looks_like_hls(&url);
            Ok(ResolvedSource {
                url,
                http: base_http.clone(),
                is_hls,
            })
        }
        SourceConfig::VkLiveSlug(slug) => {
            let page_url = format!("https://live.vkvideo.ru/{slug}");
            let referer = base_http
                .referer
                .clone()
                .unwrap_or_else(|| page_url.clone());
            let origin = base_http
                .origin
                .clone()
                .unwrap_or_else(|| "https://live.vkvideo.ru".to_string());
            let r = vk_live::wait_for_playback_ready(slug, &referer, &origin)?;
            let url = vk_live::pick_playback_url(&r)
                .context("VK Live: no dash/hls after wait")?;
            let url = sanitize_url(&url);
            let is_hls = looks_like_hls(&url);
            let mut http = base_http.clone();
            http.referer = Some(referer);
            http.origin = Some(origin);
            if http.user_agent.is_none() {
                http.user_agent = Some(
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
                        .to_string(),
                );
            }
            eprintln!(
                "[flicker/server] VK `{}` -> {} ({})",
                slug,
                url.chars().take(80).collect::<String>(),
                if is_hls { "hls" } else { "dash" }
            );
            Ok(ResolvedSource { url, http, is_hls })
        }
    }
}

fn looks_like_hls(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains(".m3u8") || u.contains("/hls")
}

/// Strip okcdn CMAF ultra-low-latency flag (`low-latency=yes|1`) from the query.
/// VK Live's ULL CMAF chunks are not consumed correctly by stock ffmpeg; removing
/// this param forces the CDN to serve regular segments that ffmpeg can parse.
fn sanitize_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            let lower = kv.to_ascii_lowercase();
            !(lower == "low-latency=yes"
                || lower == "low-latency=1"
                || lower == "low-latency=true")
        })
        .collect();
    let mut changed = kept.len() != query.split('&').count();
    if !changed {
        return url.to_string();
    }
    if kept.is_empty() {
        changed = true;
        let _ = changed;
        return base.to_string();
    }
    format!("{base}?{}", kept.join("&"))
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

pub fn run(cfg: ServerConfig) -> Result<()> {
    eprintln!(
        "[flicker/server] grid: {}x{} cells of {}px ({} bits of ts)",
        cfg.grid.cols,
        cfg.grid.rows,
        cfg.grid.cell,
        cfg.grid.total_cells.min(64),
    );
    if cfg.log_every_frame {
        eprintln!("[flicker/server] logging every frame (stream_log_every_frame=1)");
    } else {
        eprintln!("[flicker/server] logging on ts change (set stream_log_every_frame=1 for per-frame)");
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
        let resolved = match resolve(&cfg.source, &cfg.http) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[flicker/server] resolve failed: {e:#}");
                backoff(attempt, &running);
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        let args = ingest::read_args(&resolved.url, &resolved.http, resolved.is_hls);
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
