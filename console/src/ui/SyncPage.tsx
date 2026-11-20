/**
 * Synchronization status for source-managed servers. The console never issues
 * feeder commands (session, heartbeat, import, events, gap, barrier).
 */
import { syncGuidance, readinessGuidance } from '../domain/sync';
import type { OverviewState } from '../state/overview';
import { Exact, Field, StatusBadge, Time, Unavailable } from './components';

/** Synchronization page. */
export function SyncPage({ state }: { state: OverviewState }) {
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
          <Field label="Observed at">{stats ? <Time date={stats.receivedAt} /> : 'never'}</Field>
          <Field label="Source lag">
            <Unavailable reason="not exposed by the HTTP API; not inferred from readiness" />
          </Field>
          <Field label="Feeder lease remaining">
            <Unavailable reason="exposed only as a Prometheus metric when metrics are enabled" />
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
        <p className="note">
          Synchronization is driven by the external feeder from the authoritative source. This
          console does not take over sessions or send heartbeats, imports, barriers or gap
          declarations.
        </p>
      </section>
    </div>
  );
}
