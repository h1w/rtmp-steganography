# Flicker Protocol v2 — Design Spec

**Date:** 2026-04-19
**Status:** Approved for implementation
**Supersedes:** `2026-04-19-flicker-protocol-design.md` (v1 — will be deleted)

## 1. Goal

Design a lossy, high-throughput, bidirectional steganographic data channel over VK Live RTMP → HLS. Each peer simultaneously publishes and consumes a video stream; real data is encoded into pixel patterns that survive YUV 4:2:0 subsampling, x264 lossy compression, loop-filter smoothing, scaler interpolation, and possible transcoding at VK's ingest.

v2 is a **clean rewrite** of the entire flicker stack — v1 (grayscale 16×16 grid carrying only a 64-bit timestamp) is deleted, no backward compatibility.

## 2. Non-goals

- Reliable delivery (ARQ, retransmission, ordered streams). Channel is **UDP-like**: datagrams in, datagrams out, lost frames stay lost.
- Encryption / authentication of payload. Orthogonal concern, can layer above.
- Adaptive bitrate / resolution / fps at runtime. Peers negotiate via config, not in-band.
- TLS on RTMP output (VK ingest is `rtmp://`).
- DCT-domain embedding (QIM) — left for v3 if pixel-domain hits a wall.
- LDPC / Raptor FEC — header has a `fec_scheme` slot for future upgrade; v2 ships with RS only.

## 3. Throughput and inspiration

