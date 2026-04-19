use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crate::config::PeerConfig;
use crate::peer::Direction;

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker v2 protocol over RTMP")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Mode,
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    Peer(PeerArgs),
}

#[derive(clap::Args, Debug)]
pub struct PeerArgs {
    #[arg(long = "publish-only", conflicts_with = "receive_only")]
    pub publish_only: bool,
    #[arg(long = "receive-only")]
    pub receive_only: bool,
}

impl Cli {
    pub fn resolve(self, _cfg: &PeerConfig) -> Result<Direction> {
        match self.command {
            Mode::Peer(args) => match (args.publish_only, args.receive_only) {
                (false, false) => Ok(Direction { tx: true, rx: true }),
                (true, false) => Ok(Direction { tx: true, rx: false }),
                (false, true) => Ok(Direction { tx: false, rx: true }),
                (true, true) => Err(anyhow!("--publish-only and --receive-only are mutually exclusive")),
            },
        }
    }
}
