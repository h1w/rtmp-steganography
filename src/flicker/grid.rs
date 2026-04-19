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
