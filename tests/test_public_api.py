#!/usr/bin/env python3
"""Tests for scripts/public-api.py: rendering of rustdoc JSON, the snapshot
comparison, the classification rules and the stability.md cross-check.

The fixtures are small hand-written rustdoc JSON documents (format 61), so the
tests need neither cargo nor a toolchain; CI runs the real check separately.
"""

import importlib.util
import os
import re
import sys
import unittest

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SCRIPT = os.path.join(ROOT, "scripts", "public-api.py")
spec = importlib.util.spec_from_file_location("public_api", SCRIPT)
api = importlib.util.module_from_spec(spec)
sys.modules["public_api"] = api
spec.loader.exec_module(api)


def item(id_, name, inner, visibility="public", attrs=()):
    return {
        "id": id_,
        "crate_id": 0,
        "name": name,
        "visibility": visibility,
        "attrs": list(attrs),
        "deprecation": None,
        "inner": inner,
    }


def path(id_, name, args=None):
    return {"resolved_path": {"path": name, "id": id_, "args": args}}


def fn(inputs, output=None, const=False):
    return {
        "function": {
            "sig": {"inputs": inputs, "output": output, "is_c_variadic": False},
            "generics": {"params": [], "where_predicates": []},
            "header": {"is_const": const, "is_unsafe": False, "is_async": False, "abi": "Rust"},
            "has_body": True,
        }
    }


NO_GENERICS = {"params": [], "where_predicates": []}


def doc(extra_root_items=(), extra_index=(), widget_impls=(10, 11, 12, 13)):
    """A crate `demo` with a private module `inner` re-exported at the root.

    pub use inner::Widget;   // #[non_exhaustive] struct with field `size`
    impl Widget { pub fn new(size: u8) -> Self; fn private(); }
    impl Clone for Widget;   impl<T> From<T> for T (blanket);  impl Send
    pub enum Mode { Fast, Slow { level: u8 } }
    """
    index = {
        1: item(1, None, {"module": {"is_crate": True, "items": [2, 30] + list(extra_root_items), "is_stripped": False}}),
        2: item(2, None, {"use": {"source": "inner::Widget", "name": "Widget", "id": 3, "is_glob": False}}),
        3: item(
            3,
            "Widget",
            {"struct": {"kind": {"plain": {"fields": [4], "has_stripped_fields": False}}, "generics": NO_GENERICS, "impls": list(widget_impls)}},
            attrs=["non_exhaustive"],
        ),
        4: item(4, "size", {"struct_field": {"primitive": "u8"}}),
        10: item(10, None, {"impl": {"is_unsafe": False, "generics": NO_GENERICS, "trait": None, "for": path(3, "Widget"), "items": [20, 21], "is_negative": False, "is_synthetic": False, "blanket_impl": None}}),
        11: item(11, None, {"impl": {"is_unsafe": False, "generics": NO_GENERICS, "trait": {"path": "Clone", "id": 90, "args": None}, "for": path(3, "Widget"), "items": [], "is_negative": False, "is_synthetic": False, "blanket_impl": None}}),
        12: item(12, None, {"impl": {"is_unsafe": False, "generics": NO_GENERICS, "trait": {"path": "From", "id": 91, "args": None}, "for": {"generic": "T"}, "items": [], "is_negative": False, "is_synthetic": False, "blanket_impl": {"generic": "T"}}}),
        13: item(13, None, {"impl": {"is_unsafe": False, "generics": NO_GENERICS, "trait": {"path": "Send", "id": 92, "args": None}, "for": path(3, "Widget"), "items": [], "is_negative": False, "is_synthetic": True, "blanket_impl": None}}),
        20: item(20, "new", fn([["size", {"primitive": "u8"}]], {"generic": "Self"}, const=True)),
        21: item(21, "private", fn([]), visibility="default"),
        30: item(30, "Mode", {"enum": {"generics": NO_GENERICS, "has_stripped_variants": False, "variants": [31, 32], "impls": []}}),
        31: item(31, "Fast", {"variant": {"kind": "plain", "discriminant": None}}),
        32: item(32, "Slow", {"variant": {"kind": {"struct": {"fields": [33], "has_stripped_fields": False}}, "discriminant": None}}),
        33: item(33, "level", {"struct_field": {"primitive": "u8"}}),
    }
    for entry in extra_index:
        index[entry["id"]] = entry
    return {
        "format_version": 61,
        "root": 1,
        "index": {str(k): v for k, v in index.items()},
        "paths": {
            "3": {"crate_id": 0, "path": ["demo", "inner", "Widget"], "kind": "struct"},
            "90": {"crate_id": 2, "path": ["core", "clone", "Clone"], "kind": "trait"},
            "91": {"crate_id": 2, "path": ["core", "convert", "From"], "kind": "trait"},
            "92": {"crate_id": 2, "path": ["core", "marker", "Send"], "kind": "trait"},
        },
    }


