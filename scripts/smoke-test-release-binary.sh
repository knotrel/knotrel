#!/usr/bin/env bash

set -euo pipefail

# smoke-test-release-binary.sh
# Black-box smoke test for release binary of knotrel-server.
# Tests loopback binding, health/readiness, graph mutations, exact queries,
# error handling, idle graceful shutdown, and fresh restart state isolation.
#
# Requirements addressed:
# - Finding 9: Runs under explicit, controlled environment (isolated from parent env).
# - Finding 10: Enforces effective deadlines on all requests, startup, and shutdown waits.
# - Finding 11: Validates exact status codes, response bodies, and actual process exit codes.
# - Finding 15: Uses safe per-execution private temporary files.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BINARY="${1:-}"

if [[ -z "$BINARY" ]]; then
    if [[ -f "target/release/knotrel-server" ]]; then
        BINARY="target/release/knotrel-server"
    elif [[ -f "target/aarch64-apple-darwin/release/knotrel-server" ]]; then
        BINARY="target/aarch64-apple-darwin/release/knotrel-server"
    elif [[ -f "target/x86_64-unknown-linux-gnu/release/knotrel-server" ]]; then
        BINARY="target/x86_64-unknown-linux-gnu/release/knotrel-server"
    else
        echo "Usage: $0 <path-to-knotrel-server>" >&2
        exit 1
    fi
fi

if [[ ! -x "$BINARY" ]]; then
    echo "ERROR: Target binary does not exist or is not executable: $BINARY" >&2
    exit 1
fi

echo "=== Knotrel Release Binary Black-Box Smoke Test ==="
echo "Target executable: $BINARY"

TEST_TMPDIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'knotrel-smoke')"
echo "Test temporary directory: $TEST_TMPDIR"

ACTIVE_PIDS=()
cleanup() {
    echo "Running smoke test cleanup..."
    for pid in "${ACTIVE_PIDS[@]}"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "Terminating child process $pid..."
            kill -TERM "$pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
                if ! kill -0 "$pid" 2>/dev/null; then
                    break
                fi
                sleep 0.05
            done
            if kill -0 "$pid" 2>/dev/null; then
                echo "Force killing child process $pid..."
                kill -KILL "$pid" 2>/dev/null || true
            fi
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -rf "$TEST_TMPDIR"
    echo "Cleanup complete."
}
trap cleanup EXIT INT TERM

# Bounded curl helper with 5s timeout and 2s connect timeout (Finding 10)
curl_req() {
    curl -sS --max-time 5 --connect-timeout 2 "$@"
}

# Helper: waits for child process termination in the current shell context (no subshell).
# Stores exit code in WAIT_EXIT_CODE, sets WAIT_TIMED_OUT=1 if deadline is reached.
WAIT_EXIT_CODE=""
WAIT_TIMED_OUT=0
wait_child_exit() {
    local pid="$1"
    local timeout_secs="${2:-5}"
    local start_time
    start_time="$(date +%s)"
    WAIT_EXIT_CODE=""
    WAIT_TIMED_OUT=0

    while true; do
        if ! kill -0 "$pid" 2>/dev/null; then
            if wait "$pid" 2>/dev/null; then
                WAIT_EXIT_CODE=0
            else
                WAIT_EXIT_CODE=$?
            fi
            return 0
        fi
        local now
        now="$(date +%s)"
        if (( now - start_time >= timeout_secs )); then
            WAIT_TIMED_OUT=1
            return 1
        fi
        sleep 0.05
    done
}

