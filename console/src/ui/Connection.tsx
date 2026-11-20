/**
 * Connection indicator for the top bar: the server's name and status, with a
 * details panel that separates the server (backend) from the console host
 * (frontend) the browser actually talks to.
 */
import { useEffect, useId, useRef, useState } from 'react';
import { connectivityOf, isStale, type OverviewState } from '../state/overview';
import { Exact, Time, Unavailable } from './components';
import { Icon } from './widgets';

/** Status text and tone shown for the connection. */
export function connectionStatus(
  state: OverviewState,
  now: Date,
): { label: string; tone: 'ok' | 'warning' | 'critical' | 'progress' } {
  const connectivity = connectivityOf(state);
  switch (connectivity) {
    case 'connected':
      return isStale(state, now)
        ? { label: 'Connected, stale data', tone: 'warning' }
        : { label: 'Connected', tone: 'ok' };
    case 'connecting':
      return { label: 'Connecting', tone: 'progress' };
    case 'console-unreachable':
      return { label: 'Console host unreachable', tone: 'critical' };
    case 'server-unreachable':
      return { label: 'Server unreachable', tone: 'critical' };
    case 'incompatible':
      return { label: 'Incompatible server responses', tone: 'critical' };
    default:
      return { label: 'Degraded', tone: 'warning' };
  }
}

/** Properties of {@link ConnectionButton}. */
export interface ConnectionProps {
  state: OverviewState;
  now: Date;
  onRefresh: () => void;
}

/** Top-bar connection button and its details panel. */
export function ConnectionButton({ state, now, onRefresh }: ConnectionProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const panelId = useId();
  const status = connectionStatus(state, now);
  const runtime = state.runtimeConfig?.ok ? state.runtimeConfig.value : undefined;
  const info = state.lastInfo?.value;
  const name = runtime?.serverLabel ?? 'Knotrel server';
  const mode = info ? (info.mode === 'source_managed' ? 'source-managed' : 'standalone') : '';

  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  return (
    <div className="connection" ref={root}>
      <button
        type="button"
        className={`connection-button tone-${status.tone}`}
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="dot" aria-hidden="true" />
        <span className="connection-name">{name}</span>
        <span className="connection-meta" role="status" aria-live="polite">
          {status.tone === 'ok' ? (
            <span className="visually-hidden">{status.label}</span>
          ) : (
            status.label
          )}
          {status.tone === 'ok' && mode ? mode : ''}
        </span>
        <Icon name="chevron" size={14} />
      </button>
      <div id={panelId} className="connection-panel" hidden={!open}>
        <h2>Connection</h2>
        <section aria-labelledby={`${panelId}-server`}>
          <h3 id={`${panelId}-server`}>Server (backend)</h3>
          <dl>
            <div>
              <dt>Name</dt>
              <dd>
                {runtime?.serverLabel ? (
                  <Exact value={runtime.serverLabel} />
                ) : (
                  <Unavailable reason="not configured by the console host" />
                )}
              </dd>
            </div>
            <div>
              <dt>Status</dt>
              <dd>{status.label}</dd>
            </div>
            {info && (
              <>
                <div>
                  <dt>Mode</dt>
                  <dd>{mode}</dd>
                </div>
                <div>
                  <dt>Engine</dt>
                  <dd>
                    <Exact value={info.engine} />
                  </dd>
                </div>
                <div>
                  <dt>Lifecycle</dt>
                  <dd>{info.topologyLifecycle ? 'on' : 'off'}</dd>
                </div>
                <div>
                  <dt>Inspection</dt>
                  <dd>{info.inspection ? 'on' : 'off'}</dd>
                </div>
              </>
            )}
            {state.instanceId && (
              <div>
                <dt>Instance</dt>
                <dd>
                  <Exact value={state.instanceId} />
                </dd>
              </div>
            )}
          </dl>
        </section>
        <section aria-labelledby={`${panelId}-console`}>
          <h3 id={`${panelId}-console`}>Console (frontend)</h3>
          <dl>
            <div>
              <dt>Address</dt>
              <dd>
                <Exact value={window.location.host || window.location.origin} />
              </dd>
            </div>
            <div>
              <dt>Version</dt>
              <dd>
                <Exact value={__CONSOLE_VERSION__} />
              </dd>
            </div>
            <div>
              <dt>Deployment</dt>
              <dd>{runtime?.deployment ?? 'not reported'}</dd>
            </div>
          </dl>
          <p className="note">
            The browser sends requests to this console under <code className="exact">api/</code>;
            the console host forwards them to the server. The console adds no authentication; access
            control belongs to the gateway in front of it.
          </p>
        </section>
        <div className="connection-foot">
          <span className="note">
            {state.lastSuccessAt ? (
              <>
                Last response <Time date={state.lastSuccessAt} />
              </>
            ) : (
              'No response yet'
            )}
          </span>
          <button type="button" onClick={onRefresh} disabled={state.refreshing}>
            {state.refreshing ? 'Refreshing…' : 'Refresh now'}
          </button>
        </div>
      </div>
    </div>
  );
}
