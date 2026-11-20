/**
 * A query handed from the Browser to the Workbench, prefilled with exact IDs.
 */
export interface QueryPreset {
  op: 'connected' | 'node_state' | 'edge_state';
  /** First ID (source or node). */
  a: string;
  /** Second ID (target), empty for `node_state` or when not yet chosen. */
  b: string;
}
