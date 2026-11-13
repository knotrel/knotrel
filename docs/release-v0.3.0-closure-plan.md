# Knotrel v0.3.0 reduced-scope release closure plan

**Status: D1–D8 and the Rust 1.100.0 comparison amendment were approved on 2026-11-12.
Packages 1–3 are complete for the identities recorded below. The O3 deployment check passed from
clean commit `a076c03` on 2026-11-13, and M4.5/M4 acceptance is recorded under D2. Package 4
verification is in progress; the final committed-RC dry run, qualification-validity check and
maintainer release approval remain open.**

Prepared 2026-11-12 against `knotrel` `cdb0e5b4d5694ec38087250e5134c1481cc347b7` (clean, equal to
`origin/main`) and `knotrel-benchmarks` `f85d88eb` (clean). The approval record is in §6.
Approval of the measurement criteria precedes any qualification run; it is not a retroactive approval
of historical campaigns.

Path convention: `crates/…`, `docs/…`, `scripts/…` are relative to the `knotrel` repository;
`evidence/…` and `baseline-v0.2.0/…` are relative to the workspace root `/Users/rt/workspace/knotrel`
(outside both repositories); `knotrel-benchmarks/…` is the sibling repository.

## Proposed release contents

v0.3.0 ships the functionality that already exists on `main`, plus a minimum operability kit:

- **Compatible batch contract (M3.1)** — accepted 2026-11-12 (ROADMAP line 405, ADR 0013).
- **Feeder failure model, retry/uncertain-outcome reconciliation, shared mutation execution and
  their tests (M3.2–M3.5)** — implemented and committed (`cc27279`, `ce6e784`, `c6dc3e1`), with
  evidence in `docs/integration-contract.md` §9.3–9.5. Functional acceptance was recorded under D2.
- **Lifecycle (M4.1–M4.4)** — existing liveness, readiness, drain/shutdown and startup-only
  configuration, documented in `docs/http-api.md` and accepted under D2.
- **Deployment example and runbook (M4.5)** — new in Package 2: a non-published container example
  driving the existing reference feeder, plus a verified restart/rebuild runbook.
- **Candidate changes since v0.2.0** — the only production source change is Candidate A
  (`Graph::get_or_insert_index` in `crates/knotrel-core/src/lib.rs`, used by `Graph::link`), plus
  dependency bumps: `simd-json` 0.18→0.19, `parking_lot` 0.12→0.13 (`lock_api` 0.5,
  `parking_lot_core` 0.10), and minor `libc`/`mime`/`mio`/`value-trait` updates. `crates/knotrel-server/src`
  is unchanged since `v0.2.0` (`7e9becd`). Candidate A ships as part of the candidate **with no
  claimed performance benefit**.
- **A bounded performance gate** — one pre-approved contemporary baseline/candidate comparison on
  9 cells, plus correctness and identity guardrails (Package 3).

## Explicit exclusions (deferred, not completed)

| Deferred item | Roadmap location | Why deferring is coherent |
|---|---|---|
| M3.6 throughput, fairness and queue-depth evaluation of batching | ROADMAP line 412 | No batching change ships; the legacy batch path's regression risk is covered by the 4 batch cells in Package 3. |
| M5 contention instrumentation and concurrency redesign (M5.1–M5.5) | lines 433–440 | The coarse `parking_lot::Mutex` is retained unchanged. No alternative is evaluated, so no claim about one is made. |
| M6 Prometheus metrics, alerting and instrumentation-overhead budgets (M6.1–M6.5) | lines 424–431 | Existing stats/monitoring surfaces (`docs/http-api.md` external-feeder operations and monitoring, lines ~736–790) ship unchanged. No new metrics endpoint. |
| P2 (M6/M5 evaluation) | line 398 | Depends on the M5 and M6 work, which is deferred. |
| The full original P3 matrix (`docs/performance-v0.3.0.md` §4) | line 399 | Replaced by the bounded 9-cell gate. The deferred dimensions are listed in §4.3. |
| Quantitative source-relative lag reporting | Acceptance M4, line 422 | The server has no comparable source-side frontier. `stale` and `catching_up` are synchronization states, not lag measurements, and are not presented as such. |
| Performance qualification of `ett`, `hdt` and `petgraph-dfs`, and of non-macOS hosts | P3 | Their code paths are unchanged by Candidate A. They are disclosed as not re-qualified for v0.3.0. |

Also excluded: persistence, new endpoints or response fields, new concurrency mechanisms, the
optional M3.4 shared error-code refactor, the parked `put`/FIFO experiments, and any further
optimization.

## The finish line

v0.3.0 is releasable when all of the following hold:

1. The approval block (§6) is signed.
2. Packages 1–4 reach their completion conditions (§2).
3. The qualification campaign has run against the final candidate SHA under the pre-approved
   criteria, with zero correctness or identity failures and a recorded disposition for every cell.
4. `git diff --name-only <qualified-sha>..<tag-sha>` contains no invalidating path (§5.9).
5. The maintainer gives the final release approval (checklist item R4.8).

Nothing else blocks the release.

---

## 1. Work packages in dependency order

```
Approval block (§6)
   └─► P1 Freeze scope ──► P2 Minimum operability ──┐
                     └──► P3 Qualify candidate ─────┴─► P4 Prepare and approve release
```

Packages 2 and 3 are independent of each other. Package 3 must measure the **final** candidate
source identity. Production sources, every file compiled into a measured binary (including
`tests/common/feeder.rs` and `examples/measure_e2e.rs`), `Cargo.toml`/`Cargo.lock` and build
configuration must therefore be frozen before Package 3 executes. Package 2 adds docs and container
files and must not modify any of those sources (§5.9). The workspace version is already `0.3.0` in
`Cargo.toml` and `Cargo.lock`, so **no version bump is needed before the qualification build**, and
the qualified source identity can be the tagged identity.

### Package 1 — Freeze the release scope

Purpose: record what ships, what is accepted, and what is deferred, in the roadmap and current docs.

