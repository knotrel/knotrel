/**
 * Typed access to the server endpoints used by the console.
 *
 * Each function performs exactly one HTTP request and returns a {@link Result}:
 * either a decoded value with the metadata and timing of that same response, or
 * a classified {@link Failure}. Independent reads are never merged into a claimed
 * atomic snapshot; every value carries its own `receivedAt`.
 */
import type { ApiClient, Transport } from './client';
import {
  DecodeError,
  decodeBatch,
  decodeEdgePage,
  decodeHealth,
  decodeIncidentPage,
  decodeInspectedNode,
  decodeNodePage,
  decodeInfo,
  decodeOperationOutcome,
  decodeReadiness,
  decodeServerError,
  decodeStats,
  type BatchEntry,
  type EdgePage,
  type GraphStats,
  type Health,
  type IncidentPage,
  type InspectedNode,
  type NodePage,
  type OperationOutcome,
  type Readiness,
  type ServerError,
  type ServerInfo,
} from './decode';
import { isJsonObject, type JsonValue } from './lossless-json';
import type { VersionMetadata } from './metadata';
import type { Operation } from './wire';

/** Classified reason a request did not produce a decoded value. */
export type Failure =
  | { kind: 'console-unreachable'; message: string }
  | { kind: 'server-unreachable'; delivered: 'no' | 'unknown'; status: number }
  | { kind: 'timeout'; where: 'client' | 'gateway' }
  | { kind: 'server-error'; status: number; error: ServerError }
  | { kind: 'gateway-rejected'; status: number; message: string }
  | { kind: 'http'; status: number }
  | { kind: 'incompatible'; status: number; reason: string }
  | { kind: 'aborted' };

/** Outcome of one request. */
export type Result<T> =
  | {
      ok: true;
      value: T;
      status: number;
      metadata: VersionMetadata;
      durationMs: number;
      receivedAt: Date;
      /** Raw response text, exactly as received. */
      raw: string;
    }
  | { ok: false; failure: Failure; durationMs: number; receivedAt: Date; raw?: string };

/** Default timeout for reads, in milliseconds. */
export const READ_TIMEOUT_MS = 10_000;

/**
 * Default timeout for mutations, in milliseconds. It exceeds the bundled gateway's
 * upstream timeout (35 s) and the server's default request timeout (30 s) so that the most
 * specific timeout is the one reported.
 */
export const MUTATION_TIMEOUT_MS = 40_000;

function messageOf(body: JsonValue | undefined): string {
  if (isJsonObject(body) && typeof body.message === 'string') {
    return body.message;
  }
  return 'request rejected by the console gateway';
}

/**
 * Classifies a transport result. `accept` lists statuses whose body is decoded with
 * `decode`; any other status is treated as an error response.
 */
export function classify<T>(
  transport: Transport,
  accept: readonly number[],
  decode: (body: JsonValue) => T,
): Result<T> {
  const receivedAt = new Date();
  const { durationMs } = transport;
  const raw = transport.kind === 'response' ? transport.text : undefined;
  const failed = (failure: Failure): Result<T> =>
    raw === undefined
      ? { ok: false, failure, durationMs, receivedAt }
      : { ok: false, failure, durationMs, receivedAt, raw };
  switch (transport.kind) {
    case 'aborted':
      return failed({ kind: 'aborted' });
    case 'timeout':
      return failed({ kind: 'timeout', where: 'client' });
    case 'network':
      return failed({ kind: 'console-unreachable', message: transport.message });
    case 'response':
      break;
  }
  const { status, body, proxy } = transport;
  switch (proxy) {
    case 'upstream_unreachable':
      return failed({ kind: 'server-unreachable', delivered: 'no', status });
    case 'upstream_failed':
    case 'response_too_large':
      return failed({ kind: 'server-unreachable', delivered: 'unknown', status });
    case 'upstream_timeout':
      return failed({ kind: 'timeout', where: 'gateway' });
    case 'rejected':
      return failed({ kind: 'gateway-rejected', status, message: messageOf(body) });
    case undefined:
      break;
  }
  if (transport.bodyError !== undefined) {
    return failed({ kind: 'incompatible', status, reason: transport.bodyError });
  }
  if (accept.includes(status)) {
    if (body === undefined) {
      return failed({ kind: 'incompatible', status, reason: 'empty response body' });
    }
    try {
      return {
        ok: true,
        value: decode(body),
        status,
        metadata: transport.metadata,
        durationMs,
        receivedAt,
        raw: transport.text,
      };
    } catch (error) {
      const reason = error instanceof DecodeError ? error.message : 'undecodable response';
      return failed({ kind: 'incompatible', status, reason });
    }
  }
  if (body !== undefined) {
    try {
      return failed({ kind: 'server-error', status, error: decodeServerError(body) });
    } catch {
      return failed({ kind: 'incompatible', status, reason: `unexpected HTTP ${status} body` });
    }
  }
  return failed({ kind: 'http', status });
}

