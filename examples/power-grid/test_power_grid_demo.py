#!/usr/bin/env python3
"""Dataset-validation regression checks for power_grid_demo.py.

Run from the repository root with:

    python3 -m unittest discover -s examples/power-grid -p 'test_*.py'

Each malformed dataset must exit with code 2, print a dataset diagnostic
without a traceback, and send no HTTP request. A local listening socket stands
in for the server and must never receive a connection.
"""

from __future__ import annotations

import copy
import importlib.util
import json
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "power_grid_demo.py"
VALID = json.loads((HERE / "grid.json").read_text(encoding="utf-8"))

spec = importlib.util.spec_from_file_location("power_grid_demo", SCRIPT)
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


def mutated(change):
    data = copy.deepcopy(VALID)
    change(data)
    return data


def set_item(path, value):
    def change(data):
        target = data
        for key in path[:-1]:
            target = target[key]
        target[path[-1]] = value
    return change


MALFORMED = {
    "top-level list": ([], "top level must be a JSON object"),
    "top-level string": ("grid", "top level must be a JSON object"),
    "nodes missing": (mutated(lambda d: d.pop("nodes")), "nodes must be a JSON array"),
    "nodes object": (mutated(set_item(["nodes"], {})), "nodes must be a JSON array"),
    "node not object": (mutated(set_item(["nodes", 0], "100")), "nodes[0] must be a JSON object"),
    "node id integer": (mutated(set_item(["nodes", 0, "id"], 100)), "nodes[0].id 100 is not a string"),
    "node id empty": (mutated(set_item(["nodes", 0, "id"], "")), "nodes[0].id '' is not a string"),
    "node id signed": (mutated(set_item(["nodes", 0, "id"], "+100")), "nodes[0].id '+100'"),
    "node id non-ASCII digit": (mutated(set_item(["nodes", 0, "id"], "١٠٠")), "nodes[0].id"),
    "node id above u64": (
        mutated(set_item(["nodes", 0, "id"], "18446744073709551616")),
        "exceeds 18446744073709551615",
    ),
    "duplicate numeric node id": (
        mutated(set_item(["nodes", 1, "id"], "0100")),
        "nodes[1].id '0100' duplicates node '100' numerically",
    ),
    "node name missing": (mutated(lambda d: d["nodes"][0].pop("name")), "nodes[0].name must be a JSON string"),
    "node name empty": (mutated(set_item(["nodes", 0, "name"], "")), "nodes[0].name must not be empty"),
    "node type unknown": (mutated(set_item(["nodes", 0, "type"], "generator")), "nodes[0].type must be one of"),
    "node type list": (mutated(set_item(["nodes", 0, "type"], [])), "nodes[0].type must be a JSON string"),
    "connection state object": (mutated(set_item(["connections", 0, "state"], {})), "connections[0].state must be a JSON string"),
    "connection state unknown": (mutated(set_item(["connections", 0, "state"], "tripped")), "state must be 'closed' or 'open'"),
    "connections object": (mutated(set_item(["connections"], {})), "connections must be a JSON array"),
    "connection not object": (mutated(set_item(["connections", 0], [])), "connections[0] must be a JSON object"),
    "connection id empty": (mutated(set_item(["connections", 0, "id"], "")), "connections[0].id must not be empty"),
    "connection id integer": (mutated(set_item(["connections", 0, "id"], 1)), "connections[0].id must be a JSON string"),
    "duplicate connection id": (
        mutated(set_item(["connections", 1, "id"], "A-P-A1")),
        "connections[1].id duplicates connection 'A-P-A1'",
    ),
    "unknown endpoint": (mutated(set_item(["connections", 0, "b"], "999")), "connections[0].b '999' is not a declared node"),
    "endpoint not string": (mutated(set_item(["connections", 0, "a"], None)), "connections[0].a None"),
    "self-loop by numeric identity": (
        mutated(set_item(["connections", 0, "b"], "0100")),
        "connection A-P-A1 must join two distinct nodes",
    ),
    "scenario list": (mutated(set_item(["scenario"], [])), "scenario must be a JSON object"),
    "scenario missing": (mutated(lambda d: d.pop("scenario")), "scenario must be a JSON object"),
    "open connection unhashable": (
        mutated(set_item(["scenario", "open_connection"], ["A-A1-A2"])),
        "scenario.open_connection must be a JSON string",
    ),
    "open connection unknown": (
        mutated(set_item(["scenario", "open_connection"], "NOPE")),
        "scenario.open_connection must name a normally closed connection",
    ),
    "close connection not open": (
        mutated(set_item(["scenario", "close_connection"], "A-P-A1")),
        "scenario.close_connection must name a normally open connection",
    ),
    "expected list string": (
        mutated(set_item(["scenario", "expected_without_path_after_open"], "303")),
        "scenario.expected_without_path_after_open must be a JSON array",
    ),
    "expected element not string": (
        mutated(set_item(["scenario", "expected_without_path_after_open"], [303])),
        "scenario.expected_without_path_after_open[0] 303",
    ),
    "expected element not customer": (
        mutated(set_item(["scenario", "expected_without_path_after_close"], ["201"])),
        "scenario.expected_without_path_after_close[0] '201' is not a customer node",
    ),
}


