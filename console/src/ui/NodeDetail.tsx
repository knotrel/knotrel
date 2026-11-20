/**
 * Detail pane of one node, in reading order: identity and state, its neighbors
 * (paged, in a bounded box), and a connectivity check from this node. Common
 * actions are buttons; rare or destructive ones sit in a menu. After a mutation
 * the node is read again; a removed node is reported as such.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ConsoleApi, EdgeState, Result } from '../api/endpoints';
import type { IncidentEdge, IncidentPage, InspectedNode, OperationOutcome } from '../api/decode';
import type { Operation } from '../api/wire';
import type { QueryPreset } from './presets';
import type { Capabilities } from '../domain/capabilities';
import { describeOperation, type MutationVerdict } from '../domain/outcomes';
import { checkNodeId } from '../domain/node-id';
import { ConfirmDialog } from './ConfirmDialog';
import { Exact, FailureView, formatDuration, NodeIdField, StatusBadge, Time } from './components';
import { runMutation, type RecordHistory } from './mutation';
import { describeOutcome, VerdictView } from './results';
import { IconButton, InfoTip, Menu, StateDot } from './widgets';

/** Page size of the neighbor list. */
export const INCIDENT_PAGE = 25;

/** Why an edge is not effective, from what the pane knows. */
export function ineffectiveReason(edge: IncidentEdge, nodeActive: boolean): string | undefined {
  if (edge.effective) return undefined;
  if (!edge.enabled) return 'disabled';
  return nodeActive ? 'neighbor inactive' : 'this node inactive';
}

/** Explanation of the four states, shown in info tips. */
export const STATE_DEFINITIONS =
  'Registered: exists in the topology. Active (nodes): takes part in connectivity; inactive nodes keep their edges. Enabled (edges): the desired state. Effective: enabled and both endpoints active; only effective edges carry connectivity.';

interface Props {
  api: ConsoleApi;
  node: string;
  capabilities: Capabilities;
  onSelectNode: (node: string) => void;
  onSelectEdge: (source: string, target: string) => void;
  onQuery: (preset: QueryPreset) => void;
  onClose: () => void;
  record: RecordHistory;
  /** Called after any mutation so lists can be marked out of date. */
  onMutated: () => void;
}

/** Connectivity check from the detail pane. */
function Connectivity({
  api,
  node,
  record,
  onQuery,
}: Pick<Props, 'api' | 'node' | 'record' | 'onQuery'>) {
  const [target, setTarget] = useState('');
  const [pending, setPending] = useState(false);
  const [answer, setAnswer] = useState<{ target: string; result: Result<OperationOutcome> }>();
  const check = checkNodeId(target);
  const valid = check.ok && target !== node;

  const run = async () => {
    if (!valid || pending) return;
    setPending(true);
    const op: Operation = { op: 'connected', source: node, target };
    const result = await api.operation(op, 'query');
    setAnswer({ target, result });
    record({
      at: result.receivedAt,
      description: describeOperation(op),
      summary: result.ok ? describeOutcome(result.value) : 'Failed',
      severity: result.ok ? 'ok' : 'critical',
      mutating: false,
      request: { path: 'v1/operations', body: op },
      instanceId: result.ok ? result.metadata.instanceId : undefined,
      durationMs: result.durationMs,
    });
    setPending(false);
  };

  const outcome = answer?.result.ok ? answer.result.value : undefined;
  return (
    <section className="detail-section connectivity" aria-labelledby={`conn-${node}`}>
      <h3 id={`conn-${node}`}>Connectivity</h3>
      <form
        className="connectivity-form"
        onSubmit={(event) => {
          event.preventDefault();
          void run();
        }}
      >
        <span className="from">
          From <Exact value={node} /> to
        </span>
        <NodeIdField label="Target node" hideLabel value={target} onChange={setTarget} />
        <button type="submit" className="primary" disabled={!valid || pending}>
          {pending ? 'Checking…' : 'Check'}
        </button>
      </form>
      {answer && (
        <div className="connectivity-result" role="status">
          {outcome?.result === 'connected' ? (
            outcome.connected ? (
              <StateDot tone="ok">
                Connected to <Exact value={answer.target} />
              </StateDot>
            ) : (
              <StateDot tone="warning">
                Not connected to <Exact value={answer.target} />
              </StateDot>
            )
          ) : answer.result.ok ? (
            <span>{describeOutcome(answer.result.value)}</span>
          ) : (
            <FailureView failure={answer.result.failure} />
          )}
          <span className="note push">
            <Time date={answer.result.receivedAt} /> · {formatDuration(answer.result.durationMs)}{' '}
            round trip
          </span>
        </div>
      )}
      <button
        type="button"
        className="link small"
        onClick={() => onQuery({ op: 'connected', a: node, b: valid ? target : '' })}
      >
        Open in Workbench
      </button>
    </section>
  );
}

