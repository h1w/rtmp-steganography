# Stage 1: Pilot Calibration + Multi-Level Coding for Mode C

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Mode C work at cell_size=4 on 240p publish (through VK CMAF passthrough) by (a) measuring actual Y/U/V level positions per-frame from pilot cells instead of using fixed thresholds, and (b) splitting the 4-bit Mode C payload into two independent Y-stream and UV-stream RS codewords so chroma errors can't corrupt luma data.

**Architecture:** Decoder executes in three passes per frame: (1) header RS recovery using static Mode B reads, (2) pilot calibration — paint pilots using all 16 Mode C symbols, read raw YUV means per pilot, compute per-frame calibrated level means via median, (3) payload decode using calibrated thresholds, with Y-stream and UV-stream encoded/decoded as independent RS(172,120) blocks so chroma-lane errors don't poison luma-lane bytes.

**Tech Stack:** Rust, existing `reed-solomon-erasure` crate, `rand_chacha` PRNG, `crc32fast`. No new dependencies.

---

## Problem Context

Live run 2026-04-20 with `peer_stream_width=432 peer_stream_height=240 peer_flicker_cell_size=4 flicker_modulation_mode=C peer_x264_qp=22 peer_vk_prefer=cmaf` produced:

- `ffprobe native stream: width=432 height=240` — grid aligns with VK output (CMAF passthrough, no upscale)
- `[peer/tunnel] flicker=432x240@24 mode=C cell_size=4px grid=108x60 total_cells=6480 block_count=17`
- peer-a rx=5645 drop=5524 HdrRs=29 BlockRs=5495 CRC=0
- Drop rate **97.9%**, dominated by `BlockRsFailed`

Root cause: yuv420 chroma plane is 216×120 at 240p. With cell_size=4 luma, each cell's chroma area is 2×2 = 4 chroma samples. VK DCT quantization drifts U/V by ±20-30 LSB per macroblock. Current `LEVELS_U = [80, 176]` with fixed threshold at 128 catches bit-flips when drift pushes U=80→U=130. Mode C packs 2 Y-bits + 1 U-bit + 1 V-bit per cell into a shared 4-bit symbol, so a single bit-flip in U/V corrupts the byte that also carries Y-data, and the shared RS(172,120) codeword can't separate the errors.

Docs reference: `docs/vk-transcoder-analysis.md` §1.4, §3.1, §3.5, Stage 1.

## Success Criteria

After Phase 3 validation on live VK with `peer_stream_width=432 peer_stream_height=240 peer_flicker_cell_size=4 flicker_modulation_mode=C peer_x264_qp=22 peer_vk_prefer=cmaf`:

- `bench smoke --throughput-bytes 1024 --skip-iperf` returns `ok=true` within 30 seconds
- `oneway_kbits_per_s >= 0.3` (matches earlier 360p cell=8 Mode C baseline)
- `peer-a` BlockRsFailed/rx ratio ≤ 15% (was 97%)
- `peer-a` PayloadCrcMismatch/rx ratio ≤ 2%

## File Structure

**Create:**
- `src/flicker/calibration.rs` — pilot-based adaptive Y/U/V level extraction
- `src/flicker/channels.rs` — split Mode C payload into Y-stream and UV-stream byte arrays
- `docs/superpowers/plans/2026-04-20-stage1-pilot-cal-and-multi-level-mode-c.md` — this document

**Modify:**
- `src/flicker/mod.rs` — `pub mod calibration; pub mod channels;`
- `src/flicker/levels.rs` — add `CalibratedLevels` struct, `quantise_*_cal` variants accepting calibrated thresholds
- `src/flicker/codec.rs` — `read_cell_c_cal` variant accepting `Option<&CalibratedLevels>`; `paint_cell_c` usable for pilots (no change needed)
- `src/flicker/pilot.rs` — `pilot_value` returns full 4-bit symbols (0..16) stratified across palette; painter uses `paint_cell_c`; reader returns raw YUV means
- `src/flicker/frame.rs` — encode splits payload into Y-stream + UV-stream RS codewords; decode runs pilot calibration between header decode and payload decode; uses calibrated levels and split streams
- `src/flicker/header.rs` — no schema change (reuse `fec_params[3]` flags byte — bit 0 = multi-level enabled)

All changes are bit-exact between peers (both deploy together — no wire-compat constraints).

---

# Phase 1: Pilot Calibration

Goal: bring 240p cell=4 Mode C from 97% BlockRsFailed down to ≤30% by measuring actual per-frame U/V midpoints from pilots.

---

### Task 1: Stratified Mode C pilot symbol generator

**Files:**
- Modify: `src/flicker/pilot.rs`

- [ ] **Step 1: Write failing test for 4-bit stratified pilot values**

Add to `src/flicker/pilot.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn pilot_value_c_covers_all_16_symbols_evenly() {
    // For any frame_counter, iterating i in 0..PILOT_COUNT must produce
    // every 4-bit symbol at least floor(PILOT_COUNT / 16) times so the
    // calibrator has enough samples per level.
    let mut counts = [0usize; 16];
    for i in 0..PILOT_COUNT {
        let sym = pilot_value_c(42, i);
        assert!(sym < 16, "pilot_value_c must return symbol < 16, got {sym}");
        counts[sym as usize] += 1;
    }
    let min_expected = PILOT_COUNT / 16;
    for (s, c) in counts.iter().enumerate() {
        assert!(*c >= min_expected,
            "symbol {s} has only {c} pilots (need >= {min_expected})");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography pilot_value_c_covers_all_16 -- --nocapture`
Expected: FAIL with `cannot find function 'pilot_value_c'`.

- [ ] **Step 3: Implement `pilot_value_c`**

Add to `src/flicker/pilot.rs` (above the `#[cfg(test)]` module):

```rust
/// Mode C pilot symbol — 4 bits (0..16). Stratified so every symbol gets
/// at least `PILOT_COUNT / 16` occurrences. Assignment is deterministic:
/// pilot index `i` → symbol `((i * 16) / PILOT_COUNT) % 16` rotated by a
/// PRNG-derived per-frame offset (keeps decoder in sync).
pub fn pilot_value_c(frame_counter: u32, index_in_pilot_list: usize) -> u8 {
    let mut rng = ChaCha8Rng::from_seed(seed_for(frame_counter ^ 0xC0DE_BEEF));
    let offset = (rng.next_u32() & 0x0F) as usize;
    let base = (index_in_pilot_list * 16) / PILOT_COUNT;
    ((base + offset) % 16) as u8
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib -p rtmp-steganography pilot_value_c_covers_all_16 -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/flicker/pilot.rs
git commit -m "feat(flicker): stratified Mode C pilot symbol generator"
```

---

### Task 2: Per-pilot raw YUV reader

**Files:**
- Modify: `src/flicker/codec.rs`
- Modify: `src/flicker/pilot.rs`

- [ ] **Step 1: Write failing test for raw YUV cell reader**

Add to `src/flicker/codec.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn read_cell_c_raw_returns_yuv_means_near_palette() {
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    for sym in 0u8..16 {
        paint_cell_c(&mut buf, 10, 5, sym, p.w(), p.cs());
        let (y, u, v) = read_cell_c_raw(&buf, 10, 5, p.w(), p.cs(), p.read_offset(), p.read_size());
        let y_expect = crate::flicker::levels::LEVELS_Y[(sym as usize >> 2) & 0b11];
        let u_expect = crate::flicker::levels::LEVELS_U[(sym as usize >> 1) & 0b1];
        let v_expect = crate::flicker::levels::LEVELS_V[sym as usize & 0b1];
        assert!((y as i32 - y_expect as i32).abs() <= 3,
            "sym {sym} Y: got {y}, expect ~{y_expect}");
        assert!((u as i32 - u_expect as i32).abs() <= 3,
            "sym {sym} U: got {u}, expect ~{u_expect}");
        assert!((v as i32 - v_expect as i32).abs() <= 3,
            "sym {sym} V: got {v}, expect ~{v_expect}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography read_cell_c_raw_returns_yuv -- --nocapture`
