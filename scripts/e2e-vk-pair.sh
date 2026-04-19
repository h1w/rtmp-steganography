#!/usr/bin/env bash
# Level D — VK end-to-end PAIR smoke test.
# Launches two peer processes, each with its own .env, measures cross-traffic.
#
# Peer A publishes to stream1, reads stream2.
# Peer B publishes to stream2, reads stream1.
#
# Requires:
#   .env.peer-a, .env.peer-b at repo root.
#   Both VK streams provisioned and in "waiting for signal" state.

set -euo pipefail
cd "$(dirname "$0")/.."

DURATION="${DURATION:-60}"
LOG_A="$(mktemp)"
LOG_B="$(mktemp)"

echo "[e2e-pair] logs: A=$LOG_A  B=$LOG_B"
echo "[e2e-pair] duration: ${DURATION}s"

cargo build --release

start_peer() {
    local env_file="$1"
    local log="$2"
    env -i PATH="$PATH" HOME="$HOME" TEMP="${TEMP:-/tmp}" TMP="${TMP:-/tmp}" \
        bash -c "set -a; source '$env_file'; set +a; exec ./target/release/rtmp-steganography peer" \
        > "$log" 2>&1 &
    echo $!
}

PID_A=$(start_peer .env.peer-a "$LOG_A")
echo "[e2e-pair] peer A pid=$PID_A"
sleep 2
PID_B=$(start_peer .env.peer-b "$LOG_B")
echo "[e2e-pair] peer B pid=$PID_B"

sleep "$DURATION"

kill -INT "$PID_A" "$PID_B" 2>/dev/null || true
sleep 3
kill -9 "$PID_A" "$PID_B" 2>/dev/null || true
wait 2>/dev/null || true

summarize() {
    local label="$1"
    local log="$2"
    local sent rcvd drops pilot_fail
    sent=$(grep -c 'time_sync ts=' "$log" || true)
    rcvd=$(grep -c '\[app\] time_sync' "$log" || true)
    drops=$(grep -c 'dropped:' "$log" || true)
    pilot_fail=$(grep -c 'PilotValidationFailed' "$log" || true)
    echo "[e2e-pair] peer $label: sent=$sent rcvd=$rcvd drops=$drops pilot_fail=$pilot_fail"
    if [ "$sent" -gt 0 ]; then
        echo "[e2e-pair] peer $label: delivery = $(( 100 * rcvd / sent ))%"
    fi
}

echo ""
echo "[e2e-pair] ===== RESULTS ====="
summarize A "$LOG_A"
summarize B "$LOG_B"
echo ""
echo "[e2e-pair] full logs: $LOG_A  $LOG_B"
