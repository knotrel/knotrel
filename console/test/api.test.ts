import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';
import { ApiClient, type Transport } from '../src/api/client';
import {
  DecodeError,
  decodeHealth,
  decodeInfo,
  decodeOperationOutcome,
  decodeReadiness,
  decodeServerError,
  decodeStats,
  decimalString,
  isKnownErrorCode,
} from '../src/api/decode';
import {
  classify,
  createConsoleApi,
  MUTATION_TIMEOUT_MS,
  READ_TIMEOUT_MS,
} from '../src/api/endpoints';
import {
  isJsonObject,
  JsonNumber,
  JsonSyntaxError,
  MAX_DEPTH,
  parseLosslessJson,
} from '../src/api/lossless-json';
import { compareDecimal, isEmptyMetadata, readVersionMetadata } from '../src/api/metadata';
import { ERROR_CODES, SYNC_STATUSES } from '../src/api/wire';
import { EMPTY_METADATA } from './helpers';

const U64_MAX = '18446744073709551615';

describe('parseLosslessJson', () => {
  it('keeps integers above 2^53 exact', () => {
    const value = parseLosslessJson(`{"generation": ${U64_MAX}, "n": 9007199254740993}`);
    expect(isJsonObject(value)).toBe(true);
    const obj = value as Record<string, JsonNumber>;
    expect(obj.generation?.raw).toBe(U64_MAX);
    expect(obj.n?.raw).toBe('9007199254740993');
    expect(String(JSON.parse('9007199254740993'))).not.toBe('9007199254740993');
  });

  it('parses every JSON value kind and escapes', () => {
    const value = parseLosslessJson(
      ' [true, false, null, -1.5e3, "a\\"b\\\\c\\/\\b\\f\\n\\r\\t\\u00e9", {}, []] ',
    );
    expect(Array.isArray(value)).toBe(true);
    const items = value as unknown[];
    expect(items.slice(0, 3)).toEqual([true, false, null]);
    expect((items[3] as JsonNumber).raw).toBe('-1.5e3');
    expect((items[3] as JsonNumber).isUnsignedInteger()).toBe(false);
    expect(items[4]).toBe('a"b\\c/\b\f\n\r\té');
    expect(isJsonObject(items[5] as never)).toBe(true);
    expect(items[6]).toEqual([]);
  });

  it('creates prototype-less objects', () => {
    const value = parseLosslessJson('{"__proto__": {"polluted": true}}') as Record<string, unknown>;
    expect(Object.getPrototypeOf(value)).toBeNull();
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  });

  it.each([
    ['', 'empty'],
    ['{', 'unterminated object'],
    ['{"a" 1}', 'missing colon'],
    ['{"a":1,}', 'trailing comma'],
    ['{1:2}', 'non-string key'],
    ['[1 2]', 'missing comma'],
    ['"abc', 'unterminated string'],
    ['"\\x"', 'bad escape'],
    ['"\\u12"', 'bad unicode'],
    ['"a\u0001"', 'control character'],
    ['01', 'leading zero'],
    ['{"a":1,"a":2}', 'duplicate key'],
    ['true false', 'trailing'],
    ['nul', 'bad literal'],
  ])('rejects %j (%s)', (text) => {
    expect(() => parseLosslessJson(text)).toThrow(JsonSyntaxError);
  });

  it('bounds nesting depth', () => {
    const deep = '['.repeat(MAX_DEPTH + 2) + ']'.repeat(MAX_DEPTH + 2);
    expect(() => parseLosslessJson(deep)).toThrow(/too deep/);
    const ok = '['.repeat(MAX_DEPTH) + ']'.repeat(MAX_DEPTH);
    expect(() => parseLosslessJson(ok)).not.toThrow();
  });
});

