// @ts-check
/**
 * Builds the versioned static archive of the console, or verifies one.
 *
 *   node scripts/package-static.mjs [--out <dir>] [--revision <git sha>] [--source-date-epoch <s>]
 *   node scripts/package-static.mjs --verify <archive.tar.gz>
 *
 * Output: <out>/knotrel-console-<version>.tar.gz and a `.sha256` file next to it.
 * Layout inside the archive (single top-level directory):
 *
 *   knotrel-console-<version>/
 *     html/                     the built console (serve this directory)
 *     nginx/knotrel-console.conf  example same-origin gateway configuration
 *     README.md                 installation notes
 *     LICENSE.md                Elastic License 2.0
 *     console-manifest.json     version, source revision, OpenAPI digest, compatibility
 *     SHA256SUMS                checksums of every other file
 *
 * The archive is reproducible: entries are sorted by UTF-16 code unit (not by
 * locale), owners are 0, modes are fixed, every timestamp is
 * --source-date-epoch or SOURCE_DATE_EPOCH (default 0) and the gzip header
 * carries no time or OS. Building twice from the same `dist/` yields identical
 * bytes. The revision comes from --revision or GITHUB_SHA (default "unknown");
 * the script spawns no processes.
 */
import { createHash } from 'node:crypto';
import { mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { gunzipSync, gzipSync } from 'node:zlib';

const root = fileURLToPath(new URL('..', import.meta.url));
const arg = (/** @type {string} */ name) => {
  const i = process.argv.indexOf(name);
  return i === -1 ? undefined : process.argv[i + 1];
};
const sha256 = (/** @type {Buffer} */ data) => createHash('sha256').update(data).digest('hex');

/**
 * Orders strings by UTF-16 code unit. Deliberately not `localeCompare`: locale
 * collation varies between systems and ICU versions, which would make the
 * archive differ between platforms.
 * @param {string} a
 * @param {string} b
 */
export const byCodeUnit = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * Encodes one ustar header block (POSIX.1-1988).
 * @param {string} name Entry path (directories end with '/').
 * @param {number} size File size (0 for directories).
 * @param {number} mtime Seconds since the epoch.
 * @param {boolean} directory Entry kind.
 */
function header(name, size, mtime, directory) {
  const block = Buffer.alloc(512, 0);
  let prefix = '';
  let base = name;
  if (Buffer.byteLength(name) > 100) {
    const cut = name.lastIndexOf('/', name.length - 2);
    prefix = name.slice(0, cut);
    base = name.slice(cut + 1);
    if (Buffer.byteLength(base) > 100 || Buffer.byteLength(prefix) > 155) {
      throw new Error(`path too long for ustar: ${name}`);
    }
  }
  const put = (
    /** @type {string} */ text,
    /** @type {number} */ offset,
    /** @type {number} */ length,
  ) => block.write(text, offset, length, 'utf8');
  const octal = (/** @type {number} */ value, /** @type {number} */ length) =>
    `${value.toString(8).padStart(length - 1, '0')}\0`;
  put(base, 0, 100);
  put(octal(directory ? 0o755 : 0o644, 8), 100, 8);
  put(octal(0, 8), 108, 8);
  put(octal(0, 8), 116, 8);
  put(octal(size, 12), 124, 12);
  put(octal(mtime, 12), 136, 12);
  put('        ', 148, 8);
  put(directory ? '5' : '0', 156, 1);
  put('ustar\0', 257, 6);
  put('00', 263, 2);
  put(prefix, 345, 155);
  let sum = 0;
  for (const byte of block) sum += byte;
  put(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8);
  return block;
}

/**
 * Creates a deterministic gzip-compressed tar archive.
 * @param {{ name: string, data?: Buffer }[]} entries Files (with data) and directories.
 * @param {number} mtime Timestamp for every entry.
 */
export function createTarGz(entries, mtime) {
  /** @type {Buffer[]} */
  const parts = [];
  for (const entry of [...entries].sort((a, b) => byCodeUnit(a.name, b.name))) {
    const directory = entry.data === undefined;
    const size = entry.data?.length ?? 0;
    parts.push(header(entry.name, size, mtime, directory));
    if (entry.data) {
      parts.push(entry.data, Buffer.alloc((512 - (size % 512)) % 512, 0));
    }
  }
  parts.push(Buffer.alloc(1024, 0));
  const gz = gzipSync(Buffer.concat(parts), { level: 9 });
  // RFC 1952: MTIME (bytes 4-7) is already 0; set OS (byte 9) to 255 "unknown"
  // so archives built on different platforms are byte-identical.
  gz[9] = 255;
  return gz;
}

/**
 * Lists the regular files of a gzip-compressed ustar archive.
 * @param {Buffer} archive
 * @returns {Map<string, Buffer>}
 */
export function readTarGz(archive) {
  const tar = gunzipSync(archive);
  /** @type {Map<string, Buffer>} */
  const files = new Map();
  let offset = 0;
  while (offset + 512 <= tar.length) {
    const block = tar.subarray(offset, offset + 512);
    if (block.every((b) => b === 0)) break;
    const text = (/** @type {number} */ start, /** @type {number} */ length) => {
      // Fields are NUL-terminated (or fill their whole width).
      const field = block.subarray(start, start + length);
      const end = field.indexOf(0);
      return field.subarray(0, end === -1 ? field.length : end).toString('utf8');
    };
    const prefix = text(345, 155);
    const name = prefix ? `${prefix}/${text(0, 100)}` : text(0, 100);
    const size = Number.parseInt(text(124, 12).trim() || '0', 8);
    const type = text(156, 1);
    offset += 512;
    if (type === '0' || type === '') {
      files.set(name, Buffer.from(tar.subarray(offset, offset + size)));
    }
    offset += Math.ceil(size / 512) * 512;
  }
  return files;
}

/** Recursively lists files under `dir`, as POSIX paths relative to it. */
function walk(/** @type {string} */ dir, /** @type {string} */ rel = '') {
  /** @type {string[]} */
  const out = [];
  for (const name of readdirSync(path.join(dir, rel)).sort(byCodeUnit)) {
    const child = rel ? `${rel}/${name}` : name;
    if (statSync(path.join(dir, child)).isDirectory()) out.push(...walk(dir, child));
    else out.push(child);
  }
  return out;
}

function verify(/** @type {string} */ file) {
  const files = readTarGz(readFileSync(file));
  const sumsName = [...files.keys()].find((n) => n.endsWith('/SHA256SUMS'));
  if (!sumsName) throw new Error('SHA256SUMS missing');
  const top = sumsName.slice(0, -'SHA256SUMS'.length);
  const listed = new Set();
  for (const line of /** @type {Buffer} */ (files.get(sumsName))
    .toString('utf8')
    .trim()
    .split('\n')) {
    const [digest, name] = line.split(/ {2}/);
    const data = files.get(`${top}${name}`);
    if (!data) throw new Error(`listed file missing: ${name}`);
    if (sha256(data) !== digest) throw new Error(`checksum mismatch: ${name}`);
    listed.add(`${top}${name}`);
  }
  for (const name of files.keys()) {
    if (name !== sumsName && !listed.has(name)) throw new Error(`unlisted file: ${name}`);
  }
  if (!files.has(`${top}html/index.html`)) throw new Error('html/index.html missing');
  console.log(`${path.basename(file)}: ${listed.size} files verified against SHA256SUMS`);
}

function build() {
  const manifest = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
  const version = manifest.version;
  const dist = path.join(root, 'dist');
  const htmlFiles = walk(dist);
  if (!htmlFiles.includes('index.html'))
    throw new Error('dist/index.html missing: run npm run build');

  const revision = arg('--revision') ?? process.env.GITHUB_SHA;
  const epoch = arg('--source-date-epoch') ?? process.env.SOURCE_DATE_EPOCH;
  const mtime = Number(epoch ?? '0');
  if (!Number.isSafeInteger(mtime) || mtime < 0)
    throw new Error(`invalid source date epoch: ${epoch}`);

  const top = `knotrel-console-${version}/`;
  /** @type {Map<string, Buffer>} */
  const files = new Map();
  for (const rel of htmlFiles) files.set(`html/${rel}`, readFileSync(path.join(dist, rel)));
  // Text files from the repository are normalized to LF: a Windows checkout may
  // convert them to CRLF, which would make the archive platform-dependent.
  const text = (/** @type {string} */ file) =>
    Buffer.from(readFileSync(file, 'utf8').replace(/\r\n/g, '\n'), 'utf8');
  files.set('nginx/knotrel-console.conf', text(path.join(root, 'deploy', 'nginx-gateway.conf')));
  files.set('README.md', text(path.join(root, 'deploy', 'ARCHIVE-README.md')));
  files.set('LICENSE.md', text(path.join(root, '..', 'LICENSE.md')));
  files.set(
    'console-manifest.json',
    Buffer.from(
      `${JSON.stringify(
        {
          name: 'knotrel-console',
          version,
          revision: revision ?? 'unknown',
          openapiSha256: manifest.knotrel.openapiSha256,
          supportedServer: manifest.knotrel.supportedServer,
          apiPrefix: 'api/',
        },
        null,
        2,
      )}\n`,
    ),
  );
  const sums = [...files.keys()]
    .sort(byCodeUnit)
    .map((name) => `${sha256(/** @type {Buffer} */ (files.get(name)))}  ${name}`)
    .join('\n');
  files.set('SHA256SUMS', Buffer.from(`${sums}\n`));

  /** @type {{ name: string, data?: Buffer }[]} */
  const entries = [];
  const dirs = new Set([top]);
  for (const [name, data] of files) {
    const parts = name.split('/');
    for (let i = 1; i < parts.length; i += 1) dirs.add(`${top}${parts.slice(0, i).join('/')}/`);
    entries.push({ name: `${top}${name}`, data });
  }
  for (const dir of dirs) entries.push({ name: dir });

  const outDir = path.resolve(arg('--out') ?? path.join(root, '.artifacts'));
  mkdirSync(outDir, { recursive: true });
  const archiveName = `knotrel-console-${version}.tar.gz`;
  const archive = createTarGz(entries, mtime);
  writeFileSync(path.join(outDir, archiveName), archive);
  const digest = sha256(archive);
  writeFileSync(path.join(outDir, `${archiveName}.sha256`), `${digest}  ${archiveName}\n`);
  console.log(
    `${archiveName}\n  sha256 ${digest}\n  ${files.size} files, revision ${revision ?? 'unknown'}, mtime ${mtime}`,
  );
}

if (process.argv.includes('--verify')) {
  verify(path.resolve(arg('--verify') ?? ''));
} else {
  build();
}
