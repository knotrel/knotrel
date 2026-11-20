/**
 * Data browser: node and edge lists with cursor paging and state filters,
 * exact ID lookup, and a detail pane for the selection. Filters and the
 * selection live in the URL hash, so views can be shared and survive
 * navigation.
 */
import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from 'react';
import type { ConsoleApi, EdgeState } from '../api/endpoints';
import type { GraphStats, InspectedEdge, InspectedNode } from '../api/decode';
import type { Capabilities } from '../domain/capabilities';
import { checkNodeId } from '../domain/node-id';
import {
  appendPage,
  emptyListing,
  failPage,
  LISTING_CAP,
  LISTING_PAGE,
  loading,
  type Listing,
} from '../state/listing';
import { Exact, FailureView, NodeIdField, StatusBadge, Time } from './components';
import { EdgeDetail } from './EdgeDetail';
import { runMutation, type RecordHistory } from './mutation';
import { NodeDetail } from './NodeDetail';
import type { QueryPreset } from './presets';
import { VerdictView } from './results';
import type { MutationVerdict } from '../domain/outcomes';

/** Browser parameters kept in the URL hash. */
export interface BrowserParams {
  view: 'nodes' | 'edges';
  state: string;
  node: string | undefined;
  edge: string | undefined;
}

/** Reads browser parameters from hash parameters. */
export function browserParams(params: URLSearchParams): BrowserParams {
  const view = params.get('view') === 'edges' ? 'edges' : 'nodes';
  return {
    view,
    state: params.get('state') ?? 'all',
    node: params.get('node') ?? undefined,
    edge: params.get('edge') ?? undefined,
  };
}

type NodeCursor = string;
type EdgeCursor = { source: string; target: string | null };

const NODE_STATES = ['all', 'active', 'inactive'] as const;
const EDGE_STATES = ['all', 'enabled', 'disabled', 'effective', 'ineffective'] as const;
/** Empty scan-budget pages followed automatically per user action. */
const AUTO_CONTINUE = 5;

interface Props {
  api: ConsoleApi;
  capabilities: Capabilities;
  stats: GraphStats | undefined;
  params: BrowserParams;
  navigate: (params: Partial<BrowserParams>) => void;
  record: RecordHistory;
  onQuery: (preset: QueryPreset) => void;
}

function Counts({ stats, lifecycle }: { stats: GraphStats | undefined; lifecycle: boolean }) {
  if (!stats) return null;
  return (
    <dl className="counts" aria-label="Graph counts from GET /v1/stats">
      <div>
        <dt>Registered nodes</dt>
        <dd>
          <Exact value={stats.nodeCount} />
        </dd>
      </div>
      {lifecycle && stats.activeNodeCount !== undefined && (
        <div>
          <dt>Active nodes</dt>
          <dd>
            <Exact value={stats.activeNodeCount} />
          </dd>
        </div>
      )}
      <div>
        <dt>Registered edges</dt>
        <dd>
          <Exact value={stats.edgeCount} />
        </dd>
      </div>
      {lifecycle && stats.enabledEdgeCount !== undefined && (
        <div>
          <dt>Enabled edges</dt>
          <dd>
            <Exact value={stats.enabledEdgeCount} />
          </dd>
        </div>
      )}
      {lifecycle && stats.effectiveEdgeCount !== undefined && (
        <div>
          <dt>Effective edges</dt>
          <dd>
            <Exact value={stats.effectiveEdgeCount} />
          </dd>
        </div>
      )}
    </dl>
  );
}

