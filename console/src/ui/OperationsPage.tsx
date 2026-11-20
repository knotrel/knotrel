/**
 * Standalone operations: add/link/cut and, with lifecycle support, activation,
 * edge enabling and permanent node removal. Each mutation is sent once; there is
 * no automatic retry. Uncertain outcomes offer a read-back query.
 */
import { useRef, useState, type ReactNode } from 'react';
import type { ConsoleApi, Result } from '../api/endpoints';
import type { OperationOutcome } from '../api/decode';
import type { Operation } from '../api/wire';
import type { Capabilities } from '../domain/capabilities';
import { appendHistory, HISTORY_LIMIT, type HistoryEntry } from '../domain/history';
import {
  classifyMutation,
  describeOperation,
  planReadBack,
  type MutationVerdict,
} from '../domain/outcomes';
import {
  allValid,
  Exact,
  formatDuration,
  MetadataView,
  NodeIdField,
  StatusBadge,
  Time,
} from './components';
import { ConfirmDialog } from './ConfirmDialog';
import { QueryResult } from './QueriesPage';

/** Verdict badge with explanation. */
export function VerdictView({ verdict }: { verdict: MutationVerdict }) {
  switch (verdict.kind) {
    case 'effective':
      return (
        <p>
          <StatusBadge severity="ok" label="Effective" /> The graph changed
          {verdict.stateVersion !== undefined && (
            <>
              ; state version is now <Exact value={verdict.stateVersion} />
            </>
          )}
          .
        </p>
      );
    case 'no-op':
      return (
        <p>
          <StatusBadge severity="unknown" label="No-op" /> Accepted without change (already in the
          requested state).
        </p>
      );
    case 'rejected':
      return (
        <p>
          <StatusBadge severity="critical" label="Rejected" /> Not applied
          {verdict.code !== undefined && (
            <>
              {' '}
              (<Exact value={verdict.code} />)
            </>
          )}
          : <span className="server-text">{verdict.reason}</span>
        </p>
      );
    case 'uncertain':
      return (
        <p>
          <StatusBadge severity="warning" label="Outcome uncertain" /> {verdict.reason}. The change
          may or may not have been applied; a timeout is not a rollback. It was not retried
          automatically. Read the state back before trying again.
        </p>
      );
  }
}

