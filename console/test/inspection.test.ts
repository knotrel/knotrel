import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ApiClient, Transport } from '../src/api/client';
import {
  decodeBatch,
  decodeEdgePage,
  decodeIncidentPage,
  decodeInspectedNode,
  decodeNodePage,
  nodeId,
} from '../src/api/decode';
import { createConsoleApi } from '../src/api/endpoints';
import { parseLosslessJson } from '../src/api/lossless-json';
import { checkBatch, formatBatch } from '../src/domain/batch';
import { deriveCapabilities } from '../src/domain/capabilities';
import { curlCommand } from '../src/domain/curl';
import {
  gauge,
  histogramQuantile,
  increase,
  parseExposition,
  type Scrape,
} from '../src/domain/prometheus';
import { appendPage, emptyListing, failPage, loading } from '../src/state/listing';
import { MetricsStore } from '../src/state/metrics';
import { formatHash, parseHash } from '../src/state/route';
import { loadSaved, storeSaved, upsertSaved, type KeyValueStorage } from '../src/state/saved';
import { EMPTY_METADATA, exposition, failed, fakeApi, info, metadata, ok } from './helpers';

const MAX = '18446744073709551615';
const j = (text: string) => parseLosslessJson(text);

describe('inspection decoders', () => {
  it('decodes node pages with exact IDs and null cursors', () => {
    const page = decodeNodePage(
      j(`{"nodes":[{"node":"${MAX}","active":false,"degree":null}],"next_after":"${MAX}"}`),
    );
    expect(page).toEqual({ nodes: [{ node: MAX, active: false, degree: null }], nextAfter: MAX });
    expect(decodeNodePage(j('{"nodes":[],"next_after":null}')).nextAfter).toBeNull();
    expect(() => decodeNodePage(j('{"nodes":[]}'))).toThrow(/missing/);
    expect(() => decodeNodePage(j('{"nodes":{},"next_after":null}'))).toThrow(/array/);
    expect(() =>
      decodeInspectedNode(j('{"node":"18446744073709551616","active":true,"degree":1}')),
    ).toThrow(/u64/);
    expect(() => nodeId(j('1'), 'x')).toThrow(/string/);
  });

  it('decodes incident and edge pages', () => {
    const incident = decodeIncidentPage(
      j(
        '{"node":{"node":"1","active":true,"degree":2},"edges":[{"neighbor":"2","enabled":false,"effective":false}],"next_after":null}',
      ),
    );
    expect(incident.edges[0]).toEqual({ neighbor: '2', enabled: false, effective: false });
    expect(incident.node.degree).toBe('2');
    const edges = decodeEdgePage(
      j(
        `{"edges":[{"source":"1","target":"${MAX}","enabled":true,"effective":true}],"next_after_source":"1","next_after_target":"${MAX}"}`,
      ),
    );
    expect(edges.nextAfterTarget).toBe(MAX);
    expect(() =>
      decodeEdgePage(j('{"edges":[],"next_after_source":null,"next_after_target":"1"}')),
    ).toThrow(/requires/);
  });

  it('decodes batch results in order, keeping per-operation errors', () => {
    const entries = decodeBatch(
      j(
        '{"results":[{"result":"changed","changed":true},{"result":"error","code":"self_loop","message":"m"}]}',
      ),
    );
    expect(entries).toEqual([
      { ok: true, outcome: { result: 'changed', changed: true } },
      { ok: false, error: { code: 'self_loop', known: true, message: 'm' } },
    ]);
    expect(() => decodeBatch(j('{"results":[1]}'))).toThrow();
  });
});

