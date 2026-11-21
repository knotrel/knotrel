# Compatibility and deprecation policy

**Status: PROPOSED for v0.9.0 (roadmap item 0.9.4), awaiting maintainer
approval.** Until it is approved, nothing here binds a release. It implements
readiness criterion 2 of the [v1.0 assessment](releases/v1.0-readiness.md).
Which surfaces are stable is listed in [stability.md](stability.md).

## 1. Versions

- **One version for everything (lockstep):** `knotrel-core`, `knotrel-server`
  and the console share the version number and the tag `v<version>`
  ([CONTRIBUTING.md](../CONTRIBUTING.md#versioning)).
- **Semantic Versioning**, with Cargo's rule for `0.x`: before 1.0, the minor
  number is the compatibility boundary. A change that is breaking for a
  **stable** surface requires at least a minor version (`0.9` → `0.10`), and
  is allowed only after the deprecation of section 4. Patch releases contain
  no breaking change to stable surfaces.
- From 1.0, a breaking change to a stable surface requires a major version.

## 2. Stability classes

| Class | Promise | May change |
| --- | --- | --- |
| **Stable** | Kept compatible as defined below; removed only after deprecation (section 4) | Additively |
| **Experimental** | Documented and supported, but may change or be removed in any minor release; each such change is listed in the release notes | Anytime in a minor release |
| **Internal** | No promise; not documented as an interface (test helpers, log text, help strings, file layouts) | Anytime |

New surfaces may start as experimental. Promotion to stable is recorded in
the release notes and in [stability.md](stability.md). Every surface that is
not listed is internal.

## 3. What "compatible" means

**Rust crates (stable items).** No item removed or renamed, no signature
narrowed, no trait bound added, no enum made exhaustive or variant removed,
no change of documented behavior or error contract. Additions are allowed,
including new methods, new variants of `#[non_exhaustive]` enums and new
optional configuration. The MSRV of `knotrel-core` (1.98.1) is raised only in a
minor release and announced in its notes; the workspace toolchain (1.100.0) is
a development setting, not a promise.

**HTTP API `/v1` (stable routes).** Changes inside `/v1` are additive only:
- allowed: new routes; new optional request fields and parameters; new
  response fields; new values in fields that clients must already treat as
  open (for example a new error `code` for a new condition);
- breaking: removing or renaming a route, method, field, header or error code;
  changing a field's type, meaning or unit; making an optional request field
  required; returning a different status or `code` for an existing condition;
  removing an accepted enum value from a request.

Clients must ignore unknown response fields and handle unknown error codes as
errors of their status class. Error `message` texts are internal.

**Configuration.** A stable `KNOTREL_*` variable keeps its name, accepted
values and default; a new variable has a default that preserves previous
behavior.

**Metrics** (`GET /metrics`, opt-in). Stable families keep their name, type,
unit and label names; label values documented as a closed set are only
extended. Help texts are internal.

**Feeder protocol** ([integration contract](integration-contract.md)). The
request and response shapes, ordering, deduplication and readiness rules of
the feeder routes follow the HTTP rules above.

**Server and console.** See section 5.

## 4. Deprecation

1. A stable surface is deprecated in a release: the documentation marks it
   deprecated with its replacement, and the release notes list it. Rust items
   carry `#[deprecated]`.
2. It stays fully functional for **at least one further minor release**.
3. Its removal is a breaking change: it happens only in a minor (before 1.0)
   or major (from 1.0) release, listed in the release notes.

Security fixes may shorten this when keeping the surface is unsafe; the
release notes then say so.

## 5. Console and server compatibility

The console declares the servers it supports in `console/package.json`
(`knotrel.supportedServer`, a semver range) and in
[docs/console.md](console.md).

**Mandatory before every release, starting with v0.9.0:**
1. Run the console's browser tests (`npm run test:e2e`) against a server
   built from the release candidate, and against the previous release's
   server.
2. Only then update `knotrel.supportedServer` to the versions verified, and
   the compatibility statement in `docs/console.md`; the range is never
   widened beyond what was tested.
3. Record the result in the release notes.

The release checklist ([AGENTS.md](../AGENTS.md#before-release) and the
console release procedure) includes this step. As of 2026-11-21 the range is
`>=0.7.0 <0.9.0`, which excludes 0.9.0 until this check has run.

## 6. Enforcement

- **Public Rust API:** CI runs `cargo-semver-checks` 0.51.0 for both crates
  against the previous release tag with `--release-type minor`, so a breaking
  change fails the job even when the version was bumped. An approved breaking
  change (after section 4) is declared in the crate's
  `[package.metadata.cargo-semver-checks]` lint configuration and reviewed
  with the change.
- **HTTP API:** CI compares `docs/openapi.yaml` with the previous release's
  (`scripts/check-openapi-compat.py`). Breaking changes to stable operations
  fail the job; operations marked `x-knotrel-stability: experimental` are
  reported, not failed. An approved breaking change is listed, with its
  release and justification, in `docs/api/approved-breaking-changes.yaml`.
- **Review:** the classification in [stability.md](stability.md) is reviewed
  with every change that adds a public surface.

## 7. What this does not cover

The PostgreSQL reference feeder (`integrations/postgres-feeder`) and other
examples are examples: their code, configuration and output carry no
stability promise. Persistent formats do not exist (Knotrel keeps no state on
disk). Performance is not part of compatibility; it is reported per release in
[performance by version](performance-by-version.md).