describe('decoders', () => {
  const j = (text: string) => parseLosslessJson(text);

  it('decodes standalone info without mode as standalone', () => {
    const info = decodeInfo(
      j(
        '{"engine":"ett","experimental":true,"max_pending_jobs":16,"max_body_bytes":1048576,"max_batch_operations":1024,"max_nodes":null,"max_edges":null}',
      ),
    );
    expect(info).toMatchObject({
      engine: 'ett',
      experimental: true,
      mode: 'standalone',
      maxNodes: null,
      topologyLifecycle: false,
      instanceId: undefined,
    });
  });

  it('decodes source-managed info with instance and lifecycle', () => {
    const info = decodeInfo(
      j(
        '{"engine":"compact-bfs","experimental":false,"max_pending_jobs":32,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":5,"max_edges":6,"mode":"source_managed","instance_id":"srv-x","topology_lifecycle":true,"future":1}',
      ),
    );
    expect(info.mode).toBe('source_managed');
    expect(info.instanceId).toBe('srv-x');
    expect(info.topologyLifecycle).toBe(true);
    expect(info.maxNodes).toBe('5');
  });

  it.each([
    ['{"engine":"bogus"}', /engine/],
    [
      '{"engine":"ett","experimental":false,"max_pending_jobs":1,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":1,"max_edges":1,"inspection":false}',
      /inspection/,
    ],
    ['[]', /expected an object/],
    [
      '{"engine":"ett","experimental":"no","max_pending_jobs":1,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":1,"max_edges":1}',
      /experimental/,
    ],
    [
      '{"engine":"ett","experimental":false,"max_pending_jobs":-1,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":1,"max_edges":1}',
      /max_pending_jobs/,
    ],
    [
      '{"engine":"ett","experimental":false,"max_pending_jobs":1,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":1,"max_edges":1,"topology_lifecycle":false}',
      /topology_lifecycle/,
    ],
    [
      '{"engine":"ett","experimental":false,"max_pending_jobs":1,"max_body_bytes":1,"max_batch_operations":1,"max_nodes":1,"max_edges":1,"mode":"standalone"}',
      /mode/,
    ],
  ])('rejects malformed info %s', (text, message) => {
    expect(() => decodeInfo(j(text))).toThrow(message);
  });

  it('decodes stats with an exact u64 generation and rejects overflow', () => {
    const stats = decodeStats(
      j(
        `{"node_count":1,"edge_count":0,"max_nodes":null,"max_edges":null,"remaining_nodes":null,"remaining_edges":null,"state_version":"123456789012345678901234567890","instance_id":"srv","generation":${U64_MAX},"source_position":"0/16B3740","sync_status":"catching_up"}`,
      ),
    );
    expect(stats.generation).toBe(U64_MAX);
    expect(stats.stateVersion).toBe('123456789012345678901234567890');
    expect(stats.syncStatus).toBe('catching_up');
    expect(stats.activeNodeCount).toBeUndefined();
    const lifecycle = decodeStats(
      j(
        '{"node_count":3,"edge_count":2,"active_node_count":2,"enabled_edge_count":1,"effective_edge_count":0,"max_nodes":null,"max_edges":null,"remaining_nodes":null,"remaining_edges":null,"state_version":"1"}',
      ),
    );
    expect([
      lifecycle.activeNodeCount,
      lifecycle.enabledEdgeCount,
      lifecycle.effectiveEdgeCount,
    ]).toEqual(['2', '1', '0']);
    expect(() =>
      decodeStats(
        j(
          '{"node_count":1,"edge_count":0,"max_nodes":null,"max_edges":null,"remaining_nodes":null,"remaining_edges":null,"state_version":"1","generation":18446744073709551616}',
        ),
      ),
    ).toThrow(/out of range/);
    expect(() =>
      decodeStats(
        j(
          '{"node_count":1.5,"edge_count":0,"max_nodes":null,"max_edges":null,"remaining_nodes":null,"remaining_edges":null,"state_version":"1"}',
        ),
      ),
    ).toThrow(/node_count/);
    expect(() => decimalString(j('"12a"'), 'x')).toThrow(DecodeError);
  });

  it('decodes health, readiness and outcomes', () => {
    expect(decodeHealth(j('{"status":"ok"}'))).toEqual({ status: 'ok' });
    expect(decodeReadiness(j('{"status":"desynchronized"}')).status).toBe('desynchronized');
    expect(() => decodeReadiness(j('{"status":"weird"}'))).toThrow(/unsupported/);
    expect(decodeOperationOutcome(j('{"result":"changed","changed":false}'))).toEqual({
      result: 'changed',
      changed: false,
    });
    expect(decodeOperationOutcome(j('{"result":"connected","connected":true}'))).toEqual({
      result: 'connected',
      connected: true,
    });
    expect(
      decodeOperationOutcome(j('{"result":"node_state","registered":true,"active":false}')),
    ).toEqual({ result: 'node_state', registered: true, active: false });
    expect(
      decodeOperationOutcome(
        j('{"result":"edge_state","registered":true,"enabled":true,"effective":false}'),
      ),
    ).toEqual({ result: 'edge_state', registered: true, enabled: true, effective: false });
    expect(() => decodeOperationOutcome(j('{"result":"connected"}'))).toThrow(/connected/);
  });

  it('keeps unknown error codes instead of rejecting them', () => {
    expect(decodeServerError(j('{"result":"error","code":"self_loop","message":"m"}'))).toEqual({
      code: 'self_loop',
      known: true,
      message: 'm',
    });
    expect(
      decodeServerError(j('{"result":"error","code":"new_code","message":"<b>x</b>"}')),
    ).toEqual({ code: 'new_code', known: false, message: '<b>x</b>' });
    expect(isKnownErrorCode('busy')).toBe(true);
    expect(() => decodeServerError(j('{"result":"changed"}'))).toThrow();
  });
});

