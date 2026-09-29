# Immutable graph cardinality limits

Status: implemented. Compact BFS remains default.

The approved goal is to control graph growth uniformly across engines, with
atomic rejection of an individual operation. Hierarchical graphs and domain
node types remain future design work; limits apply to one graph instance.

`GraphLimits` contains optional `usize` node/edge ceilings. None is unlimited;
zero admits no new corresponding entries. `ConnectivityGraph::with_limits`
constructs an empty bounded instance, and exposes its immutable limits.
Existing constructors and conversion from `Graph` remain unlimited. Raw
algorithm types remain unrestricted for compatibility and benchmarking.

The wrapper validates self-loops, counts absent endpoints without traversal,
checks remaining node capacity using saturating subtraction, then checks edge
capacity. At the edge ceiling it uses the backend's ordered membership index
to preserve duplicate no-op semantics. All validation precedes insertion, so
rejected links cannot leak endpoints or affect connectivity. The private
backend cannot be mutated around the checks. Constant additional storage and
O(log V) ordered membership overhead preserve existing asymptotic operation
bounds. No speculative mutation/rollback or duplicate shadow index is used.

`ConnectivityGraph::add_node` changes from bool to Result<bool, GraphError>.
This is a source-level API change for wrapper callers; raw algorithm add_node
methods stay unchanged. GraphError gains node/edge capacity variants.

The server snapshots optional MAX_NODES/MAX_EDGES environment variables with
the other KNOTREL settings. Invalid values fail before listening; missing
values stay unlimited to preserve existing deployments. Settings are available
in /v1/info and startup logs. HTTP 422 and stable per-operation error codes
follow the existing semantic-error convention. The existing graph mutex covers
checks and mutation, including ordered non-transactional batches.

Tests cover all backends, full-width IDs, zero capacity, duplicate operations,
both-new endpoint rejection, node retention after cuts, edge capacity reuse,
environment validation, batch error continuation and concurrent admissions.
Cardinality limits cannot guarantee allocator capacity, RSS or latency ceilings.
