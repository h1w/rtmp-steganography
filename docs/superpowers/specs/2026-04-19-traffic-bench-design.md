# Traffic Tunnel & Benchmarks over Flicker

**Date:** 2026-04-19
**Status:** Spec — awaiting user review
**Scope:** Build a reliable, bidirectional SOCKS5 tunnel over the existing flicker/RTMP/VK covert channel, plus two benchmarks (realistic workload, saturation) with full structured metrics.

---

## 1. Motivation

The repo today carries application messages through a flicker-modulated video stream on VK Live. The pair smoke test (`scripts/e2e-vk-pair.sh`) measures delivery by grepping heartbeat logs — a single metric, per direction, with no notion of TCP, sessions, retransmit, or goodput.

We want two deliverables:

1. **A real traffic tunnel.** Accept arbitrary TCP traffic (via a SOCKS5 entry point) on one peer, carry it across the flicker channel, egress on the other peer, return the reply. That is: `curl`, `iperf3`, `ssh -D`, browsers, `dig` all work end-to-end through VK video.
2. **Two benchmarks with full metrics.** One that emulates realistic mixed traffic (HTTP, DNS, SSH, small HTTPS). One that drives the channel to saturation and records where and how it breaks.

All metrics (flicker / KCP / yamux / SOCKS5 / workload) are emitted as structured events to `events.jsonl` with an aggregated `summary.json` per run.

## 2. Channel physics (constraints that shape every decision)

These are measured properties of the flicker-over-VK channel, not assumptions:

| Property | Value |
|---|---|
| One-way frame latency (p50 / p99) | ≈ 4s / ≈ 10s |
| Raw flicker frame delivery | ≈ 80–85% per direction |
| Goodput ceiling (post-FEC) | tens of kbit/s |
| Ordering | none (frame-scoped) |
| Retransmit | none (flicker is UDP-like) |
| Bidirectional mechanism | two one-way streams, one per peer |

Anything built on top must tolerate 4-second round-trips, 15–20% per-frame loss, and must implement its own reliability and ordering if TCP sessions are to survive.

## 3. Non-goals

Explicitly out of scope for this spec (future work, separate specs):

- sing-box custom transport integration and the full protocol zoo (Trojan, VMess, VLESS, Hysteria, TUIC, AnyTLS, Wireguard, NaïveProxy, Juicity, ShadowTLS, etc.).
- UDP ASSOCIATE in SOCKS5. DNS uses TCP mode in v1.
- HTTP CONNECT proxy inbound.
- Runtime profile switching (throughput ↔ latency).
- Video / streaming workloads — the channel cannot sustain them.
- Modifying the flicker/FEC layer itself.

## 4. Architecture

### 4.1 Layer stack

```
[curl / iperf3 / dig / ssh -D / browser]
        │ TCP  (proxychains wraps clients that don't speak SOCKS natively)
        ▼
[SOCKS5 listener @ 127.0.0.1:1080]
        │ one yamux stream per inbound connection
        ▼
[yamux session]                    ← N:1 stream multiplex, flow control
        │ framed bytes
        ▼
[KCP session]                      ← ARQ, ordering, adaptive presets
        │ datagrams
        ▼
[flicker tunnel adapter]
        │ OutboundMessage { msg_type=0x02, payload=kcp_datagram }
        ▼
[existing flicker encoder → RTMP → VK → flicker decoder]
        │ reverse path
        ▼
[peer B: adapter → KCP → yamux accept → egress]
        │ TCP
        ▼
[target host:port]
```

Every peer runs the full stack. Who is "client" vs "server" is decided per yamux stream: the side that opens a stream is the SOCKS5 inbound; the accepting side is the egress worker.

### 4.2 Module layout

```
src/tunnel/
    mod.rs              public API, layer glue, supervisor
    kcp.rs              KCP session wrapper, presets, tick driver
    mux.rs              yamux session wrapper, stream lifecycle
    socks5.rs           SOCKS5 inbound (auth=none, CONNECT)
    egress.rs           TCP egress worker
    framing.rs          CONNECT request frame (host, port) — internal
    flicker_adapter.rs  KCP datagrams ↔ flicker msg_type=0x02
    metrics.rs          structured event emitter, counters, histograms
src/bench/
    mod.rs              bench harness, supervisor
    realistic.rs        Bench 1 driver
    saturation.rs       Bench 2 driver
    workloads/
        http_echo.rs
        dns.rs
        ssh_probe.rs
        https_fetch.rs
    report.rs           events.jsonl → summary.json aggregator
src/cli.rs              + `tunnel` and `bench` subcommands
src/peer/app.rs         branch: heartbeat mode vs tunnel mode
```

### 4.3 Integration with the existing peer

