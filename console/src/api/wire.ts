/**
 * Wire types derived from the generated OpenAPI definitions.
 *
 * `generated/openapi.ts` is produced from `docs/openapi.yaml` and only describes
 * shapes; it validates nothing at runtime. The decoders in `decode.ts` perform the
 * runtime checks and convert integers to exact decimal strings.
 */
import type { components } from './generated/openapi';

type Schemas = components['schemas'];

/** A single request body accepted by `POST /v1/operations`. */
export type Operation = Schemas['Operation'];

/** The `op` discriminator values of {@link Operation}. */
export type OperationName = Operation['op'];

/** Machine-readable server error codes. */
export type ErrorCode = Schemas['ErrorCode'];

/** Source-managed synchronization status. */
export type SyncStatus = Schemas['FeederSyncStatus'];

/** Every `/ready` status label, in both modes. */
export type ReadinessLabel =
  Schemas['ReadinessReadyResponse']['status'] | Schemas['ReadinessNotReadyResponse']['status'];

/** Connectivity engine names. */
export type EngineName = Schemas['ServerInfo']['engine'];

/** All known error codes, kept in sync with the OpenAPI enumeration by a unit test. */
export const ERROR_CODES = [
  'barrier_not_reached',
  'barrier_position_mismatch',
  'busy',
  'conflicting_duplicate',
  'desynchronized',
  'direct_mutation_disabled',
  'edge_limit_exceeded',
  'event_failed',
  'fenced_feeder_session',
  'generation_exhausted',
  'graph_already_initialized',
  'graph_desynchronized',
  'graph_uninitialized',
  'import_in_progress',
  'inspection_disabled',
  'inspection_unsupported',
  'invalid_batch_size',
  'invalid_cursor',
  'invalid_edge',
  'invalid_graph_id',
  'invalid_instance_id',
  'invalid_lease_duration',
  'invalid_node',
  'invalid_request',
  'lease_expired',
  'loading',
  'mode_conflict',
  'node_limit_exceeded',
  'out_of_order_sequence',
  'payload_too_large',
  'precondition_failed',
  'request_timeout',
  'self_loop',
  'sequence_exhausted',
  'session_required',
  'shutting_down',
  'source_epoch_mismatch',
  'source_mismatch',
  'source_predecessor_mismatch',
  'stale_generation',
  'stale_instance_id',
  'superseded_barrier',
  'too_many_connections',
  'topology_lifecycle_disabled',
  'unavailable',
  'uninitialized',
  'unknown_edge',
  'unknown_node',
  'unverifiable_duplicate',
] as const satisfies readonly ErrorCode[];

/** Every synchronization status value. */
export const SYNC_STATUSES = [
  'uninitialized',
  'loading',
  'catching_up',
  'ready',
  'stale',
  'desynchronized',
] as const satisfies readonly SyncStatus[];

/** Every readiness label. */
export const READINESS_LABELS = [
  'ready',
  'starting',
  'draining',
  'unavailable',
  ...SYNC_STATUSES.filter((s) => s !== 'ready'),
] as const satisfies readonly ReadinessLabel[];

/** Every engine name. */
export const ENGINES = ['compact-bfs', 'ett', 'hdt'] as const satisfies readonly EngineName[];
