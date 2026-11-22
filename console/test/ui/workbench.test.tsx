import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { BatchEntry, OperationOutcome } from '../../src/api/decode';
import type { ConsoleApi, Result } from '../../src/api/endpoints';
import { deriveCapabilities } from '../../src/domain/capabilities';
import { appendHistory, type HistoryEntry } from '../../src/domain/history';
import { MetricsStore } from '../../src/state/metrics';
import { parseExposition } from '../../src/domain/prometheus';
import {
  byLane,
  laneSaturation,
  PerformancePage,
  windowScrapes,
} from '../../src/ui/PerformancePage';
import type { QueryPreset } from '../../src/ui/presets';
import { WorkbenchPage } from '../../src/ui/WorkbenchPage';
import { exposition, failed, fakeApi, info, ok } from '../helpers';

afterEach(() => {
  localStorage.clear();
  vi.useRealTimers();
});

function Harness({
  api,
  serverInfo = info(),
  preset,
}: {
  api: ConsoleApi;
  serverInfo?: ReturnType<typeof info>;
  preset?: QueryPreset;
}) {
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  return (
    <WorkbenchPage
      api={api}
      capabilities={deriveCapabilities(serverInfo)}
      maxBatch={1024}
      history={history}
      record={(entry) => setHistory((h) => appendHistory(h, { ...entry, id: h.length + 1 }))}
      onClearHistory={() => setHistory([])}
      preset={preset}
      base="https://console.example/ops/"
    />
  );
}

const editor = () => screen.getByLabelText('Batch request JSON') as HTMLTextAreaElement;
const setEditor = (text: string) => fireEvent.change(editor(), { target: { value: text } });

