#!/usr/bin/env bash

set -euo pipefail

# package-release-binary.sh
# Packages knotrel-server executable into a release archive with LICENSE.md and README.md.
# Generates an individual SHA-256 checksum file and a machine-readable metadata JSON file.
# Verifies binary architecture format, provenance receipt, archive integrity, and exact entries.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-}"
TARGET="${2:-}"
BINARY_INPUT="${3:-}"
COMMIT_SHA="${4:-$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo "unknown")}"
PROVENANCE_INPUT="${5:-${PROVENANCE_FILE:-}}"

if [[ -z "$VERSION" || -z "$TARGET" || -z "$BINARY_INPUT" ]]; then
    echo "Usage: $0 <version> <target-triple> <path-to-binary> [commit-sha] [provenance-file]" >&2
    echo "Example: $0 0.1.0 aarch64-apple-darwin target/release/knotrel-server" >&2
    exit 1
fi

compute_sha256() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        echo "ERROR: Neither sha256sum nor shasum available" >&2
        exit 1
    fi
}

RUST_TOOLCHAIN_INFO="$(rustc --version 2>/dev/null || echo "unknown")"

# 1. Verify version against Cargo.toml workspace package version
EXPECTED_VERSION="$(python3 -c "
import re
with open('Cargo.toml', 'r') as f:
    content = f.read()
m = re.search(r'version\s*=\s*\"([^\"]+)\"', content)
if m:
    print(m.group(1))
else:
    raise SystemExit('Version not found in Cargo.toml')
")"

if [[ "$VERSION" != "$EXPECTED_VERSION" ]]; then
    echo "ERROR: Requested version '$VERSION' does not match workspace Cargo.toml version '$EXPECTED_VERSION'." >&2
    exit 1
fi

# 2. Strict verification of binary input existence (no fallback searches)
if [[ ! -f "$BINARY_INPUT" ]]; then
    echo "ERROR: Specified binary does not exist at: $BINARY_INPUT" >&2
    echo "Silent fallback to host/debug binary is forbidden." >&2
    exit 1
fi

# 3. Compute digest of actual binary input
BINARY_SHA256="$(compute_sha256 "$BINARY_INPUT")"

# 4. Strict binary architecture and format verification
python3 -c "
import os, struct, sys

bin_path = sys.argv[1]
target = sys.argv[2]

if not os.path.isfile(bin_path):
    sys.exit(f'ERROR: Binary file does not exist: {bin_path}')

with open(bin_path, 'rb') as f:
    data = f.read(1024)

if target == 'x86_64-pc-windows-msvc':
    if not bin_path.endswith('.exe'):
        sys.exit(f'ERROR: Windows binary must have .exe extension: {bin_path}')
    if len(data) < 64 or data[:2] != b'MZ':
        sys.exit(f'ERROR: File {bin_path} is not a valid PE executable: missing MZ header')
    pe_offset = struct.unpack_from('<I', data, 0x3C)[0]
    with open(bin_path, 'rb') as f2:
        f2.seek(pe_offset)
        pe_sig = f2.read(4)
        if pe_sig != b'PE\x00\x00':
            sys.exit(f'ERROR: File {bin_path} is missing PE signature at offset {hex(pe_offset)}')
        file_hdr = f2.read(20)
        if len(file_hdr) < 20:
            sys.exit(f'ERROR: File {bin_path} has truncated PE file header')
        machine = struct.unpack_from('<H', file_hdr, 0)[0]
        if machine != 0x8664:
            sys.exit(f'ERROR: PE machine architecture is {hex(machine)}, expected 0x8664 (x86-64 AMD64)')
        opt_hdr = f2.read(2)
        if len(opt_hdr) < 2:
            sys.exit(f'ERROR: File {bin_path} has truncated PE optional header')
        magic = struct.unpack_from('<H', opt_hdr, 0)[0]
        if magic != 0x020B:
            sys.exit(f'ERROR: PE optional header magic is {hex(magic)}, expected 0x20b (PE32+ 64-bit)')
    print('Verified binary format: Windows PE32+ (x86-64)')