| Item | Requirement | Category | Evidence | Smallest remaining action |
|---|---|---|---|---|
| M3.1 | Compatible batch contract | Satisfied | ROADMAP line 405; ADR 0013 "Accepted (functional contract)"; `crates/knotrel-server/tests/batch.rs` | None |
| M3.2 | Feeder failure model | Accepted under D2 | `cc27279`; `docs/integration-contract.md` §9.3; `tests/events.rs` `test_20`, `test_21`, `test_23`, `test_24`, `test_26` | None |
| M3.3 | Retry and uncertain outcome | Accepted under D2 | `ce6e784`; integration contract §9.4 decision table; `events.rs` `test_25`, `test_27`; `tests/e2e_rebuild.rs` `test_15`, `test_22`, `test_23` | None |
| M3.4 | Shared mutation execution | Accepted under D2 | `c6dc3e1` (no production change needed); integration contract §9.5; `events.rs` `test_28` | None |
| M3.5 | Failure/duplicate/concurrency/restart/rebuild tests | Accepted under D2 | `events.rs` `test_20`, `test_22`; `batch.rs`; `e2e_rebuild.rs` `test_15`, `test_23` | No new tests |
| M3.6 | Batching measurement | Deferred | — | None in v0.3.0 |
| M4.1 | Liveness | Accepted under D2 | `/health`; `tests/bootstrap.rs`, `tests/import.rs`, `tests/lifecycle.rs`, `tests/process.rs` | None |
| M4.2 | Readiness and freshness policy | Accepted under D2 | Synchronization states in `service.rs`; `/ready` precedence in `docs/http-api.md` | None; quantitative lag is deferred |
| M4.3 | Drain and shutdown | Accepted under D2 | `docs/http-api.md`; `tests/lifecycle.rs`, `tests/process.rs` | None |
| M4.4 | Configuration validation | Accepted under D2 | Startup-only configuration and lease bounds in `docs/http-api.md` | None |
| M4.5 | Deployment example | Accepted under D2 | `examples/container/`, `docs/deployment.md`, and the 2026-11-13 candidate transcript | Revalidate if relevant inputs change |
| M5, M6, P2 | Deferred | — | — | None in v0.3.0 |
| P0 | Released baseline | Closed under D3 | Increment 1 (17 cells, 102/102 pairs), revalidated under Q1 | None; historical evidence is not a new comparison baseline |
| P1, P3 | Regression and qualification | Bounded gate completed | Package 3 decision report; 7 PASS and 2 INCONCLUSIVE with D6 disclosure | Verify applicability to final RC |

Deliverables:

- `ROADMAP.md` amended with the text in §3, after approval.
- Release notes carry the limitations in D8, written in Package 4.

Completion condition: `ROADMAP.md` shows the approved text, with each acceptance dated and its
evidence cited, and every deferral labelled "Deferred to a later release". No box is checked
without a recorded owner acceptance.

### Package 2 — Minimum operability

Existing assets to reuse:

- `crates/knotrel-server/examples/external_feeder.rs` — a reference feeder. It runs against an
  existing server via `--addr`/`KNOTREL_ADDR`, or spawns one with `--spawn-server`. Its restart
  step (lines 395–428) runs only in spawn mode.
- `docs/http-api.md` sections, at the line ranges given above: trust model (loopback default,
  `KNOTREL_ALLOW_NON_LOOPBACK=true` required otherwise), resource bounds, probes, graceful
  shutdown, timeouts, configuration, external feeder operations and recovery.
- `docs/integration-contract.md` §9.3–9.4 — failure model and retry decision table.

Deliverables:

| # | Deliverable | Missing behavior or instructions | Completion check |
|---|---|---|---|
| O1 | `examples/container/Dockerfile` and `examples/container/Dockerfile.dockerignore` (BuildKit applies a `<Dockerfile>.dockerignore` next to the Dockerfile when the repository root is the context) | No container asset exists. Needed: a multi-stage build with `cargo build --release --locked -p knotrel-server` (no `test-support`); a non-root runtime user; `KNOTREL_MODE=source_managed` (the spelling documented in the `docs/http-api.md` configuration table; the parser in `crates/knotrel-server/src/configuration.rs` also accepts `source-managed`, but the example uses the documented form), because the default `standalone` mode never reports `uninitialized` and rejects feeder traffic; `KNOTREL_ADDR=0.0.0.0:8080` and `KNOTREL_ALLOW_NON_LOOPBACK=true` set **inside the container only**; no volumes (no graph state is persisted); `EXPOSE 8080`; `HEALTHCHECK` against `/health`. | `docker build` exits 0; the image runs as a non-root UID; `docker run --rm -p 127.0.0.1:8080:8080 --memory 512m --stop-timeout 15` yields `/health` 200 and `/ready` 503 with status `uninitialized`. |
| O2 | `docs/deployment.md` (runbook) | No single operator procedure exists. Needed: start; probe semantics (`/health` = process alive in every state, `/ready` = synchronized; the state table maps `uninitialized`/`loading`/`catching_up`/`ready`/`stale`/`desynchronized`/draining to operator actions, with an explicit statement that source-relative lag is not reported); stop and drain (`--stop-timeout` must exceed `KNOTREL_SHUTDOWN_TIMEOUT_MS`, default 10 s; exit codes 0/1/2/130); restart, after which state is empty and the feeder must perform an authoritative `create_only` import; desynchronization recovery = rebuild via import, never blind skip; feeder exit: readiness is kept only while the feeder heartbeats. Once the lease expires without renewal the server reports `stale` (503), and a later heartbeat alone does not restore `ready`. Leaving `stale` needs a new session epoch plus a barrier, or a restart and rebuild. Assumptions are listed below. | Each command in the runbook is executed once in the O3 transcript, and each documented state transition is observed or cross-referenced to a named test. |
| O3 | Verified transcript in `docs/deployment.md` (appendix) | The external feeder's restart step does not run in `--addr` mode, so the runbook must drive restart externally. The feeder also asserts `/ready` 503 at startup (`examples/external_feeder.rs` line 201), so every feeder run needs a freshly started, unfed server. It registers a 10,000 ms lease, heartbeats every 2 s, and stops heartbeating when it exits (Step 9). | Recorded sequence with wall-clock timestamps and exit codes, using a named container (not `--rm`) started with `KNOTREL_MODE=source_managed`: (1) image build; (2) container start; (3) `/ready` 503 `uninitialized`, observed **before** any feeder request; (4) `cargo run --release --example external_feeder -- --addr 127.0.0.1:8080` exits 0; (5) `/ready` 200 `ready`, probed within 8 s of feeder exit (the 10 s lease minus one 2 s heartbeat interval); (6) after more than 10 s, `/ready` 503 `stale`, recorded as expected lease expiry, not a failure; (7) `docker restart`; (8) `/ready` 503 `uninitialized` (state lost, as documented); (9) feeder rerun exits 0 (its fresh `create_only` import proves the rebuild); (10) `/ready` 200, again within 8 s of exit; (11) `docker stop`; (12) `docker inspect -f '{{.State.ExitCode}}'` reports 0. A step-5 or step-10 probe outside the window, or answering `stale`, does not meet the check and is repeated from step 2 or step 7. The transcript claims readiness only at the probed instants, never continuously. |
| O4 | `docs/http-api.md` stale wording | "v0.1.0" labels (lines 3, 24, 42, 65) describe current behavior. The text "graph state changes only through /v1/operations and /v1/batch" (line ~17) omits feeder and import paths. "rate limiting" is duplicated (line ~78). | Wording corrected with no change in meaning to any current guarantee. `grep -n "v0.1.0" docs/http-api.md` returns only intended historical references. |

