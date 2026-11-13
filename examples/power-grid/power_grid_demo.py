#!/usr/bin/env python3
"""Synthetic power-grid topology demonstration for the Knotrel HTTP API.

The script acts as the application that supplies topology changes to a fresh,
dedicated standalone Knotrel server. It loads a small synthetic distribution
network, opens one connection with ``cut``, closes a normally open reserve
connection with ``link`` and checks after every step which customer nodes are
topologically connected to the assumed source (the primary substation).

This is a topology demonstration only. Connectivity to the primary substation
does not prove that a customer is energized, and the scripted reserve closure is
not an operating recommendation for any real electrical network.

Only the Python 3 standard library is used. Exit codes:

0 every check passed; 1 an unexpected HTTP status or operation outcome;
2 invalid arguments or dataset; 3 server unreachable or a request timed out
before any mutation was sent; 4 the server is not a fresh, ready standalone
instance (nothing was modified); 5 a mutation's outcome is uncertain.
"""

from __future__ import annotations

import argparse
import json
import socket
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

EXIT_OK = 0
EXIT_UNEXPECTED = 1
EXIT_USAGE = 2
EXIT_UNAVAILABLE = 3
EXIT_REFUSED = 4
EXIT_UNCERTAIN = 5

NODE_TYPES = {"primary_substation", "secondary_substation", "customer"}
MUTATIONS = {"add_node", "link", "cut"}
MAX_BATCH = 1024


class DemoError(Exception):
    """A failure that stops the demonstration with a specific exit code."""

    def __init__(self, exit_code: int, message: str) -> None:
        super().__init__(message)
        self.exit_code = exit_code


class Response:
    """Status, headers and decoded JSON body of one HTTP exchange."""

    def __init__(self, status: int, headers: Any, body: Any) -> None:
        self.status = status
        self.headers = headers
        self.body = body

    def header(self, name: str) -> str | None:
        return self.headers.get(name) if self.headers is not None else None