Expected: FAIL with `cannot find function 'read_cell_c_raw'`.

- [ ] **Step 3: Implement `read_cell_c_raw`**

Add to `src/flicker/codec.rs` after `read_cell_c`:

```rust
/// Raw per-cell YUV means without quantisation. Used by pilot calibration
/// to measure actual level positions after VK transcode drift.
pub fn read_cell_c_raw(
    buf: &[u8], col: usize, row: usize,
    width: usize, cell_size: usize, read_offset: usize, read_size: usize,
) -> (u8, u8, u8) {
    let (x0, y0) = cell_topleft(col, row, cell_size);
    let rx0 = x0 + read_offset;
    let ry0 = y0 + read_offset;
    let mut r_sum = 0u32;
    let mut g_sum = 0u32;
    let mut b_sum = 0u32;
    let mut count = 0u32;
    for py in ry0..ry0 + read_size {
        for px in rx0..rx0 + read_size {
            let o = rgb24_offset(px, py, width);
            r_sum += buf[o] as u32;
            g_sum += buf[o + 1] as u32;
            b_sum += buf[o + 2] as u32;
            count += 1;
        }
    }
    let r_mean = (r_sum / count.max(1)) as u8;
    let g_mean = (g_sum / count.max(1)) as u8;
    let b_mean = (b_sum / count.max(1)) as u8;
    rgb_to_yuv(r_mean, g_mean, b_mean)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib -p rtmp-steganography read_cell_c_raw_returns_yuv -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/flicker/codec.rs
git commit -m "feat(flicker): raw per-cell YUV reader for pilot calibration"
```

---

### Task 3: `CalibratedLevels` struct + median calibrator

**Files:**
- Create: `src/flicker/calibration.rs`
- Modify: `src/flicker/mod.rs`

- [ ] **Step 1: Write calibration.rs skeleton + failing test**

Create `src/flicker/calibration.rs`:

```rust
//! Per-frame adaptive Y/U/V level calibration from pilot observations.
//!
//! Each pilot cell carries a KNOWN Mode C symbol (painted by the encoder
//! from `pilot_value_c`). The decoder reads raw YUV means from those cells
//! after VK transcode, groups them by expected y_sym/u_sym/v_sym, and takes
//! the median to produce actual level positions for this specific frame.
//!
//! Motivation: VK DCT quantization drifts chroma by ±20-30 LSB per
//! macroblock. Fixed thresholds (midpoint of static `LEVELS_U = [80, 176]`)
//! then catch bit-flips on every drifted cell. Recomputing thresholds from
//! pilots that endured the same drift as payload cells recovers symbol
//! accuracy.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibratedLevels {
    pub y: [u8; 4],
    pub u: [u8; 2],
    pub v: [u8; 2],
}

impl CalibratedLevels {
    /// Fallback used when fewer than `MIN_OBSERVATIONS` samples exist for a
    /// given level — decoder reverts to static `LEVELS_*` from levels.rs.
    pub fn fallback() -> Self {
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        Self { y: LEVELS_Y, u: LEVELS_U, v: LEVELS_V }
    }
}

/// Need at least this many pilot observations per level slot before we
/// trust the calibrated value. Below this, that slot falls back to static.
pub const MIN_OBSERVATIONS: usize = 3;

/// Accumulate raw YUV samples grouped by expected symbol, then compute
/// per-level medians. `samples[sym]` = Vec of (y, u, v) means observed
/// for pilots whose expected Mode C symbol was `sym`.
pub fn calibrate(samples: &[Vec<(u8, u8, u8)>; 16]) -> CalibratedLevels {
    let fallback = CalibratedLevels::fallback();
    let mut out = fallback;

    for y_sym in 0..4 {
        let mut ys: Vec<u8> = (0..16)
            .filter(|s| (s >> 2) & 0b11 == y_sym)
            .flat_map(|s| samples[s].iter().map(|&(y, _, _)| y))
            .collect();
        if ys.len() >= MIN_OBSERVATIONS {
            ys.sort_unstable();
            out.y[y_sym] = ys[ys.len() / 2];
        }
    }

    for u_sym in 0..2 {
        let mut us: Vec<u8> = (0..16)
            .filter(|s| (s >> 1) & 0b1 == u_sym)
            .flat_map(|s| samples[s].iter().map(|&(_, u, _)| u))
            .collect();
        if us.len() >= MIN_OBSERVATIONS {
            us.sort_unstable();
            out.u[u_sym] = us[us.len() / 2];
        }
    }

    for v_sym in 0..2 {
        let mut vs: Vec<u8> = (0..16)
            .filter(|s| s & 0b1 == v_sym)
            .flat_map(|s| samples[s].iter().map(|&(_, _, v)| v))
            .collect();
        if vs.len() >= MIN_OBSERVATIONS {
            vs.sort_unstable();
            out.v[v_sym] = vs[vs.len() / 2];
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrate_returns_fallback_when_no_samples() {
        let empty: [Vec<(u8, u8, u8)>; 16] = Default::default();
        let cal = calibrate(&empty);
        assert_eq!(cal, CalibratedLevels::fallback());
    }

    #[test]
    fn calibrate_tracks_uniform_chroma_drift() {
        // Simulate: every cell's U was shifted +30 LSB by VK. Expected
        // calibration: U[0] and U[1] both shift by ~30.
        let mut samples: [Vec<(u8, u8, u8)>; 16] = Default::default();
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        for sym in 0..16u8 {
            let y_sym = (sym >> 2) & 0b11;
            let u_sym = (sym >> 1) & 0b1;
            let v_sym = sym & 0b1;
            let y = LEVELS_Y[y_sym as usize];
            let u = LEVELS_U[u_sym as usize].saturating_add(30);
            let v = LEVELS_V[v_sym as usize];
            for _ in 0..7 { samples[sym as usize].push((y, u, v)); }
        }
        let cal = calibrate(&samples);
        assert!((cal.u[0] as i32 - (LEVELS_U[0] as i32 + 30)).abs() <= 2,
            "calibrated U[0] should track +30 drift, got {}", cal.u[0]);
        assert!((cal.u[1] as i32 - (LEVELS_U[1] as i32 + 30)).abs() <= 2,
            "calibrated U[1] should track +30 drift, got {}", cal.u[1]);
        assert_eq!(cal.y, LEVELS_Y, "Y undrifted stays at static levels");
        assert_eq!(cal.v, LEVELS_V, "V undrifted stays at static levels");
    }

    #[test]
    fn calibrate_falls_back_when_only_one_symbol_sparse() {
        // symbol 0 has 2 samples (below MIN_OBSERVATIONS when grouped by y_sym=0)
        // — so Y[0] should fall back. But Y[1..3] with many samples should
        // not be touched (remain static since no drift injected).
        let mut samples: [Vec<(u8, u8, u8)>; 16] = Default::default();
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        samples[0] = vec![(LEVELS_Y[0], LEVELS_U[0], LEVELS_V[0]); 2];
        for sym in 4..16u8 {
            let y_sym = (sym >> 2) & 0b11;
            let u_sym = (sym >> 1) & 0b1;
            let v_sym = sym & 0b1;
            for _ in 0..7 {
                samples[sym as usize].push((
                    LEVELS_Y[y_sym as usize],
                    LEVELS_U[u_sym as usize],
                    LEVELS_V[v_sym as usize],
                ));
            }
        }
        let cal = calibrate(&samples);
        assert_eq!(cal.y[0], LEVELS_Y[0], "sparse Y[0] falls back");
    }
}
```

