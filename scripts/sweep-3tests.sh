#!/usr/bin/env bash
# 3 back-to-back live tests at 640x360 cell=8 Mode C:
#   1) CBR 6000
#   2) CBR 8000
#   3) CRF 22 + maxrate 5000  (semantic "qp=22 --maxrate 5M")
set -u
cd "$(dirname "$0")/.."
mkdir -p metrics/live
RESULTS="metrics/live/sweep-3tests.md"
: > "$RESULTS"
echo "| test | mode | ok | bytes_rx | elapsed_ms | kbit/s | rx_frames | dropped | HdrRs | BlockRs | PayloadCrc | fail_stage | x264_q_sample |" >> "$RESULTS"
echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|" >> "$RESULTS"

run_test() {
  local label="$1" ; shift
  local desc="$1"  ; shift
  # remaining args = env KEY=VAL lines to write
  echo ""
  echo "========================================================"
  echo "[$label] $desc"
  echo "========================================================"
  taskkill //F //IM rtmp-steganography.exe >/dev/null 2>&1 || true
  sleep 2
  for f in .env.peer-a .env.peer-b; do
    grep -v -E '^peer_x264_qp=|^peer_x264_bitrate_kbps=|^peer_x264_crf=|^peer_x264_maxrate_kbps=' "$f" > "${f}.tmp"
    for kv in "$@"; do echo "$kv" >> "${f}.tmp"; done
    mv "${f}.tmp" "$f"
  done
  [ -f .env ] && mv .env .env.bak.sw3 2>/dev/null || true
  export METRICS_DIR=./metrics TUNNEL_PROFILE=throughput
  local LOG_A="metrics/live/peer-a-${label}.log"
  local LOG_B="metrics/live/peer-b-${label}.log"
  rm -f "$LOG_A" "$LOG_B"
  (set -a; source .env.peer-a; export PEER_ID=A TUNNEL_PROFILE METRICS_DIR; \
    exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11080) > "$LOG_A" 2>&1 &
  sleep 3
  (set -a; source .env.peer-b; export PEER_ID=B TUNNEL_PROFILE METRICS_DIR; \
    exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11081 --with-bench-support) > "$LOG_B" 2>&1 &
  sleep 3
  echo "[$label] warmup 75s"; sleep 75
  PEER_ID=A ./target/release/rtmp-steganography bench smoke \
    --socks 127.0.0.1:11080 --iterations 0 --skip-iperf --throughput-bytes 1024 \
    --raw-echo-host 127.0.0.1 --raw-echo-port 18090 \
    --metrics-dir ./metrics > /tmp/sw3-${label}.log 2>&1 || true

  local latest=$(find metrics -name events.jsonl -mmin -3 2>/dev/null | sort | tail -1)
  local tl=$(grep '"event":"throughput_done"' "$latest" | tail -1)
  local ok=$(echo "$tl" | grep -oE '"ok":(true|false)' | cut -d: -f2)
  local bytes_rx=$(echo "$tl" | grep -oE '"bytes_received":[0-9]+' | grep -oE '[0-9]+')
  local elapsed=$(echo "$tl" | grep -oE '"elapsed_ms":[0-9]+' | grep -oE '[0-9]+')
  local kbps=$(echo "$tl" | grep -oE '"oneway_kbits_per_s":[0-9.]+' | grep -oE '[0-9.]+$')
  local fail=$(echo "$tl" | grep -oE '"fail_stage":"[^"]*"' | sed 's/"fail_stage":"//;s/"$//')
  local rx=$(grep -c 'rx frame=' "$LOG_A" 2>/dev/null || echo 0)
  local drop=$(grep -c 'dropped:' "$LOG_A" 2>/dev/null || echo 0)
  local hdr=$(grep -c HeaderRsFailed "$LOG_A" 2>/dev/null || echo 0)
  local blk=$(grep -c BlockRsFailed "$LOG_A" 2>/dev/null || echo 0)
  local crc=$(grep -c PayloadCrcMismatch "$LOG_A" 2>/dev/null || echo 0)
  local qsam=$(grep -oE 'q=[0-9.]+' "$LOG_A" | tail -1)

  printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n" \
    "$label" "$desc" "${ok:-?}" "${bytes_rx:-0}" "${elapsed:-0}" "${kbps:-0}" \
    "$rx" "$drop" "$hdr" "$blk" "$crc" "${fail:--}" "${qsam:--}" >> "$RESULTS"
  echo "[$label] ok=${ok:-?} rx=$rx drop=$drop blk=$blk kbps=${kbps:-0} q=${qsam:--}"

  taskkill //F //IM rtmp-steganography.exe >/dev/null 2>&1 || true
  sleep 2
  [ -f .env.bak.sw3 ] && mv .env.bak.sw3 .env 2>/dev/null || true
}

run_test "cbr6M"        "CBR 6000"          "peer_x264_bitrate_kbps=6000"
run_test "cbr8M"        "CBR 8000"          "peer_x264_bitrate_kbps=8000"
run_test "crf22_mx5M"   "CRF 22 maxrate 5M" "peer_x264_crf=22" "peer_x264_maxrate_kbps=5000"

echo ""
echo "=== SWEEP DONE ==="
cat "$RESULTS"
