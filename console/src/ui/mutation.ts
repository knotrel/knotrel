/**
 * Runs one mutation exactly once and records it in the console history.
 * There is no automatic retry; uncertain outcomes are reported as such.
 */
import type { ConsoleApi } from '../api/endpoints';
import type { Operation } from '../api/wire';
import type { HistoryEntry } from '../domain/history';
import { classifyMutation, describeOperation, type MutationVerdict } from '../domain/outcomes';
import type { Severity } from '../domain/sync';

/** Records a history entry (id and time are assigned by the recorder). */
export type RecordHistory = (entry: Omit<HistoryEntry, 'id'>) => void;

const SEVERITY: Record<MutationVerdict['kind'], Severity> = {
  effective: 'ok',
  'no-op': 'unknown',
  rejected: 'critical',
  uncertain: 'warning',
};

const SUMMARY: Record<MutationVerdict['kind'], string> = {
  effective: 'Effective',
  'no-op': 'No-op',
  rejected: 'Rejected',
  uncertain: 'Outcome uncertain',
};

/** Sends `op` once and returns its verdict. */
export async function runMutation(
  api: ConsoleApi,
  op: Operation,
  record: RecordHistory,
): Promise<MutationVerdict> {
  const result = await api.operation(op, 'mutation');
  const verdict = classifyMutation(result);
  record({
    at: result.receivedAt,
    description: describeOperation(op),
    summary: SUMMARY[verdict.kind],
    severity: SEVERITY[verdict.kind],
    mutating: true,
    request: { path: 'v1/operations', body: op },
    instanceId: result.ok ? result.metadata.instanceId : undefined,
    durationMs: result.durationMs,
  });
  return verdict;
}
