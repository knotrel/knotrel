#!/usr/bin/env bash

set -euo pipefail

# publish-crates.sh
# Publishes knotrel-core and knotrel-server sequentially to crates.io using CARGO_REGISTRY_TOKEN.
# - Validates candidate commit SHA, clean working tree, manifests, and pre-built bundle checksums.
# - Generates prospective packages and asserts byte-for-byte checksum identity before publication.
# - Preflights BOTH target crate versions against crates.io before attempting any upload.
# - Publishes knotrel-core first, polling until indexed, available via API, and resolvable via Cargo.
# - Publishes knotrel-server with full verification (never --no-verify) against published knotrel-core.
# - Enforces a true overall deadline for readiness checks with actionable diagnostics.
# - Stops for manual reconciliation if identity is uncertain or checksums mismatch.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="${KNOTREL_REPO_DIR:-$REPO_ROOT}"
cd "$TARGET_REPO"

VERSION="${1:-}"
VERSION="${VERSION#v}"
CANDIDATE_SHA="${2:-}"
ASSETS_DIR="${3:-final-release-assets}"

if [[ -z "$VERSION" || -z "$CANDIDATE_SHA" ]]; then
    echo "Usage: $0 <version> <candidate-commit-sha> [assets-dir]" >&2
    echo "Example: $0 0.1.0 9ef3afdecb4c72673e382c143dcc60a01ab77089 final-release-assets" >&2
    exit 1
fi

CARGO_EXEC="${CARGO_BIN:-${CARGO_EXEC:-cargo}}"
API_BASE="${CRATES_IO_API_BASE:-https://crates.io/api/v1/crates}"
INDEX_BASE="${CRATES_IO_INDEX_BASE:-https://index.crates.io}"
MAX_WAIT_SECONDS="${PUBLISH_MAX_WAIT_SECONDS:-300}"
POLL_INTERVAL_SECONDS="${PUBLISH_POLL_INTERVAL_SECONDS:-10}"

compute_sha256() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        python3 -c "import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest())" "$file"
    fi
}

echo "=== 1. Validating inputs, commit SHA, and repository state ==="

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "ERROR: Invalid version format '$VERSION'. Must strictly match 'X.Y.Z'." >&2
    exit 1
fi

if [[ ! "$CANDIDATE_SHA" =~ ^[0-9a-f]{40}$ ]]; then
    echo "ERROR: Invalid commit SHA format '$CANDIDATE_SHA'. Must be a 40-character hex SHA." >&2
    exit 1
fi

CURRENT_HEAD="$(git rev-parse HEAD)"
if [[ "$CURRENT_HEAD" != "$CANDIDATE_SHA" ]]; then
    echo "ERROR: Checked out HEAD ($CURRENT_HEAD) does not match candidate commit SHA ($CANDIDATE_SHA)!" >&2
    exit 1
fi

if ! git diff --quiet; then
    echo "ERROR: Working tree contains unstaged changes! Publishing requires a clean checkout." >&2
    git status --short >&2
    exit 1
fi

if ! git diff --cached --quiet; then
    echo "ERROR: Working tree contains staged uncommitted changes! Publishing requires a clean checkout." >&2
    git status --short >&2
    exit 1
fi

UNTRACKED="$(git ls-files --others --exclude-standard)"
if [[ -n "$UNTRACKED" ]]; then
    echo "ERROR: Working tree contains untracked files! Publishing requires a clean checkout." >&2
    echo "$UNTRACKED" >&2
    exit 1
fi

TAG_NAME="v${VERSION}"

