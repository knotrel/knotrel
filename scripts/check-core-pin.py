#!/usr/bin/env python3
"""Check the knotrel-server requirement on knotrel-core against the release version.

Policy (CONTRIBUTING.md, release procedure section 4.1):

- for a release candidate ``X.Y.Z-rc.N`` the requirement is exact, ``=X.Y.Z-rc.N``:
  the caret form would also accept a later candidate or the final release, so a
  published server candidate could resolve a different core;
- for a stable release ``X.Y.Z`` it is the caret form ``^X.Y.Z`` (written
  ``version = "X.Y.Z"`` in Cargo.toml).

The requirement is read from ``cargo metadata`` (the resolved manifest that
``cargo publish`` packages), not from Cargo.toml text, so a workspace dependency
and a crate-level override are both covered. Both packages must carry the release
version.

    check-core-pin.py [VERSION]   # VERSION defaults to the knotrel-core version
"""

from __future__ import annotations

import json
import re
import subprocess
import sys

VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$")


class PinError(Exception):
    """The requirement does not match the policy for this version."""


def expected_requirement(version: str) -> str:
    """The requirement cargo metadata must report for VERSION."""
    if not VERSION.match(version):
        raise PinError(f"invalid release version {version!r}: must be X.Y.Z or X.Y.Z-rc.N")
    return f"={version}" if "-rc." in version else f"^{version}"


def check(metadata: dict, version: str | None = None) -> str:
    """Return a confirmation line, or raise PinError."""
    packages = {p["name"]: p for p in metadata.get("packages", [])}
    for name in ("knotrel-core", "knotrel-server"):
        if name not in packages:
            raise PinError(f"package {name} not in cargo metadata")
    core, server = packages["knotrel-core"], packages["knotrel-server"]
    version = version or core["version"]
    expected = expected_requirement(version)
    for p in (core, server):
        if p["version"] != version:
            raise PinError(f"{p['name']} is {p['version']}, the release version is {version}")
    deps = [d for d in server.get("dependencies", []) if d["name"] == "knotrel-core" and d.get("kind") in (None, "normal")]
    if len(deps) != 1:
        raise PinError(f"knotrel-server has {len(deps)} normal dependencies on knotrel-core, expected 1")
    req = deps[0]["req"]
    if req != expected:
        form = "exact" if expected.startswith("=") else "caret"
        raise PinError(f"knotrel-server requires knotrel-core {req!r}; {version} needs the {form} requirement "
                       f"{expected!r} (CONTRIBUTING.md, release candidates)")
    return f"knotrel-server requires knotrel-core {req} for release {version}"


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        print(__doc__, file=sys.stderr)
        return 2
    raw = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
                         check=True, capture_output=True, text=True).stdout
    try:
        print(check(json.loads(raw), argv[1] if len(argv) == 2 else None))
    except PinError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
