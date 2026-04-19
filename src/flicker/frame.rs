//! Full encode/decode pipeline for one flicker frame.

use anyhow::{anyhow, Result};

use crate::flicker::codec::{paint_cell, read_cell};
use crate::flicker::fec::{decode_block, encode_block, RS_BLOCK_K, RS_BLOCK_N};
use crate::flicker::fragment::{Fragment, FRAGMENT_HEADER_BYTES};
use crate::flicker::grid::FlickerParams;
use crate::flicker::header::{decode_header, encode_header, FecScheme, FrameHeader, HEADER_TOTAL_BYTES};
use crate::flicker::interleave::{cell_index_to_col_row, col_row_to_cell_index, cell_permutation};
use crate::flicker::markers::{paint_markers, frame_offset, marker_centers, MARKER_SIZE};
use crate::flicker::pilot::{pilot_positions, validate_pilots, PILOT_COUNT};
use crate::flicker::ModulationMode;

pub const PILOT_CONFIDENCE_THRESHOLD: f32 = 0.5;
pub const PILOT_SUCCESS_MIN: f32 = 0.80;

/// Compute cell indices occupied by corner markers for the given params.
pub fn marker_cell_indices(p: &FlickerParams) -> Vec<usize> {
    let mut out = Vec::new();
    for (cx, cy) in marker_centers(p).iter() {
        let x0 = (*cx - MARKER_SIZE as i32 / 2) as usize;
        let y0 = (*cy - MARKER_SIZE as i32 / 2) as usize;
        for dy in 0..MARKER_SIZE / 4 {
            for dx in 0..MARKER_SIZE / 4 {
                let col = (x0 / 4) + dx;
                let row = (y0 / 4) + dy;
                out.push(col_row_to_cell_index(col, row, p.grid_cols()));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Number of RS(172,120) blocks that fit in the payload area of the given params
/// and modulation mode. Derived dynamically from available cells.
pub fn block_count_for(p: &FlickerParams, mode: ModulationMode) -> usize {
    let markers = marker_cell_indices(p).len();
    let header_cells = HEADER_TOTAL_BYTES * 4;
    let pilots = PILOT_COUNT;
    let payload_cells = p.total_cells().saturating_sub(markers + header_cells + pilots);
    let cells_per_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 2 };
    let payload_bytes_capacity = payload_cells / cells_per_byte;
    // Each RS codeword carries RS_BLOCK_N encoded bytes on the wire.
    payload_bytes_capacity / RS_BLOCK_N
}

pub struct FrameEncoder {
    pub params: FlickerParams,
    pub mode: ModulationMode,
    pub channel_id: u8,
    pub frame_counter: u32,
}

impl FrameEncoder {
    pub fn bits_per_payload_cell(&self) -> usize { self.mode.bits_per_cell() }

    pub fn block_count(&self) -> usize { block_count_for(&self.params, self.mode) }

    pub fn payload_bytes_per_frame(&self) -> usize { self.block_count() * RS_BLOCK_K }

    pub fn encode(&mut self, out_buf: &mut [u8], fragments: &[Fragment]) -> Result<()> {
        if out_buf.len() < self.params.frame_bytes_rgb24() {
            return Err(anyhow!("out buf too small"));
        }
        out_buf.fill(128);
        paint_markers(out_buf, &self.params);
        let markers = marker_cell_indices(&self.params);

        let total_capacity = self.payload_bytes_per_frame();
        if total_capacity == 0 {
            return Err(anyhow!("grid too small for any RS block (width={}, height={})", self.params.width, self.params.height));
        }
        let mut payload_bytes = Vec::with_capacity(total_capacity);
        for f in fragments {
            let mut hdr = [0u8; FRAGMENT_HEADER_BYTES];
            f.serialize_header(&mut hdr);
            payload_bytes.extend_from_slice(&hdr);
            payload_bytes.extend_from_slice(&f.payload);
        }
        let crc = crc32fast::hash(&payload_bytes);
        payload_bytes.extend_from_slice(&crc.to_le_bytes());
        let payload_len = payload_bytes.len();
        if payload_len > total_capacity {
            return Err(anyhow!("fragments too large: {} > {}", payload_len, total_capacity));
        }
        payload_bytes.resize(self.block_count() * RS_BLOCK_K, 0);

        let mut encoded_blocks: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
        for i in 0..self.block_count() {
            let chunk = &payload_bytes[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K];
            let encoded = encode_block(chunk)?;
            encoded_blocks.extend_from_slice(&encoded);
        }

        let header = FrameHeader {
            frame_counter: self.frame_counter,
            channel_id: self.channel_id,
            modulation_mode: self.mode,
            fec_scheme: FecScheme::RS_172_120,
            fec_params: [RS_BLOCK_N as u8, RS_BLOCK_K as u8, self.block_count() as u8, 0],
            payload_len: payload_len as u16,
        };
        let header_bytes = encode_header(&header)?;

        let header_perm = cell_permutation(&markers, self.params.total_cells(), self.params.grid_cols());
        let header_cells = HEADER_TOTAL_BYTES * 4;
        let header_cell_indices: Vec<usize> = header_perm[..header_cells]
            .iter()
            .map(|(c, r)| col_row_to_cell_index(*c, *r, self.params.grid_cols()))
            .collect();

        let mut pilot_excluded = markers.clone();
        pilot_excluded.extend(&header_cell_indices);
        pilot_excluded.sort_unstable();
        pilot_excluded.dedup();
        let pilot_list = pilot_positions(self.frame_counter, &pilot_excluded, &self.params);

        let mut payload_excluded = pilot_excluded.clone();
        payload_excluded.extend(&pilot_list);
        payload_excluded.sort_unstable();
        payload_excluded.dedup();
        let payload_perm = cell_permutation(&payload_excluded, self.params.total_cells(), self.params.grid_cols());

        for (i, &idx) in pilot_list.iter().enumerate() {
            let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
            let sym = crate::flicker::pilot::pilot_value(self.frame_counter, i);
            crate::flicker::codec::paint_cell_b(out_buf, col, row, sym, self.params.w());
        }

        for (byte_idx, &byte) in header_bytes.iter().enumerate() {
            for bit_pair in 0..4 {
                let symbol = (byte >> (2 * (3 - bit_pair))) & 0b11;
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                crate::flicker::codec::paint_cell_b(out_buf, col, row, symbol, self.params.w());
            }
        }

        let cells_per_byte = match self.mode { ModulationMode::B => 4, ModulationMode::C => 2 };
        for (byte_idx, &byte) in encoded_blocks.iter().enumerate() {
            for unit in 0..cells_per_byte {
                let symbol = match self.mode {
                    ModulationMode::B => (byte >> (2 * (3 - unit))) & 0b11,
                    ModulationMode::C => (byte >> (4 * (1 - unit))) & 0b1111,
                };
                let cell_pos = byte_idx * cells_per_byte + unit;
                if cell_pos >= payload_perm.len() { break; }
                let (col, row) = payload_perm[cell_pos];
                paint_cell(out_buf, col, row, symbol, self.mode, self.params.w());
            }
        }
        self.frame_counter = self.frame_counter.wrapping_add(1);
        Ok(())
    }
}

pub struct FrameDecoder {
    pub params: FlickerParams,
}

#[derive(Debug)]
pub enum DecodeOutcome {
    Ok { header: FrameHeader, fragments: Vec<Fragment>, pilot_success: f32 },
    Dropped { reason: DropReason },
}

#[derive(Debug)]
pub enum DropReason {
    SyncOffsetMissing,
    HeaderRsFailed,
    HeaderCrc,
    PilotValidationFailed(f32),
    BlockRsFailed(usize),
    PayloadCrcMismatch,
    FragmentParse,
}

impl FrameDecoder {
    pub fn decode(&self, buf: &[u8]) -> DecodeOutcome {
        let p = &self.params;
        let markers = marker_cell_indices(p);

        if frame_offset(buf, p).is_none() {
            return DecodeOutcome::Dropped { reason: DropReason::SyncOffsetMissing };
        }

        let header_perm = cell_permutation(&markers, p.total_cells(), p.grid_cols());
        let header_cells = HEADER_TOTAL_BYTES * 4;
        let mut header_shards: [Option<u8>; HEADER_TOTAL_BYTES] = [None; HEADER_TOTAL_BYTES];
        for byte_idx in 0..HEADER_TOTAL_BYTES {
            let mut byte = 0u8;
            let mut byte_confidence_min = 1.0f32;
            for bit_pair in 0..4 {
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                let (sym, conf) = read_cell(buf, col, row, ModulationMode::B, p.w());
                byte = (byte << 2) | (sym & 0b11);
                byte_confidence_min = byte_confidence_min.min(conf);
            }
            if byte_confidence_min >= PILOT_CONFIDENCE_THRESHOLD {
                header_shards[byte_idx] = Some(byte);
            }
        }
        let header = match decode_header(&header_shards) {
            Ok(h) => h,
            Err(_) => return DecodeOutcome::Dropped { reason: DropReason::HeaderRsFailed },
        };

        let header_cell_indices: Vec<usize> = header_perm[..header_cells]
            .iter()
            .map(|(c, r)| col_row_to_cell_index(*c, *r, p.grid_cols()))
            .collect();
        let mut pilot_excluded = markers.clone();
        pilot_excluded.extend(&header_cell_indices);
        pilot_excluded.sort_unstable();
        pilot_excluded.dedup();
        let pilot_list = pilot_positions(header.frame_counter, &pilot_excluded, p);
        let (pilot_ok, _pilot_conf) = validate_pilots(buf, header.frame_counter, &pilot_excluded, p);
        if pilot_ok < PILOT_SUCCESS_MIN {
            return DecodeOutcome::Dropped { reason: DropReason::PilotValidationFailed(pilot_ok) };
        }

        let mut payload_excluded = pilot_excluded.clone();
        payload_excluded.extend(&pilot_list);
        payload_excluded.sort_unstable();
        payload_excluded.dedup();
        let payload_perm = cell_permutation(&payload_excluded, p.total_cells(), p.grid_cols());

        let mode = header.modulation_mode;
        let block_count = header.fec_params[2] as usize;
        let cells_per_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 2 };

        let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);
        for block_i in 0..block_count {
            let mut shards: Vec<Option<u8>> = vec![None; RS_BLOCK_N];
            for byte_i in 0..RS_BLOCK_N {
                let mut byte = 0u8;
                let mut min_conf = 1.0f32;
                for unit in 0..cells_per_byte {
                    let cell_pos = (block_i * RS_BLOCK_N + byte_i) * cells_per_byte + unit;
                    if cell_pos >= payload_perm.len() { break; }
                    let (col, row) = payload_perm[cell_pos];
                    let (sym, conf) = read_cell(buf, col, row, mode, p.w());
                    let bits = match mode { ModulationMode::B => 2, ModulationMode::C => 4 };
                    byte = (byte << bits) | (sym & ((1 << bits) - 1));
                    min_conf = min_conf.min(conf);
                }
                if min_conf >= PILOT_CONFIDENCE_THRESHOLD {
                    shards[byte_i] = Some(byte);
                }
            }
            match decode_block(&shards) {
                Ok(d) => decoded_blocks.push(d),
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::BlockRsFailed(block_i) },
            }
        }

        let mut payload_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
        for b in &decoded_blocks { payload_bytes.extend_from_slice(b); }
        payload_bytes.truncate(header.payload_len as usize);

        if payload_bytes.len() < 4 {
            return DecodeOutcome::Dropped { reason: DropReason::PayloadCrcMismatch };
        }
        let crc_start = payload_bytes.len() - 4;
        let got_crc = u32::from_le_bytes([
            payload_bytes[crc_start], payload_bytes[crc_start + 1],
            payload_bytes[crc_start + 2], payload_bytes[crc_start + 3],
        ]);
        let expected_crc = crc32fast::hash(&payload_bytes[..crc_start]);
        if got_crc != expected_crc {
            return DecodeOutcome::Dropped { reason: DropReason::PayloadCrcMismatch };
        }
        payload_bytes.truncate(crc_start);

        let mut fragments = Vec::new();
        let mut cursor = 0usize;
        while cursor + FRAGMENT_HEADER_BYTES <= payload_bytes.len() {
            let (mut frag, used) = match Fragment::deserialize_header(&payload_bytes[cursor..]) {
                Ok(v) => v,
                Err(_) => return DecodeOutcome::Dropped { reason: DropReason::FragmentParse },
            };
            cursor += used;
            frag.payload = payload_bytes[cursor..].to_vec();
            fragments.push(frag);
            break;
        }

        DecodeOutcome::Ok { header, fragments, pilot_success: pilot_ok }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip_b_default() {
        let p = FlickerParams::default_256x144_24();
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        let mut enc = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 7 };
        let frag = Fragment {
            msg_type: 0x02, message_id: 1, fragment_idx: 0, fragment_total: 1,
            payload: b"hello world".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        let dec = FrameDecoder { params: p };
        match dec.decode(&buf) {
            DecodeOutcome::Ok { header, fragments, .. } => {
                assert_eq!(header.modulation_mode, ModulationMode::B);
                assert_eq!(fragments.len(), 1);
                assert!(fragments[0].payload.starts_with(b"hello world"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_b_at_360p() {
        let p = FlickerParams::new(640, 360, 24);
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        let bc = block_count_for(&p, ModulationMode::B);
        assert!(bc > 2, "360p should carry more blocks than 144p; got {bc}");
        let mut enc = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 7 };
        let frag = Fragment {
            msg_type: 0x02, message_id: 1, fragment_idx: 0, fragment_total: 1,
            payload: b"hello 360p world".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        let dec = FrameDecoder { params: p };
        match dec.decode(&buf) {
            DecodeOutcome::Ok { fragments, .. } => {
                assert!(fragments[0].payload.starts_with(b"hello 360p world"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
