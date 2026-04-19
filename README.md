# rtmp-steganography

Bidirectional, UDP-like steganographic data channel over RTMP/HLS video streams.
v2 of the "flicker" protocol — each video frame carries ~236 bytes (mode B) or
~476 bytes (mode C) of application payload, protected by Reed-Solomon FEC and
a payload-level CRC32.

## Quick start

```bash
cargo build --release

# Full duplex (publish + receive simultaneously)
./target/release/rtmp-steganography peer

# Publish only
./target/release/rtmp-steganography peer --publish-only

# Receive only
./target/release/rtmp-steganography peer --receive-only
```

## Configuration

`.env` at repo root:

```env
peer_my_rtmp_url=rtmp://host/live
peer_my_stream_key=<your VK key>
peer_their_vk_channel=<channel slug>
peer_their_stream_name=<stream name>

flicker_modulation_mode=B          # B (2bpp, ~5.5 KB/s) or C (4bpp, ~11.3 KB/s)
flicker_frag_timeout_ms=2000
flicker_log_every_frame=0
```

## Migration from v1

| v1 | v2 |
|----|----|
| `rtmp-steganography client` | `rtmp-steganography peer --publish-only` |
| `rtmp-steganography server` | `rtmp-steganography peer --receive-only` |
| `client_stream_key` | `peer_my_stream_key` |
| `rtmp_server` | `peer_my_rtmp_url` |
| `vk_live_channel` | `peer_their_vk_channel` |
| `client_stream_name` | `peer_their_stream_name` |

The on-the-wire protocol is completely incompatible — v1 and v2 peers cannot interoperate.

## Testing

- `cargo test` — Level B (unit + synthetic damage)
- `cargo test --features ffmpeg-integration` — Level C (libx264 round-trip)
- `./scripts/e2e-vk.sh` — Level D (manual VK end-to-end)

See `docs/testing-e2e.md` for Level D procedure and acceptance thresholds.

## Architecture

See `docs/superpowers/specs/2026-04-19-flicker-protocol-v2-design.md` for the full design.

Core idea: treat each video frame like an SDH STM frame — corner markers for alignment, header with its own RS code, pilot cells for brightness bias calibration, payload zone with interleaved RS(172,120) blocks plus a CRC32 trailer that detects silent corruption.
