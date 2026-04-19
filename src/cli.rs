use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker protocol over RTMP video")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Mode>,

    /// Alias for `client` subcommand.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "server")]
    pub client: bool,

    /// Alias for `server` subcommand.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "client")]
    pub server: bool,
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    /// Publish RTMP stream with flicker-encoded timestamps.
    Client,
    /// Receive and decode a flicker-encoded stream.
    Server,
}

pub enum Resolved {
    Client,
    Server,
}

impl Cli {
    pub fn resolve(self) -> anyhow::Result<Resolved> {
        match (self.command, self.client, self.server) {
            (Some(Mode::Client), _, _) | (None, true, false) => Ok(Resolved::Client),
            (Some(Mode::Server), _, _) | (None, false, true) => Ok(Resolved::Server),
            (None, false, false) => Err(anyhow::anyhow!(
                "specify a mode: `client` / `server` subcommand or --client / --server"
            )),
            _ => unreachable!("clap conflicts_with prevents both flags"),
        }
    }
}
