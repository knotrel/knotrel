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
  decodeHealth,
  decodeInfo,
  decodeOperationOutcome,
  decodeReadiness,
  decodeServerError,
  decodeStats,
  type GraphStats,
  type Health,
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
    }
  | { ok: false; failure: Failure; durationMs: number; receivedAt: Date };

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
  const failed = (failure: Failure): Result<T> => ({ ok: false, failure, durationMs, receivedAt });
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

/** The console API used by the UI. */
export interface ConsoleApi {
  health(signal?: AbortSignal): Promise<Result<Health>>;
  ready(signal?: AbortSignal): Promise<Result<Readiness>>;
  info(signal?: AbortSignal): Promise<Result<ServerInfo>>;
  stats(signal?: AbortSignal): Promise<Result<GraphStats>>;
  runtimeConfig(signal?: AbortSignal): Promise<Result<RuntimeConfig>>;
  operation(op: Operation, kind: 'query' | 'mutation'): Promise<Result<OperationOutcome>>;
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
  };
}
