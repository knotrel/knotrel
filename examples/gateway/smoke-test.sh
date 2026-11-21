#!/usr/bin/env bash
# Validates the gateway example end to end (roadmap 0.9.5, readiness
# criterion 7): TLS only, authentication required, operator and feeder
# credentials separated, Knotrel unreachable except through the gateway.
#
#   examples/gateway/smoke-test.sh            # builds, starts, checks, stops
#   KEEP_RUNNING=1 examples/gateway/smoke-test.sh
set -euo pipefail
cd "$(dirname "$0")"
for dir in /usr/local/bin /opt/homebrew/bin /Applications/Docker.app/Contents/Resources/bin "$HOME/.docker/bin"; do
  [[ -d "$dir" && ":$PATH:" != *":$dir:"* ]] && PATH="$PATH:$dir"
done
./setup.sh
compose=(docker compose -f compose.yaml)
# Credentials go to curl in config files, never on its command line.
auth_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$auth_dir"
  if [[ "${KEEP_RUNNING:-0}" != "1" ]]; then
    "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

"${compose[@]}" up -d --build --wait

base=https://localhost:8443
ca=.generated/tls/ca.crt
read -r _ operator_pw < <(grep '^operator ' .generated/credentials)
read -r _ feeder_pw < <(grep '^feeder ' .generated/credentials)
(umask 077
 printf 'user = "operator:%s"\n' "$operator_pw" > "$auth_dir/operator"
 printf 'user = "feeder:%s"\n' "$feeder_pw" > "$auth_dir/feeder"
 printf 'user = "operator:wrong"\n' > "$auth_dir/wrong")
unset operator_pw feeder_pw
as_operator=(-K "$auth_dir/operator")
as_feeder=(-K "$auth_dir/feeder")
failures=0
check() {
  local name="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then
    echo "PASS  $name ($actual)"
  else
    echo "FAIL  $name: expected $expected, got $actual"
    failures=$((failures + 1))
  fi
}
status() { curl -s -o /dev/null -w '%{http_code}' --cacert "$ca" "$@"; }

# Wait until the gateway answers.
for _ in $(seq 1 60); do
  [[ "$(status "${as_operator[@]}" "$base/health")" == "200" ]] && break
  sleep 1
done

check "health over TLS with operator credentials" 200 "$(status "${as_operator[@]}" "$base/health")"
check "no credentials refused" 401 "$(status "$base/v1/info")"
check "wrong password refused" 401 "$(status -K "$auth_dir/wrong" "$base/v1/info")"
check "operator reads /v1/info" 200 "$(status "${as_operator[@]}" "$base/v1/info")"
check "operator links two nodes" 200 "$(status "${as_operator[@]}" -H 'content-type: application/json' \
  -d '{"op":"link","source":"1","target":"2"}' "$base/v1/operations")"
connected="$(curl -s --cacert "$ca" "${as_operator[@]}" -H 'content-type: application/json' \
  -d '{"op":"connected","source":"1","target":"2"}' "$base/v1/operations")"
check "operator query answered by Knotrel" true "$(printf '%s' "$connected" | grep -o '"connected":true' | head -1 | cut -d: -f2)"
check "operator cannot use feeder routes" 401 "$(status "${as_operator[@]}" -H 'content-type: application/json' \
  -d '{}' "$base/v1/feeder/heartbeat")"
feeder_reply="$(curl -s -o /dev/null -w '%{http_code}' --cacert "$ca" "${as_feeder[@]}" \
  -H 'content-type: application/json' -d '{}' "$base/v1/feeder/heartbeat")"
# Knotrel itself answers (standalone mode refuses feeder routes with 409, or
# 422 for the empty body): not the gateway's 401.
if [[ "$feeder_reply" == "409" || "$feeder_reply" == "422" ]]; then
  echo "PASS  feeder credentials reach the feeder routes ($feeder_reply from Knotrel)"
else
  echo "FAIL  feeder credentials reach the feeder routes: got $feeder_reply"
  failures=$((failures + 1))
fi
check "feeder cannot use the operator API" 401 "$(status "${as_feeder[@]}" "$base/v1/info")"
check "feeder can read readiness" 200 "$(status "${as_feeder[@]}" "$base/ready")"
check "console UI for operators" 200 "$(status "${as_operator[@]}" "$base/")"
check "console UI refused without credentials" 401 "$(status "$base/")"
check "console API through the gateway" 200 "$(status "${as_operator[@]}" "$base/api/v1/stats")"
check "plain HTTP on the TLS port refused" 400 "$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:8443/health")"
# Negotiated protocol, with OpenSSL (some curl builds cannot do TLS 1.3).
negotiated() {
  openssl s_client -connect localhost:8443 -servername localhost -CAfile "$ca" "$1" </dev/null 2>/dev/null |
    grep -Eo 'Protocol *: *TLSv1\.[0-9]|New, TLSv1\.[0-9]' | grep -Eo 'TLSv1\.[0-9]' | head -1
}
check "TLS 1.2 accepted" TLSv1.2 "$(negotiated -tls1_2)"
check "TLS 1.3 accepted" TLSv1.3 "$(negotiated -tls1_3)"
# Older versions: a client probe cannot prove a refusal (current clients
# refuse TLS 1.0/1.1 themselves), so the gateway's effective configuration is
# checked instead.
check "gateway allows only TLS 1.2 and 1.3" "ssl_protocols TLSv1.2 TLSv1.3;" \
  "$("${compose[@]}" exec -T gateway nginx -T -c /etc/knotrel-gateway/nginx.conf 2>/dev/null | grep -o 'ssl_protocols[^;]*;' | sort -u | tr -s ' ')"
check "CA key not present in the gateway" absent \
  "$("${compose[@]}" exec -T gateway sh -c 'ls /etc/knotrel-gateway/tls/ca.key >/dev/null 2>&1 && echo present || echo absent')"
if curl -s -o /dev/null --max-time 3 "http://127.0.0.1:9080/health"; then
  echo "FAIL  Knotrel not reachable from the host"
  failures=$((failures + 1))
else
  echo "PASS  Knotrel not reachable from the host"
fi
network="$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}}{{end}}' "$("${compose[@]}" ps -q knotrel)")"
gateway_image="$(docker inspect -f '{{.Config.Image}}' "$("${compose[@]}" ps -q gateway)")"
if docker run --rm --network "$network" --entrypoint wget "$gateway_image" -q -T 3 -O /dev/null \
  "http://knotrel:9080/health" >/dev/null 2>&1; then
  echo "FAIL  Knotrel not reachable from another container on the network"
  failures=$((failures + 1))
else
  echo "PASS  Knotrel not reachable from another container on the network"
fi
if docker run --rm --network "$network" --entrypoint wget "$gateway_image" -q -T 3 -O /dev/null \
  --header 'Host: localhost' "http://knotrel:8080/healthz" >/dev/null 2>&1; then
  echo "FAIL  console not reachable from another container on the network"
  failures=$((failures + 1))
else
  echo "PASS  console not reachable from another container on the network"
fi

if [[ "$failures" -gt 0 ]]; then
  echo "$failures check(s) failed"
  "${compose[@]}" logs --tail 50 gateway || true
  exit 1
fi
echo "all gateway checks passed"