resolve_and_verify_tag() {
    local tag_name="$1"
    local expected_sha="$2"
    local resolved_sha=""

    # 1. Verify local tag if present
    local local_tag_sha=""
    if git rev-parse "refs/tags/${tag_name}^{commit}" >/dev/null 2>&1; then
        local_tag_sha="$(git rev-parse "refs/tags/${tag_name}^{commit}")"
        if [[ "$local_tag_sha" != "$expected_sha" ]]; then
            echo "ERROR: Local tag '$tag_name' resolves to $local_tag_sha, expected candidate SHA $expected_sha!" >&2
            exit 1
        fi
        resolved_sha="$local_tag_sha"
    fi

    # 2. Verify remote tag on origin if origin remote is configured
    local has_origin=0
    if git remote get-url origin >/dev/null 2>&1; then
        has_origin=1
    fi

    if [[ "$has_origin" -eq 1 ]]; then
        local remote_err_file
        remote_err_file="$(mktemp 2>/dev/null || mktemp -t 'knotrel-tag-err')"
        local remote_out=""
        if ! remote_out="$(git ls-remote origin "refs/tags/${tag_name}" "refs/tags/${tag_name}^{}" 2>"$remote_err_file")"; then
            echo "ERROR: Failed to query remote origin for tag '$tag_name':" >&2
            cat "$remote_err_file" >&2
            rm -f "$remote_err_file"
            exit 1
        fi
        rm -f "$remote_err_file"

        local peeled_sha=""
        local direct_sha=""
        while IFS=$'\t' read -r sha ref; do
            if [[ "$ref" == "refs/tags/${tag_name}^{}" ]]; then
                peeled_sha="$sha"
            elif [[ "$ref" == "refs/tags/${tag_name}" ]]; then
                direct_sha="$sha"
            fi
        done <<< "$remote_out"

        local remote_tag_sha=""
        if [[ -n "$peeled_sha" ]]; then
            remote_tag_sha="$peeled_sha"
        elif [[ -n "$direct_sha" ]]; then
            remote_tag_sha="$direct_sha"
        fi

        if [[ -z "$remote_tag_sha" ]]; then
            echo "ERROR: Tag '$tag_name' not found on remote origin!" >&2
            exit 1
        fi

        if [[ "$remote_tag_sha" != "$expected_sha" ]]; then
            echo "ERROR: Remote tag '$tag_name' on origin resolves to $remote_tag_sha, expected candidate SHA $expected_sha!" >&2
            exit 1
        fi
        resolved_sha="$remote_tag_sha"
    fi

    if [[ -z "$resolved_sha" ]]; then
        echo "ERROR: Tag '$tag_name' could not be resolved (tag absent locally and no remote origin configured)!" >&2
        exit 1
    fi

    echo "Confirmed: Tag '$tag_name' points exactly to candidate commit $expected_sha."
}

resolve_and_verify_tag "$TAG_NAME" "$CANDIDATE_SHA"

# Verify manifest versions match requested version
python3 -c "
import json, subprocess, sys
cargo_bin = sys.argv[2]
try:
    raw = subprocess.check_output([cargo_bin, 'metadata', '--format-version', '1', '--no-deps'], text=True)
    pkgs = {p['name']: p['version'] for p in json.loads(raw).get('packages', [])}
except Exception:
    raw = subprocess.check_output(['cargo', 'metadata', '--format-version', '1', '--no-deps'], text=True)
    pkgs = {p['name']: p['version'] for p in json.loads(raw).get('packages', [])}
for crate in ['knotrel-core', 'knotrel-server']:
    v = pkgs.get(crate)
    if v != sys.argv[1]:
        print(f'ERROR: Manifest version mismatch for {crate}: manifest={v}, requested={sys.argv[1]}', file=sys.stderr)
        sys.exit(1)
" "$VERSION" "$CARGO_EXEC"

CARGO_REGISTRY_TOKEN="${CARGO_REGISTRY_TOKEN:-${CRATES_IO_TOKEN:-}}"
if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
    echo "ERROR: CARGO_REGISTRY_TOKEN is unset or empty! Publication requires an authenticated crates.io token." >&2
    exit 1
fi
echo "Authenticated environment verified (token present, length ${#CARGO_REGISTRY_TOKEN} chars)."

echo "=== 2. Validating release bundle and prospective package integrity ==="
CORE_BUNDLE="${ASSETS_DIR}/knotrel-core-${VERSION}.crate"
SERVER_BUNDLE="${ASSETS_DIR}/knotrel-server-${VERSION}.crate"

