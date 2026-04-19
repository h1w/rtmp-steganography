//! Four 16×16 corner markers for frame alignment.
//!
//! Pattern: alternating 2×2 checkerboard of extreme luma levels (32 and 224).
//! Decoder finds each marker via local cross-correlation, then fits an affine
//! transform mapping logical grid coords to actual pixel coords.

use crate::flicker::grid::{rgb24_offset, FRAME_BYTES_RGB24, FRAME_HEIGHT, FRAME_WIDTH};
use crate::flicker::levels::LEVELS_Y;

pub const MARKER_SIZE: usize = 16;
pub const MARKER_SEARCH_RADIUS: i32 = 8;

/// Nominal (ideal) centre of each corner marker.
pub const MARKER_CENTERS: [(i32, i32); 4] = [
    (MARKER_SIZE as i32 / 2, MARKER_SIZE as i32 / 2),
    (FRAME_WIDTH as i32 - MARKER_SIZE as i32 / 2, MARKER_SIZE as i32 / 2),
    (MARKER_SIZE as i32 / 2, FRAME_HEIGHT as i32 - MARKER_SIZE as i32 / 2),
    (FRAME_WIDTH as i32 - MARKER_SIZE as i32 / 2, FRAME_HEIGHT as i32 - MARKER_SIZE as i32 / 2),
];

/// Ideal marker pattern (16×16 grayscale).
pub fn marker_pattern() -> [[u8; MARKER_SIZE]; MARKER_SIZE] {
    let mut pat = [[0u8; MARKER_SIZE]; MARKER_SIZE];
    for y in 0..MARKER_SIZE {
        for x in 0..MARKER_SIZE {
            // 2×2 blocks of alternating low/high.
            let block_x = x / 2;
            let block_y = y / 2;
            pat[y][x] = if (block_x + block_y) % 2 == 0 {
                LEVELS_Y[0]
            } else {
                LEVELS_Y[3]
            };
        }
    }
    pat
}

/// Paint all 4 markers into a frame.
pub fn paint_markers(buf: &mut [u8]) {
    assert!(buf.len() >= FRAME_BYTES_RGB24);
    let pat = marker_pattern();
    for &(cx, cy) in MARKER_CENTERS.iter() {
        let x0 = (cx - MARKER_SIZE as i32 / 2).max(0) as usize;
        let y0 = (cy - MARKER_SIZE as i32 / 2).max(0) as usize;
        for dy in 0..MARKER_SIZE {
            for dx in 0..MARKER_SIZE {
                let x = x0 + dx;
                let y = y0 + dy;
                if x >= FRAME_WIDTH || y >= FRAME_HEIGHT {
                    continue;
                }
                let o = rgb24_offset(x, y);
                let v = pat[dy][dx];
                buf[o] = v;
                buf[o + 1] = v;
                buf[o + 2] = v;
            }
        }
    }
}

/// Locate one marker via local cross-correlation, returning offset (dx, dy)
/// from nominal centre. None if no peak above threshold.
pub fn locate_marker(buf: &[u8], nominal: (i32, i32)) -> Option<(i32, i32)> {
    let pat = marker_pattern();
    let mut best_score = f64::MIN;
    let mut best_off = (0i32, 0i32);
    for dy in -MARKER_SEARCH_RADIUS..=MARKER_SEARCH_RADIUS {
        for dx in -MARKER_SEARCH_RADIUS..=MARKER_SEARCH_RADIUS {
            let cx = nominal.0 + dx;
            let cy = nominal.1 + dy;
            let x0 = cx - MARKER_SIZE as i32 / 2;
            let y0 = cy - MARKER_SIZE as i32 / 2;
            if x0 < 0 || y0 < 0
                || x0 + MARKER_SIZE as i32 > FRAME_WIDTH as i32
                || y0 + MARKER_SIZE as i32 > FRAME_HEIGHT as i32
            {
                continue;
            }
            let score = correlate(buf, x0 as usize, y0 as usize, &pat);
            if score > best_score {
                best_score = score;
                best_off = (dx, dy);
            }
        }
    }
    // Sanity threshold: perfect match would yield ~(~200^2 * 256 area) = very large positive.
    if best_score > 0.0 {
        Some(best_off)
    } else {
        None
    }
}

fn correlate(buf: &[u8], x0: usize, y0: usize, pat: &[[u8; MARKER_SIZE]; MARKER_SIZE]) -> f64 {
    let mut sum = 0f64;
    for dy in 0..MARKER_SIZE {
        for dx in 0..MARKER_SIZE {
            let o = rgb24_offset(x0 + dx, y0 + dy);
            // Luma approx as R+G+B /3 (good enough for grayscale markers).
            let pixel = (buf[o] as f64 + buf[o + 1] as f64 + buf[o + 2] as f64) / 3.0;
            let ideal = pat[dy][dx] as f64;
            // Zero-mean correlation: (pixel - 128) * (ideal - 128).
            sum += (pixel - 128.0) * (ideal - 128.0);
        }
    }
    sum
}

/// Average offset over all 4 markers. Returns None if any marker missing.
pub fn frame_offset(buf: &[u8]) -> Option<(i32, i32)> {
    let mut sum = (0i32, 0i32);
    for &centre in MARKER_CENTERS.iter() {
        let off = locate_marker(buf, centre)?;
        sum.0 += off.0;
        sum.1 += off.1;
    }
    Some((sum.0 / 4, sum.1 / 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_markers_locate_at_zero_offset() {
        let mut buf = vec![128u8; FRAME_BYTES_RGB24];
        paint_markers(&mut buf);
        let off = frame_offset(&buf).expect("markers found");
        assert_eq!(off, (0, 0));
    }

    #[test]
    fn locate_marker_with_offset() {
        let mut buf = vec![128u8; FRAME_BYTES_RGB24];
        let pat = marker_pattern();

        // Paint marker at position (2-17, 1-16), center (10, 9), offset (+2, +1) from nominal (8, 8).
        for dy in 0..MARKER_SIZE {
            for dx in 0..MARKER_SIZE {
                let x = 2 + dx;
                let y = 1 + dy;
                let o = rgb24_offset(x, y);
                let v = pat[dy][dx];
                buf[o] = v;
                buf[o + 1] = v;
                buf[o + 2] = v;
            }
        }

        let off = locate_marker(&buf, (8, 8)).expect("marker found");
        assert_eq!(off, (2, 1), "expected offset (+2, +1), got {:?}", off);
    }
}