Assumptions the runbook must state:

- **Security:**
  - No authentication, authorization or TLS.
  - Publish only to `127.0.0.1` or a trusted network.
  - Anyone who can reach the port can mutate the graph and take over the feeder.
- **Resources:**
  - Memory grows with graph size.
  - Set a container memory limit and the existing request/graph limits.
  - No durable graph persistence: graph, generation, revision, source frontier and deduplication history exist only in process memory and are lost on any restart. No volume is needed for graph state.
- **Ownership:**
  - Exactly one feeder session owns the graph, fenced by session/epoch and a lease (1000–60000 ms).
  - A second feeder takes over; it is not load-balanced.
- **Source retention:**
  - The server has no persistence.
  - After a restart or desynchronization, the **source** must be able to supply a complete authoritative snapshot for a rebuild by import.
  - Normal eviction of old entries from the deduplication window (`KNOTREL_DEDUP_WINDOW_SIZE`, default 10000) requires no action and no rebuild; the ordered stream continues.
  - A rebuild is required only when an uncertain outcome must be resolved for an event no longer retained in that history. This covers timeouts or lost acknowledgements whose exact retry returns 409 `unverifiable_duplicate`. Knotrel cannot then attribute the earlier outcome, so per the decision table in `docs/integration-contract.md` §9.4 the recovery is a replacement import, not a skip or a blind resend.
  - Event-ID reuse beyond the retained window is not detected (`docs/integration-contract.md`, evicted history).
  - The source must retain events beyond the acknowledged frontier until they are acknowledged.
  - These statements restate existing documented behavior; the runbook adds no recovery guarantee.
- **Reference status:**
  - The external feeder is a test-support-derived reference client (`#[path]` include of `tests/common/feeder.rs`), not a supported production connector.
  - The container image is an example, not a published release artifact.

Package completion condition: O1–O4 delivered. The O3 transcript is recorded from the image built
from the release candidate SHA (or a docs-only descendant of it). Any later change to
`examples/container/**`, `crates/knotrel-server/examples/external_feeder.rs`,
`crates/knotrel-server/tests/common/feeder.rs` or the server sources requires repeating the O1 checks
and the O3 transcript at the new SHA.

Progress:
- Package 1 (S1) applied to `ROADMAP.md` and `docs/performance-v0.3.0.md`.
- O1, O2 and O4 delivered: `examples/container/`, `docs/deployment.md` and `docs/http-api.md`.
- O3 is scripted as `examples/container/check-deployment.sh`. It was executed with 0 failures on an
  uncommitted working tree based on `cdb0e5b`. That run is retained in the `docs/deployment.md`
  appendix as a pre-RC record.
- O3 was repeated on 2026-11-13 from clean commit `a076c03`; the
  [transcript](../../evidence/v0.3.0-package4-local-20261113T063235Z-a076c03/deployment/transcript.txt)
  records zero failures. M4.5 and Acceptance M4 are recorded under D2 for that source or a
  documentation-only descendant.
- Q0/Q1 completed 2026-11-12. The driver copy and hash record are under
  `evidence/v0.3.0-package3-q0-q1-20261112/q0/`; the offline validation report and per-observation
  CSV are under `evidence/v0.3.0-package3-q0-q1-20261112/q1/`. P0 is closed under the approved D3
  coverage exception.
- The maintainer approved the same-toolchain amendment on 2026-11-12. A separate Rust 1.100.0
  v0.2.0 baseline and its provenance are retained under
  `evidence/v0.3.0-rust110-baseline-20261112/`. Q2–Q6 have since completed as recorded below.

### Package 3 — Qualify the final candidate

Detailed in §4 and §5. Ordered tasks:

| # | Task | Completion condition |
|---|---|---|
| Q0 | Preserve the Increment 1 driver binary (`knotrel/target/release/examples/measure_e2e`, sha256 `3dda9c76…`) before any release build of the examples overwrites it. | A copy exists under a new `evidence/` directory, with a recorded sha256 equal to the manifest value. |
| Q1 | Revalidate Increment 1 offline with the repaired collector validator (`knotrel-benchmarks` `d3cf6198`), per `evidence/collector-validation-repair-20261111/closure.md`. Read-only over the historical bundle; output goes to a new directory. | Report states, for each of the 204 observations: schema valid, exit 0, `parsed_metrics` present, oracle/`all_items_matched` true. Per-cell A/A dispersion is recomputed and matches `offline_analysis/ANALYSIS.md`, or differences are explained. Any invalid observation is listed, and its cell is removed from noise calibration. |
| Q2 | Add two-label baseline/candidate mode to `knotrel-benchmarks/scripts/run_p0_aa.py`, with independent binary identities and the balanced schedule; implemented at collector commit `580c7e44` while retaining A/A mode. | `--dry-run` over the new manifest passes provenance checks. A dry run where both labels point to the same binaries reproduces the existing A/A behavior. Collector tests cover label balance, per-label hash mismatch rejection, and validation of every entry. |
| Q3 | Build the candidate binaries from the frozen final SHA (§5.2). | Manifest records the full SHA, dirty=false, `rustc -vV`, lockfile hashes, binary sha256 values, and the dep-info source set of the server, driver and core harness (§5.9). The derived benchmark lockfile differs from the baseline's only in the `knotrel-core` version line. |
| Q4 | Host qualification and pre-execution dry run (§5.6). | Dry run exits 0 and host checks pass. |
| Q5 | Execute the 9-cell, 12-pair campaign (§5.3). | All 108 pairs recorded, or a stop condition is recorded with all partial evidence retained. |
| Q6 | Apply the pre-approved decision rules (§5.5). Run the single pre-approved extension only for inconclusive cells. | Every cell is PASS, REGRESSION (with an owner disposition) or INCONCLUSIVE (with the D6 disposition). Correctness and identity guardrails are zero-failure. |

