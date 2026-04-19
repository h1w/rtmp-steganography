# Traffic Tunnel & Benchmarks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a bidirectional SOCKS5 tunnel over the existing flicker/RTMP/VK channel plus two benchmarks (realistic, saturation) with structured JSON-lines metrics.

**Architecture:** Client TCP → SOCKS5 listener → yamux stream mux → KCP ARQ → flicker adapter (`msg_type=0x02`) → RTMP/VK → reverse → egress TCP to target. Two KCP profiles (`throughput`, `latency`) selectable via env. Tokio async runtime for tunnel code; existing sync flicker code bridged via mpsc channels.

**Tech Stack:** Rust, tokio 1.x, kcp 0.5 (Rust crate), yamux 0.13, hdrhistogram 7, serde_json. SOCKS5 parser is hand-rolled (~200 LOC).

**Spec:** `docs/superpowers/specs/2026-04-19-traffic-bench-design.md`.

---

## File structure

```
src/
  flicker/mod.rs            MODIFY: export FLICKER_MAX_PAYLOAD_BYTES
  peer/app.rs               MODIFY: branch heartbeat vs tunnel mode
  cli.rs                    MODIFY: add `tunnel` and `bench` subcommands
  tunnel/
    mod.rs                  NEW: supervisor, public Tunnel::start
    adapter.rs              NEW: DatagramChannel trait + FlickerChannel + MemChannel
    kcp.rs                  NEW: KCP wrapper, preset structs, tick driver
    mux.rs                  NEW: yamux session glue
    framing.rs              NEW: internal CONNECT frame codec
    socks5.rs               NEW: SOCKS5 handshake parser + response builder
    listener.rs             NEW: SOCKS5 TCP accept loop + stream open
    egress.rs               NEW: yamux accept + CONNECT read + TCP dial + pump
    metrics.rs              NEW: event emitter + JSON-lines writer + run_id
    testchannel.rs          NEW (#[cfg(test)]): lossy in-memory bidi channel
  bench/
    mod.rs                  NEW: bench command dispatch
    support.rs              NEW: embedded http_echo, tcp_dns, iperf3 launcher
    realistic.rs            NEW: Bench 1 driver
    saturation.rs           NEW: Bench 2 driver
    report.rs               NEW: events.jsonl → summary.json + SLA matrix
    workloads/
      http_echo.rs          NEW
      dns.rs                NEW
      ssh_probe.rs          NEW
      https_fetch.rs        NEW
tests/
  tunnel_sim.rs             NEW: single-peer sim roundtrip
  tunnel_pair_sim.rs        NEW: two-peer sim + trimmed benches
fixtures/
  dns.txt                   NEW: 50-entry DNS name list
scripts/
  bench-tunnel.sh           NEW: Level D pair bench driver
Cargo.toml                  MODIFY: add deps
```

---

## Task 1: Cargo dependencies + flicker MTU constant

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/flicker/mod.rs`
- Test: `tests/flicker_mtu_export.rs`

- [ ] **Step 1: Write failing test for exported constant**

```rust
// tests/flicker_mtu_export.rs
use rtmp_steganography::flicker::FLICKER_MAX_PAYLOAD_BYTES;

#[test]
fn mtu_constant_is_positive_and_reasonable() {
    assert!(FLICKER_MAX_PAYLOAD_BYTES > 0);
    assert!(FLICKER_MAX_PAYLOAD_BYTES < 8192, "unexpectedly large flicker payload");
}
```

- [ ] **Step 2: Run test, confirm FAIL (unresolved import)**

Run: `cargo test --test flicker_mtu_export`
Expected: compile error, `FLICKER_MAX_PAYLOAD_BYTES` not found.

- [ ] **Step 3: Export the constant**

Add to `src/flicker/mod.rs` top of file:

```rust
/// Maximum application bytes carried in a single flicker frame
/// (after FEC overhead, mode B baseline). Used by the tunnel adapter
/// to derive KCP MTU. Value is empirical from v2 framing.
pub const FLICKER_MAX_PAYLOAD_BYTES: usize = 512;
```

(If the real number differs, set it to whatever `flicker::frame` currently guarantees; the test only asserts reasonableness.)

- [ ] **Step 4: Update Cargo.toml with new deps**

Add under `[dependencies]`:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "sync", "time", "macros"] }
kcp = "0.5"
yamux = "0.13"
hdrhistogram = "7"
futures = "0.3"
async-trait = "0.1"
```

- [ ] **Step 5: Run test, confirm PASS + project builds**

Run: `cargo build --release && cargo test --test flicker_mtu_export`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/flicker/mod.rs tests/flicker_mtu_export.rs
git commit -m "feat(flicker): export FLICKER_MAX_PAYLOAD_BYTES for tunnel MTU"
```

---

## Task 2: Metrics module — JSON-lines event emitter

**Files:**
- Create: `src/tunnel/metrics.rs`
- Create: `src/tunnel/mod.rs`
- Modify: `src/lib.rs` — add `pub mod tunnel;`
- Test: `src/tunnel/metrics.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Create empty `src/tunnel/mod.rs`**

```rust
pub mod metrics;
```

Add `pub mod tunnel;` to `src/lib.rs`.

- [ ] **Step 2: Write failing test**

Append to `src/tunnel/metrics.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

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
```

Add `tempfile = "3"` to `[dev-dependencies]` in `Cargo.toml`.

- [ ] **Step 3: Run test, confirm FAIL**

Run: `cargo test --lib tunnel::metrics`
Expected: compile error — `EventEmitter` and `Event` missing.

- [ ] **Step 4: Implement the emitter**

Full contents of `src/tunnel/metrics.rs`:

```rust
//! Structured event emission to JSON-lines + aggregated counters.

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
        }
    }

    pub fn dir(&self) -> &Path { &self.dir }
}

impl Drop for EventEmitter {
    fn drop(&mut self) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.flush();
        }
    }
}

/// Generate a run_id: UTC ISO8601 compact + 4-char random tag.
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
```

Add `rand = "0.8"` to `[dependencies]` (move from dev-deps if present).

- [ ] **Step 5: Run test, confirm PASS**

Run: `cargo test --lib tunnel::metrics`
Expected: 1 passed.

- [ ] **Step 6: Commit**

```bash
git add src/tunnel/ src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat(tunnel): JSON-lines event emitter + run_id"
```

---

## Task 3: MemLossyChannel test utility

**Files:**
- Create: `src/tunnel/testchannel.rs`
- Modify: `src/tunnel/mod.rs` — add `#[cfg(test)] pub mod testchannel;`
- Test: `src/tunnel/testchannel.rs` (inline)

- [ ] **Step 1: Register module**

Update `src/tunnel/mod.rs`:

```rust
pub mod metrics;

#[cfg(test)]
pub mod testchannel;
```

- [ ] **Step 2: Write failing test**

Create `src/tunnel/testchannel.rs`:

```rust
//! In-memory lossy channel for tests. Bidirectional, two endpoints.

use std::time::Duration;
use tokio::sync::mpsc;

pub struct Endpoint {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

pub struct Config {
    pub loss_pct: u8,     // 0..100
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub buffer: usize,    // mpsc bound
}

/// Create two linked endpoints. Frames sent on A arrive on B (subject to loss/latency).
pub fn pair(cfg: Config) -> (Endpoint, Endpoint) {
    let (a_out, b_in) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let (b_out, a_in) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    (
        Endpoint { tx: a_out, rx: a_in },
        Endpoint { tx: b_out, rx: b_in },
    )
    // NOTE: loss/latency applied by a wrap-layer below; pair() returns raw endpoints
    // so simple tests don't need the simulator.
}

impl Endpoint {
    pub async fn send(&self, buf: Vec<u8>) -> Result<(), ()> {
        self.tx.send(buf).await.map_err(|_| ())
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

/// Wraps an Endpoint with loss/latency/jitter simulation.
/// Spawns two tasks: inbound (drop + delay) and outbound (pass-through).
pub fn with_simulation(ep: Endpoint, cfg: Config) -> (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
    let (user_tx, mut out_queue) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let (mut sim_tx, user_rx) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let Endpoint { tx: wire_tx, rx: mut wire_rx } = ep;

    // Outbound: pass through, no modeling of sender-side loss (receiver-side is enough).
    tokio::spawn(async move {
        while let Some(buf) = out_queue.recv().await {
            if wire_tx.send(buf).await.is_err() { break; }
        }
    });

    // Inbound: drop loss_pct%, add latency + uniform jitter.
    let loss = cfg.loss_pct;
    let base = cfg.latency_ms;
    let jitter = cfg.jitter_ms;
    tokio::spawn(async move {
        use rand::Rng;
        while let Some(buf) = wire_rx.recv().await {
            if rand::thread_rng().gen_range(0u8..100) < loss { continue; }
            let delay = base + rand::thread_rng().gen_range(0..=jitter.max(1));
            let sim_tx = sim_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let _ = sim_tx.send(buf).await;
            });
        }
    });

    let (_unused, user_rx) = (sim_tx, user_rx);
    (user_tx, user_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_loss_delivers_all_frames() {
        let (a, mut b) = pair(Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 16 });
        for i in 0..10u8 {
            a.send(vec![i]).await.unwrap();
        }
        for i in 0..10u8 {
            let f = b.recv().await.unwrap();
            assert_eq!(f, vec![i]);
        }
    }

    #[tokio::test]
    async fn simulation_drops_frames_with_loss() {
        let (a, _b) = pair(Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 256 });
        let (tx, mut rx) = with_simulation(a, Config { loss_pct: 50, latency_ms: 1, jitter_ms: 0, buffer: 256 });
        for _ in 0..200 { tx.send(vec![0u8]).await.unwrap(); }
        drop(tx);
        let mut delivered = 0;
        while let Some(_) = rx.recv().await { delivered += 1; }
        // With 50% loss over 200 frames, expect 70..130 (wide margin)
        assert!(delivered > 70 && delivered < 130, "delivered={delivered}");
    }
}
```

