# Flicker Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the single-binary RTMP publisher into a dual-mode `rtmp-steganography.exe` (`--client` publishes, `--server` auto-resolves a VK Live stream and decodes timestamp frames with Δms latency logging). Extract the steganography codec into a named `flicker` module.

**Architecture:** One binary with clap subcommands + top-level alias flags dispatches to `client::run` or `server::run`. The `flicker` module owns the grid codec (bit plane ↔ RGB24 frame). The server ports `vk_live.rs` from `docs/reference/vkvideo`, drains ffmpeg output with low-latency flags, decodes each RGB24 frame, and logs latency. Auto-reconnect on stream failure (signed URL expiry, CDN errors) with bounded backoff.

**Tech Stack:** Rust 2021, clap v4 (derive), reqwest (blocking + rustls-tls), serde_json, dotenvy, ctrlc, anyhow. External: ffmpeg on PATH. No crypto / VPN / random-word deps from the reference project.

---

## File Structure

**New/modified Rust sources:**

```
src/
  main.rs                — dispatch CLI → client::run | server::run
  cli.rs                 — clap Args; subcommands + --client/--server alias flags
  config.rs              — .env loader: grid, rtmp, vk slug, http headers
  flicker/
    mod.rs               — re-exports public API
    grid.rs              — GridConfig, validation, WIDTH/HEIGHT/FPS constants
    codec.rs             — paint_bit_into_frame / read_bit_from_cell
    frame.rs             — encode_timestamp_frame / decode_timestamp_frame
  client/
    mod.rs               — run(): spawn ffmpeg publish, frame generation loop
    ffmpeg.rs            — RTMP publish arg builder
  server/
    mod.rs               — run(): resolve URL → ingest → decode loop + reconnect
    vk_live.rs           — port from reference: resolve_channel, wait_for_playback_ready, probe
    ingest.rs            — ffmpeg read-subprocess (low-latency flags)
    decoder.rs           — read loop: raw RGB24 → flicker::decode → log Δms
tests/
  flicker_roundtrip.rs   — encode → decode roundtrip for multiple cell sizes
```

**Config:**
- `Cargo.toml` — add `clap`, `reqwest`, `serde_json`; bump existing.
- `.env` — append `vk_live_channel`, `stream_read_url`, `stream_referer`, `stream_origin`, `stream_user_agent`, `stream_log_every_frame`.

**Removed:**
- The monolithic `src/main.rs` — replaced by the dispatcher version.

---

## Task 1: Prepare dependencies and .env

**Files:**
- Modify: `Cargo.toml`
- Modify: `.env`

- [ ] **Step 1: Update `Cargo.toml` dependencies**

Replace the `[dependencies]` block with:

```toml
[dependencies]
anyhow = "1"
dotenvy = "0.15"
ctrlc = "3"
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }
serde_json = "1"
```

- [ ] **Step 2: Run `cargo check` to fetch deps**

Run: `cargo check`
Expected: Warnings about unused main.rs code are OK; must compile cleanly otherwise.

- [ ] **Step 3: Append server config to `.env`**

Append to `.env` (keep existing `stream_key`, `rtmp_server`, `cell_size`, `update_every_frames`):

```
# --- server (receive + decode) ---
# VK Live channel slug from https://live.vkvideo.ru/<slug>
vk_live_channel = "pavel8899"

# Optional: explicit MPD/HLS URL (overrides vk_live_channel resolve)
# stream_read_url =

# Optional HTTP headers for okcdn (VK Live CDN)
# stream_referer =
# stream_origin =
# stream_user_agent =

# Log every frame (default: log only on ts change)
# stream_log_every_frame = 1

# VK resolve retry tuning (seconds)
# VK_LIVE_WAIT_INTERVAL_SECS = 5
# VK_LIVE_WAIT_TIMEOUT_SECS = 0
# VK_LIVE_SKIP_PROBE = 0
```

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock .env
git commit -m "chore: add clap/reqwest/serde_json deps and server .env keys"
```

---

## Task 2: Create `flicker::grid` module

**Files:**
- Create: `src/flicker/mod.rs`
- Create: `src/flicker/grid.rs`

- [ ] **Step 1: Create `src/flicker/grid.rs`**

```rust
use anyhow::{anyhow, Result};

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 144;
pub const FPS: u32 = 30;
pub const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;

#[derive(Clone, Debug)]
pub struct GridConfig {
    pub cell: usize,
    pub cols: usize,
    pub rows: usize,
    pub total_cells: usize,
    pub update_every: u64,
}