describe('inspection endpoints', () => {
  function client(body: string, status = 200, text = false) {
    return vi.fn(async (): Promise<Transport> => ({
      kind: 'response',
      status,
      body: text ? undefined : parseLosslessJson(body),
      text: body,
      bodyError: undefined,
      metadata: EMPTY_METADATA,
      proxy: undefined,
      durationMs: 1,
    }));
  }

  it('builds encoded paths and query strings', async () => {
    const request = client('{"nodes":[],"next_after":null}');
    const api = createConsoleApi({ request } as unknown as ApiClient);
    await api.listNodes({ after: MAX, limit: 100, state: 'inactive' });
    await api.listNodes({ limit: 5, state: 'all' });
    const paths = request.mock.calls.map((c) => (c as unknown[])[0]);
    expect(paths).toEqual([
      `v1/nodes?after=${MAX}&limit=100&state=inactive`,
      'v1/nodes?limit=5&state=all',
    ]);
  });

  it('routes node, incident, edge, batch and metrics requests', async () => {
    const node = createConsoleApi({
      request: client('{"node":"7","active":true,"degree":0}'),
    } as unknown as ApiClient);
    expect((await node.getNode('7')).ok).toBe(true);
    const incidentRequest = client(
      '{"node":{"node":"7","active":true,"degree":0},"edges":[],"next_after":null}',
    );
    const incident = createConsoleApi({ request: incidentRequest } as unknown as ApiClient);
    await incident.listNodeEdges('7', { after: '3', limit: 50, state: 'disabled' });
    expect((incidentRequest.mock.calls[0] as unknown[])[0]).toBe(
      'v1/nodes/7/edges?after=3&limit=50&state=disabled',
    );
    const edgeRequest = client('{"edges":[],"next_after_source":null,"next_after_target":null}');
    const edges = createConsoleApi({ request: edgeRequest } as unknown as ApiClient);
    await edges.listEdges({ afterSource: '1', afterTarget: MAX, limit: 10, state: 'effective' });
    expect((edgeRequest.mock.calls[0] as unknown[])[0]).toBe(
      `v1/edges?after_source=1&after_target=${MAX}&limit=10&state=effective`,
    );
    const batchRequest = client('{"results":[]}');
    const batch = createConsoleApi({ request: batchRequest } as unknown as ApiClient);
    await batch.batch([{ op: 'add_node', node: '1' }], 'mutation');
    expect((batchRequest.mock.calls[0] as unknown[])[1]).toMatchObject({
      body: { operations: [{ op: 'add_node', node: '1' }] },
      timeoutMs: 40_000,
    });
    await batch.batch([{ op: 'connected', source: '1', target: '2' }], 'query');
    expect((batchRequest.mock.calls[1] as unknown[])[1]).toMatchObject({ timeoutMs: 10_000 });
  });

  it('returns metrics text verbatim and classifies disabled metrics', async () => {
    const text = '# HELP x\nknotrel_graph_job_capacity 32\n';
    const ok200 = createConsoleApi({ request: client(text, 200, true) } as unknown as ApiClient);
    const result = await ok200.metrics(new AbortController().signal);
    expect(result.ok && result.value).toBe(text);
    const missing = createConsoleApi({ request: client('', 404, true) } as unknown as ApiClient);
    const disabled = await missing.metrics();
    expect(!disabled.ok && disabled.failure).toEqual({ kind: 'http', status: 404 });
    const network = createConsoleApi({
      request: async (): Promise<Transport> => ({ kind: 'network', message: 'x', durationMs: 1 }),
    } as unknown as ApiClient);
    expect((await network.metrics()).ok).toBe(false);
  });
});

describe('prometheus parsing and statistics', () => {
  it('parses samples, labels with escapes and special values', () => {
    const { samples, skipped } = parseExposition(
      [
        '# HELP knotrel_x help',
        'knotrel_x{a="1",b="q\\"uote\\\\n"} 3.5',
        'knotrel_y +Inf',
        'knotrel_z NaN 1700000000',
        'knotrel_bad{a=1} 2',
        'not a sample line at all',
        'knotrel_w{a="1"',
        '',
      ].join('\n'),
    );
    expect(samples[0]).toEqual({
      name: 'knotrel_x',
      labels: { a: '1', b: 'q"uote\\n' },
      value: 3.5,
    });
    expect(samples[1]!.value).toBe(Infinity);
    expect(Number.isNaN(samples[2]!.value)).toBe(true);
    expect(skipped).toBe(3);
    expect(parseExposition('knotrel_v -Inf').samples[0]!.value).toBe(-Infinity);
    expect(parseExposition('knotrel_v{a="x\\n"} 1').samples[0]!.labels.a).toBe('x\n');
  });

  const scrape = (at: number, text: string): Scrape => ({
    at,
    samples: parseExposition(text).samples,
  });

  it('computes reset-aware increases and gauges', () => {
    const a = scrape(0, 'c{k="1"} 10\nc{k="2"} 5\ng 3\ng{x="y"} 4');
    const b = scrape(10_000, 'c{k="1"} 30\nc{k="2"} 2\nc{k="3"} 1\ng 6');
    expect(increase(a, b, 'c')).toEqual({ value: 20 + 2 + 1, seconds: 10, reset: true });
    expect(increase(a, b, 'c', { k: '1' })).toEqual({ value: 20, seconds: 10, reset: false });
    expect(increase(a, a, 'c')).toBeUndefined();
    expect(increase(a, b, 'missing')).toBeUndefined();
    expect(gauge(b, 'g')).toBe(6);
    expect(gauge(a, 'g')).toBe(7);
    expect(gauge(undefined, 'g')).toBeUndefined();
    expect(gauge(b, 'none')).toBeUndefined();
  });

  it('estimates quantiles from bucket deltas', () => {
    const a = scrape(0, exposition(0, [0, 0, 0]));
    const b = scrape(10_000, exposition(0, [50, 90, 100]));
    const family = 'knotrel_http_request_duration_seconds';
    const p50 = histogramQuantile(a, b, family, { route: '/v1/stats' }, 0.5)!;
    expect(p50.value).toBeCloseTo(0.001, 6);
    expect(p50.count).toBe(100);
    const p95 = histogramQuantile(a, b, family, { route: '/v1/stats' }, 0.95)!;
    expect(p95.value).toBe(0.01); // beyond the last finite bucket: its bound
    const p70 = histogramQuantile(a, b, family, { route: '/v1/stats' }, 0.7)!;
    expect(p70.value).toBeGreaterThan(0.001);
    expect(p70.value).toBeLessThan(0.01);
    expect(histogramQuantile(a, a, family, { route: '/v1/stats' }, 0.5)).toBeUndefined();
    expect(histogramQuantile(b, a, family, { route: '/v1/stats' }, 0.5)).toBeUndefined();
    expect(histogramQuantile(a, b, 'none', {}, 0.5)).toBeUndefined();
  });
});

