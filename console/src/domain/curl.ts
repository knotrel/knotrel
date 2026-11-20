/**
 * Equivalent `curl` commands for console requests. They name the console's
 * own origin (the gateway), never the upstream server, and contain no
 * credentials: the console holds none, and cookies are never included.
 */

function shellQuote(text: string): string {
  return `'${text.replace(/'/g, `'\\''`)}'`;
}

/**
 * Builds a `curl` command for `path` (relative to the API base) against the
 * console origin `base` (for example `https://console.example/` or the current
 * page URL).
 */
export function curlCommand(base: string, path: string, body?: unknown): string {
  const url = new URL(`api/${path.replace(/^\//, '')}`, base);
  url.hash = '';
  const parts = ['curl', '-sS'];
  if (body !== undefined) {
    parts.push(
      '-X',
      'POST',
      '-H',
      shellQuote('content-type: application/json'),
      '--data-raw',
      shellQuote(JSON.stringify(body)),
    );
  }
  parts.push(shellQuote(url.toString()));
  return parts.join(' ');
}
