/**
 * Detail pane of one node: flags, incident edges (paged), navigation to
 * neighbors and edges, and the operations the server allows on it. After a
 * mutation the node is read again; a removed node is reported as such.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ConsoleApi, EdgeState, Result } from '../api/endpoints';
import type { IncidentEdge, IncidentPage, InspectedNode } from '../api/decode';
import type { Operation } from '../api/wire';
import type { QueryPreset } from './presets';
import type { Capabilities } from '../domain/capabilities';
import type { MutationVerdict } from '../domain/outcomes';
import { checkNodeId } from '../domain/node-id';
import { ConfirmDialog } from './ConfirmDialog';
import { Exact, FailureView, NodeIdField, StatusBadge, Time } from './components';
import { runMutation, type RecordHistory } from './mutation';
import { VerdictView } from './results';

/** Page size of the incident list. */
export const INCIDENT_PAGE = 50;
/** Most incident rows kept in the pane. */
export const INCIDENT_CAP = 2000;

/** Why an edge is not effective, from what the pane knows. */
export function ineffectiveReason(edge: IncidentEdge, nodeActive: boolean): string | undefined {
  if (edge.effective) return undefined;
  if (!edge.enabled) return 'disabled';
  return nodeActive ? 'neighbor inactive' : 'this node inactive';
}

interface Props {
  api: ConsoleApi;
  node: string;
  capabilities: Capabilities;
  onSelectNode: (node: string) => void;
  onSelectEdge: (source: string, target: string) => void;
  onQuery: (preset: QueryPreset) => void;
  record: RecordHistory;
  /** Called after any mutation so lists can be marked out of date. */
  onMutated: () => void;
}

