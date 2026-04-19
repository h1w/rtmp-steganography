//! ffmpeg args for publishing raw RGB24 to RTMP.

use crate::flicker::grid::{FPS, FRAME_HEIGHT, FRAME_WIDTH};

pub fn publish_args(rtmp_url: &str) -> Vec<String> {
    let size_arg = format!("{}x{}", FRAME_WIDTH, FRAME_HEIGHT);
    let rate_arg = FPS.to_string();
    let gop_arg = (FPS * 2).to_string();
    [
        "-hide_banner", "-loglevel", "info", "-y",
        "-use_wallclock_as_timestamps", "1",
        "-thread_queue_size", "1024",
        "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-s", &size_arg, "-r", &rate_arg, "-i", "-",
        "-f", "lavfi", "-i", "anullsrc=channel_layout=stereo:sample_rate=44100",
        "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
        "-profile:v", "baseline", "-level", "3.0", "-pix_fmt", "yuv420p",
        "-b:v", "300k", "-maxrate", "300k", "-bufsize", "600k",
        "-g", &gop_arg, "-keyint_min", &rate_arg,
        "-c:a", "aac", "-b:a", "64k", "-ar", "44100", "-ac", "2",
        "-shortest", "-flvflags", "no_duration_filesize",
        "-f", "flv", rtmp_url,
    ].into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn publish_args_contain_rtmp_url() {
        let args = publish_args("rtmp://example/live/key");
        assert!(args.iter().any(|a| a == "rtmp://example/live/key"));
        assert!(args.iter().any(|a| a == "256x144"));
    }
}
