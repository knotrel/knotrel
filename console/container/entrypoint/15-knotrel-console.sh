#!/bin/sh
# Validates operator configuration and writes the console's runtime file before
# nginx renders its templates. Any invalid value stops the container.
set -eu

fail() {
    echo "knotrel-console: $*" >&2
    exit 1
}

: "${KNOTREL_UPSTREAM:?knotrel-console: set KNOTREL_UPSTREAM to the Knotrel server base URL (see docs/console.md)}"
# http(s)://host[:port][/path] with no credentials, query, fragment, spaces or
# trailing slash. The value is fixed for the container's lifetime.
printf '%s' "$KNOTREL_UPSTREAM" | grep -Eq '^https?://(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+)(:[0-9]{1,5})?(/[A-Za-z0-9._~-]+)*$' ||
    fail "KNOTREL_UPSTREAM must be scheme://host[:port][/path] with scheme http or https, and no credentials, query or trailing slash"

printf '%s' "$KNOTREL_CONSOLE_SERVER_NAMES" | grep -Eq '^[][A-Za-z0-9.:_* -]+$' ||
    fail "KNOTREL_CONSOLE_SERVER_NAMES must be space-separated host names"
printf '%s' "$KNOTREL_UPSTREAM_TIMEOUT" | grep -Eq '^[0-9]{1,6}(ms|s|m)?$' ||
    fail "KNOTREL_UPSTREAM_TIMEOUT must be an nginx time such as 35s"

# Listen addresses: port, IPv4:port or [IPv6]:port, space separated.
listen="${KNOTREL_CONSOLE_LISTEN:-8080 [::]:8080}"
printf '%s' "$listen" | grep -Eq '^(([0-9]{1,3}(\.[0-9]{1,3}){3}:|\[[0-9A-Fa-f:.]+\]:)?[0-9]{1,5})( ([0-9]{1,3}(\.[0-9]{1,3}){3}:|\[[0-9A-Fa-f:.]+\]:)?[0-9]{1,5})*$' ||
    fail "KNOTREL_CONSOLE_LISTEN must be space-separated port, IPv4:port or [IPv6]:port values"
listen_dir=/tmp/knotrel-console-listen
mkdir -p "$listen_dir"
: > "$listen_dir/default-server.conf"
: > "$listen_dir/console.conf"
for address in $listen; do
    printf '    listen %s default_server;\n' "$address" >> "$listen_dir/default-server.conf"
    printf '    listen %s;\n' "$address" >> "$listen_dir/console.conf"
done

label="${KNOTREL_CONSOLE_SERVER_LABEL:-$KNOTREL_UPSTREAM}"
printf '%s' "$label" | grep -Eq '^[][A-Za-z0-9 ._~:/@()-]{1,200}$' ||
    fail "KNOTREL_CONSOLE_SERVER_LABEL contains unsupported characters"

out="${NGINX_ENVSUBST_OUTPUT_DIR:-/tmp/knotrel-console}"
mkdir -p "$out"
printf '{"serverLabel":"%s","deployment":"container"}\n' "$label" > "$out/console-config.json"
echo "knotrel-console: listening on $listen; forwarding /api to $KNOTREL_UPSTREAM; accepted hosts: $KNOTREL_CONSOLE_SERVER_NAMES"
