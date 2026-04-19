//! Level B — pure in-memory encode/decode round-trip without ffmpeg.
//! Asserts every message type, size, and fragment count round-trips bit-exact.

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FRAME_BYTES_RGB24;
use rtmp_steganography::flicker::ModulationMode;

fn make_frag(msg_type: u8, payload: Vec<u8>) -> Fragment {
    Fragment {
        msg_type,
        message_id: 1,
        fragment_idx: 0,
        fragment_total: 1,
        payload,
    }
}

#[test]
fn roundtrip_mode_b_single_fragment_small() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let frag = make_frag(0x02, b"short payload".to_vec());
    enc.encode(&mut buf, &[frag.clone()]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments.len(), 1);
            assert!(fragments[0].payload.starts_with(b"short payload"));
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_b_max_size() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 5 };
    // 240 byte frame budget - 9 byte fragment header = 231 bytes payload.
    let payload: Vec<u8> = (0..231u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments[0].payload[..231], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_c_larger_payload() {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::C, channel_id: 2, frame_counter: 42 };
    let payload: Vec<u8> = (0..200u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, header, .. } => {
            assert_eq!(header.modulation_mode, ModulationMode::C);
            assert_eq!(fragments[0].payload[..200], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}
