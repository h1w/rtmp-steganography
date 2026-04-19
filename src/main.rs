use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::Cli;
use rtmp_steganography::{config, peer};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let (direction, cfg) = cli.resolve(&config::load_peer()?)?;
    peer::run_peer(cfg, direction)
}
