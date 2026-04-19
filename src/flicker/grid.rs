//! Runtime-configurable grid parameters for flicker v2.
//!
//! Both peers must load identical `FlickerParams` from env (.env) — the
//! protocol is symmetric and assumes tx / rx agree on frame dimensions.
//!
//! Cell size is fixed at 4 px for now; width and height must be multiples of
//! it. Cell-level logic (markers, pilots, permutations) all derives from
//! grid_cols = width / 4 and grid_rows = height / 4.

pub const CELL_SIZE: usize = 4;

pub const DEFAULT_FRAME_W: u32 = 256;
pub const DEFAULT_FRAME_H: u32 = 144;
pub const DEFAULT_FPS: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlickerParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl FlickerParams {
    pub const fn new(width: u32, height: u32, fps: u32) -> Self {
        Self { width, height, fps }
    }

    pub const fn default_256x144_24() -> Self {
        Self { width: DEFAULT_FRAME_W, height: DEFAULT_FRAME_H, fps: DEFAULT_FPS }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.width as usize % CELL_SIZE != 0 {
            return Err(format!("flicker width {} not divisible by cell size {}", self.width, CELL_SIZE));
        }
        if self.height as usize % CELL_SIZE != 0 {
            return Err(format!("flicker height {} not divisible by cell size {}", self.height, CELL_SIZE));
        }
        if self.fps == 0 { return Err("flicker fps cannot be 0".to_string()); }
        if (self.grid_cols() as usize) < 16 || (self.grid_rows() as usize) < 16 {
            return Err(format!("grid {}x{} too small (need >= 16x16 cells)", self.grid_cols(), self.grid_rows()));
        }
        Ok(())
    }

    #[inline] pub const fn w(&self) -> usize { self.width  as usize }
    #[inline] pub const fn h(&self) -> usize { self.height as usize }
    #[inline] pub const fn grid_cols(&self) -> usize { self.w() / CELL_SIZE }
    #[inline] pub const fn grid_rows(&self) -> usize { self.h() / CELL_SIZE }
    #[inline] pub const fn total_cells(&self) -> usize { self.grid_cols() * self.grid_rows() }
    #[inline] pub const fn frame_bytes_rgb24(&self) -> usize { self.w() * self.h() * 3 }
}

/// Central readable region within a cell: 2×2 px centered in 4×4, giving 1 px guard on each side.
pub const CELL_READ_OFFSET: usize = 1;
pub const CELL_READ_SIZE: usize = 2;

/// Convert (col, row) logical cell coords to top-left pixel (x, y).
#[inline]
pub fn cell_topleft(col: usize, row: usize) -> (usize, usize) {
    (col * CELL_SIZE, row * CELL_SIZE)
}

/// Byte offset into a RGB24 buffer for pixel (x, y) given a frame width.
#[inline]
pub fn rgb24_offset(x: usize, y: usize, width: usize) -> usize {
    (y * width + x) * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_grid_dimensions_are_consistent() {
        let p = FlickerParams::default_256x144_24();
        assert_eq!(p.grid_cols(), 64);
        assert_eq!(p.grid_rows(), 36);
        assert_eq!(p.total_cells(), 2304);
        assert_eq!(p.frame_bytes_rgb24(), 110_592);
    }

    #[test]
    fn hd_360p_grid() {
        let p = FlickerParams::new(640, 360, 24);
        assert_eq!(p.grid_cols(), 160);
        assert_eq!(p.grid_rows(), 90);
        assert_eq!(p.total_cells(), 14400);
        assert_eq!(p.frame_bytes_rgb24(), 691_200);
    }

    #[test]
    fn cell_topleft_maps_correctly() {
        assert_eq!(cell_topleft(0, 0), (0, 0));
        assert_eq!(cell_topleft(63, 35), (252, 140));
    }

    #[test]
    fn rgb24_offset_is_row_major() {
        assert_eq!(rgb24_offset(0, 0, 256), 0);
        assert_eq!(rgb24_offset(1, 0, 256), 3);
        assert_eq!(rgb24_offset(0, 1, 256), 256 * 3);
    }

    #[test]
    fn validate_rejects_misaligned() {
        assert!(FlickerParams::new(255, 144, 24).validate().is_err());
        assert!(FlickerParams::new(256, 143, 24).validate().is_err());
        assert!(FlickerParams::new(256, 144, 0).validate().is_err());
    }
}
