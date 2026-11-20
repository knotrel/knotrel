/**
 * Application shell: header, connection banner, keyboard-navigable tabs and pages.
 */
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
} from 'react';
import type { ConsoleApi } from '../api/endpoints';
import { deriveCapabilities } from '../domain/capabilities';
import { connectivityOf, isStale, OverviewStore } from '../state/overview';
import { Exact, StatusBadge, Time } from './components';
import { OperationsPage } from './OperationsPage';
import { OverviewPage } from './OverviewPage';
import { QueriesPage } from './QueriesPage';
import { SyncPage } from './SyncPage';

/** Navigable pages, in tab order. */
export const PAGES = [
  { id: 'overview', label: 'Overview' },
  { id: 'queries', label: 'Graph/Queries' },
  { id: 'sync', label: 'Synchronization' },
  { id: 'operations', label: 'Operations' },
] as const;

/** Page identifier. */
export type PageId = (typeof PAGES)[number]['id'];

function pageFromHash(hash: string): PageId {
  const id = hash.replace(/^#\/?/, '');
  return PAGES.find((p) => p.id === id)?.id ?? 'overview';
}

function useNow(intervalMs: number): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

/** Properties of {@link App}. */
export interface AppProps {
  /** API used for every request. */
  api: ConsoleApi;
  /** Optional store, for tests; created from `api` by default. */
  store?: OverviewStore;
}

/** Root component. */
export function App({ api, store: providedStore }: AppProps) {
  const store = useMemo(() => providedStore ?? new OverviewStore(api), [api, providedStore]);
  useEffect(() => {
    store.start();
    return () => store.stop();
  }, [store]);
  const state = useSyncExternalStore(store.subscribe, store.getState);
  const now = useNow(1000);
  const [page, setPage] = useState<PageId>(() => pageFromHash(window.location.hash));
  const tabRefs = useRef<Partial<Record<PageId, HTMLButtonElement | null>>>({});

  useEffect(() => {
    const onHash = () => setPage(pageFromHash(window.location.hash));
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const select = (id: PageId, focus = false) => {
    setPage(id);
    if (window.location.hash !== `#${id}`) {
      window.history.replaceState(null, '', `#${id}`);
    }
    if (focus) {
      tabRefs.current[id]?.focus();
    }
  };

  const onTabKey = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = PAGES.length - 1;
    const target =
      event.key === 'ArrowRight'
        ? index === last
          ? 0
          : index + 1
        : event.key === 'ArrowLeft'
          ? index === 0
            ? last
            : index - 1
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? last
              : undefined;
    if (target !== undefined) {
      event.preventDefault();
      const next = PAGES[target];
      if (next) {
        select(next.id, true);
      }
    }
  };

  const capabilities = deriveCapabilities(state.lastInfo?.value);
  const connectivity = connectivityOf(state);
  const stale = isStale(state, now);

  return (
    <div className="app">
      <header className="app-header">
        <h1>Knotrel Console</h1>
        <div className="header-status" role="status" aria-live="polite">
          {connectivity === 'connected' ? (
            <StatusBadge
              severity={stale ? 'warning' : 'ok'}
              label={stale ? 'Connected, stale data' : 'Connected'}
            />
          ) : connectivity === 'connecting' ? (
            <StatusBadge severity="progress" label="Connecting" />
          ) : connectivity === 'console-unreachable' ? (
            <StatusBadge severity="critical" label="Console host unreachable" />
          ) : connectivity === 'server-unreachable' ? (
            <StatusBadge severity="critical" label="Server unreachable" />
          ) : connectivity === 'incompatible' ? (
            <StatusBadge severity="critical" label="Incompatible server responses" />
          ) : (
            <StatusBadge severity="warning" label="Degraded" />
          )}
          {capabilities.mode && (
            <span className="mode">
              Mode: <Exact value={capabilities.mode} />
            </span>
          )}
          <button type="button" onClick={() => store.refresh()} disabled={state.refreshing}>
            {state.refreshing ? 'Refreshing…' : 'Refresh now'}
          </button>
        </div>
      </header>

      {state.instanceChange && (
        <div className="banner" role="alert">
          <StatusBadge severity="warning" label="Server instance changed" /> The server process
          changed from <Exact value={state.instanceChange.from} /> to{' '}
          <Exact value={state.instanceChange.to} /> at <Time date={state.instanceChange.at} />.
          In-memory graph state does not survive a restart; earlier results describe the previous
          instance.
        </div>
      )}

      <nav aria-label="Console sections">
        <div role="tablist" aria-label="Console sections" className="tabs">
          {PAGES.map((p, index) => (
            <button
              key={p.id}
              ref={(el) => {
                tabRefs.current[p.id] = el;
              }}
              type="button"
              role="tab"
              id={`tab-${p.id}`}
              aria-selected={page === p.id}
              aria-controls={`panel-${p.id}`}
              tabIndex={page === p.id ? 0 : -1}
              onClick={() => select(p.id)}
              onKeyDown={(event) => onTabKey(event, index)}
            >
              {p.label}
            </button>
          ))}
        </div>
      </nav>

      {/*
        Every panel stays mounted (inactive ones are hidden) so form input and the
        session operation history survive tab switches.
      */}
      <main className="panel">
        <div
          id="panel-overview"
          role="tabpanel"
          aria-labelledby="tab-overview"
          tabIndex={0}
          hidden={page !== 'overview'}
        >
          <OverviewPage state={state} now={now} />
        </div>
        <div
          id="panel-queries"
          role="tabpanel"
          aria-labelledby="tab-queries"
          tabIndex={0}
          hidden={page !== 'queries'}
        >
          <QueriesPage api={api} capabilities={capabilities} />
        </div>
        <div
          id="panel-sync"
          role="tabpanel"
          aria-labelledby="tab-sync"
          tabIndex={0}
          hidden={page !== 'sync'}
        >
          <SyncPage state={state} />
        </div>
        <div
          id="panel-operations"
          role="tabpanel"
          aria-labelledby="tab-operations"
          tabIndex={0}
          hidden={page !== 'operations'}
        >
          <OperationsPage api={api} capabilities={capabilities} instanceId={state.instanceId} />
        </div>
      </main>

      <footer className="app-footer">
        Knotrel console <Exact value={__CONSOLE_VERSION__} />. The console adds no authentication;
        access control belongs to the gateway in front of it.
      </footer>
    </div>
  );
}
