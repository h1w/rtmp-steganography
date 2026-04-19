//! Events → aggregated summary with full latency/jitter/goodput breakdown.

use std::collections::BTreeMap;
use std::path::Path;
use serde_json::{json, Value};

#[derive(Default, Debug, Clone)]
pub struct Summary {
    pub events_seen: u64,
    pub flicker_tx: u64,
    pub flicker_rx: u64,
    pub kcp_tx: u64,
    pub kcp_retx: u64,
    pub rtt_samples_ms: Vec<u64>,
    pub bench_requests: BTreeMap<String, WorkloadStats>,
    pub saturation: BTreeMap<String, Option<u32>>,
    pub goodput: Vec<GoodputSample>,
}

#[derive(Default, Debug, Clone)]
pub struct WorkloadStats {
    pub ok: u64,
    pub fail: u64,
    pub bytes: u64,
    pub latency_ms: Vec<u64>,
    pub socks_connect_ms: Vec<u64>,
    pub tunnel_connect_ms: Vec<u64>,
    pub ttfb_ms: Vec<u64>,
    pub total_ms: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct GoodputSample {
    pub profile: String,
    pub target_kbps: u64,
    pub duration_s: u64,
    pub bits_per_second: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub retransmits: u64,
    pub delivery_pct: f64,
}

pub fn aggregate(events_path: &Path) -> std::io::Result<Summary> {
    aggregate_many(&[events_path.to_path_buf()])
}

/// Aggregate events across multiple jsonl files. Missing files are silently
/// skipped (returning an empty contribution) so a caller can pass all peers'
/// events.jsonl + the bench runner's events.jsonl without knowing which exist.
pub fn aggregate_many(paths: &[std::path::PathBuf]) -> std::io::Result<Summary> {
    let mut s = Summary::default();
    for p in paths {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(_) => continue,
        };
        ingest_text(&text, &mut s);
    }
    Ok(s)
}

/// Collect all events.jsonl files under `base_dir` (one level deep — i.e.
/// `base_dir/<run_id>/events.jsonl`). Useful for merging peer-A, peer-B and
/// bench-runner events from the same bench invocation.
pub fn collect_events_in_dir(base_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(base_dir) else { return out; };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let ev = p.join("events.jsonl");
            if ev.exists() {
                out.push(ev);
            }
        }
    }
    out
}

fn ingest_text(text: &str, s: &mut Summary) {
    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) { Ok(v) => v, _ => continue };
        s.events_seen += 1;
        let layer = v["layer"].as_str().unwrap_or("");
        let event = v["event"].as_str().unwrap_or("");
        match (layer, event) {
            ("flicker", "tx") => s.flicker_tx += 1,
            ("flicker", "rx") => s.flicker_rx += 1,
            ("kcp", "tx") => s.kcp_tx += 1,
            ("kcp", "retx") => s.kcp_retx += 1,
            ("kcp", "rtt_sample") => {
                if let Some(n) = v["rtt_ms"].as_u64() {
                    s.rtt_samples_ms.push(n);
                }
            }
            ("bench", "request_done") => {
                let name = v["workload"].as_str().unwrap_or("").to_string();
                let stats = s.bench_requests.entry(name).or_default();
                if v["ok"].as_bool() == Some(true) { stats.ok += 1 } else { stats.fail += 1 }
                if let Some(l) = v["latency_ms"].as_u64() { stats.latency_ms.push(l); }
                if let Some(b) = v["bytes"].as_u64() { stats.bytes += b; }
                if let Some(x) = v["socks_connect_ms"].as_u64() { stats.socks_connect_ms.push(x); }
                if let Some(x) = v["tunnel_connect_ms"].as_u64() { stats.tunnel_connect_ms.push(x); }
                if let Some(x) = v["ttfb_ms"].as_u64() { stats.ttfb_ms.push(x); }
                if let Some(x) = v["total_ms"].as_u64() { stats.total_ms.push(x); }
            }
            ("bench", "goodput") => {
                s.goodput.push(GoodputSample {
                    profile: v["profile"].as_str().unwrap_or("").to_string(),
                    target_kbps: v["target_kbps"].as_u64().unwrap_or(0),
                    duration_s: v["duration_s"].as_u64().unwrap_or(0),
                    bits_per_second: v["bits_per_second"].as_u64().unwrap_or(0),
                    bytes_sent: v["bytes_sent"].as_u64().unwrap_or(0),
                    bytes_received: v["bytes_received"].as_u64().unwrap_or(0),
                    retransmits: v["retransmits"].as_u64().unwrap_or(0),
                    delivery_pct: v["delivery_pct"].as_f64().unwrap_or(0.0),
                });
            }
            ("bench", "saturation_result") => {
                let profile = v["profile"].as_str().unwrap_or("unknown").to_string();
                let point = v["saturation_point"].as_u64().map(|x| x as u32);
                s.saturation.insert(profile, point);
            }
            _ => {}
        }
    }
}

