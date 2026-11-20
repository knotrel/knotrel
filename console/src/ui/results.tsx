/**
 * Shared rendering of request results: outcomes, mutation verdicts and the
 * timing/version block that accompanies every response.
 */
import type { OperationOutcome } from '../api/decode';
import type { Result } from '../api/endpoints';
import type { MutationVerdict } from '../domain/outcomes';
import { Exact, FailureView, formatDuration, MetadataView, StatusBadge, Time } from './components';

/** Plain-language answer of an operation outcome. */
export function describeOutcome(outcome: OperationOutcome): string {
  switch (outcome.result) {
    case 'connected':
      return outcome.connected ? 'Connected' : 'Not connected';
    case 'node_state':
      return outcome.registered
        ? `Registered, ${outcome.active ? 'active' : 'inactive'}`
        : 'Not registered';
    case 'edge_state':
      return outcome.registered
        ? `Registered, ${outcome.enabled ? 'enabled' : 'disabled'}, ${outcome.effective ? 'effective' : 'not effective'}`
        : 'Not registered';
    case 'changed':
      return outcome.changed ? 'Changed' : 'No change';
  }
}

/** Verdict badge with explanation. */
export function VerdictView({ verdict }: { verdict: MutationVerdict }) {
  switch (verdict.kind) {
    case 'effective':
      return (
        <p>
          <StatusBadge severity="ok" label="Effective" /> The graph changed
          {verdict.stateVersion !== undefined && (
            <>
              ; state version is now <Exact value={verdict.stateVersion} />
            </>
          )}
          .
        </p>
      );
    case 'no-op':
      return (
        <p>
          <StatusBadge severity="unknown" label="No-op" /> Accepted without change (already in the
          requested state).
        </p>
      );
    case 'rejected':
      return (
        <p>
          <StatusBadge severity="critical" label="Rejected" /> Not applied
          {verdict.code !== undefined && (
            <>
              {' '}
              (<Exact value={verdict.code} />)
            </>
          )}
          : <span className="server-text">{verdict.reason}</span>
        </p>
      );
    case 'uncertain':
      return (
        <p>
          <StatusBadge severity="warning" label="Outcome uncertain" /> {verdict.reason}. The change
          may or may not have been applied; a timeout is not a rollback. It was not retried
          automatically.
        </p>
      );
  }
}

/**
 * Timing and version facts of one response. Browser time and server time are
 * different measures and are labelled as such.
 */
export function ResponseFacts<T>({ result }: { result: Result<T> }) {
  return (
    <div className="facts">
      {result.ok && <MetadataView metadata={result.metadata} />}
      <p className="source">
        Received at <Time date={result.receivedAt} />. Browser round trip{' '}
        {formatDuration(result.durationMs)} (measured in this browser, including network and
        gateway). Server processing time is not reported per request; the Performance page shows
        aggregate server job time from metrics.
      </p>
    </div>
  );
}

/** A query result card. */
export function OutcomeView({ result }: { result: Result<OperationOutcome> }) {
  return (
    <div className="result" aria-live="polite">
      {result.ok ? (
        <p className="answer">
          <StatusBadge
            severity={
              result.value.result === 'connected' && !result.value.connected ? 'warning' : 'ok'
            }
            label={describeOutcome(result.value)}
          />
        </p>
      ) : (
        <FailureView failure={result.failure} />
      )}
      <ResponseFacts result={result} />
    </div>
  );
}