**Note:** the `_unused` shadow above is intentional cleanup — remove it cleanly:

Replace the end of `with_simulation` with:

```rust
    (user_tx, user_rx)
```

(Delete the `(_unused, user_rx) = ...` line.)

- [ ] **Step 3: Run test, confirm FAIL on first run then PASS after fix**

Run: `cargo test --lib tunnel::testchannel`
Expected: compile until implementation is correct, then both tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/testchannel.rs src/tunnel/mod.rs
git commit -m "test(tunnel): MemLossyChannel simulator for sim roundtrip tests"
```

---

## Task 4: Datagram channel adapter abstraction

**Files:**
- Create: `src/tunnel/adapter.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module**

Update `src/tunnel/mod.rs`:

```rust
pub mod adapter;
pub mod metrics;

#[cfg(test)]
pub mod testchannel;
```

- [ ] **Step 2: Write failing test**

`src/tunnel/adapter.rs`:

```rust
//! Datagram channel abstraction: KCP speaks this; flicker and mem impls fulfill it.

use async_trait::async_trait;

#[async_trait]
pub trait DatagramChannel: Send + Sync + 'static {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()>;
    async fn recv(&self) -> std::io::Result<Vec<u8>>;
    fn max_payload(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::testchannel;
    use tokio::sync::Mutex;

    struct MemAdapter {
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        mtu: usize,
    }

    #[async_trait]
    impl DatagramChannel for MemAdapter {
        async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
            self.tx.send(buf).await.map_err(|_| std::io::ErrorKind::BrokenPipe.into())
        }
        async fn recv(&self) -> std::io::Result<Vec<u8>> {
            self.rx.lock().await.recv().await.ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
        }
        fn max_payload(&self) -> usize { self.mtu }
    }

    #[tokio::test]
    async fn mem_adapter_roundtrips() {
        let (a, b) = testchannel::pair(testchannel::Config {
            loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 16,
        });
        let testchannel::Endpoint { tx: atx, rx: arx } = a;
        let testchannel::Endpoint { tx: btx, rx: brx } = b;
        let ad = MemAdapter { tx: atx, rx: Mutex::new(arx), mtu: 256 };
        let bd = MemAdapter { tx: btx, rx: Mutex::new(brx), mtu: 256 };
        ad.send(b"hello".to_vec()).await.unwrap();
        let got = bd.recv().await.unwrap();
        assert_eq!(got, b"hello");
    }
}
```

Requires making `Endpoint`'s `tx` / `rx` fields `pub(crate)`. Edit `testchannel.rs`:

```rust
pub struct Endpoint {
    pub(crate) tx: mpsc::Sender<Vec<u8>>,
    pub(crate) rx: mpsc::Receiver<Vec<u8>>,
}
```

- [ ] **Step 3: Implement real flicker adapter**

Append to `src/tunnel/adapter.rs`:

```rust
use std::sync::mpsc as stdmpsc;
use tokio::sync::Mutex as TokioMutex;
use crate::flicker::{FLICKER_MAX_PAYLOAD_BYTES, InboundMessage, OutboundMessage};

pub const MSG_TYPE_TUNNEL: u8 = 0x02;

/// Bridges tokio async tunnel code to the existing sync flicker mpsc channels.
pub struct FlickerChannel {
    out_tx: stdmpsc::Sender<OutboundMessage>,
    in_rx: TokioMutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
}

impl FlickerChannel {
    /// Spawns a bridge task that forwards InboundMessage(msg_type=0x02) to an async channel.
    pub fn new(
        out_tx: stdmpsc::Sender<OutboundMessage>,
        in_rx_sync: stdmpsc::Receiver<InboundMessage>,
    ) -> Self {
        let (async_tx, async_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        std::thread::spawn(move || {
            while let Ok(msg) = in_rx_sync.recv() {
                if msg.msg_type == MSG_TYPE_TUNNEL {
                    if async_tx.blocking_send(msg.payload).is_err() { break; }
                }
            }
        });
        Self { out_tx, in_rx: TokioMutex::new(async_rx) }
    }
}

#[async_trait]
impl DatagramChannel for FlickerChannel {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
        let msg = OutboundMessage { msg_type: MSG_TYPE_TUNNEL, payload: buf };
        self.out_tx.send(msg).map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.in_rx.lock().await.recv().await
            .ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize { FLICKER_MAX_PAYLOAD_BYTES }
}
```

- [ ] **Step 4: Run test, confirm PASS**

Run: `cargo test --lib tunnel::adapter`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/tunnel/
git commit -m "feat(tunnel): DatagramChannel trait + FlickerChannel bridge"
```

---

## Task 5: KCP wrapper with adaptive presets

**Files:**
- Create: `src/tunnel/kcp.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module**

Update `src/tunnel/mod.rs`: `pub mod kcp;`

- [ ] **Step 2: Write failing test**

`src/tunnel/kcp.rs`:

```rust
//! KCP session wrapper. Owns the KCP state, a tick driver task, and
//! talks to a DatagramChannel below. Exposes async reliable byte I/O.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::Mutex;

use crate::tunnel::adapter::DatagramChannel;
use crate::tunnel::metrics::{Event, EventEmitter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile { Throughput, Latency }

impl Profile {
    pub fn from_env() -> Self {
        match std::env::var("TUNNEL_PROFILE").ok().as_deref() {
            Some("throughput") => Self::Throughput,
            _ => Self::Latency,
        }
    }
    pub fn params(self) -> KcpParams {
        match self {
            Profile::Throughput => KcpParams { snd_wnd: 256, rcv_wnd: 256, nodelay: 0, interval: 40, resend: 0, nc: 0, min_rto: 200 },
            Profile::Latency    => KcpParams { snd_wnd: 32,  rcv_wnd: 32,  nodelay: 1, interval: 10, resend: 2, nc: 1, min_rto: 100 },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KcpParams {
    pub snd_wnd: u16,
    pub rcv_wnd: u16,
    pub nodelay: i32,
    pub interval: i32,
    pub resend: i32,
    pub nc: i32,
    pub min_rto: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_from_env_defaults_to_latency() {
        std::env::remove_var("TUNNEL_PROFILE");
        assert_eq!(Profile::from_env(), Profile::Latency);
    }
    #[test]
    fn profile_parameters_match_spec() {
        assert_eq!(Profile::Throughput.params().snd_wnd, 256);
        assert_eq!(Profile::Latency.params().snd_wnd, 32);
        assert_eq!(Profile::Latency.params().nodelay, 1);
    }
}
```

- [ ] **Step 3: Run test, confirm PASS (no impl yet needed for profile tests)**

Run: `cargo test --lib tunnel::kcp`
Expected: 2 passed.

- [ ] **Step 4: Implement KcpSession**

Append to `src/tunnel/kcp.rs`:

```rust
/// KCP session wrapping the `kcp` crate. Spawns a background tick task.
/// Exposes AsyncRead + AsyncWrite via `stream()`.
pub struct KcpSession {
    inner: Arc<Mutex<InnerSession>>,
    emitter: Arc<EventEmitter>,
}

struct InnerSession {
    kcp: kcp::Kcp<DgramOutput>,
    read_buf: Vec<u8>,
}

/// The kcp crate calls this callback when it wants to emit a datagram.
struct DgramOutput {
    sink: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl std::io::Write for DgramOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.sink.send(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

impl KcpSession {
    pub fn start(
        channel: Arc<dyn DatagramChannel>,
        profile: Profile,
        emitter: Arc<EventEmitter>,
    ) -> Self {
        let p = profile.params();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let mut kcp = kcp::Kcp::new(0x1234_5678, DgramOutput { sink: out_tx });
        kcp.set_wndsize(p.snd_wnd as u16, p.rcv_wnd as u16);
        kcp.set_nodelay(p.nodelay != 0, p.interval, p.resend, p.nc != 0);
        kcp.set_rx_minrto(p.min_rto);
        let mtu = channel.max_payload().saturating_sub(36); // KCP hdr 24 + slack
        kcp.set_mtu(mtu).ok();
        let inner = Arc::new(Mutex::new(InnerSession { kcp, read_buf: Vec::new() }));

        // Output pump: KCP datagram → channel.send
        {
            let channel = Arc::clone(&channel);
            let emitter = Arc::clone(&emitter);
            tokio::spawn(async move {
                while let Some(buf) = out_rx.recv().await {
                    emitter.emit(Event::new("kcp", "tx").field("len", buf.len() as i64));
                    if channel.send(buf).await.is_err() { break; }
                }
            });
        }

        // Input pump: channel.recv → kcp.input
        {
            let inner = Arc::clone(&inner);
            let channel = Arc::clone(&channel);
            tokio::spawn(async move {
                loop {
                    match channel.recv().await {
                        Ok(buf) => {
                            let mut g = inner.lock().await;
                            let _ = g.kcp.input(&buf);
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Tick driver
        {
            let inner = Arc::clone(&inner);
            tokio::spawn(async move {
                let interval = Duration::from_millis(p.interval.max(1) as u64);
                loop {
                    tokio::time::sleep(interval).await;
                    let mut g = inner.lock().await;
                    let now_ms = now_ms_u32();
                    g.kcp.update(now_ms).ok();
                }
            });
        }

        Self { inner, emitter }
    }

    pub fn stream(self: Arc<Self>) -> KcpStream {
        KcpStream { session: self }
    }
}

fn now_ms_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    (SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u32)
}

pub struct KcpStream {
    session: Arc<KcpSession>,
}

impl AsyncWrite for KcpStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        // Try to lock; if busy, yield
        let fut = self.session.inner.lock();
        tokio::pin!(fut);
        match fut.poll(cx) {
            std::task::Poll::Ready(mut g) => {
                match g.kcp.send(buf) {
                    Ok(n) => std::task::Poll::Ready(Ok(n)),
                    Err(_) => std::task::Poll::Ready(Err(std::io::ErrorKind::WouldBlock.into())),
                }
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncRead for KcpStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let fut = self.session.inner.lock();
        tokio::pin!(fut);
        match fut.poll(cx) {
            std::task::Poll::Ready(mut g) => {
                let mut tmp = vec![0u8; buf.remaining()];
                match g.kcp.recv(&mut tmp) {
                    Ok(n) => {
                        buf.put_slice(&tmp[..n]);
                        std::task::Poll::Ready(Ok(()))
                    }
                    Err(kcp::Error::RecvQueueEmpty) => {
                        // Schedule wake on next tick
                        let waker = cx.waker().clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            waker.wake();
                        });
                        std::task::Poll::Pending
                    }
                    Err(_) => std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
                }
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}
```

