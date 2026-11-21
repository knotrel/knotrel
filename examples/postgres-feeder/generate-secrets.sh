#!/usr/bin/env bash
# Creates random passwords for the demo in examples/postgres-feeder/secrets/
# (ignored by git).
# Existing files are kept.
set -euo pipefail
dir="$(cd "$(dirname "$0")" && pwd)/secrets"
mkdir -p "$dir"
chmod 700 "$dir"
for name in postgres_password feeder_password; do
  if [[ ! -s "$dir/$name" ]]; then
    od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$dir/$name"
    # Readable inside the containers (other uids); the directory stays 700.
    chmod 644 "$dir/$name"
    echo "created $dir/$name"
  fi
done
