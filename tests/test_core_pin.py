#!/usr/bin/env python3
"""Tests for scripts/check-core-pin.py on synthetic cargo metadata."""

import importlib.util
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("check_core_pin", ROOT / "scripts" / "check-core-pin.py")
pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pin)


def metadata(version, req, server_version=None, extra_deps=()):
    return {"packages": [
        {"name": "knotrel-core", "version": version, "dependencies": []},
        {"name": "knotrel-server", "version": server_version or version,
         "dependencies": [{"name": "knotrel-core", "req": req, "kind": None}, *extra_deps]},
    ]}


class CorePinTest(unittest.TestCase):
    def test_rc_requires_exact(self):
        self.assertIn("=1.0.0-rc.1", pin.check(metadata("1.0.0-rc.1", "=1.0.0-rc.1")))
        for req in ("^1.0.0-rc.1", "=1.0.0-rc.2", "=1.0.0", ">=1.0.0-rc.1"):
            with self.subTest(req), self.assertRaisesRegex(pin.PinError, "exact"):
                pin.check(metadata("1.0.0-rc.1", req))

    def test_stable_requires_caret(self):
        self.assertIn("^1.0.0", pin.check(metadata("1.0.0", "^1.0.0")))
        for req in ("=1.0.0", "^1.0.0-rc.1", "^0.9.0", "~1.0.0"):
            with self.subTest(req), self.assertRaisesRegex(pin.PinError, "caret"):
                pin.check(metadata("1.0.0", req))

    def test_explicit_version_must_match_packages(self):
        with self.assertRaisesRegex(pin.PinError, "release version"):
            pin.check(metadata("1.0.0-rc.1", "=1.0.0-rc.1"), "1.0.0")
        with self.assertRaisesRegex(pin.PinError, "knotrel-server is 1.0.0"):
            pin.check(metadata("1.0.0-rc.1", "=1.0.0-rc.1", server_version="1.0.0"))

    def test_invalid_versions_rejected(self):
        for version in ("1.0.0-rc.0", "1.0.0-rc.01", "1.0.0-alpha", "1.0", "01.0.0", "1.0.0+b"):
            with self.subTest(version), self.assertRaisesRegex(pin.PinError, "invalid release version"):
                pin.expected_requirement(version)

    def test_dependency_count(self):
        with self.assertRaisesRegex(pin.PinError, "2 normal dependencies"):
            pin.check(metadata("1.0.0", "^1.0.0", extra_deps=[{"name": "knotrel-core", "req": "^1.0.0", "kind": None}]))
        dev = {"name": "knotrel-core", "req": "*", "kind": "dev"}
        self.assertIn("^1.0.0", pin.check(metadata("1.0.0", "^1.0.0", extra_deps=[dev])))
        with self.assertRaisesRegex(pin.PinError, "not in cargo metadata"):
            pin.check({"packages": [{"name": "knotrel-core", "version": "1.0.0"}]})

    def test_workspace_manifest_passes(self):
        """The repository's own manifest satisfies the policy for its version."""
        result = subprocess.run(["python3", str(ROOT / "scripts" / "check-core-pin.py")], cwd=ROOT,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
