#![cfg(feature = "ffmpeg-integration")]

use std::io::Write;
use std::process::{Command, Stdio};

use rtmp_steganography::flicker::frame::FrameEncoder;
use rtmp_steganography::flicker::fragment::Fragment;
use rtmp_steganography::flicker::grid::FlickerParams;
use rtmp_steganography::flicker::ModulationMode;

fn flicker_input(p: FlickerParams, seconds: u32) -> Vec<u8> {
    let n_frames = (p.fps * seconds) as usize;
    let mut encoder = FrameEncoder { params: p, mode: ModulationMode::B, channel_id: 1, frame_counter: 0 };
    let mut input = Vec::with_capacity(p.frame_bytes_rgb24() * n_frames);
    for i in 0..n_frames {
        let payload: Vec<u8> = (0..200u8).map(|b| b.wrapping_add(i as u8)).collect();
        let frag = Fragment { msg_type: 0x02, message_id: i as u32, fragment_idx: 0, fragment_total: 1, payload };
        let mut frame = vec![0u8; p.frame_bytes_rgb24()];
        encoder.encode(&mut frame, &[frag]).unwrap();
        input.extend_from_slice(&frame);
    }
    input
}

fn encode_and_measure(p: FlickerParams, input: &[u8], qp: u32, seconds: u32) -> usize {
    let mut child = Command::new("ffmpeg").args([
        "-hide_banner","-loglevel","error","-y",
        "-f","rawvideo","-pix_fmt","rgb24",
        "-s",&format!("{}x{}", p.width, p.height),
        "-r",&p.fps.to_string(),
        "-i","pipe:0",
        "-c:v","libx264","-preset","ultrafast","-tune","zerolatency",
        "-profile:v","baseline","-pix_fmt","yuv420p",
        "-qp",&qp.to_string(),
        "-x264-params","no-deblock=1",
        "-g",&p.fps.to_string(),
        "-f","h264","-",
    ]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let clone = input.to_vec();
    std::thread::spawn(move || { stdin.write_all(&clone).ok(); });
    let h264 = child.wait_with_output().unwrap().stdout;
    let kbps = h264.len() * 8 / 1000 / seconds as usize;
    eprintln!("  qp={}: {} bytes / {}s = {} kbit/s", qp, h264.len(), seconds, kbps);
    kbps
}

#[test]
fn measure_qp_bitrate_1280x720_cell8() {
    let p = FlickerParams::with_cell(1280, 720, 24, 8);
    let input = flicker_input(p, 5);
    eprintln!("\n=== 1280x720 cell=8, qp bitrate vs quality ===");
    for qp in [18u32, 22, 26, 30, 34] {
        encode_and_measure(p, &input, qp, 5);
    }
}

#[test]
fn measure_qp_bitrate_1280x720_cell16() {
    let p = FlickerParams::with_cell(1280, 720, 24, 16);
    let input = flicker_input(p, 5);
    eprintln!("\n=== 1280x720 cell=16, qp bitrate vs quality ===");
    for qp in [18u32, 22, 26, 30, 34, 36, 38, 40, 42] {
        encode_and_measure(p, &input, qp, 5);
    }
}

#[test]
fn measure_qp_decode_quality_1280x720_cell8() {
    // For cell=8 we need qp high enough to fit VK bitrate, but decode still works.
    use rtmp_steganography::flicker::frame::{FrameDecoder, DecodeOutcome};
    let p = FlickerParams::with_cell(1280, 720, 24, 8);
    let seconds = 5u32;
    let n_frames = (p.fps * seconds) as usize;
    let input = flicker_input(p, seconds);

    eprintln!("\n=== cell=8 qp sweep: bitrate vs decode pass rate ===");
    for qp in [18u32, 22, 26, 30, 34, 38, 42, 46] {
        let mut enc = Command::new("ffmpeg").args([
            "-hide_banner","-loglevel","error","-y",
            "-f","rawvideo","-pix_fmt","rgb24",
            "-s",&format!("{}x{}", p.width, p.height),
            "-r",&p.fps.to_string(),"-i","pipe:0",
            "-c:v","libx264","-preset","ultrafast","-tune","zerolatency",
            "-profile:v","baseline","-pix_fmt","yuv420p",
            "-qp",&qp.to_string(),
            "-x264-params","no-deblock=1",
            "-g",&p.fps.to_string(),"-f","h264","-",
        ]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = enc.stdin.take().unwrap();
        let clone = input.clone();
        std::thread::spawn(move || { stdin.write_all(&clone).ok(); });
        let h264 = enc.wait_with_output().unwrap().stdout;
        let kbps = h264.len() * 8 / 1000 / seconds as usize;

        let mut dec = Command::new("ffmpeg").args([
            "-hide_banner","-loglevel","error",
            "-f","h264","-i","pipe:0",
            "-f","rawvideo","-pix_fmt","rgb24","-",
        ]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin2 = dec.stdin.take().unwrap();
        std::thread::spawn(move || { stdin2.write_all(&h264).ok(); });
        use std::io::Read;
        let mut decoded = Vec::new();
        dec.stdout.take().unwrap().read_to_end(&mut decoded).unwrap();
        let _ = dec.wait();

        let fbytes = p.frame_bytes_rgb24();
        let got = decoded.len() / fbytes;
        let decoder = FrameDecoder { params: p };
        let mut ok = 0usize;
        for i in 0..got.min(n_frames) {
            let frame = &decoded[i*fbytes..(i+1)*fbytes];
            if matches!(decoder.decode(frame), DecodeOutcome::Ok { .. }) { ok += 1; }
        }
        eprintln!("  qp={:>2}: {:>6} kbit/s  ok={}/{}", qp, kbps, ok, got.min(n_frames));
    }
}
