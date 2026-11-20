import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import type { ConsoleApi, Result } from '../../src/api/endpoints';
import type {
  EdgePage,
  IncidentPage,
  InspectedNode,
  NodePage,
  OperationOutcome,
} from '../../src/api/decode';
import { deriveCapabilities } from '../../src/domain/capabilities';
import {
  BrowserPage,
  browserParams,
  DETAIL_DEFAULT,
  DETAIL_MIN,
  type BrowserParams,
} from '../../src/ui/BrowserPage';
import { ConfirmDialog } from '../../src/ui/ConfirmDialog';
import { INCIDENT_PAGE, ineffectiveReason } from '../../src/ui/NodeDetail';
import { failed, fakeApi, info, ok, stats } from '../helpers';

const MAX = '18446744073709551615';

function Harness({
  api,
  initial = {},
  serverInfo = info(),
  onQuery = () => {},
  record = () => {},
}: {
  api: ConsoleApi;
  initial?: Partial<BrowserParams>;
  serverInfo?: ReturnType<typeof info>;
  onQuery?: (preset: unknown) => void;
  record?: () => void;
}) {
  const [params, setParams] = useState<BrowserParams>({
    ...browserParams(new URLSearchParams()),
    ...initial,
  });
  return (
    <BrowserPage
      api={api}
      capabilities={deriveCapabilities(serverInfo)}
      stats={stats()}
      params={params}
      navigate={(patch) => setParams((p) => ({ ...p, ...patch }))}
      record={record}
      onQuery={onQuery}
    />
  );
}

