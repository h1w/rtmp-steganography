use anyhow::{anyhow, Context, Result};

use crate::flicker::GridConfig;

const DEFAULT_CELL: usize = 16;
const DEFAULT_UPDATE_EVERY: u64 = 5;

pub struct ClientConfig {
    pub rtmp_url: String,
    pub grid: GridConfig,
}

pub struct ServerConfig {
    pub page_url: String,
    pub grid: GridConfig,
    pub log_every_frame: bool,
}

pub fn load_grid() -> Result<GridConfig> {
    let cell = env_usize("cell_size")?.unwrap_or(DEFAULT_CELL);
    let update_every = env_u64("update_every_frames")?.unwrap_or(DEFAULT_UPDATE_EVERY);
    GridConfig::new(cell, update_every)
}

pub fn load_client() -> Result<ClientConfig> {
    let key = std::env::var("client_stream_key")
        .context("client_stream_key not set in .env")?;
    let server = std::env::var("rtmp_server").context("rtmp_server not set in .env")?;
    let rtmp_url = format!("{}/{}", server.trim_end_matches('/'), key.trim());
    Ok(ClientConfig {
        rtmp_url,
        grid: load_grid()?,
    })
}

pub fn load_server() -> Result<ServerConfig> {
    let channel = env_nonempty("vk_live_channel")
        .ok_or_else(|| anyhow!("vk_live_channel not set in .env"))?;
    let name = env_nonempty("client_stream_name")
        .ok_or_else(|| anyhow!("client_stream_name not set in .env"))?;
    let page_url = format!(
        "https://live.vkvideo.ru/{}/stream/{}",
        channel.trim_matches('/'),
        name.trim_matches('/')
    );
    Ok(ServerConfig {
        page_url,
        grid: load_grid()?,
        log_every_frame: env_flag("stream_log_every_frame"),
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_usize(name: &str) -> Result<Option<usize>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| {
            format!("{name} is not a valid unsigned integer: {v:?}")
        })?)),
        Err(_) => Ok(None),
    }
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| {
            format!("{name} is not a valid unsigned integer: {v:?}")
        })?)),
        Err(_) => Ok(None),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
