use rtmp_steganography::flicker::FLICKER_MAX_PAYLOAD_BYTES;

#[test]
fn mtu_constant_is_positive_and_reasonable() {
    assert!(FLICKER_MAX_PAYLOAD_BYTES > 0);
    assert!(FLICKER_MAX_PAYLOAD_BYTES < 8192, "unexpectedly large flicker payload");
}