/** The node detail pane. */
export function NodeDetail(props: Props) {
  const { api, node, capabilities, record } = props;
  const [detail, setDetail] = useState<Result<InspectedNode>>();
  const [edges, setEdges] = useState<IncidentEdge[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [edgesError, setEdgesError] = useState<Result<IncidentPage>>();
  const [filter, setFilter] = useState<EdgeState>('all');
  const [loading, setLoading] = useState(false);
  const [verdict, setVerdict] = useState<{ text: string; verdict: MutationVerdict }>();
  const [busy, setBusy] = useState(false);
  const [linkTarget, setLinkTarget] = useState('');
  const [confirm, setConfirm] = useState<'remove' | undefined>();
  const [readAt, setReadAt] = useState<Date>();
  const generation = useRef(0);

  const load = useCallback(
    async (after: string | null, append: boolean) => {
      const id = ++generation.current;
      // Loads may start from an effect: publish state only after a microtask.
      await Promise.resolve();
      if (id !== generation.current) return;
      setLoading(true);
      const [nodeResult, page] = await Promise.all([
        append ? Promise.resolve(undefined) : api.getNode(node),
        capabilities.browseEdges
          ? api.listNodeEdges(node, {
              after: after ?? undefined,
              limit: INCIDENT_PAGE,
              state: filter,
            })
          : Promise.resolve(undefined),
      ]);
      if (id !== generation.current) return; // superseded
      if (nodeResult) {
        setDetail(nodeResult);
        setReadAt(nodeResult.receivedAt);
      }
      if (page?.ok) {
        setEdges((rows) =>
          (append ? [...rows, ...page.value.edges] : page.value.edges).slice(0, INCIDENT_CAP),
        );
        setNext(page.value.nextAfter);
        setEdgesError(undefined);
      } else if (page) {
        if (!append) setEdges([]);
        setNext(null);
        setEdgesError(page);
      }
      setLoading(false);
    },
    [api, node, filter, capabilities.browseEdges],
  );

  // The pane is keyed by node, so a new node starts with fresh state; this
  // effect only (re)loads when the node or the edge filter changes.
  const reload = useCallback(() => {
    void load(null, false);
  }, [load]);
  useEffect(() => {
    const timer = setTimeout(reload, 0);
    return () => clearTimeout(timer);
  }, [reload]);

  const mutate = async (op: Operation, text: string) => {
    if (busy) return;
    setBusy(true);
    const result = await runMutation(api, op, record);
    setVerdict({ text, verdict: result });
    setBusy(false);
    props.onMutated();
    await load(null, false);
  };

  if (!detail) {
    return (
      <section className="card detail" aria-label={`Node ${node}`}>
        <h2>
          Node <Exact value={node} />
        </h2>
        <p role="status">
          <StatusBadge severity="progress" label="Loading" />
        </p>
      </section>
    );
  }

  const missing =
    !detail.ok &&
    detail.failure.kind === 'server-error' &&
    detail.failure.error.code === 'unknown_node';
  const record_ = detail.ok ? detail.value : undefined;
  const lifecycle = capabilities.lifecycle;
  const canMutate = capabilities.canMutate;
  const linkCheck = checkNodeId(linkTarget);

  return (
    <section className="card detail" aria-label={`Node ${node}`}>
      <div className="detail-head">
        <h2>
          Node <Exact value={node} />
        </h2>
        <button type="button" onClick={() => void load(null, false)} disabled={loading}>
          {loading ? 'Reading…' : 'Re-read'}
        </button>
      </div>
      {readAt && (
        <p className="source">
          Read at <Time date={readAt} />. Details are not refreshed automatically.
        </p>
      )}
      {verdict && (
        <div className="result" aria-live="polite">
          <h3>{verdict.text}</h3>
          <VerdictView verdict={verdict.verdict} />
          <p className="source">The node was read again after the operation.</p>
        </div>
      )}
      {missing ? (
        <p>
          <StatusBadge severity="unknown" label="Not registered" /> Node <Exact value={node} /> does
          not exist{verdict ? ' (any more)' : ''}.
        </p>
      ) : !detail.ok ? (
        <FailureView failure={detail.failure} />
      ) : (
        record_ && (
          <>
            <dl className="flags">
              <div className="field">
                <dt>Registered</dt>
                <dd>
                  <StatusBadge severity="ok" label="Yes" />
                </dd>
              </div>
              <div className="field">
                <dt>Active</dt>
                <dd>
                  {record_.active ? (
                    <StatusBadge severity="ok" label="Active" />
                  ) : (
                    <StatusBadge severity="warning" label="Inactive" />
                  )}
                  {!lifecycle && (
                    <span className="note"> (always active without lifecycle support)</span>
                  )}
                </dd>
              </div>
              <div className="field">
                <dt>Registered degree</dt>
                <dd>
                  {record_.degree === null ? (
                    <span className="unavailable">Unavailable — not reported by this engine</span>
                  ) : (
                    <Exact value={record_.degree} />
                  )}
                </dd>
              </div>
            </dl>
            <details className="definitions">
              <summary>What do registered, active, enabled and effective mean?</summary>
              <ul>
                <li>
                  <strong>Registered:</strong> the node or edge exists in the topology.
                </li>
                <li>
                  <strong>Active</strong> (nodes): the node takes part in connectivity. Inactive
                  nodes stay registered with their edges.
                </li>
                <li>
                  <strong>Enabled</strong> (edges): the edge&apos;s own desired state.
                </li>
                <li>
                  <strong>Effective</strong> (edges): enabled <em>and</em> both endpoints active.
                  Only effective edges carry connectivity.
                </li>
              </ul>
            </details>

            <div className="actions-row">
              <button
                type="button"
                onClick={() => props.onQuery({ op: 'connected', a: node, b: '' })}
              >
                Check connectivity from here…
              </button>
              {lifecycle && (
                <button
                  type="button"
                  onClick={() => props.onQuery({ op: 'node_state', a: node, b: '' })}
                >
                  Query node state
                </button>
              )}
            </div>

            <h3>Operations</h3>
            {!canMutate ? (
              <p className="note">{capabilities.mutationBlockedReason}</p>
            ) : (
              <>
                <div className="actions-row">
                  {lifecycle ? (
                    record_.active ? (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          void mutate(
                            { op: 'set_node_active', node, active: false },
                            `Deactivate node ${node}`,
                          )
                        }
                      >
                        Deactivate
                      </button>
                    ) : (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          void mutate(
                            { op: 'set_node_active', node, active: true },
                            `Activate node ${node}`,
                          )
                        }
                      >
                        Activate
                      </button>
                    )
                  ) : null}
                  {lifecycle && (
                    <button
                      type="button"
                      className="danger"
                      disabled={busy}
                      onClick={() => setConfirm('remove')}
                    >
                      Remove permanently…
                    </button>
                  )}
                </div>
                {!lifecycle && (
                  <p className="note">
                    Activation and removal need lifecycle support (KNOTREL_TOPOLOGY_LIFECYCLE=true).
                  </p>
                )}
                <form
                  className="inline-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (linkCheck.ok && linkTarget !== node) {
                      void mutate(
                        { op: 'link', source: node, target: linkTarget },
                        `Link ${node} – ${linkTarget}`,
                      );
                    }
                  }}
                >
                  <NodeIdField
                    label="Link to node"
                    value={linkTarget}
                    onChange={setLinkTarget}
                    disabled={busy}
                  />
                  <button type="submit" disabled={busy || !linkCheck.ok || linkTarget === node}>
                    Link
                  </button>
                </form>
              </>
            )}

            <h3>Incident edges</h3>
            {!capabilities.browseEdges ? (
              <p className="note">{capabilities.browseBlockedReason}</p>
            ) : (
              <>
                {lifecycle && (
                  <label className="filter">
                    Show{' '}
                    <select
                      value={filter}
                      onChange={(event) => setFilter(event.target.value as EdgeState)}
                    >
                      <option value="all">all</option>
                      <option value="enabled">enabled</option>
                      <option value="disabled">disabled</option>
                      <option value="effective">effective</option>
                      <option value="ineffective">not effective</option>
                    </select>
                  </label>
                )}
                {edgesError && !edgesError.ok && <FailureView failure={edgesError.failure} />}
                {edges.length === 0 && !loading && !edgesError ? (
                  <p>
                    {filter === 'all'
                      ? 'No incident edges.'
                      : 'No incident edges match this filter.'}
                  </p>
                ) : (
                  <table className="list">
                    <caption className="visually-hidden">Incident edges of node {node}</caption>
                    <thead>
                      <tr>
                        <th scope="col">Neighbor</th>
                        <th scope="col">Enabled</th>
                        <th scope="col">Effective</th>
                        <th scope="col">Actions</th>
                      </tr>
                    </thead>
                    <tbody>
                      {edges.map((edge) => {
                        const reason = ineffectiveReason(edge, record_.active);
                        return (
                          <tr key={edge.neighbor}>
                            <td>
                              <button
                                type="button"
                                className="link"
                                onClick={() => props.onSelectNode(edge.neighbor)}
                              >
                                <Exact value={edge.neighbor} />
                              </button>
                            </td>
                            <td>{edge.enabled ? 'enabled' : 'disabled'}</td>
                            <td>
                              {edge.effective ? (
                                <StatusBadge severity="ok" label="effective" />
                              ) : (
                                <StatusBadge
                                  severity="warning"
                                  label={`not effective: ${reason ?? ''}`}
                                />
                              )}
                            </td>
                            <td className="row-actions">
                              <button
                                type="button"
                                onClick={() => props.onSelectEdge(node, edge.neighbor)}
                              >
                                Open edge
                              </button>
                              {canMutate && lifecycle && (
                                <button
                                  type="button"
                                  disabled={busy}
                                  onClick={() =>
                                    void mutate(
                                      {
                                        op: 'set_edge_enabled',
                                        source: node,
                                        target: edge.neighbor,
                                        enabled: !edge.enabled,
                                      },
                                      `${edge.enabled ? 'Disable' : 'Enable'} edge ${node} – ${edge.neighbor}`,
                                    )
                                  }
                                >
                                  {edge.enabled ? 'Disable' : 'Enable'}
                                </button>
                              )}
                              {canMutate && (
                                <button
                                  type="button"
                                  disabled={busy}
                                  onClick={() =>
                                    void mutate(
                                      { op: 'cut', source: node, target: edge.neighbor },
                                      `Cut ${node} – ${edge.neighbor}`,
                                    )
                                  }
                                >
                                  Cut
                                </button>
                              )}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}
                {next !== null && edges.length < INCIDENT_CAP && (
                  <button type="button" disabled={loading} onClick={() => void load(next, true)}>
                    {loading ? 'Loading…' : 'Load more edges'}
                  </button>
                )}
                {edges.length >= INCIDENT_CAP && (
                  <p className="note">
                    Showing the first {INCIDENT_CAP} edges. Use a filter to narrow the list.
                  </p>
                )}
              </>
            )}
          </>
        )
      )}
      {confirm === 'remove' && (
        <ConfirmDialog
          title={`Remove node ${node} permanently?`}
          description="This removes the node and every incident edge. Deactivation is reversible; removal is not. Re-adding the ID creates an isolated node."
          confirmText={node}
          actionLabel="Remove node"
          onCancel={() => setConfirm(undefined)}
          onConfirm={() => {
            setConfirm(undefined);
            void mutate({ op: 'remove_node', node }, `Remove node ${node}`);
          }}
        />
      )}
    </section>
  );
}
