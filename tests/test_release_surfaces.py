#!/usr/bin/env python3
"""Tests for scripts/check-release-surfaces.py on small synthetic repositories.

The fixtures use the repository's own manifest
(docs/releases/release-surfaces.toml), so a pattern that stops matching the
real wording is caught here as well as by the repository test.
"""

import importlib.util
import io
import json
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location(
    "check_release_surfaces", ROOT / "scripts" / "check-release-surfaces.py")
surfaces = importlib.util.module_from_spec(spec)
spec.loader.exec_module(surfaces)

README = """[Performance by version](docs/performance-by-version.md) summarizes, for
users, the comparison of the current release with the previous qualified
release (v1.0.0 against v0.9.0), with its limits.
"""
CONSOLE = "> **Status: released.** Console 1.0.0 was released together with server 1.0.0\n"
INDEX = "---\nlayout: home\ntitle: Knotrel Documentation\nrelease: v1.0.0\n---\nText.\n"
GETTING_STARTED = """1. Download the server for your platform from [release v1.0.0](https://github.com/knotrel/knotrel/releases/tag/v1.0.0) and verify it.
"""
BENCHMARKS = """## Current release

**v1.0.0** was measured against v0.9.0.
"""
PERFORMANCE = """# Performance by version

## v1.0.0 compared with v0.9.0

Results.

## How to read these numbers
"""
HOME = '<a href="{{ page.release }}">{{ page.release }}</a>\n'
DEFAULT = '<a href="https://github.com/knotrel/knotrel/releases">Releases</a>\n'

BENCH_README = """### Latest published release: v1.0.0

Kept at their release tags; the current release's
qualification is [`results/v1.0.0-qualification`](results/v1.0.0-qualification/DECISION.md).

- **Release reports.** 1.0.0 is the
  latest published release; no 1.1.0 release report exists before its tag.
"""
SITE = "The site contains the published 1.0.0 report. 1.0.0\nremains the latest published release until the 1.1.0 tag.\n"
RELEASES_JSON = {"schema_version": 2, "releases": [{"version": "1.0.0", "status": "published"}]}

FILES = {
    "README.md": README,
    "console/README.md": CONSOLE,
    "docs/index.md": INDEX,
    "docs/getting-started.md": GETTING_STARTED,
    "docs/benchmarks.md": BENCHMARKS,
    "docs/performance-by-version.md": PERFORMANCE,
    "docs/_layouts/home.html": HOME,
    "docs/_layouts/default.html": DEFAULT,
}
BENCH_FILES = {
    "README.md": BENCH_README,
    "docs/SITE.md": SITE,
    "docs/data/releases.json": json.dumps(RELEASES_JSON),
}
NOTES = {
    "0.8.0": "released 2026-11-21. No performance qualification",
    "0.9.0": "released 2026-11-22",
    "1.0.0-rc.1": "never released; superseded by v1.0.0",
    "1.0.0": "released 2026-11-24",
}


def write(base, files):
    for name, text in files.items():
        path = base / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


def repo(changes=None, notes=None, bench_changes=None, manifest=None):
    """(knotrel root, knotrel-benchmarks root) with optional file overrides."""
    d = Path(tempfile.mkdtemp())
    root, bench = d / "knotrel", d / "knotrel-benchmarks"
    write(root, {**FILES, **(changes or {})})
    write(bench, {**BENCH_FILES, **(bench_changes or {})})
    (root / "docs" / "releases").mkdir(parents=True, exist_ok=True)
    if manifest is None:
        shutil.copy(ROOT / surfaces.MANIFEST, root / surfaces.MANIFEST)
    else:
        (root / surfaces.MANIFEST).write_text(manifest, encoding="utf-8")
    for version, status in (NOTES if notes is None else notes).items():
        (root / "docs" / "releases" / f"v{version}-release-notes.md").write_text(
            f"# v{version}\n\n**Status: {status}.**\n", encoding="utf-8")
    return root, bench


def bump(text, old="1.0.0", new="1.1.0"):
    return text.replace(old, new)


