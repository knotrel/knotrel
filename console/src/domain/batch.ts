/**
 * Client-side validation of batch requests against the `POST /v1/batch`
 * contract, before anything is sent. The server remains authoritative; this
 * catches mistakes early and explains what the server would reject.
 */
import { parseLosslessJson, isJsonObject, type JsonValue } from '../api/lossless-json';
import type { Operation, OperationName } from '../api/wire';
import type { Capabilities } from './capabilities';
import { LIFECYCLE_OPERATIONS, MUTATIONS } from './capabilities';
import { checkNodeId } from './node-id';

/** Operand fields required by each operation. */
const FIELDS: Record<OperationName, readonly string[]> = {
  add_node: ['node'],
  link: ['source', 'target'],
  cut: ['source', 'target'],
  connected: ['source', 'target'],
  set_node_active: ['node', 'active'],
  set_edge_enabled: ['source', 'target', 'enabled'],
  remove_node: ['node'],
  node_state: ['node'],
  edge_state: ['source', 'target'],
};

/** One validation problem, located by JSON path. */
export interface BatchProblem {
  path: string;
  message: string;
}

/** Result of validating a batch editor's text. */
export type BatchCheck =
  | { ok: true; operations: Operation[]; mutating: boolean }
  | { ok: false; problems: BatchProblem[] };

function validateOperation(
  value: JsonValue,
  path: string,
  problems: BatchProblem[],
): Operation | undefined {
  if (!isJsonObject(value)) {
    problems.push({ path, message: 'expected an operation object' });
    return undefined;
  }
  const op = value.op;
  if (typeof op !== 'string' || !(op in FIELDS)) {
    problems.push({
      path: `${path}.op`,
      message: `unknown operation ${JSON.stringify(op ?? null)}`,
    });
    return undefined;
  }
  const fields = FIELDS[op as OperationName];
  const before = problems.length;
  for (const key of Object.keys(value)) {
    if (key !== 'op' && !fields.includes(key)) {
      problems.push({ path: `${path}.${key}`, message: `field not allowed on ${op}` });
    }
  }
  for (const field of fields) {
    const v = value[field];
    if (field === 'active' || field === 'enabled') {
      if (typeof v !== 'boolean') {
        problems.push({ path: `${path}.${field}`, message: 'expected true or false' });
      }
    } else if (typeof v !== 'string') {
      problems.push({
        path: `${path}.${field}`,
        message: 'node IDs must be JSON strings of digits (numbers lose precision)',
      });
    } else {
      const check = checkNodeId(v);
      if (!check.ok) {
        problems.push({ path: `${path}.${field}`, message: check.message });
      }
    }
  }
  if (
    (op === 'link' || op === 'cut' || op === 'edge_state' || op === 'set_edge_enabled') &&
    typeof value.source === 'string' &&
    value.source === value.target
  ) {
    problems.push({ path, message: 'source and target must differ (self-loops are rejected)' });
  }
  if (problems.length > before) {
    return undefined;
  }
  return Object.fromEntries(
    Object.entries(value).map(([k, v]) => [k, v as string | boolean]),
  ) as unknown as Operation;
}

/**
 * Parses and validates editor text: either `{"operations": [...]}` or a bare
 * array of operations.
 */
export function checkBatch(
  text: string,
  capabilities: Capabilities,
  maxOperations: number,
): BatchCheck {
  let value: JsonValue;
  try {
    value = parseLosslessJson(text);
  } catch (error) {
    return {
      ok: false,
      problems: [{ path: '$', message: error instanceof Error ? error.message : 'invalid JSON' }],
    };
  }
  let list: JsonValue[];
  if (Array.isArray(value)) {
    list = value;
  } else if (isJsonObject(value) && Array.isArray(value.operations)) {
    const extra = Object.keys(value).filter((k) => k !== 'operations');
    if (extra.length > 0) {
      return { ok: false, problems: [{ path: `$.${extra[0]}`, message: 'field not allowed' }] };
    }
    list = value.operations;
  } else {
    return {
      ok: false,
      problems: [{ path: '$', message: 'expected {"operations": [...]} or an array' }],
    };
  }
  if (list.length < 1 || list.length > maxOperations) {
    return {
      ok: false,
      problems: [{ path: '$', message: `a batch holds 1 to ${maxOperations} operations` }],
    };
  }
  const problems: BatchProblem[] = [];
  const operations: Operation[] = [];
  list.forEach((entry, index) => {
    const op = validateOperation(entry, `$[${index}]`, problems);
    if (op) {
      operations.push(op);
    }
  });
  if (problems.length > 0) {
    return { ok: false, problems };
  }
  const lifecycleOp = operations.findIndex((op) =>
    (LIFECYCLE_OPERATIONS as readonly string[]).includes(op.op),
  );
  if (!capabilities.lifecycle && lifecycleOp !== -1) {
    problems.push({
      path: `$[${lifecycleOp}]`,
      message: 'lifecycle support is disabled: the server rejects the whole batch',
    });
  }
  const mutation = operations.findIndex((op) => (MUTATIONS as readonly string[]).includes(op.op));
  if (capabilities.mode === 'source_managed' && mutation !== -1) {
    problems.push({
      path: `$[${mutation}]`,
      message:
        'source-managed server: mutations belong to the source and feeder; the server rejects the whole batch',
    });
  }
  if (problems.length > 0) {
    return { ok: false, problems };
  }
  return { ok: true, operations, mutating: mutation !== -1 };
}

/** Pretty JSON of a batch request, IDs as strings. */
export function formatBatch(operations: readonly Operation[]): string {
  return JSON.stringify({ operations }, null, 2);
}
