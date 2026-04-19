use rtmp_steganography::flicker::frame::{decode_timestamp_frame, encode_timestamp_frame};
use rtmp_steganography::flicker::grid::GridConfig;

fn case(cell: usize, ts: u64) {
    let cfg = GridConfig::new(256, 144, 24, cell, 1).expect("valid grid");
    let mut buf = vec![0u8; cfg.frame_bytes()];
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
    assert!(GridConfig::new(256, 144, 24, 3, 1).is_err());
    assert!(GridConfig::new(256, 144, 24, 0, 1).is_err());
    assert!(GridConfig::new(256, 144, 24, 16, 0).is_err());
    assert!(GridConfig::new(256, 144, 0, 16, 1).is_err());
    assert!(GridConfig::new(0, 144, 24, 16, 1).is_err());
}

#[test]
fn different_resolution_works() {
    let cfg = GridConfig::new(320, 180, 30, 10, 1).expect("valid grid");
    assert_eq!(cfg.cols, 32);
    assert_eq!(cfg.rows, 18);
    assert_eq!(cfg.frame_bytes(), 320 * 180 * 3);
}
