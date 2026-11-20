#!/usr/bin/env bash
# Smoke-tests a knotrel-console image against a real knotrel-server container.
#
#   scripts/smoke-container.sh <console-image> <server-image> [<dist-dir>]
#
# With <dist-dir>, also checks that the image serves byte-identical assets
# (for example the files of the released static archive).
set -euo pipefail

console_image="$1"
server_image="$2"
dist_dir="${3:-}"
suffix="$$-$RANDOM"
network="knotrel-console-smoke-$suffix"
server="knotrel-smoke-server-$suffix"
console="knotrel-smoke-console-$suffix"
port="${SMOKE_PORT:-17381}"
base="http://127.0.0.1:$port"

cleanup() {
    docker rm -f "$console" "$server" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT

pass() { echo "ok - $*"; }
fail() {
    echo "FAIL - $*" >&2
    docker logs "$console" >&2 2>/dev/null || true
    exit 1
}
# status <curl args...>: prints the HTTP status only.
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
header() { curl -s -D - -o /dev/null "${@:2}" | tr -d '\r' | awk -v h="$1" 'tolower($1) == tolower(h)":" { $1 = ""; sub(/^ /, ""); print }'; }

docker network create "$network" >/dev/null
docker run -d --name "$server" --network "$network" --network-alias knotrel \
    -e KNOTREL_MODE=standalone -e KNOTREL_TOPOLOGY_LIFECYCLE=true \
    --read-only --cap-drop ALL "$server_image" >/dev/null
docker run -d --name "$console" --network "$network" -p "127.0.0.1:$port:8080" \
    -e KNOTREL_UPSTREAM=http://knotrel:8080 -e KNOTREL_CONSOLE_SERVER_LABEL="smoke server" \
    --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
    "$console_image" >/dev/null

for _ in $(seq 1 100); do
    if [[ "$(status "$base/api/health")" == 200 ]]; then break; fi
    sleep 0.2
done
[[ "$(status "$base/api/health")" == 200 ]] || fail "console did not reach the server"
pass "console started read-only and reached the server"

curl -sf "$base/" | grep -q '<div id="root">' || fail "index.html"
[[ "$(header content-security-policy "$base/")" == *"frame-ancestors 'none'"* ]] || fail "CSP header"
asset="$(curl -sf "$base/" | grep -o 'assets/[^"]*\.js' | head -n 1)"
[[ "$(status "$base/$asset")" == 200 ]] || fail "asset $asset"
[[ "$(header cache-control "$base/$asset")" == *immutable* ]] || fail "asset caching"
pass "static console, assets and security headers"

[[ "$(curl -sf "$base/console-config.json")" == '{"serverLabel":"smoke server","deployment":"container"}' ]] || fail "console-config.json"
curl -sf "$base/api/v1/info" | grep -q '"topology_lifecycle":true' || fail "info"
op() { curl -s -X POST -H 'content-type: application/json' -H "origin: $base" -H 'sec-fetch-site: same-origin' -d "$1" "$base/api/v1/operations"; }
[[ "$(op '{"op":"link","source":"1","target":"18446744073709551615"}')" == '{"result":"changed","changed":true}' ]] || fail "link"
[[ "$(op '{"op":"connected","source":"18446744073709551615","target":"1"}')" == '{"result":"connected","connected":true}' ]] || fail "connected"
[[ "$(header x-knotrel-state-version -X POST -H 'content-type: application/json' -d '{"op":"connected","source":"1","target":"18446744073709551615"}' "$base/api/v1/operations")" == 1 ]] || fail "version header"
[[ "$(op '{"op":"link","source":"2","target":"2"}')" == *'"code":"self_loop"'* ]] || fail "server error passthrough"
pass "API forwarding with exact IDs, version headers and server errors"

[[ "$(status -H 'Host: attacker.example' "$base/api/v1/info")" == 421 ]] || fail "Host check"
[[ "$(status -X POST -H 'content-type: application/json' -H 'origin: http://evil.example' -d '{}' "$base/api/v1/operations")" == 403 ]] || fail "Origin check"
[[ "$(header x-knotrel-console-proxy -H 'sec-fetch-site: cross-site' "$base/api/v1/stats")" == rejected ]] || fail "Sec-Fetch-Site check"
[[ "$(status -X POST -H 'content-type: application/json' -d '{}' "$base/api/v1/feeder/session")" == 404 ]] || fail "feeder route hidden"
[[ "$(status -X POST -H 'content-type: application/json' -d '{"operations":[]}' "$base/api/v1/batch")" == 404 ]] || fail "batch hidden"
[[ "$(status "$base/api/v1/operations")" == 405 ]] || fail "method check"
big="$(head -c 70000 /dev/zero | tr '\0' ' ')"
[[ "$(status -X POST -H 'content-type: application/json' --data-binary "$big" "$base/api/v1/operations")" == 413 ]] || fail "body limit"
pass "Host, Origin, route, method and size restrictions"

docker stop -t 1 "$server" >/dev/null
[[ "$(header x-knotrel-console-proxy "$base/api/health")" == upstream_unreachable ]] || fail "unreachable classification"
[[ "$(status "$base/api/health")" =~ ^50[24]$ ]] || fail "unreachable status"
pass "stopped server reported as upstream_unreachable (not delivered)"

if docker run --rm -e KNOTREL_UPSTREAM='http://user:pw@knotrel:8080' "$console_image" >/dev/null 2>&1; then
    fail "credentials in KNOTREL_UPSTREAM accepted"
fi
if docker run --rm "$console_image" >/dev/null 2>&1; then
    fail "missing KNOTREL_UPSTREAM accepted"
fi
pass "invalid configuration stops the container"

if [[ -n "$dist_dir" ]]; then
    expected="$(cd "$dist_dir" && find . -type f | LC_ALL=C sort | xargs sha256sum 2>/dev/null || (cd "$dist_dir" && find . -type f | LC_ALL=C sort | xargs shasum -a 256))"
    actual="$(docker run --rm --entrypoint sh "$console_image" -c 'cd /usr/share/nginx/html && find . -type f | LC_ALL=C sort | xargs sha256sum')"
    [[ "$(echo "$expected" | awk '{print $1, $2}')" == "$(echo "$actual" | awk '{print $1, $2}')" ]] || fail "image assets differ from $dist_dir"
    pass "image assets identical to $dist_dir"
fi
echo "Container smoke test passed."