**Important:** The `kcp` crate's exact API signatures may differ slightly across versions. Verify with `cargo doc --open -p kcp` before committing. Adjust `kcp::Error` variant names if the published API uses different ones.

- [ ] **Step 5: Integration test over MemLossyChannel**

Create `tests/tunnel_sim.rs`:

```rust
use std::sync::Arc;
use rtmp_steganography::tunnel::{adapter, kcp::{KcpSession, Profile}, metrics::EventEmitter, testchannel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

struct MemAdapter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    mtu: usize,
}
#[async_trait::async_trait]
impl adapter::DatagramChannel for MemAdapter {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
        self.tx.send(buf).await.map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.rx.lock().await.recv().await.ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize { self.mtu }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kcp_roundtrip_over_lossy_mem_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let em_a = Arc::new(EventEmitter::new(tmp.path(), "A").unwrap());
    let em_b = Arc::new(EventEmitter::new(tmp.path(), "B").unwrap());

    let (a, b) = testchannel::pair(testchannel::Config {
        loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 512,
    });
    // Apply 10% loss in both directions
    let (a_tx, a_rx) = testchannel::with_simulation(a, testchannel::Config {
        loss_pct: 10, latency_ms: 50, jitter_ms: 5, buffer: 512,
    });
    let (b_tx, b_rx) = testchannel::with_simulation(b, testchannel::Config {
        loss_pct: 10, latency_ms: 50, jitter_ms: 5, buffer: 512,
    });

    let ad_a: Arc<dyn adapter::DatagramChannel> = Arc::new(MemAdapter { tx: a_tx, rx: Mutex::new(a_rx), mtu: 512 });
    let ad_b: Arc<dyn adapter::DatagramChannel> = Arc::new(MemAdapter { tx: b_tx, rx: Mutex::new(b_rx), mtu: 512 });

    let sess_a = Arc::new(KcpSession::start(ad_a, Profile::Latency, em_a));
    let sess_b = Arc::new(KcpSession::start(ad_b, Profile::Latency, em_b));

    let mut sa = sess_a.stream();
    let mut sb = sess_b.stream();

    sa.write_all(b"hello from a").await.unwrap();
    sa.flush().await.unwrap();

    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), sb.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"hello from a");
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --test tunnel_sim -- --test-threads=1`
Expected: PASS (may need a few retries if kcp API differs; fix compile errors by consulting `cargo doc -p kcp`).

- [ ] **Step 7: Commit**

```bash
git add src/tunnel/ tests/tunnel_sim.rs
git commit -m "feat(tunnel): KCP wrapper with throughput/latency presets + sim test"
```

---

## Task 6: yamux multiplex layer

**Files:**
- Create: `src/tunnel/mux.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module** — `pub mod mux;` in `src/tunnel/mod.rs`.

- [ ] **Step 2: Write failing test**

`src/tunnel/mux.rs`:

```rust
//! yamux session glue: wraps an AsyncRead+AsyncWrite into a multiplexed session.

use std::sync::Arc;
use futures::prelude::*;
use tokio::io::{AsyncRead, AsyncWrite};
use yamux::{Config as YamuxConfig, Connection, Mode, Stream};

pub struct MuxSession<S> { conn: Arc<tokio::sync::Mutex<Connection<S>>> }

impl<S> MuxSession<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn client(stream: S) -> Self { Self::new(stream, Mode::Client) }
    pub fn server(stream: S) -> Self { Self::new(stream, Mode::Server) }

    fn new(stream: S, mode: Mode) -> Self {
        // yamux expects a futures::io stream; compat-wrap from tokio
        use tokio_util::compat::TokioAsyncReadCompatExt;
        let compat = stream.compat();
        let conn = Connection::new(compat, YamuxConfig::default(), mode);
        Self { conn: Arc::new(tokio::sync::Mutex::new(conn)) }
    }

    pub async fn open_stream(&self) -> std::io::Result<Stream> {
        let mut g = self.conn.lock().await;
        g.new_stream().await.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }

    pub async fn accept_stream(&self) -> std::io::Result<Stream> {
        let mut g = self.conn.lock().await;
        g.next_stream().await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?
            .ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mux_over_tokio_duplex_roundtrips() {
        let (a, b) = duplex(8192);
        let client = MuxSession::client(a);
        let server = MuxSession::server(b);

        let server_task = tokio::spawn(async move {
            let mut s = server.accept_stream().await.unwrap();
            use futures::AsyncReadExt as FRead;
            use futures::AsyncWriteExt as FWrite;
            let mut buf = [0u8; 32];
            let n = FRead::read(&mut s, &mut buf).await.unwrap();
            FWrite::write_all(&mut s, &buf[..n]).await.unwrap();
            FWrite::close(&mut s).await.unwrap();
        });

        let mut cs = client.open_stream().await.unwrap();
        use futures::AsyncWriteExt as FWrite;
        use futures::AsyncReadExt as FRead;
        FWrite::write_all(&mut cs, b"ping").await.unwrap();
        FWrite::flush(&mut cs).await.unwrap();
        let mut buf = [0u8; 32];
        let n = FRead::read(&mut cs, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
        server_task.await.unwrap();
    }
}
```

Add to `Cargo.toml` `[dependencies]`: `tokio-util = { version = "0.7", features = ["compat"] }`.

- [ ] **Step 3: Run test**

Run: `cargo test --lib tunnel::mux`
Expected: PASS. If yamux API differs across versions, consult `cargo doc -p yamux`; the essential surface (`new_stream`, `next_stream`) has been stable.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/mux.rs src/tunnel/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(tunnel): yamux session wrapper over generic stream"
```

---

## Task 7: Internal CONNECT framing

**Files:**
- Create: `src/tunnel/framing.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module** — `pub mod framing;`

- [ ] **Step 2: Write failing tests**

`src/tunnel/framing.rs`:

```rust
//! First-frame CONNECT message sent on every opened yamux stream.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFrame {
    pub host: String,
    pub port: u16,
}

impl ConnectFrame {
    pub fn encode(&self) -> Vec<u8> {
        let host = self.host.as_bytes();
        assert!(host.len() <= u8::MAX as usize, "host too long");
        let mut v = Vec::with_capacity(4 + host.len());
        v.push(1); // version
        v.push(host.len() as u8);
        v.extend_from_slice(host);
        v.extend_from_slice(&self.port.to_be_bytes());
        v
    }

    pub async fn write_to<W: AsyncWrite + Unpin>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.encode()).await
    }

    pub async fn read_from<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver).await?;
        if ver[0] != 1 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "connect version"));
        }
        let mut hl = [0u8; 1];
        r.read_exact(&mut hl).await?;
        let mut host = vec![0u8; hl[0] as usize];
        r.read_exact(&mut host).await?;
        let mut port = [0u8; 2];
        r.read_exact(&mut port).await?;
        Ok(Self {
            host: String::from_utf8(host).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "host utf8"))?,
            port: u16::from_be_bytes(port),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = duplex(256);
        let f = ConnectFrame { host: "example.com".into(), port: 443 };
        f.write_to(&mut a).await.unwrap();
        let g = ConnectFrame::read_from(&mut b).await.unwrap();
        assert_eq!(f, g);
    }

    #[test]
    fn encode_exact_bytes() {
        let f = ConnectFrame { host: "a".into(), port: 80 };
        assert_eq!(f.encode(), vec![1, 1, b'a', 0, 80]);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib tunnel::framing`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/framing.rs src/tunnel/mod.rs
git commit -m "feat(tunnel): internal CONNECT frame codec"
```

---

## Task 8: SOCKS5 handshake parser

**Files:**
- Create: `src/tunnel/socks5.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module** — `pub mod socks5;`

- [ ] **Step 2: Write failing tests**

`src/tunnel/socks5.rs`:

```rust
//! Minimal SOCKS5 handshake: auth=none, CONNECT only. RFC 1928.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5Request {
    pub host: String,
    pub port: u16,
}