describe('OpenAPI enumerations', () => {
  const spec = readFileSync(resolve(process.cwd(), '../docs/openapi.yaml'), 'utf8');

  it('lists exactly the documented error codes', () => {
    const block = spec.split('    ErrorCode:')[1]!.split('    ErrorOutcome:')[0]!;
    const codes = [...block.matchAll(/^ {8}- ([a-z_]+)$/gm)].map((m) => m[1]);
    expect([...ERROR_CODES].sort()).toEqual(codes.sort());
  });

  it('lists exactly the documented sync statuses', () => {
    const block = spec.split('    FeederSyncStatus:')[1]!.split('    SessionRequest:')[0]!;
    const values = [...block.matchAll(/^ {6}- ([a-z_]+)$/gm)].map((m) => m[1]);
    expect([...SYNC_STATUSES]).toEqual(values);
  });
});

describe('readVersionMetadata', () => {
  it('reads exact values and decodes the source position', () => {
    const headers = new Headers({
      'x-knotrel-instance-id': 'srv-1',
      'x-knotrel-graph-id': 'default',
      'x-knotrel-generation': U64_MAX,
      'x-knotrel-state-version': '99999999999999999999999',
      'x-knotrel-source-position': '0%2F16B3740',
      'x-knotrel-sync-status': 'ready',
      etag: '"1.2.0%2F16B3740"',
    });
    const m = readVersionMetadata(headers);
    expect(m).toMatchObject({
      generation: U64_MAX,
      stateVersion: '99999999999999999999999',
      sourcePosition: '0/16B3740',
      syncStatus: 'ready',
      invalid: [],
    });
    expect(isEmptyMetadata(m)).toBe(false);
  });

  it('flags malformed headers without guessing', () => {
    const m = readVersionMetadata(
      new Headers({
        'x-knotrel-generation': '1e3',
        'x-knotrel-source-position': '%E0%A4%A',
        'x-knotrel-sync-status': 'bogus',
      }),
    );
    expect(m.generation).toBeUndefined();
    expect(m.sourcePosition).toBe('%E0%A4%A');
    expect(m.invalid).toEqual([
      'x-knotrel-source-position',
      'x-knotrel-sync-status',
      'x-knotrel-generation',
    ]);
    expect(isEmptyMetadata(readVersionMetadata(new Headers()))).toBe(true);
  });

  it('compares revisions with arbitrary precision', () => {
    expect(compareDecimal('9007199254740993', '9007199254740992')).toBe(1);
    expect(compareDecimal('2', '10')).toBe(-1);
    expect(compareDecimal('7', '7')).toBe(0);
  });
});

function response(status: number, body: string, headers: Record<string, string> = {}) {
  return new Response(body === '' ? null : body, { status, headers });
}

