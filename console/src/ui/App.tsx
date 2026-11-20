/**
 * Application shell: header, keyboard-navigable tabs adapted to the server
 * mode, shareable hash routes and the pages. Every page stays mounted (inactive
 * ones are hidden), so filters, selections, forms and history survive
 * navigation.
 */
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
} from 'react';
import type { ConsoleApi } from '../api/endpoints';
import { deriveCapabilities } from '../domain/capabilities';
import { appendHistory, type HistoryEntry } from '../domain/history';
import { MetricsStore } from '../state/metrics';
import { connectivityOf, isStale, OverviewStore } from '../state/overview';
import { formatHash, parseHash, type PageId } from '../state/route';
import { BrowserPage, browserParams, type BrowserParams } from './BrowserPage';
import { Exact, StatusBadge, Time } from './components';
import type { RecordHistory } from './mutation';
import { OverviewPage } from './OverviewPage';
import { PerformancePage } from './PerformancePage';
import type { QueryPreset } from './presets';
import { SyncPage } from './SyncPage';
import { WorkbenchPage } from './WorkbenchPage';

const LABELS: Record<PageId, string> = {
  overview: 'Overview',
  browser: 'Browser',
  workbench: 'Workbench',
  performance: 'Performance',
  sync: 'Synchronization',
};

function useNow(intervalMs: number): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

/** Hash parameters of a browser view, without defaults. */
function browserHashParams(params: BrowserParams): Record<string, string | undefined> {
  return {
    view: params.view === 'nodes' ? undefined : params.view,
    state: params.state === 'all' ? undefined : params.state,
    node: params.node,
    edge: params.edge,
  };
}

/** Properties of {@link App}. */
export interface AppProps {
  /** API used for every request. */
  api: ConsoleApi;
  /** Optional stores, for tests; created from `api` by default. */
  store?: OverviewStore;
  metricsStore?: MetricsStore;
}

/** Root component. */
export function App({ api, store: providedStore, metricsStore: providedMetrics }: AppProps) {
  const store = useMemo(() => providedStore ?? new OverviewStore(api), [api, providedStore]);
  const metricsStore = useMemo(
    () => providedMetrics ?? new MetricsStore(api),
    [api, providedMetrics],
  );
  useEffect(() => {
    store.start();
    metricsStore.start();
    return () => {
      store.stop();
      metricsStore.stop();
    };
  }, [store, metricsStore]);
  const state = useSyncExternalStore(store.subscribe, store.getState);
  const metrics = useSyncExternalStore(metricsStore.subscribe, metricsStore.getState);
  const now = useNow(1000);
  const [route, setRoute] = useState(() => parseHash(window.location.hash));
  const [browser, setBrowser] = useState<BrowserParams>(() =>
    browserParams(parseHash(window.location.hash).params),
  );
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [preset, setPreset] = useState<QueryPreset>();
  const nextId = useRef(1);
  const tabRefs = useRef<Partial<Record<PageId, HTMLButtonElement | null>>>({});

  const capabilities = deriveCapabilities(state.lastInfo?.value);
  const pages: PageId[] = ['overview', 'browser', 'workbench', 'performance'];
  if (capabilities.mode === 'source_managed') pages.push('sync');
  const page: PageId = pages.includes(route.page) ? route.page : 'overview';

  useEffect(() => {
    const onHash = () => {
      const next = parseHash(window.location.hash);
      setRoute(next);
      if (next.page === 'browser') setBrowser(browserParams(next.params));
    };
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const go = useCallback(
    (target: PageId, params: Record<string, string | undefined> = {}, focus = false) => {
      const hash = formatHash(target, params);
      if (window.location.hash !== hash) {
        window.history.replaceState(null, '', hash);
      }
      setRoute(parseHash(hash));
      if (focus) tabRefs.current[target]?.focus();
    },
    [],
  );

  const navigateBrowser = useCallback(
    (patch: Partial<BrowserParams>, focus = false) => {
      setBrowser((current) => {
        const next = { ...current, ...patch };
        go('browser', browserHashParams(next), focus);
        return next;
      });
    },
    [go],
  );

  const record: RecordHistory = useCallback((entry) => {
    setHistory((h) => appendHistory(h, { ...entry, id: nextId.current++ }));
  }, []);

  const onQuery = useCallback(
    (query: QueryPreset) => {
      setPreset({ ...query });
      go('workbench');
    },
    [go],
  );

  const open = (target: PageId, focus = false) => {
    if (target === 'browser') {
      navigateBrowser({}, focus);
    } else {
      go(target, {}, focus);
    }
  };

  const onTabKey = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = pages.length - 1;
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
    const next = target === undefined ? undefined : pages[target];
    if (next) {
      event.preventDefault();
      open(next, true);
    }
  };

  const connectivity = connectivityOf(state);
  const stale = isStale(state, now);
  const base = window.location.href.split('#')[0] ?? window.location.href;

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
          {pages.map((p, index) => (
            <button
              key={p}
              ref={(el) => {
                tabRefs.current[p] = el;
              }}
              type="button"
              role="tab"
              id={`tab-${p}`}
              aria-selected={page === p}
              aria-controls={`panel-${p}`}
              tabIndex={page === p ? 0 : -1}
              onClick={() => open(p)}
              onKeyDown={(event) => onTabKey(event, index)}
            >
              {LABELS[p]}
            </button>
          ))}
        </div>
      </nav>

      <main className="panel">
        <div
          id="panel-overview"
          role="tabpanel"
          aria-labelledby="tab-overview"
          tabIndex={0}
          hidden={page !== 'overview'}
        >
          <OverviewPage state={state} metrics={metrics} now={now} />
        </div>
        <div
          id="panel-browser"
          role="tabpanel"
          aria-labelledby="tab-browser"
          tabIndex={0}
          hidden={page !== 'browser'}
        >
          <BrowserPage
            api={api}
            capabilities={capabilities}
            stats={state.lastStats?.value}
            params={browser}
            navigate={navigateBrowser}
            record={record}
            onQuery={onQuery}
          />
        </div>
        <div
          id="panel-workbench"
          role="tabpanel"
          aria-labelledby="tab-workbench"
          tabIndex={0}
          hidden={page !== 'workbench'}
        >
          <WorkbenchPage
            api={api}
            capabilities={capabilities}
            maxBatch={Number(state.lastInfo?.value.maxBatchOperations ?? '1024')}
            history={history}
            record={record}
            onClearHistory={() => setHistory([])}
            preset={preset}
            base={base}
          />
        </div>
        <div
          id="panel-performance"
          role="tabpanel"
          aria-labelledby="tab-performance"
          tabIndex={0}
          hidden={page !== 'performance'}
        >
          <PerformancePage store={metricsStore} />
        </div>
        {capabilities.mode === 'source_managed' && (
          <div
            id="panel-sync"
            role="tabpanel"
            aria-labelledby="tab-sync"
            tabIndex={0}
            hidden={page !== 'sync'}
          >
            <SyncPage state={state} metrics={metrics} now={now} />
          </div>
        )}
      </main>

      <footer className="app-footer">
        Knotrel console <Exact value={__CONSOLE_VERSION__} />. The console adds no authentication;
        access control belongs to the gateway in front of it.
      </footer>
    </div>
  );
}
