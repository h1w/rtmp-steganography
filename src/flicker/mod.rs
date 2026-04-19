pub mod codec;
pub mod grid;
pub mod levels;

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
