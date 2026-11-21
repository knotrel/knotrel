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

    def test_repository_readmes(self):
        self.assertEqual(check_readme.check(ROOT), [])


if __name__ == "__main__":
    unittest.main()
