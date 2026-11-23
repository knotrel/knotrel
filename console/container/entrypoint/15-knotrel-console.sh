#!/bin/sh
# Resolves and validates the operator configuration, then renders the nginx
# configuration and the console's runtime file before nginx starts. Any invalid
# value stops the container. Configuration: docs/console.md.
#
# Every console variable is named KNOTREL_CONSOLE_*. KNOTREL_UPSTREAM and
# KNOTREL_UPSTREAM_TIMEOUT are deprecated aliases (supported in 1.x) of
# KNOTREL_CONSOLE_UPSTREAM and KNOTREL_CONSOLE_UPSTREAM_TIMEOUT; see
# resolve_alias for the precedence rule.
#
# This script renders the templates itself, with an explicit variable list, so
# that a value resolved from an alias reaches nginx: the image's own envsubst
# step runs in a separate process and would only see the raw environment. The
# templates therefore live outside /etc/nginx/templates, where that step finds
# nothing to render.
#
# CONSOLE_ENTRYPOINT_TEMPLATE_DIR, CONSOLE_ENTRYPOINT_OUTPUT_DIR and
# CONSOLE_ENTRYPOINT_LISTEN_DIR override the paths for the entrypoint tests
# (console/scripts/test-entrypoint.sh) only; the image does not set them.
set -eu

fail() {
    echo "knotrel-console: $*" >&2
    exit 1
}
warn() {
    echo "knotrel-console: warning: $*" >&2
}

# single_line NAME VALUE: refuses a value that contains a line feed or a
# carriage return. `grep -Eq` succeeds when any one line matches, so without
# this check a multi-line value would pass validation and its other lines
# would be rendered into the nginx configuration or the JSON file.
newline='
'
carriage_return="$(printf '\r')"
single_line() {
    case "$2" in
        *"$newline"* | *"$carriage_return"*) fail "$1 must be a single line" ;;
    esac
}

# resolve_alias NEW OLD NEW_VALUE OLD_VALUE: sets $resolved to the effective
# value of NEW. An empty value counts as not set.
#   - only one name set: its value is used (a warning if it is OLD);
#   - both set to the same value: NEW is used, with a warning;
#   - both set to different values: the container refuses to start.
resolve_alias() {
    resolved="$3"
    [ -n "$4" ] || return 0
    if [ -z "$3" ]; then
        warn "$2 is deprecated; rename it to $1 (the alias is supported in 1.x)"
        resolved="$4"
    elif [ "$3" = "$4" ]; then
        warn "$2 is deprecated and redundant with $1 (same value); remove $2"
    else
        fail "$1 and its deprecated alias $2 are both set, with different values; set only $1"
    fi
}

resolve_alias KNOTREL_CONSOLE_UPSTREAM KNOTREL_UPSTREAM \
    "${KNOTREL_CONSOLE_UPSTREAM:-}" "${KNOTREL_UPSTREAM:-}"
upstream="$resolved"
resolve_alias KNOTREL_CONSOLE_UPSTREAM_TIMEOUT KNOTREL_UPSTREAM_TIMEOUT \
    "${KNOTREL_CONSOLE_UPSTREAM_TIMEOUT:-}" "${KNOTREL_UPSTREAM_TIMEOUT:-}"
upstream_timeout="${resolved:-35s}"
server_names="${KNOTREL_CONSOLE_SERVER_NAMES-localhost 127.0.0.1 [::1]}"

single_line KNOTREL_CONSOLE_UPSTREAM "$upstream"
single_line KNOTREL_CONSOLE_UPSTREAM_TIMEOUT "$upstream_timeout"
single_line KNOTREL_CONSOLE_SERVER_NAMES "$server_names"

[ -n "$upstream" ] ||
    fail "set KNOTREL_CONSOLE_UPSTREAM to the Knotrel server base URL (see docs/console.md)"
