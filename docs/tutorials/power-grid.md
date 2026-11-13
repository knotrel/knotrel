# Tutorial: connectivity loss and restoration in a synthetic power grid

This tutorial uses a small **synthetic** electrical distribution network to show
what Knotrel answers. It opens one connection, sees which customers lose their
path to the source, then closes a reserve connection and sees connectivity
return. Everything runs against the existing HTTP API of a Knotrel v0.3.0
server, driven by a Python 3 standard-library script.

> **Scope.** This is a topology demonstration, not an electrical simulator or
> an operational switching tool. The network is invented for teaching and is
> not a validated grid model.

## The question Knotrel answers

Knotrel keeps an undirected graph that changes over time and answers one exact
question: *is there currently any path between node X and node Y?*

In this tutorial the question is: *is this customer meter **topologically
connected** to the primary substation, or is there **no path to the assumed
source**?* The primary substation is treated as an available source. Generation
failures and multiple supplies are not modelled.

**Connectivity to the primary substation does not prove that a customer is
energized.** Knotrel does not calculate voltage, current, power flow, thermal
limits, protection coordination or switching safety. A topological path is a
necessary condition for supply in this model, not a sufficient one.

## The synthetic network

The dataset in [`examples/power-grid/grid.json`](../../examples/power-grid/grid.json)
has 13 nodes on two feeders from one primary substation:

- **Primary substation** P (`100`): the assumed source.
- **Secondary substations**: A1, A2, A3 on feeder A (`201`–`203`) and B1, B2 on
  feeder B (`204`, `205`).
- **Customer meters** M1–M7 (`301`–`307`).
- **Twelve normally closed connections**: feeder sections and service drops.
- **One normally open reserve connection**: `TIE-A3-B2`, between the ends of
  the two feeders.

```mermaid
flowchart LR
    P[["P · primary substation (100)"]]
    A1["A1 · secondary (201)"]
    A2["A2 · secondary (202)"]
    A3["A3 · secondary (203)"]
    B1["B1 · secondary (204)"]
    B2["B2 · secondary (205)"]
    M1(["M1 · meter (301)"])
    M2(["M2 · meter (302)"])
    M3(["M3 · meter (303)"])
    M4(["M4 · meter (304)"])
    M5(["M5 · meter (305)"])
    M6(["M6 · meter (306)"])
    M7(["M7 · meter (307)"])

    P ---|closed| A1
    A1 -. "A-A1-A2: closed, opened in step 3" .- A2
    A2 ---|closed| A3
    P ---|closed| B1
    B1 ---|closed| B2
    A1 --- M1
    A1 --- M2
    A2 --- M3
    A2 --- M4
    A3 --- M5
    B1 --- M6
    B2 --- M7
    A3 -. "TIE-A3-B2: normally open reserve, closed in step 5" .- B2

    classDef primary fill:#1f4e79,color:#ffffff,stroke:#0b2545
    classDef secondary fill:#d9e8f5,color:#000000,stroke:#1f4e79
    classDef customer fill:#fff4d6,color:#000000,stroke:#b8860b
    class P primary
    class A1,A2,A3,B1,B2 secondary
    class M1,M2,M3,M4,M5,M6,M7 customer
    linkStyle 1 stroke:#c0392b,stroke-width:3px
    linkStyle 12 stroke:#27ae60,stroke-width:3px
```

Legend:
- Double-bordered box: primary substation.
- Rectangles: secondary substations.
- Rounded shapes: customer meters.
- Solid lines: normally closed connections.
- Red dotted line: the connection opened in step 3.
- Green dotted line: the normally open reserve, closed in step 5.

## Mapping the network to Knotrel

| Network concept | Knotrel graph |
| --- | --- |
| Substation or meter | Node with a decimal string ID, such as `"202"` |
| Closed connection | Edge present in the graph |
| Open connection | Edge **absent** from the graph |
| Opening a closed connection | `{"op":"cut","source":"201","target":"202"}` |
| Closing an open connection | `{"op":"link","source":"203","target":"205"}` |
| "Is meter M5 reachable from P?" | `{"op":"connected","source":"305","target":"100"}` |

An open switch is just an absent edge, so the normally open reserve is not sent
during the initial load. Closing it later is an ordinary `link`.

### Why node types stay outside the core

Names such as "Meter M5" and types such as `customer` are **application
metadata**. They live in the dataset and in the script, not in Knotrel. Knotrel
receives only node IDs and the undirected edges that are currently active. The
core has no typed nodes, and connectivity does not depend on them. The
application decides which nodes count as customers, which node is the source,
and what an answer means for its domain.

