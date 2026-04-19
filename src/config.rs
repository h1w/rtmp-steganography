use anyhow::{anyhow, Context, Result};

use crate::flicker::GridConfig;

const DEFAULT_CELL: usize = 16;
const DEFAULT_UPDATE_EVERY: u64 = 5;

pub struct ClientConfig {
    pub rtmp_url: String,
    pub grid: GridConfig,
}

pub struct ServerConfig {
    pub grid: GridConfig,
    pub source: SourceConfig,
    pub http: HttpConfig,
    pub log_every_frame: bool,
}

pub enum SourceConfig {
    DirectUrl(String),
    VkLiveSlug(String),
}

#[derive(Clone, Default)]
pub struct HttpConfig {
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub origin: Option<String>,
}

pub fn load_grid() -> Result<GridConfig> {
    let cell = env_usize("cell_size")?.unwrap_or(DEFAULT_CELL);
    let update_every = env_u64("update_every_frames")?.unwrap_or(DEFAULT_UPDATE_EVERY);
    GridConfig::new(cell, update_every)
}

pub fn load_client() -> Result<ClientConfig> {
    let key = std::env::var("stream_key").context("stream_key not set in .env")?;
    let server = std::env::var("rtmp_server").context("rtmp_server not set in .env")?;
    let rtmp_url = format!("{}/{}", server.trim_end_matches('/'), key);
    Ok(ClientConfig {
        rtmp_url,
        grid: load_grid()?,
    })
}

pub fn load_server() -> Result<ServerConfig> {
    let grid = load_grid()?;
    let source = if let Some(u) = env_nonempty("stream_read_url") {
        SourceConfig::DirectUrl(u)
    } else if let Some(slug) = env_nonempty("vk_live_channel") {
        SourceConfig::VkLiveSlug(slug)
    } else {
        return Err(anyhow!(
            "set stream_read_url or vk_live_channel in .env"
        ));
    };
    let http = HttpConfig {
        user_agent: env_nonempty("stream_user_agent"),
        referer: env_nonempty("stream_referer"),
        origin: env_nonempty("stream_origin"),
    };
    let log_every_frame = env_flag("stream_log_every_frame");
    Ok(ServerConfig {
        grid,
        source,
        http,
        log_every_frame,
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
