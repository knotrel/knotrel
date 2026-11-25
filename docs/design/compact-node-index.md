# Compact internal node indices (option B): design and comparison protocol

**Status: PROPOSED — design and protocol awaiting maintainer approval; nothing implemented or measured.**

Issue: [knotrel/knotrel#151](https://github.com/knotrel/knotrel/issues/151)
(v1.2.0, Performance Hardening). This document covers Step 1 (design) and
Step 2 (comparison protocol) of the issue. Steps 3 and 4 (implementation,
measurement and the adoption decision) have not started.

Under the per-release measurement policy
([#186](https://github.com/knotrel/knotrel/issues/186), recorded in
knotrel-benchmarks `docs/methodology.md`, "Per-release measurement policy"),
the Step 2 protocol is an **impact estimate**. It decides adoption only. If B
is adopted, its areas enter the scope of the v1.2.0 release qualification
([#182](https://github.com/knotrel/knotrel/issues/182)).

Every number in this document is either read from the code (with a
`file:line` citation, paths relative to the repository root) or computed
from those numbers with the arithmetic shown. Memory figures in section 4
are **predictions to be checked by measurement, not results**. No
performance claim in this document is a measured result.

Unit convention: "bytes" is always written out, so that "B" means only
option B.

## Contents

- [1. Summary and open questions for the maintainer](#1-summary-and-open-questions-for-the-maintainer)
- [2. Capacity check](#2-capacity-check)
- [3. Layouts](#3-layouts)
- [4. Expected memory (predictions)](#4-expected-memory-predictions)
- [5. Index lifecycle and cursor resolution](#5-index-lifecycle-and-cursor-resolution)
- [6. Comparison protocol: current vs B](#6-comparison-protocol-current-vs-b)
- [7. Exploratory reference: option A](#7-exploratory-reference-option-a)

## 1. Summary and open questions for the maintainer

**What B changes.** Only the lifecycle catalog of `LifecycleGraph`
(`crates/knotrel-core/src/topology.rs`). Today it is one
`FxHashMap<NodeId, NodeRecord>` whose incident lists store the neighbor's
8-byte external ID plus a flag, 16 bytes per entry. Under B the catalog
becomes:

- an ID → index map `FxHashMap<NodeId, usize>`;
- a dense slot vector `Vec<NodeRecord>` indexed by the internal index, each
  record carrying its own external ID (the index → ID direction);
- incident lists of 8-byte packed entries (internal neighbor index plus the
  `enabled` bit), still sorted by **external** neighbor ID.

The engines (`compact-bfs`, `cluster-forest`, `ett`, `hdt`), the plain
(non-lifecycle) mode, the server, the HTTP API, the cursor format and the
inspection index are unchanged.

**Capacity verdict.** With the packed index stored in a `usize` whose top
bit is the `enabled` flag ("B64" on 64-bit targets), B keeps every limit
enforced today for every engine and mode (section 2.3). A 32-bit packed
index ("B32") would keep the `cluster-forest` lifecycle limit but would
lower the `compact-bfs` lifecycle node limit from unlimited to 2³¹. B32 is
therefore not proposed unless the maintainer takes the contract decision of
question Q1.

**Predicted saving (to be measured).** 16 bytes per registered edge in the
catalog (two incidences of 16 → 8 bytes), plus a node term of
`24·s − 40·σ` bytes per node, which can be positive or negative
(section 4). For the `sparse` workload after bulk import with
`compact-bfs`, the predicted total live-heap ratio B / current is about
0.85 at 10⁴ nodes, 0.88 at 10⁵ and 0.83 at 10⁶ (section 4.4).

**Open contract and protocol questions for the maintainer** (to be answered
before the protocol is frozen):

| # | Question | Proposal |
| --- | --- | --- |
| Q1 | Index width. B64 keeps every limit. B32 saves 8 more bytes per registered edge but lowers the `compact-bfs` lifecycle node limit (unlimited today) to 2³¹ registered nodes, unless a width promotion (32 → 64 bits at 2³¹ slots) is implemented. | Formal candidate: **B64**. B32 only if the maintainer explicitly accepts the lower limit, or approves the promotion design, before the freeze. |
| Q2 | Narrowing the `compact-bfs` engine adjacency (`Vec<Vec<usize>>`) to `u32` is **not** part of B: it would lower the plain `compact-bfs` limit (unlimited today) to 2³² vertices. | Keep it out of this change. A separate contract decision if ever wanted. |
| Q3 | Diagnostics. `LifecycleStorage` (stable, `#[non_exhaustive]`) reports catalog sizes in elements. B could add the slot capacity and the vacant-slot count. That is an additive public API change (snapshot, classification). | Keep `LifecycleStorage` unchanged in the candidate; the protocol uses allocator counts. Add fields later only if wanted. |
| Q4 | Memory materiality thresholds. The CF5 house margin for memory is 20 %; the predicted effect of B is about 12–18 %, so a 20 % margin could never show the benefit. | Benefit: ratio ≤ 0.90 and ≥ 8 bytes per registered edge; harm: ratio ≥ 1.05 (section 6.6). |
| Q5 | Strictness on 10⁶-degree hubs. | Strict (recommended): any SLOWER primary time metric, hubs included, rejects B. Alternative to approve or refuse now: tolerate SLOWER up to 25 % on the 10⁶ hub cells only. |
| Q6 | Graphs with fewer registered edges than nodes (`E_r/V < 1`, for example many isolated nodes). The node term can make B up to 12.6 bytes per node larger after bulk import (section 4.3). | Report-only cell with mandatory disclosure if B is adopted; not a rejection gate. |

No limit that is enforced or documented today is lowered by the proposed
candidate (B64). Q1 and Q2 exist only because the narrower widths the issue
mentions would lower limits.

## 2. Capacity check

### 2.1 Limits enforced today

`GraphLimits` holds two optional `usize` ceilings, `None` meaning unlimited
(`crates/knotrel-core/src/configuration.rs:10-16`). `ConnectivityGraph`
checks them before every `add_node` and `link`
(`configuration.rs:321-337`, `configuration.rs:348-380`). The server default
is 100 000 nodes and 500 000 edges, with `unlimited` accepted
(`crates/knotrel-server/src/configuration.rs:184`, `:186`).

| Engine | Mode | Enforced node limit | Enforced edge limit | Engine index types | Source |
| --- | --- | --- | --- | --- | --- |
| `compact-bfs` | plain | `max_nodes` (default unlimited) | `max_edges` | ID map `IntMap<NodeId, usize>`, adjacency `Vec<Vec<usize>>` | `lib.rs:404-412` |
| `compact-bfs` | lifecycle | `max_nodes` on registered nodes; `bounded` leaves it unchanged | `max_edges` on registered edges | catalog keyed by `NodeId`; engine as above; bulk build uses a `u32` scratch index only when the catalog has fewer than 2³² nodes, otherwise the incremental path | `topology.rs:455-458`, `topology.rs:793-799` |
| `cluster-forest` | plain | `min(max_nodes, 2²⁸)` | `min(max_edges, 2²⁸)` | `u32` handles, `IntMap<NodeId, u32>` | `cluster_forest.rs:53-60`, `:236-237`, `:305-315` |
| `cluster-forest` | lifecycle | `min(max_nodes, (2²⁸ − 1024) / 2)` = `2²⁷ − 512` | `min(max_edges, 2²⁸)` | as plain; catalog keyed by `NodeId` | `topology.rs:446-470`, `configuration.rs:68-79` |
| `ett` | plain | `max_nodes` | `max_edges` | `IntMap<NodeId, usize>`; tree `Index(Option<NonZeroUsize>)` | `dynamic.rs:91-97`, `index.rs:43` |
| `hdt` | plain | `max_nodes` | `max_edges` | `IntMap<NodeId, usize>`, level maps `usize` | `hdt.rs:256-262`, `hdt.rs:94-103` |
| `ett`, `hdt` | lifecycle | not available: `UnsupportedEngine` | — | — | `topology.rs:438-443` |

The `cluster-forest` lifecycle node cap is derived in `bounded`
(`topology.rs:446-470`): engine vertices include up to
`max(catalog, 1024)` retired slots (`MIN_RETIRED_BEFORE_COMPACTION`,
`topology.rs:79`), so `2·V + 1024 ≤ 2²⁸`, giving
`V ≤ (2²⁸ − 1024) / 2 = 134 217 216 = 2²⁷ − 512`.

### 2.2 What B touches

B changes only the catalog of `LifecycleGraph`. The engine inside it is
called with external IDs exactly as today (`topology.rs:1030-1046`), so
engine limits and retired-slot accounting (which `bounded` relies on) are
unchanged. Plain mode has no catalog (`crates/knotrel-server/src/graph.rs:36-42`),
so plain limits are unchanged by construction. `ett` and `hdt` have no
lifecycle support and are untouched.

### 2.3 Proof that B64 keeps every limit

Let the catalog slot vector be `Vec<NodeRecord>` with
`size_of::<NodeRecord>() = 40` bytes under B (section 3.2). Rust guarantees
that an allocation is at most `isize::MAX` bytes, so the vector length is
below `isize::MAX / 40`:

- 64-bit: `(2⁶³ − 1) / 40 ≈ 2.31·10¹⁷ < 2⁵⁸ ≈ 2.88·10¹⁷`, so every slot
  index fits in **58 bits**;
- 32-bit: `(2³¹ − 1) / 40 < 2²⁶`, so every index fits in 26 bits.

The packed incidence stores the index in the low `usize::BITS − 1` bits (63
on 64-bit, 31 on 32-bit) and the `enabled` flag in the top bit. Since every
index that can exist fits in that width with spare bits, **no node count
that can be stored at all is rejected because of the index width**. In
particular:

1. `compact-bfs` lifecycle (unlimited, or any `max_nodes`): unchanged.
2. `cluster-forest` lifecycle (`2²⁷ − 512`): unchanged; that cap still comes
   from the unchanged engine through `bounded`.
3. Edge limits: B introduces no edge index. Registered edge counters stay
   `usize` (`topology.rs:421-424`). The longest possible incident list grows
   from `isize::MAX / 16` to `isize::MAX / 8` entries, so per-node degree
   capacity rises, never falls.
4. Slot count versus node count: with the free-list reuse of section 5, a
   new slot is appended only when no vacant slot exists, so the slot count
   never exceeds the largest number of nodes ever registered at once,
   which `max_nodes` (or the engine cap) already bounds.
5. Plain modes, `ett`, `hdt`: untouched (section 2.2).

### 2.4 Narrower widths (not proposed)

| Variant | Index bits | Lifecycle node limit implied | Keeps today's limits? |
| --- | --- | --- | --- |
| B64 (proposed) | 63 (58 needed) | none below what memory allows | yes, all engines |
| B32 for `cluster-forest` lifecycle only | 31 | 2³¹ slots ≥ `2²⁷ − 512` (needs 27 bits; 4 spare) | yes for `cluster-forest`; needs two catalog types |
| B32 for `compact-bfs` lifecycle | 31 | 2³¹ registered nodes | **no**: lowers "unlimited" (Q1) |
| B32 with promotion to 64 bits at 2³¹ slots | 31 then 63 | none | yes, at the cost of a one-time `O(V + E_r)` rewrite with old and new incidences live (peak +8 bytes per incidence during the rewrite) and two code paths |
| `u32` engine adjacency in `compact-bfs` | 32 | 2³² engine vertices, plain and lifecycle | **no** (Q2) |

Being hard to reach is not a reason to accept a lower limit (issue #151,
"Capacity"). Hence B64 is the formal candidate.

## 3. Layouts

Sizes are for 64-bit targets. Rust's default layout may reorder fields, so
field order is not guaranteed; the sizes below follow from field sizes and
8-byte alignment, and the implementation must pin them with
`std::mem::size_of` assertions under `#[cfg(target_pointer_width = "64")]`,
as the code already does for `NodeRecord` (`topology.rs:1554-1558`).

Hash-table cost: Rust's `std::collections::HashMap` (hashbrown) allocates
`b` buckets of `size_of::<(K, V)>()` bytes plus `b + W` control bytes, where
`b` is a power of two with at most `7b/8` entries (for `b ≥ 8`) and
`W = Group::WIDTH` (16 on x86-64 with SSE2, 8 on aarch64). Write
`T(n, e) = b(n)·(e + 1) + W` for a table of `n` entries of `e` bytes, and
`s = b(n) / n` (the bucket slack, between 8/7 just before a growth and about
16/7 just after one). These are properties of the current standard library
implementation, not a language guarantee; the protocol measures actual
allocations.

### 3.1 `Incidence`

Current (`topology.rs:305-310`):

| Field | Type | Bytes |
| --- | --- | --- |
| `neighbor` | `NodeId = u64` (`lib.rs:315`) | 8 |
| `enabled` | `bool` | 1 |
| padding | — | 7 |
| **total** | | **16** |

Free bits: 7 bits of `enabled` plus 56 bits of padding. Two incidences per
registered edge (one per endpoint, `topology.rs:1011-1020`): **32 bytes per
registered edge**.

Proposed (B64): `struct Incidence(usize)`, index in bits 0–62, `enabled` in
bit 63.

| Bits | Content |
| --- | --- |
| 0–57 | slot index (58 bits always suffice, section 2.3) |
| 58–62 | free (5 bits) |
| 63 | `enabled` |
| **total** | **8 bytes** |

Two per registered edge: **16 bytes per registered edge**. Assertion to add:
`size_of::<Incidence>() == 8`.

(B32 for reference: `struct Incidence(u32)`, 31 index bits, 4 bytes, 8 bytes
per registered edge; see Q1.)

### 3.2 `NodeRecord` and the catalog map entry

Current (`topology.rs:312-324`, size asserted at `topology.rs:1554-1558`):

| Field | Type | Bytes |
| --- | --- | --- |
| `incident` | `Vec<Incidence>` (pointer, capacity, length) | 24 |
| `pending` | `u32` (construction scratch) | 4 |
| `active` | `bool` | 1 |
| padding | — | 3 |
| **total** | | **32** |

Free bits: 7 bits of `active` and 3 bytes of padding. The catalog is
`FxHashMap<NodeId, NodeRecord>` (`topology.rs:411-414`), entry
`(u64, NodeRecord)` = 8 + 32 = **40 bytes**, plus one control byte per
bucket. Catalog table: `T(V, 40)`.

Proposed: the catalog splits into an ID map and a slot vector.

ID map `FxHashMap<NodeId, usize>`: entry `(u64, usize)` = **16 bytes** plus
one control byte; table `T(V, 16)`. (A `u32` value would not help: `(u64,
u32)` pads to 16 bytes.) It stays `FxHashMap`, not the identity-hashed
`IntMap`, for the reason recorded at `topology.rs:411-413`.

Slot record (`Vec<NodeRecord>`, indexed by slot):

| Field | Type | Bytes |
| --- | --- | --- |
| `id` | `NodeId` (external ID of an occupied slot; next free slot of a vacant one) | 8 |
| `incident` | `Vec<Incidence>` | 24 |
| `pending` | `u32` | 4 |
| `state` | `u8`: vacant, inactive or active | 1 |
| padding | — | 3 |
| **total** | | **40** |

Free bits: 6 bits of `state` and 3 bytes of padding. Assertions to add:
`size_of::<NodeRecord>() == 40` and `size_of::<(NodeId, usize)>() == 16`.

Per node, before slack: current 40 bytes in the map; B 16 bytes in the map
plus 40 in the slot vector, 56 bytes. With slack the comparison is
`41·s` versus `17·s + 40·σ` bytes per node, where `σ` is the slot vector's
capacity over its length (section 4).

**Alternative considered:** keep a 32-byte record in the map and add a
separate `Vec<NodeId>` for index → ID. It costs the same 8 bytes per slot,
but every traversal step would then touch two random locations (the ID
vector and a hash probe for the `active` flag) instead of one slot record
holding both. Rejected.

### 3.3 Engine adjacency and engine ID map (`compact-bfs`)

`Graph` (`lib.rs:404-412`), unchanged by B:

| Structure | Element | Bytes |
| --- | --- | --- |
| `ids: IntMap<NodeId, usize>` | `(u64, usize)` + 1 control byte per bucket | 16 + 1 |
| `adjacency: Vec<Vec<usize>>`, outer | `Vec` header per engine vertex | 24 |
| `adjacency`, inner | one `usize` per effective incidence | 8 (16 per effective edge) |
| `nodes: Option<Vec<NodeId>>` | only after `enable_neighbor_listing` | 8 per vertex |

Engine vertices include retired slots, at most `V + max(V, 1024)`
(`topology.rs:36-48`). The engine index space is separate from the catalog
slot space under B; making them one would remove the engine's ID map in
lifecycle mode but would cross the engine abstraction, so it is out of
scope here. Narrowing the inner `usize` to `u32` is excluded (Q2).

The `cluster-forest` engine already uses `u32` handles
(`cluster_forest.rs:236-252`) and is unchanged.

### 3.4 ID ↔ index mapping and the inspection index

| Structure | Where | When present | Changed by B? |
| --- | --- | --- | --- |
| Catalog ID → index | `FxHashMap<NodeId, usize>` (new) | lifecycle, always | new (replaces the ID-keyed catalog map) |
| Catalog index → ID | `id` field of each slot record (new) | lifecycle, always | new, 8 bytes per slot |
| Engine ID → index | `Graph::ids` (`lib.rs:405`) | `compact-bfs`, always | no |
| Engine index → ID | `Graph::nodes` (`lib.rs:406-409`) | plain `compact-bfs` with inspection only (`graph.rs:76-93`) | no |
| Inspection node index | `BTreeSet<u64>` (`graph.rs:27-33`) | server with `KNOTREL_INSPECTION`, both modes | no |

The inspection `BTreeSet<u64>` holds external IDs in ascending order and
serves `/v1/nodes` and the source order of `/v1/edges`
(`graph.rs:278`, `graph.rs:400`). It cannot replace the catalog ID map
(it exists only with inspection and costs `O(log V)` per lookup) and B does
not need it. In lifecycle mode the engine's `nodes` vector is never
allocated (`graph.rs:83-93` enables it only for `Topology::Plain`), so B's
index → ID field is the only index → ID structure of a lifecycle graph; it
is mandatory because every comparison in a sorted incident list needs the
neighbor's external ID, with or without inspection.

### 3.5 Out of scope (listed for completeness)

ETT and HDT keep a 64-byte AVL tour `Token<C>` (`index.rs:77-94`, size
asserted at `index.rs:490` and `hdt_forest.rs:122`). B does not touch it.

## 4. Expected memory (predictions)

**These formulas are predictions to verify by measurement, not results.**

### 4.1 Notation

- `V` registered nodes; `R` retired engine slots, `0 ≤ R ≤ max(V, 1024)`
  after every public call (`topology.rs:1238-1250`); `F` vacant catalog
  slots under B, `0 ≤ F ≤ max(V, 1024)` (section 5.4).
- `E_r` registered, `E_n` enabled, `E_f` effective edges
  (`E_f ≤ E_n ≤ E_r`).
- `T(n, e)` and `s` as in section 3; `s_c` for the catalog (or ID map),
  `s_e` for the engine ID map.
- `κ_c` = incident capacity / incident length in the catalog (1 after bulk
  import of duplicate-free input, `topology.rs:752`; up to about 2 under
  insertion growth; up to 4 after removals, since a vector is shrunk only
  when its length falls below a quarter of its capacity and the capacity
  exceeds 16, `topology.rs:341-349`).
- `κ_e` = engine adjacency capacity / length (capacity is retained,
  `lib.rs:395-403`).
- `σ` = slot vector capacity / length under B (1 if reserved exactly).

### 4.2 Steady state, lifecycle

Engine term, unchanged by B. For `compact-bfs`:

`M_eng = T(V + R, 16) + 24·(V + R) + 16·κ_e·E_f`

For `cluster-forest` the engine term is that engine's own storage, also
unchanged; this document does not size it.

Catalog, current:

`M_cat,cur = T(V, 40) + 32·κ_c·E_r`

Catalog, B64:

`M_cat,B = T(V, 16) + 40·σ·(V + F) + 16·κ_c·E_r`

(The free list is intrusive, section 5.2, so it adds only a head and a count.)

Difference, current minus B, with `T(V, 40) − T(V, 16) = 24·b(V) = 24·s_c·V`:

`Δ = (24·s_c − 40·σ·(1 + F/V))·V + 16·κ_c·E_r`

The edge term is always a saving. The node term ranges from about
`24·(16/7) − 40 = +14.9` bytes per node (`σ = 1`, `F = 0`, table just
grown) to `24·(8/7) − 80 = −52.6` bytes per node (`σ = 2`, table just
before growth), and lower while `F > 0`.

Mitigation proposed for the implementation (and checked by the protocol):
grow the slot vector in step with the ID map, reserving it to the map's
capacity `7·b/8`, so that `σ = (7/8)·s_c`. The node term becomes
`24·s_c − 35·s_c = −11·s_c`, between −12.6 and −25.1 bytes per node, and B
is then never larger at steady state with `F = 0` once
`16·κ_c·E_r ≥ 25.1·V`, that is for `E_r/V ≥ 1.57` with `κ_c = 1`.

B32 (for reference only): the edge term becomes `24·κ_c·E_r`.

### 4.3 After bulk import (`F = 0`, `σ = 1`, `κ_c = 1`, `R = 0`)

`Δ = (24·s_c − 40)·V + 16·E_r`

B is smaller whenever `16·E_r > (40 − 24·s_c)·V`. With the worst slack
`s_c = 8/7`, `40 − 27.43 = 12.57`, so B is smaller for every
`E_r/V ≥ 0.79`. Below that (many isolated nodes), B can be larger by up to
12.6 bytes per node (Q6).

### 4.4 Worked predictions (bulk import, `compact-bfs` lifecycle)

Assumptions: all nodes active, all edges enabled, duplicate-free input,
`E_r = E_f = 2V` (the `sparse` family, about 4 incidences per node; the
generator's deduplication makes `E_r` slightly smaller), `κ_e = 1`,
`s_c = s_e = s`, control-byte tail `W` and constant fields ignored.

Buckets for `V` entries: smallest power of two `b` with `7b/8 ≥ V`:

| `V` | `V·8/7` | `b` | `s` |
| --- | --- | --- | --- |
| 10⁴ | 11 428.6 | 16 384 | 1.6384 |
| 10⁵ | 114 285.7 | 131 072 | 1.3107 |
| 10⁶ | 1 142 857.1 | 2 097 152 | 2.0972 |

Per node (bytes), with `2E_r/V = 4` incidences per node:

- current catalog: `41·s + 16·4`; engine: `17·s + 24 + 8·4`;
- B catalog: `17·s + 40 + 8·4`; engine: unchanged.

| `V` | current catalog | engine | current total | B catalog | B total | Δ per node | Δ per registered edge | ratio B / current |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 10⁴ | 67.17 + 64 = 131.17 | 27.85 + 56 = 83.85 | 215.03 | 27.85 + 72 = 99.85 | 183.71 | 31.32 | 15.66 | 0.854 |
| 10⁵ | 53.74 + 64 = 117.74 | 22.28 + 56 = 78.28 | 196.02 | 22.28 + 72 = 94.28 | 172.57 | 23.45 | 11.73 | 0.880 |
| 10⁶ | 85.98 + 64 = 149.98 | 35.65 + 56 = 91.65 | 241.63 | 35.65 + 72 = 107.65 | 199.30 | 42.33 | 21.17 | 0.825 |

Check with the formula of section 4.3: at 10⁴, `24·1.6384 − 40 + 32 =
31.32`. A star of degree `D` (`V = D + 1`, `E_r = 2D − 1`) has the same
incidences per node to within `1/D` and therefore the same predictions. A
hub of degree `D` inside a 10⁶-node `sparse` graph has
`2E_r/V = 4 + 2D/10⁶` incidences per node; at `D = 10⁶` that is 6, and the
predicted Δ is `50.33 − 40 + 48 = 58.33` bytes per node.

With `cluster-forest` the catalog terms are identical; the engine term is
larger, so the same absolute saving gives a ratio closer to 1. The protocol
therefore states the benefit criterion in bytes per registered edge as well
as in ratio (section 6.6).

### 4.5 Peaks (predictions)

Input buffers (`NodeSpec` and `EdgeSpec`, `topology.rs:92-107`, and the
server's wire vectors and inspection `BTreeSet`, `service.rs:396-480`) are
identical in both arms and are left out of the differences below.

**Bulk import** (`try_from_topology`, `catalog_fast` path). Current: the
peak is inside `build_engine` (`topology.rs:793-856`) while the engine ID
map is built by `from_sorted_adjacency` (`lib.rs:582-611`; `records` is
dropped just before, `topology.rs:850`):

`P_imp,cur = T(V, 40) + 32·E_r + 8V (order) + 8V (ids) + 24V + 8·A + T(V, 16)`

where `A` is the sum of enabled registered degrees of active nodes (the
reserved adjacency capacity, `topology.rs:816-825`). Earlier, during the
edge loop, the 8-byte-per-node `records` vector is live instead of the
engine ID map; since `T(V, 16) ≥ 17·(8/7)·V > 8V`, the later point is the
higher.

B: the slot vector is indexable, so `records` is unnecessary; if slots are
assigned in ascending ID order at import (section 5.1), the sorted ID
vector also gives the engine indices and `pending` is not needed for them:

`P_imp,B = T(V, 16) + 40·σ·V + 16·E_r + k·8V + 24V + 8·A + T(V, 16)`

with `k ∈ {1, 2}` scratch vectors (sorted IDs, and possibly an
old → new slot map), depending on the implementation. A hash-table growth
during import holds the old and the new table at once in both arms; the
largest such transient is `T(V/2, 40)` extra today and `T(V/2, 16)` under B.
A slot-vector growth without exact reservation can briefly hold
`40·L + 80·L` bytes for `L` slots; reserving exactly avoids it.

**Rebuild** (import replace or create while a graph is published): the
candidate is built off-lock while the old graph stays published
(`service.rs:2538` and following), so

`P_rebuild = M_old + P_imp(candidate)`

in both arms; the difference is `Δ(M_old) + Δ(P_imp)`.

**Engine compaction** (`compact_if_needed`, `topology.rs:1238-1250`): the
new engine is built while the old one is live. Current, `compact-bfs`:

`P_cmp,cur = M_cat,cur + M_eng,old + 8V (ids) + 8V (records) + 24V + 16·E_f + T(V, 16)`

B: the same without `records` (`−8V`). If a catalog slot compaction runs in
the same call (section 5.4), add its scratch: an old → new slot map of
`8·(V + F)` bytes and, for ID-ordered renumbering, a sorted permutation of
`8·V` bytes; the slot vector is compacted in place. For `cluster-forest`,
`build_engine_incremental` (`topology.rs:863-885`) holds a key vector `8V`
and an effective-edge list `16·E_f` in both arms.

**Catalog table rebuild** on tombstones (`reserve_catalog_slot`,
`topology.rs:951-965`): old and new tables live together, `+T(V, 40)` today
and `+T(V, 16)` under B (the ID map has the same tombstone behaviour).

## 5. Index lifecycle and cursor resolution

### 5.1 Assignment

- `add_node`, and `link` creating an endpoint: pop a vacant slot from the
  free list if any, otherwise push a new slot; write `id`, `state = active`
  and an empty `incident`; insert `id → slot` into the ID map. Expected
  `O(1)`, amortized for growth.
- Bulk import (`try_from_topology`) and slot compaction: assign slots in
  **ascending external ID** order, so that right after them the slot order
  equals the ID order. Then sorting an incident list by slot sorts it by ID,
  and the binary searches of section 5.5 touch slot records in increasing
  address order. Correctness never depends on this property, which later
  insertions break.

### 5.2 Freeing

`remove_node(u)` with `u` at slot `i`:

1. For every incidence of `u` (pointing to slot `j`), remove the reverse
   incidence (pointing to `i`) from slot `j`'s list, as today
   (`topology.rs:1203-1235`). Locating it is a binary search by `u`'s
   external ID in `j`'s list.
2. Remove `u` from the ID map.
3. Mark slot `i` vacant, drop its incident vector, store the current free
   list head in its `id` field and make `i` the head (an intrusive LIFO free
   list; no extra memory).
4. Engine work and engine compaction exactly as today.

`cut` removes two incidences and frees no slot.

### 5.3 Reuse

A vacant slot is reused by the next node registration (section 5.1), with
a new ID. Safety is the invariant of section 5.6.

### 5.4 Remap on compaction

When vacant slots exceed `max(V, 1024)` (the same rule as engine compaction,
`topology.rs:1238-1250`), `remove_node` compacts the slot vector: it builds
the bijection from occupied old slots to dense new slots (in ascending ID
order), moves the records, rewrites every incidence and every ID-map value
through that bijection, then shrinks the vector. Cost `O(V log V + E_r)`
(an ID sort plus one pass over all incidences). More than `max(V, 1024)`
removals separate two slot compactions, so the amortized cost is
`O(log V + E_r / V)` per removal, the same form as the engine compaction
bound documented at `topology.rs:41-47`. With LIFO reuse, fresh-ID churn
(remove one node, add another) keeps `F` near zero, so slot compaction runs
only after a net decrease of the node count.

### 5.5 Cursor resolution

An incidence cursor is an external `u64` ID `c`; nothing about indices
reaches the API, the server or the cursor (`graph.rs:311-353`,
`graph.rs:400-480`). `incident_after(node, Some(c))` today computes
`partition_point(|i| i.neighbor <= c)` (`topology.rs:1317-1338`). Under B:

`start = partition_point(|i| slots[i.index()].id <= c)`

a lower bound by external ID with index → ID translation at each of the
`⌈log₂ d⌉` probes. Each emitted edge reads `slots[i.index()]` once, which
yields the neighbor's ID and its `active` state together (today: the ID is
inline and `active` costs one hash probe). `/v1/edges` keeps its order: the
source order comes from the inspection `BTreeSet` and the incidence order
is still ascending neighbor ID.

Access counts per operation (a cost model, **not** a timing prediction):

| Operation | Current | B |
| --- | --- | --- |
| Locate a neighbor in a list of degree `d` | `⌈log₂ d⌉` probes in a contiguous 16-byte array | `⌈log₂ d⌉` probes in a contiguous 8-byte array, each followed by one slot read |
| Endpoint lookup (`link`, `cut`, `set_edge_enabled`) | one hash probe gives the record | one hash probe gives the slot, one more read gives the record |
| One step of `incident_after` | one hash probe (neighbor `active`) | one slot read (ID and state) |
| `set_node_active` | `d` hash probes | `d` slot reads |
| `remove_node` | `d` hash probes + `d` binary searches | `d` slot reads + `d` translated binary searches |
| Insert or remove in a sorted list | shifts 16-byte entries | shifts 8-byte entries |

Whether the translated probes cost more than they save on hubs is exactly
what the protocol measures; no outcome is assumed.

### 5.6 Why removals and compactions cannot invalidate a cursor, and why a stored index never points to a recycled slot

Invariants under B, added to the four of `topology.rs:19-28` and restored
before every public method returns:

- **I5 (occupied targets).** Every stored incidence index names an occupied
  slot, and that slot's `id` is the neighbor's external ID.
- **I6 (map).** `map[x] = i` iff slot `i` is occupied with `id = x`.
- **I7 (closure).** Catalog slot indices are stored only in incidences, in
  ID-map values and in the free list (which names only vacant slots). No
  index is returned to callers, stored by the engine, the server or a
  cursor, or kept across calls: the iterator of `incident_after` borrows the
  graph, so no mutation can happen while it lives.
- **Invariant 1, restated.** Each incident list is strictly sorted by the
  external ID of its targets; symmetry holds on external IDs.

*A stored index never points to a recycled slot.* A slot becomes vacant only
in `remove_node`, after step 1 of section 5.2 removed every incidence
pointing to it; by symmetry (invariant 1) those are all incidences naming
`u`, so after the removal no incidence names slot `i` (I5 holds) and the ID
map no longer maps `u` (I6). By I7 there is no other place an index to `i`
could survive. When `i` is reused for a node `w`, no stored index names it,
so no edge can be misattributed to `w`. Compaction rewrites every stored
index (all of them are in the places I7 lists) through one bijection inside
one `&mut self` call, and vacant slots stop existing. A panic in the middle
leaves the graph unspecified, as today (`topology.rs:379-380`).

*Removal cannot invalidate a cursor.* Resolution needs only that the list is
sorted by `id` of its targets and that every target is occupied (I5 and
invariant 1). It never looks up `c` itself, so `c` need not be registered;
after `c` is removed, its incidences are gone, the rest of the list keeps
its order, and the lower bound lands on the first neighbor with an ID above
`c`, exactly as today. Removing other nodes deletes entries without
reordering the remaining ones.

*Compaction or rebuild cannot invalidate a cursor.* Remapping replaces each
entry's index by the index of the same node, in place, so every entry keeps
its external ID and its position. The lower bound by external ID therefore
returns the same position before and after. An import replace builds a new
graph whose lists are sorted by ID too; a cursor is resolved against it by
ID, with the generation headers signalling the change as today
(`docs/http-api.md`, "Order and cursors" and "Consistency").

Hence the documented guarantees hold unchanged: a cursor is a position, it
never expires, it stays valid after the node it names is removed, and every
edge registered for the whole traversal is visited exactly once.

## 6. Comparison protocol: current vs B

**Status: PROPOSED.** Criteria, thresholds, run counts, order and the
decision rule are written here before any measurement; they become binding
only when the maintainer approves them and the matrix is frozen. No result
is assumed: not the memory saving, and not the cost of the extra
index → ID reads on hubs.

### 6.1 Classification and arms

- **Kind:** impact estimate (#186). Scope: the cells B can affect (memory,
  insertions, removals, pagination, churn on lifecycle graphs). It decides
  adoption only and qualifies nothing.
- **Reference (CUR):** the base commit, the `main` commit the candidate
  branch starts from, its SHA written into the campaign record before the
  first observation.
- **Candidate (B):** the base commit plus the B implementation only. The
  record lists the diff between them; any change outside
  `crates/knotrel-core/src/topology.rs` (and its tests and docs) is listed
  and justified, and if the diff touches `lib.rs`, `configuration.rs`, an
  engine module or the server, the conditional cells of section 6.3 are
  added before the freeze.
- **Builds:** toolchain 1.100.0, release profile, `--locked`. Each probe is
  built twice from the same probe source, once against each checkout, the
  method of `release-cells` in knotrel-benchmarks
  `tools/lifecycle-probes/src/bin/release-cells.rs`. Executable SHA-256 and
  lockfile hashes are recorded.
- **Host:** one machine, declared as in previous qualifications (CPU model,
  cores, frequency policy, memory, OS and kernel), nothing else running.
  The campaign runs serially with the other v1.2.0 impact estimates on the
  shared host (#186), with cumulative swap counters recorded before the
  first and after the last observation (methodology, "Host swap activity").

### 6.2 Engines and modes in scope

- In scope: `LifecycleGraph` with `compact-bfs` (CB-L) and `cluster-forest`
  (CF-L), the only lifecycle engines (`topology.rs:438-443`).
- Out of scope, with the diff as evidence: plain mode of every engine, and
  `ett` and `hdt` (no lifecycle support). The freeze record lists them as
  "no code change in the candidate".

### 6.3 Workloads

Existing generators are reused (knotrel-benchmarks
`tools/lifecycle-probes`):

| Name | Definition | Source | Sizes |
| --- | --- | --- | --- |
| `sparse` (ordinary nodes) | `2n` random edges, average degree about 4 | `tools/lifecycle-probes/src/lib.rs` `Topology::Sparse` | 10⁴, 10⁵, 10⁶ nodes |
| `ring` (low degree) | ring plus one chord every 64 nodes, about 2 incidences per node | `Topology::Ring` | 10⁶ |
| `hub` (existing mixed hubs) | ring plus `max(1, n/2000)` hubs of degree `min(2000, n − 1)` | `Topology::Hub` | 10⁵, 10⁶ |
| `star-D` (hub alone) | star of degree `D` plus a path among the leaves | `core-lifecycle` `star` (`degree`, `degree-remove` experiments) | `D` = 10⁴, 10⁵, 10⁶ |
| `hub-in-sparse-D` (new, to add before the freeze) | `sparse` with 10⁶ nodes plus one hub linked to `D` distinct random nodes | new generator, seeded | `D` = 10⁴, 10⁵, 10⁶ |
| `iso` (report-only, Q6) | 10⁶ registered nodes, no edges | new | 10⁶ |

Hub degrees are fixed at **10⁴, 10⁵ and 10⁶**. No code limit prevents
them: the core lifecycle graph is unlimited by default, and `cluster-forest`
lifecycle admits `2²⁷ − 512` nodes and `2²⁸` edges (section 2.1). Server
cells set `KNOTREL_MAX_NODES=unlimited` and `KNOTREL_MAX_EDGES=unlimited`,
since the defaults (100 000 / 500 000) are below a 10⁶ hub.

**Slot-order states.** Because B's translated reads depend on where slots
lie, hub cells run in two states: *ordered* (built by `try_from_topology`,
slot order = ID order) and *scrambled* (nodes registered incrementally in a
seeded random ID order, so slot order and ID order are unrelated). The
current layout has no slot order; both arms run the same two input
sequences.

**Conditional cells** (only if the candidate diff touches plain-mode code,
section 6.1): `hub-alternatives-v1` and `chain-split-rejoin-v1` at 10⁵ nodes
through the knotrel-benchmarks harness (`connectivity-compact-bfs`,
`connectivity-cluster-forest`), and plain `compact-bfs` incident pagination
on `star-10⁵`, classified by the time rules below.

### 6.4 Cells

Counts are classified comparisons CUR vs B.

**M — memory (deterministic).** Live heap and exact high-water mark from the
counting allocator of `tools/lifecycle-probes` (`count-alloc` and
`peak-alloc` builds, `memory-peak`), plus `LifecycleStorage` element counts
(identical counts are a correctness gate; capacities are reported). One run
per arm and cell; the smoke runs each twice and records whether the counts
repeat.

| Cell | Scenario | Graphs | Engines | Metrics | Comparisons |
| --- | --- | --- | --- | --- | --- |
| M1 | bulk import (`try_from_topology`) | `sparse` ×3, `ring`, `star-D` ×3, `hub-in-sparse-D` ×3 | CB-L, CF-L | live after build; high-water during build | 20 × 2 |
| M1-iso | bulk import | `iso` | CB-L, CF-L | as M1, **report-only** (Q6) | 2 × 2 |
| M2 | incremental build in seeded random edge order (`link`) | `sparse-10⁶`, `hub-in-sparse-10⁶` (hub spokes appended in ascending leaf ID) | CB-L, CF-L | live after build; high-water | 4 × 2 |
| M3 | import replace with the old graph published (`memory-peak import-http --mode replace`) | `sparse-10⁶`, `hub-in-sparse-10⁶` | CB-L, CF-L | high-water; live after swap | 4 × 2 |
| M4 | churn with compactions (`memory-peak churn`, the existing schedule) | `sparse-10⁵`, `hub-10⁵` × reuse fresh / same / toggle | CB-L, CF-L | live at end; high-water | 12 × 2 |
| M5 | mass removal: remove 60 % of nodes (seeded), then register as many fresh IDs | `sparse-10⁶` | CB-L, CF-L | live after removal; high-water; live at end | 2 × 3 |
| M6 | RSS (`core-lifecycle resources`), **supporting** | M1 10⁶ graphs | CB-L, CF-L | RSS after build | 6, report-only |

**I — insertions (timed).**

| Cell | Scenario | Graphs | Engines | Primary metric | Comparisons |
| --- | --- | --- | --- | --- | --- |
| I1 | incremental build, seeded random edge order | `sparse-10⁵`, `sparse-10⁶` | CB-L, CF-L | total build time | 4 |
| I2 | window of `k` new spokes linked to a hub of degree `D` at random positions (leaves pre-registered; untimed restore) | `hub-in-sparse-D` ×3, scrambled | CB-L, CF-L | window time | 6 |
| I3 | `set_edge_enabled(true)` on `k` disabled edges | `sparse-10⁶`; hub spokes of `hub-in-sparse-D` ×3, scrambled | CB-L, CF-L | window time | 8 |

**R — removals (timed).**

| Cell | Scenario | Graphs | Engines | Primary metric | Comparisons |
| --- | --- | --- | --- | --- | --- |
| R1 | `cut` of `k` random edges | `sparse-10⁵`, `sparse-10⁶` | CB-L, CF-L | window time | 4 |
| R2 | `cut` of `k` random hub spokes | `hub-in-sparse-D` ×3, scrambled | CB-L, CF-L | window time | 6 |
| R3 | `set_edge_enabled(false)` on `k` edges | as I3 | CB-L, CF-L | window time | 8 |
| R4 | hub node removal (existing `degree-remove`: remove the center, re-register, re-link) | `star-D` ×3 | CB-L, CF-L | per-cycle time | 6 |
| R5 | `remove_node` of `k` random ordinary nodes (untimed re-add) | `sparse-10⁶` | CB-L, CF-L | window time | 2 |

**P — pagination (timed).** The catalog code is identical for both engines,
so P runs on CB-L, with CF-L at `D = 10⁶` only as a check.

| Cell | Scenario | Graphs and states | Engines | Primary; tail | Comparisons |
| --- | --- | --- | --- | --- | --- |
| P1 | core `incident_after`, full traversal of a hub in pages of 1000 | `hub-in-sparse-D` ×3 × ordered / scrambled | CB-L (+ CF-L at 10⁶) | traversal time | 6 + 2 |
| P2 | core single page (limit 1000) at 300 seeded cursors: one third registered neighbors, one third **cursors naming removed nodes** (registered, then removed before paging), one third never-registered IDs | as P1 | CB-L (+ CF-L at 10⁶) | median page time; p99 page time | 6 + 2 (× 2 metrics) |
| P3 | HTTP `GET /v1/nodes/{hub}/edges?limit=1000`, 300 pages at seeded cursors (same three kinds), server with lifecycle and inspection, graph loaded with `POST /v1/batch` (scrambled) | `hub-in-sparse-D` ×3 | CB-L (+ CF-L at 10⁶) | client median; client p99 | 3 + 1 (× 2 metrics) |
| P4 | HTTP `GET /v1/edges?limit=1000`: full traversal; and 300 pages at seeded cursors including `next_after_source` / `next_after_target` naming removed nodes | `hub-in-sparse-10⁶` | CB-L, CF-L | traversal time; page p99 | 2 (× 2 metrics) |

The HTTP cells reuse the method of `scripts/measure-inspection-costs.py`
(persistent HTTP/1.1, client-side latency). Every P observation records the
digest of every page body; CUR and B must produce identical digests for the
same seed (correctness gate, section 6.5).

**C — churn (timed and memory over time).**

| Cell | Scenario | Graphs | Engines | Primary; tail | Comparisons |
| --- | --- | --- | --- | --- | --- |
| C1 | existing `release-cells churn` schedule (5 % churn, 30 cycles) | `sparse-10⁵`, `hub-10⁵` × fresh / same / toggle | CB-L, CF-L | total time (`observation_ns`); slowest single operation | 12 (× 2 metrics) |
| C2 | mass-removal churn of M5, timed | `sparse-10⁶` | CB-L, CF-L | total time; slowest operation | 2 (× 2 metrics) |
| C3 | end-to-end: `http-lifecycle` `ops` scenario | `sparse-10⁴`, `hub-10⁴` | CB-L, CF-L | `requests_per_s` | 4 |

Memory over time for churn comes from M4 and M5 (deterministic). Per-cycle
RSS of C1 is reported, not classified.

**Q — inspection cost check (absolute).** Because B changes the cost of the
lifecycle incidence listing, `AGENTS.md` requires a new measurement for
`docs/console-inspection-costs.md` against criteria declared beforehand. The
B arm reruns that check unchanged (500 000 nodes, 1 500 000 edges, one
20 000-neighbor hub) with its criteria C1–C4 as published there; CUR runs
it as reference. Its outcome is reported with the campaign and, if B is
adopted, written into that page.

**Totals (classified):** M 42 cells (86 metric comparisons, M1–M5),
I 18, R 26, P 22 cells (36 metric comparisons), C 18 cells (32 metric
comparisons), plus Q (absolute) and the report-only M1-iso and M6.

### 6.5 Method

**Timed cells** (I, R, P, C) use the paired method of the v1.0.0 and CF5
plans ([CF5 plan, section 5](../releases/v1.1.0-cf5-qualification-plan.md#5-method)):

- one observation of each arm per pair, each in a fresh process (a fresh
  server for P3, P4, C3); the order within a pair balanced 12/12 by a frozen
  seeded schedule; pairs interleaved across the cells of a block;
- **n = 24 pairs per cell**, 2 warmup pairs discarded, no interim analysis,
  no extension; the analyzer runs once, after all pairs;
- interval: `[r(7), r(18)]` of the 24 sorted paired log ratios
  `ln(B / CUR)` (throughput: `ln(CUR / B)`), distribution-free coverage
  97.73 % ([CF5 plan, section 6](../releases/v1.1.0-cf5-qualification-plan.md#6-confidence-level-rationale));
- windows (`k`): set from a pilot run (2 pairs, no warmup, never used for
  decisions) so that the faster arm's window is at least 2 ms, as powers of
  two up to 64 times the base window, the rule of
  `run_lifecycle_qualification.py` `apply_multipliers`; the windows are then
  frozen in the matrix before the first measured pair;
- TIMER_LIMITED: the rule of
  [CF5 plan, section 6.1](../releases/v1.1.0-cf5-qualification-plan.md#61-measurement-limit-timer_limited),
  with the host's clock calibration recorded with the campaign (re-run if
  the host or toolchain differs from the CF5 calibration).

**Deterministic cells** (M): one run per arm, ratio `q = B / CUR` on each
metric; UNDEFINED when the reference value is not positive.

**Correctness gate.** Every observation checks its answers against the
workload's independent oracle where one exists (`lifecycle-probes` model
counts, `connected` on sampled pairs). For every cell, the two arms must
report the same input fingerprint, the same logical counts, the same
`LifecycleStorage` entry counts (`catalog_nodes`, `incident_entries`,
`engine_vertices`, `retired_vertices`) and, for P cells, the same page
digests; HTTP cells require `errors == 0`. Before any measured pair, a
tier-1 smoke runs every distinct arm and cell once (deterministic cells
twice). A failed check, a non-zero exit or unparsable output is a **FAIL**:
the campaign stops, the defect goes back to implementation, and a new
candidate restarts the campaign from scratch. A timeout or a missing
executable is an infrastructure failure: retried once, then the campaign
stops.

**Recorded before the first observation:** the matrix file (every cell's
ID, parameters, seeds, arms, metrics, margins and rules) and its hash, the
checkout SHAs (base, candidate, knotrel-benchmarks), lockfile and
executable hashes, the host description, and the order seed, derived from
the matrix hash and the candidate SHA so it cannot be chosen. Proposed
location: knotrel-benchmarks `results/v1.2.0-151/` with a `FREEZE.json`, as
for CF5.

### 6.6 Classification and thresholds

Timed cells, interval `[lo, hi]` of `ln(B / CUR)`, margins as in CF5: time
`m = 10 %`, tail (p99, slowest operation) `m = 25 %`, throughput `m = 10 %`.

| Class | Rule |
| --- | --- |
| FASTER | `hi < ln(1 − m)` |
| SLOWER | `lo > ln(1 + m)` |
| EQUIVALENT | `ln(1 − m) ≤ lo` and `hi ≤ ln(1 + m)` |
| INCONCLUSIVE | any other complete interval |
| TIMER_LIMITED | as CF5 section 6.1; never counted as EQUIVALENT |

Deterministic memory cells, ratio `q = B / CUR` (Q4):

| Class | Rule |
| --- | --- |
| SMALLER | `q ≤ 0.90` |
| LARGER | `q ≥ 1.05` |
| SIMILAR | otherwise |

Benefit criteria (all on M1, which has the edge-dominated graphs):

- **G1:** CB-L, `sparse-10⁶` and `star-10⁶`: live heap after build is
  SMALLER (`q ≤ 0.90`). Predicted: about 0.83 (section 4.4).
- **G2:** every M1 cell with `V ≥ 10⁵`, both engines: live heap after build
  is lower by at least **8 bytes per registered edge**
  (`(CUR − B) / E_r ≥ 8`). Predicted: 11.7 to 21.2 bytes per edge for
  `sparse` with CB-L; the engine term cancels, so the same is expected with
  CF-L.

Why these thresholds differ from the CF5 20 % memory margin: allocator
counts are deterministic, so the margin expresses materiality, not noise;
the predicted effect (about 12–18 %) would always be SIMILAR under 20 %.
The harm threshold (5 %) is tighter than the benefit threshold because B
exists only to save memory.

### 6.7 Decision rule (fixed before measurement)

**ADOPT** B only if all of the following hold:

1. **D1 correctness:** no FAIL in any block.
2. **D2 benefit:** G1 and G2 hold.
3. **D3 no memory harm:** no M1–M5 metric is LARGER (M1-iso and M6 are
   report-only).
4. **D4 no time harm:** no primary or tail metric of I, R, P or C is SLOWER
   (Q5: strict; the alternative tolerates SLOWER up to `ln(1.25)` on the
   `D = 10⁶` hub cells only, and must be chosen before the freeze if at
   all). FASTER cells do not offset SLOWER ones.
5. **D5 inspection:** the Q check passes its published criteria C1–C4 on
   the B arm.
6. **D6 evidence:** every INCONCLUSIVE or TIMER_LIMITED cell has a written
   maintainer disposition (accept with the interval disclosed, or a targeted
   follow-up under a separately approved protocol on the same candidate).
   Such cells are never rerun inside this campaign and never counted as
   EQUIVALENT.

**REJECT** if D1 fails without a fix (or a fixed candidate fails again),
or D2, D3, D4 or D5 fails. No threshold, margin, cell or rule changes after
the freeze; a cell is never rerun because of its result.

If adopted: the M1-iso result (Q6) is disclosed in the release notes and in
the `LifecycleGraph` memory documentation, and the core enters the v1.2.0
release qualification scope (#182).

**Multiplicity.** With about 110 timed interval comparisons (I 18, R 26, P 36, C 32), a few intervals
are expected to miss their median by chance. Because D4 rejects on a single
SLOWER cell, the rule errs toward rejection: under true equivalence a false
SLOWER needs `lo > ln(1.10)`, which is rarer than the 1.13 % one-sided miss
rate at the margin itself. This conservatism is intended: the change exists
for memory and must not buy it with latency.

### 6.8 Execution order and budget

1. **Tier 1:** tier-1 smoke (all arms and cells), then M (deterministic,
   cheap, and carries the benefit criteria).
2. **Tier 2:** hub cells: P1–P4, I2, I3, R2, R3, R4; then Q.
3. **Tier 3:** ordinary cells: I1, R1, R5, C1, C2, C3.
4. **Exploratory block X** (section 7), only after the formal analyzer has
   run on tiers 1–3 and its output is hashed into the record.

Blocks run one after another; pairs interleave only within a block. The
host-time cap is proposed after the pilot (which estimates each cell's
duration) and approved by the maintainer before the freeze; a cap cuts the
last blocks, and a cell without its 24 pairs gets no class (missing
evidence under D6).

## 7. Exploratory reference: option A

Option A (incidences sorted by internal index, opaque cursor) is **not** a
candidate: it changes the documented incidence order (ascending neighbor ID
with lifecycle support) and replaces position cursors by tokens
(`docs/http-api.md`, "Order and cursors").

It may be measured as block X, kept apart from the formal comparison:
same host, same base commit, arm A = base plus an A implementation;
cells M1, M2, P1 (full traversal only, since cursors differ) and C1; the
same methods. Its results are published in a separate section labelled
exploratory, with no class, and they cannot affect the decision of
section 6.7 (block X runs after the formal analysis is recorded).
