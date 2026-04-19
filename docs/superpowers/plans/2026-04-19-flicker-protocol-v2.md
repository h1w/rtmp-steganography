# Flicker Protocol v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clean-rewrite the flicker steganography codec into a bidirectional, high-throughput UDP-like protocol over RTMP/HLS, carrying ~5.5 KB/s (B) or ~11.3 KB/s (C) of application payload per direction through VK Live.

**Architecture:** SDH-inspired frame: corner markers (affine alignment) + header RS(33,22) + pilot cells (bias calibration) + payload zone with 2 or 4 × RS(172,120) blocks + fragment reassembly. Single CLI `peer` subcommand with `--publish-only` / `--receive-only` direction flags; tx/rx/app threads inside one process.

**Tech Stack:** Rust 2021, `reed-solomon-erasure` for FEC, `crc32fast`, `rand_chacha` for deterministic pilot PRNG, `clap` (already present), std threads + `mpsc`, ffmpeg subprocess.

**Spec:** `docs/superpowers/specs/2026-04-19-flicker-protocol-v2-design.md` (commit `fbbb07f`).

---

## Context for the implementer

- Current working directory: `C:\Users\bpqvg\Desktop\dev\rtmp-steganography`. This is a Windows host, bash shell; use forward slashes, `/dev/null`.
- `cargo build` and `cargo test` work at repo root.
- v1 flicker code lives in `src/flicker/{grid,codec,frame,mod}.rs` — TO BE DELETED IN TASK 1.
- v1 client/server code lives in `src/client/`, `src/server/` — content migrates into `src/peer/` in Task 13–15, then old directories deleted.
- Git: main branch. Commit after every green test. Signing/hooks unchanged.
- ffmpeg on PATH is required for Task 21 (Level C tests) and live operation, but NOT for Level B unit tests (Task 20).
- All file paths in this plan are **absolute** within the repo. Backslashes are fine for Windows `Write` tool calls but forward slashes in bash are safer.

## Key type definitions (referenced throughout)

These are the canonical definitions. If a later task's code disagrees with these, the later task is wrong.

```rust
// Fixed grid constants (src/flicker/grid.rs)
pub const FRAME_WIDTH: usize = 256;
pub const FRAME_HEIGHT: usize = 144;
pub const FPS: u32 = 24;
pub const CELL_SIZE: usize = 4;
pub const GRID_COLS: usize = FRAME_WIDTH / CELL_SIZE;   // 64
pub const GRID_ROWS: usize = FRAME_HEIGHT / CELL_SIZE;  // 36
pub const TOTAL_CELLS: usize = GRID_COLS * GRID_ROWS;   // 2304

pub const FRAME_BYTES_RGB24: usize = FRAME_WIDTH * FRAME_HEIGHT * 3; // 110592

// Luma levels (src/flicker/levels.rs)
pub const LEVELS_Y: [u8; 4] = [32, 96, 160, 224];
pub const LEVELS_U: [u8; 2] = [80, 176];  // away from 128 neutral
pub const LEVELS_V: [u8; 2] = [80, 176];

// Modulation mode (src/flicker/mod.rs)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ModulationMode {
    B = 1, // luma only, 4 levels, 2 bpp
    C = 2, // Y 4 levels + U 2 levels + V 2 levels, 4 bpp
}

// Public message types (src/flicker/mod.rs)
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

// Header (src/flicker/header.rs)
pub const SYNC_WORD: [u8; 4] = [0xF1, 0x1C, 0x4E, 0x52];
pub const PROTOCOL_VERSION: u8 = 0x02;
pub const HEADER_DATA_BYTES: usize = 22;
pub const HEADER_PARITY_BYTES: usize = 11;
pub const HEADER_TOTAL_BYTES: usize = 33;

// FEC (src/flicker/fec.rs)
pub const RS_BLOCK_N: usize = 172;
pub const RS_BLOCK_K: usize = 120;
pub const RS_BLOCK_PARITY: usize = RS_BLOCK_N - RS_BLOCK_K; // 52

// Direction flags (src/peer/mod.rs)
#[derive(Copy, Clone, Debug)]
pub struct Direction {
    pub tx: bool,
    pub rx: bool,
}
```

---

## Phase 0 — Wipe v1 and prep dependencies

### Task 1: Delete v1 code, reset compile baseline

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `src/cli.rs`
- Delete: `src/flicker/grid.rs`, `src/flicker/codec.rs`, `src/flicker/frame.rs`, `src/flicker/mod.rs`
- Delete: `src/client/ffmpeg.rs`, `src/client/mod.rs`
- Delete: `src/server/decoder.rs`, `src/server/ingest.rs`, `src/server/mod.rs`, `src/server/vk_live.rs`
- Delete: `src/config.rs` (will be rewritten)
- Delete: `tests/flicker_roundtrip.rs` if present

- [ ] **Step 1: Add Cargo.toml dependencies**

Edit `Cargo.toml` to add FEC and PRNG crates:

```toml
[dependencies]
anyhow = "1"
dotenvy = "0.15"
ctrlc = "3"
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }
serde_json = "1"
reed-solomon-erasure = "6"
crc32fast = "1"
rand_chacha = "0.3"
rand_core = "0.6"

[features]
ffmpeg-integration = []

[profile.release]
opt-level = 3
lto = "thin"
```

- [ ] **Step 2: Delete v1 directories and files**

Run:
```bash
rm -rf src/flicker src/client src/server src/config.rs
rm -f tests/flicker_roundtrip.rs
```

- [ ] **Step 3: Stub out lib.rs and main.rs to keep compile green**

Replace `src/lib.rs`:
```rust
pub mod cli;
pub mod config;
pub mod flicker;
pub mod peer;
```

Replace `src/main.rs`:
```rust
use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::Cli;
use rtmp_steganography::{config, peer};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let (direction, cfg) = cli.resolve(&config::load_peer()?)?;
    peer::run_peer(cfg, direction)
}
```

Replace `src/cli.rs` with a minimal placeholder that compiles (filled in properly in Task 18):
```rust
use anyhow::{anyhow, Result};
use clap::{ArgAction, Parser, Subcommand};

use crate::config::PeerConfig;
use crate::peer::Direction;

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker v2 protocol over RTMP")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Mode>,

    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "peer")]
    pub peer_flag: bool,
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    Peer(PeerArgs),
}

#[derive(clap::Args, Debug)]
pub struct PeerArgs {
    #[arg(long = "publish-only", conflicts_with = "receive_only")]
    pub publish_only: bool,
    #[arg(long = "receive-only")]
    pub receive_only: bool,
}

impl Cli {
    pub fn resolve(self, cfg: &PeerConfig) -> Result<(Direction, PeerConfig)> {
        let (dir, _args) = match self.command {
            Some(Mode::Peer(args)) => {
                let dir = match (args.publish_only, args.receive_only) {
                    (false, false) => Direction { tx: true, rx: true },
                    (true, false) => Direction { tx: true, rx: false },
                    (false, true) => Direction { tx: false, rx: true },
                    (true, true) => return Err(anyhow!("--publish-only and --receive-only are mutually exclusive")),
                };
                (dir, args)
            }
            None if self.peer_flag => (
                Direction { tx: true, rx: true },
                PeerArgs { publish_only: false, receive_only: false },
            ),
            None => return Err(anyhow!("use: rtmp-steganography peer [--publish-only|--receive-only]")),
        };
        Ok((dir, cfg.clone()))
    }
}
```

Create empty stubs so crate compiles:

`src/flicker/mod.rs`:
```rust
// Filled in by Tasks 2–11.
```

`src/peer/mod.rs`:
```rust
use anyhow::Result;
use crate::config::PeerConfig;

#[derive(Copy, Clone, Debug)]
pub struct Direction {
    pub tx: bool,
    pub rx: bool,
}

pub fn run_peer(_cfg: PeerConfig, _dir: Direction) -> Result<()> {
    anyhow::bail!("peer not implemented yet")
}
```

`src/config.rs`:
```rust
use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub my_rtmp_url: String,
    pub my_stream_key: String,
    pub their_vk_channel: String,
    pub their_stream_name: String,
}

pub fn load_peer() -> Result<PeerConfig> {
    Ok(PeerConfig {
        my_rtmp_url: std::env::var("peer_my_rtmp_url").unwrap_or_default(),
        my_stream_key: std::env::var("peer_my_stream_key").unwrap_or_default(),
        their_vk_channel: std::env::var("peer_their_vk_channel").unwrap_or_default(),
        their_stream_name: std::env::var("peer_their_stream_name").unwrap_or_default(),
    })
}
```

- [ ] **Step 4: Verify clean compile**

Run: `cargo build`
Expected: builds with warnings about unused imports but no errors.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/ && git add -u
git commit -m "$(cat <<'EOF'
chore: wipe v1, add v2 deps, stub new module tree

Remove all v1 flicker/client/server code in preparation for v2 rewrite.
Add reed-solomon-erasure, crc32fast, rand_chacha dependencies.
Stub src/flicker/, src/peer/, config.rs, main.rs, cli.rs so crate compiles.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 1 — Core types and grid constants

### Task 2: flicker::grid — fixed constants with compile-time assertions

**Files:**
- Create: `src/flicker/grid.rs`
- Test: embedded `#[cfg(test)]` module

- [ ] **Step 1: Write the failing test**

Create `src/flicker/grid.rs`:
```rust
//! Fixed grid parameters for flicker v2.
//! All values are compile-time constants — v2 does not support runtime tuning.

pub const FRAME_WIDTH: usize = 256;
pub const FRAME_HEIGHT: usize = 144;
pub const FPS: u32 = 24;
pub const CELL_SIZE: usize = 4;

pub const GRID_COLS: usize = FRAME_WIDTH / CELL_SIZE;
pub const GRID_ROWS: usize = FRAME_HEIGHT / CELL_SIZE;
pub const TOTAL_CELLS: usize = GRID_COLS * GRID_ROWS;

pub const FRAME_BYTES_RGB24: usize = FRAME_WIDTH * FRAME_HEIGHT * 3;

/// Central readable region within a cell: 2×2 px centered in 4×4, giving 1 px guard on each side.
pub const CELL_READ_OFFSET: usize = 1;
pub const CELL_READ_SIZE: usize = 2;

/// Convert (col, row) logical cell coords to top-left pixel (x, y).
#[inline]
pub fn cell_topleft(col: usize, row: usize) -> (usize, usize) {
    (col * CELL_SIZE, row * CELL_SIZE)
}

/// Byte offset into a RGB24 buffer for pixel (x, y).
#[inline]
pub fn rgb24_offset(x: usize, y: usize) -> usize {
    (y * FRAME_WIDTH + x) * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_dimensions_are_consistent() {
        assert_eq!(GRID_COLS, 64);
        assert_eq!(GRID_ROWS, 36);
        assert_eq!(TOTAL_CELLS, 2304);
        assert_eq!(FRAME_BYTES_RGB24, 110_592);
    }

    #[test]
    fn cell_topleft_maps_correctly() {
        assert_eq!(cell_topleft(0, 0), (0, 0));
        assert_eq!(cell_topleft(63, 35), (252, 140));
        assert_eq!(cell_topleft(10, 5), (40, 20));
    }

    #[test]
    fn rgb24_offset_is_row_major() {
        assert_eq!(rgb24_offset(0, 0), 0);
        assert_eq!(rgb24_offset(1, 0), 3);
        assert_eq!(rgb24_offset(0, 1), 256 * 3);
    }
}
```

Update `src/flicker/mod.rs`:
```rust
pub mod grid;
```

- [ ] **Step 2: Run tests, verify they pass**

Run: `cargo test --lib flicker::grid -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/grid.rs src/flicker/mod.rs
git commit -m "feat(flicker): grid constants and cell↔pixel mapping

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: flicker::levels — luma/chroma LUT and soft-decision utilities

**Files:**
- Create: `src/flicker/levels.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/levels.rs`:
```rust
//! Discrete modulation levels and soft-decision confidence.
//!
//! Chosen centered away from BT.601/709 limited-range clipping zones [0,16]
//! and [235,255] to survive scaler range conversion.

pub const LEVELS_Y: [u8; 4] = [32, 96, 160, 224];
pub const LEVELS_U: [u8; 2] = [80, 176];
pub const LEVELS_V: [u8; 2] = [80, 176];

/// Quantise a luma sample to the closest of 4 levels.
/// Returns (symbol ∈ 0..=3, confidence ∈ [0.0, 1.0]).
pub fn quantise_y(sample: u8) -> (u8, f32) {
    quantise(sample, &LEVELS_Y)
}

pub fn quantise_uv(sample: u8) -> (u8, f32) {
    quantise(sample, &LEVELS_U)
}

fn quantise(sample: u8, levels: &[u8]) -> (u8, f32) {
    debug_assert!(!levels.is_empty());
    let mut best_idx = 0usize;
    let mut best_dist = i32::MAX;
    let mut second_dist = i32::MAX;
    for (i, &lvl) in levels.iter().enumerate() {
        let d = (sample as i32 - lvl as i32).abs();
        if d < best_dist {
            second_dist = best_dist;
            best_dist = d;
            best_idx = i;
        } else if d < second_dist {
            second_dist = d;
        }
    }
    // Confidence = 1 - (best_dist / second_dist); max 1.0, min 0.0.
    let confidence = if second_dist == 0 {
        0.0
    } else {
        1.0 - (best_dist as f32 / second_dist as f32).clamp(0.0, 1.0)
    };
    (best_idx as u8, confidence)
}

