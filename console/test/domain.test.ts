import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Failure, Result } from '../src/api/endpoints';
import type { Health } from '../src/api/decode';
import type { Operation } from '../src/api/wire';
import { ERROR_CODES, READINESS_LABELS, SYNC_STATUSES } from '../src/api/wire';
import { deriveCapabilities } from '../src/domain/capabilities';
import { explainServerError, summarizeFailure } from '../src/domain/errors';
import { appendHistory, HISTORY_LIMIT, type HistoryEntry } from '../src/domain/history';
import { checkNodeId, hasLeadingZeroes } from '../src/domain/node-id';
import { classifyMutation, describeOperation, planReadBack } from '../src/domain/outcomes';
import { readinessGuidance, syncGuidance } from '../src/domain/sync';
import { Poller, type VisibilitySource } from '../src/state/poller';
import { connectivityOf, INITIAL_OVERVIEW, isStale, OverviewStore } from '../src/state/overview';
import { failed, fakeApi, info, ok, stats } from './helpers';

describe('capabilities', () => {
  it('offers nothing before the configuration is known', () => {
    const caps = deriveCapabilities(undefined);
    expect(caps.mode).toBeUndefined();
    expect(caps.canMutate).toBe(false);
    expect(caps.allowed.size).toBe(0);
    expect(caps.mutationBlockedReason).toMatch(/Waiting/);
  });

  it('standalone with lifecycle allows every operation', () => {
    const caps = deriveCapabilities(info({ topologyLifecycle: true }));
    expect(caps.canMutate).toBe(true);
    expect([...caps.allowed].sort()).toEqual(
      [
        'add_node',
        'connected',
        'cut',
        'edge_state',
        'link',
        'node_state',
        'remove_node',
        'set_edge_enabled',
        'set_node_active',
      ].sort(),
    );
  });

  it('standalone without lifecycle excludes lifecycle operations', () => {
    const caps = deriveCapabilities(info({ topologyLifecycle: false }));
    expect([...caps.allowed].sort()).toEqual(['add_node', 'connected', 'cut', 'link']);
  });

  it('source-managed allows only diagnostic queries', () => {
    const caps = deriveCapabilities(info({ mode: 'source_managed', topologyLifecycle: true }));
    expect(caps.canMutate).toBe(false);
    expect([...caps.allowed].sort()).toEqual(['connected', 'edge_state', 'node_state']);
    expect(caps.mutationBlockedReason).toMatch(/authoritative source/);
  });
});

describe('error explanations', () => {
  it('explains every documented code', () => {
    for (const code of ERROR_CODES) {
      expect(explainServerError({ code, known: true, message: '' })).not.toBe('');
    }
    expect(explainServerError({ code: 'brand_new', known: false, message: '' })).toMatch(
      /Undocumented/,
    );
  });

  it.each<[Failure, RegExp]>([
    [{ kind: 'console-unreachable', message: 'x' }, /web server hosting the console/],
    [{ kind: 'server-unreachable', delivered: 'no', status: 502 }, /not delivered/],
    [{ kind: 'server-unreachable', delivered: 'unknown', status: 502 }, /may have been sent/],
    [{ kind: 'timeout', where: 'client' }, /browser/],
    [{ kind: 'timeout', where: 'gateway' }, /gateway/],
    [
      {
        kind: 'server-error',
        status: 422,
        error: { code: 'self_loop', known: true, message: 'm' },
      },
      /Self-loops/,
    ],
    [{ kind: 'gateway-rejected', status: 403, message: 'm' }, /refused/],
    [{ kind: 'http', status: 404 }, /not available/],
    [{ kind: 'http', status: 500 }, /no error body/],
    [{ kind: 'incompatible', status: 200, reason: 'r' }, /contract/],
    [{ kind: 'aborted' }, /superseded/],
  ])('summarizes %o', (failure, pattern) => {
    expect(summarizeFailure(failure).detail).toMatch(pattern);
  });
});

