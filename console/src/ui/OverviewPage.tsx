/**
 * Overview: a short operational summary (connection, readiness, configuration,
 * graph size), the conditions that need attention, and technical details on
 * demand.
 */
import type { Result } from '../api/endpoints';
import { readinessGuidance, syncGuidance, type Severity } from '../domain/sync';
import type { MetricsState } from '../state/metrics';
import { connectivityOf, isStale, type OverviewState } from '../state/overview';
import {
  Exact,
  FailureView,
  Field,
  formatDuration,
  MetadataView,
  StatusBadge,
  Time,
  Unavailable,
} from './components';

/** One condition worth an operator's attention. */
export interface Attention {
  severity: Severity;
  text: string;
}

/** Derives attention items from the latest reads. */
export function attentionItems(
  state: OverviewState,
  metrics: MetricsState,
  now: Date,
): Attention[] {
  const items: Attention[] = [];
  const connectivity = connectivityOf(state);
  if (connectivity === 'console-unreachable') {
    items.push({ severity: 'critical', text: 'The console host is unreachable.' });
  } else if (connectivity === 'server-unreachable') {
    items.push({
      severity: 'critical',
      text: 'The Knotrel server is unreachable through the gateway.',
    });
  } else if (connectivity === 'incompatible') {
    items.push({
      severity: 'critical',
      text: 'Server responses do not match the API contract this console expects.',
    });
  }
  if (isStale(state, now)) {
    items.push({
      severity: 'warning',
      text: 'Displayed data is stale: the latest refresh failed or is overdue.',
    });
  }
  const ready = state.ready?.ok ? state.ready.value.status : undefined;
  if (ready && ready !== 'ready') {
    const guidance = readinessGuidance(ready);
    items.push({
      severity: guidance.severity,
      text: `Not ready (${guidance.label}): ${guidance.meaning}`,
    });
  }
  if (state.instanceChange) {
    items.push({
      severity: 'warning',
      text: 'The server process restarted during this session; its in-memory graph was rebuilt or lost.',
    });
  }
  const stats = state.lastStats?.value;
  const nearLimit = (remaining: string | null | undefined, max: string | null | undefined) =>
    remaining != null && max != null && BigInt(max) > 0n && BigInt(remaining) * 10n < BigInt(max);
  if (stats && nearLimit(stats.remainingNodes, stats.maxNodes)) {
    items.push({ severity: 'warning', text: 'Less than 10% of the node capacity remains.' });
  }
  if (stats && nearLimit(stats.remainingEdges, stats.maxEdges)) {
    items.push({ severity: 'warning', text: 'Less than 10% of the edge capacity remains.' });
  }
  const info = state.lastInfo?.value;
  if (info && !info.inspection) {
    items.push({
      severity: 'unknown',
      text: 'The data browser is limited: it needs server 0.8 or later with KNOTREL_INSPECTION=true.',
    });
  }
  if (metrics.availability === 'disabled') {
    items.push({
      severity: 'unknown',
      text: 'Performance metrics are disabled (KNOTREL_METRICS_ENABLED is not true).',
    });
  }
  return items;
}

function Source<T>({ label, result }: { label: string; result: Result<T> | undefined }) {
  if (!result) return <li>{label}: not read yet</li>;
  return (
    <li>
      {label}: {result.ok ? `HTTP ${result.status}` : 'failed'} at <Time date={result.receivedAt} />
      , browser round trip {formatDuration(result.durationMs)}
    </li>
  );
}

const unlimited = (value: string | null) => (value === null ? 'unlimited' : value);

function connectionLabel(connectivity: ReturnType<typeof connectivityOf>): string {
  switch (connectivity) {
    case 'console-unreachable':
      return 'Console host unreachable';
    case 'server-unreachable':
      return 'Server unreachable';
    case 'incompatible':
      return 'Incompatible responses';
    default:
      return 'Degraded';
  }
}

