/**
 * Lossless JSON parsing.
 *
 * `JSON.parse` converts every number to an IEEE-754 double, which silently rounds
 * integers above 2^53 (for example a `generation` near 2^64). This parser keeps the
 * exact source text of every number in a {@link JsonNumber}; semantic decoders then
 * validate it as a decimal string or `BigInt`, never as a JavaScript `number`.
 *
 * The grammar is RFC 8259. Parsing is a single left-to-right pass: O(n) time and
 * O(n) space in the input length, with recursion depth bounded by
 * {@link MAX_DEPTH} so hostile nesting cannot exhaust the call stack.
 */

/** A JSON number preserved as its exact source text. */
export class JsonNumber {
  /** Exact RFC 8259 number text, for example `"18446744073709551615"`. */
  readonly raw: string;

  constructor(raw: string) {
    this.raw = raw;
  }

  /** True when the number is a nonnegative integer without fraction or exponent. */
  isUnsignedInteger(): boolean {
    return /^(0|[1-9][0-9]*)$/.test(this.raw);
  }
}

/** A parsed JSON value whose numbers are kept lossless. */
export type JsonValue = null | boolean | string | JsonNumber | JsonValue[] | JsonObject;

/** A parsed JSON object. Keys are own properties of a prototype-less object. */
export interface JsonObject {
  [key: string]: JsonValue;
}

/** Raised when the input is not valid JSON. */
export class JsonSyntaxError extends Error {
  override readonly name = 'JsonSyntaxError';
}

/** Maximum nesting of arrays and objects accepted by {@link parseLosslessJson}. */
export const MAX_DEPTH = 64;

const ESCAPES: Record<string, string> = {
  '"': '"',
  '\\': '\\',
  '/': '/',
  b: '\b',
  f: '\f',
  n: '\n',
  r: '\r',
  t: '\t',
};

const NUMBER = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/y;

/**
 * Parses `text` as one JSON value, keeping numbers as {@link JsonNumber}.
 *
 * Duplicate object keys are rejected, because the server never emits them and
 * accepting them would make the decoded value ambiguous.
 *
 * @throws {JsonSyntaxError} When `text` is not exactly one valid JSON value.
 */
export function parseLosslessJson(text: string): JsonValue {
  let index = 0;

  const fail = (what: string): never => {
    throw new JsonSyntaxError(`invalid JSON at offset ${index}: ${what}`);
  };

  const skipWhitespace = (): void => {
    while (index < text.length) {
      const c = text[index];
      if (c === ' ' || c === '\t' || c === '\n' || c === '\r') {
        index += 1;
      } else {
        break;
      }
    }
  };

  const parseString = (): string => {
    // Precondition: text[index] === '"'.
    index += 1;
    let out = '';
    while (index < text.length) {
      const c = text.charAt(index);
      if (c === '"') {
        index += 1;
        return out;
      }
      if (c === '\\') {
        const e = text.charAt(index + 1);
        if (e === 'u') {
          const hex = text.slice(index + 2, index + 6);
          if (!/^[0-9a-fA-F]{4}$/.test(hex)) {
            fail('bad unicode escape');
          }
          out += String.fromCharCode(Number.parseInt(hex, 16));
          index += 6;
        } else {
          const mapped = ESCAPES[e];
          if (mapped === undefined) {
            fail('bad escape');
          }
          out += mapped;
          index += 2;
        }
        continue;
      }
      if (c < ' ') {
        fail('control character in string');
      }
      out += c;
      index += 1;
    }
    return fail('unterminated string');
  };

  const parseValue = (depth: number): JsonValue => {
    if (depth > MAX_DEPTH) {
      fail('nesting too deep');
    }
    skipWhitespace();
    const c = text[index];
    if (c === '{') {
      index += 1;
      const object: JsonObject = Object.create(null) as JsonObject;
      skipWhitespace();
      if (text[index] === '}') {
        index += 1;
        return object;
      }
      for (;;) {
        skipWhitespace();
        if (text[index] !== '"') {
          fail('expected object key');
        }
        const key = parseString();
        if (Object.prototype.hasOwnProperty.call(object, key)) {
          fail('duplicate object key');
        }
        skipWhitespace();
        if (text[index] !== ':') {
          fail('expected colon');
        }
        index += 1;
        object[key] = parseValue(depth + 1);
        skipWhitespace();
        if (text[index] === ',') {
          index += 1;
        } else if (text[index] === '}') {
          index += 1;
          return object;
        } else {
          fail('expected comma or closing brace');
        }
      }
    }
    if (c === '[') {
      index += 1;
      const array: JsonValue[] = [];
      skipWhitespace();
      if (text[index] === ']') {
        index += 1;
        return array;
      }
      for (;;) {
        array.push(parseValue(depth + 1));
        skipWhitespace();
        if (text[index] === ',') {
          index += 1;
        } else if (text[index] === ']') {
          index += 1;
          return array;
        } else {
          fail('expected comma or closing bracket');
        }
      }
    }
    if (c === '"') {
      return parseString();
    }
    if (text.startsWith('true', index)) {
      index += 4;
      return true;
    }
    if (text.startsWith('false', index)) {
      index += 5;
      return false;
    }
    if (text.startsWith('null', index)) {
      index += 4;
      return null;
    }
    NUMBER.lastIndex = index;
    const match = NUMBER.exec(text);
    if (match) {
      index += match[0].length;
      return new JsonNumber(match[0]);
    }
    return fail('unexpected token');
  };

  const value = parseValue(0);
  skipWhitespace();
  if (index !== text.length) {
    fail('trailing characters');
  }
  return value;
}

/** True when `value` is a parsed JSON object (not an array or null). */
export function isJsonObject(value: JsonValue | undefined): value is JsonObject {
  return (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof JsonNumber)
  );
}
