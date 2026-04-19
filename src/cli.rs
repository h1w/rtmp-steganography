use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

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
    /// Run bench workloads (realistic or saturation)
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Aggregate an events.jsonl into summary.json
    Report {
        #[command(subcommand)]
        cmd: ReportCmd,
    },
}

#[derive(clap::Args, Debug)]
pub struct PeerArgs {
    #[arg(long = "publish-only", conflicts_with = "receive_only")]
    pub publish_only: bool,
    #[arg(long = "receive-only")]
    pub receive_only: bool,
    #[arg(long = "tunnel-socks")]
    pub tunnel_socks: Option<std::net::SocketAddr>,
    /// When in tunnel mode, also bring up embedded http_echo (18080) and
    /// tcp_dns (18053) listeners on 127.0.0.1 so remote bench drivers can
    /// reach them via tunnel egress.
    #[arg(long = "with-bench-support")]
    pub with_bench_support: bool,
}

#[derive(clap::Subcommand, Debug)]
pub enum BenchCmd {
    /// Quick live-channel smoke: N x http_echo + one iperf3 ramp step + summary.
    /// Designed to fit in ~5 minutes on a real VK Live channel.
    Smoke {
        #[arg(long, default_value = "127.0.0.1:1080")]  socks: std::net::SocketAddr,
        #[arg(long, default_value = "127.0.0.1")]        echo_host: String,
        #[arg(long, default_value_t = 18080)]            echo_port: u16,
        #[arg(long, default_value_t = 1024)]             payload_bytes: usize,
        #[arg(long, default_value_t = 10)]               iterations: usize,
        #[arg(long, default_value = "127.0.0.1")]        iperf_host: String,
        #[arg(long, default_value_t = 15201)]            iperf_port: u16,
        #[arg(long, default_value_t = 20)]               iperf_rate_kbps: u32,
        #[arg(long, default_value_t = 30)]               iperf_duration_s: u64,
        #[arg(long, default_value = "./metrics")]        metrics_dir: std::path::PathBuf,
        #[arg(long)]                                     skip_iperf: bool,
    },
    Realistic {
        #[arg(long, default_value = "127.0.0.1:1080")]  socks: std::net::SocketAddr,
        #[arg(long, default_value = "127.0.0.1")]        echo_host: String,
        #[arg(long, default_value_t = 18080)]            echo_port: u16,
        #[arg(long, default_value = "127.0.0.1")]        dns_host: String,
        #[arg(long, default_value_t = 18053)]            dns_port: u16,
        #[arg(long)]                                     ssh_target: Option<String>,
        #[arg(long, default_value = "./fixtures")]       fixtures_dir: std::path::PathBuf,
        #[arg(long, default_value = "./metrics")]        metrics_dir: std::path::PathBuf,
    },
    Saturation {
        #[arg(long, default_value = "127.0.0.1:1080")]  socks: std::net::SocketAddr,
        #[arg(long, default_value = "127.0.0.1")]        iperf_host: String,
        #[arg(long, default_value_t = 15201)]            iperf_port: u16,
        #[arg(long, value_enum, default_value_t = ProfileArg::Both)] profile: ProfileArg,
        #[arg(long, default_value = "./metrics")]        metrics_dir: std::path::PathBuf,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ProfileArg { Throughput, Latency, Both }

#[derive(clap::Subcommand, Debug)]
pub enum ReportCmd {
    Summarize {
        events: std::path::PathBuf,
        #[arg(long)] out: std::path::PathBuf,
    },
}

pub enum PeerMode {
    Heartbeat(Direction),
    Tunnel { dir: Direction, socks_bind: std::net::SocketAddr, with_bench_support: bool },
}

pub enum Resolved {
    Peer(PeerMode),
    Bench(BenchCmd),
    Report(ReportCmd),
}

impl Cli {
    pub fn resolve(self) -> Result<Resolved> {
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
                    Ok(Resolved::Peer(PeerMode::Tunnel {
                        dir,
                        socks_bind: addr,
                        with_bench_support: args.with_bench_support,
                    }))
                } else {
                    if args.with_bench_support {
                        return Err(anyhow!("--with-bench-support requires --tunnel-socks"));
                    }
                    Ok(Resolved::Peer(PeerMode::Heartbeat(dir)))
                }
            }
            Mode::Bench { cmd } => Ok(Resolved::Bench(cmd)),
            Mode::Report { cmd } => Ok(Resolved::Report(cmd)),
        }
    }
}