describe('workbench queries', () => {
  it('runs a query with exact IDs, shows timing labels, raw view and curl', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const api = fakeApi({
      operation: vi.fn(async () =>
        ok<OperationOutcome>({ result: 'connected', connected: true }, { stateVersion: '77' }),
      ),
    });
    render(<Harness api={api} preset={{ op: 'connected', a: '18446744073709551615', b: '' }} />);
    const query = screen.getByRole('region', { name: 'Query' });
    expect(within(query).getByLabelText('Source')).toHaveValue('18446744073709551615');
    await user.type(within(query).getByLabelText('Target'), '9007199254740993');
    await user.click(within(query).getByRole('button', { name: 'Run query' }));
    expect(api.operation).toHaveBeenCalledWith(
      { op: 'connected', source: '18446744073709551615', target: '9007199254740993' },
      'query',
    );
    expect(await within(query).findByText('Connected')).toBeInTheDocument();
    expect(within(query).getByText('77')).toBeInTheDocument();
    expect(
      within(query).getByText(/Server processing time is not reported per request/),
    ).toBeInTheDocument();
    expect(within(query).getByText('Raw response')).toBeInTheDocument();
    await user.click(within(query).getByRole('button', { name: 'Copy as curl' }));
    const curl = within(query).getByLabelText('Equivalent curl command') as HTMLTextAreaElement;
    expect(curl.value).toContain("'https://console.example/ops/api/v1/operations'");
    expect(curl.value).toContain('"source":"18446744073709551615"');
    expect(writeText).toHaveBeenCalledWith(curl.value);
    const history = screen.getByRole('region', { name: 'Console history' });
    expect(within(history).getByText(/not the server's activity/)).toBeInTheDocument();
    expect(within(history).getByText('Connected')).toBeInTheDocument();
  });

  it('switches to node and edge state queries and disables them without lifecycle', async () => {
    const user = userEvent.setup();
    const api = fakeApi({
      operation: vi.fn(async () =>
        ok<OperationOutcome>({ result: 'node_state', registered: true, active: false }),
      ),
    });
    const { unmount } = render(<Harness api={api} />);
    await user.selectOptions(screen.getByRole('combobox', { name: /Query/ }), 'node_state');
    await user.type(screen.getByLabelText('Node'), '4');
    await user.click(screen.getByRole('button', { name: 'Run query' }));
    expect((await screen.findAllByText('Registered, inactive')).length).toBeGreaterThan(0);
    unmount();
    render(<Harness api={api} serverInfo={info({ topologyLifecycle: false })} />);
    expect(screen.getByRole('option', { name: /node_state \(needs lifecycle\)/ })).toBeDisabled();
  });

  it('shows query failures', async () => {
    const user = userEvent.setup();
    const api = fakeApi({
      operation: vi.fn(async () =>
        failed<OperationOutcome>({
          kind: 'server-error',
          status: 404,
          error: { code: 'unknown_node', known: true, message: 'unknown node: 3' },
        }),
      ),
    });
    render(<Harness api={api} preset={{ op: 'connected', a: '1', b: '3' }} />);
    await user.click(screen.getByRole('button', { name: 'Run query' }));
    expect(await screen.findByText('unknown node: 3')).toBeInTheDocument();
  });
});

describe('workbench batch editor', () => {
  it('validates before sending and reports problems by path', () => {
    render(<Harness api={fakeApi()} />);
    expect(screen.getByText('Valid')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send batch (changes the graph)' })).toBeEnabled();
    setEditor('{"operations":[{"op":"link","source":1,"target":"2"}]}');
    expect(screen.getByText(/numbers lose precision/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send batch' })).toBeDisabled();
    setEditor('not json');
    expect(screen.getByText(/invalid JSON/)).toBeInTheDocument();
  });

  it('blocks mutations in source-managed mode and lifecycle ops without support', () => {
    const { unmount } = render(
      <Harness api={fakeApi()} serverInfo={info({ mode: 'source_managed' })} />,
    );
    expect(screen.getByText(/belong to the source and feeder/)).toBeInTheDocument();
    unmount();
    render(<Harness api={fakeApi()} serverInfo={info({ topologyLifecycle: false })} />);
    setEditor('[{"op":"set_node_active","node":"1","active":false}]');
    expect(screen.getByText(/lifecycle support is disabled/)).toBeInTheDocument();
  });

  it('shows per-operation results of a partially failed batch without rollback claims', async () => {
    const user = userEvent.setup();
    const batch = vi.fn(async () =>
      ok<BatchEntry[]>([
        { ok: true, outcome: { result: 'changed', changed: true } },
        {
          ok: false,
          error: { code: 'self_loop', known: true, message: 'self-loops are not allowed' },
        },
        { ok: true, outcome: { result: 'connected', connected: false } },
      ]),
    );
    render(<Harness api={fakeApi({ batch })} />);
    setEditor(
      JSON.stringify({
        operations: [
          { op: 'add_node', node: '1' },
          { op: 'add_node', node: '2' },
          { op: 'connected', source: '1', target: '2' },
        ],
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Send batch (changes the graph)' }));
    expect(batch).toHaveBeenCalledTimes(1);
    expect((batch.mock.calls[0] as unknown[])[1]).toBe('mutation');
    const region = screen.getByRole('region', { name: 'Batch' });
    expect(await within(region).findByText('2 ok, 1 error')).toBeInTheDocument();
    expect(within(region).getByText(/There is no rollback/)).toBeInTheDocument();
    expect(within(region).getByText('self-loops are not allowed')).toBeInTheDocument();
    expect(within(region).getByText('Not connected')).toBeInTheDocument();
    const history = screen.getByRole('region', { name: 'Console history' });
    expect(within(history).getByText('2 ok, 1 error')).toBeInTheDocument();
  });

  it('marks a timed-out mutating batch as uncertain and never retries', async () => {
    const user = userEvent.setup();
    const batch = vi.fn(async () => failed<BatchEntry[]>({ kind: 'timeout', where: 'client' }));
    render(<Harness api={fakeApi({ batch })} />);
    await user.click(screen.getByRole('button', { name: 'Send batch (changes the graph)' }));
    expect(await screen.findByText(/a timeout is not a rollback/)).toBeInTheDocument();
    expect(batch).toHaveBeenCalledTimes(1);
    expect(
      within(screen.getByRole('region', { name: 'Console history' })).getByText(
        'Outcome uncertain',
      ),
    ).toBeInTheDocument();
  });

  it('saves, loads and deletes requests, and reuses history entries', async () => {
    const user = userEvent.setup();
    const api = fakeApi({
      operation: vi.fn(async () => ok<OperationOutcome>({ result: 'connected', connected: true })),
    });
    render(<Harness api={api} preset={{ op: 'connected', a: '1', b: '2' }} />);
    const saved = screen.getByRole('region', { name: 'Saved requests' });
    setEditor('[{"op":"connected","source":"5","target":"6"}]');
    await user.type(within(saved).getByLabelText(/Name/), 'probe');
    await user.click(within(saved).getByRole('button', { name: 'Save batch editor' }));
    expect(within(saved).getByText('probe')).toBeInTheDocument();
    expect(localStorage.getItem('knotrel-console.saved-requests.v1')).toContain('probe');
    setEditor('[]');
    await user.click(within(saved).getByRole('button', { name: 'Load' }));
    expect(editor().value).toContain('"5"');
    await user.click(within(saved).getByRole('button', { name: 'Delete' }));
    expect(within(saved).getByText('No saved requests.')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Run query' }));
    const history = screen.getByRole('region', { name: 'Console history' });
    await within(history).findByText('Connected');
    await user.selectOptions(screen.getByRole('combobox', { name: /Query/ }), 'connected');
    await user.click(within(history).getByRole('button', { name: 'Load' }));
    expect(screen.getByLabelText('Source')).toHaveValue('1');
    await user.click(within(history).getByRole('button', { name: 'Clear history' }));
    expect(within(history).getByText('No requests yet.')).toBeInTheDocument();
  });
});

describe('performance view', () => {
  it('explains disabled metrics and can check again', async () => {
    const user = userEvent.setup();
    const metrics = vi.fn(async () => failed<string>({ kind: 'http', status: 404 }));
    const store = new MetricsStore(fakeApi({ metrics }), { intervalMs: 60_000 });
    store.start();
    render(<PerformancePage store={store} />);
    expect(await screen.findByText('Metrics disabled')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Check again' }));
    expect(metrics).toHaveBeenCalledTimes(2);
    store.stop();
  });

  it('computes rates, quantiles, rejections and saturation from session scrapes', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let n = 0;
    const scrapes = [exposition(100, [10, 20, 30]), exposition(200, [20, 60, 70])];
    const metrics = vi.fn(async (): Promise<Result<string>> => ok(scrapes[Math.min(n++, 1)]!));
    let clock = 1_000_000;
    const store = new MetricsStore(fakeApi({ metrics }), {
      intervalMs: 5_000,
      now: () => (clock += 10_000),
    });
    store.start();
    render(<PerformancePage store={store} />);
    expect(await screen.findByText(/Rates need at least two scrapes/)).toBeInTheDocument();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(await screen.findByText('/v1/stats')).toBeInTheDocument();
    expect(screen.getByText('10.0/s')).toBeInTheDocument();
    expect(screen.getByText(/At capacity/)).toBeInTheDocument();
    expect(screen.getByText(/Rejected/)).toHaveTextContent('busy');
    expect(screen.getAllByText(/ms|µs/).length).toBeGreaterThan(0);
    store.stop();
  });

  it('flags counter resets and scrape errors', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const results: Result<string>[] = [
      ok(exposition(500, [5, 5, 5])),
      ok(exposition(10, [1, 1, 1])),
      failed({ kind: 'server-unreachable', delivered: 'no', status: 502 }),
    ];
    const metrics = vi.fn(async () => results.shift() ?? failed<string>({ kind: 'aborted' }));
    let clock = 0;
    const store = new MetricsStore(fakeApi({ metrics }), {
      intervalMs: 5_000,
      now: () => (clock += 5_000),
    });
    store.start();
    render(<PerformancePage store={store} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(await screen.findByText('Counter reset')).toBeInTheDocument();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(await screen.findByText(/Scrape failed/)).toBeInTheDocument();
    store.stop();
  });

  it('pairs outstanding work with the capacity of the same lane (1.0.0 servers)', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const perLane = (requests: number) =>
      exposition(requests, [1, 2, 3])
        .replace(
          /knotrel_requests_rejected_total\{reason="busy"\} \d+/,
          [
            `knotrel_requests_rejected_total{reason="busy",lane="shared"} 0`,
            `knotrel_requests_rejected_total{reason="busy",lane="feeder_data"} ${requests / 50}`,
            'knotrel_requests_rejected_total{reason="shutting_down",lane="none"} 0',
          ].join('\n'),
        )
        .replace(
          'knotrel_graph_jobs_outstanding 2\nknotrel_graph_job_capacity 2',
          [
            'knotrel_graph_jobs_outstanding{lane="import"} 1',
            'knotrel_graph_jobs_outstanding{lane="feeder_data"} 4',
            'knotrel_graph_jobs_outstanding{lane="shared"} 3',
            'knotrel_graph_job_capacity{lane="import"} 1',
            'knotrel_graph_job_capacity{lane="feeder_data"} 4',
            'knotrel_graph_job_capacity{lane="shared"} 32',
          ].join('\n'),
        );
    const scrapes = [perLane(100), perLane(200)];
    let n = 0;
    const metrics = vi.fn(async (): Promise<Result<string>> => ok(scrapes[Math.min(n++, 1)]!));
    let clock = 1_000_000;
    const store = new MetricsStore(fakeApi({ metrics }), {
      intervalMs: 5_000,
      now: () => (clock += 10_000),
    });
    store.start();
    render(<PerformancePage store={store} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    const table = await screen.findByRole('table', { name: /by admission lane/ });
    const rows = within(table).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(3);
    const [shared, data, imports] = rows as [HTMLElement, HTMLElement, HTMLElement];
    expect(shared).toHaveTextContent(/^shared3 \/ 32$/);
    expect(data).toHaveTextContent(/^feeder_data4 \/ 4/);
    expect(within(data).getByText(/new work on this lane is refused/)).toBeInTheDocument();
    expect(imports).toHaveTextContent(/^import1 \/ 1/);
    expect(within(imports).getByText(/an import is in progress/)).toBeInTheDocument();
    const rejected = screen.getAllByText(/Rejected/).map((li) => li.textContent);
    expect(rejected).toEqual([
      'Rejected (busy, lane shared): 0.0/s (0 in window)',
      'Rejected (busy, lane feeder_data): 0.2/s (2 in window)',
      'Rejected (shutting_down): 0.0/s (0 in window)',
    ]);
    store.stop();
  });

  it('keeps one unlabelled saturation row for servers before 1.0.0 and orders lanes', () => {
    const legacy = { at: 0, samples: parseExposition(exposition(1, [1, 1, 1])).samples };
    expect(laneSaturation(legacy)).toEqual([{ lane: undefined, outstanding: 2, capacity: 2 }]);
    expect(laneSaturation({ at: 0, samples: [] })).toEqual([
      { lane: undefined, outstanding: undefined, capacity: undefined },
    ]);
    expect(
      ['zeta', 'import', 'alpha', 'shared', 'feeder_control', 'feeder_data'].sort(byLane),
    ).toEqual(['shared', 'feeder_data', 'feeder_control', 'import', 'alpha', 'zeta']);
  });

  it('picks the window boundaries', () => {
    const s = (at: number) => ({ at, samples: [] });
    expect(windowScrapes([s(0)], 60_000)).toBeUndefined();
    expect(windowScrapes([s(0), s(30_000), s(90_000)], 60_000)?.[0].at).toBe(30_000);
    expect(windowScrapes([s(0), s(30_000), s(90_000)], Infinity)?.[0].at).toBe(0);
    expect(windowScrapes([s(0), s(100_000)], 60_000)?.[0].at).toBe(0);
  });
});
