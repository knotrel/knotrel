#!/usr/bin/env bash
# Unit tests of the container entrypoint (container/entrypoint/15-knotrel-console.sh)
# without Docker: runs it with chosen environments against the real templates
# and checks the exit status, the warnings and the rendered configuration.
# Needs sh, grep and envsubst (gettext). The container smoke test
# (scripts/smoke-container.sh) covers the same cases against the built image.
#
#   scripts/test-entrypoint.sh
set -euo pipefail

console_dir="$(cd "$(dirname "$0")/.." && pwd)"
entrypoint="$console_dir/container/entrypoint/15-knotrel-console.sh"
templates="$console_dir/container/templates"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
pass() { echo "ok - $*"; }
fail() {
    echo "FAIL - $*" >&2
    sed 's/^/    stderr: /' "$work/stderr" >&2 || true
    failures=$((failures + 1))
}

# run <env assignments...>: runs the entrypoint in a clean environment with
# fresh output directories; sets $status, and leaves stderr in $work/stderr.
run() {
    rm -rf "$work/out" "$work/listen"
    status=0
    env -i PATH="$PATH" \
        CONSOLE_ENTRYPOINT_TEMPLATE_DIR="$templates" \
        CONSOLE_ENTRYPOINT_OUTPUT_DIR="$work/out" \
        CONSOLE_ENTRYPOINT_LISTEN_DIR="$work/listen" \
        "$@" ${ENTRYPOINT_SHELL:-sh} "$entrypoint" > "$work/stdout" 2> "$work/stderr" || status=$?
}
warned() { grep -q "warning: $1 is deprecated" "$work/stderr"; }
no_warning() { ! grep -q 'warning' "$work/stderr"; }
rendered() { grep -qF "$1" "$work/out/$2"; }
# Every forwarded route of the rendered configuration uses this exact upstream.
upstream_is() {
    local routes others
    routes="$(grep -c "proxy_pass $1/" "$work/out/default.conf")"
    others="$(grep -c 'proxy_pass' "$work/out/default.conf")"
    [[ "$routes" -gt 0 && "$routes" == "$others" ]]
}
unrendered() { grep -q '\${KNOTREL' "$work/out/default.conf" "$work/out/api-proxy.inc"; }

# The plain-http upstream is a loopback address (the console accepts http and
# https); the other values use https.
new=http://127.0.0.1:8080
old=https://legacy.example:8443

# --- Upstream URL --------------------------------------------------------------
run KNOTREL_CONSOLE_UPSTREAM="$new"
if [[ "$status" == 0 ]] && no_warning && upstream_is "$new" && ! unrendered &&
    rendered 'proxy_read_timeout 35s;' api-proxy.inc &&
    rendered 'server_name localhost 127.0.0.1 [::1];' default.conf &&
    rendered "{\"serverLabel\":\"$new\",\"deployment\":\"container\"}" console-config.json &&
    rendered 'listen [::]:8080 default_server;' ../listen/default-server.conf; then
    pass "new name only: started, no warning, defaults applied"
else
    fail "new name only"
fi

run KNOTREL_UPSTREAM="$old"
if [[ "$status" == 0 ]] && warned KNOTREL_UPSTREAM && upstream_is "$old" && ! unrendered &&
    rendered "\"serverLabel\":\"$old\"" console-config.json; then
    pass "alias only: started with the alias's value, deprecation warning"
else
    fail "alias only"
fi

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_UPSTREAM="$new"
if [[ "$status" == 0 ]] && warned KNOTREL_UPSTREAM && upstream_is "$new"; then
    pass "both equal: started, deprecation warning"
else
    fail "both equal"
fi

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_UPSTREAM="$old"
if [[ "$status" != 0 ]] && grep -q 'KNOTREL_CONSOLE_UPSTREAM and its deprecated alias KNOTREL_UPSTREAM are both set, with different values' "$work/stderr" &&
    [[ ! -e "$work/out/default.conf" ]]; then
    pass "both different: refused to start, nothing rendered"
else
    fail "both different"
fi

run KNOTREL_CONSOLE_UPSTREAM="" KNOTREL_UPSTREAM="$old"
if [[ "$status" == 0 ]] && warned KNOTREL_UPSTREAM && upstream_is "$old"; then
    pass "empty new name counts as not set"
else
    fail "empty new name"
fi

# --- Upstream timeout ----------------------------------------------------------
run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_UPSTREAM_TIMEOUT=50s
if [[ "$status" == 0 ]] && no_warning && rendered 'proxy_send_timeout 50s;' api-proxy.inc &&
    rendered 'proxy_read_timeout 50s;' api-proxy.inc; then
    pass "timeout new name only"
