/**
 * Runtime validation of server responses.
 *
 * Every decoder accepts a lossless {@link JsonValue} and either returns a typed,
 * exact domain value or throws {@link DecodeError}. Integers are returned as
 * canonical decimal strings so that counts, generations and limits are never
 * rounded. Unknown extra fields are ignored (forward compatibility); missing,
 * mistyped or out-of-range fields are rejected.
 */
import { isJsonObject, JsonNumber, type JsonObject, type JsonValue } from './lossless-json';
import {
  ENGINES,
  ERROR_CODES,
  READINESS_LABELS,
  SYNC_STATUSES,
  type EngineName,
  type ErrorCode,
  type ReadinessLabel,
  type SyncStatus,
} from './wire';

/** Raised when a response does not match the documented contract. */
export class DecodeError extends Error {
  override readonly name = 'DecodeError';
}

/** Largest value of a JSON `u64` field. */
export const U64_MAX = 18446744073709551615n;

function fail(path: string, what: string): never {
  throw new DecodeError(`${path}: ${what}`);
}

function object(value: JsonValue | undefined, path: string): JsonObject {
  if (!isJsonObject(value)) {
    fail(path, 'expected an object');
  }
  return value;
}

function string(value: JsonValue | undefined, path: string): string {
  if (typeof value !== 'string') {
    fail(path, 'expected a string');
  }
  return value;
}

function boolean(value: JsonValue | undefined, path: string): boolean {
  if (typeof value !== 'boolean') {
    fail(path, 'expected a boolean');
  }
  return value;
}

/** Decodes a nonnegative JSON integer up to `max` into its exact decimal string. */
function unsigned(value: JsonValue | undefined, path: string, max: bigint = U64_MAX): string {
  if (!(value instanceof JsonNumber) || !value.isUnsignedInteger()) {
    fail(path, 'expected a nonnegative integer');
  }
  if (BigInt(value.raw) > max) {
    fail(path, 'integer out of range');
  }
  return value.raw;
}

function nullableUnsigned(value: JsonValue | undefined, path: string): string | null {
  return value === null ? null : unsigned(value, path);
}

function optional<T>(
  obj: JsonObject,
  key: string,
  path: string,
  decode: (value: JsonValue, path: string) => T,
): T | undefined {
  const value = obj[key];
  return value === undefined ? undefined : decode(value, `${path}.${key}`);
}

function oneOf<T extends string>(
  values: readonly T[],
  value: JsonValue | undefined,
  path: string,
): T {
  const text = string(value, path);
  if (!(values as readonly string[]).includes(text)) {
    fail(path, `unsupported value "${text}"`);
  }
  return text as T;
}

/** A decimal revision string such as `state_version`. */
export function decimalString(value: JsonValue | undefined, path: string): string {
  const text = string(value, path);
  if (!/^[0-9]+$/.test(text)) {
    fail(path, 'expected a decimal string');
  }
  return text;
}

/** Decoded `GET /health` body. */
export interface Health {
  /** Always `ok` for a live process. */
  status: 'ok';
}

/** Decodes the liveness body. */
export function decodeHealth(value: JsonValue): Health {
  const body = object(value, 'health');
  return { status: oneOf(['ok'] as const, body.status, 'health.status') };
}

/** Decoded `GET /ready` body. */
export interface Readiness {
  /** Readiness label reported by the server. */
  status: ReadinessLabel;
}

/** Decodes the readiness body (HTTP 200 or 503). */
export function decodeReadiness(value: JsonValue): Readiness {
  const body = object(value, 'ready');
  return { status: oneOf(READINESS_LABELS, body.status, 'ready.status') };
}

/** Server operating mode. */
export type ServerMode = 'standalone' | 'source_managed';

