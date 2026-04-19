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
/// to derive KCP MTU.
///
/// Derivation for mode B: block_count=2, RS_BLOCK_K=120, so per-frame
/// capacity = 2 * 120 = 240 bytes. From that subtract
/// FRAGMENT_HEADER_BYTES (9) and the payload-zone CRC32 trailer (4)
/// that the flicker encoder appends after fragment serialization,
/// leaving 227 bytes for a single application message per frame.
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