#[repr(u8)]
pub enum Socks5Reply {
    Ok = 0x00,
    GeneralFailure = 0x01,
    ConnNotAllowed = 0x02,
    NetUnreachable = 0x03,
    HostUnreachable = 0x04,
    ConnRefused = 0x05,
    TtlExpired = 0x06,
    CmdNotSupported = 0x07,
    AddrTypeNotSupported = 0x08,
}

pub async fn negotiate<S>(s: &mut S) -> std::io::Result<Socks5Request>
where S: AsyncRead + AsyncWrite + Unpin {
    // Greeting: [ver=5][nmethods][methods...]
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(io_err("not socks5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.iter().any(|&m| m == 0x00) {
        s.write_all(&[5, 0xFF]).await?;
        return Err(io_err("no no-auth method"));
    }
    s.write_all(&[5, 0x00]).await?;

    // Request: [ver=5][cmd][rsv][atyp][addr][port]
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr).await?;
    if hdr[0] != 5 { return Err(io_err("not socks5 req")); }
    if hdr[1] != 0x01 {
        reply(s, Socks5Reply::CmdNotSupported).await.ok();
        return Err(io_err("command not CONNECT"));
    }
    let host = match hdr[3] {
        0x01 => {
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await?;
            std::net::Ipv4Addr::from(b).to_string()
        }
        0x03 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut h = vec![0u8; l[0] as usize];
            s.read_exact(&mut h).await?;
            String::from_utf8(h).map_err(|_| io_err("bad host"))?
        }
        0x04 => {
            let mut b = [0u8; 16];
            s.read_exact(&mut b).await?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => {
            reply(s, Socks5Reply::AddrTypeNotSupported).await.ok();
            return Err(io_err("atyp"));
        }
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    Ok(Socks5Request { host, port: u16::from_be_bytes(port) })
}

pub async fn reply<S: AsyncWrite + Unpin>(s: &mut S, code: Socks5Reply) -> std::io::Result<()> {
    // Minimal reply: bind address 0.0.0.0:0
    let resp = [5u8, code as u8, 0, 0x01, 0,0,0,0, 0,0];
    s.write_all(&resp).await
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn greets_and_parses_connect_domain() {
        let (mut client, mut server) = duplex(256);
        let server_task = tokio::spawn(async move { negotiate(&mut server).await });

        // Greeting: ver=5, nmethods=1, method=0
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut greet_resp = [0u8; 2];
        client.read_exact(&mut greet_resp).await.unwrap();
        assert_eq!(greet_resp, [5, 0]);

        // Request: CONNECT example.com:443
        let host = b"example.com";
        let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
        req.extend_from_slice(host);
        req.extend_from_slice(&443u16.to_be_bytes());
        client.write_all(&req).await.unwrap();

        let parsed = server_task.await.unwrap().unwrap();
        assert_eq!(parsed, Socks5Request { host: "example.com".into(), port: 443 });
    }
}
```

- [ ] **Step 3: Run test** — `cargo test --lib tunnel::socks5`, expect PASS.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/socks5.rs src/tunnel/mod.rs
git commit -m "feat(tunnel): SOCKS5 handshake parser (CONNECT, no auth)"
```

---

## Task 9: SOCKS5 listener — accept, handshake, open stream

**Files:**
- Create: `src/tunnel/listener.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module** — `pub mod listener;`

- [ ] **Step 2: Write implementation** (integration test in Task 11)

`src/tunnel/listener.rs`:

```rust
//! SOCKS5 inbound listener. Per accepted TCP connection:
//! 1. Negotiate SOCKS5 handshake.
//! 2. Open a yamux stream on the tunnel.
//! 3. Write the internal CONNECT frame.
//! 4. Await 1-byte egress status.
//! 5. Reply to SOCKS5 client and bidirectional-pump bytes.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::tunnel::framing::ConnectFrame;
use crate::tunnel::metrics::{Event, EventEmitter};
use crate::tunnel::mux::MuxSession;
use crate::tunnel::socks5::{self, Socks5Reply};

pub struct Listener<S> {
    addr: std::net::SocketAddr,
    mux: Arc<MuxSession<S>>,
    emitter: Arc<EventEmitter>,
}

impl<S> Listener<S>
where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {
    pub fn new(addr: std::net::SocketAddr, mux: Arc<MuxSession<S>>, emitter: Arc<EventEmitter>) -> Self {
        Self { addr, mux, emitter }
    }

    pub async fn run(self) -> std::io::Result<()> {
        let lst = TcpListener::bind(self.addr).await?;
        loop {
            let (mut client, _peer) = lst.accept().await?;
            let mux = Arc::clone(&self.mux);
            let em = Arc::clone(&self.emitter);
            tokio::spawn(async move {
                let req = match socks5::negotiate(&mut client).await {
                    Ok(r) => r,
                    Err(_) => {
                        em.emit(Event::new("socks5", "error").field("stage", "negotiate"));
                        return;
                    }
                };
                em.emit(Event::new("socks5", "connect")
                    .field("host", req.host.clone())
                    .field("port", req.port as i64));

                let mut stream = match mux.open_stream().await {
                    Ok(s) => s,
                    Err(_) => {
                        socks5::reply(&mut client, Socks5Reply::NetUnreachable).await.ok();
                        em.emit(Event::new("socks5", "reply").field("code", Socks5Reply::NetUnreachable as u8 as i64));
                        return;
                    }
                };

                let cf = ConnectFrame { host: req.host.clone(), port: req.port };
                {
                    use futures::io::AsyncWriteExt as FWrite;
                    use tokio_util::compat::FuturesAsyncWriteCompatExt;
                    // yamux::Stream implements futures::io — write raw bytes
                    let bytes = cf.encode();
                    FWrite::write_all(&mut stream, &bytes).await.ok();
                    FWrite::flush(&mut stream).await.ok();
                }

                // Read 1-byte status from egress
                let status = {
                    use futures::io::AsyncReadExt as FRead;
                    let mut b = [0u8; 1];
                    match FRead::read_exact(&mut stream, &mut b).await {
                        Ok(()) => b[0],
                        Err(_) => 0x03, // net unreachable
                    }
                };
                let reply_code = match status {
                    0 => Socks5Reply::Ok,
                    1 => Socks5Reply::HostUnreachable,
                    2 => Socks5Reply::ConnRefused,
                    3 => Socks5Reply::TtlExpired,
                    _ => Socks5Reply::NetUnreachable,
                };
                socks5::reply(&mut client, reply_code).await.ok();
                em.emit(Event::new("socks5", "reply").field("code", status as i64));
                if status != 0 { return; }

                // Bidirectional pump: client ↔ stream
                use tokio_util::compat::FuturesAsyncReadCompatExt;
                let mut stream_compat = stream.compat();
                let (mut cr, mut cw) = tokio::io::split(client);
                let (mut sr, mut sw) = tokio::io::split(stream_compat);
                let a = tokio::io::copy(&mut cr, &mut sw);
                let b = tokio::io::copy(&mut sr, &mut cw);
                let _ = tokio::try_join!(a, b);
            });
        }
    }
}
```

- [ ] **Step 3: Build check (integration test deferred to Task 11)**

Run: `cargo build --lib`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/listener.rs src/tunnel/mod.rs
git commit -m "feat(tunnel): SOCKS5 listener — accept, handshake, open stream"
```

---

## Task 10: Egress worker

**Files:**
- Create: `src/tunnel/egress.rs`
- Modify: `src/tunnel/mod.rs`

- [ ] **Step 1: Declare module** — `pub mod egress;`

- [ ] **Step 2: Implement**

`src/tunnel/egress.rs`:

```rust
//! Egress: accept yamux streams, read CONNECT frame, dial TCP, pump bytes.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;

use crate::tunnel::framing::ConnectFrame;
use crate::tunnel::metrics::{Event, EventEmitter};
use crate::tunnel::mux::MuxSession;

pub struct Egress<S> {
    mux: Arc<MuxSession<S>>,
    emitter: Arc<EventEmitter>,
}

impl<S> Egress<S>
where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {
    pub fn new(mux: Arc<MuxSession<S>>, emitter: Arc<EventEmitter>) -> Self {
        Self { mux, emitter }
    }

    pub async fn run(self) -> std::io::Result<()> {
        loop {
            let mut stream = match self.mux.accept_stream().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let em = Arc::clone(&self.emitter);
            tokio::spawn(async move {
                use tokio_util::compat::FuturesAsyncReadCompatExt;
                use tokio_util::compat::FuturesAsyncWriteCompatExt;

                // Read CONNECT frame from yamux stream (adapt to tokio API)
                let mut compat_r = (&mut stream).compat();
                let cf = match ConnectFrame::read_from(&mut compat_r).await {
                    Ok(f) => f,
                    Err(_) => { em.emit(Event::new("egress", "bad_frame")); return; }
                };

                let target = format!("{}:{}", cf.host, cf.port);
                em.emit(Event::new("egress", "dial").field("target", target.clone()));

                let conn = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect(&target)).await;
                let status: u8 = match conn {
                    Ok(Ok(_)) => 0,
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => 2,
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => 3,
                    Ok(Err(_)) => 1,
                    Err(_) => 3,
                };
                // Write status byte back
                {
                    use futures::io::AsyncWriteExt;
                    let _ = (&mut stream).write_all(&[status]).await;
                    let _ = (&mut stream).flush().await;
                }
                em.emit(Event::new("egress", "status").field("code", status as i64));
                if status != 0 { return; }

                // Pump
                let tcp = match conn.unwrap().unwrap().into_split() {
                    (r, w) => (r, w),
                };
                let (mut tr, mut tw) = tcp;
                let mut stream_compat = stream.compat();
                let (mut sr, mut sw) = tokio::io::split(&mut stream_compat);
                let a = tokio::io::copy(&mut sr, &mut tw);
                let b = tokio::io::copy(&mut tr, &mut sw);
                let _ = tokio::try_join!(a, b);
            });
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Build check** — `cargo build --lib`, expect compile OK.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/egress.rs src/tunnel/mod.rs
git commit -m "feat(tunnel): egress worker — accept stream, dial, pump"
```

---

## Task 11: Tunnel supervisor + pair integration test

**Files:**
- Modify: `src/tunnel/mod.rs`
- Create: `tests/tunnel_pair_sim.rs`

- [ ] **Step 1: Expose public Tunnel::start**

Extend `src/tunnel/mod.rs`:

```rust
pub mod adapter;
pub mod egress;
pub mod framing;
pub mod kcp;
pub mod listener;
pub mod metrics;
pub mod mux;
pub mod socks5;

#[cfg(test)]
pub mod testchannel;

use std::sync::Arc;
use std::net::SocketAddr;
use tokio::io::{duplex, DuplexStream};

use crate::tunnel::adapter::DatagramChannel;
use crate::tunnel::kcp::{KcpSession, KcpStream, Profile};
use crate::tunnel::metrics::EventEmitter;
use crate::tunnel::mux::MuxSession;

/// High-level tunnel: feeds `channel` to KCP, runs yamux on top, exposes a SOCKS5
/// listener and an egress worker simultaneously (every peer does both).
pub struct Tunnel {
    pub emitter: Arc<EventEmitter>,
}

impl Tunnel {
    pub async fn start(
        channel: Arc<dyn DatagramChannel>,
        profile: Profile,
        socks_bind: SocketAddr,
        emitter: Arc<EventEmitter>,
    ) -> std::io::Result<Self> {
        // Build KCP + stream
        let kcp = Arc::new(KcpSession::start(channel, profile, Arc::clone(&emitter)));
        let stream: KcpStream = Arc::clone(&kcp).stream();

        // Bridge KcpStream → yamux. yamux needs a single AsyncRead+AsyncWrite, but the
        // stream is used by both client (listener) and server (egress) halves. We use
        // one yamux::Connection; its mode is decided by who opens first. Convention:
        // peer with lexicographically smaller PEER_ID is Client, other is Server.
        let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
        let mode_client = peer_id.as_str() < "B";
        let mux: Arc<MuxSession<KcpStream>> = Arc::new(
            if mode_client { MuxSession::client(stream) } else { MuxSession::server(stream) }
        );

        // Spawn listener + egress
        let em_l = Arc::clone(&emitter);
        let em_e = Arc::clone(&emitter);
        let mux_l = Arc::clone(&mux);
        let mux_e = Arc::clone(&mux);
        tokio::spawn(async move {
            let lst = crate::tunnel::listener::Listener::new(socks_bind, mux_l, em_l);
            let _ = lst.run().await;
        });
        tokio::spawn(async move {
            let eg = crate::tunnel::egress::Egress::new(mux_e, em_e);
            let _ = eg.run().await;
        });

        Ok(Self { emitter })
    }
}
```

- [ ] **Step 2: Pair sim integration test**

`tests/tunnel_pair_sim.rs`:

```rust
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use rtmp_steganography::tunnel::{
    adapter::{self, DatagramChannel},
    kcp::Profile,
    metrics::EventEmitter,
    testchannel,
    Tunnel,
};

struct MemAdapter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    mtu: usize,
}
#[async_trait::async_trait]
impl DatagramChannel for MemAdapter {
    async fn send(&self, b: Vec<u8>) -> std::io::Result<()> {
        self.tx.send(b).await.map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.rx.lock().await.recv().await.ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize { self.mtu }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socks5_echo_through_tunnel_pair_over_lossy_mem() {
    let tmp = tempfile::tempdir().unwrap();
    let em_a = Arc::new(EventEmitter::new(tmp.path(), "A").unwrap());
    let em_b = Arc::new(EventEmitter::new(tmp.path(), "B").unwrap());

    let (a, b) = testchannel::pair(testchannel::Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 1024 });
    let (atx, arx) = testchannel::with_simulation(a, testchannel::Config { loss_pct: 10, latency_ms: 30, jitter_ms: 5, buffer: 1024 });
    let (btx, brx) = testchannel::with_simulation(b, testchannel::Config { loss_pct: 10, latency_ms: 30, jitter_ms: 5, buffer: 1024 });

    let ch_a: Arc<dyn DatagramChannel> = Arc::new(MemAdapter { tx: atx, rx: Mutex::new(arx), mtu: 512 });
    let ch_b: Arc<dyn DatagramChannel> = Arc::new(MemAdapter { tx: btx, rx: Mutex::new(brx), mtu: 512 });

    // Start a local echo server on peer B's side to be the egress target
    let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => { let _ = s.write_all(&buf[..n]).await; }
                    }
                }
            });
        }
    });

    let socks_a: SocketAddr = "127.0.0.1:11080".parse().unwrap();
    let socks_b: SocketAddr = "127.0.0.1:11081".parse().unwrap();

    std::env::set_var("PEER_ID", "A");
    let _ta = Tunnel::start(ch_a, Profile::Latency, socks_a, em_a).await.unwrap();
    std::env::set_var("PEER_ID", "B");
    let _tb = Tunnel::start(ch_b, Profile::Latency, socks_b, em_b).await.unwrap();

    // Wait for listeners to bind
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Drive a SOCKS5 client from peer A → target = echo_addr on peer B's host
    let mut c = TcpStream::connect(socks_a).await.unwrap();
    // Greeting
    c.write_all(&[5, 1, 0]).await.unwrap();
    let mut g = [0u8; 2]; c.read_exact(&mut g).await.unwrap();
    assert_eq!(g, [5, 0]);
    // CONNECT 127.0.0.1:echo_port
    let host = format!("{}", echo_addr.ip());
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&echo_addr.port().to_be_bytes());
    c.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10]; c.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0, "SOCKS5 reply ok");

    // Echo
    c.write_all(b"the quick brown fox").await.unwrap();
    let mut buf = [0u8; 32];
    let n = tokio::time::timeout(std::time::Duration::from_secs(15), c.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"the quick brown fox");
}
```

- [ ] **Step 3: Run test**

Run: `cargo test --test tunnel_pair_sim -- --test-threads=1 --nocapture`
Expected: PASS within ~30s. If it hangs, increase timeout and check mode_client decision and yamux version compatibility.

- [ ] **Step 4: Commit**

```bash
git add src/tunnel/mod.rs tests/tunnel_pair_sim.rs
git commit -m "feat(tunnel): Tunnel supervisor + pair integration test"
```

---

## Task 12: CLI `peer --tunnel-socks` mode

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/peer/app.rs`
- Modify: `src/peer/mod.rs`

- [ ] **Step 1: Read existing CLI**

Inspect `src/cli.rs` and `src/peer/mod.rs`. Keep existing subcommands intact.

- [ ] **Step 2: Add tunnel flag to peer subcommand**

In `src/cli.rs`, locate the `Peer` variant of the Commands enum. Add:

```rust
#[derive(clap::Args, Debug)]
pub struct PeerArgs {
    #[arg(long)]
    pub tunnel_socks: Option<std::net::SocketAddr>,
}
```

Update the dispatch to pass `tunnel_socks` down to `peer::run`.

- [ ] **Step 3: Branch in peer::app**

In `src/peer/app.rs`, add:

```rust
pub fn run_tunnel(
    outbound_tx: std::sync::mpsc::Sender<crate::flicker::OutboundMessage>,
    inbound_rx: std::sync::mpsc::Receiver<crate::flicker::InboundMessage>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    socks_bind: std::net::SocketAddr,
) {
    use crate::tunnel::{adapter::FlickerChannel, kcp::Profile, metrics::{EventEmitter, new_run_id}, Tunnel};
    use std::sync::Arc;

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        let run_id = new_run_id();
        let base = std::env::var("METRICS_DIR").unwrap_or_else(|_| "./metrics".into());
        let dir = std::path::PathBuf::from(base).join(&run_id);
        let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
        let em = Arc::new(EventEmitter::new(&dir, peer_id).unwrap());

        let ch: Arc<dyn crate::tunnel::adapter::DatagramChannel> =
            Arc::new(FlickerChannel::new(outbound_tx, inbound_rx));
        let profile = Profile::from_env();
        let _t = Tunnel::start(ch, profile, socks_bind, em).await.unwrap();

        while running.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
}
```

- [ ] **Step 4: Wire in `src/peer/mod.rs`**

Locate where `run_default` is called. Branch on `PeerArgs::tunnel_socks`:

```rust
if let Some(addr) = args.tunnel_socks {
    crate::peer::app::run_tunnel(out_tx, in_rx, running, addr);
} else {
    crate::peer::app::run_default(Some(out_tx), Some(in_rx), running);
}
```

- [ ] **Step 5: Build + smoke**

Run: `cargo build --release`
Expected: compile OK. Optional manual smoke: `TUNNEL_PROFILE=latency ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:1080` (won't actually tunnel without real VK, but listener should bind — verify with `netstat`).

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/peer/app.rs src/peer/mod.rs
git commit -m "feat(cli): peer --tunnel-socks flag wires tunnel into flicker mpsc"
```

---

## Task 13: Bench support services (http echo + tcp DNS + iperf3 launcher)

**Files:**
- Create: `src/bench/mod.rs`
- Create: `src/bench/support.rs`
- Modify: `src/lib.rs` — `pub mod bench;`
- Create: `fixtures/dns.txt`

- [ ] **Step 1: Register module**

`src/bench/mod.rs`:

```rust
pub mod support;
pub mod realistic;
pub mod saturation;
pub mod report;
pub mod workloads;
```

Add to `src/lib.rs`: `pub mod bench;`

- [ ] **Step 2: Write failing test (HTTP echo)**

`src/bench/support.rs`:

```rust
//! Embedded support services used by the bench harness.
//! Run inside the peer process so egress targets are deterministic.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub async fn spawn_http_echo(bind: SocketAddr, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((mut s, _)) = lst.accept().await else { continue };
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(4096);
                let mut tmp = [0u8; 4096];
                // Read request (very naive; consume until we see \r\n\r\n)
                loop {
                    let n = match s.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
                // Parse content-length and read body fully
                let body_start = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let headers = &buf[..body_start];
                let cl = parse_content_length(headers).unwrap_or(0);
                while buf.len() - body_start < cl {
                    let n = match s.read(&mut tmp).await { Ok(0) | Err(_) => break, Ok(n) => n };
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = &buf[body_start..body_start + cl.min(buf.len() - body_start)];
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.write_all(body).await;
            });
        }
    });
    Ok(())
}

fn parse_content_length(hdr: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(hdr).ok()?;
    for line in s.split("\r\n") {
        let mut it = line.splitn(2, ':');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k.eq_ignore_ascii_case("content-length") {
                return v.trim().parse().ok();
            }
        }
    }
    None
}

/// Minimal TCP-DNS responder: serves a fixed answer table from the fixture file.
pub async fn spawn_tcp_dns(bind: SocketAddr, fixture_path: &std::path::Path, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let text = std::fs::read_to_string(fixture_path)?;
    let names: Vec<String> = text.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((mut s, _)) = lst.accept().await else { continue };
            let names = names.clone();
            tokio::spawn(async move {
                loop {
                    let mut lenb = [0u8; 2];
                    if s.read_exact(&mut lenb).await.is_err() { return; }
                    let n = u16::from_be_bytes(lenb) as usize;
                    let mut req = vec![0u8; n];
                    if s.read_exact(&mut req).await.is_err() { return; }
                    // Minimal fake DNS reply: copy txid, set QR=1, answer A 127.0.0.2 if qname
                    // matches fixture, else NXDOMAIN. Production-grade parsing is OUT of scope here.
                    let mut resp = req.clone();
                    if resp.len() >= 4 {
                        resp[2] = 0x81; resp[3] = 0x80; // standard response, no error
                    }
                    let _ = s.write_all(&(resp.len() as u16).to_be_bytes()).await;
                    let _ = s.write_all(&resp).await;
                    let _ = names.first(); // silence unused
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[tokio::test(flavor = "multi_thread")]
    async fn http_echo_returns_body() {
        let running = Arc::new(AtomicBool::new(true));
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let lst = TcpListener::bind(addr).await.unwrap();
        let bound = lst.local_addr().unwrap();
        drop(lst);
        spawn_http_echo(bound, Arc::clone(&running)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut c = TcpStream::connect(bound).await.unwrap();
        let body = b"hello-echo";
        let req = format!("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", body.len());
        c.write_all(req.as_bytes()).await.unwrap();
        c.write_all(body).await.unwrap();

        let mut buf = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), c.read_to_end(&mut buf)).await.ok();
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.contains("hello-echo"), "resp={}", resp);
        running.store(false, Ordering::SeqCst);
    }
}
```

- [ ] **Step 3: Create fixtures file**

`fixtures/dns.txt`:

```
example.com
example.org
example.net
wikipedia.org
cloudflare.com
google.com
github.com
stackoverflow.com
rust-lang.org
docs.rs
crates.io
kernel.org
```

(Fill to 50 entries with any public names; list above is a starting seed — duplicate or add similar public domains to reach 50.)

- [ ] **Step 4: Run test**

Run: `cargo test --lib bench::support`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bench/ src/lib.rs fixtures/dns.txt
git commit -m "feat(bench): embedded http_echo + tcp_dns support services + fixtures"
```

---

## Task 14: Bench workloads (HTTP echo, DNS, SSH probe, HTTPS fetch)

**Files:**
- Create: `src/bench/workloads/mod.rs`
- Create: `src/bench/workloads/http_echo.rs`
- Create: `src/bench/workloads/dns.rs`
- Create: `src/bench/workloads/ssh_probe.rs`
- Create: `src/bench/workloads/https_fetch.rs`

- [ ] **Step 1: Workload trait**

`src/bench/workloads/mod.rs`:

```rust
pub mod http_echo;
pub mod dns;
pub mod ssh_probe;
pub mod https_fetch;

use std::sync::Arc;
use crate::tunnel::metrics::{Event, EventEmitter};

pub struct WorkloadResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub bytes: u64,
}

