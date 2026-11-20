/**
 * Workbench: queries, a batch editor validated against the contract before
 * sending, readable and raw results, console history, saved requests and
 * `curl` equivalents. Mutations are sent once and never retried.
 */
import { useMemo, useState } from 'react';
import type { BatchEntry, OperationOutcome } from '../api/decode';
import type { ConsoleApi, Result } from '../api/endpoints';
import type { Operation } from '../api/wire';
import { checkBatch, formatBatch } from '../domain/batch';
import type { Capabilities } from '../domain/capabilities';
import { curlCommand } from '../domain/curl';
import type { HistoryEntry, ReplayableRequest } from '../domain/history';
import { describeOperation } from '../domain/outcomes';
import { explainServerError } from '../domain/errors';
import { loadSaved, storeSaved, upsertSaved, type SavedRequest } from '../state/saved';
import { allValid, Exact, FailureView, NodeIdField, StatusBadge, Time } from './components';
import type { RecordHistory } from './mutation';
import type { QueryPreset } from './presets';
import { describeOutcome, OutcomeView, ResponseFacts } from './results';

const TEMPLATE = formatBatch([
  { op: 'add_node', node: '1' },
  { op: 'link', source: '1', target: '2' },
  { op: 'connected', source: '1', target: '2' },
]);

interface Props {
  api: ConsoleApi;
  capabilities: Capabilities;
  maxBatch: number;
  history: readonly HistoryEntry[];
  record: RecordHistory;
  onClearHistory: () => void;
  preset: QueryPreset | undefined;
  /** Base URL of the console, for `curl` commands. */
  base: string;
}

function CopyCurl({ base, path, body }: { base: string; path: string; body?: unknown }) {
  const [shown, setShown] = useState(false);
  const [copied, setCopied] = useState(false);
  const command = curlCommand(base, path, body);
  return (
    <div className="curl">
      <button
        type="button"
        onClick={() => {
          setShown(true);
          void navigator.clipboard?.writeText(command).then(
            () => setCopied(true),
            () => setCopied(false),
          );
        }}
      >
        Copy as curl
      </button>
      {copied && <span className="note"> Copied.</span>}
      {shown && (
        <textarea
          className="code"
          readOnly
          rows={3}
          value={command}
          aria-label="Equivalent curl command"
        />
      )}
    </div>
  );
}

function RawView({ raw }: { raw: string | undefined }) {
  if (raw === undefined) return null;
  return (
    <details className="raw">
      <summary>Raw response</summary>
      <pre className="code">{raw === '' ? '(empty body)' : raw}</pre>
    </details>
  );
}

