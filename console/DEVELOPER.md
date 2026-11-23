# Console developer guide

Short guide to working on the console. Behavior and deployment are covered in
[docs/console.md](../docs/console.md); releases in
[docs/releases/console-release.md](../docs/releases/console-release.md).

## Prerequisites

- Node.js 24.22.3 with npm 11 (`console/.nvmrc`; with nvm: `nvm use`)
- Rust 1.100.0 (pinned by `rust-toolchain.toml`) to build the server
- Docker, only for the image and the container test

## 1. Build and start a server

From the repository root:

```sh
cargo build -p knotrel-server --locked
KNOTREL_TOPOLOGY_LIFECYCLE=true ./target/debug/knotrel-server
```

The server listens on `127.0.0.1:8080`. Useful variables:
`KNOTREL_MODE=source_managed` and `KNOTREL_ENGINE=ett` or `hdt`. Lifecycle
support requires `compact-bfs`, the default engine. The full list is in
[docs/http-api.md](../docs/http-api.md#startup-configuration-and-engine-selection).
Stop the server with Ctrl+C.

## 2. Run the console with live reload

In another terminal:

```sh
cd console
npm ci
npm run dev
```

Open the printed URL (default <http://localhost:5173/>). The dev server
forwards `/api` to `http://127.0.0.1:8080`. For another server:
`KNOTREL_SERVER=http://127.0.0.1:9090 npm run dev`.

## 3. Check before pushing

```sh
npm run check        # format, lint, types, OpenAPI drift, unit tests + coverage, build
npm run test:e2e     # browser tests; needs ../target/debug/knotrel-server (step 1)
```

The first E2E run needs browsers: `npx playwright install chromium firefox`.
Fix formatting with `npm run format`.

## Common tasks

| Task                                                   | Command (in `console/`)                                                                                                                                           |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unit tests in watch mode                               | `npx vitest`                                                                                                                                                      |
| One browser only                                       | `npx playwright test --project=chromium`                                                                                                                          |
| Regenerate API types after editing `docs/openapi.yaml` | `npm run generate:api` (commit the result)                                                                                                                        |
| Production build and preview                           | `npm run build && npm run preview`                                                                                                                                |
| Static archive                                         | `npm run archive` (writes `.artifacts/`)                                                                                                                          |
| Image                                                  | `docker build -t knotrel-console:dev .`                                                                                                                           |
| Entrypoint tests (no Docker; needs `envsubst`)         | `scripts/test-entrypoint.sh`                                                                                                                                      |
| Container test                                         | `docker build -f ../examples/container/Dockerfile -t knotrel-server:example ..` then `scripts/smoke-container.sh knotrel-console:dev knotrel-server:example dist` |
| Server and console in Docker                           | `scripts/start-local.sh --build`, `scripts/seed-demo.sh` (demo data), then `scripts/stop-local.sh`                                                                |
| Documentation screenshots                              | With the stack running and the demo data loaded: `node scripts/capture-screenshots.mjs` (writes `docs/images/console/`)                                           |

## Rules worth knowing

- Keep node IDs, revisions and counts as strings; never use JavaScript
  `number` for them.
- Do not retry mutations, and do not render server text as HTML.
- Use only existing server endpoints.

The full list is in [AGENTS.md](../AGENTS.md#operational-console-console).
