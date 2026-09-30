# Coherent graph snapshots and instance-local revisions

Status: implemented, 2026-09-29.

The approved contract separates immutable GET /v1/info from live GET /v1/stats.
Stats contains node_count, edge_count, max_nodes/max_edges, remaining_nodes/
remaining_edges and a decimal-string state_version. Unlimited capacities are
null. The revision starts at zero per service instance, including a service
wrapping populated state, and counts effective operations rather than requests
or topology differences. No-op/error outcomes and reads do not increment it.

The service owns GraphState (graph plus decimal revision) under one mutex.
Every Operation::apply result of Changed(true) increments the revision before
releasing that mutex. One guard still spans each batch. Snapshots capture all
fields under that guard, then serialize their owned values after release.
The graph algorithms and core public API have no new bookkeeping.

A shared run helper owns worker admission, blocking execution, locking, poison
handling and permit lifetime for both mutations and snapshots. Canceled callers
cannot prematurely free worker capacity. Saturation returns the existing 503
busy; poisoned workers fail closed as unavailable. Info bypasses this helper.

Little-endian decimal digits avoid silently wrapping/saturating a finite integer
and avoid JSON-number precision loss. Revision increment has amortized O(1)
work (worst-case O(log M) carry after M mutations); storage and string conversion
are O(log M). The snapshot does not traverse the graph or collect heap/storage
diagnostics. This revision is not persisted, not a restart identifier and not
a conditional-write token. HTTP no-store prevents storing stats responses.

Tests cover every engine, capacity rejection, duplicates, missing edges, queries,
malformed batches, implicit endpoint insertion, net-zero topology changes,
legacy populated graphs, independent instances and concurrent batches/snapshots.
Worker tests exercise saturation with info available, poisoned locks, admitted
read cancellation, exclusion of intermediate locked states, decimal carries and
values beyond JavaScript and u64 integer ranges.
