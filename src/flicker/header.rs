//! Main frame header: 22 bytes data + 22 bytes RS(44,22) parity.
//! Corrects up to 11 byte erasures — twice the original 5.5-erasure budget.
//! The extra 11 bytes add 44 cells (~2%) to the overhead but header RS
//! failure dominated real-world VK transcoder drops at the original RS(33,22).

use anyhow::{anyhow, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::flicker::ModulationMode;

pub const SYNC_WORD: [u8; 4] = [0xF1, 0x1C, 0x4E, 0x52];
pub const PROTOCOL_VERSION: u8 = 0x02;
pub const HEADER_DATA_BYTES: usize = 22;
pub const HEADER_PARITY_BYTES: usize = 22;
pub const HEADER_TOTAL_BYTES: usize = 44;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FecScheme(pub u8);
impl FecScheme { pub const RS_172_120: Self = FecScheme(1); }

#[derive(Copy, Clone, Debug)]
pub struct FrameHeader {
    pub frame_counter: u32,
    pub channel_id: u8,
    pub modulation_mode: ModulationMode,
    pub fec_scheme: FecScheme,
    pub fec_params: [u8; 4], // (n, k, block_count, flags)
    pub payload_len: u16,
}

impl FrameHeader {
    pub fn serialize(&self) -> [u8; HEADER_DATA_BYTES] {
        let mut out = [0u8; HEADER_DATA_BYTES];
        out[0..4].copy_from_slice(&SYNC_WORD);
        out[4] = PROTOCOL_VERSION;
        out[5..9].copy_from_slice(&self.frame_counter.to_le_bytes());
        out[9] = self.channel_id;
        out[10] = self.modulation_mode as u8;
        out[11] = self.fec_scheme.0;
        out[12..16].copy_from_slice(&self.fec_params);
        out[16..18].copy_from_slice(&self.payload_len.to_le_bytes());
        let crc = crc32fast::hash(&out[..18]);
        out[18..22].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn deserialize(buf: &[u8; HEADER_DATA_BYTES]) -> Result<Self> {
        if buf[..4] != SYNC_WORD {
            return Err(anyhow!("sync word mismatch"));
        }
        if buf[4] != PROTOCOL_VERSION {
            return Err(anyhow!("unsupported version: {}", buf[4]));
        }
        let expected_crc = u32::from_le_bytes([buf[18], buf[19], buf[20], buf[21]]);
        let actual_crc = crc32fast::hash(&buf[..18]);
        if expected_crc != actual_crc {
            return Err(anyhow!("header CRC mismatch"));
        }
        let modulation_mode = ModulationMode::from_u8(buf[10])
            .ok_or_else(|| anyhow!("unknown modulation_mode {}", buf[10]))?;
        let mut fec_params = [0u8; 4];
        fec_params.copy_from_slice(&buf[12..16]);
        Ok(Self {
            frame_counter: u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]),
            channel_id: buf[9],
            modulation_mode,
            fec_scheme: FecScheme(buf[11]),
            fec_params,
            payload_len: u16::from_le_bytes([buf[16], buf[17]]),
        })
    }
}

/// Encode header with RS(33, 22): returns 33 bytes.
pub fn encode_header(header: &FrameHeader) -> Result<[u8; HEADER_TOTAL_BYTES]> {
    let data = header.serialize();
    let rs = ReedSolomon::new(HEADER_DATA_BYTES, HEADER_PARITY_BYTES)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    let mut shards: Vec<Vec<u8>> = data.iter().map(|b| vec![*b]).collect();
    for _ in 0..HEADER_PARITY_BYTES {
        shards.push(vec![0u8]);
    }
    rs.encode(&mut shards).map_err(|e| anyhow!("RS encode: {e}"))?;
    let mut out = [0u8; HEADER_TOTAL_BYTES];
    for (i, s) in shards.iter().enumerate() {
        out[i] = s[0];
    }
    Ok(out)
}

/// Decode 33 bytes (some may be erasures) into a FrameHeader.
pub fn decode_header(shards: &[Option<u8>; HEADER_TOTAL_BYTES]) -> Result<FrameHeader> {
    let rs = ReedSolomon::new(HEADER_DATA_BYTES, HEADER_PARITY_BYTES)
        .map_err(|e| anyhow!("RS init: {e}"))?;
    let mut mutable: Vec<Option<Vec<u8>>> = shards.iter().map(|o| o.map(|b| vec![b])).collect();
    rs.reconstruct(&mut mutable).map_err(|e| anyhow!("RS decode: {e}"))?;
    let mut data = [0u8; HEADER_DATA_BYTES];
    for i in 0..HEADER_DATA_BYTES {
        data[i] = mutable[i].as_ref().unwrap()[0];
    }
    FrameHeader::deserialize(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FrameHeader {
        FrameHeader {
            frame_counter: 0xDEADBEEF,
            channel_id: 1,
            modulation_mode: ModulationMode::B,
            fec_scheme: FecScheme::RS_172_120,
            fec_params: [172, 120, 2, 0],
            payload_len: 240,
        }
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let h = sample();
        let bytes = h.serialize();
        let back = FrameHeader::deserialize(&bytes).unwrap();
        assert_eq!(back.frame_counter, h.frame_counter);
        assert_eq!(back.channel_id, h.channel_id);
        assert_eq!(back.modulation_mode, h.modulation_mode);
        assert_eq!(back.payload_len, h.payload_len);
    }

    #[test]
    fn header_rs_corrects_11_byte_erasures() {
        let h = sample();
        let encoded = encode_header(&h).unwrap();
        let mut shards = [None; HEADER_TOTAL_BYTES];
        for i in 0..HEADER_TOTAL_BYTES {
            shards[i] = Some(encoded[i]);
        }
        // Erase 11 bytes — RS(44,22) capability.
        for &i in &[3, 7, 11, 15, 18, 22, 25, 29, 32, 36, 40] {
            shards[i] = None;
        }
        let back = decode_header(&shards).unwrap();
        assert_eq!(back.frame_counter, h.frame_counter);
    }

    #[test]
    fn header_rs_fails_above_capacity() {
        let h = sample();
        let encoded = encode_header(&h).unwrap();
        let mut shards = [None; HEADER_TOTAL_BYTES];
        for i in 0..HEADER_TOTAL_BYTES {
            shards[i] = Some(encoded[i]);
        }
        // 23 erasures — above the 22-byte parity budget.
        for i in 0..23 {
            shards[i] = None;
        }
        assert!(decode_header(&shards).is_err());
    }

    #[test]
    fn bad_sync_word_rejected() {
        let mut data = sample().serialize();
        data[0] = 0;
        assert!(FrameHeader::deserialize(&data).is_err());
    }
}