describe('ApiClient', () => {
  it('sends JSON POSTs and measures duration', async () => {
    let t = 0;
    const fetch = vi.fn(async () => {
      t = 42;
      return response(200, '{"result":"changed","changed":true}', {
        'x-knotrel-state-version': '3',
      });
    });
    const client = new ApiClient({ baseUrl: '/api/', fetch, now: () => t });
    const result = await client.request('/v1/operations', {
      body: { op: 'add_node', node: '1' },
      timeoutMs: 1000,
    });
    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/operations',
      expect.objectContaining({
        method: 'POST',
        body: '{"op":"add_node","node":"1"}',
        redirect: 'error',
      }),
    );
    expect(result).toMatchObject({ kind: 'response', status: 200, durationMs: 42 });
    expect(result.kind === 'response' && result.metadata.stateVersion).toBe('3');
  });

  it('reports empty and invalid bodies and proxy conditions', async () => {
    const client = new ApiClient({
      fetch: vi
        .fn()
        .mockResolvedValueOnce(response(404, ''))
        .mockResolvedValueOnce(response(200, '<html>'))
        .mockResolvedValueOnce(
          response(502, '{"result":"error","code":"console_upstream_unreachable","message":"m"}', {
            'x-knotrel-console-proxy': 'upstream_unreachable',
          }),
        ),
    });
    const a = await client.request('health', { timeoutMs: 1000 });
    expect(a).toMatchObject({
      kind: 'response',
      status: 404,
      body: undefined,
      bodyError: undefined,
      text: '',
    });
    const b = await client.request('health', { timeoutMs: 1000 });
    expect(b.kind === 'response' && b.bodyError).toMatch(/invalid JSON/);
    const c = await client.request('health', { timeoutMs: 1000 });
    expect(c.kind === 'response' && c.proxy).toBe('upstream_unreachable');
  });

  const hanging = () =>
    vi.fn(
      (_url: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () =>
            reject(new DOMException('aborted', 'AbortError')),
          );
        }),
    );

  it('times out', async () => {
    const client = new ApiClient({ fetch: hanging() });
    expect((await client.request('health', { timeoutMs: 5 })).kind).toBe('timeout');
  });

  it('honors caller aborts, including already-aborted signals', async () => {
    const client = new ApiClient({ fetch: hanging() });
    const controller = new AbortController();
    const pending = client.request('health', { timeoutMs: 10_000, signal: controller.signal });
    controller.abort();
    expect((await pending).kind).toBe('aborted');
    const pre = new AbortController();
    pre.abort();
    expect((await client.request('health', { timeoutMs: 10_000, signal: pre.signal })).kind).toBe(
      'aborted',
    );
  });

  it('reports network errors', async () => {
    const client = new ApiClient({
      fetch: vi.fn().mockRejectedValue(new TypeError('Failed to fetch')),
    });
    expect(await client.request('health', { timeoutMs: 1000 })).toMatchObject({
      kind: 'network',
      message: 'Failed to fetch',
    });
    const odd = new ApiClient({ fetch: vi.fn().mockRejectedValue('weird') });
    expect(await odd.request('health', { timeoutMs: 1000 })).toMatchObject({
      message: 'network error',
    });
  });
});