ITEMS = {
    "demo::Widget": (None, None),
    "impl core::clone::Clone for demo::Widget": ("demo::Widget", None),
    "impl core::marker::Send for demo::Widget": ("demo::Widget", None),
    "demo::Widget::size": ("demo::Widget", None),
    "demo::Widget::new": ("demo::Widget", None),
    "demo::Mode": (None, None),
    "demo::Mode::Fast": ("demo::Mode", None),
    "demo::Mode::Slow": ("demo::Mode", None),
    "demo::Mode::Slow::level": ("demo::Mode::Slow", None),
}
CLASSES = {path: "stable" for path in ITEMS}
POLICY = """
[non_exhaustive]
items = ["demo::Widget"]
"""
STABILITY = """# Stability

## Rust: `demo`

| Item | Class | Notes |
| --- | --- | --- |
| `Widget`, `Mode` | Stable | |

## HTTP API

| Route | Class |
| --- | --- |
| `GET /x` | Stable and something else |
"""


class RenderTests(unittest.TestCase):
    def test_renders_public_paths_attributes_members_and_impls(self):
        snapshot = api.build_snapshot("demo", [(None, doc())])
        self.assertEqual(
            snapshot.lines,
            [
                "pub enum demo::Mode",
                "pub demo::Mode::Fast",
                "pub demo::Mode::Slow { level }",
                "pub demo::Mode::Slow::level: u8",
                "#[non_exhaustive] pub struct demo::Widget",
                "impl core::clone::Clone for demo::Widget",
                "impl core::marker::Send for demo::Widget",
                "pub const fn demo::Widget::new(size: u8) -> Self",
                "pub demo::Widget::size: u8",
            ],
        )
        # Re-exported under its public path, not the private module path;
        # the blanket impl and the private method are omitted; trait
        # implementations are keyed for classification.
        self.assertEqual(snapshot.items, ITEMS)
        self.assertEqual(snapshot.non_exhaustive, {"demo::Widget"})

    def test_feature_only_items_and_lost_auto_traits_are_marked(self):
        extra = item(40, "helper", fn([]))
        featured = doc(extra_root_items=[40], extra_index=[extra], widget_impls=(10, 11, 12))
        snapshot = api.build_snapshot("demo", [(None, doc()), ("test-support", featured)])
        self.assertIn('#[cfg(feature = "test-support")] pub fn demo::helper()', snapshot.lines)
        self.assertIn(
            '#[cfg(not(feature = "test-support"))] impl core::marker::Send for demo::Widget',
            snapshot.lines,
        )
        self.assertEqual(snapshot.items["demo::helper"], (None, "test-support"))

    def test_named_item_missing_with_a_feature_fails(self):
        without_mode = doc()
        without_mode["index"]["1"]["inner"]["module"]["items"] = [2]
        with self.assertRaises(SystemExit):
            api.build_snapshot("demo", [(None, doc()), ("test-support", without_mode)])

    def test_unsupported_format_fails_loudly(self):
        old = doc()
        old["format_version"] = 60
        with self.assertRaises(SystemExit):
            api.Renderer(old)

    def test_type_rendering(self):
        renderer = api.Renderer(doc())
        result = path(35, "Result", {"angle_bracketed": {"args": [{"type": {"primitive": "bool"}}, {"type": path(3, "Widget")}], "constraints": []}})
        self.assertEqual(renderer.ty(result), "Result<bool, demo::inner::Widget>")
        renderer.render("demo")  # public paths are known after the walk
        self.assertEqual(renderer.ty(result), "Result<bool, demo::Widget>")
        self.assertEqual(
            renderer.ty({"borrowed_ref": {"lifetime": "'a", "is_mutable": True, "type": {"slice": {"tuple": [{"primitive": "u64"}, {"primitive": "u64"}]}}}}),
            "&'a mut [(u64, u64)]",
        )
        self.assertEqual(
            renderer.ty({"impl_trait": [{"trait_bound": {"trait": {"path": "Iterator", "id": 99, "args": {"angle_bracketed": {"args": [], "constraints": [{"name": "Item", "args": None, "binding": {"equality": {"type": {"primitive": "u64"}}}}]}}}, "generic_params": [], "modifier": "none"}}, {"outlives": "'_"}]}),
            "impl Iterator<Item = u64> + '_",
        )


