# Power-grid topology example

A runnable demonstration of connectivity loss and restoration in a small
**synthetic** electrical distribution network. Read the
[tutorial](../../docs/tutorials/power-grid.md) for the explanation and diagram.

This is a topology demonstration, not an electrical simulator or an operational
switching tool. The dataset is invented for teaching and is not a validated grid
model.

| File | Purpose |
| --- | --- |
| [`grid.json`](grid.json) | Synthetic dataset: node names and types, connection states and the expected scenario results. Names and types are application metadata; Knotrel receives only node IDs and active edges. |
| [`power_grid_demo.py`](power_grid_demo.py) | Python 3 standard-library script that acts as the application sending topology changes over the HTTP API. |
| [`lifecycle_demo.py`](lifecycle_demo.py) | v0.5.0 node and edge lifecycle scenario on the same network; needs a server with `KNOTREL_TOPOLOGY_LIFECYCLE=true`. |
| [`test_power_grid_demo.py`](test_power_grid_demo.py) | Dataset-validation regression checks and an independent model of the lifecycle scenario; they need no server. |

## Run it

Requires Python 3.9 or newer and a Knotrel v0.5.0 server. `power_grid_demo.py`
uses only operations that exist since v0.1.0, so a v0.3.0 or v0.4.0 server also
works. No Python packages are installed.

1. Start a **fresh, dedicated, standalone** server in one terminal, from a clone
   of this repository (or run a release binary with the same variables):

   ```sh
   KNOTREL_MODE=standalone cargo run --release --locked -p knotrel-server
   ```

   It listens on `127.0.0.1:8080` and starts with an empty in-memory graph.

2. Run the demonstration in another terminal, from the repository root:

   ```sh
   python3 examples/power-grid/power_grid_demo.py
   ```

   Options: `--url` (default `http://127.0.0.1:8080`), `--timeout` seconds per
   request (default `5`) and `--dataset` (default `grid.json` next to the script).

3. Stop the server with Ctrl+C. Restarting it discards the graph, so the
   demonstration can run again.

The script refuses a second run against the same server process, because the
graph is no longer empty.

## What the script checks

1. **Preflight.** `GET /ready` must return 200 `ready`. `GET /v1/stats` must
   report zero nodes, zero edges and `state_version` `"0"`, without a
   source-managed `X-Knotrel-Sync-Status` header. Otherwise nothing is sent.
2. **Load.** One batch adds every node and links every normally closed
   connection. The normally open reserve connection is not linked.
3. **Query.** One query-only batch asks `connected` once for each customer
   against the primary substation.
4. **Open** `A-A1-A2` with a single `cut`, then query again. Exactly meters
   M3, M4 and M5 (`303`, `304`, `305`) must have no path to the assumed source.
5. **Close** the reserve `TIE-A3-B2` with a single `link`, then query again.
   Every customer must be topologically connected again.

Every HTTP status, every per-operation result and the `X-Knotrel-State-Version`
header are checked. A mismatch in the revision usually means another client is
using the server.

The preflight is a single point-in-time check. It **assumes exclusive use** of
the server and is not a concurrency guarantee: another client could still
change the graph after the check. Use a server dedicated to this
demonstration.

## Lifecycle scenario (v0.5.0)

`lifecycle_demo.py` needs a fresh, dedicated standalone server with lifecycle
support:

```sh
KNOTREL_MODE=standalone KNOTREL_TOPOLOGY_LIFECYCLE=true cargo run --release --locked -p knotrel-server
python3 examples/power-grid/lifecycle_demo.py
```

It accepts the same options and exit codes. Exit 4 also covers a server
without lifecycle support (`/v1/info` lacks `"topology_lifecycle": true`).
After the same preflight and load, it checks:

1. **Deactivate** substation A2 (`202`) with `set_node_active`. Meters M3, M4
   and M5 lose their path; A2's connections stay registered.
2. **Disable** connection `A-A2-A3` with `set_edge_enabled` while A2 is
   inactive. `edge_state` reports it registered, disabled and not effective.
3. **Reactivate** A2. M3 and M4 regain their path through A1; `A-A2-A3`
   stays disabled, so M5 still has no path.
4. **Remove** A2 with `remove_node`. All four of its connections are deleted.
5. **Re-create** A2 with `add_node`. It is isolated: `edge_state` reports its
   old connections as not registered, and M3, M4 and M5 still have no path.

Each mutation must advance `X-Knotrel-State-Version` by exactly one, including
the cascading removal. These are topology changes only, not switching
instructions; connectivity is not power flow.

## Exit codes and failures

| Exit | Meaning |
| --- | --- |
| 0 | Every check passed. |
| 1 | Unexpected HTTP status, operation result, revision or connectivity set. |
| 2 | Invalid arguments, or a dataset that is unreadable or structurally invalid (checked before any HTTP request). |
| 3 | The server could not be reached, or a read-only request timed out. |
| 4 | Refused: the server is not ready, runs in source-managed mode, already contains graph data, or lacks capacity. Nothing was modified. |
| 5 | A mutation timed out, lost its connection or returned 503 `unavailable`. Its outcome is uncertain. |

The script never retries. After exit 5 the server may or may not have applied
the mutation (see [Admission and cancellation](../../docs/http-api.md#admission-and-cancellation)).
Restart the server to obtain a known empty graph before running again.

A custom `--dataset` must keep the structure of `grid.json`. Node IDs follow the
HTTP API rules: ASCII decimal strings from `0` to `18446744073709551615`,
compared numerically (`"7"` and `"007"` are the same node). To run the
validation checks from the repository root:

```sh
python3 -m unittest discover -s examples/power-grid -p 'test_*.py'
```

Production integration with an authoritative external source uses source-managed
mode, covered by the [integration contract](../../docs/integration-contract.md)
and the [deployment runbook](../../docs/deployment.md).