- `msg_type=0x01` stays `time_sync` (the current heartbeat). Unchanged.
- `msg_type=0x02` is `TUNNEL_DATAGRAM`: payload is a raw KCP datagram.
- A peer runs in exactly one mode per process lifetime — heartbeat **or** tunnel — selected by CLI flag. No hybrid v1.
- The existing `run_default` path in `src/peer/app.rs` becomes a branch:
  - `peer --heartbeat` (default, current behavior).
  - `peer --tunnel-socks 127.0.0.1:1080` (new).

## 5. Reliability layer — KCP

### 5.1 Why KCP

KCP is a selective-repeat ARQ designed for high-loss, high-latency datagram links. It is parameterized (window, RTO bounds, fast-retransmit thresholds, nodelay/nc flags), which is what lets us expose two distinct profiles. Rust crates (`kcp`) are available.

### 5.2 Adaptive presets

Selected at process start via env var `TUNNEL_PROFILE` (default `latency`):

| Parameter | `throughput` | `latency` |
|---|---|---|
| `snd_wnd` / `rcv_wnd` | 256 / 256 | 32 / 32 |
| `nodelay` | 0 | 1 |
| `interval` (ms) | 40 | 10 |
| `resend` (fast-retx threshold) | 0 | 2 |
| `nc` (no-congestion) | 0 | 1 |
| `rx_minrto` (ms) | 200 | 100 |

Both profiles are exercised by both benchmarks. `summary.json` contains a per-profile block for comparison.

### 5.3 MTU

KCP's per-datagram MTU is derived at runtime:

```
kcp_mtu = flicker_max_payload_bytes − kcp_header (24) − yamux_header (12)
```

`flicker_max_payload_bytes` is the per-frame application payload capacity of the current flicker profile. The adapter reads it at startup from a `pub const` in `src/flicker/mod.rs` (exporting this constant is an implementation prerequisite; it is not exposed today). The adapter asserts the MTU at startup and panics if `kcp_mtu <= 0`.

### 5.4 Clock driver

A single dedicated OS thread calls `ikcp_update` every 10 ms. The interval is tight enough to serve the `latency` preset's 10 ms tick and cheap enough at throughput scale (≤ 0.1% CPU).

## 6. Multiplex layer — yamux

Yamux provides per-stream flow control and FIN semantics on top of KCP's reliable byte stream.

- One logical yamux stream per SOCKS5 client connection.
- The first frame on each stream is the internal CONNECT request (see §7).
- Yamux `RST` propagates to the client as TCP RST.
- Yamux `GO_AWAY` tears down all streams on hard KCP failure.

## 7. Inbound — SOCKS5

- Version: SOCKS5 only.
- Auth: `0x00` (no authentication) in v1.
- Commands supported: `CONNECT` (TCP).
- Commands rejected: `BIND`, `UDP ASSOCIATE` → reply `0x07` (command not supported).
- Address types: `IPv4`, `IPv6`, `DOMAIN`.
- Error codes mapped from egress outcomes:
  - Egress DNS failure → `0x04` (host unreachable).
  - Egress TCP connect refused → `0x05` (connection refused).
  - Egress timeout → `0x06` (TTL expired).
  - Tunnel down (KCP timeout propagated up) → `0x03` (network unreachable).

### 7.1 Internal CONNECT frame

After SOCKS5 negotiation but before any user bytes, the listener writes one internal frame on the yamux stream:

```
CONNECT {
    version: u8 = 1,
    host_len: u8,
    host: [u8; host_len],   // UTF-8, matches SOCKS5 DOMAIN semantics
    port: u16 be,
}
```

The egress reads exactly one CONNECT frame, attempts the TCP dial, then enters raw byte-pump mode.

## 8. Outbound — Egress

- Accepts each incoming yamux stream, reads the CONNECT frame, dials target.
- On success: 64 KB buffers, bidirectional pump, half-close on either direction's EOF.
- On failure: write a 1-byte status reply (`0=ok, >0=error code`) back on the stream, then close. Listener translates the code to SOCKS5 error.
- Idle read timeout on target TCP: 300 s. Exceeding → close stream.

## 9. Flicker adapter

Thin bidirectional bridge:

- **Out:** KCP emits a datagram → wrap as `OutboundMessage { msg_type=0x02, payload }` → existing flicker encoder.
- **In:** existing flicker decoder delivers `InboundMessage { msg_type=0x02, payload }` → feed to KCP `input()`.

The adapter MUST NOT modify the payload. Any fragmentation is KCP's responsibility (via MTU).

## 10. Error handling

