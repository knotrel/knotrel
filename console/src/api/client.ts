/**
 * HTTP transport for the console.
 *
 * The console talks only to its own origin under the relative `api/` prefix; a
 * same-origin gateway (the bundled container configuration or the operator's
 * own web server) forwards those requests to one configured Knotrel server. Every request has a bounded timeout and an abort signal. Nothing here
 * retries: retry policy belongs to callers, and mutations are never retried.
 */
import { parseLosslessJson, type JsonValue } from './lossless-json';
import { readVersionMetadata, type VersionMetadata } from './metadata';

/**
 * Response header set by the bundled gateway configuration when it answers on the
 * server's behalf. Other gateways may omit it; their errors are then reported as
 * plain HTTP statuses.
 */
export const PROXY_HEADER = 'x-knotrel-console-proxy';

/**
 * Gateway outcomes reported through {@link PROXY_HEADER}.
 *
 * - `upstream_unreachable`: the connection to the server failed before the request
 *   was sent; the server did not receive it.
 * - `upstream_failed`: the connection failed after the request may have been sent.
 * - `upstream_timeout`: the server did not answer within the gateway timeout.
 * - `rejected`: the gateway refused the request itself (route, origin, size).
 * - `response_too_large`: the server response exceeded the gateway limit.
 */
export type ProxyCondition =
  | 'upstream_unreachable'
  | 'upstream_failed'
  | 'upstream_timeout'
  | 'rejected'
  | 'response_too_large';

const PROXY_CONDITIONS: readonly ProxyCondition[] = [
  'upstream_unreachable',
  'upstream_failed',
  'upstream_timeout',
  'rejected',
  'response_too_large',
];

/** The result of one HTTP exchange, before semantic decoding. */
export type Transport =
  | {
      /** The server (or an intermediary) returned an HTTP response. */
      kind: 'response';
      /** HTTP status code. */
      status: number;
      /** Parsed body, or `undefined` when the body was empty or not parsed as JSON. */
      body: JsonValue | undefined;
      /** Raw response text, exactly as received (for raw views and `/metrics`). */
      text: string;
      /** Set when the body was present but not valid JSON. */
      bodyError: string | undefined;
      /** Version headers captured with the body. */
      metadata: VersionMetadata;
      /** Gateway condition, when the gateway answered instead of the server. */
      proxy: ProxyCondition | undefined;
      /** Browser-measured round-trip duration in milliseconds. */
      durationMs: number;
    }
  | {
      /** No response within the client timeout. The request may have been processed. */
      kind: 'timeout';
      durationMs: number;
    }
  | {
      /** The console origin itself could not be reached (gateway down). */
      kind: 'network';
      message: string;
      durationMs: number;
    }
  | {
      /** The caller aborted the request because it became obsolete. */
      kind: 'aborted';
      durationMs: number;
    };

/** Options for one request. */
export interface RequestOptions {
  /** JSON request body; implies `POST`. */
  body?: unknown;
  /** Client-side timeout in milliseconds. */
  timeoutMs: number;
  /** Caller cancellation (for obsolete reads). */
  signal?: AbortSignal;
  /** Expect plain text (Prometheus exposition) instead of JSON. */
  text?: boolean;
}

/** Minimal clock, injectable for tests. */
export type Clock = () => number;

/** Performs console API requests against `baseUrl` (default: `api/`, same origin). */
export class ApiClient {
  readonly #baseUrl: string;
  readonly #fetch: typeof fetch;
  readonly #now: Clock;

  constructor(options: { baseUrl?: string; fetch?: typeof fetch; now?: Clock } = {}) {
    this.#baseUrl = options.baseUrl ?? 'api/';
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.#now = options.now ?? (() => performance.now());
  }

  /**
   * Sends one request. Never throws for transport conditions; they are returned as
   * {@link Transport} values so callers handle each case explicitly.
   */
  async request(path: string, options: RequestOptions): Promise<Transport> {
    if (options.signal?.aborted) {
      return { kind: 'aborted', durationMs: 0 };
    }
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, options.timeoutMs);
    const onCallerAbort = (): void => controller.abort();
    options.signal?.addEventListener('abort', onCallerAbort, { once: true });

    const started = this.#now();
    const elapsed = (): number => Math.max(0, this.#now() - started);
    try {
      const init: RequestInit = {
        method: options.body === undefined ? 'GET' : 'POST',
        headers:
          options.body === undefined
            ? { accept: options.text ? 'text/plain' : 'application/json' }
            : { accept: 'application/json', 'content-type': 'application/json' },
        cache: 'no-store',
        credentials: 'same-origin',
        redirect: 'error',
        signal: controller.signal,
      };
      if (options.body !== undefined) {
        init.body = JSON.stringify(options.body);
      }
      const response = await this.#fetch(this.#baseUrl + path.replace(/^\//, ''), init);
      const text = await response.text();
      const durationMs = elapsed();
      let body: JsonValue | undefined;
      let bodyError: string | undefined;
      if (text.length > 0 && !options.text) {
        try {
          body = parseLosslessJson(text);
        } catch (error) {
          bodyError = error instanceof Error ? error.message : 'invalid JSON';
        }
      }
      const proxyHeader = response.headers.get(PROXY_HEADER);
      const proxy = PROXY_CONDITIONS.find((condition) => condition === proxyHeader);
      return {
        kind: 'response',
        status: response.status,
        body,
        text,
        bodyError,
        metadata: readVersionMetadata(response.headers),
        proxy,
        durationMs,
      };
    } catch (error) {
      const durationMs = elapsed();
      if (timedOut) {
        return { kind: 'timeout', durationMs };
      }
      if (controller.signal.aborted) {
        return { kind: 'aborted', durationMs };
      }
      return {
        kind: 'network',
        message: error instanceof Error ? error.message : 'network error',
        durationMs,
      };
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener('abort', onCallerAbort);
    }
  }
}
