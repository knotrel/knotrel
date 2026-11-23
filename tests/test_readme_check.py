#!/usr/bin/env python3
"""Tests for scripts/check-readme.py on small synthetic repositories."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("check_readme", ROOT / "scripts" / "check-readme.py")
check_readme = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_readme)

README = """**Latest release: [v0.8.0](https://github.com/knotrel/knotrel/releases/tag/v0.8.0)**

Download and extract the **v0.8.0** server from the
[release assets](https://github.com/knotrel/knotrel/releases/tag/v0.8.0).

```toml
knotrel-core = "0.8"
```

tar -xzf knotrel-server-v0.8.0-x86_64-unknown-linux-gnu.tar.gz
- feeder (unreleased, roadmap 0.9.1)
"""
CONSOLE = """> **Status: released.** Console 0.8.0 was released together with server 0.8.0

docker pull ghcr.io/knotrel/knotrel-console:0.8.0
"""


def repo(readme=README, console=CONSOLE, workspace="0.9.0", notes=None):
    d = Path(tempfile.mkdtemp())
    (d / "console").mkdir()
    (d / "docs" / "releases").mkdir(parents=True)
    (d / "README.md").write_text(readme)
    (d / "console" / "README.md").write_text(console)
    (d / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{workspace}"\n')
    for version, status in (notes or {"0.8.0": "released 2026-11-21"}).items():
        (d / "docs" / "releases" / f"v{version}-release-notes.md").write_text(
            f"# Notes\n\n**Status: {status}.**\n")
    return d


class ReadmeCheck(unittest.TestCase):
    def test_consistent_readmes_pass(self):
        self.assertEqual(check_readme.check(repo()), [])

    def test_stale_dependency_snippet(self):
        errors = check_readme.check(repo(README.replace('"0.8"', '"0.6.0"')))
        self.assertTrue(any("dependency snippet names '0.6.0'" in e for e in errors), errors)

    def test_stale_asset_name_and_console(self):
        errors = check_readme.check(repo(README.replace("server-v0.8.0", "server-v0.7.0"),
                                         CONSOLE.replace("console:0.8.0", "console:0.7.0")))
        self.assertTrue(any("release asset name names '0.7.0'" in e for e in errors), errors)
        self.assertTrue(any("console image tag names '0.7.0'" in e for e in errors), errors)

    def test_latest_release_must_be_released(self):
        errors = check_readme.check(repo(notes={"0.8.0": "DRAFT, not released"}))
        self.assertTrue(any("is not released" in e for e in errors), errors)

    def test_release_mode(self):
        errors = check_readme.check(repo(), release="0.9.0")
        self.assertTrue(any("must describe v0.9.0" in e for e in errors), errors)
        self.assertTrue(any("still says 'unreleased'" in e for e in errors), errors)

        bumped = README.replace("0.8.0", "0.9.0").replace('"0.8"', '"0.9"').replace(
            "- feeder (unreleased, roadmap 0.9.1)\n", "")
        good = repo(bumped, CONSOLE.replace("0.8.0", "0.9.0"),
                    notes={"0.9.0": "released 2026-11-22"})
        self.assertEqual(check_readme.check(good, release="0.9.0"), [])

    def test_prerelease_workspace(self):
        """A release-candidate workspace keeps the READMEs on the latest stable release."""
        self.assertEqual(check_readme.check(repo(workspace="0.9.0-rc.1")), [])
        errors = check_readme.check(repo(workspace="0.8.0-rc.1"))
        self.assertTrue(any("newer than the workspace 0.8.0-rc.1" in e for e in errors), errors)

    def test_prerelease_release_mode(self):
        """--release X.Y.Z-rc.N: README on an older stable release, 'unreleased' allowed."""
        self.assertEqual(check_readme.check(repo(workspace="0.9.0-rc.1"), release="0.9.0-rc.1"), [])
        # The workspace must be the candidate being released.
        errors = check_readme.check(repo(workspace="0.9.0-rc.2"), release="0.9.0-rc.1")
        self.assertTrue(any("is not the release candidate v0.9.0-rc.1" in e for e in errors), errors)
        # A candidate of the release the README already names is not newer.
        errors = check_readme.check(repo(workspace="0.8.0-rc.1"), release="0.8.0-rc.1")
        self.assertTrue(any("is not older than the release candidate" in e for e in errors), errors)
        # The stable release the README names must still be released.
        errors = check_readme.check(repo(workspace="0.9.0-rc.1", notes={"0.8.0": "DRAFT"}),
                                    release="0.9.0-rc.1")
        self.assertTrue(any("is not released" in e for e in errors), errors)

    def test_readme_never_names_a_candidate(self):
        rc = README.replace("v0.8.0", "v0.9.0-rc.1")
        errors = check_readme.check(repo(rc, workspace="0.9.0-rc.1",
                                         notes={"0.9.0-rc.1": "released 2026-11-24"}))
        self.assertTrue(any("names the release candidate v0.9.0-rc.1" in e for e in errors), errors)
        # An rc asset name is not read as its stable version.
        errors = check_readme.check(repo(README.replace("server-v0.8.0", "server-v0.8.0-rc.1")))
        self.assertTrue(any("release asset name names '0.8.0-rc.1'" in e for e in errors), errors)

    def test_version_order(self):
        order = ["0.9.0", "1.0.0-rc.1", "1.0.0-rc.2", "1.0.0-rc.10", "1.0.0", "1.0.1"]
        self.assertEqual(sorted(reversed(order), key=check_readme.key), order)
        for bad in ("1.0.0-alpha", "1.0.0-rc.01", "1.0", "1.0.0-rc.0"):
            with self.assertRaises(ValueError):
                check_readme.key(bad)

    def test_release_argument_format(self):
        for bad in ("1.0.0-alpha", "1.0.0-rc.01", "1.0", "1.0.0-rc.0", "1.0.0-rc.1x"):
            self.assertEqual(check_readme.main(["check-readme.py", "--release", bad, str(ROOT)]), 2, bad)

    def test_repository_readmes(self):
        self.assertEqual(check_readme.check(ROOT), [])


if __name__ == "__main__":
    unittest.main()
