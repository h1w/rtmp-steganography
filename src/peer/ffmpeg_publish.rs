//! ffmpeg args for publishing raw RGB24 to RTMP.
//!
//! The flicker encoder emits internal frames at `flicker.width × flicker.height`.
//! If `stream_width × stream_height` differ, ffmpeg scales up using nearest-
//! neighbor (preserves cells intact). The encoded video on the wire is at
//! `stream_width × stream_height @ fps`.

pub struct PublishOpts<'a> {
    pub rtmp_url: &'a str,
    pub flicker_width: u32,
    pub flicker_height: u32,
    pub stream_width: u32,
    pub stream_height: u32,
    pub fps: u32,
}

pub fn publish_args(opts: &PublishOpts) -> Vec<String> {
    let in_size = format!("{}x{}", opts.flicker_width, opts.flicker_height);
    let rate_arg = opts.fps.to_string();
    let gop_arg = (opts.fps * 2).to_string();
    let needs_scale = opts.flicker_width != opts.stream_width
        || opts.flicker_height != opts.stream_height;
    let scale_filter = format!(
        "scale={}:{}:flags=neighbor,format=yuv420p",
        opts.stream_width, opts.stream_height
    );
    // Bitrate: sqrt-scaled with pixel count, capped at VK-safe ceilings.
    // VK Live rejects ingests that exceed its per-slot bitrate expectation
    // (observed: at 3.1 Mbps on a 640x360 slot the stream stays offline).
    // Anchor: 256x144@24 -> 500 kbps. 640x360@24 -> ~1250 kbps. 720p@24 -> ~2 Mbps.
    // Formula: sqrt(pixels * fps / anchor) * 500.
    let pixels = opts.stream_width as u64 * opts.stream_height as u64;
    let anchor = 256u64 * 144 * 24;
    let ratio = ((pixels * opts.fps as u64) as f64 / anchor as f64).sqrt();
    let kbps = (500.0 * ratio).round() as u32;
    let kbps = kbps.clamp(500, 2000);
    let bv_arg = format!("{}k", kbps);
    let bufsize_arg = format!("{}k", kbps * 2);

    let mut args: Vec<String> = Vec::with_capacity(64);
    let push = |args: &mut Vec<String>, s: &str| args.push(s.to_string());

    push(&mut args, "-hide_banner"); push(&mut args, "-loglevel"); push(&mut args, "info");
    push(&mut args, "-y");
    push(&mut args, "-use_wallclock_as_timestamps"); push(&mut args, "1");
    push(&mut args, "-thread_queue_size"); push(&mut args, "1024");
    push(&mut args, "-f"); push(&mut args, "rawvideo");
    push(&mut args, "-pix_fmt"); push(&mut args, "rgb24");
    push(&mut args, "-s"); args.push(in_size);
    push(&mut args, "-r"); args.push(rate_arg.clone());
    push(&mut args, "-i"); push(&mut args, "-");
    push(&mut args, "-f"); push(&mut args, "lavfi");
    push(&mut args, "-i"); push(&mut args, "anullsrc=channel_layout=stereo:sample_rate=44100");

    if needs_scale {
        push(&mut args, "-vf");
        args.push(scale_filter);
    }

    push(&mut args, "-c:v"); push(&mut args, "libx264");
    push(&mut args, "-preset"); push(&mut args, "ultrafast");
    push(&mut args, "-tune"); push(&mut args, "zerolatency");
    push(&mut args, "-profile:v"); push(&mut args, "baseline");
    push(&mut args, "-level"); push(&mut args, "3.0");
    push(&mut args, "-pix_fmt"); push(&mut args, "yuv420p");
    // Disable ONLY the in-loop deblocking filter — it's what smears the
    // hard 4x4 cell boundaries into a gradient noise field on decode.
    // Other "visual quality" heuristics (aq-mode, psy-rd, mbtree) stay at
    // their preset=ultrafast defaults; turning them off simultaneously
    // made x264 CPU-bound and drop below realtime, which killed the RTMP
    // ingest entirely.
    push(&mut args, "-x264-params");
    push(&mut args, "no-deblock=1");
    push(&mut args, "-b:v"); args.push(bv_arg.clone());
    push(&mut args, "-maxrate"); args.push(bv_arg);
    push(&mut args, "-bufsize"); args.push(bufsize_arg);
    push(&mut args, "-g"); args.push(gop_arg);
    push(&mut args, "-keyint_min"); args.push(rate_arg);
    push(&mut args, "-c:a"); push(&mut args, "aac");
    push(&mut args, "-b:a"); push(&mut args, "64k");
    push(&mut args, "-ar"); push(&mut args, "44100");
    push(&mut args, "-ac"); push(&mut args, "2");
    push(&mut args, "-shortest");
    push(&mut args, "-flvflags"); push(&mut args, "no_duration_filesize");
    push(&mut args, "-f"); push(&mut args, "flv");
    args.push(opts.rtmp_url.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publish_args_contain_rtmp_url_and_native_size() {
        let opts = PublishOpts {
            rtmp_url: "rtmp://example/live/key",
            flicker_width: 256, flicker_height: 144,
            stream_width: 256, stream_height: 144,
            fps: 24,
        };
        let args = publish_args(&opts);
        assert!(args.iter().any(|a| a == "rtmp://example/live/key"));
        assert!(args.iter().any(|a| a == "256x144"));
        assert!(!args.iter().any(|a| a.starts_with("scale=")));
    }

    #[test]
    fn publish_args_inject_scale_filter_when_stream_larger() {
        let opts = PublishOpts {
            rtmp_url: "rtmp://x/y",
            flicker_width: 256, flicker_height: 144,
            stream_width: 640, stream_height: 360,
            fps: 24,
        };
        let args = publish_args(&opts);
        assert!(args.iter().any(|a| a.contains("scale=640:360")));
    }
}
