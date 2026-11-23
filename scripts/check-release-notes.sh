#!/usr/bin/env bash
# Verifies that the release notes for a version are final before tagging.
#
# The GitHub Release body is taken from docs/releases/v<version>-release-notes.md
# as it is at the tagged commit (scripts/create-or-verify-draft-release.sh), and
# the tag is immutable. Notes that still say "DRAFT" or "unreleased" at the tag
# would be published as such, so the release workflow runs this check in every
# run, dry runs included.
#
# Rules:
# - <version> is X.Y.Z or a release candidate X.Y.Z-rc.N (N >= 1, no leading
#   zero), the forms release.yml accepts; a candidate has its own notes file,
#   docs/releases/vX.Y.Z-rc.N-release-notes.md, because its GitHub Release
#   body is taken from that file at the candidate's tag;
# - the file docs/releases/v<version>-release-notes.md exists;
# - its status paragraph (the paragraph starting with "**Status:" within the
#   first 10 lines) starts with "**Status: released";
# - that paragraph does not contain "draft" or "unreleased" (any case).
#
# Usage: scripts/check-release-notes.sh <version> [repo-root]
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "Usage: $0 <version> [repo-root]" >&2
    exit 2
fi

VERSION="${1#v}"
ROOT="${2:-.}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[1-9][0-9]*)?$ ]]; then
    echo "ERROR: invalid version '$1': expected X.Y.Z or X.Y.Z-rc.N." >&2
    exit 2
fi
NOTES="$ROOT/docs/releases/v${VERSION}-release-notes.md"

if [[ ! -f "$NOTES" ]]; then
    echo "ERROR: release notes $NOTES not found." >&2
    exit 1
fi

# The status paragraph: from the first "**Status:" line in the first 10 lines
# up to the next blank line.
STATUS="$(head -n 10 "$NOTES" | awk '
    /^\*\*Status:/ { found = 1 }
    found && /^[[:space:]]*$/ { exit }
    found { print }
')"

if [[ -z "$STATUS" ]]; then
    echo "ERROR: $NOTES has no '**Status:' line in its first 10 lines." >&2
    exit 1
fi

if ! grep -qiE '^\*\*Status: released' <<<"$STATUS"; then
    echo "ERROR: $NOTES status does not start with '**Status: released':" >&2
    echo "$STATUS" >&2
    exit 1
fi

if grep -qiE 'draft|unreleased' <<<"$STATUS"; then
    echo "ERROR: $NOTES status still says draft or unreleased:" >&2
    echo "$STATUS" >&2
    exit 1
fi

echo "Release notes $NOTES are final: $(head -n 1 <<<"$STATUS")"
