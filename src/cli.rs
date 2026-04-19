use anyhow::{anyhow, Result};
use clap::{ArgAction, Parser, Subcommand};

use crate::config::PeerConfig;
use crate::peer::Direction;

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker v2 protocol over RTMP")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Mode>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub peer_flag: bool,
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
    pub fn resolve(self, cfg: &PeerConfig) -> Result<(Direction, PeerConfig)> {
        let dir = match self.command {
            Some(Mode::Peer(args)) => match (args.publish_only, args.receive_only) {
                (false, false) => Direction { tx: true, rx: true },
                (true, false) => Direction { tx: true, rx: false },
                (false, true) => Direction { tx: false, rx: true },
                (true, true) => return Err(anyhow!("--publish-only and --receive-only are mutually exclusive")),
            },
            None if self.peer_flag => Direction { tx: true, rx: true },
            None => return Err(anyhow!("use: rtmp-steganography peer [--publish-only|--receive-only]")),
        };
        Ok((dir, cfg.clone()))
    }
}
