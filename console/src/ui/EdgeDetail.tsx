/**
 * Detail pane of one edge: its desired state (`enabled`) against its
 * effectiveness, which also depends on both endpoints being active.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ConsoleApi, Result } from '../api/endpoints';
import type { InspectedNode, OperationOutcome } from '../api/decode';
import type { Capabilities } from '../domain/capabilities';
import type { MutationVerdict } from '../domain/outcomes';
import type { QueryPreset } from './presets';
import { Exact, FailureView, StatusBadge, Time } from './components';
import { runMutation, type RecordHistory } from './mutation';
import { VerdictView } from './results';

interface Props {
  api: ConsoleApi;
  source: string;
  target: string;
  capabilities: Capabilities;
  onSelectNode: (node: string) => void;
  onQuery: (preset: QueryPreset) => void;
  record: RecordHistory;
  onMutated: () => void;
}

interface EdgeFacts {
  /** `edge_state` (lifecycle) or `undefined` when unavailable. */
  state: Result<OperationOutcome> | undefined;
  sourceNode: Result<InspectedNode>;
  targetNode: Result<InspectedNode>;
  readAt: Date;
}

function endpointLabel(result: Result<InspectedNode>): {
  severity: 'ok' | 'warning' | 'unknown';
  text: string;
} {
  if (result.ok) {
    return result.value.active
      ? { severity: 'ok', text: 'active' }
      : { severity: 'warning', text: 'inactive' };
  }
  if (result.failure.kind === 'server-error' && result.failure.error.code === 'unknown_node') {
    return { severity: 'unknown', text: 'not registered' };
  }
  return { severity: 'unknown', text: 'unknown' };
}

/** The edge detail pane. */
export function EdgeDetail(props: Props) {
  const { api, source, target, capabilities, record } = props;
  const [facts, setFacts] = useState<EdgeFacts>();
  const [verdict, setVerdict] = useState<{ text: string; verdict: MutationVerdict }>();
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  const lifecycle = capabilities.lifecycle;

  const load = useCallback(async () => {
    const id = ++generation.current;
    const [state, sourceNode, targetNode] = await Promise.all([
      lifecycle
        ? api.operation({ op: 'edge_state', source, target }, 'query')
        : Promise.resolve(undefined),
      api.getNode(source),
      api.getNode(target),
    ]);
    if (id === generation.current) {
      setFacts({ state, sourceNode, targetNode, readAt: new Date() });
    }
  }, [api, source, target, lifecycle]);

  // Keyed by edge: a new selection starts with fresh state.
  useEffect(() => {
    void load();
  }, [load]);

  const mutate = async (op: Parameters<typeof runMutation>[1], text: string) => {
    if (busy) return;
    setBusy(true);
    setVerdict({ text, verdict: await runMutation(api, op, record) });
    setBusy(false);
    props.onMutated();
    await load();
  };

  const state = facts?.state;
  const edge = state?.ok && state.value.result === 'edge_state' ? state.value : undefined;
  const removed = edge !== undefined && !edge.registered;
  const cut = verdict?.verdict.kind === 'effective' && verdict.text.startsWith('Cut');

  return (
    <section className="card detail" aria-label={`Edge ${source} – ${target}`}>
      <div className="detail-head">
        <h2>
          Edge <Exact value={source} /> – <Exact value={target} />
        </h2>
        <button type="button" onClick={() => void load()}>
          Re-read
        </button>
      </div>
      {facts && (
        <p className="source">
          Read at <Time date={facts.readAt} />.
        </p>
      )}
      {verdict && (
        <div className="result" aria-live="polite">
          <h3>{verdict.text}</h3>
          <VerdictView verdict={verdict.verdict} />
        </div>
      )}
      {!facts ? (
        <p role="status">
          <StatusBadge severity="progress" label="Loading" />
        </p>
      ) : (
        <>
          {removed || (!lifecycle && cut) ? (
            <p>
              <StatusBadge severity="unknown" label="Not registered" /> This edge does not exist
              {verdict ? ' any more' : ''}.
            </p>
          ) : (
            <dl className="flags">
              <div className="field">
                <dt>Enabled (desired state)</dt>
                <dd>
                  {lifecycle ? (
                    edge ? (
                      edge.enabled ? (
                        <StatusBadge severity="ok" label="Enabled" />
                      ) : (
                        <StatusBadge severity="warning" label="Disabled" />
                      )
                    ) : state && !state.ok ? (
                      <FailureView failure={state.failure} />
                    ) : null
                  ) : (
                    <span className="note">Always enabled without lifecycle support.</span>
                  )}
                </dd>
              </div>
              <div className="field">
                <dt>Effective</dt>
                <dd>
                  {lifecycle ? (
                    edge?.effective ? (
                      <StatusBadge severity="ok" label="Effective: carries connectivity" />
                    ) : (
                      <StatusBadge severity="warning" label="Not effective" />
                    )
                  ) : (
                    <span className="note">
                      Effective whenever it exists (no lifecycle support).
                    </span>
                  )}
                </dd>
              </div>
            </dl>
          )}
          <p className="note">
            An edge is effective when it is enabled and both endpoints are active.
          </p>
          <table className="list">
            <caption className="visually-hidden">Endpoints</caption>
            <thead>
              <tr>
                <th scope="col">Endpoint</th>
                <th scope="col">State</th>
              </tr>
            </thead>
            <tbody>
              {[
                [source, facts.sourceNode],
                [target, facts.targetNode],
              ].map(([id, result]) => {
                const label = endpointLabel(result as Result<InspectedNode>);
                return (
                  <tr key={id as string}>
                    <td>
                      <button
                        type="button"
                        className="link"
                        onClick={() => props.onSelectNode(id as string)}
                      >
                        <Exact value={id as string} />
                      </button>
                    </td>
                    <td>
                      <StatusBadge severity={label.severity} label={label.text} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="actions-row">
            <button
              type="button"
              onClick={() => props.onQuery({ op: 'connected', a: source, b: target })}
            >
              Check connectivity in Workbench
            </button>
          </div>
          <h3>Operations</h3>
          {!capabilities.canMutate ? (
            <p className="note">{capabilities.mutationBlockedReason}</p>
          ) : (
            <div className="actions-row">
              {lifecycle && edge?.registered && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void mutate(
                      { op: 'set_edge_enabled', source, target, enabled: !edge.enabled },
                      `${edge.enabled ? 'Disable' : 'Enable'} edge ${source} – ${target}`,
                    )
                  }
                >
                  {edge.enabled ? 'Disable' : 'Enable'}
                </button>
              )}
              {!removed && !(cut && !lifecycle) && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void mutate({ op: 'cut', source, target }, `Cut ${source} – ${target}`)
                  }
                >
                  Cut
                </button>
              )}
              {(removed || (cut && !lifecycle)) && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void mutate({ op: 'link', source, target }, `Link ${source} – ${target}`)
                  }
                >
                  Link again
                </button>
              )}
            </div>
          )}
        </>
      )}
    </section>
  );
}
