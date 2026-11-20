/**
 * Operator guidance for synchronization and readiness states.
 *
 * Guidance is derived only from reported labels. Source lag, lease remaining time
 * and event age are not exposed by the HTTP API and are never inferred here.
 */
import type { ReadinessLabel, SyncStatus } from '../api/wire';

/** Severity used for icons and wording; never conveyed by color alone. */
export type Severity = 'ok' | 'progress' | 'warning' | 'critical' | 'unknown';

/** Guidance for one state. */
export interface Guidance {
  /** Short label. */
  label: string;
  /** Severity. */
  severity: Severity;
  /** What the state means. */
  meaning: string;
  /** What an operator can do. */
  action: string;
}

const SYNC: Record<SyncStatus, Guidance> = {
  uninitialized: {
    label: 'Uninitialized',
    severity: 'warning',
    meaning: 'The server is running but no baseline image has been imported.',
    action: 'Start the feeder so it opens a session and imports a baseline (create_only).',
  },
  loading: {
    label: 'Loading',
    severity: 'progress',
    meaning: 'A baseline or replacement image is being parsed and built.',
    action: 'Wait for the import to finish; queries keep using the previous image, if any.',
  },
  catching_up: {
    label: 'Catching up',
    severity: 'progress',
    meaning: 'An image is published and events are being applied, but no barrier has verified it.',
    action: 'The feeder must deliver remaining events and issue a barrier to restore readiness.',
  },
  ready: {
    label: 'Synchronized',
    severity: 'ok',
    meaning: 'A barrier verified the applied stream and the feeder lease is active.',
    action: 'No action needed.',
  },
  stale: {
    label: 'Stale',
    severity: 'warning',
    meaning: 'The feeder lease expired without a heartbeat; data may lag the source.',
    action: 'Check the feeder process. It must re-register a session and pass a barrier.',
  },
  desynchronized: {
    label: 'Desynchronized',
    severity: 'critical',
    meaning: 'A gap, conflicting duplicate, predecessor mismatch or epoch reset was detected.',
    action:
      'Events are rejected. The feeder must perform a replacement import (mode replace) from the source.',
  },
};

const READINESS: Record<Exclude<ReadinessLabel, SyncStatus>, Guidance> = {
  starting: {
    label: 'Starting',
    severity: 'progress',
    meaning: 'The process is starting and not yet accepting graph work.',
    action: 'Wait for startup to complete.',
  },
  draining: {
    label: 'Draining',
    severity: 'warning',
    meaning: 'The server is shutting down and refuses new graph work.',
    action: 'Route clients to another instance; this one is stopping.',
  },
  unavailable: {
    label: 'Unavailable',
    severity: 'critical',
    meaning: 'A graph job failed and the instance is permanently unavailable.',
    action: 'Restart or replace the server; in-memory data is not persisted.',
  },
};

/** Guidance for a synchronization status. */
export function syncGuidance(status: SyncStatus): Guidance {
  return SYNC[status];
}

/** Guidance for a readiness label. */
export function readinessGuidance(label: ReadinessLabel): Guidance {
  if (label === 'ready') {
    return { ...SYNC.ready, label: 'Ready', meaning: 'The server accepts graph work.' };
  }
  return label in READINESS
    ? READINESS[label as keyof typeof READINESS]
    : SYNC[label as SyncStatus];
}
