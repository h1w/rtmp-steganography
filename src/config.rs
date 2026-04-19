use anyhow::{anyhow, Context, Result};

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
        };
        assert!(validate_tx(&c).is_err());
        c.my_rtmp_url = "rtmp://x".into();
        assert!(validate_tx(&c).is_err());
        c.my_stream_key = "k".into();
        assert!(validate_tx(&c).is_ok());
    }
}