describe('data browser lists', () => {
  it('lists nodes with exact u64 IDs and graph counts', async () => {
    const api = fakeApi();
    render(<Harness api={api} />);
    expect(await screen.findByText(MAX)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Nodes 2' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: 'Edges 1' })).toBeInTheDocument();
    expect(screen.getByText('2 of 2 loaded')).toBeInTheDocument();
    expect(screen.getByText('End of list')).toBeInTheDocument();
    expect(within(screen.getByRole('table')).getByText('Inactive')).toBeInTheDocument();
    expect(screen.getByRole('region', { name: 'No selection' })).toBeInTheDocument();
    expect(api.listNodes).toHaveBeenCalledWith({ after: undefined, limit: 100, state: 'all' });
  });

  it('pages with the cursor and flags a graph change between pages', async () => {
    const user = userEvent.setup();
    const pages: Result<NodePage>[] = [
      ok(
        { nodes: [{ node: '1', active: true, degree: '0' }], nextAfter: '1' },
        { generation: '1' },
      ),
      ok(
        { nodes: [{ node: '2', active: true, degree: '0' }], nextAfter: null },
        { generation: '2' },
      ),
      ok({ nodes: [{ node: '1', active: true, degree: '0' }], nextAfter: null }),
    ];
    const listNodes = vi.fn(async () => pages.shift()!);
    render(<Harness api={fakeApi({ listNodes })} />);
    await user.click(await screen.findByRole('button', { name: 'Load 100 more' }));
    expect(await screen.findByRole('button', { name: '2' })).toBeInTheDocument();
    expect(listNodes).toHaveBeenLastCalledWith({ after: '1', limit: 100, state: 'all' });
    expect(screen.getByText('Graph changed while browsing')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'About paging' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Restart listing' }));
    await waitFor(() => expect(screen.queryByText('Graph changed while browsing')).toBeNull());
    expect(listNodes).toHaveBeenLastCalledWith({ after: undefined, limit: 100, state: 'all' });
  });

  it('follows empty scan-budget pages automatically', async () => {
    const pages: Result<NodePage>[] = [
      ok({ nodes: [], nextAfter: '5000' }),
      ok({ nodes: [{ node: '9001', active: false, degree: '0' }], nextAfter: null }),
    ];
    const listNodes = vi.fn(async () => pages.shift()!);
    render(<Harness api={fakeApi({ listNodes })} initial={{ state: 'inactive' }} />);
    expect(await screen.findByText('9001')).toBeInTheDocument();
    expect(listNodes).toHaveBeenCalledTimes(2);
    expect(listNodes).toHaveBeenLastCalledWith({ after: '5000', limit: 100, state: 'inactive' });
    expect(screen.getByText('1 loaded')).toBeInTheDocument();
  });

  it('filters by state and explains empty results', async () => {
    const user = userEvent.setup();
    const listNodes = vi.fn(async () => ok<NodePage>({ nodes: [], nextAfter: null }));
    render(<Harness api={fakeApi({ listNodes })} />);
    expect(await screen.findByText('The graph has no nodes.')).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText('State'), 'inactive');
    expect(await screen.findByText('No nodes match the filter “Inactive”.')).toBeInTheDocument();
    expect(listNodes).toHaveBeenLastCalledWith({ after: undefined, limit: 100, state: 'inactive' });
  });

  it('lists edges and reports failures without dropping rows', async () => {
    const user = userEvent.setup();
    const pages: Result<EdgePage>[] = [
      ok({
        edges: [
          { source: '1', target: MAX, enabled: false, effective: false },
          { source: '2', target: '3', enabled: true, effective: false },
          { source: '3', target: '4', enabled: true, effective: true },
        ],
        nextAfterSource: '3',
        nextAfterTarget: '4',
      }),
      failed({ kind: 'timeout', where: 'client' }),
    ];
    const listEdges = vi.fn(async () => pages.shift()!);
    render(<Harness api={fakeApi({ listEdges })} initial={{ view: 'edges' }} />);
    const table = await screen.findByRole('table');
    expect(within(table).getByText('Disabled')).toBeInTheDocument();
    expect(within(table).getByText('Not effective')).toBeInTheDocument();
    expect(within(table).getByText('Effective')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load 100 more' }));
    expect(await screen.findByText('No response in time')).toBeInTheDocument();
    expect(listEdges).toHaveBeenLastCalledWith({
      afterSource: '3',
      afterTarget: '4',
      limit: 100,
      state: 'all',
    });
    expect(within(screen.getByRole('table')).getByText('Disabled')).toBeInTheDocument();
    await user.click(within(table).getByText('Not effective'));
    expect(await screen.findByRole('region', { name: 'Edge 2 – 3' })).toBeInTheDocument();
  });

  it('explains when inspection is disabled or edges are unsupported', async () => {
    const { unmount } = render(
      <Harness api={fakeApi()} serverInfo={info({ inspection: false })} />,
    );
    expect(screen.getByText('Node list unavailable')).toBeInTheDocument();
    expect(screen.getByText(/KNOTREL_INSPECTION=true/)).toBeInTheDocument();
    unmount();
    render(
      <Harness
        api={fakeApi()}
        initial={{ view: 'edges' }}
        serverInfo={info({ engine: 'ett', topologyLifecycle: false })}
      />,
    );
    expect(screen.getByText('Edge list unavailable')).toBeInTheDocument();
    expect(screen.getByText(/keeps no per-node adjacency/)).toBeInTheDocument();
    expect(screen.queryByLabelText('State')).toBeNull();
  });

  it('moves between rows with the arrow keys and opens a node', async () => {
    const user = userEvent.setup();
    render(<Harness api={fakeApi()} />);
    const first = await screen.findByRole('button', { name: '1' });
    first.focus();
    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('button', { name: MAX })).toHaveFocus();
    await user.keyboard('{ArrowUp}');
    expect(first).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('region', { name: 'Node 1' })).toBeInTheDocument();
    await user.click(within(screen.getByRole('table')).getByText('Inactive'));
    expect(await screen.findByRole('region', { name: `Node ${MAX}` })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Close detail' }));
    expect(screen.getByRole('region', { name: 'No selection' })).toBeInTheDocument();
  });

  it('goes to a node by exact ID and creates nodes in a dialog', async () => {
    const user = userEvent.setup();
    const record = vi.fn();
    const api = fakeApi();
    render(<Harness api={api} record={record} />);
    await user.type(screen.getByLabelText('Go to node ID'), '9007199254740993{Enter}');
    expect(
      await screen.findByRole('region', { name: 'Node 9007199254740993' }),
    ).toBeInTheDocument();
    expect(api.getNode).toHaveBeenCalledWith('9007199254740993');
    await user.click(screen.getByRole('button', { name: 'New node' }));
    const dialog = screen.getByRole('dialog', { name: 'New node' });
    expect(within(dialog).getByRole('button', { name: 'Create node' })).toBeDisabled();
    await user.type(within(dialog).getByLabelText('Node ID'), '42');
    await user.click(within(dialog).getByRole('button', { name: 'Create node' }));
    expect(await screen.findByRole('region', { name: 'Node 42' })).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(api.operation).toHaveBeenCalledWith({ op: 'add_node', node: '42' }, 'mutation');
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ summary: 'Effective', mutating: true }),
    );
    await user.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(screen.getByText(/may be out of date/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Refresh list' }));
    expect(screen.queryByText(/may be out of date/)).toBeNull();
  });

  it('keeps the dialog open when creation is rejected, and cancels', async () => {
    const user = userEvent.setup();
    const operation = vi.fn(async () =>
      failed<OperationOutcome>({
        kind: 'server-error',
        status: 409,
        error: { code: 'node_limit_exceeded', known: true, message: 'full' },
      }),
    );
    render(<Harness api={fakeApi({ operation })} />);
    await user.click(screen.getByRole('button', { name: 'New node' }));
    const dialog = screen.getByRole('dialog', { name: 'New node' });
    await user.type(within(dialog).getByLabelText('Node ID'), '7{Enter}');
    expect(await within(dialog).findByText('Rejected')).toBeInTheDocument();
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('resizes the detail pane with the keyboard and remembers the width', async () => {
    const user = userEvent.setup();
    window.localStorage.removeItem('knotrel-console.detail-width');
    render(<Harness api={fakeApi()} initial={{ node: '1' }} />);
    const splitter = await screen.findByRole('separator', { name: 'Resize detail pane' });
    expect(splitter).toHaveAttribute('aria-valuenow', String(DETAIL_DEFAULT));
    splitter.focus();
    await user.keyboard('{ArrowLeft}');
    expect(splitter).toHaveAttribute('aria-valuenow', String(DETAIL_DEFAULT + 16));
    await user.keyboard('{ArrowRight}{ArrowRight}');
    expect(splitter).toHaveAttribute('aria-valuenow', String(DETAIL_DEFAULT - 16));
    await user.keyboard('{End}');
    expect(splitter).toHaveAttribute('aria-valuenow', String(DETAIL_MIN));
    await user.keyboard('{Home}');
    expect(Number(splitter.getAttribute('aria-valuenow'))).toBeGreaterThanOrEqual(DETAIL_MIN);
    await user.keyboard('a');
    expect(window.localStorage.getItem('knotrel-console.detail-width')).not.toBeNull();
  });

  it('resizes the detail pane by dragging', async () => {
    window.localStorage.removeItem('knotrel-console.detail-width');
    render(<Harness api={fakeApi()} initial={{ node: '1' }} />);
    const splitter = await screen.findByRole('separator', { name: 'Resize detail pane' });
    const parent = splitter.parentElement!;
    vi.spyOn(parent, 'getBoundingClientRect').mockReturnValue({
      right: 1200,
      left: 0,
      top: 0,
      bottom: 600,
      width: 1200,
      height: 600,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent.pointerDown(splitter, { pointerId: 1, clientX: 640 });
    fireEvent.pointerMove(splitter, { pointerId: 1, clientX: 700 });
    expect(splitter).toHaveAttribute('aria-valuenow', '500');
    fireEvent.pointerUp(splitter, { pointerId: 1 });
    fireEvent.pointerMove(splitter, { pointerId: 1, clientX: 100 });
    expect(splitter).toHaveAttribute('aria-valuenow', '500');
  });
});

describe('browser edge cases', () => {
  it('reports a failed node page and switches between nodes and edges', async () => {
    const user = userEvent.setup();
    const listNodes = vi.fn(async () => failed<NodePage>({ kind: 'http', status: 503 }));
    render(<Harness api={fakeApi({ listNodes })} />);
    expect(await screen.findByText('HTTP 503')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /^Edges/ }));
    const table = await screen.findByRole('table');
    await user.click(within(table).getByRole('button', { name: `1 – ${MAX}` }));
    expect(await screen.findByRole('region', { name: `Edge 1 – ${MAX}` })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /^Nodes/ }));
    expect(listNodes).toHaveBeenCalledTimes(2);
  });

  it('works without storage and bounds the pane by the measured width', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    let report: ((entries: { contentRect: { width: number } }[]) => void) | undefined;
    const disconnect = vi.fn();
    vi.stubGlobal(
      'ResizeObserver',
      class {
        constructor(callback: typeof report) {
          report = callback;
        }
        observe() {}
        disconnect = disconnect;
      },
    );
    const user = userEvent.setup();
    const { unmount } = render(<Harness api={fakeApi()} initial={{ node: '1' }} />);
    const splitter = await screen.findByRole('separator', { name: 'Resize detail pane' });
    expect(splitter).toHaveAttribute('aria-valuenow', String(DETAIL_DEFAULT));
    act(() => report?.([{ contentRect: { width: 900 } }]));
    expect(splitter).toHaveAttribute('aria-valuemax', '540');
    splitter.focus();
    await user.keyboard('{Home}');
    expect(splitter).toHaveAttribute('aria-valuenow', '540');
    act(() => report?.([{ contentRect: { width: 0 } }]));
    expect(splitter).toHaveAttribute('aria-valuemax', '540');
    unmount();
    expect(disconnect).toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it('opens a neighbor from the node pane', async () => {
    const user = userEvent.setup();
    render(<Harness api={fakeApi()} initial={{ node: '1' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 1' });
    const list = await within(pane).findByRole('list', { name: 'Neighbors of node 1' });
    await user.click(within(list).getByRole('button', { name: MAX }));
    expect(await screen.findByRole('region', { name: `Node ${MAX}` })).toBeInTheDocument();
  });
});

describe('node detail', () => {
  it('shows identity, neighbors with reasons, and navigates', async () => {
    const user = userEvent.setup();
    const onQuery = vi.fn();
    render(<Harness api={fakeApi()} initial={{ node: '1' }} onQuery={onQuery} />);
    const pane = await screen.findByRole('region', { name: 'Node 1' });
    expect(await within(pane).findByText('Not effective: neighbor inactive')).toBeInTheDocument();
    expect(within(pane).getByText('degree 1')).toBeInTheDocument();
    const tip = within(pane).getByRole('button', { name: 'About node and edge states' });
    await user.click(tip);
    expect(tip).toHaveAttribute('aria-expanded', 'true');
    expect(within(pane).getByRole('tooltip')).toHaveTextContent(/only effective edges/);
    await user.keyboard('{Escape}');
    expect(tip).toHaveAttribute('aria-expanded', 'false');
    await user.click(within(pane).getByRole('button', { name: 'More node actions' }));
    await user.click(screen.getByRole('menuitem', { name: 'Query node state in Workbench' }));
    expect(onQuery).toHaveBeenLastCalledWith({ op: 'node_state', a: '1', b: '' });
    await user.click(within(pane).getByRole('button', { name: `Actions for edge 1 – ${MAX}` }));
    await user.click(screen.getByRole('menuitem', { name: 'Open edge' }));
    expect(await screen.findByRole('region', { name: `Edge 1 – ${MAX}` })).toBeInTheDocument();
  });

  it('checks connectivity in the pane and records the query', async () => {
    const user = userEvent.setup();
    const record = vi.fn();
    const onQuery = vi.fn();
    const answers: Result<OperationOutcome>[] = [
      ok({ result: 'connected', connected: true }),
      ok({ result: 'connected', connected: false }),
      failed({ kind: 'http', status: 500 }),
      ok({ result: 'changed', changed: false }),
    ];
    const operation = vi.fn(async () => answers.shift()!);
    render(
      <Harness
        api={fakeApi({ operation })}
        initial={{ node: '1' }}
        record={record}
        onQuery={onQuery}
      />,
    );
    const pane = await screen.findByRole('region', { name: 'Node 1' });
    const field = await within(pane).findByLabelText('Target node');
    const check = within(pane).getByRole('button', { name: 'Check' });
    await user.type(field, '1');
    expect(check).toBeDisabled();
    await user.clear(field);
    await user.type(field, MAX);
    await user.click(check);
    expect(operation).toHaveBeenLastCalledWith(
      { op: 'connected', source: '1', target: MAX },
      'query',
    );
    expect(await within(pane).findByText(/^Connected to/)).toBeInTheDocument();
    expect(record).toHaveBeenCalledWith(expect.objectContaining({ mutating: false }));
    await user.click(check);
    expect(await within(pane).findByText(/^Not connected to/)).toBeInTheDocument();
    await user.click(check);
    expect(await within(pane).findByText('HTTP 500')).toBeInTheDocument();
    expect(record).toHaveBeenLastCalledWith(expect.objectContaining({ summary: 'Failed' }));
    await user.click(check);
    await waitFor(() => expect(within(pane).queryByText('HTTP 500')).toBeNull());
    await user.click(within(pane).getByRole('button', { name: 'Open in Workbench' }));
    expect(onQuery).toHaveBeenCalledWith({ op: 'connected', a: '1', b: MAX });
  });

  it('applies operations once, re-reads the node and reports removal', async () => {
    const user = userEvent.setup();
    let removed = false;
    const getNode = vi.fn(async (node: string) =>
      removed
        ? failed<InspectedNode>({
            kind: 'server-error',
            status: 404,
            error: { code: 'unknown_node', known: true, message: 'unknown node' },
          })
        : ok({ node, active: true, degree: '1' }),
    );
    const operation = vi.fn(async (op: { op: string }) => {
      if (op.op === 'remove_node') removed = true;
      return ok<OperationOutcome>({ result: 'changed', changed: true }, { stateVersion: '9' });
    });
    render(<Harness api={fakeApi({ getNode, operation })} initial={{ node: '5' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 5' });
    await user.click(await within(pane).findByRole('button', { name: 'Deactivate' }));
    expect(operation).toHaveBeenCalledWith(
      { op: 'set_node_active', node: '5', active: false },
      'mutation',
    );
    expect(await within(pane).findByText('Deactivate node 5')).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Dismiss' }));
    expect(within(pane).queryByText('Deactivate node 5')).toBeNull();
    await user.click(within(pane).getByRole('button', { name: 'Disable' }));
    expect(operation).toHaveBeenLastCalledWith(
      { op: 'set_edge_enabled', source: '5', target: '1', enabled: false },
      'mutation',
    );
    await user.click(await within(pane).findByRole('button', { name: 'Actions for edge 5 – 1' }));
    await user.click(screen.getByRole('menuitem', { name: 'Cut edge' }));
    expect(operation).toHaveBeenLastCalledWith({ op: 'cut', source: '5', target: '1' }, 'mutation');
    const linkToggle = within(pane).getByRole('button', { name: 'Link to node…' });
    await user.click(linkToggle);
    expect(linkToggle).toHaveAttribute('aria-expanded', 'true');
    await user.type(within(pane).getByLabelText('Link to node'), '6');
    await user.click(within(pane).getByRole('button', { name: 'Link' }));
    expect(operation).toHaveBeenLastCalledWith(
      { op: 'link', source: '5', target: '6' },
      'mutation',
    );
    await user.click(within(pane).getByRole('button', { name: 'More node actions' }));
    await user.click(screen.getByRole('menuitem', { name: 'Remove permanently…' }));
    const dialog = screen.getByRole('alertdialog');
    expect(within(dialog).getByRole('button', { name: 'Remove node' })).toBeDisabled();
    await user.type(within(dialog).getByRole('textbox'), '5');
    await user.click(within(dialog).getByRole('button', { name: 'Remove node' }));
    expect(await within(pane).findByText('Not registered')).toBeInTheDocument();
    expect(within(pane).getByText(/does not exist \(any more\)/)).toBeInTheDocument();
    expect(getNode.mock.calls.length).toBeGreaterThanOrEqual(5);
  });

  it('cancels a removal with Escape and reports uncertain outcomes', async () => {
    const user = userEvent.setup();
    const operation = vi.fn(async () =>
      failed<OperationOutcome>({ kind: 'timeout', where: 'client' }),
    );
    render(<Harness api={fakeApi({ operation })} initial={{ node: '5' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 5' });
    await user.click(await within(pane).findByRole('button', { name: 'More node actions' }));
    await user.click(screen.getByRole('menuitem', { name: 'Remove permanently…' }));
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(operation).not.toHaveBeenCalled();
    await user.click(within(pane).getByRole('button', { name: 'Deactivate' }));
    expect(await within(pane).findByText('Outcome uncertain')).toBeInTheDocument();
    expect(operation).toHaveBeenCalledTimes(1);
  });

  it('hides operations in source-managed mode and without lifecycle support', async () => {
    const { unmount } = render(
      <Harness
        api={fakeApi()}
        initial={{ node: '1' }}
        serverInfo={info({ mode: 'source_managed' })}
      />,
    );
    const pane = await screen.findByRole('region', { name: 'Node 1' });
    expect(await within(pane).findByText(/authoritative source/)).toBeInTheDocument();
    expect(
      within(pane).queryByRole('button', { name: /Deactivate|Link to node|^Disable$/ }),
    ).toBeNull();
    unmount();
    render(
      <Harness
        api={fakeApi()}
        initial={{ node: '1' }}
        serverInfo={info({ topologyLifecycle: false })}
      />,
    );
    const plain = await screen.findByRole('region', { name: 'Node 1' });
    expect(await within(plain).findByRole('button', { name: 'Link to node…' })).toBeInTheDocument();
    expect(within(plain).getByRole('tooltip')).toHaveTextContent(
      /Without lifecycle support every node is active/,
    );
    expect(within(plain).queryByRole('button', { name: 'Deactivate' })).toBeNull();
    expect(within(plain).queryByRole('button', { name: 'More node actions' })).toBeNull();
  });

  it('pages and filters neighbors and shows listing errors', async () => {
    const user = userEvent.setup();
    const listNodeEdges = vi
      .fn<(node: string, query: unknown) => Promise<Result<IncidentPage>>>()
      .mockResolvedValueOnce(
        ok({
          node: { node: '7', active: false, degree: '3' },
          edges: [{ neighbor: '1', enabled: false, effective: false }],
          nextAfter: '1',
        }),
      )
      .mockResolvedValueOnce(
        ok({
          node: { node: '7', active: false, degree: '3' },
          edges: [{ neighbor: '2', enabled: true, effective: false }],
          nextAfter: null,
        }),
      )
      .mockResolvedValueOnce(
        ok({
          node: { node: '7', active: false, degree: '3' },
          edges: [{ neighbor: '1', enabled: false, effective: false }],
          nextAfter: '1',
        }),
      )
      .mockResolvedValue(failed({ kind: 'http', status: 500 }));
    const getNode = vi.fn(async (node: string) => ok({ node, active: false, degree: '3' }));
    render(<Harness api={fakeApi({ listNodeEdges, getNode })} initial={{ node: '7' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 7' });
    const list = await within(pane).findByRole('list', { name: 'Neighbors of node 7' });
    expect(within(list).getByText('Disabled')).toBeInTheDocument();
    expect(within(pane).getByText('1–1 of 3')).toBeInTheDocument();
    expect(within(pane).getByRole('button', { name: 'Previous' })).toBeDisabled();
    await user.click(within(pane).getByRole('button', { name: 'Next' }));
    expect(await within(pane).findByText('Not effective: this node inactive')).toBeInTheDocument();
    const second = INCIDENT_PAGE + 1;
    expect(within(pane).getByText(`${second}–${second} of 3`)).toBeInTheDocument();
    expect(listNodeEdges).toHaveBeenLastCalledWith('7', {
      after: '1',
      limit: INCIDENT_PAGE,
      state: 'all',
    });
    await user.click(within(pane).getByRole('button', { name: 'Previous' }));
    expect(await within(pane).findByText('1–1 of 3')).toBeInTheDocument();
    expect(listNodeEdges).toHaveBeenLastCalledWith('7', {
      after: undefined,
      limit: INCIDENT_PAGE,
      state: 'all',
    });
    await user.selectOptions(within(pane).getByLabelText('Edge state'), 'disabled');
    expect(await within(pane).findByText('HTTP 500')).toBeInTheDocument();
    expect(listNodeEdges).toHaveBeenLastCalledWith('7', {
      after: undefined,
      limit: INCIDENT_PAGE,
      state: 'disabled',
    });
  });

  it('reports an unknown node and other read failures', async () => {
    const getNode = vi.fn().mockResolvedValueOnce(
      failed({
        kind: 'server-error',
        status: 404,
        error: { code: 'unknown_node', known: true, message: 'x' },
      }),
    );
    const { unmount } = render(<Harness api={fakeApi({ getNode })} initial={{ node: '3' }} />);
    expect(await screen.findByText('Not registered')).toBeInTheDocument();
    expect(screen.getByText(/does not exist\./)).toBeInTheDocument();
    unmount();
    const broken = vi.fn(async () => failed<InspectedNode>({ kind: 'http', status: 502 }));
    render(<Harness api={fakeApi({ getNode: broken })} initial={{ node: '3' }} />);
    expect(await screen.findByText('HTTP 502')).toBeInTheDocument();
  });

  it('reports engines without degrees and copies the node ID', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const getNode = vi.fn(async (node: string) => ok({ node, active: true, degree: null }));
    render(
      <Harness
        api={fakeApi({ getNode })}
        initial={{ node: '4' }}
        serverInfo={info({ engine: 'ett', topologyLifecycle: false })}
      />,
    );
    const pane = await screen.findByRole('region', { name: 'Node 4' });
    expect(await within(pane).findByText('degree unavailable')).toBeInTheDocument();
    expect(within(pane).getByText(/keeps no per-node adjacency/)).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Copy node ID' }));
    expect(writeText).toHaveBeenCalledWith('4');
    expect(await within(pane).findByRole('button', { name: 'Copied' })).toBeInTheDocument();
  });
});

describe('menus', () => {
  it('moves with the arrow keys and closes with Escape, Tab or an outside click', async () => {
    const user = userEvent.setup();
    render(<Harness api={fakeApi()} initial={{ node: '5' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 5' });
    const button = await within(pane).findByRole('button', { name: 'More node actions' });
    await user.click(button);
    const items = screen.getAllByRole('menuitem');
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowUp}');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(button).toHaveFocus();
    await user.click(button);
    await user.keyboard('{Tab}');
    expect(screen.queryByRole('menu')).toBeNull();
    await user.click(button);
    await user.click(document.body);
    expect(screen.queryByRole('menu')).toBeNull();
    await user.click(button);
    fireEvent.scroll(window);
    expect(screen.getByRole('menu')).toBeInTheDocument();
    vi.spyOn(button, 'getBoundingClientRect').mockReturnValue({
      top: 300,
      left: 10,
      right: 40,
      bottom: 330,
      width: 30,
      height: 30,
      x: 10,
      y: 300,
      toJSON: () => ({}),
    });
    fireEvent.scroll(window);
    expect(screen.queryByRole('menu')).toBeNull();
  });
});

describe('edge detail', () => {
  it('distinguishes the desired state from effectiveness and acts on the edge', async () => {
    const user = userEvent.setup();
    let enabled = true;
    const operation = vi.fn(async (op: { op: string; enabled?: boolean }) => {
      if (op.op === 'set_edge_enabled') enabled = op.enabled ?? enabled;
      if (op.op === 'edge_state') {
        return ok<OperationOutcome>({
          result: 'edge_state',
          registered: true,
          enabled,
          effective: false,
        });
      }
      return ok<OperationOutcome>({ result: 'changed', changed: true });
    });
    const getNode = vi.fn(async (node: string) => ok({ node, active: node === '1', degree: '1' }));
    const onQuery = vi.fn();
    render(
      <Harness
        api={fakeApi({ operation, getNode })}
        initial={{ edge: `1-${MAX}` }}
        onQuery={onQuery}
      />,
    );
    const pane = await screen.findByRole('region', { name: `Edge 1 – ${MAX}` });
    expect(await within(pane).findByText('Enabled')).toBeInTheDocument();
    expect(within(pane).getByText('Not effective')).toBeInTheDocument();
    expect(within(pane).getByText('Inactive')).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Disable' }));
    expect(await within(pane).findByText('Disabled')).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Dismiss' }));
    await user.click(within(pane).getByRole('button', { name: 'Re-read' }));
    await user.click(within(pane).getByRole('button', { name: 'Check connectivity in Workbench' }));
    expect(onQuery).toHaveBeenCalledWith({ op: 'connected', a: '1', b: MAX });
    await user.click(within(pane).getByRole('button', { name: MAX }));
    expect(await screen.findByRole('region', { name: `Node ${MAX}` })).toBeInTheDocument();
  });

  it('reports a cut edge as gone in plain mode and offers to link again', async () => {
    const user = userEvent.setup();
    const getNode = vi.fn(async (node: string) =>
      node === '9'
        ? failed<InspectedNode>({
            kind: 'server-error',
            status: 404,
            error: { code: 'unknown_node', known: true, message: '' },
          })
        : node === '8'
          ? failed<InspectedNode>({ kind: 'http', status: 500 })
          : ok({ node, active: true, degree: '1' }),
    );
    render(
      <Harness
        api={fakeApi({ getNode })}
        initial={{ edge: '1-9' }}
        serverInfo={info({ topologyLifecycle: false })}
      />,
    );
    const pane = await screen.findByRole('region', { name: 'Edge 1 – 9' });
    expect(await within(pane).findByText('Not registered')).toBeInTheDocument();
    expect(within(pane).getByRole('tooltip')).toHaveTextContent(/every existing edge is effective/);
    await user.click(within(pane).getByRole('button', { name: 'More edge actions' }));
    await user.click(screen.getByRole('menuitem', { name: 'Cut edge' }));
    expect(await within(pane).findByText(/does not exist any more/)).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Link again' }));
    await user.click(within(pane).getByRole('button', { name: '1' }));
    expect(await screen.findByRole('region', { name: 'Node 1' })).toBeInTheDocument();
  });

  it('shows an endpoint of unknown state', async () => {
    const getNode = vi.fn(async (node: string) =>
      node === '8'
        ? failed<InspectedNode>({ kind: 'http', status: 500 })
        : ok({ node, active: true, degree: '1' }),
    );
    render(<Harness api={fakeApi({ getNode })} initial={{ edge: '1-8' }} />);
    const pane = await screen.findByRole('region', { name: 'Edge 1 – 8' });
    expect(await within(pane).findByText('Unknown')).toBeInTheDocument();
  });

  it('shows a removed edge with lifecycle support', async () => {
    const operation = vi.fn(async () =>
      ok<OperationOutcome>({
        result: 'edge_state',
        registered: false,
        enabled: false,
        effective: false,
      }),
    );
    render(<Harness api={fakeApi({ operation })} initial={{ edge: '1-2' }} />);
    const pane = await screen.findByRole('region', { name: 'Edge 1 – 2' });
    expect(await within(pane).findByText('Not registered')).toBeInTheDocument();
    expect(within(pane).getByRole('button', { name: 'Link again' })).toBeInTheDocument();
  });

  it('shows read failures and no operations in source-managed mode', async () => {
    const operation = vi.fn(async () => failed<OperationOutcome>({ kind: 'http', status: 500 }));
    render(
      <Harness
        api={fakeApi({ operation })}
        initial={{ edge: '1-2' }}
        serverInfo={info({ mode: 'source_managed' })}
      />,
    );
    const pane = await screen.findByRole('region', { name: 'Edge 1 – 2' });
    expect(await within(pane).findByText('HTTP 500')).toBeInTheDocument();
    expect(within(pane).getByText(/authoritative source/)).toBeInTheDocument();
    await waitFor(() =>
      expect(within(pane).queryByRole('button', { name: 'More edge actions' })).toBeNull(),
    );
  });
});

describe('helpers', () => {
  it('explains why an edge is not effective', () => {
    expect(
      ineffectiveReason({ neighbor: '1', enabled: true, effective: true }, true),
    ).toBeUndefined();
    expect(ineffectiveReason({ neighbor: '1', enabled: false, effective: false }, true)).toBe(
      'disabled',
    );
    expect(ineffectiveReason({ neighbor: '1', enabled: true, effective: false }, true)).toBe(
      'neighbor inactive',
    );
    expect(ineffectiveReason({ neighbor: '1', enabled: true, effective: false }, false)).toBe(
      'this node inactive',
    );
  });

  it('reads browser parameters', () => {
    expect(browserParams(new URLSearchParams('view=edges&state=disabled&edge=1-2'))).toEqual({
      view: 'edges',
      state: 'disabled',
      node: undefined,
      edge: '1-2',
    });
  });
});

describe('ConfirmDialog focus handling', () => {
  it('traps focus within the dialog', async () => {
    const user = userEvent.setup();
    render(
      <ConfirmDialog
        title="t"
        description="d"
        confirmText="x"
        actionLabel="Go"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    const input = screen.getByRole('textbox');
    expect(input).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus();
    await user.tab();
    expect(input).toHaveFocus();
    await user.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus();
    await user.type(input, 'x');
    await user.tab();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Go' })).toHaveFocus();
    await user.tab();
    expect(input).toHaveFocus();
  });
});
