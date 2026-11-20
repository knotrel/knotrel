import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import type { ConsoleApi, Result } from '../../src/api/endpoints';
import type { EdgePage, InspectedNode, NodePage, OperationOutcome } from '../../src/api/decode';
import { deriveCapabilities } from '../../src/domain/capabilities';
import { BrowserPage, browserParams, type BrowserParams } from '../../src/ui/BrowserPage';
import { ConfirmDialog } from '../../src/ui/ConfirmDialog';
import { ineffectiveReason } from '../../src/ui/NodeDetail';
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
    expect(screen.getByText('Registered nodes')).toBeInTheDocument();
    expect(screen.getByText('Effective edges')).toBeInTheDocument();
    expect(screen.getByText(/2 nodes loaded \(1 page\)/)).toBeInTheDocument();
    expect(screen.getByText('End of list.')).toBeInTheDocument();
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
    ];
    const listNodes = vi.fn(async () => pages.shift()!);
    render(<Harness api={fakeApi({ listNodes })} />);
    await user.click(await screen.findByRole('button', { name: 'Load more' }));
    expect(await screen.findByRole('button', { name: '2' })).toBeInTheDocument();
    expect(listNodes).toHaveBeenLastCalledWith({ after: '1', limit: 100, state: 'all' });
    expect(screen.getByText('Graph changed while browsing')).toBeInTheDocument();
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
  });

  it('filters by state and explains empty results', async () => {
    const user = userEvent.setup();
    const listNodes = vi.fn(async () => ok<NodePage>({ nodes: [], nextAfter: null }));
    render(<Harness api={fakeApi({ listNodes })} />);
    expect(await screen.findByText('The graph has no nodes.')).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText(/Filter/), 'inactive');
    expect(await screen.findByText('No nodes match the filter “inactive”.')).toBeInTheDocument();
    expect(listNodes).toHaveBeenLastCalledWith({ after: undefined, limit: 100, state: 'inactive' });
  });

  it('lists edges and reports failures without dropping rows', async () => {
    const user = userEvent.setup();
    const pages: Result<EdgePage>[] = [
      ok({
        edges: [{ source: '1', target: MAX, enabled: false, effective: false }],
        nextAfterSource: '1',
        nextAfterTarget: MAX,
      }),
      failed({ kind: 'timeout', where: 'client' }),
    ];
    const listEdges = vi.fn(async () => pages.shift()!);
    render(<Harness api={fakeApi({ listEdges })} initial={{ view: 'edges' }} />);
    const table = await screen.findByRole('table');
    expect(within(table).getByText('disabled')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load more' }));
    expect(await screen.findByText('No response in time')).toBeInTheDocument();
    expect(listEdges).toHaveBeenLastCalledWith({
      afterSource: '1',
      afterTarget: MAX,
      limit: 100,
      state: 'all',
    });
    expect(within(screen.getByRole('table')).getByText('disabled')).toBeInTheDocument();
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
    expect(screen.getByText(/No state filters/)).toBeInTheDocument();
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
  });

  it('finds a node by exact ID and creates nodes', async () => {
    const user = userEvent.setup();
    const record = vi.fn();
    const api = fakeApi();
    render(<Harness api={api} record={record} />);
    await user.type(screen.getByLabelText('Find node by exact ID'), '9007199254740993');
    await user.click(screen.getByRole('button', { name: 'Open' }));
    expect(
      await screen.findByRole('region', { name: 'Node 9007199254740993' }),
    ).toBeInTheDocument();
    expect(api.getNode).toHaveBeenCalledWith('9007199254740993');
    await user.type(screen.getByLabelText('Create node'), '42');
    await user.click(screen.getByRole('button', { name: 'Add node' }));
    expect(await screen.findByRole('region', { name: 'Node 42' })).toBeInTheDocument();
    expect(api.operation).toHaveBeenCalledWith({ op: 'add_node', node: '42' }, 'mutation');
    expect(record).toHaveBeenCalledWith(
      expect.objectContaining({ summary: 'Effective', mutating: true }),
    );
    expect(screen.getByText(/may be out of date/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Refresh list' }));
    expect(screen.queryByText(/may be out of date/)).toBeNull();
  });
});

describe('node detail', () => {
  it('shows flags, incident edges with reasons, and navigates', async () => {
    const user = userEvent.setup();
    const onQuery = vi.fn();
    render(<Harness api={fakeApi()} initial={{ node: '1' }} onQuery={onQuery} />);
    const pane = await screen.findByRole('region', { name: 'Node 1' });
    expect(await within(pane).findByText('not effective: neighbor inactive')).toBeInTheDocument();
    expect(within(pane).getByText(/What do registered/)).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Check connectivity from here…' }));
    expect(onQuery).toHaveBeenCalledWith({ op: 'connected', a: '1', b: '' });
    await user.click(within(pane).getByRole('button', { name: 'Query node state' }));
    expect(onQuery).toHaveBeenLastCalledWith({ op: 'node_state', a: '1', b: '' });
    await user.click(within(pane).getByRole('button', { name: MAX }));
    expect(await screen.findByRole('region', { name: `Node ${MAX}` })).toBeInTheDocument();
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
    expect(within(pane).getAllByText('Effective').length).toBeGreaterThan(0);
    await user.click(within(pane).getByRole('button', { name: 'Disable' }));
    expect(operation).toHaveBeenLastCalledWith(
      { op: 'set_edge_enabled', source: '5', target: '1', enabled: false },
      'mutation',
    );
    await user.click(await within(pane).findByRole('button', { name: 'Cut' }));
    expect(operation).toHaveBeenLastCalledWith({ op: 'cut', source: '5', target: '1' }, 'mutation');
    await user.type(within(pane).getByLabelText('Link to node'), '6');
    await user.click(within(pane).getByRole('button', { name: 'Link' }));
    expect(operation).toHaveBeenLastCalledWith(
      { op: 'link', source: '5', target: '6' },
      'mutation',
    );
    await user.click(within(pane).getByRole('button', { name: 'Remove permanently…' }));
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
    await user.click(await within(pane).findByRole('button', { name: 'Remove permanently…' }));
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
    expect(within(pane).queryByRole('button', { name: /Deactivate|Remove|Link$|Cut/ })).toBeNull();
    unmount();
    render(
      <Harness
        api={fakeApi()}
        initial={{ node: '1' }}
        serverInfo={info({ topologyLifecycle: false })}
      />,
    );
    const plain = await screen.findByRole('region', { name: 'Node 1' });
    expect(
      await within(plain).findByText(/always active without lifecycle support/),
    ).toBeInTheDocument();
    expect(
      within(plain).getByText(/Activation and removal need lifecycle support/),
    ).toBeInTheDocument();
    expect(within(plain).queryByRole('button', { name: 'Deactivate' })).toBeNull();
  });

  it('pages and filters incident edges and shows listing errors', async () => {
    const user = userEvent.setup();
    const listNodeEdges = vi
      .fn()
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
      .mockResolvedValue(failed({ kind: 'http', status: 500 }));
    const getNode = vi.fn(async (node: string) => ok({ node, active: false, degree: '3' }));
    render(<Harness api={fakeApi({ listNodeEdges, getNode })} initial={{ node: '7' }} />);
    const pane = await screen.findByRole('region', { name: 'Node 7' });
    expect(await within(pane).findByText('not effective: disabled')).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Load more edges' }));
    expect(await within(pane).findByText('not effective: this node inactive')).toBeInTheDocument();
    await user.selectOptions(within(pane).getByLabelText(/Show/), 'disabled');
    expect(await within(pane).findByText('HTTP 500')).toBeInTheDocument();
    expect(listNodeEdges).toHaveBeenLastCalledWith('7', {
      after: undefined,
      limit: 50,
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
    render(<Harness api={fakeApi({ getNode })} initial={{ node: '3' }} />);
    expect(await screen.findByText('Not registered')).toBeInTheDocument();
    expect(screen.getByText(/does not exist\./)).toBeInTheDocument();
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
    expect(within(pane).getByText('inactive')).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Disable' }));
    expect(await within(pane).findByText('Disabled')).toBeInTheDocument();
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
    expect(await within(pane).findByText('not registered')).toBeInTheDocument();
    expect(within(pane).getByText(/Always enabled without lifecycle support/)).toBeInTheDocument();
    await user.click(within(pane).getByRole('button', { name: 'Cut' }));
    expect(await within(pane).findByText(/does not exist any more/)).toBeInTheDocument();
    expect(within(pane).getByRole('button', { name: 'Link again' })).toBeInTheDocument();
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
    await waitFor(() => expect(within(pane).queryByRole('button', { name: 'Cut' })).toBeNull());
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
