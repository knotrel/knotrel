/**
 * Periodic diagnostic reads (liveness, readiness, configuration, live stats).
 *
 * Each cycle issues the four reads concurrently, but they are independent
 * requests: the store keeps each result with its own timestamp and never claims
 * they form one atomic snapshot. `/v1/stats` itself is coherent with its headers.
 */
import type { ConsoleApi, Result, RuntimeConfig } from '../api/endpoints';
import type { GraphStats, Health, Readiness, ServerInfo } from '../api/decode';
import { Poller, type VisibilitySource } from './poller';

/** A successful value with the time it was received. */
export interface Timed<T> {
  value: T;
  receivedAt: Date;
  durationMs: number;
}

/** Overall connectivity from the browser's point of view. */
export type Connectivity =
  | 'connecting'
  | 'connected'
  | 'console-unreachable'
  | 'server-unreachable'
  | 'incompatible'
  | 'degraded';

/** A detected change of server process instance. */
export interface InstanceChange {
  from: string;
  to: string;
  at: Date;
}

/** Snapshot of the overview store. */
export interface OverviewState {
  health: Result<Health> | undefined;
  ready: Result<Readiness> | undefined;
  info: Result<ServerInfo> | undefined;
  stats: Result<GraphStats> | undefined;
  runtimeConfig: Result<RuntimeConfig> | undefined;
  /** Last successfully decoded values, kept while later reads fail. */
  lastInfo: Timed<ServerInfo> | undefined;
  lastStats: Timed<GraphStats> | undefined;
  /** Instance ID from the latest stats headers (both modes). */
  instanceId: string | undefined;
  instanceChange: InstanceChange | undefined;
  lastAttemptAt: Date | undefined;
  lastSuccessAt: Date | undefined;
  refreshing: boolean;
  consecutiveFailures: number;
}

/** Initial state before any read. */
export const INITIAL_OVERVIEW: OverviewState = {
  health: undefined,
  ready: undefined,
  info: undefined,
  stats: undefined,
  runtimeConfig: undefined,
  lastInfo: undefined,
  lastStats: undefined,
  instanceId: undefined,
  instanceChange: undefined,
  lastAttemptAt: undefined,
  lastSuccessAt: undefined,
  refreshing: false,
  consecutiveFailures: 0,
};

/** Polling interval while visible, in milliseconds. */
export const POLL_INTERVAL_MS = 5_000;
/** Maximum error backoff, in milliseconds. */
export const MAX_BACKOFF_MS = 60_000;

/** Derives connectivity from the latest liveness read. */
export function connectivityOf(state: OverviewState): Connectivity {
  const health = state.health;
  if (!health) {
    return 'connecting';
  }
  if (health.ok) {
    return 'connected';
  }
  switch (health.failure.kind) {
    case 'console-unreachable':
      return 'console-unreachable';
    case 'server-unreachable':
      return 'server-unreachable';
    case 'timeout':
      return health.failure.where === 'gateway' ? 'server-unreachable' : 'degraded';
    case 'incompatible':
      return 'incompatible';
    default:
      return 'degraded';
  }
}

/** True when displayed data may no longer reflect the server. */
export function isStale(state: OverviewState, now: Date, intervalMs = POLL_INTERVAL_MS): boolean {
  if (!state.lastSuccessAt) {
    return false;
  }
  const latestFailed = state.stats !== undefined && !state.stats.ok;
  return latestFailed || now.getTime() - state.lastSuccessAt.getTime() > 3 * intervalMs;
}

/** Holds overview state and drives polling. */
export class OverviewStore {
  #state: OverviewState = INITIAL_OVERVIEW;
  readonly #listeners = new Set<() => void>();
  readonly #api: ConsoleApi;
  readonly #poller: Poller;

  constructor(
    api: ConsoleApi,
    options: { intervalMs?: number; maxBackoffMs?: number; visibility?: VisibilitySource } = {},
  ) {
    this.#api = api;
    const pollerOptions = {
      task: (signal: AbortSignal) => this.#cycle(signal),
      intervalMs: options.intervalMs ?? POLL_INTERVAL_MS,
      maxBackoffMs: options.maxBackoffMs ?? MAX_BACKOFF_MS,
    };
    this.#poller = new Poller(
      options.visibility ? { ...pollerOptions, visibility: options.visibility } : pollerOptions,
    );
  }

  /** Current state (stable reference between changes). */
  getState = (): OverviewState => this.#state;

  /** Subscribes to state changes. */
  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  /** Starts polling and loads the optional deployment facts once. */
  start(): void {
    this.#poller.start();
    void this.#api.runtimeConfig().then((runtimeConfig) => this.#set({ runtimeConfig }));
  }

  /** Stops polling and aborts in-flight reads. */
  stop(): void {
    this.#poller.stop();
  }

  /** Requests an immediate refresh, cancelling an obsolete one. */
  refresh(): void {
    this.#poller.refreshNow();
  }

  #set(patch: Partial<OverviewState>): void {
    this.#state = { ...this.#state, ...patch };
    for (const listener of this.#listeners) {
      listener();
    }
  }

  async #cycle(signal: AbortSignal): Promise<boolean> {
    this.#set({ refreshing: true, lastAttemptAt: new Date() });
    const [health, ready, info, stats] = await Promise.all([
      this.#api.health(signal),
      this.#api.ready(signal),
      this.#api.info(signal),
      this.#api.stats(signal),
    ]);
    if (signal.aborted) {
      return false; // Superseded: do not publish partial, obsolete results.
    }
    const patch: Partial<OverviewState> = { health, ready, info, stats, refreshing: false };
    if (info.ok) {
      patch.lastInfo = {
        value: info.value,
        receivedAt: info.receivedAt,
        durationMs: info.durationMs,
      };
    }
    if (stats.ok) {
      patch.lastStats = {
        value: stats.value,
        receivedAt: stats.receivedAt,
        durationMs: stats.durationMs,
      };
      const instanceId = stats.metadata.instanceId ?? stats.value.instanceId;
      const previous = this.#state.instanceId;
      if (instanceId !== undefined) {
        patch.instanceId = instanceId;
        if (previous !== undefined && previous !== instanceId) {
          patch.instanceChange = { from: previous, to: instanceId, at: stats.receivedAt };
        }
      }
    }
    const ok = health.ok && ready.ok && info.ok && stats.ok;
    if (ok) {
      patch.lastSuccessAt = new Date();
      patch.consecutiveFailures = 0;
    } else {
      patch.consecutiveFailures = this.#state.consecutiveFailures + 1;
    }
    this.#set(patch);
    return ok;
  }
}
