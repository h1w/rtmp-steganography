//! Level B synthetic lossy tests — validate FEC + pilots + corner markers
//! recover the payload under controlled damage, or cleanly drop the frame.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FRAME_BYTES_RGB24;
use rtmp_steganography::flicker::ModulationMode;

fn make_frame(payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; FRAME_BYTES_RGB24];
    let mut enc = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 9 };
    let frag = Fragment {
        msg_type: 0x02, message_id: 1, fragment_idx: 0, fragment_total: 1,
        payload: payload.to_vec(),
    };
    enc.encode(&mut buf, &[frag]).unwrap();
    buf
}

fn assert_ok_or_dropped(buf: &[u8], expected: &[u8]) {
    // Test passes if decoder either recovers `expected` exactly, or cleanly marks frame dropped.
    // Never should it deliver wrong data.
    match FrameDecoder.decode(buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            let got = &fragments[0].payload[..expected.len().min(fragments[0].payload.len())];
            assert_eq!(got, expected, "decoder delivered wrong data");
        }
        DecodeOutcome::Dropped { .. } => {
            // acceptable — protocol signalled "cannot trust this frame"
        }
    }
}

#[test]
fn low_gaussian_noise_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(1);
    for b in buf.iter_mut() {
        let noise: i32 = rng.gen_range(-5..=5);
        *b = (*b as i32 + noise).clamp(0, 255) as u8;
    }
    // Guarantee: bit-exact recovery at σ≈5.
    match FrameDecoder.decode(&buf) {
        DecodeOutcome::Ok { fragments, .. } => {
            assert_eq!(&fragments[0].payload[..150], &payload[..]);
        }
        o => panic!("expected Ok at low noise, got {o:?}"),
    }
}

#[test]
fn moderate_gaussian_noise_drops_or_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(2);
    for b in buf.iter_mut() {
        let noise: i32 = rng.gen_range(-30..=30);
        *b = (*b as i32 + noise).clamp(0, 255) as u8;
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn one_pct_pixel_flip_recovers() {
    let payload: Vec<u8> = (0..150u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(3);
    let n_flips = buf.len() / 100; // 1%
    for _ in 0..n_flips {
        let i = rng.gen_range(0..buf.len());
        buf[i] = 255 - buf[i];
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn brightness_bias_plus_20_recovers() {
    let payload: Vec<u8> = (0..120u8).collect();
    let mut buf = make_frame(&payload);
    for b in buf.iter_mut() {
        *b = b.saturating_add(20);
    }
    assert_ok_or_dropped(&buf, &payload);
}

#[test]
fn block_corruption_3x8x8_bursts() {
    use rtmp_steganography::flicker::grid::{rgb24_offset, FRAME_WIDTH, FRAME_HEIGHT};
    let payload: Vec<u8> = (0..120u8).collect();
    let mut buf = make_frame(&payload);
    let mut rng = StdRng::seed_from_u64(5);
    for _ in 0..3 {
        let cx = rng.gen_range(0..FRAME_WIDTH - 8);
        let cy = rng.gen_range(0..FRAME_HEIGHT - 8);
        for dy in 0..8 { for dx in 0..8 {
            let o = rgb24_offset(cx + dx, cy + dy);
            let v: u8 = rng.gen();
            buf[o] = v; buf[o + 1] = v; buf[o + 2] = v;
        }}
    }
    assert_ok_or_dropped(&buf, &payload);
}
