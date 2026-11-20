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
}

/** Decodes the effective configuration. */
export function decodeInfo(value: JsonValue): ServerInfo {
  const body = object(value, 'info');
  const mode = optional(body, 'mode', 'info', (v, p) => oneOf(['source_managed'] as const, v, p));
  const lifecycle = optional(body, 'topology_lifecycle', 'info', boolean);
  if (lifecycle === false) {
    fail('info.topology_lifecycle', 'must be absent or true');
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
