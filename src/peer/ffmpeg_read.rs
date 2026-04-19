//! Low-latency HLS/DASH → raw RGB24 ingest via ffmpeg subprocess.

use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use crate::flicker::grid::{FPS, FRAME_HEIGHT, FRAME_WIDTH};

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub fn read_args(input_url: &str, page_url: &str, input_is_hls: bool) -> Vec<String> {
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(), "-loglevel".into(), "warning".into(),
        "-fflags".into(), "nobuffer+discardcorrupt+flush_packets".into(),
        "-flags".into(), "low_delay".into(),
        "-probesize".into(), "500000".into(),
        "-analyzeduration".into(), "500000".into(),
        "-max_delay".into(), "500000".into(),
        "-rtbufsize".into(), "8M".into(),
        "-reconnect".into(), "1".into(),
        "-reconnect_streamed".into(), "1".into(),
        "-reconnect_delay_max".into(), "2".into(),
    ];
    if input_is_hls {
        args.push("-live_start_index".into());
        args.push("-1".into());
        args.push("-http_persistent".into());
        args.push("1".into());
    }
    args.push("-user_agent".into());
    args.push(USER_AGENT.into());
    args.push("-headers".into());
    args.push(format!("Referer: {page_url}\r\nOrigin: https://live.vkvideo.ru\r\n"));
    args.push("-i".into());
    args.push(input_url.to_string());
    args.push("-thread_queue_size".into());
    args.push("1024".into());
    args.push("-map".into()); args.push("0:v:0".into());
    args.push("-an".into());
    args.push("-vf".into());
    args.push(format!("scale={}:{}:flags=neighbor,format=rgb24,fps={}", FRAME_WIDTH, FRAME_HEIGHT, FPS));
    args.push("-fps_mode".into()); args.push("cfr".into());
    args.extend([
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "rgb24".into(),
        "-s".into(), size_arg,
        "-".into(),
    ]);
    args
}

pub fn spawn(args: &[String]) -> Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn ffmpeg — is it on PATH?")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn read_args_include_input_url() {
        let args = read_args("https://x/playlist.m3u8", "https://page", true);
        assert!(args.iter().any(|a| a == "https://x/playlist.m3u8"));
        assert!(args.iter().any(|a| a.contains("scale=256:144")));
    }
}
