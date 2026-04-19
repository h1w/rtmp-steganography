//! VK Live playback URL resolver.
//!
//! We scrape the stream page HTML and parse the inline `"playerUrls"` array.
//! The `/v1/blog/<slug>/public_video_stream` API endpoint only sees the blog's
//! primary public stream — it returns an empty `data[]` for unlisted sub-streams
//! like `/pavel8899/stream/sl_76330`. The HTML page always embeds the active
//! `playerUrls` for whatever stream the URL points at, so parsing it is both
//! more reliable and works for sub-streams automatically.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// Sleep in 200ms chunks so Ctrl+C (which flips `running`) is honored quickly.
fn sleep_interruptible(total: Duration, running: &Arc<AtomicBool>) {
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < total && running.load(Ordering::SeqCst) {
        thread::sleep(step);
        waited += step;
    }
}

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct VkPlaybackResolved {
    pub dash_mpd: Option<String>,
    pub hls: Option<String>,
    pub page_url: String,
}

/// Accepts a slug (`pavel8899`), a path (`pavel8899/stream/sl_76330`) or a
/// full URL (`https://live.vkvideo.ru/...`) and normalises to a page URL.
pub fn page_url_from_channel(channel: &str) -> String {
    let c = channel.trim();
    if c.starts_with("http://") || c.starts_with("https://") {
        c.to_string()
    } else {
        format!("https://live.vkvideo.ru/{}", c.trim_start_matches('/'))
    }
}

/// Extracts the first balanced `[...]` JSON array that follows `"playerUrls"`
/// in the HTML, without needing a full HTML parser. Returns the slice
/// including the enclosing brackets, ready to feed to `serde_json`.
fn extract_player_urls_json(html: &str) -> Option<&str> {
    let needle = "\"playerUrls\"";
    let mut search_from = 0usize;
    while let Some(rel) = html[search_from..].find(needle) {
        let after_key = search_from + rel + needle.len();
        let tail = &html[after_key..];
        let trimmed = tail.trim_start();
        if !trimmed.starts_with(':') {
            search_from = after_key;
            continue;
        }
        let after_colon = &trimmed[1..];
        let after_colon_trim = after_colon.trim_start();
        if !after_colon_trim.starts_with('[') {
            search_from = after_key;
            continue;
        }
        let open_offset = html.len() - after_colon_trim.len();
        let bytes = html.as_bytes();
        let mut depth: i32 = 0;
        let mut in_str = false;
        let mut esc = false;
        let mut i = open_offset;
        while i < bytes.len() {
            let c = bytes[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&html[open_offset..=i]);
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        search_from = after_key;
    }
    None
}

pub fn resolve_channel(channel: &str) -> Result<VkPlaybackResolved> {
    let channel = channel.trim();
    if channel.is_empty() {
        return Err(anyhow!("vk live: empty channel"));
    }
    let page_url = page_url_from_channel(channel);

    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .context("reqwest client")?;

    let resp = client
        .get(&page_url)
        .header("Referer", &page_url)
        .header("Origin", "https://live.vkvideo.ru")
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .with_context(|| format!("GET {page_url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("{}: HTTP {}", page_url, resp.status()));
    }
    let body = resp.text().with_context(|| format!("read body {page_url}"))?;

    let json_slice = extract_player_urls_json(&body).ok_or_else(|| {
        anyhow!(
            "VK Live: no `playerUrls` in page HTML at {page_url} \
             — stream is offline, link is wrong, or the page layout changed"
        )
    })?;
    let arr: Value = serde_json::from_str(json_slice)
        .with_context(|| format!("parse playerUrls from {page_url}"))?;

    // Prefer ffmpeg-friendly streams: `live_hls` (plain m3u8) and `live_dash`
    // (plain MPEG-DASH). Skip the CMAF / ultra-low-latency variants
    // (`live_cmaf`, `live_ondemand_hls`) — stock ffmpeg struggles with okcdn's
    // low-latency CMAF fragments.
    let mut dash_mpd: Option<String> = None;
    let mut hls: Option<String> = None;
    for p in arr.as_array().into_iter().flatten() {
        let t = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let u = p.get("url").and_then(|x| x.as_str()).unwrap_or("").trim();
        if u.is_empty() {
            continue;
        }
        match t {
            "live_hls" => {
                hls.get_or_insert_with(|| u.to_string());
            }
            "live_dash" => {
                dash_mpd.get_or_insert_with(|| u.to_string());
            }
            _ => {}
        }
    }

    if hls.is_none() && dash_mpd.is_none() {
        return Err(anyhow!(
            "VK Live: page has playerUrls but no plain live_hls/live_dash entry"
        ));
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
        .timeout(Duration::from_secs(8))
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

pub fn wait_for_playback_ready(
    slug: &str,
    referer: &str,
    origin: &str,
    running: &Arc<AtomicBool>,
) -> Result<VkPlaybackResolved> {
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
    while running.load(Ordering::SeqCst) {
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
                    sleep_interruptible(interval, running);
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
        sleep_interruptible(interval, running);
    }
    Err(anyhow!("VK Live: wait aborted (Ctrl+C)"))
}