Package completion condition: Q6 report filed under `evidence/v0.3.0-qualification-<timestamp>/`,
naming the qualified SHA.

**Q6 outcome:** The [decision report](../../evidence/v0.3.0-qualification-20261112T224213Z-6a1267f/final-decision-analysis.json)
records candidate `6a1267f48c0f72f5b9705ad7f44da74af54b9b70`, 108 initial pairs and
36 extension pairs, with 288/288 valid observations. Seven cells are PASS; two remain INCONCLUSIVE
and are accepted with disclosure under the previously approved D6 policy:

- HTTP batch B8: median paired change **-1.42%**, interval **-15.63% to +12.09%**.
- HTTP lifecycle readiness: median paired change **+8.02%**, interval **-11.46% to +21.97%**.

The two inconclusive cells are not PASS results, and their intervals do not exclude regressions.
The comparison covers only the specified nine cells on the macOS arm64 host; it makes no speedup
claim. Applicability to the final committed RC is checked under §5.9.

### Package 4 — Prepare and approve the release

Reuse `docs/releases/release-procedure.md` and the existing scripts, which detect the version
dynamically.

| # | Check | Source | Completion condition |
|---|---|---|---|
| R4.1 | Version and dependency consistency | Workspace `Cargo.toml` (`0.3.0`), `knotrel-core` path dependency `0.3.0`, `Cargo.lock`, server `rust-version` 1.100.0 and core MSRV 1.98.1 | At the RC SHA, `cargo metadata --locked` succeeds, the per-crate Rust versions match, and no `0.2.0` self-references remain. |
| R4.2 | Quality gates | Procedure §3.1: `cargo fmt` check, strict Clippy with `--locked` for default and `test-support`, warning-denied Rustdoc, workspace tests, `test-support` tests, the 3 core examples, and `cargo +1.98.1 test --locked -p knotrel-core` for the core MSRV | Every command exits 0 at the RC SHA, with logs retained. |
| R4.3 | Source package verification | §3.2 `scripts/verify-source-packages.sh` | Exits 0. |
| R4.4 | Release binary smoke test | §3.3 release build plus `scripts/smoke-test-release-binary.sh` (standalone mode only); the O3 transcript covers source-managed mode in a container | Exits 0. |
| R4.5 | CI dry run | §4 `release.yml` dry run: input validation, quality gates, binaries for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` with smoke tests, source build, 14-asset bundle; publish jobs skipped | Workflow succeeds at the RC SHA. **Limitation:** `scripts/verify-dry-run-publish.sh` defers the `knotrel-server` `cargo publish --dry-run` while `knotrel-core` 0.3.0 is absent from crates.io (404), so server publication is not fully verified until `knotrel-core` is published. §7 partial-failure recovery applies. |
| R4.6 | Release documents | New `docs/releases/v0.3.0-release-notes.md` (structure of `v0.2.0-release-notes.md`); `README.md` install/checksum examples and security note updated from `v0.1.0` (lines 40–58, 89, 146–148, 199); `release-procedure.md` "0.1.0" examples marked as templates or updated | Release notes list: shipped contents; the D8 limitations; the supported-platform matrix (3 targets, Rust 1.100.0 for the server and core MSRV 1.98.1) with the statement that performance was qualified **only** on the macOS arm64 measurement host for `compact-bfs`/`compact-workspace`; per-cell qualification outcomes, including inconclusive cells. No performance improvement claim. |
| R4.7 | Qualification still valid | §5.9 | The intersection of `git diff --name-only <qualified>..<rc>` with the recorded dep-info source set and the build-configuration paths is empty. Any container or feeder change has a repeated O1/O3 record. |
| R4.8 | Final maintainer approval | Procedure §5 checklist | Maintainer approval recorded **before** procedure §6 (tag and publish). |

A successful dry run does not guarantee registry publication. Publication follows procedure §6–7
and is outside this plan.

**Local Package 4 evidence (2026-11-13).** On `a076c03` with the documentation changes in
this working tree, [retained logs and exit codes](../../evidence/v0.3.0-package4-local-20261113T063235Z-a076c03/logs/)
show R4.1 metadata/version checks and all R4.2 quality gates exiting 0. The default and
`test-support` workspace suites passed 214 and 301 tests respectively; the core MSRV suite
passed 59 tests, including doctests. R4.3 source-package verification passed in a disposable
clone with the documentation overlaid; R4.4 passed for a locally built release binary, an
extracted macOS archive and the O3 source-managed deployment transcript. The local archive
deliberately records unknown commit provenance and is not a release artifact. These are local
results, not checks at a committed RC.

The [provisional applicability record](../../evidence/v0.3.0-package4-local-20261113T063235Z-a076c03/logs/qualification-applicability.log)
compares the recorded dependency inputs and frozen binary hashes with `6a1267f`. The sole
subsequent committed change at `a076c03` reorders the coverage-workflow cache and tool
installation steps; the working-tree changes are documentation only. No measured input or
deployment input changed. R4.7 remains open for the final committed RC. The R4.5 workflow
must run after that RC is committed and pushed; a 2026-11-13 crates.io check returned 404 for
`knotrel-core` 0.3.0, so the `knotrel-server` publish dry run is presently deferred.

## 2. Finite checklist

Execute in order. Each line is complete only when its condition holds.

**1 — Corrections and missing functionality**

- [x] **S1** `ROADMAP.md` amended per §3; acceptances M3.2–M3.5 and M4.1–M4.4 dated with evidence (D1, D2).
- [x] **O1** Container example builds and runs as non-root; the candidate build and check are recorded in the O3 transcript.
- [x] **O2** `docs/deployment.md` runbook covers the Package 2 assumptions.
- [x] **O4** `docs/http-api.md` stale wording corrected.

**2 — Focused verification of concrete gaps**

- [x] **O3** Candidate container restart/rebuild [transcript](../../evidence/v0.3.0-package4-local-20261113T063235Z-a076c03/deployment/transcript.txt) recorded; revalidate if relevant inputs change.
- [x] **Q0** Driver `3dda9c76…` preserved at `evidence/v0.3.0-package3-q0-q1-20261112/q0/measure_e2e`; [hash record](../../evidence/v0.3.0-package3-q0-q1-20261112/q0/SHA256SUMS.txt) matches the manifest.
- [x] **Q1** [Offline revalidation report](../../evidence/v0.3.0-package3-q0-q1-20261112/q1/REPORT.md): 204 observations revalidated; per-cell dispersion matches all 17 rows.
- [x] **Q2** Two-label collector mode at `580c7e44`, with dry run and collector tests passing.

**3 — Approved performance qualification**

- [x] **Q3** Candidate binaries built from `6a1267f`, with provenance recorded.
- [x] **Q4** Host qualified and dry runs passed for the campaign and extension.
- [x] **Q5** Campaign and permitted extension executed with all evidence retained.
- [x] **Q6** Per-cell decisions recorded; both INCONCLUSIVE cells use the pre-approved D6 disclosure.

**4 — Release candidate checks and packaging**

- [ ] **R4.1–R4.5** Local R4.1–R4.4 checks passed; the committed-RC checks and R4.5 workflow remain.
- [x] **R4.6** Release notes, README and procedure examples updated, including both D6 disclosures.
- [ ] **R4.7** Working-snapshot applicability passed; check the final committed RC.

**5 — Release approval**

- [ ] **R4.8** Maintainer release approval recorded; then tag and publish per procedure §6.

## 3. ROADMAP text changes (approved 2026-11-12, applied in S1)

**Line 390, append to the v0.3.0 scope statement:**

> **Reduced scope (approved YYYY-MM-DD):** v0.3.0 ships M3.1–M3.5, M4 and a bounded performance
> qualification gate. M3.6, M5, M6, P2, the full original P3 matrix and quantitative source-lag
> reporting are deferred to a later release and are not part of v0.3.0 acceptance. The coarse graph
> mutex is retained unchanged.

**Performance checkpoints (lines 396–401), replace with:**

> - [ ] **P0 — Released baseline:** v0.2.0 (`7e9becd`) frozen binaries and Increment 1 (17 cells,
>   102 pairs) retained as historical A/A evidence. The remaining original P0 matrix is closed by an
>   owner-approved coverage exception (YYYY-MM-DD). Increment 1 is reused only for noise calibration
>   and procedure, after offline revalidation.
> - [ ] **P1/P3 — Bounded regression and release qualification:** one contemporary, interleaved
>   v0.2.0-versus-final-candidate comparison over 9 cells (`docs/release-v0.3.0-closure-plan.md`
>   §5), with criteria approved before execution, run against the final candidate source identity.
> - **P2 — M6/M5 evaluation:** Deferred to a later release with M5 and M6.
>
> **Performance exit gate:** Correctness and source/binary identity failures block release. Every
> qualification cell has a recorded PASS, owner-dispositioned REGRESSION, or disclosed INCONCLUSIVE
> outcome under the pre-approved rules. No speedup claim is made without a cell-level PASS showing
> improvement.

**M3 (lines 408–413):** check M3.2–M3.5 with dated evidence lines citing §9.3–9.5 of the
integration contract and the tests named in Package 1. Replace M3.6 with:

> - **M3.6 Measurement:** Deferred to a later release. v0.3.0 qualifies legacy batch sizes 1/8/64/256
>   for regression only; batching benefit, fairness and queue depth are not evaluated.

Acceptance M3: unchanged wording; checked once M3.2–M3.5 are accepted.

**M4 (lines 417–422):** check M4.1–M4.4 with evidence. M4.5 is checked after Package 2. Replace
Acceptance M4 with:

> - [ ] **Acceptance M4:** Operators can distinguish process liveness (`/health`), synchronized
>   readiness, `uninitialized`/`loading`/`catching_up`/`stale`/`desynchronized` states, rebuild after
>   restart, and draining, using documented probes and the deployment runbook. Source-relative lag is
>   not reported in v0.3.0: `stale` and `catching_up` indicate synchronization state, not a lag
>   magnitude.

**M6 (lines 424–431) and M5 (lines 433–440):** prefix each section heading with
"Deferred to a later release (not part of v0.3.0)". Leave the items unchecked and unchanged.

**`docs/performance-v0.3.0.md` status line (line 5):** append a pointer to this plan and the approved
reduced gate.

## 4. Corrected coverage and reuse table

### 4.1 Increment 1 cells (`evidence/p0-coverage-increment-1-plan-20261030/manifest.json`)

**Common facts for all 17 cells:**
- Status: `protocol_pending_approval_execution_blocked`.
- Run: campaign `evidence/p0-coverage-increment-1-campaign-20261107-2158`, 6 A/A′ pairs per cell, both labels the frozen v0.2.0 binaries.
- Collection: pre-repair collector.
- Host: Darwin arm64, categorical thermal telemetry only.
- **Valid historical observation; not a baseline comparable to a new run** (different date, driver build `3dda9c76` from `1eb2f9b` plus the patch at `evidence/p0-coverage-increment-1-prep-20261102/source.patch`, older host state).
- Offline validation required for every cell: Q1.

| Cell | Parameters | A/A dispersion (CV; median / max \|log ratio\|) | Intended reuse | New candidate measurement |
|---|---|---|---|---|
| `core_sparse_traversal_10k_compact-bfs_warmed` | chain-split-rejoin-v1, 10,000 nodes, seed 42, warmup 1 | 12.13%; 0.035 / 0.301 | Noise calibration | Yes (Candidate A path) |
| `core_sparse_traversal_10k_compact-workspace_warmed` | same | 9.07%; 0.108 / 0.300 | Noise calibration | Yes (Candidate A path) |
| `core_sustained_churn_10k_compact-bfs_warmed` | sustained-churn-path-v1, 20 rounds, 10,000 nodes | 3.07%; 0.014 / 0.060 | Noise calibration | Yes (Candidate A path) |
| `core_sustained_churn_10k_compact-workspace_warmed` | same | 3.82%; 0.014 / 0.105 | Noise calibration | Yes (Candidate A path) |
| `core_sparse_traversal_10k_{ett-scan,hdt,petgraph-dfs}_warmed` (3 cells) | as above | see `offline_analysis/ANALYSIS.md` | Historical v0.2.0 observation only | No: code unchanged; deferred and disclosed |
| `core_sustained_churn_10k_{ett-scan,hdt,petgraph-dfs}_warmed` (3 cells) | as above | see `ANALYSIS.md` | Historical observation only | No: deferred and disclosed |
| `http_standalone_batch_b1_compact_bfs` | 1000 isolated nodes, 2000 ops (50% query / 25% link / 25% cut), seed 42, 50-query warmup | 7.29%; 0.064 / 0.151 | Noise calibration | Yes (HTTP dependency bumps) |
| `http_standalone_batch_b8_compact_bfs` | same, B=8 | 21.6%; 0.171 / 0.240 | Noise calibration | Yes |
| `http_standalone_batch_b64_compact_bfs` | same, B=64 | 19.3%; 0.029 / 0.145 | Noise calibration | Yes |
| `http_standalone_batch_b256_compact_bfs` | same, B=256 | 46.4%; 0.063 / 1.075 | Noise calibration | Yes |
| `http_lifecycle_compact_bfs_small` | 1k nodes, 2k edges, 500 events, 1k queries, seed 42, warmup 50 | 27.8%; 0.296 / 0.724 | Noise calibration | Yes (default engine, source-managed path) |
| `http_lifecycle_{ett,hdt}_small` (2 cells) | same | see `ANALYSIS.md` | Historical observation only | No: experimental engines, server code unchanged; deferred and disclosed |

### 4.2 Other retained evidence

| Evidence | Role in this release | Limitation |
|---|---|---|
| A/A pilot `p0-aa-pilot-20261027-2212` (31 cells, 186 pairs, "PENDING INDEPENDENT REVIEW") | Historical only | Never reviewed. Not used for criteria. |
| Candidate A comparison `candidate-a-comparison-20261108-1429` (core-only, 6 cells) | Historical only | `offline-closure/corrected-report.md`: inconclusive for attribution. Not used to credit or reject Candidate A. |
| Batch warmup study `batch-warmup-investigation-20261108-0737`, `docs/batch-warmup-investigation.md` | Justifies keeping warmup chunk size 50 | Its CV and position cutoffs are not thresholds. |

### 4.3 Original-matrix coverage explicitly deferred

From `docs/performance-v0.3.0.md` §4:
- core sparse scenarios other than chain-split-rejoin, at 1,024/100k/1M;
- churn at 1k/100k/1M and query mixes 10/50/90/99%;
- dense 128/512;
- HTTP standalone medium/large tiers, B=1024 and clients 1–32;
- source-managed medium/large tiers and event batches 8–1024;
- interference;
- saturation;
- scale supplement;
- robustness seeds 0 and 2026;
- all non-macOS hosts.

None of these is claimed for v0.3.0.

## 5. Proposed measurement protocol and budgets

### 5.1 Matrix — 9 cells, selected by concrete release risk

| Risk | Cells |
|---|---|
| Candidate A (`Graph::link`, used by `compact-bfs` and `compact-workspace`) | `core_sparse_traversal_10k_{compact-bfs,compact-workspace}_warmed`, `core_sustained_churn_10k_{compact-bfs,compact-workspace}_warmed` |
| `simd-json` and `parking_lot` bumps on every HTTP request; legacy batch path | `http_standalone_batch_b{1,8,64,256}_compact_bfs` |
| Same bumps on the source-managed import/event/restart path, default engine | `http_lifecycle_compact_bfs_small` |

Cell parameters are identical to the Increment 1 manifest. No new workloads are added.

### 5.2 Comparison design and identities

**Approved amendment, 2026-11-12:** The maintainer approved building both labels with
Rust 1.100.0. The historical Rust 1.98.1 binaries, sources, lockfiles and evidence remain
unchanged. This amendment does not qualify performance or change the nine cells, thresholds,
resource limits or stopping rules.

**Historical v0.2.0 baseline, Rust 1.98.1:** The retained local server binary is
`baseline-v0.2.0/target/release/knotrel-server` (sha256 `b156d397…`); the retained core
harness is `baseline-v0.2.0/target/release/knotrel-benchmarks` (sha256 `9d198be6…`).
They remain historical artifacts and are not the B binaries for the amended comparison.
Their local provenance does not establish byte-for-byte identity with a separately downloaded
release.

**Design:** a contemporary, interleaved baseline/candidate comparison with paired runs.
- Label **B**: v0.2.0 sources at `7e9becd54040b9d7f05e2b548e5f86ee3f3e0915`, rebuilt
  with Rust 1.100.0 in a separate clean checkout and build directory:
  - Server: `evidence/v0.3.0-rust110-baseline-20261112/targets/server/release/knotrel-server`,
    sha256 `3a3ffe1d47f5b061ffb324272a585abb04e76a16a80fe6420a7ba527ce5d5d43`.
  - Core harness: `evidence/v0.3.0-rust110-baseline-20261112/targets/harness/release/knotrel-benchmarks`,
    sha256 `5b148693fe94ccee1453a335c8fb9b067a3b26f65f32456b29b5701b1e7d4429`,
    built from harness `df635a1e872fd9e2d556b166ac23f342ee45d675` against v0.2.0 core.
  - Full source identities, build commands, lockfiles, toolchain details and logs:
    `evidence/v0.3.0-rust110-baseline-20261112/PROVENANCE.txt`.
- Label **C**: v0.3.0 binaries built with Rust 1.100.0 from clean candidate
  `6a1267f48c0f72f5b9705ad7f44da74af54b9b70`; the binary identities and
  provenance are in the Q6 decision report and its retained campaign manifest.

Historical Increment 1 values are **not** compared with new values.

**Fixed elements shared by both labels**, so the product is the only variable:
- **HTTP driver:** `measure_e2e` built once with Rust 1.100.0 from the final committed, clean candidate SHA and used for both labels (it is an HTTP client; the v0.2.0 server API it exercises is unchanged), with `--batch-warmup-chunk-size 50`. The historical Q0 driver remains preserved and is not substituted for this build.
- **Core harness:** `knotrel-benchmarks` at `df635a1` for both labels.
  - Candidate C harness is built in a sibling-layout checkout (`<dir>/knotrel` at the candidate SHA, `<dir>/knotrel-benchmarks` at `df635a1`), because of the path dependency `../knotrel/crates/knotrel-core`.
  - The derived `Cargo.lock` may differ from the baseline's derived lock (sha256 `23291d87…`) only in the `knotrel-core` version line (`0.2.0` → `0.3.0`). **Any other difference stops Q3.**
- **Toolchain and build settings:** Rust 1.100.0, host target, release profile with thin
  LTO and one codegen unit, locked dependencies and no `test-support` feature in any timed
  binary. Use the same settings for both labels; record the exact commands and identities.

### 5.3 Schedule, warmup and order balancing

- 12 pairs per cell (108 pairs, 216 launches), split into 3 sessions × 4 pairs per cell.
- In each session, each cell gets 2 B→C and 2 C→B pairs.
- Cell order is randomized per session from a recorded seed.
- Each launch is a fresh process.
- Warmup is the same as Increment 1:
  - core warmup 1, repetitions 1;
  - batch warmup in 50-query chunks;
  - lifecycle warmup 50.
- Waits: 60 s pre-session quiet period, 300 s cooldown between sessions.
- Expected wall time: about 14–15 min. Increment 1 ran 204 launches in 806.8 s, of which 780 s were scheduled waits.

### 5.4 Metrics

| Group | Primary (decision) | Supporting (guardrail or report) |
|---|---|---|
| Core | `workload_wall_ns` | `connected`/`link`/`cut` `p99_ns`; retrospective process peak RSS of the core harness (`peak_rss_bytes`, from the platform time tool after the process exits) |
| Batch | `window_duration_ns` | `request_latency_p99_ns`; sampled server RSS (`sampled_max_rss_bytes`, polled every 25 ms by the driver; it can miss short peaks and is not the exact process peak); `requests_per_sec` (report only) |
| Lifecycle | `time_to_ready_ms` | query latency p99; `restart_recovery_ms`, `import`, `catchup` (report only); sampled server RSS (`sampled_max_rss`, same sampling limits) |

### 5.5 Decision rules (approved; product judgements, not significance claims)

For each cell, compute the paired log ratio `r_i = ln(C_i / B_i)` over the pairs. Report the median
and the distribution-free order-statistic interval for the median, assuming independent pairs:

- n = 12: interval `[r(3), r(10)]`, coverage 96.1%.
- n = 24: interval `[r(7), r(18)]`, coverage 97.7%.

Approved tolerances:

| Guardrail | Ratio | Log ratio |
|---|---|---|
| Primary metric slowdown | 10% | `t = ln 1.10 = 0.0953` |
| Query/request p99 tail | 25% | `ln 1.25 = 0.223` |
| RSS (core retrospective peak; HTTP sampled maximum, each compared only like-for-like) | 20% | `ln 1.20 = 0.182` |

**Rationale for the tolerances:**
- These are maximum acceptable product slowdowns for a release with no performance objective, not detection thresholds.
- The sustained-churn A/A noise (median |log ratio| 0.014) is well below 0.0953, so those cells are expected to resolve.
- Sparse `compact-workspace`, b8, b256 and lifecycle (A/A medians 0.06–0.30, maxima up to 1.075) are expected to be wide. That is why an INCONCLUSIVE rule is pre-declared rather than tolerances being widened after the fact.

**Outcome per cell (primary metric):**
- **PASS:** upper bound ≤ t.
- **REGRESSION:** lower bound > t. Requires an owner disposition (accept the trade-off with release-note text, or block). Improvements elsewhere do not offset it.
- **INCONCLUSIVE:** otherwise. Run the single pre-approved extension: 12 more pairs in 3 further balanced sessions, same binaries, for inconclusive cells only. Recompute on n = 24, then stop. This one pre-declared second look slightly lowers the nominal coverage; it is a decision procedure, not a hypothesis test.
- **After the extension:**
  - Still inconclusive with a median ≤ t: release with disclosure (D6).
  - Still inconclusive with a median > t: treated as REGRESSION for disposition.

**Supporting guardrails:**
- A p99 tail or RSS interval whose lower bound exceeds its tolerance is a REGRESSION requiring disposition.
- Otherwise these values are reported.

**Correctness and identity guardrails (zero tolerance, block release):**
- `all_items_matched` false;
- oracle agreement below 100%;
- revision/fingerprint mismatch;
- schema validation failure;
- binary sha256 drift;
- dirty source;
- an unexpected lockfile difference.

Process failures (non-zero exit, timeout, RSS-ceiling violation, capture failure) are not timing
observations. They are handled under §5.6. If diagnosis attributes one to the candidate, it blocks
release like a correctness failure.

### 5.6 Host qualification, limits and failure preservation

**Host:** the same Darwin arm64 P0 host.
- AC power.
- No concurrent builds, profilers or benchmark processes.
- Sleep is disabled for the run; sleep events are detected and recorded.
- Categorical thermal state is recorded.
- Use the existing `run_p0_aa.py` host checks; `--skip-host-checks` is not permitted.

**Resource and time limits.** Each limit is stated as what the existing collector (`run_p0_aa.py`)
actually does. A retrospective check is not a hard memory cap: a process can exceed the value
before the violation is detected.

| Limit | Value | Mechanism |
|---|---|---|
| Per-process wall time | 15 s | **Enforced:** the process group is killed with SIGKILL at the deadline. |
| Output capture | 1 MiB per stdout/stderr stream, lower if remaining disk budget requires | **Enforced:** bounded capture; overflow is a capture failure. |
| Overall deadline | 45 min for the main run; another 45 min for the extension run | **Enforced** by the collector across phases. |
| Evidence bundle | 50 MiB (Increment 1 was 2.7 MB) | **Checked at session boundaries**, not continuously. The collector constant is currently 100 MiB; Q2 sets it to the approved value. |
| Core harness memory | 512 MiB | **Retrospective:** process peak RSS from the platform time tool is compared after exit. |
| HTTP server memory | No ceiling | **Observation only:** sampled server RSS is recorded and assessed under §5.5. No collector check exists, so no cap is claimed. |

The container `--memory` limit in Package 2 is a deployment example. It is not a measurement
control and is not used in Package 3.

**Stop conditions.** Reuse `evidence/p0-coverage-plan-and-p1-transition.md` §7.4, as implemented by
the collector:
- **Class 1** (oracle, fingerprint or binary-identity failure): immediate abort; the run is disqualified and raw logs are preserved.
- **Class 2** (environmental: power, sleep, competing processes): the session is invalidated and logs are preserved with the reason. The campaign does not continue on that session.
- **Class 3** (process failure: non-zero exit, timeout, retrospective RSS-ceiling violation, capture failure): the process group is terminated. The partial pair record, stdout and stderr are preserved, and **execution halts for diagnosis**. A Class 3 attempt is never a valid timing observation and is never replaced by a rerun inside the same campaign.

**Failure preservation:**
- Every attempt, including failed and invalidated ones, is retained.
- No selective reruns.
- After any stop, the cause is diagnosed and recorded. Execution resumes only as a new campaign directory against the same frozen binaries.
- If the cause is a product defect, the fix creates a new candidate, and §5.9 applies.

### 5.7 Candidate A

Candidate A is assessed only as part of the whole candidate. Cell outcomes are attributed to "v0.2.0
→ v0.3.0 candidate (Candidate A plus dependency bumps)", never to Candidate A alone. No speedup is
claimed for it. A REGRESSION on a core cell is handled by the owner disposition in §5.5. Reverting
Candidate A would create a new candidate and require a new qualification run.

### 5.8 Scope of the qualification

The qualification covers the candidate source identity on the measurement host, for the 9 cells.
It does not qualify CI-built release binaries, other targets, other engines or the deferred matrix.

### 5.9 Invalidation

The rule is dependency-aware. The question is whether a changed file is compiled into, or controls
the build of, a measured binary, not whether its path looks like a test or an example.

**Measured binaries and their inputs:**
- server: `knotrel-server` and `knotrel-core` sources;
- HTTP driver: `crates/knotrel-server/examples/measure_e2e.rs`, plus `crates/knotrel-server/tests/common/feeder.rs` (pulled in via `#[path]`), plus the `knotrel-server` library and dev-dependencies;
- core harness: `knotrel-benchmarks` at `df635a1`, plus candidate `knotrel-core` sources.