- [ ] **Step 2: Wire module in `src/flicker/mod.rs`**

Edit `src/flicker/mod.rs` — add `pub mod calibration;` between `pub mod pilot;` and the remaining content. Result:

```rust
pub mod codec;
pub mod fec;
pub mod fragment;
pub mod frame;
pub mod grid;
pub mod header;
pub mod interleave;
pub mod levels;
pub mod markers;
pub mod pilot;
pub mod calibration;
```

- [ ] **Step 3: Run the three tests**

Run: `cargo test --lib -p rtmp-steganography calibration:: -- --nocapture`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add src/flicker/calibration.rs src/flicker/mod.rs
git commit -m "feat(flicker): per-frame calibrated YUV levels from pilot medians"
```

---

### Task 4: Calibrated quantise + calibrated cell reader

**Files:**
- Modify: `src/flicker/levels.rs`
- Modify: `src/flicker/codec.rs`

- [ ] **Step 1: Write failing test for calibrated quantise**

Add to `src/flicker/levels.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn quantise_with_levels_uses_custom_thresholds() {
    // Simulated drifted U palette: [110, 200] instead of static [80, 176].
    // Static quantise would threshold at 128 — 110 falls on 80 side.
    // quantise_with_levels should threshold at (110+200)/2 = 155 → 110 → idx 0.
    let drifted_u = [110u8, 200u8];
    let (s, _) = quantise_with_levels(110, &drifted_u);
    assert_eq!(s, 0);
    let (s2, _) = quantise_with_levels(200, &drifted_u);
    assert_eq!(s2, 1);
    // Midway sample 155 is ambiguous — low confidence.
    let (_, c) = quantise_with_levels(155, &drifted_u);
    assert!(c < 0.1, "midway between drifted levels should have low conf, got {c}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography quantise_with_levels_uses_custom -- --nocapture`
Expected: FAIL with `cannot find function 'quantise_with_levels'`.

- [ ] **Step 3: Implement `quantise_with_levels`**

Edit `src/flicker/levels.rs`. Expose the existing private `quantise` as a public `quantise_with_levels` by adding this pub wrapper **above** the private one (and keep the private one unchanged, both coexist for now):

```rust
/// Public variant: quantise against caller-supplied levels (e.g. calibrated).
pub fn quantise_with_levels(sample: u8, levels: &[u8]) -> (u8, f32) {
    quantise(sample, levels)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib -p rtmp-steganography quantise_with_levels_uses_custom -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Write failing test for calibrated `read_cell_c_cal`**

Add to `src/flicker/codec.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn read_cell_c_cal_tolerates_chroma_drift_with_calibrated_levels() {
    use crate::flicker::calibration::CalibratedLevels;
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    // Paint symbol 5 = Y1 U0 V1 with DRIFTED chroma levels — simulate VK.
    // Directly set pixels to YUV that maps from drifted (Y=96, U=110, V=200).
    // We reuse paint_cell_c by first telling it: palette is [96..96..], etc.
    // Easier: paint with static levels, then inject drift in buffer pixel.
    paint_cell_c(&mut buf, 10, 5, 5, p.w(), p.cs());
    // Inject +30 LSB drift on U by patching every pixel's blue channel.
    // Do this ACROSS the whole frame (chunk around the read zone).
    for py in 0..p.h() {
        for px in 0..p.w() {
            let o = crate::flicker::grid::rgb24_offset(px, py, p.w());
            buf[o + 2] = buf[o + 2].saturating_add(40); // shift blue → shifts U
        }
    }
    // Static-level read: U threshold moved (drift beats it); symbol likely wrong.
    let (static_sym, _) = read_cell_c(&buf, 10, 5, p.w(), p.cs(), p.read_offset(), p.read_size());
    // Calibrated read with matching drifted U palette recovers the symbol.
    let cal = CalibratedLevels {
        y: crate::flicker::levels::LEVELS_Y,
        u: [crate::flicker::levels::LEVELS_U[0].saturating_add(18),
            crate::flicker::levels::LEVELS_U[1].saturating_add(18)],
        v: crate::flicker::levels::LEVELS_V,
    };
    let (cal_sym, _) = read_cell_c_cal(&buf, 10, 5, p.w(), p.cs(), p.read_offset(), p.read_size(), &cal);
    assert_eq!(cal_sym, 5, "calibrated read must recover drifted symbol");
    // (static_sym may or may not equal 5 depending on drift magnitude;
    // we assert the CALIBRATED path correctness, not that static fails.)
    let _ = static_sym;
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography read_cell_c_cal_tolerates -- --nocapture`
Expected: FAIL with `cannot find function 'read_cell_c_cal'`.

- [ ] **Step 7: Implement `read_cell_c_cal`**

Edit `src/flicker/codec.rs`. Add after `read_cell_c`:

```rust
/// Mode C cell reader using caller-supplied calibrated Y/U/V levels.
/// Semantically identical to `read_cell_c` but uses dynamic thresholds.
pub fn read_cell_c_cal(
    buf: &[u8], col: usize, row: usize,
    width: usize, cell_size: usize, read_offset: usize, read_size: usize,
    cal: &crate::flicker::calibration::CalibratedLevels,
) -> (u8, f32) {
    use crate::flicker::levels::quantise_with_levels;
    let (x0, y0) = cell_topleft(col, row, cell_size);
    let rx0 = x0 + read_offset;
    let ry0 = y0 + read_offset;
    let mut r_sum = 0u32;
    let mut g_sum = 0u32;
    let mut b_sum = 0u32;
    let mut count = 0u32;
    for py in ry0..ry0 + read_size {
        for px in rx0..rx0 + read_size {
            let o = rgb24_offset(px, py, width);
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
    let (y_sym, y_conf) = quantise_with_levels(y, &cal.y);
    let (u_sym, u_conf) = quantise_with_levels(u, &cal.u);
    let (v_sym, v_conf) = quantise_with_levels(v, &cal.v);
    let symbol = (y_sym << 2) | (u_sym << 1) | v_sym;
    let conf = y_conf.min(u_conf).min(v_conf);
    (symbol, conf)
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test --lib -p rtmp-steganography read_cell_c_cal_tolerates -- --nocapture`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/flicker/levels.rs src/flicker/codec.rs
git commit -m "feat(flicker): calibrated Mode C cell reader"
```

---

### Task 5: Pilot painter & reader emit Mode C symbols

**Files:**
- Modify: `src/flicker/pilot.rs`
- Modify: `src/flicker/frame.rs`

- [ ] **Step 1: Write failing test — paint/read Mode C pilots roundtrip**

Add to `src/flicker/pilot.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn paint_pilots_c_then_read_observations_match_expected_symbols() {
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    paint_pilots_c(&mut buf, 100, &[], &p);
    let obs = read_pilot_observations_c(&buf, 100, &[], &p);
    // Every symbol slot should have at least PILOT_COUNT/16 observations
    // (stratified distribution guarantee).
    for sym in 0..16 {
        assert!(obs[sym].len() >= PILOT_COUNT / 16,
            "symbol {sym} has {} observations, need >= {}", obs[sym].len(), PILOT_COUNT / 16);
    }
    // Observed YUV for each symbol should sit near the static palette points.
    for sym in 0..16 {
        let y_expect = crate::flicker::levels::LEVELS_Y[(sym >> 2) & 0b11];
        let u_expect = crate::flicker::levels::LEVELS_U[(sym >> 1) & 0b1];
        let v_expect = crate::flicker::levels::LEVELS_V[sym & 0b1];
        for &(y, u, v) in &obs[sym] {
            assert!((y as i32 - y_expect as i32).abs() <= 5,
                "sym {sym} observed Y {y}, expect ~{y_expect}");
            assert!((u as i32 - u_expect as i32).abs() <= 5,
                "sym {sym} observed U {u}, expect ~{u_expect}");
            assert!((v as i32 - v_expect as i32).abs() <= 5,
                "sym {sym} observed V {v}, expect ~{v_expect}");
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography paint_pilots_c_then_read -- --nocapture`
Expected: FAIL — `paint_pilots_c` and `read_pilot_observations_c` don't exist.

- [ ] **Step 3: Implement `paint_pilots_c` and `read_pilot_observations_c`**

Edit `src/flicker/pilot.rs`. Add these functions (keep existing `paint_pilots` and `validate_pilots` untouched for Mode B use):

```rust
use crate::flicker::codec::{paint_cell_c, read_cell_c_raw};

pub fn paint_pilots_c(buf: &mut [u8], frame_counter: u32, excluded: &[usize], p: &FlickerParams) {
    let positions = pilot_positions(frame_counter, excluded, p);
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx, p.grid_cols());
        paint_cell_c(buf, col, row, pilot_value_c(frame_counter, i), p.w(), p.cs());
    }
}

/// Returns 16 buckets; bucket `sym` holds raw (Y,U,V) means observed at
/// each pilot cell whose expected Mode C symbol was `sym`.
pub fn read_pilot_observations_c(
    buf: &[u8], frame_counter: u32, excluded: &[usize], p: &FlickerParams,
) -> [Vec<(u8, u8, u8)>; 16] {
    let positions = pilot_positions(frame_counter, excluded, p);
    let mut out: [Vec<(u8, u8, u8)>; 16] = Default::default();
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx, p.grid_cols());
        let yuv = read_cell_c_raw(buf, col, row, p.w(), p.cs(), p.read_offset(), p.read_size());
        let sym = pilot_value_c(frame_counter, i) as usize;
        out[sym].push(yuv);
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib -p rtmp-steganography paint_pilots_c_then_read -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Switch encoder to `paint_pilots_c` for Mode C frames**

Edit `src/flicker/frame.rs` in `FrameEncoder::encode`. Locate the existing pilot-painting loop (uses `paint_cell_b`):

```rust
for (i, &idx) in pilot_list.iter().enumerate() {
    let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
    let sym = crate::flicker::pilot::pilot_value(self.frame_counter, i);
    crate::flicker::codec::paint_cell_b(out_buf, col, row, sym, self.params.w(), self.params.cs());
}
```

Replace with mode-branched version:

```rust
match self.mode {
    ModulationMode::B => {
        for (i, &idx) in pilot_list.iter().enumerate() {
            let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
            let sym = crate::flicker::pilot::pilot_value(self.frame_counter, i);
            crate::flicker::codec::paint_cell_b(out_buf, col, row, sym, self.params.w(), self.params.cs());
        }
    }
    ModulationMode::C => {
        for (i, &idx) in pilot_list.iter().enumerate() {
            let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
            let sym = crate::flicker::pilot::pilot_value_c(self.frame_counter, i);
            crate::flicker::codec::paint_cell_c(out_buf, col, row, sym, self.params.w(), self.params.cs());
        }
    }
}
```

- [ ] **Step 6: Run existing frame tests to confirm Mode B still works**

Run: `cargo test --lib -p rtmp-steganography frame::tests -- --nocapture`
Expected: all existing tests PASS (Mode B roundtrip, 360p Mode B roundtrip, cell_size_actually_changes).

- [ ] **Step 7: Commit**

```bash
git add src/flicker/pilot.rs src/flicker/frame.rs
git commit -m "feat(flicker): Mode C pilots paint full palette and expose raw observations"
```

---

### Task 6: Decoder runs pilot calibration before payload read

**Files:**
- Modify: `src/flicker/frame.rs`

- [ ] **Step 1: Write failing end-to-end calibration test**

Add to `src/flicker/frame.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn mode_c_decode_recovers_frame_under_uniform_chroma_drift() {
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 77 };
    let frag = Fragment {
        msg_type: 2, message_id: 1, fragment_idx: 0, fragment_total: 1,
        payload: b"calibration-smoke-test-payload".to_vec(),
    };
    enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
    // Inject uniform +25 LSB drift on blue channel — simulates VK chroma shift
    // (blue → U direction in BT.601).
    for py in 0..p.h() {
        for px in 0..p.w() {
            let o = crate::flicker::grid::rgb24_offset(px, py, p.w());
            buf[o + 2] = buf[o + 2].saturating_add(25);
        }
    }
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments.len(), 1);
            assert!(fragments[0].payload.starts_with(b"calibration-smoke-test-payload"));
        }
        other => panic!("expected Ok with calibrated decode, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib -p rtmp-steganography mode_c_decode_recovers_frame_under_uniform_chroma -- --nocapture`
Expected: FAIL — decoder still uses static `read_cell` for payload, symbol errors from drift break the RS block.

- [ ] **Step 3: Wire pilot calibration into `FrameDecoder::decode`**

Edit `src/flicker/frame.rs`. In `FrameDecoder::decode`, locate the payload-reading loop (starts with `let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);`). Insert this BEFORE that loop (after `payload_perm` is built):

```rust
// Pilot calibration: for Mode C, read raw YUV at known pilot cells and
// compute per-frame adaptive levels. For Mode B, skip (no chroma).
let calibrated: Option<crate::flicker::calibration::CalibratedLevels> = match mode {
    ModulationMode::C => {
        let obs = crate::flicker::pilot::read_pilot_observations_c(
            buf, header.frame_counter, &pilot_excluded, p,
        );
        Some(crate::flicker::calibration::calibrate(&obs))
    }
    ModulationMode::B => None,
};
```

Then in the same `decode` method, replace the inner `read_cell(...)` call (inside the `for unit in 0..cells_per_byte` loop) with a mode-branched variant. Replace:

```rust
let (sym, conf) = read_cell(buf, col, row, mode, p.w(), p.cs(), p.read_offset(), p.read_size());
```

with:

```rust
let (sym, conf) = match (mode, calibrated.as_ref()) {
    (ModulationMode::C, Some(cal)) => crate::flicker::codec::read_cell_c_cal(
        buf, col, row, p.w(), p.cs(), p.read_offset(), p.read_size(), cal,
    ),
    _ => read_cell(buf, col, row, mode, p.w(), p.cs(), p.read_offset(), p.read_size()),
};
```

- [ ] **Step 4: The existing `validate_pilots` function still uses Mode B readers — drop the Mode B pilot validation gate for Mode C frames**

In the same `decode` method, locate:

```rust
let (pilot_ok, _pilot_conf) = validate_pilots(buf, header.frame_counter, &pilot_excluded, p);
if pilot_ok < PILOT_SUCCESS_MIN {
    return DecodeOutcome::Dropped { reason: DropReason::PilotValidationFailed(pilot_ok) };
}
```

Replace with:

```rust
match mode {
    ModulationMode::B => {
        let (pilot_ok, _) = validate_pilots(buf, header.frame_counter, &pilot_excluded, p);
        if pilot_ok < PILOT_SUCCESS_MIN {
            return DecodeOutcome::Dropped { reason: DropReason::PilotValidationFailed(pilot_ok) };
        }
    }
    ModulationMode::C => {
        // Mode C pilots are read during calibration below; the calibrator
        // itself falls back to static levels when observations are sparse,
        // so a bad-lock frame will simply fail at BlockRs/CRC rather than
        // here. Skip the Mode B gate.
    }
}
```

- [ ] **Step 5: Run the new test + all existing frame tests**

Run: `cargo test --lib -p rtmp-steganography frame::tests -- --nocapture`
Expected: all pass, including `mode_c_decode_recovers_frame_under_uniform_chroma_drift`.

- [ ] **Step 6: Commit**

```bash
git add src/flicker/frame.rs
git commit -m "feat(flicker): decoder calibrates Mode C levels from pilot observations"
```

---

### Task 7: Build the release binary and stop here for live validation

**Files:** none (compile & handoff)

- [ ] **Step 1: Build release binary**

Run from repo root:

```bash
cargo build --release --bin rtmp-steganography
```

Expected: clean compile. One warning about unused `FlickerParams` import in `src/config.rs` is pre-existing and OK.

- [ ] **Step 2: Confirm Mode C still roundtrips without drift**

Run: `cargo test --release --lib -p rtmp-steganography flicker:: -- --nocapture 2>&1 | tail -30`
Expected: all flicker tests pass.

- [ ] **Step 3: Commit (if any residual changes)**

```bash
git status
# If clean, no commit needed. Otherwise commit remaining hunks.
```

- [ ] **Step 4: Handoff note for live-validation stage**

Phase 1 is complete. Live validation against VK happens in Phase 3 Task 12 after Phase 2 is merged (so we measure combined impact). Do not run live validation here — the bench tool costs VK rate-limit budget and we want a single clean measurement after both phases.

---

# Phase 2: Multi-Level Coding for Mode C

Goal: decouple Y-lane from UV-lane in Mode C so chroma errors can't poison luma bytes. Independent RS(172,120) codewords per lane, same cell positions, same palette.

---

### Task 8: Split/join primitives for Y-lane and UV-lane byte streams

**Files:**
- Create: `src/flicker/channels.rs`
- Modify: `src/flicker/mod.rs`

- [ ] **Step 1: Write channels.rs skeleton + failing test**

Create `src/flicker/channels.rs`:

```rust
//! Split/join Mode C symbol streams into independent Y-lane and UV-lane
//! byte streams.
//!
//! Each Mode C cell carries:
//!   - 2 Y-bits (luma, 4 levels)
//!   - 1 U-bit (chroma, 2 levels)
//!   - 1 V-bit (chroma, 2 levels)
//!
//! Shared RS(172,120) over mixed Y+U+V bits means a single U/V error can
//! corrupt a byte whose Y-bits were perfect — chroma drift poisons luma.
//! This module packs 4 cells' Y-bits into one Y-byte and the same 4 cells'
//! UV-bits into one UV-byte, so the two lanes can be RS-encoded separately.
//!
//! Packing layout (4 cells c0..c3):
//!   Y-byte  = (y0 << 6) | (y1 << 4) | (y2 << 2) | y3
//!   UV-byte = (u0 << 7) | (v0 << 6) | (u1 << 5) | (v1 << 4)
//!           | (u2 << 3) | (v2 << 2) | (u3 << 1) |  v3

/// Number of cells consumed per Y-byte or per UV-byte.
pub const CELLS_PER_LANE_BYTE: usize = 4;

/// Pack a slice of cell symbols (each < 16) into one Y-byte + one UV-byte.
/// Caller must pass exactly `CELLS_PER_LANE_BYTE` symbols.
pub fn pack_lane_bytes(cells: &[u8; CELLS_PER_LANE_BYTE]) -> (u8, u8) {
    debug_assert!(cells.iter().all(|&s| s < 16));
    let mut y_byte = 0u8;
    let mut uv_byte = 0u8;
    for (i, &sym) in cells.iter().enumerate() {
        let y = (sym >> 2) & 0b11;
        let u = (sym >> 1) & 0b1;
        let v = sym & 0b1;
        y_byte |= y << (6 - 2 * i);
        uv_byte |= u << (7 - 2 * i);
        uv_byte |= v << (6 - 2 * i);
    }
    (y_byte, uv_byte)
}

/// Inverse of `pack_lane_bytes`: rebuild 4 cell symbols from Y-byte + UV-byte.
pub fn unpack_lane_bytes(y_byte: u8, uv_byte: u8) -> [u8; CELLS_PER_LANE_BYTE] {
    let mut out = [0u8; CELLS_PER_LANE_BYTE];
    for i in 0..CELLS_PER_LANE_BYTE {
        let y = (y_byte >> (6 - 2 * i)) & 0b11;
        let u = (uv_byte >> (7 - 2 * i)) & 0b1;
        let v = (uv_byte >> (6 - 2 * i)) & 0b1;
        out[i] = (y << 2) | (u << 1) | v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip_all_symbols() {
        for c0 in 0u8..16 {
            for c1 in 0u8..16 {
                for c2 in 0u8..16 {
                    for c3 in 0u8..16 {
                        let cells = [c0, c1, c2, c3];
                        let (y, uv) = pack_lane_bytes(&cells);
                        let back = unpack_lane_bytes(y, uv);
                        assert_eq!(back, cells, "failed at {:?}", cells);
                    }
                }
            }
        }
    }

    #[test]
    fn y_byte_isolates_luma_bits() {
        // c0 = Y=3 U=0 V=0 (sym = 12); c1..c3 = zero
        let (y, uv) = pack_lane_bytes(&[12, 0, 0, 0]);
        assert_eq!(y, 0b11_00_00_00, "Y-byte got {:#b}", y);
        assert_eq!(uv, 0, "UV-byte for pure-luma symbol must be 0, got {:#b}", uv);
    }

    #[test]
    fn uv_byte_isolates_chroma_bits() {
        // c0 = Y=0 U=1 V=1 (sym = 3); c1..c3 = zero
        let (y, uv) = pack_lane_bytes(&[3, 0, 0, 0]);
        assert_eq!(y, 0, "Y-byte for pure-chroma symbol must be 0, got {:#b}", y);
        assert_eq!(uv, 0b11_00_00_00, "UV-byte got {:#b}", uv);
    }
}
```

- [ ] **Step 2: Wire module in `src/flicker/mod.rs`**

Add `pub mod channels;` after `pub mod calibration;`:

```rust
pub mod calibration;
pub mod channels;
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib -p rtmp-steganography channels:: -- --nocapture`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add src/flicker/channels.rs src/flicker/mod.rs
git commit -m "feat(flicker): split Mode C symbols into independent Y-lane and UV-lane bytes"
```

---

### Task 9: Frame encoder emits Y-lane RS + UV-lane RS side by side

**Files:**
- Modify: `src/flicker/frame.rs`

- [ ] **Step 1: Write failing test — multi-level encode/decode roundtrip**

Add to `src/flicker/frame.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn mode_c_multilevel_roundtrip_no_drift() {
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 321 };
    let frag = Fragment {
        msg_type: 2, message_id: 9, fragment_idx: 0, fragment_total: 1,
        payload: b"multi-level-mode-c-test-payload-enough-bytes".to_vec(),
    };
    enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments.len(), 1);
            assert!(fragments[0].payload.starts_with(b"multi-level-mode-c-test-payload"));
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn mode_c_multilevel_y_lane_survives_chroma_only_errors() {
    // Burn down UV-lane with noise. Y-lane must still decode — that's the
    // whole point of multi-level coding.
    let p = FlickerParams::with_cell(432, 240, 24, 4);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 555 };
    let frag = Fragment {
        msg_type: 2, message_id: 1, fragment_idx: 0, fragment_total: 1,
        payload: b"y-lane-resilience".to_vec(),
    };
    enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
    // Push EVERY pixel's blue channel to 255 → obliterates U-bits while
    // leaving luma mostly intact (luma is 0.299R + 0.587G + 0.114B —
    // only 11% blue contribution).
    for py in 0..p.h() {
        for px in 0..p.w() {
            let o = crate::flicker::grid::rgb24_offset(px, py, p.w());
            buf[o + 2] = 255;
        }
    }
    let dec = FrameDecoder { params: p };
    // Accept either Ok or CRC failure on the UV-lane specifically. The Y-
    // lane block MUST reconstruct (no BlockRsFailed on even-indexed blocks).
    match dec.decode(&buf) {
        DecodeOutcome::Dropped { reason: DropReason::PayloadCrcMismatch } => {
            // Expected: UV-lane bad, Y-lane OK → joined payload fails CRC.
        }
        DecodeOutcome::Ok { .. } => {
            // Even better: UV-lane parity was enough to recover chroma.
        }
        other => panic!("Y-lane must not fail with BlockRs; got {other:?}"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib -p rtmp-steganography mode_c_multilevel -- --nocapture`
Expected: both FAIL — current encoder packs mixed bits, not split lanes.

- [ ] **Step 3: Refactor `FrameEncoder::encode` to split Y-lane and UV-lane**

Edit `src/flicker/frame.rs::FrameEncoder::encode`. Locate this block (near the bottom of `encode`):

```rust
let mut encoded_blocks: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
for i in 0..self.block_count() {
    let chunk = &payload_bytes[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K];
    let encoded = encode_block(chunk)?;
    encoded_blocks.extend_from_slice(&encoded);
}
```

and further down:

```rust
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
        paint_cell(out_buf, col, row, symbol, self.mode, self.params.w(), self.params.cs());
    }
}
```

Replace that whole tail (both blocks above) with mode-branched multi-level:

```rust
match self.mode {
    ModulationMode::B => {
        let mut encoded_blocks: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
        for i in 0..self.block_count() {
            let chunk = &payload_bytes[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K];
            let encoded = encode_block(chunk)?;
            encoded_blocks.extend_from_slice(&encoded);
        }
        for (byte_idx, &byte) in encoded_blocks.iter().enumerate() {
            for unit in 0..4 {
                let symbol = (byte >> (2 * (3 - unit))) & 0b11;
                let cell_pos = byte_idx * 4 + unit;
                if cell_pos >= payload_perm.len() { break; }
                let (col, row) = payload_perm[cell_pos];
                paint_cell(out_buf, col, row, symbol, self.mode, self.params.w(), self.params.cs());
            }
        }
    }
    ModulationMode::C => {
        // Multi-level: split payload into Y-lane and UV-lane byte streams,
        // each of size (total_cells_for_payload / 4) bytes. Each lane gets
        // its own RS(172,120) encoding, independent codewords.
        use crate::flicker::channels::{pack_lane_bytes, CELLS_PER_LANE_BYTE};
        let lane_byte_count = self.block_count() * RS_BLOCK_K; // K data bytes per lane
        // Split input payload_bytes: first half → Y-lane, second half → UV-lane.
        // Caller sees payload_len bytes total (Y + UV concatenated) in logical
        // order. Padding was already applied in payload_bytes above.
        debug_assert_eq!(payload_bytes.len(), 2 * lane_byte_count,
            "payload_bytes length must be 2 * lane_byte_count for Mode C multi-level");
        let (y_data, uv_data) = payload_bytes.split_at(lane_byte_count);
        let mut y_encoded: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
        let mut uv_encoded: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
        for i in 0..self.block_count() {
            y_encoded.extend_from_slice(&encode_block(&y_data[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K])?);
            uv_encoded.extend_from_slice(&encode_block(&uv_data[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K])?);
        }
        // Paint: each (y_byte_i, uv_byte_i) pair consumes CELLS_PER_LANE_BYTE cells.
        let pair_count = y_encoded.len(); // == uv_encoded.len()
        for pair_i in 0..pair_count {
            let cells_packed = [
                ((y_encoded[pair_i] >> 6) & 0b11) << 2 | ((uv_encoded[pair_i] >> 7) & 0b1) << 1 | ((uv_encoded[pair_i] >> 6) & 0b1),
                ((y_encoded[pair_i] >> 4) & 0b11) << 2 | ((uv_encoded[pair_i] >> 5) & 0b1) << 1 | ((uv_encoded[pair_i] >> 4) & 0b1),
                ((y_encoded[pair_i] >> 2) & 0b11) << 2 | ((uv_encoded[pair_i] >> 3) & 0b1) << 1 | ((uv_encoded[pair_i] >> 2) & 0b1),
                ((y_encoded[pair_i]) & 0b11) << 2 | ((uv_encoded[pair_i] >> 1) & 0b1) << 1 | (uv_encoded[pair_i] & 0b1),
            ];
            // Sanity-check the pack helper matches the formula above.
            debug_assert_eq!(
                pack_lane_bytes(&cells_packed),
                (y_encoded[pair_i], uv_encoded[pair_i]),
                "pack_lane_bytes mismatch at pair_i={}",
                pair_i
            );
            for unit in 0..CELLS_PER_LANE_BYTE {
                let cell_pos = pair_i * CELLS_PER_LANE_BYTE + unit;
                if cell_pos >= payload_perm.len() { break; }
                let (col, row) = payload_perm[cell_pos];
                paint_cell(out_buf, col, row, cells_packed[unit], self.mode, self.params.w(), self.params.cs());
            }
        }
    }
}
```

- [ ] **Step 4: Recompute `block_count_for` to account for Mode C's lane-byte denominator**

Edit `src/flicker/frame.rs::block_count_for`. Replace:

```rust
let cells_per_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 2 };
let payload_bytes_capacity = payload_cells / cells_per_byte;
// Each RS codeword carries RS_BLOCK_N encoded bytes on the wire.
payload_bytes_capacity / RS_BLOCK_N
```

with:

```rust
// Mode B: 4 cells per byte (2 bits each). Mode C multi-level: 4 cells per
// PAIR of bytes (one Y-byte + one UV-byte). Either way, each RS codeword
// is RS_BLOCK_N bytes long. Block count in Mode C refers to PER-LANE blocks:
// total encoded bytes on the wire = block_count * RS_BLOCK_N * 2 (both lanes).
let cells_per_lane_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 4 };
let lane_bytes_capacity = payload_cells / cells_per_lane_byte;
lane_bytes_capacity / RS_BLOCK_N
```

Also, `FrameEncoder::payload_bytes_per_frame` currently returns `block_count * RS_BLOCK_K`. For Mode C multi-level, capacity DOUBLES because two lanes each carry `block_count * RS_BLOCK_K` bytes. Update:

```rust
pub fn payload_bytes_per_frame(&self) -> usize {
    let per_lane = self.block_count() * RS_BLOCK_K;
    match self.mode {
        ModulationMode::B => per_lane,
        ModulationMode::C => 2 * per_lane,
    }
}
```

- [ ] **Step 5: Refactor `FrameDecoder::decode` to split Y-lane and UV-lane reads**

Edit `src/flicker/frame.rs::FrameDecoder::decode`. Locate the existing payload-decode loop (starts with `let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);`). Replace the entire loop body (everything from `let mode = header.modulation_mode;` through the end of the `for block_i in 0..block_count` loop, until just before the `let mut payload_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);` line) with mode-branched version. Paste this replacement:

```rust
let mode = header.modulation_mode;
let block_count = header.fec_params[2] as usize;

