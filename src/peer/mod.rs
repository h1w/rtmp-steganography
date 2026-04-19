pub mod app;
pub mod ffmpeg_publish;
pub mod ffmpeg_read;
pub mod vk_live;

use anyhow::Result;
use crate::config::PeerConfig;

#[derive(Copy, Clone, Debug)]
pub struct Direction {
    pub tx: bool,
    pub rx: bool,
}

pub fn run_peer(_cfg: PeerConfig, _dir: Direction) -> Result<()> {
    anyhow::bail!("peer not implemented yet")
}
