//! Deterministic permutations for cell↔byte mapping.
//!
//! Runtime-configurable: grid dimensions come from `FlickerParams`.

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

const PERMUTATION_SEED: [u8; 32] = [
    0x46, 0x4c, 0x49, 0x43, 0x4b, 0x45, 0x52, 0x32, // "FLICKER2"
    0x2d, 0x53, 0x50, 0x41, 0x54, 0x49, 0x41, 0x4c, // "-SPATIAL"
    0x2d, 0x56, 0x32, 0x2d, 0x50, 0x45, 0x52, 0x4d, // "-V2-PERM"
    0x55, 0x54, 0x41, 0x54, 0x49, 0x4f, 0x4e, 0x21, // "UTATION!"
];

/// Generate the spatial permutation of all cells in the grid.
/// `excluded` is a set of cell indices to skip.
/// Returns a Vec of (col, row) in the order payload cells should be laid out.
pub fn cell_permutation(excluded: &[usize], total_cells: usize, grid_cols: usize) -> Vec<(usize, usize)> {
    let mut rng = ChaCha8Rng::from_seed(PERMUTATION_SEED);
    let excluded_set: std::collections::HashSet<usize> = excluded.iter().copied().collect();
    let mut indices: Vec<usize> = (0..total_cells).filter(|i| !excluded_set.contains(i)).collect();
    for i in (1..indices.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        indices.swap(i, j);
    }
    indices.into_iter().map(|idx| cell_index_to_col_row(idx, grid_cols)).collect()
}

#[inline]
pub fn cell_index_to_col_row(idx: usize, grid_cols: usize) -> (usize, usize) {
    (idx % grid_cols, idx / grid_cols)
}

#[inline]
pub fn col_row_to_cell_index(col: usize, row: usize, grid_cols: usize) -> usize {
    row * grid_cols + col
}

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
    use crate::flicker::grid::FlickerParams;

    #[test]
    fn cell_permutation_is_deterministic() {
        let p = FlickerParams::default_256x144_24();
        let a = cell_permutation(&[], p.total_cells(), p.grid_cols());
        let b = cell_permutation(&[], p.total_cells(), p.grid_cols());
        assert_eq!(a, b);
    }

    #[test]
    fn cell_permutation_excludes_markers() {
        let p = FlickerParams::default_256x144_24();
        let excluded = vec![0, 1, 2, 3];
        let perm = cell_permutation(&excluded, p.total_cells(), p.grid_cols());
        assert_eq!(perm.len(), p.total_cells() - 4);
        for (col, row) in &perm {
            let idx = col_row_to_cell_index(*col, *row, p.grid_cols());
            assert!(!excluded.contains(&idx));
        }
    }

    #[test]
    fn cell_permutation_bijection_at_360p() {
        let p = FlickerParams::new(640, 360, 24);
        let perm = cell_permutation(&[], p.total_cells(), p.grid_cols());
        let mut seen = vec![false; p.total_cells()];
        for (col, row) in &perm {
            let idx = col_row_to_cell_index(*col, *row, p.grid_cols());
            assert!(!seen[idx]);
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
