#!/usr/bin/env bash
# Loads a small demo network into a running Knotrel server through the console
# proxy, so the browser, workbench and performance pages have something to show.
# Run it after start-local.sh. Works from any directory.
#   console/scripts/seed-demo.sh                          # default: http://127.0.0.1:7380/api
#   console/scripts/seed-demo.sh http://127.0.0.1:7379    # a server reached directly
#
# The network (170 nodes, 166 edges):
#   - chain 1-2-3-4-5, with edge 2-3 disabled (needs lifecycle support)
#   - ring 10..19
#   - hub 100 with 150 spokes 101..250 (more than one page of incident edges)
#   - huge exact IDs 9007199254740993 (2^53+1) and 18446744073709551615 (u64 max),
#     linked to node 5
#   - isolated nodes 42 and 43, with 42 and spoke 105 inactive (needs lifecycle)
# Running it twice is harmless: existing nodes and edges are reported as no-ops.
set -euo pipefail

base="${1:-http://127.0.0.1:7380/api}"
base="${base%/}"

ops=()
link() { ops+=("{\"op\":\"link\",\"source\":\"$1\",\"target\":\"$2\"}"); }

for i in 1 2 3 4; do link "$i" "$((i + 1))"; done
for i in $(seq 10 19); do link "$i" "$(( i == 19 ? 10 : i + 1 ))"; done
for i in $(seq 101 250); do link 100 "$i"; done
link 5 9007199254740993
link 9007199254740993 18446744073709551615
ops+=('{"op":"add_node","node":"42"}' '{"op":"add_node","node":"43"}')

send() { # $1 = label, rest = operations
    local label="$1" body status
    shift
    body="{\"operations\":[$(IFS=,; echo "$*")]}"
    status="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
        -H 'content-type: application/json' --data "$body" "$base/v1/batch")" || status="000"
    echo "$label: $# operations, HTTP $status"
    [[ "$status" == "200" ]]
}

if ! curl -sf "$base/v1/info" >/dev/null; then
    echo "No Knotrel server answers at $base. Start one with console/scripts/start-local.sh." >&2
    exit 1
fi

send "Structure" "${ops[@]}" || {
    echo "The structure batch failed. Is the server in standalone mode?" >&2
    exit 1
}

# Lifecycle operations are rejected when the server runs without
# KNOTREL_TOPOLOGY_LIFECYCLE=true; the structure above is still loaded.
send "Lifecycle states" \
    '{"op":"set_edge_enabled","source":"2","target":"3","enabled":false}' \
    '{"op":"set_node_active","node":"42","active":false}' \
    '{"op":"set_node_active","node":"105","active":false}' ||
    echo "Lifecycle states not applied (server without lifecycle support?)."

echo "Done. Open the console Browser page, or try: curl -s '$base/v1/nodes?limit=5'"
