#!/usr/bin/env bash

set -euo pipefail

# verify-source-packages.sh
# Packages knotrel-core and knotrel-server into .crate archives, extracts them into
# an isolated temporary directory outside the workspace, verifies license integrity,
# patches server -> extracted core dependency, and validates offline buildability.
# Supports dynamic version detection from Cargo metadata or an explicit version argument.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REQUESTED_VERSION="${1:-}"
REQUESTED_VERSION="${REQUESTED_VERSION#v}"

echo "=== 1. Resolving candidate package version ==="
READ_VERSIONS="$(python3 -c "
import json, subprocess, sys

raw_meta = subprocess.check_output(
    ['cargo', 'metadata', '--format-version', '1', '--no-deps'],
    cwd='${REPO_ROOT}',
    text=True
)
meta = json.loads(raw_meta)
pkgs = {p['name']: p['version'] for p in meta.get('packages', [])}

core_ver = pkgs.get('knotrel-core')
server_ver = pkgs.get('knotrel-server')

if not core_ver or not server_ver:
    print('ERROR: Could not locate knotrel-core or knotrel-server in workspace metadata.', file=sys.stderr)
    sys.exit(1)

if core_ver != server_ver:
    print(f'ERROR: Workspace crate version mismatch: knotrel-core={core_ver}, knotrel-server={server_ver}', file=sys.stderr)
    sys.exit(1)

req_ver = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] else ''
if req_ver:
    if req_ver != core_ver:
        print(f'ERROR: Requested version {req_ver} does not match workspace metadata version {core_ver}', file=sys.stderr)
        sys.exit(1)

print(f'{core_ver}')
" "${REQUESTED_VERSION}")"

VERSION="$READ_VERSIONS"
echo "Candidate package version: $VERSION"

echo "=== 2. Cleaning stale package directory ==="
# Prevent accidental reuse or detection of stale artifacts from prior runs/versions
rm -rf target/package

echo "=== 3. Preparation: Fetching locked dependencies ==="
cargo fetch --locked

echo "=== 4. Packaging and verifying workspace crates offline ==="
cargo package --workspace --offline --allow-dirty

CORE_CRATE="target/package/knotrel-core-${VERSION}.crate"
SERVER_CRATE="target/package/knotrel-server-${VERSION}.crate"

if [[ ! -f "$CORE_CRATE" ]]; then
    echo "ERROR: Expected $CORE_CRATE does not exist." >&2
    exit 1
fi

if [[ ! -f "$SERVER_CRATE" ]]; then
    echo "ERROR: Expected $SERVER_CRATE does not exist." >&2
    exit 1
fi

# Reject unexpected/stale crate files in target/package
CRATE_COUNT="$(find target/package -maxdepth 1 -name '*.crate' | wc -l | tr -d ' ')"
if [[ "$CRATE_COUNT" -ne 2 ]]; then
    echo "ERROR: Unexpected number of .crate archives in target/package (expected 2, found $CRATE_COUNT)." >&2
    exit 1
fi

echo "=== 5. Creating isolated temporary directory ==="
TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'knotrel-src-verify')"
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

echo "Extracted workspace directory: $TMP_DIR"

echo "=== 6. Extracting source archives ==="
tar -xzf "$CORE_CRATE" -C "$TMP_DIR"
tar -xzf "$SERVER_CRATE" -C "$TMP_DIR"

EXTRACTED_CORE="$TMP_DIR/knotrel-core-${VERSION}"
EXTRACTED_SERVER="$TMP_DIR/knotrel-server-${VERSION}"

if [[ ! -d "$EXTRACTED_CORE" || ! -d "$EXTRACTED_SERVER" ]]; then
    echo "ERROR: Failed to find extracted directories in $TMP_DIR." >&2
    exit 1
fi

echo "=== 7. Verifying licenses in extracted packages ==="
if [[ ! -f "$EXTRACTED_CORE/LICENSE.md" ]]; then
    echo "ERROR: LICENSE.md missing from knotrel-core archive." >&2
    exit 1
fi

if [[ ! -f "$EXTRACTED_SERVER/LICENSE.md" ]]; then
    echo "ERROR: LICENSE.md missing from knotrel-server archive." >&2
    exit 1
fi

if ! cmp -s "$REPO_ROOT/LICENSE.md" "$EXTRACTED_CORE/LICENSE.md"; then
    echo "ERROR: LICENSE.md in knotrel-core does not match repository LICENSE.md." >&2
    exit 1
fi

if ! cmp -s "$REPO_ROOT/LICENSE.md" "$EXTRACTED_SERVER/LICENSE.md"; then
    echo "ERROR: LICENSE.md in knotrel-server does not match repository LICENSE.md." >&2
    exit 1
fi

if ! grep -q "Elastic License 2.0" "$EXTRACTED_CORE/LICENSE.md"; then
    echo "ERROR: Elastic License 2.0 text missing from knotrel-core LICENSE.md." >&2
    exit 1
fi

if ! grep -q "Elastic License 2.0" "$EXTRACTED_SERVER/LICENSE.md"; then
    echo "ERROR: Elastic License 2.0 text missing from knotrel-server LICENSE.md." >&2
    exit 1
fi
echo "License verification succeeded for both packages."

echo "=== 8. Configuring isolated Cargo resolution override ==="
mkdir -p "$EXTRACTED_SERVER/.cargo"
cat << EOF > "$EXTRACTED_SERVER/.cargo/config.toml"
[patch.crates-io]
knotrel-core = { path = "../knotrel-core-${VERSION}" }
EOF

echo "=== 9. Validating exact isolated dependency resolution via metadata ==="
python3 -c "
import json, os, subprocess, sys

server_dir = sys.argv[1]
expected_core_manifest = os.path.realpath(sys.argv[2])
repo_root = os.path.realpath(sys.argv[3])

raw_meta = subprocess.check_output(
    ['cargo', 'metadata', '--format-version', '1', '--offline'],
    cwd=server_dir,
    text=True
)
meta = json.loads(raw_meta)
core_packages = [p for p in meta.get('packages', []) if p.get('name') == 'knotrel-core']

if not core_packages:
    print('ERROR: knotrel-core not found in cargo metadata packages list', file=sys.stderr)
    sys.exit(1)

resolved_manifest = os.path.realpath(core_packages[0]['manifest_path'])

print(f'Expected core manifest: {expected_core_manifest}')
print(f'Resolved core manifest: {resolved_manifest}')

if resolved_manifest != expected_core_manifest:
    print(f'ERROR: Manifest path mismatch! Expected {expected_core_manifest}, got {resolved_manifest}', file=sys.stderr)
    sys.exit(1)

if resolved_manifest.startswith(repo_root):
    print(f'ERROR: Resolved core manifest is inside repository root ({repo_root})!', file=sys.stderr)
    sys.exit(1)

print('Confirmed: knotrel-core resolved exactly and exclusively from the isolated extracted source archive.')
" "$EXTRACTED_SERVER" "$EXTRACTED_CORE/Cargo.toml" "$REPO_ROOT"

echo "=== 10. Building extracted knotrel-server against extracted core ==="
(
    cd "$EXTRACTED_SERVER"
    cargo check --offline --locked
    cargo test --offline --locked --no-run
)

echo "=== Source package verification succeeded! ==="