describe('sync guidance', () => {
  it('covers every sync status and readiness label without color-only meaning', () => {
    for (const status of SYNC_STATUSES) {
      const g = syncGuidance(status);
      expect(g.label).not.toBe('');
      expect(g.action).not.toBe('');
    }
    for (const label of READINESS_LABELS) {
      expect(readinessGuidance(label).label).not.toBe('');
    }
    expect(readinessGuidance('ready').label).toBe('Ready');
    expect(readinessGuidance('draining').severity).toBe('warning');
    expect(readinessGuidance('stale').label).toBe('Stale');
    expect(syncGuidance('desynchronized').action).toMatch(/replacement import/);
  });
});

describe('node IDs', () => {
  it('accepts the full u64 range exactly', () => {
    expect(checkNodeId('18446744073709551615')).toEqual({
      ok: true,
      value: '18446744073709551615',
    });
    expect(checkNodeId('0')).toEqual({ ok: true, value: '0' });
  });

  it.each([
    ['', /Enter/],
    [' 1', /digits/],
    ['-1', /digits/],
    ['1e3', /digits/],
    ['18446744073709551616', /u64/],
  ])('rejects %j', (input, message) => {
    const check = checkNodeId(input);
    expect(check.ok).toBe(false);
    expect(!check.ok && check.message).toMatch(message);
  });

  it('detects server normalization of leading zeroes', () => {
    expect(hasLeadingZeroes('007')).toBe(true);
    expect(hasLeadingZeroes('0')).toBe(false);
  });
});

describe('mutation outcomes', () => {
  it('classifies effective and no-op changes with their state version', () => {
    expect(
      classifyMutation(ok({ result: 'changed', changed: true }, { stateVersion: '5' })),
    ).toEqual({
      kind: 'effective',
      stateVersion: '5',
    });
    expect(
      classifyMutation(ok({ result: 'changed', changed: false }, { stateVersion: '5' })),
    ).toEqual({
      kind: 'no-op',
      stateVersion: '5',
    });
    expect(classifyMutation(ok({ result: 'connected', connected: true })).kind).toBe('uncertain');
  });

  it.each<[Failure, string]>([
    [
      {
        kind: 'server-error',
        status: 503,
        error: { code: 'request_timeout', known: true, message: '' },
      },
      'uncertain',
    ],
    [
      { kind: 'server-error', status: 503, error: { code: 'busy', known: true, message: '' } },
      'rejected',
    ],
    [{ kind: 'server-unreachable', delivered: 'no', status: 502 }, 'rejected'],
    [{ kind: 'server-unreachable', delivered: 'unknown', status: 502 }, 'uncertain'],
    [{ kind: 'gateway-rejected', status: 403, message: 'x' }, 'rejected'],
    [{ kind: 'timeout', where: 'client' }, 'uncertain'],
    [{ kind: 'timeout', where: 'gateway' }, 'uncertain'],
    [{ kind: 'console-unreachable', message: 'x' }, 'uncertain'],
    [{ kind: 'incompatible', status: 200, reason: 'x' }, 'uncertain'],
    [{ kind: 'http', status: 502 }, 'uncertain'],
    [{ kind: 'http', status: 404 }, 'rejected'],
    [{ kind: 'aborted' }, 'uncertain'],
  ])('treats %o as %s', (failure, kind) => {
    expect(classifyMutation(failed(failure)).kind).toBe(kind);
  });

  const ops: Operation[] = [
    { op: 'add_node', node: '1' },
    { op: 'link', source: '1', target: '2' },
    { op: 'cut', source: '1', target: '2' },
    { op: 'connected', source: '1', target: '2' },
    { op: 'set_node_active', node: '1', active: true },
    { op: 'set_node_active', node: '1', active: false },
    { op: 'set_edge_enabled', source: '1', target: '2', enabled: true },
    { op: 'set_edge_enabled', source: '1', target: '2', enabled: false },
    { op: 'remove_node', node: '1' },
    { op: 'node_state', node: '1' },
    { op: 'edge_state', source: '1', target: '2' },
  ];

  it('describes every operation with exact IDs', () => {
    const texts = ops.map(describeOperation);
    expect(new Set(texts).size).toBe(ops.length);
    expect(describeOperation({ op: 'add_node', node: '18446744073709551615' })).toContain(
      '18446744073709551615',
    );
  });

  it('plans read-backs with and without lifecycle support', () => {
    expect(planReadBack(ops[0]!, true).query).toEqual({ op: 'node_state', node: '1' });
    expect(planReadBack(ops[0]!, false).query).toBeUndefined();
    expect(planReadBack(ops[1]!, true).query).toEqual({
      op: 'edge_state',
      source: '1',
      target: '2',
    });
    expect(planReadBack(ops[2]!, false)).toMatchObject({
      query: { op: 'connected', source: '1', target: '2' },
      hint: expect.stringMatching(/other paths/),
    });
    expect(planReadBack(ops[3]!, true).query).toBeUndefined();
  });
});