/**
 * Optional deployment facts read from `console-config.json` next to the console.
 *
 * The container image writes this file at startup from operator configuration.
 * Other deployments may omit it; the console then reports the server address as
 * determined by the hosting gateway. It is display information only: the browser
 * never chooses the upstream server.
 */
export interface RuntimeConfig {
  /** Human-readable description of the upstream server, for display only. */
  serverLabel: string | undefined;
  /** Deployment kind reported by the host, for example `container`. */
  deployment: string | undefined;
}

function decodeRuntimeConfig(body: JsonValue): RuntimeConfig {
  if (!isJsonObject(body)) {
    throw new DecodeError('console-config: expected an object');
  }
  const text = (key: string): string | undefined => {
    const value = body[key];
    if (value === undefined || value === null || value === '') {
      return undefined;
    }
    if (typeof value !== 'string') {
      throw new DecodeError(`console-config.${key}: expected a string`);
    }
    return value;
  };
  return { serverLabel: text('serverLabel'), deployment: text('deployment') };
}

/** Paging parameters of `GET /v1/nodes`. */
export interface NodeQuery {
  after?: string | undefined;
  limit: number;
  state: 'all' | 'active' | 'inactive';
}

/** Edge state filter. */
export type EdgeState = 'all' | 'enabled' | 'disabled' | 'effective' | 'ineffective';

/** Paging parameters of `GET /v1/nodes/{node}/edges`. */
export interface IncidentQuery {
  after?: string | undefined;
  limit: number;
  state: EdgeState;
}

/** Paging parameters of `GET /v1/edges`. */
export interface EdgeQuery {
  afterSource?: string | undefined;
  afterTarget?: string | undefined;
  limit: number;
  state: EdgeState;
}

function query(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) {
      search.set(key, String(value));
    }
  }
  const text = search.toString();
  return text ? `?${text}` : '';
}

/** Plain-text decoder for `/metrics`; the body is kept verbatim. */
function metricsText(transport: Transport): Result<string> {
  if (transport.kind === 'response' && transport.status === 200 && transport.proxy === undefined) {
    return {
      ok: true,
      value: transport.text,
      status: 200,
      metadata: transport.metadata,
      durationMs: transport.durationMs,
      receivedAt: new Date(),
      raw: transport.text,
    };
  }
  return classify(
    {
      ...transport,
      ...(transport.kind === 'response' ? { body: undefined, bodyError: undefined } : {}),
    } as Transport,
    [],
    () => '',
  );
}