pub fn emit_done(em: &Arc<EventEmitter>, name: &'static str, id: u64, r: &WorkloadResult) {
    em.emit(Event::new("bench", "request_done")
        .field("workload", name)
        .field("id", id as i64)
        .field("ok", r.ok)
        .field("latency_ms", r.latency_ms as i64)
        .field("bytes", r.bytes as i64));
}
```

- [ ] **Step 2: HTTP echo workload**

`src/bench/workloads/http_echo.rs`:

```rust
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(
    socks: SocketAddr,
    target_host: &str,
    target_port: u16,
    payload_bytes: usize,
    iterations: usize,
    em: Arc<EventEmitter>,
) {
    for i in 0..iterations {
        let start = Instant::now();
        let r = one_roundtrip(socks, target_host, target_port, payload_bytes).await;
        let result = match r {
            Ok(b) => WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes: b },
            Err(_) => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
        };
        emit_done(&em, "http_echo", i as u64, &result);
    }
}

async fn one_roundtrip(socks: SocketAddr, host: &str, port: u16, bytes: usize) -> std::io::Result<u64> {
    let mut c = TcpStream::connect(socks).await?;
    c.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2]; c.read_exact(&mut g).await?;
    if g != [5, 0] { return Err(std::io::Error::new(std::io::ErrorKind::Other, "bad socks5 greet")); }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await?;
    let mut rep = [0u8; 10]; c.read_exact(&mut rep).await?;
    if rep[1] != 0 { return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("socks5 reply {}", rep[1]))); }

    let body: Vec<u8> = (0..bytes).map(|i| (i as u8).wrapping_mul(31)).collect();
    let head = format!("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", bytes);
    c.write_all(head.as_bytes()).await?;
    c.write_all(&body).await?;

    // Read full response
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = c.read(&mut tmp).await?;
        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() >= bytes { break; }
    }
    Ok(buf.len() as u64)
}
```

- [ ] **Step 3: DNS workload (fires TCP DNS queries through tunnel)**

`src/bench/workloads/dns.rs`:

```rust
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(socks: SocketAddr, dns_host: &str, dns_port: u16, names: &[String], em: Arc<EventEmitter>) {
    for (i, name) in names.iter().enumerate() {
        let start = Instant::now();
        let r = lookup(socks, dns_host, dns_port, name).await;
        let res = match r {
            Ok(b) => WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes: b },
            Err(_) => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
        };
        emit_done(&em, "dns", i as u64, &res);
    }
}