pub fn write_summary_json(s: &Summary, out: &Path) -> std::io::Result<()> {
    let rtt_p = stats_block(&s.rtt_samples_ms);
    let mut workloads = serde_json::Map::new();
    for (k, ws) in &s.bench_requests {
        workloads.insert(k.clone(), json!({
            "ok": ws.ok,
            "fail": ws.fail,
            "bytes_total": ws.bytes,
            "ping_ms":            stats_block(&ws.tunnel_connect_ms),
            "ttfb_ms":            stats_block(&ws.ttfb_ms),
            "total_ms":           stats_block(&ws.total_ms),
            "socks_connect_ms":   stats_block(&ws.socks_connect_ms),
            "latency_ms_legacy":  stats_block(&ws.latency_ms),
        }));
    }

    let mut goodput_arr = Vec::new();
    for g in &s.goodput {
        goodput_arr.push(json!({
            "profile": g.profile,
            "target_kbps": g.target_kbps,
            "duration_s": g.duration_s,
            "bits_per_second": g.bits_per_second,
            "kbits_per_second": (g.bits_per_second as f64 / 1000.0 * 100.0).round() / 100.0,
            "bytes_sent": g.bytes_sent,
            "bytes_received": g.bytes_received,
            "retransmits": g.retransmits,
            "delivery_pct": g.delivery_pct,
        }));
    }

    let sla = evaluate_sla(s);

    let flicker_delivery = if s.flicker_tx > 0 {
        (s.flicker_rx as f64 / s.flicker_tx as f64) * 100.0
    } else { 0.0 };

    let j = json!({
        "events_seen": s.events_seen,
        "flicker": {
            "tx": s.flicker_tx,
            "rx": s.flicker_rx,
            "delivery_pct_est": (flicker_delivery * 100.0).round() / 100.0
        },
        "kcp": {
            "tx": s.kcp_tx,
            "retx": s.kcp_retx,
            "retx_rate_pct": if s.kcp_tx > 0 {
                ((s.kcp_retx as f64 / s.kcp_tx as f64) * 10000.0).round() / 100.0
            } else { 0.0 },
            "rtt_ms": rtt_p
        },
        "bench": {
            "requests": workloads,
            "goodput": goodput_arr,
            "saturation_point_kbps": saturation_json(s)
        },
        "sla": sla,
    });
    std::fs::write(out, serde_json::to_string_pretty(&j).unwrap())?;
    Ok(())
}

fn saturation_json(s: &Summary) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in &s.saturation {
        m.insert(k.clone(), match v { Some(kbps) => json!(kbps), None => Value::Null });
    }
    Value::Object(m)
}

