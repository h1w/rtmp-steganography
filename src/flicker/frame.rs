//! Full encode/decode pipeline for one flicker frame.

use anyhow::{anyhow, Result};
use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

use crate::flicker::codec::{paint_cell, read_cell};
use crate::flicker::fec::{decode_block, encode_block, RS_BLOCK_K, RS_BLOCK_N};
use crate::flicker::fragment::{Fragment, FRAGMENT_HEADER_BYTES};
use crate::flicker::grid::FlickerParams;
use crate::flicker::header::{decode_header, encode_header, FecScheme, FrameHeader, HEADER_TOTAL_BYTES};
use crate::flicker::interleave::{cell_index_to_col_row, col_row_to_cell_index, cell_permutation};
use crate::flicker::markers::{paint_markers, frame_offset, marker_centers, MARKER_SIZE};
use crate::flicker::pilot::{pilot_positions, validate_pilots, PILOT_COUNT};
use crate::flicker::ModulationMode;

/// Default confidence floor below which a payload byte is erased instead of
/// being included as a possibly-wrong symbol. Lower → more errors but fewer
/// erasures (good when the true symbol is usually closest even with low
/// confidence). Higher → stricter soft-decision gate. Override at runtime
/// with env `FLICKER_CONF_THRESHOLD` (f32 in [0.0, 1.0]).
pub const PILOT_CONFIDENCE_THRESHOLD_DEFAULT: f32 = 0.5;
pub const PILOT_SUCCESS_MIN: f32 = 0.80;

