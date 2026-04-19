#!/usr/bin/env bash
# Live-run driver — peer A tunnel, peer B tunnel + bench support services.
set -euo pipefail
cd "$(dirname "$0")/.."

METRICS_DIR="${METRICS_DIR:-./metrics}"
TUNNEL_PROFILE="${TUNNEL_PROFILE:-latency}"
SOCKS_A="${SOCKS_A:-127.0.0.1:11080}"
SOCKS_B="${SOCKS_B:-127.0.0.1:11081}"
WARMUP_S="${WARMUP_S:-45}"

LOG_A="metrics/live/peer-a.log"
LOG_B="metrics/live/peer-b.log"
mkdir -p metrics/live
rm -f "$LOG_A" "$LOG_B"

BAK=".env.bak.$$"
if [ -f .env ]; then mv .env "$BAK"; fi
trap 'taskkill //F //IM rtmp-steganography.exe 2>/dev/null; if [ -f "$BAK" ]; then mv "$BAK" .env 2>/dev/null || true; fi' EXIT

echo "[bench] starting peer A"
(set -a; source .env.peer-a; export PEER_ID=A TUNNEL_PROFILE METRICS_DIR; \
  exec ./target/release/rtmp-steganography peer --tunnel-socks "$SOCKS_A") > "$LOG_A" 2>&1 &
sleep 3

echo "[bench] starting peer B (with bench support)"
(set -a; source .env.peer-b; export PEER_ID=B TUNNEL_PROFILE METRICS_DIR; \
  exec ./target/release/rtmp-steganography peer --tunnel-socks "$SOCKS_B" --with-bench-support) > "$LOG_B" 2>&1 &
sleep 3

echo "[bench] warmup ${WARMUP_S}s for VK HLS first segments"
sleep "$WARMUP_S"

echo "[bench] running bench realistic (reduced iterations; via peer A SOCKS5)"
PEER_ID=A ./target/release/rtmp-steganography bench realistic \
    --socks "$SOCKS_A" \
    --metrics-dir "$METRICS_DIR" || echo "[bench] realistic returned nonzero; continuing"

echo "[bench] done — logs in $LOG_A, $LOG_B; metrics in $METRICS_DIR"