| Failure | Detection | Response |
|---|---|---|
| SOCKS5 malformed request | Parse error | Close client TCP. `socks5.error` event. |
| Egress DNS / TCP failure | `connect()` result | Status byte → listener → SOCKS5 reply. |
| KCP send-buffer full | `ikcp_send` return | Back-pressure yamux; after 10 s emit `tunnel_congested`. |
| KCP no ACK for 60 s | RTO exhaustion | `tunnel_dead` → yamux GO_AWAY, all streams reset. |
| Flicker rx silent ≥ 120 s | Adapter watchdog | `tunnel_degraded` event, yamux paused (not torn down). |
| Tunnel thread panic | Thread join handle | Process exits non-zero; bench supervisor logs and aborts the run. |

## 11. Metrics

### 11.1 Output files

Per run, `$METRICS_DIR/<run_id>/`:

- `events.jsonl` — append-only, one JSON object per event line.
- `summary.json` — written once on clean shutdown.

Default `METRICS_DIR=./metrics`, `run_id = UTC ISO8601 + short random tag`.

### 11.2 Event schema

```json
{ "ts_ns": 1712345678900000000,
  "peer_id": "A",
  "layer":  "kcp",
  "event":  "retx",
  "seq":    4711,
  "attempt": 2 }
```

All events share `ts_ns`, `peer_id`, `layer`, `event`. Additional keys are event-specific.

### 11.3 Event catalogue

| Layer | Events |
|---|---|
| `flicker` | `tx`, `rx`, `drop{reason}` |
| `kcp` | `tx{seq,len}`, `retx{seq,attempt}`, `ack{seq}`, `rtt_sample{rtt_ms}`, `wnd{snd,rcv}`, `congested`, `dead` |
| `yamux` | `stream_open{id}`, `stream_close{id,reason}`, `window_update{id}` |
| `socks5` | `connect{target}`, `reply{code}`, `error{code}` |
| `bench` | `request_start{workload,id}`, `request_done{workload,id,ok,latency_ms,bytes}` |

### 11.4 Summary shape

`summary.json` keys:

- `run`: id, duration, peer_role, profile
- `sla`: per-row pass/fail and measured vs target (see §14)
- `flicker`: counts, delivery%, per-drop-reason breakdown
- `kcp`: tx / ack / retx counts, retx_rate, rtt histograms (p50/p90/p99/max), congested / dead events
- `yamux`: stream open/close counts, errors
- `socks5`: connects, replies by code
- `bench.realistic`: per-workload success%, latency percentiles, goodput, stall count
- `bench.saturation`: per-profile saturation point (kbps), retx_rate at saturation, queue_depth curve

## 12. Benchmark 1 — realistic

### 12.0 Support services (both peers run these during a bench)

Peer B brings up three local loopback services that are reachable via the tunnel egress:

- **HTTP echo server** on `127.0.0.1:18080` — echoes POST body.
- **Minimal TCP-DNS responder** on `127.0.0.1:18053` — answers from the fixtures file.
- **iperf3 server** on `127.0.0.1:15201` (Bench 2 only).

These are started by the bench harness inside the peer process (not external binaries) so egress targets are deterministic. Fixture files live in `fixtures/dns.txt` at repo root.

### 12.1 Sequence

Driver runs in a fixed sequence; total ≈ 16 minutes per profile (6 min active + 10 min idle hold).

1. **Warm-up.** 10 s idle (existing rx warm-up convention).
2. **HTTP echo.** Client: `curl --socks5 127.0.0.1:1080 -X POST --data-binary @payload.bin http://127.0.0.1:18080/echo` (DNS is short-circuited via IP). Payloads: 1 KB, 10 KB, 100 KB. 10 iterations each.
3. **DNS.** 50 lookups (TCP mode) via `proxychains4 dig @127.0.0.1 -p 18053 +tcp <name>`. Names drawn from `fixtures/dns.txt` (50 entries).
4. **SSH probe.** `ssh -o ProxyCommand="nc -X 5 -x 127.0.0.1:1080 %h %p" user@<peerB-sshd-host> 'echo ok'`, 5 iterations, 30 s timeout each. `peerB-sshd-host` is configured via env `BENCH_SSH_TARGET`; step is skipped if unset.
5. **HTTPS.** `curl --socks5 127.0.0.1:1080 https://example.com` and `https://en.wikipedia.org/wiki/Main_Page`, 5 iterations each, 60 s timeout. Requires real internet reachability from peer B.
6. **Idle hold.** 10 minutes — tunnel stays open with no workload. This is the dedicated measurement window for SLA row 6 (idle survivability).
7. **Report.** `report.rs` consumes `events.jsonl`, writes `summary.json`.

## 13. Benchmark 2 — saturation

Driver runs both KCP profiles sequentially; total ≈ 25 minutes per full run.

1. **Warm-up.** 10 s idle.
2. **iperf3 server** on peer B (reachable through tunnel target list).
3. **Client:** `proxychains4 iperf3 -c peerB -t 30 -b <rate>k -J` (iperf3 has no native SOCKS). TCP mode only.
4. **Ramp:** 10, 20, 50, 100, 200 kbps. 30 s each.
5. **Saturation predicate (evaluated every 5 s over a 30 s window):**
   - `retx_rate > 20%` for 30 s continuously, **OR**
   - `kcp_send_queue_depth` strictly increasing for 30 s.
