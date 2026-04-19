use crate::flicker::codec::{paint_bit_into_frame, read_bit_from_cell};
use crate::flicker::grid::GridConfig;

pub fn encode_timestamp_frame(buf: &mut [u8], ts_ns: u64, cfg: &GridConfig) {
    assert!(buf.len() >= cfg.frame_bytes());
    buf.fill(0);
    let ts_bits = cfg.total_cells.min(64);
    for bit_idx in 0..ts_bits {
        let bit = ((ts_ns >> (ts_bits - 1 - bit_idx)) & 1) as u8;
        paint_bit_into_frame(buf, cfg, bit_idx, bit);
    }
}

pub fn decode_timestamp_frame(buf: &[u8], cfg: &GridConfig) -> u64 {
    assert!(buf.len() >= cfg.frame_bytes());
    let ts_bits = cfg.total_cells.min(64);
    let mut ts = 0u64;
    for bit_idx in 0..ts_bits {
        let bit = read_bit_from_cell(buf, cfg, bit_idx);
        ts |= (bit as u64) << (ts_bits - 1 - bit_idx);
    }
    ts
}