**Recorded source set.** Q3 records, in the campaign manifest, the union of source files listed in
the Cargo dep-info (`*.d`) files of these three binaries at the qualified SHA.

**A change between the qualified SHA and the release tag invalidates the qualification if:**
- it touches any path in that recorded source set, including helpers under `tests/common/` and any example source compiled into the driver; or
- it touches any `Cargo.toml` or `Cargo.lock`, `rust-toolchain.toml`, `.cargo/` configuration, build profiles or features; or
- it changes build flags in `release.yml`; or
- it changes the core harness or collector revision used.

**Exempt, if absent from the recorded source set:**
- `*.md` documentation, `ROADMAP.md` and release notes;
- integration-test files such as `crates/knotrel-server/tests/events.rs`;
- `examples/external_feeder.rs`;
- CI changes that do not alter build flags.

**Container and deployment files.** Changes to `examples/container/**` do not invalidate
performance qualification. They do require repeating the O1 checks and the O3 transcript, as do
changes to `examples/external_feeder.rs` or `tests/common/feeder.rs`.

**Check (R4.7).** Intersect `git diff --name-only <qualified>..<rc>` with the recorded source set
and the build-configuration paths above. The intersection must be empty.

## 6. Approval block (owner decisions)

**Approval record:** on 2026-11-12 the maintainer approved D1–D8 as recommended below, including the
reduced release scope, the proposed qualification tolerances and the disclosed inconclusive-result
policy, and authorized Packages 1 and 2 only. Package 3 (qualification) and Package 4 (release) were
not started by that authorization.

