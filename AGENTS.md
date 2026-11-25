# Development requirements

- Use Rust 1.100.0, edition 2024, and the existing Cargo workspace. Preserve
  the `knotrel-core` MSRV of Rust 1.98.1 and verify it separately.
- Keep graph algorithms synchronous in `knotrel-core`; HTTP belongs in `knotrel-server` and uses Axum.
- Write code, comments, Rustdoc and project documentation in English.
- Document every public item, including structs, enums, variants, fields, traits, functions and methods. Explain errors and provide examples where useful.
- Document nontrivial private algorithms: invariants, correctness, time/space complexity, and whether bounds are worst-case, expected or amortized. Cite sources when implementing published algorithms.
- Inherit workspace lints in every crate. Do not suppress missing documentation or introduce unsafe code.
- Preserve Cargo.lock. Use stable Rust features and minimal dependencies.
- Verify formatting, Clippy with warnings denied, workspace tests/doctests, and Rustdoc with warnings denied.
- Every server HTTP API change must update `docs/openapi.yaml` in the same
  change: routes, methods, request/response schemas, headers, status/error codes,
  defaults, limits and conditional behavior. Keep it aligned with the
  implementation and `docs/http-api.md`; Markdown documentation alone is not
  sufficient. Validate the specification and relevant API contract tests before
  considering the change complete.
- Keep benchmarks reproducible. Never present unmeasured performance claims or compare different consistency/result contracts.
- Do not commit or push unless the user explicitly asks for it.

## Operational console (`console/`)

- Keep the console outside the Cargo workspace. Cargo builds, server
  installation and the Rust binary must never require Node.js or embed console
  assets. The console version (`console/package.json`; no separate tag, it ships
  under the release tag `v*`) equals
  the Rust workspace version (lockstep: console, server and core share one
  number, see `CONTRIBUTING.md`), and the console is released only through
  [the console release procedure](docs/releases/console-release.md).
- Toolchain: Node 24.22.3 LTS (`console/.nvmrc`) with npm 11; exact dependency
  versions and the committed `package-lock.json`. npm is development tooling
  only (`"private": true`); do not add runtime dependencies to the container
  image, install scripts or an npm publication without an explicit decision.
- `docs/openapi.yaml` is the wire contract. After changing it, run
  `npm run generate:api` in `console/` and commit the regenerated
  `src/api/generated/openapi.ts` and the `knotrel.openapiSha256` digest
  (`npm run check:api` fails otherwise). Update `knotrel.supportedServer` and
  `docs/console.md` when compatibility changes. Generated types do not validate
  responses: extend the runtime decoders in `src/api/decode.ts` too.
- Never convert node IDs, revisions, generations or counts to a JavaScript
  `number`; parse JSON with `src/api/lossless-json.ts`. Render server text as
  text, never HTML. Never retry mutations automatically.
- Use only existing HTTP endpoints. Report a missing capability instead of
  adding a server endpoint for the console.
- The container gateway forwards only `GET /health`, `GET /ready`,
  `GET /v1/info`, `GET /v1/stats`, `POST /v1/operations`, `POST /v1/batch`,
  `GET /metrics` and the read-only inspection routes (`GET /v1/nodes`,
  `/v1/nodes/{id}`, `/v1/nodes/{id}/edges`, `/v1/edges`). Feeder routes are never
  forwarded. Changing that list, the `Host`/`Origin` checks or the upstream
  configuration is a security change: update `docs/console.md`,
  `console/deploy/nginx-gateway.conf` and `console/scripts/smoke-container.sh`.
- Inspection routes must stay read-only, bounded per page (scan budget) and
  free of graph-proportional work per request. A change to their cost or to the
  node index updates `docs/console-inspection-costs.md` with a new measurement
  against criteria declared beforehand.
- Verify in `console/`: `npm run check` (formatting, ESLint, TypeScript,
  OpenAPI drift, unit tests with at least 80% statements, branches, functions
  and lines, production build), `npm run test:e2e` against a freshly built
  `knotrel-server`, and for container or gateway changes
  `scripts/smoke-container.sh`. Do not exclude handwritten logic from coverage.