describe('batch validation', () => {
  const caps = deriveCapabilities(info());

  it('accepts both request shapes and reports mutations', () => {
    const wrapped = checkBatch(
      '{"operations":[{"op":"connected","source":"1","target":"2"}]}',
      caps,
      10,
    );
    expect(wrapped).toEqual({
      ok: true,
      operations: [{ op: 'connected', source: '1', target: '2' }],
      mutating: false,
    });
    const bare = checkBatch(
      `[{"op":"set_edge_enabled","source":"1","target":"${MAX}","enabled":false}]`,
      caps,
      10,
    );
    expect(bare.ok && bare.mutating).toBe(true);
    expect(JSON.parse(formatBatch([{ op: 'add_node', node: MAX }]))).toEqual({
      operations: [{ op: 'add_node', node: MAX }],
    });
  });

  it.each([
    ['{"operations":[],"x":1}', /not allowed/],
    ['{"ops":[]}', /expected/],
    ['[]', /1 to 3/],
    ['[1,2,3,4]', /1 to 3/],
    ['[1]', /operation object/],
    ['[{"op":"drop"}]', /unknown operation/],
    ['[{"op":"add_node"}]', /JSON strings/],
    ['[{"op":"add_node","node":"1","active":true}]', /not allowed/],
    ['[{"op":"set_node_active","node":"1","active":"no"}]', /true or false/],
    ['[{"op":"link","source":"1","target":"1"}]', /self-loops/],
    ['[{"op":"node_state","node":"18446744073709551616"}]', /u64/],
  ])('rejects %s', (text, message) => {
    const check = checkBatch(text, caps, 3);
    expect(check.ok).toBe(false);
    expect(!check.ok && check.problems.map((p) => p.message).join(' ')).toMatch(message);
  });
});

