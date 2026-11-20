# Knotrel Console

Browser console for operating one [Knotrel](https://github.com/knotrel/knotrel)
server: liveness and readiness, configuration, graph counts, identity and
version metadata, synchronization status, connectivity and lifecycle queries,
and topology operations on standalone servers.

> **Status: unreleased.** Version 0.1.0 is prepared but not published. The
> console is versioned independently of the server and supports servers
> `>=0.7.0 <0.9.0` (`knotrel.supportedServer` in `package.json`).

It is a static React application, distributed as:

- the container image `ghcr.io/knotrel/knotrel-console:<version>`: an
  unprivileged nginx that serves the console and forwards an allowlist of
  `/api` routes to one server set by `KNOTREL_UPSTREAM`;
- the static archive `knotrel-console-<version>.tar.gz`, for your own gateway.

Usage, configuration, trust boundary and API mapping:
[docs/console.md](../docs/console.md). Release procedure:
[docs/releases/console-release.md](../docs/releases/console-release.md).

## Try it locally

```sh
docker compose -f console/deploy/compose.yaml up --build   # from the repository root
```

Then open <http://127.0.0.1:7380/>.

## Development

npm is used for development only (`"private": true`).

```sh
cd console
nvm use                     # Node.js 24.22.3 (.nvmrc), npm 11
npm ci
KNOTREL_SERVER=http://127.0.0.1:8080 npm run dev   # Vite dev server; proxies /api to that server
```

| Command                                                            | Purpose                                                                                                                                  |
| ------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run check`                                                    | Formatting, ESLint, TypeScript, OpenAPI drift, unit tests with coverage gates, production build                                          |
| `npm run test:coverage`                                            | Vitest + React Testing Library. LCOV in `coverage/lcov.info`; at least 80% statements, branches, functions and lines                     |
| `npm run test:e2e`                                                 | Playwright (Chromium, Firefox) against real `knotrel-server` processes. Build the server first: `cargo build -p knotrel-server --locked` |
| `npm run generate:api`                                             | Regenerate `src/api/generated/openapi.ts` from `../docs/openapi.yaml` and record its SHA-256 in `package.json`                           |
| `npm run build`                                                    | Production bundle in `dist/`                                                                                                             |
| `npm run archive`                                                  | Reproducible `.artifacts/knotrel-console-<version>.tar.gz` and `.sha256` (`node scripts/package-static.mjs --verify <file>` checks one)  |
| `scripts/smoke-container.sh <console-image> <server-image> [dist]` | Container smoke test against a real server container                                                                                     |

Layout:

| Path                       | Content                                                                                                              |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `src/api/`                 | Lossless JSON parser, runtime decoders, transport and endpoint classification; `generated/` is produced from OpenAPI |
| `src/domain/`              | Capabilities, error and synchronization guidance, node-ID validation, mutation outcomes, history                     |
| `src/state/`               | Bounded polling and overview store                                                                                   |
| `src/ui/`                  | React pages and components                                                                                           |
| `container/`, `Dockerfile` | Image: nginx configuration templates and startup validation                                                          |
| `deploy/`                  | Compose example, nginx gateway example, archive README                                                               |
| `test/`, `e2e/`            | Unit/component tests and browser integration tests                                                                   |

## License

[Elastic License 2.0](../LICENSE.md).
