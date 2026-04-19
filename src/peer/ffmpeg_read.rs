//! Low-latency HLS/DASH → raw RGB24 ingest via ffmpeg subprocess.
//!
//! Output is always scaled down to `flicker_width × flicker_height` at
//! `fps` frames/sec — the flicker decoder only understands that geometry.

use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub struct ReadOpts<'a> {
    pub input_url: &'a str,
    pub page_url: &'a str,
    pub input_is_hls: bool,
    pub flicker_width: u32,
    pub flicker_height: u32,
    pub fps: u32,
}

pub fn read_args(opts: &ReadOpts) -> Vec<String> {
    let size_arg = format!("{}x{}", opts.flicker_width, opts.flicker_height);
    let scale_filter = format!(
        "scale={}:{}:flags=neighbor,format=rgb24,fps={}",
        opts.flicker_width, opts.flicker_height, opts.fps
    );
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
    if opts.input_is_hls {
        args.push("-live_start_index".into());
        args.push("-1".into());
        args.push("-http_persistent".into());
        args.push("1".into());
    }
    args.push("-user_agent".into());
    args.push(USER_AGENT.into());
    args.push("-headers".into());
    args.push(format!("Referer: {}\r\nOrigin: https://live.vkvideo.ru\r\n", opts.page_url));
    args.push("-i".into());
    args.push(opts.input_url.to_string());
    args.push("-thread_queue_size".into());
    args.push("1024".into());
    args.push("-map".into()); args.push("0:v:0".into());
    args.push("-an".into());
    args.push("-vf".into());
    args.push(scale_filter);
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
    #[test]
    fn read_args_include_input_url_and_scale() {
        let opts = ReadOpts {
            input_url: "https://x/playlist.m3u8",
            page_url: "https://page",
            input_is_hls: true,
            flicker_width: 256, flicker_height: 144, fps: 24,
        };
        let args = read_args(&opts);
        assert!(args.iter().any(|a| a == "https://x/playlist.m3u8"));
        assert!(args.iter().any(|a| a.contains("scale=256:144")));
    }
}
