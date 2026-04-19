# Flicker Protocol — Design Spec

**Date:** 2026-04-19
**Status:** Approved for implementation

## Goal

Transform the current single-binary RTMP publisher into a dual-mode tool (`rtmp-steganography.exe`) with `--client` (publish) and `--server` (receive + decode) modes. Extract the steganography codec into a named protocol (`flicker`) living in its own module. Server must auto-resolve a VK Live stream by channel slug and minimize ingest latency.

## Out of scope

- Two-way communication.
- Encryption / authentication of payload.
- Payloads beyond a 64-bit timestamp.
- Reference-project extras: VPN tunnel, ChaCha crypto, random word stream.

## CLI

Single binary, clap with subcommands and top-level flags as aliases:

```
rtmp-steganography.exe client        # publish stream
rtmp-steganography.exe server        # receive + decode
rtmp-steganography.exe --client      # alias
rtmp-steganography.exe --server      # alias
```

## Module layout

```
src/
  main.rs              — dispatch CLI → client::run | server::run
  cli.rs               — clap Args (subcommands + alias flags)
  config.rs            — .env loader: grid params, rtmp target, vk slug, referer/origin
  flicker/
    mod.rs             — public API (re-exports)
    grid.rs            — GridConfig, cell validation, cols/rows/total_cells
    codec.rs           — paint_bit_into_frame / read_bit_from_cell (RGB24 @ 256x144)
    frame.rs           — encode_timestamp_frame / decode_timestamp_frame (64-bit BE)
  client/
    mod.rs             — run(): spawn ffmpeg publish, generate frames, handle Ctrl+C
    ffmpeg.rs          — RTMP publish args (zerolatency, baseline, 300k)
  server/
    mod.rs             — run(): resolve URL, spawn ingest, decode loop, auto-restart
    vk_live.rs         — resolve_channel, wait_for_playback_ready, probe (port from reference)
    ingest.rs          — ffmpeg read subprocess with low-latency flags
    decoder.rs         — raw RGB24 read loop → flicker::decode → log Δms
```

## Flicker protocol v1

- Frame: RGB24, `256x144`, `30 fps` (`libx264`, `tune=zerolatency`, `preset=ultrafast`).
- Grid: `cell_size` px squares (must divide 256 and 144 evenly; valid: 1,2,4,8,16).
- Bit layout:
  - bits `0..64` — `timestamp_ns` (big-endian, MSB first, row-major cell index).
  - bits `64..total_cells` — reserved (zero-filled).
- Encoding: bit=1 → white (RGB 255,255,255) square; bit=0 → black (0,0,0).
- Decoding: per-cell mean channel value; `mean > 127 ⇒ 1`.
- Update cadence: every `update_every_frames` frames (env-tunable, default 3).

## Server latency minimization

1. **URL choice:** prefer HLS over DASH when both are returned by VK API. Rationale: HLS via okcdn on VK Live typically has shorter segment duration (~1–2 s) than DASH (~4–6 s).
2. **ffmpeg ingest flags:**
   - `-fflags nobuffer+discardcorrupt+flush_packets`
   - `-flags low_delay`
   - `-avioflags direct`
   - `-probesize 32k -analyzeduration 0`
   - HLS: `-live_start_index -1`, `-http_persistent 1`
   - Do **not** pass `-re` (we want to drain as fast as possible).
3. **Video filter chain:** `scale=256:144:flags=neighbor,format=rgb24,fps=30`, output raw RGB24 to stdout.
4. **Auto-reconnect:** on stdout EOF/short-read, re-run `resolve_channel` (signed URL may have expired) and respawn ffmpeg. Backoff 1s → 2s → 5s → 10s (capped).
5. **No pacing on read side:** drain ffmpeg stdout as fast as possible; accept bursty frames from segment boundaries.

## Config (.env)

Existing (kept):
- `stream_key` — RTMP stream key (client).
- `rtmp_server` — RTMP publish URL base (client).
- `cell_size` — grid cell px (both).
- `update_every_frames` — timestamp refresh cadence (client).

New (server):
- `vk_live_channel` — VK Live slug (e.g. `pavel8899`). Required unless `stream_read_url` set.
- `stream_read_url` — explicit MPD/HLS URL, overrides VK resolve.
- `stream_referer`, `stream_origin`, `stream_user_agent` — HTTP headers for okcdn.
- `stream_log_every_frame` — `1/true` to log every frame; default logs only on ts change.
- `VK_LIVE_WAIT_INTERVAL_SECS`, `VK_LIVE_WAIT_TIMEOUT_SECS`, `VK_LIVE_SKIP_PROBE` — resolve-retry tuning (inherited from reference).

## Logging

On each decoded frame where `ts_ns` changes (or always, with `stream_log_every_frame=1`):

```
[flicker] frame=<N> ts_ns=<u64> Δ=<ms> ms
```

`Δ = (local_now_ns − ts_ns) / 1_000_000`. May be negative briefly if client/server clocks drift — log as-is, no clamping.

## Dependencies (additions)

- `clap = { version = "4", features = ["derive"] }`
- `reqwest = { version = "0.12", default-features = false, features = ["blocking","json","rustls-tls"] }`
- `serde_json = "1"`

## Non-goals reaffirmed

- No graceful mid-stream reconfig (restart client to change grid).
- No payload beyond timestamp in v1 (reserved bits ready for v2).
- No TLS on RTMP output (VK ingest is `rtmp://`).
