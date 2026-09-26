# Rust Foundation Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Do not commit or push: the user explicitly prohibited both.

**Goal:** Deliver a documented, testable Rust foundation and a reproducible local baseline.

**Architecture:** One core/server workspace and a separate benchmark workspace. Exact reference connectivity stays independent of HTTP; bounded blocking work isolates CPU traversal from the async runtime.

**Tech Stack:** Rust 1.98.1, edition 2024, Axum 0.8, Tokio, Serde.

**Spec:** `docs/architecture/0001-foundation.md`

## Global Constraints

- Rust 1.98.1; edition 2024; resolver 3.
- English public Rustdoc and algorithm documentation; deny missing docs and unsafe code.
- No commits, pushes, publishing or changes to remote repositories.
- Preserve existing repository content and license choices.
- Work in cloud clones; deliver checked patches because the user's Mac is inaccessible.

## Review Focus

- Bridge deletion versus deletion on a cycle: exact answers after both.
- Unknown, isolated, duplicate and maximum-width node IDs: consistent core and HTTP behavior.
- Malformed/oversized batches: no partial execution from envelope validation.
- Concurrent requests and dropped callers: bounded admission and retained worker permits.
- Benchmark parity: known answers after splits and rejoins; reject invalid inputs.

### Task 1: Workspace and reference core

**Files:** Root Cargo/toolchain/lint configuration; `crates/knotrel-core/{Cargo.toml,src/lib.rs,tests/connectivity.rs}`; contributor instructions and CI.

**Interfaces:** Produces `NodeId = u64`, `Graph::{new,add_node,link,cut,connected,node_count,edge_count}`, and `GraphError::{SelfLoop,UnknownNode}`. Mutations return whether state changed; invalid loops cannot create vertices.

- [x] Write behavioral tests for the graph contract, including an independent transitive-closure oracle over deterministic update traces.
- [x] Run `cargo test -p knotrel-core`; verify the unimplemented behavior fails.
- [x] Implement adjacency maintenance and documented breadth-first traversal.
- [x] Run `cargo test -p knotrel-core`; require all tests and doctests to pass.

### Task 2: HTTP service

**Files:** `crates/knotrel-server/{Cargo.toml,src/lib.rs,src/api.rs,src/service.rs,src/main.rs,tests/http.rs}`; `docs/http-api.md`.

**Interfaces:** Consumes Task 1's `Graph`; produces `router(Graph) -> axum::Router`. Operations are `add_node`, `link`, `cut`, `connected`, with decimal-string node IDs. Batch returns ordered per-operation results.

- [x] Write integration tests for updates/queries, JSON errors, batch continuation and concurrent duplicate links; write service tests for admission and dropped callers.
- [x] Run server tests and verify the missing behavior fails.
- [x] Implement bounded blocking dispatch, validation, structured errors, routes and graceful shutdown.
- [x] Run `cargo test --workspace`; require all tests and doctests to pass.

### Task 3: Benchmark workspace

**Files:** Separate repository's manifests/toolchain, `crates/knotrel-benchmarks/{src/lib.rs,src/main.rs,tests/workload.rs}`, CI, README and `docs/methodology.md`.

**Interfaces:** Consumes Task 1's core using `../../../knotrel/crates/knotrel-core`; produces deterministic workload configuration and a JSON baseline report.

- [x] Write tests for known split/rejoin answers, deterministic seeds and invalid inputs.
- [x] Run benchmark tests and verify the missing behavior fails.
- [x] Implement workload generation, timed operations, independent expected answers and JSON metadata.
- [x] Run workspace tests and a release smoke run; require correct answers and valid JSON.

### Task 4: Verification and delivery

**Files:** Both READMEs, documentation, workflow files, lockfiles and an external patch bundle.

- [x] Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` and `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --no-deps --locked` in both repositories.
- [x] Review the complete uncommitted changes and address material findings.
- [x] Generate patches against recorded base commits and verify they apply to clean copies.
- [x] Deliver patch bundle and verification evidence; confirm original HEADs did not move.