## Before release

Follow [the release procedure](docs/releases/release-procedure.md); this list
covers what is easy to miss. Check every item on the exact candidate commit.

- **Crate docs (crates.io and docs.rs):** for every item under "What is new"
  in the release notes, update:
  - `crates/*/src/lib.rs` crate-level `//!` docs (the docs.rs landing page),
    with a doctest where an example helps;
  - `crates/*/README.md` (the crates.io page);
  - the main `README.md`, `docs/http-api.md` and `docs/openapi.yaml` for any
    new public type, `KNOTREL_*` variable or HTTP operation.
  Mark unreleased features as such until publication, and never link to
  release assets that do not exist yet.
- **Preview what will be published:** `cargo package --list -p <crate>` and
  `cargo doc --no-deps -p <crate>`; run `python3 scripts/check-crate-docs.py`
  (it checks version references only, not coverage).
- **Versions:** workspace version, the `knotrel-core` dependency version,
  `Cargo.lock` and `console/package.json` (with its lockfile) agree with the
  release (lockstep, see `CONTRIBUTING.md`).
- **Console compatibility (mandatory, from v0.9.0):** run the console's
  browser tests against a server built from the release candidate and against
  the previous release's server; only then set `knotrel.supportedServer` and
  the statement in `docs/console.md` to the versions verified, never wider
  ([compatibility policy](docs/compatibility.md#5-console-and-server-compatibility)).
  Record the result in the release notes.
- **Public API snapshot:** the `public API` CI job passes (snapshot equal to
  the code, every public item classified, `docs/stability.md` consistent).
- **API compatibility:** the `API compatibility` CI job passes against the
  previous release (public Rust API and OpenAPI); any approved breaking
  change follows the deprecation rule of `docs/compatibility.md` and is listed
  in the release notes.
- **Documentation website (`docs/index.md`, `docs/architecture-site.md`):**
  update it for the release like the README: release status, what is
  available versus planned, links to the new release notes and to new pages.
  Check that it builds (`python3 scripts/prepare-docs-site.py <dir>`, then the
  `Documentation site` workflow) and that new pages are linked from it.
- **User-facing performance pages:** add the release to
  `docs/performance-by-version.md` and the README performance section, with
  no claim beyond the qualification and its maintainer decisions.
- **Release notes:** the "Outstanding release gates" section reflects the
  current state. The last change before the tag sets the status to
  `**Status: released <date>**`: the GitHub Release body is taken from the
  notes at the tag, and `scripts/check-release-notes.sh` (run by the release
  workflow, dry runs included) rejects a draft status.
- **Evidence:** release code of the final commit identical to the measured
  candidate (or re-measured); raw evidence stored with checksums.
- **CI and dry run:** all CI on all three platforms and the release dry run
  passed on the final commit; the deployment and rebuild smoke test repeated.
- **Tags:** signed tag (`git tag -s`, `git verify-tag`) in knotrel and the
  matching tag in knotrel-benchmarks.
- **After publication:** remove "unreleased" labels, update installation
  examples and the roadmap.

## Git & Naming Rulesets

- **Branch Naming**: All branches must follow the pattern:
  `^(feature|bugfix|fix|hotfix|docs|chore|refactor|test|ci|dependabot)/.+$`
  Allowed prefixes: `feature/`, `bugfix/`, `fix/`, `hotfix/`, `docs/`, `chore/`, `refactor/`, `test/`, `ci/`, `dependabot/`.
- **Commit Messages**: All commits and PR titles must follow Conventional Commits matching:
  `^(build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test)(\([a-z0-9_./-]+\))?!?: .+$`
  Examples: `feat(core): add pruning`, `fix(server): handle zero capacity`, `chore: update dependencies`.

## Planning

Track future work in GitHub issues with a version label and priority label. https://github.com/knotrel/knotrel/issues/183 records coverage and dependencies. Do not create a parallel planning file; retain historical measurement provenance unchanged.