interface LastMutation {
  op: Operation;
  result: Result<OperationOutcome>;
  verdict: MutationVerdict;
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="card" aria-label={title}>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

/** Operations page. */
export function OperationsPage({
  api,
  capabilities,
  instanceId,
}: {
  api: ConsoleApi;
  capabilities: Capabilities;
  instanceId: string | undefined;
}) {
  const [node, setNode] = useState('');
  const [source, setSource] = useState('');
  const [target, setTarget] = useState('');
  const [pending, setPending] = useState<Operation>();
  const [last, setLast] = useState<LastMutation>();
  const [readBack, setReadBack] = useState<{ op: Operation; result: Result<OperationOutcome> }>();
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [confirmRemove, setConfirmRemove] = useState<string>();
  const nextId = useRef(1);

  if (!capabilities.canMutate) {
    return (
      <div className="page">
        <Section title="Operations">
          <p>
            <StatusBadge
              severity={capabilities.mode === undefined ? 'progress' : 'unknown'}
              label={capabilities.mode === undefined ? 'Waiting' : 'Not available'}
            />{' '}
            {capabilities.mutationBlockedReason}
          </p>
          {capabilities.mode === 'source_managed' && (
            <p className="note">
              Diagnostic reads remain available on the Overview, Graph/Queries and Synchronization
              pages.
            </p>
          )}
        </Section>
      </div>
    );
  }

  const busy = pending !== undefined;
  const lifecycle = capabilities.lifecycle;

  const execute = async (op: Operation) => {
    if (busy || !capabilities.allowed.has(op.op)) {
      return;
    }
    setPending(op);
    setReadBack(undefined);
    const result = await api.operation(op, 'mutation');
    const verdict = classifyMutation(result);
    setLast({ op, result, verdict });
    setHistory((h) =>
      appendHistory(h, {
        id: nextId.current++,
        at: result.receivedAt,
        description: describeOperation(op),
        verdict,
        instanceId: result.ok ? result.metadata.instanceId : instanceId,
        durationMs: result.durationMs,
      }),
    );
    setPending(undefined);
  };

  const runReadBack = async (op: Operation) => {
    setReadBack({ op, result: await api.operation(op, 'query') });
  };

  const nodeValid = allValid(node);
  const edgeValid = allValid(source, target);
  const plan =
    last && last.verdict.kind === 'uncertain' ? planReadBack(last.op, lifecycle) : undefined;
  const lifecycleNote = lifecycle
    ? undefined
    : 'Unsupported: requires lifecycle support on the server (KNOTREL_TOPOLOGY_LIFECYCLE=true).';

  return (
    <div className="page">
      <p className="note">
        Mutations are sent once and never retried automatically. Results show the state version of
        their own response.
      </p>

      <Section title="Node">
        <div className="inline-form">
          <NodeIdField label="Node ID" value={node} onChange={setNode} disabled={busy} />
          <button
            type="button"
            disabled={busy || !nodeValid}
            onClick={() => void execute({ op: 'add_node', node })}
          >
            Add node
          </button>
          <button
            type="button"
            disabled={busy || !nodeValid || !lifecycle}
            onClick={() => void execute({ op: 'set_node_active', node, active: true })}
          >
            Activate
          </button>
          <button
            type="button"
            disabled={busy || !nodeValid || !lifecycle}
            onClick={() => void execute({ op: 'set_node_active', node, active: false })}
          >
            Deactivate
          </button>
          <button
            type="button"
            className="danger"
            disabled={busy || !nodeValid || !lifecycle}
            onClick={() => setConfirmRemove(node)}
          >
            Remove permanently…
          </button>
        </div>
        {lifecycleNote && <p className="note">Activate, deactivate and remove: {lifecycleNote}</p>}
      </Section>

      <Section title="Edge">
        <div className="inline-form">
          <NodeIdField label="Source" value={source} onChange={setSource} disabled={busy} />
          <NodeIdField label="Target" value={target} onChange={setTarget} disabled={busy} />
          <button
            type="button"
            disabled={busy || !edgeValid}
            onClick={() => void execute({ op: 'link', source, target })}
          >
            Link
          </button>
          <button
            type="button"
            disabled={busy || !edgeValid}
            onClick={() => void execute({ op: 'cut', source, target })}
          >
            Cut
          </button>
          <button
            type="button"
            disabled={busy || !edgeValid || !lifecycle}
            onClick={() => void execute({ op: 'set_edge_enabled', source, target, enabled: true })}
          >
            Enable
          </button>
          <button
            type="button"
            disabled={busy || !edgeValid || !lifecycle}
            onClick={() => void execute({ op: 'set_edge_enabled', source, target, enabled: false })}
          >
            Disable
          </button>
        </div>
        {lifecycleNote && <p className="note">Enable and disable: {lifecycleNote}</p>}
      </Section>

      <Section title="Last result">
        {busy && (
          <p role="status">
            <StatusBadge severity="progress" label="Sending" /> {describeOperation(pending)}
          </p>
        )}
        {!busy && !last && <p>No operation performed in this session.</p>}
        {last && (
          <div className="result" aria-live="polite">
            <h3>{describeOperation(last.op)}</h3>
            <VerdictView verdict={last.verdict} />
            {last.result.ok && <MetadataView metadata={last.result.metadata} />}
            <p className="source">
              Observed at <Time date={last.result.receivedAt} />; browser round trip{' '}
              {formatDuration(last.result.durationMs)}. Server execution time is not reported by the
              API.
            </p>
            {plan && (
              <div className="guidance">
                <p>{plan.hint}</p>
                {plan.query && (
                  <button type="button" onClick={() => void runReadBack(plan.query as Operation)}>
                    Read back: {describeOperation(plan.query)}
                  </button>
                )}
              </div>
            )}
            {readBack && <QueryResult op={readBack.op} result={readBack.result} />}
          </div>
        )}
      </Section>

      <Section title="Session history">
        <p className="note">
          The last {HISTORY_LIMIT} operations of this browser session, kept in memory only.
        </p>
        {history.length === 0 ? (
          <p>No operations yet.</p>
        ) : (
          <>
            <table>
              <caption className="visually-hidden">Operations performed in this session</caption>
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Operation</th>
                  <th scope="col">Outcome</th>
                  <th scope="col">State version</th>
                  <th scope="col">Round trip</th>
                </tr>
              </thead>
              <tbody>
                {history.map((entry) => (
                  <tr key={entry.id}>
                    <td>
                      <Time date={entry.at} />
                    </td>
                    <td>{entry.description}</td>
                    <td>{entry.verdict.kind}</td>
                    <td>
                      {(entry.verdict.kind === 'effective' || entry.verdict.kind === 'no-op') &&
                      entry.verdict.stateVersion !== undefined ? (
                        <Exact value={entry.verdict.stateVersion} />
                      ) : (
                        '—'
                      )}
                    </td>
                    <td>{formatDuration(entry.durationMs)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <button type="button" onClick={() => setHistory([])}>
              Clear history
            </button>
          </>
        )}
      </Section>

      {confirmRemove !== undefined && (
        <ConfirmDialog
          title={`Remove node ${confirmRemove} permanently?`}
          description="This removes the node and every incident edge. It cannot be undone through deactivation; the node would have to be added and linked again."
          confirmText={confirmRemove}
          actionLabel="Remove node"
          onCancel={() => setConfirmRemove(undefined)}
          onConfirm={() => {
            const id = confirmRemove;
            setConfirmRemove(undefined);
            void execute({ op: 'remove_node', node: id });
          }}
        />
      )}
    </div>
  );
}
