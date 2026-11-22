#!/usr/bin/env bash

set -euo pipefail

# check-reproducible-build.sh
#
# Rebuilds knotrel-server from a release tag or candidate commit and compares
# it with the binary of the GitHub Release, or of a release dry-run bundle.
#
# The rebuild runs the release workflow's build command
#   cargo build --release --locked --target <target> -p knotrel-server
# on a clean `git archive` export of the commit, with the toolchain pinned by
# that commit's rust-toolchain.toml (it must equal the release metadata's).
# Two inputs are fixed to the release build's values:
#   - SOURCE_DATE_EPOCH: C dependencies (libmimalloc-sys) embed __DATE__ and
#     __TIME__, which GCC and Clang take from it. Defaults to the commit time,
#     which the release workflow uses (from v1.0.0).
#   - --remap-path-prefix: dependency source paths under CARGO_HOME appear in
#     panic locations. The flag is added only when the local CARGO_HOME
#     differs from the GitHub runner's; on a runner the build is unchanged.
#
# Usage:
#   scripts/check-reproducible-build.sh <tag-or-commit> [target]
#
#   target defaults to the host target: x86_64-unknown-linux-gnu or
#   aarch64-apple-darwin (cross-target rebuilds are not supported).
#
# Environment:
#   RELEASE_VERSION   version of the reference assets, e.g. 1.0.0 (default:
#                     the tag without "v"); required for a commit.
#   PUBLISHED_DIR     directory holding the reference assets for the target
#                     (archive, .sha256, -metadata.json, SHA256SUMS), e.g. an
#                     extracted release dry-run bundle. Otherwise the assets
#                     are downloaded with `gh release download` into a new
#                     empty directory.
#   GITHUB_REPOSITORY repository to download from (default knotrel/knotrel).
#   RUNNER_CARGO_HOME CARGO_HOME of the runner that built the reference
#                     (default /home/runner/.cargo on Linux,
#                     /Users/runner/.cargo on macOS).
#   SOURCE_DATE_EPOCH build timestamp (default: the commit time). Releases up
#                     to v0.9.0 were built without it; pass the time embedded
#                     in the published binary to rebuild them.
#   ACCEPT            weakest accepted result: byte (default; SHA-256
#                     identical), equivalent (Mach-O: identical after
#                     excluding only LC_UUID and LC_CODE_SIGNATURE) or code
#                     (only __TEXT,__text / .text identical; diagnostic, it
#                     cannot close the gate without a maintainer decision).
#   Windows (x86_64-pc-windows-msvc) is not supported: MSVC ignores
#   SOURCE_DATE_EPOCH and PE headers carry the link time.
#   KEEP_WORKDIR=1    keep the temporary export, target dir and downloads.
#
# Exit status: 0 reproducible at the accepted level, 1 not reproducible,
# 3 setup error.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REF="${1:-}"
TARGET="${2:-}"
REPO="${GITHUB_REPOSITORY:-knotrel/knotrel}"
ACCEPT="${ACCEPT:-byte}"

if [[ -z "$REF" ]]; then
    sed -n '3,52p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 3
fi
case "$ACCEPT" in
    byte | equivalent | code) ;;
    *) echo "ERROR: ACCEPT must be byte, equivalent or code (got '$ACCEPT')." >&2; exit 3 ;;
esac

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) HOST_TARGET="x86_64-unknown-linux-gnu" ;;
    Darwin-arm64) HOST_TARGET="aarch64-apple-darwin" ;;
    *) HOST_TARGET="" ;;
esac
TARGET="${TARGET:-$HOST_TARGET}"
if [[ -z "$TARGET" || "$TARGET" != "$HOST_TARGET" ]]; then
    echo "ERROR: target '${TARGET:-?}' must be the host target (${HOST_TARGET:-unsupported host})." >&2
    exit 3
fi
case "$TARGET" in
    x86_64-unknown-linux-gnu) DEFAULT_RUNNER_CARGO_HOME="/home/runner/.cargo" ;;
    aarch64-apple-darwin) DEFAULT_RUNNER_CARGO_HOME="/Users/runner/.cargo" ;;
esac
RUNNER_CARGO_HOME="${RUNNER_CARGO_HOME:-$DEFAULT_RUNNER_CARGO_HOME}"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

sha256_check() {
    # Verify a "<digest>  <file>" list file from the current directory.
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$1"
    else
        shasum -a 256 -c "$1"
    fi
}

COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify "${REF}^{commit}")"
VERSION="${RELEASE_VERSION:-${REF#v}}"
VERSION="${VERSION#v}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
    echo "ERROR: cannot derive a release version from '$REF'; set RELEASE_VERSION." >&2
    exit 3
fi
PKG="knotrel-server-v${VERSION}-${TARGET}"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/knotrel-repro.XXXXXX")"
cleanup() {
    if [[ "${KEEP_WORKDIR:-0}" == "1" ]]; then
        echo "Kept work directory: $WORKDIR"
    else
        rm -rf "$WORKDIR"
    fi
}
trap cleanup EXIT

echo "=== Reproducible-build check: $REF ($COMMIT), $PKG ==="

# 1. Reference binary, with every published checksum verified first.
if [[ -n "${PUBLISHED_DIR:-}" ]]; then
    ASSETS="$(cd "$PUBLISHED_DIR" && pwd)"
