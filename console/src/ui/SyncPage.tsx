/**
 * Synchronization status for source-managed servers. The console never issues
 * feeder commands (session, heartbeat, import, events, gap, barrier).
 */
import { gauge } from '../domain/prometheus';
import { syncGuidance, readinessGuidance } from '../domain/sync';
import type { MetricsState } from '../state/metrics';
import { isStale, type OverviewState } from '../state/overview';
import { Exact, Field, StatusBadge, Time, Unavailable } from './components';

/** Synchronization page. */
export function SyncPage({
  state,
  metrics,
  now,
}: {
  state: OverviewState;
  metrics: MetricsState;
  now: Date;
}) {
  const info = state.lastInfo?.value;
  if (!info) {
    return (
      <div className="page">
        <section className="card">
          <h2>Synchronization</h2>
          <p>Waiting for the server configuration.</p>
        </section>
      </div>
    );
  }
  if (info.mode === 'standalone') {
    return (
      <div className="page">
        <section className="card">
          <h2>Synchronization</h2>
          <p>
            <StatusBadge severity="unknown" label="Not applicable" /> This server runs in standalone
            mode: there is no external source or feeder, and the graph changes only through direct
            operations.
          </p>
        </section>
      </div>
    );
  }
  const stats = state.lastStats;
  const status = stats?.value.syncStatus;
  const guidance = status ? syncGuidance(status) : undefined;
  const ready = state.ready?.ok ? state.ready.value.status : undefined;
  const scrape = metrics.scrapes[metrics.scrapes.length - 1];
  const lease = gauge(scrape, 'knotrel_feeder_lease_remaining_seconds');
  const age = gauge(scrape, 'knotrel_feeder_last_applied_age_seconds');
  const metricsNote =
    metrics.availability === 'disabled'
      ? 'metrics disabled on the server'
      : metrics.availability === 'available'
        ? 'not reported in the latest metrics scrape'
        : 'metrics not read yet';
  return (
    <div className="page">
      <section className="card" aria-labelledby="sync-h">
        <h2 id="sync-h">Synchronization</h2>
        <dl>
          <Field label="Sync status (GET /v1/stats)">
            {guidance && status ? (
              <>
                <StatusBadge severity={guidance.severity} label={guidance.label} />{' '}
                <Exact value={status} />
              </>
            ) : (
              <Unavailable reason="statistics not loaded" />
            )}
          </Field>
          <Field label="Readiness (GET /ready)">
            {ready ? (
              <>
                <StatusBadge
                  severity={readinessGuidance(ready).severity}
                  label={readinessGuidance(ready).label}
                />{' '}
                <Exact value={ready} />
              </>
            ) : (
              <Unavailable reason="readiness not loaded" />
            )}
          </Field>
          <Field label="Instance">
            {info.instanceId ? <Exact value={info.instanceId} /> : <Unavailable reason="absent" />}
          </Field>
          <Field label="Generation">
            {stats?.value.generation !== undefined ? (
              <Exact value={stats.value.generation} />
            ) : (
              <Unavailable reason="not reported" />
            )}
          </Field>
          <Field label="Applied source position">
            {stats?.value.sourcePosition !== undefined ? (
              <Exact value={stats.value.sourcePosition} />
            ) : (
              <Unavailable reason="no source position yet" />
            )}
          </Field>
          <Field label="Local state version">
            {state.stats?.ok && state.stats.metadata.stateVersion !== undefined ? (
              <Exact value={state.stats.metadata.stateVersion} />
            ) : (
              <Unavailable reason="not reported" />
            )}
          </Field>
          <Field label="Observed at">
            {stats ? (
              <>
                <Time date={stats.receivedAt} />
                {isStale(state, now) && (
                  <>
                    {' '}
                    <StatusBadge severity="warning" label="Stale" />
                  </>
                )}
              </>
            ) : (
              'never'
            )}
          </Field>
          <Field label="Feeder lease remaining">
            {lease !== undefined ? (
              `${lease.toFixed(1)} s (metrics)`
            ) : (
              <Unavailable reason={metricsNote} />
            )}
          </Field>
          <Field label="Since last applied event">
            {age !== undefined ? (
              `${age.toFixed(1)} s (metrics)`
            ) : (
              <Unavailable reason={metricsNote} />
            )}
          </Field>
          <Field label="Source lag">
            <Unavailable reason="not measurable: the source position is opaque and the local revision counts local changes, so they cannot be subtracted" />
          </Field>
        </dl>
        {guidance && (
          <div className="guidance">
            <h3>What this means</h3>
            <p>{guidance.meaning}</p>
            <h3>What to do</h3>
            <p>{guidance.action}</p>
          </div>
        )}
        <details className="definitions">
          <summary>What do source position and state version mean?</summary>
          <ul>
            <li>
              <strong>Applied source position:</strong> an opaque cursor of the authoritative source
              (for example a log position) up to which events have been applied. Knotrel never
              interprets it.
            </li>
            <li>
              <strong>Generation:</strong> the published baseline; each replacement import
              increments it.
            </li>
            <li>
              <strong>State version:</strong> local revision, counting effective changes within the
              current generation. It is not a source position and does not measure lag.
            </li>
            <li>
              <strong>Since last applied event:</strong> time since the feeder last applied an
              event. A quiet source also produces large values, so it is not lag either.
            </li>
          </ul>
        </details>
        <p className="note">
          Synchronization is driven by the external feeder from the authoritative source. This
          console does not take over sessions or send heartbeats, imports, barriers or gap
          declarations.
        </p>
      </section>
    </div>
  );
}