if [[ ! -f "$CORE_BUNDLE" ]]; then
    echo "ERROR: Expected core bundle not found at $CORE_BUNDLE" >&2
    exit 1
fi

if [[ ! -f "$SERVER_BUNDLE" ]]; then
    echo "ERROR: Expected server bundle not found at $SERVER_BUNDLE" >&2
    exit 1
fi

BUNDLE_CORE_SHA="$(compute_sha256 "$CORE_BUNDLE")"
BUNDLE_SERVER_SHA="$(compute_sha256 "$SERVER_BUNDLE")"

echo "Verified bundle artifact checksums:"
echo "  knotrel-core   v${VERSION}: sha256=${BUNDLE_CORE_SHA}"
echo "  knotrel-server v${VERSION}: sha256=${BUNDLE_SERVER_SHA}"

# Generate prospective packages in target/package to verify byte-level candidate reproducibility
echo "Ensuring candidate dependencies are fetched..."
"$CARGO_EXEC" fetch --locked
echo "Generating prospective packages from current candidate checkout..."
rm -rf target/package
"$CARGO_EXEC" package --workspace --locked ${CARGO_PUBLISH_EXTRA_FLAGS:-}

PROSPECTIVE_CORE="target/package/knotrel-core-${VERSION}.crate"
PROSPECTIVE_SERVER="target/package/knotrel-server-${VERSION}.crate"

if [[ ! -f "$PROSPECTIVE_CORE" || ! -f "$PROSPECTIVE_SERVER" ]]; then
    echo "ERROR: Prospective packaging failed to produce expected .crate archives!" >&2
    exit 1
fi

PROSPECTIVE_CORE_SHA="$(compute_sha256 "$PROSPECTIVE_CORE")"
PROSPECTIVE_SERVER_SHA="$(compute_sha256 "$PROSPECTIVE_SERVER")"

if [[ "$PROSPECTIVE_CORE_SHA" != "$BUNDLE_CORE_SHA" ]]; then
    echo "ERROR: Prospective package checksum mismatch for knotrel-core!" >&2
    echo "  Prospective: $PROSPECTIVE_CORE_SHA" >&2
    echo "  Bundle:      $BUNDLE_CORE_SHA" >&2
    echo "Candidate drift detected before publication! Aborting." >&2
    exit 1
fi

if [[ "$PROSPECTIVE_SERVER_SHA" != "$BUNDLE_SERVER_SHA" ]]; then
    echo "ERROR: Prospective package checksum mismatch for knotrel-server!" >&2
    echo "  Prospective: $PROSPECTIVE_SERVER_SHA" >&2
    echo "  Bundle:      $BUNDLE_SERVER_SHA" >&2
    echo "Candidate drift detected before publication! Aborting." >&2
    exit 1
fi

echo "Confirmed: Prospective candidate packages match verified bundle artifacts byte-for-byte."

# Helper: Query crates.io REST API
# Returns:
#   ABSENT
#   MATCHING:<checksum>
#   CONFLICT:<checksum>
#   ERROR:<diagnostic>
query_crate_rest_api() {
    local crate_name="$1"
    local version="$2"
    local expected_sha="$3"
    local remaining_timeout="$4"

    python3 -c "
import json, sys, urllib.error, urllib.request

crate = sys.argv[1]
ver = sys.argv[2]
expected = sys.argv[3]
timeout = float(sys.argv[4])
api_base = sys.argv[5].rstrip('/')
url = f'{api_base}/{crate}/{ver}'

headers = {'User-Agent': 'knotrel-release-script (knotrel@users.noreply.github.com)'}
req = urllib.request.Request(url, headers=headers)

try:
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        if resp.status == 200:
            data = json.loads(resp.read().decode('utf-8'))
            cksum = data.get('version', {}).get('checksum', '')
            num = data.get('version', {}).get('num', '')
            if num != ver:
                print(f'CONFLICT:mismatched_version_num_{num}')
                sys.exit(0)
            if cksum == expected:
                print(f'MATCHING:{cksum}')
            else:
                print(f'CONFLICT:{cksum}')
            sys.exit(0)
        else:
            print(f'ERROR:Unexpected HTTP status {resp.status}')
            sys.exit(0)
except urllib.error.HTTPError as e:
    if e.code == 404:
        print('ABSENT')
        sys.exit(0)
    elif e.code == 429:
        print('ERROR:crates.io API rate limit (HTTP 429)')
        sys.exit(0)
    elif 500 <= e.code < 600:
        print(f'ERROR:crates.io API server error (HTTP {e.code})')
        sys.exit(0)
    else:
        print(f'ERROR:crates.io API HTTP {e.code}: {e.reason}')
        sys.exit(0)
except urllib.error.URLError as e:
    print(f'ERROR:Network/timeout: {e.reason}')
    sys.exit(0)
except Exception as e:
    print(f'ERROR:Unexpected exception: {e}')
    sys.exit(0)
" "$crate_name" "$version" "$expected_sha" "$remaining_timeout" "$API_BASE"
}

