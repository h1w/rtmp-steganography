use rtmp_steganography::flicker::frame::{decode_timestamp_frame, encode_timestamp_frame};
use rtmp_steganography::flicker::grid::GridConfig;
use rtmp_steganography::flicker::FRAME_BYTES;

fn case(cell: usize, ts: u64) {
    let cfg = GridConfig::new(cell, 1).expect("valid grid");
    let mut buf = vec![0u8; FRAME_BYTES];
    encode_timestamp_frame(&mut buf, ts, &cfg);
    let decoded = decode_timestamp_frame(&buf, &cfg);
    assert_eq!(decoded, ts, "cell={cell} ts={ts:#x}");
}

#[test]
fn roundtrip_cell_16() {
    case(16, 0x0123_4567_89ab_cdef);
}

#[test]
fn roundtrip_cell_8() {
    case(8, 0xdead_beef_1234_5678);
}

#[test]
fn roundtrip_cell_4() {
    case(4, 0xffff_ffff_ffff_fffe);
}

#[test]
fn roundtrip_cell_2() {
    case(2, 1);
}

#[test]
fn roundtrip_zero() {
    case(16, 0);
}

#[test]
fn invalid_cell_rejected() {
    assert!(GridConfig::new(3, 1).is_err());
    assert!(GridConfig::new(0, 1).is_err());
    assert!(GridConfig::new(16, 0).is_err());
}