class ReleaseSurfaces(unittest.TestCase):
    def assertError(self, errors, needle):
        self.assertTrue(any(needle in e for e in errors), errors)

    def test_consistent_surfaces_pass(self):
        root, bench = repo()
        self.assertEqual(surfaces.check(root, benchmarks=bench), [])

    def test_latest_release_and_qualification(self):
        root, _ = repo()
        manifest = surfaces.tomllib.loads((root / surfaces.MANIFEST).read_text())
        self.assertEqual(surfaces.released_versions(root), ["0.8.0", "0.9.0", "1.0.0"])
        self.assertEqual(surfaces.expected_versions(root, manifest), ("1.0.0", "1.0.0"))
        # A newer release that was not qualified keeps the performance pages
        # on the last qualified release only when the maintainer says so.
        root, _ = repo(notes={"0.7.0": "released", "0.8.0": "released"})
        self.assertEqual(surfaces.expected_versions(root, manifest), ("0.8.0", "0.7.0"))

    def test_deliberately_stale_fixture_fails(self):
        """The pre-1.0.0 wording found after the 1.0.0 release (knotrel #174)."""
        stale_benchmarks = """<p class="actions-note">The performance site publishes its first release-to-release report with 1.0.0; until then it describes the plan.</p>

## Current release

**v0.9.0** was measured against v0.7.0 on its release candidate.

- [Performance site](https://knotrel.github.io/knotrel-benchmarks/): No report is published yet.
"""
        root, bench = repo({
            "README.md": README.replace("(v1.0.0 against v0.9.0)", "(v0.7.0 candidate against v0.6.0)"),
            "docs/benchmarks.md": stale_benchmarks,
            "docs/performance-by-version.md": PERFORMANCE.replace(
                "## v1.0.0 compared with v0.9.0", "## v0.9.0 compared with v0.7.0"),
        }, bench_changes={
            "README.md": BENCH_README.replace("release: v1.0.0", "release: v0.2.0")
                                     .replace("results/v1.0.0-qualification`", "results/v0.9.0-qualification`"),
        })
        errors = surfaces.check(root, benchmarks=bench)
        self.assertError(errors, "README.md: Performance by version summary names v0.7.0, expected v1.0.0")
        self.assertError(errors, "docs/benchmarks.md: 'Current release' section names v0.9.0")
        self.assertError(errors, "docs/benchmarks.md:1: stale phrase 'describes the plan'")
        self.assertError(errors, "stale phrase 'No report is published yet'")
        self.assertError(errors, "docs/performance-by-version.md: first comparison heading names v0.9.0")
        self.assertError(errors, "knotrel-benchmarks/README.md: 'Latest published release' heading names v0.2.0")
        self.assertError(errors, "knotrel-benchmarks/README.md: current release's qualification names v0.9.0")

    def test_older_release_on_release_surfaces(self):
        root, bench = repo({
            "docs/index.md": INDEX.replace("v1.0.0", "v0.9.0"),
            "docs/getting-started.md": GETTING_STARTED.replace("tag/v1.0.0", "tag/v0.9.0"),
            "docs/_layouts/default.html": '<span>v0.9.0</span>\n',
            "docs/_layouts/home.html": "<span>release</span>\n",
            "console/README.md": CONSOLE.replace("Console 1.0.0", "Console 0.9.0"),
        })
        errors = surfaces.check(root, benchmarks=bench)
        self.assertError(errors, "docs/index.md: front matter release names v0.9.0, expected v1.0.0")
        self.assertError(errors, "docs/getting-started.md: download step names v0.9.0")
        self.assertError(errors, "docs/getting-started.md: release link names v0.9.0")
        self.assertError(errors, "docs/_layouts/default.html: hard-coded release version names v0.9.0")
        self.assertError(errors, "docs/_layouts/home.html: does not contain 'page.release'")
        self.assertError(errors, "console/README.md: console status line names v0.9.0")

    def test_unreleased_version_on_a_surface(self):
        """Naming a version whose notes are not released yet fails too."""
        root, bench = repo({"docs/index.md": bump(INDEX)})
        self.assertError(surfaces.check(root, benchmarks=bench),
                         "docs/index.md: front matter release names v1.1.0, expected v1.0.0")

    def test_missing_pattern_and_file(self):
        root, bench = repo({"docs/benchmarks.md": "## Releases\n"})
        (root / "docs" / "getting-started.md").unlink()
        errors = surfaces.check(root, benchmarks=bench)
        self.assertError(errors, "docs/benchmarks.md: 'Current release' section not found")
        self.assertError(errors, "docs/getting-started.md: listed in")

    def test_cross_repo_surfaces_need_the_sibling(self):
        root, bench = repo(bench_changes={
            "docs/SITE.md": SITE.replace("1.0.0\nremains", "0.9.0\nremains"),
            "docs/data/releases.json": json.dumps({"releases": [
                {"version": "0.9.0", "status": "published"},
                {"version": "1.0.0", "status": "planned"}]}),
        })
        self.assertEqual(surfaces.check(root, benchmarks=None), [])
        errors = surfaces.check(root, benchmarks=bench)
        self.assertError(errors, "knotrel-benchmarks/docs/SITE.md: latest published release names v0.9.0")
        self.assertError(errors, "knotrel-benchmarks/docs/data/releases.json: newest published release entry names v0.9.0")

    def test_release_mode(self):
        root, bench = repo({
            "README.md": README + "- `cluster-forest` (from 1.1, unreleased)\n",
            "docs/benchmarks.md": BENCHMARKS + "\n## Choosing an engine (1.1.0 candidate)\n",
        })
        errors = surfaces.check(root, release="1.1.0", benchmarks=bench)
        self.assertError(errors, "docs/index.md: front matter release names v1.0.0, expected v1.1.0")
        self.assertError(errors, "docs/performance-by-version.md: first comparison heading names v1.0.0, expected v1.1.0")
        self.assertError(errors, "README.md:4: still labels v1.1.0 unreleased")
        self.assertError(errors, "docs/benchmarks.md:5: still labels v1.1.0 unreleased or a candidate")

        good = {name: bump(text) for name, text in FILES.items()}
        good["docs/performance-by-version.md"] = PERFORMANCE.replace(
            "## v1.0.0 compared with v0.9.0", "## v1.1.0 compared with v1.0.0")
        good["README.md"] = README.replace("(v1.0.0 against", "(v1.1.0 against")
        bench_good = {"README.md": bump(BENCH_README).replace("no 1.1.0", "no 1.2.0"),
                      "docs/SITE.md": bump(SITE).replace("the 1.1.0 tag", "the 1.2.0 tag"),
                      "docs/data/releases.json": bump(json.dumps(RELEASES_JSON))}
        root, bench = repo(good, bench_changes=bench_good)
        self.assertEqual(surfaces.check(root, release="1.1.0", benchmarks=bench), [])

    def test_release_mode_not_qualified(self):
        """A release the maintainer declared not qualified keeps the older evidence."""
        manifest = (ROOT / surfaces.MANIFEST).read_text().replace('"0.8.0", #', '"0.8.0", "1.1.0", #')
        good = {name: bump(text) for name, text in FILES.items()
                if name not in ("README.md", "docs/benchmarks.md", "docs/performance-by-version.md")}
        root, bench = repo(good, manifest=manifest)
        self.assertEqual(surfaces.check(root, release="1.1.0", benchmarks=None), [])

    def test_release_candidate_mode(self):
        """--release X.Y.Z-rc.N: surfaces stay on the latest stable release, labels allowed."""
        root, bench = repo({"README.md": README + "- feature (1.1.0 candidate, unreleased)\n"})
        self.assertEqual(surfaces.check(root, release="1.1.0-rc.1", benchmarks=bench), [])

    def test_unreleased_labels(self):
        text = "\n".join([
            "**1.1.0 candidate, unreleased**",      # 1
            "from 1.1, unreleased",                 # 2
            "The unreleased v1.1.0 adds",           # 3
            "## Engines in the 1.1 candidate",      # 4
            "measured on 1.1.0-rc.1, a candidate",  # 5: an rc is not the release
            "unreleased 1.10.0 work",               # 6: another version
            "1.1.0 was released",                   # 7
            "unreleased features",                  # 8: no version
        ])
        self.assertEqual([n for n, _ in surfaces.unreleased_labels(text, "1.1.0")], [1, 2, 3, 4])
        # Another version's label on the same line is not a label of 1.0.0.
        self.assertEqual(surfaces.unreleased_labels(
            "| **1.1.0 candidate, unreleased** (not in v1.0.0: build from `main`) |", "1.0.0"), [])
        self.assertEqual(surfaces.unreleased_labels("unreleased 1.1 work", "1.1.2"), [])
        self.assertEqual(len(surfaces.unreleased_labels("unreleased 1.1.2 work", "1.1.2")), 1)

    def test_arguments(self):
        for bad in (["--release", "1.0"], ["--release"], ["--benchmarks"], ["a", "b"]):
            with redirect_stderr(io.StringIO()):
                self.assertEqual(surfaces.run(bad), 2, bad)
        self.assertEqual(surfaces.parse_args(["--release", "v1.1.0", "--no-cross-repo", "x"]),
                         ("1.1.0", None, False, Path("x")))

    def test_run_reports(self):
        root, bench = repo({"docs/index.md": INDEX.replace("v1.0.0", "v0.9.0")})
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            self.assertEqual(surfaces.run(["--benchmarks", str(bench), str(root)]), 1)
        self.assertIn("front matter release names v0.9.0", err.getvalue())
        with redirect_stdout(out), redirect_stderr(err):
            self.assertEqual(surfaces.run(["--no-cross-repo", str(ROOT)]), 0)
        self.assertIn("knotrel-benchmarks not checked", out.getvalue())

    def test_repository_surfaces(self):
        """This repository's own surfaces (the sibling is checked by the script, not here)."""
        self.assertEqual(surfaces.check(ROOT, benchmarks=None), [])


if __name__ == "__main__":
    unittest.main()
