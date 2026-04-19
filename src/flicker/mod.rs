pub mod codec;
pub mod fec;
pub mod fragment;
pub mod frame;
pub mod grid;
pub mod header;
pub mod interleave;
pub mod levels;
pub mod markers;
pub mod pilot;

/// Legacy fallback for the 256x144 mode B grid (block_count=2):
/// 2 * 120 − 9 (FRAGMENT_HEADER_BYTES) − 4 (payload CRC32) = 227.
///
/// DEPRECATED as a constant source of truth: the real per-frame
/// application-byte capacity is computed at runtime from `FlickerParams`
/// and `ModulationMode` via `frame::block_count_for`, and piped into the
/// tunnel through `FlickerChannel::new(.., max_payload)`. Kept here only
/// so pre-runtime callers and the baseline test still resolve.
pub const FLICKER_MAX_PAYLOAD_BYTES: usize = 227;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ModulationMode {
    B = 1,
    C = 2,
}

impl ModulationMode {
    pub fn bits_per_cell(self) -> usize {
        match self {
            ModulationMode::B => 2,
            ModulationMode::C => 4,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(ModulationMode::B),
            2 => Some(ModulationMode::C),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}
