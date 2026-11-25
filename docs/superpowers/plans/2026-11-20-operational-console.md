# Operational Console (console 0.1.0) — design and plan

**Status (2026-11-20):** implemented on `feature/v0.8.0-operational-console` and
validated locally; not merged, not released, not accepted. The roadmap entry
is [Operational Console](https://github.com/knotrel/knotrel/blob/d6a273c5ffa9819cd2a6904c6a3f5e4bb74475b7/ROADMAP.md#v080--operational-console).

## Goal and bounds

The goal is a browser console for operating **one** Knotrel server through
the existing HTTP API: diagnostics in every mode, and topology operations in
standalone mode. It is a separate artifact from the server and adds no
server endpoint, engine change or authentication.

Out of scope: graph visualization or maps, path reconstruction, multi-server
management, bulk import, log ingestion, historical metrics, server
authentication, persistence, multi-graph and new engines. Performance
qualification does not apply: the console changes no server code path.

The milestone is separate from server correctness, feeder fairness and the
v1.0 requirements. It needs no server release: it supports the published
v0.7.0 server.

## Decisions

| Topic | Decision | Reason |
| --- | --- | --- |
| Location | `console/` in the monorepo, outside the Cargo workspace | API, OpenAPI and console change in one PR; integration tests coordinate easily. A dedicated repository remains possible later. |
| Versioning | Independent SemVer, starting at 0.1.0; tags `console-v*`; compatibility declared in `package.json` | No server release is needed; versions do not imply lockstep. |
| Distribution | Container image on GHCR (single registry) and a reproducible static archive on the GitHub Release; `latest` for stable versions only | Hosting a web UI needs a web server; operators either run the image or use their own gateway. |
| npm | Development tooling only (`private: true`) | An `npx` launcher adds a Node server and proxy to maintain. A tested draft is kept on the local branch `feature/console-npx-launcher` for a possible later increment. |
| Stack | React 19, TypeScript 5.9 (strict), Vite 8, Vitest 5 + React Testing Library, Playwright 1.65, ESLint 10, Prettier 3; Node 24.22.3 LTS, npm 11 | Small static build, current supported toolchain. TypeScript 5.9 because openapi-typescript and typescript-eslint do not yet support 6.x/7.x. |
| API types | openapi-typescript 7.13 generates types from `docs/openapi.yaml`; a drift check and an OpenAPI digest in `package.json` | One wire-contract source. Handwritten runtime decoders validate responses. |
| Numbers | A lossless JSON parser keeps every number's source text | `generation` and counts are JSON u64; `JSON.parse` rounds above 2^53. |
| Gateway | nginx (unprivileged) with a route allowlist, `Host` and `Origin`/`Sec-Fetch-Site` checks, a 64 KiB body limit, bounded timeouts and `X-Knotrel-Console-Proxy` on gateway-generated errors | Prevents DNS rebinding, cross-site request forgery and exposure of feeder routes; keeps "not delivered" separate from "uncertain". |
| API base | Relative `api/` | One build works at any path; no rebuild per installation. |

## API findings (no server change)

The console needed no new endpoint. Writing it found specification gaps,
corrected in `docs/openapi.yaml`, `docs/http-api.md` and
`tests/test_openapi_contract.py` without changing server behavior:

- `GET /v1/info` returns `mode` and `instance_id` in source-managed mode
  (undocumented). `http-api.md` claimed it reports a server version; it does not.
- `GET /v1/stats` returns `instance_id`, `generation` (JSON u64),
  `source_position` and `sync_status` in source-managed mode (undocumented).
- `/ready` 503 also reports `uninitialized`, `loading`, `catching_up`, `stale`
  and `desynchronized` (undocumented).
- `/v1/stats` and `/v1/operations` responses carry the version headers
  (documented only for `/v1/batch`).
- The `Operation` and `Outcome` discriminators lacked `mapping`. OpenAPI then
  implies the schema names (`AddNodeOperation`) as wire values.

Remaining gaps, reported but not implemented (they would be server API
changes): no server software version, no server execution time, and no
source lag or lease information outside `/metrics`.

## Increments

1. Read-only console: Overview, Graph/Queries, Synchronization. Done.
2. Standalone operations, uncertain outcomes with read-back, and the
   session history. Done.
3. Container image, gateway configuration, Compose example and container smoke
   test. Done.
4. Static archive (reproducible, with checksums) and the release workflow
   (dry run by default). Done; no publication performed.
5. CI: console workflow with a stable aggregate check, Codecov flag
   `console`, one combined SonarCloud scan, Dependabot. Done; external
   results pending.

## Acceptance (pending maintainer)

- [ ] Console CI green on the PR; Sonar quality gate passed in authorized CI.
- [ ] Dry run of `console-release` passed on the candidate commit.
- [ ] Maintainer setup done (environment, package visibility after the first push).
- [ ] RC published and checked; stable released.
