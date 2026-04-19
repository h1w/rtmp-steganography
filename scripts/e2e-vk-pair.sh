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
BAK=".env.bak.$$"

# Move the default .env aside so dotenvy::dotenv() doesn't clobber our
# per-peer env vars on startup. Restore on exit.
if [ -f .env ]; then mv .env "$BAK"; fi
restore_env() {
    if [ -f "$BAK" ]; then mv "$BAK" .env 2>/dev/null || true; fi
}
trap restore_env EXIT

echo "[e2e-pair] logs: A=$LOG_A  B=$LOG_B"
echo "[e2e-pair] duration: ${DURATION}s"

cargo build --release

start_peer() {
    local env_file="$1"
    local log="$2"
    (
        set -a
        # shellcheck disable=SC1090
        source "$env_file"
        set +a
        exec ./target/release/rtmp-steganography peer
    ) > "$log" 2>&1 &
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
    local sent rcvd drops pilot_fail hdr_fail crc_fail resolve_fail
    # Sent side: heartbeat_loop prints "[app] time_sync ts=... sent"
    sent=$(grep -cE '\[app\] time_sync ts=[0-9]+ sent' "$log" || true)
    # Rcvd side: log_loop prints "[app] time_sync ts=... Δ=...ms"
    rcvd=$(grep -cE '\[app\] time_sync ts=[0-9]+ Δ=' "$log" || true)
    drops=$(grep -c 'dropped:' "$log" || true)
    pilot_fail=$(grep -c 'PilotValidationFailed' "$log" || true)
    hdr_fail=$(grep -c 'HeaderRsFailed' "$log" || true)
    crc_fail=$(grep -c 'PayloadCrcMismatch' "$log" || true)
    resolve_fail=$(grep -c 'vk resolve failed' "$log" || true)
    echo "[e2e-pair] peer $label: sent=$sent rcvd=$rcvd drops=$drops pilot_fail=$pilot_fail hdr_fail=$hdr_fail crc_fail=$crc_fail vk_resolve_retries=$resolve_fail"
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
