#!/usr/bin/env bash

set -euo pipefail

# create-or-verify-draft-release.sh
# Safely and idempotently creates or verifies a draft GitHub Release.
# - If no release exists for the tag: creates draft release attaching verified assets.
# - If a draft release already exists: verifies candidate commit binding, tagName, exact asset inventory, sizes, and digests.
# - If an already-public release exists: verifies matching commit, tag, and asset integrity.
# - Distinguishes GitHub API errors from missing release.
# - Never silently overwrites or deletes an existing release.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="${KNOTREL_REPO_DIR:-$REPO_ROOT}"
cd "$TARGET_REPO"

VERSION="${1:-}"
VERSION="${VERSION#v}"
CANDIDATE_SHA="${2:-}"
ASSETS_DIR="${3:-final-release-assets}"

if [[ -z "$VERSION" || -z "$CANDIDATE_SHA" ]]; then
    echo "Usage: $0 <version> <candidate-commit-sha> [assets-dir]" >&2
    exit 1
fi

GH_EXEC="${GH_BIN:-gh}"
TAG_NAME="v${VERSION}"

NOTES_FILE="${NOTES_FILE:-}"
if [[ -z "$NOTES_FILE" ]]; then
    if [[ -f "docs/releases/${TAG_NAME}-release-notes.md" ]]; then
        NOTES_FILE="docs/releases/${TAG_NAME}-release-notes.md"
    elif [[ -f "docs/releases/release-notes-${TAG_NAME}.md" ]]; then
        NOTES_FILE="docs/releases/release-notes-${TAG_NAME}.md"
    fi
fi

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

echo "=== 1. Validating local release assets inventory ==="
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

for f in "${EXPECTED_FILES[@]}"; do
    if [[ ! -f "${ASSETS_DIR}/${f}" ]]; then
        echo "ERROR: Expected asset missing in ${ASSETS_DIR}: $f" >&2
        exit 1
    fi
done

echo "=== 2. Inspecting GitHub Release status for $TAG_NAME ==="
VIEW_STDERR_FILE="$(mktemp)"
VIEW_STDOUT_FILE="$(mktemp)"
cleanup_tmp() {
    rm -f "$VIEW_STDERR_FILE" "$VIEW_STDOUT_FILE"
}
trap cleanup_tmp EXIT

RELEASE_EXISTS=0
if "$GH_EXEC" release view "$TAG_NAME" --json isDraft,tagName,targetCommitish,assets >"$VIEW_STDOUT_FILE" 2>"$VIEW_STDERR_FILE"; then
    RELEASE_EXISTS=1
else
    VIEW_ERR="$(cat "$VIEW_STDERR_FILE")"
    if [[ "$VIEW_ERR" =~ (release\ not\ found|HTTP\ 404|could\ not\ find\ release|not\ found) ]]; then
        RELEASE_EXISTS=0
    else
        echo "ERROR: GitHub API error inspecting release $TAG_NAME:" >&2
        echo "$VIEW_ERR" >&2
        exit 1
    fi
fi

if [[ "$RELEASE_EXISTS" -eq 0 ]]; then
    echo "No existing release found for $TAG_NAME. Creating draft GitHub Release..."
    CREATE_ARGS=(
        release create "$TAG_NAME"
        --verify-tag
        --target "$CANDIDATE_SHA"
        --title "Knotrel $TAG_NAME Preview"
        --draft
    )
    if [[ -n "$NOTES_FILE" && -f "$NOTES_FILE" ]]; then
        CREATE_ARGS+=(--notes-file "$NOTES_FILE")
    else
        CREATE_ARGS+=(--generate-notes)
    fi
    CREATE_ARGS+=("${ASSETS_DIR}"/*)

    "$GH_EXEC" "${CREATE_ARGS[@]}"
    echo "Confirmed: Draft GitHub Release $TAG_NAME created successfully."
    exit 0
fi

echo "=== 3. Validating existing release identity and asset inventory ==="
RAW_JSON="$(cat "$VIEW_STDOUT_FILE")"

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
    echo "ERROR: Existing release $TAG_NAME failed identity verification:" >&2
    echo "  $VALIDATION_RESULT" >&2
    echo "Refusing to overwrite or reuse inconsistent release. Stopping for manual reconciliation." >&2
    exit 1
elif [[ "$VALIDATION_RESULT" != "VALID_DRAFT" && "$VALIDATION_RESULT" != "VALID_PUBLIC" ]]; then
    echo "ERROR: Unexpected validation output: $VALIDATION_RESULT" >&2
    exit 1
fi

echo "=== 4. Resolving and validating release tag ref ==="
resolve_and_verify_tag "$TAG_NAME" "$CANDIDATE_SHA"

echo "=== 5. Downloading release assets to verify digests against candidate bundle ==="
DOWNLOAD_VERIFY_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'knotrel-release-verify')"
cleanup_download() {
    rm -rf "$DOWNLOAD_VERIFY_DIR"
}
trap 'cleanup_tmp; cleanup_download' EXIT

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

if [[ "$VALIDATION_RESULT" == "VALID_DRAFT" ]]; then
    echo "Confirmed: Existing draft release $TAG_NAME matches candidate commit and exact 14-asset bundle."
    echo "Safely reusing existing draft release."
    exit 0
else
    echo "INFO: Existing release $TAG_NAME is already public and matches candidate commit and asset inventory."
    echo "No modifications needed."
    exit 0
fi
