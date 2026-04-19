//! Pilot cells — deterministic per-frame brightness references.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

use crate::flicker::codec::{paint_cell_b, read_cell_b};
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
}