/** Decoded `GET /v1/info` body. */
export interface ServerInfo {
  /** Configured engine. */
  engine: EngineName;
  /** Whether the engine is experimental. */
  experimental: boolean;
  /** Admission capacity. */
  maxPendingJobs: string;
  /** Request body limit in bytes. */
  maxBodyBytes: string;
  /** Batch size limit. */
  maxBatchOperations: string;
  /** Node ceiling or `null` when unlimited. */
  maxNodes: string | null;
  /** Edge ceiling or `null` when unlimited. */
  maxEdges: string | null;
  /** Operating mode; the server omits `mode` in standalone mode. */
  mode: ServerMode;
  /** Instance ID; reported in the body only in source-managed mode. */
  instanceId: string | undefined;
  /** Whether node and edge lifecycle support is enabled. */
  topologyLifecycle: boolean;
  /** Whether the read-only inspection routes are enabled. */
  inspection: boolean;
}

/** Decodes the effective configuration. */
export function decodeInfo(value: JsonValue): ServerInfo {
  const body = object(value, 'info');
  const mode = optional(body, 'mode', 'info', (v, p) => oneOf(['source_managed'] as const, v, p));
  const lifecycle = optional(body, 'topology_lifecycle', 'info', boolean);
  if (lifecycle === false) {
    fail('info.topology_lifecycle', 'must be absent or true');
  }
  const inspection = optional(body, 'inspection', 'info', boolean);
  if (inspection === false) {
    fail('info.inspection', 'must be absent or true');
  }
  return {
    engine: oneOf(ENGINES, body.engine, 'info.engine'),
    experimental: boolean(body.experimental, 'info.experimental'),
    maxPendingJobs: unsigned(body.max_pending_jobs, 'info.max_pending_jobs'),
    maxBodyBytes: unsigned(body.max_body_bytes, 'info.max_body_bytes'),
    maxBatchOperations: unsigned(body.max_batch_operations, 'info.max_batch_operations'),
    maxNodes: nullableUnsigned(body.max_nodes, 'info.max_nodes'),
    maxEdges: nullableUnsigned(body.max_edges, 'info.max_edges'),
    mode: mode ?? 'standalone',
    instanceId: optional(body, 'instance_id', 'info', string),
    topologyLifecycle: lifecycle === true,
    inspection: inspection === true,
  };
}

/** Decoded `GET /v1/stats` body. */
export interface GraphStats {
  /** Registered nodes. */
  nodeCount: string;
  /** Registered edges. */
  edgeCount: string;
  /** Active nodes (lifecycle support only). */
  activeNodeCount: string | undefined;
  /** Enabled edges (lifecycle support only). */
  enabledEdgeCount: string | undefined;
  /** Effective edges (lifecycle support only). */
  effectiveEdgeCount: string | undefined;
  /** Node ceiling or `null`. */
  maxNodes: string | null;
  /** Edge ceiling or `null`. */
  maxEdges: string | null;
  /** Remaining node capacity or `null`. */
  remainingNodes: string | null;
  /** Remaining edge capacity or `null`. */
  remainingEdges: string | null;
  /** Exact local revision. */
  stateVersion: string;
  /** Instance ID (source-managed body only). */
  instanceId: string | undefined;
  /** Generation (source-managed body only), exact. */
  generation: string | undefined;
  /** Raw applied source position (source-managed, once one exists). */
  sourcePosition: string | undefined;
  /** Synchronization status (source-managed only). */
  syncStatus: SyncStatus | undefined;
}

/** Decodes the live graph snapshot. */
export function decodeStats(value: JsonValue): GraphStats {
  const body = object(value, 'stats');
  return {
    nodeCount: unsigned(body.node_count, 'stats.node_count'),
    edgeCount: unsigned(body.edge_count, 'stats.edge_count'),
    activeNodeCount: optional(body, 'active_node_count', 'stats', (v, p) => unsigned(v, p)),
    enabledEdgeCount: optional(body, 'enabled_edge_count', 'stats', (v, p) => unsigned(v, p)),
    effectiveEdgeCount: optional(body, 'effective_edge_count', 'stats', (v, p) => unsigned(v, p)),
    maxNodes: nullableUnsigned(body.max_nodes, 'stats.max_nodes'),
    maxEdges: nullableUnsigned(body.max_edges, 'stats.max_edges'),
    remainingNodes: nullableUnsigned(body.remaining_nodes, 'stats.remaining_nodes'),
    remainingEdges: nullableUnsigned(body.remaining_edges, 'stats.remaining_edges'),
    stateVersion: decimalString(body.state_version, 'stats.state_version'),
    instanceId: optional(body, 'instance_id', 'stats', string),
    generation: optional(body, 'generation', 'stats', (v, p) => unsigned(v, p)),
    sourcePosition: optional(body, 'source_position', 'stats', string),
    syncStatus: optional(body, 'sync_status', 'stats', (v, p) => oneOf(SYNC_STATUSES, v, p)),
  };
}

