#!/usr/bin/env bash

set -euo pipefail

# finalize-release.sh
# Finalizes a draft GitHub Release after crates.io publishing and validation succeed.
# - Revalidates release tag and candidate commit SHA.
# - Revalidates that both knotrel-core and knotrel-server are indexed and verified on crates.io.
# - Revalidates draft release identity, exact 14-asset bundle, sizes, and downloaded digests.
# - Explicitly publishes the release using `gh release edit --draft=false`.
# - Safe to retry; gracefully recognizes already-finalized releases.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="${KNOTREL_REPO_DIR:-$REPO_ROOT}"
cd "$TARGET_REPO"

VERSION="${1:-}"
CANDIDATE_SHA="${2:-}"
ASSETS_DIR="${3:-final-release-assets}"

if [[ -z "$VERSION" || -z "$CANDIDATE_SHA" ]]; then
    echo "Usage: $0 <version> <candidate-commit-sha> [assets-dir]" >&2
    exit 1
fi

GH_EXEC="${GH_BIN:-gh}"
API_BASE="${CRATES_IO_API_BASE:-https://crates.io/api/v1/crates}"
TAG_NAME="v${VERSION}"

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

echo "=== 1. Revalidating candidate tag and commit SHA ==="
resolve_and_verify_tag "$TAG_NAME" "$CANDIDATE_SHA"

echo "=== 2. Revalidating crates.io publication status ==="
CORE_BUNDLE="${ASSETS_DIR}/knotrel-core-${VERSION}.crate"
SERVER_BUNDLE="${ASSETS_DIR}/knotrel-server-${VERSION}.crate"

if [[ ! -f "$CORE_BUNDLE" || ! -f "$SERVER_BUNDLE" ]]; then
    echo "ERROR: Missing crate bundle artifacts in $ASSETS_DIR" >&2
    exit 1
fi

EXPECTED_CORE_SHA="$(compute_sha256 "$CORE_BUNDLE")"
EXPECTED_SERVER_SHA="$(compute_sha256 "$SERVER_BUNDLE")"

python3 -c "
import json, sys, urllib.error, urllib.request

api_base = sys.argv[1].rstrip('/')
ver = sys.argv[2]
exp_core = sys.argv[3]
exp_server = sys.argv[4]

def verify_crate(name, exp):
    url = f'{api_base}/{name}/{ver}'
    req = urllib.request.Request(url, headers={'User-Agent': 'knotrel-release-script (knotrel@users.noreply.github.com)'})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            if resp.status != 200:
                print(f'ERROR: {name} API status {resp.status}', file=sys.stderr)
                sys.exit(1)
            data = json.loads(resp.read().decode('utf-8'))
            cksum = data.get('version', {}).get('checksum', '')
            if cksum != exp:
                print(f'ERROR: {name} checksum mismatch on crates.io: expected {exp}, got {cksum}', file=sys.stderr)
                sys.exit(1)
    except Exception as e:
        print(f'ERROR: Failed to verify {name} on crates.io: {e}', file=sys.stderr)
        sys.exit(1)

verify_crate('knotrel-core', exp_core)
verify_crate('knotrel-server', exp_server)
print('Confirmed: Both crates.io packages verified with matching SHA256 checksums.')
" "$API_BASE" "$VERSION" "$EXPECTED_CORE_SHA" "$EXPECTED_SERVER_SHA"

echo "=== 3. Inspecting and verifying GitHub Release before finalization ==="
VIEW_STDERR_FILE="$(mktemp)"
VIEW_STDOUT_FILE="$(mktemp)"
cleanup_view_tmp() {
    rm -f "$VIEW_STDERR_FILE" "$VIEW_STDOUT_FILE"
}
trap cleanup_view_tmp EXIT

if ! "$GH_EXEC" release view "$TAG_NAME" --json isDraft,tagName,targetCommitish,assets >"$VIEW_STDOUT_FILE" 2>"$VIEW_STDERR_FILE"; then
    echo "ERROR: Failed to inspect release $TAG_NAME:" >&2
    cat "$VIEW_STDERR_FILE" >&2
    exit 1
fi

RAW_JSON="$(cat "$VIEW_STDOUT_FILE")"

EXPECTED_FILES=(
    "knotrel-server-${TAG_NAME}-x86_64-unknown-linux-gnu.tar.gz"
    "knotrel-server-${TAG_NAME}-x86_64-unknown-linux-gnu.tar.gz.sha256"
    "knotrel-server-${TAG_NAME}-x86_64-unknown-linux-gnu-metadata.json"
    "knotrel-server-${TAG_NAME}-aarch64-apple-darwin.tar.gz"
    "knotrel-server-${TAG_NAME}-aarch64-apple-darwin.tar.gz.sha256"
    "knotrel-server-${TAG_NAME}-aarch64-apple-darwin-metadata.json"
    "knotrel-server-${TAG_NAME}-x86_64-pc-windows-msvc.zip"
    "knotrel-server-${TAG_NAME}-x86_64-pc-windows-msvc.zip.sha256"
    "knotrel-server-${TAG_NAME}-x86_64-pc-windows-msvc-metadata.json"
    "knotrel-core-${VERSION}.crate"
    "knotrel-core-${VERSION}.crate.sha256"
    "knotrel-server-${VERSION}.crate"
    "knotrel-server-${VERSION}.crate.sha256"
    "SHA256SUMS"
)

VALIDATION_RESULT="$(python3 -c "
import json, os, re, sys

try:
    data = json.loads(sys.argv[1])
