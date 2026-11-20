/**
 * Classification of mutation results and read-back planning.
 *
 * A mutation is sent once and never retried automatically. When the console
 * cannot know whether it was applied (timeouts, connection loss after sending,
 * server `request_timeout`, an undecodable success), the outcome is `uncertain`:
 * a timeout is not a rollback. The operator can then read the state back.
 */
import type { Result } from '../api/endpoints';
import type { OperationOutcome } from '../api/decode';
import type { Operation } from '../api/wire';

/** How a mutation ended, from the console's point of view. */
export type MutationVerdict =
  | { kind: 'effective'; stateVersion: string | undefined }
  | { kind: 'no-op'; stateVersion: string | undefined }
  | { kind: 'rejected'; code: string | undefined; reason: string }
  | { kind: 'uncertain'; reason: string };

/** Classifies the result of one mutation request. */
export function classifyMutation(result: Result<OperationOutcome>): MutationVerdict {
  if (result.ok) {
    const outcome = result.value;
    if (outcome.result !== 'changed') {
      return { kind: 'uncertain', reason: `unexpected "${outcome.result}" result for a mutation` };
    }
    return {
      kind: outcome.changed ? 'effective' : 'no-op',
      stateVersion: result.metadata.stateVersion,
    };
  }
  const { failure } = result;
  switch (failure.kind) {
    case 'server-error':
      if (failure.error.code === 'request_timeout') {
        return {
          kind: 'uncertain',
          reason: 'the server time limit expired; its effect is unknown',
        };
      }
      return { kind: 'rejected', code: failure.error.code, reason: failure.error.message };
    case 'server-unreachable':
      return failure.delivered === 'no'
        ? { kind: 'rejected', code: undefined, reason: 'the server was unreachable; not delivered' }
        : {
            kind: 'uncertain',
            reason: 'the connection failed after the request may have been sent',
          };
    case 'gateway-rejected':
      return { kind: 'rejected', code: undefined, reason: failure.message };
    case 'timeout':
      return { kind: 'uncertain', reason: `no response before the ${failure.where} timeout` };
    case 'console-unreachable':
      return { kind: 'uncertain', reason: 'the connection to the console host was lost' };
    case 'incompatible':
      return { kind: 'uncertain', reason: `the response could not be decoded (${failure.reason})` };
    case 'http':
      return failure.status >= 500
        ? { kind: 'uncertain', reason: `HTTP ${failure.status} without an error body` }
        : { kind: 'rejected', code: undefined, reason: `HTTP ${failure.status}` };
    case 'aborted':
      return { kind: 'uncertain', reason: 'the request was cancelled after it was sent' };
  }
}

/** A read that helps establish whether a mutation took effect. */
export interface ReadBack {
  /** Query to send. */
  query: Operation | undefined;
  /** What the operator should look for, including caveats. */
  hint: string;
}

/** Plans a read-back for an uncertain mutation. */
export function planReadBack(op: Operation, lifecycle: boolean): ReadBack {
  switch (op.op) {
    case 'add_node':
    case 'set_node_active':
    case 'remove_node':
      return lifecycle
        ? {
            query: { op: 'node_state', node: op.node },
            hint: 'Compare the node registration and active flag with the intended change.',
          }
        : {
            query: undefined,
            hint: 'Node state queries need lifecycle support. Check the node count and state version on the Overview page.',
          };
    case 'link':
    case 'cut':
    case 'set_edge_enabled':
      return lifecycle
        ? {
            query: { op: 'edge_state', source: op.source, target: op.target },
            hint: 'Compare the edge registration and flags with the intended change.',
          }
        : {
            query: { op: 'connected', source: op.source, target: op.target },
            hint: 'Connectivity can also come from other paths, so it does not prove the edge exists. Compare the edge count and state version on the Overview page.',
          };
    default:
      return { query: undefined, hint: 'Queries have no side effects; run it again.' };
  }
}

/** One-line human description of an operation, using exact IDs. */
export function describeOperation(op: Operation): string {
  switch (op.op) {
    case 'add_node':
      return `add node ${op.node}`;
    case 'link':
      return `link ${op.source} – ${op.target}`;
    case 'cut':
      return `cut ${op.source} – ${op.target}`;
    case 'connected':
      return `connected? ${op.source} – ${op.target}`;
    case 'set_node_active':
      return `${op.active ? 'activate' : 'deactivate'} node ${op.node}`;
    case 'set_edge_enabled':
      return `${op.enabled ? 'enable' : 'disable'} edge ${op.source} – ${op.target}`;
    case 'remove_node':
      return `remove node ${op.node}`;
    case 'node_state':
      return `state of node ${op.node}`;
    case 'edge_state':
      return `state of edge ${op.source} – ${op.target}`;
  }
}
