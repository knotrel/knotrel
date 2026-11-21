# PostgreSQL reference feeder: protocol, assumptions and correctness

**Status:** design for roadmap items 0.9.1 and 0.9.2, written before the
implementation and corrected where testing showed a gap (§4.3 recovery, §8.9
barrier under load); implemented in
[`integrations/postgres-feeder/`](../../integrations/postgres-feeder/README.md).
It is a **reference integration**: one schema, one source,
reproducible, with stated limits. It is not a general PostgreSQL connector,
and nothing here runs inside the Knotrel server.

The feeder is a separate program (`integrations/postgres-feeder/`, its own
Cargo workspace and lockfile). It reads one PostgreSQL database with logical
replication and drives a Knotrel server in source-managed mode through the
[integration contract](../integration-contract.md) and the feeder routes of
the [HTTP API](../http-api.md#ordered-incremental-event-stream-and-gap-declaration).

## 1. What it guarantees, in short

- **No lost update between snapshot and stream.** The initial image and the
  change stream come from the same replication slot: the image is the
  slot's exported snapshot, and the stream starts exactly after it (§5).
- **Ordered prefix, not transactions.** Knotrel applies the source changes in
  commit order, one event at a time. A reader can observe a state in the
  middle of a PostgreSQL transaction. The feeder marks the last event of
  every transaction, so a reader can tell whether the state it saw is a
  transaction boundary (§6.3). **PostgreSQL transaction atomicity is not
  preserved.**
- **Progress is confirmed to PostgreSQL only after Knotrel applied it** (§7).
  A crash of the feeder, of the network or of PostgreSQL never makes the slot
  skip a change that Knotrel has not applied.
- **A restarted Knotrel is always rebuilt.** The feeder never takes its own
  cursor or the slot position as proof that a Knotrel process still holds the
  graph. A new Knotrel `instance_id` always means a fresh snapshot and a full
  import (§8.4).
- **Bounded retention, no foreign slots touched.** The feeder refuses to run
  unless PostgreSQL bounds the WAL a slot may retain, and it only creates or
  drops slots carrying its own prefix (§9).
- **Unsupported changes stop the feeder with a clear error** instead of being
  applied wrongly (§4.3).

## 2. Components

```text
 application ──writes──▶ PostgreSQL ──logical replication (pgoutput)──▶ feeder ──HTTP──▶ Knotrel (source-managed)
                         (authority)        slot + publication                          ▲
                                                                                         └── console (diagnostics)
```

- **PostgreSQL** is the authority. The feeder never writes to it.
- **Feeder** (`knotrel-pg-feeder`): one process, one database, one
  publication, one Knotrel server. Two connections to PostgreSQL: a
  replication connection (slot creation and streaming) and a normal
  connection (snapshot reads and catalog checks).
- **Knotrel** runs with `KNOTREL_MODE=source-managed`,
  `KNOTREL_TOPOLOGY_LIFECYCLE=true` and the default `compact-bfs` engine. The
  feeder checks this at startup through `GET /v1/info` and refuses to run
  otherwise.

## 3. Assumptions

- **A1.** PostgreSQL 15, 16, 17, 18 or 19, `wal_level = logical`, and
  `max_slot_wal_keep_size` set to a finite value (§9). Older versions are not
  supported.
- **A2.** The two graph tables follow the schema of §4.1. The feeder checks
  the structural parts at startup and on every relation change it receives.
- **A3.** One feeder per Knotrel server. A second feeder can take over (it
  registers a higher epoch); the first one then stops (§8.6).
- **A4.** Feeder epochs are derived from the wall clock in milliseconds and
  are kept strictly increasing within a process. A clock that steps back
  across a feeder restart can make the new feeder lose to the old epoch; it
  then stops with `fenced` and an operator must intervene.
- **A5.** The feeder keeps no durable state. Everything it needs to resume is
  in Knotrel (frontier) and in PostgreSQL (slot).
- **A6.** No primary failover. A timeline change is treated as a new source
  lineage and causes a rebuild (§8.8).
- **A7.** Connections to PostgreSQL and Knotrel run on a trusted network
  without TLS in this version (TLS and the gateway are roadmap item 0.9.5).

## 4. Source schema and mapping

### 4.1 Schema

The reference schema ([`sql/schema.sql`](../../integrations/postgres-feeder/sql/schema.sql)):

```sql
CREATE TABLE knotrel_graph.nodes (
    id     bigint  PRIMARY KEY CHECK (id >= 0),
    active boolean NOT NULL DEFAULT true
);

CREATE TABLE knotrel_graph.edges (
    source  bigint  NOT NULL REFERENCES knotrel_graph.nodes (id) ON DELETE CASCADE,
    target  bigint  NOT NULL REFERENCES knotrel_graph.nodes (id) ON DELETE CASCADE,
    enabled boolean NOT NULL DEFAULT true,
    PRIMARY KEY (source, target),
    CHECK (source < target)
);
```

Applications may add other columns; the feeder ignores them. Schema and
table names are configurable.

### 4.2 Mapping

| PostgreSQL | Knotrel |
| --- | --- |
| `nodes.id` (`bigint`, `0 ≤ id ≤ 2^63−1`) | node ID, the same integer as a decimal string. Knotrel node IDs above `2^63−1` are not reachable from this schema. |
| `nodes.active` | node lifecycle flag `active` |
| row of `edges` `(source, target)` | the undirected edge `{source, target}`. `CHECK (source < target)` makes each undirected edge one row and excludes self-loops, which Knotrel rejects. |
| `edges.enabled` | edge lifecycle flag `enabled` |

The graph is the complete registered topology: every node row (including
isolated and inactive nodes) and every edge row (including disabled edges).
Connectivity in Knotrel is computed on the effective graph (active nodes,
enabled edges between active nodes), as in
[topology-lifecycle.md](../topology-lifecycle.md).

Changes are translated row by row, from the change record alone (no feeder
state), so that the same transaction always produces the same events:

| Change | Events, in order |
| --- | --- |
| `INSERT nodes (id, active)` | `add_node(id)`; then `set_node_active(id, false)` if `active` is false |
| `UPDATE nodes`, same `id` | `set_node_active(id, active)` (a no-op in Knotrel if unchanged) |
| `UPDATE nodes`, `id` changed | `remove_node(old)`, `add_node(new)`, then `set_node_active(new, false)` if inactive. With the reference foreign keys (`NO ACTION` on update) an `id` with edges cannot change; with `ON UPDATE CASCADE` the cascaded edge updates follow and are translated below |
| `DELETE nodes` | `remove_node(id)` (removes its edges in Knotrel) |
| `INSERT edges (s, t, enabled)` | `link(s, t)`; then `set_edge_enabled(s, t, false)` if `enabled` is false |
| `UPDATE edges`, same key | `set_edge_enabled(s, t, enabled)` |
| `UPDATE edges`, key changed | `cut(old_s, old_t)`, `link(s, t)`, then `set_edge_enabled(s, t, false)` if disabled |
| `DELETE edges` | `cut(s, t)` |
| `TRUNCATE` of either table | no events: the feeder **rebuilds** from a new snapshot (§8.7) |

Why this is exact under Knotrel's rules:

- `ON DELETE CASCADE` deletes the edge rows after their node row in the same
  transaction. Knotrel's `remove_node` already removed those edges, and `cut`
  of an edge whose endpoint is unregistered is a no-op, so the cascaded
  `cut` events change nothing.
- `link` registers missing endpoints as active and never reactivates an
  existing node. With deferred foreign keys an edge row can precede its node
  rows in a transaction; the later `add_node` is then a no-op and the
  explicit `set_node_active(id, false)` still applies an inactive flag.
- `set_node_active` and `set_edge_enabled` with the value already set are
  no-ops (`changed: false`): they advance the source position, not the
  revision.

### 4.3 Validation and unsupported changes

At startup the feeder checks, and refuses to start with an error naming the
problem:

- both tables exist and the publication contains exactly these two tables,
  publishes `insert, update, delete, truncate`, and has no row filter or
  column list;
- the columns `id`, `active`, `source`, `target`, `enabled` exist with types
  `bigint` and `boolean` and are `NOT NULL`;
- primary keys are `nodes (id)` and `edges (source, target)`, replica
  identity is `DEFAULT` or `FULL`;
- `edges` has the check `source < target`, `nodes` has `id >= 0`.

While streaming, PostgreSQL sends a relation description before the first
change after any DDL. The feeder validates it the same way. A dropped,
renamed or retyped required column, a renamed table, or a changed replica
identity stops the feeder with exit code 3 and a message such as:

```text
unsupported schema change on knotrel_graph.nodes: column "active" has type integer (OID 23), expected boolean
```

The feeder confirms nothing to PostgreSQL past the last applied change, so
Knotrel keeps an exact prefix and the slot keeps every later change. Those
later changes cannot simply be resumed, though: logical decoding describes
each change with the catalog as it was when the change was written, so
changes made under the unsupported schema decode the same way again. The
recovery is a rebuild:

1. fix or revert the schema;
2. with the feeder stopped, run `knotrel-pg-feeder drop-slots`;
3. start the feeder. The slot named in Knotrel's position is gone, so it
   declares a gap and rebuilds from a fresh snapshot (§8.8).

Added columns are accepted and ignored. A value outside the mapping (a
negative `id`, a `NULL` flag, a self-loop) is reported the same way, should
a schema without the reference constraints let it through.

## 5. Bootstrap: one slot, one snapshot, no gap

1. On the replication connection: `IDENTIFY_SYSTEM` (system identifier,
   timeline), then
   `CREATE_REPLICATION_SLOT <slot> LOGICAL pgoutput (SNAPSHOT 'export')`.
   PostgreSQL returns the slot's **consistent point** `L0` and an exported
   snapshot name.
2. On the normal connection, before any other command on the replication
   connection: `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
   SET TRANSACTION SNAPSHOT '<name>';` then read both tables.
3. Import the image into Knotrel at source position `S = (slot, L0)` (§6.1):
   `create_only` on a new instance, `replace` with `If-Match` otherwise.
4. `START_REPLICATION` on the slot.

**No-gap argument.** PostgreSQL guarantees that the exported snapshot
contains exactly the transactions committed before the slot's consistent
point, and that the slot decodes exactly the transactions committing after
it. The image (step 2) and the stream (step 4) therefore partition the
committed history at `L0`: no transaction is in both, none in neither. Writes
during the bootstrap are simply streamed after the import. Rolled-back
transactions are never decoded and never reach Knotrel.

## 6. Identities and positions

### 6.1 Tokens sent to Knotrel

| Knotrel field | Value |
| --- | --- |
| `source_id` | `pg;sys=<system identifier>;db=<database>;pub=<publication>` |
| `source_epoch` | `pg1;sys=<system identifier>;tli=<timeline>;slot=<slot name>` |
| `source_position` | `<source_epoch>;lsn=<commit LSN>;i=<n>` plus `;end` on the last event of a transaction; the snapshot position is `<source_epoch>;lsn=<L0>;i=0;end` |
| `delivery_sequence` | 1, 2, 3, … after each import, as the contract requires |
| `event_id` | `<commit LSN>.<n>` (unique in the stream; Knotrel resets its deduplication window on import) |
| `feeder_session_id` | `pgfeeder-<random>`, new for each process |
| `feeder_epoch` | wall-clock milliseconds at registration, strictly increasing in a process (A4) |

Slot names are `<prefix>_<suffix>` with a fresh random suffix per bootstrap
(prefix `knotrel_feeder` by default). Because the slot name is part of
`source_epoch`, every bootstrap starts a new source lineage, and an event
from an older lineage can never be accepted by a graph imported from a newer
one (`source_epoch_mismatch`).

Knotrel treats positions as opaque. The feeder parses its own positions: in
one lineage, `(commit LSN, n)` is totally ordered, because logical decoding
emits transactions in commit order and commit LSNs increase.

### 6.2 Events per transaction

A transaction with commit LSN `C` that touches the graph tables becomes the
events `(C, 1) … (C, k)` in the order of its changes. Transactions with no
graph change produce no event. The feeder sends events in batches of at most
1024 (and under Knotrel's 1 MiB request limit), each event carrying its
predecessor's position, so Knotrel can verify continuity across batches.

### 6.3 What a reader can rely on

Knotrel applies events one at a time under its graph lock. Every response
carries the applied position (`X-Knotrel-Source-Position`, `ETag`) captured
under the same lock as the answer. Therefore:

- every answer is computed on the state after some prefix of the change
  stream, at event granularity;
- if that position ends with `;end`, the prefix ends on a transaction
  boundary and the graph equals the source as of commit LSN `C`;
- otherwise the answer reflects a partly applied transaction. Example: a
  transaction that deletes edge `a–b` and inserts edge `a–c` may be observed
  with neither edge.

There is no multi-event atomicity in Knotrel's feeder protocol, so the feeder
cannot offer more. Applications that need transaction-consistent answers can
check the position marker and retry.

## 7. Confirming progress to PostgreSQL

The feeder reports a flushed position to the slot (standby status update)
only under this rule:

> **R1.** Report `F` only if every graph change of every transaction whose
> commit end LSN is at most `F` has been acknowledged by Knotrel (a 200
> applied or duplicate response, or a frontier read showing it applied), in
> the current Knotrel instance and generation.

In practice the feeder reports the commit end LSN of a transaction after
Knotrel acknowledged its last event, and, when nothing is pending, the
`wal_end` of keepalives so an idle graph does not pin WAL. It never reports a
position for a transaction it has only partly sent.

Consequence: the slot's confirmed position never passes Knotrel's applied
frontier in the same lineage. After any disconnection, PostgreSQL resends
from the confirmed position; the feeder skips what Knotrel already holds
(§8.2), so nothing is lost and nothing is applied twice.

R1 does **not** make the slot a record of what Knotrel holds: a restarted
Knotrel has lost everything regardless of the slot (§8.4).

## 8. Failures and recovery

### 8.1 Knotrel request fails or its answer is lost

A batch request that times out or loses its connection has an uncertain
outcome. The feeder does not resend blindly: it re-registers the same session
and epoch (idempotent), whose response reports Knotrel's applied delivery
sequence and position, and resends only the events after that frontier. An
exact resend of an applied batch would also be answered `duplicate` by
Knotrel's window; the feeder does not rely on that.

| Knotrel answer | Feeder action |
| --- | --- |
| 200 applied / duplicate | advance; confirm finished transactions (R1) |
| 503 `busy` / `import_in_progress`, timeout, connection error | back off (bounded exponential), reconcile frontier, resend the rest |
| 409 `stale_instance_id` | Knotrel restarted: rebuild (§8.4) |
| 409 `fenced_feeder_session` | another feeder took over: stop (§8.6) |
| 409 `lease_expired` | register a new epoch, reconcile, continue; barrier later |
| 409 `out_of_order_sequence`, `source_predecessor_mismatch`, `conflicting_duplicate`, `graph_desynchronized`, `source_epoch_mismatch` | divergence: rebuild (replace import) |
| 422 `limit_exceeded` | configuration error (Knotrel capacity): stop with exit code 4, confirm nothing |
| 422 other domain errors (`unknown_edge`, …) | divergence: declare a gap, rebuild |

### 8.2 PostgreSQL connection lost

The feeder reconnects with bounded backoff and starts replication on the same
slot. PostgreSQL resumes at the slot's confirmed position, which by R1 is at
or before Knotrel's frontier. The feeder regenerates each resent
transaction's events (§4.2 is deterministic) and skips every event at or
before Knotrel's applied position `(C, n)`; transactions entirely before it
are skipped whole.

### 8.3 Feeder crash

The new process registers a session with a higher epoch. The response gives
Knotrel's generation, applied sequence `D`, applied position `P` and status.
The feeder **resumes** on the slot named in `P` only if all of these hold:

1. Knotrel's status is `catching_up`, `ready` or `stale` and its generation is
   at least 1;
2. `P` parses as this feeder's format, and its system identifier and timeline
   match `IDENTIFY_SYSTEM`;
3. the slot named in `P` exists, belongs to this database, uses `pgoutput`,
   is not invalidated (`wal_status <> 'lost'`).

Otherwise it **rebuilds**. When it resumes, it streams from the slot and
continues at `D + 1` with predecessor `P`, skipping events at or before `P`
as in §8.2. By R1, the slot cannot have advanced past what Knotrel applied.

### 8.4 Knotrel restart (state lost)

Any `409 stale_instance_id`, or a different `instance_id` in `GET /v1/info`,
means the graph is gone. The feeder always rebuilds: new slot, new exported
snapshot, `create_only` import on the new instance, then streaming and a
barrier. The old slot's position is not reused, even though it is valid,
because it describes what the *old* process applied.

### 8.5 Lease expiry

The feeder heartbeats at a third of the lease, also during a bootstrap
(reading and importing a large image can take longer than the lease). If
Knotrel reports `stale` (lease expired during a pause), the feeder registers a
higher epoch, catches up and declares a new barrier, as §4.3 of the contract
requires. A heartbeat answer for an earlier epoch, arriving after the new one
was registered, is ignored.

### 8.6 Takeover

`409 fenced_feeder_session` means a feeder with a higher epoch owns the
server. The feeder stops (exit code 5) without confirming anything further to
PostgreSQL; it does not fight for the lease.

### 8.7 Rebuild

Used after a Knotrel restart, a divergence, a `TRUNCATE`, a lost slot or a
lineage mismatch:

1. stop streaming (the old slot is released);
2. bootstrap a new slot with an exported snapshot (§5);
3. import with `create_only` (generation 0) or `replace` with the current
   `ETag` as `If-Match`;
4. drop the feeder's other slots (§9), stream, barrier.

During a replacement Knotrel keeps serving the previous graph until the new
one is published. If the feeder dies in the middle, the next start either
resumes the old lineage (if Knotrel still holds it and its slot still exists)
or rebuilds again; an unused new slot is removed by the next cleanup.

### 8.8 History no longer available

If the slot is invalidated (its WAL exceeded `max_slot_wal_keep_size` and was
removed), PostgreSQL refuses to stream from it. The feeder then declares a
gap to Knotrel (`POST /v1/feeder/gap`, Knotrel becomes `desynchronized` and
answers queries with 503 rather than serve a graph that misses changes) and
rebuilds. The same applies when the slot named in Knotrel's position is
missing, or the timeline changed.

### 8.9 Readiness

After an import, a new epoch or a lease recovery, the feeder declares a
barrier at Knotrel's applied frontier once it is caught up: no transaction
in progress, every received event applied, and either

- it has received a WAL position (commit end or keepalive) at or past
  `pg_current_wal_lsn()` sampled when the barrier became necessary, so every
  transaction committed before that moment is applied; or
- the replication stream has been idle for a short interval (default
  500 ms).

The first condition is reached under continuous writes, where the stream is
never idle. Knotrel then becomes `ready`. After registering a new epoch the
feeder expects `stale` until this barrier and does not treat it as a new
lapse.

## 9. Slots, retained WAL and limits

- **Ownership.** The feeder creates slots named `<prefix>_<suffix>` and
  considers every slot with that prefix in its database its own. It drops
  such a slot only when it is inactive and is not the slot it streams from.
  It never drops a slot without its prefix. The prefix must be reserved for
  one feeder (one Knotrel server) per database.
- **Retention bound.** A slot retains WAL until confirmed. The feeder refuses
  to start if `max_slot_wal_keep_size` is `-1` (unbounded), unless
  `KNOTREL_FEEDER_ALLOW_UNBOUNDED_WAL=true` is set. With a bound, a stalled feeder
  costs at most that much disk; past it PostgreSQL invalidates the slot and
  the feeder rebuilds (§8.8).
- **Idle graph.** Keepalive confirmation (§7) lets the slot advance when the
  graph tables do not change.
- **Monitoring and cleanup.** Every `KNOTREL_FEEDER_SLOT_CHECK_SECS`
  (default 30 s) the feeder logs the retained WAL of its slot
  (`pg_current_wal_lsn() - confirmed_flush_lsn`) and its `wal_status`, warns
  at `unreserved`, and drops leftover inactive slots of its prefix.
- **Decommissioning.** `knotrel-pg-feeder drop-slots` removes the feeder's
  inactive slots. A feeder that is stopped for good must be cleaned up this
  way, or its slot keeps WAL up to the bound.
- **Memory.** The image is held in memory once while it is imported (bounded
  by Knotrel's `KNOTREL_MAX_IMPORT_BODY_BYTES`, default 64 MiB). While
  streaming, the feeder holds at most one batch (1024 events).

## 10. Privileges and credentials

- PostgreSQL roles ([`sql/roles.sql`](../../integrations/postgres-feeder/sql/roles.sql)):
  the **feeder role** has `LOGIN REPLICATION`, `USAGE` on the schema and
  `SELECT` on the two tables, nothing else. It does not own the tables, does
  not create the publication and cannot write. The table owner creates the
  publication.
- `REPLICATION` lets a role drop any slot of the cluster; the prefix rule of
  §9 is what keeps the feeder away from others.
- The password is read from a file (`KNOTREL_PG_PASSWORD_FILE`) or from the
  environment, never from the command line, and is never logged. Connection
  strings in logs are printed without passwords.

## 11. Correctness criteria

Each criterion is checked by tests against a real PostgreSQL server (§12).

- **C1 Convergence.** When writes stop and the feeder is caught up, Knotrel's
  registered topology (nodes with `active`, edges with `enabled`) equals the
  projection of the PostgreSQL tables, and Knotrel's applied position is the
  last graph transaction's position with `;end`.
- **C2 No loss window.** C1 holds when writes run concurrently with the
  bootstrap (before, during and after the snapshot and the import).
- **C3 Rolled-back work is invisible.** Changes of aborted transactions never
  reach Knotrel.
- **C4 Safe confirmation.** At every observed moment, the slot's confirmed
  position never passes the end of the last transaction Knotrel has fully
  applied (R1), and C1 holds after a feeder crash at any of these points:
  after the import, mid-transaction between batches, after a batch was
  applied but before it was confirmed, after confirmation.
- **C5 Disconnections.** C1 holds after the replication connection is
  terminated during streaming, and after Knotrel answers are lost.
- **C6 Knotrel restart.** After Knotrel restarts, the feeder rebuilds on the
  new instance (new slot, `create_only`), and C1 holds; it never streams
  events to the new instance without an import.
- **C7 Lost history.** After the slot is invalidated, Knotrel is
  `desynchronized` until the rebuild completes, then C1 holds.
- **C8 Bounded slots.** After any test, at most one feeder slot exists;
  slots without the prefix are never dropped; the feeder refuses unbounded
  retention unless overridden.
- **C9 Unsupported schema change.** Dropping or retyping a required column
  stops the feeder with exit code 3 and the message of §4.3, and nothing past
  the last applied change is confirmed; after the schema is fixed, the
  recovery of §4.3 converges (C1).

## 12. Tests

Integration tests (`integrations/postgres-feeder/tests/`) start a real
`knotrel-server` binary and the feeder binary as child processes against a
PostgreSQL server provided by the environment (a container in CI and in the
local script). They connect as an administrator to create the schema, the
roles and the publication, and run the feeder with the least-privileged role.
Fault points are injected with an environment variable that makes the feeder
abort at a named point, or discard a Knotrel answer as if the connection had
dropped (compiled in all builds, inert unless set).

| Test (`tests/`) | Criteria |
| --- | --- |
| `initial_load_with_concurrent_writers`: four writers before, during and after the bootstrap | C1, C2 |
| `rolled_back_and_mixed_transactions`: aborted transactions, savepoints, one 3000-change transaction in batches of 37 | C1, C3 |
| `steady_streaming_with_concurrent_writers`: six writers with lifecycle changes, cascades, node and edge key updates; deferred foreign keys | C1 |
| `feeder_crash_at_each_fault_point`: crash after slot creation, after the snapshot read, after the import, before a batch, after a batch was applied, after a confirmation, and inside a transaction; then restart | C1, C4 |
| `disconnections_and_lost_answers`: replication connection terminated three times under writes; a Knotrel answer discarded | C1, C5 |
| `knotrel_restart_rebuilds`: Knotrel killed and restarted under writes | C1, C6, C8 |
| `slot_invalidated_declares_gap_and_rebuilds`: 200 MB of WAL with a 64 MB bound while the feeder is stopped | C7, C1 |
| `truncate_rebuilds` | C1 |
| `foreign_slots_untouched_and_slot_count_bounded`, `unbounded_retention_is_refused` | C8 |
| `retyped_column_stops_then_recovers_by_rebuild`, `dropped_column_stops_with_clear_error`, `startup_rejects_unsupported_schema` | C9 |

**Comparison method.** The tests stop the writers, read
`pg_current_wal_lsn()`, wait until the feeder has confirmed at least that
position and Knotrel is `ready`, then read the full registered topology from
Knotrel's inspection routes (`KNOTREL_INSPECTION=true`) and the tables from
PostgreSQL, and compare them. Because no write happens after that LSN, the
tables are the projection at the verified position.

The CI job runs these tests on PostgreSQL 15, 16, 17, 18 and 19.

## 13. Limits

- One database, one publication, one Knotrel server per feeder.
- Graph size is bounded by Knotrel's import body limit and graph limits.
- No PostgreSQL transaction atomicity (§6.3).
- A full rebuild re-reads the whole graph; its cost grows with the graph.
- No TLS in this version (A7). No failover support (A6).
- No performance claim: this reference integration has not been measured.
