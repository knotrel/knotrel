# Startup engine configuration

Implement the approved design in the current checkout, preserving the staged work and existing public APIs. No commits or dependency changes.

1. Add core `EngineConfig` (default `CompactBfs`, experimental `EulerTour`) and an owned `ConnectivityGraph` wrapper with private backend state. Construction is infallible because every enum value is supported. Forward the existing graph contract without caching or runtime switching. Test growing/full-width IDs, replacement edges, errors and queries after cuts; preserve populated `Graph` conversion.
2. Add server `ServerConfig::new(address, engine, max_pending_jobs)` with validated positive admission capacity bounded by Tokio's supported maximum. `from_env` loads `KNOTREL_ADDR`, `KNOTREL_ENGINE`, `KNOTREL_MAX_PENDING_JOBS`, rejecting unknown `KNOTREL_` keys, non-Unicode values, unsupported engines and malformed values. Parse an isolated environment snapshot in tests, without mutating process environment.
3. Preserve `router(Graph)` and add `router_with_config(ServerConfig)` for an empty selected graph. Keep body (1 MiB) and batch (1024) limits fixed. Service holds `ConnectivityGraph`; admission and whole-batch mutex isolation remain unchanged. Add read-only `/v1/info` with actual engine, experimental status and limits, without locking the graph. Validate configuration and construct the router before binding in main; log selected settings.
4. Test HTTP behavior on both engines, immutable per-request selection, populated legacy router and configured admission. Test invalid startup configuration in subprocesses, before binding. Document the environment contract, effective info response, ephemeral state and absence of migration/hot reload. Run formatting, Clippy, workspace tests/doctests and Rustdoc with warnings denied; also check the dependent benchmark crate still compiles.

Review focus: an unsupported HDT name must fail rather than fall back; a configured ETT must own ETT state; rejected requests must not mutate; metadata must describe the actual backend; existing router callers retain their populated graph; admission must never panic on excessive capacity. No file loader, admin mutation endpoint, answer cache or HDT implementation is included in this configuration milestone.

## Completion record

- Core selection, server configuration, HTTP integration and documentation completed.
- New core/HTTP tests first failed on the absent APIs; the startup subprocess test first failed because the old server ignored an unsupported engine. All now pass.
- Final checks: 31 workspace tests and 2 doctests; formatting, Clippy and Rustdoc with warnings denied; dependent benchmark all-targets check.
- Real localhost smoke checks passed for both engines with a configured capacity of 3 and link/query/cut/query visibility. Socket checks required execution outside the filesystem/network sandbox; a sandbox-only run failed at the local bind, then passed with local socket access.
- Independent code review found no actionable defects. Its optional startup-order suggestion was incorporated using an occupied listener address. Admission remains tested deterministically at service level, with configured capacity wiring checked by HTTP metadata.
- No dependency changes, commits, pushes or index modifications. HDT and live migration remain separate milestones.
