#!/usr/bin/env python3
"""Tests for scripts/check-openapi-compat.py: each rule detects its break,
compatible additions pass, experimental operations only produce notes."""

import copy
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

import yaml

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SCRIPT = os.path.join(ROOT, "scripts", "check-openapi-compat.py")
spec = importlib.util.spec_from_file_location("compat", SCRIPT)
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)

BASE = {
    "openapi": "3.1.0",
    "info": {"title": "t", "version": "1"},
    "paths": {
        "/v1/info": {
            "get": {
                "parameters": [
                    {"name": "verbose", "in": "query", "required": False, "schema": {"type": "boolean"}}
                ],
                "responses": {
                    "200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Info"}}}},
                    "503": {"description": "busy", "content": {
                        "application/problem+json": {"schema": {"$ref": "#/components/schemas/Error"}}}},
                },
            }
        },
        "/ready": {
            "get": {
                "responses": {
                    "200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Readiness"}}}},
                },
            }
        },
        "/v1/operations": {
            "post": {
                "requestBody": {
                    "required": True,
                    "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Op"}}},
                },
                "responses": {"200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Info"}}}}},
            }
        },
        "/v1/nodes": {
            "get": {
                "x-knotrel-stability": "experimental",
                "responses": {"200": {"content": {"application/json": {"schema": {
                    "type": "object", "properties": {"nodes": {"type": "array", "items": {"type": "string"}}}}}}}},
            }
        },
    },
    "components": {
        "schemas": {
            "Info": {
                "type": "object",
                "required": ["engine", "count"],
                "properties": {
                    "engine": {"type": "string", "enum": ["compact-bfs", "ett", "hdt"]},
                    "mode": {"type": "string", "const": "source_managed"},
                    "count": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                },
            },
            "ErrorCode": {"type": "string", "enum": ["busy", "invalid_request", "unknown_node"]},
            "Error": {
                "type": "object",
                "required": ["code"],
                "properties": {"code": {"$ref": "#/components/schemas/ErrorCode"}},
            },
            "Readiness": {
                "oneOf": [
                    {"$ref": "#/components/schemas/ReadinessReady"},
                    {"$ref": "#/components/schemas/ReadinessNotReady"},
                ],
            },
            "ReadinessReady": {
                "type": "object",
                "required": ["status"],
                "properties": {"status": {"type": "string", "enum": ["ready"]}},
            },
            "ReadinessNotReady": {
                "allOf": [{
                    "type": "object",
                    "required": ["status"],
                    "properties": {"status": {"type": "string", "enum": ["starting", "loading", "catching_up"]}},
                }],
            },
            "Op": {
                "type": "object",
                "required": ["op"],
                "properties": {
                    "op": {"type": "string", "enum": ["connected", "link"]},
                    "source": {"type": "string"},
                },
            },
        }
    },
}


def breaks(mutate):
    head = copy.deepcopy(BASE)
    mutate(head)
    return compat.check(BASE, head)


