use anyhow::{anyhow, Context, Result};

use crate::flicker::grid::{FlickerParams, DEFAULT_FPS, DEFAULT_FRAME_H, DEFAULT_FRAME_W};
use crate::flicker::ModulationMode;
use crate::peer::DEFAULT_RX_WARMUP_MS;

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub my_rtmp_url: String,
    pub my_stream_key: String,
    pub their_vk_channel: String,
    pub their_stream_name: String,
    pub modulation_mode: ModulationMode,
    pub frag_timeout_ms: u64,
    pub rx_warmup_ms: u64,
    pub log_every_frame: bool,
    /// Frames per second at which the tx thread emits flicker frames and at
    /// which ffmpeg publishes to RTMP / reads HLS. Defaults to the compile-time
    /// flicker grid FPS. The flicker codec itself doesn't care about wall-clock
    /// cadence as long as tx and rx both agree on this value.
    pub flicker_fps: u32,
    /// Output video resolution on the RTMP/HLS wire. The internal flicker grid
    /// is always 256x144 — ffmpeg upscales to this on publish and downscales
    /// back on read. Defaults to the native 256x144 (no scaling).
    pub stream_width: u32,
    pub stream_height: u32,
    /// Flicker cell size in pixels. Larger cells survive lossy codec
    /// quantisation better at the cost of grid density and capacity.
    pub flicker_cell_size: u32,
    /// If Some, use `-qp N` fixed quantiser instead of CBR bitrate. This
    /// stops x264 from dynamically crushing cells to hit a bitrate target.
    /// Actual bitrate becomes content-driven.
    pub x264_qp: Option<u32>,
    /// If Some, override the sqrt-scaled default bitrate (kbps) for CBR mode.
    /// Ignored when `x264_qp` is Some.
    pub x264_bitrate_kbps: Option<u32>,
}

pub fn load_peer() -> Result<PeerConfig> {
    Ok(PeerConfig {
        my_rtmp_url: env_opt("peer_my_rtmp_url"),
        my_stream_key: env_opt("peer_my_stream_key"),
        their_vk_channel: env_opt("peer_their_vk_channel"),
        their_stream_name: env_opt("peer_their_stream_name"),
        modulation_mode: parse_mode(env_opt("flicker_modulation_mode").as_str())?,
        frag_timeout_ms: env_u64("flicker_frag_timeout_ms")?.unwrap_or(2000),
        rx_warmup_ms: env_u64("peer_rx_warmup_ms")?.unwrap_or(DEFAULT_RX_WARMUP_MS),
        log_every_frame: env_flag("flicker_log_every_frame"),
        flicker_fps: env_u64("peer_flicker_fps")?.unwrap_or(DEFAULT_FPS as u64) as u32,
        stream_width:  env_u64("peer_stream_width")?.unwrap_or(DEFAULT_FRAME_W as u64) as u32,
        stream_height: env_u64("peer_stream_height")?.unwrap_or(DEFAULT_FRAME_H as u64) as u32,
        flicker_cell_size: env_u64("peer_flicker_cell_size")?.unwrap_or(4) as u32,
        x264_qp: env_u64("peer_x264_qp")?.map(|v| v as u32),
        x264_bitrate_kbps: env_u64("peer_x264_bitrate_kbps")?.map(|v| v as u32),
    })
}

pub fn validate_tx(cfg: &PeerConfig) -> Result<()> {
    if cfg.my_rtmp_url.is_empty() { return Err(anyhow!("peer_my_rtmp_url required for tx")); }
    if cfg.my_stream_key.is_empty() { return Err(anyhow!("peer_my_stream_key required for tx")); }
    Ok(())
}

pub fn validate_rx(cfg: &PeerConfig) -> Result<()> {
    if cfg.their_vk_channel.is_empty() { return Err(anyhow!("peer_their_vk_channel required for rx")); }
    if cfg.their_stream_name.is_empty() { return Err(anyhow!("peer_their_stream_name required for rx")); }
    Ok(())
}

fn parse_mode(s: &str) -> Result<ModulationMode> {
    match s.trim().to_ascii_uppercase().as_str() {
        "" | "B" => Ok(ModulationMode::B),
        "C" => Ok(ModulationMode::C),
        other => Err(anyhow!("flicker_modulation_mode: expected B or C, got {other}")),
    }
}

fn env_opt(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| format!("{name}: not u64"))?)),
        Err(_) => Ok(None),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_defaults_to_b() {
        assert_eq!(parse_mode("").unwrap(), ModulationMode::B);
        assert_eq!(parse_mode("B").unwrap(), ModulationMode::B);
        assert_eq!(parse_mode("c").unwrap(), ModulationMode::C);
        assert!(parse_mode("D").is_err());
    }

    #[test]
    fn validate_tx_requires_publish_fields() {
        let mut c = PeerConfig {
            my_rtmp_url: String::new(), my_stream_key: String::new(),
            their_vk_channel: String::new(), their_stream_name: String::new(),
            modulation_mode: ModulationMode::B, frag_timeout_ms: 2000,
            rx_warmup_ms: 0, log_every_frame: false,
            flicker_fps: 24, stream_width: 256, stream_height: 144, flicker_cell_size: 4, x264_qp: None, x264_bitrate_kbps: None,
        };
        assert!(validate_tx(&c).is_err());
        c.my_rtmp_url = "rtmp://x".into();
        assert!(validate_tx(&c).is_err());
        c.my_stream_key = "k".into();
        assert!(validate_tx(&c).is_ok());
    }
}