let payload_bytes: Vec<u8> = match mode {
    ModulationMode::B => {
        let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);
        for block_i in 0..block_count {
            let mut shards: Vec<Option<u8>> = vec![None; RS_BLOCK_N];
            for byte_i in 0..RS_BLOCK_N {
                let mut byte = 0u8;
                let mut min_conf = 1.0f32;
                for unit in 0..4 {
                    let cell_pos = (block_i * RS_BLOCK_N + byte_i) * 4 + unit;
                    if cell_pos >= payload_perm.len() { break; }
                    let (col, row) = payload_perm[cell_pos];
                    let (sym, conf) = read_cell(buf, col, row, ModulationMode::B, p.w(), p.cs(), p.read_offset(), p.read_size());
                    byte = (byte << 2) | (sym & 0b11);
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
        let mut bytes = Vec::with_capacity(block_count * RS_BLOCK_K);
        for b in &decoded_blocks { bytes.extend_from_slice(b); }
        bytes
    }
    ModulationMode::C => {
        use crate::flicker::channels::{unpack_lane_bytes, CELLS_PER_LANE_BYTE};
        let mut y_shards: Vec<Vec<Option<u8>>> = (0..block_count).map(|_| vec![None; RS_BLOCK_N]).collect();
        let mut uv_shards: Vec<Vec<Option<u8>>> = (0..block_count).map(|_| vec![None; RS_BLOCK_N]).collect();
        for block_i in 0..block_count {
            for byte_i in 0..RS_BLOCK_N {
                let pair_i = block_i * RS_BLOCK_N + byte_i;
                let mut cells_observed = [0u8; CELLS_PER_LANE_BYTE];
                let mut min_conf = 1.0f32;
                let mut any_skipped = false;
                for unit in 0..CELLS_PER_LANE_BYTE {
                    let cell_pos = pair_i * CELLS_PER_LANE_BYTE + unit;
                    if cell_pos >= payload_perm.len() { any_skipped = true; break; }
                    let (col, row) = payload_perm[cell_pos];
                    let (sym, conf) = match calibrated.as_ref() {
                        Some(cal) => crate::flicker::codec::read_cell_c_cal(
                            buf, col, row, p.w(), p.cs(), p.read_offset(), p.read_size(), cal,
                        ),
                        None => read_cell(buf, col, row, ModulationMode::C, p.w(), p.cs(), p.read_offset(), p.read_size()),
                    };
                    cells_observed[unit] = sym & 0b1111;
                    min_conf = min_conf.min(conf);
                }
                if any_skipped { continue; }
                // Re-pack observed cells into (y_byte, uv_byte) for this pair.
                let cells_packed_back = unpack_lane_bytes(
                    // We want the inverse path: observed cell symbols → y_byte, uv_byte.
                    // `pack_lane_bytes` is that inverse here.
                    crate::flicker::channels::pack_lane_bytes(&cells_observed).0,
                    crate::flicker::channels::pack_lane_bytes(&cells_observed).1,
                );
                let _ = cells_packed_back; // (sanity path not needed at runtime)
                let (y_byte, uv_byte) = crate::flicker::channels::pack_lane_bytes(&cells_observed);
                if min_conf >= PILOT_CONFIDENCE_THRESHOLD {
                    y_shards[block_i][byte_i] = Some(y_byte);
                    uv_shards[block_i][byte_i] = Some(uv_byte);
                }
            }
        }
        let mut y_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
        let mut uv_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
        for block_i in 0..block_count {
            match decode_block(&y_shards[block_i]) {
                Ok(d) => y_bytes.extend_from_slice(&d),
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::BlockRsFailed(block_i) },
            }
            match decode_block(&uv_shards[block_i]) {
                Ok(d) => uv_bytes.extend_from_slice(&d),
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::BlockRsFailed(block_count + block_i) },
            }
        }
        // Concatenate Y-lane + UV-lane in the same order encoder used.
        let mut bytes = y_bytes;
        bytes.extend_from_slice(&uv_bytes);
        bytes
    }
};
```

Note: the downstream code that slices `payload_bytes` into header.payload_len + CRC is unchanged — it reads whatever layout we produce.

- [ ] **Step 6: Run all frame tests**

Run: `cargo test --lib -p rtmp-steganography frame::tests -- --nocapture`
Expected: all pass, including `mode_c_multilevel_roundtrip_no_drift`, `mode_c_multilevel_y_lane_survives_chroma_only_errors`, `mode_c_decode_recovers_frame_under_uniform_chroma_drift`, `encode_decode_roundtrip_b_default`, `encode_decode_roundtrip_b_at_360p`, `cell_size_actually_changes_painted_output`.

- [ ] **Step 7: Commit**

```bash
git add src/flicker/frame.rs
git commit -m "feat(flicker): Mode C multi-level encoder/decoder splits Y-lane and UV-lane into independent RS codewords"
```

---

### Task 10: Update `max_payload` callers for Mode C doubled capacity

**Files:**
- Modify: `src/peer/app.rs` (reference only; recalculates via `block_count_for` which now doubles for Mode C)

- [ ] **Step 1: Verify no external recalculation assumes old Mode C capacity**

Run: `grep -rn 'cells_per_byte\|block_count * RS_BLOCK_K' src/ | grep -v 'flicker/'`
Expected: zero matches outside `src/flicker/`.

- [ ] **Step 2: Confirm `max_payload` log line will show doubled capacity on Mode C**

Find the log line in `src/peer/app.rs`:

```rust
let frame_capacity = block_count * RS_BLOCK_K;
let max_payload = frame_capacity.saturating_sub(FRAGMENT_HEADER_BYTES).saturating_sub(4);
```

This undercounts for Mode C post-refactor (real capacity is `2 * block_count * RS_BLOCK_K` for Mode C). Update:

```rust
let per_lane = block_count * RS_BLOCK_K;
let frame_capacity = match cfg.modulation_mode {
    crate::flicker::ModulationMode::B => per_lane,
    crate::flicker::ModulationMode::C => 2 * per_lane,
};
let max_payload = frame_capacity.saturating_sub(FRAGMENT_HEADER_BYTES).saturating_sub(4);
```

- [ ] **Step 3: Build and smoke-run tests**

```bash
cargo build --release --bin rtmp-steganography 2>&1 | tail -5
cargo test --lib -p rtmp-steganography -- --nocapture 2>&1 | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/peer/app.rs
git commit -m "fix(peer): correct max_payload accounting for Mode C multi-level doubled capacity"
```

---

# Phase 3: Live Validation

Goal: prove the 240p cell=4 Mode C goal from Success Criteria on real VK.

---

### Task 11: Rebuild and prepare fresh peer config

**Files:**
- Modify: `.env.peer-a`, `.env.peer-b`

- [ ] **Step 1: Ensure release binary is up to date**

```bash
cargo build --release --bin rtmp-steganography 2>&1 | tail -5
```

Expected: `Finished 'release' profile`.

- [ ] **Step 2: Set canonical validation config in both env files**

Both `.env.peer-a` and `.env.peer-b` must contain:

```
peer_my_rtmp_url=rtmp://vsu.mycdn.me/input
peer_their_vk_channel=pavel8899
peer_rx_warmup_ms=10000
flicker_modulation_mode=C
flicker_log_every_frame=1
peer_flicker_fps=24
peer_stream_width=432
peer_stream_height=240
peer_flicker_cell_size=4
peer_x264_qp=22
peer_vk_prefer=cmaf
```

(Each also keeps its unique `peer_my_stream_key` and `peer_their_stream_name` as before.)

Verify with:

```bash
diff <(grep -E 'flicker_|peer_stream|peer_x264|peer_vk_prefer|peer_rx_warmup' .env.peer-a | sort) \
     <(grep -E 'flicker_|peer_stream|peer_x264|peer_vk_prefer|peer_rx_warmup' .env.peer-b | sort)
