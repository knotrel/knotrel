/**
 * Owned test processes: a real knotrel-server and the built console served by
 * `vite preview`. Nothing here talks to a server the tests did not start.
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync } from 'node:fs';
import type { ServerResponse } from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { preview, type PreviewServer } from 'vite';

const consoleRoot = path.resolve(import.meta.dirname, '..');
const serverBin =
  process.env.KNOTREL_SERVER_BIN ??
  path.resolve(
    consoleRoot,
    '..',
    'target',
    'debug',
    process.platform === 'win32' ? 'knotrel-server.exe' : 'knotrel-server',
  );

/** Returns a currently free loopback port. */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address() as net.AddressInfo;
      s.close(() => resolve(port));
    });
  });
}

async function waitUntil(probe: () => Promise<boolean>, what: string): Promise<void> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (await probe().catch(() => false)) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/**
 * The test process's environment without the harness's own settings: the
 * server refuses unknown `KNOTREL_*` names, so `KNOTREL_SERVER_BIN` (which
 * selects the binary under test) must not reach it.
 */
function serverEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(process.env).filter(([name]) => name !== 'KNOTREL_SERVER_BIN'),
  );
}

/** A running knotrel-server owned by the test. */
export class KnotrelServer {
  #child: ChildProcess | undefined;
  constructor(
    readonly port: number,
    readonly env: Record<string, string>,
  ) {}

  get url(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  static async start(env: Record<string, string>, port?: number): Promise<KnotrelServer> {
    if (!existsSync(serverBin)) {
      throw new Error(
        `knotrel-server not found at ${serverBin}; run cargo build -p knotrel-server`,
      );
    }
    const server = new KnotrelServer(port ?? (await freePort()), env);
    await server.restart();
    return server;
  }

  /** Starts (again) on the same port; a restart creates a new instance. */
  async restart(): Promise<void> {
    this.#child = spawn(serverBin, [], {
      env: { ...serverEnvironment(), ...this.env, KNOTREL_ADDR: `127.0.0.1:${this.port}` },
      stdio: 'ignore',
    });
    await waitUntil(async () => (await fetch(`${this.url}/health`)).ok, 'knotrel-server');
  }

  async stop(): Promise<void> {
    const child = this.#child;
    if (!child || child.exitCode !== null) return;
    const exited = new Promise((r) => child.once('exit', r));
    child.kill('SIGKILL');
    await exited;
  }

  /** Direct call to the server (test setup only, e.g. acting as a feeder). */
  async call(
    method: 'GET' | 'POST',
    route: string,
    body?: unknown,
    headers: Record<string, string> = {},
  ): Promise<{ status: number; json: Record<string, unknown> }> {
    const res = await fetch(`${this.url}${route}`, {
      method,
      headers: body === undefined ? headers : { 'content-type': 'application/json', ...headers },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { status: res.status, json: (await res.json()) as Record<string, unknown> };
  }
}

function gatewayError(res: ServerResponse, status: number, condition: string): void {
  if (res.headersSent) {
    res.destroy();
    return;
  }
  res.writeHead(status, {
    'content-type': 'application/json',
    'x-knotrel-console-proxy': condition,
  });
  res.end(JSON.stringify({ result: 'error', code: `console_${condition}`, message: condition }));
}

/** Serves dist/ and forwards /api/* to `upstream`, like the container gateway. */
export async function startConsole(
  upstream: string,
): Promise<{ url: string; close: () => Promise<void> }> {
  const server: PreviewServer = await preview({
    root: consoleRoot,
    logLevel: 'silent',
    preview: {
      host: '127.0.0.1',
      port: await freePort(),
      strictPort: true,
      proxy: {
        '/api': {
          target: upstream,
          rewrite: (p) => p.replace(/^\/api/, ''),
          // Mirror the gateway contract for unreachable upstreams.
          configure: (proxy) => {
            proxy.on('error', (error, _req, res) => {
              const code = (error as NodeJS.ErrnoException).code;
              const notDelivered = code === 'ECONNREFUSED' || code === 'EHOSTUNREACH';
              gatewayError(
                res as ServerResponse,
                502,
                notDelivered ? 'upstream_unreachable' : 'upstream_failed',
              );
            });
          },
        },
      },
    },
  });
  const address = server.resolvedUrls?.local[0];
  if (!address) throw new Error('vite preview did not report a URL');
  return { url: address, close: () => server.close() };
}

/** Opens the connection panel, asks for an immediate refresh and closes it. */
export async function refreshNow(page: import('@playwright/test').Page): Promise<void> {
  await page.locator('.connection-button').click();
  await page.getByRole('button', { name: 'Refresh now' }).click();
  await page.keyboard.press('Escape');
}
