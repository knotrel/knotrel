# Experimental cluster forest backend (CF1 design)

Status: Accepted, revision 4 (2026-11-24; roadmap v1.1.0, item CF1). Awaiting
maintainer approval. No code exists yet; CF2 starts only after approval.

Extends ADR 0007 (startup engine configuration) and follows the pattern of ADR
0008 (experimental HDT): a new opt-in backend behind `ConnectivityGraph`,
`compact-bfs` stays the default.

**Maintainer decisions already recorded (2026-11-24):** implementation written
from the papers with no reuse of the authors' code (D4); integration with the
existing lifecycle facade (D3); public interfaces under the 1.x compatibility
rules of `ett` and `hdt`, internals kept private (D2, section 7). The variant
(D1) is approved only after review items R1–R3 are closed (section 11).

## 0. How to read claims in this document

Every statement of a property, invariant or bound carries one tag:

- **[P]** stated or proved in [DDKLSW] (with its section, lemma or theorem);
- **[W]** stated in [WN13] (with its section);
- **[D]** proposed or derived by this document. A [D] claim is not a
  published result. Its status is given as *reviewed* (checked by the
  independent review of section 12) or *pending* (not yet checked).

No bound tagged [D] is presented as established until its review is
recorded as concluded in section 12.

## 1. Sources

