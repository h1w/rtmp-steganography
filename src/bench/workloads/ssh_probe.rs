use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(
    socks_host: &str,
    socks_port: u16,
    target: &str,
    iterations: usize,
    em: Arc<EventEmitter>,
) {
    for i in 0..iterations {
        let start = Instant::now();
        let proxy = format!("nc -X 5 -x {}:{} %h %p", socks_host, socks_port);
        let out = Command::new("ssh")
            .args([
                "-o", &format!("ProxyCommand={}", proxy),
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "ConnectTimeout=30",
                target,
                "echo ok",
            ])
            .output().await;
        let result = match out {
            Ok(o) if o.status.success() => WorkloadResult {
                ok: true,
                latency_ms: start.elapsed().as_millis() as u64,
                bytes: o.stdout.len() as u64,
            },
            _ => WorkloadResult {
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                bytes: 0,
            },
        };
        emit_done(&em, "ssh_probe", i as u64, &result);
    }
}
