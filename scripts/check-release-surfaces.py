#!/usr/bin/env python3
"""Checks that every user-facing release surface names the current release.

The surfaces, and the patterns that find the version each one names, are
listed in docs/releases/release-surfaces.toml. Each surface has a kind:

- "release": names the latest released version, the newest
  docs/releases/vX.Y.Z-release-notes.md (stable X.Y.Z) whose status line
  starts with "**Status: released";
- "performance": names the latest performance-qualified release, the latest
  released version not listed in [performance] not_qualified.

A surface naming an older (or a newer, unreleased) version fails. Stale
phrases listed in [stale] fail on every surface.

With --release VERSION (release workflow, dry runs included):
- for a stable VERSION, release surfaces must name VERSION, performance
  surfaces VERSION too unless it is listed as not qualified, and no surface
  may still carry an "unreleased" or "X.Y(.Z) candidate" label naming
  VERSION;
- for a release candidate (X.Y.Z-rc.N), the surfaces keep describing the
  latest stable release, as without --release.

Surfaces in the knotrel-benchmarks repository are checked only when its
checkout exists: --benchmarks PATH, or ../knotrel-benchmarks next to
REPO_ROOT. --no-cross-repo skips them.

Usage: check-release-surfaces.py [--release VERSION] [--benchmarks PATH]
                                 [--no-cross-repo] [REPO_ROOT]
"""

import json
import re
import sys
import tomllib
from pathlib import Path

VERSION = r"(\d+\.\d+\.\d+(?:-rc\.[1-9]\d*)?)"
MANIFEST = Path("docs") / "releases" / "release-surfaces.toml"
SIBLING = "knotrel-benchmarks"


def key(version):
    """SemVer precedence key: a release candidate sorts before its release."""
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-rc\.([1-9]\d*))?", version)
    if not m:
        raise ValueError(f"not a release version: {version!r}")
    major, minor, patch, rc = m.groups()
    return (int(major), int(minor), int(patch), 0 if rc else 1, int(rc or 0))


def is_prerelease(version):
    return "-" in version


def released_versions(root):
    """Stable versions whose release notes say "**Status: released"."""
    versions = []
    for notes in (root / "docs" / "releases").glob("v*-release-notes.md"):
        m = re.fullmatch(r"v" + VERSION + r"-release-notes\.md", notes.name)
        if not m or is_prerelease(m.group(1)):
            continue
        if re.search(r"^\*\*Status: released", notes.read_text(encoding="utf-8"), re.M):
            versions.append(m.group(1))
    return sorted(versions, key=key)


def expected_versions(root, manifest, release=None):
    """(release version, performance version) every surface must name."""
    released = released_versions(root)
    if release is not None and not is_prerelease(release):
        candidates = sorted(set(released) | {release}, key=key)
        candidates = [v for v in candidates if key(v) <= key(release)]
        latest = release
    else:
        candidates = released
        latest = released[-1] if released else None
    skipped = set(manifest.get("performance", {}).get("not_qualified", []))
    qualified = [v for v in candidates if v not in skipped]
    return latest, (qualified[-1] if qualified else None)


def label_pattern(version):
    """Regex for a version X.Y.Z written as X.Y.Z, vX.Y.Z or X.Y (with X.Y.0)."""
    major, minor, patch = version.split(".")
    short = rf"{major}\.{minor}(?:\.{patch})?" if patch == "0" else rf"{major}\.{minor}\.{patch}"
    return rf"(?<![\w.])v?{short}(?![\w-]|\.\d)"


def unreleased_labels(text, version):
    """(line number, line) still calling VERSION unreleased or a candidate.

    Only labels attached to the version count ("1.1.0 candidate",
    "from 1.1, unreleased", "the unreleased 1.1.0"), so a line that merely
    mentions the version next to another version's label ("1.1.0 candidate,
    unreleased (not in v1.0.0)") does not fail the 1.0.0 release.
    """
    ver = label_pattern(version)
    label = re.compile(rf"{ver}\s+candidate\b|\bunreleased\s+{ver}|{ver},?\s+unreleased\b", re.I)
    return [(n, line.strip()) for n, line in enumerate(text.splitlines(), 1) if label.search(line)]


