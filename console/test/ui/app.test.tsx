import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { ConsoleApi, RuntimeConfig } from '../../src/api/endpoints';
import { OverviewStore } from '../../src/state/overview';
import { App } from '../../src/ui/App';
import { failed, fakeApi, info, ok, stats } from '../helpers';

function renderApp(api: ConsoleApi, hash = '') {
  window.location.hash = hash;
  const store = new OverviewStore(api, { intervalMs: 60_000 });
  const view = render(<App api={api} store={store} />);
  return { ...view, store };
}

describe('App shell', () => {
  it('shows connection, mode, server facts and exact counts', async () => {
    const api = fakeApi({
      stats: vi.fn(async () =>
        ok(stats({ nodeCount: '18446744073709551615' }), {
          instanceId: 'srv-9',
          stateVersion: '123456789012345678901234567890',
        }),
      ),
    });
    renderApp(api);
    expect(await screen.findAllByText('Connected')).not.toHaveLength(0);
    expect(screen.getByText('18446744073709551615')).toBeInTheDocument();
    expect(screen.getByText('123456789012345678901234567890')).toBeInTheDocument();
    expect(screen.getByText('srv-9')).toBeInTheDocument();
    expect(screen.getByText('http://knotrel:8080')).toBeInTheDocument();
    expect(screen.getByText(/not exposed by the Knotrel HTTP API/)).toBeInTheDocument();
    expect(screen.getAllByText('standalone').length).toBeGreaterThan(0);
    expect(screen.getByText('Process alive')).toBeInTheDocument();
    expect(screen.getByText('Ready')).toBeInTheDocument();
  });

  it('distinguishes liveness from readiness', async () => {
    const api = fakeApi({ ready: vi.fn(async () => ok({ status: 'draining' as const }, {}, 503)) });
    renderApp(api);
    expect(await screen.findByText('Process alive')).toBeInTheDocument();
    expect(screen.getByText('Draining')).toBeInTheDocument();
    expect(screen.getByText(/HTTP 503/, { selector: 'dd' })).toBeInTheDocument();
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

  it('reports a disconnected server and keeps the last values marked stale', async () => {
    let up = true;
    const down = failed<never>({ kind: 'server-unreachable', delivered: 'no', status: 502 });
    const api = fakeApi({
      health: vi.fn(async () => (up ? ok({ status: 'ok' as const }) : down)),
      stats: vi.fn(async () => (up ? ok(stats({ nodeCount: '7' })) : down)),
    });
    const { store } = renderApp(api);
    await screen.findByText('7');
    up = false;
    act(() => store.refresh());
    expect(await screen.findAllByText('Server unreachable')).not.toHaveLength(0);
    expect(screen.getByText('7')).toBeInTheDocument();
    expect(screen.getAllByText('Stale data').length).toBeGreaterThan(0);
    expect(screen.getAllByText(/request was not delivered/).length).toBeGreaterThan(0);
  });

  it('reports an unreachable console host', async () => {
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

  it('marks lifecycle-only counts as unavailable', async () => {
    const api = fakeApi(
      {
        stats: vi.fn(async () =>
          ok(
            stats({
              activeNodeCount: undefined,
              enabledEdgeCount: undefined,
              effectiveEdgeCount: undefined,
            }),
          ),
        ),
      },
      info({ topologyLifecycle: false }),
    );
    renderApp(api);
    const graph = await screen.findByRole('region', { name: 'Graph' });
    expect(await within(graph).findAllByText(/requires lifecycle support/)).toHaveLength(3);
  });

  it('warns about incompatible responses and server instance changes', async () => {
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
  });

  it('supports keyboard navigation across tabs', async () => {
    const user = userEvent.setup();
    renderApp(fakeApi());
    const tabs = screen.getAllByRole('tab');
    expect(tabs.map((t) => t.textContent)).toEqual([
      'Overview',
      'Graph/Queries',
      'Synchronization',
      'Operations',
    ]);
    await user.click(tabs[0]!);
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Graph/Queries' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(screen.getByRole('tab', { name: 'Graph/Queries' })).toHaveFocus();
    await user.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'Operations' })).toHaveFocus();
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveFocus();
    await user.keyboard('{ArrowLeft}');
    expect(screen.getByRole('tab', { name: 'Operations' })).toHaveFocus();
    await user.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: 'Overview' })).toHaveFocus();
    await user.keyboard('a');
    expect(window.location.hash).toBe('#overview');
    expect(screen.getByRole('tabpanel')).toHaveAttribute('aria-labelledby', 'tab-overview');
  });

  it('opens the page named by the hash and follows hash changes', async () => {
    renderApp(fakeApi(), '#sync');
    expect(screen.getByRole('tab', { name: 'Synchronization' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    act(() => {
      window.location.hash = '#operations';
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(screen.getByRole('tab', { name: 'Operations' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('refreshes on demand', async () => {
    const user = userEvent.setup();
    const api = fakeApi();
    renderApp(api);
    await screen.findAllByText('Connected');
    await user.click(await screen.findByRole('button', { name: 'Refresh now' }));
    await waitFor(() => expect(api.stats).toHaveBeenCalledTimes(2));
  });
});

describe('Synchronization page', () => {
  it('explains that standalone mode has no synchronization', async () => {
    renderApp(fakeApi(), '#sync');
    expect(await screen.findByText('Not applicable')).toBeInTheDocument();
  });

  it('waits for configuration', () => {
    renderApp(fakeApi({ info: vi.fn(() => new Promise<never>(() => {})) }), '#sync');
    const panel = document.getElementById('panel-sync')!;
    expect(within(panel).getByText('Waiting for the server configuration.')).toBeInTheDocument();
  });

  it('shows source-managed status, exact generation and guidance without inferring lag', async () => {
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
          ),
        ),
      },
      info({ mode: 'source_managed', instanceId: 'srv-s' }),
    );
    renderApp(api, '#sync');
    expect(await screen.findByText('18446744073709551615')).toBeInTheDocument();
    expect(screen.getByText('lsn/0')).toBeInTheDocument();
    expect(screen.getAllByText('Desynchronized').length).toBeGreaterThan(0);
    expect(screen.getByText(/replacement import/)).toBeInTheDocument();
    expect(screen.getByText(/not inferred from readiness/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /barrier|heartbeat|session|import/i })).toBeNull();
  });

  it('marks missing source-managed fields as unavailable', async () => {
    const api = fakeApi(
      {
        ready: vi.fn(async () => failed({ kind: 'http', status: 500 })),
        stats: vi.fn(async () => failed({ kind: 'http', status: 500 })),
      },
      info({ mode: 'source_managed', instanceId: undefined }),
    );
    renderApp(api, '#sync');
    expect(await screen.findByText(/statistics not loaded/)).toBeInTheDocument();
    expect(screen.getByText(/readiness not loaded/)).toBeInTheDocument();
    const section = screen.getByRole('region', { name: 'Synchronization' });
    expect(within(section).getByText(/no source position yet/)).toBeInTheDocument();
  });
});
