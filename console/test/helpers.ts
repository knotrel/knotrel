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
    raw: JSON.stringify(value),
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
    inspection: true,
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
    batch: vi.fn(async () =>
      ok([{ ok: true as const, outcome: { result: 'changed' as const, changed: true } }]),
    ),
    listNodes: vi.fn(async () =>
      ok({
        nodes: [
          { node: '1', active: true, degree: '1' },
          { node: '18446744073709551615', active: false, degree: '1' },
        ],
        nextAfter: null,
      }),
    ),
    getNode: vi.fn(async (node: string) => ok({ node, active: true, degree: '1' })),
    listNodeEdges: vi.fn(async (node: string) =>
      ok({
        node: { node, active: true, degree: '1' },
        edges: [
          {
            neighbor: node === '1' ? '18446744073709551615' : '1',
            enabled: true,
            effective: false,
          },
        ],
        nextAfter: null,
      }),
    ),
    listEdges: vi.fn(async () =>
      ok({
        edges: [{ source: '1', target: '18446744073709551615', enabled: true, effective: false }],
        nextAfterSource: null,
        nextAfterTarget: null,
      }),
    ),
    metrics: vi.fn(async () => failed<string>({ kind: 'http', status: 404 })),
    ...overrides,
  };
}

/** A minimal Prometheus exposition for the Performance view. */
export function exposition(requests: number, bucketCounts: [number, number, number]): string {
  return [
    '# TYPE knotrel_http_requests_total counter',
    `knotrel_http_requests_total{route="/v1/stats",method="GET",status_class="2xx"} ${requests}`,
    `knotrel_http_requests_total{route="/v1/stats",method="GET",status_class="5xx"} 1`,
    `knotrel_http_request_duration_seconds_bucket{route="/v1/stats",le="0.001"} ${bucketCounts[0]}`,
    `knotrel_http_request_duration_seconds_bucket{route="/v1/stats",le="0.01"} ${bucketCounts[1]}`,
    `knotrel_http_request_duration_seconds_bucket{route="/v1/stats",le="+Inf"} ${bucketCounts[2]}`,
    `knotrel_requests_rejected_total{reason="busy"} ${Math.floor(requests / 10)}`,
    'knotrel_graph_jobs_outstanding 2',
    'knotrel_graph_job_capacity 2',
    `knotrel_graph_job_execution_seconds_bucket{kind="stats",le="0.0001"} ${bucketCounts[0]}`,
    `knotrel_graph_job_execution_seconds_bucket{kind="stats",le="+Inf"} ${bucketCounts[2]}`,
    `knotrel_graph_job_queue_seconds_bucket{kind="stats",le="+Inf"} ${bucketCounts[2]}`,
    `knotrel_graph_lock_wait_seconds_bucket{kind="stats",le="+Inf"} ${bucketCounts[2]}`,
    'knotrel_feeder_lease_remaining_seconds 12.5',
    'knotrel_feeder_last_applied_age_seconds 3',
    '',
  ].join('\n');
}
