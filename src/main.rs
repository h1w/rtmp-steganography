use std::sync::Arc;
use anyhow::Result;
use clap::Parser;

use rtmp_steganography::bench;
use rtmp_steganography::cli::{BenchCmd, Cli, PeerMode, ProfileArg, ReportCmd, Resolved};
use rtmp_steganography::tunnel::metrics::{new_run_id, EventEmitter};
use rtmp_steganography::{config, peer};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    match cli.resolve()? {
        Resolved::Peer(mode) => {
            let cfg = config::load_peer()?;
            match mode {
                PeerMode::Heartbeat(dir) => peer::run_peer(cfg, dir),
                PeerMode::Tunnel { dir, socks_bind, with_bench_support } =>
                    peer::run_peer_tunnel(cfg, dir, socks_bind, with_bench_support),
            }
        }
        Resolved::Bench(cmd) => run_bench(cmd),
        Resolved::Report(cmd) => run_report(cmd),
    }
}

fn run_bench(cmd: BenchCmd) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        match cmd {
            BenchCmd::Smoke {
                socks, echo_host, echo_port, payload_bytes, iterations,
                iperf_host, iperf_port, iperf_rate_kbps, iperf_duration_s,
                metrics_dir, skip_iperf,
                throughput_bytes, raw_echo_host, raw_echo_port,
            } => {
                let run_id = new_run_id();
                let dir = metrics_dir.join(&run_id);
                let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
                let em = Arc::new(EventEmitter::new(&dir, peer_id)?);
                bench::smoke::run(
                    bench::smoke::Config {
                        socks, echo_host, echo_port, payload_bytes, iterations,
                        iperf_host, iperf_port, iperf_rate_kbps, iperf_duration_s,
                        skip_iperf,
                        throughput_bytes, raw_echo_host, raw_echo_port,
                    },
                    Arc::clone(&em),
                ).await;
                // Flush by dropping the emitter before reading events back.
                drop(em);
                // Merge: bench-runner events + all peer events under metrics_dir
                // (each peer writes into its own <run_id>/events.jsonl).
                let mut paths = vec![dir.join("events.jsonl")];
                paths.extend(bench::report::collect_events_in_dir(&metrics_dir));
                paths.sort();
                paths.dedup();
                let s = bench::report::aggregate_many(&paths)?;
                bench::report::write_summary_json(&s, &dir.join("summary.json"))?;
                let summary = std::fs::read_to_string(dir.join("summary.json"))?;
                println!("\n===== BENCH SMOKE SUMMARY =====");
                println!("metrics dir: {}", dir.display());
                println!("merged events from:");
                for p in &paths { println!("  - {}", p.display()); }
                println!("{}", summary);
                Ok::<(), anyhow::Error>(())
            }
            BenchCmd::Realistic {
                socks, echo_host, echo_port, dns_host, dns_port, ssh_target, fixtures_dir, metrics_dir,
            } => {
                let run_id = new_run_id();
                let dir = metrics_dir.join(&run_id);
                let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
                let em = Arc::new(EventEmitter::new(&dir, peer_id)?);
                bench::realistic::run(
                    bench::realistic::Config {
                        socks, echo_host, echo_port, dns_host, dns_port, ssh_target, fixtures_dir,
                    },
                    Arc::clone(&em),
                ).await;
                let s = bench::report::aggregate(&dir.join("events.jsonl"))?;
                bench::report::write_summary_json(&s, &dir.join("summary.json"))?;
                Ok::<(), anyhow::Error>(())
            }
            BenchCmd::Saturation {
                socks, iperf_host, iperf_port, profile, metrics_dir,
            } => {
                let run_id = new_run_id();
                let dir = metrics_dir.join(&run_id);
                let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
                let em = Arc::new(EventEmitter::new(&dir, peer_id)?);
                let labels: &[&'static str] = match profile {
                    ProfileArg::Throughput => &["throughput"],
                    ProfileArg::Latency    => &["latency"],
                    ProfileArg::Both       => &["throughput", "latency"],
                };
                for label in labels {
                    bench::saturation::run(
                        bench::saturation::Config {
                            socks,
                            iperf_host: iperf_host.clone(),
                            iperf_port,
                            profile_label: label,
                        },
                        Arc::clone(&em),
                    ).await;
                }
                let s = bench::report::aggregate(&dir.join("events.jsonl"))?;
                bench::report::write_summary_json(&s, &dir.join("summary.json"))?;
                Ok::<(), anyhow::Error>(())
            }
        }
    })
}

fn run_report(cmd: ReportCmd) -> Result<()> {
    match cmd {
        ReportCmd::Summarize { events, out, metrics_dir } => {
            let mut paths: Vec<std::path::PathBuf> = events;
            if let Some(dir) = metrics_dir.as_ref() {
                paths.extend(bench::report::collect_events_in_dir(dir));
            }
            if paths.is_empty() {
                return Err(anyhow::anyhow!("report summarize: no events files — pass paths or --metrics-dir"));
            }
            paths.sort();
            paths.dedup();
            let s = bench::report::aggregate_many(&paths)?;
            bench::report::write_summary_json(&s, &out)?;
            println!("[report] merged {} event file(s) -> {}", paths.len(), out.display());
            for p in &paths { println!("  - {}", p.display()); }
            Ok(())
        }
    }
}