/** Overview page. */
export function OverviewPage({
  state,
  metrics,
  now,
}: {
  state: OverviewState;
  metrics: MetricsState;
  now: Date;
}) {
  const connectivity = connectivityOf(state);
  const stale = isStale(state, now);
  const info = state.lastInfo?.value;
  const stats = state.lastStats?.value;
  const ready = state.ready?.ok ? state.ready.value.status : undefined;
  const attention = attentionItems(state, metrics, now);
  const runtime = state.runtimeConfig?.ok ? state.runtimeConfig.value : undefined;

  return (
    <div className="page">
      <div className="tiles">
        <section className="card tile" aria-labelledby="t-conn">
          <h2 id="t-conn">Connection</h2>
          {connectivity === 'connected' ? (
            <StatusBadge
              severity={stale ? 'warning' : 'ok'}
              label={stale ? 'Connected, stale data' : 'Connected'}
            />
          ) : connectivity === 'connecting' ? (
            <StatusBadge severity="progress" label="Connecting" />
          ) : (
            <StatusBadge severity="critical" label={connectionLabel(connectivity)} />
          )}
          <p className="source">
            {runtime?.serverLabel ? (
              <Exact value={runtime.serverLabel} />
            ) : (
              <Unavailable reason="server address configured by the hosting gateway" />
            )}
          </p>
        </section>
        <section className="card tile" aria-labelledby="t-ready">
          <h2 id="t-ready">Readiness</h2>
          {ready ? (
            <StatusBadge
              severity={readinessGuidance(ready).severity}
              label={readinessGuidance(ready).label}
            />
          ) : (
            <StatusBadge severity="unknown" label="Unknown" />
          )}
          <p className="source">
            Liveness:{' '}
            {state.health?.ok ? 'process alive' : state.health ? 'no response' : 'checking'}
          </p>
        </section>
        <section className="card tile" aria-labelledby="t-server">
          <h2 id="t-server">Server</h2>
          {info ? (
            <ul className="facts-list">
              <li>
                <Exact value={info.mode} />, <Exact value={info.engine} />
                {info.experimental && ' (experimental)'}
              </li>
              <li>
                Lifecycle {info.topologyLifecycle ? 'on' : 'off'} · Inspection{' '}
                {info.inspection ? 'on' : 'off'} · Metrics{' '}
                {metrics.availability === 'available'
                  ? 'on'
                  : metrics.availability === 'disabled'
                    ? 'off'
                    : 'unknown'}
              </li>
            </ul>
          ) : (
            <p>Configuration not loaded yet.</p>
          )}
        </section>
        <section className="card tile" aria-labelledby="t-graph">
          <h2 id="t-graph">Graph</h2>
          {stats ? (
            <ul className="facts-list">
              <li>
                <Exact value={stats.nodeCount} /> nodes
                {stats.activeNodeCount !== undefined && (
                  <>
                    {' '}
                    (<Exact value={stats.activeNodeCount} /> active)
                  </>
                )}
              </li>
              <li>
                <Exact value={stats.edgeCount} /> edges
                {stats.effectiveEdgeCount !== undefined && (
                  <>
                    {' '}
                    (<Exact value={stats.effectiveEdgeCount} /> effective)
                  </>
                )}
              </li>
              {stats.syncStatus !== undefined && (
                <li>Sync: {syncGuidance(stats.syncStatus).label}</li>
              )}
            </ul>
          ) : (
            <p>Graph statistics not loaded yet.</p>
          )}
        </section>
      </div>

      <section className="card" aria-labelledby="att-h">
        <h2 id="att-h">Needs attention</h2>
        {attention.length === 0 ? (
          <p>
            <StatusBadge severity="ok" label="Nothing to report" />
          </p>
        ) : (
          <ul className="attention">
            {attention.map((item) => (
              <li key={item.text}>
                <StatusBadge
                  severity={item.severity}
                  label={
                    item.severity === 'critical'
                      ? 'Critical'
                      : item.severity === 'warning'
                        ? 'Warning'
                        : 'Note'
                  }
                />{' '}
                {item.text}
              </li>
            ))}
          </ul>
        )}
        {state.health && !state.health.ok && <FailureView failure={state.health.failure} />}
      </section>

      <details className="card technical">
        <summary>Technical details</summary>
        <dl>
          <Field label="Last successful refresh">
            {state.lastSuccessAt ? <Time date={state.lastSuccessAt} /> : 'never'}
          </Field>
          <Field label="Server version">
            <Unavailable reason="not exposed by the Knotrel HTTP API" />
          </Field>
          <Field label="Console version">
            <Exact value={__CONSOLE_VERSION__} />
          </Field>
          {info && (
            <>
              <Field label="Max pending jobs">
                <Exact value={info.maxPendingJobs} />
              </Field>
              <Field label="Max request body (bytes)">
                <Exact value={info.maxBodyBytes} />
              </Field>
              <Field label="Max batch operations">
                <Exact value={info.maxBatchOperations} />
              </Field>
            </>
          )}
          {stats && (
            <>
              <Field label="Node capacity">
                {unlimited(stats.maxNodes)} (remaining {unlimited(stats.remainingNodes)})
              </Field>
              <Field label="Edge capacity">
                {unlimited(stats.maxEdges)} (remaining {unlimited(stats.remainingEdges)})
              </Field>
            </>
          )}
        </dl>
        {state.stats?.ok && (
          <>
            <h3>Identity and version of the last statistics read</h3>
            <MetadataView metadata={state.stats.metadata} />
          </>
        )}
        <h3>Requests</h3>
        <ul className="source">
          <Source label="GET /health" result={state.health} />
          <Source label="GET /ready" result={state.ready} />
          <Source label="GET /v1/info" result={state.info} />
          <Source label="GET /v1/stats" result={state.stats} />
        </ul>
        <p className="note">
          These are independent requests and may describe different instants; only statistics and
          their version headers form one coherent snapshot.
        </p>
      </details>
    </div>
  );
}