class SnapshotComparisonTests(unittest.TestCase):
    def setUp(self):
        self.snapshot = api.build_snapshot("demo", [(None, doc())])
        self.committed = {"demo": api.render_file(self.snapshot)}

    def inputs(self, **changes):
        values = dict(committed=self.committed, classification=self._classes(), stability=STABILITY, policy=POLICY)
        values.update(changes)
        return api.Inputs(**values)

    def test_identical_snapshot_passes(self):
        self.assertEqual(api.run_checks([self.snapshot], self.inputs()), [])

    def test_changed_code_fails_with_a_diff(self):
        committed = {"demo": self.committed["demo"].replace("size: u8", "size: u16")}
        errors = api.run_checks([self.snapshot], self.inputs(committed=committed))
        self.assertEqual(len(errors), 1)
        self.assertIn("differs from the code", errors[0])
        self.assertIn("+pub demo::Widget::size: u8", errors[0])

    def test_missing_snapshot_fails(self):
        errors = api.run_checks([self.snapshot], self.inputs(committed={}))
        self.assertTrue(any("is missing" in e for e in errors))

    def test_cosmetic_stability_edit_does_not_satisfy_a_new_item(self):
        # A new public item, added to the snapshot and named in stability.md,
        # still fails until the classification file classifies it.
        extra = item(40, "helper", fn([]))
        snapshot = api.build_snapshot("demo", [(None, doc(extra_root_items=[40], extra_index=[extra]))])
        committed = {"demo": api.render_file(snapshot)}
        stability = STABILITY.replace("| `Widget`, `Mode` | Stable | |", "| `Widget`, `Mode`, `helper` | Stable | |")
        errors = api.run_checks([snapshot], self.inputs(committed=committed, stability=stability))
        self.assertIn(
            "unclassified public item: demo::helper (add it to docs/api/public-api-classification.txt)",
            errors,
        )

    def test_new_trait_implementation_needs_a_class(self):
        # A derive added to a public type is a new public promise too.
        classes = self._classes().replace("stable impl core::clone::Clone for demo::Widget\n", "")
        errors = api.run_checks([self.snapshot], self.inputs(classification=classes))
        self.assertEqual(
            errors,
            ["unclassified trait implementation: impl core::clone::Clone for demo::Widget "
             "(add it to docs/api/public-api-classification.txt)"],
        )

    def test_base_narrowing_is_checked(self):
        base = self._classes()
        narrowed = base.replace("stable demo::Widget::new", "experimental demo::Widget::new")
        errors = api.run_checks([self.snapshot], self.inputs(classification=narrowed, base_classification=base))
        self.assertTrue(any(e.startswith("demo::Widget::new was stable at the base") for e in errors), errors)

    def test_semver_exceptions_are_checked(self):
        errors = api.run_checks(
            [self.snapshot], self.inputs(crate_lints={"demo": {"enum_marked_non_exhaustive": "allow"}})
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("has no [[semver_exception]]", errors[0])

    @staticmethod
    def _classes():
        return "".join(f"{cls} {path}\n" for path, cls in CLASSES.items())


class ClassificationTests(unittest.TestCase):
    def test_parse(self):
        classes, errors = api.parse_classification(
            "# comment\n\nstable demo::Widget  # trailing\ninternal demo::Mode\n"
            "stable       impl core::convert::From<demo::Mode> for demo::Widget\n"
        )
        self.assertEqual(
            (classes, errors),
            (
                {
                    "demo::Widget": "stable",
                    "demo::Mode": "internal",
                    "impl core::convert::From<demo::Mode> for demo::Widget": "stable",
                },
                [],
            ),
        )

    def test_parse_rejects_malformed_keys(self):
        _, errors = api.parse_classification("stable demo::Widget extra\nstable impl Clone\n")
        self.assertEqual(len(errors), 2)

    def test_parse_rejects_unknown_classes_and_duplicates(self):
        _, errors = api.parse_classification("frozen demo::Widget\nstable demo::Mode\nstable demo::Mode\n")
        self.assertEqual(len(errors), 2)
        self.assertIn("line 1", errors[0])
        self.assertIn("classified twice", errors[1])

    def test_complete_classification_passes(self):
        self.assertEqual(api.check_classification(ITEMS, CLASSES), [])

    def test_unclassified_item_fails(self):
        classes = dict(CLASSES)
        del classes["demo::Widget::new"]
        self.assertEqual(
            api.check_classification(ITEMS, classes),
            ["unclassified public item: demo::Widget::new (add it to docs/api/public-api-classification.txt)"],
        )

    def test_stale_classification_fails(self):
        classes = dict(CLASSES, **{"demo::Gone": "stable"})
        self.assertEqual(api.check_classification(ITEMS, classes), ["classification names no public item: demo::Gone"])

    def test_member_above_parent_fails(self):
        classes = dict(CLASSES, **{"demo::Mode": "experimental"})
        errors = api.check_classification(ITEMS, classes)
        self.assertIn("demo::Mode::Fast is stable but its parent demo::Mode is experimental", errors)

    def test_member_below_parent_passes(self):
        classes = dict(CLASSES, **{"demo::Mode::Slow": "experimental", "demo::Mode::Slow::level": "internal"})
        self.assertEqual(api.check_classification(ITEMS, classes), [])

    def test_implementation_above_its_type_fails(self):
        classes = dict(CLASSES, **{"demo::Widget": "experimental", "demo::Widget::size": "experimental",
                                   "demo::Widget::new": "experimental"})
        self.assertEqual(
            api.check_classification(ITEMS, classes),
            [
                "impl core::clone::Clone for demo::Widget is stable but its parent demo::Widget is experimental",
                "impl core::marker::Send for demo::Widget is stable but its parent demo::Widget is experimental",
            ],
        )

    def test_unstable_trait_implementations_must_be_internal(self):
        key = "impl core::marker::Freeze for demo::Widget"
        items = dict(ITEMS, **{key: ("demo::Widget", None)})
        self.assertEqual(
            api.check_classification(items, dict(CLASSES, **{key: "stable"})),
            [f"{key} implements an unstable trait and must be internal, not stable"],
        )
        self.assertEqual(api.check_classification(items, dict(CLASSES, **{key: "internal"})), [])

    def test_test_support_items_must_be_internal(self):
        items = dict(ITEMS, **{"demo::helper": (None, "test-support")})
        classes = dict(CLASSES, **{"demo::helper": "stable"})
        self.assertEqual(
            api.check_classification(items, classes),
            ["demo::helper exists only with feature 'test-support' and must be internal, not stable"],
        )


class StabilityTests(unittest.TestCase):
    def check(self, text, classes=CLASSES, items=ITEMS):
        rows, errors = api.parse_stability(text)
        return errors + api.check_stability(rows, items, classes)

    def test_matching_document_passes(self):
        self.assertEqual(self.check(STABILITY), [])

    def test_only_rust_sections_are_read(self):
        rows, errors = api.parse_stability(STABILITY)
        self.assertEqual(errors, [])
        self.assertEqual(rows, [("demo", 7, ["Widget", "Mode"], "stable")])

    def test_stale_name_fails(self):
        text = STABILITY.replace("`Widget`, `Mode`", "`Widget`, `Mode`, `Mode::Ett`")
        self.assertEqual(self.check(text), ["stability.md:7: `Mode::Ett` is not a public item of demo"])

    def test_class_mismatch_fails_both_ways(self):
        # A cosmetic class change in the document alone fails...
        text = STABILITY.replace("| `Widget`, `Mode` | Stable | |", "| `Widget` | Stable | |\n| `Mode` | Experimental | |")
        self.assertIn("stability.md:8: `demo::Mode` is Experimental there but stable in the classification", self.check(text))

    def test_mixed_class_cell_fails(self):
        text = STABILITY.replace("| Stable | |", "| Stable, except members | |")
        errors = self.check(text)
        self.assertIn("class must be exactly", errors[0])

    def test_unlisted_crate_root_item_fails(self):
        text = STABILITY.replace("`Widget`, `Mode`", "`Widget`")
        self.assertEqual(self.check(text), ["stability.md does not list the crate-root item `demo::Mode` (stable)"])

    def test_member_with_a_different_class_must_be_listed(self):
        classes = dict(CLASSES, **{"demo::Mode::Slow": "experimental", "demo::Mode::Slow::level": "experimental"})
        self.assertEqual(
            self.check(STABILITY, classes),
            ["stability.md does not list `demo::Mode::Slow`, which is experimental while demo::Mode is stable"],
        )
        listed = STABILITY.replace("| `Widget`, `Mode` | Stable | |", "| `Widget`, `Mode` | Stable | |\n| `Mode::Slow` | Experimental | |")
        self.assertEqual(self.check(listed, classes), [])

    def test_explicit_name_beats_a_pattern(self):
        classes = dict(CLASSES, **{"demo::Mode::Slow": "experimental", "demo::Mode::Slow::level": "experimental"})
        text = STABILITY.replace(
            "| `Widget`, `Mode` | Stable | |",
            "| `Widget`, `Mode`, `Mode::*` | Stable | |\n| `Mode::Slow` | Experimental | |",
        )
        self.assertEqual(self.check(text, classes), [])

    def test_conflicting_rows_fail(self):
        text = STABILITY.replace("| `Widget`, `Mode` | Stable | |", "| `Widget`, `Mode` | Stable | |\n| `Mode` | Internal | |")
        errors = self.check(text)
        self.assertIn("stability.md:8: `demo::Mode` is also listed as stable on line 7", errors)

    def test_document_without_rust_rows_fails(self):
        self.assertIn("stability.md: no Rust classification rows found", self.check("# Nothing\n"))


class PolicyTests(unittest.TestCase):
    NARROWING = """
[[narrowing]]
item = "demo::Widget::new"
from = "stable"
to = "experimental"
release = "1.0.0"
reason = "test"
"""

    def policy(self, text):
        policy, errors = api.parse_policy(text)
        self.assertEqual(errors, [])
        return policy

    def test_invalid_entries_are_rejected(self):
        _, errors = api.parse_policy(self.NARROWING.replace('to = "experimental"', 'to = "stable"'))
        self.assertIn("is not a narrowing", errors[0])
        _, errors = api.parse_policy('[[narrowing]]\nitem = "demo::Widget"\n')
        self.assertIn("lacks from, to, release, reason", errors[0])
        _, errors = api.parse_policy('[[semver_exception]]\ncrate = "demo"\nlints = ["x"]\nuntil = "next"\nitems = ["a"]\ndecision = "d"\n')
        self.assertIn("invalid version", errors[0])
        _, errors = api.parse_policy("not toml [")
        self.assertEqual(len(errors), 1)

    # -- non_exhaustive (A3) --

    def test_non_exhaustive_must_be_approved(self):
        # E.g. #[non_exhaustive] added to a type frozen exhaustive.
        policy = self.policy(POLICY)
        self.assertEqual(
            api.check_non_exhaustive({"demo::Widget", "demo::Mode"}, policy),
            ["demo::Mode is #[non_exhaustive] without approval in docs/api/public-api-policy.toml"],
        )

    def test_approved_non_exhaustive_must_stay(self):
        self.assertEqual(
            api.check_non_exhaustive(set(), self.policy(POLICY)),
            ["demo::Widget is approved as #[non_exhaustive] in docs/api/public-api-policy.toml but is not"],
        )

    # -- narrowing (A2) --

    def test_unapproved_narrowing_fails(self):
        current = dict(CLASSES, **{"demo::Widget::new": "experimental"})
        errors = api.check_narrowing(CLASSES, current, api.Policy())
        self.assertEqual(len(errors), 1)
        self.assertIn("demo::Widget::new was stable at the base and is now experimental", errors[0])

    def test_approved_narrowing_passes(self):
        current = dict(CLASSES, **{"demo::Widget::new": "experimental"})
        self.assertEqual(api.check_narrowing(CLASSES, current, self.policy(self.NARROWING)), [])

    def test_removing_a_stable_item_or_implementation_is_a_narrowing(self):
        current = dict(CLASSES)
        del current["demo::Widget::new"]
        del current["impl core::marker::Send for demo::Widget"]
        errors = api.check_narrowing(CLASSES, current, api.Policy())
        self.assertEqual(len(errors), 2)
        self.assertTrue(all("is now removed" in e for e in errors), errors)
        policy = self.policy(self.NARROWING.replace('to = "experimental"', 'to = "removed"'))
        self.assertEqual(len(api.check_narrowing(CLASSES, current, policy)), 1)

    def test_experimental_changes_are_not_narrowings(self):
        base = dict(CLASSES, **{"demo::Mode::Slow": "experimental", "demo::Mode::Slow::level": "experimental"})
        current = dict(base, **{"demo::Mode::Slow::level": "internal"})
        del current["demo::Mode::Slow"]
        self.assertEqual(api.check_narrowing(base, current, api.Policy()), [])

    def test_stale_narrowing_entry_fails_without_a_base(self):
        errors = api.check_narrowing(None, CLASSES, self.policy(self.NARROWING))
        self.assertEqual(
            errors, ["narrowing entry demo::Widget::new (stable -> experimental) does not match its current class stable"]
        )

    def test_no_base_means_no_comparison(self):
        current = dict(CLASSES, **{"demo::Widget::new": "internal"})
        self.assertEqual(api.check_narrowing(None, current, api.Policy()), [])

    # -- semver-checks exceptions (B) --

    EXCEPTION = """
[[semver_exception]]
crate = "demo"
lints = ["enum_marked_non_exhaustive"]
until = "1.0.0"
items = ["demo::Mode"]
decision = "test"
"""
    LINTS = {"demo": {"enum_marked_non_exhaustive": "allow"}}

    def test_approved_exception_passes_until_its_release(self):
        policy = self.policy(self.EXCEPTION)
        for version in ("0.9.0", "1.0.0-rc.1", "1.0.0"):
            self.assertEqual(api.check_semver_exceptions(policy, self.LINTS, version), [], version)

    def test_exception_expires_after_its_release(self):
        policy = self.policy(self.EXCEPTION)
        for version in ("1.0.1", "1.1.0", "2.0.0"):
            errors = api.check_semver_exceptions(policy, self.LINTS, version)
            self.assertEqual(len(errors), 1, version)
            self.assertIn("remove it from crates/demo/Cargo.toml", errors[0])

    def test_lint_without_entry_and_stale_entry_fail(self):
        errors = api.check_semver_exceptions(api.Policy(), self.LINTS, "0.9.0")
        self.assertIn("has no [[semver_exception]]", errors[0])
        errors = api.check_semver_exceptions(self.policy(self.EXCEPTION), {}, "0.9.0")
        self.assertIn("is stale", errors[0])


class RustdocInvocationTests(unittest.TestCase):
    def test_hidden_items_are_documented(self):
        # A4: `#[doc(hidden)] pub` items must reach the snapshot.
        calls = []
        original_run = api.subprocess.run
        api.subprocess.run = lambda cmd, **kwargs: calls.append((cmd, kwargs))
        try:
            with self.assertRaises(FileNotFoundError):
                api.rustdoc_json("demo", "demo", ["test-support"], "/nonexistent-target-dir")
        finally:
            api.subprocess.run = original_run
        cmd, kwargs = calls[0]
        self.assertIn("--document-hidden-items", cmd)
        self.assertEqual(cmd[cmd.index("--features") + 1], "test-support")
        self.assertEqual(kwargs["env"]["RUSTC_BOOTSTRAP"], "1")


class CommittedFilesTests(unittest.TestCase):
    """The committed files parse; the full check runs in CI with cargo."""

    def test_committed_classification_parses(self):
        with open(api.CLASSIFICATION, encoding="utf-8") as handle:
            classes, errors = api.parse_classification(handle.read())
        self.assertEqual(errors, [])
        self.assertGreater(len(classes), 0)

    def test_committed_stability_parses(self):
        with open(api.STABILITY, encoding="utf-8") as handle:
            rows, errors = api.parse_stability(handle.read())
        self.assertEqual(errors, [])
        self.assertEqual({row[0] for row in rows}, {"knotrel_core", "knotrel_server"})

    def test_committed_snapshots_cover_every_classified_item(self):
        with open(api.CLASSIFICATION, encoding="utf-8") as handle:
            classes, _ = api.parse_classification(handle.read())
        text = ""
        for _, crate_name, _ in api.CRATES:
            with open(api.snapshot_file(crate_name), encoding="utf-8") as handle:
                text += handle.read()
        missing = [
            p for p in classes
            if not (p in text if api.is_impl(p) else re.search(rf"(?m)(^|[ (]){re.escape(p)}([ :(<{{;=]|$)", text))
        ]
        self.assertEqual(missing, [])

    def test_committed_policy_matches_the_manifests(self):
        with open(api.POLICY, encoding="utf-8") as handle:
            policy, errors = api.parse_policy(handle.read())
        self.assertEqual(errors, [])
        self.assertEqual(api.check_semver_exceptions(policy, api.crate_semver_lints(), api.workspace_version()), [])


if __name__ == "__main__":
    unittest.main()