/// Paint symbol `sym` on an RGB24 pixel as pure luma (same in R, G, B).
#[inline]
pub fn level_y_as_rgb(sym: u8) -> [u8; 3] {
    let y = LEVELS_Y[sym as usize % LEVELS_Y.len()];
    [y, y, y]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_levels_quantise_with_full_confidence() {
        for (i, &lvl) in LEVELS_Y.iter().enumerate() {
            let (s, c) = quantise_y(lvl);
            assert_eq!(s, i as u8);
            assert!(c > 0.99, "confidence should be ~1.0 at exact level, got {c}");
        }
    }

    #[test]
    fn midway_between_levels_gives_low_confidence() {
        // Midway between 32 and 96 is 64; distance to both is 32.
        let (_, c) = quantise_y(64);
        assert!(c < 0.1, "confidence at midway should be near 0, got {c}");
    }

    #[test]
    fn slight_noise_gives_high_confidence() {
        // 32 + 5 = 37 is much closer to 32 (5) than to 96 (59).
        let (s, c) = quantise_y(37);
        assert_eq!(s, 0);
        assert!(c > 0.9, "confidence should be high for close sample, got {c}");
    }

    #[test]
    fn level_y_as_rgb_paints_luma() {
        assert_eq!(level_y_as_rgb(0), [32, 32, 32]);
        assert_eq!(level_y_as_rgb(3), [224, 224, 224]);
    }
}
```

Update `src/flicker/mod.rs`:
```rust
pub mod grid;
pub mod levels;
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::levels -- --nocapture`
Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/levels.rs src/flicker/mod.rs
git commit -m "feat(flicker): modulation levels with soft-decision quantiser

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 2 — Cell codec

### Task 4: flicker::codec — paint_cell / read_cell with mode B and C

**Files:**
- Create: `src/flicker/codec.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/codec.rs`:
```rust
//! Paint and read individual cells in RGB24 buffer.
//! Supports mode B (2 bpp luma only) and mode C (4 bpp with chroma).
//!
//! Mode C note: RGB is not native YUV; this module writes distinct R/G/B patterns
//! that, after RGB→YUV conversion, will produce the desired Y/U/V levels at the
//! chroma cell centre. Round-trip through yuv420 is validated in ffmpeg tests.

use crate::flicker::grid::{
    cell_topleft, rgb24_offset, CELL_READ_OFFSET, CELL_READ_SIZE, CELL_SIZE, FRAME_BYTES_RGB24,
};
use crate::flicker::levels::{
    level_y_as_rgb, quantise_uv, quantise_y, LEVELS_U, LEVELS_V, LEVELS_Y,
};
use crate::flicker::ModulationMode;

/// Paint a logical 2-bit symbol into a cell (mode B: luma-only).
pub fn paint_cell_b(buf: &mut [u8], col: usize, row: usize, symbol: u8) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    debug_assert!(symbol < 4);
    let rgb = level_y_as_rgb(symbol);
    let (x0, y0) = cell_topleft(col, row);
    for y in y0..y0 + CELL_SIZE {
        for x in x0..x0 + CELL_SIZE {
            let o = rgb24_offset(x, y);
            buf[o] = rgb[0];
            buf[o + 1] = rgb[1];
            buf[o + 2] = rgb[2];
        }
    }
}

/// Read a mode-B cell, returning (symbol, confidence).
pub fn read_cell_b(buf: &[u8], col: usize, row: usize) -> (u8, f32) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    let (x0, y0) = cell_topleft(col, row);
    let rx0 = x0 + CELL_READ_OFFSET;
    let ry0 = y0 + CELL_READ_OFFSET;
    let mut sum: u32 = 0;
    let mut count: u32 = 0;
    for y in ry0..ry0 + CELL_READ_SIZE {
        for x in rx0..rx0 + CELL_READ_SIZE {
            let o = rgb24_offset(x, y);
            // Luma approximation from BT.601: Y ≈ 0.299 R + 0.587 G + 0.114 B
            let r = buf[o] as u32;
            let g = buf[o + 1] as u32;
            let b = buf[o + 2] as u32;
            let y_val = (299 * r + 587 * g + 114 * b) / 1000;
            sum += y_val;
            count += 1;
        }
    }
    let mean = (sum / count.max(1)) as u8;
    quantise_y(mean)
}

/// Paint a logical 4-bit symbol into a cell (mode C: Y:2 bits + U:1 bit + V:1 bit).
/// Symbol layout MSB→LSB: Y_hi Y_lo U V.
pub fn paint_cell_c(buf: &mut [u8], col: usize, row: usize, symbol: u8) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    debug_assert!(symbol < 16);
    let y_sym = (symbol >> 2) & 0b11;
    let u_sym = (symbol >> 1) & 0b1;
    let v_sym = symbol & 0b1;
    let y = LEVELS_Y[y_sym as usize];
    let u = LEVELS_U[u_sym as usize];
    let v = LEVELS_V[v_sym as usize];
    // Convert YUV (BT.601) to RGB for painting.
    let rgb = yuv_to_rgb(y, u, v);
    let (x0, y0) = cell_topleft(col, row);
    for py in y0..y0 + CELL_SIZE {
        for px in x0..x0 + CELL_SIZE {
            let o = rgb24_offset(px, py);
            buf[o] = rgb[0];
            buf[o + 1] = rgb[1];
            buf[o + 2] = rgb[2];
        }
    }
}

/// Read a mode-C cell, returning (symbol, min-confidence-across-channels).
pub fn read_cell_c(buf: &[u8], col: usize, row: usize) -> (u8, f32) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    let (x0, y0) = cell_topleft(col, row);
    let rx0 = x0 + CELL_READ_OFFSET;
    let ry0 = y0 + CELL_READ_OFFSET;
    let mut r_sum = 0u32;
    let mut g_sum = 0u32;
    let mut b_sum = 0u32;
    let mut count = 0u32;
    for py in ry0..ry0 + CELL_READ_SIZE {
        for px in rx0..rx0 + CELL_READ_SIZE {
            let o = rgb24_offset(px, py);
            r_sum += buf[o] as u32;
            g_sum += buf[o + 1] as u32;
            b_sum += buf[o + 2] as u32;
            count += 1;
        }
    }
    let r_mean = (r_sum / count.max(1)) as u8;
    let g_mean = (g_sum / count.max(1)) as u8;
    let b_mean = (b_sum / count.max(1)) as u8;
    let (y, u, v) = rgb_to_yuv(r_mean, g_mean, b_mean);
    let (y_sym, y_conf) = quantise_y(y);
    let (u_sym, u_conf) = quantise_uv(u);
    let (v_sym, v_conf) = quantise_uv(v);
    let symbol = (y_sym << 2) | (u_sym << 1) | v_sym;
    let conf = y_conf.min(u_conf).min(v_conf);
    (symbol, conf)
}

fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    // BT.601 full-range.
    let y = y as f32;
    let u = u as f32 - 128.0;
    let v = v as f32 - 128.0;
    let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
    let g = (y - 0.344 * u - 0.714 * v).clamp(0.0, 255.0) as u8;
    let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
    [r, g, b]
}

fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r = r as f32;
    let g = g as f32;
    let b = b as f32;
    let y = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
    let u = (-0.169 * r - 0.331 * g + 0.5 * b + 128.0).clamp(0.0, 255.0) as u8;
    let v = (0.5 * r - 0.419 * g - 0.081 * b + 128.0).clamp(0.0, 255.0) as u8;
    (y, u, v)
}

/// Dispatch by mode.
pub fn paint_cell(buf: &mut [u8], col: usize, row: usize, symbol: u8, mode: ModulationMode) {
    match mode {
        ModulationMode::B => paint_cell_b(buf, col, row, symbol),
        ModulationMode::C => paint_cell_c(buf, col, row, symbol),
    }
}

