use anyhow::{anyhow, Result};

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 144;
pub const FPS: u32 = 30;
pub const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;

#[derive(Clone, Debug)]
pub struct GridConfig {
    pub cell: usize,
    pub cols: usize,
    pub rows: usize,
    pub total_cells: usize,
    pub update_every: u64,
}

impl GridConfig {
    pub fn new(cell: usize, update_every: u64) -> Result<Self> {
        if cell == 0 {
            return Err(anyhow!("cell_size must be > 0"));
        }
        if WIDTH % cell != 0 || HEIGHT % cell != 0 {
            return Err(anyhow!(
                "cell_size={cell} must divide both WIDTH={WIDTH} and HEIGHT={HEIGHT} evenly \
                 (valid values: 1, 2, 4, 8, 16)"
            ));
        }
        if update_every == 0 {
            return Err(anyhow!("update_every_frames must be >= 1"));
        }
        let cols = WIDTH / cell;
        let rows = HEIGHT / cell;
        Ok(Self {
            cell,
            cols,
            rows,
            total_cells: cols * rows,
            update_every,
        })
    }
}
