use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(socks: SocketAddr, urls: &[&str], iterations: usize, em: Arc<EventEmitter>) {
    for i in 0..iterations {
        for url in urls {
            let start = Instant::now();
            let res = Command::new("curl")
                .args([
                    "--silent", "--show-error",
                    "--max-time", "60",
                    "--socks5", &format!("{}", socks),
                    "-o", if cfg!(windows) { "NUL" } else { "/dev/null" },
                    "-w", "%{size_download}",
                    url,
                ])
                .output().await;
            let result = match res {
                Ok(o) if o.status.success() => {
                    let bytes: u64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
                    WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes }
                }
                _ => WorkloadResult {
                    ok: false,
                    latency_ms: start.elapsed().as_millis() as u64,
                    bytes: 0,
                },
            };
            emit_done(&em, "https_fetch", i as u64, &result);
        }
    }
}