Finding every affected customer therefore takes **one connectivity query per
customer**. Knotrel has no "affected customers" endpoint. The script sends the
seven queries together in one query-only [batch](../http-api.md#batches), which
still runs seven `connected` operations.

## Run the demonstration

Start a fresh, dedicated standalone server in one terminal:

```sh
KNOTREL_MODE=standalone cargo run --release --locked -p knotrel-server
```

Then run the script from the repository root in another terminal:

```sh
python3 examples/power-grid/power_grid_demo.py
```

Pass `--url` to target another address; the default is
`http://127.0.0.1:8080`. See the
[example README](../../examples/power-grid/README.md) for options and exit codes.

The script runs a fixed sequence:

1. **Preflight and load.** It checks that the server is ready, empty and
   standalone. One batch then adds the 13 nodes and links the 12 closed
   connections.
2. **Query.** It asks whether each meter connects to P. All seven are connected.
3. **Open `A-A1-A2`** with `cut`. This splits feeder A between A1 and A2.
4. **Verify the outage.** A2 and A3 now have no path to P, so exactly meters
   M3, M4 and M5 (`303`, `304`, `305`) have **no path to the assumed source**.
   M1 and M2 still connect through A1, and M6 and M7 through feeder B.
5. **Close `TIE-A3-B2`** with `link`. A3 is now connected to B2 on feeder B.
6. **Verify the restoration.** M3, M4 and M5 are again topologically connected
   to P, through P–B1–B2–A3 (and A3–A2 for M3 and M4). All seven meters are
   connected.

The outage is not limited to the meters next to the opened connection: M5,
two sections downstream, also loses its path. The restoration path runs through
the other feeder, not back through the opened section.

Expected output, abridged:

```text
Loaded 13 nodes and 12 closed connections; reserve TIE-A3-B2 stays open (absent edge).
...
After opening: topological connectivity to Primary substation P (100)
  Meter M1 (301)     connected
  Meter M2 (302)     connected
  Meter M3 (303)     NO PATH to the assumed source
  Meter M4 (304)     NO PATH to the assumed source
  Meter M5 (305)     NO PATH to the assumed source
  Meter M6 (306)     connected
  Meter M7 (307)     connected
  check passed: customers without a path = Meter M3 (303), Meter M4 (304), Meter M5 (305)
...
All checks passed. Topological connectivity only: no voltage, current, power flow, thermal, protection or switching-safety analysis was performed.
```

**Closing the reserve connection is a scripted topology change, not a
recommendation for operating a real electrical network.** Whether a real tie can
be closed safely depends on load, protection and operating rules that are
entirely outside this example.

## How the script handles failure

The script checks every HTTP status, every per-operation result and the
`X-Knotrel-State-Version` header. It exits nonzero on any unexpected result,
instead of printing an apparently successful scenario.

Requests use a bounded timeout. If a mutation times out or its response is
lost, the server may still have applied it (see
[Admission and cancellation](../http-api.md#admission-and-cancellation)). The
script then stops with exit code 5 and does not retry blindly. Restart the server
to return to a known empty graph.

The script never overwrites an existing graph. It refuses to start unless the
server is ready, runs in standalone mode, reports zero nodes and edges, and
reports `state_version` `"0"`. This preflight **assumes exclusive use** of the
server and is not a concurrency guarantee. Another client could change the
graph after the check, which is why the server must be dedicated to this
demonstration. Knotrel has no authentication, so keep the server on loopback
(see the [trust model](../http-api.md#trust-model)).

## Relation to an external source of truth

Here the script plays the application: it holds the topology and sends each
change itself. This uses **standalone mode**, where clients mutate the graph
directly through `/v1/operations` and `/v1/batch`. Standalone mode keeps the
first tutorial self-contained, with no feeder session, import or lease to
manage.

In a real integration, the authoritative topology lives elsewhere, for
example in a network model database or an event stream of switch states.
Knotrel holds only derived, in-memory state: a restart loses the graph and the
state must be rebuilt from that source. For that setting, Knotrel provides
**source-managed mode**, which:

- rejects direct mutations;
- accepts a baseline import followed by an ordered event stream from a feeder;
- reports readiness only after a synchronization barrier is verified.

See:

- [Integration contract](../integration-contract.md): sessions, imports, events,
  barriers and readiness.
- [Deployment runbook](../deployment.md): the example container, the reference
  feeder (an example, not a production connector), restart and rebuild.
- [HTTP API: mode rules](../http-api.md#mode-rules).

## Summary

- Knotrel answers exact "is there any path?" questions on a changing graph.
- Opening a connection is `cut`; closing one is `link`; an open switch is an
  absent edge.
- Domain meaning, such as node types, the source and customers, stays in the
  application.
- A path to the primary substation means *topologically connected*, not
  *energized*. No electrical quantity, limit or safety condition is evaluated.
