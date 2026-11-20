/**
 * What the console may offer for a given server configuration.
 *
 * Capabilities are derived only from `GET /v1/info`. Until it has been read, nothing
 * beyond diagnostics is offered.
 */
import type { ServerInfo, ServerMode } from '../api/decode';
import type { OperationName } from '../api/wire';

/** Operations that change topology. */
export const MUTATIONS = [
  'add_node',
  'link',
  'cut',
  'set_node_active',
  'set_edge_enabled',
  'remove_node',
] as const satisfies readonly OperationName[];

/** Operations that only need lifecycle support. */
export const LIFECYCLE_OPERATIONS = [
  'set_node_active',
  'set_edge_enabled',
  'remove_node',
  'node_state',
  'edge_state',
] as const satisfies readonly OperationName[];

/** Capability set offered by the UI. */
export interface Capabilities {
  /** Known mode, or `undefined` before `/v1/info` has been read. */
  mode: ServerMode | undefined;
  /** Lifecycle support. */
  lifecycle: boolean;
  /** Whether direct topology mutations may be offered. */
  canMutate: boolean;
  /** Operations that may be sent from the UI. */
  allowed: ReadonlySet<OperationName>;
  /** Reason mutations are unavailable, for display. */
  mutationBlockedReason: string | undefined;
}

/** Derives the capability set. */
export function deriveCapabilities(info: ServerInfo | undefined): Capabilities {
  if (!info) {
    return {
      mode: undefined,
      lifecycle: false,
      canMutate: false,
      allowed: new Set(),
      mutationBlockedReason: 'Waiting for the server configuration (GET /v1/info).',
    };
  }
  const allowed = new Set<OperationName>(['connected']);
  const canMutate = info.mode === 'standalone';
  if (info.topologyLifecycle) {
    allowed.add('node_state');
    allowed.add('edge_state');
  }
  if (canMutate) {
    for (const op of MUTATIONS) {
      if (info.topologyLifecycle || !(LIFECYCLE_OPERATIONS as readonly string[]).includes(op)) {
        allowed.add(op);
      }
    }
  }
  return {
    mode: info.mode,
    lifecycle: info.topologyLifecycle,
    canMutate,
    allowed,
    mutationBlockedReason: canMutate
      ? undefined
      : 'This server is source-managed. Topology changes belong in the authoritative source and are delivered by its feeder; the console offers diagnostics only.',
  };
}
