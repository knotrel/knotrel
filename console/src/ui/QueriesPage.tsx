/**
 * Graph queries: connectivity between two nodes and, with lifecycle support,
 * node and edge state. Each result shows the version metadata of its own response.
 */
import { useRef, useState, type FormEvent } from 'react';
import type { ConsoleApi, Result } from '../api/endpoints';
import type { OperationOutcome } from '../api/decode';
import type { Operation } from '../api/wire';
import type { Capabilities } from '../domain/capabilities';
import { describeOperation } from '../domain/outcomes';
import {
  allValid,
  FailureView,
  formatDuration,
  MetadataView,
  NodeIdField,
  StatusBadge,
  Time,
} from './components';

/** Plain-language answer of a query outcome. */
export function describeOutcome(outcome: OperationOutcome): string {
  switch (outcome.result) {
    case 'connected':
      return outcome.connected ? 'Connected' : 'Not connected';
    case 'node_state':
      return outcome.registered
        ? `Registered, ${outcome.active ? 'active' : 'inactive'}`
        : 'Not registered';
    case 'edge_state':
      return outcome.registered
        ? `Registered, ${outcome.enabled ? 'enabled' : 'disabled'}, ${outcome.effective ? 'effective' : 'not effective'}`
        : 'Not registered';
    case 'changed':
      return outcome.changed ? 'Changed' : 'No change';
  }
}

/** Result card for one query response. */
export function QueryResult({ op, result }: { op: Operation; result: Result<OperationOutcome> }) {
  return (
    <div className="result" aria-live="polite">
      <h3>{describeOperation(op)}</h3>
      {result.ok ? (
        <>
          <p className="answer">
            <StatusBadge
              severity={
                result.value.result === 'connected' && !result.value.connected ? 'warning' : 'ok'
              }
              label={describeOutcome(result.value)}
            />
          </p>
          <MetadataView metadata={result.metadata} />
        </>
      ) : (
        <FailureView failure={result.failure} />
      )}
      <p className="source">
        Received at <Time date={result.receivedAt} />; browser round trip{' '}
        {formatDuration(result.durationMs)}. Server execution time is not reported by the API.
      </p>
    </div>
  );
}

function useQuery(api: ConsoleApi) {
  const [pending, setPending] = useState(false);
  const [last, setLast] = useState<{ op: Operation; result: Result<OperationOutcome> }>();
  const sequence = useRef(0);
  const run = async (op: Operation) => {
    const id = ++sequence.current;
    setPending(true);
    const result = await api.operation(op, 'query');
    if (id === sequence.current) {
      setLast({ op, result });
      setPending(false);
    }
  };
  return { pending, last, run };
}

function TwoNodeQuery({
  api,
  title,
  build,
  disabledReason,
}: {
  api: ConsoleApi;
  title: string;
  build: (source: string, target: string) => Operation;
  disabledReason: string | undefined;
}) {
  const [source, setSource] = useState('');
  const [target, setTarget] = useState('');
  const query = useQuery(api);
  const disabled = disabledReason !== undefined;
  const valid = allValid(source, target);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (valid && !disabled) {
      void query.run(build(source, target));
    }
  };
  return (
    <section className="card" aria-label={title}>
      <h2>{title}</h2>
      {disabled && <p className="note">{disabledReason}</p>}
      <form onSubmit={submit} className="inline-form">
        <NodeIdField label="Source" value={source} onChange={setSource} disabled={disabled} />
        <NodeIdField label="Target" value={target} onChange={setTarget} disabled={disabled} />
        <button type="submit" disabled={disabled || !valid || query.pending}>
          {query.pending ? 'Querying…' : 'Run query'}
        </button>
      </form>
      {query.last && <QueryResult op={query.last.op} result={query.last.result} />}
    </section>
  );
}

function NodeStateQuery({
  api,
  disabledReason,
}: {
  api: ConsoleApi;
  disabledReason: string | undefined;
}) {
  const [node, setNode] = useState('');
  const query = useQuery(api);
  const disabled = disabledReason !== undefined;
  const valid = allValid(node);
  return (
    <section className="card" aria-label="Node state">
      <h2>Node state</h2>
      {disabled && <p className="note">{disabledReason}</p>}
      <form
        className="inline-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (valid && !disabled) {
            void query.run({ op: 'node_state', node });
          }
        }}
      >
        <NodeIdField label="Node" value={node} onChange={setNode} disabled={disabled} />
        <button type="submit" disabled={disabled || !valid || query.pending}>
          {query.pending ? 'Querying…' : 'Run query'}
        </button>
      </form>
      {query.last && <QueryResult op={query.last.op} result={query.last.result} />}
    </section>
  );
}

/** Queries page. */
export function QueriesPage({
  api,
  capabilities,
}: {
  api: ConsoleApi;
  capabilities: Capabilities;
}) {
  const waiting =
    capabilities.mode === undefined ? 'Waiting for the server configuration.' : undefined;
  const lifecycleReason =
    waiting ??
    (capabilities.lifecycle
      ? undefined
      : 'Unsupported: node and edge state queries require lifecycle support on the server.');
  return (
    <div className="page">
      <p className="note">
        Queries never change the graph. Each result is computed on the state identified by its own
        version metadata.
      </p>
      <TwoNodeQuery
        api={api}
        title="Connectivity"
        build={(source, target) => ({ op: 'connected', source, target })}
        disabledReason={waiting}
      />
      <NodeStateQuery api={api} disabledReason={lifecycleReason} />
      <TwoNodeQuery
        api={api}
        title="Edge state"
        build={(source, target) => ({ op: 'edge_state', source, target })}
        disabledReason={lifecycleReason}
      />
    </div>
  );
}