/** Decoded successful outcome of `POST /v1/operations`. */
export type OperationOutcome =
  | { result: 'changed'; changed: boolean }
  | { result: 'connected'; connected: boolean }
  | { result: 'node_state'; registered: boolean; active: boolean }
  | { result: 'edge_state'; registered: boolean; enabled: boolean; effective: boolean };

/** Decodes a 200 response of `POST /v1/operations`. */
export function decodeOperationOutcome(value: JsonValue): OperationOutcome {
  const body = object(value, 'outcome');
  const result = oneOf(
    ['changed', 'connected', 'node_state', 'edge_state'] as const,
    body.result,
    'outcome.result',
  );
  switch (result) {
    case 'changed':
      return { result, changed: boolean(body.changed, 'outcome.changed') };
    case 'connected':
      return { result, connected: boolean(body.connected, 'outcome.connected') };
    case 'node_state':
      return {
        result,
        registered: boolean(body.registered, 'outcome.registered'),
        active: boolean(body.active, 'outcome.active'),
      };
    case 'edge_state':
      return {
        result,
        registered: boolean(body.registered, 'outcome.registered'),
        enabled: boolean(body.enabled, 'outcome.enabled'),
        effective: boolean(body.effective, 'outcome.effective'),
      };
  }
}

/** A server error envelope. Unknown codes are preserved, not rejected. */
export interface ServerError {
  /** Error code as sent by the server. */
  code: string;
  /** True when {@link code} is documented in the OpenAPI enumeration. */
  known: boolean;
  /** Server message, to be displayed as plain text only. */
  message: string;
}

/** Decodes the `{"result":"error","code","message"}` envelope. */
export function decodeServerError(value: JsonValue): ServerError {
  const body = object(value, 'error');
  oneOf(['error'] as const, body.result, 'error.result');
  const code = string(body.code, 'error.code');
  return {
    code,
    known: (ERROR_CODES as readonly string[]).includes(code),
    message: string(body.message, 'error.message'),
  };
}

/** Narrows a code string to a documented {@link ErrorCode}. */
export function isKnownErrorCode(code: string): code is ErrorCode {
  return (ERROR_CODES as readonly string[]).includes(code);
}

/** A node ID string: ASCII decimal digits within the u64 range, kept exact. */
export function nodeId(value: JsonValue | undefined, path: string): string {
  const text = decimalString(value, path);
  if (BigInt(text) > U64_MAX) {
    fail(path, 'node ID exceeds u64');
  }
  return text;
}

function nullable<T>(
  value: JsonValue | undefined,
  path: string,
  decode: (value: JsonValue, path: string) => T,
): T | null {
  if (value === undefined) {
    fail(path, 'missing (expected a value or null)');
  }
  return value === null ? null : decode(value, path);
}

function array(value: JsonValue | undefined, path: string): JsonValue[] {
  if (!Array.isArray(value)) {
    fail(path, 'expected an array');
  }
  return value;
}

/** A node reported by the inspection routes. */
export interface InspectedNode {
  /** Exact node ID. */
  node: string;
  /** Node flag; always true without lifecycle support. */
  active: boolean;
  /** Registered degree, or `null` when the engine does not report it. */
  degree: string | null;
}