# http(s)://host[:port][/path] with no credentials, query, fragment, spaces or
# trailing slash. The value is fixed for the container's lifetime.
printf '%s' "$upstream" | grep -Eq '^https?://(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+)(:[0-9]{1,5})?(/[A-Za-z0-9._~-]+)*$' ||
    fail "KNOTREL_CONSOLE_UPSTREAM must be scheme://host[:port][/path] with scheme http or https, and no credentials, query or trailing slash"

printf '%s' "$server_names" | grep -Eq '^[][A-Za-z0-9.:_* -]+$' ||
    fail "KNOTREL_CONSOLE_SERVER_NAMES must be space-separated host names"
printf '%s' "$upstream_timeout" | grep -Eq '^[0-9]{1,6}(ms|s|m)?$' ||
    fail "KNOTREL_CONSOLE_UPSTREAM_TIMEOUT must be an nginx time such as 35s"

# Listen addresses: port, IPv4:port or [IPv6]:port, space separated.
listen="${KNOTREL_CONSOLE_LISTEN:-8080 [::]:8080}"
single_line KNOTREL_CONSOLE_LISTEN "$listen"
printf '%s' "$listen" | grep -Eq '^(([0-9]{1,3}(\.[0-9]{1,3}){3}:|\[[0-9A-Fa-f:.]+\]:)?[0-9]{1,5})( ([0-9]{1,3}(\.[0-9]{1,3}){3}:|\[[0-9A-Fa-f:.]+\]:)?[0-9]{1,5})*$' ||
    fail "KNOTREL_CONSOLE_LISTEN must be space-separated port, IPv4:port or [IPv6]:port values"

label="${KNOTREL_CONSOLE_SERVER_LABEL:-$upstream}"
single_line KNOTREL_CONSOLE_SERVER_LABEL "$label"
printf '%s' "$label" | grep -Eq '^[][A-Za-z0-9 ._~:/@()-]{1,200}$' ||
    fail "KNOTREL_CONSOLE_SERVER_LABEL contains unsupported characters"

command -v envsubst >/dev/null 2>&1 || fail "envsubst is missing from the image"

listen_dir="${CONSOLE_ENTRYPOINT_LISTEN_DIR:-/tmp/knotrel-console-listen}"
mkdir -p "$listen_dir"
: > "$listen_dir/default-server.conf"
: > "$listen_dir/console.conf"
for address in $listen; do
    printf '    listen %s default_server;\n' "$address" >> "$listen_dir/default-server.conf"
    printf '    listen %s;\n' "$address" >> "$listen_dir/console.conf"
done

# Render each *.template with exactly the validated values; any other ${...}
# in a template is left untouched.
template_dir="${CONSOLE_ENTRYPOINT_TEMPLATE_DIR:-/etc/nginx/knotrel/templates}"
out="${CONSOLE_ENTRYPOINT_OUTPUT_DIR:-/tmp/knotrel-console}"
mkdir -p "$out"
rendered=0
for template in "$template_dir"/*.template; do
    [ -f "$template" ] || continue
    name="$(basename "$template" .template)"
    KNOTREL_CONSOLE_UPSTREAM="$upstream" \
        KNOTREL_CONSOLE_UPSTREAM_TIMEOUT="$upstream_timeout" \
        KNOTREL_CONSOLE_SERVER_NAMES="$server_names" \
        envsubst '${KNOTREL_CONSOLE_UPSTREAM} ${KNOTREL_CONSOLE_UPSTREAM_TIMEOUT} ${KNOTREL_CONSOLE_SERVER_NAMES}' \
        < "$template" > "$out/$name"
    rendered=$((rendered + 1))
done
[ "$rendered" -gt 0 ] || fail "no nginx templates found in $template_dir"

printf '{"serverLabel":"%s","deployment":"container"}\n' "$label" > "$out/console-config.json"
echo "knotrel-console: listening on $listen; forwarding /api to $upstream; accepted hosts: $server_names"