/** The data browser page. */
export function BrowserPage(props: Props) {
  const { api, capabilities, params, navigate, record } = props;
  const [nodes, setNodes] = useState<Listing<InspectedNode, NodeCursor>>(emptyListing);
  const [edges, setEdges] = useState<Listing<InspectedEdge, EdgeCursor>>(emptyListing);
  const [search, setSearch] = useState('');
  const [newNode, setNewNode] = useState('');
  const [addVerdict, setAddVerdict] = useState<MutationVerdict>();
  const [outOfDate, setOutOfDate] = useState(false);
  const request = useRef(0);
  const listRef = useRef<HTMLTableSectionElement>(null);
  const lifecycle = capabilities.lifecycle;
  const view = params.view;
  const stateFilter = lifecycle ? params.state : 'all';

  const loadNodes = useCallback(
    async (listing: Listing<InspectedNode, NodeCursor>) => {
      const id = ++request.current;
      // Loaders may start from an effect: publish state only after a microtask.
      await Promise.resolve();
      if (id !== request.current) return;
      let current = loading(listing);
      setNodes(current);
      for (let empty = 0; empty <= AUTO_CONTINUE; empty += 1) {
        const result = await api.listNodes({
          after: current.next ?? undefined,
          limit: LISTING_PAGE,
          state: (NODE_STATES as readonly string[]).includes(stateFilter)
            ? (stateFilter as (typeof NODE_STATES)[number])
            : 'all',
        });
        if (id !== request.current) return;
        if (!result.ok) {
          setNodes(failPage(current, result.failure));
          return;
        }
        current = appendPage(
          current,
          result.value.nodes,
          result.value.nextAfter,
          result.metadata,
          result.receivedAt,
        );
        if (result.value.nodes.length > 0 || current.next === null) break;
      }
      setNodes(current);
    },
    [api, stateFilter],
  );

  const loadEdges = useCallback(
    async (listing: Listing<InspectedEdge, EdgeCursor>) => {
      const id = ++request.current;
      await Promise.resolve();
      if (id !== request.current) return;
      let current = loading(listing);
      setEdges(current);
      for (let empty = 0; empty <= AUTO_CONTINUE; empty += 1) {
        const cursor = current.next ?? undefined;
        const result = await api.listEdges({
          afterSource: cursor?.source,
          afterTarget: cursor?.target ?? undefined,
          limit: LISTING_PAGE,
          state: (EDGE_STATES as readonly string[]).includes(stateFilter)
            ? (stateFilter as EdgeState)
            : 'all',
        });
        if (id !== request.current) return;
        if (!result.ok) {
          setEdges(failPage(current, result.failure));
          return;
        }
        const next =
          result.value.nextAfterSource === null
            ? null
            : { source: result.value.nextAfterSource, target: result.value.nextAfterTarget };
        current = appendPage(current, result.value.edges, next, result.metadata, result.receivedAt);
        if (result.value.edges.length > 0 || current.next === null) break;
      }
      setEdges(current);
    },
    [api, stateFilter],
  );

  const startListing = useCallback(() => {
    if (view === 'nodes' && capabilities.browseNodes) {
      void loadNodes(emptyListing());
    } else if (view === 'edges' && capabilities.browseEdges) {
      void loadEdges(emptyListing());
    }
  }, [view, capabilities.browseNodes, capabilities.browseEdges, loadNodes, loadEdges]);

  const restart = () => {
    setOutOfDate(false);
    startListing();
  };

  // A new view, filter or capability set starts a new listing.
  useEffect(() => {
    startListing();
  }, [startListing]);

  const selectNode = (node: string) => navigate({ node, edge: undefined });
  const selectEdge = (source: string, target: string) =>
    navigate({ edge: `${source}-${target}`, node: undefined });
  const edgeSelection = params.edge?.split('-');

  const onListKey = (event: KeyboardEvent<HTMLTableSectionElement>) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    const buttons = Array.from(
      listRef.current?.querySelectorAll<HTMLButtonElement>('button.row-select') ?? [],
    );
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const target = buttons[event.key === 'ArrowDown' ? index + 1 : Math.max(0, index - 1)];
    if (target) {
      event.preventDefault();
      target.focus();
    }
  };

  const searchCheck = checkNodeId(search);
  const newCheck = checkNodeId(newNode);
  const listing = view === 'nodes' ? nodes : edges;
  const states = view === 'nodes' ? NODE_STATES : EDGE_STATES;
  const blocked = view === 'nodes' ? !capabilities.browseNodes : !capabilities.browseEdges;

  return (
    <div className="browser">
      <div className="browser-list card">
        <div className="toolbar" role="toolbar" aria-label="Browser controls">
          <div className="segmented" role="group" aria-label="List">
            <button
              type="button"
              aria-pressed={view === 'nodes'}
              onClick={() => navigate({ view: 'nodes', state: 'all' })}
            >
              Nodes
            </button>
            <button
              type="button"
              aria-pressed={view === 'edges'}
              onClick={() => navigate({ view: 'edges', state: 'all' })}
            >
              Edges
            </button>
          </div>
          {lifecycle ? (
            <label className="filter">
              Filter{' '}
              <select
                value={stateFilter}
                onChange={(event) => navigate({ state: event.target.value })}
              >
                {states.map((s) => (
                  <option key={s} value={s}>
                    {s === 'ineffective' ? 'not effective' : s}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <span className="note">
              No state filters: without lifecycle support every node is active and every edge
              effective.
            </span>
          )}
          <button
            type="button"
            onClick={restart}
            disabled={blocked || listing.status === 'loading'}
          >
            Restart listing
          </button>
        </div>
        <form
          className="inline-form"
          onSubmit={(event) => {
            event.preventDefault();
            if (searchCheck.ok) selectNode(search);
          }}
        >
          <NodeIdField label="Find node by exact ID" value={search} onChange={setSearch} />
          <button type="submit" disabled={!searchCheck.ok || !capabilities.browseNodes}>
            Open
          </button>
        </form>
        {capabilities.canMutate && (
          <form
            className="inline-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (!newCheck.ok) return;
              const id = newNode;
              void runMutation(api, { op: 'add_node', node: id }, record).then((verdict) => {
                setAddVerdict(verdict);
                setOutOfDate(true);
                if (verdict.kind === 'effective' || verdict.kind === 'no-op') selectNode(id);
              });
            }}
          >
            <NodeIdField label="Create node" value={newNode} onChange={setNewNode} />
            <button type="submit" disabled={!newCheck.ok}>
              Add node
            </button>
          </form>
        )}
        {addVerdict && <VerdictView verdict={addVerdict} />}
        <Counts stats={props.stats} lifecycle={lifecycle} />

        {blocked ? (
          <div className="empty" role="note">
            <StatusBadge
              severity="unknown"
              label={view === 'nodes' ? 'Node list unavailable' : 'Edge list unavailable'}
            />
            <p>{capabilities.browseBlockedReason}</p>
          </div>
        ) : (
          <>
            <p className="source" aria-live="polite">
              {listing.status === 'loading' && listing.pages === 0
                ? 'Loading…'
                : listing.loadedAt && (
                    <>
                      {listing.rows.length} {view} loaded ({listing.pages} page
                      {listing.pages === 1 ? '' : 's'}), last page at{' '}
                      <Time date={listing.loadedAt} />
                      {listing.identity?.generation !== undefined && (
                        <>
                          , generation <Exact value={listing.identity.generation} />
                        </>
                      )}
                      . Pages are read one at a time and are not a snapshot.
                    </>
                  )}
            </p>
            {listing.identityChanged && (
              <div className="banner" role="alert">
                <StatusBadge severity="warning" label="Graph changed while browsing" /> The server
                instance or generation changed between pages; this list mixes different graphs.{' '}
                <button type="button" onClick={restart}>
                  Restart listing
                </button>
              </div>
            )}
            {outOfDate && (
              <p className="note">
                The graph was changed from this console; the list may be out of date.{' '}
                <button type="button" onClick={restart}>
                  Refresh list
                </button>
              </p>
            )}
            {listing.failure && <FailureView failure={listing.failure} />}
            {listing.rows.length === 0 && listing.status === 'ready' ? (
              <div className="empty">
                <p>
                  {stateFilter === 'all'
                    ? view === 'nodes'
                      ? 'The graph has no nodes.'
                      : 'The graph has no edges.'
                    : `No ${view} match the filter “${stateFilter}”.`}
                </p>
              </div>
            ) : (
              listing.rows.length > 0 && (
                <table className="list">
                  <caption className="visually-hidden">
                    {view === 'nodes' ? 'Nodes' : 'Edges'}
                  </caption>
                  <thead>
                    {view === 'nodes' ? (
                      <tr>
                        <th scope="col">Node</th>
                        <th scope="col">State</th>
                        <th scope="col">Degree</th>
                      </tr>
                    ) : (
                      <tr>
                        <th scope="col">Edge</th>
                        <th scope="col">Enabled</th>
                        <th scope="col">Effective</th>
                      </tr>
                    )}
                  </thead>
                  <tbody ref={listRef} onKeyDown={onListKey}>
                    {view === 'nodes'
                      ? nodes.rows.map((row) => (
                          <tr
                            key={row.node}
                            aria-selected={params.node === row.node}
                            className={params.node === row.node ? 'selected' : undefined}
                          >
                            <td>
                              <button
                                type="button"
                                className="row-select link"
                                onClick={() => selectNode(row.node)}
                              >
                                <Exact value={row.node} />
                              </button>
                            </td>
                            <td>
                              {row.active ? (
                                'active'
                              ) : (
                                <StatusBadge severity="warning" label="inactive" />
                              )}
                            </td>
                            <td>{row.degree ?? '—'}</td>
                          </tr>
                        ))
                      : edges.rows.map((row) => {
                          const key = `${row.source}-${row.target}`;
                          return (
                            <tr
                              key={key}
                              aria-selected={params.edge === key}
                              className={params.edge === key ? 'selected' : undefined}
                            >
                              <td>
                                <button
                                  type="button"
                                  className="row-select link"
                                  onClick={() => selectEdge(row.source, row.target)}
                                >
                                  <Exact value={row.source} /> – <Exact value={row.target} />
                                </button>
                              </td>
                              <td>
                                {row.enabled ? (
                                  'enabled'
                                ) : (
                                  <StatusBadge severity="warning" label="disabled" />
                                )}
                              </td>
                              <td>
                                {row.effective ? (
                                  'effective'
                                ) : (
                                  <StatusBadge severity="warning" label="not effective" />
                                )}
                              </td>
                            </tr>
                          );
                        })}
                  </tbody>
                </table>
              )
            )}
            {listing.next !== null && listing.next !== undefined && (
              <button
                type="button"
                disabled={listing.status === 'loading'}
                onClick={() => (view === 'nodes' ? void loadNodes(nodes) : void loadEdges(edges))}
              >
                {listing.status === 'loading' ? 'Loading…' : 'Load more'}
              </button>
            )}
            {listing.next === null && listing.rows.length > 0 && !listing.truncated && (
              <p className="source">End of list.</p>
            )}
            {listing.truncated && (
              <p className="note">
                Showing the first {LISTING_CAP} rows. Use a filter or the exact ID search.
              </p>
            )}
          </>
        )}
      </div>

      <div className="browser-detail">
        {params.node !== undefined ? (
          <NodeDetail
            key={params.node}
            api={api}
            node={params.node}
            capabilities={capabilities}
            onSelectNode={selectNode}
            onSelectEdge={selectEdge}
            onQuery={props.onQuery}
            record={record}
            onMutated={() => setOutOfDate(true)}
          />
        ) : edgeSelection?.length === 2 ? (
          <EdgeDetail
            key={params.edge}
            api={api}
            source={edgeSelection[0] as string}
            target={edgeSelection[1] as string}
            capabilities={capabilities}
            onSelectNode={selectNode}
            onQuery={props.onQuery}
            record={record}
            onMutated={() => setOutOfDate(true)}
          />
        ) : (
          <section className="card detail empty">
            <h2>No selection</h2>
            <p>Select a node or edge in the list, or open a node by its exact ID.</p>
          </section>
        )}
      </div>
    </div>
  );
}
