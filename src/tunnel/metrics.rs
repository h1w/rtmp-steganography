//! Structured event emission to JSON-lines + aggregated counters.
//! Pure sync — no tokio or async.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub struct Event {
    layer: &'static str,
    event: &'static str,
    fields: Vec<(&'static str, Value)>,
}

impl Event {
    pub fn new(layer: &'static str, event: &'static str) -> Self {
        Self { layer, event, fields: Vec::new() }
    }

    pub fn field<V: Into<Value>>(mut self, k: &'static str, v: V) -> Self {
        self.fields.push((k, v.into()));
        self
    }
}

pub struct EventEmitter {
    peer_id: String,
    writer: Mutex<BufWriter<File>>,
    dir: PathBuf,
}

impl EventEmitter {
    pub fn new(dir: &Path, peer_id: impl Into<String>) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("events.jsonl");
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            peer_id: peer_id.into(),
            writer: Mutex::new(BufWriter::new(f)),
            dir: dir.to_path_buf(),
        })
    }

    pub fn emit(&self, ev: Event) {
        let ts_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let mut map = serde_json::Map::with_capacity(4 + ev.fields.len());
        map.insert("ts_ns".into(), json!(ts_ns));
        map.insert("peer_id".into(), json!(self.peer_id));
        map.insert("layer".into(), json!(ev.layer));
        map.insert("event".into(), json!(ev.event));
        for (k, v) in ev.fields {
            map.insert(k.to_string(), v);
        }
        let line = serde_json::to_string(&Value::Object(map)).unwrap();
        if let Ok(mut w) = self.writer.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for EventEmitter {
    fn drop(&mut self) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.flush();
        }
    }
}

/// Generate a run_id: Unix timestamp in seconds + 4-char random alphanumeric tag.
pub fn new_run_id() -> String {
    use rand::Rng;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let tag: String = (0..4)
        .map(|_| {
            let c = rand::thread_rng().gen_range(0u8..36);
            if c < 10 { (b'0' + c) as char } else { (b'a' + c - 10) as char }
        })
        .collect();
    format!("{now}-{tag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_jsonl_event_with_required_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let emitter = EventEmitter::new(&dir, "A").unwrap();
        emitter.emit(Event::new("kcp", "retx").field("seq", 42).field("attempt", 2));
        drop(emitter); // flush

        let path = dir.join("events.jsonl");
        let content = std::fs::read_to_string(path).unwrap();
        let line = content.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["peer_id"], "A");
        assert_eq!(v["layer"], "kcp");
        assert_eq!(v["event"], "retx");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["attempt"], 2);
        assert!(v["ts_ns"].as_u64().is_some());
    }
}
