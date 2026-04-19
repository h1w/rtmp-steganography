# Level D — VK End-to-End Smoke Test

## What this tests

A single peer publishes to your own VK Live channel and reads the same channel back, validating the full pipeline: flicker encode → x264 → RTMP → VK ingest → HLS output → ffmpeg demux → flicker decode.

> **Why Level D is the authoritative gate:** Level C (`cargo test --features ffmpeg-integration`) runs libx264 locally at **1500 kbit/s**, which is enough headroom that the pixel-domain cells survive without issue. Production publishing is **300 kbit/s** (`src/peer/ffmpeg_publish.rs`), and VK may re-encode at any rate it chooses. Level D is the only place where the full real-world BER is exercised end-to-end — a passing Level C does **not** guarantee a passing Level D.

## Prerequisites

1. VK Live account with an active stream slot.
2. `ffmpeg` on `PATH`.
3. `rtmp-steganography` built (`cargo build --release`).

## Environment

Create a `.env` at the repo root:

```env
peer_my_rtmp_url=rtmp://ok-push.vkvideo.ru/live
peer_my_stream_key=<your VK stream key>
peer_their_vk_channel=<your VK channel slug — same as where you publish>
peer_their_stream_name=<stream name, usually matches key prefix>
flicker_modulation_mode=B
flicker_log_every_frame=1
```

## Run

Linux/macOS:
```bash
./scripts/e2e-vk.sh
```

Windows (PowerShell):
```powershell
.\scripts\e2e-vk.ps1 -Seconds 60
```

## Acceptance thresholds

- `delivery rate ≥ 75%` (measured as `time_sync rcvd / time_sync sent`)
- p50 end-to-end latency ≤ 4 s (read from `Δ` values in log)
- p99 end-to-end latency ≤ 10 s
- No panics, clean exit on Ctrl+C

**Empirical baseline (mode B, 500 kbit/s publish):** ~80–85% delivery per direction
on VK Live. The 15–20% residual loss is HLS segment boundary drops on VK's CDN,
not flicker FEC capacity — at this bitrate `hdr_fail` and `crc_fail` are both 0
in steady state. Raising acceptance to 90% requires application-layer
retransmission or deduplication, which is out of scope for v2 (UDP semantics).

## Interpreting failures

| Symptom | Likely cause | Investigate |
|---------|--------------|-------------|
| Zero `time_sync rcvd` | VK not publishing frames downstream | VK dashboard, HLS URL resolution |
| `frames dropped: PilotValidationFailed` dominant | Alignment off, cells misread | Check `scale=neighbor` ffmpeg arg, VK transcoder resolution |
| `BlockRsFailed` dominant | BER > FEC capacity | Try mode B if on C; check VK output bitrate |
| `HeaderRsFailed` dominant | Header zone corrupted | Check corner marker pattern in saved frames |
| `PayloadCrcMismatch` dominant | Silent byte corruption slipping past erasure threshold | Likely x264 loop filter eating cell edges — needs higher publish bitrate or larger cells |

## Debugging tools

Save a raw frame for offline analysis:
```bash
ffmpeg -i "https://<VK HLS URL>" -vf "scale=256:144:flags=neighbor,format=rgb24" -frames:v 1 -pix_fmt rgb24 frame.raw
```

Then inspect with a tool that can render raw RGB24 at 256×144.

## When to re-run

Any PR that touches:
- `src/flicker/frame.rs`
- `src/flicker/markers.rs`
- `src/flicker/pilot.rs`
- `src/flicker/header.rs`
- `src/peer/ffmpeg_*.rs`

must include a successful Level D run note in the PR description.
