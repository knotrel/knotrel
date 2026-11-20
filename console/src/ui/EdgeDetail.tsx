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
import { STATE_DEFINITIONS } from './NodeDetail';
import { VerdictView } from './results';
import { IconButton, InfoTip, Menu, StateDot } from './widgets';

interface Props {
  api: ConsoleApi;
  source: string;
  target: string;
  capabilities: Capabilities;
  onSelectNode: (node: string) => void;
  onQuery: (preset: QueryPreset) => void;
  onClose: () => void;
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
  tone: 'ok' | 'warning' | 'muted';
  text: string;
} {
  if (result.ok) {
    return result.value.active
      ? { tone: 'ok', text: 'Active' }
      : { tone: 'warning', text: 'Inactive' };
  }
  if (result.failure.kind === 'server-error' && result.failure.error.code === 'unknown_node') {
    return { tone: 'muted', text: 'Not registered' };
  }
  return { tone: 'muted', text: 'Unknown' };
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

  const gone = removed || (!lifecycle && cut);
  const menu =
    capabilities.canMutate && !gone
      ? [{ id: 'cut', label: 'Cut edge', danger: true, disabled: busy }]
      : [];

  return (
    <section className="detail" aria-label={`Edge ${source} – ${target}`}>
      <div className="detail-head">
        <span className="kicker">Edge</span>
        <div className="detail-tools">
          <IconButton icon="reload" label="Re-read" onClick={() => void load()} />
          {menu.length > 0 && (
            <Menu
              label="More edge actions"
              items={menu}
              onSelect={() =>
                void mutate({ op: 'cut', source, target }, `Cut ${source} – ${target}`)
              }
            />
          )}
          <IconButton icon="close" label="Close detail" onClick={props.onClose} />
        </div>
      </div>
      <div className="detail-identity">
        <h2>
          <Exact value={source} /> – <Exact value={target} />
        </h2>
        {!facts ? (
          <span role="status">
            <StatusBadge severity="progress" label="Loading" />
          </span>
        ) : gone ? (
          <StatusBadge severity="unknown" label="Not registered" />
        ) : (
          <div className="identity-facts">
            {lifecycle ? (
              edge ? (
                <>
                  {edge.enabled ? (
                    <StateDot tone="ok">Enabled</StateDot>
                  ) : (
                    <StateDot tone="warning">Disabled</StateDot>
                  )}
                  {edge.effective ? (
                    <StateDot tone="ok">Effective</StateDot>
                  ) : (
                    <StateDot tone="warning">Not effective</StateDot>
                  )}
                </>
              ) : null
            ) : (
              <StateDot tone="ok">Effective</StateDot>
            )}
            <InfoTip label="About node and edge states">
              {STATE_DEFINITIONS}
              {!lifecycle && ' Without lifecycle support every existing edge is effective.'}
            </InfoTip>
          </div>
        )}
      </div>
      {state && !state.ok && (
        <div className="detail-section">
          <FailureView failure={state.failure} />
        </div>
      )}
      {gone && (
        <p className="detail-section">This edge does not exist{verdict ? ' any more' : ''}.</p>
      )}

      {facts && capabilities.canMutate && (
        <div className="detail-actions">
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
          {gone && (
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
      {facts && !capabilities.canMutate && (
        <p className="detail-section note">{capabilities.mutationBlockedReason}</p>
      )}
      {verdict && (
        <div className="detail-section result-line" aria-live="polite">
          <strong>{verdict.text}</strong>
          <VerdictView verdict={verdict.verdict} />
          <IconButton icon="close" label="Dismiss" onClick={() => setVerdict(undefined)} />
        </div>
      )}

      {facts && (
        <section className="detail-section" aria-labelledby={`ep-${source}-${target}`}>
          <div className="section-head">
            <h3 id={`ep-${source}-${target}`}>Endpoints</h3>
          </div>
          <ul className="neighbors" aria-label="Endpoints">
            {[
              [source, facts.sourceNode],
              [target, facts.targetNode],
            ].map(([id, result]) => {
              const label = endpointLabel(result as Result<InspectedNode>);
              return (
                <li key={id as string}>
                  <button
                    type="button"
                    className="link neighbor"
                    onClick={() => props.onSelectNode(id as string)}
                  >
                    <Exact value={id as string} />
                  </button>
                  <StateDot tone={label.tone}>{label.text}</StateDot>
                </li>
              );
            })}
          </ul>
          <button
            type="button"
            className="link small"
            onClick={() => props.onQuery({ op: 'connected', a: source, b: target })}
          >
            Check connectivity in Workbench
          </button>
        </section>
      )}
      {facts && (
        <p className="detail-foot note">
          Read <Time date={facts.readAt} />.
        </p>
      )}
    </section>
  );
}
