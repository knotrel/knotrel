import { vi } from 'vitest';
import type { ConsoleApi, Failure, Result } from '../src/api/endpoints';
import type { GraphStats, ServerInfo } from '../src/api/decode';
import type { VersionMetadata } from '../src/api/metadata';

export const EMPTY_METADATA: VersionMetadata = {
  instanceId: undefined,
  graphId: undefined,
  generation: undefined,
  stateVersion: undefined,
  sourcePosition: undefined,
  syncStatus: undefined,
  etag: undefined,
  invalid: [],
};

export function metadata(patch: Partial<VersionMetadata> = {}): VersionMetadata {
  return {
    ...EMPTY_METADATA,
    instanceId: 'srv-1',
    graphId: 'default',
    generation: '0',
    stateVersion: '0',
    ...patch,
  };
}

export function ok<T>(value: T, patch: Partial<VersionMetadata> = {}, status = 200): Result<T> {
  return {
    ok: true,
    value,
    status,
    metadata: metadata(patch),
    durationMs: 12,
    receivedAt: new Date('2026-11-20T10:00:00Z'),
  };
}

export function failed<T = never>(failure: Failure): Result<T> {
  return { ok: false, failure, durationMs: 5, receivedAt: new Date('2026-11-20T10:00:00Z') };
}

export function info(patch: Partial<ServerInfo> = {}): ServerInfo {
  return {
    engine: 'compact-bfs',
    experimental: false,
    maxPendingJobs: '32',
    maxBodyBytes: '1048576',
    maxBatchOperations: '1024',
    maxNodes: '100000',
    maxEdges: '500000',
    mode: 'standalone',
    instanceId: undefined,
    topologyLifecycle: true,
    ...patch,
  };
}

export function stats(patch: Partial<GraphStats> = {}): GraphStats {
  return {
    nodeCount: '2',
    edgeCount: '1',
    activeNodeCount: '2',
    enabledEdgeCount: '1',
    effectiveEdgeCount: '1',
    maxNodes: '100000',
    maxEdges: '500000',
    remainingNodes: '99998',
    remainingEdges: '499999',
    stateVersion: '1',
    instanceId: undefined,
    generation: undefined,
    sourcePosition: undefined,
    syncStatus: undefined,
    ...patch,
  };
}

/** A healthy fake API; individual methods can be overridden. */
export function fakeApi(
  overrides: Partial<ConsoleApi> = {},
  serverInfo: ServerInfo = info(),
): ConsoleApi {
  return {
    health: vi.fn(async () => ok({ status: 'ok' as const })),
    ready: vi.fn(async () => ok({ status: 'ready' as const })),
    info: vi.fn(async () => ok(serverInfo)),
    stats: vi.fn(async () => ok(stats(), { stateVersion: '1' })),
    runtimeConfig: vi.fn(async () =>
      ok({ serverLabel: 'http://knotrel:8080', deployment: 'container' }),
    ),
    operation: vi.fn(async () =>
      ok({ result: 'changed' as const, changed: true }, { stateVersion: '2' }),
    ),
    ...overrides,
  };
}
