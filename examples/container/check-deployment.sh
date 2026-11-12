#!/usr/bin/env bash
# Startup, feeder, lease-expiry, restart/rebuild and shutdown check for the
# example container (docs/deployment.md, "Verification transcript").
#
# Prerequisites: docker, curl, python3, an image built from
# examples/container/Dockerfile, and a release build of the reference feeder:
#   cargo build --release --locked -p knotrel-server --example external_feeder
#
# Environment (all optional):
#   IMAGE   image tag            (default knotrel-server:example)
#   FEEDER  feeder executable    (default target/release/examples/external_feeder)
#   OUT     directory for logs   (default: a new temporary directory)
#   PORT    host port on 127.0.0.1 (default 8080)
#
# Waits are bounded polls; no fixed sleep is used as evidence of ordering.
# Exit status is 0 only if every expected outcome was observed.
set -u

IMAGE=${IMAGE:-knotrel-server:example}
FEEDER=${FEEDER:-target/release/examples/external_feeder}
PORT=${PORT:-8080}
OUT=${OUT:-$(mktemp -d)}
NAME=knotrel-deploy-check-$$
URL=http://127.0.0.1:$PORT
FAILURES=0

mkdir -p "$OUT"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

now() { python3 -c 'import time; print(f"{time.time():.3f}")'; }
ts() { python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).strftime("%H:%M:%S.%f")[:-3])'; }
since() { python3 -c "import time; print(f'{time.time() - $1:.3f}')"; }
expect() { # description, observed, expected
    if [ "$2" = "$3" ]; then echo "  PASS $1: $2"; else echo "  FAIL $1: observed '$2', expected '$3'"; FAILURES=$((FAILURES + 1)); fi
}
ready() { curl -s -m 2 -o "$OUT/ready.body" -w '%{http_code}' "$URL/ready"; printf ' %s' "$(cat "$OUT/ready.body")"; }
wait_health() {
    for _ in $(seq 1 50); do
        [ "$(curl -s -m 1 -o /dev/null -w '%{http_code}' "$URL/health")" = 200 ] && { echo "[$(ts)] /health 200"; return 0; }
        sleep 0.2
    done
    echo "[$(ts)] /health not 200 within 10 s"; FAILURES=$((FAILURES + 1)); return 1
}
run_feeder() { # log file name
    local rc t
    "$FEEDER" --addr "127.0.0.1:$PORT" >"$OUT/$1" 2>&1; rc=$?; t=$(now)
    local r; r=$(ready); echo "[$(ts)] feeder exit=$rc; /ready probed $(since "$t") s after exit -> $r"
    expect "feeder exit status" "$rc" 0
    expect "/ready immediately after feeder" "$r" '200 {"status":"ready"}'
}

echo "== image and feeder"
echo "image=$(docker image inspect -f '{{.Id}}' "$IMAGE") feeder=$FEEDER out=$OUT"

echo "== 1. start container (source-managed, published on 127.0.0.1 only)"
docker run -d --name "$NAME" -p "127.0.0.1:$PORT:8080" --memory 512m --stop-timeout 15 "$IMAGE" >/dev/null
expect "docker run" "$?" 0
expect "container user" "$(docker inspect -f '{{.Config.User}}' "$NAME")" "10001:10001"
expect "uid inside container" "$(docker exec "$NAME" id -u)" 10001
wait_health
for _ in $(seq 1 30); do h=$(docker inspect -f '{{.State.Health.Status}}' "$NAME"); [ "$h" = healthy ] && break; sleep 1; done
expect "docker HEALTHCHECK" "$h" healthy

echo "== 2. readiness before any feeder request"
expect "/ready at startup" "$(ready)" '503 {"status":"uninitialized"}'

echo "== 3. feeder run 1 (initial import, catch-up, barrier)"
run_feeder feeder-run1.log
t_exit=$(now)

echo "== 4. lease expiry after feeder exit (bounded 30 s poll)"
stale=""
for _ in $(seq 1 30); do
    r=$(ready)
    [ "$r" = '503 {"status":"stale"}' ] && { stale=$r; echo "[$(ts)] stale observed $(since "$t_exit") s after feeder exit"; break; }
    sleep 1
done
expect "/ready after lease expiry" "${stale:-$(ready)}" '503 {"status":"stale"}'
expect "/health while stale" "$(curl -s -m 2 -w '%{http_code}' -o /dev/null "$URL/health")" 200

echo "== 5. restart (graph state is lost)"
docker restart "$NAME" >/dev/null
expect "docker restart" "$?" 0
wait_health
expect "/ready after restart" "$(ready)" '503 {"status":"uninitialized"}'

echo "== 6. feeder run 2 (authoritative rebuild)"
run_feeder feeder-run2.log

echo "== 7. stop (SIGTERM, drain)"
docker stop "$NAME" >/dev/null
expect "docker stop" "$?" 0
expect "container exit code" "$(docker inspect -f '{{.State.ExitCode}}' "$NAME")" 0
expect "OOM killed" "$(docker inspect -f '{{.State.OOMKilled}}' "$NAME")" false
docker logs "$NAME" >"$OUT/server.log" 2>&1
grep -E "shutdown requested|draining|shutdown complete" "$OUT/server.log"

echo "== result: $FAILURES failure(s); logs in $OUT"
[ "$FAILURES" -eq 0 ]
