#!/usr/bin/env bash
# Starts a local Knotrel server and the console with Docker Compose, waits until
# the console answers, and prints its URL. Works from any directory.
#   console/scripts/start-local.sh            # build if needed and start
#   console/scripts/start-local.sh --build    # force a rebuild first
set -euo pipefail

compose_file="$(cd "$(dirname "${BASH_SOURCE[0]}")/../deploy" && pwd)/compose.yaml"
url="http://127.0.0.1:7380/"

if [[ "${1:-}" == "--build" ]]; then
    docker compose -f "$compose_file" build
fi
docker compose -f "$compose_file" up -d

printf 'Waiting for the console'
for _ in $(seq 1 60); do
    if curl -sf "${url}api/ready" >/dev/null 2>&1; then
        printf '\nKnotrel console ready at %s\n' "$url"
        echo "Logs: docker compose -f $compose_file logs -f"
        echo "Stop: $(dirname "${BASH_SOURCE[0]}")/stop-local.sh"
        exit 0
    fi
    printf '.'
    sleep 1
done
printf '\nThe console did not become ready in 60 s. Recent logs:\n' >&2
docker compose -f "$compose_file" logs --tail 20 >&2
exit 1
