//! Level C — roundtrip flicker frame through libx264 encode+decode locally
//! (no VK). If this fails at 1280x720, problem is x264 cell-smearing, not VK.

#![cfg(feature = "ffmpeg-integration")]

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use rtmp_steganography::flicker::frame::{FrameDecoder, FrameEncoder, DecodeOutcome, DropReason};
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FlickerParams;
use rtmp_steganography::flicker::ModulationMode;

/// Full x264 encoder knobs for a loopback test.
/// `extra_args` lets the test inject any ffmpeg flags (e.g. -qp, -g, -intra).
/// `x264_params` is a `:`-joined `-x264-params` string.
fn roundtrip_full(
    p: FlickerParams, n_frames: usize,
    profile: &str, extra_args: &[&str], x264_params: &str,
) -> (usize, Vec<String>) {
    let mut encoder = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let mut input = Vec::with_capacity(p.frame_bytes_rgb24() * n_frames);
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for i in 0..n_frames {
        let payload: Vec<u8> = (0..100u8).map(|b| b.wrapping_add(i as u8)).collect();
        payloads.push(payload.clone());
        let frag = Fragment {
            msg_type: 0x02, message_id: i as u32, fragment_idx: 0, fragment_total: 1,
            payload,
        };
        let mut frame = vec![0u8; p.frame_bytes_rgb24()];
        encoder.encode(&mut frame, &[frag]).unwrap();
        input.extend_from_slice(&frame);
    }

    let size_arg = format!("{}x{}", p.width, p.height);
    let fps_arg = p.fps.to_string();
    let mut args: Vec<String> = vec![
        "-hide_banner".into(), "-loglevel".into(), "error".into(), "-y".into(),
        "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "rgb24".into(),
        "-s".into(), size_arg, "-r".into(), fps_arg.clone(), "-i".into(), "pipe:0".into(),
        "-c:v".into(), "libx264".into(),
        "-preset".into(), "ultrafast".into(),
        "-tune".into(), "zerolatency".into(),
        "-profile:v".into(), profile.into(),
        "-pix_fmt".into(), "yuv420p".into(),
        "-g".into(), fps_arg,
    ];
    if !x264_params.is_empty() {
        args.push("-x264-params".into()); args.push(x264_params.into());
    }
    for &a in extra_args { args.push(a.to_string()); }
    args.extend(["-f".into(), "h264".into(), "-".into()]);

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().expect("spawn ffmpeg encode");
    let mut stdin = child.stdin.take().unwrap();
    let input_clone = input.clone();
    std::thread::spawn(move || { stdin.write_all(&input_clone).ok(); });
    let h264 = child.wait_with_output().unwrap().stdout;

    let mut child2 = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error",
               "-f", "h264", "-i", "pipe:0",
               "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().expect("spawn ffmpeg decode");
    let mut stdin2 = child2.stdin.take().unwrap();
    std::thread::spawn(move || { stdin2.write_all(&h264).ok(); });
    let mut stdout2 = child2.stdout.take().unwrap();
    let mut decoded = Vec::new();
    stdout2.read_to_end(&mut decoded).unwrap();
    let _ = child2.wait();

    let fbytes = p.frame_bytes_rgb24();
    let got_frames = decoded.len() / fbytes;
    let decoder = FrameDecoder { params: p };
    let mut ok_count = 0usize;
    let mut drops: Vec<String> = Vec::new();
    for i in 0..got_frames.min(n_frames) {
        let start = i * fbytes;
        let frame = &decoded[start..start + fbytes];
        match decoder.decode(frame) {
            DecodeOutcome::Ok { fragments, .. } => {
                if !fragments.is_empty() && fragments[0].payload.starts_with(&payloads[i][..100]) {
                    ok_count += 1;
                }
            }
            DecodeOutcome::Dropped { reason } => {
                drops.push(format!("{:?}", match reason {
                    DropReason::HeaderRsFailed => "HeaderRs",
                    DropReason::BlockRsFailed(_) => "BlockRs",
                    DropReason::BlockRsFailedY(_) => "BlockRsY",
                    DropReason::BlockRsFailedUV(_) => "BlockRsUV",
                    DropReason::PilotValidationFailed(_) => "Pilot",
                    DropReason::PayloadCrcMismatch => "CRC",
                    DropReason::HeaderCrc => "HeaderCrc",
                    DropReason::SyncOffsetMissing => "Sync",
                    DropReason::FragmentParse => "FragParse",
                }));
            }
        }
    }
    (ok_count, drops)
}

