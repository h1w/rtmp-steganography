//! Pilot cells — deterministic per-frame brightness references.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

use crate::flicker::codec::{paint_cell_b, read_cell_b, paint_cell_c, read_cell_c_raw};
use crate::flicker::grid::FlickerParams;
use crate::flicker::interleave::cell_index_to_col_row;

pub const PILOT_COUNT: usize = 115;
pub const PILOT_BASE_SEED: [u8; 16] = *b"flicker-pilot-v2";

fn seed_for(frame_counter: u32) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(&PILOT_BASE_SEED);
    seed[16..20].copy_from_slice(&frame_counter.to_le_bytes());
    seed
}

pub fn pilot_positions(frame_counter: u32, excluded: &[usize], p: &FlickerParams) -> Vec<usize> {
    let mut rng = ChaCha8Rng::from_seed(seed_for(frame_counter));
    let excluded_set: std::collections::HashSet<usize> = excluded.iter().copied().collect();
    let mut candidates: Vec<usize> = (0..p.total_cells()).filter(|i| !excluded_set.contains(i)).collect();
    let mut picked = Vec::with_capacity(PILOT_COUNT);
    for _ in 0..PILOT_COUNT.min(candidates.len()) {
        let j = (rng.next_u32() as usize) % candidates.len();
        picked.push(candidates.swap_remove(j));
    }
    picked.sort_unstable();
    picked
}

pub fn pilot_value(frame_counter: u32, index_in_pilot_list: usize) -> u8 {
    let mut rng = ChaCha8Rng::from_seed(seed_for(frame_counter ^ 0xDEADBEEF));
    for _ in 0..index_in_pilot_list { let _ = rng.next_u32(); }
    (rng.next_u32() & 0b11) as u8
}

pub fn paint_pilots(buf: &mut [u8], frame_counter: u32, excluded: &[usize], p: &FlickerParams) {
    let positions = pilot_positions(frame_counter, excluded, p);
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx, p.grid_cols());
        paint_cell_b(buf, col, row, pilot_value(frame_counter, i), p.w(), p.cs());
    }
}

pub fn validate_pilots(buf: &[u8], frame_counter: u32, excluded: &[usize], p: &FlickerParams) -> (f32, f32) {
    let positions = pilot_positions(frame_counter, excluded, p);
    let mut ok = 0usize;
    let mut conf_sum = 0f32;
    for (i, &idx) in positions.iter().enumerate() {
        let (col, row) = cell_index_to_col_row(idx, p.grid_cols());
        let (sym, conf) = read_cell_b(buf, col, row, p.w(), p.cs(), p.read_offset(), p.read_size());
        conf_sum += conf;
        if sym == pilot_value(frame_counter, i) { ok += 1; }
    }
    let total = positions.len().max(1) as f32;
    (ok as f32 / total, conf_sum / total)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pilot_positions_deterministic_per_counter() {
        let p = FlickerParams::default_256x144_24();
        let a = pilot_positions(42, &[], &p);
        let b = pilot_positions(42, &[], &p);
        assert_eq!(a, b);
        assert_eq!(a.len(), PILOT_COUNT);
    }

    #[test]
    fn paint_and_validate_cell4() {
        let p = FlickerParams::default_256x144_24();
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        paint_pilots(&mut buf, 100, &[], &p);
        let (ok, _) = validate_pilots(&buf, 100, &[], &p);
        assert!(ok > 0.99);
    }

    #[test]
    fn paint_and_validate_cell16_at_640x360() {
        let p = FlickerParams::with_cell(640, 360, 24, 16);
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        paint_pilots(&mut buf, 100, &[], &p);
        let (ok, _) = validate_pilots(&buf, 100, &[], &p);
        assert!(ok > 0.99);
    }

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
        // Tolerance is ±30 rather than ±5: YUV→RGB→YUV round-trip (the paint/read
        // path goes through 8-bit RGB) loses up to ~25 LSB at palette extremes.
        // This matches the pre-approved ±30 precedent set in Task 2's
        // `read_cell_c_raw_returns_yuv_means_near_palette`.
        for sym in 0..16 {
            let y_expect = crate::flicker::levels::LEVELS_Y[(sym >> 2) & 0b11];
            let u_expect = crate::flicker::levels::LEVELS_U[(sym >> 1) & 0b1];
            let v_expect = crate::flicker::levels::LEVELS_V[sym & 0b1];
            for &(y, u, v) in &obs[sym] {
                assert!((y as i32 - y_expect as i32).abs() <= 30,
                    "sym {sym} observed Y {y}, expect ~{y_expect}");
                assert!((u as i32 - u_expect as i32).abs() <= 30,
                    "sym {sym} observed U {u}, expect ~{u_expect}");
                assert!((v as i32 - v_expect as i32).abs() <= 30,
                    "sym {sym} observed V {v}, expect ~{v_expect}");
            }
        }
    }
}