```

Expected: empty output (identical runtime knobs on both peers).

- [ ] **Step 3: Commit env changes (if any)**

```bash
git diff .env.peer-a .env.peer-b
# Review. If intentional runtime config changes for validation, commit:
git add .env.peer-a .env.peer-b
git commit -m "test(live): canonical 240p cell=4 Mode C CMAF validation env"
```

---

### Task 12: Run the 1 KB / 30 s bench and record metrics

**Files:**
- Create: `metrics/live/stage1-validation.md`

- [ ] **Step 1: Kill stale peers and start fresh peers**

```bash
taskkill //F //IM rtmp-steganography.exe 2>/dev/null; sleep 1
[ -f .env ] && mv .env .env.bak.stage1 || true
rm -f metrics/live/peer-a-stage1.log metrics/live/peer-b-stage1.log
export METRICS_DIR=./metrics TUNNEL_PROFILE=throughput
(set -a; source .env.peer-a; export PEER_ID=A TUNNEL_PROFILE METRICS_DIR; \
  exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11080) \
  > metrics/live/peer-a-stage1.log 2>&1 &
sleep 3
(set -a; source .env.peer-b; export PEER_ID=B TUNNEL_PROFILE METRICS_DIR; \
  exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11081 --with-bench-support) \
  > metrics/live/peer-b-stage1.log 2>&1 &
