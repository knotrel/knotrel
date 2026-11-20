import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { ConsoleApi, RuntimeConfig } from '../../src/api/endpoints';
import { MetricsStore } from '../../src/state/metrics';
import { INITIAL_OVERVIEW, OverviewStore } from '../../src/state/overview';
import { App } from '../../src/ui/App';
import { attentionItems } from '../../src/ui/OverviewPage';
import { exposition, failed, fakeApi, info, ok, stats } from '../helpers';

function renderApp(api: ConsoleApi, hash = '') {
  window.location.hash = hash;
  const store = new OverviewStore(api, { intervalMs: 60_000 });
  const metricsStore = new MetricsStore(api, { intervalMs: 60_000 });
  const view = render(<App api={api} store={store} metricsStore={metricsStore} />);
  return { ...view, store, metricsStore };
}

const panel = (id: string) => document.getElementById(`panel-${id}`)!;

describe('App shell and overview', () => {
  it('summarizes connection, readiness, server and graph with exact counts', async () => {
    const api = fakeApi({
      stats: vi.fn(async () =>
        ok(stats({ nodeCount: '18446744073709551615', effectiveEdgeCount: '0' }), {
          instanceId: 'srv-9',
        }),
      ),
    });
    renderApp(api);
    const overview = within(panel('overview'));
    expect(await overview.findAllByText('18446744073709551615')).not.toHaveLength(0);
    expect(overview.getByText('http://knotrel:8080')).toBeInTheDocument();
    expect(overview.getAllByText('Ready').length).toBeGreaterThan(0);
    expect(overview.getByText(/Inspection on/)).toBeInTheDocument();
    expect(overview.getByText(/Liveness: process alive/)).toBeInTheDocument();
    expect(overview.getByText(/not exposed by the Knotrel HTTP API/)).toBeInTheDocument();
  });

  it('lists conditions that need attention', async () => {
    const api = fakeApi(
      {
        ready: vi.fn(async () => ok({ status: 'draining' as const }, {}, 503)),
        stats: vi.fn(async () => ok(stats({ maxNodes: '100', remainingNodes: '5' }))),
      },
      info({ inspection: false }),
    );
    renderApp(api);
    const overview = within(panel('overview'));
    expect(await overview.findByText(/Not ready \(Draining\)/)).toBeInTheDocument();
    expect(overview.getByText(/Less than 10% of the node capacity/)).toBeInTheDocument();
    expect(overview.getByText(/data browser is limited/)).toBeInTheDocument();
    expect(await overview.findByText(/Performance metrics are disabled/)).toBeInTheDocument();
  });

  it('reports nothing when all is well and metrics are available', async () => {
    const api = fakeApi({ metrics: vi.fn(async () => ok(exposition(1, [1, 1, 1]))) });
    renderApp(api);
    const overview = within(panel('overview'));
    expect(await overview.findByText('Nothing to report')).toBeInTheDocument();
    expect(await overview.findByText(/Metrics on/)).toBeInTheDocument();
  });

  it('shows the loading state before the first response', () => {
    const never = () => new Promise<never>(() => {});
    const api = fakeApi({
      health: vi.fn(never),
      ready: vi.fn(never),
      info: vi.fn(never),
      stats: vi.fn(never),
    });
    renderApp(api);
    expect(screen.getAllByText('Connecting').length).toBeGreaterThan(0);
    expect(screen.getByText('Configuration not loaded yet.')).toBeInTheDocument();
    expect(screen.getByText('Graph statistics not loaded yet.')).toBeInTheDocument();
  });

  it('reports a disconnected server, keeps last values and marks them stale', async () => {
    let up = true;
    const down = failed<never>({ kind: 'server-unreachable', delivered: 'no', status: 502 });
    const api = fakeApi({
      health: vi.fn(async () => (up ? ok({ status: 'ok' as const }) : down)),
      stats: vi.fn(async () => (up ? ok(stats({ nodeCount: '7777' })) : down)),
    });
    const { store } = renderApp(api);
    const overview = within(panel('overview'));
    await overview.findAllByText('7777');
    up = false;
    act(() => store.refresh());
    expect(await screen.findAllByText('Server unreachable')).not.toHaveLength(0);
    expect(overview.getAllByText('7777').length).toBeGreaterThan(0);
    expect(overview.getByText(/Displayed data is stale/)).toBeInTheDocument();
    expect(overview.getAllByText(/request was not delivered/).length).toBeGreaterThan(0);
  });

  it('reports an unreachable console host and a missing runtime config', async () => {
    const down = failed<never>({ kind: 'console-unreachable', message: 'Failed to fetch' });
    const api = fakeApi({
      health: vi.fn(async () => down),
      ready: vi.fn(async () => down),
      info: vi.fn(async () => down),
      stats: vi.fn(async () => down),
      runtimeConfig: vi.fn(async () => failed<RuntimeConfig>({ kind: 'http', status: 404 })),
    });
    renderApp(api);
    expect(await screen.findAllByText('Console host unreachable')).not.toHaveLength(0);
    expect(screen.getByText(/configured by the hosting gateway/)).toBeInTheDocument();
  });

  it('warns about incompatible responses and server restarts', async () => {
    let instance = 'srv-a';
    const api = fakeApi({
      health: vi.fn(async () =>
        failed({ kind: 'incompatible', status: 200, reason: 'health.status: x' }),
      ),
      stats: vi.fn(async () => ok(stats(), { instanceId: instance })),
    });
    const { store } = renderApp(api);
    expect(await screen.findAllByText('Incompatible server responses')).not.toHaveLength(0);
    instance = 'srv-b';
    act(() => store.refresh());
    expect(await screen.findByText('Server instance changed')).toBeInTheDocument();
    expect(screen.getByText(/process restarted during this session/)).toBeInTheDocument();
  });

  it('adapts navigation to the mode and supports the keyboard', async () => {
    const user = userEvent.setup();
    renderApp(fakeApi());
    await screen.findAllByText('Connected');
    const tabs = screen.getAllByRole('tab');
    expect(tabs.map((t) => t.textContent)).toEqual([
      'Overview',
      'Browser',
      'Workbench',
      'Performance',
    ]);
    await user.click(tabs[0]!);
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Browser' })).toHaveAttribute('aria-selected', 'true');
    expect(window.location.hash).toBe('#/browser');
    await user.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'Performance' })).toHaveFocus();
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveFocus();
    await user.keyboard('{ArrowLeft}');
    expect(screen.getByRole('tab', { name: 'Performance' })).toHaveFocus();
    await user.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveFocus();
    await user.keyboard('x');
    expect(screen.getByRole('tabpanel')).toHaveAttribute('aria-labelledby', 'tab-overview');
  });

  it('restores the browser selection from a shared URL and follows hash changes', async () => {
    renderApp(fakeApi(), '#/browser?node=18446744073709551615');
    expect(screen.getByRole('tab', { name: 'Browser' })).toHaveAttribute('aria-selected', 'true');
    expect(
      await within(panel('browser')).findByRole('region', { name: 'Node 18446744073709551615' }),
    ).toBeInTheDocument();
    act(() => {
      window.location.hash = '#/workbench';
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(screen.getByRole('tab', { name: 'Workbench' })).toHaveAttribute('aria-selected', 'true');
    act(() => {
      window.location.hash = '#/browser?view=edges';
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(screen.getByRole('button', { name: /^Edges/ })).toHaveAttribute('aria-pressed', 'true');
    act(() => {
      window.location.hash = '#/nowhere';
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveAttribute('aria-selected', 'true');
  });

  it('refreshes on demand', async () => {
    const user = userEvent.setup();
    const api = fakeApi();
    renderApp(api);
    await screen.findAllByText('Connected');
    const connection = screen.getByRole('button', { name: /http:\/\/knotrel:8080/ });
    expect(connection).toHaveAttribute('aria-expanded', 'false');
    await user.click(connection);
    const details = screen
      .getByText('Server (backend)')
      .closest('.connection-panel') as HTMLElement;
    expect(within(details).getByText('Server (backend)')).toBeInTheDocument();
    expect(within(details).getByText('Console (frontend)')).toBeInTheDocument();
    expect(within(details).getByText('compact-bfs')).toBeInTheDocument();
    expect(within(details).getByText('container')).toBeInTheDocument();
    await user.click(within(details).getByRole('button', { name: 'Refresh now' }));
    await waitFor(() => expect(api.stats).toHaveBeenCalledTimes(2));
    await user.keyboard('{Escape}');
    expect(connection).toHaveAttribute('aria-expanded', 'false');
    await user.click(connection);
    await user.click(document.body);
    expect(connection).toHaveAttribute('aria-expanded', 'false');
  });

  it('starts dark and switches theme, remembering the choice', async () => {
    const user = userEvent.setup();
    window.localStorage.removeItem('knotrel-console.theme');
    renderApp(fakeApi());
    await screen.findAllByText('Connected');
    expect(document.documentElement.dataset.theme).toBe('dark');
    await user.click(screen.getByRole('button', { name: 'Switch to light theme' }));
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(window.localStorage.getItem('knotrel-console.theme')).toBe('light');
    await user.click(screen.getByRole('button', { name: 'Switch to dark theme' }));
    expect(document.documentElement.dataset.theme).toBe('dark');
  });
});

describe('Synchronization page', () => {
  it('exists only in source-managed mode', async () => {
    renderApp(fakeApi(), '#/sync');
    await screen.findAllByText('Connected');
    expect(screen.queryByRole('tab', { name: 'Synchronization' })).toBeNull();
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveAttribute('aria-selected', 'true');
  });

  it('shows status, exact generation, version meanings and lease from metrics', async () => {
    const api = fakeApi(
      {
        ready: vi.fn(async () => ok({ status: 'desynchronized' as const }, {}, 503)),
        stats: vi.fn(async () =>
          ok(
            stats({
              syncStatus: 'desynchronized',
              generation: '18446744073709551615',
              sourcePosition: 'lsn/0',
              instanceId: 'srv-s',
            }),
            { stateVersion: '42' },
          ),
        ),
        metrics: vi.fn(async () => ok(exposition(10, [1, 2, 3]))),
      },
      info({ mode: 'source_managed', instanceId: 'srv-s' }),
    );
    renderApp(api, '#/sync');
    await screen.findByRole('tab', { name: 'Synchronization' });
    const sync = within(panel('sync'));
    expect(await sync.findByText('18446744073709551615')).toBeInTheDocument();
    expect(sync.getByText('lsn/0')).toBeInTheDocument();
    expect(sync.getByText('42')).toBeInTheDocument();
    expect(sync.getAllByText('Desynchronized').length).toBeGreaterThan(0);
    expect(sync.getAllByText(/replacement import/).length).toBeGreaterThan(0);
    expect(await sync.findByText('12.5 s (metrics)')).toBeInTheDocument();
    expect(sync.getByText('3.0 s (metrics)')).toBeInTheDocument();
    expect(sync.getByText(/cannot be subtracted/)).toBeInTheDocument();
    expect(sync.queryByRole('button', { name: /barrier|heartbeat|session|import/i })).toBeNull();
  });

  it('marks unavailable fields explicitly', async () => {
    const api = fakeApi(
      {
        ready: vi.fn(async () => failed({ kind: 'http', status: 500 })),
        stats: vi.fn(async () => failed({ kind: 'http', status: 500 })),
      },
      info({ mode: 'source_managed', instanceId: undefined }),
    );
    renderApp(api, '#/sync');
    await screen.findByRole('tab', { name: 'Synchronization' });
    const sync = within(panel('sync'));
    expect(await sync.findByText(/statistics not loaded/)).toBeInTheDocument();
    expect(sync.getByText(/readiness not loaded/)).toBeInTheDocument();
    expect(sync.getByText(/no source position yet/)).toBeInTheDocument();
    expect(await sync.findAllByText(/metrics disabled on the server/)).toHaveLength(2);
  });
});

describe('attention items', () => {
  it('covers unreachable, incompatible and edge capacity conditions', () => {
    const now = new Date();
    const metrics = {
      availability: 'available' as const,
      scrapes: [],
      error: undefined,
      skippedLines: 0,
    };
    const at = (health: Parameters<typeof failed>[0]) =>
      attentionItems({ ...INITIAL_OVERVIEW, health: failed(health) }, metrics, now).map(
        (a) => a.text,
      );
    expect(at({ kind: 'console-unreachable', message: '' })[0]).toMatch(/console host/);
    expect(at({ kind: 'server-unreachable', delivered: 'no', status: 502 })[0]).toMatch(
      /unreachable/,
    );
    expect(at({ kind: 'incompatible', status: 200, reason: '' })[0]).toMatch(/contract/);
    const edges = attentionItems(
      {
        ...INITIAL_OVERVIEW,
        lastStats: {
          value: stats({ maxEdges: '10', remainingEdges: '0' }),
          receivedAt: now,
          durationMs: 1,
        },
      },
      metrics,
      now,
    );
    expect(edges.map((a) => a.text)).toContain('Less than 10% of the edge capacity remains.');
  });
});