describe('curl, routes, saved requests and listings', () => {
  it('builds curl commands against the console origin with safe quoting', () => {
    expect(curlCommand('https://c.example/ops/#/browser', 'v1/stats')).toBe(
      "curl -sS 'https://c.example/ops/api/v1/stats'",
    );
    const post = curlCommand('http://localhost:7380/', '/v1/operations', {
      op: 'add_node',
      node: "1'",
    });
    expect(post).toContain(`--data-raw '{"op":"add_node","node":"1'\\''"}'`);
    expect(post).toContain("'http://localhost:7380/api/v1/operations'");
    expect(post).not.toMatch(/cookie|authorization/i);
  });

  it('parses and formats shareable hash routes', () => {
    expect(parseHash('#/browser?view=edges&node=7').page).toBe('browser');
    expect(parseHash('#/browser?view=edges&node=7').params.get('node')).toBe('7');
    expect(parseHash('#nowhere').page).toBe('overview');
    expect(parseHash('').page).toBe('overview');
    expect(formatHash('browser', { node: MAX, edge: undefined, state: '' })).toBe(
      `#/browser?node=${MAX}`,
    );
    expect(formatHash('overview')).toBe('#/overview');
  });

  it('stores saved requests defensively', () => {
    const memory = new Map<string, string>();
    const storage: KeyValueStorage = {
      getItem: (k) => memory.get(k) ?? null,
      setItem: (k, v) => void memory.set(k, v),
    };
    expect(loadSaved(storage)).toEqual([]);
    const list = upsertSaved([], { name: 'a', text: '[]', savedAt: 't' });
    expect(storeSaved(upsertSaved(list, { name: 'a', text: '[1]', savedAt: 't2' }), storage)).toBe(
      true,
    );
    expect(loadSaved(storage)).toEqual([{ name: 'a', text: '[1]', savedAt: 't2' }]);
    memory.set('knotrel-console.saved-requests.v1', '{oops');
    expect(loadSaved(storage)).toEqual([]);
    memory.set(
      'knotrel-console.saved-requests.v1',
      '[{"name":1},{"name":"b","text":"x","savedAt":"s"}]',
    );
    expect(loadSaved(storage)).toEqual([{ name: 'b', text: 'x', savedAt: 's' }]);
    memory.set('knotrel-console.saved-requests.v1', '{"a":1}');
    expect(loadSaved(storage)).toEqual([]);
    const throwing: KeyValueStorage = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
    };
    expect(loadSaved(throwing)).toEqual([]);
    expect(storeSaved(list, throwing)).toBe(false);
  });

  it('appends pages, detects identity changes and caps rows', () => {
    const at = new Date();
    let listing = loading(emptyListing<string, string>());
    expect(listing.status).toBe('loading');
    listing = appendPage(listing, ['a', 'b'], 'b', metadata({ generation: '1' }), at, 3);
    expect(listing).toMatchObject({
      rows: ['a', 'b'],
      next: 'b',
      pages: 1,
      identityChanged: false,
    });
    listing = appendPage(listing, ['c', 'd'], 'd', metadata({ generation: '2' }), at, 3);
    expect(listing).toMatchObject({
      rows: ['a', 'b', 'c'],
      next: null,
      truncated: true,
      identityChanged: true,
    });
    expect(failPage(listing, { kind: 'aborted' })).toMatchObject({
      status: 'error',
      rows: ['a', 'b', 'c'],
    });
  });
});

describe('metrics store', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('keeps a bounded window and stops polling when metrics are disabled', async () => {
    let calls = 0;
    const metrics = vi.fn(async () => {
      calls += 1;
      return calls < 3
        ? ok('knotrel_graph_job_capacity 32\nbroken line\n')
        : failed<string>({ kind: 'http', status: 404 });
    });
    const store = new MetricsStore(fakeApi({ metrics }), { intervalMs: 1000 });
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);
    store.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.getState()).toMatchObject({ availability: 'available', skippedLines: 1 });
    await vi.advanceTimersByTimeAsync(1000);
    expect(store.getState().scrapes).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1000);
    expect(store.getState().availability).toBe('disabled');
    await vi.advanceTimersByTimeAsync(10_000);
    expect(metrics).toHaveBeenCalledTimes(3);
    expect(listener).toHaveBeenCalled();
    unsubscribe();
    store.stop();
  });

  it('reports scrape errors and ignores aborted scrapes', async () => {
    const metrics = vi
      .fn()
      .mockResolvedValueOnce(failed<string>({ kind: 'console-unreachable', message: 'x' }))
      .mockResolvedValue(failed<string>({ kind: 'aborted' }));
    const store = new MetricsStore(fakeApi({ metrics }), { intervalMs: 1000 });
    store.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.getState()).toMatchObject({ availability: 'error', error: 'console-unreachable' });
    await vi.advanceTimersByTimeAsync(2000);
    expect(store.getState().availability).toBe('error');
    store.stop();
  });
});

describe('capabilities for browsing', () => {
  it('derives browse support from inspection and engine', () => {
    expect(deriveCapabilities(info()).browseEdges).toBe(true);
    expect(deriveCapabilities(info({ inspection: false }))).toMatchObject({
      browseNodes: false,
      browseEdges: false,
    });
    expect(deriveCapabilities(info({ engine: 'hdt', topologyLifecycle: false }))).toMatchObject({
      browseNodes: true,
      browseEdges: false,
    });
    expect(deriveCapabilities(undefined).browseBlockedReason).toMatch(/Waiting/);
  });
});
