#!/usr/bin/env bash
# Removes the container started by start-test-postgres.sh.
set -euo pipefail
for dir in /usr/local/bin /opt/homebrew/bin /Applications/Docker.app/Contents/Resources/bin "$HOME/.docker/bin"; do
  [[ -d "$dir" && ":$PATH:" != *":$dir:"* ]] && PATH="$PATH:$dir"
done
docker rm -f "knotrel-pg-test-${1:-18}" >/dev/null 2>&1 || true
