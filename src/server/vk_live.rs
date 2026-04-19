//! VK Live playback URL resolver via `api.live.vkvideo.ru`.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const API_BASE: &str = "https://api.live.vkvideo.ru/v1";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct VkPlaybackResolved {
    pub dash_mpd: Option<String>,
    pub hls: Option<String>,
    pub page_url: String,
}

pub fn resolve_channel(slug: &str) -> Result<VkPlaybackResolved> {
    let slug = slug.trim().trim_matches('/');
    if slug.is_empty() {
        return Err(anyhow!("vk live: empty channel slug"));
    }
    let page_url = format!("https://live.vkvideo.ru/{slug}");
    let api_url = format!("{API_BASE}/blog/{slug}/public_video_stream");

    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .build()
        .context("reqwest client")?;

    let resp = client
        .get(&api_url)
        .header("Referer", &page_url)
        .header("Origin", "https://live.vkvideo.ru")
        .send()
        .with_context(|| format!("GET {api_url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("api.live.vkvideo.ru: HTTP {}", resp.status()));
    }

    let v: Value = resp.json().context("parse VK Live API JSON")?;

    if let Some(msg) = v.get("error_description").and_then(|x| x.as_str()) {
        if !msg.is_empty() {
            return Err(anyhow!("VK Live API: {msg}"));
        }
    }
    let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
    if !err.is_empty() {
        return Err(anyhow!("VK Live API error field: {err}"));
    }

    let first = v
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!(
            "VK Live: API returned no data[] for slug \"{slug}\" \
             (channel offline, wrong slug, or API requires different headers). \
             Workaround: paste the MPD/HLS URL from the browser's network tab into .env as `stream_read_url=...`"
        ))?;

    let pairs = first
        .get("playerUrls")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow!("VK Live: no playerUrls (no active player?)"))?;

    let mut dash_mpd = None;
    let mut hls = None;
    for p in pairs {
        let t = p
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_lowercase();
        let u = p.get("url").and_then(|x| x.as_str()).unwrap_or("").trim();
        if u.is_empty() {
            continue;
        }
        if u.contains(".mpd") || t.contains("dash") {
            dash_mpd.get_or_insert_with(|| u.to_string());
        }
        if t.contains("hls") || t.contains("m3u8") || u.contains(".m3u8") {
            hls.get_or_insert_with(|| u.to_string());
        }
    }

    Ok(VkPlaybackResolved {
        dash_mpd,
        hls,
        page_url,
    })
}

/// Prefer HLS for lower latency, fall back to DASH.
pub fn pick_playback_url(r: &VkPlaybackResolved) -> Option<String> {
    r.hls.clone().or_else(|| r.dash_mpd.clone())
}

fn wait_interval() -> Duration {
    std::env::var("VK_LIVE_WAIT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(5))
}

fn wait_timeout() -> Option<Duration> {
    std::env::var("VK_LIVE_WAIT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
}

fn skip_probe() -> bool {
    std::env::var("VK_LIVE_SKIP_PROBE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub fn probe_playback_url(url: &str, referer: &str, origin: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(25))
        .build()
        .context("reqwest probe client")?;
    let resp = client
        .get(url)
        .header("Referer", referer)
        .header("Origin", origin)
        .send()
        .with_context(|| format!("probe GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("CDN replied {}", status));
    }
    let body = resp.text().with_context(|| format!("probe read body {url}"))?;
    validate_probe_body(url, &body)
}

fn validate_probe_body(url: &str, body: &str) -> Result<()> {
    let u = url.to_ascii_lowercase();
    let is_hls_hint = u.contains(".m3u8") || u.contains("/hls") || u.contains("m3u8");
    let t = body.trim_start();
    let head = if t.len() > 2048 { &t[..2048] } else { t };
    let head_lower = head.to_ascii_lowercase();
    if head_lower.starts_with("#extm3u") || body.trim_start().starts_with("#EXTM3U") {
        return Ok(());
    }
    if is_hls_hint {
        if !body.contains("#EXTM3U") {
            return Err(anyhow!("CDN: body does not look like HLS (no #EXTM3U)"));
        }
        return Ok(());
    }
    if head_lower.starts_with("<!doctype") || head_lower.contains("<html") {
        return Err(anyhow!(
            "CDN: got HTML instead of manifest (403/redirect/expired URL)"
        ));
    }
    if !body.contains("<MPD") && !body.contains("<mpd") {
        return Err(anyhow!("CDN: not MPD XML (no <MPD> root)"));
    }
    if !body.contains("<Period") && !body.contains("<period") {
        return Err(anyhow!("CDN: MPD has no <Period> (expired URL or offline)"));
    }
    Ok(())
}

pub fn wait_for_playback_ready(slug: &str, referer: &str, origin: &str) -> Result<VkPlaybackResolved> {
    let interval = wait_interval();
    let timeout = wait_timeout();
    let started = Instant::now();
    let mut attempt = 0u32;
    let no_probe = skip_probe();
    eprintln!(
        "[flicker/vk] waiting for slug={slug:?} (interval {}s; timeout {}; probe {})",
        interval.as_secs(),
        match timeout {
            Some(t) => format!("{}s", t.as_secs()),
            None => "none".to_string(),
        },
        if no_probe { "off" } else { "on" }
    );
    loop {
        if let Some(t) = timeout {
            if started.elapsed() > t {
                return Err(anyhow!("VK Live: wait timeout ({t:?})"));
            }
        }
        attempt += 1;
        match resolve_channel(slug) {
            Ok(r) => {
                let Some(url) = pick_playback_url(&r) else {
                    eprintln!(
                        "[flicker/vk] attempt {attempt}: no dash/hls yet, retry in {:?}",
                        interval
                    );
                    thread::sleep(interval);
                    continue;
                };
                if no_probe {
                    eprintln!("[flicker/vk] got URL, probe skipped");
                    return Ok(r);
                }
                match probe_playback_url(&url, referer, origin) {
                    Ok(()) => {
                        eprintln!("[flicker/vk] manifest OK");
                        return Ok(r);
                    }
                    Err(e) => {
                        eprintln!(
                            "[flicker/vk] attempt {attempt}: CDN: {e}; retry in {:?}",
                            interval
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[flicker/vk] attempt {attempt}: {e}");
            }
        }
        thread::sleep(interval);
    }
}