**Inspiration: SDH/STM frame model.** Every video frame is a fixed-size container with a small overhead zone (analogous to SOH+POH) and a payload zone (analogous to VC-4). FEC is layered (like SDH's B1/B2/B3): a tight RS over the header, independent RS blocks over payload, with byte-level interleaving to fight burst errors from x264 artefacts.

**Key deviations from SDH:**
- No pointer justification — fps is fixed, drift is the player's problem; duplicate frames (HLS segment-boundary replays) are detected via `frame_counter` and dropped.
- No BIP-8 over bytes — we use **soft-decision** (per-cell confidence) to generate erasure hints before RS decode, yielding ~2 dB effective coding gain.
- Single-hop topology (peer ↔ VK ↔ peer), but with an opaque transcoder in the middle that behaves like a very noisy regenerator.

**Honest throughput numbers (after all overhead, fragment header, and FEC):**

| Mode | Bits/cell | Cells | App throughput @ 24 fps |
|------|-----------|-------|-------------------------|
| B (baseline — luma only, 4 levels) | 2 | 2304 | **~5.5 KB/s** |
| C (upgrade — Y + U + V) | 4 | 2304 | **~11.3 KB/s** |

## 4. Physical layer

### 4.1 Video frame parameters

Unchanged from v1: RGB24 raw on the wire between ffmpeg and flicker code, `256×144` resolution, `24 fps`. libx264 baseline profile, `tune=zerolatency`, `preset=ultrafast`, 300 kbit/s CBR, `yuv420p`.

### 4.2 Grid

- **Cell size:** `4×4 px` (fixed). Grid: `64 cols × 36 rows = 2304 cells`.
- **Readout region per cell:** central `2×2 px` only (1 px guard band on each side). This isolates a cell from its neighbours through the x264 deblocking filter and yuv420 chroma subsampling boundary.
- **Cell values:** 4 discrete luma levels `{32, 96, 160, 224}` — 2 bits per cell in mode B. Levels chosen centred away from BT.601/709 limited-range clipping zones (`[0, 16]` and `[235, 255]`) so scaler range-conversion survives.

### 4.3 Modulation modes

| Mode | Description | Bits/cell |
|------|-------------|-----------|
| **B (baseline)** | Luma only, 4 levels | 2 |
| **C (upgrade)** | Y (4 levels) + U (2 levels) + V (2 levels) | 4 |

- Mode is selected **by the sender** via config (`flicker_modulation_mode=B|C`) and declared in the frame header (`modulation_mode: u8`).
- Receiver auto-adapts via header and pilot validation: if the header says C but U/V pilot cells fail confidence threshold ⇒ decode as if it were B for this frame (⇒ likely to fail RS, frame dropped, logged).
- **Why chroma works only at cell ≥ 4×4:** yuv420 subsamples chroma 2× in each axis; a 4×4 luma cell corresponds to a 2×2 chroma cell — the smallest size where U/V modulation is retrievable after round-trip.

### 4.4 Corner markers

- **4 markers, each `16×16 px`**, placed at the absolute four corners of the frame.
- **Pattern:** fixed `2×2` checkerboard of the extreme levels (`32` and `224`), repeating 8 times per axis inside the `16×16` — gives a strong correlation peak for `cross-correlation` alignment.
- **Purpose:** every frame, decoder does `±8 px` local cross-correlation around each theoretical corner to find the real marker centre, then builds an **affine transform** (`a·x + b·y + tx`, etc.) that maps logical grid coordinates to the actual pixel grid. Resilient to ±1–2 px scaler jitter and mild shear.
- Markers are **the first thing read** in the decode pipeline; without them, nothing else is attempted.

### 4.5 Pilot cells

- **~5% of the grid** (~115 cells) are pilots: their values are derived deterministically from `frame_counter` via a shared-seed PRNG (ChaCha8 keyed by a constant + `frame_counter`).
- **Positions** of pilots are also PRNG-derived (same PRNG, different domain), so pilots move every frame — preventing a spatially-localised transcode artefact from always hitting pilots.
- **Purpose:**
  1. **Brightness/contrast bias estimation** — if the transcoder added a vignette or gamma shift, pilots give a per-region affine correction `y' = a·y + b` before level thresholding.
  2. **Alignment validation** — if >20% of pilots fail confidence check after bias correction, the corner-based alignment is untrustworthy → drop the entire frame without attempting RS decode.
  3. **Modulation mode auto-fallback** — separate pilot set in U/V tells decoder if chroma channel is carrying information or is mud.

### 4.6 Interleaving

Two-level, deterministic, both sides share the permutation.

- **Level 1 — spatial permutation across frame:** payload+parity cells are scattered pseudo-randomly over the grid so any spatial burst artefact (a corrupted macroblock region) is distributed across multiple RS blocks and multiple positions within each RS block.
- **Level 2 — byte-level interleave within an RS block:** standard depth-`n` interleave, so a short byte-run burst error maps to scattered single-byte errors that RS can correct.

Both permutations are seeded by a constant (compiled-in), **not** `frame_counter` — position of each cell must be stable frame-to-frame for hardware caching.

## 5. Frame layout (logical, after spatial de-permutation)

Total cells per frame: `2304`.

```
┌─────────────────────────────────────────────────┐
│ Corner markers       4 × (16×16 px) = 64 cells  │  — fixed content, ~2.8%
├─────────────────────────────────────────────────┤
│ Header zone          132 cells = 33 bytes @2bpp │  — RS(33,22), ~5.7%
├─────────────────────────────────────────────────┤
│ Pilot cells          ~115 cells                 │  — PRNG-generated, ~5%
├─────────────────────────────────────────────────┤
│ Payload+parity zone  ~1993 cells                │  — RS block count
│                      = ~498 bytes (mode B)      │    scales with mode:
│                      = ~996 bytes (mode C)      │    B: 2 × RS(172,120)
│                                                 │    C: 4 × RS(172,120)
│                                                 │  ~86.5%
└─────────────────────────────────────────────────┘
Total overhead budget: ~13.5% (header + markers + pilots)
```

Note: header and pilot zones consume cells at the raw 2-bpp rate regardless of modulation mode (they're read in luma only, for robustness — corner detection and pilot validation cannot depend on chroma surviving). Only the payload+parity zone benefits from mode C's extra bits per cell.

### 5.1 Main header (22 bytes data, protected by RS(33,22))

```
Offset  Size   Field                Description
------  ----   -----                -----------
0       4      SYNC_WORD            0xF1 0x1C 0x4E 0x52
4       1      protocol_version     0x02
5       4      frame_counter        u32 LE, wraps; PRNG seed for pilots
9       1      channel_id           0x01 = A→B, 0x02 = B→A
10      1      modulation_mode      1=B (2bpp Y), 2=C (4bpp YUV)
11      1      fec_scheme           1=RS(255,k) over GF(256)
12      4      fec_params           [n, k, block_count, flags]
16      2      payload_len          u16 LE, used bytes in payload zone (0..480)
18      4      header_crc32         CRC32 of bytes 0..18
                                    (integrity check after RS correction)
------  ----
22 bytes data + 11 bytes RS parity = 33 bytes = 132 cells @ 2 bits/cell
RS(33, 22) corrects up to 5 byte errors in the header
```

Sync word `0xF1 0x1C 0x4E 0x52` doubles as a quick visual confirmation that the frame was parsed correctly ("F1 1C KE R" — `flicker`).

### 5.2 Payload zone structure

- **RS block type:** `RS(172, 120)` over GF(256). One block = 120 data bytes + 52 parity bytes; independently corrects up to **26 byte errors** or **52 erasures** (via soft-decision erasure flags from pilot calibration).
- **Block count scales with modulation mode** (declared in `fec_params.block_count`):
  - **Mode B** (2 bpp payload cells): **2 blocks** × 172 bytes = 344 bytes used of ~498 available. Tail = ~154 bytes reserved.
  - **Mode C** (4 bpp payload cells): **4 blocks** × 172 bytes = 688 bytes used of ~996 available. Tail = ~308 bytes reserved.
- **Per-frame raw payload capacity:** 240 bytes (B) / 480 bytes (C).
- **Tail bytes** zero-filled in v2.0, reserved for v2.1 extended metadata (routing, priority hints, multi-message fragment indexing).
- **FEC split:** parity takes ~30.2% of each block (52/172), regardless of mode.

### 5.3 Fragment header (inside payload, per encoded message fragment)

Datagram API with auto-fragmentation. One or more fragments per frame (back-to-back in payload zone).

```
Offset  Size   Field                Description
------  ----   -----                -----------
0       1      msg_type             Application message type;
                                    high bit = HAS_NEXT (another fragment
                                    follows in this same frame).
                                    Reserved: 0x00 = ignore, 0x01 = time_sync,
                                    0x02 = app_data, 0x03..0x7F = free.
1       4      message_id           u32 LE, wraps. Groups fragments of
                                    one logical message.
5       2      fragment_idx         u16 LE, zero-based.
7       2      fragment_total       u16 LE, total fragments for message_id.
                                    1 = single-fragment message.
9       N      fragment_payload     Up to payload-zone-remaining.
                                    Short messages: N ≤ 231 bytes (B mode)
                                    or N ≤ 471 bytes (C mode) in a
                                    single-fragment single-message frame.
```

### 5.4 Reassembly buffer

- Map `message_id → Vec<Option<FragmentPayload>>`.
- **Timeout:** 2 seconds of `frame_counter` wall time (configurable via env). On timeout, partial message dropped with a warning log.
- **UDP semantics preserved:** any missing fragment ⇒ entire message dropped silently (lost fragment never retransmitted, no ARQ).
- **Duplicate detection:** if `(message_id, fragment_idx)` arrives twice, second occurrence ignored (tolerates HLS segment-boundary re-delivery).

## 6. Soft-decision / erasure marking

For every cell in payload/header zone:
- Decoder computes `value ∈ {0, 1, 2, 3}` by closest-centre to the 4 levels.
- Decoder also computes `confidence ∈ [0, 1]` = `1 - normalised_distance_to_second_closest_level`.
- Cells with `confidence < 0.4` (tunable threshold) are marked as **erasures** in the RS byte grouping.
- RS with erasures corrects up to `2t` positions where `t = (n-k)/2`, vs. `t` positions for unknown errors. Doubles effective correction capability when we know which cells are untrusted.

## 7. Process architecture

### 7.1 CLI

Single operational mode: **`peer`**. The v2 protocol is frame-symmetric, so the old v1 split into `--client` / `--server` no longer reflects anything in the wire format. Directionality is a peer-local runtime choice, expressed via flags:

```
rtmp-steganography peer                    # full duplex (both directions)
rtmp-steganography peer --publish-only     # tx thread + ffmpeg-publish only
rtmp-steganography peer --receive-only     # rx thread + ffmpeg-read only
```

`--publish-only` and `--receive-only` are mutually exclusive; omitting both means full duplex. The flags only toggle which threads the peer starts — the code path is unified.

v1's `--client` / `--server` subcommands and `rtmp-steganography client|server` forms are **removed** (part of the clean rewrite). Existing user scripts must be updated: `--client` ⇒ `peer --publish-only`, `--server` ⇒ `peer --receive-only`.

### 7.2 Peer mode internals (single process, sync threads, no async runtime)

```
┌────────────────────┐   mpsc   ┌────────────────────┐   stdin   ┌────────┐
│ app thread         │ ───────> │ tx thread          │ ────────> │ ffmpeg │
│ (send/receive API) │          │ (flicker encoder)  │           │ publish│
│                    │ <─────── │                    │           └────────┘
│                    │   mpsc   │                    │
│                    │          │                    │
│                    │ <─────── │ rx thread          │ <──────── ┌────────┐
│                    │          │ (flicker decoder)  │   stdout  │ ffmpeg │
│                    │          │                    │           │ read   │
└────────────────────┘          └────────────────────┘           └────────┘
```

- **`tx` thread:** pulls `OutboundMessage {msg_type, bytes}` from outbound mpsc; accumulates until frame capacity, emits fragments, calls flicker encoder, writes raw RGB24 to publisher ffmpeg stdin.
- **`rx` thread:** reads raw RGB24 from reader ffmpeg stdout (one full frame per `read_exact`), calls flicker decoder, pushes reassembled `InboundMessage {msg_type, bytes}` into inbound mpsc.
- **`app` thread:** owns public API (`send(msg_type, payload)`, `recv() -> InboundMessage` or callback-style). For MVP the built-in `app` behaviour is: send `time_sync` heartbeat every second, log incoming.
- **Shared `Arc<AtomicBool> running`** for Ctrl+C cooperative shutdown.
- `--publish-only` starts only the `tx` thread (and a minimal `app` stub that feeds the outbound mpsc). `--receive-only` starts only the `rx` thread. No flags = both.

### 7.3 Module layout

```
src/
  main.rs
  cli.rs                       — clap: `peer` subcommand + --publish-only / --receive-only
  config.rs                    — .env loader, single load_peer()
  flicker/
    mod.rs                     — pub API: PeerConfig, encode_frame, decode_frame
    grid.rs                    — fixed 256×144 / 4×4 / 64×36 cells
    levels.rs                  — {32,96,160,224} LUT, soft-decision utilities
    markers.rs                 — corner-marker pattern, cross-correlation, affine fit
    pilot.rs                   — PRNG-driven pilot positions and values, bias estimation
    interleave.rs              — spatial + byte-level permutation
    header.rs                  — main header (22B) + RS(33,22) encode/decode, sync word scan
    fec.rs                     — RS(255,k) wrapper over reed-solomon-simd or similar
    fragment.rs                — fragment header, reassembly buffer, timeout
    codec.rs                   — paint_cell / read_cell (4-level + multi-channel)
    frame.rs                   — orchestration: encode_frame(outbound) / decode_frame(buf)
  peer/
    mod.rs                     — run_peer(Direction): spawn tx/rx/app per direction mask
    app.rs                     — default app behaviour (heartbeat + log)
    ffmpeg_publish.rs          — RTMP publish subprocess (ex-src/client/ffmpeg.rs)
    ffmpeg_read.rs             — HLS/DASH read subprocess (ex-src/server/ingest.rs)
    vk_live.rs                 — VK channel resolve (ex-src/server/vk_live.rs)
tests/
  flicker_roundtrip.rs         — Level B: pure in-memory encode↔decode
  flicker_lossy.rs             — Level B: round-trip + synthetic damage matrix
  flicker_ffmpeg.rs            — Level C: gated by `ffmpeg-integration` feature
scripts/
  e2e-vk.sh / e2e-vk.ps1       — Level D: manual VK loopback smoke test
docs/
  testing-e2e.md               — Level D procedure + acceptance criteria
```

## 8. Configuration

All via `.env`. New keys in v2 (alongside existing `stream_resolution`, `stream_fps`, etc.):

| Key | Default | Used by | Purpose |
|-----|---------|---------|---------|
| `flicker_modulation_mode` | `B` | tx direction | `B` or `C` |
| `flicker_cell_size` | `4` | all | Cell size in px (fixed at 4 for MVP) |
| `flicker_fec_rate` | `0.30` | tx direction | Future: runtime-tunable FEC rate |
| `flicker_frag_timeout_ms` | `2000` | rx direction | Reassembly buffer TTL |
| `flicker_pilot_confidence` | `0.4` | rx direction | Erasure threshold |

Peer topology (4 fields):
- `peer_my_rtmp_url`, `peer_my_stream_key` — our publish endpoint (required when tx direction active)
- `peer_their_vk_channel`, `peer_their_stream_name` — their VK Live slug (required when rx direction active)

When `--publish-only` is set, only the tx-side keys are validated at startup; `--receive-only` only validates rx-side keys. Full duplex requires all four.

v1 keys (`client_stream_key`, `rtmp_server`, `vk_live_channel`, `client_stream_name`) are **removed** — migration: rename to the `peer_my_*` / `peer_their_*` equivalents.

## 9. Logging

Per-frame decode log (replaces v1's `[flicker] frame=... ts_ns=... Δ=... ms`):

```
[flicker] rx frame=<N> ch=<A|B> mode=<B|C> payload_len=<bytes> \
          fec_corrected=<bytes> fec_erasures=<cells> \
          pilot_fail=<N/115> confidence_p50=<0.00-1.00> \
          fragments={msg_id:<id> idx:<i>/<total>, ...} \
          drop_reason=<sync_miss|header_crc|rs_uncorrectable|pilot_fail|none>
```

Per-message log when a fragment set completes:
```
[flicker] msg_delivered ch=<A|B> msg_type=<0x..> msg_id=<id> \
          bytes=<len> fragments=<n> latency_ms=<N>
```

Verbose mode (`flicker_log_every_frame=1`) prints every frame even on no-change; otherwise coalesces.

## 10. Testing strategy

### 10.1 Level B — unit + synthetic lossy tests (mandatory, CI-gated)

`tests/flicker_roundtrip.rs` — pure bit-exact round-trip without ffmpeg; asserts `encode(x) → decode → x` for a range of `msg_type`, `payload_len`, and multi-fragment messages.

`tests/flicker_lossy.rs` — matrix of damage functions applied between encode and decode:

| Damage | Parameters tested | Assertion |
|--------|-------------------|-----------|
| `gaussian_noise(σ)` | σ ∈ {5, 15, 30} | σ=5,15 bit-exact; σ=30 soft-fail OR bit-exact |
| `pixel_flip(rate)` | rate ∈ {1%, 5%, 15%} | 1%, 5% bit-exact; 15% soft-fail OR bit-exact |
| `spatial_shift(dx,dy)` | (±1, ±2) | bit-exact (tests corner detection) |
| `brightness_bias(δ)` | ±20, ±40 | bit-exact (tests pilot calibration) |
| `gamma_jitter(γ)` | 0.8, 1.2 | bit-exact |
| `block_corruption(8, 3)` | 3 blocks of 8×8 random | bit-exact (tests interleaving) |

**"Soft-fail"** means: the frame is marked corrupted and dropped; the test passes if the protocol **never delivers wrong data upstream** — only `Ok(message)` or `Drop(frame)` are acceptable outcomes.

### 10.2 Level C — ffmpeg integration round-trip (mandatory, `ffmpeg-integration` feature flag)

`tests/flicker_ffmpeg.rs`:
- Spawns ffmpeg-encode with exact client-mode flags.
- Pipes 50 test frames carrying known payload through ffmpeg-encode → tempfile/FIFO → ffmpeg-decode.
- Verifies ≥48/50 frames bit-exact delivered, ≥49/50 messages reassembled.
- Gated behind `--features ffmpeg-integration` so default `cargo test` stays clean.
- Separate CI job installs ffmpeg and runs with the feature flag.

### 10.3 Level D — VK end-to-end loopback (mandatory, manual, documented)

`scripts/e2e-vk.sh` + `scripts/e2e-vk.ps1`:
- Starts two peer instances on localhost pointed at the same VK Live channel (self-loop: peer publishes in, same peer reads own stream out).
- Runs 60 seconds, collects: frames_sent, frames_received, frames_decoded, frames_dropped (by reason), messages_sent, messages_delivered, bit-error rate, latency p50/p95/p99.
- Acceptance (MVP threshold): ≥90% frames decoded, ≥95% messages delivered bit-exact, p50 latency ≤ 4 s, p99 ≤ 10 s.
- `docs/testing-e2e.md` documents: VK Live account setup, env config template, how to run the script, how to read the results, diagnostic steps on failure (ffmpeg log analysis, pilot miss-rate interpretation, tcpdump procedure).
- **Not** in CI; run manually before merging any PR that changes the protocol.

## 11. Implementation dependencies (Cargo.toml additions)

- `reed-solomon-simd = "3"` (or `reed-solomon-erasure = "6"` if SIMD unavailable) — RS(255,k)
- `crc32fast = "1"` — header CRC
- `rand_chacha = "0.3"` + `rand_core = "0.6"` — pilot PRNG (deterministic)
- (already present from v1: `clap`, `reqwest`, `serde_json`, `anyhow`)

## 12. Open items deferred to v2.1 / v3

- **DCT-domain embedding (QIM)** — only if pixel-domain hits a wall against aggressive VK transcoder behaviour.
- **LDPC / Raptor FEC** — swap `fec_scheme=2` without rest of the protocol changing.
- **Adaptive modulation mode** — per-N-second negotiation based on measured BER.
- **Extended header metadata** — routing hints, QoS, message priority (154-byte tail in mode B).
- **Multiple logical channels per stream** — current `channel_id: u8` allows 255; only 2 in use (A→B, B→A).

## 13. Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| VK transcodes to <200 kbit/s, 4×4 cells smudge | Fallback to 8×8 mode A variant (empirical from Level D test). Add as fallback in v2.1. |
| x264 deblocking eats 1-px guard, leaks between cells | Guard band is 1 px; if insufficient, widen to 2 px (cell 6×6, grid 42×24 = 1008 cells → 2.5 KB/s). Parameterisable. |
| HLS segment boundary drops 1–3 frames | UDP semantics — lost messages stay lost. Application can re-send on timeout (that's app concern). |
| Corner marker collision with video letterbox | Markers are in-frame at absolute corners; VK's HLS output is not letterboxed for 256×144 — verified during v1. |
| Pilot PRNG desync between peers (version drift) | PRNG seed is constant + `frame_counter`; `protocol_version` in header gates compat. |
| Clock skew between peers breaks `time_sync` | `time_sync` is an app-level message; handled outside protocol. |

## 14. Acceptance checklist

This spec is implementable when all of the following are satisfied:

- [ ] All v1 flicker code removed (`src/flicker/*.rs` replaced wholesale).
- [ ] `cargo test` passes Level B without ffmpeg.
- [ ] `cargo test --features ffmpeg-integration` passes Level C with ffmpeg on PATH.
- [ ] Level D script produces a results report meeting MVP thresholds on at least one successful run against real VK Live.
- [ ] `peer` (full duplex) runs to steady state for ≥60 s with no reassembly leaks, no panics, clean Ctrl+C.
- [ ] `peer --publish-only` and `peer --receive-only` each run standalone for ≥60 s with the non-active direction quiescent (no ffmpeg subprocess for that direction, no leaked threads).
- [ ] Migration note in README: old `--client` / `--server` invocations replaced by `peer --publish-only` / `peer --receive-only`, old `.env` keys renamed to `peer_*`.