/** The console API used by the UI. */
export interface ConsoleApi {
  health(signal?: AbortSignal): Promise<Result<Health>>;
  ready(signal?: AbortSignal): Promise<Result<Readiness>>;
  info(signal?: AbortSignal): Promise<Result<ServerInfo>>;
  stats(signal?: AbortSignal): Promise<Result<GraphStats>>;
  runtimeConfig(signal?: AbortSignal): Promise<Result<RuntimeConfig>>;
  operation(op: Operation, kind: 'query' | 'mutation'): Promise<Result<OperationOutcome>>;
  batch(ops: Operation[], kind: 'query' | 'mutation'): Promise<Result<BatchEntry[]>>;
  listNodes(params: NodeQuery, signal?: AbortSignal): Promise<Result<NodePage>>;
  getNode(node: string, signal?: AbortSignal): Promise<Result<InspectedNode>>;
  listNodeEdges(
    node: string,
    params: IncidentQuery,
    signal?: AbortSignal,
  ): Promise<Result<IncidentPage>>;
  listEdges(params: EdgeQuery, signal?: AbortSignal): Promise<Result<EdgePage>>;
  /** Raw Prometheus exposition of `GET /metrics` (404 when metrics are disabled). */
  metrics(signal?: AbortSignal): Promise<Result<string>>;
}

/** Builds the {@link ConsoleApi} on top of an {@link ApiClient}. */
export function createConsoleApi(client: ApiClient): ConsoleApi {
  const read = async <T>(
    path: string,
    accept: readonly number[],
    decode: (body: JsonValue) => T,
    signal?: AbortSignal,
  ): Promise<Result<T>> => {
    const options = signal
      ? { timeoutMs: READ_TIMEOUT_MS, signal }
      : { timeoutMs: READ_TIMEOUT_MS };
    return classify(await client.request(path, options), accept, decode);
  };
  return {
    health: (signal) => read('health', [200], decodeHealth, signal),
    // 503 carries a valid readiness label; it is a result, not a failure.
    ready: (signal) => read('ready', [200, 503], decodeReadiness, signal),
    info: (signal) => read('v1/info', [200], decodeInfo, signal),
    stats: (signal) => read('v1/stats', [200], decodeStats, signal),
    // Optional; normally 404 outside the container image.
    runtimeConfig: async (signal) => {
      const options = signal
        ? { timeoutMs: READ_TIMEOUT_MS, signal }
        : { timeoutMs: READ_TIMEOUT_MS };
      return classify(
        await client.request('../console-config.json', options),
        [200],
        decodeRuntimeConfig,
      );
    },
    operation: async (op, kind) =>
      classify(
        await client.request('v1/operations', {
          body: op,
          timeoutMs: kind === 'mutation' ? MUTATION_TIMEOUT_MS : READ_TIMEOUT_MS,
        }),
        [200],
        decodeOperationOutcome,
      ),
    batch: async (ops, kind) =>
      classify(
        await client.request('v1/batch', {
          body: { operations: ops },
          timeoutMs: kind === 'mutation' ? MUTATION_TIMEOUT_MS : READ_TIMEOUT_MS,
        }),
        [200],
        decodeBatch,
      ),
    listNodes: (params, signal) =>
      read(
        `v1/nodes${query({ after: params.after, limit: params.limit, state: params.state })}`,
        [200],
        decodeNodePage,
        signal,
      ),
    getNode: (node, signal) =>
      read(`v1/nodes/${encodeURIComponent(node)}`, [200], (b) => decodeInspectedNode(b), signal),
    listNodeEdges: (node, params, signal) =>
      read(
        `v1/nodes/${encodeURIComponent(node)}/edges${query({
          after: params.after,
          limit: params.limit,
          state: params.state,
        })}`,
        [200],
        decodeIncidentPage,
        signal,
      ),
    listEdges: (params, signal) =>
      read(
        `v1/edges${query({
          after_source: params.afterSource,
          after_target: params.afterTarget,
          limit: params.limit,
          state: params.state,
        })}`,
        [200],
        decodeEdgePage,
        signal,
      ),
    metrics: async (signal) => {
      const options = signal
        ? { timeoutMs: READ_TIMEOUT_MS, signal, text: true }
        : { timeoutMs: READ_TIMEOUT_MS, text: true };
      return metricsText(await client.request('metrics', options));
    },
  };
}
