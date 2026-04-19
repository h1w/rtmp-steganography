//! Bench 1 — realistic mixed workload.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::bench::support;
use crate::bench::workloads::{dns, http_echo, https_fetch, ssh_probe};
use crate::tunnel::metrics::EventEmitter;

#[derive(Debug, Clone)]
pub struct Config {
    pub socks: SocketAddr,
    pub echo_host: String,
    pub echo_port: u16,
    pub dns_host: String,
    pub dns_port: u16,
    pub ssh_target: Option<String>,
    pub fixtures_dir: PathBuf,
}

pub async fn run(cfg: Config, em: Arc<EventEmitter>) {
    let running = Arc::new(AtomicBool::new(true));

    // Bring up support services on peer B's loopback.
    let echo_bind: SocketAddr = format!("{}:{}", cfg.echo_host, cfg.echo_port)
        .parse()
        .expect("echo bind addr");
    let dns_bind: SocketAddr = format!("{}:{}", cfg.dns_host, cfg.dns_port)
        .parse()
        .expect("dns bind addr");
    if let Err(e) = support::spawn_http_echo(echo_bind, Arc::clone(&running)).await {
        eprintln!("[bench/realistic] failed to bind http_echo @ {echo_bind}: {e}");
    }
    if let Err(e) = support::spawn_tcp_dns(dns_bind, &cfg.fixtures_dir.join("dns.txt"), Arc::clone(&running)).await {
        eprintln!("[bench/realistic] failed to bind tcp_dns @ {dns_bind}: {e}");
    }

    // 1. Warm-up
    tokio::time::sleep(Duration::from_secs(10)).await;

    // 2. HTTP echo — 3 payload sizes × 10 iterations each
    for bytes in [1024, 10 * 1024, 100 * 1024] {
        http_echo::run(cfg.socks, &cfg.echo_host, cfg.echo_port, bytes, 10, Arc::clone(&em)).await;
    }

    // 3. DNS — up to 50 names from fixture
    let names: Vec<String> = std::fs::read_to_string(cfg.fixtures_dir.join("dns.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    let slice_end = names.len().min(50);
    dns::run(cfg.socks, &cfg.dns_host, cfg.dns_port, &names[..slice_end], Arc::clone(&em)).await;

    // 4. SSH probe (optional)
    if let Some(t) = cfg.ssh_target.as_deref() {
        let host = cfg.socks.ip().to_string();
        ssh_probe::run(&host, cfg.socks.port(), t, 5, Arc::clone(&em)).await;
    }

    // 5. HTTPS — real internet-reachable URLs
    https_fetch::run(
        cfg.socks,
        &["https://example.com", "https://en.wikipedia.org/wiki/Main_Page"],
        5,
        Arc::clone(&em),
    ).await;

    // 6. Idle hold — 10 minutes for SLA row 6 (idle survivability)
    tokio::time::sleep(Duration::from_secs(600)).await;

    running.store(false, Ordering::SeqCst);
}