/** Decodes one inspected node. */
export function decodeInspectedNode(value: JsonValue, path = 'node'): InspectedNode {
  const body = object(value, path);
  return {
    node: nodeId(body.node, `${path}.node`),
    active: boolean(body.active, `${path}.active`),
    degree: nullable(body.degree, `${path}.degree`, (v, p) => unsigned(v, p)),
  };
}

/** A page of `GET /v1/nodes`. */
export interface NodePage {
  nodes: InspectedNode[];
  /** Cursor of the next page, or `null` when complete. */
  nextAfter: string | null;
}

/** Decodes a node page. */
export function decodeNodePage(value: JsonValue): NodePage {
  const body = object(value, 'page');
  return {
    nodes: array(body.nodes, 'page.nodes').map((n, i) =>
      decodeInspectedNode(n, `page.nodes[${i}]`),
    ),
    nextAfter: nullable(body.next_after, 'page.next_after', nodeId),
  };
}

/** An incident edge, seen from the inspected node. */
export interface IncidentEdge {
  neighbor: string;
  /** The edge's own flag (desired state). */
  enabled: boolean;
  /** Enabled and both endpoints active. */
  effective: boolean;
}

/** A page of `GET /v1/nodes/{node}/edges`. */
export interface IncidentPage {
  node: InspectedNode;
  edges: IncidentEdge[];
  nextAfter: string | null;
}

/** Decodes an incident edge page. */
export function decodeIncidentPage(value: JsonValue): IncidentPage {
  const body = object(value, 'page');
  return {
    node: decodeInspectedNode(body.node as JsonValue, 'page.node'),
    edges: array(body.edges, 'page.edges').map((edge, i) => {
      const e = object(edge, `page.edges[${i}]`);
      return {
        neighbor: nodeId(e.neighbor, `page.edges[${i}].neighbor`),
        enabled: boolean(e.enabled, `page.edges[${i}].enabled`),
        effective: boolean(e.effective, `page.edges[${i}].effective`),
      };
    }),
    nextAfter: nullable(body.next_after, 'page.next_after', nodeId),
  };
}

/** One edge of `GET /v1/edges`. */
export interface InspectedEdge {
  source: string;
  target: string;
  enabled: boolean;
  effective: boolean;
}

/** A page of `GET /v1/edges`. */
export interface EdgePage {
  edges: InspectedEdge[];
  /** Resume position, or `null` when complete. */
  nextAfterSource: string | null;
  nextAfterTarget: string | null;
}

/** Decodes an edge page. */
export function decodeEdgePage(value: JsonValue): EdgePage {
  const body = object(value, 'page');
  const page = {
    edges: array(body.edges, 'page.edges').map((edge, i) => {
      const e = object(edge, `page.edges[${i}]`);
      return {
        source: nodeId(e.source, `page.edges[${i}].source`),
        target: nodeId(e.target, `page.edges[${i}].target`),
        enabled: boolean(e.enabled, `page.edges[${i}].enabled`),
        effective: boolean(e.effective, `page.edges[${i}].effective`),
      };
    }),
    nextAfterSource: nullable(body.next_after_source, 'page.next_after_source', nodeId),
    nextAfterTarget: nullable(body.next_after_target, 'page.next_after_target', nodeId),
  };
  if (page.nextAfterSource === null && page.nextAfterTarget !== null) {
    fail('page.next_after_target', 'requires next_after_source');
  }
  return page;
}

/** One entry of a batch response: an outcome or a per-operation error. */
export type BatchEntry =
  { ok: true; outcome: OperationOutcome } | { ok: false; error: ServerError };

/** Decodes a `POST /v1/batch` 200 body. Results are in operation order. */
export function decodeBatch(value: JsonValue): BatchEntry[] {
  const body = object(value, 'batch');
  return array(body.results, 'batch.results').map((entry, i) => {
    const e = object(entry, `batch.results[${i}]`);
    return e.result === 'error'
      ? { ok: false as const, error: decodeServerError(e) }
      : { ok: true as const, outcome: decodeOperationOutcome(e) };
  });
}