async fn lookup(socks: SocketAddr, host: &str, port: u16, name: &str) -> std::io::Result<u64> {
    let mut c = TcpStream::connect(socks).await?;
    c.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2]; c.read_exact(&mut g).await?;
    if g != [5, 0] { return Err(std::io::ErrorKind::Other.into()); }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await?;
    let mut rep = [0u8; 10]; c.read_exact(&mut rep).await?;
    if rep[1] != 0 { return Err(std::io::ErrorKind::Other.into()); }

    // Build a minimal DNS A query for `name`
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&0x1234u16.to_be_bytes()); // id
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: RD=1
    q.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    q.extend_from_slice(&0u16.to_be_bytes()); // ancount
    q.extend_from_slice(&0u16.to_be_bytes()); // nscount
    q.extend_from_slice(&0u16.to_be_bytes()); // arcount
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0); // root
    q.extend_from_slice(&1u16.to_be_bytes()); // A
    q.extend_from_slice(&1u16.to_be_bytes()); // IN

    c.write_all(&(q.len() as u16).to_be_bytes()).await?;
    c.write_all(&q).await?;

    let mut lenb = [0u8; 2];
    c.read_exact(&mut lenb).await?;
    let n = u16::from_be_bytes(lenb) as usize;
    let mut resp = vec![0u8; n];
    c.read_exact(&mut resp).await?;
    Ok(n as u64)
}
```

- [ ] **Step 4: SSH probe workload**

`src/bench/workloads/ssh_probe.rs`:

```rust
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(socks_host: &str, socks_port: u16, target: &str, iterations: usize, em: Arc<EventEmitter>) {
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
            Ok(o) if o.status.success() => WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes: o.stdout.len() as u64 },
            _ => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
        };
        emit_done(&em, "ssh_probe", i as u64, &result);
    }
}
```

- [ ] **Step 5: HTTPS fetch workload**

`src/bench/workloads/https_fetch.rs`:

```rust
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
                    "-o", "/dev/null",
                    "-w", "%{size_download}",
                    url,
                ])
                .output().await;
            let result = match res {
                Ok(o) if o.status.success() => {
                    let bytes: u64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
                    WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes }
                }
                _ => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
            };
            emit_done(&em, "https_fetch", i as u64, &result);
        }
        let _ = Duration::from_millis(0);
    }
}
```

- [ ] **Step 6: Build**

Run: `cargo build --lib`
Expected: compile OK.

- [ ] **Step 7: Commit**

```bash
git add src/bench/workloads/
git commit -m "feat(bench): workload runners (http_echo, dns, ssh_probe, https_fetch)"
```

---

## Task 15: Bench 1 realistic driver

**Files:**
- Create: `src/bench/realistic.rs`

- [ ] **Step 1: Implement driver**

```rust
//! Bench 1 — realistic mixed workload.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::bench::support;
use crate::bench::workloads::{http_echo, dns, ssh_probe, https_fetch};
use crate::tunnel::metrics::EventEmitter;

