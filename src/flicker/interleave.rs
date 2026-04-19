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
