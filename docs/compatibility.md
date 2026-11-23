# Compatibility and deprecation policy

**Status: APPROVED by the maintainer on 2026-11-21 (roadmap item 0.9.4);
applies from v0.9.0.** It implements readiness criterion 2 of the [v1.0 assessment](releases/v1.0-readiness.md).
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
not listed is internal. An engine may be *experimental* as a qualification
while its interfaces are Stable (section 2.2).

### 2.1 Guarantees of experimental items

An experimental item (Rust item, HTTP route, field, error code, variable value
or metric label value) is:
- documented, tested and supported in every release that contains it;
- changed or removed only in a minor or major release, never in a patch
  release, and each change is listed in the release notes;
- independent of its parent: an experimental variant, method or field never
  weakens the promise of the stable type, route or family that contains it.

Because the tools do not know the classes, removing an experimental Rust item
still fails `cargo-semver-checks`. Such a removal is declared for one release
only:
1. the crate's `[package.metadata.cargo-semver-checks.lints]` allows the
   failing lint, and a matching `[[semver_exception]]` entry in
   `docs/api/public-api-policy.toml` names the crate, the lint, the removed
   items, the decision and the release that ships the removal (`until`);
2. the public API check (section 6) is the per-item guard while the lint
   exception exists: the snapshot diff shows exactly what was removed, and any
   stable item or implementation that is removed or narrowed without its own
   approved `[[narrowing]]` entry fails, whatever the crate-wide lint allows;
3. once the workspace version is past `until`, the check fails until the lint
   and the entry are removed, so no exception outlives its release.

Removing an experimental HTTP element that OpenAPI does not mark
`x-knotrel-stability: experimental` is listed in
`docs/api/approved-breaking-changes.yaml` (section 6). These declarations
document the change; they need no deprecation window.

### 2.2 Stable interface, experimental engine

