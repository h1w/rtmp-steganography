//! Bench 2 — saturation ramp with retx heuristic.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use crate::tunnel::metrics::{Event, EventEmitter};

#[derive(Debug, Clone)]
pub struct Config {
    pub socks: SocketAddr,
    pub iperf_host: String,
    pub iperf_port: u16,
    pub profile_label: &'static str,
}

const RAMP_KBPS: &[u32] = &[10, 20, 50, 100, 200];
const STEP_SECS: u64 = 30;
const HOLD_SECS: u64 = 60;

pub async fn run(cfg: Config, em: Arc<EventEmitter>) {
    // Warm-up
    tokio::time::sleep(Duration::from_secs(10)).await;

    let mut saturation_kbps: Option<u32> = None;
    for &rate in RAMP_KBPS {
        em.emit(Event::new("bench", "saturation_step")
            .field("profile", cfg.profile_label)
            .field("rate_kbps", rate as i64));
        let saturated = run_iperf(&cfg, rate, STEP_SECS, &em).await;
        if saturated {
            saturation_kbps = Some(rate);
            em.emit(Event::new("bench", "saturation_hold")
                .field("profile", cfg.profile_label)
                .field("rate_kbps", rate as i64));
            run_iperf(&cfg, rate, HOLD_SECS, &em).await;
            break;
        }
    }

    match saturation_kbps {
        Some(kbps) => em.emit(Event::new("bench", "saturation_result")
            .field("profile", cfg.profile_label)
            .field("saturation_point", kbps as i64)),
        None => em.emit(Event::new("bench", "saturation_result")
            .field("profile", cfg.profile_label)
            .field("saturation_point", serde_json::Value::Null)
            .field("reason", "ramp_ceiling_reached")),
    }
}

/// Run a single iperf3 ramp step without the full saturation logic.
/// Emits the same `bench.goodput` / `bench.iperf3` events. Useful for `bench smoke`.
pub async fn run_single_step(cfg: &Config, rate_kbps: u32, secs: u64, em: &Arc<EventEmitter>) {
    em.emit(Event::new("bench", "saturation_step")
        .field("profile", cfg.profile_label)
        .field("rate_kbps", rate_kbps as i64));
    let _ = run_iperf(cfg, rate_kbps, secs, em).await;
}

async fn run_iperf(cfg: &Config, rate_kbps: u32, secs: u64, em: &Arc<EventEmitter>) -> bool {
    // iperf3 has no native SOCKS5 — wrap with proxychains on $PATH.
    // Note: proxychains-windows uses the name `proxychains` (the Linux build
    // ships as `proxychains4`; override via PROXYCHAINS_BIN env if needed).
    let bin = std::env::var("PROXYCHAINS_BIN").unwrap_or_else(|_| "proxychains".to_string());
    let out = Command::new(&bin)
        .args([
            "-q",
            "iperf3",
            "-c", &cfg.iperf_host,
            "-p", &cfg.iperf_port.to_string(),
            "-t", &secs.to_string(),
            "-b", &format!("{}k", rate_kbps),
            "-J",
        ])
        .output().await;

    match out {
        Ok(o) if o.status.success() => {
            let (bps, bytes_sent, bytes_recv, retr) = parse_iperf_summary(&o.stdout);
            let delivery_pct = if bytes_sent > 0 {
                (bytes_recv as f64 / bytes_sent as f64) * 100.0
            } else { 0.0 };
            em.emit(Event::new("bench", "goodput")
                .field("profile", cfg.profile_label)
                .field("target_kbps", rate_kbps as i64)
                .field("duration_s", secs as i64)
                .field("bits_per_second", bps as i64)
                .field("bytes_sent", bytes_sent as i64)
                .field("bytes_received", bytes_recv as i64)
                .field("retransmits", retr as i64)
                .field("delivery_pct", (delivery_pct * 100.0).round() / 100.0));
            em.emit(Event::new("bench", "iperf3")
                .field("rate_kbps", rate_kbps as i64)
                .field("duration_s", secs as i64)
                .field("ok", true)
                .field("json_len", o.stdout.len() as i64));
            retx_predicate(&o.stdout)
        }
        Ok(o) => {
            em.emit(Event::new("bench", "iperf3")
                .field("rate_kbps", rate_kbps as i64)
                .field("ok", false)
                .field("exit_code", o.status.code().unwrap_or(-1) as i64));
            true
        }
        Err(e) => {
            em.emit(Event::new("bench", "iperf3")
                .field("rate_kbps", rate_kbps as i64)
                .field("ok", false)
                .field("err", e.to_string()));
            true
        }
    }
}

/// Heuristic: >2 retransmits per MB sent is treated as saturation.
/// Parses iperf3 --json output (key: end.sum_sent).
/// Parse iperf3 --json output and return (bits_per_second, bytes_sent, bytes_received, retransmits).
/// Returns zeros on parse failure — caller should treat as untrusted result.
fn parse_iperf_summary(json_bytes: &[u8]) -> (u64, u64, u64, u64) {
    let v: serde_json::Value = match serde_json::from_slice(json_bytes) {
        Ok(v) => v,
        _ => return (0, 0, 0, 0),
    };
    let sent = v.pointer("/end/sum_sent");
    let recv = v.pointer("/end/sum_received");
    let bps = sent.and_then(|s| s.get("bits_per_second")).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let bytes_sent = sent.and_then(|s| s.get("bytes")).and_then(|x| x.as_u64()).unwrap_or(0);
    let bytes_recv = recv.and_then(|r| r.get("bytes")).and_then(|x| x.as_u64()).unwrap_or(0);
    let retr = sent.and_then(|s| s.get("retransmits")).and_then(|x| x.as_u64()).unwrap_or(0);
    (bps as u64, bytes_sent, bytes_recv, retr)
}

fn retx_predicate(json_bytes: &[u8]) -> bool {
    let v: serde_json::Value = match serde_json::from_slice(json_bytes) {
        Ok(v) => v,
        _ => return false,
    };
    let Some(sum) = v.pointer("/end/sum_sent") else { return false; };
    let retr = sum.get("retransmits").and_then(|x| x.as_u64()).unwrap_or(0);
    let sent_b = sum.get("bytes").and_then(|x| x.as_u64()).unwrap_or(1);
    let mb = (sent_b / 1_000_000).max(1);
    (retr / mb) > 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retx_predicate_fires_above_threshold() {
        let json = br#"{"end":{"sum_sent":{"bytes":1000000,"retransmits":3}}}"#;
        assert!(retx_predicate(json));
    }

    #[test]
    fn retx_predicate_holds_below_threshold() {
        let json = br#"{"end":{"sum_sent":{"bytes":1000000,"retransmits":1}}}"#;
        assert!(!retx_predicate(json));
    }

    #[test]
    fn retx_predicate_tolerates_malformed_json() {
        assert!(!retx_predicate(b"not json"));
    }
}
