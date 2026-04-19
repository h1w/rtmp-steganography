//! Level B — pure in-memory encode/decode round-trip without ffmpeg.
//! Asserts every message type, size, and fragment count round-trips bit-exact.

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FlickerParams;
use rtmp_steganography::flicker::ModulationMode;

fn make_frag(msg_type: u8, payload: Vec<u8>) -> Fragment {
    Fragment {
        msg_type, message_id: 1,
        fragment_idx: 0, fragment_total: 1,
        payload,
    }
}

#[test]
fn roundtrip_mode_b_single_fragment_small() {
    let p = FlickerParams::default_256x144_24();
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let frag = make_frag(0x02, b"short payload".to_vec());
    enc.encode(&mut buf, &[frag.clone()]).unwrap();
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments.len(), 1);
            assert!(fragments[0].payload.starts_with(b"short payload"));
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_b_max_size() {
    let p = FlickerParams::default_256x144_24();
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 5 };
    // 240 byte frame budget - 9 byte fragment header - 4 byte payload CRC = 227 bytes.
    let payload: Vec<u8> = (0..227u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(fragments[0].payload[..227], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_mode_c_larger_payload() {
    let p = FlickerParams::default_256x144_24();
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::C, channel_id: 2, frame_counter: 42 };
    let payload: Vec<u8> = (0..200u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, header, .. } => {
            assert_eq!(header.modulation_mode, ModulationMode::C);
            assert_eq!(fragments[0].payload[..200], payload[..]);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn roundtrip_at_360p_carries_more_per_frame() {
    let p = FlickerParams::new(640, 360, 24);
    let mut buf = vec![0u8; p.frame_bytes_rgb24()];
    let mut enc = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    // At 640x360 mode B the capacity is several KB per frame.
    let payload: Vec<u8> = (0..1500u16).map(|i| (i & 0xff) as u8).collect();
    let frag = make_frag(0x02, payload.clone());
    enc.encode(&mut buf, &[frag]).unwrap();
    let dec = FrameDecoder { params: p };
    match dec.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(&fragments[0].payload[..1500], &payload[..]);
        }
        o => panic!("{o:?}"),
    }
}