describe('classify', () => {
  const base = {
    kind: 'response' as const,
    body: undefined,
    text: '',
    bodyError: undefined,
    metadata: EMPTY_METADATA,
    proxy: undefined,
    durationMs: 3,
  };
  const j = (text: string) => parseLosslessJson(text);
  const decode = (body: unknown) => body;

  it.each<[Transport, string]>([
    [{ kind: 'aborted', durationMs: 1 }, 'aborted'],
    [{ kind: 'timeout', durationMs: 1 }, 'timeout'],
    [{ kind: 'network', message: 'x', durationMs: 1 }, 'console-unreachable'],
    [{ ...base, status: 502, proxy: 'upstream_unreachable' }, 'server-unreachable'],
    [{ ...base, status: 502, proxy: 'upstream_failed' }, 'server-unreachable'],
    [{ ...base, status: 502, proxy: 'response_too_large' }, 'server-unreachable'],
    [{ ...base, status: 504, proxy: 'upstream_timeout' }, 'timeout'],
    [{ ...base, status: 403, proxy: 'rejected', body: j('{"message":"no"}') }, 'gateway-rejected'],
    [{ ...base, status: 403, proxy: 'rejected' }, 'gateway-rejected'],
    [{ ...base, status: 200, bodyError: 'bad' }, 'incompatible'],
    [{ ...base, status: 200 }, 'incompatible'],
    [{ ...base, status: 404 }, 'http'],
    [{ ...base, status: 500, body: j('{"x":1}') }, 'incompatible'],
    [
      { ...base, status: 409, body: j('{"result":"error","code":"busy","message":"m"}') },
      'server-error',
    ],
  ])('classifies %o as %s', (transport, kind) => {
    const result = classify(transport, [200], decode);
    expect(result.ok).toBe(false);
    expect(!result.ok && result.failure.kind).toBe(kind);
  });

  it('distinguishes delivered and undelivered upstream failures', () => {
    const no = classify({ ...base, status: 502, proxy: 'upstream_unreachable' }, [200], decode);
    const maybe = classify({ ...base, status: 502, proxy: 'upstream_failed' }, [200], decode);
    expect(!no.ok && no.failure).toMatchObject({ delivered: 'no' });
    expect(!maybe.ok && maybe.failure).toMatchObject({ delivered: 'unknown' });
  });

  it('decodes accepted statuses and reports decode errors', () => {
    const good = classify(
      { ...base, status: 503, body: j('{"status":"draining"}') },
      [200, 503],
      decodeReadiness,
    );
    expect(good.ok && good.value.status).toBe('draining');
    const bad = classify(
      { ...base, status: 200, body: j('{"status":"x"}') },
      [200],
      decodeReadiness,
    );
    expect(!bad.ok && bad.failure).toMatchObject({
      kind: 'incompatible',
      reason: expect.stringMatching(/status/),
    });
    const thrown = classify({ ...base, status: 200, body: j('1') }, [200], () => {
      throw new Error('boom');
    });
    expect(!thrown.ok && thrown.failure).toMatchObject({ reason: 'undecodable response' });
  });
});

describe('createConsoleApi', () => {
  it('routes each method to its endpoint with bounded timeouts', async () => {
    const request = vi.fn(async (): Promise<Transport> => ({
      kind: 'response',
      status: 200,
      body: parseLosslessJson('{"status":"ok"}'),
      bodyError: undefined,
      text: '',
      metadata: EMPTY_METADATA,
      proxy: undefined,
      durationMs: 1,
    }));
    const api = createConsoleApi({ request } as unknown as ApiClient);
    const signal = new AbortController().signal;
    expect((await api.health(signal)).ok).toBe(true);
    await api.ready();
    await api.info();
    await api.stats(signal);
    await api.runtimeConfig(signal);
    await api.runtimeConfig();
    await api.operation({ op: 'add_node', node: '1' }, 'mutation');
    await api.operation({ op: 'connected', source: '1', target: '2' }, 'query');
    const paths = request.mock.calls.map((c) => (c as unknown[])[0]);
    expect(paths).toEqual([
      'health',
      'ready',
      'v1/info',
      'v1/stats',
      '../console-config.json',
      '../console-config.json',
      'v1/operations',
      'v1/operations',
    ]);
    const options = request.mock.calls.map((c) => (c as unknown[])[1] as { timeoutMs: number });
    expect(options[6]!.timeoutMs).toBe(MUTATION_TIMEOUT_MS);
    expect(options[7]!.timeoutMs).toBe(READ_TIMEOUT_MS);
  });

  it('decodes optional runtime configuration', async () => {
    const make = (body: string) =>
      createConsoleApi({
        request: async (): Promise<Transport> => ({
          kind: 'response',
          status: 200,
          body: parseLosslessJson(body),
          bodyError: undefined,
          text: '',
          metadata: EMPTY_METADATA,
          proxy: undefined,
          durationMs: 1,
        }),
      } as unknown as ApiClient);
    const good = await make(
      '{"serverLabel":"http://knotrel:8080","deployment":"container"}',
    ).runtimeConfig();
    expect(good.ok && good.value).toEqual({
      serverLabel: 'http://knotrel:8080',
      deployment: 'container',
    });
    const empty = await make('{"serverLabel":"","deployment":null}').runtimeConfig();
    expect(empty.ok && empty.value).toEqual({ serverLabel: undefined, deployment: undefined });
    expect((await make('{"serverLabel":1}').runtimeConfig()).ok).toBe(false);
    expect((await make('[]').runtimeConfig()).ok).toBe(false);
  });
});