describe('history', () => {
  const entry = (id: number): HistoryEntry => ({
    id,
    at: new Date(),
    description: `op ${id}`,
    verdict: { kind: 'no-op', stateVersion: undefined },
    instanceId: undefined,
    durationMs: 1,
  });

  it('keeps the newest entries within the bound', () => {
    let history: HistoryEntry[] = [];
    for (let i = 1; i <= HISTORY_LIMIT + 10; i += 1) {
      history = appendHistory(history, entry(i));
    }
    expect(history).toHaveLength(HISTORY_LIMIT);
    expect(history[0]!.id).toBe(HISTORY_LIMIT + 10);
    expect(history.at(-1)!.id).toBe(11);
    expect(appendHistory([], entry(1), 0)).toEqual([]);
  });
});

class FakeVisibility implements VisibilitySource {
  hidden = false;
  listeners = new Set<() => void>();
  isHidden = () => this.hidden;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  set(hidden: boolean) {
    this.hidden = hidden;
    for (const l of this.listeners) l();
  }
}

describe('Poller', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('never overlaps runs and backs off exponentially on errors', async () => {
    let active = 0;
    let maxActive = 0;
    const results = [false, false, false, true];
    const task = vi.fn(async () => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await new Promise((r) => setTimeout(r, 50));
      active -= 1;
      return results.shift() ?? true;
    });
    const poller = new Poller({
      task,
      intervalMs: 1000,
      maxBackoffMs: 3000,
      visibility: new FakeVisibility(),
    });
    poller.start();
    poller.start(); // idempotent
    await vi.advanceTimersByTimeAsync(50);
    expect(poller.failures).toBe(1);
    expect(poller.nextDelay()).toBe(2000);
    await vi.advanceTimersByTimeAsync(2050);
    expect(poller.failures).toBe(2);
    expect(poller.nextDelay()).toBe(3000); // capped
    await vi.advanceTimersByTimeAsync(3050);
    await vi.advanceTimersByTimeAsync(3050);
    expect(poller.failures).toBe(0);
    expect(poller.nextDelay()).toBe(1000);
    expect(maxActive).toBe(1);
    poller.stop();
  });

  it('aborts the obsolete run on manual refresh and ignores its result', async () => {
    const signals: AbortSignal[] = [];
    const task = vi.fn(
      (signal: AbortSignal) =>
        new Promise<boolean>((resolve) => {
          signals.push(signal);
          setTimeout(() => resolve(false), 100);
        }),
    );
    const poller = new Poller({
      task,
      intervalMs: 1000,
      maxBackoffMs: 5000,
      visibility: new FakeVisibility(),
    });
    poller.start();
    poller.refreshNow();
    expect(signals[0]!.aborted).toBe(true);
    expect(signals[1]!.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(100);
    expect(poller.failures).toBe(1); // only the current run counted
    poller.stop();
    expect(signals[1]!.aborted).toBe(false);
    poller.refreshNow(); // no-op after stop
    expect(task).toHaveBeenCalledTimes(2);
  });

  it('pauses while hidden and resumes immediately when visible', async () => {
    const visibility = new FakeVisibility();
    const task = vi.fn(async () => true);
    const poller = new Poller({ task, intervalMs: 1000, maxBackoffMs: 5000, visibility });
    poller.start();
    await vi.advanceTimersByTimeAsync(0);
    visibility.set(true);
    await vi.advanceTimersByTimeAsync(1000);
    const callsWhileHidden = task.mock.calls.length;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(task.mock.calls.length).toBe(callsWhileHidden);
    visibility.set(false);
    await vi.advanceTimersByTimeAsync(0);
    expect(task.mock.calls.length).toBe(callsWhileHidden + 1);
    poller.stop();
    visibility.set(false);
    expect(visibility.listeners.size).toBe(0);
  });

  it('treats a throwing task as a failure', async () => {
    const poller = new Poller({
      task: async () => {
        throw new Error('x');
      },
      intervalMs: 10,
      maxBackoffMs: 100,
      visibility: new FakeVisibility(),
    });
    poller.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(poller.failures).toBe(1);
    poller.stop();
  });
});

