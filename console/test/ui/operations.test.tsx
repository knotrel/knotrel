import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { ConsoleApi, Result } from '../../src/api/endpoints';
import type { OperationOutcome } from '../../src/api/decode';
import { deriveCapabilities } from '../../src/domain/capabilities';
import { HISTORY_LIMIT } from '../../src/domain/history';
import { ConfirmDialog } from '../../src/ui/ConfirmDialog';
import { OperationsPage } from '../../src/ui/OperationsPage';
import { QueriesPage } from '../../src/ui/QueriesPage';
import { failed, fakeApi, info, ok } from '../helpers';

const standalone = deriveCapabilities(info({ topologyLifecycle: true }));
const noLifecycle = deriveCapabilities(info({ topologyLifecycle: false }));
const sourceManaged = deriveCapabilities(info({ mode: 'source_managed', topologyLifecycle: true }));

function renderOps(api: ConsoleApi, capabilities = standalone) {
  return render(<OperationsPage api={api} capabilities={capabilities} instanceId="srv-1" />);
}

describe('Operations page', () => {
  it('hides mutations in source-managed mode and explains why', () => {
    renderOps(fakeApi(), sourceManaged);
    expect(screen.getByText(/authoritative source/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Add node|Link|Cut|Remove/ })).toBeNull();
  });

  it('waits for capabilities', () => {
    renderOps(fakeApi(), deriveCapabilities(undefined));
    expect(screen.getByText('Waiting')).toBeInTheDocument();
  });

  it('disables lifecycle actions without lifecycle support', async () => {
    const user = userEvent.setup();
    renderOps(fakeApi(), noLifecycle);
    await user.type(screen.getByLabelText('Node ID'), '1');
    expect(screen.getByRole('button', { name: 'Add node' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Activate' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Remove permanently…' })).toBeDisabled();
    expect(screen.getAllByText(/requires lifecycle support/).length).toBe(2);
  });

  it('validates node IDs before enabling actions', async () => {
    const user = userEvent.setup();
    renderOps(fakeApi());
    const input = screen.getByLabelText('Node ID');
    await user.type(input, '12a');
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(screen.getByRole('button', { name: 'Add node' })).toBeDisabled();
    await user.clear(input);
    await user.type(input, '007');
    expect(screen.getByText('The server normalizes leading zeroes.')).toBeInTheDocument();
  });

  it('sends exact IDs once and reports an effective change with its state version', async () => {
    const user = userEvent.setup();
    const api = fakeApi();
    renderOps(api);
    await user.type(screen.getByLabelText('Source'), '18446744073709551615');
    await user.type(screen.getByLabelText('Target'), '9007199254740993');
    await user.click(screen.getByRole('button', { name: 'Link' }));
    expect(api.operation).toHaveBeenCalledTimes(1);
    expect(api.operation).toHaveBeenCalledWith(
      { op: 'link', source: '18446744073709551615', target: '9007199254740993' },
      'mutation',
    );
    expect(await screen.findByText('Effective')).toBeInTheDocument();
    const history = screen.getByRole('table');
    expect(
      within(history).getByText('link 18446744073709551615 – 9007199254740993'),
    ).toBeInTheDocument();
  });

  it('reports no-ops and structured rejections as plain text', async () => {
    const user = userEvent.setup();
    const responses: Result<OperationOutcome>[] = [
      ok({ result: 'changed', changed: false }),
      failed({
        kind: 'server-error',
        status: 422,
        error: { code: 'self_loop', known: true, message: '<img src=x onerror=alert(1)>' },
      }),
    ];
    const api = fakeApi({ operation: vi.fn(async () => responses.shift()!) });
    const { container } = renderOps(api);
    await user.type(screen.getByLabelText('Node ID'), '1');
    await user.click(screen.getByRole('button', { name: 'Add node' }));
    expect(await screen.findByText('No-op')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Source'), '1');
    await user.type(screen.getByLabelText('Target'), '1');
    await user.click(screen.getByRole('button', { name: 'Cut' }));
    expect(await screen.findByText('Rejected')).toBeInTheDocument();
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument();
    expect(container.querySelector('img')).toBeNull();
  });

  it('marks a timeout as uncertain, never retries, and offers a read-back', async () => {
    const user = userEvent.setup();
    const operation = vi
      .fn()
      .mockResolvedValueOnce(failed({ kind: 'timeout', where: 'client' }))
      .mockResolvedValueOnce(ok({ result: 'node_state', registered: true, active: false }));
    const api = fakeApi({ operation });
    renderOps(api);
    await user.type(screen.getByLabelText('Node ID'), '5');
    await user.click(screen.getByRole('button', { name: 'Deactivate' }));
    expect(await screen.findByText('Outcome uncertain')).toBeInTheDocument();
    expect(screen.getByText(/a timeout is not a rollback/)).toBeInTheDocument();
    expect(operation).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole('button', { name: 'Read back: state of node 5' }));
    expect(operation).toHaveBeenLastCalledWith({ op: 'node_state', node: '5' }, 'query');
    expect(await screen.findByText('Registered, inactive')).toBeInTheDocument();
  });

  it('disables all actions while a mutation is in flight', async () => {
    const user = userEvent.setup();
    let resolve: (r: Result<OperationOutcome>) => void = () => {};
    const api = fakeApi({
      operation: vi.fn(() => new Promise<Result<OperationOutcome>>((r) => (resolve = r))),
    });
    renderOps(api);
    await user.type(screen.getByLabelText('Node ID'), '1');
    await user.click(screen.getByRole('button', { name: 'Add node' }));
    expect(screen.getByRole('status')).toHaveTextContent('add node 1');
    expect(screen.getByRole('button', { name: 'Add node' })).toBeDisabled();
    resolve(ok({ result: 'changed', changed: true }));
    expect(await screen.findByText('Effective')).toBeInTheDocument();
  });

  it('requires typing the exact node ID before a permanent removal', async () => {
    const user = userEvent.setup();
    const api = fakeApi();
    renderOps(api);
    await user.type(screen.getByLabelText('Node ID'), '42');
    await user.click(screen.getByRole('button', { name: 'Remove permanently…' }));
    const dialog = screen.getByRole('alertdialog');
    const confirm = within(dialog).getByRole('button', { name: 'Remove node' });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByRole('textbox'), '4');
    expect(confirm).toBeDisabled();
    await user.keyboard('{Enter}');
    expect(api.operation).not.toHaveBeenCalled();
    await user.type(within(dialog).getByRole('textbox'), '2');
    await user.click(confirm);
    expect(api.operation).toHaveBeenCalledWith({ op: 'remove_node', node: '42' }, 'mutation');
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull());
  });

  it('cancels a removal with Escape', async () => {
    const user = userEvent.setup();
    const api = fakeApi();
    renderOps(api);
    await user.type(screen.getByLabelText('Node ID'), '42');
    await user.click(screen.getByRole('button', { name: 'Remove permanently…' }));
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(api.operation).not.toHaveBeenCalled();
  });

  it('bounds the session history and can clear it', async () => {
    const user = userEvent.setup();
    renderOps(fakeApi());
    await user.type(screen.getByLabelText('Source'), '1');
    await user.type(screen.getByLabelText('Target'), '2');
    const enable = screen.getByRole('button', { name: 'Enable' });
    const disable = screen.getByRole('button', { name: 'Disable' });
    for (let i = 0; i < HISTORY_LIMIT + 3; i += 1) {
      await user.click(i % 2 ? enable : disable);
      await waitFor(() => expect(enable).toBeEnabled());
    }
    expect(within(screen.getByRole('table')).getAllByRole('row')).toHaveLength(HISTORY_LIMIT + 1);
    await user.click(screen.getByRole('button', { name: 'Clear history' }));
    expect(screen.getByText('No operations yet.')).toBeInTheDocument();
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

describe('Queries page', () => {
  it('runs a connectivity query and shows its own version metadata and timing', async () => {
    const user = userEvent.setup();
    const api = fakeApi({
      operation: vi.fn(async () =>
        ok(
          { result: 'connected' as const, connected: false },
          { stateVersion: '77', generation: '3' },
        ),
      ),
    });
    render(<QueriesPage api={api} capabilities={standalone} />);
    const card = screen.getByRole('region', { name: 'Connectivity' });
    await user.type(within(card).getByLabelText('Source'), '1');
    await user.type(within(card).getByLabelText('Target'), '2');
    await user.click(within(card).getByRole('button', { name: 'Run query' }));
    expect(await within(card).findByText('Not connected')).toBeInTheDocument();
    expect(within(card).getByText('77')).toBeInTheDocument();
    expect(within(card).getByText(/Server execution time is not reported/)).toBeInTheDocument();
    expect(api.operation).toHaveBeenCalledWith(
      { op: 'connected', source: '1', target: '2' },
      'query',
    );
  });

  it('runs node and edge state queries and shows failures', async () => {
    const user = userEvent.setup();
    const operation = vi
      .fn()
      .mockResolvedValueOnce(ok({ result: 'node_state', registered: false, active: false }))
      .mockResolvedValueOnce(
        ok({ result: 'edge_state', registered: true, enabled: false, effective: false }),
      )
      .mockResolvedValueOnce(failed({ kind: 'timeout', where: 'client' }));
    render(<QueriesPage api={fakeApi({ operation })} capabilities={standalone} />);
    const nodeCard = screen.getByRole('region', { name: 'Node state' });
    await user.type(within(nodeCard).getByLabelText('Node'), '3');
    await user.click(within(nodeCard).getByRole('button', { name: 'Run query' }));
    expect(await within(nodeCard).findByText('Not registered')).toBeInTheDocument();
    const edgeCard = screen.getByRole('region', { name: 'Edge state' });
    await user.type(within(edgeCard).getByLabelText('Source'), '1');
    await user.type(within(edgeCard).getByLabelText('Target'), '2');
    await user.click(within(edgeCard).getByRole('button', { name: 'Run query' }));
    expect(
      await within(edgeCard).findByText('Registered, disabled, not effective'),
    ).toBeInTheDocument();
    await user.click(within(edgeCard).getByRole('button', { name: 'Run query' }));
    expect(await within(edgeCard).findByText('No response in time')).toBeInTheDocument();
  });

  it('disables unsupported queries with an explicit reason', () => {
    render(<QueriesPage api={fakeApi()} capabilities={noLifecycle} />);
    expect(screen.getAllByText(/Unsupported: node and edge state queries/)).toHaveLength(2);
    const nodeCard = screen.getByRole('region', { name: 'Node state' });
    expect(within(nodeCard).getByLabelText('Node')).toBeDisabled();
  });

  it('keeps diagnostic queries available in source-managed mode', () => {
    render(<QueriesPage api={fakeApi()} capabilities={sourceManaged} />);
    const card = screen.getByRole('region', { name: 'Connectivity' });
    expect(within(card).getByLabelText('Source')).toBeEnabled();
  });

  it('waits for configuration before querying', () => {
    render(<QueriesPage api={fakeApi()} capabilities={deriveCapabilities(undefined)} />);
    expect(screen.getAllByText('Waiting for the server configuration.').length).toBe(3);
  });
});
