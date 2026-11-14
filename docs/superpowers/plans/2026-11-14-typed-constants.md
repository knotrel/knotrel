# Typed Constants Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace production magic strings and meaningful magic numbers with authoritative Rust types and constants while preserving every external contract.

**Architecture:** Keep wire vocabulary in the existing API boundary, lifecycle vocabulary in `lifecycle.rs`, configuration vocabulary in `configuration.rs`, and packed-state invariants beside `PublishedReadiness`. Reuse those definitions from routing, service, metrics, and middleware rather than introducing a general constants module.

**Tech Stack:** Rust 2024, Axum 0.8, Serde, prometheus-client; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-typed-constants-design.md`

## Global Constraints

- Preserve all HTTP paths, headers, JSON values, error codes, metric names and labels, environment names, limits, and defaults byte-for-byte.
- Keep Rust 1.100.0 workspace compatibility and Rust 1.98.1 `knotrel-core` compatibility.
- Add no dependency and preserve `Cargo.lock`; specifically, do not add `strum` for the reasons recorded in the spec.
- Do not edit frozen baselines, evidence artifacts, or `knotrel-benchmarks`.
- Do not commit or push.

## Review Focus

- Unknown routes must still use the bounded `unmatched` Prometheus label.
- Expired source-managed readiness must still collapse `ready` and `catching_up` to `stale` with existing precedence.
- Every API error variant must serialize to its existing snake-case code and retain its existing HTTP status.
- Configuration aliases (`source_managed` and `source-managed`) and unknown-key rejection must remain unchanged.
- Maximum packed readiness values must round-trip without overlapping bit fields.

---

### Task 1: Typed HTTP and API vocabulary

**Files:**
- Modify: `crates/knotrel-server/src/api.rs`
- Modify: `crates/knotrel-server/src/lib.rs`
- Modify: `crates/knotrel-server/src/lifecycle.rs`
- Modify: `crates/knotrel-server/src/metrics.rs`
- Modify: `crates/knotrel-server/src/service.rs`
- Test: `crates/knotrel-server/src/api.rs`
- Test: `crates/knotrel-server/tests/contract.rs`

**Interfaces:**
- Produces: `Route::{ALL, as_str}`, shared custom `HeaderName` constants, `DEFAULT_GRAPH_ID`, and typed `ErrorCode::as_str` at the API boundary; HTTP status selection remains explicit where one code has multiple valid statuses.
- Consumes: existing Axum handlers, Serde response types, and metric route matching.

- [x] Add contract tests referencing the proposed route/error vocabulary and asserting all existing external spellings; run them and confirm compilation fails because the types do not exist.
- [x] Implement the route enum, header constants, default graph ID, feeder field limits, lease bounds, and API error-code enum with exhaustive mappings.
- [x] Replace duplicated production literals in router construction, middleware, service validation, response metadata, and metric route classification.
- [x] Run focused API, lifecycle, metrics, service, and contract tests; preserve negative tests as wire literals so they remain independent contract checks.

### Task 2: Typed readiness and metric domains

**Files:**
- Modify: `crates/knotrel-server/src/lifecycle.rs`
- Modify: `crates/knotrel-server/src/metrics.rs`
- Modify: `crates/knotrel-server/src/configuration.rs`
- Test: `crates/knotrel-server/src/lifecycle.rs`
- Test: `crates/knotrel-server/tests/metrics.rs`

**Interfaces:**
- Produces: `ReadinessState::{STANDALONE, SOURCE_MANAGED, as_str}` and `ServerMode::as_str`.
- Consumes: `Phase`, `SyncStatus`, readiness response creation, and Prometheus collection.

- [x] Add tests asserting the complete ordered state domains and mode labels; run them and confirm failure because the typed APIs are absent.
- [x] Make readiness computation return `ReadinessState` and serialize/compare only through `as_str` at HTTP and Prometheus boundaries.
- [x] Remove parallel string state arrays from `metrics.rs` and use the typed state domains.
- [x] Give lifecycle's atomic phase an explicit `repr(u8)` conversion so raw phase constants disappear without weakening the atomic representation.
- [x] Run lifecycle and metrics tests, including expired-lease and scrape-cardinality cases.

### Task 3: Typed configuration keys and numeric invariants

**Files:**
- Modify: `crates/knotrel-server/src/configuration.rs`
- Modify: `crates/knotrel-server/src/service.rs`
- Test: `crates/knotrel-server/src/configuration.rs`
- Test: `crates/knotrel-server/src/service.rs`

**Interfaces:**
- Produces: private `Setting::{parse, as_str}`; named packed-readiness masks/shifts; named event and lease constraints from Task 1.
- Consumes: environment snapshot parsing, `PublishedReadiness::{pack, unpack}`, feeder-event validation, and session validation.

- [x] Add tests for exhaustive setting-name parsing and packed-layout boundary round trips; run them and confirm the new setting API is absent.
- [x] Store recognized environment settings as `BTreeMap<Setting, String>` and use typed keys for every lookup and validation error.
- [x] Replace readiness bit masks/shifts, milliseconds-per-second conversions, and feeder field/lease limits with named constants where the name carries an invariant.
- [x] Leave algorithmic zero/one values, mathematical operations, prose, and test fixtures inline as specified.
- [x] Run configuration and service tests.

### Task 4: Audit and full verification

**Files:**
- Modify: any production file above only when the final audit finds an in-scope duplicated domain value.
- Verify: whole workspace and documentation.

**Interfaces:**
- Consumes: all earlier task interfaces.
- Produces: a clean, contract-compatible workspace.

- [x] Re-scan production sources for repeated route, header, graph-ID, configuration-key, error-code, readiness-state, feeder-limit, and packed-layout literals; classify every remaining candidate against the spec.
- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [x] Run `cargo test --workspace --all-features` and `cargo test --workspace --doc`.
- [x] Run `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps`.
- [x] Verify `knotrel-core` with Rust 1.98.1 using the repository-supported toolchain command.
- [x] Confirm `Cargo.lock` is unchanged and inspect the final diff for accidental contract changes.