describe('OverviewStore', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('publishes reads, tracks the instance and detects restarts', async () => {
    let instance = 'srv-a';
    const api = fakeApi({ stats: vi.fn(async () => ok(stats(), { instanceId: instance })) });
    const store = new OverviewStore(api, { intervalMs: 1000, visibility: new FakeVisibility() });
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);
    store.start();
    await vi.advanceTimersByTimeAsync(0);
    let state = store.getState();
    expect(state.instanceId).toBe('srv-a');
    expect(state.lastSuccessAt).toBeDefined();
    expect(state.runtimeConfig?.ok).toBe(true);
    expect(connectivityOf(state)).toBe('connected');
    instance = 'srv-b';
    await vi.advanceTimersByTimeAsync(1000);
    state = store.getState();
    expect(state.instanceChange).toMatchObject({ from: 'srv-a', to: 'srv-b' });
    expect(listener).toHaveBeenCalled();
    unsubscribe();
    store.stop();
  });

  it('keeps last good values while failing and marks them stale', async () => {
    let up = true;
    const fail = failed<never>({ kind: 'server-unreachable', delivered: 'no', status: 502 });
    const api = fakeApi({
      health: vi.fn(async () => (up ? ok({ status: 'ok' as const }) : fail)),
      stats: vi.fn(async () => (up ? ok(stats()) : fail)),
    });
    const store = new OverviewStore(api, { intervalMs: 1000, visibility: new FakeVisibility() });
    store.start();
    await vi.advanceTimersByTimeAsync(0);
    up = false;
    store.refresh();
    await vi.advanceTimersByTimeAsync(0);
    const state = store.getState();
    expect(state.consecutiveFailures).toBe(1);
    expect(state.lastStats?.value.nodeCount).toBe('2');
    expect(connectivityOf(state)).toBe('server-unreachable');
    expect(isStale(state, new Date())).toBe(true);
    store.stop();
  });

  it('does not publish results of a superseded cycle', async () => {
    const api = fakeApi({
      health: vi.fn(
        (signal?: AbortSignal) =>
          new Promise<Result<Health>>((resolve) => {
            setTimeout(() => resolve(ok({ status: 'ok' as const })), signal?.aborted ? 0 : 10);
          }),
      ),
    });
    const store = new OverviewStore(api, { intervalMs: 1000, visibility: new FakeVisibility() });
    store.start();
    store.refresh();
    await vi.advanceTimersByTimeAsync(20);
    expect(store.getState().lastSuccessAt).toBeDefined();
    store.stop();
  });

  it('derives connectivity for each failure kind', () => {
    const at = (failure: Parameters<typeof failed>[0]) =>
      connectivityOf({ ...INITIAL_OVERVIEW, health: failed(failure) });
    expect(connectivityOf(INITIAL_OVERVIEW)).toBe('connecting');
    expect(at({ kind: 'console-unreachable', message: '' })).toBe('console-unreachable');
    expect(at({ kind: 'timeout', where: 'gateway' })).toBe('server-unreachable');
    expect(at({ kind: 'timeout', where: 'client' })).toBe('degraded');
    expect(at({ kind: 'incompatible', status: 200, reason: '' })).toBe('incompatible');
    expect(at({ kind: 'http', status: 500 })).toBe('degraded');
  });

  it('marks data stale after three missed intervals', () => {
    const lastSuccessAt = new Date('2026-11-20T10:00:00Z');
    const state = { ...INITIAL_OVERVIEW, lastSuccessAt, stats: ok(stats()) };
    expect(isStale(INITIAL_OVERVIEW, new Date())).toBe(false);
    expect(isStale(state, new Date(lastSuccessAt.getTime() + 14_000))).toBe(false);
    expect(isStale(state, new Date(lastSuccessAt.getTime() + 16_000))).toBe(true);
  });
});
