use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use crate::flicker::GridConfig;

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Build ffmpeg args that drain an HLS/DASH URL to raw RGB24 on stdout with
/// aggressive low-latency flags and the browser-ish headers okcdn expects.
pub fn read_args(
    input_url: &str,
    page_url: &str,
    input_is_hls: bool,
    cfg: &GridConfig,
) -> Vec<String> {
    let size_arg = format!("{}x{}", cfg.width, cfg.height);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-fflags".into(),
        "nobuffer+discardcorrupt+flush_packets".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "500000".into(),
        "-analyzeduration".into(),
        "500000".into(),
        "-max_delay".into(),
        "500000".into(),
        "-rtbufsize".into(),
        "8M".into(),
        "-reconnect".into(),
        "1".into(),
        "-reconnect_streamed".into(),
        "1".into(),
        "-reconnect_delay_max".into(),
        "2".into(),
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
    args.push(format!(
        "Referer: {page_url}\r\nOrigin: https://live.vkvideo.ru\r\n"
    ));

    args.push("-i".into());
    args.push(input_url.to_string());
    args.push("-thread_queue_size".into());
    args.push("1024".into());
    args.push("-an".into());
    args.push("-vf".into());
    args.push(format!(
        "scale={}:{}:flags=neighbor,format=rgb24,fps={}",
        cfg.width, cfg.height, cfg.fps
    ));
    args.push("-fps_mode".into());
    args.push("cfr".into());
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-s".into(),
        size_arg,
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