# Helper: Check sparse index
query_sparse_index() {
    local crate_name="$1"
    local version="$2"
    local expected_sha="$3"
    local remaining_timeout="$4"

    python3 -c "
import json, sys, urllib.error, urllib.request

crate = sys.argv[1]
ver = sys.argv[2]
expected = sys.argv[3]
timeout = float(sys.argv[4])
index_base = sys.argv[5].rstrip('/')

# Compute crates.io 2/2 prefix path
if len(crate) == 1:
    path = f'1/{crate}'
elif len(crate) == 2:
    path = f'2/{crate}'
elif len(crate) == 3:
    path = f'3/{crate[0]}/{crate}'
else:
    path = f'{crate[0:2]}/{crate[2:4]}/{crate}'

url = f'{index_base}/{path}'
headers = {'User-Agent': 'knotrel-release-script (knotrel@users.noreply.github.com)'}
req = urllib.request.Request(url, headers=headers)

try:
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        if resp.status == 200:
            lines = resp.read().decode('utf-8').splitlines()
            for line in lines:
                if not line.strip():
                    continue
                try:
                    entry = json.loads(line)
                    if entry.get('vers') == ver:
                        cksum = entry.get('cksum', '')
                        if cksum == expected:
                            print(f'MATCHING:{cksum}')
                        else:
                            print(f'CONFLICT:{cksum}')
                        sys.exit(0)
                except Exception:
                    pass
            print('ABSENT')
            sys.exit(0)
        else:
            print(f'ERROR:Index HTTP status {resp.status}')
            sys.exit(0)
except urllib.error.HTTPError as e:
    if e.code == 404:
        print('ABSENT')
        sys.exit(0)
    else:
        print(f'ERROR:Index HTTP {e.code}: {e.reason}')
        sys.exit(0)
except urllib.error.URLError as e:
    print(f'ERROR:Index network/timeout: {e.reason}')
    sys.exit(0)
except Exception as e:
    print(f'ERROR:Index unexpected exception: {e}')
    sys.exit(0)
" "$crate_name" "$version" "$expected_sha" "$remaining_timeout" "$INDEX_BASE"
}