class Rules(unittest.TestCase):
    def assert_break(self, mutate, fragment):
        breaking, _ = breaks(mutate)
        self.assertTrue(any(fragment in b for b in breaking), f"{fragment!r} not in {breaking}")

    def test_identical_and_additive_changes_pass(self):
        self.assertEqual(compat.check(BASE, copy.deepcopy(BASE)), ([], []))

        def additive(h):
            info = h["components"]["schemas"]["Info"]["properties"]
            info["feeder_reserved_jobs"] = {"type": "integer"}
            h["components"]["schemas"]["Op"]["properties"]["target"] = {"type": "string"}
            h["components"]["schemas"]["Op"]["properties"]["op"]["enum"].append("cut")
            h["paths"]["/v1/stats"] = {"get": {"responses": {"200": {"description": "ok"}}}}
            h["paths"]["/v1/info"]["get"]["responses"]["429"] = {"description": "new"}
            h["components"]["schemas"]["ErrorCode"]["enum"].append("request_timeout")
            info["engine"]["enum"].append("other")
            h["components"]["schemas"]["ReadinessNotReady"]["allOf"][0]["properties"]["status"]["enum"].append("stale")

        self.assertEqual(breaks(additive), ([], []))

    def test_operation_removed(self):
        self.assert_break(lambda h: h["paths"].pop("/v1/info"), "GET /v1/info: operation removed")

    def test_response_status_removed(self):
        self.assert_break(lambda h: h["paths"]["/v1/info"]["get"]["responses"].pop("503"), "response 503 removed")

    def test_parameter_removed_or_required(self):
        self.assert_break(lambda h: h["paths"]["/v1/info"]["get"].update(parameters=[]), "parameter verbose (query) removed")

        def required(h):
            h["paths"]["/v1/info"]["get"]["parameters"][0]["required"] = True

        self.assert_break(required, "parameter verbose (query) became required")

        def new_required(h):
            h["paths"]["/v1/info"]["get"]["parameters"].append(
                {"name": "x", "in": "header", "required": True, "schema": {"type": "string"}})

        self.assert_break(new_required, "new required parameter x (header)")

    def test_response_property_removed_or_optional(self):
        self.assert_break(lambda h: h["components"]["schemas"]["Info"]["properties"].pop("engine"),
                          "200.engine: response property removed")
        self.assert_break(lambda h: h["components"]["schemas"]["Info"]["required"].remove("count"),
                          "200.count: response property no longer required")

    def test_response_type_widened_or_nullable(self):
        def widen(h):
            h["components"]["schemas"]["Info"]["properties"]["count"]["type"] = ["integer", "null"]

        breaking, _ = breaks(widen)
        self.assertTrue(any("response type widened" in b for b in breaking), breaking)
        self.assertTrue(any("became nullable" in b for b in breaking), breaking)

    def test_nested_array_items_are_compared(self):
        def change(h):
            h["components"]["schemas"]["Info"]["properties"]["tags"]["items"]["type"] = ["string", "integer"]

        self.assert_break(change, "200.tags[]: response type widened")

    def test_request_rules(self):
        def required(h):
            h["components"]["schemas"]["Op"]["required"].append("source")

        self.assert_break(required, "request.source: request property became required")
        self.assert_break(lambda h: h["components"]["schemas"]["Op"]["properties"].pop("source"),
                          "request.source: request property removed")
        self.assert_break(lambda h: h["components"]["schemas"]["Op"]["properties"]["op"]["enum"].remove("link"),
                          "accepted values removed ['link']")

        def narrow(h):
            h["components"]["schemas"]["Op"]["properties"]["source"]["type"] = "integer"

        self.assert_break(narrow, "request type narrowed")

    def test_response_enum_values_removed(self):
        self.assert_break(lambda h: h["components"]["schemas"]["ErrorCode"]["enum"].remove("busy"),
                          "GET /v1/info 503.code: response values removed ['busy']")
        self.assert_break(lambda h: h["components"]["schemas"]["Info"]["properties"]["engine"]["enum"].remove("hdt"),
                          "GET /v1/info 200.engine: response values removed ['hdt']")

        def readiness(h):
            h["components"]["schemas"]["ReadinessNotReady"]["allOf"][0]["properties"]["status"]["enum"].remove(
                "catching_up")

        self.assert_break(readiness, "GET /ready 200<oneOf #/components/schemas/ReadinessNotReady>.status: "
                                     "response values removed ['catching_up']")

    def test_response_enum_dropped(self):
        self.assert_break(lambda h: h["components"]["schemas"]["Info"]["properties"]["engine"].pop("enum"),
                          "GET /v1/info 200.engine: response enum removed")

        def pin(h):
            engine = h["components"]["schemas"]["Info"]["properties"]["engine"]
            engine.pop("enum")
            engine["const"] = "ett"

        breaking, _ = breaks(pin)
        self.assertFalse(any("200.engine" in b for b in breaking), breaking)

    def test_response_constant_changed_or_removed(self):
        def change(h):
            h["components"]["schemas"]["Info"]["properties"]["mode"]["const"] = "source-managed"

        self.assert_break(change, "GET /v1/info 200.mode: response constant changed from "
                                  "'source_managed' to 'source-managed'")
        self.assert_break(lambda h: h["components"]["schemas"]["Info"]["properties"]["mode"].pop("const"),
                          "GET /v1/info 200.mode: response constant removed ('source_managed')")

        def as_enum(h):
            mode = h["components"]["schemas"]["Info"]["properties"]["mode"]
            mode.pop("const")
            mode["enum"] = ["source_managed"]

        self.assertEqual(breaks(as_enum), ([], []))

    def test_schema_level_location_of_shared_values(self):
        breaking, _ = breaks(lambda h: h["components"]["schemas"]["ErrorCode"]["enum"].remove("busy"))
        self.assertEqual({b.schema_change for b in breaking},
                         {"#/components/schemas/ErrorCode: response values removed ['busy']"})
        breaking, _ = breaks(lambda h: h["components"]["schemas"]["Info"]["properties"]["engine"]["enum"].remove("hdt"))
        self.assertEqual({b.schema_change for b in breaking},
                         {"#/components/schemas/Info/properties/engine: response values removed ['hdt']"})

    def test_response_enum_removal_on_experimental_operation_only_notes(self):
        base = copy.deepcopy(BASE)
        base["paths"]["/v1/info"]["get"]["x-knotrel-stability"] = "experimental"
        head = copy.deepcopy(base)
        head["components"]["schemas"]["ErrorCode"]["enum"].remove("busy")
        breaking, info = compat.check(base, head)
        self.assertEqual(breaking, [])
        self.assertIn("(experimental) GET /v1/info 503.code: response values removed ['busy']", info)

    def test_experimental_operations_only_note(self):
        def remove(h):
            h["paths"].pop("/v1/nodes")

        breaking, info = breaks(remove)
        self.assertEqual(breaking, [])
        self.assertEqual(info, ["(experimental) GET /v1/nodes: operation removed"])

    def test_all_of_is_flattened(self):
        def to_all_of(h):
            info = h["components"]["schemas"]["Info"]
            h["components"]["schemas"]["Info"] = {
                "allOf": [
                    {"type": "object", "required": ["engine"], "properties": {
                        "engine": info["properties"]["engine"], "mode": info["properties"]["mode"]}},
                    {"type": "object", "required": ["count"], "properties": {
                        "count": info["properties"]["count"], "tags": info["properties"]["tags"]}},
                ]
            }

        self.assertEqual(breaks(to_all_of), ([], []))

    def test_head_experimental_marker_does_not_silence(self):
        def downgrade(h):
            op = h["paths"]["/v1/info"]["get"]
            op["x-knotrel-stability"] = "experimental"
            op["responses"].pop("503")

        breaking, info = breaks(downgrade)
        self.assertIn("GET /v1/info: stable operation marked experimental", breaking)
        self.assertIn("GET /v1/info: response 503 removed", breaking)
        self.assertEqual(info, [])

    def test_one_of_members_are_compared(self):
        def one_of(doc):
            doc["components"]["schemas"]["Info"]["properties"]["count"] = {
                "oneOf": [{"$ref": "#/components/schemas/Op"}, {"type": "integer"}]}

        base = copy.deepcopy(BASE)
        one_of(base)
        head = copy.deepcopy(base)
        head["components"]["schemas"]["Op"]["properties"].pop("source")
        breaking, _ = compat.check(base, head)
        self.assertTrue(any("<oneOf #/components/schemas/Op>.source: response property removed" in b
                            for b in breaking), breaking)
        self.assertEqual(compat.check(base, copy.deepcopy(base)), ([], []))

    def test_null_alternative_is_nullable(self):
        def any_of_null(h):
            h["components"]["schemas"]["Info"]["properties"]["engine"] = {
                "anyOf": [{"type": "string"}, {"type": "null"}]}

        self.assert_break(any_of_null, "200.engine: response value became nullable")

    def test_response_header_removed(self):
        def with_header(doc):
            doc["paths"]["/v1/info"]["get"]["responses"]["200"]["headers"] = {
                "ETag": {"schema": {"type": "string"}}}

        base = copy.deepcopy(BASE)
        with_header(base)
        head = copy.deepcopy(BASE)
        breaking, _ = compat.check(base, head)
        self.assertIn("GET /v1/info 200: response header ETag removed", breaking)
        self.assertEqual(compat.check(base, copy.deepcopy(base)), ([], []))

    def test_request_constraints_tightened(self):
        def tighten(h):
            h["components"]["schemas"]["Op"]["properties"]["source"]["maxLength"] = 8

        self.assert_break(tighten, "request.source: request constraint tightened (maxLength None -> 8)")

        def loosen(doc):
            doc["components"]["schemas"]["Op"]["properties"]["source"]["maxLength"] = 8

        base = copy.deepcopy(BASE)
        loosen(base)
        head = copy.deepcopy(base)
        head["components"]["schemas"]["Op"]["properties"]["source"]["maxLength"] = 16
        self.assertEqual(compat.check(base, head), ([], []))

    def test_request_body_required_flip_and_head_only_ref(self):
        base = copy.deepcopy(BASE)
        base["paths"]["/v1/operations"]["post"]["requestBody"]["required"] = False
        head = copy.deepcopy(BASE)
        breaking, _ = compat.check(base, head)
        self.assertIn("POST /v1/operations: request body became required", breaking)

        base = copy.deepcopy(BASE)
        base["paths"]["/v1/operations"]["post"].pop("requestBody")
        head = copy.deepcopy(BASE)
        head["components"]["requestBodies"] = {
            "OpBody": head["paths"]["/v1/operations"]["post"]["requestBody"]}
        head["paths"]["/v1/operations"]["post"]["requestBody"] = {"$ref": "#/components/requestBodies/OpBody"}
        breaking, _ = compat.check(base, head)
        self.assertIn("POST /v1/operations: request body became required", breaking)


