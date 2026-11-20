/**
 * Prometheus text exposition (format 0.0.4) parsing and window statistics.
 *
 * Only what the Performance view needs: samples with labels, counter rates
 * that survive resets, gauges, and quantiles estimated from histogram bucket
 * deltas. Metric values are measurements, not identifiers, so they are kept
 * as JavaScript numbers.
 */

/** One sample line. */
export interface MetricSample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

/** Parses exposition text. Malformed lines are skipped and counted. */
export function parseExposition(text: string): { samples: MetricSample[]; skipped: number } {
  const samples: MetricSample[] = [];
  let skipped = 0;
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim();
    if (line === '' || line.startsWith('#')) {
      continue;
    }
    const sample = parseLine(line);
    if (sample) {
      samples.push(sample);
    } else {
      skipped += 1;
    }
  }
  return { samples, skipped };
}

function parseValue(text: string): number | undefined {
  if (text === '+Inf') return Infinity;
  if (text === '-Inf') return -Infinity;
  if (text === 'NaN') return NaN;
  if (!/^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(text)) return undefined;
  return Number(text);
}

function parseLine(line: string): MetricSample | undefined {
  const nameMatch = /^[a-zA-Z_:][a-zA-Z0-9_:]*/.exec(line);
  if (!nameMatch) return undefined;
  const name = nameMatch[0];
  let rest = line.slice(name.length);
  const labels: Record<string, string> = {};
  if (rest.startsWith('{')) {
    let i = 1;
    while (i < rest.length && rest[i] !== '}') {
      const key = /^[a-zA-Z_][a-zA-Z0-9_]*/.exec(rest.slice(i));
      if (!key || rest[i + key[0].length] !== '=' || rest[i + key[0].length + 1] !== '"') {
        return undefined;
      }
      i += key[0].length + 2;
      let value = '';
      while (i < rest.length && rest[i] !== '"') {
        if (rest[i] === '\\') {
          const next = rest[i + 1];
          value += next === 'n' ? '\n' : (next ?? '');
          i += 2;
        } else {
          value += rest[i];
          i += 1;
        }
      }
      if (rest[i] !== '"') return undefined;
      labels[key[0]] = value;
      i += 1;
      if (rest[i] === ',') i += 1;
    }
    if (rest[i] !== '}') return undefined;
    rest = rest.slice(i + 1);
  }
  const fields = rest.trim().split(/\s+/);
  const value = fields[0] === undefined ? undefined : parseValue(fields[0]);
  if (value === undefined || fields.length > 2) return undefined;
  return { name, labels, value };
}

/** A scrape kept in the session window. */
export interface Scrape {
  /** Local time of the scrape, in milliseconds since the epoch. */
  at: number;
  samples: MetricSample[];
}

/** Selects samples by name and an optional label subset. */
export function select(
  scrape: Scrape,
  name: string,
  labels: Record<string, string> = {},
): MetricSample[] {
  return scrape.samples.filter(
    (s) => s.name === name && Object.entries(labels).every(([k, v]) => s.labels[k] === v),
  );
}

function seriesKey(sample: MetricSample): string {
  return Object.keys(sample.labels)
    .sort()
    .map((k) => `${k}=${sample.labels[k]}`)
    .join(',');
}

/** Sum of a gauge over matching series in the latest scrape, or `undefined`. */
export function gauge(
  scrape: Scrape | undefined,
  name: string,
  labels: Record<string, string> = {},
): number | undefined {
  if (!scrape) return undefined;
  const matching = select(scrape, name, labels);
  return matching.length === 0 ? undefined : matching.reduce((sum, s) => sum + s.value, 0);
}

/** Result of a counter increase over a window. */
export interface Increase {
  /** Total increase across matching series, reset-corrected. */
  value: number;
  /** Seconds between the two scrapes. */
  seconds: number;
  /** True when at least one series decreased (process restart or reset). */
  reset: boolean;
}

/**
 * Increase of matching counter series between `from` and `to`. A series that
 * decreased is treated as reset: its increase is its current value (the
 * Prometheus convention). Series absent in `from` count from zero.
 */
export function increase(
  from: Scrape,
  to: Scrape,
  name: string,
  labels: Record<string, string> = {},
): Increase | undefined {
  const seconds = (to.at - from.at) / 1000;
  if (seconds <= 0) return undefined;
  const before = new Map(select(from, name, labels).map((s) => [seriesKey(s), s.value]));
  const after = select(to, name, labels);
  if (after.length === 0) return undefined;
  let value = 0;
  let reset = false;
  for (const sample of after) {
    const previous = before.get(seriesKey(sample)) ?? 0;
    if (sample.value < previous) {
      reset = true;
      value += sample.value;
    } else {
      value += sample.value - previous;
    }
  }
  return { value, seconds, reset };
}

/**
 * Quantile estimated from histogram bucket deltas between two scrapes, with
 * linear interpolation inside the bucket (as Prometheus `histogram_quantile`).
 * Returns `undefined` without observations in the window or after a reset.
 */
export function histogramQuantile(
  from: Scrape,
  to: Scrape,
  family: string,
  labels: Record<string, string>,
  q: number,
): { value: number; count: number } | undefined {
  const bucketName = `${family}_bucket`;
  const sumBy = (scrape: Scrape) => {
    const totals = new Map<number, number>();
    for (const s of select(scrape, bucketName, labels)) {
      const le = s.labels.le === '+Inf' ? Infinity : Number(s.labels.le);
      totals.set(le, (totals.get(le) ?? 0) + s.value);
    }
    return totals;
  };
  const before = sumBy(from);
  const after = sumBy(to);
  const bounds = [...after.keys()].sort((a, b) => a - b);
  if (bounds.length === 0) return undefined;
  const deltas = bounds.map((le) => (after.get(le) ?? 0) - (before.get(le) ?? 0));
  if (deltas.some((d) => d < 0)) return undefined; // reset inside the window
  const total = deltas[deltas.length - 1] ?? 0;
  if (total <= 0) return undefined;
  const rank = q * total;
  let previousBound = 0;
  let previousCount = 0;
  for (let i = 0; i < bounds.length; i += 1) {
    const bound = bounds[i] as number;
    const count = deltas[i] as number;
    if (count >= rank) {
      if (bound === Infinity) {
        return { value: previousBound, count: total };
      }
      const inBucket = count - previousCount;
      const fraction = inBucket > 0 ? (rank - previousCount) / inBucket : 1;
      return { value: previousBound + (bound - previousBound) * fraction, count: total };
    }
    previousBound = bound;
    previousCount = count;
  }
  return undefined;
}
