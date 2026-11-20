/**
 * Cursor-paged listings of the data browser.
 *
 * Pages are fetched on demand and appended up to {@link LISTING_CAP} rows; the
 * whole graph is never downloaded. Each page carries the instance and
 * generation it was read from. If they change between pages the listing
 * spans different graphs and is flagged for a restart, never silently merged.
 */
import type { Failure } from '../api/endpoints';
import type { VersionMetadata } from '../api/metadata';

/** Most rows kept in one listing. */
export const LISTING_CAP = 5000;
/** Rows requested per page. */
export const LISTING_PAGE = 100;

/** Graph identity a page was read from. */
export interface Identity {
  instanceId: string | undefined;
  generation: string | undefined;
}

/** State of one listing. `C` is the cursor type. */
export interface Listing<Row, C> {
  rows: Row[];
  /** Next cursor; `null` when complete; `undefined` before the first page. */
  next: C | null | undefined;
  status: 'idle' | 'loading' | 'ready' | 'error';
  failure: Failure | undefined;
  identity: Identity | undefined;
  /** True when a later page came from another instance or generation. */
  identityChanged: boolean;
  /** Time the latest page was received. */
  loadedAt: Date | undefined;
  pages: number;
  /** True when rows were dropped because of {@link LISTING_CAP}. */
  truncated: boolean;
}

/** A listing before its first page. */
export function emptyListing<Row, C>(): Listing<Row, C> {
  return {
    rows: [],
    next: undefined,
    status: 'idle',
    failure: undefined,
    identity: undefined,
    identityChanged: false,
    loadedAt: undefined,
    pages: 0,
    truncated: false,
  };
}

/** Marks a listing as loading. */
export function loading<Row, C>(listing: Listing<Row, C>): Listing<Row, C> {
  return { ...listing, status: 'loading', failure: undefined };
}

/** Appends one page. */
export function appendPage<Row, C>(
  listing: Listing<Row, C>,
  rows: readonly Row[],
  next: C | null,
  metadata: VersionMetadata,
  receivedAt: Date,
  cap: number = LISTING_CAP,
): Listing<Row, C> {
  const identity = { instanceId: metadata.instanceId, generation: metadata.generation };
  const changed =
    listing.identity !== undefined &&
    (listing.identity.instanceId !== identity.instanceId ||
      listing.identity.generation !== identity.generation);
  const all = [...listing.rows, ...rows];
  return {
    rows: all.slice(0, cap),
    next: all.length > cap ? null : next,
    status: 'ready',
    failure: undefined,
    identity: listing.identity ?? identity,
    identityChanged: listing.identityChanged || changed,
    loadedAt: receivedAt,
    pages: listing.pages + 1,
    truncated: listing.truncated || all.length > cap,
  };
}

/** Records a failed page; rows already loaded are kept. */
export function failPage<Row, C>(listing: Listing<Row, C>, failure: Failure): Listing<Row, C> {
  return { ...listing, status: 'error', failure };
}