def rule_versions(text, rule):
    """Versions a rule finds, or None when a required pattern is missing."""
    if rule.get("releases_json"):
        data = json.loads(text)
        published = [r["version"] for r in data.get("releases", []) if r.get("status") == "published"]
        if not published:
            return None if rule.get("required", True) else []
        return [max(published, key=key)]
    matches = list(re.finditer(rule["pattern"], text, re.M))
    if not matches:
        return None if rule.get("required", True) else []
    if rule.get("match", "first") == "first":
        matches = matches[:1]
    return [g for m in matches for g in m.groups() if g is not None]


def check(root, release=None, benchmarks=None, manifest_path=None):
    """Errors as strings; benchmarks is the sibling checkout or None to skip it."""
    root = Path(root)
    manifest_path = manifest_path or root / MANIFEST
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    latest, qualified = expected_versions(root, manifest, release)
    if latest is None:
        return ["docs/releases: no release notes with '**Status: released'"]
    stale = manifest.get("stale", {}).get("phrases", [])
    errors = []

    def add(error):
        if error not in errors:
            errors.append(error)
    for surface in manifest.get("surface", []):
        repo = surface.get("repo", "knotrel")
        if repo == "knotrel":
            base = root
        elif repo == SIBLING:
            if benchmarks is None:
                continue
            base = Path(benchmarks)
        else:
            errors.append(f"{manifest_path.name}: unknown repo {repo!r}")
            continue
        name = surface["path"] if repo == "knotrel" else f"{repo}/{surface['path']}"
        path = base / surface["path"]
        if not path.is_file():
            errors.append(f"{name}: listed in {MANIFEST} but missing")
            continue
        text = path.read_text(encoding="utf-8")
        kind = surface["kind"]
        expected = latest if kind == "release" else qualified
        what = "latest release" if kind == "release" else "latest performance-qualified release"
        for rule in surface.get("rules", []):
            found = rule_versions(text, rule)
            describe = rule.get("describe", rule.get("pattern", "rule"))
            if found is None:
                errors.append(f"{name}: {describe} not found (update the surface or its "
                              f"pattern in {MANIFEST})")
                continue
            for version in found:
                if version != expected:
                    add(f"{name}: {describe} names v{version}, expected v{expected} "
                                  f"(the {what})")
        for needle in surface.get("must_contain", []):
            if needle not in text:
                errors.append(f"{name}: does not contain {needle!r}")
        for phrase in stale:
            for n, line in enumerate(text.splitlines(), 1):
                if phrase.lower() in line.lower():
                    errors.append(f"{name}:{n}: stale phrase {phrase!r}: {line.strip()[:100]}")
        if release is not None and not is_prerelease(release):
            for n, line in unreleased_labels(text, release):
                errors.append(f"{name}:{n}: still labels v{release} unreleased or a candidate: "
                              f"{line[:100]}")
    return errors


def default_benchmarks(root):
    sibling = Path(root).resolve().parent / SIBLING
    return sibling if sibling.is_dir() else None


def parse_args(args):
    """(release, benchmarks, cross_repo, root) or None on a usage error."""
    args = list(args)
    release = benchmarks = None
    cross_repo = True
    if "--no-cross-repo" in args:
        args.remove("--no-cross-repo")
        cross_repo = False
    for flag in ("--release", "--benchmarks"):
        if flag in args:
            i = args.index(flag)
            if i + 1 >= len(args):
                return None
            value = args[i + 1]
            del args[i : i + 2]
            if flag == "--release":
                release = value.removeprefix("v")
                if not re.fullmatch(VERSION, release):
                    return None
            else:
                benchmarks = Path(value)
    if len(args) > 1:
        return None
    return release, benchmarks, cross_repo, Path(args[0] if args else ".")


def run(args):
    """Runs the check; returns the exit code. Used by check-readme.py too."""
    parsed = parse_args(args)
    if parsed is None:
        print(__doc__, file=sys.stderr)
        return 2
    release, benchmarks, cross_repo, root = parsed
    if not cross_repo:
        benchmarks = None
    elif benchmarks is None:
        benchmarks = default_benchmarks(root)
    errors = check(root, release, benchmarks)
    for e in errors:
        print(f"ERROR: {e}", file=sys.stderr)
    scope = f" and {benchmarks}" if benchmarks else f" ({SIBLING} not checked)"
    if errors:
        print(f"Update the release surfaces listed in {MANIFEST} "
              "(docs/releases/release-procedure.md, section 5; the release-docs skill "
              "proposes the edits).", file=sys.stderr)
        return 1
    print("Release surfaces consistent" + (f" for release v{release}" if release else "") + scope)
    return 0


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
