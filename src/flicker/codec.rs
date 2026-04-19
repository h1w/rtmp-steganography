use crate::flicker::grid::GridConfig;

pub fn paint_bit_into_frame(buf: &mut [u8], cfg: &GridConfig, bit_idx: usize, bit: u8) {
    if bit == 0 {
        return;
    }
    let cx = bit_idx % cfg.cols;
    let cy = bit_idx / cfg.cols;
    let x0 = cx * cfg.cell;
    let y0 = cy * cfg.cell;
    for y in y0..y0 + cfg.cell {
        let row_start = (y * cfg.width + x0) * 3;
        let row_end = row_start + cfg.cell * 3;
        buf[row_start..row_end].fill(255);
    }
}

pub fn read_bit_from_cell(buf: &[u8], cfg: &GridConfig, bit_idx: usize) -> u8 {
    let cx = bit_idx % cfg.cols;
    let cy = bit_idx / cfg.cols;
    let x0 = cx * cfg.cell;
    let y0 = cy * cfg.cell;
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    for y in y0..y0 + cfg.cell {
        for x in x0..x0 + cfg.cell {
            let idx = (y * cfg.width + x) * 3;
            sum += buf[idx] as u64 + buf[idx + 1] as u64 + buf[idx + 2] as u64;
            count += 3;
        }
    }
    let mean = (sum / count.max(1)) as u8;
    if mean > 127 { 1 } else { 0 }
}