elif target == 'x86_64-unknown-linux-gnu':
    if len(data) < 20 or data[:4] != b'\x7fELF':
        sys.exit(f'ERROR: File {bin_path} is not a valid ELF binary: missing \\x7fELF magic')
    if data[4] != 2:
        sys.exit(f'ERROR: ELF binary {bin_path} is not 64-bit (EI_CLASS != 2)')
    machine = struct.unpack_from('<H', data, 18)[0]
    if machine != 0x3E:
        sys.exit(f'ERROR: ELF machine architecture is {hex(machine)}, expected 0x3e (x86-64)')
    print('Verified binary format: Linux ELF 64-bit (x86-64)')

elif target == 'aarch64-apple-darwin':
    if len(data) < 8 or data[:4] != b'\xcf\xfa\xed\xfe':
        sys.exit(f'ERROR: File {bin_path} is not a valid 64-bit little-endian Mach-O binary')
    cputype = struct.unpack_from('<I', data, 4)[0]
    if cputype != 0x0100000C:
        sys.exit(f'ERROR: Mach-O cputype is {hex(cputype)}, expected 0x0100000c (arm64)')
    print('Verified binary format: macOS Mach-O 64-bit (arm64)')

else:
    sys.exit(f'ERROR: Unsupported target triple: {target}. Only x86_64-unknown-linux-gnu, aarch64-apple-darwin, and x86_64-pc-windows-msvc are supported.')
" "$BINARY_INPUT" "$TARGET"

# 5. Build provenance receipt verification
if [[ -z "$PROVENANCE_INPUT" && -f "$(dirname "$BINARY_INPUT")/build-provenance.json" ]]; then
    PROVENANCE_INPUT="$(dirname "$BINARY_INPUT")/build-provenance.json"
fi

BUILD_PROVENANCE_VERIFIED="false"
if [[ -n "$PROVENANCE_INPUT" && -f "$PROVENANCE_INPUT" ]]; then
    echo "Verifying build provenance from: $PROVENANCE_INPUT"
    PROV_OUT="$(python3 -c "
import json, sys

prov_file = sys.argv[1]
expected_version = sys.argv[2]
expected_target = sys.argv[3]
expected_commit = sys.argv[4]
actual_binary_sha = sys.argv[5]

try:
    with open(prov_file, 'r') as f:
        data = json.load(f)
except Exception as e:
    sys.exit(f'ERROR: Failed to parse provenance JSON {prov_file}: {e}')

if data.get('version') != expected_version:
    sys.exit(f'ERROR: Provenance version mismatch: expected {expected_version}, got {data.get(\"version\")}')
if data.get('target') != expected_target:
    sys.exit(f'ERROR: Provenance target mismatch: expected {expected_target}, got {data.get(\"target\")}')
if expected_commit != 'unknown' and data.get('commit_sha') != expected_commit:
    sys.exit(f'ERROR: Provenance commit SHA mismatch: expected {expected_commit}, got {data.get(\"commit_sha\")}')
if data.get('binary_sha256') != actual_binary_sha:
    sys.exit(f'ERROR: Provenance binary SHA256 mismatch! Build receipt: {data.get(\"binary_sha256\")}, actual binary: {actual_binary_sha}')

rustc_ver = data.get('rustc_version')
if not rustc_ver or not isinstance(rustc_ver, str) or not rustc_ver.strip():
    sys.exit('ERROR: Provenance receipt is missing required toolchain metadata (rustc_version).')
if not rustc_ver.startswith('rustc '):
    sys.exit(f'ERROR: Invalid rustc_version format in provenance receipt: {rustc_ver}')

print(rustc_ver.strip())
" "$PROVENANCE_INPUT" "$VERSION" "$TARGET" "$COMMIT_SHA" "$BINARY_SHA256")"

    RUST_TOOLCHAIN_INFO="$PROV_OUT"
    BUILD_PROVENANCE_VERIFIED="true"
    echo "Confirmed: Build provenance matches binary SHA-256, target, version, commit SHA, and toolchain ($RUST_TOOLCHAIN_INFO)."
