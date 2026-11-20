/**
 * Data browser: a compact toolbar, a node or edge table that fills the
 * available height, and a resizable detail pane for the selection. Filters and
 * the selection live in the URL hash, so views can be shared and survive
 * navigation.
 */
import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';
import type { ConsoleApi, EdgeState } from '../api/endpoints';
import type { GraphStats, InspectedEdge, InspectedNode } from '../api/decode';
import type { Capabilities } from '../domain/capabilities';
import { checkNodeId } from '../domain/node-id';
import type { MutationVerdict } from '../domain/outcomes';
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
import { Dialog } from './Dialog';
import { EdgeDetail } from './EdgeDetail';
import { runMutation, type RecordHistory } from './mutation';
import { NodeDetail } from './NodeDetail';
import type { QueryPreset } from './presets';
import { VerdictView } from './results';
import { Icon, IconButton, InfoTip, Splitter, StateDot } from './widgets';

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
const STATE_LABELS: Record<string, string> = {
  all: 'All',
  active: 'Active',
  inactive: 'Inactive',
  enabled: 'Enabled',
  disabled: 'Disabled',
  effective: 'Effective',
  ineffective: 'Not effective',
};
/** Empty scan-budget pages followed automatically per user action. */
const AUTO_CONTINUE = 5;

/** Narrowest detail pane, in CSS pixels. */
export const DETAIL_MIN = 360;
/** Narrowest list pane, in CSS pixels. */
export const LIST_MIN = 360;
/** Initial detail width: the detail gets more room than a three-column table needs. */
export const DETAIL_DEFAULT = 560;
const WIDTH_KEY = 'knotrel-console.detail-width';

function loadWidth(): number {
  try {
    const stored = Number(window.localStorage.getItem(WIDTH_KEY));
    return Number.isFinite(stored) && stored >= DETAIL_MIN ? stored : DETAIL_DEFAULT;
  } catch {
    return DETAIL_DEFAULT;
  }
}

function storeWidth(width: number) {
  try {
    window.localStorage.setItem(WIDTH_KEY, String(Math.round(width)));
  } catch {
    // A per-browser convenience only.
  }
}

interface Props {
  api: ConsoleApi;
  capabilities: Capabilities;
  stats: GraphStats | undefined;
  params: BrowserParams;
  navigate: (params: Partial<BrowserParams>) => void;
  record: RecordHistory;
  onQuery: (preset: QueryPreset) => void;
}

/** Dialog that creates one node. */
function NewNodeDialog({
  api,
  record,
  onClose,
  onCreated,
}: {
  api: ConsoleApi;
  record: RecordHistory;
  onClose: () => void;
  onCreated: (node: string, verdict: MutationVerdict) => void;
}) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [verdict, setVerdict] = useState<MutationVerdict>();
  const check = checkNodeId(value);
  return (
    <Dialog title="New node" onClose={onClose}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!check.ok || busy) return;
          setBusy(true);
          void runMutation(api, { op: 'add_node', node: value }, record).then((result) => {
            setBusy(false);
            if (result.kind === 'effective' || result.kind === 'no-op') {
              onCreated(value, result);
            } else {
              setVerdict(result);
            }
          });
        }}
      >
        <NodeIdField label="Node ID" value={value} onChange={setValue} disabled={busy} />
        {verdict && <VerdictView verdict={verdict} />}
        <div className="actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={!check.ok || busy}>
            {busy ? 'Creating…' : 'Create node'}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