except Exception as e:
    print(f'ERROR_JSON:Malformed release JSON: {e}')
    sys.exit(0)

cand_sha = sys.argv[2]
version = sys.argv[3]
assets_dir = sys.argv[4]
expected_names = sys.argv[5].split(',')

if 'isDraft' not in data or not isinstance(data['isDraft'], bool):
    print('ERROR_DATA:Missing or invalid boolean isDraft in release view')
    sys.exit(0)

is_draft = data['isDraft']
tag = data.get('tagName', '')
target_commit = data.get('targetCommitish', '')

if tag != f'v{version}':
    print(f'ERROR_TAG:Tag mismatch: expected v{version}, got {tag}')
    sys.exit(0)

if target_commit and re.fullmatch(r'[0-9a-f]{7,40}', target_commit):
    if not cand_sha.startswith(target_commit) and not target_commit.startswith(cand_sha):
        print(f'ERROR_COMMIT:Target commit mismatch (expected {cand_sha}, got {target_commit})')
        sys.exit(0)

existing_assets = {a.get('name'): a.get('size', 0) for a in data.get('assets', [])}

missing = [name for name in expected_names if name not in existing_assets]
if missing:
    print(f'ERROR_ASSETS:Missing assets on existing release: {missing}')
    sys.exit(0)

if len(existing_assets) != len(expected_names):
    extra = [name for name in existing_assets if name not in expected_names]
    print(f'ERROR_ASSETS:Unexpected asset inventory on existing release: {extra}')
    sys.exit(0)

for name in expected_names:
    exp_path = os.path.join(assets_dir, name)
    exp_size = os.path.getsize(exp_path)
    act_size = existing_assets[name]
    if act_size != exp_size:
        print(f'ERROR_SIZE:Asset {name} size mismatch: expected {exp_size}, got {act_size}')
        sys.exit(0)

if is_draft:
    print('VALID_DRAFT')
else:
    print('VALID_PUBLIC')
" "$RAW_JSON" "$CANDIDATE_SHA" "$VERSION" "$ASSETS_DIR" "$(IFS=,; echo "${EXPECTED_FILES[*]}")")"

if [[ "$VALIDATION_RESULT" =~ ^ERROR_ ]]; then
    echo "ERROR: Existing release $TAG_NAME failed pre-finalization verification:" >&2
    echo "  $VALIDATION_RESULT" >&2
    exit 1
elif [[ "$VALIDATION_RESULT" != "VALID_DRAFT" && "$VALIDATION_RESULT" != "VALID_PUBLIC" ]]; then
    echo "ERROR: Unexpected validation output: $VALIDATION_RESULT" >&2
    exit 1
fi

echo "=== 4. Downloading release assets to verify digests before finalization ==="
DOWNLOAD_VERIFY_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'knotrel-finalize-verify')"
cleanup_download() {
    rm -rf "$DOWNLOAD_VERIFY_DIR"
}
trap 'cleanup_view_tmp; cleanup_download' EXIT

if ! "$GH_EXEC" release download "$TAG_NAME" --dir "$DOWNLOAD_VERIFY_DIR"; then
    echo "ERROR: Failed to download release assets for verification of $TAG_NAME!" >&2
    exit 1
fi

for f in "${EXPECTED_FILES[@]}"; do
    REMOTE_FILE="${DOWNLOAD_VERIFY_DIR}/${f}"
    LOCAL_FILE="${ASSETS_DIR}/${f}"
    if [[ ! -f "$REMOTE_FILE" ]]; then
        echo "ERROR: Downloaded asset $f missing from release $TAG_NAME!" >&2
        exit 1
    fi
    REMOTE_DIGEST="$(compute_sha256 "$REMOTE_FILE")"
    LOCAL_DIGEST="$(compute_sha256 "$LOCAL_FILE")"
    if [[ "$REMOTE_DIGEST" != "$LOCAL_DIGEST" ]]; then
        echo "ERROR: SHA256 digest mismatch for release asset $f!" >&2
        echo "  Remote: $REMOTE_DIGEST" >&2
        echo "  Bundle: $LOCAL_DIGEST" >&2
        exit 1
    fi
done
echo "Confirmed: All 14 release asset digests verified byte-for-byte against candidate release bundle."

if [[ "$VALIDATION_RESULT" == "VALID_PUBLIC" ]]; then
    echo "INFO: GitHub Release $TAG_NAME is already public and fully verified with matching commit, tag, and 14-asset bundle."
    echo "Finalization already complete."
    exit 0
fi

echo "=== 5. Publishing draft release to public status ==="
"$GH_EXEC" release edit "$TAG_NAME" --draft=false

# Confirm status is now public
POST_VIEW_JSON="$("$GH_EXEC" release view "$TAG_NAME" --json isDraft)"
IS_STILL_DRAFT="$(python3 -c "
import json, sys
try:
    data = json.loads(sys.argv[1])
    is_draft = data.get('isDraft')
    if not isinstance(is_draft, bool):
        print('INVALID')
    elif is_draft:
        print('STILL_DRAFT')
    else:
        print('PUBLIC')
except Exception:
    print('ERROR')
" "$POST_VIEW_JSON")"

if [[ "$IS_STILL_DRAFT" != "PUBLIC" ]]; then
    echo "ERROR: GitHub Release $TAG_NAME failed transition to public (status: $IS_STILL_DRAFT)!" >&2
    exit 1
fi

echo "=== 6. GitHub Release $TAG_NAME finalized and published successfully ==="