impl GridConfig {
    pub fn new(cell: usize, update_every: u64) -> Result<Self> {
        if cell == 0 {
            return Err(anyhow!("cell_size must be > 0"));
        }
        if WIDTH % cell != 0 || HEIGHT % cell != 0 {
            return Err(anyhow!(
                "cell_size={cell} must divide both WIDTH={WIDTH} and HEIGHT={HEIGHT} evenly \
                 (valid values: 1, 2, 4, 8, 16)"
            ));
        }
        if update_every == 0 {
            return Err(anyhow!("update_every_frames must be >= 1"));
        }
        let cols = WIDTH / cell;
        let rows = HEIGHT / cell;
        Ok(Self {
            cell,
            cols,
            rows,
            total_cells: cols * rows,
            update_every,
        })
    }
}
```

- [ ] **Step 2: Create `src/flicker/mod.rs`**

```rust
pub mod codec;
pub mod frame;
pub mod grid;

pub use grid::{GridConfig, FPS, FRAME_BYTES, HEIGHT, WIDTH};
```

- [ ] **Step 3: Run `cargo check` (will fail: codec/frame missing)**

Run: `cargo check`
Expected: Compilation errors for missing `codec` and `frame` modules — proceed to Task 3.

---

## Task 3: Create `flicker::codec` module (bit painter/reader)

**Files:**
- Create: `src/flicker/codec.rs`

- [ ] **Step 1: Create `src/flicker/codec.rs`**

```rust
use crate::flicker::grid::{GridConfig, WIDTH};

pub fn paint_bit_into_frame(buf: &mut [u8], cfg: &GridConfig, bit_idx: usize, bit: u8) {
    if bit == 0 {
        return;
    }
    let cx = bit_idx % cfg.cols;
    let cy = bit_idx / cfg.cols;
    let x0 = cx * cfg.cell;
    let y0 = cy * cfg.cell;
    for y in y0..y0 + cfg.cell {
        let row_start = (y * WIDTH + x0) * 3;
        let row_end = row_start + cfg.cell * 3;
        buf[row_start..row_end].fill(255);
    }
}

pub fn read_bit_from_cell(buf: &[u8], cfg: &GridConfig, bit_idx: usize) -> u8 {
    let cx = bit_idx % cfg.cols;
    let cy = bit_idx / cfg.cols;
    let x0 = cx * cfg.cell;
    let y0 = cy * cfg.cell;
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    for y in y0..y0 + cfg.cell {
        for x in x0..x0 + cfg.cell {
            let idx = (y * WIDTH + x) * 3;
            sum += buf[idx] as u64 + buf[idx + 1] as u64 + buf[idx + 2] as u64;
            count += 3;
        }
    }
    let mean = (sum / count.max(1)) as u8;
    if mean > 127 { 1 } else { 0 }
}
```

- [ ] **Step 2: Run `cargo check`**

Run: `cargo check`
Expected: Still fails because `frame` module missing — proceed to Task 4.

---

## Task 4: Create `flicker::frame` module with roundtrip test

**Files:**
- Create: `src/flicker/frame.rs`
- Create: `tests/flicker_roundtrip.rs`

- [ ] **Step 1: Write failing roundtrip test `tests/flicker_roundtrip.rs`**

```rust
use rtmp_steganography::flicker::frame::{decode_timestamp_frame, encode_timestamp_frame};
use rtmp_steganography::flicker::grid::GridConfig;
use rtmp_steganography::flicker::{FRAME_BYTES};

fn case(cell: usize, ts: u64) {
    let cfg = GridConfig::new(cell, 1).expect("valid grid");
    let mut buf = vec![0u8; FRAME_BYTES];
    encode_timestamp_frame(&mut buf, ts, &cfg);
    let decoded = decode_timestamp_frame(&buf, &cfg);
    // total_cells may be < 64 (e.g. cell=16 → 144 cells, still >= 64); timestamp is
    // encoded using min(total_cells, 64) MSBs. For cell in {1,2,4,8,16} we always
    // have total_cells >= 144 so the full 64-bit ts roundtrips.
    assert_eq!(decoded, ts, "cell={cell} ts={ts:#x}");
}

#[test]
fn roundtrip_cell_16() {
    case(16, 0x0123_4567_89ab_cdef);
}

#[test]
fn roundtrip_cell_8() {
    case(8, 0xdead_beef_1234_5678);
}

#[test]
fn roundtrip_cell_4() {
    case(4, 0xffff_ffff_ffff_fffe);
}

#[test]
fn roundtrip_cell_2() {
    case(2, 1);
}

#[test]
fn roundtrip_zero() {
    case(16, 0);
}

#[test]
fn invalid_cell_rejected() {
    assert!(GridConfig::new(3, 1).is_err());
    assert!(GridConfig::new(0, 1).is_err());
    assert!(GridConfig::new(16, 0).is_err());
}
```

- [ ] **Step 2: Create minimal `src/lib.rs` exposing the module**

Create `src/lib.rs`:

```rust
pub mod flicker;
```

- [ ] **Step 3: Run the failing test**

Run: `cargo test --test flicker_roundtrip`
Expected: Compile error or FAIL — `frame` module / functions not defined yet.

- [ ] **Step 4: Create `src/flicker/frame.rs`**

```rust
use crate::flicker::codec::{paint_bit_into_frame, read_bit_from_cell};
use crate::flicker::grid::GridConfig;
use crate::flicker::FRAME_BYTES;