echo "[warmup 180s — VK needs time to register 240p stream + both peers resolve via CMAF ondemand_hls]"
sleep 180
```

- [ ] **Step 2: Confirm both peers resolved CMAF at 432×240**

```bash
grep -E 'pick order|ffprobe native|flicker EXPECTED' metrics/live/peer-a-stage1.log | head -4
grep -E 'pick order|ffprobe native|flicker EXPECTED' metrics/live/peer-b-stage1.log | head -4
```

Expected: each peer shows `pick order: ondemand_hls, ...` and `ffprobe native stream: codec_name=h264 width=432 height=240`. If peer-a or peer-b is stuck on `vk resolve failed`, wait another 60s and retry. If still failing, STOP — the test premise (CMAF resolved successfully) is broken; investigate VK-side before continuing.

- [ ] **Step 3: Run 1 KB / 30 s bench**

```bash
PEER_ID=A ./target/release/rtmp-steganography bench smoke \
  --socks 127.0.0.1:11080 --iterations 0 --skip-iperf \
  --throughput-bytes 1024 \
  --raw-echo-host 127.0.0.1 --raw-echo-port 18090 \
  --metrics-dir ./metrics 2>&1 | tail -5
```

- [ ] **Step 4: Extract bench + per-peer flicker counters**

```bash
latest=$(find metrics -name events.jsonl -mmin -3 2>/dev/null | sort | tail -1)
echo "[log: $latest]"
grep '"event":"throughput_done"' "$latest" | tail -1 | python -m json.tool
for p in a b; do
  log="metrics/live/peer-${p}-stage1.log"
  rx=$(grep -c 'rx frame=' "$log")
  drop=$(grep -c 'dropped:' "$log")
  hdr=$(grep -c HeaderRsFailed "$log")
  blk=$(grep -c BlockRsFailed "$log")
  crc=$(grep -c PayloadCrcMismatch "$log")
  echo "peer-$p rx=$rx drop=$drop HdrRs=$hdr BlockRs=$blk CRC=$crc"