elif [[ "${REQUIRE_PROVENANCE:-0}" == "1" ]]; then
    echo "ERROR: Build provenance is required (REQUIRE_PROVENANCE=1) but no provenance receipt was provided or found." >&2
    exit 1
else
    echo "NOTE: Packaging without build provenance receipt (unverified provenance)."
fi

DIST_DIR="$REPO_ROOT/dist"
mkdir -p "$DIST_DIR"

STAGE_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'knotrel-pkg')"
cleanup() {
    rm -rf "$STAGE_DIR"
}
trap cleanup EXIT INT TERM

echo "=== Packaging knotrel-server v$VERSION for $TARGET ==="
echo "Source binary: $BINARY_INPUT"
echo "Binary SHA-256: $BINARY_SHA256"
echo "Commit SHA: $COMMIT_SHA"
echo "Rust toolchain: $RUST_TOOLCHAIN_INFO"
echo "Provenance verified: $BUILD_PROVENANCE_VERIFIED"

PKG_NAME="knotrel-server-v${VERSION}-${TARGET}"
STAGE_PKG="$STAGE_DIR/$PKG_NAME"
mkdir -p "$STAGE_PKG"

EXE_NAME="knotrel-server"
if [[ "$TARGET" == *"windows"* ]]; then
    EXE_NAME="knotrel-server.exe"
fi

cp "$BINARY_INPUT" "$STAGE_PKG/$EXE_NAME"
chmod 755 "$STAGE_PKG/$EXE_NAME"
cp "$REPO_ROOT/LICENSE.md" "$STAGE_PKG/LICENSE.md"
chmod 644 "$STAGE_PKG/LICENSE.md"
cp "$REPO_ROOT/README.md" "$STAGE_PKG/README.md"
chmod 644 "$STAGE_PKG/README.md"

# Remove any stray macOS AppleDouble metadata files before packaging
export COPYFILE_DISABLE=1
find "$STAGE_DIR" -name '._*' -delete 2>/dev/null || true

ARCHIVE_FILE=""
if [[ "$TARGET" == *"windows"* ]]; then
    ARCHIVE_FILE="$DIST_DIR/${PKG_NAME}.zip"
    rm -f "$ARCHIVE_FILE"
    python3 -c "
import os, sys, zipfile

stage_dir = sys.argv[1]
pkg_name = sys.argv[2]
archive_path = sys.argv[3]

