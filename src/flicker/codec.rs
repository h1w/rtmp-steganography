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
