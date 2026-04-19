#!/usr/bin/env bash
# Level D — tunnel pair bench via VK.
# Launches two peer processes in tunnel mode and drives bench workloads
# from peer A's side while peer B hosts support services via tunnel egress.
#
# Requires: .env.peer-a, .env.peer-b, built release binary, proxychains4, curl, iperf3.

set -euo pipefail
cd "$(dirname "$0")/.."

DURATION="${DURATION:-1800}"
METRICS_DIR="${METRICS_DIR:-./metrics}"
TUNNEL_PROFILE="${TUNNEL_PROFILE:-latency}"
SOCKS_A="${SOCKS_A:-127.0.0.1:11080}"
SOCKS_B="${SOCKS_B:-127.0.0.1:11081}"

LOG_A="$(mktemp)"
LOG_B="$(mktemp)"
BAK=".env.bak.$$"
if [ -f .env ]; then mv .env "$BAK"; fi
trap 'if [ -f "$BAK" ]; then mv "$BAK" .env 2>/dev/null || true; fi' EXIT

cargo build --release

start_peer() {
    local env_file="$1" log="$2" peer_id="$3" socks="$4"
    (
        set -a
        # shellcheck disable=SC1090
        source "$env_file"
        export PEER_ID="$peer_id"
        export TUNNEL_PROFILE
        export METRICS_DIR
        set +a
        exec ./target/release/rtmp-steganography peer --tunnel-socks "$socks"
    ) > "$log" 2>&1 &
    echo $!
}

PID_A=$(start_peer .env.peer-a "$LOG_A" A "$SOCKS_A")
echo "[bench-tunnel] peer A pid=$PID_A  log=$LOG_A"
sleep 2
PID_B=$(start_peer .env.peer-b "$LOG_B" B "$SOCKS_B")
echo "[bench-tunnel] peer B pid=$PID_B  log=$LOG_B"

# Wait for tunnel warm-up (VK HLS can take ~15s for first segment)
sleep 30

# Run Bench 1 realistic from peer A
PEER_ID=A ./target/release/rtmp-steganography bench realistic \
    --socks "$SOCKS_A" \
    --metrics-dir "$METRICS_DIR"

# Run Bench 2 saturation (both profiles)
PEER_ID=A ./target/release/rtmp-steganography bench saturation \
    --socks "$SOCKS_A" \
    --profile both \
    --metrics-dir "$METRICS_DIR"

# Teardown
kill -INT "$PID_A" "$PID_B" 2>/dev/null || true
sleep 3
kill -9 "$PID_A" "$PID_B" 2>/dev/null || true
wait 2>/dev/null || true

echo ""
echo "[bench-tunnel] ===== RESULTS ====="
echo "metrics: $METRICS_DIR (latest run_id subdir)"
echo "peer logs: $LOG_A  $LOG_B"