*Experimental engine* is a qualification of an engine, not a fourth class:
every interface of such an engine is classified **Stable** and follows the
Stable promise of section 3 in full. The qualification says that the
engine's performance, maturity and usage recommendation carry no promise.
It applies to the two non-default engines (maintainer decisions of
2026-11-22,
[freeze review](releases/v1.0-contract-freeze-review.md#71-later-maintainer-decisions-2026-11-22)),
with their methods, fields and trait implementations
([stability.md](stability.md#rust-knotrel-core)):
- ETT: `EngineConfig::EulerTour`, `ForestGraph`, `ForestStats`, the
  `KNOTREL_ENGINE` value `ett`, the `/v1/info` `engine` value `ett` and the
  `knotrel_build_info` label value `engine="ett"`;
- HDT: `EngineConfig::Hdt`, `HdtGraph`, `HdtStats`, `HdtStorageStats`,
  `HdtLevelStorage`, the `KNOTREL_ENGINE` value `hdt`, the `/v1/info`
  `engine` value `hdt` and the `knotrel_build_info` label value
  `engine="hdt"`;
- both: `EngineConfig::is_experimental`, which reports the qualification.

It promises, for the whole 1.x series:
- no item, variant, field, method or trait implementation is removed or
  renamed, and no signature is narrowed; a removal needs the deprecation of
  section 4 and a major release. `#[non_exhaustive]` on `EngineConfig` does
  not authorize removing `EngineConfig::EulerTour` or `EngineConfig::Hdt`;
- the documented behavior and error contract hold: exact connectivity under
  the same contract as `compact-bfs`, the selector values above, the meaning
  and unit of every counter and storage field, and the properties their
  documentation lists as guaranteed (see *Diagnostic values* in section 3);
- the counter and storage structs are exhaustive, so their field sets are
  frozen: a new field requires a major release.

It does not promise:
- performance, latency or memory: the cost bounds in the documentation
  describe the current implementation; they and the measured results may
  change in any release (section 7);
- maturity or a usage recommendation: `compact-bfs` stays the default and the
  recommended engine; lifecycle support is not available on these engines
  (adding it later is an addition);
- promotion: `EngineConfig::is_experimental()` returns `true` for
  `EulerTour` and `Hdt`, and `/v1/info` reports `"experimental": true`,
  possibly for the whole 1.x series. The value becomes `false` only when the engine is promoted, which
  the release notes and [stability.md](stability.md) record; that change is
  not breaking, because the value describes the qualification, not the
  interface;
- a stability class in `experimental`: `EngineConfig::is_experimental()` and
  the `/v1/info` field `experimental` report the engine's qualification
  (whether it is qualified experimental as to performance, maturity and usage
  recommendation), not whether its interfaces are stable; they are stable
  either way. The value may change from `true` to `false` in a minor release
  when an engine is promoted, and never changes from `false` to `true` for an
  existing engine (an engine added later may start at `true`);
- exact counter and storage values, which are implementation-dependent.

## 3. What "compatible" means

**Rust crates (stable items).** No item removed or renamed, no signature
narrowed, no trait bound added, no enum made exhaustive or variant removed,
no change of documented behavior or error contract. Additions are allowed,
including new methods, new variants of `#[non_exhaustive]` enums, new fields
of `#[non_exhaustive]` structs and new optional configuration.
`#[non_exhaustive]` only permits these additions: it does **not** authorize
removing or renaming a public variant, field or engine, which stays a breaking
change under section 4 (only items classified Experimental follow section
2.1; the stable interfaces of an experimental engine, section 2.2, do not).
The types that stay exhaustive are listed in [stability.md](stability.md#exhaustiveness-and-non_exhaustive).

**Diagnostic values.** Some stable structs report counters or storage
figures: `LifecycleStorage` (from `LifecycleGraph::storage_stats`),
`ForestStats`, `HdtStats`, `HdtStorageStats` and `HdtLevelStorage`. For them the stable contract is the
field set, each field's type, meaning and unit (counts or bytes, as
documented), when the values are computed, and the properties their rustdoc
lists under "Guaranteed" (equalities with the graph's counts, sums, bounds,
orderings, reset and saturation behavior). The exact numbers, capacity growth
and retention, allocator and standard-library effects are listed under
"Implementation-dependent": they may change in any release, including a
patch release, and such a change is not breaking. Breaking a guaranteed
property is.

**Public dependencies.** `knotrel-server`'s stable API exposes `axum::Router`
(axum 0.8) and `tokio::net::TcpListener` (tokio 1); these dependency versions
are part of that API ([stability.md](stability.md#public-dependencies-of-knotrel-server)).
An upgrade of either to an incompatible version requires a SemVer assessment
of Knotrel, recorded in the release notes; from 1.0 it is released as a major
version unless the assessment shows the public API unchanged.
Each crate declares its MSRV in `rust-version`:
`knotrel-core` 1.98.1 and `knotrel-server` 1.100.0 (inherited from the
workspace). Either is raised only in a minor release and announced in its
notes. The toolchain used to develop the workspace is a separate development
setting, not a promise.

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
behavior. A renamed variable keeps its previous name as a deprecated alias
(section 4), with a documented rule for when both names are set: the console's
`KNOTREL_UPSTREAM` and `KNOTREL_UPSTREAM_TIMEOUT` are such aliases in 1.x
([console.md](console.md#container-configuration)). The `KNOTREL_CONSOLE_`
prefix and those two aliases belong to the console: the server never defines a
setting under them and ignores them.

**Metrics** (`GET /metrics`, opt-in). Stable families keep their name, type,
unit and label names; label values documented as a closed set are only
extended. Help texts are internal.

**Feeder protocol** ([integration contract](integration-contract.md)). The
request and response shapes, ordering, deduplication and readiness rules of
the feeder routes follow the HTTP rules above.

**Server and console.** See section 5.

**HTTP exceptions approved for v1.0.0.** The maintainer approved these
breaking HTTP changes on 2026-11-22 without the deprecation window of
section 4 ([contract freeze review](releases/v1.0-contract-freeze-review.md),
section 7); the release notes list them:
- *A different `code` for an existing condition (decision 4):* an exhausted
  feeder admission lane (data lane: `POST /v1/feeder/events`, `/barrier`,
  `/gap`; control lane: `/session`, `/heartbeat`) answers `503 busy`, as the
  shared lane does, instead of `503 import_in_progress`, which is now
  returned only by `POST /v1/feeder/import` while another import runs.
  Clients that retried on `import_in_progress` from those five routes retry
  on `busy`.
- *Stricter input validation (decision 6):* the documented limits are
  enforced on every field they apply to. Source positions (event
  `source_position` and `prev_source_position`, import `source_position`,
  `barrier_source_position` when present) are 1 to 256 bytes of UTF-8;
  `event_id` is at most 64 bytes. The limits count bytes, not characters. A
  request whose only fault is a value outside them is refused with `422
  invalid_request` and changes nothing. The bound is checked at the existing
  validation step, not first: on events after the session, lease, generation,
  initialization, desynchronization, `source_id` and `source_epoch` checks
  (so a request that also carries a different `source_epoch` still
  desynchronizes with `409 source_epoch_mismatch`); on a barrier after the
  session, lease, generation, initialization, desynchronization and
  recovery-fence checks;
  on an import after the instance and `graph_id` checks and before the
  session checks. Before, empty positions and over-long import and barrier
  positions were accepted. The OpenAPI entries are in
  `docs/api/approved-breaking-changes.yaml`.
- *A different status and `code` for an existing condition (backlog N6,
  approved by the maintainer on 2026-11-23,
  [v1.0 backlog](releases/v1.0-backlog.md)):* on `POST /v1/feeder/import`,
  413 `payload_too_large` now means only that the body exceeds
  `KNOTREL_MAX_IMPORT_BODY_BYTES`, by its declared `Content-Length` (refused
  before any body byte is read) or while it is received. A body that cannot
  be read to its end for another reason (client abort, malformed chunked
  encoding, receive error) answers `400 invalid_request` ("request body could
  not be read"), as it already did on the other JSON routes. Before, every
  import body read failure answered 413 `payload_too_large`. Feeders must not
  shrink an import after a 400; they retry it unchanged once the transport
  problem is fixed. The import still counts as `payload_rejected` in
  `knotrel_imports_total` (or `abandoned` when the connection ends first).
- *A different `code` for an existing combined-fault condition (review N5a,
  approved by the maintainer on 2026-11-23,
  [v1.0 backlog](releases/v1.0-backlog.md)):* on `POST /v1/feeder/barrier`,
  the recovery fence (`feeder_epoch <= last_expired_epoch` after a lease
  expiry) is now evaluated right after the desynchronization check (B5) and
  before the source-position bound and the sequence and position comparisons
  (B8, B9, B7), as the
  [integration contract](integration-contract.md#43-lease-renewal-vs-synchronization-proof--recovery-fencing)
  §4.3 states. A barrier under a fenced epoch that also has a premature
  sequence, an older sequence, a mismatched position or an out-of-bounds
  position now answers `409 fenced_feeder_session`; before, it answered
  `409 barrier_not_reached`, `409 superseded_barrier`,
  `409 barrier_position_mismatch` or `422 invalid_request`. The guaranteed
  precedence is: failed, session and lease, generation, uninitialized (B3,
  B4), desynchronized (B5), recovery fence, position bound, B8, B9, B7, then
  B1/B2 ([integration contract](integration-contract.md#53-verification-only-synchronization-barriers)
  §5.3). Every rejection still leaves the state unchanged, and a barrier
  under a newly registered epoch answers as before. Feeders already had to
  register a newer epoch to recover from an expiry; only the code of these
  doomed barriers changes. No OpenAPI entry is needed: the route, its
  statuses and its codes are unchanged.

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
console release procedure) includes this step. The current range, and the
servers it was verified against, are stated in [docs/console.md](console.md);
for 1.0 it is set by this check against the release candidate and the 0.9.x
server.

## 6. Enforcement

- **Public Rust API:** CI runs `cargo-semver-checks` 0.51.0 for both crates
  against the previous release tag with `--release-type minor`, so a breaking
  change fails the job even when the version was bumped. An approved breaking
  change (after section 4) is declared in the crate's
  `[package.metadata.cargo-semver-checks]` lint configuration and reviewed
  with the change. The configuration works per lint and per crate, not per
  item, so every configured lint has a `[[semver_exception]]` entry in
  `docs/api/public-api-policy.toml` (crate, lints, items, decision and the
  release `until` for which it is valid); the public API check below fails for a
  lint without an entry, for a stale entry, and for an entry whose release is
  older than the workspace version. Current entries: `#[non_exhaustive]` on
  `EngineConfig`, `GraphError`, `IncidenceError`, `EdgeState` (core) and
  `Phase` (server), approved for v1.0.0 on 2026-11-22.
- **Public Rust API snapshot and classification:** CI (job `public API`)
  regenerates the public API of both crates from rustdoc JSON (default
  features and `test-support`, `#[doc(hidden)]` items included) with
  `scripts/public-api.py check`, and fails when:
  - it differs from `docs/api/public-api-knotrel-core.txt` or
    `docs/api/public-api-knotrel-server.txt`;
  - a public item or trait implementation has no class in
    `docs/api/public-api-classification.txt`, a member or implementation is
    classified above its type, a `test-support` item or an implementation of
    an unstable trait is not internal;
  - `#[non_exhaustive]` appears on an item not approved in
    `docs/api/public-api-policy.toml`, or is missing from an approved one
    (this keeps the types frozen exhaustive frozen, which the crate-wide
    semver-checks exception would not);
  - compared with the pull request's base, a stable key became experimental,
    internal or disappeared without an approved `[[narrowing]]` entry (the
    machine-readable form of the separately marked list in
    [stability.md](stability.md#reclassified-from-stable-to-experimental));
  - a cargo-semver-checks exception lacks its entry or has expired;
  - [stability.md](stability.md#how-the-rust-tables-are-checked) disagrees with
    the classification.
  Unlike `cargo-semver-checks`, this also catches additions, so every new
  public item gets an explicit class in the change that adds it.
- **HTTP API:** CI compares `docs/openapi.yaml` with the previous release's
  (`scripts/check-openapi-compat.py`). Breaking changes to stable operations
  fail the job, including a value removed from an enum reachable from a
  stable response (an error code, an `engine` value, a status or mode), a
  response enum dropped (an open set of values) and a response constant
  changed or removed; new response enum values pass. Changes to operations
  marked `x-knotrel-stability: experimental` in the previous release are
  reported, not failed. Marking a stable operation experimental is itself a
  breaking change. An approved breaking change is listed, with its release
  and justification, in `docs/api/approved-breaking-changes.yaml`, either by
  its per-operation message or, for a value of a shared schema (for example
  an `ErrorCode` value), once by the schema-level message the job prints.
- **Review:** the classification in [stability.md](stability.md) is reviewed
  with every change that adds a public surface.

## 7. What this does not cover

The PostgreSQL reference feeder (`integrations/postgres-feeder`) and other
examples are examples: their code, configuration and output carry no
stability promise. Persistent formats do not exist (Knotrel keeps no state on
disk). Performance is not part of compatibility; it is reported per release in
[performance by version](performance-by-version.md).