6. **Hold:** on first trigger, hold the current rate for 60 s to gather a stable saturation sample.
7. **Unsaturated outcome:** if the ramp completes (200 kbps) without the predicate firing, record `saturation_point: null, saturation_reason: "ramp_ceiling_reached"`. This is a report finding, not a failure — it means the channel tolerated the full test range.
8. **Repeat** with the other profile.
9. **Report.** Per profile: saturation rate, retx distribution at saturation, queue-depth curve PNG (optional, matplotlib if available).

## 14. Success criteria (SLA)

| # | Metric | Target | Measured in |
|---|---|---|---|
| 1 | TCP handshake (connect → first byte) through tunnel, p50 | ≤ 15 s | Bench 1 HTTP echo 1 KB |
| 2 | Same, p99 | ≤ 30 s | Bench 1 HTTP echo 1 KB |
| 3 | Goodput stable (bulk), per direction | ≥ 20 kbit/s | Bench 2 sustained 60 s window |
| 4 | Goodput peak (burst) | ≥ 50 kbit/s | Bench 2 pre-saturation max |
| 5 | Reliability at tunnel layer (post-ARQ) | ≥ 99.9% bytes | Bench 1 HTTP echo 100 KB |
| 6 | Idle tunnel survivability | ≥ 10 min no teardown | Bench 1 warm-up + inter-test idle |
| 7 | Flicker-layer delivery | unchanged, ≥ 80% | always-on |

Each SLA row must be satisfied by **at least one** of the two profiles (`throughput` or `latency`). Profiles are evaluated independently; `summary.json` records pass/fail per profile per row. A row failing on *both* profiles is a release blocker.

## 15. Testing strategy

**Unit tests** (pure Rust, `cargo test`):
- KCP preset loader and parameter application.
- SOCKS5 handshake parser (positive + malformed inputs).
- Internal CONNECT frame encode/decode.
- JSON-lines event emitter.
- `report.rs` aggregator (percentile math, SLA matrix).

**Integration — in-process, simulated channel** (`tests/tunnel_sim.rs`):
- `flicker_adapter` is swappable; a test-only `MemLossyChannel { loss%, latency_ms, jitter_ms }` stands in for flicker.
- Full SOCKS5 → local echo roundtrip at configurable loss/latency.
- Matrix: {0%, 10%, 20%} loss × {10 ms, 1 s, 4 s} latency.
- Asserts: tunnel delivers 100% of bytes, metrics emit expected counts.

**Integration — pair, in-process** (`tests/tunnel_pair_sim.rs`):
- Two peer runtimes in one process connected by `MemLossyChannel`.
- Runs a trimmed Bench 1 (1 iteration per workload) and Bench 2 (10 kbps only).
- Asserts: `summary.json` shape; SLA matrix is populated; no panics.

**End-to-end — Level D (real VK):**
- `scripts/bench-tunnel.sh` — two peers, real VK, 60 minutes.
- Exit 0 only if all SLA rows pass on at least one profile.

## 16. CLI surface

```
rtmp-steganography peer --heartbeat            # existing behavior
rtmp-steganography peer --tunnel-socks <addr>  # new: run as tunnel peer

rtmp-steganography bench realistic   [--profile throughput|latency] [--metrics-dir DIR]
rtmp-steganography bench saturation  [--profile both|throughput|latency]

rtmp-steganography report summarize <events.jsonl> --out <summary.json>
```

Env vars:

- `TUNNEL_PROFILE` — default profile when `--profile` is omitted.
- `METRICS_DIR` — base dir for per-run output.
- `SOCKS5_LISTEN` — alternate default for `--tunnel-socks`.
- `PEER_ID` — single-letter label (`A` or `B`) stamped on every event for post-run correlation. Falls back to the existing peer env (`peer_my_stream_key` hash) if unset.
- `BENCH_SSH_TARGET` — `user@host` for Bench 1 SSH probe; step is skipped if unset.

## 17. Dependencies (new)

- `kcp` — reliability layer.
- `yamux` — mux.
- `serde_json` (already present) — event emission and summary.
- `hdrhistogram` — latency percentiles without full-sample retention.

No new runtime deps for SOCKS5 — hand-rolled parser (~200 LOC, well-specified protocol).

## 18. Open / deferred items

- Full sing-box custom-transport plugin — next spec.
- UDP ASSOCIATE — next spec.
- Runtime profile switch — not planned.
- Graphical output beyond optional PNG — not planned.
- Traffic shaping on flicker side (pacing policies) — out of scope; inherit whatever v2 flicker does.
