//! Runtime-configurable grid parameters for flicker v2.
//!
//! Both peers must load identical `FlickerParams` from env (.env) — the
//! protocol is symmetric and assumes tx / rx agree on frame dimensions.
//!
//! Cell size is fixed at 4 px for now; width and height must be multiples of
//! it. Cell-level logic (markers, pilots, permutations) all derives from
//! grid_cols = width / 4 and grid_rows = height / 4.

pub const DEFAULT_CELL_SIZE: usize = 4;
pub const DEFAULT_FRAME_W: u32 = 256;
pub const DEFAULT_FRAME_H: u32 = 144;
pub const DEFAULT_FPS: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlickerParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub cell_size: u32,
}

impl FlickerParams {
    pub const fn new(width: u32, height: u32, fps: u32) -> Self {
        Self { width, height, fps, cell_size: DEFAULT_CELL_SIZE as u32 }
    }

    pub const fn with_cell(width: u32, height: u32, fps: u32, cell_size: u32) -> Self {
        Self { width, height, fps, cell_size }
    }

    pub const fn default_256x144_24() -> Self {
        Self { width: DEFAULT_FRAME_W, height: DEFAULT_FRAME_H, fps: DEFAULT_FPS, cell_size: DEFAULT_CELL_SIZE as u32 }
    }

    pub fn validate(&self) -> Result<(), String> {
        let cs = self.cell_size as usize;
        if cs < 2 { return Err(format!("cell_size {} too small (min 2)", cs)); }
        if self.width as usize % cs != 0 {
            return Err(format!("flicker width {} not divisible by cell size {}", self.width, cs));
        }
        if self.height as usize % cs != 0 {
            return Err(format!("flicker height {} not divisible by cell size {}", self.height, cs));
        }
        if self.fps == 0 { return Err("flicker fps cannot be 0".to_string()); }
        if (self.grid_cols() as usize) < 16 || (self.grid_rows() as usize) < 16 {
            return Err(format!("grid {}x{} too small (need >= 16x16 cells)", self.grid_cols(), self.grid_rows()));
        }
        Ok(())
    }

    #[inline] pub const fn w(&self) -> usize { self.width  as usize }
    #[inline] pub const fn h(&self) -> usize { self.height as usize }
    #[inline] pub const fn cs(&self) -> usize { self.cell_size as usize }
    #[inline] pub const fn grid_cols(&self) -> usize { self.w() / self.cs() }
    #[inline] pub const fn grid_rows(&self) -> usize { self.h() / self.cs() }
    #[inline] pub const fn total_cells(&self) -> usize { self.grid_cols() * self.grid_rows() }
    #[inline] pub const fn frame_bytes_rgb24(&self) -> usize { self.w() * self.h() * 3 }
    /// Central readable region offset from cell TL: 1/8 of cell_size (minimum 1).
    /// Smaller guard lets us average more samples per cell, fighting VK
    /// transcode noise that jitters individual pixel luma by ±15 levels.
    #[inline] pub const fn read_offset(&self) -> usize {
        let v = self.cs() / 8;
        if v == 0 { 1 } else { v }
    }
    /// Central readable region size: 3/4 of cell_size.
    #[inline] pub const fn read_size(&self) -> usize { self.cs() * 3 / 4 }
}

/// Convert (col, row) logical cell coords to top-left pixel (x, y).
#[inline]
pub fn cell_topleft(col: usize, row: usize, cell_size: usize) -> (usize, usize) {
    (col * cell_size, row * cell_size)
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
        assert_eq!(cell_topleft(0, 0, 4), (0, 0));
        assert_eq!(cell_topleft(63, 35, 4), (252, 140));
        assert_eq!(cell_topleft(5, 3, 16), (80, 48));
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