pub fn encode_timestamp_frame(buf: &mut [u8], ts_ns: u64, cfg: &GridConfig) {
    assert!(buf.len() >= FRAME_BYTES);
    buf.fill(0);
    let ts_bits = cfg.total_cells.min(64);
    for bit_idx in 0..ts_bits {
        let bit = ((ts_ns >> (ts_bits - 1 - bit_idx)) & 1) as u8;
        paint_bit_into_frame(buf, cfg, bit_idx, bit);
    }
}

pub fn decode_timestamp_frame(buf: &[u8], cfg: &GridConfig) -> u64 {
    assert!(buf.len() >= FRAME_BYTES);
    let ts_bits = cfg.total_cells.min(64);
    let mut ts = 0u64;
    for bit_idx in 0..ts_bits {
        let bit = read_bit_from_cell(buf, cfg, bit_idx);
        ts |= (bit as u64) << (ts_bits - 1 - bit_idx);
    }
    ts
}
```

- [ ] **Step 5: Run the tests again**

Run: `cargo test --test flicker_roundtrip`
Expected: All 6 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/flicker tests/flicker_roundtrip.rs
git commit -m "feat(flicker): extract grid/codec/frame modules with roundtrip tests"
```

---

## Task 5: Create `config` loader

**Files:**
- Create: `src/config.rs`

- [ ] **Step 1: Create `src/config.rs`**

```rust
use anyhow::{anyhow, Context, Result};

use crate::flicker::GridConfig;

const DEFAULT_CELL: usize = 16;
const DEFAULT_UPDATE_EVERY: u64 = 5;

pub struct ClientConfig {
    pub rtmp_url: String,
    pub grid: GridConfig,
}

pub struct ServerConfig {
    pub grid: GridConfig,
    pub source: SourceConfig,
    pub http: HttpConfig,
    pub log_every_frame: bool,
}

pub enum SourceConfig {
    /// Explicit URL (MPD/HLS) — overrides VK resolution.
    DirectUrl(String),
    /// Resolve via VK Live API by channel slug.
    VkLiveSlug(String),
}

#[derive(Clone, Default)]
pub struct HttpConfig {
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub origin: Option<String>,
}

pub fn load_grid() -> Result<GridConfig> {
    let cell = env_usize("cell_size")?.unwrap_or(DEFAULT_CELL);
    let update_every = env_u64("update_every_frames")?.unwrap_or(DEFAULT_UPDATE_EVERY);
    GridConfig::new(cell, update_every)
}

pub fn load_client() -> Result<ClientConfig> {
    let key = std::env::var("stream_key").context("stream_key not set in .env")?;
    let server = std::env::var("rtmp_server").context("rtmp_server not set in .env")?;
    let rtmp_url = format!("{}/{}", server.trim_end_matches('/'), key);
    Ok(ClientConfig {
        rtmp_url,
        grid: load_grid()?,
    })
}

pub fn load_server() -> Result<ServerConfig> {
    let grid = load_grid()?;
    let source = if let Some(u) = env_nonempty("stream_read_url") {
        SourceConfig::DirectUrl(u)
    } else if let Some(slug) = env_nonempty("vk_live_channel") {
        SourceConfig::VkLiveSlug(slug)
    } else {
        return Err(anyhow!(
            "set stream_read_url or vk_live_channel in .env"
        ));
    };
    let http = HttpConfig {
        user_agent: env_nonempty("stream_user_agent"),
        referer: env_nonempty("stream_referer"),
        origin: env_nonempty("stream_origin"),
    };
    let log_every_frame = env_flag("stream_log_every_frame");
    Ok(ServerConfig {
        grid,
        source,
        http,
        log_every_frame,
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_usize(name: &str) -> Result<Option<usize>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| {
            format!("{name} is not a valid unsigned integer: {v:?}")
        })?)),
        Err(_) => Ok(None),
    }
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| {
            format!("{name} is not a valid unsigned integer: {v:?}")
        })?)),
        Err(_) => Ok(None),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
```

- [ ] **Step 2: Expose `config` in `src/lib.rs`**

Replace `src/lib.rs` with:

```rust
pub mod config;
pub mod flicker;
```

- [ ] **Step 3: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly (warnings for unused items are fine).

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/config.rs
git commit -m "feat(config): typed loaders for client/server .env settings"
```

---

## Task 6: Create `cli` module with subcommands + alias flags

**Files:**
- Create: `src/cli.rs`

- [ ] **Step 1: Create `src/cli.rs`**

```rust
use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker protocol over RTMP video")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Mode>,

    /// Alias for `client` subcommand.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "server", global = false)]
    pub client: bool,

    /// Alias for `server` subcommand.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "client", global = false)]
    pub server: bool,
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    /// Publish RTMP stream with flicker-encoded timestamps.
    Client,
    /// Receive and decode a flicker-encoded stream.
    Server,
}

pub enum Resolved {
    Client,
    Server,
}