/** The data browser page. */
export function BrowserPage(props: Props) {
  const { api, capabilities, params, navigate, record, stats } = props;
  const [nodes, setNodes] = useState<Listing<InspectedNode, NodeCursor>>(emptyListing);
  const [edges, setEdges] = useState<Listing<InspectedEdge, EdgeCursor>>(emptyListing);
  const [search, setSearch] = useState('');
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<{ node: string; verdict: MutationVerdict }>();
  const [outOfDate, setOutOfDate] = useState(false);
  const [width, setWidth] = useState(loadWidth);
  const [splitWidth, setSplitWidth] = useState(DETAIL_DEFAULT * 2);
  const request = useRef(0);
  const listRef = useRef<HTMLTableSectionElement>(null);
  const splitRef = useRef<HTMLDivElement>(null);
  const filterId = useId();
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

  // The split container's width bounds the detail pane.
  useEffect(() => {
    const element = splitRef.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver((entries) => {
      const measured = entries[0]?.contentRect.width;
      if (measured) setSplitWidth(measured);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // A new view, filter or capability set starts a new listing.
  useEffect(() => {
    startListing();
  }, [startListing]);

  const selectNode = (node: string) => navigate({ node, edge: undefined });
  const selectEdge = (source: string, target: string) =>
    navigate({ edge: `${source}-${target}`, node: undefined });
  const closeDetail = () => navigate({ node: undefined, edge: undefined });
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

  const maxWidth = Math.max(DETAIL_MIN, splitWidth - LIST_MIN);
  const resize = (next: number) => {
    const clamped = Math.min(Math.max(next, DETAIL_MIN), maxWidth);
    setWidth(clamped);
    storeWidth(clamped);
  };

  const searchCheck = checkNodeId(search);
  const listing = view === 'nodes' ? nodes : edges;
  const states = view === 'nodes' ? NODE_STATES : EDGE_STATES;
  const blocked = view === 'nodes' ? !capabilities.browseNodes : !capabilities.browseEdges;
  const total =
    stateFilter === 'all' ? (view === 'nodes' ? stats?.nodeCount : stats?.edgeCount) : undefined;
  // Stats are polled separately and can lag behind the listing.
  const shownTotal =
    total !== undefined && BigInt(total) >= BigInt(listing.rows.length) ? total : undefined;
  const hasDetail = params.node !== undefined || edgeSelection?.length === 2;

  return (
    <div className="browser">
      <div className="browser-toolbar" role="toolbar" aria-label="Browser controls">
        <div className="segmented" role="group" aria-label="List">
          <button
            type="button"
            aria-pressed={view === 'nodes'}
            onClick={() => navigate({ view: 'nodes', state: 'all' })}
          >
            Nodes {stats && <span className="count">{stats.nodeCount}</span>}
          </button>
          <button
            type="button"
            aria-pressed={view === 'edges'}
            onClick={() => navigate({ view: 'edges', state: 'all' })}
          >
            Edges {stats && <span className="count">{stats.edgeCount}</span>}
          </button>
        </div>
        <form
          className="search"
          role="search"
          onSubmit={(event) => {
            event.preventDefault();
            if (searchCheck.ok) selectNode(search);
          }}
        >
          <span className="search-icon">
            <Icon name="search" />
          </span>
          <NodeIdField
            label="Go to node ID"
            hideLabel
            placeholder="Go to node ID…"
            value={search}
            onChange={setSearch}
            disabled={!capabilities.browseNodes}
          />
          <button type="submit" className="visually-hidden" tabIndex={-1}>
            Open
          </button>
        </form>
        {lifecycle && (
          <span className="filter">
            <label htmlFor={filterId}>State</label>
            <select
              id={filterId}
              value={stateFilter}
              onChange={(event) => navigate({ state: event.target.value })}
            >
              {states.map((s) => (
                <option key={s} value={s}>
                  {STATE_LABELS[s]}
                </option>
              ))}
            </select>
          </span>
        )}
        <div className="toolbar-end">
          {listing.loadedAt && (
            <span className="note">
              Updated <Time date={listing.loadedAt} />
            </span>
          )}
          <IconButton
            icon="reload"
            label="Reload list"
            onClick={restart}
            disabled={blocked || listing.status === 'loading'}
          />
          {capabilities.canMutate && (
            <button type="button" className="primary" onClick={() => setCreating(true)}>
              <Icon name="plus" size={14} />
              New node
            </button>
          )}
        </div>
      </div>

      {(listing.identityChanged || outOfDate || created) && (
        <div className="notices">
          {listing.identityChanged && (
            <div className="notice warning" role="alert">
              <StatusBadge severity="warning" label="Graph changed while browsing" />
              <span>The server instance or generation changed between pages.</span>
              <button type="button" onClick={restart}>
                Restart listing
              </button>
            </div>
          )}
          {outOfDate && !listing.identityChanged && (
            <div className="notice">
              <span>The graph was changed from this console; the list may be out of date.</span>
              <button type="button" onClick={restart}>
                Refresh list
              </button>
            </div>
          )}
          {created && (
            <div className="notice" aria-live="polite">
              <span>
                Node <Exact value={created.node} />:
              </span>
              <VerdictView verdict={created.verdict} />
              <IconButton icon="close" label="Dismiss" onClick={() => setCreated(undefined)} />
            </div>
          )}
        </div>
      )}

      <div className="browser-split" ref={splitRef}>
        <section className="browser-list" aria-label={view === 'nodes' ? 'Nodes' : 'Edges'}>
          <div className="table-scroll">
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
                {listing.failure && <FailureView failure={listing.failure} />}
                {listing.status === 'loading' && listing.pages === 0 && (
                  <p className="empty-hint" role="status">
                    Loading…
                  </p>
                )}
                {listing.rows.length === 0 && listing.status === 'ready' ? (
                  <p className="empty-hint">
                    {stateFilter === 'all'
                      ? view === 'nodes'
                        ? 'The graph has no nodes.'
                        : 'The graph has no edges.'
                      : `No ${view} match the filter “${STATE_LABELS[stateFilter] ?? stateFilter}”.`}
                  </p>
                ) : (
                  listing.rows.length > 0 && (
                    <table className="list grid">
                      <caption className="visually-hidden">
                        {view === 'nodes' ? 'Nodes' : 'Edges'}
                      </caption>
                      <thead>
                        {view === 'nodes' ? (
                          <tr>
                            <th scope="col">Node ID</th>
                            {lifecycle && <th scope="col">State</th>}
                            <th scope="col" className="num">
                              Degree
                            </th>
                          </tr>
                        ) : (
                          <tr>
                            <th scope="col">Edge</th>
                            <th scope="col">State</th>
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
                                onClick={() => selectNode(row.node)}
                              >
                                <td>
                                  <button
                                    type="button"
                                    className="row-select"
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      selectNode(row.node);
                                    }}
                                  >
                                    <Exact value={row.node} />
                                  </button>
                                </td>
                                {lifecycle && (
                                  <td>
                                    {row.active ? (
                                      <StateDot tone="ok">Active</StateDot>
                                    ) : (
                                      <StateDot tone="warning">Inactive</StateDot>
                                    )}
                                  </td>
                                )}
                                <td className="num">{row.degree ?? '—'}</td>
                              </tr>
                            ))
                          : edges.rows.map((row) => {
                              const key = `${row.source}-${row.target}`;
                              return (
                                <tr
                                  key={key}
                                  aria-selected={params.edge === key}
                                  className={params.edge === key ? 'selected' : undefined}
                                  onClick={() => selectEdge(row.source, row.target)}
                                >
                                  <td>
                                    <button
                                      type="button"
                                      className="row-select"
                                      onClick={(event) => {
                                        event.stopPropagation();
                                        selectEdge(row.source, row.target);
                                      }}
                                    >
                                      <Exact value={row.source} /> – <Exact value={row.target} />
                                    </button>
                                  </td>
                                  <td>
                                    {row.effective ? (
                                      <StateDot tone="ok">Effective</StateDot>
                                    ) : !row.enabled ? (
                                      <StateDot tone="warning">Disabled</StateDot>
                                    ) : (
                                      <StateDot tone="warning">Not effective</StateDot>
                                    )}
                                  </td>
                                </tr>
                              );
                            })}
                      </tbody>
                    </table>
                  )
                )}
              </>
            )}
          </div>
          {!blocked && listing.rows.length > 0 && (
            <footer className="list-foot">
              <span aria-live="polite">
                {listing.rows.length}
                {shownTotal !== undefined ? ` of ${shownTotal}` : ''} loaded
              </span>
              <InfoTip label="About paging">
                Rows are read in pages of {LISTING_PAGE}, in ascending ID order. Pages are read one
                at a time and are not a snapshot.
                {listing.identity?.generation !== undefined &&
                  ` Generation ${listing.identity.generation}.`}
              </InfoTip>
              {listing.truncated ? (
                <span className="note">
                  First {LISTING_CAP} rows only. Use a filter or go to an ID.
                </span>
              ) : listing.next === null ? (
                <span className="note">End of list</span>
              ) : null}
              {listing.next !== null && listing.next !== undefined && !listing.truncated && (
                <button
                  type="button"
                  className="push"
                  disabled={listing.status === 'loading'}
                  onClick={() => (view === 'nodes' ? void loadNodes(nodes) : void loadEdges(edges))}
                >
                  {listing.status === 'loading' ? 'Loading…' : `Load ${LISTING_PAGE} more`}
                </button>
              )}
            </footer>
          )}
        </section>

        {hasDetail && (
          <Splitter
            label="Resize detail pane"
            value={width}
            min={DETAIL_MIN}
            max={Math.round(maxWidth)}
            onResize={resize}
            onStep={(delta) =>
              resize(delta === 'min' ? DETAIL_MIN : delta === 'max' ? maxWidth : width + delta)
            }
          />
        )}

        <div
          className="browser-detail"
          style={hasDetail ? { flexBasis: `${width}px` } : undefined}
          data-empty={!hasDetail || undefined}
        >
          {params.node !== undefined ? (
            <NodeDetail
              key={params.node}
              api={api}
              node={params.node}
              capabilities={capabilities}
              onSelectNode={selectNode}
              onSelectEdge={selectEdge}
              onQuery={props.onQuery}
              onClose={closeDetail}
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
              onClose={closeDetail}
              record={record}
              onMutated={() => setOutOfDate(true)}
            />
          ) : (
            <section className="detail detail-empty" aria-label="No selection">
              <h2>No selection</h2>
              <p>Select a row, or go to a node by its ID.</p>
            </section>
          )}
        </div>
      </div>

      {creating && (
        <NewNodeDialog
          api={api}
          record={record}
          onClose={() => setCreating(false)}
          onCreated={(node, verdict) => {
            setCreating(false);
            setCreated({ node, verdict });
            setOutOfDate(true);
            selectNode(node);
          }}
        />
      )}
    </div>
  );
}