fn stats_block(v: &[u64]) -> Value {
    if v.is_empty() {
        return json!({ "count": 0 });
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let pct = |p: f64| {
        let idx = ((s.len() as f64 - 1.0) * p) as usize;
        s[idx]
    };
    let mean = s.iter().sum::<u64>() as f64 / s.len() as f64;
    let variance = s.iter().map(|x| {
        let d = *x as f64 - mean;
        d * d
    }).sum::<f64>() / s.len() as f64;
    let stddev = variance.sqrt();
    json!({
        "count": s.len(),
        "min": s[0],
        "p50": pct(0.5),
        "p90": pct(0.9),
        "p99": pct(0.99),
        "max": *s.last().unwrap(),
        "mean_ms": (mean * 100.0).round() / 100.0,
        "jitter_ms": (stddev * 100.0).round() / 100.0,
    })
}

fn evaluate_sla(s: &Summary) -> Value {
    let http = s.bench_requests.get("http_echo");
    let (p50, p99) = http.map(|w| {
        let mut v = w.total_ms.clone();
        if v.is_empty() { return (u64::MAX, u64::MAX); }
        v.sort_unstable();
        let idx = |p: f64| v[((v.len() as f64 - 1.0) * p) as usize];
        (idx(0.5), idx(0.99))
    }).unwrap_or((u64::MAX, u64::MAX));

    let peak_bps = s.goodput.iter().map(|g| g.bits_per_second).max().unwrap_or(0);
    let stable_bps = s.goodput.iter()
        .filter(|g| g.delivery_pct >= 95.0)
        .map(|g| g.bits_per_second).max().unwrap_or(0);

    let (tot_sent, tot_recv) = s.goodput.iter()
        .fold((0u64, 0u64), |(a, b), g| (a + g.bytes_sent, b + g.bytes_received));
    let reliability_pct = if tot_sent > 0 {
        (tot_recv as f64 / tot_sent as f64) * 100.0
    } else { 0.0 };

    json!({
        "rtt_p50_le_15s":   { "pass": p50 <= 15_000, "measured_ms": p50 },
        "rtt_p99_le_30s":   { "pass": p99 <= 30_000, "measured_ms": p99 },
        "goodput_stable_ge_20kbps": { "pass": stable_bps >= 20_000, "measured_bps": stable_bps },
        "goodput_peak_ge_50kbps":   { "pass": peak_bps   >= 50_000, "measured_bps": peak_bps },
        "tunnel_reliability_ge_99_9": {
            "pass": reliability_pct >= 99.9,
            "measured_pct": (reliability_pct * 100.0).round() / 100.0,
            "bytes_sent": tot_sent,
            "bytes_received": tot_recv
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn aggregates_rich_http_echo_event() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("events.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, r#"{{"ts_ns":1,"peer_id":"A","layer":"bench","event":"request_done","workload":"http_echo","id":0,"ok":true,"socks_connect_ms":1,"tunnel_connect_ms":9000,"ttfb_ms":18000,"total_ms":22000,"latency_ms":22000,"bytes":1024}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":2,"peer_id":"A","layer":"bench","event":"request_done","workload":"http_echo","id":1,"ok":true,"socks_connect_ms":1,"tunnel_connect_ms":11000,"ttfb_ms":20000,"total_ms":23500,"latency_ms":23500,"bytes":1024}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":3,"peer_id":"A","layer":"bench","event":"goodput","profile":"latency","target_kbps":20,"duration_s":30,"bits_per_second":34936,"bytes_sent":131072,"bytes_received":131072,"retransmits":0,"delivery_pct":100.0}}"#).unwrap();
        drop(f);
        let s = aggregate(&p).unwrap();
        assert_eq!(s.bench_requests.get("http_echo").unwrap().total_ms, vec![22000, 23500]);
        assert_eq!(s.goodput.len(), 1);
        assert_eq!(s.goodput[0].bits_per_second, 34936);
    }

    #[test]
    fn writes_rich_summary_and_sla() {
        let tmp = tempfile::tempdir().unwrap();
        let events = tmp.path().join("events.jsonl");
        let mut f = std::fs::File::create(&events).unwrap();
        writeln!(f, r#"{{"ts_ns":1,"peer_id":"A","layer":"bench","event":"request_done","workload":"http_echo","id":0,"ok":true,"tunnel_connect_ms":10000,"ttfb_ms":18000,"total_ms":22000,"latency_ms":22000,"bytes":1024}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":2,"peer_id":"A","layer":"bench","event":"goodput","profile":"latency","target_kbps":20,"duration_s":30,"bits_per_second":34000,"bytes_sent":131072,"bytes_received":131072,"retransmits":0,"delivery_pct":100.0}}"#).unwrap();
        drop(f);
        let s = aggregate(&events).unwrap();
        let out = tmp.path().join("summary.json");
        write_summary_json(&s, &out).unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(parsed["sla"]["goodput_stable_ge_20kbps"]["pass"], true);
        assert_eq!(parsed["sla"]["tunnel_reliability_ge_99_9"]["pass"], true);
        assert!(parsed["bench"]["requests"]["http_echo"]["ping_ms"]["p50"].is_number());
        assert!(parsed["bench"]["requests"]["http_echo"]["total_ms"]["jitter_ms"].is_number());
    }

    #[test]
    fn stats_block_empty() {
        let b = stats_block(&[]);
        assert_eq!(b["count"], 0);
    }

    #[test]
    fn stats_block_nontrivial() {
        let b = stats_block(&[10, 20, 30, 40, 50]);
        assert_eq!(b["count"], 5);
        assert_eq!(b["p50"], 30);
        assert_eq!(b["max"], 50);
        assert!(b["jitter_ms"].as_f64().unwrap() > 0.0);
    }
}