impl Cli {
    pub fn resolve(self) -> anyhow::Result<Resolved> {
        match (self.command, self.client, self.server) {
            (Some(Mode::Client), _, _) | (None, true, false) => Ok(Resolved::Client),
            (Some(Mode::Server), _, _) | (None, false, true) => Ok(Resolved::Server),
            (None, false, false) => Err(anyhow::anyhow!(
                "specify a mode: `client` / `server` subcommand or --client / --server"
            )),
            _ => unreachable!("clap conflicts_with prevents both flags"),
        }
    }
}
```

- [ ] **Step 2: Expose `cli` in `src/lib.rs`**

Replace `src/lib.rs` with:

```rust
pub mod cli;
pub mod config;
pub mod flicker;
```

- [ ] **Step 3: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/cli.rs
git commit -m "feat(cli): clap subcommands with --client/--server alias flags"
```

---

## Task 7: Create `client::ffmpeg` argument builder

**Files:**
- Create: `src/client/mod.rs`
- Create: `src/client/ffmpeg.rs`

- [ ] **Step 1: Create `src/client/ffmpeg.rs`**

```rust
use crate::flicker::{FPS, HEIGHT, WIDTH};

pub fn publish_args(rtmp_url: &str) -> Vec<String> {
    let size_arg = format!("{}x{}", WIDTH, HEIGHT);
    let rate_arg = FPS.to_string();
    let gop_arg = (FPS * 2).to_string();

    [
        "-hide_banner",
        "-loglevel", "info",
        "-y",
        "-use_wallclock_as_timestamps", "1",
        "-thread_queue_size", "1024",
        "-f", "rawvideo",
        "-pix_fmt", "rgb24",
        "-s", &size_arg,
        "-r", &rate_arg,
        "-i", "-",
        "-f", "lavfi",
        "-i", "anullsrc=channel_layout=stereo:sample_rate=44100",
        "-c:v", "libx264",
        "-preset", "ultrafast",
        "-tune", "zerolatency",
        "-profile:v", "baseline",
        "-level", "3.0",
        "-pix_fmt", "yuv420p",
        "-b:v", "300k",
        "-maxrate", "300k",
        "-bufsize", "600k",
        "-g", &gop_arg,
        "-keyint_min", &rate_arg,
        "-c:a", "aac",
        "-b:a", "64k",
        "-ar", "44100",
        "-ac", "2",
        "-shortest",
        "-flvflags", "no_duration_filesize",
        "-f", "flv",
        rtmp_url,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}
```

- [ ] **Step 2: Create placeholder `src/client/mod.rs`**

```rust
pub mod ffmpeg;
```

- [ ] **Step 3: Expose `client` in `src/lib.rs`**

Replace `src/lib.rs` with:

```rust
pub mod cli;
pub mod client;
pub mod config;
pub mod flicker;
```

- [ ] **Step 4: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

---

## Task 8: Implement `client::run`

**Files:**
- Modify: `src/client/mod.rs`

- [ ] **Step 1: Replace `src/client/mod.rs` with the publish loop**

```rust
pub mod ffmpeg;

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

use crate::config::ClientConfig;
use crate::flicker::frame::encode_timestamp_frame;
use crate::flicker::{FPS, FRAME_BYTES};

fn now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

pub fn run(cfg: ClientConfig) -> Result<()> {
    eprintln!(
        "[flicker/client] grid: {}x{} cells of {}px ({} total, {} bits of ts); update every {} frames",
        cfg.grid.cols,
        cfg.grid.rows,
        cfg.grid.cell,
        cfg.grid.total_cells,
        cfg.grid.total_cells.min(64),
        cfg.grid.update_every,
    );
    eprintln!("[flicker/client] rtmp target: {}", cfg.rtmp_url);

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
            .context("failed to set Ctrl+C handler")?;
    }

    let args = ffmpeg::publish_args(&cfg.rtmp_url);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg — is it on PATH?")?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on ffmpeg"))?;

    let mut frame = vec![0u8; FRAME_BYTES];
    let frame_period = Duration::from_nanos(1_000_000_000 / FPS as u64);
    let start = Instant::now();
    let mut frame_idx: u64 = 0;

    while running.load(Ordering::SeqCst) {
        if frame_idx % cfg.grid.update_every == 0 {
            encode_timestamp_frame(&mut frame, now_ns(), &cfg.grid);
        }

        if let Err(e) = stdin.write_all(&frame) {
            eprintln!("[flicker/client] ffmpeg stdin closed: {e}");
            break;
        }

        frame_idx += 1;
        let target = start + frame_period * frame_idx as u32;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
    }

    drop(stdin);
    let status = child.wait().context("failed to wait for ffmpeg")?;
    if !status.success() {
        eprintln!("[flicker/client] ffmpeg exited with {status}");
    }
    Ok(())
}
```