# Helper: Test Cargo resolution in an isolated probe directory outside workspace
probe_cargo_resolution() {
    local crate_name="$1"
    local version="$2"
    local expected_sha="$3"
    local timeout_sec="${4:-30}"

    python3 -c "
import os, shutil, subprocess, sys, tempfile

timeout = float(sys.argv[1])
crate_name = sys.argv[2]
version = sys.argv[3]
expected_sha = sys.argv[4]
cargo_exec = sys.argv[5]

probe_dir = tempfile.mkdtemp(prefix='knotrel-cargo-probe-')
try:
    # Write minimal valid Rust crate manifest
    cargo_toml = f'''[package]
name = \"knotrel-registry-readiness-probe\"
version = \"0.0.0\"
edition = \"2021\"

[dependencies]
{crate_name} = \"={version}\"
'''
    with open(os.path.join(probe_dir, 'Cargo.toml'), 'w') as f:
        f.write(cargo_toml)

    src_dir = os.path.join(probe_dir, 'src')
    os.makedirs(src_dir, exist_ok=True)
    with open(os.path.join(src_dir, 'lib.rs'), 'w') as f:
        f.write('pub fn probe() {}\n')

    # Run cargo generate-lockfile under bounded timeout
    try:
        res = subprocess.run(
            [cargo_exec, 'generate-lockfile'],
            cwd=probe_dir,
            capture_output=True,
            text=True,
            timeout=timeout
        )
        if res.returncode != 0:
            print('UNRESOLVED')
            sys.exit(0)
    except subprocess.TimeoutExpired:
        print('UNRESOLVED')
        sys.exit(0)

    lockfile_path = os.path.join(probe_dir, 'Cargo.lock')
    if not os.path.exists(lockfile_path):
        print('UNRESOLVED')
        sys.exit(0)

    with open(lockfile_path, 'rb') as f:
        try:
            import tomllib
            data = tomllib.load(f)
        except Exception:
            print('UNRESOLVED')
            sys.exit(0)

    packages = data.get('package', [])
    found = False
    for pkg in packages:
        if pkg.get('name') == crate_name and pkg.get('version') == version:
            src = pkg.get('source', '')
            cksum = pkg.get('checksum', '')
            if src.startswith('registry+') and cksum == expected_sha:
                found = True
                break

    if found:
        print('RESOLVED')
    else:
        print('UNRESOLVED')
finally:
    shutil.rmtree(probe_dir, ignore_errors=True)
" "$timeout_sec" "$crate_name" "$version" "$expected_sha" "$CARGO_EXEC"
}

echo "=== 3. Preflight check: inspecting both target crates on crates.io ==="
# Query both crates BEFORE executing any upload command (Finding MEDIUM 5)
CORE_PREFLIGHT="$(query_crate_rest_api "knotrel-core" "$VERSION" "$BUNDLE_CORE_SHA" 15)"
SERVER_PREFLIGHT="$(query_crate_rest_api "knotrel-server" "$VERSION" "$BUNDLE_SERVER_SHA" 15)"

echo "Preflight query results:"
echo "  knotrel-core   v${VERSION}: $CORE_PREFLIGHT"
echo "  knotrel-server v${VERSION}: $SERVER_PREFLIGHT"

# Strict preflight conflict / error handling
if [[ "$CORE_PREFLIGHT" =~ ^ERROR: ]]; then
    echo "ERROR: Preflight query failed for knotrel-core: $CORE_PREFLIGHT" >&2
    exit 1
fi

if [[ "$SERVER_PREFLIGHT" =~ ^ERROR: ]]; then
    echo "ERROR: Preflight query failed for knotrel-server: $SERVER_PREFLIGHT" >&2
    exit 1
fi

if [[ "$CORE_PREFLIGHT" =~ ^CONFLICT: ]]; then
    echo "ERROR: Preflight conflict! knotrel-core v${VERSION} already exists on crates.io with conflicting checksum!" >&2
    echo "  Expected: $BUNDLE_CORE_SHA" >&2
    echo "  Remote:   ${CORE_PREFLIGHT#CONFLICT:}" >&2
    echo "Stopping before any upload action for manual reconciliation." >&2
    exit 1
fi

if [[ "$SERVER_PREFLIGHT" =~ ^CONFLICT: ]]; then
    echo "ERROR: Preflight conflict! knotrel-server v${VERSION} already exists on crates.io with conflicting checksum!" >&2
    echo "  Expected: $BUNDLE_SERVER_SHA" >&2
    echo "  Remote:   ${SERVER_PREFLIGHT#CONFLICT:}" >&2
    echo "Stopping before any upload action for manual reconciliation." >&2
    exit 1
fi

if [[ "$CORE_PREFLIGHT" =~ ^MATCHING: && "$SERVER_PREFLIGHT" =~ ^MATCHING: ]]; then
    echo "Confirmed: Both knotrel-core v${VERSION} and knotrel-server v${VERSION} already exist on crates.io with verified matching checksums."
    echo "No uploads necessary. Release artifacts already published."
    exit 0
