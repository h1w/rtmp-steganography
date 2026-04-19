//! Quick live-channel smoke: N × http_echo + one iperf3 step + summary.
//! Intended for fitting a useful run into ~5 minutes on a real VK Live channel.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::bench::saturation;
use crate::bench::workloads::http_echo;
use crate::tunnel::metrics::EventEmitter;

pub struct Config {
    pub socks: SocketAddr,
    pub echo_host: String,
    pub echo_port: u16,
    pub payload_bytes: usize,
    pub iterations: usize,
    pub iperf_host: String,
    pub iperf_port: u16,
    pub iperf_rate_kbps: u32,
    pub iperf_duration_s: u64,
    pub skip_iperf: bool,
}

pub async fn run(cfg: Config, em: Arc<EventEmitter>) {
    eprintln!("[bench/smoke] running {} x http_echo ({} bytes) via SOCKS5 {}",
        cfg.iterations, cfg.payload_bytes, cfg.socks);
    http_echo::run(
        cfg.socks,
        &cfg.echo_host,
        cfg.echo_port,
        cfg.payload_bytes,
        cfg.iterations,
        Arc::clone(&em),
    ).await;

    if !cfg.skip_iperf {
        eprintln!("[bench/smoke] running iperf3 {} kbps x {}s via proxychains",
            cfg.iperf_rate_kbps, cfg.iperf_duration_s);
        let sat_cfg = saturation::Config {
            socks: cfg.socks,
            iperf_host: cfg.iperf_host.clone(),
            iperf_port: cfg.iperf_port,
            profile_label: "latency",
        };
        // Single-step ramp: override ramp with one explicit rate.
        saturation::run_single_step(&sat_cfg, cfg.iperf_rate_kbps, cfg.iperf_duration_s, &em).await;
    }

    eprintln!("[bench/smoke] done emitting events");
}