function BatchResult({ ops, result }: { ops: Operation[]; result: Result<BatchEntry[]> }) {
  if (!result.ok) {
    return (
      <div className="result">
        <FailureView failure={result.failure} />
        {(result.failure.kind === 'timeout' || result.failure.kind === 'console-unreachable') && (
          <p className="note">
            The batch may have been applied partly or fully: a timeout is not a rollback. Inspect
            the affected elements before sending it again.
          </p>
        )}
        <ResponseFacts result={result} />
        <RawView raw={result.raw} />
      </div>
    );
  }
  const errors = result.value.filter((e) => !e.ok).length;
  return (
    <div className="result">
      <p>
        <StatusBadge
          severity={errors ? 'warning' : 'ok'}
          label={`${result.value.length - errors} ok, ${errors} error${errors === 1 ? '' : 's'}`}
        />{' '}
        Operations ran in order under one lock acquisition. There is no rollback: an error does not
        undo earlier operations or stop later ones.
      </p>
      <table className="list">
        <caption className="visually-hidden">Batch results</caption>
        <thead>
          <tr>
            <th scope="col">#</th>
            <th scope="col">Operation</th>
            <th scope="col">Result</th>
          </tr>
        </thead>
        <tbody>
          {result.value.map((entry, i) => (
            <tr key={i}>
              <td>{i + 1}</td>
              <td>{ops[i] ? describeOperation(ops[i]) : '—'}</td>
              <td>
                {entry.ok ? (
                  describeOutcome(entry.outcome)
                ) : (
                  <>
                    <StatusBadge severity="critical" label={entry.error.code} />{' '}
                    <span className="server-text">{entry.error.message}</span>
                    <span className="note"> — {explainServerError(entry.error)}</span>
                  </>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <ResponseFacts result={result} />
      <RawView raw={result.raw} />
    </div>
  );
}

/** The Workbench page. */
export function WorkbenchPage(props: Props) {
  const { api, capabilities, record } = props;
  const [query, setQuery] = useState<QueryPreset>(
    props.preset ?? { op: 'connected', a: '', b: '' },
  );
  const [queryResult, setQueryResult] = useState<{
    op: Operation;
    result: Result<OperationOutcome>;
  }>();
  const [text, setText] = useState(TEMPLATE);
  const [batchResult, setBatchResult] = useState<{
    ops: Operation[];
    result: Result<BatchEntry[]>;
  }>();
  const [pending, setPending] = useState(false);
  const [saved, setSaved] = useState<SavedRequest[]>(() => loadSaved());
  const [saveName, setSaveName] = useState('');
  const [storageWarning, setStorageWarning] = useState(false);

  // A new preset from the Browser replaces the query form (adjusting state
  // while rendering, instead of an effect).
  const [seenPreset, setSeenPreset] = useState(props.preset);
  if (props.preset !== seenPreset) {
    setSeenPreset(props.preset);
    if (props.preset) setQuery(props.preset);
  }

  const check = useMemo(
    () => checkBatch(text, capabilities, props.maxBatch),
    [text, capabilities, props.maxBatch],
  );
  const lifecycle = capabilities.lifecycle;
  const queryOp: Operation | undefined =
    query.op === 'node_state'
      ? allValid(query.a)
        ? { op: 'node_state', node: query.a }
        : undefined
      : allValid(query.a, query.b) && query.a !== query.b
        ? { op: query.op, source: query.a, target: query.b }
        : undefined;

  const runQuery = async () => {
    if (!queryOp || pending) return;
    setPending(true);
    const result = await api.operation(queryOp, 'query');
    setQueryResult({ op: queryOp, result });
    record({
      at: result.receivedAt,
      description: describeOperation(queryOp),
      summary: result.ok ? describeOutcome(result.value) : 'Failed',
      severity: result.ok ? 'ok' : 'critical',
      mutating: false,
      request: { path: 'v1/operations', body: queryOp },
      instanceId: result.ok ? result.metadata.instanceId : undefined,
      durationMs: result.durationMs,
    });
    setPending(false);
  };

  const runBatch = async () => {
    if (!check.ok || pending) return;
    setPending(true);
    const ops = check.operations;
    const result = await api.batch(ops, check.mutating ? 'mutation' : 'query');
    setBatchResult({ ops, result });
    const errors = result.ok ? result.value.filter((e) => !e.ok).length : 0;
    record({
      at: result.receivedAt,
      description: `batch of ${ops.length} operation${ops.length === 1 ? '' : 's'}`,
      summary: result.ok
        ? `${result.value.length - errors} ok, ${errors} error${errors === 1 ? '' : 's'}`
        : check.mutating &&
            (result.failure.kind === 'timeout' || result.failure.kind === 'console-unreachable')
          ? 'Outcome uncertain'
          : 'Failed',
      severity: result.ok ? (errors ? 'warning' : 'ok') : 'critical',
      mutating: check.mutating,
      request: { path: 'v1/batch', body: { operations: ops } },
      instanceId: result.ok ? result.metadata.instanceId : undefined,
      durationMs: result.durationMs,
    });
    setPending(false);
  };

  const loadRequest = (request: ReplayableRequest) => {
    if (request.path === 'v1/batch') {
      setText(JSON.stringify(request.body, null, 2));
    } else {
      const op = request.body as Operation;
      if (op.op === 'connected' || op.op === 'edge_state') {
        setQuery({ op: op.op, a: op.source, b: op.target });
      } else if (op.op === 'node_state') {
        setQuery({ op: 'node_state', a: op.node, b: '' });
      } else {
        setText(formatBatch([op]));
      }
    }
  };

  const persist = (next: SavedRequest[]) => {
    setSaved(next);
    setStorageWarning(!storeSaved(next));
  };

  return (
    <div className="workbench">
      <section className="card" aria-labelledby="wb-query">
        <h2 id="wb-query">Query</h2>
        <form
          className="inline-form"
          onSubmit={(event) => {
            event.preventDefault();
            void runQuery();
          }}
        >
          <label className="filter">
            Query{' '}
            <select
              value={query.op}
              onChange={(event) =>
                setQuery({ ...query, op: event.target.value as QueryPreset['op'] })
              }
            >
              <option value="connected">connected</option>
              <option value="node_state" disabled={!lifecycle}>
                node_state{lifecycle ? '' : ' (needs lifecycle)'}
              </option>
              <option value="edge_state" disabled={!lifecycle}>
                edge_state{lifecycle ? '' : ' (needs lifecycle)'}
              </option>
            </select>
          </label>
          <NodeIdField
            label={query.op === 'node_state' ? 'Node' : 'Source'}
            value={query.a}
            onChange={(a) => setQuery({ ...query, a })}
          />
          {query.op !== 'node_state' && (
            <NodeIdField
              label="Target"
              value={query.b}
              onChange={(b) => setQuery({ ...query, b })}
            />
          )}
          <button type="submit" disabled={!queryOp || pending}>
            Run query
          </button>
        </form>
        {queryOp && <CopyCurl base={props.base} path="v1/operations" body={queryOp} />}
        {queryResult && (
          <>
            <h3>{describeOperation(queryResult.op)}</h3>
            <OutcomeView result={queryResult.result} />
            <RawView raw={queryResult.result.raw} />
          </>
        )}
      </section>

      <section className="card" aria-labelledby="wb-batch">
        <h2 id="wb-batch">Batch</h2>
        <p className="note">
          JSON for <code className="exact">POST /v1/batch</code>:{' '}
          <code className="exact">{'{"operations": [...]}'}</code> with 1 to {props.maxBatch}{' '}
          operations. Node IDs are strings. Validated before sending.
        </p>
        <label htmlFor="batch-editor" className="visually-hidden">
          Batch request JSON
        </label>
        <textarea
          id="batch-editor"
          className="code editor"
          rows={12}
          spellCheck={false}
          value={text}
          aria-invalid={!check.ok}
          aria-describedby="batch-check"
          onChange={(event) => setText(event.target.value)}
        />
        <div id="batch-check" aria-live="polite">
          {check.ok ? (
            <p>
              <StatusBadge severity="ok" label="Valid" /> {check.operations.length} operation
              {check.operations.length === 1 ? '' : 's'}
              {check.mutating && ' — changes the graph'}.
            </p>
          ) : (
            <ul className="problems">
              {check.problems.map((problem, i) => (
                <li key={i}>
                  <code className="exact">{problem.path}</code>: {problem.message}
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="actions-row">
          <button type="button" disabled={!check.ok || pending} onClick={() => void runBatch()}>
            {pending
              ? 'Sending…'
              : check.ok && check.mutating
                ? 'Send batch (changes the graph)'
                : 'Send batch'}
          </button>
          {check.ok && (
            <CopyCurl base={props.base} path="v1/batch" body={{ operations: check.operations }} />
          )}
        </div>
        {batchResult && <BatchResult ops={batchResult.ops} result={batchResult.result} />}
      </section>

      <section className="card" aria-labelledby="wb-saved">
        <h2 id="wb-saved">Saved requests</h2>
        <p className="note">
          Stored in this browser only. Requests contain operations and IDs, never credentials.
        </p>
        <form
          className="inline-form"
          onSubmit={(event) => {
            event.preventDefault();
            const name = saveName.trim();
            if (name)
              persist(upsertSaved(saved, { name, text, savedAt: new Date().toISOString() }));
          }}
        >
          <label>
            Name{' '}
            <input
              value={saveName}
              onChange={(event) => setSaveName(event.target.value)}
              maxLength={80}
            />
          </label>
          <button type="submit" disabled={saveName.trim() === ''}>
            Save batch editor
          </button>
        </form>
        {storageWarning && (
          <p className="note">
            Browser storage is unavailable; saved requests last only until the page is closed.
          </p>
        )}
        {saved.length === 0 ? (
          <p>No saved requests.</p>
        ) : (
          <ul className="saved">
            {saved.map((item) => (
              <li key={item.name}>
                <strong>{item.name}</strong>{' '}
                <button type="button" onClick={() => setText(item.text)}>
                  Load
                </button>{' '}
                <button
                  type="button"
                  onClick={() => persist(saved.filter((s) => s.name !== item.name))}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="card" aria-labelledby="wb-history">
        <h2 id="wb-history">Console history</h2>
        <p className="note">
          Requests sent from this browser tab, in memory only (last {100}). This is not the
          server&apos;s activity: other clients and the feeder do not appear here.
        </p>
        {props.history.length === 0 ? (
          <p>No requests yet.</p>
        ) : (
          <>
            <table className="list">
              <caption className="visually-hidden">Console history</caption>
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Request</th>
                  <th scope="col">Outcome</th>
                  <th scope="col">Round trip</th>
                  <th scope="col">Reuse</th>
                </tr>
              </thead>
              <tbody>
                {props.history.map((entry) => (
                  <tr key={entry.id}>
                    <td>
                      <Time date={entry.at} />
                    </td>
                    <td>
                      {entry.description}
                      {entry.mutating && <span className="note"> (mutation)</span>}
                    </td>
                    <td>
                      <StatusBadge severity={entry.severity} label={entry.summary} />
                    </td>
                    <td>{Math.round(entry.durationMs)} ms</td>
                    <td>
                      {entry.request && (
                        <button
                          type="button"
                          onClick={() => loadRequest(entry.request as ReplayableRequest)}
                        >
                          Load
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            <button type="button" onClick={props.onClearHistory}>
              Clear history
            </button>
          </>
        )}
        {props.history.some((e) => e.instanceId) && (
          <p className="source">
            Latest instance:{' '}
            <Exact value={props.history.find((e) => e.instanceId)?.instanceId ?? ''} />
          </p>
        )}
      </section>
    </div>
  );
}
