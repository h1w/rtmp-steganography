#!/usr/bin/env bash
# Level D — VK end-to-end smoke test.
# Starts a single peer publishing + reading its OWN stream via VK, measures
# delivery rate over 60 seconds, emits a summary report.
#
# Required env (set in .env or exported):
#   peer_my_rtmp_url
#   peer_my_stream_key
#   peer_their_vk_channel   (your own channel — self-loop)
#   peer_their_stream_name
set -euo pipefail
cd "$(dirname "$0")/.."

: "${peer_my_rtmp_url:?set peer_my_rtmp_url}"
: "${peer_my_stream_key:?set peer_my_stream_key}"
: "${peer_their_vk_channel:?set peer_their_vk_channel}"
: "${peer_their_stream_name:?set peer_their_stream_name}"

export flicker_log_every_frame=1

LOG="$(mktemp)"
echo "[e2e] log: $LOG"
echo "[e2e] running peer for 60s..."

cargo build --release
timeout 60 ./target/release/rtmp-steganography peer 2>&1 | tee "$LOG" || true

echo ""
echo "[e2e] results:"
SENT=$(grep -c 'time_sync ts=' "$LOG" || true)
RCVD=$(grep -c '\[app\] time_sync' "$LOG" || true)
DROPS=$(grep -c 'dropped:' "$LOG" || true)
echo "  time_sync sent: $SENT"
echo "  time_sync rcvd: $RCVD"
echo "  frames dropped: $DROPS"
if [ "$SENT" -gt 0 ]; then
  RATE=$(( 100 * RCVD / SENT ))
  echo "  delivery rate: ${RATE}%"
  if [ "$RATE" -ge 90 ]; then
    echo "[e2e] PASS (>= 90%)"
    exit 0
  else
    echo "[e2e] FAIL (below 90% delivery)"
    exit 1
  fi
fi
echo "[e2e] INCONCLUSIVE (no time_sync traffic observed)"
exit 2
