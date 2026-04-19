use anyhow::{anyhow, Result};

#[derive(Clone, Debug)]
pub struct GridConfig {
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub cell: usize,
    pub cols: usize,
    pub rows: usize,
    pub total_cells: usize,
    pub update_every: u64,
}

impl GridConfig {
    pub fn new(
        width: usize,
        height: usize,
        fps: u32,
        cell: usize,
        update_every: u64,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(anyhow!("width/height must be > 0"));
        }
        if fps == 0 {
            return Err(anyhow!("fps must be >= 1"));
        }
        if cell == 0 {
            return Err(anyhow!("cell_size must be > 0"));
        }
        if width % cell != 0 || height % cell != 0 {
            return Err(anyhow!(
                "cell_size={cell} must divide width={width} and height={height} evenly"
            ));
        }
        if update_every == 0 {
            return Err(anyhow!("update_every_frames must be >= 1"));
        }
        let cols = width / cell;
        let rows = height / cell;
        Ok(Self {
            width,
            height,
            fps,
            cell,
            cols,
            rows,
            total_cells: cols * rows,
            update_every,
        })
    }

    pub fn frame_bytes(&self) -> usize {
        self.width * self.height * 3
    }
}
