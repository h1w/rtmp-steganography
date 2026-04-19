use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::{Cli, Resolved};
use rtmp_steganography::{client, config, server};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    match cli.resolve()? {
        Resolved::Client => {
            let cfg = config::load_client()?;
            client::run(cfg)
        }
        Resolved::Server => {
            let cfg = config::load_server()?;
            server::run(cfg)
        }
    }
}
