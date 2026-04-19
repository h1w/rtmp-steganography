//! Events → aggregated summary with SLA matrix.

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
}

#[derive(Default, Debug, Clone)]
pub struct WorkloadStats {
    pub ok: u64,
    pub fail: u64,
    pub latency_ms: Vec<u64>,
    pub bytes: u64,
}

pub fn aggregate(events_path: &Path) -> std::io::Result<Summary> {
    let text = std::fs::read_to_string(events_path)?;
    let mut s = Summary::default();
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
            }
            ("bench", "saturation_result") => {
                let profile = v["profile"].as_str().unwrap_or("unknown").to_string();
                let point = v["saturation_point"].as_u64().map(|x| x as u32);
                s.saturation.insert(profile, point);
            }
            _ => {}
        }
    }
    Ok(s)
}

pub fn write_summary_json(s: &Summary, out: &Path) -> std::io::Result<()> {
    let rtt_p = percentiles(&s.rtt_samples_ms);
    let mut workloads = serde_json::Map::new();
    for (k, ws) in &s.bench_requests {
        let p = percentiles(&ws.latency_ms);
        workloads.insert(k.clone(), json!({
            "ok": ws.ok,
            "fail": ws.fail,
            "bytes": ws.bytes,
            "latency_ms": { "p50": p.p50, "p90": p.p90, "p99": p.p99, "max": p.max }
        }));
    }
    let sla = evaluate_sla(s);
    let j = json!({
        "events_seen": s.events_seen,
        "flicker": { "tx": s.flicker_tx, "rx": s.flicker_rx },
        "kcp": {
            "tx": s.kcp_tx,
            "retx": s.kcp_retx,
            "rtt_ms": { "p50": rtt_p.p50, "p90": rtt_p.p90, "p99": rtt_p.p99, "max": rtt_p.max }
        },
        "bench": { "requests": workloads, "saturation": saturation_json(s) },
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

struct Pctls { p50: u64, p90: u64, p99: u64, max: u64 }

fn percentiles(v: &[u64]) -> Pctls {
    if v.is_empty() { return Pctls { p50: 0, p90: 0, p99: 0, max: 0 }; }
    let mut s = v.to_vec();
    s.sort_unstable();
    let pct = |p: f64| {
        let idx = ((s.len() as f64 - 1.0) * p) as usize;
        s[idx]
    };
    Pctls { p50: pct(0.5), p90: pct(0.9), p99: pct(0.99), max: *s.last().unwrap() }
}

fn evaluate_sla(s: &Summary) -> Value {
    let http = s.bench_requests.get("http_echo");
    let p_all = http.map(|w| percentiles(&w.latency_ms));
    let (p50, p99) = match p_all {
        Some(p) => (p.p50, p.p99),
        None => (u64::MAX, u64::MAX),
    };
    json!({
        "rtt_p50_le_15s": { "pass": p50 <= 15_000, "measured_ms": p50 },
        "rtt_p99_le_30s": { "pass": p99 <= 30_000, "measured_ms": p99 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn aggregates_simple_events() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("events.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, r#"{{"ts_ns":1,"peer_id":"A","layer":"kcp","event":"tx","len":10}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":2,"peer_id":"A","layer":"kcp","event":"retx","seq":1,"attempt":1}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":3,"peer_id":"A","layer":"bench","event":"request_done","workload":"http_echo","id":0,"ok":true,"latency_ms":1234,"bytes":1024}}"#).unwrap();
        drop(f);
        let s = aggregate(&p).unwrap();
        assert_eq!(s.kcp_tx, 1);
        assert_eq!(s.kcp_retx, 1);
        let h = s.bench_requests.get("http_echo").unwrap();
        assert_eq!(h.ok, 1);
        assert_eq!(h.fail, 0);
    }

    #[test]
    fn writes_summary_json_with_sla_matrix() {
        let tmp = tempfile::tempdir().unwrap();
        let events = tmp.path().join("events.jsonl");
        let mut f = std::fs::File::create(&events).unwrap();
        writeln!(f, r#"{{"ts_ns":1,"peer_id":"A","layer":"bench","event":"request_done","workload":"http_echo","id":0,"ok":true,"latency_ms":5000,"bytes":1024}}"#).unwrap();
        writeln!(f, r#"{{"ts_ns":2,"peer_id":"A","layer":"bench","event":"saturation_result","profile":"latency","saturation_point":50}}"#).unwrap();
        drop(f);
        let s = aggregate(&events).unwrap();
        let out = tmp.path().join("summary.json");
        write_summary_json(&s, &out).unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(parsed["sla"]["rtt_p50_le_15s"]["pass"], true);
        assert_eq!(parsed["sla"]["rtt_p50_le_15s"]["measured_ms"], 5000);
        assert_eq!(parsed["bench"]["saturation"]["latency"], 50);
    }

    #[test]
    fn percentiles_small_samples() {
        let p = percentiles(&[10, 20, 30, 40, 50]);
        assert_eq!(p.max, 50);
        assert_eq!(p.p50, 30);
    }

    #[test]
    fn percentiles_empty() {
        let p = percentiles(&[]);
        assert_eq!(p.max, 0);
    }
}