#[test]
fn sweep_1280x720_cell8_x264_knobs() {
    let p = FlickerParams::with_cell(1280, 720, 24, 8);
    let cases: Vec<(&str, &str, Vec<&str>, &str)> = vec![
        ("baseline CBR 2M + no-deblock", "baseline", vec!["-b:v","2000k","-maxrate","2000k","-bufsize","4000k"], "no-deblock=1"),
        ("baseline CBR 3M",               "baseline", vec!["-b:v","3000k","-maxrate","3000k","-bufsize","6000k"], "no-deblock=1"),
        ("baseline CBR 4M",               "baseline", vec!["-b:v","4000k","-maxrate","4000k","-bufsize","8000k"], "no-deblock=1"),
        ("baseline -qp 22 (fixed)",       "baseline", vec!["-qp","22"], "no-deblock=1"),
        ("baseline -qp 18",               "baseline", vec!["-qp","18"], "no-deblock=1"),
        ("main CBR 2M + no-deblock",      "main",     vec!["-b:v","2000k","-maxrate","2000k","-bufsize","4000k"], "no-deblock=1"),
        ("high -qp 18",                   "high",     vec!["-qp","18"], "no-deblock=1"),
        ("baseline intra-only -qp 22",    "baseline", vec!["-qp","22","-intra"], "no-deblock=1"),
    ];
    eprintln!("\n=== 1280x720 cell=8 x264 sweep ===");
    eprintln!("| {:<40} | OK/30 | Drops",
        "variant");
    eprintln!("|------------------------------------------|-------|");
    for (name, profile, extra, x264p) in cases {
        let (ok, drops) = roundtrip_full(p, 30, profile, &extra, x264p);
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
        eprintln!("| {:<40} | {:>5} | {:?}", name, format!("{}/30", ok), counts);
    }
}

fn roundtrip_at(p: FlickerParams, n_frames: usize, bitrate_kbps: u32) -> (usize, Vec<String>) {
    let mut encoder = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let mut input = Vec::with_capacity(p.frame_bytes_rgb24() * n_frames);
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for i in 0..n_frames {
        let payload: Vec<u8> = (0..100u8).map(|b| b.wrapping_add(i as u8)).collect();
        payloads.push(payload.clone());
        let frag = Fragment {
            msg_type: 0x02, message_id: i as u32, fragment_idx: 0, fragment_total: 1,
            payload,
        };
        let mut frame = vec![0u8; p.frame_bytes_rgb24()];
        encoder.encode(&mut frame, &[frag]).unwrap();
        input.extend_from_slice(&frame);
    }

    let size_arg = format!("{}x{}", p.width, p.height);
    let fps_arg = p.fps.to_string();
    let b_arg = format!("{}k", bitrate_kbps);
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgb24",
            "-s", &size_arg, "-r", &fps_arg, "-i", "pipe:0",
            "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
            "-profile:v", "baseline", "-pix_fmt", "yuv420p",
            "-b:v", &b_arg, "-g", &fps_arg,
            "-x264-params", "no-deblock=1",
            "-f", "h264", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg encode");
    let mut stdin = child.stdin.take().unwrap();
    let input_clone = input.clone();
    std::thread::spawn(move || { stdin.write_all(&input_clone).ok(); });
    let h264 = child.wait_with_output().unwrap().stdout;

    let mut child2 = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "h264", "-i", "pipe:0",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn().expect("spawn ffmpeg decode");
    let mut stdin2 = child2.stdin.take().unwrap();
    std::thread::spawn(move || { stdin2.write_all(&h264).ok(); });
    let mut stdout2 = child2.stdout.take().unwrap();
    let mut decoded = Vec::new();
    stdout2.read_to_end(&mut decoded).unwrap();
    let _ = child2.wait();

    let fbytes = p.frame_bytes_rgb24();
    let got_frames = decoded.len() / fbytes;
    let decoder = FrameDecoder { params: p };
    let mut ok_count = 0usize;
    let mut drops: Vec<String> = Vec::new();
    for i in 0..got_frames.min(n_frames) {
        let start = i * fbytes;
        let frame = &decoded[start..start + fbytes];
        match decoder.decode(frame) {
            DecodeOutcome::Ok { fragments, .. } => {
                if !fragments.is_empty() && fragments[0].payload.starts_with(&payloads[i][..100]) {
                    ok_count += 1;
                }
            }
            DecodeOutcome::Dropped { reason } => {
                drops.push(format!("{:?}", match reason {
                    DropReason::HeaderRsFailed => "HeaderRs",
                    DropReason::BlockRsFailed(_) => "BlockRs",
                    DropReason::BlockRsFailedY(_) => "BlockRsY",
                    DropReason::BlockRsFailedUV(_) => "BlockRsUV",
                    DropReason::PilotValidationFailed(_) => "Pilot",
                    DropReason::PayloadCrcMismatch => "CRC",
                    DropReason::HeaderCrc => "HeaderCrc",
                    DropReason::SyncOffsetMissing => "Sync",
                    DropReason::FragmentParse => "FragParse",
                }));
            }
        }
    }
    (ok_count, drops)
}

