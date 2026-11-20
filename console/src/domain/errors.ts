/**
 * Operator-facing explanations for server error codes and request failures.
 *
 * The `Record<ErrorCode, string>` type makes the compiler reject a missing code
 * whenever the OpenAPI enumeration (and therefore the generated type) changes.
 */
import type { Failure } from '../api/endpoints';
import type { ServerError } from '../api/decode';
import { isKnownErrorCode } from '../api/decode';
import type { ErrorCode } from '../api/wire';

const EXPLANATIONS: Record<ErrorCode, string> = {
  barrier_not_reached: 'The feeder barrier was ahead of the applied stream.',
  barrier_position_mismatch: 'The feeder barrier source position did not match.',
  busy: 'The server admission queue was full. The request was not admitted; retry later.',
  conflicting_duplicate: 'A redelivered feeder event conflicted with history.',
  desynchronized: 'The graph is desynchronized from its source; a full rebuild is required.',
  direct_mutation_disabled:
    'The server is source-managed: topology changes must go through the authoritative source and its feeder.',
  edge_limit_exceeded: 'The configured edge ceiling would be exceeded. Nothing was changed.',
  event_failed: 'A feeder event failed.',
  fenced_feeder_session: 'A newer feeder session holds the lease.',
  generation_exhausted: 'The generation counter is exhausted; restart the server.',
  graph_already_initialized: 'The graph already has a published baseline.',
  graph_desynchronized: 'The graph is desynchronized; only a replacement import can recover it.',
  graph_uninitialized: 'No baseline has been imported yet.',
  import_in_progress: 'An import is in progress; retry after it completes.',
  inspection_disabled:
    'The data browser needs the inspection routes: start the server with KNOTREL_INSPECTION=true.',
  inspection_unsupported:
    'This engine keeps no per-node adjacency (ett, hdt): incident and edge listing need compact-bfs.',
  invalid_batch_size: 'The batch size is outside the accepted range.',
  invalid_cursor: 'The listing position is no longer valid; restart the listing.',
  invalid_edge: 'The edge is invalid.',
  invalid_graph_id: 'Only the graph ID "default" is supported.',
  invalid_instance_id: 'The instance ID header is missing or invalid.',
  invalid_lease_duration: 'The feeder lease duration is outside the accepted range.',
  invalid_node: 'The node is invalid.',
  invalid_request: 'The server rejected the request shape or a node ID.',
  lease_expired: 'The feeder lease expired.',
  loading: 'A baseline is loading.',
  mode_conflict: 'The endpoint is not available in the current server mode.',
  node_limit_exceeded: 'The configured node ceiling would be exceeded. Nothing was changed.',
  out_of_order_sequence: 'A feeder delivery sequence arrived out of order.',
  payload_too_large: 'The request body exceeded the server limit.',
  precondition_failed: 'The replacement precondition (If-Match) did not match.',
  request_timeout:
    'The server time limit expired. The effect is unknown: an admitted worker may still apply the change.',
  self_loop: 'Self-loops are not allowed: source and target must differ.',
  sequence_exhausted: 'The feeder delivery sequence is exhausted.',
  session_required: 'A feeder session is required.',
  shutting_down: 'The server is draining for shutdown and refused the request.',
  source_epoch_mismatch: 'The source epoch changed; a rebuild is required.',
  source_mismatch: 'The source identity did not match.',
  source_predecessor_mismatch: 'The feeder predecessor position did not match.',
  stale_generation: 'The request referred to an older generation.',
  stale_instance_id: 'The request referred to a previous server instance.',
  superseded_barrier: 'A newer barrier superseded this one.',
  topology_lifecycle_disabled:
    'Node and edge lifecycle support is disabled on this server (KNOTREL_TOPOLOGY_LIFECYCLE).',
  unavailable: 'The graph worker is unavailable; the server must be restarted.',
  uninitialized: 'The server is not initialized.',
  unknown_edge: 'The edge is not registered.',
  unknown_node: 'The node is not registered.',
  unverifiable_duplicate: 'A redelivered feeder event can no longer be verified.',
};

/** Returns the explanation of a server error, including undocumented codes. */
export function explainServerError(error: ServerError): string {
  return isKnownErrorCode(error.code)
    ? EXPLANATIONS[error.code]
    : `Undocumented error code "${error.code}"; this console may be older than the server.`;
}

/** A displayable summary of a failure. */
export interface FailureSummary {
  /** Short heading. */
  title: string;
  /** Plain-text explanation. */
  detail: string;
  /** Server-provided message, if any; render as text only. */
  serverMessage?: string;
  /** Error code, if any. */
  code?: string;
}

/** Summarizes any request failure for display. */
export function summarizeFailure(failure: Failure): FailureSummary {
  switch (failure.kind) {
    case 'console-unreachable':
      return {
        title: 'Console host unreachable',
        detail:
          'The browser could not reach the web server hosting the console. Check that it is still running.',
      };
    case 'server-unreachable':
      return {
        title: 'Knotrel server unreachable',
        detail:
          failure.delivered === 'no'
            ? 'The gateway could not connect to the configured server. The request was not delivered.'
            : 'The connection to the server failed after the request may have been sent.',
      };
    case 'timeout':
      return {
        title: 'No response in time',
        detail:
          failure.where === 'client'
            ? 'The browser stopped waiting for a response.'
            : 'The gateway stopped waiting for the server.',
      };
    case 'server-error':
      return {
        title: `Server error ${failure.status}`,
        detail: explainServerError(failure.error),
        serverMessage: failure.error.message,
        code: failure.error.code,
      };
    case 'gateway-rejected':
      return {
        title: 'Rejected by the console gateway',
        detail: 'The gateway refused to forward this request.',
        serverMessage: failure.message,
      };
    case 'http':
      return {
        title: `HTTP ${failure.status}`,
        detail:
          failure.status === 404
            ? 'The endpoint is not available through this console host.'
            : 'The response had no error body.',
      };
    case 'incompatible':
      return {
        title: 'Incompatible response',
        detail: `The response does not match the documented API contract (${failure.reason}).`,
      };
    case 'aborted':
      return { title: 'Cancelled', detail: 'The request was superseded by a newer one.' };
  }
}