# Launches an isolated server instance on loopback with an ephemeral port (Finding 9)
# Uses env -i to prevent inherited environment variables (e.g. conflicting RUST_LOG or LOG_FORMAT)
# from breaking server discovery or log expectations.
spawn_isolated_server() {
    local log_file="$1"

    # env -i clears parent environment; we pass only explicitly controlled settings
    env -i \
        PATH="$PATH" \
        HOME="$HOME" \
        TMPDIR="${TMPDIR:-/tmp}" \
        RUST_LOG="info" \
        LOG_FORMAT="text" \
        NO_COLOR="1" \
        KNOTREL_ENGINE="compact-bfs" \
        KNOTREL_ADDR="127.0.0.1:0" \
        KNOTREL_ALLOW_NON_LOOPBACK="false" \
        KNOTREL_MAX_PENDING_JOBS="32" \
        KNOTREL_MAX_NODES="100000" \
        KNOTREL_MAX_EDGES="500000" \
        KNOTREL_SHUTDOWN_TIMEOUT_MS="10000" \
        "$BINARY" > "$log_file" 2>&1 &

    local child_pid=$!
    ACTIVE_PIDS+=("$child_pid")

    # Bounded port discovery loop with real overall deadline (max 5 seconds total)
    local port=""
    local port_start
    port_start="$(date +%s)"
    local timeout_secs=5

    while true; do
        if [[ -s "$log_file" ]]; then
            port=$(sed -n 's/.*address=127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log_file" | head -n1)
            if [[ -n "$port" ]]; then
                break
            fi
        fi
        if ! kill -0 "$child_pid" 2>/dev/null; then
            echo "ERROR: Server process $child_pid died unexpectedly during startup. Log:" >&2
            cat "$log_file" >&2
            exit 1
        fi
        local now
        now="$(date +%s)"
        if (( now - port_start >= timeout_secs )); then
            break
        fi
        sleep 0.05
    done

    if [[ -z "$port" ]]; then
        echo "ERROR: Server failed to report bound port within timeout (5s). Log:" >&2
        cat "$log_file" >&2
        exit 1
    fi

    # Bounded readiness polling with real overall deadline (max 5 seconds total)
    local ready=0
    local ready_start
    ready_start="$(date +%s)"

    while true; do
        local now
        now="$(date +%s)"
        local elapsed=$(( now - ready_start ))
        local remaining=$(( timeout_secs - elapsed ))
        if (( remaining <= 0 )); then
            break
        fi

        # Limit probe timeout strictly to the remaining budget, capped at 2s per probe
        local req_timeout="$remaining"
        if (( req_timeout > 2 )); then
            req_timeout=2
        fi

        local ready_body
        ready_body=$(curl -sS --max-time "$req_timeout" --connect-timeout "$req_timeout" "http://127.0.0.1:$port/ready" 2>/dev/null || true)
        if [[ "$ready_body" == '{"status":"ready"}' ]]; then
            ready=1
            break
        fi
        if ! kill -0 "$child_pid" 2>/dev/null; then
            echo "ERROR: Server died while waiting for /ready. Log:" >&2
            cat "$log_file" >&2
            exit 1
        fi
        sleep 0.05
    done

    if [[ "$ready" -ne 1 ]]; then
        echo "ERROR: Server at port $port failed to become ready within timeout (5s). Log:" >&2
        cat "$log_file" >&2
        exit 1
    fi

    SERVER_PID="$child_pid"
    SERVER_PORT="$port"
}

# 1. Non-loopback rejection test with strict deadline (Finding 10)
echo "--- 1. Testing non-loopback protection boundary ---"
NON_LOOPBACK_LOG="$TEST_TMPDIR/non_loopback.log"
env -i \
    PATH="$PATH" \
    HOME="$HOME" \
    KNOTREL_ADDR="0.0.0.0:8080" \
    KNOTREL_ALLOW_NON_LOOPBACK="false" \
    "$BINARY" > "$NON_LOOPBACK_LOG" 2>&1 &
REJECT_PID=$!
ACTIVE_PIDS+=("$REJECT_PID")

# Bound the wait for the process to reject and exit (max 5 seconds)
wait_child_exit "$REJECT_PID" 5 || true
if [[ "$WAIT_TIMED_OUT" -eq 1 ]]; then
    echo "ERROR: Server failed to reject non-loopback address within 5s deadline!" >&2
    kill -KILL "$REJECT_PID" 2>/dev/null || true
    cat "$NON_LOOPBACK_LOG" >&2
    exit 1
fi

REJECT_EXIT="$WAIT_EXIT_CODE"
if [[ "$REJECT_EXIT" -eq 0 ]]; then
    echo "ERROR: Server bound to non-loopback address unexpectedly exited with code 0!" >&2
    cat "$NON_LOOPBACK_LOG" >&2
    exit 1
fi

if ! grep -q "KNOTREL_ADDR must be a loopback address" "$NON_LOOPBACK_LOG"; then
    echo "ERROR: Server exited but did not print expected non-loopback error. Output:" >&2
    cat "$NON_LOOPBACK_LOG" >&2
    exit 1
fi
echo "Confirmed: Non-loopback address strictly rejected at startup (verified exit code: $REJECT_EXIT)."

# 2. Spawning instance 1 on ephemeral loopback port
echo "--- 2. Spawning release server instance on loopback ---"
LOG_1="$TEST_TMPDIR/instance_1.log"
spawn_isolated_server "$LOG_1"
PORT_1="$SERVER_PORT"
PID_1="$SERVER_PID"
echo "Instance 1 running on 127.0.0.1:$PORT_1 (PID $PID_1)"

# 3. Liveness, readiness, and instance info (Finding 11: verify status and body)
echo "--- 3. Verifying health, readiness, and info endpoints ---"
HEALTH_OUT="$TEST_TMPDIR/health.out"
HEALTH_STATUS=$(curl_req -o "$HEALTH_OUT" -w "%{http_code}" "http://127.0.0.1:$PORT_1/health")
HEALTH_BODY=$(cat "$HEALTH_OUT")
echo "Health: status=$HEALTH_STATUS body=$HEALTH_BODY"
if [[ "$HEALTH_STATUS" -ne 200 || "$HEALTH_BODY" != '{"status":"ok"}' ]]; then
    echo "ERROR: Unexpected /health response: status=$HEALTH_STATUS body=$HEALTH_BODY" >&2
    exit 1
fi

READY_OUT="$TEST_TMPDIR/ready.out"
READY_STATUS=$(curl_req -o "$READY_OUT" -w "%{http_code}" "http://127.0.0.1:$PORT_1/ready")
READY_BODY=$(cat "$READY_OUT")
echo "Ready: status=$READY_STATUS body=$READY_BODY"
if [[ "$READY_STATUS" -ne 200 || "$READY_BODY" != '{"status":"ready"}' ]]; then
    echo "ERROR: Unexpected /ready response: status=$READY_STATUS body=$READY_BODY" >&2
    exit 1
fi

INFO_OUT="$TEST_TMPDIR/info.out"
INFO_STATUS=$(curl_req -o "$INFO_OUT" -w "%{http_code}" "http://127.0.0.1:$PORT_1/v1/info")
INFO_BODY=$(cat "$INFO_OUT")
echo "Info: status=$INFO_STATUS body=$INFO_BODY"
if [[ "$INFO_STATUS" -ne 200 ]]; then
    echo "ERROR: /v1/info returned non-200: $INFO_STATUS" >&2
    exit 1
fi
if ! echo "$INFO_BODY" | grep -q '"engine":"compact-bfs"'; then
    echo "ERROR: /v1/info did not report default compact-bfs engine: $INFO_BODY" >&2
    exit 1
fi

# 4. Graph mutations and connectivity queries (Finding 11: positive verification)
echo "--- 4. Verifying graph mutations and connectivity queries ---"
OP_OUT="$TEST_TMPDIR/op.out"

# Link 100 -> 200
LINK1_STATUS=$(curl_req -o "$OP_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"link","source":"100","target":"200"}' "http://127.0.0.1:$PORT_1/v1/operations")
LINK1_BODY=$(cat "$OP_OUT")
echo "Link 100->200: status=$LINK1_STATUS body=$LINK1_BODY"
if [[ "$LINK1_STATUS" -ne 200 || "$LINK1_BODY" != '{"result":"changed","changed":true}' ]]; then
    echo "ERROR: Unexpected link response: status=$LINK1_STATUS body=$LINK1_BODY" >&2
    exit 1
fi

# Link 200 -> 300
LINK2_STATUS=$(curl_req -o "$OP_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"link","source":"200","target":"300"}' "http://127.0.0.1:$PORT_1/v1/operations")
LINK2_BODY=$(cat "$OP_OUT")
echo "Link 200->300: status=$LINK2_STATUS body=$LINK2_BODY"
if [[ "$LINK2_STATUS" -ne 200 || "$LINK2_BODY" != '{"result":"changed","changed":true}' ]]; then
    echo "ERROR: Unexpected link response: status=$LINK2_STATUS body=$LINK2_BODY" >&2
    exit 1
fi

# Connected 100 -> 300 (true)
CONN1_STATUS=$(curl_req -o "$OP_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"connected","source":"100","target":"300"}' "http://127.0.0.1:$PORT_1/v1/operations")
CONN1_BODY=$(cat "$OP_OUT")
echo "Connected 100->300: status=$CONN1_STATUS body=$CONN1_BODY"
if [[ "$CONN1_STATUS" -ne 200 || "$CONN1_BODY" != '{"result":"connected","connected":true}' ]]; then
    echo "ERROR: Expected connected=true: status=$CONN1_STATUS body=$CONN1_BODY" >&2
    exit 1
fi

# Cut bridge edge 200 -> 300
CUT_STATUS=$(curl_req -o "$OP_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"cut","source":"200","target":"300"}' "http://127.0.0.1:$PORT_1/v1/operations")
CUT_BODY=$(cat "$OP_OUT")
echo "Cut 200->300: status=$CUT_STATUS body=$CUT_BODY"
if [[ "$CUT_STATUS" -ne 200 || "$CUT_BODY" != '{"result":"changed","changed":true}' ]]; then
    echo "ERROR: Expected cut changed=true: status=$CUT_STATUS body=$CUT_BODY" >&2
    exit 1
fi

# Connected 100 -> 300 after cut (false)
CONN2_STATUS=$(curl_req -o "$OP_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"connected","source":"100","target":"300"}' "http://127.0.0.1:$PORT_1/v1/operations")
CONN2_BODY=$(cat "$OP_OUT")
echo "Connected 100->300 after cut: status=$CONN2_STATUS body=$CONN2_BODY"
if [[ "$CONN2_STATUS" -ne 200 || "$CONN2_BODY" != '{"result":"connected","connected":false}' ]]; then
    echo "ERROR: Expected connected=false after cut: status=$CONN2_STATUS body=$CONN2_BODY" >&2
    exit 1
fi

# 5. Error handling and input rejection (Finding 11)
echo "--- 5. Verifying documented error handling ---"
ERR_OUT="$TEST_TMPDIR/err.out"

# Self-loop returns 422 self_loop
SL_STATUS=$(curl_req -o "$ERR_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"link","source":"100","target":"100"}' "http://127.0.0.1:$PORT_1/v1/operations")
SL_BODY=$(cat "$ERR_OUT")
echo "Self-loop: status=$SL_STATUS body=$SL_BODY"
if [[ "$SL_STATUS" -ne 422 ]] || ! echo "$SL_BODY" | grep -q '"code":"self_loop"'; then
    echo "ERROR: Self-loop did not return 422 self_loop: status=$SL_STATUS body=$SL_BODY" >&2
    exit 1
fi

# Unknown vertex returns 404 unknown_node
UN_STATUS=$(curl_req -o "$ERR_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"connected","source":"100","target":"999999"}' "http://127.0.0.1:$PORT_1/v1/operations")
UN_BODY=$(cat "$ERR_OUT")
echo "Unknown node: status=$UN_STATUS body=$UN_BODY"
if [[ "$UN_STATUS" -ne 404 ]] || ! echo "$UN_BODY" | grep -q '"code":"unknown_node"'; then
    echo "ERROR: Unknown vertex did not return 404 unknown_node: status=$UN_STATUS body=$UN_BODY" >&2
    exit 1
fi

# Unsupported Media Type returns 415
UM_STATUS=$(curl_req -o "$ERR_OUT" -w "%{http_code}" -H "Content-Type: text/plain" -d 'hello' "http://127.0.0.1:$PORT_1/v1/operations")
echo "Unsupported media type: status=$UM_STATUS"
if [[ "$UM_STATUS" -ne 415 ]]; then
    echo "ERROR: Expected HTTP 415 for text/plain, got: $UM_STATUS" >&2
    exit 1
fi

# Malformed JSON returns 400 invalid_request (Finding 11)
MJ_STATUS=$(curl_req -o "$ERR_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{' "http://127.0.0.1:$PORT_1/v1/operations")
MJ_BODY=$(cat "$ERR_OUT")
echo "Malformed JSON: status=$MJ_STATUS body=$MJ_BODY"
if [[ "$MJ_STATUS" -ne 400 ]] || ! echo "$MJ_BODY" | grep -q '"code":"invalid_request"'; then
    echo "ERROR: Malformed JSON did not return 400 invalid_request: status=$MJ_STATUS body=$MJ_BODY" >&2
    exit 1
fi

# 6. Idle graceful shutdown via SIGTERM (Finding 10: bounded wait, Finding 11: clean idle drain)
echo "--- 6. Verifying idle graceful shutdown via SIGTERM ---"
kill -TERM "$PID_1"

wait_child_exit "$PID_1" 5 || true
if [[ "$WAIT_TIMED_OUT" -eq 1 ]]; then
    echo "ERROR: Instance 1 failed to shut down within 5s deadline!" >&2
    kill -KILL "$PID_1" 2>/dev/null || true
    cat "$LOG_1" >&2
    exit 1
fi

EXIT_CODE_1="$WAIT_EXIT_CODE"
if [[ "$EXIT_CODE_1" -ne 0 ]]; then
    echo "ERROR: Instance 1 exited with unexpected non-zero code: $EXIT_CODE_1 (expected 0). Log:" >&2
    cat "$LOG_1" >&2
    exit 1
fi

if ! grep -q "shutdown complete" "$LOG_1"; then
    echo "ERROR: 'shutdown complete' not found in shutdown log. Output:" >&2
    cat "$LOG_1" >&2
    exit 1
fi
echo "Instance 1 cleanly drained idle connections and exited 0 (verified exit code: $EXIT_CODE_1)."

# 7. Restart with fresh process: verify volatile state is empty
echo "--- 7. Verifying fresh restart starts with empty graph ---"
LOG_2="$TEST_TMPDIR/instance_2.log"
spawn_isolated_server "$LOG_2"
PORT_2="$SERVER_PORT"
PID_2="$SERVER_PID"
echo "Instance 2 running on 127.0.0.1:$PORT_2 (PID $PID_2)"

STATS_OUT="$TEST_TMPDIR/stats.out"
STATS_STATUS=$(curl_req -o "$STATS_OUT" -w "%{http_code}" "http://127.0.0.1:$PORT_2/v1/stats")
STATS_BODY=$(cat "$STATS_OUT")
echo "Stats on fresh instance: status=$STATS_STATUS body=$STATS_BODY"
if [[ "$STATS_STATUS" -ne 200 ]]; then
    echo "ERROR: /v1/stats returned non-200: $STATS_STATUS" >&2
    exit 1
fi
if ! echo "$STATS_BODY" | grep -q '"node_count":0' || ! echo "$STATS_BODY" | grep -q '"edge_count":0' || ! echo "$STATS_BODY" | grep -q '"state_version":"0"'; then
    echo "ERROR: Fresh restart stats do not reflect empty state: $STATS_BODY" >&2
    exit 1
fi

# Querying vertices 100 and 200 from instance 1 must fail with 404 unknown_node
CF_STATUS=$(curl_req -o "$ERR_OUT" -w "%{http_code}" -H "Content-Type: application/json" -d '{"op":"connected","source":"100","target":"200"}' "http://127.0.0.1:$PORT_2/v1/operations")
CF_BODY=$(cat "$ERR_OUT")
echo "Query on prior vertices: status=$CF_STATUS body=$CF_BODY"
if [[ "$CF_STATUS" -ne 404 ]] || ! echo "$CF_BODY" | grep -q '"code":"unknown_node"'; then
    echo "ERROR: Expected 404 unknown_node on fresh restart: status=$CF_STATUS body=$CF_BODY" >&2
    exit 1
fi

# Clean shutdown of instance 2
kill -TERM "$PID_2"
wait_child_exit "$PID_2" 5 || true
if [[ "$WAIT_TIMED_OUT" -eq 1 ]]; then
    echo "ERROR: Instance 2 failed to shut down within 5s deadline!" >&2
    kill -KILL "$PID_2" 2>/dev/null || true
    cat "$LOG_2" >&2
    exit 1
fi

EXIT_CODE_2="$WAIT_EXIT_CODE"
if [[ "$EXIT_CODE_2" -ne 0 ]]; then
    echo "ERROR: Instance 2 exited with unexpected non-zero code: $EXIT_CODE_2 (expected 0). Log:" >&2
    cat "$LOG_2" >&2
    exit 1
fi
echo "Instance 2 cleanly drained and exited 0 (verified exit code: $EXIT_CODE_2)."

echo "=== Release Binary Smoke Tests Passed Successfully! ==="