done
```

- [ ] **Step 5: Record results in metrics/live/stage1-validation.md**

Create `metrics/live/stage1-validation.md` with the numbers captured above, following this template:

```markdown
# Stage 1 validation — 240p cell=4 Mode C CMAF (pilot-cal + multi-level)

**Date:** (fill today)
**Binary:** $(git rev-parse HEAD) — (release build)
**Config:** 432x240 / cell=4 / Mode C / qp=22 / peer_vk_prefer=cmaf

## Bench

| metric | value |
|---|---:|
| ok | (true/false) |
| bytes_sent | |
| bytes_received | |
| elapsed_ms | |
| oneway_kbits_per_s | |
| tunnel_internal_kbits_per_s | |
| fail_stage | |

## Flicker counters

| metric | peer-a | peer-b |
|---|---:|---:|
| rx frames | | |
| dropped | | |
| HeaderRsFailed | | |
| BlockRsFailed | | |
| PayloadCrcMismatch | | |

## Comparison vs pre-Stage-1 baseline

| metric | baseline (static levels, single RS) | Stage 1 (pilot-cal + multi-level) | delta |
|---|---:|---:|---:|
| drop rate | 97.9% | (fill) | |
| BlockRsFailed rate | 97.3% | (fill) | |
| bytes_rx on 1 KB bench | 0 | (fill) | |

