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
