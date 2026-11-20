#!/usr/bin/env bash
# Stops and removes the local server and console started by start-local.sh.
# Graph data is in memory and is discarded. Works from any directory.
set -euo pipefail

compose_file="$(cd "$(dirname "${BASH_SOURCE[0]}")/../deploy" && pwd)/compose.yaml"
docker compose -f "$compose_file" down
echo "Knotrel console and server stopped."
