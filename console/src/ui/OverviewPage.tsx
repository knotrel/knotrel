/**
 * Overview: connectivity, liveness, readiness, configuration, graph counts and
 * instance identity. Each section names the request it came from and when.
 */
import type { Result } from '../api/endpoints';
import { readinessGuidance, syncGuidance } from '../domain/sync';
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

function Source<T>({ label, result }: { label: string; result: Result<T> | undefined }) {
  if (!result) {
    return <p className="source">{label}: not read yet</p>;
  }
  return (
    <p className="source">
      {label}: {result.ok ? `HTTP ${result.status}` : 'failed'} at <Time date={result.receivedAt} />
      , browser round trip {formatDuration(result.durationMs)}
    </p>
  );
}

const unlimited = (value: string | null) => (value === null ? 'unlimited' : value);

/** Overview page. */
export function OverviewPage({ state, now }: { state: OverviewState; now: Date }) {
  const connectivity = connectivityOf(state);
  const stale = isStale(state, now);
  const info = state.lastInfo?.value;
  const stats = state.lastStats?.value;
  const runtime = state.runtimeConfig?.ok ? state.runtimeConfig.value : undefined;

  return (
    <div className="page">
      <section aria-labelledby="conn-h" className="card">
        <h2 id="conn-h">Connection</h2>
        <dl>
          <Field label="Console host">
            {connectivity === 'console-unreachable' ? (
              <StatusBadge severity="critical" label="Unreachable" />
            ) : connectivity === 'connecting' ? (
              <StatusBadge severity="progress" label="Connecting" />
            ) : (
              <StatusBadge severity="ok" label="Reachable" />
            )}
          </Field>
          <Field label="Server address">
            {runtime?.serverLabel !== undefined ? (
              <Exact value={runtime.serverLabel} />
            ) : (
              <Unavailable reason="configured by the hosting gateway" />
            )}
          </Field>
          <Field label="Connectivity to server">
            {connectivity === 'connected' ? (
              <StatusBadge severity="ok" label="Connected" />
            ) : connectivity === 'server-unreachable' ? (
              <StatusBadge severity="critical" label="Server unreachable" />
            ) : connectivity === 'incompatible' ? (
              <StatusBadge severity="critical" label="Incompatible responses" />
            ) : connectivity === 'console-unreachable' ? (
              <StatusBadge severity="unknown" label="Unknown (console host unreachable)" />
            ) : connectivity === 'connecting' ? (
              <StatusBadge severity="progress" label="Connecting" />
            ) : (
              <StatusBadge severity="warning" label="Degraded" />
            )}
          </Field>
          <Field label="Last successful refresh">
            {state.lastSuccessAt ? <Time date={state.lastSuccessAt} /> : 'never'}
            {stale && (
              <>
                {' '}
                <StatusBadge severity="warning" label="Stale data" />
              </>
            )}
          </Field>
          <Field label="Last attempt">
            {state.lastAttemptAt ? <Time date={state.lastAttemptAt} /> : 'not yet'}
            {state.consecutiveFailures > 0 &&
              ` (${state.consecutiveFailures} consecutive failure${state.consecutiveFailures > 1 ? 's' : ''}; retrying with backoff)`}
          </Field>
        </dl>
        {state.health && !state.health.ok && <FailureView failure={state.health.failure} />}
      </section>

      <section aria-labelledby="health-h" className="card">
        <h2 id="health-h">Liveness and readiness</h2>
        <dl>
          <Field label="Liveness (GET /health)">
            {!state.health ? (
              <StatusBadge severity="progress" label="Checking" />
            ) : state.health.ok ? (
              <StatusBadge severity="ok" label="Process alive" />
            ) : (
              <StatusBadge severity="critical" label="No liveness response" />
            )}
          </Field>
          <Field label="Readiness (GET /ready)">
            {!state.ready ? (
              <StatusBadge severity="progress" label="Checking" />
            ) : state.ready.ok ? (
              <>
                <StatusBadge
                  severity={readinessGuidance(state.ready.value.status).severity}
                  label={readinessGuidance(state.ready.value.status).label}
                />{' '}
                <Exact value={state.ready.value.status} /> (HTTP {state.ready.status})
              </>
            ) : (
              <StatusBadge severity="unknown" label="Unknown" />
            )}
          </Field>
        </dl>
        {state.ready?.ok && <p>{readinessGuidance(state.ready.value.status).meaning}</p>}
        <p className="note">
          Liveness only means the process answers. Readiness means it accepts graph work and, in
          source-managed mode, that synchronization was verified.
        </p>
        <Source label="Liveness" result={state.health} />
        <Source label="Readiness" result={state.ready} />
      </section>

      <section aria-labelledby="server-h" className="card">
        <h2 id="server-h">Server</h2>
        {info ? (
          <dl>
            <Field label="Server version">
              <Unavailable reason="not exposed by the Knotrel HTTP API" />
            </Field>
            <Field label="Console version">
              <Exact value={__CONSOLE_VERSION__} />
            </Field>
            <Field label="Mode">
              <Exact value={info.mode} />
            </Field>
            <Field label="Engine">
              <Exact value={info.engine} />
              {info.experimental && ' (experimental)'}
            </Field>
            <Field label="Node and edge lifecycle">
              {info.topologyLifecycle ? 'enabled' : 'disabled'}
            </Field>
            <Field label="Max pending jobs">
              <Exact value={info.maxPendingJobs} />
            </Field>
            <Field label="Max request body (bytes)">
              <Exact value={info.maxBodyBytes} />
            </Field>
            <Field label="Max batch operations">
              <Exact value={info.maxBatchOperations} />
            </Field>
          </dl>
        ) : (
          <p>Configuration not loaded yet.</p>
        )}
        {state.info && !state.info.ok && <FailureView failure={state.info.failure} />}
        <Source label="Configuration (GET /v1/info)" result={state.info} />
      </section>

      <section aria-labelledby="graph-h" className="card">
        <h2 id="graph-h">Graph</h2>
        {stats ? (
          <>
            <dl>
              <Field label="Registered nodes">
                <Exact value={stats.nodeCount} /> of {unlimited(stats.maxNodes)} (remaining{' '}
                {unlimited(stats.remainingNodes)})
              </Field>
              <Field label="Registered edges">
                <Exact value={stats.edgeCount} /> of {unlimited(stats.maxEdges)} (remaining{' '}
                {unlimited(stats.remainingEdges)})
              </Field>
              <Field label="Active nodes">
                {stats.activeNodeCount === undefined ? (
                  <Unavailable reason="requires lifecycle support" />
                ) : (
                  <Exact value={stats.activeNodeCount} />
                )}
              </Field>
              <Field label="Enabled edges">
                {stats.enabledEdgeCount === undefined ? (
                  <Unavailable reason="requires lifecycle support" />
                ) : (
                  <Exact value={stats.enabledEdgeCount} />
                )}
              </Field>
              <Field label="Effective edges">
                {stats.effectiveEdgeCount === undefined ? (
                  <Unavailable reason="requires lifecycle support" />
                ) : (
                  <Exact value={stats.effectiveEdgeCount} />
                )}
              </Field>
              {stats.syncStatus !== undefined && (
                <Field label="Sync status">
                  <StatusBadge
                    severity={syncGuidance(stats.syncStatus).severity}
                    label={syncGuidance(stats.syncStatus).label}
                  />
                </Field>
              )}
            </dl>
            {state.stats?.ok && (
              <>
                <h3>Identity and version (same response)</h3>
                <MetadataView metadata={state.stats.metadata} />
              </>
            )}
          </>
        ) : (
          <p>Graph statistics not loaded yet.</p>
        )}
        {state.stats && !state.stats.ok && <FailureView failure={state.stats.failure} />}
        <Source label="Statistics (GET /v1/stats)" result={state.stats} />
        <p className="note">
          Statistics and their version headers are one coherent server snapshot. Liveness,
          readiness, configuration and statistics are separate requests and may describe different
          instants.
        </p>
      </section>
    </div>
  );
}