- [ ] **Step 2: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/client
git commit -m "feat(client): port RTMP publish loop to module, driven by ClientConfig"
```

---

## Task 9: Port `server::vk_live` from reference

**Files:**
- Create: `src/server/mod.rs`
- Create: `src/server/vk_live.rs`

- [ ] **Step 1: Create `src/server/vk_live.rs`**

Copy the full reference module, trimmed to what the server needs:

```rust
//! VK Live playback URL resolver via `api.live.vkvideo.ru`.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const API_BASE: &str = "https://api.live.vkvideo.ru/v1";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct VkPlaybackResolved {
    pub dash_mpd: Option<String>,
    pub hls: Option<String>,
    pub page_url: String,
}

pub fn resolve_channel(slug: &str) -> Result<VkPlaybackResolved> {
    let slug = slug.trim().trim_matches('/');
    if slug.is_empty() {
        return Err(anyhow!("vk live: empty channel slug"));
    }
    let page_url = format!("https://live.vkvideo.ru/{slug}");
    let api_url = format!("{API_BASE}/blog/{slug}/public_video_stream");

    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .build()
        .context("reqwest client")?;

    let resp = client
        .get(&api_url)
        .header("Referer", &page_url)
        .header("Origin", "https://live.vkvideo.ru")
        .send()
        .with_context(|| format!("GET {api_url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("api.live.vkvideo.ru: HTTP {}", resp.status()));
    }

    let v: Value = resp.json().context("parse VK Live API JSON")?;

    if let Some(msg) = v.get("error_description").and_then(|x| x.as_str()) {
        if !msg.is_empty() {
            return Err(anyhow!("VK Live API: {msg}"));
        }
    }
    let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
    if !err.is_empty() {
        return Err(anyhow!("VK Live API error field: {err}"));
    }

    let first = v
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("VK Live: no data[] (channel offline or wrong slug)"))?;

    let pairs = first
        .get("playerUrls")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow!("VK Live: no playerUrls (no active player?)"))?;

    let mut dash_mpd = None;
    let mut hls = None;
    for p in pairs {
        let t = p
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_lowercase();
        let u = p.get("url").and_then(|x| x.as_str()).unwrap_or("").trim();
        if u.is_empty() {
            continue;
        }
        if u.contains(".mpd") || t.contains("dash") {
            dash_mpd.get_or_insert_with(|| u.to_string());
        }
        if t.contains("hls") || t.contains("m3u8") || u.contains(".m3u8") {
            hls.get_or_insert_with(|| u.to_string());
        }
    }

    Ok(VkPlaybackResolved {
        dash_mpd,
        hls,
        page_url,
    })
}

/// Prefer HLS for lower latency, fall back to DASH.
pub fn pick_playback_url(r: &VkPlaybackResolved) -> Option<String> {
    r.hls.clone().or_else(|| r.dash_mpd.clone())
}

