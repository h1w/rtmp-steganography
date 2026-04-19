//! Fragment header (9 bytes) + reassembly buffer with TTL.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::flicker::InboundMessage;

pub const FRAGMENT_HEADER_BYTES: usize = 9;
pub const DEFAULT_REASSEMBLY_TIMEOUT_MS: u64 = 2000;
pub const MSG_TYPE_HAS_NEXT_BIT: u8 = 0x80;
pub const MSG_TYPE_MASK: u8 = 0x7F;

#[derive(Debug, Clone)]
pub struct Fragment {
    pub msg_type: u8,      // low 7 bits; high bit = HAS_NEXT
    pub message_id: u32,
    pub fragment_idx: u16,
    pub fragment_total: u16,
    pub payload: Vec<u8>,
}

impl Fragment {
    pub fn serialize_header(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= FRAGMENT_HEADER_BYTES);
        buf[0] = self.msg_type;
        buf[1..5].copy_from_slice(&self.message_id.to_le_bytes());
        buf[5..7].copy_from_slice(&self.fragment_idx.to_le_bytes());
        buf[7..9].copy_from_slice(&self.fragment_total.to_le_bytes());
    }

    pub fn deserialize_header(buf: &[u8]) -> Result<(Self, usize)> {
        if buf.len() < FRAGMENT_HEADER_BYTES {
            return Err(anyhow!("fragment header underflow"));
        }
        let msg_type = buf[0];
        let message_id = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let fragment_idx = u16::from_le_bytes([buf[5], buf[6]]);
        let fragment_total = u16::from_le_bytes([buf[7], buf[8]]);
        if fragment_total == 0 {
            return Err(anyhow!("fragment_total = 0 invalid"));
        }
        if fragment_idx >= fragment_total {
            return Err(anyhow!("fragment_idx {} >= total {}", fragment_idx, fragment_total));
        }
        Ok((
            Fragment { msg_type, message_id, fragment_idx, fragment_total, payload: Vec::new() },
            FRAGMENT_HEADER_BYTES,
        ))
    }

    pub fn has_next_in_frame(&self) -> bool {
        self.msg_type & MSG_TYPE_HAS_NEXT_BIT != 0
    }

    pub fn app_msg_type(&self) -> u8 {
        self.msg_type & MSG_TYPE_MASK
    }
}

struct PartialMessage {
    received_at: Instant,
    total: u16,
    parts: Vec<Option<Vec<u8>>>,
    msg_type: u8,
}

pub struct Reassembler {
    partials: HashMap<u32, PartialMessage>,
    timeout: Duration,
}

impl Reassembler {
    pub fn new(timeout_ms: u64) -> Self {
        Self { partials: HashMap::new(), timeout: Duration::from_millis(timeout_ms) }
    }

    /// Accept a fragment. Returns Some(InboundMessage) when the message is complete.
    pub fn accept(&mut self, frag: Fragment) -> Option<InboundMessage> {
        self.gc();
        if frag.fragment_total == 1 {
            return Some(InboundMessage { msg_type: frag.app_msg_type(), payload: frag.payload });
        }
        let partial = self.partials.entry(frag.message_id).or_insert_with(|| PartialMessage {
            received_at: Instant::now(),
            total: frag.fragment_total,
            parts: vec![None; frag.fragment_total as usize],
            msg_type: frag.app_msg_type(),
        });
        if partial.total != frag.fragment_total {
            // Sender mid-stream mismatch: drop partial.
            self.partials.remove(&frag.message_id);
            return None;
        }
        let idx = frag.fragment_idx as usize;
        if partial.parts[idx].is_none() {
            partial.parts[idx] = Some(frag.payload);
        }
        // Check if complete.
        if partial.parts.iter().all(Option::is_some) {
            let completed = self.partials.remove(&frag.message_id).unwrap();
            let mut payload: Vec<u8> = Vec::new();
            for p in completed.parts {
                payload.extend_from_slice(&p.unwrap());
            }
            return Some(InboundMessage { msg_type: completed.msg_type, payload });
        }
        None
    }

    fn gc(&mut self) {
        let now = Instant::now();
        self.partials.retain(|_, p| now.duration_since(p.received_at) < self.timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fragment_completes_immediately() {
        let mut r = Reassembler::new(2000);
        let f = Fragment {
            msg_type: 0x02,
            message_id: 1,
            fragment_idx: 0,
            fragment_total: 1,
            payload: b"hi".to_vec(),
        };
        let m = r.accept(f).unwrap();
        assert_eq!(m.msg_type, 0x02);
        assert_eq!(m.payload, b"hi");
    }

    #[test]
    fn three_fragments_reassemble() {
        let mut r = Reassembler::new(2000);
        for i in 0..3 {
            let f = Fragment {
                msg_type: 0x02,
                message_id: 42,
                fragment_idx: i,
                fragment_total: 3,
                payload: vec![i as u8; 10],
            };
            let m = r.accept(f);
            if i < 2 { assert!(m.is_none()); }
            else {
                let m = m.unwrap();
                assert_eq!(m.payload.len(), 30);
                assert_eq!(m.payload[0], 0);
                assert_eq!(m.payload[10], 1);
                assert_eq!(m.payload[20], 2);
            }
        }
    }

    #[test]
    fn duplicate_fragment_ignored() {
        let mut r = Reassembler::new(2000);
        let f0 = Fragment { msg_type: 0x02, message_id: 7, fragment_idx: 0, fragment_total: 2, payload: vec![1, 2] };
        let f0_dup = f0.clone();
        let f1 = Fragment { msg_type: 0x02, message_id: 7, fragment_idx: 1, fragment_total: 2, payload: vec![3, 4] };
        assert!(r.accept(f0).is_none());
        assert!(r.accept(f0_dup).is_none());
        let m = r.accept(f1).unwrap();
        assert_eq!(m.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn header_roundtrip() {
        let f = Fragment {
            msg_type: 0x82, // HAS_NEXT | 0x02
            message_id: 0xCAFEBABE,
            fragment_idx: 3,
            fragment_total: 5,
            payload: Vec::new(),
        };
        let mut buf = [0u8; FRAGMENT_HEADER_BYTES];
        f.serialize_header(&mut buf);
        let (back, used) = Fragment::deserialize_header(&buf).unwrap();
        assert_eq!(used, FRAGMENT_HEADER_BYTES);
        assert_eq!(back.msg_type, 0x82);
        assert!(back.has_next_in_frame());
        assert_eq!(back.app_msg_type(), 0x02);
        assert_eq!(back.message_id, 0xCAFEBABE);
        assert_eq!(back.fragment_idx, 3);
        assert_eq!(back.fragment_total, 5);
    }
}
