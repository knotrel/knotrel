#!/usr/bin/env bash
# Starts a disposable PostgreSQL container configured for the feeder tests and
# prints the admin URL to use as KNOTREL_PG_ADMIN_URL.
#
#   scripts/start-test-postgres.sh [major] [port]    # default: 18 55432
#
# The container is named knotrel-pg-test-<major> and removed by
# scripts/stop-test-postgres.sh. The password is random and only printed in the
# URL; nothing is written to the repository.
set -euo pipefail

major="${1:-18}"
port="${2:-55432}"
case "$major" in
  15) image="postgres:15.20-alpine@sha256:e18480a22d6e48f4d816a6696769b1ec09ac73a824c5649e49c7d8b684913f3d" ;;
  16) image="postgres:16.16-alpine@sha256:1d5b02d3a9941fbd52437db4107f8c52d6297dac553ca1e5c299ebb3756b4857" ;;
  17) image="postgres:17.12-alpine@sha256:526f86d93c6ea08fd3f1fe5605650cacc781b2c6013169a0ad8f42c7d3d84f32" ;;
  18) image="postgres:18.7-alpine@sha256:ff64c9e3bdf8e2120107d3559de8db314e8305f7e38c5105903c6295876738ff" ;;
  19) image="postgres:19.0-alpine@sha256:8b0ff4cdf221f0c53d7fcda681d3b461be725c492cd881c5ec9c0ad25023f188" ;;
  *) echo "unsupported PostgreSQL major $major (supported: 15 to 19)" >&2; exit 2 ;;
esac

for dir in /usr/local/bin /opt/homebrew/bin /Applications/Docker.app/Contents/Resources/bin "$HOME/.docker/bin"; do
  [[ -d "$dir" && ":$PATH:" != *":$dir:"* ]] && PATH="$PATH:$dir"
done
command -v docker >/dev/null || { echo "docker not found" >&2; exit 1; }

name="knotrel-pg-test-$major"
password="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
docker rm -f "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" -p "127.0.0.1:$port:5432" \
  -e POSTGRES_PASSWORD="$password" \
  "$image" \
  -c wal_level=logical \
  -c max_replication_slots=64 \
  -c max_wal_senders=64 \
  -c wal_sender_timeout=10s \
  -c max_connections=200 >/dev/null

for _ in $(seq 1 60); do
  if docker exec "$name" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1; then
    # Reloadable, so set with ALTER SYSTEM (a -c option would take precedence
    # and the retention test could not change it).
    docker exec "$name" psql -U postgres -qAt \
      -c "ALTER SYSTEM SET max_slot_wal_keep_size = '64MB'" -c "SELECT pg_reload_conf()" >/dev/null
    echo "postgres://postgres:${password}@127.0.0.1:${port}/postgres"
    exit 0
  fi
  sleep 1
done
echo "PostgreSQL $major did not become ready" >&2
docker logs "$name" >&2 || true
exit 1