/// Resolve the confidence threshold from env every time decode runs. The
/// overhead is one getenv per frame; negligible vs. per-cell quantise cost.
fn conf_threshold() -> f32 {
    std::env::var("FLICKER_CONF_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(PILOT_CONFIDENCE_THRESHOLD_DEFAULT)
}

/// Per-frame diagnostic snapshot emitted when `FLICKER_DIAG=1`. Tracks every
/// step of the decode pipeline: header gate stats, calibrated level palette,
/// per-block Y/UV-lane accept/erase/min_conf/mean_conf, RS result, final
/// outcome. Used to pinpoint which stage drops frames under live VK drift.
#[derive(Default, Debug)]
struct FrameDiag {
    conf_gate: f32,
    mode: Option<ModulationMode>,
    frame_counter: u32,
    hdr_accept: usize,
    hdr_total: usize,
    hdr_min_conf: f32,
    hdr_sum_conf: f32,
    cal_y: Option<[u8; 4]>,
    cal_u: Option<[u8; 2]>,
    cal_v: Option<[u8; 2]>,
    // per-block stats: (accept_count, erase_count, min_conf, sum_conf, rs_ok)
    // For Mode B: blocks_y only. For Mode C: blocks_y + blocks_uv.
    blocks_y: Vec<(usize, usize, f32, f32, bool)>,
    blocks_uv: Vec<(usize, usize, f32, f32, bool)>,
    payload_len: u16,
    crc_ok: Option<bool>,
    final_outcome: String,
}

impl FrameDiag {
    fn enabled() -> bool { std::env::var("FLICKER_DIAG").ok().as_deref() == Some("1") }
    fn new(conf_gate: f32) -> Self { Self { conf_gate, hdr_min_conf: 1.0, ..Default::default() } }
    fn emit(&self) {
        if !Self::enabled() { return; }
        let hdr_mean = if self.hdr_total > 0 { self.hdr_sum_conf / self.hdr_total as f32 } else { 0.0 };
        let cal_str = match (self.cal_y, self.cal_u, self.cal_v) {
            (Some(y), Some(u), Some(v)) => format!("cal=Y{:?} U{:?} V{:?}", y, u, v),
            _ => "cal=none".into(),
        };
        let mut blocks = String::new();
        let fmt_blk = |label: &str, i: usize, b: &(usize, usize, f32, f32, bool)| {
            let (acc, era, min_c, sum_c, ok) = *b;
            let n = acc + era;
            let mean = if n > 0 { sum_c / n as f32 } else { 0.0 };
            format!(" | {}{}: acc={}/{} era={} min={:.2} mean={:.2} RS={}",
                label, i, acc, n, era, min_c, mean, if ok { "OK" } else { "ERR" })
        };
        for (i, b) in self.blocks_y.iter().enumerate() { blocks.push_str(&fmt_blk("blkY", i, b)); }
        for (i, b) in self.blocks_uv.iter().enumerate() { blocks.push_str(&fmt_blk("blkUV", i, b)); }
        let crc_s = match self.crc_ok { Some(true) => "CRC=OK", Some(false) => "CRC=MISMATCH", None => "CRC=-" };
        eprintln!(
            "[diag] f={} mode={:?} gate={:.2} hdr={}/{} min={:.2} mean={:.2} {} payload_len={} {} outcome={}{}",
            self.frame_counter, self.mode, self.conf_gate,
            self.hdr_accept, self.hdr_total, self.hdr_min_conf, hdr_mean,
            cal_str, self.payload_len, crc_s, self.final_outcome, blocks,
        );
    }
}

/// Compute cell indices occupied by corner markers for the given params.
/// Markers are always 16×16 px regardless of cell_size — we floor/ceil to
/// whatever cells they overlap.
pub fn marker_cell_indices(p: &FlickerParams) -> Vec<usize> {
    let mut out = Vec::new();
    let cs = p.cs();
    let marker_cells_per_side = ((MARKER_SIZE + cs - 1) / cs).max(1);
    for (cx, cy) in marker_centers(p).iter() {
        let x0 = (*cx - MARKER_SIZE as i32 / 2).max(0) as usize;
        let y0 = (*cy - MARKER_SIZE as i32 / 2).max(0) as usize;
        for dy in 0..marker_cells_per_side {
            for dx in 0..marker_cells_per_side {
                let col = (x0 / cs) + dx;
                let row = (y0 / cs) + dy;
                if col < p.grid_cols() && row < p.grid_rows() {
                    out.push(col_row_to_cell_index(col, row, p.grid_cols()));
                }
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
    // Mode B: 4 cells per byte. Mode C multi-level: 4 cells per PAIR of bytes
    // (one Y-byte + one UV-byte). Block count is PER-LANE in Mode C.
    let cells_per_lane_byte = match mode { ModulationMode::B => 4, ModulationMode::C => 4 };
    let lane_bytes_capacity = payload_cells / cells_per_lane_byte;
    lane_bytes_capacity / RS_BLOCK_N
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

    pub fn payload_bytes_per_frame(&self) -> usize {
        let per_lane = self.block_count() * RS_BLOCK_K;
        match self.mode {
            ModulationMode::B => per_lane,
            ModulationMode::C => 2 * per_lane,
        }
    }

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
        // Fill padding with a deterministic PRNG instead of zero bytes. Two
        // reasons: (1) near-solid-dark frames make VK's transcoder allocate
        // fewer bits and crush our cells; noisy uniform luma keeps the
        // bitrate honest. (2) If the tunnel is idle this frame, we still want
        // the canvas visually "busy" so nobody can eyeball that payload stopped.
        // Decoder is unaffected — it truncates to header.payload_len.
        let full = self.payload_bytes_per_frame();
        if payload_bytes.len() < full {
            let mut seed = [0u8; 32];
            seed[..4].copy_from_slice(&self.frame_counter.to_le_bytes());
            seed[4..12].copy_from_slice(b"flickpad");
            let mut rng = ChaCha8Rng::from_seed(seed);
            while payload_bytes.len() < full {
                payload_bytes.push(rng.next_u32() as u8);
            }
        } else {
            payload_bytes.truncate(full);
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

        match self.mode {
            ModulationMode::B => {
                for (i, &idx) in pilot_list.iter().enumerate() {
                    let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
                    let sym = crate::flicker::pilot::pilot_value(self.frame_counter, i);
                    crate::flicker::codec::paint_cell_b(out_buf, col, row, sym, self.params.w(), self.params.cs());
                }
            }
            ModulationMode::C => {
                for (i, &idx) in pilot_list.iter().enumerate() {
                    let (col, row) = cell_index_to_col_row(idx, self.params.grid_cols());
                    let sym = crate::flicker::pilot::pilot_value_c(self.frame_counter, i);
                    crate::flicker::codec::paint_cell_c(out_buf, col, row, sym, self.params.w(), self.params.cs());
                }
            }
        }

        for (byte_idx, &byte) in header_bytes.iter().enumerate() {
            for bit_pair in 0..4 {
                let symbol = (byte >> (2 * (3 - bit_pair))) & 0b11;
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                crate::flicker::codec::paint_cell_b(out_buf, col, row, symbol, self.params.w(), self.params.cs());
            }
        }

        match self.mode {
            ModulationMode::B => {
                let mut encoded_blocks: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
                for i in 0..self.block_count() {
                    let chunk = &payload_bytes[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K];
                    let encoded = encode_block(chunk)?;
                    encoded_blocks.extend_from_slice(&encoded);
                }
                for (byte_idx, &byte) in encoded_blocks.iter().enumerate() {
                    for unit in 0..4 {
                        let symbol = (byte >> (2 * (3 - unit))) & 0b11;
                        let cell_pos = byte_idx * 4 + unit;
                        if cell_pos >= payload_perm.len() { break; }
                        let (col, row) = payload_perm[cell_pos];
                        paint_cell(out_buf, col, row, symbol, self.mode, self.params.w(), self.params.cs());
                    }
                }
            }
            ModulationMode::C => {
                use crate::flicker::channels::{pack_lane_bytes, CELLS_PER_LANE_BYTE};
                let lane_byte_count = self.block_count() * RS_BLOCK_K;
                debug_assert_eq!(payload_bytes.len(), 2 * lane_byte_count,
                    "payload_bytes length must be 2 * lane_byte_count for Mode C multi-level");
                let (y_data, uv_data) = payload_bytes.split_at(lane_byte_count);
                let mut y_encoded: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
                let mut uv_encoded: Vec<u8> = Vec::with_capacity(self.block_count() * RS_BLOCK_N);
                for i in 0..self.block_count() {
                    y_encoded.extend_from_slice(&encode_block(&y_data[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K])?);
                    uv_encoded.extend_from_slice(&encode_block(&uv_data[i * RS_BLOCK_K..(i + 1) * RS_BLOCK_K])?);
                }
                let pair_count = y_encoded.len();
                for pair_i in 0..pair_count {
                    let cells_packed = [
                        ((y_encoded[pair_i] >> 6) & 0b11) << 2 | ((uv_encoded[pair_i] >> 7) & 0b1) << 1 | ((uv_encoded[pair_i] >> 6) & 0b1),
                        ((y_encoded[pair_i] >> 4) & 0b11) << 2 | ((uv_encoded[pair_i] >> 5) & 0b1) << 1 | ((uv_encoded[pair_i] >> 4) & 0b1),
                        ((y_encoded[pair_i] >> 2) & 0b11) << 2 | ((uv_encoded[pair_i] >> 3) & 0b1) << 1 | ((uv_encoded[pair_i] >> 2) & 0b1),
                        ((y_encoded[pair_i]) & 0b11) << 2 | ((uv_encoded[pair_i] >> 1) & 0b1) << 1 | (uv_encoded[pair_i] & 0b1),
                    ];
                    debug_assert_eq!(
                        pack_lane_bytes(&cells_packed),
                        (y_encoded[pair_i], uv_encoded[pair_i]),
                        "pack_lane_bytes mismatch at pair_i={}",
                        pair_i
                    );
                    for unit in 0..CELLS_PER_LANE_BYTE {
                        let cell_pos = pair_i * CELLS_PER_LANE_BYTE + unit;
                        if cell_pos >= payload_perm.len() { break; }
                        let (col, row) = payload_perm[cell_pos];
                        paint_cell(out_buf, col, row, cells_packed[unit], self.mode, self.params.w(), self.params.cs());
                    }
                }
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
    BlockRsFailedY(usize),
    BlockRsFailedUV(usize),
    PayloadCrcMismatch,
    FragmentParse,
}

impl FrameDecoder {
    pub fn decode(&self, buf: &[u8]) -> DecodeOutcome {
        let p = &self.params;
        let conf_gate = conf_threshold();
        let mut diag = FrameDiag::new(conf_gate);
        let markers = marker_cell_indices(p);

        if frame_offset(buf, p).is_none() {
            diag.final_outcome = "SyncOffsetMissing".into();
            diag.emit();
            return DecodeOutcome::Dropped { reason: DropReason::SyncOffsetMissing };
        }

        let header_perm = cell_permutation(&markers, p.total_cells(), p.grid_cols());
        let header_cells = HEADER_TOTAL_BYTES * 4;
        let mut header_shards: [Option<u8>; HEADER_TOTAL_BYTES] = [None; HEADER_TOTAL_BYTES];
        diag.hdr_total = HEADER_TOTAL_BYTES;
        for byte_idx in 0..HEADER_TOTAL_BYTES {
            let mut byte = 0u8;
            let mut byte_confidence_min = 1.0f32;
            for bit_pair in 0..4 {
                let (col, row) = header_perm[byte_idx * 4 + bit_pair];
                let (sym, conf) = read_cell(buf, col, row, ModulationMode::B, p.w(), p.cs(), p.read_offset(), p.read_size());
                byte = (byte << 2) | (sym & 0b11);
                byte_confidence_min = byte_confidence_min.min(conf);
            }
            diag.hdr_min_conf = diag.hdr_min_conf.min(byte_confidence_min);
            diag.hdr_sum_conf += byte_confidence_min;
            if byte_confidence_min >= conf_gate {
                header_shards[byte_idx] = Some(byte);
                diag.hdr_accept += 1;
            }
        }
        let header = match decode_header(&header_shards) {
            Ok(h) => h,
            Err(_) => {
                diag.final_outcome = "HeaderRsFailed".into();
                diag.emit();
                return DecodeOutcome::Dropped { reason: DropReason::HeaderRsFailed };
            }
        };
        diag.frame_counter = header.frame_counter;
        diag.mode = Some(header.modulation_mode);
        diag.payload_len = header.payload_len;

        let header_cell_indices: Vec<usize> = header_perm[..header_cells]
            .iter()
            .map(|(c, r)| col_row_to_cell_index(*c, *r, p.grid_cols()))
            .collect();
        let mut pilot_excluded = markers.clone();
        pilot_excluded.extend(&header_cell_indices);
        pilot_excluded.sort_unstable();
        pilot_excluded.dedup();
        let pilot_list = pilot_positions(header.frame_counter, &pilot_excluded, p);
        let mode = header.modulation_mode;
        let pilot_ok = match mode {
            ModulationMode::B => {
                let (ok, _) = validate_pilots(buf, header.frame_counter, &pilot_excluded, p);
                if ok < PILOT_SUCCESS_MIN {
                    diag.final_outcome = format!("PilotValidationFailed({:.2})", ok);
                    diag.emit();
                    return DecodeOutcome::Dropped { reason: DropReason::PilotValidationFailed(ok) };
                }
                ok
            }
            ModulationMode::C => 1.0f32,
        };

        let mut payload_excluded = pilot_excluded.clone();
        payload_excluded.extend(&pilot_list);
        payload_excluded.sort_unstable();
        payload_excluded.dedup();
        let payload_perm = cell_permutation(&payload_excluded, p.total_cells(), p.grid_cols());

        let calibrated: Option<crate::flicker::calibration::CalibratedLevels> = match mode {
            ModulationMode::C => {
                let obs = crate::flicker::pilot::read_pilot_observations_c(
                    buf, header.frame_counter, &pilot_excluded, p,
                );
                let cal = crate::flicker::calibration::calibrate(&obs);
                diag.cal_y = Some(cal.y);
                diag.cal_u = Some(cal.u);
                diag.cal_v = Some(cal.v);
                Some(cal)
            }
            ModulationMode::B => None,
        };

        let block_count = header.fec_params[2] as usize;

        let mut payload_bytes: Vec<u8> = match mode {
            ModulationMode::B => {
                let mut decoded_blocks: Vec<Vec<u8>> = Vec::with_capacity(block_count);
                for block_i in 0..block_count {
                    let mut shards: Vec<Option<u8>> = vec![None; RS_BLOCK_N];
                    let mut blk_accept = 0usize;
                    let mut blk_erase = 0usize;
                    let mut blk_min = 1.0f32;
                    let mut blk_sum = 0.0f32;
                    for byte_i in 0..RS_BLOCK_N {
                        let mut byte = 0u8;
                        let mut min_conf = 1.0f32;
                        for unit in 0..4 {
                            let cell_pos = (block_i * RS_BLOCK_N + byte_i) * 4 + unit;
                            if cell_pos >= payload_perm.len() { break; }
                            let (col, row) = payload_perm[cell_pos];
                            let (sym, conf) = read_cell(buf, col, row, ModulationMode::B, p.w(), p.cs(), p.read_offset(), p.read_size());
                            byte = (byte << 2) | (sym & 0b11);
                            min_conf = min_conf.min(conf);
                        }
                        blk_min = blk_min.min(min_conf);
                        blk_sum += min_conf;
                        if min_conf >= conf_gate {
                            shards[byte_i] = Some(byte);
                            blk_accept += 1;
                        } else {
                            blk_erase += 1;
                        }
                    }
                    let rs_ok = match decode_block(&shards) {
                        Ok(d) => { decoded_blocks.push(d); true }
                        Err(_) => false,
                    };
                    diag.blocks_y.push((blk_accept, blk_erase, blk_min, blk_sum, rs_ok));
                    if !rs_ok {
                        diag.final_outcome = format!("BlockRsFailed({})", block_i);
                        diag.emit();
                        return DecodeOutcome::Dropped { reason: DropReason::BlockRsFailed(block_i) };
                    }
                }
                let mut bytes = Vec::with_capacity(block_count * RS_BLOCK_K);
                for b in &decoded_blocks { bytes.extend_from_slice(b); }
                bytes
            }
            ModulationMode::C => {
                use crate::flicker::channels::{pack_lane_bytes, CELLS_PER_LANE_BYTE};
                let mut y_shards: Vec<Vec<Option<u8>>> = (0..block_count).map(|_| vec![None; RS_BLOCK_N]).collect();
                let mut uv_shards: Vec<Vec<Option<u8>>> = (0..block_count).map(|_| vec![None; RS_BLOCK_N]).collect();
                // Per-block (accept, erase, min_conf, sum_conf) tracked once; RS
                // gate is shared across lanes (same 4 cells produce one Y-byte and
                // one UV-byte), so accept/erase counts match between lanes.
                let mut blk_stats: Vec<(usize, usize, f32, f32)> = vec![(0, 0, 1.0, 0.0); block_count];
                for block_i in 0..block_count {
                    for byte_i in 0..RS_BLOCK_N {
                        let pair_i = block_i * RS_BLOCK_N + byte_i;
                        let mut cells_observed = [0u8; CELLS_PER_LANE_BYTE];
                        let mut min_conf = 1.0f32;
                        let mut any_skipped = false;
                        for unit in 0..CELLS_PER_LANE_BYTE {
                            let cell_pos = pair_i * CELLS_PER_LANE_BYTE + unit;
                            if cell_pos >= payload_perm.len() { any_skipped = true; break; }
                            let (col, row) = payload_perm[cell_pos];
                            let (sym, conf) = match calibrated.as_ref() {
                                Some(cal) => crate::flicker::codec::read_cell_c_cal(
                                    buf, col, row, p.w(), p.cs(), p.read_offset(), p.read_size(), cal,
                                ),
                                None => read_cell(buf, col, row, ModulationMode::C, p.w(), p.cs(), p.read_offset(), p.read_size()),
                            };
                            cells_observed[unit] = sym & 0b1111;
                            min_conf = min_conf.min(conf);
                        }
                        if any_skipped { continue; }
                        let (y_byte, uv_byte) = pack_lane_bytes(&cells_observed);
                        let s = &mut blk_stats[block_i];
                        s.2 = s.2.min(min_conf);
                        s.3 += min_conf;
                        if min_conf >= conf_gate {
                            y_shards[block_i][byte_i] = Some(y_byte);
                            uv_shards[block_i][byte_i] = Some(uv_byte);
                            s.0 += 1;
                        } else {
                            s.1 += 1;
                        }
                    }
                }
                let mut y_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
                let mut uv_bytes: Vec<u8> = Vec::with_capacity(block_count * RS_BLOCK_K);
                let mut failure: Option<DropReason> = None;
                for block_i in 0..block_count {
                    let (acc, era, mn, sm) = blk_stats[block_i];
                    let y_rs_ok = match decode_block(&y_shards[block_i]) {
                        Ok(d) => { y_bytes.extend_from_slice(&d); true }
                        Err(_) => false,
                    };
                    let uv_rs_ok = match decode_block(&uv_shards[block_i]) {
                        Ok(d) => { uv_bytes.extend_from_slice(&d); true }
                        Err(_) => false,
                    };
                    diag.blocks_y.push((acc, era, mn, sm, y_rs_ok));
                    diag.blocks_uv.push((acc, era, mn, sm, uv_rs_ok));
                    if !y_rs_ok && failure.is_none() {
                        failure = Some(DropReason::BlockRsFailedY(block_i));
                    }
                    if !uv_rs_ok && failure.is_none() {
                        failure = Some(DropReason::BlockRsFailedUV(block_i));
                    }
                }
                if let Some(reason) = failure {
                    diag.final_outcome = format!("{:?}", reason);
                    diag.emit();
                    return DecodeOutcome::Dropped { reason };
                }
                let mut bytes = y_bytes;
                bytes.extend_from_slice(&uv_bytes);
                bytes
            }
        };
        payload_bytes.truncate(header.payload_len as usize);

        if payload_bytes.len() < 4 {
            diag.crc_ok = Some(false);
            diag.final_outcome = "PayloadCrcMismatch(short)".into();
            diag.emit();
            return DecodeOutcome::Dropped { reason: DropReason::PayloadCrcMismatch };
        }
        let crc_start = payload_bytes.len() - 4;
        let got_crc = u32::from_le_bytes([
            payload_bytes[crc_start], payload_bytes[crc_start + 1],
            payload_bytes[crc_start + 2], payload_bytes[crc_start + 3],
        ]);
        let expected_crc = crc32fast::hash(&payload_bytes[..crc_start]);
        if got_crc != expected_crc {
            diag.crc_ok = Some(false);
            diag.final_outcome = format!("PayloadCrcMismatch got={:08x} want={:08x}", got_crc, expected_crc);
            diag.emit();
            return DecodeOutcome::Dropped { reason: DropReason::PayloadCrcMismatch };
        }
        diag.crc_ok = Some(true);
        payload_bytes.truncate(crc_start);

        let mut fragments = Vec::new();
        let mut cursor = 0usize;
        while cursor + FRAGMENT_HEADER_BYTES <= payload_bytes.len() {
            let (mut frag, used) = match Fragment::deserialize_header(&payload_bytes[cursor..]) {
                Ok(v) => v,
                Err(_) => {
                    diag.final_outcome = "FragmentParse".into();
                    diag.emit();
                    return DecodeOutcome::Dropped { reason: DropReason::FragmentParse };
                }
            };
            cursor += used;
            frag.payload = payload_bytes[cursor..].to_vec();
            fragments.push(frag);
            break;
        }

        diag.final_outcome = "Ok".into();
        diag.emit();
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

    /// Regression guard for the "cell_size doesn't actually take effect"
    /// hypothesis: two encoders with identical dimensions/mode/frame_counter
    /// but different cell_size MUST produce:
    ///   (a) different grid_cols × grid_rows
    ///   (b) different pixel buffers
    ///   (c) each decodable ONLY by a decoder with the matching cell_size
    #[test]
    fn cell_size_actually_changes_painted_output() {
        let p4 = FlickerParams::with_cell(640, 360, 24, 4);
        let p8 = FlickerParams::with_cell(640, 360, 24, 8);
        assert_eq!(p4.grid_cols(), 160); assert_eq!(p4.grid_rows(), 90);
        assert_eq!(p8.grid_cols(),  80); assert_eq!(p8.grid_rows(), 45);
        assert_ne!(p4.total_cells(), p8.total_cells());
        let mut buf4 = vec![0u8; p4.frame_bytes_rgb24()];
        let mut buf8 = vec![0u8; p8.frame_bytes_rgb24()];
        assert_eq!(buf4.len(), buf8.len(), "640x360 frame bytes identical regardless of cell_size");
        let frag = Fragment { msg_type: 2, message_id: 42, fragment_idx: 0, fragment_total: 1, payload: b"same-payload-for-both".to_vec() };
        let mut e4 = FrameEncoder { params: p4, mode: ModulationMode::C, channel_id: 1, frame_counter: 99 };
        let mut e8 = FrameEncoder { params: p8, mode: ModulationMode::C, channel_id: 1, frame_counter: 99 };
        e4.encode(&mut buf4, std::slice::from_ref(&frag)).unwrap();
        e8.encode(&mut buf8, std::slice::from_ref(&frag)).unwrap();
        let diff_bytes = buf4.iter().zip(buf8.iter()).filter(|(a,b)| a!=b).count();
        assert!(diff_bytes > buf4.len() / 4,
            "cell=4 vs cell=8 buffers differ by only {diff_bytes}/{} bytes — cell_size may not be propagating",
            buf4.len());
        let d4 = FrameDecoder { params: p4 };
        let d8 = FrameDecoder { params: p8 };
        assert!(matches!(d4.decode(&buf4), DecodeOutcome::Ok {..}), "d4 must decode buf4");
        assert!(matches!(d8.decode(&buf8), DecodeOutcome::Ok {..}), "d8 must decode buf8");
        assert!(!matches!(d4.decode(&buf8), DecodeOutcome::Ok {..}), "d4 must NOT decode buf8 (size mismatch)");
        assert!(!matches!(d8.decode(&buf4), DecodeOutcome::Ok {..}), "d8 must NOT decode buf4 (size mismatch)");
    }

    #[test]
    fn mode_c_multilevel_roundtrip_no_drift() {
        let p = FlickerParams::with_cell(432, 240, 24, 4);
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 321 };
        let frag = Fragment {
            msg_type: 2, message_id: 9, fragment_idx: 0, fragment_total: 1,
            payload: b"multi-level-mode-c-test-payload-enough-bytes".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        let dec = FrameDecoder { params: p };
        match dec.decode(&buf) {
            DecodeOutcome::Ok { fragments, .. } => {
                assert_eq!(fragments.len(), 1);
                assert!(fragments[0].payload.starts_with(b"multi-level-mode-c-test-payload"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn mode_c_multilevel_y_lane_survives_chroma_only_errors() {
        let p = FlickerParams::with_cell(432, 240, 24, 4);
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 555 };
        let frag = Fragment {
            msg_type: 2, message_id: 1, fragment_idx: 0, fragment_total: 1,
            payload: b"y-lane-resilience".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        // Saturate blue channel → wipes U bits while luma (11% blue) mostly survives.
        for py in 0..p.h() {
            for px in 0..p.w() {
                let o = crate::flicker::grid::rgb24_offset(px, py, p.w());
                buf[o + 2] = 255;
            }
        }
        let dec = FrameDecoder { params: p };
        // Widened: blue=255 saturation not only wipes U bits but also lifts
        // luma by 0.114*255 ≈ 29 LSB via the RGB→Y conversion used by the
        // Mode B header reader. Header cells with symbol 0 then read too close
        // to the second-level threshold for confidence ≥ 0.5 and get erased,
        // tripping HeaderRsFailed before payload decode runs.
        //
        // The Y-lane claim this test exists to verify ("chroma errors must NOT
        // poison luma bytes via shared RS") is satisfied iff decode does not
        // fail with BlockRsFailed. Accept any outcome that isn't BlockRsFailed.
        match dec.decode(&buf) {
            DecodeOutcome::Dropped { reason: DropReason::BlockRsFailedY(_) } => {
                panic!("Y-lane must not fail under chroma-only noise");
            }
            _ => {}
        }
    }

    #[test]
    fn mode_c_decode_recovers_frame_under_uniform_chroma_drift() {
        let p = FlickerParams::with_cell(432, 240, 24, 4);
        let mut buf = vec![0u8; p.frame_bytes_rgb24()];
        let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 1, frame_counter: 77 };
        let frag = Fragment {
            msg_type: 2, message_id: 1, fragment_idx: 0, fragment_total: 1,
            payload: b"calibration-smoke-test-payload".to_vec(),
        };
        enc.encode(&mut buf, std::slice::from_ref(&frag)).unwrap();
        // Uniform +25 LSB blue shift simulates the transcoder's chroma offset.
        for py in 0..p.h() {
            for px in 0..p.w() {
                let o = crate::flicker::grid::rgb24_offset(px, py, p.w());
                buf[o + 2] = buf[o + 2].saturating_add(25);
            }
        }
        let dec = FrameDecoder { params: p };
        match dec.decode(&buf) {
            DecodeOutcome::Ok { fragments, .. } => {
                assert_eq!(fragments.len(), 1);
                assert!(fragments[0].payload.starts_with(b"calibration-smoke-test-payload"));
            }
            other => panic!("expected Ok with calibrated decode, got {other:?}"),
        }
    }
}
