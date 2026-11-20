/**
 * Node ID validation.
 *
 * Node IDs are nonempty ASCII decimal strings representing an unsigned 64-bit
 * integer. They are validated with `BigInt` and sent exactly as entered; they are
 * never converted to a JavaScript `number`.
 */
import { U64_MAX } from '../api/decode';

/** Result of validating user input as a node ID. */
export type NodeIdCheck = { ok: true; value: string } | { ok: false; message: string };

/** Validates a node ID entered by the operator. */
export function checkNodeId(input: string): NodeIdCheck {
  if (input.length === 0) {
    return { ok: false, message: 'Enter a node ID.' };
  }
  if (!/^[0-9]+$/.test(input)) {
    return { ok: false, message: 'Use ASCII digits only (no spaces, signs or separators).' };
  }
  if (BigInt(input) > U64_MAX) {
    return { ok: false, message: 'The value exceeds 18446744073709551615 (u64 maximum).' };
  }
  return { ok: true, value: input };
}

/** True when the server will normalize the ID (it strips leading zeroes). */
export function hasLeadingZeroes(id: string): boolean {
  return id.length > 1 && id.startsWith('0');
}
