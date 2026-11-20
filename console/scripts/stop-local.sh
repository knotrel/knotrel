#!/usr/bin/env bash
# Stops and removes the local server and console started by start-local.sh.
# Graph data is in memory and is discarded. Works from any directory.
set -euo pipefail

# Find docker even when the shell PATH lacks it (Docker Desktop on macOS).
if ! command -v docker >/dev/null 2>&1; then
    for d in /usr/local/bin /opt/homebrew/bin /Applications/Docker.app/Contents/Resources/bin "$HOME/.docker/bin"; do
        [[ -x "$d/docker" ]] && PATH="$d:$PATH" && break
    done
fi
command -v docker >/dev/null 2>&1 || { echo "docker not found. Install or start Docker Desktop, or add docker to PATH." >&2; exit 1; }

compose_file="$(cd "$(dirname "${BASH_SOURCE[0]}")/../deploy" && pwd)/compose.yaml"
docker compose -f "$compose_file" down
echo "Knotrel console and server stopped."