/** The node detail pane. */
export function NodeDetail(props: Props) {
  const { api, node, capabilities, record } = props;
  const [detail, setDetail] = useState<Result<InspectedNode>>();
  const [edges, setEdges] = useState<IncidentEdge[]>([]);
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  const [pageIndex, setPageIndex] = useState(0);
  const [next, setNext] = useState<string | null>(null);
  const [edgesError, setEdgesError] = useState<Result<IncidentPage>>();
  const [filter, setFilter] = useState<EdgeState>('all');
  const [loading, setLoading] = useState(false);
  const [verdict, setVerdict] = useState<{ text: string; verdict: MutationVerdict }>();
  const [busy, setBusy] = useState(false);
  const [linking, setLinking] = useState(false);
  const [linkTarget, setLinkTarget] = useState('');
  const [confirm, setConfirm] = useState<'remove' | undefined>();
  const [readAt, setReadAt] = useState<Date>();
  const [copied, setCopied] = useState(false);
  const generation = useRef(0);

  /** Loads one neighbor page (and the node itself unless only paging). */
  const load = useCallback(
    async (after: string | null, index: number, withNode: boolean) => {
      const id = ++generation.current;
      // Loads may start from an effect: publish state only after a microtask.
      await Promise.resolve();
      if (id !== generation.current) return;
      setLoading(true);
      const [nodeResult, page] = await Promise.all([
        withNode ? api.getNode(node) : Promise.resolve(undefined),
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
        setEdges(page.value.edges);
        setNext(page.value.nextAfter);
        setEdgesError(undefined);
        setPageIndex(index);
        setCursors((stack) => [...stack.slice(0, index), after]);
      } else if (page) {
        setEdges([]);
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
    void load(null, 0, true);
  }, [load]);
  useEffect(() => {
    const timer = setTimeout(reload, 0);
    return () => clearTimeout(timer);
  }, [reload]);

  const reread = () => void load(cursors[pageIndex] ?? null, pageIndex, true);

  const mutate = async (op: Operation, text: string) => {
    if (busy) return;
    setBusy(true);
    const result = await runMutation(api, op, record);
    setVerdict({ text, verdict: result });
    setBusy(false);
    props.onMutated();
    await load(cursors[pageIndex] ?? null, pageIndex, true);
  };

  const copy = () => {
    void navigator.clipboard?.writeText(node).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => undefined,
    );
  };

  const missing =
    detail !== undefined &&
    !detail.ok &&
    detail.failure.kind === 'server-error' &&
    detail.failure.error.code === 'unknown_node';
  const record_ = detail?.ok ? detail.value : undefined;
  const lifecycle = capabilities.lifecycle;
  const canMutate = capabilities.canMutate;
  const linkCheck = checkNodeId(linkTarget);
  const degree = record_?.degree ?? null;
  const first = pageIndex * INCIDENT_PAGE + 1;
  const last = pageIndex * INCIDENT_PAGE + edges.length;

  const menu = [
    ...(lifecycle ? [{ id: 'node-state', label: 'Query node state in Workbench' }] : []),
    ...(lifecycle && canMutate && record_
      ? [{ id: 'remove', label: 'Remove permanently…', danger: true, disabled: busy }]
      : []),
  ];

  return (
    <section className="detail" aria-label={`Node ${node}`}>
      <div className="detail-head">
        <span className="kicker">Node</span>
        <div className="detail-tools">
          <IconButton icon="copy" label={copied ? 'Copied' : 'Copy node ID'} onClick={copy} />
          <IconButton
            icon="reload"
            label={loading ? 'Reading…' : 'Re-read'}
            onClick={reread}
            disabled={loading}
          />
          {menu.length > 0 && (
            <Menu
              label="More node actions"
              items={menu}
              onSelect={(id) =>
                id === 'remove'
                  ? setConfirm('remove')
                  : props.onQuery({ op: 'node_state', a: node, b: '' })
              }
            />
          )}
          <IconButton icon="close" label="Close detail" onClick={props.onClose} />
        </div>
      </div>
      <div className="detail-identity">
        <h2>
          <Exact value={node} />
        </h2>
        {!detail ? (
          <span role="status">
            <StatusBadge severity="progress" label="Loading" />
          </span>
        ) : missing ? (
          <StatusBadge severity="unknown" label="Not registered" />
        ) : (
          record_ && (
            <div className="identity-facts">
              {record_.active ? (
                <StateDot tone="ok">Active</StateDot>
              ) : (
                <StateDot tone="warning">Inactive</StateDot>
              )}
              <span className="note">
                {degree === null ? 'degree unavailable' : `degree ${degree}`}
              </span>
              <InfoTip label="About node and edge states">
                {STATE_DEFINITIONS}
                {!lifecycle && ' Without lifecycle support every node is active.'}
                {degree === null && ' This engine does not report degrees.'}
              </InfoTip>
            </div>
          )
        )}
      </div>

      {missing && (
        <p className="detail-section">
          Node <Exact value={node} /> does not exist{verdict ? ' (any more)' : ''}.
        </p>
      )}
      {detail && !detail.ok && !missing && (
        <div className="detail-section">
          <FailureView failure={detail.failure} />
        </div>
      )}

      {record_ && canMutate && (
        <div className="detail-actions">
          <button type="button" aria-expanded={linking} onClick={() => setLinking((l) => !l)}>
            Link to node…
          </button>
          {lifecycle && (
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void mutate(
                  { op: 'set_node_active', node, active: !record_.active },
                  `${record_.active ? 'Deactivate' : 'Activate'} node ${node}`,
                )
              }
            >
              {record_.active ? 'Deactivate' : 'Activate'}
            </button>
          )}
        </div>
      )}
      {record_ && !canMutate && (
        <p className="detail-section note">{capabilities.mutationBlockedReason}</p>
      )}
      {linking && record_ && (
        <form
          className="detail-section inline-form"
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
            autoFocus
          />
          <button type="submit" disabled={busy || !linkCheck.ok || linkTarget === node}>
            Link
          </button>
        </form>
      )}
      {verdict && (
        <div className="detail-section result-line" aria-live="polite">
          <strong>{verdict.text}</strong>
          <VerdictView verdict={verdict.verdict} />
          <IconButton icon="close" label="Dismiss" onClick={() => setVerdict(undefined)} />
        </div>
      )}

      {record_ && (
        <section className="detail-section" aria-labelledby={`nb-${node}`}>
          <div className="section-head">
            <h3 id={`nb-${node}`}>Neighbors</h3>
            {degree !== null && filter === 'all' && <span className="note">{degree}</span>}
            {lifecycle && capabilities.browseEdges && (
              <label className="filter push">
                <span className="visually-hidden">Edge state</span>
                <select
                  value={filter}
                  onChange={(event) => setFilter(event.target.value as EdgeState)}
                >
                  <option value="all">All edges</option>
                  <option value="enabled">Enabled</option>
                  <option value="disabled">Disabled</option>
                  <option value="effective">Effective</option>
                  <option value="ineffective">Not effective</option>
                </select>
              </label>
            )}
          </div>
          {!capabilities.browseEdges ? (
            <p className="note">{capabilities.browseBlockedReason}</p>
          ) : (
            <>
              {edgesError && !edgesError.ok && <FailureView failure={edgesError.failure} />}
              {edges.length === 0 && !loading && !edgesError ? (
                <p className="note">
                  {filter === 'all' ? 'No neighbors.' : 'No neighbors match this filter.'}
                </p>
              ) : (
                <ul className="neighbors" aria-label={`Neighbors of node ${node}`}>
                  {edges.map((edge) => {
                    const reason = ineffectiveReason(edge, record_.active);
                    const rowMenu = [
                      { id: 'open', label: 'Open edge' },
                      ...(canMutate ? [{ id: 'cut', label: 'Cut edge', disabled: busy }] : []),
                    ];
                    return (
                      <li key={edge.neighbor} className={edge.effective ? undefined : 'flagged'}>
                        <button
                          type="button"
                          className="link neighbor"
                          onClick={() => props.onSelectNode(edge.neighbor)}
                        >
                          <Exact value={edge.neighbor} />
                        </button>
                        {edge.effective ? (
                          <StateDot tone="ok">Effective</StateDot>
                        ) : (
                          <StateDot tone="warning">
                            {reason === 'disabled' ? 'Disabled' : `Not effective: ${reason ?? ''}`}
                          </StateDot>
                        )}
                        <span className="row-tools">
                          {canMutate && lifecycle && (
                            <button
                              type="button"
                              className={edge.enabled ? 'small' : 'small accent'}
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
                          <Menu
                            label={`Actions for edge ${node} – ${edge.neighbor}`}
                            items={rowMenu}
                            onSelect={(id) =>
                              id === 'open'
                                ? props.onSelectEdge(node, edge.neighbor)
                                : void mutate(
                                    { op: 'cut', source: node, target: edge.neighbor },
                                    `Cut ${node} – ${edge.neighbor}`,
                                  )
                            }
                          />
                        </span>
                      </li>
                    );
                  })}
                </ul>
              )}
              {(pageIndex > 0 || next !== null) && (
                <div className="pager">
                  <span className="note">
                    {first}–{last}
                    {degree !== null && filter === 'all' ? ` of ${degree}` : ''}
                  </span>
                  <button
                    type="button"
                    className="small push"
                    disabled={loading || pageIndex === 0}
                    onClick={() => void load(cursors[pageIndex - 1] ?? null, pageIndex - 1, false)}
                  >
                    Previous
                  </button>
                  <button
                    type="button"
                    className="small"
                    disabled={loading || next === null}
                    onClick={() => void load(next, pageIndex + 1, false)}
                  >
                    Next
                  </button>
                </div>
              )}
            </>
          )}
        </section>
      )}

      {record_ && <Connectivity api={api} node={node} record={record} onQuery={props.onQuery} />}

      {readAt && (
        <p className="detail-foot note">
          Read <Time date={readAt} />. Not refreshed automatically.
        </p>
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
