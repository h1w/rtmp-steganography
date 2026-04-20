//! Split/join Mode C symbol streams into independent Y-lane and UV-lane
//! byte streams.
//!
//! Each Mode C cell carries:
//!   - 2 Y-bits (luma, 4 levels)
//!   - 1 U-bit (chroma, 2 levels)
//!   - 1 V-bit (chroma, 2 levels)
//!
//! Shared RS(172,120) over mixed Y+U+V bits means a single U/V error can
//! corrupt a byte whose Y-bits were perfect — chroma drift poisons luma.
//! This module packs 4 cells' Y-bits into one Y-byte and the same 4 cells'
//! UV-bits into one UV-byte, so the two lanes can be RS-encoded separately.
//!
//! Packing layout (4 cells c0..c3):
//!   Y-byte  = (y0 << 6) | (y1 << 4) | (y2 << 2) | y3
//!   UV-byte = (u0 << 7) | (v0 << 6) | (u1 << 5) | (v1 << 4)
//!           | (u2 << 3) | (v2 << 2) | (u3 << 1) |  v3

/// Number of cells consumed per Y-byte or per UV-byte.
pub const CELLS_PER_LANE_BYTE: usize = 4;

/// Pack a slice of cell symbols (each < 16) into one Y-byte + one UV-byte.
/// Caller must pass exactly `CELLS_PER_LANE_BYTE` symbols.
pub fn pack_lane_bytes(cells: &[u8; CELLS_PER_LANE_BYTE]) -> (u8, u8) {
    debug_assert!(cells.iter().all(|&s| s < 16));
    let mut y_byte = 0u8;
    let mut uv_byte = 0u8;
    for (i, &sym) in cells.iter().enumerate() {
        let y = (sym >> 2) & 0b11;
        let u = (sym >> 1) & 0b1;
        let v = sym & 0b1;
        y_byte |= y << (6 - 2 * i);
        uv_byte |= u << (7 - 2 * i);
        uv_byte |= v << (6 - 2 * i);
    }
    (y_byte, uv_byte)
}

/// Inverse of `pack_lane_bytes`: rebuild 4 cell symbols from Y-byte + UV-byte.
pub fn unpack_lane_bytes(y_byte: u8, uv_byte: u8) -> [u8; CELLS_PER_LANE_BYTE] {
    let mut out = [0u8; CELLS_PER_LANE_BYTE];
    for i in 0..CELLS_PER_LANE_BYTE {
        let y = (y_byte >> (6 - 2 * i)) & 0b11;
        let u = (uv_byte >> (7 - 2 * i)) & 0b1;
        let v = (uv_byte >> (6 - 2 * i)) & 0b1;
        out[i] = (y << 2) | (u << 1) | v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip_all_symbols() {
        for c0 in 0u8..16 {
            for c1 in 0u8..16 {
                for c2 in 0u8..16 {
                    for c3 in 0u8..16 {
                        let cells = [c0, c1, c2, c3];
                        let (y, uv) = pack_lane_bytes(&cells);
                        let back = unpack_lane_bytes(y, uv);
                        assert_eq!(back, cells, "failed at {:?}", cells);
                    }
                }
            }
        }
    }

    #[test]
    fn y_byte_isolates_luma_bits() {
        // c0 = Y=3 U=0 V=0 (sym = 12); c1..c3 = zero
        let (y, uv) = pack_lane_bytes(&[12, 0, 0, 0]);
        assert_eq!(y, 0b11_00_00_00, "Y-byte got {:#b}", y);
        assert_eq!(uv, 0, "UV-byte for pure-luma symbol must be 0, got {:#b}", uv);
    }

    #[test]
    fn uv_byte_isolates_chroma_bits() {
        // c0 = Y=0 U=1 V=1 (sym = 3); c1..c3 = zero
        let (y, uv) = pack_lane_bytes(&[3, 0, 0, 0]);
        assert_eq!(y, 0, "Y-byte for pure-chroma symbol must be 0, got {:#b}", y);
        assert_eq!(uv, 0b11_00_00_00, "UV-byte got {:#b}", uv);
    }
}
