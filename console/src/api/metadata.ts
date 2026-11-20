/**
 * Graph identity and version metadata carried by response headers.
 *
 * The server captures these headers under the same lock acquisition as the
 * response body, so they describe exactly the state the result was computed on.
 * Values stay strings: generations and revisions can exceed 2^53.
 */
import { SYNC_STATUSES, type SyncStatus } from './wire';

/** Version metadata of one response. Absent headers are `undefined`. */
export interface VersionMetadata {
  /** `X-Knotrel-Instance-Id`. */
  instanceId: string | undefined;
  /** `X-Knotrel-Graph-Id` (always `default` today). */
  graphId: string | undefined;
  /** `X-Knotrel-Generation`, exact decimal string. */
  generation: string | undefined;
  /** `X-Knotrel-State-Version`, exact decimal string. */
  stateVersion: string | undefined;
  /** Decoded `X-Knotrel-Source-Position`, or the raw value if it cannot be decoded. */
  sourcePosition: string | undefined;
  /** `X-Knotrel-Sync-Status` (source-managed mode only). */
  syncStatus: SyncStatus | undefined;
  /** `ETag` replacement token. */
  etag: string | undefined;
  /** Header values that were present but malformed, by header name. */
  invalid: string[];
}

/** Minimal header accessor, satisfied by `Headers`. */
export interface HeaderSource {
  get(name: string): string | null;
}

const DECIMAL = /^[0-9]+$/;

/** Extracts and validates version metadata headers. */
export function readVersionMetadata(headers: HeaderSource): VersionMetadata {
  const invalid: string[] = [];
  const decimal = (name: string): string | undefined => {
    const value = headers.get(name);
    if (value === null) {
      return undefined;
    }
    if (!DECIMAL.test(value)) {
      invalid.push(name);
      return undefined;
    }
    return value;
  };
  const rawPosition = headers.get('x-knotrel-source-position');
  let sourcePosition: string | undefined;
  if (rawPosition !== null) {
    try {
      sourcePosition = decodeURIComponent(rawPosition);
    } catch {
      invalid.push('x-knotrel-source-position');
      sourcePosition = rawPosition;
    }
  }
  const rawSync = headers.get('x-knotrel-sync-status');
  let syncStatus: SyncStatus | undefined;
  if (rawSync !== null) {
    if ((SYNC_STATUSES as readonly string[]).includes(rawSync)) {
      syncStatus = rawSync as SyncStatus;
    } else {
      invalid.push('x-knotrel-sync-status');
    }
  }
  return {
    instanceId: headers.get('x-knotrel-instance-id') ?? undefined,
    graphId: headers.get('x-knotrel-graph-id') ?? undefined,
    generation: decimal('x-knotrel-generation'),
    stateVersion: decimal('x-knotrel-state-version'),
    sourcePosition,
    syncStatus,
    etag: headers.get('etag') ?? undefined,
    invalid,
  };
}

/** True when no version header was present at all. */
export function isEmptyMetadata(metadata: VersionMetadata): boolean {
  return (
    metadata.instanceId === undefined &&
    metadata.generation === undefined &&
    metadata.stateVersion === undefined &&
    metadata.etag === undefined
  );
}

/**
 * Compares two exact decimal revisions with arbitrary precision.
 *
 * @returns A negative number, zero or a positive number like `Array.prototype.sort`.
 */
export function compareDecimal(a: string, b: string): number {
  const x = BigInt(a);
  const y = BigInt(b);
  return x < y ? -1 : x > y ? 1 : 0;
}
