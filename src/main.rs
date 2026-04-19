use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::{Cli, PeerMode};
use rtmp_steganography::{config, peer};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let cfg = config::load_peer()?;
    match cli.resolve(&cfg)? {
        PeerMode::Heartbeat(dir) => peer::run_peer(cfg, dir),
        PeerMode::Tunnel { dir, socks_bind } => peer::run_peer_tunnel(cfg, dir, socks_bind),
    }
}
