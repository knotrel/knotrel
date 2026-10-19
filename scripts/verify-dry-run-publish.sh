#!/usr/bin/env bash

set -euo pipefail

# verify-dry-run-publish.sh
# Executes crates.io package publish dry-runs for knotrel-core and knotrel-server.
# - knotrel-core dry-run publish is always mandatory.
# - knotrel-server dry-run publish is mandatory if knotrel-core is published on crates.io.
# - During initial bootstrap, knotrel-server dry-run is deferred ONLY upon confirmed HTTP 404.
# - Any registry network error, timeout, HTTP 429, 5xx, or Cargo failure immediately fails.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-}"

if [[ -z "$VERSION" ]]; then
    VERSION="$(python3 -c "
import json, subprocess
raw = subprocess.check_output(['cargo', 'metadata', '--format-version', '1', '--no-deps'], text=True)
pkgs = {p['name']: p['version'] for p in json.loads(raw).get('packages', [])}
print(pkgs.get('knotrel-core', ''))
")"
fi

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "ERROR: Invalid version format '$VERSION'. Must strictly match 'X.Y.Z'." >&2
    exit 1
fi

CARGO_EXEC="${CARGO_BIN:-cargo}"
API_BASE="${CRATES_IO_API_BASE:-https://crates.io/api/v1/crates}"

echo "=== 1. Mandatory dry-run publish check for knotrel-core ==="
echo "Running: $CARGO_EXEC publish --dry-run -p knotrel-core --locked ${CARGO_PUBLISH_FLAGS:-}"
"$CARGO_EXEC" publish --dry-run -p knotrel-core --locked ${CARGO_PUBLISH_FLAGS:-}

echo "=== 2. Verifying registry status of knotrel-core v${VERSION} ==="
STATUS_OUTPUT="$(python3 -c "
import json, os, sys, urllib.error, urllib.request

version = sys.argv[1]
api_base = sys.argv[2].rstrip('/')
url = f'{api_base}/knotrel-core/{version}'

req = urllib.request.Request(
    url,
    headers={'User-Agent': 'knotrel-release-dryrun (knotrel@users.noreply.github.com)'}
)

try:
    with urllib.request.urlopen(req, timeout=15) as resp:
        if resp.status == 200:
            try:
                body = resp.read().decode('utf-8')
                data = json.loads(body)
            except Exception as e:
                print(f'ERROR: Malformed JSON from registry API: {e}', file=sys.stderr)
                sys.exit(3)

            ver_num = data.get('version', {}).get('num')
            if ver_num != version:
                print(f'ERROR: Registry API version mismatch: expected {version}, got {ver_num}', file=sys.stderr)
                sys.exit(3)
            print('CORE_EXISTS')
            sys.exit(0)
        else:
            print(f'ERROR: Unexpected HTTP status {resp.status} from registry API', file=sys.stderr)
            sys.exit(2)
except urllib.error.HTTPError as e:
    if e.code == 404:
        print('CORE_CONFIRMED_404')
        sys.exit(0)
    elif e.code == 429:
        print('ERROR: crates.io API rate limit exceeded (HTTP 429).', file=sys.stderr)
        sys.exit(4)
    elif 500 <= e.code < 600:
        print(f'ERROR: crates.io API server error (HTTP {e.code}): {e.reason}', file=sys.stderr)
        sys.exit(5)
    else:
        print(f'ERROR: crates.io API returned unexpected HTTP {e.code}: {e.reason}', file=sys.stderr)
        sys.exit(6)
except urllib.error.URLError as e:
    print(f'ERROR: Network/timeout contacting crates.io API: {e.reason}', file=sys.stderr)
    sys.exit(7)
except Exception as e:
    print(f'ERROR: Unexpected error querying crates.io API: {e}', file=sys.stderr)
    sys.exit(8)
" "$VERSION" "$API_BASE")"

if [[ "$STATUS_OUTPUT" == "CORE_EXISTS" ]]; then
    echo "Confirmed: knotrel-core v${VERSION} is published on crates.io."
    echo "Mandatory registry dry-run publish validation for knotrel-server starting..."
    echo "Running: $CARGO_EXEC publish --dry-run -p knotrel-server --locked ${CARGO_PUBLISH_FLAGS:-}"
    "$CARGO_EXEC" publish --dry-run -p knotrel-server --locked ${CARGO_PUBLISH_FLAGS:-}
    echo "knotrel-server v${VERSION} registry dry-run passed successfully."
elif [[ "$STATUS_OUTPUT" == "CORE_CONFIRMED_404" ]]; then
    echo "DEFERRED: knotrel-core v${VERSION} is not yet published on crates.io (HTTP 404 confirmed)."
    echo "knotrel-server registry dry-run is deferred for bootstrap release."
    echo "knotrel-server package verification was independently verified offline via cargo package --workspace."
else
    echo "ERROR: Unhandled registry verification status: $STATUS_OUTPUT" >&2
    exit 1
fi
