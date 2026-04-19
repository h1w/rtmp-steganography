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

/// Maximum application bytes carried in a single flicker frame
/// (after FEC overhead, mode B baseline). Used by the tunnel adapter
/// to derive KCP MTU. Value is empirical from v2 framing.
pub const FLICKER_MAX_PAYLOAD_BYTES: usize = 512;

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
