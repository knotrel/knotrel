#!/usr/bin/env python3
"""Node and edge lifecycle on the synthetic power grid (Knotrel v0.5.0).

The script loads the same synthetic network as ``power_grid_demo.py`` into a
fresh, dedicated standalone server started with
``KNOTREL_TOPOLOGY_LIFECYCLE=true``, then:

1. deactivates secondary substation A2 and observes which customers lose
   their path to the assumed source;
2. disables connection A-A2-A3 while A2 is inactive;
3. reactivates A2 and shows that A-A2-A3 stays disabled;
4. removes A2 permanently, which deletes all its connections;
5. re-creates A2 and shows that none of its old connections return.

This is a topology demonstration only. A path to the primary substation does
not prove that a customer is energized; no voltage, current, power flow,
thermal, protection or switching-safety analysis is performed, and none of
these steps is an operating recommendation for a real network.

Only the Python 3 standard library is used. Exit codes are those of
``power_grid_demo.py``; exit code 4 also covers a server without lifecycle
support.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import power_grid_demo as base  # noqa: E402

SUBSTATION = "202"
DISABLED_CONNECTION = "A-A2-A3"
#: Customers without a path after each step, for the supplied grid.json.
EXPECTED = {
    "deactivated": {"303", "304", "305"},
    "disabled_while_inactive": {"303", "304", "305"},
    "reactivated": {"305"},
    "removed": {"303", "304", "305"},
    "recreated": {"303", "304", "305"},
}


def require_lifecycle(client: base.Client) -> None:
    info = client.request("GET", "/v1/info")
    base.expect_status(info, 200, "GET /v1/info")
    if not isinstance(info.body, dict) or info.body.get("topology_lifecycle") is not True:
        raise base.DemoError(
            base.EXIT_REFUSED,
            "server has no node and edge lifecycle support; start it with "
            "KNOTREL_TOPOLOGY_LIFECYCLE=true. Nothing was modified.",
        )


def mutate(client: base.Client, operation: dict[str, Any], version: int, what: str) -> int:
    response = client.operation(operation)
    base.expect_status(response, 200, what)
    if response.body != {"result": "changed", "changed": True}:
        raise base.fail(f"{what}: returned {response.body!r}, expected changed=true")
    base.expect_state_version(response, version + 1, what)
    return version + 1


def edge_state(client: base.Client, conn: dict[str, str]) -> dict[str, Any]:
    response = client.operation({"op": "edge_state", "source": conn["a"], "target": conn["b"]})
    base.expect_status(response, 200, f"edge_state {conn['id']}")
    if not isinstance(response.body, dict) or response.body.get("result") != "edge_state":
        raise base.fail(f"edge_state {conn['id']}: returned {response.body!r}")
    return response.body


def expect_edge(client: base.Client, conn: dict[str, str], expected: dict[str, bool]) -> None:
    state = edge_state(client, conn)
    actual = {key: state.get(key) for key in expected}
    if actual != expected:
        raise base.fail(f"edge_state {conn['id']}: expected {expected}, got {state!r}")
    flags = ", ".join(f"{key}={value}" for key, value in actual.items())
    print(f"  connection {conn['id']}: {flags}")


def connection(grid: dict[str, Any], connection_id: str) -> dict[str, str]:
    for conn in grid["closed"]:
        if conn["id"] == connection_id:
            return conn
    raise base.DemoError(base.EXIT_USAGE, f"dataset has no closed connection {connection_id}")


def run(client: base.Client, grid: dict[str, Any]) -> None:
    if SUBSTATION not in grid["nodes"]:
        raise base.DemoError(base.EXIT_USAGE, f"dataset has no node {SUBSTATION}")
    disabled = connection(grid, DISABLED_CONNECTION)
    feeder_link = next(c for c in grid["closed"] if SUBSTATION in (c["a"], c["b"]) and c is not disabled)
    require_lifecycle(client)
    base.preflight(client, grid)
    version = base.load_topology(client, grid)
    base.expect_set(base.without_path(client, grid, version, "Initial topology"), set(), grid, "Initial topology")
    name = base.label(grid, SUBSTATION)

    print(f"\nDeactivating {name} (set_node_active false); its connections are kept")
    version = mutate(client, {"op": "set_node_active", "node": SUBSTATION, "active": False}, version,
                     "deactivate substation")
    base.expect_set(base.without_path(client, grid, version, "Substation inactive"), EXPECTED["deactivated"], grid,
                    "Substation inactive")

    print(f"\nDisabling connection {disabled['id']} while {name} is inactive (set_edge_enabled false)")
    version = mutate(client, {"op": "set_edge_enabled", "source": disabled["a"], "target": disabled["b"],
                              "enabled": False}, version, "disable connection")
    expect_edge(client, disabled, {"registered": True, "enabled": False, "effective": False})
    base.expect_set(base.without_path(client, grid, version, "Connection disabled"),
                    EXPECTED["disabled_while_inactive"], grid, "Connection disabled")

    print(f"\nReactivating {name} (set_node_active true)")
    version = mutate(client, {"op": "set_node_active", "node": SUBSTATION, "active": True}, version,
                     "reactivate substation")
    expect_edge(client, disabled, {"registered": True, "enabled": False, "effective": False})
    expect_edge(client, feeder_link, {"registered": True, "enabled": True, "effective": True})
    base.expect_set(base.without_path(client, grid, version, "Substation reactivated"), EXPECTED["reactivated"],
                    grid, "Substation reactivated")

    print(f"\nRemoving {name} permanently (remove_node): all its connections are deleted")
    version = mutate(client, {"op": "remove_node", "node": SUBSTATION}, version, "remove substation")
    expect_edge(client, feeder_link, {"registered": False})
    base.expect_set(base.without_path(client, grid, version, "Substation removed"), EXPECTED["removed"], grid,
                    "Substation removed")

    print(f"\nRe-creating {name} (add_node): a fresh, isolated node")
    version = mutate(client, {"op": "add_node", "node": SUBSTATION}, version, "re-create substation")
    expect_edge(client, feeder_link, {"registered": False})
    expect_edge(client, disabled, {"registered": False})
    base.expect_set(base.without_path(client, grid, version, "Substation re-created"), EXPECTED["recreated"], grid,
                    "Substation re-created")

    print("\nAll checks passed. Topological connectivity only: no voltage, current, power flow, "
          "thermal, protection or switching-safety analysis was performed.")


def main(argv: list[str]) -> int:
    args = base.parse_args(argv)
    try:
        grid = base.load_dataset(args.dataset)
        run(base.Client(args.url, args.timeout), grid)
    except base.DemoError as err:
        sys.stdout.flush()
        print(f"\nERROR: {err}", file=sys.stderr)
        return err.exit_code
    return base.EXIT_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