class MalformedDatasetTests(unittest.TestCase):
    def setUp(self):
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(8)
        self.listener.setblocking(False)
        self.url = f"http://127.0.0.1:{self.listener.getsockname()[1]}"
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.listener.close()
        self.tmp.cleanup()

    def run_demo(self, dataset_text):
        path = Path(self.tmp.name) / "dataset.json"
        path.write_text(dataset_text, encoding="utf-8")
        return subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "--url", self.url, "--dataset", str(path), "--timeout", "2"],
            capture_output=True, text=True, timeout=30,
        )

    def assert_rejected(self, result, fragment):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("ERROR: ", result.stderr)
        self.assertIn(fragment, result.stderr)
        self.assertEqual(result.stdout, "")
        with self.assertRaises(BlockingIOError, msg="the script contacted the server"):
            self.listener.accept()

    def test_malformed_structures(self):
        for name, (data, fragment) in MALFORMED.items():
            with self.subTest(name):
                self.assert_rejected(self.run_demo(json.dumps(data)), fragment)

    def test_invalid_json(self):
        self.assert_rejected(self.run_demo("{not json"), "cannot read dataset")

    def test_missing_file(self):
        result = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "--url", self.url, "--dataset", str(Path(self.tmp.name) / "absent.json")],
            capture_output=True, text=True, timeout=30,
        )
        self.assert_rejected(result, "cannot read dataset")


class ValidDatasetTests(unittest.TestCase):
    def load(self, data):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "dataset.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            return demo.load_dataset(path)

    def test_supplied_dataset(self):
        grid = demo.load_dataset(HERE / "grid.json")
        self.assertEqual(grid["primary"], "100")
        self.assertEqual(grid["customers"], [str(n) for n in range(301, 308)])
        self.assertEqual(len(grid["closed"]), 12)
        self.assertEqual(grid["opened"]["id"], "A-A1-A2")
        self.assertEqual(grid["reserve"]["id"], "TIE-A3-B2")
        self.assertEqual(grid["expected_after_open"], {"303", "304", "305"})
        self.assertEqual(grid["expected_after_close"], set())

    def test_maximum_node_id_accepted(self):
        data = copy.deepcopy(VALID)
        data["nodes"][0]["id"] = "18446744073709551615"
        for conn in data["connections"]:
            for end in ("a", "b"):
                if conn[end] == "100":
                    conn[end] = "18446744073709551615"
        self.assertEqual(self.load(data)["primary"], "18446744073709551615")

    def test_references_resolve_by_numeric_identity(self):
        data = copy.deepcopy(VALID)
        data["scenario"]["expected_without_path_after_open"] = ["0303", "304", "305"]
        self.assertEqual(self.load(data)["expected_after_open"], {"303", "304", "305"})


if __name__ == "__main__":
    unittest.main()