class CommandLine(unittest.TestCase):
    def run_cli(self, head, approved=None, base=None):
        with tempfile.TemporaryDirectory() as d:
            b, h = os.path.join(d, "b.yaml"), os.path.join(d, "h.yaml")
            with open(b, "w") as f:
                yaml.safe_dump(BASE if base is None else base, f)
            with open(h, "w") as f:
                yaml.safe_dump(head, f)
            args = [sys.executable, SCRIPT, b, h]
            if approved is not None:
                a = os.path.join(d, "a.yaml")
                with open(a, "w") as f:
                    yaml.safe_dump(approved, f)
                args += ["--approved", a]
            return subprocess.run(args, capture_output=True, text=True)

    def test_exit_codes_and_approval(self):
        head = copy.deepcopy(BASE)
        head["paths"].pop("/v1/info")
        failed = self.run_cli(head)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.assertIn("BREAKING: GET /v1/info: operation removed", failed.stdout)
        approved = self.run_cli(head, [{"change": "GET /v1/info: operation removed",
                                        "release": "v0.10.0", "justification": "test"}])
        self.assertEqual(approved.returncode, 0, approved.stdout)
        self.assertIn("approved: GET /v1/info: operation removed", approved.stdout)
        self.assertEqual(self.run_cli(copy.deepcopy(BASE)).returncode, 0)

    def test_malformed_approval_entries_are_rejected(self):
        head = copy.deepcopy(BASE)
        head["paths"].pop("/v1/info")
        # A null `change` used to match every finding without a schema key.
        for entry in (
            {"change": None, "release": "v2.0.0", "justification": "t"},
            {"change": "", "release": "v2.0.0", "justification": "t"},
            {"change": "GET /v1/info: operation removed", "justification": "t"},
            {"change": "GET /v1/info: operation removed", "release": "v2.0.0", "justification": "  "},
            "GET /v1/info: operation removed",
        ):
            result = self.run_cli(head, [entry])
            self.assertEqual(result.returncode, 2, (entry, result.stdout, result.stderr))
            self.assertIn("approved entry 0", result.stderr)
        not_a_list = self.run_cli(head, {"change": "GET /v1/info: operation removed"})
        self.assertEqual(not_a_list.returncode, 2, not_a_list.stderr)

    def test_unrelated_approval_does_not_cover_findings_without_schema_key(self):
        head = copy.deepcopy(BASE)
        head["paths"].pop("/v1/info")
        result = self.run_cli(head, [{"change": "GET /v1/other: operation removed",
                                      "release": "v2.0.0", "justification": "t"}])
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("BREAKING: GET /v1/info: operation removed", result.stdout)

    def test_approved_response_enum_removal_passes(self):
        head = copy.deepcopy(BASE)
        head["components"]["schemas"]["ErrorCode"]["enum"].remove("busy")
        change = "GET /v1/info 503.code: response values removed ['busy']"
        failed = self.run_cli(head)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.assertIn(f"BREAKING: {change}", failed.stdout)
        approved = self.run_cli(head, [{"change": change, "release": "v2.0.0", "justification": "test"}])
        self.assertEqual(approved.returncode, 0, approved.stdout)
        self.assertIn(f"approved: {change}", approved.stdout)

    def test_schema_level_approval_covers_every_operation(self):
        head = copy.deepcopy(BASE)
        head["paths"]["/v1/operations"]["post"]["responses"]["503"] = copy.deepcopy(
            head["paths"]["/v1/info"]["get"]["responses"]["503"])
        head["components"]["schemas"]["ErrorCode"]["enum"].remove("busy")
        base = copy.deepcopy(BASE)
        base["paths"]["/v1/operations"]["post"]["responses"]["503"] = copy.deepcopy(
            base["paths"]["/v1/info"]["get"]["responses"]["503"])
        schema_change = "#/components/schemas/ErrorCode: response values removed ['busy']"
        failed = self.run_cli(head, base=base)
        self.assertEqual(failed.returncode, 1, failed.stdout)
        self.assertIn(f"one approval entry covers its 2 message(s)): {schema_change}", failed.stdout)
        approved = self.run_cli(head, [{"change": schema_change, "release": "v2.0.0", "justification": "test"}],
                                base=base)
        self.assertEqual(approved.returncode, 0, approved.stdout)
        self.assertIn("approved: GET /v1/info 503.code: response values removed ['busy']", approved.stdout)
        self.assertIn("approved: POST /v1/operations 503.code: response values removed ['busy']", approved.stdout)

    def test_usage_error(self):
        out = subprocess.run([sys.executable, SCRIPT], capture_output=True, text=True)
        self.assertEqual(out.returncode, 2)

    def test_repository_spec_against_itself(self):
        path = os.path.join(ROOT, "docs", "openapi.yaml")
        with open(path, encoding="utf-8") as f:
            doc = yaml.safe_load(f)
        self.assertEqual(compat.check(doc, copy.deepcopy(doc)), ([], []))


if __name__ == "__main__":
    unittest.main()