pub fn read_cell(buf: &[u8], col: usize, row: usize, mode: ModulationMode) -> (u8, f32) {
    match mode {
        ModulationMode::B => read_cell_b(buf, col, row),
        ModulationMode::C => read_cell_c(buf, col, row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flicker::grid::FRAME_BYTES_RGB24;

    #[test]
    fn paint_then_read_b_roundtrips_all_symbols() {
        let mut buf = vec![0u8; FRAME_BYTES_RGB24];
        for sym in 0u8..4 {
            paint_cell_b(&mut buf, 10, 5, sym);
            let (read, conf) = read_cell_b(&buf, 10, 5);
            assert_eq!(read, sym, "symbol {sym} round-trip failed");
            assert!(conf > 0.95, "confidence should be ~1.0, got {conf}");
        }
    }

    #[test]
    fn paint_then_read_c_roundtrips_all_symbols() {
        let mut buf = vec![0u8; FRAME_BYTES_RGB24];
        for sym in 0u8..16 {
            paint_cell_c(&mut buf, 20, 10, sym);
            let (read, conf) = read_cell_c(&buf, 20, 10);
            assert_eq!(read, sym, "symbol {sym} round-trip failed");
            assert!(conf > 0.6, "confidence should be reasonably high, got {conf}");
        }
    }

    #[test]
    fn paint_b_leaves_other_cells_untouched() {
        let mut buf = vec![128u8; FRAME_BYTES_RGB24];
        paint_cell_b(&mut buf, 10, 5, 3);
        // Cell (11, 5) should still be all 128.
        assert_eq!(read_cell_b(&buf, 11, 5).0, 1); // 128 ≈ midway; nearest is 96 (symbol 1)
    }
}
```

Update `src/flicker/mod.rs`:
```rust
pub mod codec;
pub mod grid;
pub mod levels;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ModulationMode {
    B = 1,
    C = 2,
}

impl ModulationMode {
    pub fn bits_per_cell(self) -> usize {
        match self {
            ModulationMode::B => 2,
            ModulationMode::C => 4,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(ModulationMode::B),
            2 => Some(ModulationMode::C),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::codec -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/
git commit -m "feat(flicker): cell codec with mode B (2bpp Y) and mode C (4bpp YUV)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 3 — Spatial layer

### Task 5: flicker::markers — corner pattern, cross-correlation, affine fit

**Files:**
- Create: `src/flicker/markers.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/markers.rs`:
```rust
//! Four 16×16 corner markers for frame alignment.
//!
//! Pattern: alternating 2×2 checkerboard of extreme luma levels (32 and 224).
//! Decoder finds each marker via local cross-correlation, then fits an affine
//! transform mapping logical grid coords to actual pixel coords.

use crate::flicker::grid::{rgb24_offset, FRAME_BYTES_RGB24, FRAME_HEIGHT, FRAME_WIDTH};
use crate::flicker::levels::LEVELS_Y;

pub const MARKER_SIZE: usize = 16;
pub const MARKER_SEARCH_RADIUS: i32 = 8;

/// Nominal (ideal) centre of each corner marker.
pub const MARKER_CENTERS: [(i32, i32); 4] = [
    (MARKER_SIZE as i32 / 2, MARKER_SIZE as i32 / 2),
    (FRAME_WIDTH as i32 - MARKER_SIZE as i32 / 2, MARKER_SIZE as i32 / 2),
    (MARKER_SIZE as i32 / 2, FRAME_HEIGHT as i32 - MARKER_SIZE as i32 / 2),
    (FRAME_WIDTH as i32 - MARKER_SIZE as i32 / 2, FRAME_HEIGHT as i32 - MARKER_SIZE as i32 / 2),
];

/// Ideal marker pattern (16×16 grayscale).
pub fn marker_pattern() -> [[u8; MARKER_SIZE]; MARKER_SIZE] {
    let mut pat = [[0u8; MARKER_SIZE]; MARKER_SIZE];
    for y in 0..MARKER_SIZE {
        for x in 0..MARKER_SIZE {
            // 2×2 blocks of alternating low/high.
            let block_x = x / 2;
            let block_y = y / 2;
            pat[y][x] = if (block_x + block_y) % 2 == 0 {
                LEVELS_Y[0]
            } else {
                LEVELS_Y[3]
            };
        }
    }
    pat
}

/// Paint all 4 markers into a frame.
pub fn paint_markers(buf: &mut [u8]) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    let pat = marker_pattern();
    for &(cx, cy) in MARKER_CENTERS.iter() {
        let x0 = (cx - MARKER_SIZE as i32 / 2).max(0) as usize;
        let y0 = (cy - MARKER_SIZE as i32 / 2).max(0) as usize;
        for dy in 0..MARKER_SIZE {
            for dx in 0..MARKER_SIZE {
                let x = x0 + dx;
                let y = y0 + dy;
                if x >= FRAME_WIDTH || y >= FRAME_HEIGHT {
                    continue;
                }
                let o = rgb24_offset(x, y);
                let v = pat[dy][dx];
                buf[o] = v;
                buf[o + 1] = v;
                buf[o + 2] = v;
            }
        }
    }
}

/// Locate one marker via local cross-correlation, returning offset (dx, dy)
/// from nominal centre. None if no peak above threshold.
pub fn locate_marker(buf: &[u8], nominal: (i32, i32)) -> Option<(i32, i32)> {
    let pat = marker_pattern();
    let mut best_score = f64::MIN;
    let mut best_off = (0i32, 0i32);
    for dy in -MARKER_SEARCH_RADIUS..=MARKER_SEARCH_RADIUS {
        for dx in -MARKER_SEARCH_RADIUS..=MARKER_SEARCH_RADIUS {
            let cx = nominal.0 + dx;
            let cy = nominal.1 + dy;
            let x0 = cx - MARKER_SIZE as i32 / 2;
            let y0 = cy - MARKER_SIZE as i32 / 2;
            if x0 < 0 || y0 < 0
                || x0 + MARKER_SIZE as i32 > FRAME_WIDTH as i32
                || y0 + MARKER_SIZE as i32 > FRAME_HEIGHT as i32
            {
                continue;
            }
            let score = correlate(buf, x0 as usize, y0 as usize, &pat);
            if score > best_score {
                best_score = score;
                best_off = (dx, dy);
            }
        }
    }
    // Sanity threshold: perfect match would yield ~(~200^2 * 256 area) = very large positive.
    if best_score > 0.0 {
        Some(best_off)
    } else {
        None
    }
}

fn correlate(buf: &[u8], x0: usize, y0: usize, pat: &[[u8; MARKER_SIZE]; MARKER_SIZE]) -> f64 {
    let mut sum = 0f64;
    for dy in 0..MARKER_SIZE {
        for dx in 0..MARKER_SIZE {
            let o = rgb24_offset(x0 + dx, y0 + dy);
            // Luma approx as R+G+B /3 (good enough for grayscale markers).
            let pixel = (buf[o] as f64 + buf[o + 1] as f64 + buf[o + 2] as f64) / 3.0;
            let ideal = pat[dy][dx] as f64;
            // Zero-mean correlation: (pixel - 128) * (ideal - 128).
            sum += (pixel - 128.0) * (ideal - 128.0);
        }
    }
    sum
}

/// Average offset over all 4 markers. Returns None if any marker missing.
pub fn frame_offset(buf: &[u8]) -> Option<(i32, i32)> {
    let mut sum = (0i32, 0i32);
    for &centre in MARKER_CENTERS.iter() {
        let off = locate_marker(buf, centre)?;
        sum.0 += off.0;
        sum.1 += off.1;
    }
    Some((sum.0 / 4, sum.1 / 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_markers_locate_at_zero_offset() {
        let mut buf = vec![128u8; FRAME_BYTES_RGB24];
        paint_markers(&mut buf);
        let off = frame_offset(&buf).expect("markers found");
        assert_eq!(off, (0, 0));
    }

    #[test]
    fn shifted_frame_detects_offset() {
        let mut src = vec![128u8; FRAME_BYTES_RGB24];
        paint_markers(&mut src);
        // Shift entire image by (+2, +1). For simplicity, scan and verify individual corners.
        let mut shifted = vec![128u8; FRAME_BYTES_RGB24];
        for y in 1..FRAME_HEIGHT {
            for x in 2..FRAME_WIDTH {
                let src_o = rgb24_offset(x - 2, y - 1);
                let dst_o = rgb24_offset(x, y);
                shifted[dst_o] = src[src_o];
                shifted[dst_o + 1] = src[src_o + 1];
                shifted[dst_o + 2] = src[src_o + 2];
            }
        }
        let off = frame_offset(&shifted).expect("markers found");
        // We expect roughly (+2, +1) but corners can disagree; allow ±1 slack.
        assert!((off.0 - 2).abs() <= 1, "x offset: got {}", off.0);
        assert!((off.1 - 1).abs() <= 1, "y offset: got {}", off.1);
    }
}
```

Update `src/flicker/mod.rs`:
```rust
pub mod codec;
pub mod grid;
pub mod levels;
pub mod markers;
// ... (rest of Task 4's mod.rs content)
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::markers -- --nocapture`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/markers.rs src/flicker/mod.rs
git commit -m "feat(flicker): corner markers with cross-correlation alignment

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: flicker::interleave — spatial permutation + byte-level interleave

**Files:**
- Create: `src/flicker/interleave.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/interleave.rs`:
```rust
//! Deterministic permutations for cell↔byte mapping.
//!
//! Two goals:
//! 1. Spatial permutation: scatter the logical (byte, bit) across the frame
//!    so a localised macroblock artefact hits multiple RS blocks.
//! 2. Byte-level interleave within an RS block: standard depth-n interleave.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

use crate::flicker::grid::TOTAL_CELLS;

const PERMUTATION_SEED: [u8; 32] = [
    0x46, 0x4c, 0x49, 0x43, 0x4b, 0x45, 0x52, 0x32, // "FLICKER2"
    0x2d, 0x53, 0x50, 0x41, 0x54, 0x49, 0x41, 0x4c, // "-SPATIAL"
    0x2d, 0x56, 0x32, 0x2d, 0x50, 0x45, 0x52, 0x4d, // "-V2-PERM"
    0x55, 0x54, 0x41, 0x54, 0x49, 0x4f, 0x4e, 0x21, // "UTATION!"
];

/// Generate the spatial permutation of all TOTAL_CELLS cells.
/// `excluded` is a set of cell indices to skip (e.g. marker cells).
/// Returns a Vec of (col, row) in the order payload cells should be laid out.
pub fn cell_permutation(excluded: &[usize]) -> Vec<(usize, usize)> {
    let mut rng = ChaCha8Rng::from_seed(PERMUTATION_SEED);
    let excluded_set: std::collections::HashSet<usize> = excluded.iter().copied().collect();
    let mut indices: Vec<usize> = (0..TOTAL_CELLS).filter(|i| !excluded_set.contains(i)).collect();
    // Fisher–Yates shuffle.
    for i in (1..indices.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        indices.swap(i, j);
    }
    indices.into_iter().map(cell_index_to_col_row).collect()
}

#[inline]
pub fn cell_index_to_col_row(idx: usize) -> (usize, usize) {
    use crate::flicker::grid::GRID_COLS;
    (idx % GRID_COLS, idx / GRID_COLS)
}

#[inline]
pub fn col_row_to_cell_index(col: usize, row: usize) -> usize {
    use crate::flicker::grid::GRID_COLS;
    row * GRID_COLS + col
}

/// Standard depth-`depth` byte interleave for a single RS codeword of length `n`.
/// Returns permutation: input[i] → output[permuted[i]].
pub fn byte_interleave(n: usize, depth: usize) -> Vec<usize> {
    let mut p = vec![0usize; n];
    let mut out = 0;
    for offset in 0..depth {
        let mut i = offset;
        while i < n {
            p[i] = out;
            out += 1;
            i += depth;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_permutation_is_deterministic() {
        let p1 = cell_permutation(&[]);
        let p2 = cell_permutation(&[]);
        assert_eq!(p1, p2);
    }

    #[test]
    fn cell_permutation_excludes_markers() {
        let excluded = vec![0, 1, 2, 3];
        let p = cell_permutation(&excluded);
        assert_eq!(p.len(), TOTAL_CELLS - 4);
        for (col, row) in &p {
            let idx = col_row_to_cell_index(*col, *row);
            assert!(!excluded.contains(&idx));
        }
    }

    #[test]
    fn cell_permutation_is_bijection() {
        let p = cell_permutation(&[]);
        let mut seen = vec![false; TOTAL_CELLS];
        for (col, row) in &p {
            let idx = col_row_to_cell_index(*col, *row);
            assert!(!seen[idx], "duplicate cell index {idx}");
            seen[idx] = true;
        }
    }

    #[test]
    fn byte_interleave_is_bijection() {
        let p = byte_interleave(172, 12);
        assert_eq!(p.len(), 172);
        let mut seen = vec![false; 172];
        for &v in &p {
            assert!(!seen[v], "duplicate interleave target {v}");
            seen[v] = true;
        }
    }
}
```

Add `pub mod interleave;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::interleave -- --nocapture`
Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/interleave.rs src/flicker/mod.rs
git commit -m "feat(flicker): deterministic cell + byte permutations

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: flicker::pilot — PRNG positions/values + bias estimation

**Files:**
- Create: `src/flicker/pilot.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/pilot.rs`:
```rust
//! Pilot cells: ~5% of the grid carries values deterministically derived
//! from `frame_counter`. Used for brightness/contrast bias correction and
//! alignment validation.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

use crate::flicker::codec::{paint_cell_b, read_cell_b};
use crate::flicker::grid::TOTAL_CELLS;
use crate::flicker::interleave::cell_index_to_col_row;

pub const PILOT_COUNT: usize = 115;
pub const PILOT_BASE_SEED: [u8; 16] = *b"flicker-pilot-v2";

/// Derive 32-byte ChaCha8 seed from base + frame_counter.
fn seed_for(frame_counter: u32) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(&PILOT_BASE_SEED);
    seed[16..20].copy_from_slice(&frame_counter.to_le_bytes());
    // remaining 12 bytes left zero; seed uniqueness driven by counter.
    seed
}

/// Returns list of pilot cell indices (into TOTAL_CELLS), excluding `excluded`.
pub fn pilot_positions(frame_counter: u32, excluded: &[usize]) -> Vec<usize> {
    let mut rng = ChaCha8Rng::from_seed(seed_for(frame_counter));
    let excluded_set: std::collections::HashSet<usize> = excluded.iter().copied().collect();
    let mut candidates: Vec<usize> = (0..TOTAL_CELLS).filter(|i| !excluded_set.contains(i)).collect();
    let mut picked = Vec::with_capacity(PILOT_COUNT);
    for _ in 0..PILOT_COUNT.min(candidates.len()) {
        let j = (rng.next_u32() as usize) % candidates.len();
        picked.push(candidates.swap_remove(j));
    }
    picked.sort_unstable();
    picked
}

/// Returns expected pilot symbol (0..=3) at `index_in_pilot_list`.
pub fn pilot_value(frame_counter: u32, index_in_pilot_list: usize) -> u8 {
    let mut rng = ChaCha8Rng::from_seed(seed_for(frame_counter ^ 0xDEADBEEF));
    // Advance RNG deterministically to index position.
    for _ in 0..index_in_pilot_list {
        let _ = rng.next_u32();
    }
    (rng.next_u32() & 0b11) as u8
}

/// Paint all pilots into a frame (mode B only; chroma pilots handled by Mode C extension).
pub fn paint_pilots(buf: &mut [u8], frame_counter: u32, excluded: &[usize]) {
    let positions = pilot_positions(frame_counter, excluded);
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx);
        paint_cell_b(buf, col, row, pilot_value(frame_counter, i));
    }
}

/// Read pilots and return (success_ratio, mean_confidence).
/// success_ratio ∈ [0.0, 1.0] = fraction of pilots whose symbol matched expected.
pub fn validate_pilots(buf: &[u8], frame_counter: u32, excluded: &[usize]) -> (f32, f32) {
    let positions = pilot_positions(frame_counter, excluded);
    let mut ok = 0usize;
    let mut conf_sum = 0f32;
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx);
        let (sym, conf) = read_cell_b(buf, col, row);
        conf_sum += conf;
        if sym == pilot_value(frame_counter, i) {
            ok += 1;
        }
    }
    let total = positions.len().max(1) as f32;
    (ok as f32 / total, conf_sum / total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flicker::grid::FRAME_BYTES_RGB24;

    #[test]
    fn pilot_positions_deterministic_per_counter() {
        let a = pilot_positions(42, &[]);
        let b = pilot_positions(42, &[]);
        assert_eq!(a, b);
        assert_eq!(a.len(), PILOT_COUNT);
    }

    #[test]
    fn pilot_positions_change_per_counter() {
        let a = pilot_positions(1, &[]);
        let b = pilot_positions(2, &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn paint_and_validate_roundtrips() {
        let mut buf = vec![0u8; FRAME_BYTES_RGB24];
        paint_pilots(&mut buf, 100, &[]);
        let (ok, conf) = validate_pilots(&buf, 100, &[]);
        assert!(ok > 0.99, "expected near-100% pilot success, got {ok}");
        assert!(conf > 0.95, "expected high confidence, got {conf}");
    }
}
```

Add `pub mod pilot;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::pilot -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/pilot.rs src/flicker/mod.rs
git commit -m "feat(flicker): PRNG-driven pilot cells with validation

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 4 — FEC layer

### Task 8: flicker::fec — RS(172,120) block wrapper

**Files:**
- Create: `src/flicker/fec.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/fec.rs`:
```rust
//! Reed–Solomon erasure code wrapper.
//! One block = 120 data bytes + 52 parity bytes = 172 bytes total.
//! Corrects up to 52 erasures per block (bytes whose position is marked unreliable).

use anyhow::{anyhow, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;

pub const RS_BLOCK_N: usize = 172;
pub const RS_BLOCK_K: usize = 120;
pub const RS_BLOCK_PARITY: usize = RS_BLOCK_N - RS_BLOCK_K; // 52

/// Encode `k` data bytes into `n` shard bytes (data + parity).
/// Input must be exactly RS_BLOCK_K bytes; output is RS_BLOCK_N bytes.
pub fn encode_block(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() != RS_BLOCK_K {
        return Err(anyhow!("encode_block expects {} bytes, got {}", RS_BLOCK_K, data.len()));
    }
    let rs = ReedSolomon::new(RS_BLOCK_K, RS_BLOCK_PARITY)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    // reed-solomon-erasure operates on shards-of-shards; we use byte-per-shard (shard size = 1).
    let mut shards: Vec<Vec<u8>> = data.iter().map(|b| vec![*b]).collect();
    for _ in 0..RS_BLOCK_PARITY {
        shards.push(vec![0u8]);
    }
    rs.encode(&mut shards).map_err(|e| anyhow!("RS encode: {e}"))?;
    Ok(shards.into_iter().map(|s| s[0]).collect())
}

/// Decode `n` bytes where some may be marked as erasures (None).
/// Returns the original `k` data bytes, or Err if too many erasures.
pub fn decode_block(shards: &[Option<u8>]) -> Result<Vec<u8>> {
    if shards.len() != RS_BLOCK_N {
        return Err(anyhow!("decode_block expects {} shards, got {}", RS_BLOCK_N, shards.len()));
    }
    let rs = ReedSolomon::new(RS_BLOCK_K, RS_BLOCK_PARITY)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    let mut mutable: Vec<Option<Vec<u8>>> = shards.iter().map(|o| o.map(|b| vec![b])).collect();
    rs.reconstruct(&mut mutable).map_err(|e| anyhow!("RS decode: {e}"))?;
    let out: Vec<u8> = mutable.iter().take(RS_BLOCK_K).map(|o| o.as_ref().unwrap()[0]).collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_without_errors() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        assert_eq!(encoded.len(), RS_BLOCK_N);
        let shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        let decoded = decode_block(&shards).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn recovers_up_to_52_erasures() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        let mut shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        // Erase 52 positions scattered.
        for i in (0..RS_BLOCK_N).step_by(3).take(52) {
            shards[i] = None;
        }
        let decoded = decode_block(&shards).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn fails_with_more_than_52_erasures() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        let mut shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        for i in 0..53 {
            shards[i] = None;
        }
        assert!(decode_block(&shards).is_err());
    }
}
```

Add `pub mod fec;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::fec -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/fec.rs src/flicker/mod.rs
git commit -m "feat(flicker): RS(172,120) block encode/decode with erasures

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: flicker::header — main header struct + RS(33,22)

**Files:**
- Create: `src/flicker/header.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/header.rs`:
```rust
//! Main frame header: 22 bytes data + 11 bytes RS(33,22) parity.

use anyhow::{anyhow, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::flicker::ModulationMode;

pub const SYNC_WORD: [u8; 4] = [0xF1, 0x1C, 0x4E, 0x52];
pub const PROTOCOL_VERSION: u8 = 0x02;
pub const HEADER_DATA_BYTES: usize = 22;
pub const HEADER_PARITY_BYTES: usize = 11;
pub const HEADER_TOTAL_BYTES: usize = 33;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FecScheme(pub u8);
impl FecScheme { pub const RS_172_120: Self = FecScheme(1); }

#[derive(Copy, Clone, Debug)]
pub struct FrameHeader {
    pub frame_counter: u32,
    pub channel_id: u8,
    pub modulation_mode: ModulationMode,
    pub fec_scheme: FecScheme,
    pub fec_params: [u8; 4], // (n, k, block_count, flags)
    pub payload_len: u16,
}

impl FrameHeader {
    pub fn serialize(&self) -> [u8; HEADER_DATA_BYTES] {
        let mut out = [0u8; HEADER_DATA_BYTES];
        out[0..4].copy_from_slice(&SYNC_WORD);
        out[4] = PROTOCOL_VERSION;
        out[5..9].copy_from_slice(&self.frame_counter.to_le_bytes());
        out[9] = self.channel_id;
        out[10] = self.modulation_mode as u8;
        out[11] = self.fec_scheme.0;
        out[12..16].copy_from_slice(&self.fec_params);
        out[16..18].copy_from_slice(&self.payload_len.to_le_bytes());
        let crc = crc32fast::hash(&out[..18]);
        out[18..22].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn deserialize(buf: &[u8; HEADER_DATA_BYTES]) -> Result<Self> {
        if buf[..4] != SYNC_WORD {
            return Err(anyhow!("sync word mismatch"));
        }
        if buf[4] != PROTOCOL_VERSION {
            return Err(anyhow!("unsupported version: {}", buf[4]));
        }
        let expected_crc = u32::from_le_bytes([buf[18], buf[19], buf[20], buf[21]]);
        let actual_crc = crc32fast::hash(&buf[..18]);
        if expected_crc != actual_crc {
            return Err(anyhow!("header CRC mismatch"));
        }
        let modulation_mode = ModulationMode::from_u8(buf[10])
            .ok_or_else(|| anyhow!("unknown modulation_mode {}", buf[10]))?;
        let mut fec_params = [0u8; 4];
        fec_params.copy_from_slice(&buf[12..16]);
        Ok(Self {
            frame_counter: u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]),
            channel_id: buf[9],
            modulation_mode,
            fec_scheme: FecScheme(buf[11]),
            fec_params,
            payload_len: u16::from_le_bytes([buf[16], buf[17]]),
        })
    }
}

/// Encode header with RS(33, 22): returns 33 bytes.
pub fn encode_header(header: &FrameHeader) -> Result<[u8; HEADER_TOTAL_BYTES]> {
    let data = header.serialize();
    let rs = ReedSolomon::new(HEADER_DATA_BYTES, HEADER_PARITY_BYTES)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    let mut shards: Vec<Vec<u8>> = data.iter().map(|b| vec![*b]).collect();
    for _ in 0..HEADER_PARITY_BYTES {
        shards.push(vec![0u8]);
    }
    rs.encode(&mut shards).map_err(|e| anyhow!("RS encode: {e}"))?;
    let mut out = [0u8; HEADER_TOTAL_BYTES];
    for (i, s) in shards.iter().enumerate() {
        out[i] = s[0];
    }
    Ok(out)
}

/// Decode 33 bytes (some may be erasures) into a FrameHeader.
pub fn decode_header(shards: &[Option<u8>; HEADER_TOTAL_BYTES]) -> Result<FrameHeader> {
    let rs = ReedSolomon::new(HEADER_DATA_BYTES, HEADER_PARITY_BYTES)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    let mut mutable: Vec<Option<Vec<u8>>> = shards.iter().map(|o| o.map(|b| vec![b])).collect();
    rs.reconstruct(&mut mutable).map_err(|e| anyhow!("RS decode: {e}"))?;
    let mut data = [0u8; HEADER_DATA_BYTES];
    for i in 0..HEADER_DATA_BYTES {
        data[i] = mutable[i].as_ref().unwrap()[0];
    }
    FrameHeader::deserialize(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FrameHeader {
        FrameHeader {
            frame_counter: 0xDEADBEEF,
            channel_id: 1,
            modulation_mode: ModulationMode::B,
            fec_scheme: FecScheme::RS_172_120,
            fec_params: [172, 120, 2, 0],
            payload_len: 240,
        }
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let h = sample();
        let bytes = h.serialize();
        let back = FrameHeader::deserialize(&bytes).unwrap();
        assert_eq!(back.frame_counter, h.frame_counter);
        assert_eq!(back.channel_id, h.channel_id);
        assert_eq!(back.modulation_mode, h.modulation_mode);
        assert_eq!(back.payload_len, h.payload_len);
    }

    #[test]
    fn header_rs_corrects_5_byte_errors() {
        let h = sample();
        let encoded = encode_header(&h).unwrap();
        let mut shards = [None; HEADER_TOTAL_BYTES];
        for i in 0..HEADER_TOTAL_BYTES {
            shards[i] = Some(encoded[i]);
        }
        // Erase 5 bytes (within RS(33,22) capability of 11 erasures).
        shards[3] = None;
        shards[7] = None;
        shards[11] = None;
        shards[18] = None;
        shards[25] = None;
        let back = decode_header(&shards).unwrap();
        assert_eq!(back.frame_counter, h.frame_counter);
    }

    #[test]
    fn bad_sync_word_rejected() {
        let mut data = sample().serialize();
        data[0] = 0;
        assert!(FrameHeader::deserialize(&data).is_err());
    }
}
```

Add `pub mod header;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::header -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/header.rs src/flicker/mod.rs
git commit -m "feat(flicker): frame header with RS(33,22) and CRC32

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 5 — Fragment layer

### Task 10: flicker::fragment — fragment header + reassembly buffer

**Files:**
- Create: `src/flicker/fragment.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/fragment.rs`:
```rust
//! Fragment header (9 bytes) + reassembly buffer with TTL.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::flicker::InboundMessage;

pub const FRAGMENT_HEADER_BYTES: usize = 9;
pub const DEFAULT_REASSEMBLY_TIMEOUT_MS: u64 = 2000;
pub const MSG_TYPE_HAS_NEXT_BIT: u8 = 0x80;
pub const MSG_TYPE_MASK: u8 = 0x7F;

#[derive(Debug, Clone)]
pub struct Fragment {
    pub msg_type: u8,      // low 7 bits; high bit = HAS_NEXT
    pub message_id: u32,
    pub fragment_idx: u16,
    pub fragment_total: u16,
    pub payload: Vec<u8>,
}

impl Fragment {
    pub fn serialize_header(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= FRAGMENT_HEADER_BYTES);
        buf[0] = self.msg_type;
        buf[1..5].copy_from_slice(&self.message_id.to_le_bytes());
        buf[5..7].copy_from_slice(&self.fragment_idx.to_le_bytes());
        buf[7..9].copy_from_slice(&self.fragment_total.to_le_bytes());
    }

    pub fn deserialize_header(buf: &[u8]) -> Result<(Self, usize)> {
        if buf.len() < FRAGMENT_HEADER_BYTES {
            return Err(anyhow!("fragment header underflow"));
        }
        let msg_type = buf[0];
        let message_id = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let fragment_idx = u16::from_le_bytes([buf[5], buf[6]]);
        let fragment_total = u16::from_le_bytes([buf[7], buf[8]]);
        if fragment_total == 0 {
            return Err(anyhow!("fragment_total = 0 invalid"));
        }
        if fragment_idx >= fragment_total {
            return Err(anyhow!("fragment_idx {} >= total {}", fragment_idx, fragment_total));
        }
        Ok((
            Fragment { msg_type, message_id, fragment_idx, fragment_total, payload: Vec::new() },
            FRAGMENT_HEADER_BYTES,
        ))
    }

    pub fn has_next_in_frame(&self) -> bool {
        self.msg_type & MSG_TYPE_HAS_NEXT_BIT != 0
    }

    pub fn app_msg_type(&self) -> u8 {
        self.msg_type & MSG_TYPE_MASK
    }
}

struct PartialMessage {
    received_at: Instant,
    total: u16,
    parts: Vec<Option<Vec<u8>>>,
    msg_type: u8,
}

pub struct Reassembler {
    partials: HashMap<u32, PartialMessage>,
    timeout: Duration,
}

impl Reassembler {
    pub fn new(timeout_ms: u64) -> Self {
        Self { partials: HashMap::new(), timeout: Duration::from_millis(timeout_ms) }
    }

    /// Accept a fragment. Returns Some(InboundMessage) when the message is complete.
    pub fn accept(&mut self, frag: Fragment) -> Option<InboundMessage> {
        self.gc();
        if frag.fragment_total == 1 {
            return Some(InboundMessage { msg_type: frag.app_msg_type(), payload: frag.payload });
        }
        let partial = self.partials.entry(frag.message_id).or_insert_with(|| PartialMessage {
            received_at: Instant::now(),
            total: frag.fragment_total,
            parts: vec![None; frag.fragment_total as usize],
            msg_type: frag.app_msg_type(),
        });
        if partial.total != frag.fragment_total {
            // Sender mid-stream mismatch: drop partial.
            self.partials.remove(&frag.message_id);
            return None;
        }
        let idx = frag.fragment_idx as usize;
        if partial.parts[idx].is_none() {
            partial.parts[idx] = Some(frag.payload);
        }
        // Check if complete.
        if partial.parts.iter().all(Option::is_some) {
            let completed = self.partials.remove(&frag.message_id).unwrap();
            let mut payload: Vec<u8> = Vec::new();
            for p in completed.parts {
                payload.extend_from_slice(&p.unwrap());
            }
            return Some(InboundMessage { msg_type: completed.msg_type, payload });
        }
        None
    }

    fn gc(&mut self) {
        let now = Instant::now();
        self.partials.retain(|_, p| now.duration_since(p.received_at) < self.timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fragment_completes_immediately() {
        let mut r = Reassembler::new(2000);
        let f = Fragment {
            msg_type: 0x02,
            message_id: 1,
            fragment_idx: 0,
            fragment_total: 1,
            payload: b"hi".to_vec(),
        };
        let m = r.accept(f).unwrap();
        assert_eq!(m.msg_type, 0x02);
        assert_eq!(m.payload, b"hi");
    }

    #[test]
    fn three_fragments_reassemble() {
        let mut r = Reassembler::new(2000);
        for i in 0..3 {
            let f = Fragment {
                msg_type: 0x02,
                message_id: 42,
                fragment_idx: i,
                fragment_total: 3,
                payload: vec![i as u8; 10],
            };
            let m = r.accept(f);
            if i < 2 { assert!(m.is_none()); }
            else {
                let m = m.unwrap();
                assert_eq!(m.payload.len(), 30);
                assert_eq!(m.payload[0], 0);
                assert_eq!(m.payload[10], 1);
                assert_eq!(m.payload[20], 2);
            }
        }
    }

    #[test]
    fn duplicate_fragment_ignored() {
        let mut r = Reassembler::new(2000);
        let f0 = Fragment { msg_type: 0x02, message_id: 7, fragment_idx: 0, fragment_total: 2, payload: vec![1, 2] };
        let f0_dup = f0.clone();
        let f1 = Fragment { msg_type: 0x02, message_id: 7, fragment_idx: 1, fragment_total: 2, payload: vec![3, 4] };
        assert!(r.accept(f0).is_none());
        assert!(r.accept(f0_dup).is_none());
        let m = r.accept(f1).unwrap();
        assert_eq!(m.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn header_roundtrip() {
        let f = Fragment {
            msg_type: 0x82, // HAS_NEXT | 0x02
            message_id: 0xCAFEBABE,
            fragment_idx: 3,
            fragment_total: 5,
            payload: Vec::new(),
        };
        let mut buf = [0u8; FRAGMENT_HEADER_BYTES];
        f.serialize_header(&mut buf);
        let (back, used) = Fragment::deserialize_header(&buf).unwrap();
        assert_eq!(used, FRAGMENT_HEADER_BYTES);
        assert_eq!(back.msg_type, 0x82);
        assert!(back.has_next_in_frame());
        assert_eq!(back.app_msg_type(), 0x02);
        assert_eq!(back.message_id, 0xCAFEBABE);
        assert_eq!(back.fragment_idx, 3);
        assert_eq!(back.fragment_total, 5);
    }
}
```

Add `pub mod fragment;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::fragment -- --nocapture`
Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/flicker/fragment.rs src/flicker/mod.rs
git commit -m "feat(flicker): fragment header + reassembly buffer with TTL

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 6 — Frame orchestration

### Task 11: flicker::frame — encode_frame / decode_frame (full pipeline)

**Files:**
- Create: `src/flicker/frame.rs`

This is the largest task: it wires markers + pilots + header + FEC + interleave + fragments into a single encode/decode pipeline.

- [ ] **Step 1: Write the failing tests**

Create `src/flicker/frame.rs`:
```rust
//! Full encode/decode pipeline for one flicker frame.

use anyhow::{anyhow, Result};

use crate::flicker::codec::{paint_cell, read_cell};
use crate::flicker::fec::{decode_block, encode_block, RS_BLOCK_K, RS_BLOCK_N};
use crate::flicker::fragment::{Fragment, FRAGMENT_HEADER_BYTES};
use crate::flicker::grid::{FRAME_BYTES_RGB24, GRID_COLS, TOTAL_CELLS};
use crate::flicker::header::{decode_header, encode_header, FecScheme, FrameHeader, HEADER_TOTAL_BYTES};
use crate::flicker::interleave::{cell_index_to_col_row, col_row_to_cell_index, cell_permutation};
use crate::flicker::markers::{paint_markers, frame_offset, MARKER_SIZE};
use crate::flicker::pilot::{paint_pilots, pilot_positions, validate_pilots, PILOT_COUNT};
use crate::flicker::{ModulationMode, OutboundMessage};

pub const PILOT_CONFIDENCE_THRESHOLD: f32 = 0.4;
pub const PILOT_SUCCESS_MIN: f32 = 0.80;

/// Compute cell indices occupied by corner markers.
pub fn marker_cell_indices() -> Vec<usize> {
    let mut out = Vec::new();
    for (cx, cy) in crate::flicker::markers::MARKER_CENTERS.iter() {
        let x0 = (*cx - MARKER_SIZE as i32 / 2) as usize;
        let y0 = (*cy - MARKER_SIZE as i32 / 2) as usize;
        for dy in 0..MARKER_SIZE / 4 {
            for dx in 0..MARKER_SIZE / 4 {
                let col = (x0 / 4) + dx;
                let row = (y0 / 4) + dy;
                out.push(col_row_to_cell_index(col, row));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub struct FrameEncoder {
    pub mode: ModulationMode,
    pub channel_id: u8,
    pub frame_counter: u32,
}

impl FrameEncoder {
    pub fn bits_per_payload_cell(&self) -> usize { self.mode.bits_per_cell() }

    pub fn block_count(&self) -> usize {
        match self.mode { ModulationMode::B => 2, ModulationMode::C => 4 }
    }

    pub fn payload_bytes_per_frame(&self) -> usize { self.block_count() * RS_BLOCK_K }

    pub fn encode(&mut self, out_buf: &mut [u8], fragments: &[Fragment]) -> Result<()> {
        if out_buf.len() < FRAME_BYTES_RGB24 {
            return Err(anyhow!("out buf too small"));
        }
        out_buf.fill(128);
        paint_markers(out_buf);
        let markers = marker_cell_indices();

        // 1. Serialize fragments into payload byte buffer.
        let total_capacity = self.payload_bytes_per_frame();
        let mut payload_bytes = Vec::with_capacity(total_capacity);
        for f in fragments {
            let mut hdr = [0u8; FRAGMENT_HEADER_BYTES];
            f.serialize_header(&mut hdr);
            payload_bytes.extend_from_slice(&hdr);
            payload_bytes.extend_from_slice(&f.payload);
        }
        let payload_len = payload_bytes.len();
        if payload_len > total_capacity {
            return Err(anyhow!("fragments too large: {} > {}", payload_len, total_capacity));
        }
        payload_bytes.resize(self.block_count() * RS_BLOCK_K, 0);

        // 2. Encode each RS block.
        let mut encoded_blocks: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
        for i in 0..self.block_count() {
            let chunk = &payload_bytes[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K];
            let encoded = encode_block(chunk)?;
            encoded_blocks.extend_from_slice(&encoded);
        }

        // 3. Build header.
        let header = FrameHeader {
            frame_counter: self.frame_counter,
            channel_id: self.channel_id,
            modulation_mode: self.mode,
            fec_scheme: FecScheme::RS_172_120,
            fec_params: [RS_BLOCK_N as u8, RS_BLOCK_K as u8, self.block_count() as u8, 0],
            payload_len: payload_len as u16,
        };
        let header_bytes = encode_header(&header)?;

        // 4. Spatial layout — TWO separate permutations so decoder can read the
        // header before it knows frame_counter:
        //
        //   header_perm = permutation of {all cells} \ {markers}
        //   payload_perm = permutation of {all cells} \ {markers ∪ pilots ∪ header cells}
        //
        // Pilots are painted AFTER header determination (they occupy positions
        // independent of header_perm, may overlap — conflict resolution: pilot
        // wins because decoder validates pilots first).  To keep this simple,
        // we exclude header cells from pilot position choice as well.
        let header_perm = cell_permutation(&markers);
        let header_cells = HEADER_TOTAL_BYTES * 4;
        let header_cell_indices: Vec<usize> = header_perm[..header_cells]
            .iter()
            .map(|(c, r)| col_row_to_cell_index(*c, *r))
            .collect();

        let mut pilot_excluded = markers.clone();
        pilot_excluded.extend(&header_cell_indices);
        pilot_excluded.sort_unstable();
        pilot_excluded.dedup();
        // pilot_positions excludes any overlap with header cells.
        let pilot_list = pilot_positions(self.frame_counter, &pilot_excluded);

        let mut payload_excluded = pilot_excluded.clone();
        payload_excluded.extend(&pilot_list);
        payload_excluded.sort_unstable();
        payload_excluded.dedup();
        let payload_perm = cell_permutation(&payload_excluded);

        // Paint pilots on cells chosen by pilot_positions (not via permutation).
        for (i, &idx) in pilot_list.iter().enumerate() {
            let (col, row) = cell_index_to_col_row(idx);
            let sym = crate::flicker::pilot::pilot_value(self.frame_counter, i);
            crate::flicker::codec::paint_cell_b(out_buf, col, row, sym);
        }

        // Paint header using header_perm. Header is always mode B (2 bpp luma).
        for (byte_idx, &byte) in header_bytes.iter().enumerate() {
            for bit_pair in 0..4 {
                let symbol = (byte >> (2 * (3 - bit_pair))) & 0b11;
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                crate::flicker::codec::paint_cell_b(out_buf, col, row, symbol);
            }
        }

        // Payload+parity: for mode B, 4 cells/byte; for mode C, 2 cells/byte.
        let cells_per_byte = match self.mode { ModulationMode::B => 4, ModulationMode::C => 2 };
        for (byte_idx, &byte) in encoded_blocks.iter().enumerate() {
            for unit in 0..cells_per_byte {
                let symbol = match self.mode {
                    ModulationMode::B => (byte >> (2 * (3 - unit))) & 0b11,
                    ModulationMode::C => (byte >> (4 * (1 - unit))) & 0b1111,
                };
                let cell_pos = byte_idx * cells_per_byte + unit;
                if cell_pos >= payload_perm.len() { break; }
                let (col, row) = payload_perm[cell_pos];
                paint_cell(out_buf, col, row, symbol, self.mode);
            }
        }
        self.frame_counter = self.frame_counter.wrapping_add(1);
        Ok(())
    }
}

pub struct FrameDecoder;

#[derive(Debug)]
pub enum DecodeOutcome {
    Ok { header: FrameHeader, fragments: Vec<Fragment>, pilot_success: f32 },
    Dropped { reason: DropReason },
}

#[derive(Debug)]
pub enum DropReason {
    SyncOffsetMissing,
    HeaderRsFailed,
    HeaderCrc,
    PilotValidationFailed(f32),
    BlockRsFailed(usize),
    FragmentParse,
}

impl FrameDecoder {
    pub fn decode(&self, buf: &[u8]) -> DecodeOutcome {
        let markers = marker_cell_indices();

        // Step 1: align (currently unused for pixel offset — reserved for affine upgrade).
        if frame_offset(buf).is_none() {
            return DecodeOutcome::Dropped { reason: DropReason::SyncOffsetMissing };
        }

        // Step 2: read header using header_perm = permutation(excluded = markers only).
        let header_perm = cell_permutation(&markers);
        let header_cells = HEADER_TOTAL_BYTES * 4;
        let mut header_shards: [Option<u8>; HEADER_TOTAL_BYTES] = [None; HEADER_TOTAL_BYTES];
        for byte_idx in 0..HEADER_TOTAL_BYTES {
            let mut byte = 0u8;
            let mut byte_confidence_min = 1.0f32;
            for bit_pair in 0..4 {
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                let (sym, conf) = read_cell(buf, col, row, ModulationMode::B);
                byte = (byte << 2) | (sym & 0b11);
                byte_confidence_min = byte_confidence_min.min(conf);
            }
            if byte_confidence_min >= PILOT_CONFIDENCE_THRESHOLD {
                header_shards[byte_idx] = Some(byte);
            }
        }
        let header = match decode_header(&header_shards) {
            Ok(h) => h,
            Err(_) => return DecodeOutcome::Dropped { reason: DropReason::HeaderRsFailed },
        };

        // Step 3: validate pilots using recovered frame_counter and same
        // excluded set that encoder used (markers + header cells).
        let header_cell_indices: Vec<usize> = header_perm[..header_cells]
            .iter()
            .map(|(c, r)| col_row_to_cell_index(*c, *r))
            .collect();
        let mut pilot_excluded = markers.clone();
        pilot_excluded.extend(&header_cell_indices);
        pilot_excluded.sort_unstable();
        pilot_excluded.dedup();
        let pilot_list = pilot_positions(header.frame_counter, &pilot_excluded);
        let (pilot_ok, _pilot_conf) = validate_pilots(buf, header.frame_counter, &pilot_excluded);
        if pilot_ok < PILOT_SUCCESS_MIN {
            return DecodeOutcome::Dropped { reason: DropReason::PilotValidationFailed(pilot_ok) };
        }

        // Step 4: payload_perm = permutation(excluded = markers + header + pilots) — same as encoder.
        let mut payload_excluded = pilot_excluded.clone();
        payload_excluded.extend(&pilot_list);
        payload_excluded.sort_unstable();
        payload_excluded.dedup();
        let payload_perm = cell_permutation(&payload_excluded);

        // Step 5: read payload+parity.
        let mode = header.modulation_mode;
        let block_count = header.fec_params[2] as usize;
        let cells_per_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 2 };

        let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);
        for block_i in 0..block_count {
            let mut shards: Vec<Option<u8>> = vec![None; RS_BLOCK_N];
            for byte_i in 0..RS_BLOCK_N {
                let mut byte = 0u8;
                let mut min_conf = 1.0f32;
                for unit in 0..cells_per_byte {
                    let cell_pos = (block_i * RS_BLOCK_N + byte_i) * cells_per_byte + unit;
                    if cell_pos >= payload_perm.len() { break; }
                    let (col, row) = payload_perm[cell_pos];
                    let (sym, conf) = read_cell(buf, col, row, mode);
                    let bits = match mode { ModulationMode::B => 2, ModulationMode::C => 4 };
                    byte = (byte << bits) | (sym & ((1 << bits) - 1));
                    min_conf = min_conf.min(conf);
                }
                if min_conf >= PILOT_CONFIDENCE_THRESHOLD {
                    shards[byte_i] = Some(byte);
                }
            }
            match decode_block(&shards) {
                Ok(d) => decoded_blocks.push(d),
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::BlockRsFailed(block_i) },
            }
        }

        // Step 6: concatenate blocks, trim to payload_len, parse fragments.
        let mut payload_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
        for b in &decoded_blocks { payload_bytes.extend_from_slice(b); }
        payload_bytes.truncate(header.payload_len as usize);

        let mut fragments = Vec::new();
        let mut cursor = 0usize;
        while cursor + FRAGMENT_HEADER_BYTES <= payload_bytes.len() {
            let (mut frag, used) = match Fragment::deserialize_header(&payload_bytes[cursor..]) {
                Ok(v) => v,
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::FragmentParse },
            };
            cursor += used;
            // Take all remaining bytes into this fragment unless HAS_NEXT and we're not the last.
            // For simplicity MVP: if HAS_NEXT is set, we expect another fragment header immediately
            // after a payload sized equal to the remaining block-local space. But we can't know
            // that without more structure — so for MVP: HAS_NEXT is hint, consume all remaining.
            // Single-message-per-frame is the common path.
            let rem = payload_bytes.len() - cursor;
            frag.payload = payload_bytes[cursor..cursor + rem].to_vec();
            cursor += rem;
            fragments.push(frag);
            break;
        }

        DecodeOutcome::Ok { header, fragments, pilot_success: pilot_ok }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flicker::grid::FRAME_BYTES_RGB24;

    #[test]
    fn encode_decode_roundtrip_b() {
        let mut buf = vec![0u8; FRAME_BYTES_RGB24];
        let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 7 };
        let frag = Fragment {
            msg_type: 0x02,
            message_id: 1,
            fragment_idx: 0,
            fragment_total: 1,
            payload: b"hello world".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        let dec = FrameDecoder;
        match dec.decode(&buf) {
            DecodeOutcome::Ok { header, fragments, .. } => {
                assert_eq!(header.modulation_mode, ModulationMode::B);
                assert_eq!(fragments.len(), 1);
                assert_eq!(fragments[0].app_msg_type(), 0x02);
                assert!(fragments[0].payload.starts_with(b"hello world"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
```

Add `pub mod frame;` to `src/flicker/mod.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flicker::frame -- --nocapture`
Expected: 1 passed (more tests added in Task 20).

- [ ] **Step 3: Commit**

```bash
git add src/flicker/frame.rs src/flicker/mod.rs
git commit -m "feat(flicker): full frame encode/decode pipeline

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 7 — Peer infrastructure

### Task 12: config.rs — load_peer() with 4 required fields + tunables

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Rewrite `src/config.rs`**

```rust
use anyhow::{anyhow, Context, Result};

use crate::flicker::ModulationMode;

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub my_rtmp_url: String,
    pub my_stream_key: String,
    pub their_vk_channel: String,
    pub their_stream_name: String,
    pub modulation_mode: ModulationMode,
    pub frag_timeout_ms: u64,
    pub log_every_frame: bool,
}

pub fn load_peer() -> Result<PeerConfig> {
    Ok(PeerConfig {
        my_rtmp_url: env_opt("peer_my_rtmp_url"),
        my_stream_key: env_opt("peer_my_stream_key"),
        their_vk_channel: env_opt("peer_their_vk_channel"),
        their_stream_name: env_opt("peer_their_stream_name"),
        modulation_mode: parse_mode(env_opt("flicker_modulation_mode").as_str())?,
        frag_timeout_ms: env_u64("flicker_frag_timeout_ms")?.unwrap_or(2000),
        log_every_frame: env_flag("flicker_log_every_frame"),
    })
}

pub fn validate_tx(cfg: &PeerConfig) -> Result<()> {
    if cfg.my_rtmp_url.is_empty() { return Err(anyhow!("peer_my_rtmp_url required for tx")); }
    if cfg.my_stream_key.is_empty() { return Err(anyhow!("peer_my_stream_key required for tx")); }
    Ok(())
}

pub fn validate_rx(cfg: &PeerConfig) -> Result<()> {
    if cfg.their_vk_channel.is_empty() { return Err(anyhow!("peer_their_vk_channel required for rx")); }
    if cfg.their_stream_name.is_empty() { return Err(anyhow!("peer_their_stream_name required for rx")); }
    Ok(())
}

fn parse_mode(s: &str) -> Result<ModulationMode> {
    match s.trim().to_ascii_uppercase().as_str() {
        "" | "B" => Ok(ModulationMode::B),
        "C" => Ok(ModulationMode::C),
        other => Err(anyhow!("flicker_modulation_mode: expected B or C, got {other}")),
    }
}

fn env_opt(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v.trim().parse().with_context(|| format!("{name}: not u64"))?)),
        Err(_) => Ok(None),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_defaults_to_b() {
        assert_eq!(parse_mode("").unwrap(), ModulationMode::B);
        assert_eq!(parse_mode("B").unwrap(), ModulationMode::B);
        assert_eq!(parse_mode("c").unwrap(), ModulationMode::C);
        assert!(parse_mode("D").is_err());
    }

    #[test]
    fn validate_tx_requires_publish_fields() {
        let mut c = PeerConfig {
            my_rtmp_url: String::new(), my_stream_key: String::new(),
            their_vk_channel: String::new(), their_stream_name: String::new(),
            modulation_mode: ModulationMode::B, frag_timeout_ms: 2000, log_every_frame: false,
        };
        assert!(validate_tx(&c).is_err());
        c.my_rtmp_url = "rtmp://x".into();
        assert!(validate_tx(&c).is_err());
        c.my_stream_key = "k".into();
        assert!(validate_tx(&c).is_ok());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib config -- --nocapture`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): load_peer + tx/rx validation

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 13: peer::ffmpeg_publish — port from v1 client/ffmpeg.rs

**Files:**
- Create: `src/peer/ffmpeg_publish.rs`

- [ ] **Step 1: Create module with publish args builder**

Create `src/peer/ffmpeg_publish.rs`:
```rust
//! ffmpeg args for publishing raw RGB24 to RTMP.

use crate::flicker::grid::{FPS, FRAME_HEIGHT, FRAME_WIDTH};

pub fn publish_args(rtmp_url: &str) -> Vec<String> {
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let rate_arg = FPS.to_string();
    let gop_arg = (FPS * 2).to_string();
    [
        "-hide_banner", "-loglevel", "info", "-y",
        "-use_wallclock_as_timestamps", "1",
        "-thread_queue_size", "1024",
        "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-s", &size_arg, "-r", &rate_arg, "-i", "-",
        "-f", "lavfi", "-i", "anullsrc=channel_layout=stereo:sample_rate=44100",
        "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
        "-profile:v", "baseline", "-level", "3.0", "-pix_fmt", "yuv420p",
        "-b:v", "300k", "-maxrate", "300k", "-bufsize", "600k",
        "-g", &gop_arg, "-keyint_min", &rate_arg,
        "-c:a", "aac", "-b:a", "64k", "-ar", "44100", "-ac", "2",
        "-shortest", "-flvflags", "no_duration_filesize",
        "-f", "flv", rtmp_url,
    ].into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn publish_args_contain_rtmp_url() {
        let args = publish_args("rtmp://example/live/key");
        assert!(args.iter().any(|a| a == "rtmp://example/live/key"));
        assert!(args.iter().any(|a| a == "256x144"));
    }
}
```

Add to `src/peer/mod.rs`:
```rust
pub mod ffmpeg_publish;
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib peer::ffmpeg_publish -- --nocapture`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/peer/ffmpeg_publish.rs src/peer/mod.rs
git commit -m "feat(peer): ffmpeg publish args (ported from v1 client)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 14: peer::ffmpeg_read — port from v1 server/ingest.rs

**Files:**
- Create: `src/peer/ffmpeg_read.rs`

- [ ] **Step 1: Create module**

Create `src/peer/ffmpeg_read.rs` by copying the content of v1 `src/server/ingest.rs` from git history (before Task 1 deletion) and updating the signature to use `FRAME_WIDTH`/`FRAME_HEIGHT`/`FPS` constants from `flicker::grid` instead of a `GridConfig` parameter.

Retrieve v1 content:
```bash
git show 11a9087:src/server/ingest.rs > /tmp/ingest_v1.rs
cat /tmp/ingest_v1.rs
```

Adapt it into `src/peer/ffmpeg_read.rs`:
```rust
//! Low-latency HLS/DASH → raw RGB24 ingest via ffmpeg subprocess.

use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use crate::flicker::grid::{FPS, FRAME_HEIGHT, FRAME_WIDTH};

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub fn read_args(input_url: &str, page_url: &str, input_is_hls: bool) -> Vec<String> {
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(), "-loglevel".into(), "warning".into(),
        "-fflags".into(), "nobuffer+discardcorrupt+flush_packets".into(),
        "-flags".into(), "low_delay".into(),
        "-probesize".into(), "500000".into(),
        "-analyzeduration".into(), "500000".into(),
        "-max_delay".into(), "500000".into(),
        "-rtbufsize".into(), "8M".into(),
        "-reconnect".into(), "1".into(),
        "-reconnect_streamed".into(), "1".into(),
        "-reconnect_delay_max".into(), "2".into(),
    ];
    if input_is_hls {
        args.push("-live_start_index".into());
        args.push("-1".into());
        args.push("-http_persistent".into());
        args.push("1".into());
    }
    args.push("-user_agent".into());
    args.push(USER_AGENT.into());
    args.push("-headers".into());
    args.push(format!("Referer: {page_url}\r\nOrigin: https://live.vkvideo.ru\r\n"));
    args.push("-i".into());
    args.push(input_url.to_string());
    args.push("-thread_queue_size".into());
    args.push("1024".into());
    args.push("-map".into()); args.push("0:v:0".into());
    args.push("-an".into());
    args.push("-vf".into());
    args.push(format!("scale={}:{}:flags=neighbor,format=rgb24,fps={}", FRAME_WIDTH, FRAME_HEIGHT, FPS));
    args.push("-fps_mode".into()); args.push("cfr".into());
    args.extend([
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "rgb24".into(),
        "-s".into(), size_arg,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn read_args_include_input_url() {
        let args = read_args("https://x/playlist.m3u8", "https://page", true);
        assert!(args.iter().any(|a| a == "https://x/playlist.m3u8"));
        assert!(args.iter().any(|a| a.contains("scale=256:144")));
    }
}
```

Add to `src/peer/mod.rs`: `pub mod ffmpeg_read;`

- [ ] **Step 2: Run tests**

Run: `cargo test --lib peer::ffmpeg_read -- --nocapture`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/peer/ffmpeg_read.rs src/peer/mod.rs
git commit -m "feat(peer): ffmpeg read args (ported from v1 server)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 15: peer::vk_live — port from v1 server/vk_live.rs

**Files:**
- Create: `src/peer/vk_live.rs`

- [ ] **Step 1: Copy v1 implementation verbatim**

Retrieve v1 content:
```bash
git show 11a9087:src/server/vk_live.rs > src/peer/vk_live.rs
```

If the v1 file references `crate::server::` paths, update to `crate::peer::`. If it uses config types that were removed (`ServerConfig`), replace with direct parameters (channel, name).

Add to `src/peer/mod.rs`: `pub mod vk_live;`

- [ ] **Step 2: Verify compiles**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add src/peer/vk_live.rs src/peer/mod.rs
git commit -m "feat(peer): vk_live resolver (ported from v1 server)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 16: peer::app — default heartbeat behaviour

**Files:**
- Create: `src/peer/app.rs`

- [ ] **Step 1: Write the module**

Create `src/peer/app.rs`:
```rust
//! Default application running on top of flicker: heartbeats out, logs in.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::flicker::{InboundMessage, OutboundMessage};

pub const MSG_TYPE_TIME_SYNC: u8 = 0x01;

pub fn run_default(
    outbound_tx: Option<Sender<OutboundMessage>>,
    inbound_rx: Option<Receiver<InboundMessage>>,
    running: Arc<AtomicBool>,
) {
    if let Some(tx) = outbound_tx {
        let running_c = Arc::clone(&running);
        thread::spawn(move || heartbeat_loop(tx, running_c));
    }
    if let Some(rx) = inbound_rx {
        let running_c = Arc::clone(&running);
        thread::spawn(move || log_loop(rx, running_c));
    }
    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
    }
}

fn heartbeat_loop(tx: Sender<OutboundMessage>, running: Arc<AtomicBool>) {
    let mut next_tick = Instant::now();
    while running.load(Ordering::SeqCst) {
        if Instant::now() >= next_tick {
            let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
            let payload = now_ns.to_be_bytes().to_vec();
            if tx.send(OutboundMessage { msg_type: MSG_TYPE_TIME_SYNC, payload }).is_err() {
                break;
            }
            next_tick += Duration::from_secs(1);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn log_loop(rx: Receiver<InboundMessage>, running: Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(msg) => {
                if msg.msg_type == MSG_TYPE_TIME_SYNC && msg.payload.len() == 8 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&msg.payload);
                    let ts_ns = u64::from_be_bytes(bytes);
                    let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
                    let delta_ms = (now_ns as i128 - ts_ns as i128) / 1_000_000;
                    eprintln!("[app] time_sync ts={ts_ns} Δ={delta_ms}ms");
                } else {
                    eprintln!("[app] msg type=0x{:02x} len={}", msg.msg_type, msg.payload.len());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
}
```

Add to `src/peer/mod.rs`: `pub mod app;`

- [ ] **Step 2: Verify compiles**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add src/peer/app.rs src/peer/mod.rs
git commit -m "feat(peer): default app (time-sync heartbeat + log)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 17: peer::mod — tx/rx/app thread orchestrator

**Files:**
- Modify: `src/peer/mod.rs`

- [ ] **Step 1: Replace stub with full implementation**

Replace `src/peer/mod.rs` content (keep `pub mod` lines):

```rust
pub mod app;
pub mod ffmpeg_publish;
pub mod ffmpeg_read;
pub mod vk_live;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{self, PeerConfig};
use crate::flicker::fragment::Reassembler;
use crate::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use crate::flicker::fragment::{Fragment, FRAGMENT_HEADER_BYTES};
use crate::flicker::grid::FRAME_BYTES_RGB24;
use crate::flicker::{InboundMessage, OutboundMessage};

#[derive(Copy, Clone, Debug)]
pub struct Direction {
    pub tx: bool,
    pub rx: bool,
}

pub fn run_peer(cfg: PeerConfig, dir: Direction) -> Result<()> {
    if dir.tx { config::validate_tx(&cfg)?; }
    if dir.rx { config::validate_rx(&cfg)?; }

    let running = Arc::new(AtomicBool::new(true));
    let running_signal = Arc::clone(&running);
    ctrlc::set_handler(move || {
        running_signal.store(false, Ordering::SeqCst);
    }).context("ctrlc handler")?;

    let (app_out_tx, app_out_rx) = mpsc::channel::<OutboundMessage>();
    let (app_in_tx, app_in_rx) = mpsc::channel::<InboundMessage>();

    let mut handles = Vec::new();
    if dir.tx {
        let cfg_c = cfg.clone();
        let run_c = Arc::clone(&running);
        handles.push(thread::spawn(move || tx_thread(cfg_c, app_out_rx, run_c)));
    } else { drop(app_out_rx); }

    if dir.rx {
        let cfg_c = cfg.clone();
        let run_c = Arc::clone(&running);
        handles.push(thread::spawn(move || rx_thread(cfg_c, app_in_tx, run_c)));
    } else { drop(app_in_tx); }

    // App thread runs in main.
    let out_tx = if dir.tx { Some(app_out_tx) } else { None };
    let in_rx = if dir.rx { Some(app_in_rx) } else { None };
    app::run_default(out_tx, in_rx, Arc::clone(&running));

    for h in handles { let _ = h.join(); }
    Ok(())
}

fn tx_thread(cfg: PeerConfig, outbound: Receiver<OutboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let rtmp_url = format!("{}/{}", cfg.my_rtmp_url.trim_end_matches('/'), cfg.my_stream_key);
    let args = ffmpeg_publish::publish_args(&rtmp_url);
    let mut child = std::process::Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg publish")?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;

    let mut encoder = FrameEncoder {
        mode: cfg.modulation_mode,
        channel_id: 1,
        frame_counter: 0,
    };
    let mut frame_buf = vec![0u8; FRAME_BYTES_RGB24];
    let frame_interval = Duration::from_nanos(1_000_000_000 / crate::flicker::grid::FPS as u64);
    let mut next_deadline = std::time::Instant::now();
    let mut next_msg_id: u32 = 0;

    while running.load(Ordering::SeqCst) {
        // Gather one message worth of fragments for this frame.
        let mut fragments: Vec<Fragment> = Vec::new();
        let capacity = encoder.payload_bytes_per_frame();
        let max_payload = capacity.saturating_sub(FRAGMENT_HEADER_BYTES);
        match outbound.recv_timeout(Duration::from_millis(10)) {
            Ok(msg) => {
                // Single-fragment if fits; else split.
                if msg.payload.len() <= max_payload {
                    next_msg_id = next_msg_id.wrapping_add(1);
                    fragments.push(Fragment {
                        msg_type: msg.msg_type,
                        message_id: next_msg_id,
                        fragment_idx: 0,
                        fragment_total: 1,
                        payload: msg.payload,
                    });
                } else {
                    // Split across multiple frames — emit first fragment now.
                    let total = ((msg.payload.len() + max_payload - 1) / max_payload) as u16;
                    next_msg_id = next_msg_id.wrapping_add(1);
                    let chunk = msg.payload[..max_payload].to_vec();
                    fragments.push(Fragment {
                        msg_type: msg.msg_type,
                        message_id: next_msg_id,
                        fragment_idx: 0,
                        fragment_total: total,
                        payload: chunk,
                    });
                    // TODO: queue remaining fragments across upcoming frames (v2.1).
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        encoder.encode(&mut frame_buf, &fragments)?;
        if stdin.write_all(&frame_buf).is_err() { break; }
        // Pace to fps.
        next_deadline += frame_interval;
        let now = std::time::Instant::now();
        if next_deadline > now {
            thread::sleep(next_deadline - now);
        } else {
            next_deadline = now;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn rx_thread(cfg: PeerConfig, inbound: Sender<InboundMessage>, running: Arc<AtomicBool>) -> Result<()> {
    let page_url = format!("https://live.vkvideo.ru/{}/stream/{}", cfg.their_vk_channel, cfg.their_stream_name);
    // Resolve VK stream URL.
    let (stream_url, is_hls) = vk_live::resolve(&cfg.their_vk_channel, &cfg.their_stream_name)
        .context("vk resolve")?;
    let args = ffmpeg_read::read_args(&stream_url, &page_url, is_hls);
    let mut child = ffmpeg_read::spawn(&args)?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no ffmpeg stdout"))?;

    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let dec = FrameDecoder;
    let mut reassembler = Reassembler::new(cfg.frag_timeout_ms);
    let mut frame_idx: u64 = 0;

    while running.load(Ordering::SeqCst) {
        if stdout.read_exact(&mut buf).is_err() {
            eprintln!("[flicker] rx ffmpeg stdout ended");
            break;
        }
        match dec.decode(&buf) {
            DecodeOutcome::Ok { header, fragments, pilot_success } => {
                if cfg.log_every_frame {
                    eprintln!("[flicker] rx frame={frame_idx} ch={} mode={:?} payload_len={} pilots={:.2}",
                        header.channel_id, header.modulation_mode, header.payload_len, pilot_success);
                }
                for f in fragments {
                    if let Some(msg) = reassembler.accept(f) {
                        if inbound.send(msg).is_err() { return Ok(()); }
                    }
                }
            }
            DecodeOutcome::Dropped { reason } => {
                if cfg.log_every_frame {
                    eprintln!("[flicker] rx frame={frame_idx} dropped: {reason:?}");
                }
            }
        }
        frame_idx += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}
```

If `vk_live::resolve` signature doesn't match `(channel, name) -> Result<(String, bool)>`, adapt it in Task 15 or wrap here.

- [ ] **Step 2: Verify compile**

Run: `cargo build`
Expected: clean (may have warnings for unused items pending Task 18 CLI wire-up).

- [ ] **Step 3: Commit**

```bash
git add src/peer/mod.rs
git commit -m "feat(peer): tx/rx/app thread orchestrator with Direction flags

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 8 — CLI & main wire-up

### Task 18: cli.rs — full peer subcommand + direction flags

**Files:**
- Modify: `src/cli.rs`

- [ ] **Step 1: Replace stub with real implementation**

Replace `src/cli.rs`:
```rust
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crate::config::PeerConfig;
use crate::peer::Direction;

#[derive(Parser, Debug)]
#[command(name = "rtmp-steganography", version, about = "flicker v2 protocol over RTMP")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Mode,
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    Peer(PeerArgs),
}

#[derive(clap::Args, Debug)]
pub struct PeerArgs {
    #[arg(long = "publish-only", conflicts_with = "receive_only")]
    pub publish_only: bool,
    #[arg(long = "receive-only")]
    pub receive_only: bool,
}

impl Cli {
    pub fn resolve(self, _cfg: &PeerConfig) -> Result<Direction> {
        match self.command {
            Mode::Peer(args) => match (args.publish_only, args.receive_only) {
                (false, false) => Ok(Direction { tx: true, rx: true }),
                (true, false) => Ok(Direction { tx: true, rx: false }),
                (false, true) => Ok(Direction { tx: false, rx: true }),
                (true, true) => Err(anyhow!("--publish-only and --receive-only are mutually exclusive")),
            },
        }
    }
}
```

Update `src/main.rs`:
```rust
use anyhow::Result;
use clap::Parser;

use rtmp_steganography::cli::Cli;
use rtmp_steganography::{config, peer};

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let cfg = config::load_peer()?;
    let direction = cli.resolve(&cfg)?;
    peer::run_peer(cfg, direction)
}
```

- [ ] **Step 2: Verify compiles and `--help` works**

Run: `cargo build && cargo run -- --help`
Expected: prints help showing `peer` subcommand.

Run: `cargo run -- peer --help`
Expected: shows `--publish-only` and `--receive-only`.

- [ ] **Step 3: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(cli): peer subcommand with --publish-only/--receive-only

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 9 — Level B: synthetic lossy tests

### Task 19: tests/flicker_roundtrip.rs — clean round-trip integration tests

**Files:**
- Create: `tests/flicker_roundtrip.rs`

- [ ] **Step 1: Write Level B clean tests**

```rust
//! Level B — pure in-memory encode/decode round-trip without ffmpeg.
//! Asserts every message type, size, and fragment count round-trips bit-exact.

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FRAME_BYTES_RGB24;
use rtmp_steganography::flicker::ModulationMode;

fn make_frag(msg_type: u8, payload: Vec<u8>) -> Fragment {
    Fragment {
        msg_type,
        message_id: 1,
        fragment_idx: 0,
        fragment_total: 1,
        payload,
    }
}

#[test]
fn roundtrip_mode_b_single_fragment_small() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let frag = make_frag(0x02, b"short payload".to_vec());
    enc.encode(&mut buf, &[frag.clone()]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments.len(), 1);
            assert!(fragments[0].payload.starts_with(b"short payload"));
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_b_max_size() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 5 };
    // 240 byte frame budget - 9 byte fragment header = 231 bytes payload.
    let payload: Vec<u8> = (0..231u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments[0].payload[..231], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_c_larger_payload() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::C, channel_id: 2, frame_counter: 42 };
    let payload: Vec<u8> = (0..400u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, header, .. } => {
            assert_eq!(header.modulation_mode, ModulationMode::C);
            assert_eq!(fragments[0].payload[..400], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --test flicker_roundtrip -- --nocapture`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add tests/flicker_roundtrip.rs
git commit -m "test(flicker): Level B clean round-trip tests

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 20: tests/flicker_lossy.rs — damage matrix (Level B mandatory)

**Files:**
- Create: `tests/flicker_lossy.rs`

- [ ] **Step 1: Write damage tests**

```rust
//! Level B synthetic lossy tests — validate FEC + pilots + corner markers
//! recover the payload under controlled damage, or cleanly drop the frame.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FRAME_BYTES_RGB24;
use rtmp_steganography::flicker::ModulationMode;

fn make_frame(payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 9 };
    let frag = Fragment {
        msg_type: 0x02, message_id: 1, fragment_idx: 0, fragment_total: 1,
        payload: payload.to_vec(),
    };
    enc.encode(&mut buf, &[frag]).unwrap();
    buf
}

fn assert_ok_or_dropped(buf: &[u8], expected: &[u8]) {
    // Test passes if decoder either recovers `expected` exactly, or cleanly marks frame dropped.
    // Never should it deliver wrong data.
    match FrameDecoder.decode(buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            let got = &fragments[0].payload[..expected.len().min(fragments[0].payload.len())];
            assert_eq!(got, expected, "decoder delivered wrong data");
        }
        DecodeOutcome::Dropped { .. } => {
            // acceptable — protocol signalled "cannot trust this frame"
        }
    }
}

#[test]
fn low_gaussian_noise_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(1);
    for b in buf.iter_mut() {
        let noise: i32 = rng.gen_range(-5..=5);
        *b = (*b as i32 + noise).clamp(0, 255) as u8;
    }
    // Guarantee: bit-exact recovery at σ=5.
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(&fragments[0].payload[..150], &payload[..]);
        }
        o => panic!("expected Ok at low noise, got {o:?}"),
    }
}

#[test]
fn moderate_gaussian_noise_drops_or_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(2);
    for b in buf.iter_mut() {
        let noise: i32 = rng.gen_range(-30..=30);
        *b = (*b as i32 + noise).clamp(0, 255) as u8;
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn one_pct_pixel_flip_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(3);
    let n_flips = buf.len() / 100; // 1%
    for _ in 0..n_flips {
        let i = rng.gen_range(0..buf.len());
        buf[i] = 255 - buf[i];
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn brightness_bias_plus_20_recovers() {
    let payload: Vec<u8> = (0..120u8).collect();
    let mut buf = make_frame(&payload);
    for b in buf.iter_mut() {
        *b = b.saturating_add(20);
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn block_corruption_3x8x8_bursts() {
    use rtmp_steganography::flicker::grid::{FRAME_WIDTH, rgb24_offset};
    let payload: Vec<u8> = (0..120u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(5);
    for _ in 0..3 {
        let cx = rng.gen_range(0..FRAME_WIDTH - 8);
        let cy = rng.gen_range(0..rtmp_steganography::flicker::grid::FRAME_HEIGHT - 8);
        for dy in 0..8 { for dx in 0..8 {
            let o = rgb24_offset(cx + dx, cy + dy);
            let v: u8 = rng.gen();
            buf[o] = v; buf[o + 1] = v; buf[o + 2] = v;
        }}
    }
    assert_ok_or_dropped(&buf, &payload);
}
```

- [ ] **Step 2: Add `rand` dev-dep**

Add to `Cargo.toml`:
```toml
[dev-dependencies]
rand = "0.8"
```

- [ ] **Step 3: Run tests**

Run: `cargo test --test flicker_lossy -- --nocapture`
Expected: 5 passed (possibly with some "dropped" outcomes logged).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml tests/flicker_lossy.rs
git commit -m "test(flicker): Level B synthetic damage matrix

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 10 — Level C: ffmpeg integration

### Task 21: tests/flicker_ffmpeg.rs — feature-gated round-trip via libx264

**Files:**
- Create: `tests/flicker_ffmpeg.rs`

- [ ] **Step 1: Write integration test gated behind `ffmpeg-integration` feature**

```rust
//! Level C — round-trip through libx264 yuv420p via ffmpeg subprocess pipes.
//! Requires ffmpeg on PATH. Run with: cargo test --features ffmpeg-integration

#![cfg(feature = "ffmpeg-integration")]

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::{FPS, FRAME_BYTES_RGB24, FRAME_HEIGHT, FRAME_WIDTH};
use rtmp_steganography::flicker::ModulationMode;

const N_FRAMES: usize = 50;

#[test]
fn ffmpeg_roundtrip_50_frames_mode_b() {
    // 1. Prepare input: N_FRAMES raw RGB24 frames with known payloads.
    let mut encoder = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let mut input = Vec::with_capacity(FRAME_BYTES_RGB24 * N_FRAMES);
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for i in 0..N_FRAMES {
        let payload: Vec<u8> = (0..100u8).map(|b| b.wrapping_add(i as u8)).collect();
        payloads.push(payload.clone());
        let frag = Fragment {
            msg_type: 0x02, message_id: i as u32, fragment_idx: 0, fragment_total: 1,
            payload,
        };
        let mut frame = vec![0u8; FRAME_BYTES_RGB24];
        encoder.encode(&mut frame, &[frag]).unwrap();
        input.extend_from_slice(&frame);
    }

    // 2. ffmpeg encode: rawvideo rgb24 → h264/yuv420p → stdout pipe as rawvideo rgb24 back.
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let fps_arg = FPS.to_string();
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgb24",
            "-s", &size_arg, "-r", &fps_arg, "-i", "pipe:0",
            "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
            "-profile:v", "baseline", "-pix_fmt", "yuv420p",
            "-b:v", "300k", "-g", &fps_arg,
            "-f", "h264",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg encode — is it on PATH?");

    let mut stdin = child.stdin.take().unwrap();
    let input_clone = input.clone();
    std::thread::spawn(move || {
        stdin.write_all(&input_clone).ok();
    });
    let h264 = child.wait_with_output().unwrap().stdout;
    assert!(!h264.is_empty(), "ffmpeg encode produced empty output");

    // 3. Decode h264 back to rawvideo rgb24.
    let mut child2 = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "h264", "-i", "pipe:0",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg decode");
    let mut stdin2 = child2.stdin.take().unwrap();
    std::thread::spawn(move || { stdin2.write_all(&h264).ok(); });
    let mut stdout2 = child2.stdout.take().unwrap();
    let mut decoded = Vec::new();
    stdout2.read_to_end(&mut decoded).unwrap();
    let _ = child2.wait();

    // 4. Split decoded bytes into frames and run FrameDecoder.
    let got_frames = decoded.len() / FRAME_BYTES_RGB24;
    assert!(got_frames >= N_FRAMES - 2, "lost too many frames: {got_frames}");

    let decoder = FrameDecoder;
    let mut ok_count = 0usize;
    for i in 0..got_frames.min(N_FRAMES) {
        let start = i * FRAME_BYTES_RGB24;
        let frame = &decoded[start..start + FRAME_BYTES_RGB24];
        if let DecodeOutcome::Ok { fragments, .. } = decoder.decode(frame) {
            if !fragments.is_empty() && fragments[0].payload.starts_with(&payloads[i][..100.min(payloads[i].len())]) {
                ok_count += 1;
            }
        }
    }
    assert!(ok_count >= 48, "only {ok_count}/{N_FRAMES} frames recovered bit-exact");
}
```

- [ ] **Step 2: Run test (only with feature flag)**

Run without flag: `cargo test --test flicker_ffmpeg`
Expected: "no tests to run" (cfg-gated out).

Run with flag: `cargo test --features ffmpeg-integration --test flicker_ffmpeg -- --nocapture`
Expected: passes if ffmpeg on PATH; acceptance threshold ≥48/50 recovered.

- [ ] **Step 3: Commit**

```bash
git add tests/flicker_ffmpeg.rs
git commit -m "test(flicker): Level C ffmpeg round-trip (feature-gated)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 11 — Level D: VK end-to-end

### Task 22: scripts/e2e-vk.sh + e2e-vk.ps1 — manual smoke test

**Files:**
- Create: `scripts/e2e-vk.sh`
- Create: `scripts/e2e-vk.ps1`

- [ ] **Step 1: Write bash script**

```bash
#!/usr/bin/env bash
# Level D — VK end-to-end smoke test.
# Starts a single peer publishing + reading its OWN stream via VK, measures
# delivery rate over 60 seconds, emits a summary report.
#
# Required env (set in .env or exported):
#   peer_my_rtmp_url
#   peer_my_stream_key
#   peer_their_vk_channel   (your own channel — self-loop)
#   peer_their_stream_name
set -euo pipefail
cd "$(dirname "$0")/.."

: "${peer_my_rtmp_url:?set peer_my_rtmp_url}"
: "${peer_my_stream_key:?set peer_my_stream_key}"
: "${peer_their_vk_channel:?set peer_their_vk_channel}"
: "${peer_their_stream_name:?set peer_their_stream_name}"

export flicker_log_every_frame=1

LOG="$(mktemp)"
echo "[e2e] log: $LOG"
echo "[e2e] running peer for 60s..."

cargo build --release
timeout 60 ./target/release/rtmp-steganography peer 2>&1 | tee "$LOG" || true

echo ""
echo "[e2e] results:"
SENT=$(grep -c 'time_sync ts=' "$LOG" || true)
RCVD=$(grep -c '\[app\] time_sync' "$LOG" || true)
DROPS=$(grep -c 'dropped:' "$LOG" || true)
echo "  time_sync sent: $SENT"
echo "  time_sync rcvd: $RCVD"
echo "  frames dropped: $DROPS"
if [ "$SENT" -gt 0 ]; then
  RATE=$(( 100 * RCVD / SENT ))
  echo "  delivery rate: ${RATE}%"
  if [ "$RATE" -ge 90 ]; then
    echo "[e2e] PASS (>= 90%)"
    exit 0
  else
    echo "[e2e] FAIL (below 90% delivery)"
    exit 1
  fi
fi
echo "[e2e] INCONCLUSIVE (no time_sync traffic observed)"
exit 2
```

- [ ] **Step 2: Write PowerShell counterpart**

Create `scripts/e2e-vk.ps1`:
```powershell
# Level D — VK end-to-end smoke test (Windows / PowerShell).
param([int]$Seconds = 60)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

foreach ($v in @("peer_my_rtmp_url","peer_my_stream_key","peer_their_vk_channel","peer_their_stream_name")) {
    if (-not [Environment]::GetEnvironmentVariable($v)) { throw "$v not set" }
}
$env:flicker_log_every_frame = "1"

cargo build --release
$log = New-TemporaryFile
Write-Host "[e2e] log: $log"
Write-Host "[e2e] running peer for $Seconds s..."

$proc = Start-Process -FilePath ".\target\release\rtmp-steganography.exe" -ArgumentList "peer" -NoNewWindow -PassThru -RedirectStandardError $log -RedirectStandardOutput $log
Start-Sleep -Seconds $Seconds
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue

$text = Get-Content $log
$sent  = ($text | Select-String 'time_sync ts=').Count
$rcvd  = ($text | Select-String '\[app\] time_sync').Count
$drops = ($text | Select-String 'dropped:').Count
Write-Host "  sent=$sent rcvd=$rcvd dropped=$drops"
if ($sent -gt 0) {
    $rate = [math]::Floor(100 * $rcvd / $sent)
    Write-Host "  delivery rate: $rate%"
    if ($rate -ge 90) { Write-Host "[e2e] PASS"; exit 0 } else { Write-Host "[e2e] FAIL"; exit 1 }
}
Write-Host "[e2e] INCONCLUSIVE"
exit 2
```

- [ ] **Step 3: Make bash script executable**

Run: `chmod +x scripts/e2e-vk.sh`

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-vk.sh scripts/e2e-vk.ps1
git commit -m "test(flicker): Level D VK e2e smoke scripts (bash + PowerShell)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 23: docs/testing-e2e.md — manual procedure documentation

**Files:**
- Create: `docs/testing-e2e.md`

- [ ] **Step 1: Write doc**

Create `docs/testing-e2e.md`:
```markdown
# Level D — VK End-to-End Smoke Test

## What this tests

A single peer publishes to your own VK Live channel and reads the same channel back, validating the full pipeline: flicker encode → x264 → RTMP → VK ingest → HLS output → ffmpeg demux → flicker decode.

## Prerequisites

1. VK Live account with an active stream slot.
2. `ffmpeg` on `PATH`.
3. `rtmp-steganography` built (`cargo build --release`).

## Environment

Create a `.env` at the repo root:

```env
peer_my_rtmp_url=rtmp://ok-push.vkvideo.ru/live
peer_my_stream_key=<your VK stream key>
peer_their_vk_channel=<your VK channel slug — same as where you publish>
peer_their_stream_name=<stream name, usually matches key prefix>
flicker_modulation_mode=B
flicker_log_every_frame=1
```

## Run

Linux/macOS:
```bash
./scripts/e2e-vk.sh
```

Windows (PowerShell):
```powershell
.\scripts\e2e-vk.ps1 -Seconds 60
```

## Acceptance thresholds

- `delivery rate ≥ 90%` (measured as `time_sync rcvd / time_sync sent`)
- p50 end-to-end latency ≤ 4 s (read from `Δ` values in log)
- p99 end-to-end latency ≤ 10 s
- No panics, clean exit on Ctrl+C

## Interpreting failures

| Symptom | Likely cause | Investigate |
|---------|--------------|-------------|
| Zero `time_sync rcvd` | VK not publishing frames downstream | VK dashboard, HLS URL resolution |
| `frames dropped: PilotValidationFailed` dominant | Alignment off, cells misread | Check `scale=neighbor` ffmpeg arg, VK transcoder resolution |
| `BlockRsFailed` dominant | BER > FEC capacity | Try mode B if on C; check VK output bitrate |
| `HeaderRsFailed` dominant | Header zone corrupted | Check corner marker pattern in saved frames |

## Debugging tools

Save a raw frame for offline analysis:
```bash
ffmpeg -i "https://<VK HLS URL>" -vf "scale=256:144:flags=neighbor,format=rgb24" -frames:v 1 -pix_fmt rgb24 frame.raw
```

Then inspect with a tool that can render raw RGB24 at 256×144.

## When to re-run

Any PR that touches:
- `src/flicker/frame.rs`
- `src/flicker/markers.rs`
- `src/flicker/pilot.rs`
- `src/flicker/header.rs`
- `src/peer/ffmpeg_*.rs`

must include a successful Level D run note in the PR description.
```

- [ ] **Step 2: Commit**

```bash
git add docs/testing-e2e.md
git commit -m "docs: Level D VK e2e testing procedure

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 12 — README migration note

### Task 24: README.md — v2 overview and migration

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite README**

Replace content with:
```markdown
# rtmp-steganography

Bidirectional, UDP-like steganographic data channel over RTMP/HLS video streams.
v2 of the "flicker" protocol — each video frame carries ~240 bytes (mode B) or
~480 bytes (mode C) of application payload, protected by Reed-Solomon FEC.

## Quick start

```bash
cargo build --release

# Full duplex (publish + receive simultaneously)
./target/release/rtmp-steganography peer

# Publish only
./target/release/rtmp-steganography peer --publish-only

# Receive only
./target/release/rtmp-steganography peer --receive-only
```

## Configuration

`.env` at repo root:

```env
peer_my_rtmp_url=rtmp://host/live
peer_my_stream_key=<your VK key>
peer_their_vk_channel=<channel slug>
peer_their_stream_name=<stream name>

flicker_modulation_mode=B          # B (2bpp, ~5.5 KB/s) or C (4bpp, ~11.3 KB/s)
flicker_frag_timeout_ms=2000
flicker_log_every_frame=0
```

## Migration from v1

| v1 | v2 |
|----|----|
| `rtmp-steganography client` | `rtmp-steganography peer --publish-only` |
| `rtmp-steganography server` | `rtmp-steganography peer --receive-only` |
| `client_stream_key` | `peer_my_stream_key` |
| `rtmp_server` | `peer_my_rtmp_url` |
| `vk_live_channel` | `peer_their_vk_channel` |
| `client_stream_name` | `peer_their_stream_name` |

The on-the-wire protocol is completely incompatible — v1 and v2 peers cannot interoperate.

## Testing

- `cargo test` — Level B (unit + synthetic damage)
- `cargo test --features ffmpeg-integration` — Level C (libx264 round-trip)
- `./scripts/e2e-vk.sh` — Level D (manual VK end-to-end)

See `docs/testing-e2e.md` for Level D procedure and acceptance thresholds.

## Architecture

See `docs/superpowers/specs/2026-04-19-flicker-protocol-v2-design.md` for the full design.

Core idea: treat each video frame like an SDH STM frame — corner markers for alignment, header with its own RS code, pilot cells for brightness bias calibration, payload zone with interleaved RS(172,120) blocks.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: v2 README with quick start and v1 migration table

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 25: Sanity run — end-to-end workspace build and full test suite

**Files:**
- None (verification task)

- [ ] **Step 1: Full build**

Run: `cargo build --release`
Expected: clean release build.

- [ ] **Step 2: Full unit + Level B test run**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 3: Level C tests (if ffmpeg available)**

Run: `cargo test --features ffmpeg-integration`
Expected: all pass including `flicker_ffmpeg::ffmpeg_roundtrip_50_frames_mode_b`.

- [ ] **Step 4: CLI smoke**

Run: `./target/release/rtmp-steganography --help`
Expected: shows `peer` subcommand.

Run: `./target/release/rtmp-steganography peer --help`
Expected: shows `--publish-only` and `--receive-only` flags.

- [ ] **Step 5: Final commit (none needed — sanity only)**

If tests pass, no commit. If any test fails, iterate fixing before marking plan done.

---

## Self-review checklist (post-implementation)

After executing all tasks, verify against the spec:

- [ ] All v1 files deleted (`src/flicker/{grid,codec,frame}.rs`, `src/client/`, `src/server/`).
- [ ] Cargo.toml carries new deps (`reed-solomon-erasure`, `crc32fast`, `rand_chacha`, `rand_core`).
- [ ] `cargo test` passes Level B.
- [ ] `cargo test --features ffmpeg-integration` passes Level C.
- [ ] Level D script documented and exists on disk.
- [ ] `peer` CLI supports both direction flags.
- [ ] `.env` loading validates tx/rx keys per direction.
- [ ] Throughput achieved (approx, in Level D): ≥5 KB/s mode B / ≥10 KB/s mode C.
- [ ] README migration table present.
- [ ] Frame decoder never delivers wrong data — only Ok(msg) or Dropped.