with zipfile.ZipFile(archive_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk(os.path.join(stage_dir, pkg_name)):
        for f in sorted(files):
            full_path = os.path.join(root, f)
            arcname = os.path.relpath(full_path, stage_dir)
            zf.write(full_path, arcname)
" "$STAGE_DIR" "$PKG_NAME" "$ARCHIVE_FILE"

    echo "Created archive: $ARCHIVE_FILE"

    # Strictly validate Windows ZIP archive
    python3 -c "
import sys, zipfile

archive_path = sys.argv[1]
pkg_name = sys.argv[2]
exe_name = sys.argv[3]

with zipfile.ZipFile(archive_path, 'r') as zf:
    bad = zf.testzip()
    if bad:
        print(f'ERROR: Corrupt zip entry detected: {bad}', file=sys.stderr)
        sys.exit(1)
    names = sorted(zf.namelist())
    expected = sorted([
        f'{pkg_name}/{exe_name}',
        f'{pkg_name}/LICENSE.md',
        f'{pkg_name}/README.md'
    ])
    if names != expected:
        print(f'ERROR: Unexpected zip namelist! Expected {expected}, got {names}', file=sys.stderr)
        sys.exit(1)
    if any(n.startswith('._') or '/._' in n for n in names):
        print('ERROR: ZIP archive contains AppleDouble files', file=sys.stderr)
        sys.exit(1)
print('ZIP archive integrity and entries verified successfully.')
" "$ARCHIVE_FILE" "$PKG_NAME" "$EXE_NAME"

else
    ARCHIVE_FILE="$DIST_DIR/${PKG_NAME}.tar.gz"
    rm -f "$ARCHIVE_FILE"
    tar -czf "$ARCHIVE_FILE" -C "$STAGE_DIR" "$PKG_NAME"

    echo "Created archive: $ARCHIVE_FILE"

    # Strictly validate Unix tar.gz archive
    ENTRIES="$(tar -tf "$ARCHIVE_FILE")"
    echo "$ENTRIES"

    # Check for forbidden AppleDouble or hidden dotfiles
    if echo "$ENTRIES" | grep -q '\._'; then
        echo "ERROR: Archive contains AppleDouble files (._*):" >&2
        echo "$ENTRIES" | grep '\._' >&2
        exit 1
    fi
    if echo "$ENTRIES" | grep -E -q '/\.[^/]+$'; then
        echo "ERROR: Archive contains hidden dotfiles:" >&2
        echo "$ENTRIES" | grep -E '/\.[^/]+$' >&2
        exit 1
    fi

    # Check all required entries are present
    for req in "$PKG_NAME/$EXE_NAME" "$PKG_NAME/LICENSE.md" "$PKG_NAME/README.md"; do
        if ! echo "$ENTRIES" | grep -q -x "$req"; then
            echo "ERROR: Missing required archive entry: $req" >&2
            exit 1
        fi
    done

    # Check entry count (1 directory + 3 files = at most 4 entries)
    ENTRY_COUNT="$(echo "$ENTRIES" | sed '/^$/d' | wc -l | tr -d ' ')"
    if [[ "$ENTRY_COUNT" -gt 4 ]]; then
        echo "ERROR: Archive contains unexpected extra entries (total count: $ENTRY_COUNT):" >&2
        echo "$ENTRIES" >&2
        exit 1
    fi

    # Verify executable permission
    if ! tar -tvf "$ARCHIVE_FILE" | grep "$PKG_NAME/$EXE_NAME" | grep -q 'x'; then
        echo "ERROR: Executable in archive lacks execute permission:" >&2
        tar -tvf "$ARCHIVE_FILE" | grep "$PKG_NAME/$EXE_NAME" >&2
        exit 1
    fi
    echo "TAR.GZ archive integrity and entries verified successfully."
fi

# Calculate checksum from final archive bytes
ARCHIVE_FILENAME="$(basename "$ARCHIVE_FILE")"
CHECKSUM="$(compute_sha256 "$ARCHIVE_FILE")"
echo "$CHECKSUM  $ARCHIVE_FILENAME" > "${ARCHIVE_FILE}.sha256"
echo "Archive Checksum: $CHECKSUM"

# Generate machine-readable metadata JSON
METADATA_FILE="$DIST_DIR/${PKG_NAME}-metadata.json"
cat << EOF > "$METADATA_FILE"
{
  "product": "knotrel-server",
  "version": "$VERSION",
  "target": "$TARGET",
  "commit_sha": "$COMMIT_SHA",
  "rust_toolchain": "$RUST_TOOLCHAIN_INFO",
  "binary_file": "$EXE_NAME",
  "binary_sha256": "$BINARY_SHA256",
  "archive_file": "$ARCHIVE_FILENAME",
  "archive_sha256": "$CHECKSUM",
  "build_provenance_verified": $BUILD_PROVENANCE_VERIFIED,
  "license": "Elastic-2.0",
  "distribution_type": "source-available-preview",
  "limitations": [
    "in-memory ephemeral state (no persistence or recovery)",
    "not a general-purpose graph database",
    "loopback listener default; unauthenticated and unencrypted",
    "experimental backends ett and hdt are opt-in"
  ]
}
EOF
echo "Metadata written: $METADATA_FILE"

echo "=== Release package created and verified successfully: $ARCHIVE_FILENAME ==="