On 2026-11-12 the maintainer additionally approved the specific §5.2 amendment to rebuild both
baseline and candidate with Rust 1.100.0, and authorized the separate baseline preparation. All
other D4 criteria remain unchanged. The rebuilt baseline alone is not a campaign or a
final-candidate qualification; no campaign is to run in this step.

| # | Decision | Recommendation |
|---|---|---|
| D1 | Adopt the reduced v0.3.0 scope, the exclusions above and the ROADMAP text in §3 | **Approve.** |
| D2 | Record functional acceptance of M3.2–M3.5 and M4.1–M4.4 on the evidence cited in Package 1. M4.5 and Acceptance M4 are recorded after Package 2. | **Approve.** |
| D3 | Close P0 with a coverage exception. Increment 1 is reused for noise calibration and procedure only, after Q1. The A/A pilot and the Candidate A comparison remain historical only. **No retroactive approval of their protocols.** | **Approve.** |
| D4 | Approve the measurement protocol in §5 before execution: 9-cell matrix, contemporary B/C design, fixed driver and harness, 12 pairs with one extension to 24, tolerances 10% / 25% / 20%, budgets as defined in §5.6 (15 s enforced per-process timeout; 1 MiB enforced per-stream capture; 45 min enforced deadline; 50 MiB bundle checked at session boundaries; 512 MiB retrospective core-harness peak RSS; HTTP server RSS observed, not capped), Class 3 halt-for-diagnosis, and the dependency-aware invalidation rule (§5.9) | **Approve as written.** |
| D5 | Keep Candidate A in the candidate, with no benefit claimed | **Keep.** |
| D6 | Cells still inconclusive after the extension, with a median ≤ tolerance and all guardrails clean, are released with per-cell disclosure in the release notes | **Approve.** |
| D7 | Keep existing `import_in_progress` behavior unchanged as documented compatibility debt | **Keep.** |
| D8 | Release-note limitations: | **Approve.** |
| | - no authentication, authorization or TLS; | |
| | - no persistence (rebuild from source after restart); | |
| | - source-relative lag not reported; | |
| | - bounded deduplication window; | |
| | - single feeder owner with takeover; | |
| | - `ett`/`hdt` not re-qualified; | |
| | - performance qualified only for `compact-bfs`/`compact-workspace` on macOS arm64 for the 9 cells; | |
| | - no Prometheus endpoint; | |
| | - reference feeder and container are examples; | |
| | - `knotrel-server` publish dry run not fully verifiable before `knotrel-core` is published; | |
| | - no performance improvement claimed. | |

## 7. First executable task after approval (S1, executed 2026-11-12)

**S1 — Apply the approved scope to `ROADMAP.md`.**
- Insert the §3 text with the approval date.
- Check M3.2–M3.5 and M4.1–M4.4, each with a dated evidence line citing the tests and sections in Package 1.
- Mark M3.6, M5, M6 and P2 as deferred.
- Append the status pointer to `docs/performance-v0.3.0.md`.

Completion condition: `git diff --name-only` shows only `ROADMAP.md` and `docs/performance-v0.3.0.md`.
Every checked box cites its evidence and approval date. No deferred item is checked.
