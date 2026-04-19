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
    #[arg(long = "tunnel-socks")]
    pub tunnel_socks: Option<std::net::SocketAddr>,
}

pub enum PeerMode {
    Heartbeat(Direction),
    Tunnel { dir: Direction, socks_bind: std::net::SocketAddr },
}

impl Cli {
    pub fn resolve(self, _cfg: &PeerConfig) -> Result<PeerMode> {
        match self.command {
            Mode::Peer(args) => {
                let dir = match (args.publish_only, args.receive_only) {
                    (false, false) => Direction { tx: true, rx: true },
                    (true, false)  => Direction { tx: true, rx: false },
                    (false, true)  => Direction { tx: false, rx: true },
                    (true, true)   => return Err(anyhow!("--publish-only and --receive-only are mutually exclusive")),
                };
                if let Some(addr) = args.tunnel_socks {
                    if !(dir.tx && dir.rx) {
                        return Err(anyhow!("--tunnel-socks requires bidirectional peer (cannot combine with --publish-only or --receive-only)"));
                    }
                    Ok(PeerMode::Tunnel { dir, socks_bind: addr })
                } else {
                    Ok(PeerMode::Heartbeat(dir))
                }
            }
        }
    }
}
