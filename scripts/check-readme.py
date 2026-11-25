#!/usr/bin/env python3
"""Checks that the user-facing READMEs point at one, real, released version.

Every reference to "the current release" in README.md and console/README.md
must name the same version X:

- README.md: the "Latest release" line (text and link), the download links and
  release asset names, and the `knotrel-core` dependency snippet (which must be
  "MAJOR.MINOR" of X);
- console/README.md: the "Status: released" line and the console image tags.

X must be released: docs/releases/vX-release-notes.md exists and its status is
"released", X is a stable version (never a release candidate such as
1.0.0-rc.1) and X is not newer than the workspace version, which may itself
be a release candidate.

With --release VERSION (release workflow, dry runs included):
- for a stable VERSION, X must also be VERSION and neither README may still
  call anything "unreleased": the READMEs of the tagged commit describe the
  release being published;
- for a release candidate (X.Y.Z-rc.N), the READMEs keep describing the
  latest stable release, which must be older than VERSION; "unreleased"
  labels stay, because the candidate's features are not in a stable release
  yet. The candidate is announced by its own release notes and GitHub
  prerelease.

After the READMEs, it runs scripts/check-release-surfaces.py with the same
arguments: the other user-facing pages listed in
docs/releases/release-surfaces.toml (documentation site, getting started,
benchmarks and performance pages, and the knotrel-benchmarks surfaces when
that checkout exists next to this one).

Usage: check-readme.py [--release VERSION] [--benchmarks PATH] [--no-cross-repo]
                       [REPO_ROOT]
"""

import importlib.util
import re
import sys
from pathlib import Path

# A release version: X.Y.Z, optionally a release candidate X.Y.Z-rc.N (the only
# prerelease form the release workflows accept). Matching the suffix keeps a
# candidate from being read as its stable version (v1.0.0-rc.1 is not v1.0.0).
VERSION = r"(\d+\.\d+\.\d+(?:-rc\.[1-9]\d*)?)"


def workspace_version(root):
    text = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1]
    return re.search(r'^version = "' + VERSION + '"', section, re.M).group(1)


def key(version):
    """SemVer precedence key: a release candidate sorts before its release."""
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-rc\.([1-9]\d*))?", version)
    if not m:
        raise ValueError(f"not a release version: {version!r}")
    major, minor, patch, rc = m.groups()
    return (int(major), int(minor), int(patch), 0 if rc else 1, int(rc or 0))


def is_prerelease(version):
    return "-" in version


def readme_references(text):
    """(description, version) pairs that must all name the current release."""
    refs = []
    for m in re.finditer(r"\*\*Latest release: \[v" + VERSION + r"\]\(([^)]*)\)", text):
        refs.append(("Latest release line", m.group(1)))
        tag = re.search(r"/releases/tag/v" + VERSION, m.group(2))
        refs.append(("Latest release link", tag.group(1) if tag else "?"))
    if not any(d == "Latest release line" for d, _ in refs):
        refs.append(("Latest release line (missing)", "?"))
    for m in re.finditer(r"/releases/tag/v" + VERSION, text):
        refs.append(("release link", m.group(1)))
    for m in re.finditer(r"knotrel-server-v" + VERSION, text):
        refs.append(("release asset name", m.group(1)))
    for m in re.finditer(r"Download and extract the \*\*v" + VERSION + r"\*\*", text):
        refs.append(("download instruction", m.group(1)))
    deps = re.findall(r'^knotrel-core = "([^"]+)"', text, re.M)
    if not deps:
        refs.append(("knotrel-core dependency snippet (missing)", "?"))
    for dep in deps:
        refs.append(("knotrel-core dependency snippet", dep))
    return refs


def console_references(text):
    refs = []
    m = re.search(r"\*\*Status: released\.\*\* Console " + VERSION, text)
    refs.append(("console status line", m.group(1) if m else "?"))
    for m in re.finditer(r"ghcr\.io/knotrel/knotrel-console:" + VERSION, text):
        refs.append(("console image tag", m.group(1)))
    return refs


