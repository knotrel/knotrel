/**
 * Performance view from `GET /metrics`: request and error rates, admission
 * rejections, server-side latency, saturation and graph-job timing.
 *
 * Every number is computed from scrapes this tab observed (no stored
 * history). Rates use reset-corrected counter increases; latencies are
 * quantiles estimated from histogram bucket deltas.
 */
import { useState, useSyncExternalStore } from 'react';
import {
  byCodeUnit,
  gauge,
  histogramQuantile,
  increase,
  select,
  type Scrape,
} from '../domain/prometheus';
import type { MetricsStore } from '../state/metrics';
import { StatusBadge, Time } from './components';

const WINDOWS = [
  { id: '60', label: 'last 1 minute', ms: 60_000 },
  { id: '300', label: 'last 5 minutes', ms: 300_000 },
  { id: 'session', label: 'whole session', ms: Infinity },
] as const;

/** Picks the oldest scrape inside the window and the latest scrape. */
export function windowScrapes(
  scrapes: readonly Scrape[],
  windowMs: number,
): [Scrape, Scrape] | undefined {
  const latest = scrapes[scrapes.length - 1];
  if (!latest || scrapes.length < 2) return undefined;
  const from =
    scrapes.find((s) => latest.at - s.at <= windowMs && s !== latest) ??
    scrapes[scrapes.length - 2];
  return from ? [from, latest] : undefined;
}

function rate(value: number | undefined): string {
  return value === undefined ? '—' : value < 0.1 && value > 0 ? '< 0.1/s' : `${value.toFixed(1)}/s`;
}

function ms(seconds: number | undefined): string {
  if (seconds === undefined) return 'no observations';
  return seconds < 0.001
    ? `${(seconds * 1_000_000).toFixed(0)} µs`
    : `${(seconds * 1000).toFixed(seconds < 0.01 ? 2 : 1)} ms`;
}

function labelValues(scrape: Scrape, name: string, label: string): string[] {
  return [...new Set(select(scrape, name).map((s) => s.labels[label] ?? ''))]
    .filter(Boolean)
    .sort(byCodeUnit);
}

/** Server admission lanes in display order (`lane` label, since 1.0.0). */
const LANE_ORDER = ['shared', 'feeder_data', 'feeder_control', 'import'];

/** Orders known lanes first, then any other value by code unit. */
export function byLane(a: string, b: string): number {
  const ia = LANE_ORDER.indexOf(a);
  const ib = LANE_ORDER.indexOf(b);
  if (ia !== ib) return (ia < 0 ? LANE_ORDER.length : ia) - (ib < 0 ? LANE_ORDER.length : ib);
  return byCodeUnit(a, b);
}

/** Outstanding work and capacity of one admission lane. */
export interface LaneSaturation {
  /** `lane` label value, or `undefined` for a server without the label. */
  lane: string | undefined;
  outstanding: number | undefined;
  capacity: number | undefined;
}

/**
 * Saturation per admission lane. Each lane's outstanding count is paired
 * with that lane's capacity only. A server before 1.0.0 exports the two
 * gauges without a `lane` label: one unlabelled row is returned.
 */
export function laneSaturation(scrape: Scrape): LaneSaturation[] {
  const lanes = labelValues(scrape, 'knotrel_graph_job_capacity', 'lane').sort(byLane);
  if (lanes.length === 0) {
    return [
      {
        lane: undefined,
        outstanding: gauge(scrape, 'knotrel_graph_jobs_outstanding'),
        capacity: gauge(scrape, 'knotrel_graph_job_capacity'),
      },
    ];
  }
  return lanes.map((lane) => ({
    lane,
    outstanding: gauge(scrape, 'knotrel_graph_jobs_outstanding', { lane }),
    capacity: gauge(scrape, 'knotrel_graph_job_capacity', { lane }),
  }));
}

/** `(reason, lane)` pairs of the rejection counter; `lane` is absent before 1.0.0. */
function rejectionSeries(scrape: Scrape): { reason: string; lane: string | undefined }[] {
  const keys = new Map<string, { reason: string; lane: string | undefined }>();
  for (const s of select(scrape, 'knotrel_requests_rejected_total')) {
    const reason = s.labels['reason'];
    if (!reason) continue;
    const lane = s.labels['lane'] || undefined;
    keys.set(`${reason}\u0000${lane ?? ''}`, { reason, lane });
  }
  return [...keys.values()].sort(
    (a, b) => byCodeUnit(a.reason, b.reason) || byLane(a.lane ?? '', b.lane ?? ''),
  );
}

