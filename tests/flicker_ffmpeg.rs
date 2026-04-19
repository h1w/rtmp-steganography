//! Level C — round-trip through libx264 yuv420p via ffmpeg subprocess pipes.
//! Requires ffmpeg on PATH. Run with: cargo test --features ffmpeg-integration

#![cfg(feature = "ffmpeg-integration")]

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::{FPS, FRAME_BYTES_RGB24, FRAME_HEIGHT, FRAME_WIDTH};
use rtmp_steganography::flicker::ModulationMode;

const N_FRAMES: usize = 50;

#[test]
fn ffmpeg_roundtrip_50_frames_mode_b() {
    // 1. Prepare input: N_FRAMES raw RGB24 frames with known payloads.
    let mut encoder = FrameEncoder { mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let mut input = Vec::with_capacity(FRAME_BYTES_RGB24 * N_FRAMES);
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for i in 0..N_FRAMES {
        let payload: Vec<u8> = (0..100u8).map(|b| b.wrapping_add(i as u8)).collect();
        payloads.push(payload.clone());
        let frag = Fragment {
            msg_type: 0x02, message_id: i as u32, fragment_idx: 0, fragment_total: 1,
            payload,
        };
        let mut frame = vec![0u8; FRAME_BYTES_RGB24];
        encoder.encode(&mut frame, &[frag]).unwrap();
        input.extend_from_slice(&frame);
    }

    // 2. ffmpeg encode: rawvideo rgb24 → h264/yuv420p
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let fps_arg = FPS.to_string();
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgb24",
            "-s", &size_arg, "-r", &fps_arg, "-i", "pipe:0",
            "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
            "-profile:v", "baseline", "-pix_fmt", "yuv420p",
            "-b:v", "1500k", "-g", &fps_arg,
            "-f", "h264", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg encode — is it on PATH?");

    let mut stdin = child.stdin.take().unwrap();
    let input_clone = input.clone();
    std::thread::spawn(move || {
        stdin.write_all(&input_clone).ok();
    });
    let h264 = child.wait_with_output().unwrap().stdout;
    assert!(!h264.is_empty(), "ffmpeg encode produced empty output");

    // 3. Decode h264 back to rawvideo rgb24.
    let mut child2 = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "h264", "-i", "pipe:0",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg decode");
    let mut stdin2 = child2.stdin.take().unwrap();
    std::thread::spawn(move || { stdin2.write_all(&h264).ok(); });
    let mut stdout2 = child2.stdout.take().unwrap();
    let mut decoded = Vec::new();
    stdout2.read_to_end(&mut decoded).unwrap();
    let _ = child2.wait();

    // 4. Split decoded bytes into frames and run FrameDecoder.
    let got_frames = decoded.len() / FRAME_BYTES_RGB24;
    assert!(got_frames >= N_FRAMES - 2, "lost too many frames: {got_frames}");

    let decoder = FrameDecoder;
    let mut ok_count = 0usize;
    for i in 0..got_frames.min(N_FRAMES) {
        let start = i * FRAME_BYTES_RGB24;
        let frame = &decoded[start..start + FRAME_BYTES_RGB24];
        if let DecodeOutcome::Ok { fragments, .. } = decoder.decode(frame) {
            if !fragments.is_empty() && fragments[0].payload.starts_with(&payloads[i][..100.min(payloads[i].len())]) {
                ok_count += 1;
            }
        }
    }
    assert!(ok_count >= 48, "only {ok_count}/{N_FRAMES} frames recovered bit-exact");
}
