//! Resolve a VK Live stream-page URL into an HLS/DASH playback URL.
//!
//! We GET the stream page and pull the inline `"playerUrls"` JSON out of the
//! HTML with a tiny hand-rolled scanner. No VK Live API involvement, no
//! auto-discovery — just the deterministic page URL built from
//! `vk_live_channel` + `client_stream_name`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct VkPlaybackResolved {
    pub dash_mpd: Option<String>,
    pub hls: Option<String>,
    pub cmaf: Option<String>,
    /// `live_ondemand_hls`: HLS manifest over the source-passthrough CMAF
    /// endpoint. Typically preserves publish resolution (no VK ladder upscale)
    /// AND serves .m3u8 that ffmpeg's HLS demuxer pulls aggressively, unlike
    /// the DASH .mpd of `live_cmaf`.
    pub ondemand_hls: Option<String>,
    pub page_url: String,
}

/// Extracts the first balanced `[...]` JSON array that follows `"playerUrls"`
/// in the HTML body.
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

pub fn resolve_page(page_url: &str) -> Result<VkPlaybackResolved> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .context("reqwest client")?;

    let resp = client
        .get(page_url)
        .header("Referer", page_url)
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
            "no `playerUrls` in HTML at {page_url} — stream is offline or link is wrong"
        )
    })?;
    let arr: Value = serde_json::from_str(json_slice)
        .with_context(|| format!("parse playerUrls from {page_url}"))?;

    // Collect all variants; caller picks one based on env preference.
    // live_cmaf = LL-HLS / CMAF endpoint (typically source-passthrough,
    // NO transcoder upscale to the VK bitrate ladder).
    let mut dash_mpd: Option<String> = None;
    let mut hls: Option<String> = None;
    let mut cmaf: Option<String> = None;
    let mut ondemand_hls: Option<String> = None;
    eprintln!("[peer/rx] VK playerUrls variants offered:");
    for p in arr.as_array().into_iter().flatten() {
        let t = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let u = p.get("url").and_then(|x| x.as_str()).unwrap_or("").trim();
        if u.is_empty() {
            continue;
        }
        let query_flags: Vec<&str> = ["llhls", "low_latency", "ll=1", "ull", "cmaf", "chunked", "partial"]
            .into_iter().filter(|k| u.contains(k)).collect();
        eprintln!("[peer/rx]   type={:<16} url_head={:<100} query_flags={:?}",
            t, &u.chars().take(100).collect::<String>(), query_flags);
        match t {
            "live_hls" => { hls.get_or_insert_with(|| u.to_string()); }
            "live_dash" => { dash_mpd.get_or_insert_with(|| u.to_string()); }
            "live_cmaf" => { cmaf.get_or_insert_with(|| u.to_string()); }
            "live_ondemand_hls" => { ondemand_hls.get_or_insert_with(|| u.to_string()); }
            _ => {}
        }
    }

    if hls.is_none() && dash_mpd.is_none() && cmaf.is_none() && ondemand_hls.is_none() {
        return Err(anyhow!(
            "page has playerUrls but no playable live_hls/live_cmaf/live_dash entry"
        ));
    }

    Ok(VkPlaybackResolved {
        dash_mpd,
        hls,
        cmaf,
        ondemand_hls,
        page_url: page_url.to_string(),
    })
}

pub fn pick_playback_url(r: &VkPlaybackResolved) -> Option<String> {
    r.hls.clone().or_else(|| r.cmaf.clone()).or_else(|| r.dash_mpd.clone())
}

/// Adapter for peer mode: given a VK channel slug and stream name, fetch the
/// playback URL. Returns `(url, is_hls)`. The variant is selected by
/// `peer_vk_prefer` env: "cmaf" (LL-HLS, no upscale), "hls" (default — plain
/// HLS with VK ladder transcoding), "dash" (MPEG-DASH).
pub fn resolve(channel: &str, name: &str) -> Result<(String, bool)> {
    let page_url = format!(
        "https://live.vkvideo.ru/{}/stream/{}",
        channel.trim_matches('/'),
        name.trim_matches('/')
    );
    let playback = resolve_page(&page_url)?;
    let prefer = std::env::var("peer_vk_prefer").unwrap_or_default().to_ascii_lowercase();
    let prefer = prefer.trim();
    // For "cmaf" preference, try live_ondemand_hls first (m3u8 over CMAF
    // source-passthrough → fast pull + no upscale), then live_cmaf (DASH
    // .mpd — correct resolution but slow demuxer), then fall back to
    // transcoded live_hls/live_dash.
    let order: Vec<&Option<String>> = match prefer {
        "cmaf" => vec![&playback.ondemand_hls, &playback.cmaf, &playback.hls, &playback.dash_mpd],
        "dash" => vec![&playback.dash_mpd, &playback.hls, &playback.ondemand_hls, &playback.cmaf],
        "ondemand" => vec![&playback.ondemand_hls, &playback.hls, &playback.cmaf, &playback.dash_mpd],
        _ => vec![&playback.hls, &playback.ondemand_hls, &playback.cmaf, &playback.dash_mpd],
    };
    eprintln!("[peer/rx] peer_vk_prefer='{}' → pick order: {}",
        if prefer.is_empty() { "hls (default)" } else { prefer },
        order.iter().map(|u| label_var(u, &playback)).collect::<Vec<_>>().join(", "));
    for candidate in order {
        if let Some(url) = candidate {
            // Classify by URL suffix: .m3u8 → HLS (needs -live_start_index),
            // .mpd → DASH (no HLS-specific flags; VK's CMAF endpoint actually
            // serves a DASH manifest despite the "cmaf" path segment).
            let lower = url.to_ascii_lowercase();
            let is_hls = lower.contains(".m3u8");
            return Ok((url.clone(), is_hls));
        }
    }
    Err(anyhow!("no playable URL in VK response"))
}

fn label_var(u: &Option<String>, p: &VkPlaybackResolved) -> &'static str {
    match u {
        Some(s) if Some(s) == p.ondemand_hls.as_ref() => "ondemand_hls",
        Some(s) if Some(s) == p.cmaf.as_ref() => "cmaf",
        Some(s) if Some(s) == p.hls.as_ref() => "hls",
        Some(s) if Some(s) == p.dash_mpd.as_ref() => "dash",
        _ => "—",
    }
}
