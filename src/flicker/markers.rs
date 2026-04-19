//! Four 16×16 corner markers for frame alignment.
//!
//! Coordinates and buffer stride derive from runtime `FlickerParams`; the
//! markers are always 16×16 pixels regardless of frame size.

use crate::flicker::grid::{rgb24_offset, FlickerParams};
use crate::flicker::levels::LEVELS_Y;

pub const MARKER_SIZE: usize = 16;
pub const MARKER_SEARCH_RADIUS: i32 = 8;

/// Nominal (ideal) centre of each corner marker for the given frame size.
pub fn marker_centers(p: &FlickerParams) -> [(i32, i32); 4] {
    let w = p.w() as i32;
    let h = p.h() as i32;
    let hm = MARKER_SIZE as i32 / 2;
    [
        (hm, hm),
        (w - hm, hm),
        (hm, h - hm),
        (w - hm, h - hm),
    ]
}

pub fn marker_pattern() -> [[u8; MARKER_SIZE]; MARKER_SIZE] {
    let mut pat = [[0u8; MARKER_SIZE]; MARKER_SIZE];
    for y in 0..MARKER_SIZE {
        for x in 0..MARKER_SIZE {
            let block_x = x / 2;
            let block_y = y / 2;
            pat[y][x] = if (block_x + block_y) % 2 == 0 { LEVELS_Y[0] } else { LEVELS_Y[3] };
        }
    }
    pat
}

pub fn paint_markers(buf: &mut [u8], p: &FlickerParams) {
    assert!(buf.len() >= p.frame_bytes_rgb24());
    let pat = marker_pattern();
    let width = p.w();
    let height = p.h();
    for &(cx, cy) in marker_centers(p).iter() {
        let x0 = (cx - MARKER_SIZE as i32 / 2).max(0) as usize;
        let y0 = (cy - MARKER_SIZE as i32 / 2).max(0) as usize;
        for dy in 0..MARKER_SIZE {
            for dx in 0..MARKER_SIZE {
                let x = x0 + dx;
                let y = y0 + dy;
                if x >= width || y >= height { continue; }
                let o = rgb24_offset(x, y, width);
                let v = pat[dy][dx];
                buf[o] = v;
                buf[o + 1] = v;
                buf[o + 2] = v;
            }
        }
    }
}

pub fn locate_marker(buf: &[u8], nominal: (i32, i32), p: &FlickerParams) -> Option<(i32, i32)> {
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
                || x0 + MARKER_SIZE as i32 > p.w() as i32
                || y0 + MARKER_SIZE as i32 > p.h() as i32 { continue; }
            let score = correlate(buf, x0 as usize, y0 as usize, &pat, p.w());
            if score > best_score {
                best_score = score;
                best_off = (dx, dy);
            }
        }
    }
    if best_score > 0.0 { Some(best_off) } else { None }
}

fn correlate(buf: &[u8], x0: usize, y0: usize, pat: &[[u8; MARKER_SIZE]; MARKER_SIZE], width: usize) -> f64 {
    let mut sum = 0f64;
    for dy in 0..MARKER_SIZE {
        for dx in 0..MARKER_SIZE {
            let o = rgb24_offset(x0 + dx, y0 + dy, width);
            let pixel = (buf[o] as f64 + buf[o + 1] as f64 + buf[o + 2] as f64) / 3.0;
            let ideal = pat[dy][dx] as f64;
            sum += (pixel - 128.0) * (ideal - 128.0);
        }
    }
    sum
}

pub fn frame_offset(buf: &[u8], p: &FlickerParams) -> Option<(i32, i32)> {
    let mut sum = (0i32, 0i32);
    for &centre in marker_centers(p).iter() {
        let off = locate_marker(buf, centre, p)?;
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
        let p = FlickerParams::default_256x144_24();
        let mut buf = vec![128u8; p.frame_bytes_rgb24()];
        paint_markers(&mut buf, &p);
        let off = frame_offset(&buf, &p).expect("markers found");
        assert_eq!(off, (0, 0));
    }

    #[test]
    fn painted_markers_locate_at_360p() {
        let p = FlickerParams::new(640, 360, 24);
        let mut buf = vec![128u8; p.frame_bytes_rgb24()];
        paint_markers(&mut buf, &p);
        let off = frame_offset(&buf, &p).expect("markers found");
        assert_eq!(off, (0, 0));
    }
}