class Client:
    """Minimal JSON client with a bounded timeout and no automatic retries."""

    def __init__(self, base_url: str, timeout: float) -> None:
        self.base_url = base_url
        self.timeout = timeout

    def request(self, method: str, path: str, payload: Any = None, *, mutates: bool = False) -> Response:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(self.base_url + path, data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                return Response(resp.status, resp.headers, self._decode(resp.read(), method, path))
        except urllib.error.HTTPError as err:
            raw = err.read()
            try:
                body = json.loads(raw) if raw else None
            except ValueError:
                body = None
            response = Response(err.code, err.headers, body)
            code = body.get("code") if isinstance(body, dict) else None
            if mutates and err.code == 503 and code == "unavailable":
                raise DemoError(
                    EXIT_UNCERTAIN,
                    f"{method} {path} returned 503 unavailable: the mutation outcome is "
                    "uncertain. Not retrying. Restart the server to obtain a known empty graph.",
                ) from None
            return response
        except (TimeoutError, socket.timeout, urllib.error.URLError, ConnectionError, OSError) as err:
            reason = getattr(err, "reason", err)
            timed_out = isinstance(reason, (TimeoutError, socket.timeout))
            # A refused connection means the request was never delivered.
            if mutates and not isinstance(reason, ConnectionRefusedError):
                raise DemoError(
                    EXIT_UNCERTAIN,
                    f"{method} {path} failed after sending a mutation ({reason!r}). The server "
                    "may still have applied it, so the graph state is uncertain. Not retrying. "
                    "Restart the server to obtain a known empty graph.",
                ) from None
            what = "timed out" if timed_out else "could not reach the server"
            raise DemoError(
                EXIT_UNAVAILABLE,
                f"{method} {self.base_url}{path} {what} ({reason!r}). Start a dedicated "
                "standalone server first (see examples/power-grid/README.md).",
            ) from None

    @staticmethod
    def _decode(raw: bytes, method: str, path: str) -> Any:
        try:
            return json.loads(raw)
        except ValueError:
            raise DemoError(EXIT_UNEXPECTED, f"{method} {path}: response body is not JSON") from None

    def batch(self, operations: list[dict[str, str]]) -> Response:
        mutates = any(op["op"] in MUTATIONS for op in operations)
        return self.request("POST", "/v1/batch", {"operations": operations}, mutates=mutates)

    def operation(self, operation: dict[str, str]) -> Response:
        return self.request("POST", "/v1/operations", operation, mutates=operation["op"] in MUTATIONS)


def fail(message: str) -> DemoError:
    return DemoError(EXIT_UNEXPECTED, message)


def load_dataset(path: Path) -> dict[str, Any]:
    """Load and validate the synthetic dataset; application metadata stays here."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as err:
        raise DemoError(EXIT_USAGE, f"cannot read dataset {path}: {err}") from None

    def invalid(message: str) -> DemoError:
        return DemoError(EXIT_USAGE, f"invalid dataset {path}: {message}")

    nodes = {}
    for node in data.get("nodes", []):
        node_id = node.get("id")
        if not isinstance(node_id, str) or not node_id.isdigit() or not node_id.isascii():
            raise invalid(f"node id {node_id!r} is not an ASCII decimal string")
        if node_id in nodes:
            raise invalid(f"duplicate node id {node_id}")
        if node.get("type") not in NODE_TYPES:
            raise invalid(f"node {node_id} has unknown type {node.get('type')!r}")
        nodes[node_id] = node
    primaries = [n for n, v in nodes.items() if v["type"] == "primary_substation"]
    if len(primaries) != 1:
        raise invalid("exactly one primary substation is required")

    connections = {}
    edges = set()
    for conn in data.get("connections", []):
        conn_id = conn.get("id")
        if conn_id in connections:
            raise invalid(f"duplicate connection id {conn_id}")
        a, b = conn.get("a"), conn.get("b")
        if a not in nodes or b not in nodes or a == b:
            raise invalid(f"connection {conn_id} must join two distinct known nodes")
        edge = frozenset((a, b))
        if edge in edges:
            raise invalid(f"connection {conn_id} duplicates another undirected edge")
        if conn.get("state") not in {"closed", "open"}:
            raise invalid(f"connection {conn_id} has state {conn.get('state')!r}")
        edges.add(edge)
        connections[conn_id] = conn

    scenario = data.get("scenario", {})
    opened = connections.get(scenario.get("open_connection"))
    closed = connections.get(scenario.get("close_connection"))
    if opened is None or opened["state"] != "closed":
        raise invalid("scenario.open_connection must name a normally closed connection")
    if closed is None or closed["state"] != "open":
        raise invalid("scenario.close_connection must name a normally open connection")
    customers = [n for n, v in nodes.items() if v["type"] == "customer"]
    for key in ("expected_without_path_after_open", "expected_without_path_after_close"):
        expected = scenario.get(key)
        if not isinstance(expected, list) or not set(expected) <= set(customers):
            raise invalid(f"scenario.{key} must list customer node ids")
    if len(nodes) + len(connections) > MAX_BATCH or len(customers) > MAX_BATCH:
        raise invalid("dataset is too large for the single-batch steps of this demonstration")

    return {
        "nodes": nodes,
        "primary": primaries[0],
        "customers": customers,
        "closed": [c for c in connections.values() if c["state"] == "closed"],
        "opened": opened,
        "reserve": closed,
        "expected_after_open": set(scenario["expected_without_path_after_open"]),
        "expected_after_close": set(scenario["expected_without_path_after_close"]),
    }


def label(grid: dict[str, Any], node_id: str) -> str:
    return f"{grid['nodes'][node_id]['name']} ({node_id})"


def expect_status(response: Response, status: int, what: str) -> None:
    if response.status != status:
        raise fail(f"{what}: expected HTTP {status}, got {response.status} with body {response.body!r}")


def expect_state_version(response: Response, expected: int, what: str) -> None:
    actual = response.header("X-Knotrel-State-Version")
    if actual != str(expected):
        raise fail(
            f"{what}: expected state version {expected}, got {actual!r}. Another client "
            "may be using this server; it must be dedicated to the demonstration."
        )


def preflight(client: Client, grid: dict[str, Any]) -> None:
    """Refuse to touch a server that is not a ready, empty, standalone instance.

    This is a best-effort check at one instant. It assumes exclusive use of the
    server and is not a concurrency guarantee: another client could still mutate
    the graph after the check.
    """
    ready = client.request("GET", "/ready")
    if ready.status != 200 or not isinstance(ready.body, dict) or ready.body.get("status") != "ready":
        raise DemoError(EXIT_REFUSED, f"server is not ready (HTTP {ready.status}, body {ready.body!r}); nothing was modified")
    stats = client.request("GET", "/v1/stats")
    expect_status(stats, 200, "GET /v1/stats")
    if stats.header("X-Knotrel-Sync-Status") is not None:
        raise DemoError(
            EXIT_REFUSED,
            "server runs in source-managed mode; this tutorial needs a dedicated standalone server. Nothing was modified.",
        )
    body = stats.body
    if not isinstance(body, dict):
        raise fail(f"GET /v1/stats returned {body!r}")
    if body.get("node_count") != 0 or body.get("edge_count") != 0 or body.get("state_version") != "0":
        raise DemoError(
            EXIT_REFUSED,
            "server already contains graph data or has changed since startup "
            f"(node_count={body.get('node_count')}, edge_count={body.get('edge_count')}, "
            f"state_version={body.get('state_version')!r}). Refusing to modify it; start a fresh, "
            "dedicated server.",
        )
    for field, needed in (("remaining_nodes", len(grid["nodes"])), ("remaining_edges", len(grid["closed"]) + 1)):
        remaining = body.get(field)
        if remaining is not None and remaining < needed:
            raise DemoError(EXIT_REFUSED, f"server capacity {field}={remaining} is below the {needed} required")


def load_topology(client: Client, grid: dict[str, Any]) -> int:
    operations = [{"op": "add_node", "node": node_id} for node_id in grid["nodes"]]
    operations += [{"op": "link", "source": c["a"], "target": c["b"]} for c in grid["closed"]]
    response = client.batch(operations)
    expect_status(response, 200, "initial load")
    results = response.body.get("results") if isinstance(response.body, dict) else None
    if not isinstance(results, list) or len(results) != len(operations):
        raise fail(f"initial load: expected {len(operations)} results, got {response.body!r}")
    for operation, result in zip(operations, results):
        if result != {"result": "changed", "changed": True}:
            raise fail(f"initial load: {operation} returned {result!r}, expected changed=true")
    expect_state_version(response, len(operations), "initial load")
    print(f"Loaded {len(grid['nodes'])} nodes and {len(grid['closed'])} closed connections; "
          f"reserve {grid['reserve']['id']} stays open (absent edge).")
    return len(operations)


def without_path(client: Client, grid: dict[str, Any], version: int, step: str) -> set[str]:
    """Ask one connectivity query per customer, in a single query-only batch."""
    operations = [{"op": "connected", "source": c, "target": grid["primary"]} for c in grid["customers"]]
    response = client.batch(operations)
    expect_status(response, 200, f"{step} queries")
    results = response.body.get("results") if isinstance(response.body, dict) else None
    if not isinstance(results, list) or len(results) != len(operations):
        raise fail(f"{step} queries: expected {len(operations)} results, got {response.body!r}")
    expect_state_version(response, version, f"{step} queries")
    missing = set()
    print(f"\n{step}: topological connectivity to {label(grid, grid['primary'])}")
    for customer, result in zip(grid["customers"], results):
        if result.get("result") != "connected" or not isinstance(result.get("connected"), bool):
            raise fail(f"{step}: query for {customer} returned {result!r}")
        print(f"  {label(grid, customer):<18} {'connected' if result['connected'] else 'NO PATH to the assumed source'}")
        if not result["connected"]:
            missing.add(customer)
    return missing


def expect_set(actual: set[str], expected: set[str], grid: dict[str, Any], step: str) -> None:
    if actual != expected:
        raise fail(
            f"{step}: customers without a path were {sorted(actual)}, expected {sorted(expected)}"
        )
    names = ", ".join(label(grid, n) for n in sorted(expected)) or "none"
    print(f"  check passed: customers without a path = {names}")


def change(client: Client, grid: dict[str, Any], op: str, conn: dict[str, str], version: int) -> int:
    operation = {"op": op, "source": conn["a"], "target": conn["b"]}
    verb = "Opening" if op == "cut" else "Closing"
    print(f"\n{verb} connection {conn['id']}: {label(grid, conn['a'])} - {label(grid, conn['b'])} ({op})")
    response = client.operation(operation)
    expect_status(response, 200, f"{op} {conn['id']}")
    if response.body != {"result": "changed", "changed": True}:
        raise fail(f"{op} {conn['id']}: returned {response.body!r}, expected changed=true")
    expect_state_version(response, version + 1, f"{op} {conn['id']}")
    return version + 1


def run(client: Client, grid: dict[str, Any]) -> None:
    preflight(client, grid)
    version = load_topology(client, grid)
    expect_set(without_path(client, grid, version, "Initial topology"), set(), grid, "Initial topology")
    version = change(client, grid, "cut", grid["opened"], version)
    expect_set(without_path(client, grid, version, "After opening"), grid["expected_after_open"], grid, "After opening")
    version = change(client, grid, "link", grid["reserve"], version)
    expect_set(without_path(client, grid, version, "After closing the reserve"), grid["expected_after_close"], grid,
               "After closing the reserve")
    print("\nAll checks passed. Topological connectivity only: no voltage, current, power flow, "
          "thermal, protection or switching-safety analysis was performed.")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--url", default="http://127.0.0.1:8080", help="server base URL (default: %(default)s)")
    parser.add_argument("--dataset", type=Path, default=Path(__file__).with_name("grid.json"),
                        help="synthetic dataset (default: grid.json next to this script)")
    parser.add_argument("--timeout", type=float, default=5.0, help="per-request timeout in seconds (default: %(default)s)")
    args = parser.parse_args(argv)
    parsed = urllib.parse.urlsplit(args.url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.query or parsed.fragment:
        parser.error("--url must be an http(s) base URL such as http://127.0.0.1:8080")
    if not 0 < args.timeout <= 300:
        parser.error("--timeout must be greater than 0 and at most 300 seconds")
    args.url = args.url.rstrip("/")
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        grid = load_dataset(args.dataset)
        run(Client(args.url, args.timeout), grid)
    except DemoError as err:
        sys.stdout.flush()
        print(f"\nERROR: {err}", file=sys.stderr)
        return err.exit_code
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