else
    fail "timeout new name only"
fi

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_UPSTREAM_TIMEOUT=40s
if [[ "$status" == 0 ]] && warned KNOTREL_UPSTREAM_TIMEOUT && rendered 'proxy_read_timeout 40s;' api-proxy.inc; then
    pass "timeout alias only: used, deprecation warning"
else
    fail "timeout alias only"
fi

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_UPSTREAM_TIMEOUT=40s KNOTREL_UPSTREAM_TIMEOUT=40s
if [[ "$status" == 0 ]] && warned KNOTREL_UPSTREAM_TIMEOUT && rendered 'proxy_read_timeout 40s;' api-proxy.inc; then
    pass "timeout both equal: started, deprecation warning"
else
    fail "timeout both equal"
fi

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_UPSTREAM_TIMEOUT=40s KNOTREL_UPSTREAM_TIMEOUT=41s
if [[ "$status" != 0 ]] && grep -q 'KNOTREL_CONSOLE_UPSTREAM_TIMEOUT and its deprecated alias KNOTREL_UPSTREAM_TIMEOUT' "$work/stderr"; then
    pass "timeout both different: refused to start"
else
    fail "timeout both different"
fi

# --- Validation still applies to the resolved values ----------------------------
expect_refused() {
    local what="$1"
    shift
    run "$@"
    if [[ "$status" != 0 && ! -e "$work/out/default.conf" ]]; then
        pass "refused: $what"
    else
        fail "accepted: $what"
    fi
}
expect_refused "missing upstream"
expect_refused "empty upstream" KNOTREL_CONSOLE_UPSTREAM=""
expect_refused "credentials in the new name" KNOTREL_CONSOLE_UPSTREAM='https://user:pw@knotrel.example:8443'
expect_refused "credentials in the alias" KNOTREL_UPSTREAM='https://user:pw@knotrel.example:8443'
expect_refused "query in the upstream" KNOTREL_CONSOLE_UPSTREAM='https://knotrel.example:8443/?a=b'
expect_refused "trailing slash" KNOTREL_CONSOLE_UPSTREAM='https://knotrel.example:8443/'
expect_refused "nginx directive in the upstream" KNOTREL_UPSTREAM='https://knotrel.example:8443;return 200'
expect_refused "invalid timeout (alias)" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_UPSTREAM_TIMEOUT='35s; evil'
expect_refused "invalid listen" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_LISTEN='0.0.0.0:8080; evil'
expect_refused "empty server names" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_SERVER_NAMES=""
expect_refused "server names with a directive" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_SERVER_NAMES='localhost; evil'
expect_refused "label with a quote" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_SERVER_LABEL='a"b'

# Multi-line values: `grep -Eq` matches any one line, so each value is also
# required to be a single line (a line feed or a carriage return is refused).
lf=$'\n'
cr=$'\r'
expect_refused "upstream with a line feed" KNOTREL_CONSOLE_UPSTREAM="$new${lf}; return 200 injected; #"
expect_refused "alias upstream with a line feed" KNOTREL_UPSTREAM="$new${lf}; return 200 injected; #"
expect_refused "upstream with a carriage return" KNOTREL_CONSOLE_UPSTREAM="$new${cr}"
expect_refused "timeout with a line feed" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_UPSTREAM_TIMEOUT="35s${lf}evil"
expect_refused "listen with a line feed" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_LISTEN="8080${lf}0.0.0.0:80;evil"
expect_refused "server names with a line feed" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_SERVER_NAMES="localhost${lf}evil"
expect_refused "label with a line feed" KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_SERVER_LABEL="demo${lf}two"

run KNOTREL_CONSOLE_UPSTREAM="$new" KNOTREL_CONSOLE_LISTEN=127.0.0.1:8080 \
    KNOTREL_CONSOLE_SERVER_NAMES='localhost console.example' KNOTREL_CONSOLE_SERVER_LABEL='demo (knotrel:8080)'
if [[ "$status" == 0 ]] && rendered 'server_name localhost console.example;' default.conf &&
    rendered '"serverLabel":"demo (knotrel:8080)"' console-config.json &&
    [[ "$(cat "$work/listen/console.conf")" == '    listen 127.0.0.1:8080;' ]]; then
    pass "listen, server names and label rendered"
else
    fail "listen, server names and label"
fi

if [[ "$failures" -gt 0 ]]; then
    echo "$failures entrypoint test(s) failed." >&2
    exit 1
fi
echo "Entrypoint tests passed."