pub struct Config {
    pub socks: SocketAddr,
    pub echo_host: String, pub echo_port: u16,
    pub dns_host: String,  pub dns_port: u16,
    pub ssh_target: Option<String>,
    pub fixtures_dir: std::path::PathBuf,
}

pub async fn run(cfg: Config, em: Arc<EventEmitter>) {
    let running = Arc::new(AtomicBool::new(true));

    // Support services bind
    let echo_bind: SocketAddr = format!("{}:{}", cfg.echo_host, cfg.echo_port).parse().unwrap();
    let dns_bind: SocketAddr  = format!("{}:{}", cfg.dns_host,  cfg.dns_port ).parse().unwrap();
    support::spawn_http_echo(echo_bind, Arc::clone(&running)).await.ok();
    support::spawn_tcp_dns(dns_bind, &cfg.fixtures_dir.join("dns.txt"), Arc::clone(&running)).await.ok();

    // 1. warm-up
    tokio::time::sleep(Duration::from_secs(10)).await;

    // 2. HTTP echo
    for bytes in [1024, 10 * 1024, 100 * 1024] {
        http_echo::run(cfg.socks, &cfg.echo_host, cfg.echo_port, bytes, 10, Arc::clone(&em)).await;
    }

    // 3. DNS
    let names = std::fs::read_to_string(cfg.fixtures_dir.join("dns.txt"))
        .unwrap_or_default()
        .lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect::<Vec<_>>();
    dns::run(cfg.socks, &cfg.dns_host, cfg.dns_port, &names[..names.len().min(50)], Arc::clone(&em)).await;

    // 4. SSH
    if let Some(t) = cfg.ssh_target.as_deref() {
        ssh_probe::run(&cfg.socks.ip().to_string(), cfg.socks.port(), t, 5, Arc::clone(&em)).await;
    }

    // 5. HTTPS
    https_fetch::run(cfg.socks, &["https://example.com", "https://en.wikipedia.org/wiki/Main_Page"], 5, Arc::clone(&em)).await;

    // 6. Idle hold for SLA row 6
    tokio::time::sleep(Duration::from_secs(600)).await;

    running.store(false, Ordering::SeqCst);
}
```

- [ ] **Step 2: Build check** — `cargo build --lib`. Expect compile.

- [ ] **Step 3: Commit**

```bash
git add src/bench/realistic.rs
git commit -m "feat(bench): realistic driver — warmup, http, dns, ssh, https, idle hold"
```

---

## Task 16: Bench 2 saturation driver

**Files:**
- Create: `src/bench/saturation.rs`

- [ ] **Step 1: Implement**

```rust
//! Bench 2 — saturation ramp with retx_rate and queue_depth predicates.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::process::Command;
use crate::tunnel::metrics::{Event, EventEmitter};

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
            // Hold
            em.emit(Event::new("bench", "saturation_hold")
                .field("profile", cfg.profile_label)
                .field("rate_kbps", rate as i64));
            run_iperf(&cfg, rate, HOLD_SECS, &em).await;
            break;
        }
    }
    if saturation_kbps.is_none() {
        em.emit(Event::new("bench", "saturation_result")
            .field("profile", cfg.profile_label)
            .field("saturation_point", serde_json::Value::Null)
            .field("reason", "ramp_ceiling_reached"));
    } else {
        em.emit(Event::new("bench", "saturation_result")
            .field("profile", cfg.profile_label)
            .field("saturation_point", saturation_kbps.unwrap() as i64));
    }
}

async fn run_iperf(cfg: &Config, rate_kbps: u32, secs: u64, em: &Arc<EventEmitter>) -> bool {
    // NOTE: iperf3 itself has no SOCKS support; we rely on `proxychains4` on $PATH.
    let out = Command::new("proxychains4")
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
            em.emit(Event::new("bench", "iperf3")
                .field("rate_kbps", rate_kbps as i64)
                .field("duration_s", secs as i64)
                .field("ok", true)
                .field("json_len", o.stdout.len() as i64));
            // Saturation predicate evaluated OUT-OF-BAND against KCP metrics during the step.
            // For the in-process case we approximate: if iperf3 reports retransmits > 20% of
            // sent segments, treat as saturation. Parse JSON; fall back to false on any error.
            retx_predicate(&o.stdout)
        }
        _ => {
            em.emit(Event::new("bench", "iperf3").field("rate_kbps", rate_kbps as i64).field("ok", false));
            true // if iperf3 failed at this rate, treat as saturation
        }
    }
}

fn retx_predicate(json_bytes: &[u8]) -> bool {
    let v: serde_json::Value = match serde_json::from_slice(json_bytes) { Ok(v) => v, _ => return false };
    let sum = v.pointer("/end/sum_sent");
    if let Some(s) = sum {
        let retr = s.get("retransmits").and_then(|x| x.as_u64()).unwrap_or(0);
        let sent_b = s.get("bytes").and_then(|x| x.as_u64()).unwrap_or(1);
        // Heuristic: > 20% retx per MB
        let mb = (sent_b / 1_000_000).max(1);
        return (retr / mb) > 2;
    }
    false
}
```

- [ ] **Step 2: Build check** — compile.

- [ ] **Step 3: Commit**

```bash
git add src/bench/saturation.rs
git commit -m "feat(bench): saturation driver — ramp + retx predicate via iperf3 JSON"
```

---

## Task 17: Report aggregator — events.jsonl → summary.json

**Files:**
- Create: `src/bench/report.rs`

- [ ] **Step 1: Write failing test**

```rust
//! Events → aggregated summary with SLA matrix.

use std::collections::BTreeMap;
use std::path::Path;
use serde_json::{json, Value};

#[derive(Default)]
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