- **[DDKLSW]** Q. De Man, L. Dhulipala, A. Karczmarz, J. Łącki, J. Shun,
  Z. Wang. *Towards Scalable and Practical Batch-Dynamic Connectivity.*
  PVLDB 18(3): 889–901 (VLDB 2025),
  DOI [10.14778/3712221.3712250](https://doi.org/10.14778/3712221.3712250).
  Read from [arXiv:2411.11781v1](https://arxiv.org/abs/2411.11781) (the only
  version), re-read in full on 2026-11-24 before CF2 (section 12). The
  camera-ready version could not be retrieved (the ACM page refused
  automated access and the VLDB volume page lists no link to it), so every
  [P] tag refers to arXiv v1, whose text still carries template
  placeholders and so predates the camera-ready version.
- **[WN13]** C. Wulff-Nilsen. *Faster Deterministic Fully-Dynamic Graph
  Connectivity.* SODA 2013, 1757–1769. Read from
  [arXiv:1209.5608v1](https://arxiv.org/abs/1209.5608) (the only version),
  §1–§4.7 in full; §4.8–§5 (the O(log² n / log log n) refinements, not used
  here) only in summary. [DDKLSW] §4 cites it as the source of the cluster
  forest and the local trees.
- **[HDT01]** J. Holm, K. de Lichtenberg, M. Thorup. JACM 48(4), 2001.
  Background for the amortization (as in ADR 0008).

**Level orientation.** [WN13] numbers levels from the root (level 0 = whole
components, edges are *raised*, a level-i cluster has at most `n / 2^i`
vertices). [DDKLSW] numbers them from the leaves (a level-i cluster has at
most `2^i` vertices, edges are *pushed down*). This document uses the
[DDKLSW] orientation throughout and translates [WN13] statements into it.

## 2. Licensing check and reuse decision (D4, approved)

The artifact listed by [DDKLSW] is `github.com/ParAlg/DynamicConnectivity`
(C++; README titled `BatchDynamicConnectivity`). Re-checked on 2026-11-24
through the GitHub API: no license (`license: null`), no license file, last
push 2026-03-26, hence all rights reserved. Nothing is reused, ported or
transcribed; implementers do not read that source while writing CF2. No new
dependency (the core's existing `rustc-hash` and `nohash-hasher` suffice).

## 3. What the sources establish

**[WN13] §3, the "simple" structure** (the basic cluster forest without path
compression):
- edge levels; level-i clusters are the components of the level-i graph;
  size invariant; one forest node per cluster at every level (§3.1) [W],
  which implies single-child nodes are kept (implied, not stated);
- insertion at the top level, merging the two roots (§3.2) [W];
- deletion by two alternating DFS searches from the endpoints' children,
  with a connected case (case 1) and a split case (case 2); in case 1 the
  meeting cluster is counted on the side of the search that reached it
  first, and the last edge of the other search (into it) is not moved
  (§3.3) [W];
- local trees: children ranked by `⌊log₂ n(v)⌋`, equal ranks paired into rank
  trees with pairwise distinct root ranks, chained along a *rank path* from
  the cluster node; depth of a child `v` of `u` at most
  `log₂(n(u)/n(v)) + 1` (attributed to Thorup) and an O(log n) leaf depth
  overall by a telescoping sum; merge, add child and remove child in
  O(log n) worst case (§3.4, §3.6) [W];
- level bitmaps on local-tree nodes, O(log n) per change (§3.5, §3.7) [W];
- **O(log² n) amortized updates and O(log n) worst-case queries** for this
  structure, with the accounting given informally (raises pay for searches
  and merges; no formal lemma) [W].

**[DDKLSW]**:
- the same definitions in the leaf-up orientation; Invariant 2.1 (a component
  of `G_i` has at most `2^i` vertices) (§2) [P];
- **path compression:** a level-i cluster is stored iff `i = 0` or some
  level-i edge has both endpoints in it, with O(n + m) space (§4) [P]; no
  proof beyond the statement. §2 separately claims O(n) nodes "if clusters
  with only a single child are not stored", a different rule; under the §4
  rule the node count is at most `(V − 1) + E` (section 5.2) [D, reviewed];
- sequential insert/delete as in [WN13], in prose, with no numbered lemma and
  no theorem for the basic variant; the Introduction says only that its
  update times are "similar to HDT" (§1, §4) [P];
- the blocked variant: Definition 5.1, Invariant 5.2, Lemmas 5.3–5.10 and
  **Theorem 5.11 (O(log² n) amortized, fixed n), proved for the blocked
  variant only** (§5) [P];
- queries by walking to the roots, O(log n) memory reads and one equality
  check, no theorem (§8.3) [P; verified before CF2];
- experiments on two sequential variants, CF-Root and CF-LCA; whether they
  enforce Invariant 5.2 is not stated (§8, App. C) [P]. On insertion speed
  the paper is inconsistent: §8.1 calls CF-LCA insertion "1.06× faster" on
  average, while §8.3 says the LCA optimization slows insertions slightly,
  and the totals of App. C.2 (Tables 2 and 3) show CF-LCA insertion slower on
  13 of 16 graphs. Deletions are reported about 1.5× faster (§8.1). These are
  the paper's measurements, not Knotrel expectations.

**Not covered by either source:** a growing vertex set, vertex removal,
rebuilding, worst-case update latency.

Typos in [DDKLSW] handled below: Definition 5.1 uses `cluster(·, i)` where
the proofs appear to use level i-1 (from the first extraction; not confirmed
by the reviews; it concerns only the blocked variant); §5.1 writes `cluster(u, i-1)` for both endpoints;
the insertion level differs between §4 (`L_max`) and §5.1 (`L_max + 1`).

## 4. Variant (D1, open until R1–R3 are closed)

**Proposal [D]:** the basic sequential cluster forest of [WN13] §3 with the
path compression of [DDKLSW] §4 and insertion at the lowest common cluster
(CF-LCA, [DDKLSW] §8.1). Not in v1.1: the blocked invariant, the flattened
root array of §8.1, the tree/non-tree marking and two-level queue of App. C.1.

Reasons:

1. The only published bound for the basic structure is [WN13] §3
   (uncompressed, informal accounting). [DDKLSW] proves a bound only for the
   blocked variant (Theorem 5.11). The blocked invariant's structural benefit
   (diameter ≤ 2 cluster graphs, Lemma 5.4) serves the depth of the parallel
   algorithm (§6–7), which v1.1 does not implement (CF7, deferred).
2. The blocked restoration (Algorithm 3, Lemma 5.9) needs a `Smallest(CG)`
   query that [DDKLSW] provides only in the parallel modified local tree
   (App. A.3, verified before CF2: not part of the sequential local trees of
   §4), and §5
  carries the typos listed above.
3. Nothing in [DDKLSW] §8 shows the blocked invariant was measured.

What the proposal costs: the update bound for the compressed CF-LCA variant
is a [D] derivation (section 6), not a published theorem. Choosing the
blocked variant instead would rest on Theorem 5.11 but adds the least
specified part of the paper.

The flattened root array is excluded for simplicity: with it, a cluster's
bitmap is the OR of up to `log n` roots, and recomputing it naively on every
bitmap change at every level costs up to O(log² n) per change, against
O(log n) with the rank path. Per-cluster, per-level counts of roots with the
bit set would avoid this, and [DDKLSW] §8.1 states the array does not hurt
the time bounds; the rank path is chosen because it is the structure whose
bounds [WN13] states [D, reviewed; second review, finding 9].

## 5. Mapping to Knotrel data structures [D]

### 5.1 Identifiers, slots and a growing vertex set

- External `NodeId = u64` maps to a dense `u32` slot through an
  `IntMap<NodeId, u32>` (as `HdtGraph` maps to `usize`). Slots are not reused
  (the plain engine has no node removal, section 7). All node handles are
  `u32`; stored clusters are at most `(V − 1) + E` and local-tree nodes fewer
  than twice the children, so every node fits while `V` and `E` stay at most
  `2^28` each. Beyond that, `add_node`/`link` return the existing
  `GraphError::NodeLimitExceeded` or `EdgeLimitExceeded` with limit `2^28`
  (no new public error; CF2). Under `LifecycleGraph` the enforced limits are
  lower, `(2^28 − 1024) / 2 = 2^27 − 512` registered nodes and `2^28`
  registered edges, whatever is configured: engine vertices include retired
  slots, at most `2 × catalog + 1024` (section 7), and the engine must never
  reject a mutation the catalog accepted (CF2 review, finding 1).
- `U` is the high-water vertex count and `L_max = max(1, ⌈log₂ U⌉)`. Levels
  are `u8` in `1..=64`; level i is bit `i-1` of a `u64` bitmap.
- **Growth [D, reviewed]:** when `U` crosses a power of two, `L_max` grows by
  one and nothing is relabeled. Bounds `2^i` do not depend on `n`; edges at
  the old top level satisfied the bound when written (component ≤ `U_old` ≤
  `2^{L_max_old}`); components of `G_i` for `i < L_max` grow only through
  pushes, which check the bound; new top-level links use the new `L_max`.
  Roots left below the new `L_max` are handled by the level-aware root merge
  of section 5.3. The sources assume a fixed vertex set ([DDKLSW] §1); this
  is our extension, with the same reasoning as ADR 0008.

### 5.2 Records

Arenas (`Vec`) addressed by `u32` handles, free lists for clusters and
local-tree nodes; no `unsafe`, no `Rc`.

| Record | Fields (indicative) | Count |
|---|---|---|
| Vertex (leaf) | handle of its local-tree leaf in the parent cluster, or none if it is an isolated root; `u64` level bitmap; per-level incident edge lists | `V` |
| Cluster | level; `n(c)`; handle of its local-tree leaf in its parent (none for a root); root of its rank path | ≤ `(V − 1) + E` (each unary cluster owns an edge of its level, section 5.7) |
| Local-tree node | parent or owning cluster; two children; rank; subtree size; `u64` OR of level bitmaps below | < 2 × children + rank-path nodes |
| Edge | key `(min slot, max slot)` in `FxHashMap`; level; index in each endpoint's level list | `E` |

Per-vertex level lists are `Vec<u32>` per occupied level, located by popcount
over the bitmap ([DDKLSW] App. C.1 describes this leaf layout, without
naming the edge-set container [P; verified before CF2]). Each edge stores
its index in both lists. Removal is O(1) worst case (swap-remove, then fix
the moved edge's stored index). Insertion is a `Vec::push`: O(1)
**amortized**, O(length) in the worst case when the vector reallocates; the
same holds for the arenas and the per-vertex level directory. These
reallocations are latency spikes, not changes to the amortized bounds of
section 6, and are listed under "Not bounded" there.

**Space [D, reviewed]:** O(V + E) words. Allocated capacity can stay at
earlier high-water marks; section 7 covers bounding it.

### 5.3 Insertion `link(u, v)`

1. Map IDs to slots, reject self-loops, return `false` for an existing edge
   (contract unchanged).
2. Find both roots (section 5.6). **If they differ**, set `l(e) = L_max` and
   join the roots by level [D, reviewed; fixes review finding 4]:
   - both roots at level `L_max`: merge their rank paths ([WN13] §3.6 merge,
     O(log n) [W]);
   - exactly one at `L_max`: the other root (a lower cluster or a bare
     vertex) becomes one child of it;
   - neither at `L_max`: a new cluster at `L_max` gets both as children.
   Merging the children of a lower root into an `L_max` root would leave its
   level-`< L_max` component without a cluster, so that never happens.
3. **If they are equal**, set `l(e) = l(A)`, where `A` is the lowest explicit
   common ancestor of `u` and `v` [D, reviewed]: `u` and `v` already share the
   level-`l(A)` cluster, so no component of any `G_j` changes and Invariant
   2.1 holds; `A` is explicit and has `u` and `v` in distinct children.
   ([WN13] §3.2 and [DDKLSW] §4 insert at the top level; LCA insertion is
   the CF-LCA variant of [DDKLSW] §8.1 [P], whose correctness the paper does
   not argue.)
4. Add `e` to both level lists and update bitmaps on both leaf-to-root paths.

### 5.4 Deletion `cut(u, v)`

Let `i = l(e)`; let `P` be the lowest explicit common ancestor of `u` and `v`.
By the compression rule, `l(P) ≤ i` always [D, reviewed]. Remove `e` from both
level lists and update bitmaps.

1. **Above the common cluster.** If `i > l(P)`, `u` and `v` stay connected
   by edges of level `≤ l(P) < i`; no `G_j` loses connectivity [D,
   reviewed]. The explicit level-i ancestor that held `e` as an internal
   edge may now hold no level-i edge: if it has one child whose bit `i-1` is
   clear, splice it out (section 5.7; second review, finding 1). Stop.
   Otherwise `i = l(P)` and `u`, `v` lie in distinct children
   `C_u`, `C_v` of `P`; "same child" cannot occur and is a debug assertion
   (review finding 8).
2. **Searches** [W §3.3; mechanics D, reviewed, fixes finding 3]. Two
   searches over `CG(P)` alternate single steps. A step of side `s` takes the
   oldest reached cluster of `s` that still has a level-i edge, fetches one
   (`FetchEdge`, section 5.5) and **detaches it** from both endpoints' level-i
   lists (updating bitmaps) into a scratch list tagged `s`, so no edge is
   fetched twice. It climbs from the far endpoint to its child of `P`
   (O(log n), section 5.6). Reached clusters carry an epoch stamp and a side
   tag (O(1) marks). The search queues, detached-edge lists and chains are
   buffers owned by the graph and reused; the local-tree operations still
   allocate one `O(log n)`-sized temporary vector per call (CF2 review,
   finding 5: this does not change the bounds; reusing a buffer is a v1.2
   candidate).
   - far child unmarked: mark it `s` and queue it;
   - far child marked `s`: an edge internal to `s`'s reached set;
   - far child marked by the other side: **meeting** (step 3).
   Before a meeting, every detached edge of a side lies inside that side's
   reached set (an edge into the other side's set *is* the meeting).
3. **Connected outcome** [W §3.3 case 1, reviewed; fixes finding 1]. Let `X`
   be the meeting cluster, `o` (owner) the side that reached `X` first and
   `t` the side whose edge just hit it. Let `R_o` be `o`'s reached set
   **including `X`** and `R_t` be `t`'s reached set. They are disjoint
   children of `P`, so `min(n(R_o), n(R_t)) ≤ n(P)/2 ≤ 2^{i-1}`.
   - If `n(R_o) ≤ n(R_t)`: push all of `o`'s detached edges to level `i-1`
     and merge `R_o` into one level-(i-1) cluster; reattach `t`'s edges at
     level `i`.
   - Otherwise: push all of `t`'s detached edges **except the meeting edge**
     and merge `R_t`; reattach the meeting edge and `o`'s edges at level `i`.
   Pushed edges lie inside the merged set, so the merged cluster is exactly
   one component of `G_{i-1}`. Stop.

   **Merging a reached set `R` at level `i-1`** (used by steps 3 and 4;
   second review, findings 2 and 3): if `|R| = 1` and nothing was pushed,
   nothing changes. If `|R| = 1` and only edges internal to that cluster
   `C` were pushed, a level-(i-1) node over `C` is created only when
   `l(C) < i-1` (the pushed edges make it explicit); otherwise `C` already
   is that node. If `|R| ≥ 2`: clusters of `R` already at level `i-1` are
   merged by rank-path merge, lower-level clusters are added to the result as
   children, and if none is at level `i-1` a new level-(i-1) node holds them
   all. After `R`'s clusters are removed from `P`'s local tree, the merged
   node is inserted into it (or re-ranked, if it is an existing cluster).
4. **Split outcome** [W §3.3 case 2, reviewed]. A side `s` has no level-i
   edge left in its reached set `R_s`: `R_s` is a whole component of
   `CG(P) − e`, and the remainder of `P` (which contains the other side's
   reached set `R_r`) is connected, since `CG(P) − e` has at most two
   components.
   - Push the detached edges of the side with the smaller reached set
     (`≤ 2^{i-1}` as above) and merge that set; reattach the other side's
     edges. The exhausted side may be the larger one ([DDKLSW] §4 assumes it
     WLOG; [WN13] §3.3 swaps roles); then `R_s` is moved unmerged, `|R_s|`
     being at most the other side's detached edges + 2.
   - Create `P'` at level `i` holding `R_s` (merged or not), remove those
     children from `P`, and attach `P'` under `P`'s parent `Q`, or make it a
     root if `P` was a root.
   - When the exhausted side was the smaller and was merged into one node
     `W`, `P'` would be unary with no level-i edge, so `W` is attached under
     `Q` directly (or becomes a root) instead of creating `P'`.
   - Apply the compression rule (section 5.7) to `P` and `P'`; a piece that
     becomes unary without an edge of its level is spliced out, and its
     **current top explicit node** below `Q` represents it from here on
     (fixes finding 6).
5. **Upward continuation** [W §3.3 recursion; skip rule D, reviewed under
   the rule of section 5.7, fixes finding 2]. If `P` was a root, the piece
   `P'` is a new component: stop. Otherwise repeat steps 2–4 in `CG(Q)` at
   level `l(Q)`, starting from the two pieces' representatives. Levels
   strictly between `l(P)` and `l(Q)` hold no edge with both endpoints in
   `P`'s former vertex set, because any such edge would have made that level
   explicit under section 5.7; so they are skipped.
6. **No push below level 1** [D, reviewed]. At level 1 a cluster is two
   vertices joined by `e`; after removing `e` no level-1 edge remains, so
   the split outcome moves single vertices and nothing is pushed. A debug
   assertion checks it.

Every merge checks Invariant 2.1 on the merged size with a debug assertion.

### 5.5 Local trees [W §3.4, §3.6, §3.7; adaptations D]

Per cluster, children are kept as in [WN13] §3.4: rank `r(v) = ⌊log₂ n(v)⌋`;
equal-rank trees paired under a node of rank +1 until root ranks are
pairwise distinct (at most `⌊log₂ n(c)⌋ + 1` rank trees); rank trees hung
from a **rank path** in decreasing rank starting at the cluster's node.

- **add child, remove child, merge two clusters:** the [WN13] §3.6
  procedures (remove the rank path, pair equal ranks, rebuild the rank path;
  for removal, split the child's rank tree along its root path into pieces
  of distinct ranks first), O(log n) worst case [W]. A child whose rank
  changes is removed and re-added.
- **aggregates:** every node stores subtree size and the OR of the level
  bitmaps below; a change at a leaf updates its ancestors, O(log n) per
  forest change [W §3.7].
- **`FetchEdge(C, i)`:** descend from `C` along nodes whose bitmap has bit
  `i-1` set down to a vertex, then take an edge from its level-i list. [WN13]
  §3.5 gives O(log n) per explored edge [W].
- **`Parent(c)`:** climb from `c`'s local-tree leaf to the cluster node that
  owns the rank path.
- **Depth:** a child `v` of `u` lies at depth at most
  `log₂(n(u)/n(v)) + 1` in `L(u)` (Thorup, cited in [WN13] §3.4 [W]);
  summing over the cluster levels of a vertex-to-root path telescopes to
  O(log n) [W]. With compression a path crosses fewer cluster levels, and the
  sum telescopes the same way [D, reviewed].

### 5.6 Query `connected(u, v)`

Map both IDs (unknown IDs: `GraphError::UnknownNode`, unchanged), `true` for
`u == v`, otherwise climb both vertices to their roots and compare. No
mutation, allocation or cache. O(log n) worst case plus ID lookup: [W §3.1]
for the uncompressed structure, carried over by the depth argument of 5.5
[D, reviewed].

### 5.7 Compression [P §4; maintenance D, reviewed (delta check)]

A cluster of level `j ≥ 1` is explicit iff some level-j edge has both
endpoints in it ([DDKLSW] §4). An explicit cluster has two or more children,
or one child whose aggregated bitmap has bit `j-1` set (a level-j edge
inside it). Revision 1 dropped the second condition; the review showed that
this gives wrong `connected` answers (an edge kept at a level above its
unary span is never searched when a deletion below splits the span), so the
paper's rule is kept (closes R3; finding 2).

Maintenance: after a split, a merge, or the removal of an edge that stops
early (5.4 step 1), a cluster with one child and no edge of its level is
spliced out (its child takes its place in the parent's local tree). When a
push lands an edge at level `j` inside a unary span, the level-j cluster is
created. A further condition used by the checker: for an explicit node `c`
with parent level `p` (`p = L_max + 1` for a root), no vertex of `c` has an
edge at a level strictly between `l(c)` and `p`. This is the "implicit ⇒ no
edge of its level" half of the paper's rule; it is equivalent to the rule
only together with check 4 ("explicit ⇒ holds an edge of its level") and
check 7 (cluster semantics) of section 8 (second review, finding 6).

## 6. Complexity

`N` is the high-water vertex count, `L_N = max(1, ⌈log₂ N⌉)`.

| Operation | Bound | Kind | Tag and status |
|---|---|---|---|
| `connected` | O(log N) + ID lookup | worst case | [W §3] uncompressed; compressed: [D, reviewed] |
| `link` | O(log N) actual; O(log² N) amortized (aggregate, below) | actual / amortized | [W §3] uncompressed, top-level insertion; CF-LCA, compressed: [D, reviewed] |
| `cut` | O(log² N) | amortized (aggregate, below) | [W §3] uncompressed; compressed: [D, reviewed] |
| `add_node` | O(1) | amortized (vector growth) | [D] |
| Space | O(V + E) | worst case, logical | [P §4] statement; [D, reviewed] for this layout |
| Rebuild (`try_from_edges`, lifecycle compaction) | O((V + E) log N) actual; O((V + E) log² N) with the later pushes charged to it | amortized | [D, reviewed] |

**Status of the update bound.** For the structure implemented here the
O(log² N) amortized update bound is **this document's proof sketch, not a
published theorem**: [WN13] §3 states it for the uncompressed structure with
informal accounting, and [DDKLSW] gives no theorem for the basic variant.
Two independent reviews and a delta check found the sketch adequate for a
design document (section 12). CF2 and CF3 test the invariants it rests on;
CF5 measures costs, it does not prove bounds.

**Derivation (proof sketch) [D, reviewed: second review and delta check].** Stated in aggregate, against the final
high-water vertex count `N` and `L_N = max(1, ⌈log₂ N⌉)`, because `U` and
the cost of a step grow during a sequence (second review, finding 7): over
any sequence with `I` insertions and `D` deletions, total time is
O((I + D) · log² N), i.e. O(log² N) amortized per update.

- *Pushes.* An edge's level only decreases, from at most `L_N` to at least 1
  (5.4 step 6), so each edge lifetime has at most `L_N − 1` pushes. A push
  costs O(log N): list moves and bitmap updates on two paths. Total
  O(I · log² N).
- *Search steps.* A step costs O(log N): a `FetchEdge` descent, a detach, a
  climb to `P`'s child. The two sides fetch the same number of edges ±1, and
  all detached edges of one side are pushed except at most the meeting edge,
  so steps and reattachments at a level cost O(pushes at that level + 1) ·
  O(log N).
- *Merges and moves.* O(log N) per reached cluster (rank-path operations);
  a side's reached clusters are at most its detached edges + 1, and in the
  larger-exhausted case `|R_s| ≤ k_r + 2`, where `k_r` is the other side's
  detached (and pushed) edges.
- *Per visited level, unpaid, each O(log N)* (finding 8): creating `P'` or
  attaching `W` under `Q`; splicing `P` and/or `P'`; creating a unary
  level-(i-1) node for pushed internal edges; re-ranking the merged node in
  `P` and re-ranking `P` in `Q` (`n(Q)` is unchanged, so no cascade upward);
  the climb to `Q`; the splice of 5.4 step 1.
- *Levels.* At most `L_N` explicit levels are visited per deletion.
- Summing: O(log N) per push or search step, pushes bounded by
  `I · (L_N − 1)`, plus O(L_N · log N) unpaid per deletion and O(log N)
  actual per insertion.

**Rebuilds.** A rebuild (`try_from_edges`, used by lifecycle compaction
through `build_engine_incremental`) registers all vertices first, so `L_max`
is final before any edge, then calls `link` once per effective edge, with
the same CF-LCA rule as any insertion: an edge that joins two components
gets level `L_max`, an edge whose endpoints are already connected gets the
level of their lowest explicit common ancestor, which is `≤ L_max`. So a
rebuilt edge does **not** necessarily start at the top level, but every
rebuilt edge starts a new lifetime at a level `≤ L_N`, which is all the
derivation uses: each lifetime still has at most `L_N − 1` pushes. The levels
reached before the rebuild are discarded. Cost: O((V + E) log N) actual
(one `link` each, plus amortized vector growth), and up to
O((V + E) log² N) of later pushes charged to the rebuild, since every
rebuilt edge counts as an insertion `I` in the aggregate bound.

Lifecycle compaction
rebuilds after more than `max(V, 1024)` retirements, so the charge per
retirement is O((1 + E/V) log² N), large when `E ≫ V`. A compaction that
relabels slots while keeping levels would avoid this; it is deferred to v1.2
(H3) if CF5 shows the cost.

**Not bounded:** worst-case latency of one `cut` (a large search can push
many edges); vector reallocations (section 5.2); allocator high-water memory
between rebuilds. Amortized bounds
are not p99 guarantees.

## 7. Integration with Knotrel's contract

- **Public interface (D2, approved direction):** `EngineConfig::ClusterForest`
  with selector `"cluster-forest"` (`as_str`, server configuration) is a
  **stable interface for an experimental engine**, like `EulerTour` and
  `Hdt` ([compatibility.md §2.2](../compatibility.md#22-stable-interface-experimental-engine)):
  the variant and selector are kept for 1.x and removed or renamed only after
  a deprecation and in a major release; `is_experimental()` returns `true`,
  qualifying only performance, maturity and usage recommendation.
  `docs/stability.md` gets the matching rows; the public API snapshot and the
  `API compatibility` job must pass against v1.0.0 (an additive variant of a
  `#[non_exhaustive]` enum).
- **Internal:** the graph type, its statistics and the local trees stay
  `pub(crate)`. Unlike `ForestGraph` and `HdtGraph`, no backend type is
  exported in v1.1: it is still expected to change (CF5, v1.2), and a stable
  type would freeze it. Exposing it later is an additive change.
- **Lifecycle (D3, approved):** `LifecycleGraph::supports_engine` returns
  `true` for `ClusterForest` (still `false` for `ett` and `hdt`). The facade
  is unchanged: it keeps the catalog, flags and registered topology, feeds
  the engine only the effective graph (active endpoints, enabled edges)
  through `link` and `cut`, and handles node removal by retiring slots;
  `compact_if_needed` rebuilds the engine through `build_engine`, which
  dispatches to `build_engine_incremental` for this backend. That rebuild
  bounds memory after churn (engine vertices at most `2 × catalog + 1024`,
  fresh arenas); its cost is in section 6. The facade enforces node and edge
  limits tightened by this backend's capacity (section 5.1), so the engine
  calls the facade makes (registration, `link`, rebuilds) never fail.
- **Plain `ConnectivityGraph`:** no node removal, as for the other engines;
  memory after mass edge deletions stays at O(V + E_high-water) allocated
  capacity, documented, not fixed.
- **Server:** `KNOTREL_ENGINE=cluster-forest` is an additive value with the
  same stability as `ett` and `hdt` (stable value, experimental engine);
  `/v1/info` reports `engine: "cluster-forest"` and `experimental: true`.
  Inspection: without lifecycle support the engine keeps no per-node
  adjacency, so like `ett` and `hdt` it reports `degree: null` and answers
  `inspection_unsupported` for incident and edge listing; with lifecycle
  support the facade's catalog answers them, as for `compact-bfs` (the
  console enables edge browsing accordingly).
  `docs/openapi.yaml`, `docs/http-api.md`, the console's generated types and
  `supportedServer` follow in CF4.
- **Imports and atomicity:** unchanged (off-lock `try_from_edges`; every
  operation completes under the graph lock before another observes state).
- **MSRV** 1.98.1 preserved; no `unsafe`; no new dependency.

## 8. Invariant checker (planned for CF2)

A crate-private `check_invariants(&self)`, called after every operation by
the in-crate CF2/CF3 harness:

1. **Edges:** each edge sits in both endpoints' level list at its level and
   stored index; a vertex's bit `i-1` is set iff its level-i list is
   non-empty; levels in `1..=L_max`; no edge below level 1.
2. **Quiescence:** between operations the search scratch is empty and every
   edge is attached to its lists.
3. **Forest shape:** child level < parent level; root level ≤ `L_max`;
   `n(c)` equals the sum over children; parent relations agree with
   local-tree membership and rank-path owners.
4. **Compression (5.7):** every explicit cluster of level `j ≥ 1` holds at
   least one level-j edge with both endpoints inside, counted literally from
   the edge lists (not inferred from its number of children); no vertex of
   an explicit `c` has an edge strictly between `l(c)` and its parent's
   level.
5. **Local trees:** pairing ranks as in 5.5, distinct root ranks in
   decreasing order along the rank path, sizes and bitmap aggregates
   recomputed from the leaves.
6. **Size invariant 2.1:** every explicit cluster of level `i` has
   `n(c) ≤ 2^i`.
7. **Cluster semantics, every level:** for every explicit `c`, **vertices
   included as level-0 clusters**, and every level `j` from `l(c)` to its
   parent's level − 1 (to `L_max` for a root), the vertex set of `c` is
   exactly one component of `G_j`, checked by BFS over edges of level ≤ j.
   (A per-edge shortcut is not equivalent: it misses an explicit node whose
   children are not connected inside it; second review, finding 4.)
8. **Components:** roots are exactly the components of `G`; `L_max =
   max(1, ⌈log₂ U⌉)`.

In debug builds the code asserts, on touched paths only: the size invariant
after every merge (check 6), that nothing is pushed below level 1 (check 1,
5.4 step 6), that the LCA separates the endpoints and that an edge never
sits below its LCA, and quiescence after every operation (check 2). Checks 3,
4, 5, 7 and 8 run only in the full checker, which is test-only. (Revision 4
said checks 1, 4 and 6 run as debug assertions; CF2 review, finding 5.) The
full checker also verifies the arena free lists: each free node slot listed
once, live and free edge records partitioning the edge arena (finding 4).

## 9. Verification plan (CF2–CF3)

- CF2: the core's existing engine-agnostic tests on the new backend; the
  full checker after every operation.
- CF3: differential tests against `ReferenceGraph` and `compact-bfs`; seeded
  property tests with the in-repo deterministic generator of
  `crates/knotrel-core/tests/topology.rs`; the roadmap's adversarial families
  (deep replacement searches, repeated split/merge, hub and star deletions,
  ring breaking, edges pushed to level 1, `L_max` growth after pushes);
  churn with lifecycle compaction.
- Regression fixtures for every review finding: the meeting-cluster case of
  finding 1 (sizes 1, 4, 3 at level 3), the unary-span case of finding 2, a
  refetch loop (finding 3), a link to a root below `L_max` (finding 4), a
  larger exhausted side, compression after a split and stale
  representatives (finding 6); from the second review, the early-stop
  splice (sizes in its finding 1), the single-cluster merge, merges across
  levels, and the disconnected explicit node of its finding 4 fed to the
  checker as a corrupted state.

## 10. What this design does not do

- No blocked invariant, no batch-parallel algorithm (CF7, deferred).
- No change to the default engine or any existing engine.
- No performance claim before CF5 measures one on Knotrel's workloads.
- Not presented as a fix for [#87](https://github.com/knotrel/knotrel/issues/87).

## 11. Decisions and review items

| ID | Item | Status |
|---|---|---|
| D1 | Variant: basic CF ([WN13] §3) + compression ([DDKLSW] §4) + CF-LCA | **Open:** maintainer decision after R2 |
| D2 | Public interface | **Approved direction** (2026-11-24): `EngineConfig::ClusterForest` / `cluster-forest` as stable interface, experimental engine; backend types internal (section 7) |
| D3 | Lifecycle support through the existing facade | **Approved** (2026-11-24) |
| D4 | No reuse of the authors' unlicensed code | **Approved** (2026-11-24) |
| R1 | Local-tree source and bounds | **Closed** (section 12): [WN13] §3.4–§3.7 read; rank path adopted, flattened array excluded |
| R2 | Independent review of deletion, invariants and the update bound | **Closed** (section 12): revision 1, 12 findings (2 blocking); revision 2, 12 findings (none blocking); delta check of revision 3, tags and wording only |
| R3 | Compression rule | **Closed** (section 12): dropping the self-loop condition is unsound; the paper's rule is kept |

## 12. Review record

**R1 (2026-11-24).** [WN13] read from arXiv v1 (§1–§4.7 in full). It
supplies the local trees used in 5.5 (rank pairing, rank path, the depth
bound attributed to Thorup, O(log n) worst-case merge/add/remove, bitmaps),
the alternating searches with the meeting cluster counted on the first
side, and the O(log² n) amortized / O(log n) query bounds for the
uncompressed structure, with informal accounting. It has no path
compression: compression is [DDKLSW] §4's. Revision 1's flattened root
array is replaced by the rank path (section 4).

**R2/R3, first independent review (2026-11-24),** of revision 1, by a
reviewer that read [DDKLSW] itself and was not given this document's
conclusions. Findings and resolution:

| # | Severity | Finding | Resolution in revision 2 |
|---|---|---|---|
| 1 | blocking | Excluding the meeting cluster from both sides lets a push break Invariant 2.1 (counterexample: sizes 1, 4, 3 at level 3) | 5.4 step 3: meeting cluster counted on the owner side, as in [WN13] §3.3 |
| 2 | blocking | Dropping the self-loop condition breaks the upward skip and gives wrong `connected` answers | 5.7: paper's rule kept; 5.4 step 5 relies on it |
| 3 | should-fix | Fetching an "unexplored" edge unspecified: refetch, non-termination | 5.4 step 2: detach to scratch, reattach unpushed edges, epoch marks |
| 4 | should-fix | Root merge wrong for roots below `L_max` | 5.3 step 2: level-aware join |
| 5 | should-fix | Amortization: credits make `link` O(log² n) amortized; search and merge costs; rebuild re-credit; bound not proved by [DDKLSW] for the basic variant | Section 6 rewritten (bound marked pending in revision 2, reviewed in revision 3) |
| 6 | should-fix | Stale representatives after compression | 5.4 step 4 |
| 7 | should-fix | Checker gaps (per-level semantics, quiescence, root level, owners); old check 6 vacuous | Section 8 checks 2, 3, 4, 7 |
| 8 | minor | "Same child" rule never fires | 5.4 step 1: debug assertion |
| 9 | minor | 1.06× insertion figure reported one-sidedly | Section 3: inconsistency stated |
| 10 | minor | Cluster count changes under the paper's rule | 5.2: `≤ (V − 1) + E` |
| 11 | minor | Root-array scan order unstated | Superseded: rank path (section 4) |
| 12 | minor | Rebuild function name | Section 7 corrected |

**Second independent review (2026-11-24),** of revision 2, by a new
reviewer that read [WN13] §3.1–3.7 and [DDKLSW] §2, §4, §8.1 and, of §5,
Definition 5.1, §5.1, Lemma 5.4 and Theorem 5.11. It
verified the resolutions of the first review (owner/trigger rule, split
including the larger-exhausted case, skip rule, termination, level-aware
root join, `L_max` growth, depth telescoping with compression) and found no
blocking error:

| # | Severity | Finding | Resolution in revision 3 |
|---|---|---|---|
| 1 | should-fix | Early stop (5.4 step 1) can leave a unary cluster with no edge of its level | 5.4 step 1 and 5.7: splice after edge removal |
| 2 | should-fix | "Merge `R`" undefined when `R` is one cluster with nothing pushed | 5.4: merge rule, `|R| = 1` cases |
| 3 | should-fix | Merge of clusters at different levels unspecified; `P'` of a merged smaller exhausted side | 5.4: merge rule; step 4 attaches `W` under `Q` |
| 4 | should-fix | Check 7's per-edge "equivalent" is not equivalent; check 4 inferred from children | Section 8 checks 4 and 7 |
| 5 | should-fix | Vertices must count as level-0 clusters in check 7 | Section 8 check 7 |
| 6 | minor | 5.7's "equivalent invariant" is only half of the rule | 5.7 wording |
| 7 | minor | Credits fixed at insertion while `log n` grows | Section 6: aggregate bound against `N` |
| 8 | minor | Per-level unpaid items not listed | Section 6 list |
| 9 | minor | Flattened-array cost overstated; [DDKLSW] §8.1 disagrees | Section 4 wording |
| 10 | minor | Node-count claim mixes §2 and §4 rules | Section 3 |
| 11 | minor | Two "[D, reviewed]" tags wrong (findings 1, 4) | Corrected with the fixes |
| 12 | minor | Not read by the reviewer: [DDKLSW] App. A.3, App. C.1, §8.3 | Verified on arXiv v1 before CF2 (section 12, "Source check"); camera-ready not retrievable |

The reviewer judged the update-bound sketch adequate for a design document
once findings 7 and 8 are written in.

**Delta check of revision 3 (2026-11-24),** by the second reviewer: all 12
findings resolved, no new correctness error (it checked the step-1 splice for
several candidates and cascades, and the merge rule across levels and for
`|R| = 1`); the O(log² N) sketch judged adequate. Its remaining items were
tags and wording only (status tags in 5.7 and section 6, the `link` row,
sources not read by it, the merge-rule wording), applied in this revision.

**R2 closed** for the design: the remaining risk is in implementation, which
the checker of section 8 and CF3 address.

**Maintainer approval (2026-11-24):** D1 approved for the sequential v1.1
backend; D2 confirmed (public selector stable for 1.x, backend and
statistics internal, maturity and performance experimental). Corrections
requested with the approval and applied in revision 4: the cost of `Vec`
insertion (5.2: amortized, not worst case) and the consistency of rebuilds
with CF-LCA insertion (section 6). The O(log² N) bound remains this
project's derivation, not a published theorem for this variant.

**Source check before CF2 (2026-11-24).** [DDKLSW] arXiv v1 re-read in full;
the camera-ready version was not retrievable. Verified: the leaf layout of
App. C.1 (bitmap, per-level edge sets located by popcount; container not
named), the query description of §8.3, the compression rule of §4 and the
§2 node-count remark, Invariant 2.1, Theorem 5.11 (blocked variant only),
the insertion-level inconsistency (§4 `L_max`, §5.1 and §7.1 `L_max + 1`),
`Smallest(C)` only in the modified local tree of App. A.3, and Definition
5.1 using `cluster(u, i)` where Lemma 5.3 and §5.1 use level `i-1` (relevant
only to the blocked variant, not implemented). Also found: §4 does not say
how the meeting cluster is counted (this design follows [WN13] §3.3), and
the CF-LCA insertion-speed statements of §8.1 and §8.3 disagree (section 3).
[WN13] §3 was read in full for R1; §4.8–§5 (not used) only in summary.

**CF2 implementation notes (2026-11-24).** `crates/knotrel-core/src/cluster_forest.rs`
implements this design; the full checker of section 8 and the tests of
section 9 live in `cluster_forest/tests.rs`. Choices made while coding, all
within the design: the `2^28` element limit above; the backend is boxed
inside `ConnectivityGraph` (its search buffers are inline); search marks are
`epoch << 1 | side` per node, reset when the epoch would reach `2^31`. Each
regression of the two reviews was checked against a deliberately
reintroduced defect (owner-side counting, the self-loop compression
condition, the level-aware root join, the early-stop splice, the
single-cluster merge): every one fails its test. The self-loop condition is
rarely exercised by random histories, so a dedicated stress test of small
dense graphs covers it.

**Independent review of the CF2 code (2026-11-24),** against this ADR and
the sources, by a reviewer that read the code (not a summary) and ran 3,200
extra full-checker histories, large sampled histories and targeted probes on
a copy of the crate. No defect in deletion, compression, local trees, handle
reuse or level growth. Findings and resolution:

| # | Severity | Finding | Resolution |
|---|---|---|---|
| 1 | should-fix (contract) | `LifecycleGraph` with `cluster-forest` panics at the 2^28 limit (retired slots count as engine vertices) instead of returning an error | The facade enforces limits tightened by the engine capacity (5.1, section 7); regression with a test-only lowered capacity, churn near the limit, import above it, configured limits below it; it fails with the fix reverted |
| 2 | should-fix | The self-loop regression never reached the internal-edge branch | New fixture; the test asserts the branch ran (counter) |
| 3 | should-fix | `wrap` never reached by any test | Two fixtures (exhausted side, meeting trigger side), asserting the branch ran |
| 4 | should-fix | Checker blind to free-list corruption | Free-list checks added to the full checker (section 8) |
| 5 | minor | ADR claimed more debug assertions and no allocation | Text corrected (5.4 step 2, section 8); level-1 push assertion added |
| 6 | minor | Reported limit is the backend's when the configured one is higher | Documented on `EngineConfig::ClusterForest`; regression |
| 7 | minor | `floor_log2(0)` wrapped in release builds | Uses `u32::ilog2`, which panics in every build |

**Exploratory observation, not a CF5 result.** While reviewing, the
reviewer timed one workload in release mode (random graphs with `E = 3V`,
insert all edges then delete all) and saw cut time divided by `log² N` stay
within 24–29 ns from 2^12 to 2^18 vertices, about 1.6 to 3.0 pushes per edge
and about 2.3 levels visited per cut. It is recorded only as an exploratory
observation: one workload, one machine, no predeclared protocol, no
repetitions or variance, a narrow size range. A flat normalized cost over a
finite range does **not** demonstrate the asymptotic bound, which remains the
project's derivation (section 6). It is not part of CF5 and must not be used
to choose CF5's acceptance criteria or thresholds after the fact; CF5
declares its protocol and criteria before any measurement
([CF5 protocol](../releases/v1.1.0-cf5-qualification-plan.md)).

**Element limit (2^28).** Node handles are `u32`; all nodes stay below
`3 · (2V + E)` (stored clusters ≤ `(V − 1) + E`, local-tree nodes fewer than
twice the children), which fits with `V, E ≤ 2^28`. Rejections at the limit
leave the graph unchanged (regressions with a test-only per-thread limit of
4 to 4096 elements, no large allocation): node limit with one or two missing
endpoints, edge limit before any endpoint is registered, duplicates still
`Ok(false)`, configured limits reported when lower.

**CF3 coverage (2026-11-24).** Covered and in CI: differential tests against
`ReferenceGraph` and `compact-bfs` through the public API
(`crates/knotrel-core/tests/cluster_forest.rs`), seeded property histories
with the full checker after every operation, lifecycle imports and churn
across engine compactions against `compact-bfs` (engine vertices within
`2 × catalog + 1024`), the adversarial families (deep searches by insert-all
then delete-all with an edge reaching level 1, rings, hubs and stars,
`L_max` growth, epoch wrap), arena reuse under build-and-destroy churn,
allocated capacity after mass deletions (logical structure back to isolated
vertices, every record on a free list, no arena growth on refill; no exact
allocator capacity is asserted), and every review regression. Long variants (`#[ignore]`) run in the
`cluster-forest-long` workflow (weekly, on demand, on release tags). Not
covered: memory measured in bytes (CF5 measures resident memory).

**CF4 (2026-11-24).** `KNOTREL_ENGINE=cluster-forest` is accepted, alone and
with lifecycle; `/v1/info` reports it as experimental; `docs/openapi.yaml`
(engine enum and descriptions), `docs/http-api.md`, `docs/stability.md`, the
console's generated types, engine list and digest are updated. The OpenAPI
compatibility check against v1.0.0 passes (an added response enum value is
compatible). The process-lifecycle, import, feeder-event, batch and
bootstrap server suites run on the engine selected by `KNOTREL_TEST_ENGINE`,
and CI runs them again with `cluster-forest` in plain mode (none of them
enables topology lifecycle). Topology lifecycle on `cluster-forest` is
covered by the topology suite (its engine list), an inspection test with
lifecycle, and a process test that starts the executable with lifecycle;
contract, stats and HTTP suites include the engine in their engine lists
(delta review, finding 4). Before CF4 the server's
internal engine-label mapping panicked on `cluster-forest` (reachable only
by embedding the server with the new variant); fixed and covered.
`supportedServer` of the console is unchanged until the release
compatibility run.

**Independent review of the fix delta and of CF4 (2026-11-24),** on PR
knotrel/knotrel#150, by a new reviewer. It verified the lifecycle limit
invariant on every path (registration, slot reuse, removal and compaction,
imports; no bypass of the enforced limits; other engines unaffected) and
found no blocking defect. Findings and resolution:

| # | Severity | Finding | Resolution |
|---|---|---|---|
| 1 | should-fix | Console disabled edge browsing for `cluster-forest` with lifecycle | Capability derived from lifecycle support; tests |
| 2 | should-fix | Messages and docs still said lifecycle is `compact-bfs` only | `UnsupportedEngine`, lifecycle-disabled and inspection messages, README, deployment, console and lifecycle docs updated |
| 3 | should-fix | Section 7 said inspection behaves as for `ett`/`hdt` | Section 7 corrected; server test of inspection with lifecycle |
| 4 | should-fix | CI's `cluster-forest` step covers plain mode only; three batch tests hardcoded `compact-bfs` | Batch tests parameterized; CI comment and the CF4 record state the coverage exactly |
| 5 | should-fix | CF5 K2 replicated the exploratory workload | K2 now follows the paper's staged protocol, with densities and sizes chosen on stated grounds and the exploratory parameters excluded |
| 6 | minor | Lifecycle capacity test never approached the bound | Churn drives retired slots to their bound; the test fails under a looser compaction policy |
| 7 | minor | Error precedence at the engine capacity | Documented on `EngineConfig::ClusterForest` |
| 8 | minor | `/v1/stats` remainders use configured limits | Documented (OpenAPI, HTTP docs) |
| 9 | minor | Message example matched feeder routes only | Both message forms documented |
| 10 | minor | `measure_e2e` and the soak test omitted the engine | Added |
| 11 | minor | Limit override had no drop guard | RAII guard |
| 12 | minor | CF5 plan wording (abbreviations, attempts, budget) | Corrected |

**Completion of CF1** (roadmap): this document approved; every invariant has
a planned check (section 8); every claimed bound names its source or is
tagged [D] with its review status (section 6).
