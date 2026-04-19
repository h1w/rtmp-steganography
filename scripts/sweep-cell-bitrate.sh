#!/usr/bin/env bash
# Sweep cell_size × x264 bitrate knobs on live VK, 1 KB throughput / 30 s each.
# Prints a markdown table at the end.

set -u
cd "$(dirname "$0")/.."

# config list: "label cell bitrate_kbps_or_qp mode"
#   mode = "cbr" or "qp"
CONFIGS=(
  "cell16_cbr2M   16 2000  cbr"
  "cell16_cbr4M   16 4000  cbr"
  "cell16_cbr8M   16 8000  cbr"
  "cell16_qp22    16 22    qp"
  "cell8_cbr2M    8  2000  cbr"
  "cell8_cbr4M    8  4000  cbr"
  "cell8_cbr8M    8  8000  cbr"
  "cell8_qp22     8  22    qp"
  "cell8_qp30     8  30    qp"
  "cell4_cbr4M    4  4000  cbr"
  "cell4_qp22     4  22    qp"
  "cell2_qp22     2  22    qp"
)

RESULTS_MD="metrics/live/sweep-results.md"
mkdir -p metrics/live
: > "$RESULTS_MD"
echo "| config | cell | mode | bitrate/qp | vk_online | rx_frames | HdrRs | bytes_recv | elapsed_s | kbits/s | fail_stage |" >> "$RESULTS_MD"
echo "|---|---|---|---|---|---|---|---|---|---|---|" >> "$RESULTS_MD"

for line in "${CONFIGS[@]}"; do
  read -r label cell val mode <<<"$line"
  echo ""
  echo "========================================================"
  echo "[$label] cell=$cell mode=$mode val=$val"
  echo "========================================================"

  # kill any running peers
  taskkill //F //IM rtmp-steganography.exe >/dev/null 2>&1 || true
  sleep 2

  # build env files: remove stale knobs, append fresh
  for f in .env.peer-a .env.peer-b; do
    grep -v -E '^peer_flicker_cell_size=|^peer_x264_qp=|^peer_x264_bitrate_kbps=|^peer_stream_width=|^peer_stream_height=' "$f" > "${f}.tmp"
    echo "peer_stream_width=1280"           >> "${f}.tmp"
    echo "peer_stream_height=720"           >> "${f}.tmp"
    echo "peer_flicker_cell_size=${cell}"   >> "${f}.tmp"
    if [ "$mode" = "qp" ]; then
      echo "peer_x264_qp=${val}"            >> "${f}.tmp"
    else
      echo "peer_x264_bitrate_kbps=${val}"  >> "${f}.tmp"
    fi
    mv "${f}.tmp" "$f"
  done

  # move default .env aside
  [ -f .env ] && mv .env .env.bak.sweep 2>/dev/null || true

  export METRICS_DIR=./metrics TUNNEL_PROFILE=throughput
  LOG_A="metrics/live/peer-a-${label}.log"
  LOG_B="metrics/live/peer-b-${label}.log"
  rm -f "$LOG_A" "$LOG_B"

  (set -a; source .env.peer-a; export PEER_ID=A TUNNEL_PROFILE; \
    exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11080) > "$LOG_A" 2>&1 &
  sleep 3
  (set -a; source .env.peer-b; export PEER_ID=B TUNNEL_PROFILE; \
    exec ./target/release/rtmp-steganography peer --tunnel-socks 127.0.0.1:11081 --with-bench-support) > "$LOG_B" 2>&1 &
  sleep 3

  echo "[$label] warmup 60s"
  sleep 60

  # Check VK online
  curl -sSL --max-time 10 -H "User-Agent: Mozilla/5.0" \
    "https://live.vkvideo.ru/pavel8899/stream/stream1" -o metrics/live/vk-check.html 2>/dev/null || true
  vk_online=$(node -e "
const fs=require('fs');
try {
  const html = fs.readFileSync('metrics/live/vk-check.html','utf8');
  const m = html.match(/<script[^>]*id=['\"]initial-state['\"][^>]*>([\s\S]*?)<\/script>/);
  const d = JSON.parse(m[1]);
  console.log(d.stream.stream.data.stream.isOnline ? 'YES' : 'no');
} catch(e){ console.log('err'); }
" 2>/dev/null)

  # run 1KB 30s bench
  PEER_ID=A ./target/release/rtmp-steganography bench smoke \
    --socks 127.0.0.1:11080 --iterations 0 --skip-iperf \
    --throughput-bytes 1024 \
    --raw-echo-host 127.0.0.1 --raw-echo-port 18090 \
    --metrics-dir ./metrics > /tmp/sweep-${label}.log 2>&1 || true

  latest=$(find metrics -name events.jsonl -mmin -3 2>/dev/null | sort | tail -1)
  throughput_line=$(grep '"event":"throughput_done"' "$latest" 2>/dev/null | tail -1)

  bytes_recv=$(echo "$throughput_line" | grep -oE '"bytes_received":[0-9]+' | grep -oE '[0-9]+')
  elapsed=$(echo "$throughput_line" | grep -oE '"elapsed_ms":[0-9]+' | grep -oE '[0-9]+')
  kbps=$(echo "$throughput_line" | grep -oE '"oneway_kbits_per_s":[0-9.]+' | grep -oE '[0-9.]+$')
  fail=$(echo "$throughput_line" | grep -oE '"fail_stage":"[^"]*"' | sed 's/"fail_stage":"//;s/"$//')
  rx_frames=$(grep -c 'rx frame=' "$LOG_A" 2>/dev/null || echo 0)
  hdr_rs=$(grep -c HeaderRsFailed "$LOG_A" 2>/dev/null || echo 0)
  elapsed_s=$(awk -v ms=${elapsed:-0} 'BEGIN { printf "%.1f", ms/1000 }')

  printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n" \
    "$label" "$cell" "$mode" "$val" "${vk_online:-err}" "$rx_frames" "$hdr_rs" "${bytes_recv:-0}" "$elapsed_s" "${kbps:-0}" "${fail:--}" >> "$RESULTS_MD"

  echo "[$label] vk=${vk_online:-err} rx=$rx_frames HdrRs=$hdr_rs bytes_recv=${bytes_recv:-0} kbps=${kbps:-0} fail=${fail:--}"

  taskkill //F //IM rtmp-steganography.exe >/dev/null 2>&1 || true
  sleep 2
  [ -f .env.bak.sweep ] && mv .env.bak.sweep .env 2>/dev/null || true
done

echo ""
echo "=== SWEEP DONE ==="
cat "$RESULTS_MD"