/** The Performance page. */
export function PerformancePage({ store }: { store: MetricsStore }) {
  const state = useSyncExternalStore(store.subscribe, store.getState);
  const [windowId, setWindowId] = useState<(typeof WINDOWS)[number]['id']>('60');

  if (state.availability === 'disabled') {
    return (
      <div className="page">
        <section className="card">
          <h2>Performance</h2>
          <p>
            <StatusBadge severity="unknown" label="Metrics disabled" /> The server answers 404 for{' '}
            <code className="exact">/metrics</code>. Start it with{' '}
            <code className="exact">KNOTREL_METRICS_ENABLED=true</code> to see rates, latency and
            saturation.
          </p>
          <button type="button" onClick={() => store.retry()}>
            Check again
          </button>
        </section>
      </div>
    );
  }
  const window = WINDOWS.find((w) => w.id === windowId) ?? WINDOWS[0];
  const pair = windowScrapes(state.scrapes, window.ms);
  const latest = state.scrapes[state.scrapes.length - 1];
  const first = state.scrapes[0];

  return (
    <div className="page">
      <section className="card" aria-labelledby="perf-h">
        <h2 id="perf-h">Performance</h2>
        <div className="toolbar">
          <label className="filter">
            Window{' '}
            <select
              value={windowId}
              onChange={(event) => setWindowId(event.target.value as typeof windowId)}
            >
              {WINDOWS.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.label}
                </option>
              ))}
            </select>
          </label>
          {state.availability === 'error' && (
            <StatusBadge severity="critical" label={`Scrape failed (${state.error ?? 'error'})`} />
          )}
        </div>
        <p className="note">
          Computed from scrapes of <code className="exact">/metrics</code> taken by this tab every 5
          s while it is visible. There is no stored history: the session started at{' '}
          {first ? <Time date={new Date(first.at)} /> : '—'} ({state.scrapes.length} scrapes).
          Server-side times exclude the network; browser round trips are shown with each request
          instead.
        </p>
        {!pair || !latest ? (
          <p role="status">
            <StatusBadge severity="progress" label="Collecting" /> Rates need at least two scrapes.
          </p>
        ) : (
          <PerformanceBody from={pair[0]} to={pair[1]} />
        )}
        {state.skippedLines > 0 && (
          <p className="note">{state.skippedLines} unparsed metric lines were ignored.</p>
        )}
      </section>
    </div>
  );
}