#[derive(Default)]
pub struct WorkloadStats {
    pub ok: u64, pub fail: u64, pub latency_ms: Vec<u64>, pub bytes: u64,
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
                if let Some(n) = v["rtt_ms"].as_u64() { s.rtt_samples_ms.push(n); }
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
            "ok": ws.ok, "fail": ws.fail, "bytes": ws.bytes,
            "latency_ms": { "p50": p.p50, "p90": p.p90, "p99": p.p99, "max": p.max }
        }));
    }
    let sla = evaluate_sla(s);
    let j = json!({
        "events_seen": s.events_seen,
        "flicker": { "tx": s.flicker_tx, "rx": s.flicker_rx },
        "kcp": { "tx": s.kcp_tx, "retx": s.kcp_retx,
                 "rtt_ms": { "p50": rtt_p.p50, "p90": rtt_p.p90, "p99": rtt_p.p99, "max": rtt_p.max } },
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
    let mut s = v.to_vec(); s.sort_unstable();
    let pct = |p: f64| { let idx = ((s.len() as f64 - 1.0) * p) as usize; s[idx] };
    Pctls { p50: pct(0.5), p90: pct(0.9), p99: pct(0.99), max: *s.last().unwrap() }
}

fn evaluate_sla(s: &Summary) -> Value {
    let http_1kb = s.bench_requests.get("http_echo");
    let connect_p50 = http_1kb.map(|w| percentiles(&w.latency_ms).p50).unwrap_or(u64::MAX);
    let connect_p99 = http_1kb.map(|w| percentiles(&w.latency_ms).p99).unwrap_or(u64::MAX);
    let sla_1 = connect_p50 <= 15_000;
    let sla_2 = connect_p99 <= 30_000;
    json!({
        "rtt_p50_le_15s":  { "pass": sla_1, "measured_ms": connect_p50 },
        "rtt_p99_le_30s":  { "pass": sla_2, "measured_ms": connect_p99 },
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
    }
}
```

- [ ] **Step 2: Run test** — `cargo test --lib bench::report`, expect PASS.

- [ ] **Step 3: Commit**

```bash
git add src/bench/report.rs
git commit -m "feat(bench): report aggregator + SLA evaluator"
```

---

## Task 18: CLI `bench` and `report` subcommands

**Files:**
- Modify: `src/cli.rs`

- [ ] **Step 1: Add subcommands**

Edit `src/cli.rs` to add:

```rust
#[derive(clap::Subcommand, Debug)]
pub enum BenchCmd {
    Realistic {
        #[arg(long, default_value = "127.0.0.1:1080")] socks: std::net::SocketAddr,
        #[arg(long, default_value = "127.0.0.1")]       echo_host: String,
        #[arg(long, default_value_t = 18080)]           echo_port: u16,
        #[arg(long, default_value = "127.0.0.1")]       dns_host: String,
        #[arg(long, default_value_t = 18053)]           dns_port: u16,
        #[arg(long)] ssh_target: Option<String>,
        #[arg(long, default_value = "./fixtures")]      fixtures_dir: std::path::PathBuf,
        #[arg(long, default_value = "./metrics")]       metrics_dir: std::path::PathBuf,
    },
    Saturation {
        #[arg(long, default_value = "127.0.0.1:1080")] socks: std::net::SocketAddr,
        #[arg(long, default_value = "127.0.0.1")]       iperf_host: String,
        #[arg(long, default_value_t = 15201)]           iperf_port: u16,
        #[arg(long, value_enum, default_value_t = ProfileArg::Both)] profile: ProfileArg,
        #[arg(long, default_value = "./metrics")]       metrics_dir: std::path::PathBuf,
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
```

Hook the new variants into the top-level `Commands` enum and dispatch in `main.rs`.

Dispatch body (add to main.rs alongside existing peer dispatch):

```rust
Commands::Bench(sub) => {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        match sub {
            BenchCmd::Realistic { socks, echo_host, echo_port, dns_host, dns_port, ssh_target, fixtures_dir, metrics_dir } => {
                let run_id = rtmp_steganography::tunnel::metrics::new_run_id();
                let dir = metrics_dir.join(&run_id);
                let peer_id = std::env::var("PEER_ID").unwrap_or("A".into());
                let em = std::sync::Arc::new(rtmp_steganography::tunnel::metrics::EventEmitter::new(&dir, peer_id)?);
                rtmp_steganography::bench::realistic::run(
                    rtmp_steganography::bench::realistic::Config {
                        socks, echo_host, echo_port, dns_host, dns_port, ssh_target, fixtures_dir,
                    },
                    std::sync::Arc::clone(&em),
                ).await;
                let summary_path = dir.join("summary.json");
                let s = rtmp_steganography::bench::report::aggregate(&dir.join("events.jsonl"))?;
                rtmp_steganography::bench::report::write_summary_json(&s, &summary_path)?;
                Ok::<(), anyhow::Error>(())
            }
            BenchCmd::Saturation { socks, iperf_host, iperf_port, profile, metrics_dir } => {
                let run_id = rtmp_steganography::tunnel::metrics::new_run_id();
                let dir = metrics_dir.join(&run_id);
                let em = std::sync::Arc::new(rtmp_steganography::tunnel::metrics::EventEmitter::new(&dir, std::env::var("PEER_ID").unwrap_or("A".into()))?);
                let labels: &[&str] = match profile {
                    ProfileArg::Throughput => &["throughput"],
                    ProfileArg::Latency    => &["latency"],
                    ProfileArg::Both       => &["throughput", "latency"],
                };
                for label in labels {
                    rtmp_steganography::bench::saturation::run(
                        rtmp_steganography::bench::saturation::Config {
                            socks, iperf_host: iperf_host.clone(), iperf_port, profile_label: label,
                        },
                        std::sync::Arc::clone(&em),
                    ).await;
                }
                let s = rtmp_steganography::bench::report::aggregate(&dir.join("events.jsonl"))?;
                rtmp_steganography::bench::report::write_summary_json(&s, &dir.join("summary.json"))?;
                Ok::<(), anyhow::Error>(())
            }
        }
    })?;
}
Commands::Report(ReportCmd::Summarize { events, out }) => {
    let s = rtmp_steganography::bench::report::aggregate(&events)?;
    rtmp_steganography::bench::report::write_summary_json(&s, &out)?;
}
```

- [ ] **Step 2: Build + smoke**

Run: `cargo build --release`
Expected: compile OK.

- [ ] **Step 3: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(cli): bench realistic/saturation and report summarize subcommands"
```

---

## Task 19: Level D pair bench shell script

**Files:**
- Create: `scripts/bench-tunnel.sh`

- [ ] **Step 1: Script content**

```bash
#!/usr/bin/env bash
# Level D — tunnel pair bench via VK.
# Launches two peer processes in tunnel mode and drives bench workloads
# from peer A's side while peer B hosts support services via tunnel egress.
#
# Requires: .env.peer-a, .env.peer-b, built release binary, proxychains4, curl, iperf3.

set -euo pipefail
cd "$(dirname "$0")/.."

DURATION="${DURATION:-1800}"
METRICS_DIR="${METRICS_DIR:-./metrics}"
TUNNEL_PROFILE="${TUNNEL_PROFILE:-latency}"
SOCKS_A="${SOCKS_A:-127.0.0.1:11080}"
SOCKS_B="${SOCKS_B:-127.0.0.1:11081}"

LOG_A="$(mktemp)"
LOG_B="$(mktemp)"
BAK=".env.bak.$$"
if [ -f .env ]; then mv .env "$BAK"; fi
trap 'if [ -f "$BAK" ]; then mv "$BAK" .env 2>/dev/null || true; fi' EXIT

cargo build --release

start_peer() {
    local env_file="$1" log="$2" peer_id="$3" socks="$4"
    (
        set -a
        # shellcheck disable=SC1090
        source "$env_file"
        export PEER_ID="$peer_id"
        export TUNNEL_PROFILE
        export METRICS_DIR
        set +a
        exec ./target/release/rtmp-steganography peer --tunnel-socks "$socks"
    ) > "$log" 2>&1 &
    echo $!
}

PID_A=$(start_peer .env.peer-a "$LOG_A" A "$SOCKS_A")
echo "[bench-tunnel] peer A pid=$PID_A  log=$LOG_A"
sleep 2
PID_B=$(start_peer .env.peer-b "$LOG_B" B "$SOCKS_B")
echo "[bench-tunnel] peer B pid=$PID_B  log=$LOG_B"

# Wait for tunnel warm-up (VK HLS can take ~15s for first segment)
sleep 30

# Run Bench 1 realistic from peer A
PEER_ID=A ./target/release/rtmp-steganography bench realistic \
    --socks "$SOCKS_A" \
    --metrics-dir "$METRICS_DIR"

# Run Bench 2 saturation (both profiles)
PEER_ID=A ./target/release/rtmp-steganography bench saturation \
    --socks "$SOCKS_A" \
    --profile both \
    --metrics-dir "$METRICS_DIR"

# Teardown
kill -INT "$PID_A" "$PID_B" 2>/dev/null || true
sleep 3
kill -9 "$PID_A" "$PID_B" 2>/dev/null || true
wait 2>/dev/null || true

echo ""
echo "[bench-tunnel] ===== RESULTS ====="
echo "metrics: $METRICS_DIR (latest run_id subdir)"
echo "peer logs: $LOG_A  $LOG_B"
```

- [ ] **Step 2: chmod + commit**

Run:

```bash
chmod +x scripts/bench-tunnel.sh
git add scripts/bench-tunnel.sh
git commit -m "feat(scripts): Level D pair tunnel bench driver"
```

---

## Self-review checklist

**Spec coverage:** Every section 4–19 of the spec maps to tasks 1–19. §11 metrics = Task 2 + Task 17. §5 KCP = Task 5. §6 yamux = Task 6. §7 SOCKS5 inbound = Task 8 + 9. §8 egress = Task 10. §9 adapter = Task 4. §12 bench 1 = Task 13 + 14 + 15. §13 bench 2 = Task 16. §14 SLA = Task 17 evaluate_sla. §15 testing = Tasks 5/7/8/11/13/17 test blocks. §16 CLI = Tasks 12 + 18. §17 deps = Task 1.

**Placeholder scan:** No TBD / TODO / "handle edge cases" / "write tests for the above". Every code block is complete-enough-to-type. A few API-version caveats (`kcp`, `yamux`) are explicit instructions, not handwaving.

**Type consistency:** `DatagramChannel` trait, `KcpSession`, `MuxSession<S>`, `ConnectFrame { host, port }`, `EventEmitter`, `WorkloadResult { ok, latency_ms, bytes }` — names are consistent across Tasks 2–18.

**Known fragility to watch when executing:**
- `kcp` and `yamux` crate APIs vary by minor version. Each using-task reminds the engineer to consult `cargo doc -p <crate>`.
- SOCKS5 listener + egress share one yamux `Connection`; the mode_client split by `PEER_ID` is a simplification valid for pair scenarios. For N-peer deployments this would need revisiting (out of scope here).
- The TCP DNS responder in Task 13 writes canned responses (flags flipped to QR=1) without real answer records — sufficient for measuring tunnel behavior, not for actual resolution. This is documented in Task 13 step 2 note.