def check(root, release=None):
    errors = []
    readme = (root / "README.md").read_text(encoding="utf-8")
    console = (root / "console" / "README.md").read_text(encoding="utf-8")
    current = re.search(r"\*\*Latest release: \[v" + VERSION, readme)
    if not current:
        return ["README.md: no '**Latest release: [vX.Y.Z](...)**' line"]
    latest = current.group(1)
    if is_prerelease(latest):
        return [f"README.md: the latest release line names the release candidate v{latest}; "
                "the READMEs describe the latest stable release"]
    minor = ".".join(latest.split(".")[:2])

    for name, refs in (("README.md", readme_references(readme)),
                       ("console/README.md", console_references(console))):
        for what, found in refs:
            expected = minor if what == "knotrel-core dependency snippet" else latest
            if found != expected:
                errors.append(f"{name}: {what} names {found!r}, expected {expected!r} "
                              f"(the latest release, v{latest})")

    notes = root / "docs" / "releases" / f"v{latest}-release-notes.md"
    if not notes.is_file():
        errors.append(f"README.md: latest release v{latest} has no {notes.relative_to(root)}")
    elif (release is None or is_prerelease(release)) and not re.search(r"^\*\*Status: released", notes.read_text(encoding="utf-8"), re.M):
        errors.append(f"README.md: v{latest} is not released ({notes.relative_to(root)} status)")

    workspace = workspace_version(root)
    if key(latest) > key(workspace):
        errors.append(f"README.md: latest release v{latest} is newer than the workspace {workspace}")

    if release is not None and is_prerelease(release):
        if key(latest) >= key(release):
            errors.append(f"README.md: latest release v{latest} is not older than the "
                          f"release candidate v{release}")
        if release != workspace:
            errors.append(f"Cargo.toml: workspace version {workspace} is not the release "
                          f"candidate v{release}")
    elif release is not None:
        if latest != release:
            errors.append(f"README.md: latest release is v{latest}; the README of the "
                          f"v{release} release commit must describe v{release}")
        for name, text in (("README.md", readme), ("console/README.md", console)):
            for n, line in enumerate(text.splitlines(), 1):
                if re.search(r"\bunreleased\b", line, re.I):
                    errors.append(f"{name}:{n}: still says 'unreleased': {line.strip()[:100]}")
    return errors


def release_surfaces(argv):
    """Runs scripts/check-release-surfaces.py with argv; returns its exit code."""
    path = Path(__file__).resolve().parent / "check-release-surfaces.py"
    spec = importlib.util.spec_from_file_location("check_release_surfaces", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.run(argv)


def main(argv):
    surfaces_args = argv[1:]
    args = argv[1:]
    # Options of check-release-surfaces.py only.
    if "--no-cross-repo" in args:
        args.remove("--no-cross-repo")
    if "--benchmarks" in args:
        i = args.index("--benchmarks")
        del args[i : i + 2]
    release = None
    if "--release" in args:
        i = args.index("--release")
        if i + 1 >= len(args):
            print(__doc__, file=sys.stderr)
            return 2
        release = args[i + 1].removeprefix("v")
        del args[i : i + 2]
        if not re.fullmatch(VERSION, release):
            print(f"ERROR: --release {release!r} is not X.Y.Z or X.Y.Z-rc.N", file=sys.stderr)
            return 2
    if len(args) > 1:
        print(__doc__, file=sys.stderr)
        return 2
    root = Path(args[0] if args else ".")
    errors = check(root, release)
    for e in errors:
        print(f"ERROR: {e}", file=sys.stderr)
    if errors:
        print("Update the READMEs (docs/releases/release-procedure.md, section 5).", file=sys.stderr)
    else:
        print("READMEs consistent" + (f" for release v{release}" if release else ""))
    surfaces = release_surfaces(surfaces_args)
    return 1 if errors or surfaces else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