function PerformanceBody({ from, to }: { from: Scrape; to: Scrape }) {
  const seconds = (to.at - from.at) / 1000;
  const total = increase(from, to, 'knotrel_http_requests_total');
  const reset = total?.reset === true;
  const routes = labelValues(to, 'knotrel_http_requests_total', 'route');
  const rejected = rejectionSeries(to);
  const uncertain = labelValues(to, 'knotrel_requests_outcome_uncertain_total', 'cause');
  const kinds = labelValues(to, 'knotrel_graph_job_execution_seconds_bucket', 'kind');
  const lanes = laneSaturation(to);
  return (
    <>
      <p className="source">
        Window: {seconds.toFixed(0)} s between <Time date={new Date(from.at)} /> and{' '}
        <Time date={new Date(to.at)} />.
      </p>
      {reset && (
        <div className="banner" role="alert">
          <StatusBadge severity="warning" label="Counter reset" /> Counters decreased in this window
          (server restart). Rates count from the reset.
        </div>
      )}
      <h3>Saturation</h3>
      <table className="list">
        <caption className="visually-hidden">
          Graph jobs outstanding and capacity by admission lane
        </caption>
        <thead>
          <tr>
            <th scope="col">Admission lane</th>
            <th scope="col">Graph jobs outstanding / capacity</th>
          </tr>
        </thead>
        <tbody>
          {lanes.map(({ lane, outstanding, capacity }) => (
            <tr key={lane ?? ''}>
              <td>
                {lane === undefined ? (
                  'all (server without lane label)'
                ) : (
                  <code className="exact">{lane}</code>
                )}
              </td>
              <td>
                {outstanding === undefined || capacity === undefined
                  ? '—'
                  : `${outstanding} / ${capacity}`}
                {outstanding !== undefined && capacity !== undefined && outstanding >= capacity && (
                  <>
                    {' '}
                    <StatusBadge
                      severity="warning"
                      label={
                        lane === 'import'
                          ? 'At capacity: an import is in progress; another is refused'
                          : 'At capacity: new work on this lane is refused (busy)'
                      }
                    />
                  </>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <h3>Requests by route</h3>
      <table className="list">
        <caption className="visually-hidden">
          Request rates and server-side latency by route
        </caption>
        <thead>
          <tr>
            <th scope="col">Route</th>
            <th scope="col">Rate</th>
            <th scope="col">4xx</th>
            <th scope="col">5xx</th>
            <th scope="col">Server p50</th>
            <th scope="col">Server p95</th>
          </tr>
        </thead>
        <tbody>
          {routes.map((route) => {
            const all = increase(from, to, 'knotrel_http_requests_total', { route });
            const c4 = increase(from, to, 'knotrel_http_requests_total', {
              route,
              status_class: '4xx',
            });
            const c5 = increase(from, to, 'knotrel_http_requests_total', {
              route,
              status_class: '5xx',
            });
            const p50 = histogramQuantile(
              from,
              to,
              'knotrel_http_request_duration_seconds',
              { route },
              0.5,
            );
            const p95 = histogramQuantile(
              from,
              to,
              'knotrel_http_request_duration_seconds',
              { route },
              0.95,
            );
            return (
              <tr key={route}>
                <td>
                  <code className="exact">{route}</code>
                </td>
                <td>{rate(all && all.value / all.seconds)}</td>
                <td>{rate(c4 && c4.value / c4.seconds)}</td>
                <td>{rate(c5 && c5.value / c5.seconds)}</td>
                <td>{ms(p50?.value)}</td>
                <td>{ms(p95?.value)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="note">
        Server p50/p95: estimated from{' '}
        <code className="exact">knotrel_http_request_duration_seconds</code> bucket deltas in the
        window (middleware entry to response, no network).
      </p>
      <h3>Admission rejections and uncertain outcomes</h3>
      {rejected.length === 0 && uncertain.length === 0 ? (
        <p>No rejection series reported.</p>
      ) : (
        <ul>
          {rejected.map(({ reason, lane }) => {
            const r = increase(
              from,
              to,
              'knotrel_requests_rejected_total',
              lane === undefined ? { reason } : { reason, lane },
            );
            return (
              <li key={`${reason} ${lane ?? ''}`}>
                Rejected (<code className="exact">{reason}</code>
                {lane !== undefined && lane !== 'none' && (
                  <>
                    , lane <code className="exact">{lane}</code>
                  </>
                )}
                ): {rate(r && r.value / r.seconds)} ({r?.value ?? 0} in window)
              </li>
            );
          })}
          {uncertain.map((cause) => {
            const r = increase(from, to, 'knotrel_requests_outcome_uncertain_total', { cause });
            return (
              <li key={cause}>
                Outcome uncertain (<code className="exact">{cause}</code>): {r?.value ?? 0} in
                window
              </li>
            );
          })}
        </ul>
      )}
      <h3>Graph jobs (server processing time)</h3>
      <table className="list">
        <caption className="visually-hidden">
          Graph job queue, lock wait and execution by kind
        </caption>
        <thead>
          <tr>
            <th scope="col">Kind</th>
            <th scope="col">Jobs</th>
            <th scope="col">Queue p95</th>
            <th scope="col">Lock wait p95</th>
            <th scope="col">Execution p95</th>
          </tr>
        </thead>
        <tbody>
          {kinds.map((kind) => {
            const exec = histogramQuantile(
              from,
              to,
              'knotrel_graph_job_execution_seconds',
              { kind },
              0.95,
            );
            return (
              <tr key={kind}>
                <td>
                  <code className="exact">{kind}</code>
                </td>
                <td>{exec?.count ?? 0}</td>
                <td>
                  {ms(
                    histogramQuantile(from, to, 'knotrel_graph_job_queue_seconds', { kind }, 0.95)
                      ?.value,
                  )}
                </td>
                <td>
                  {ms(
                    histogramQuantile(from, to, 'knotrel_graph_lock_wait_seconds', { kind }, 0.95)
                      ?.value,
                  )}
                </td>
                <td>{ms(exec?.value)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="note">
        Job timing is aggregate per kind, measured by the server around the graph lock. It is the
        only server-side processing time available; no per-request value exists.
      </p>
    </>
  );
}