fi

echo "Preflight validation succeeded. Safe to proceed with sequential publication."

echo "=== 4. Publishing and verifying knotrel-core ==="
if [[ "$CORE_PREFLIGHT" == "ABSENT" ]]; then
    echo "knotrel-core v${VERSION} is absent from crates.io. Uploading..."
    "$CARGO_EXEC" publish -p knotrel-core --locked ${CARGO_PUBLISH_EXTRA_FLAGS:-}
    echo "Uploaded knotrel-core v${VERSION} to crates.io."
else
    echo "knotrel-core v${VERSION} is already published with verified checksum. Skipping upload."
fi

echo "=== 5. Bounded registry readiness verification for knotrel-core ==="
START_TIME="$(date +%s)"
DEADLINE=$((START_TIME + MAX_WAIT_SECONDS))
CORE_READY=0

while true; do
    CURRENT_TIME="$(date +%s)"
    REMAINING=$((DEADLINE - CURRENT_TIME))
    if [[ "$REMAINING" -le 0 ]]; then
        echo "ERROR: Timed out after $MAX_WAIT_SECONDS seconds waiting for knotrel-core v${VERSION} to become ready and resolvable on crates.io." >&2
        exit 1
    fi

    API_TIMEOUT=$((REMAINING > 15 ? 15 : REMAINING))
    API_STATUS="$(query_crate_rest_api "knotrel-core" "$VERSION" "$BUNDLE_CORE_SHA" "$API_TIMEOUT")"

    CURRENT_TIME="$(date +%s)"
    REMAINING=$((DEADLINE - CURRENT_TIME))
    if [[ "$REMAINING" -le 0 ]]; then
        echo "ERROR: Timed out waiting for knotrel-core v${VERSION} sparse index readiness." >&2
        exit 1
    fi

    INDEX_TIMEOUT=$((REMAINING > 15 ? 15 : REMAINING))
    INDEX_STATUS="$(query_sparse_index "knotrel-core" "$VERSION" "$BUNDLE_CORE_SHA" "$INDEX_TIMEOUT")"

    echo "Polling knotrel-core v${VERSION} readiness (remaining: ${REMAINING}s):"
    echo "  REST API:     $API_STATUS"
    echo "  Sparse Index: $INDEX_STATUS"

    if [[ "$API_STATUS" =~ ^MATCHING: && "$INDEX_STATUS" =~ ^MATCHING: ]]; then
        CURRENT_TIME="$(date +%s)"
        REMAINING=$((DEADLINE - CURRENT_TIME))
        if [[ "$REMAINING" -le 0 ]]; then
            echo "ERROR: Timed out before Cargo resolution probe for knotrel-core v${VERSION}." >&2
            exit 1
        fi
        PROBE_TIMEOUT=$((REMAINING > 30 ? 30 : REMAINING))
        CARGO_PROBE="$(probe_cargo_resolution "knotrel-core" "$VERSION" "$BUNDLE_CORE_SHA" "$PROBE_TIMEOUT")"
        echo "  Cargo Probe:  $CARGO_PROBE"
        if [[ "$CARGO_PROBE" == "RESOLVED" ]]; then
            echo "Confirmed: knotrel-core v${VERSION} is ready on REST API, indexed in sparse registry, and resolved by Cargo."
            CORE_READY=1
            break
        fi
    elif [[ "$API_STATUS" =~ ^CONFLICT: || "$INDEX_STATUS" =~ ^CONFLICT: ]]; then
        echo "ERROR: Checksum conflict detected during readiness check!" >&2
        exit 1
    fi

    CURRENT_TIME="$(date +%s)"
    REMAINING=$((DEADLINE - CURRENT_TIME))
    if [[ "$REMAINING" -le 0 ]]; then
        echo "ERROR: Timed out waiting for knotrel-core v${VERSION} readiness." >&2
        exit 1
    fi

    SLEEP_DUR="$POLL_INTERVAL_SECONDS"
    if [[ "$SLEEP_DUR" -gt "$REMAINING" ]]; then
        SLEEP_DUR="$REMAINING"
    fi
    sleep "$SLEEP_DUR"