## Success Criteria pass/fail

- [ ] bench returns ok=true within 30 s
- [ ] oneway_kbits_per_s ≥ 0.3
- [ ] peer-a BlockRsFailed/rx ≤ 15%
- [ ] peer-a PayloadCrcMismatch/rx ≤ 2%
```

- [ ] **Step 6: Stop peers and restore env**

```bash
taskkill //F //IM rtmp-steganography.exe 2>/dev/null; sleep 1
[ -f .env.bak.stage1 ] && mv .env.bak.stage1 .env 2>/dev/null
```

- [ ] **Step 7: Commit the recorded results**

```bash
git add metrics/live/stage1-validation.md
git commit -m "test(live): Stage 1 validation results for 240p cell=4 Mode C"
```

- [ ] **Step 8: Interpret pass/fail**

If all 4 success criteria pass → Stage 1 is done; open PR.
If BlockRsFailed ≤ 15% but CRC > 2% → likely residual UV-lane drift the calibrator didn't fully track; investigate per-frame calibration stability (look at `cal.u[0]` variance across frames by adding a one-shot `eprintln!` in `frame.rs` for a debugging branch).
If BlockRsFailed still > 40% → calibration isn't converging; run `cargo test flicker::calibration::tests` to confirm the logic works in-process, then sample raw pilot observations from a live frame dump to see if VK drift exceeds the ±30 LSB the test covers.

---

## Self-review summary

- **Scope:** single plan covers pilot calibration AND multi-level coding because both modify the same decode pipeline (frame.rs::decode) — splitting them into independent plans would duplicate the pilot-list-derivation code.
- **Spec coverage check:** every success criterion maps to Task 12 step 8. Every file listed under "File Structure" has at least one task modifying it. No "TBD" or "similar to" references remain. All test functions define full assertion bodies; all implementation steps show complete code.
- **Type consistency check:** `CalibratedLevels { y: [u8;4], u: [u8;2], v: [u8;2] }` is used identically in calibration.rs, codec.rs, and frame.rs. `pack_lane_bytes`/`unpack_lane_bytes` appear only in channels.rs and frame.rs with matching signature `(&[u8;4]) -> (u8,u8)` and `(u8,u8) -> [u8;4]`. `read_cell_c_cal` signature `(buf, col, row, w, cs, read_offset, read_size, &CalibratedLevels) -> (u8, f32)` matches both its definition in codec.rs and its calls in frame.rs Task 9 Step 5.