else
    ASSETS="$WORKDIR/published"
    mkdir "$ASSETS"
    echo "Downloading $PKG assets from the $REPO release v$VERSION"
    gh release download "v$VERSION" -R "$REPO" -D "$ASSETS" \
        -p "${PKG}.tar.gz" -p "${PKG}.tar.gz.sha256" -p "${PKG}-metadata.json" -p SHA256SUMS
fi
(
    cd "$ASSETS"
    sha256_check "${PKG}.tar.gz.sha256"
    grep -F "  ${PKG}.tar.gz" SHA256SUMS > "$WORKDIR/sums-entry" || true
    if [[ "$(wc -l < "$WORKDIR/sums-entry" | tr -d ' ')" != "1" ]]; then
        echo "ERROR: ${PKG}.tar.gz is not listed exactly once in SHA256SUMS" >&2
        exit 1
    fi
    sha256_check "$WORKDIR/sums-entry"
)
mkdir "$WORKDIR/extracted"
tar -xzf "$ASSETS/${PKG}.tar.gz" -C "$WORKDIR/extracted" "${PKG}/knotrel-server"
REFERENCE="$WORKDIR/extracted/${PKG}/knotrel-server"
META="$(python3 -I -c '
import json, sys
m = json.load(open(sys.argv[1]))
print(m["commit_sha"]); print(m["rust_toolchain"]); print(m["binary_sha256"])
' "$ASSETS/${PKG}-metadata.json")"
META_COMMIT="$(sed -n 1p <<<"$META")"
META_RUSTC="$(sed -n 2p <<<"$META")"
META_SHA="$(sed -n 3p <<<"$META")"
if [[ "$META_COMMIT" != "$COMMIT" ]]; then
    echo "ERROR: reference metadata commit $META_COMMIT != $REF ($COMMIT)" >&2
    exit 1
fi
if [[ "$(sha256_of "$REFERENCE")" != "$META_SHA" ]]; then
    echo "ERROR: reference binary does not match binary_sha256 in its metadata" >&2
    exit 1
fi
echo "Reference verified: archive .sha256, SHA256SUMS entry, metadata commit and binary digest."

# 2. Clean source export of the commit (no untracked or ignored files).
SRC="$WORKDIR/src"
mkdir "$SRC"
git -C "$REPO_ROOT" archive "$COMMIT" | tar -x -C "$SRC"

# 3. Build like the release workflow. Flags and profile overrides from the
#    environment would change the output, so they are cleared.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS RUSTC RUSTC_WRAPPER \
    CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_TARGET_DIR
while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CARGO_PROFILE_[A-Z0-9_]*\)=.*/\1/p')
# CARGO_HOME may not exist yet (GitHub's macOS runners set it before
# anything creates it); cargo would create it anyway.
mkdir -p "${CARGO_HOME:-$HOME/.cargo}"
LOCAL_CARGO_HOME="$(cd "${CARGO_HOME:-$HOME/.cargo}" && pwd -P)"
if [[ "$LOCAL_CARGO_HOME" != "$RUNNER_CARGO_HOME" ]]; then
    export RUSTFLAGS="--remap-path-prefix=${LOCAL_CARGO_HOME}=${RUNNER_CARGO_HOME}"
    echo "Path remap: $LOCAL_CARGO_HOME -> $RUNNER_CARGO_HOME"
fi
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$REPO_ROOT" log -1 --format=%ct "$COMMIT")}"
echo "SOURCE_DATE_EPOCH: $SOURCE_DATE_EPOCH"
export CARGO_TARGET_DIR="$WORKDIR/target"
(
    cd "$SRC"
    RUSTC_VERSION="$(rustc --version)"
    echo "Toolchain: $RUSTC_VERSION"
    if [[ "$RUSTC_VERSION" != "$META_RUSTC" ]]; then
        echo "ERROR: toolchain '$RUSTC_VERSION' differs from the reference '$META_RUSTC'" >&2
        exit 3
    fi
    cargo build --release --locked --target "$TARGET" -p knotrel-server
)
REBUILT="$CARGO_TARGET_DIR/$TARGET/release/knotrel-server"

# 4. Compare.
set +e
python3 -I "$REPO_ROOT/scripts/compare-binaries.py" "$REFERENCE" "$REBUILT"
STATUS=$?
set -e
case "$STATUS" in
    0) LEVEL="byte" ;;
    2) LEVEL="equivalent" ;;
    4) LEVEL="code" ;;
    1) LEVEL="none" ;;
    *) exit 3 ;;
esac
rank() { case "$1" in byte) echo 3 ;; equivalent) echo 2 ;; code) echo 1 ;; *) echo 0 ;; esac; }
case "$LEVEL" in
    byte) SUMMARY="byte-identical" ;;
    equivalent) SUMMARY="NOT byte-identical; equivalent after excluding LC_UUID and LC_CODE_SIGNATURE" ;;
    code) SUMMARY="NOT byte-identical, NOT equivalent; only the code section is identical (does not close the gate without a recorded maintainer decision)" ;;
    *) SUMMARY="different" ;;
esac
if [[ "$(rank "$LEVEL")" -ge "$(rank "$ACCEPT")" ]]; then
    echo "PASS ($ACCEPT accepted): $PKG rebuild is $SUMMARY."
    exit 0
fi
echo "FAIL ($ACCEPT required): $PKG rebuild is $SUMMARY." >&2
exit 1