done

if [[ "$CORE_READY" -ne 1 ]]; then
    echo "ERROR: knotrel-core v${VERSION} is not fully ready on crates.io. Refusing to publish knotrel-server." >&2
    exit 1
fi

echo "=== 6. Publishing and verifying knotrel-server ==="
if [[ "$SERVER_PREFLIGHT" == "ABSENT" ]]; then
    echo "knotrel-server v${VERSION} is absent from crates.io. Uploading with standard verification..."
    # Cargo verification compiles knotrel-server resolving published knotrel-core from crates.io
    "$CARGO_EXEC" publish -p knotrel-server --locked ${CARGO_PUBLISH_EXTRA_FLAGS:-}
    echo "Uploaded knotrel-server v${VERSION} to crates.io."
else
    echo "knotrel-server v${VERSION} is already published with verified checksum. Skipping upload."
fi

echo "=== 7. Bounded registry readiness verification for knotrel-server ==="
SERVER_READY=0
START_TIME="$(date +%s)"
DEADLINE=$((START_TIME + MAX_WAIT_SECONDS))

while true; do
    CURRENT_TIME="$(date +%s)"
    REMAINING=$((DEADLINE - CURRENT_TIME))
    if [[ "$REMAINING" -le 0 ]]; then
        echo "ERROR: Timed out after $MAX_WAIT_SECONDS seconds waiting for knotrel-server v${VERSION} to become ready on crates.io." >&2
        exit 1
    fi

    CALL_TIMEOUT=15
    if [[ "$CALL_TIMEOUT" -gt "$REMAINING" ]]; then
        CALL_TIMEOUT="$REMAINING"
    fi

    API_STATUS="$(query_crate_rest_api "knotrel-server" "$VERSION" "$BUNDLE_SERVER_SHA" "$CALL_TIMEOUT")"
    INDEX_STATUS="$(query_sparse_index "knotrel-server" "$VERSION" "$BUNDLE_SERVER_SHA" "$CALL_TIMEOUT")"

    echo "Polling knotrel-server v${VERSION} readiness (remaining: ${REMAINING}s):"
    echo "  REST API:     $API_STATUS"
    echo "  Sparse Index: $INDEX_STATUS"

    if [[ "$API_STATUS" =~ ^MATCHING: && "$INDEX_STATUS" =~ ^MATCHING: ]]; then
        CARGO_PROBE="$(probe_cargo_resolution "knotrel-server" "$VERSION" "$BUNDLE_SERVER_SHA")"
        echo "  Cargo Probe:  $CARGO_PROBE"
        if [[ "$CARGO_PROBE" == "RESOLVED" ]]; then
            echo "Confirmed: knotrel-server v${VERSION} is ready on REST API, indexed in sparse registry, and resolved by Cargo."
            SERVER_READY=1
            break
        fi
    elif [[ "$API_STATUS" =~ ^CONFLICT: || "$INDEX_STATUS" =~ ^CONFLICT: ]]; then
        echo "ERROR: Checksum conflict detected during readiness check for knotrel-server!" >&2
        exit 1
    fi

    SLEEP_DUR="$POLL_INTERVAL_SECONDS"
    if [[ "$SLEEP_DUR" -gt "$REMAINING" ]]; then
        SLEEP_DUR="$REMAINING"
    fi
    sleep "$SLEEP_DUR"
done

if [[ "$SERVER_READY" -ne 1 ]]; then
    echo "ERROR: knotrel-server v${VERSION} is not fully ready on crates.io." >&2
    exit 1
fi

echo "=== 8. crates.io publication complete and verified ==="
echo "Candidate commit SHA: $CANDIDATE_SHA"
echo "Release version:      v${VERSION}"
echo "knotrel-core   sha256: $BUNDLE_CORE_SHA"
echo "knotrel-server sha256: $BUNDLE_SERVER_SHA"
echo "Both crates successfully published and verified on crates.io."