#[test]
fn ffmpeg_roundtrip_256x144_baseline() {
    let p = FlickerParams::default_256x144_24();
    let (ok, _) = roundtrip_at(p, 30, 500);
    assert!(ok >= 28, "256x144 baseline should recover nearly all: got {ok}/30");
}

#[test]
fn ffmpeg_roundtrip_640x360_cell8() {
    let p = FlickerParams::with_cell(640, 360, 24, 8);
    let (ok, drops) = roundtrip_at(p, 30, 1250);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("640x360 cell8 @1.25Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_1280x720_cell8() {
    let p = FlickerParams::with_cell(1280, 720, 24, 8);
    let (ok, drops) = roundtrip_at(p, 30, 2000);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("1280x720 cell8 @2.0Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_1280x720_cell16() {
    let p = FlickerParams::with_cell(1280, 720, 24, 16);
    let (ok, drops) = roundtrip_at(p, 30, 2000);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("1280x720 cell16 @2.0Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_640x360_native() {
    let p = FlickerParams::new(640, 360, 24);
    let (ok, drops) = roundtrip_at(p, 30, 1250);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("640x360@1.25Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_640x360_higher_bitrate() {
    let p = FlickerParams::new(640, 360, 24);
    let (ok, drops) = roundtrip_at(p, 30, 5000);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("640x360@5.0Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_1280x720_native() {
    let p = FlickerParams::new(1280, 720, 24);
    let (ok, drops) = roundtrip_at(p, 30, 2000);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("1280x720 ok={}/30  drops={:?}", ok, counts);
    assert!(ok >= 28, "1280x720 should recover: got {ok}/30, drops={:?}", counts);
}

#[test]
fn ffmpeg_roundtrip_1280x720_cell10() {
    let p = FlickerParams::with_cell(1280, 720, 24, 10);
    let (ok, drops) = roundtrip_at(p, 30, 2000);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
    eprintln!("1280x720 cell10 @2.0Mbps ok={}/30  drops={:?}", ok, counts);
}

#[test]
fn ffmpeg_roundtrip_1280x720_cell12() {
    // 1280/12 is not exact, so expect ~1272x720 via floor. Test will skip if validate fails.
    if let Ok(()) = FlickerParams::with_cell(1280, 720, 24, 12).validate() {
        let p = FlickerParams::with_cell(1280, 720, 24, 12);
        let (ok, drops) = roundtrip_at(p, 30, 2000);
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for d in &drops { *counts.entry(d.clone()).or_default() += 1; }
        eprintln!("1280x720 cell12 @2.0Mbps ok={}/30  drops={:?}", ok, counts);
    } else {
        eprintln!("1280x720 cell12: not evenly divisible, skipped");
    }
}