fn wait_interval() -> Duration {
    std::env::var("VK_LIVE_WAIT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(5))
}

fn wait_timeout() -> Option<Duration> {
    std::env::var("VK_LIVE_WAIT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
}

fn skip_probe() -> bool {
    std::env::var("VK_LIVE_SKIP_PROBE")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub fn probe_playback_url(url: &str, referer: &str, origin: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(25))
        .build()
        .context("reqwest probe client")?;
    let resp = client
        .get(url)
        .header("Referer", referer)
        .header("Origin", origin)
        .send()
        .with_context(|| format!("probe GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("CDN replied {}", status));
    }
    let body = resp.text().with_context(|| format!("probe read body {url}"))?;
    validate_probe_body(url, &body)
}

fn validate_probe_body(url: &str, body: &str) -> Result<()> {
    let u = url.to_ascii_lowercase();
    let is_hls_hint = u.contains(".m3u8") || u.contains("/hls") || u.contains("m3u8");
    let t = body.trim_start();
    let head = if t.len() > 2048 { &t[..2048] } else { t };
    let head_lower = head.to_ascii_lowercase();
    if head_lower.starts_with("#extm3u") || body.trim_start().starts_with("#EXTM3U") {
        return Ok(());
    }
    if is_hls_hint {
        if !body.contains("#EXTM3U") {
            return Err(anyhow!("CDN: body does not look like HLS (no #EXTM3U)"));
        }
        return Ok(());
    }
    if head_lower.starts_with("<!doctype") || head_lower.contains("<html") {
        return Err(anyhow!(
            "CDN: got HTML instead of manifest (403/redirect/expired URL)"
        ));
    }
    if !body.contains("<MPD") && !body.contains("<mpd") {
        return Err(anyhow!("CDN: not MPD XML (no <MPD> root)"));
    }
    if !body.contains("<Period") && !body.contains("<period") {
        return Err(anyhow!("CDN: MPD has no <Period> (expired URL or offline)"));
    }
    Ok(())
}

pub fn wait_for_playback_ready(slug: &str, referer: &str, origin: &str) -> Result<VkPlaybackResolved> {
    let interval = wait_interval();
    let timeout = wait_timeout();
    let started = Instant::now();
    let mut attempt = 0u32;
    let no_probe = skip_probe();
    eprintln!(
        "[flicker/vk] waiting for slug={slug:?} (interval {}s; timeout {}; probe {})",
        interval.as_secs(),
        match timeout {
            Some(t) => format!("{}s", t.as_secs()),
            None => "none".to_string(),
        },
        if no_probe { "off" } else { "on" }
    );
    loop {
        if let Some(t) = timeout {
            if started.elapsed() > t {
                return Err(anyhow!("VK Live: wait timeout ({t:?})"));
            }
        }
        attempt += 1;
        match resolve_channel(slug) {
            Ok(r) => {
                let Some(url) = pick_playback_url(&r) else {
                    eprintln!(
                        "[flicker/vk] attempt {attempt}: no dash/hls yet, retry in {:?}",
                        interval
                    );
                    thread::sleep(interval);
                    continue;
                };
                if no_probe {
                    eprintln!("[flicker/vk] got URL, probe skipped");
                    return Ok(r);
                }
                match probe_playback_url(&url, referer, origin) {
                    Ok(()) => {
                        eprintln!("[flicker/vk] manifest OK");
                        return Ok(r);
                    }
                    Err(e) => {
                        eprintln!(
                            "[flicker/vk] attempt {attempt}: CDN: {e}; retry in {:?}",
                            interval
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[flicker/vk] attempt {attempt}: {e}");
            }
        }
        thread::sleep(interval);
    }
}
```

- [ ] **Step 2: Create `src/server/mod.rs` stub**

```rust
pub mod vk_live;
```

- [ ] **Step 3: Expose `server` in `src/lib.rs`**

Replace `src/lib.rs` with:

```rust
pub mod cli;
pub mod client;
pub mod config;
pub mod flicker;
pub mod server;
```

- [ ] **Step 4: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/server
git commit -m "feat(server): port vk_live resolver (HLS preferred over DASH)"
```

---

## Task 10: Create `server::ingest` (low-latency ffmpeg read)

**Files:**
- Create: `src/server/ingest.rs`
- Modify: `src/server/mod.rs`

- [ ] **Step 1: Create `src/server/ingest.rs`**

```rust
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use crate::config::HttpConfig;
use crate::flicker::{FPS, HEIGHT, WIDTH};

/// Build ffmpeg args that drain an HLS/DASH URL to raw RGB24 on stdout with
/// aggressive low-latency flags (no probe, no analyze, nearest-neighbor scale).
pub fn read_args(input_url: &str, http: &HttpConfig, input_is_hls: bool) -> Vec<String> {
    let size_arg = format!("{}x{}", WIDTH, HEIGHT);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-fflags".into(),
        "nobuffer+discardcorrupt+flush_packets".into(),
        "-flags".into(),
        "low_delay".into(),
        "-avioflags".into(),
        "direct".into(),
        "-probesize".into(),
        "32k".into(),
        "-analyzeduration".into(),
        "0".into(),
    ];

    if input_is_hls {
        args.push("-live_start_index".into());
        args.push("-1".into());
        args.push("-http_persistent".into());
        args.push("1".into());
    }

    if let Some(ua) = http.user_agent.as_ref() {
        args.push("-user_agent".into());
        args.push(ua.clone());
    }

    let mut header_lines: Vec<String> = Vec::new();
    if let Some(r) = http.referer.as_ref() {
        header_lines.push(format!("Referer: {r}"));
    }
    if let Some(o) = http.origin.as_ref() {
        header_lines.push(format!("Origin: {o}"));
    }
    if !header_lines.is_empty() {
        args.push("-headers".into());
        args.push(header_lines.join("\r\n") + "\r\n");
    }

    args.push("-i".into());
    args.push(input_url.to_string());
    args.push("-thread_queue_size".into());
    args.push("1024".into());
    args.push("-an".into());
    args.push("-vf".into());
    args.push(format!(
        "scale={}:{}:flags=neighbor,format=rgb24,fps={}",
        WIDTH, HEIGHT, FPS
    ));
    args.push("-fps_mode".into());
    args.push("cfr".into());
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-s".into(),
        size_arg,
        "-".into(),
    ]);
    args
}

pub fn spawn(args: &[String]) -> Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn ffmpeg — is it on PATH?")
}
```

- [ ] **Step 2: Update `src/server/mod.rs`**

```rust
pub mod ingest;
pub mod vk_live;
```

- [ ] **Step 3: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

---

## Task 11: Create `server::decoder` (frame read loop)

**Files:**
- Create: `src/server/decoder.rs`
- Modify: `src/server/mod.rs`

- [ ] **Step 1: Create `src/server/decoder.rs`**

```rust
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::flicker::frame::decode_timestamp_frame;
use crate::flicker::{GridConfig, FRAME_BYTES};

fn local_now_ns() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128) as u64
}

/// Drains `stdout` of the ffmpeg subprocess, decoding one RGB24 frame at a time.
/// Returns `Ok(())` on clean EOF (caller decides whether to reconnect).
pub fn run<R: Read>(
    mut stdout: R,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    let mut frame = vec![0u8; FRAME_BYTES];
    let mut frame_idx: u64 = 0;
    let mut last_ts: Option<u64> = None;

    while running.load(Ordering::SeqCst) {
        match stdout.read_exact(&mut frame) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[flicker/server] stream ended / short read: {e}");
                return Ok(());
            }
        }

        let ts_ns = decode_timestamp_frame(&frame, cfg);
        let now_ns = local_now_ns();
        let delta_ns = now_ns as i128 - ts_ns as i128;
        let delta_ms = delta_ns / 1_000_000;

        if log_every_frame || last_ts != Some(ts_ns) {
            eprintln!(
                "[flicker] frame={:>8} ts_ns={} Δ={:>6} ms",
                frame_idx, ts_ns, delta_ms
            );
        }
        last_ts = Some(ts_ns);
        frame_idx += 1;
    }
    Ok(())
}

/// Drop-in wrapper so callers don't have to know about `read_exact` details.
pub fn run_owned_stdout(
    stdout: std::process::ChildStdout,
    cfg: &GridConfig,
    running: &Arc<AtomicBool>,
    log_every_frame: bool,
) -> Result<()> {
    run(stdout, cfg, running, log_every_frame).context("decoder loop")
}
```

- [ ] **Step 2: Update `src/server/mod.rs`**

```rust
pub mod decoder;
pub mod ingest;
pub mod vk_live;
```

- [ ] **Step 3: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add src/server
git commit -m "feat(server): ingest ffmpeg args + decoder loop with Δms log"
```

---

## Task 12: Implement `server::run` with resolve + reconnect loop

**Files:**
- Modify: `src/server/mod.rs`

- [ ] **Step 1: Replace `src/server/mod.rs` with the orchestrator**

```rust
pub mod decoder;
pub mod ingest;
pub mod vk_live;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{HttpConfig, ServerConfig, SourceConfig};

const BACKOFF_SEQ_SECS: &[u64] = &[1, 2, 5, 10];

struct ResolvedSource {
    url: String,
    http: HttpConfig,
    is_hls: bool,
}

fn resolve(source: &SourceConfig, base_http: &HttpConfig) -> Result<ResolvedSource> {
    match source {
        SourceConfig::DirectUrl(u) => {
            let is_hls = looks_like_hls(u);
            Ok(ResolvedSource {
                url: u.clone(),
                http: base_http.clone(),
                is_hls,
            })
        }
        SourceConfig::VkLiveSlug(slug) => {
            let page_url = format!("https://live.vkvideo.ru/{slug}");
            let referer = base_http
                .referer
                .clone()
                .unwrap_or_else(|| page_url.clone());
            let origin = base_http
                .origin
                .clone()
                .unwrap_or_else(|| "https://live.vkvideo.ru".to_string());
            let r = vk_live::wait_for_playback_ready(slug, &referer, &origin)?;
            let url = vk_live::pick_playback_url(&r)
                .context("VK Live: no dash/hls after wait")?;
            let is_hls = looks_like_hls(&url);
            let mut http = base_http.clone();
            http.referer = Some(referer);
            http.origin = Some(origin);
            if http.user_agent.is_none() {
                http.user_agent = Some(
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
                        .to_string(),
                );
            }
            eprintln!(
                "[flicker/server] VK `{}` -> {} ({})",
                slug,
                url.chars().take(80).collect::<String>(),
                if is_hls { "hls" } else { "dash" }
            );
            Ok(ResolvedSource { url, http, is_hls })
        }
    }
}

fn looks_like_hls(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains(".m3u8") || u.contains("/hls")
}

pub fn run(cfg: ServerConfig) -> Result<()> {
    eprintln!(
        "[flicker/server] grid: {}x{} cells of {}px ({} bits of ts)",
        cfg.grid.cols,
        cfg.grid.rows,
        cfg.grid.cell,
        cfg.grid.total_cells.min(64),
    );
    if cfg.log_every_frame {
        eprintln!("[flicker/server] logging every frame (stream_log_every_frame=1)");
    } else {
        eprintln!("[flicker/server] logging on ts change (set stream_log_every_frame=1 for per-frame)");
    }

    let running = Arc::new(AtomicBool::new(true));
    let child_slot: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
    {
        let r = running.clone();
        let slot = child_slot.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
            if let Ok(mut g) = slot.lock() {
                if let Some(c) = g.as_mut() {
                    let _ = c.kill();
                }
            }
        })
        .context("failed to set Ctrl+C handler")?;
    }

    let mut attempt: usize = 0;
    while running.load(Ordering::SeqCst) {
        let resolved = match resolve(&cfg.source, &cfg.http) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[flicker/server] resolve failed: {e:#}");
                backoff(attempt, &running);
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        let args = ingest::read_args(&resolved.url, &resolved.http, resolved.is_hls);
        let mut child = match ingest::spawn(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[flicker/server] spawn ffmpeg failed: {e:#}");
                backoff(attempt, &running);
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout on ffmpeg"))?;

        {
            let mut g = child_slot
                .lock()
                .map_err(|e| anyhow!("child mutex poisoned: {e}"))?;
            *g = Some(child);
        }

        // Successful spawn → reset backoff counter.
        attempt = 0;

        let decode_result =
            decoder::run_owned_stdout(stdout, &cfg.grid, &running, cfg.log_every_frame);

        let wait_status = {
            let mut g = child_slot
                .lock()
                .map_err(|e| anyhow!("child mutex poisoned: {e}"))?;
            if let Some(mut c) = g.take() {
                c.wait().ok()
            } else {
                None
            }
        };

        if let Err(e) = decode_result {
            eprintln!("[flicker/server] decoder error: {e:#}");
        }
        if let Some(s) = wait_status {
            if !s.success() {
                eprintln!("[flicker/server] ffmpeg exited with {s}");
            }
        }

        if !running.load(Ordering::SeqCst) {
            break;
        }

        eprintln!("[flicker/server] reconnecting…");
        attempt = attempt.saturating_add(1);
        backoff(attempt, &running);
    }
    Ok(())
}

fn backoff(attempt: usize, running: &Arc<AtomicBool>) {
    let secs = BACKOFF_SEQ_SECS
        .get(attempt.min(BACKOFF_SEQ_SECS.len().saturating_sub(1)))
        .copied()
        .unwrap_or(10);
    let total = Duration::from_secs(secs);
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < total && running.load(Ordering::SeqCst) {
        thread::sleep(step);
        waited += step;
    }
}
```

- [ ] **Step 2: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/server/mod.rs
git commit -m "feat(server): orchestrator with resolve + reconnect loop"
```

---

## Task 13: Replace `main.rs` with dispatcher

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Replace `src/main.rs`**

```rust
use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::{Cli, Resolved};
use rtmp_steganography::{client, config, server};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    match cli.resolve()? {
        Resolved::Client => {
            let cfg = config::load_client()?;
            client::run(cfg)
        }
        Resolved::Server => {
            let cfg = config::load_server()?;
            server::run(cfg)
        }
    }
}
```

- [ ] **Step 2: Build release binary**

Run: `cargo build --release`
Expected: Builds cleanly, produces `target/release/rtmp-steganography.exe`.

- [ ] **Step 3: Sanity-check both CLI forms**

Run:
```
cargo run --release -- --help
cargo run --release -- client --help
cargo run --release -- server --help
```
Expected: Help text for each command; no panic.

- [ ] **Step 4: Run the roundtrip test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: single-binary dispatcher for --client / --server"
```

---

## Task 14: Smoke test client + server locally (manual)

**Files:** None (manual verification).

- [ ] **Step 1: Run the client in one terminal**

```
cargo run --release -- client
```
Expected: ffmpeg starts publishing; stderr shows grid config and `rtmp target:` line; no crashes.

Stop with Ctrl+C after 30 s.

- [ ] **Step 2: Run the server against a known direct URL (optional)**

If a direct MPD/HLS URL is available, set `stream_read_url` in `.env` and run:

```
cargo run --release -- server
```
Expected: Resolve message, ffmpeg starts, per-frame log lines like:
```
[flicker] frame=      42 ts_ns=1713520000123456789 Δ=  1823 ms
```

- [ ] **Step 3: Run the server against VK Live**

Ensure `vk_live_channel=pavel8899` is set in `.env`, channel is live, then:

```
cargo run --release -- server
```
Expected: `[flicker/vk] manifest OK`, then decode lines. If CDN returns 403, set `stream_referer=https://live.vkvideo.ru/pavel8899`, `stream_origin=https://live.vkvideo.ru`.

- [ ] **Step 4: Kill the stream mid-flight, verify reconnect**

Stop publishing client (Ctrl+C on client terminal). Server stderr should show `stream ended / short read`, `reconnecting…`, then retry with backoff.

- [ ] **Step 5: Final commit if any doc/.env tweaks were needed**

```bash
git status
# if nothing changed, skip; otherwise:
git add -u
git commit -m "chore: tweaks from smoke test"
```

---

## Self-Review Notes

- **Spec coverage:** client publish → Task 7–8; server auto-resolve → Task 9 + 12; low-latency flags → Task 10; reconnect → Task 12; flicker extraction → Tasks 2–4; CLI `--client`/`--server` alias + subcommand → Task 6; `.env` additions → Task 1. All spec bullets covered.
- **Placeholder scan:** no "TBD" / "implement later" left; every code step has full code.
- **Type consistency:** `GridConfig::new`, `ClientConfig`, `ServerConfig`, `SourceConfig`, `HttpConfig`, `Resolved`, `Mode` used identically across tasks.
- **Reconnect semantics:** `attempt=0` reset after a successful ffmpeg spawn so a single failure after long uptime doesn't land at max backoff immediately.
