# Deployment runbook: container and external feeder

This runbook covers one supported shape: a single `knotrel-server` process in
**source-managed** mode, run in a container, fed by one external feeder. The
container image and the reference feeder are **examples**. The image is not a
published release artifact, and the feeder is not a supported production
connector.

It restates behavior defined in the [HTTP API](http-api.md) and the
[integration contract](integration-contract.md); it adds no guarantee of its
own. Configuration is read once at startup; changing it requires a restart.

## Assumptions

- **Security.**
  - Knotrel has no authentication, authorization or TLS ([trust model](http-api.md#trust-model)).
  - Publish the port only on `127.0.0.1` or a trusted network. Anyone who can
    reach it can mutate the graph and take over the feeder session.
  - The image sets `KNOTREL_ALLOW_NON_LOOPBACK=true` because the server must
    bind `0.0.0.0` inside the container. Exposure is therefore controlled by
    the `docker run -p` binding.
- **Resources.**
  - Memory grows with graph size. Set a container memory limit (`--memory`)
    and the existing request and graph limits
    ([resource bounds](http-api.md#resource-bounds),
    [startup configuration](http-api.md#startup-configuration-and-engine-selection)).
  - Node and edge lifecycle support (`KNOTREL_TOPOLOGY_LIFECYCLE=true|false`,
    default `false`; [lifecycle](http-api.md#node-and-edge-lifecycle-opt-in);
    requires `KNOTREL_ENGINE=compact-bfs`, startup fails with `ett` or `hdt`)
    keeps a catalog of the registered topology in addition to the engine.
    Structurally this is about twice the plain engine's storage for an
    all-active graph; that is an estimate, and measured values are in the
    v0.6.0 qualification evidence. Engine compaction (after many removals)
    and imports need transient memory beyond the steady state. On candidate
    `48bc82f` with 100,000 nodes, compaction peaked at 40.7 MiB of net live
    heap against a 33.5 MiB steady maximum. A 100,000-node HTTP import peaked
    at 31–33 MiB (create) and 50 MiB (replace, old graph still live) for a
    final graph of about 23 MiB; v0.5.0 peaked at 111–130 MiB. Size memory
    for `old graph + import body + new graph`. Process RSS has no hard bound
    (allocator retention). Size `--memory` accordingly. With `false`
    no catalog is allocated.
  - There is **no durable graph persistence**. The graph, generation,
    revision, source frontier and deduplication history exist only in process
    memory and are lost on any restart. No volume is needed for graph state.
- **Ownership.**
  - Exactly one feeder session owns the graph, fenced by session/epoch and a
    lease of 1000–60000 ms.
  - A second feeder takes over the session; feeders are not load-balanced.
- **Source retention.**
  - After a restart or desynchronization, the **source** must be able to
    supply a complete authoritative snapshot for a rebuild by import. With
    lifecycle support that snapshot must include every node and edge flag,
    inactive nodes, disabled edges and isolated nodes: entries without flags
    are imported active and enabled
    ([integration contract §9.6](integration-contract.md)).
  - The source must retain events beyond the acknowledged frontier until they
    are acknowledged.
  - Normal eviction of old entries from the deduplication window
    (`KNOTREL_DEDUP_WINDOW_SIZE`, default 10000) requires no action and no
    rebuild; the ordered stream continues.
  - A rebuild is required only when an uncertain outcome must be resolved for
    an event no longer retained in that history: a timeout or lost
    acknowledgement whose exact retry returns 409 `unverifiable_duplicate`.
    Knotrel cannot then attribute the earlier outcome, so the recovery is a
    replacement import, not a skip or a blind resend
    ([integration contract §9.4](integration-contract.md)).
  - Reuse of an event ID beyond the retained window is not detected.

## Build

From the `knotrel` repository root:

```sh
docker build -f examples/container/Dockerfile -t knotrel-server:example .
cargo build --release --locked -p knotrel-server --example external_feeder
```

Files:
- [`Dockerfile`](../examples/container/Dockerfile): multi-stage build. A
  digest-pinned Rust bootstrap image installs Rust 1.100.0 for the server build.
  The runtime image runs as uid/gid `10001`, sets `KNOTREL_MODE=source_managed`
  and `KNOTREL_ADDR=0.0.0.0:8080`, and has a `/health` `HEALTHCHECK`.
- [`Dockerfile.dockerignore`](../examples/container/Dockerfile.dockerignore):
  BuildKit applies it because the build context is the repository root. It
  admits only `Cargo.toml`, `Cargo.lock` and `crates/`.

## Start

```sh
docker run -d --name knotrel -p 127.0.0.1:8080:8080 \
  --memory 512m --stop-timeout 15 knotrel-server:example
curl -s -w ' %{http_code}\n' http://127.0.0.1:8080/health   # {"status":"ok"} 200
curl -s -w ' %{http_code}\n' http://127.0.0.1:8080/ready    # {"status":"uninitialized"} 503
```

Add other `-e KNOTREL_…` variables at `docker run` time. Do not use `--rm` if
you need the exit code after stopping.

## Probes and states

- `/health` reports process liveness. It returns 200 in every state,
  including `stale`, `desynchronized` and `draining`.
- `/ready` returns 200 only when the graph is synchronized
  ([probes](http-api.md#probes)).
- Route traffic on `/ready`; restart the container only on `/health`.

**Metrics (opt-in).** Add `-e KNOTREL_METRICS_ENABLED=true` to serve
`GET /metrics` in Prometheus text format on the same port
([metrics](http-api.md#metrics-opt-in)). It is **unauthenticated** and
exposes operational state: route counts, backlog, rejection and
uncertain-outcome counts, graph node and edge counts, graph-job queue,
lock-wait and execution times, graph-operation and feeder-event outcome
counts, feeder rejection and desynchronization counts, feeder lease time
remaining, time since the last applied event or import, import progress,
results and phase durations, readiness and build version. Scrape it only from inside the same trusted boundary that may reach
the API. Neither Knotrel nor this image adds authentication or TLS for it.
A minimal Prometheus scrape job:

```yaml
scrape_configs:
  - job_name: knotrel
    static_configs:
      - targets: ["127.0.0.1:8080"]
```

`knotrel_readiness_state` mirrors `/ready`; it does not replace the probes.
`knotrel_feeder_last_applied_age_seconds` measures how long the graph has
been idle, not how far it lags the source: an idle source legitimately makes
it large. `knotrel_feeder_lease_remaining_seconds` reads `0` in the lease's final
millisecond, when feeder calls may already be refused, and goes negative once
the lease has expired for every check, including `/ready`.
`knotrel_imports_total{result="published"}` counts imports that became live,
including those whose caller timed out; a `503` import response is not by
itself a failed import. `result="abandoned"` counts imports whose caller went
away during the body upload, or whose worker never ran, before any worker
outcome existed.
`knotrel_catchup_duration_seconds{kind}` (`initial`, `replacement`) records the
elapsed time from import publication to the first `synchronized` barrier of
that generation; it includes any time spent `stale` during catch-up, and is
neither source lag nor proof of ongoing freshness.
Metrics reset on every restart, like the graph. Their overhead has not yet
been qualified (see the
[observability contract](observability-contract.md#6-m62-handoff)), so
enabling them in latency-sensitive deployments is at the operator's
discretion.

**Source-relative lag is not reported.** `catching_up` and `stale` are states,
not lag measurements. `ready` means the last barrier was verified and the
feeder lease is current. It does not mean the graph is within any time or
event distance of the source.

| `/ready` | Meaning | Operator action |
|---|---|---|
| 503 `uninitialized` | Started; no baseline imported | Start the feeder: it imports with `create_only`. |
| 503 `loading` | Initial import being built | Wait. On failure or cancellation it returns to `uninitialized`. |
| 503 `catching_up` | Image published; awaiting a verified barrier | Wait for the feeder to stream and declare its barrier. |
| 200 `ready` | Barrier verified; lease current | None. Readiness holds only while the feeder keeps heartbeating. |
| 503 `stale` | Feeder lease expired without renewal | Restart the feeder. A heartbeat alone does not restore `ready`; recovery needs a new session epoch plus a barrier, or a restart and rebuild. |
| 503 `desynchronized` | Unrepairable gap, conflicting duplicate, predecessor mismatch or source epoch reset | Rebuild by replacement import (`mode: "replace"` with `If-Match`), or restart and rebuild. Never skip events. |
| 503 `draining` | Shutdown in progress | None. It cannot be reversed. |

When a feeder exits, it stops heartbeating. After the lease expires (10 s for
the reference feeder), `/ready` becomes 503 `stale`. Readiness is therefore
claimed only at the instant it is probed.

Recovery procedures: [HTTP API, recovery procedures](http-api.md#recovery-procedures).

## Feed

```sh
./target/release/examples/external_feeder --addr 127.0.0.1:8080
```

The reference feeder performs the full lifecycle:
1. discovery through `/v1/info`;
2. session with a 10,000 ms lease and heartbeats every 2 s;
3. `create_only` import;
4. events;
5. barrier.

It then stops heartbeating and exits. It asserts that `/ready` is 503 at
start, so it needs a freshly started, unfed server. In `--addr` mode it does
not restart the server; restarts are driven externally as below.

## Stop and drain

```sh
docker stop knotrel                                  # SIGTERM
docker inspect -f '{{.State.ExitCode}}' knotrel      # 0
```

- `--stop-timeout` (15 s here) must exceed `KNOTREL_SHUTDOWN_TIMEOUT_MS`
  (default 10 s). Otherwise Docker sends SIGKILL during the drain.
- The server switches `/ready` to `draining`, finishes admitted work and exits
  ([graceful shutdown](http-api.md#graceful-shutdown)).

| Exit status | Meaning |
|---|---|
| 0 | Drained cleanly |
| 1 | Configuration or bind error |
| 2 | Drain deadline exceeded |
| 130 | Second signal during the drain |

## Restart and rebuild

```sh
docker restart knotrel
curl -s http://127.0.0.1:8080/ready   # {"status":"uninitialized"}: state was lost
./target/release/examples/external_feeder --addr 127.0.0.1:8080
```

After any restart:
- the server has a new `instance_id` and an empty graph;
- the feeder must rediscover through `/v1/info` and perform an authoritative
  `create_only` import from the source.

## Gateway with TLS and authentication

Knotrel has no TLS or authentication of its own. The supported way to expose
it beyond a trusted host is a gateway in front of it, with Knotrel reachable
only by that gateway. [`examples/gateway/`](../examples/gateway/compose.yaml)
is a validated example (roadmap 0.9.5):

```sh
examples/gateway/setup.sh        # TLS certificate and credentials, into .generated/ (ignored by git)
docker compose -f examples/gateway/compose.yaml up --build
examples/gateway/smoke-test.sh   # the checks below; also run by the gateway CI workflow
```

- **Knotrel is loopback-only.** It binds `127.0.0.1:9080`
  (`KNOTREL_ALLOW_NON_LOOPBACK=false`). The console and the gateway share its
  network namespace (`network_mode: service:knotrel`), so only the gateway's
  TLS listener (8443, published on the host's loopback) is reachable.
- **TLS** 1.2 or 1.3 only; plain HTTP and older protocol versions are refused.
  The example's certificate comes from a local CA generated by `setup.sh`;
  use your own certificates in production.
- **Authentication and authorization at the gateway** (HTTP Basic over TLS,
  separate credential files):

  | Credential | Allowed |
  | --- | --- |
  | operator | console UI, `/v1/*` except the feeder routes, `/metrics`, probes |
  | feeder | `/v1/feeder/*`, probes |

  A credential cannot use the other's routes. Behind the gateway the console
  still applies its own route allowlist and `Host`/`Origin` checks.
- **What the smoke test checks:** TLS with valid credentials; refusal without
  or with wrong credentials; operator queries and mutations; the operator
  refused on feeder routes and the feeder refused on the operator API, while
  feeder requests reach Knotrel; the console and its API through the
  gateway; plain HTTP and TLS 1.1 refused; Knotrel unreachable from the host
  and from another container on the Compose network.
- **Limit of the example:** the console container listens on port 8080 of the
  shared namespace on all interfaces (its image does not offer a loopback-only
  listener). The Compose network has no other members, so nothing else can
  reach it; in another layout, keep the console on a network that only the
  gateway joins. HTTP Basic with static files is an example, not a
  recommendation: use the organization's identity provider at the gateway.

## Upgrade between minor versions

Knotrel keeps no state on disk, so an upgrade carries nothing over: the new
process starts empty and is rebuilt like after any restart. Compatibility
rules are in [compatibility.md](compatibility.md).

1. **Before:** read the release notes for deprecated or changed surfaces and
   for new `KNOTREL_*` settings (new settings default to the previous
   behavior). Check that the console's supported server range
   (`knotrel.supportedServer`, [console guide](console.md)) includes the new
   version; console and server are released together and upgraded together.
2. **Stop** the old server with SIGTERM and let it drain
   ([Stop and drain](#stop-and-drain)).
3. **Start** the new version with the same configuration.
   - **Source-managed:** the feeder sees a new `instance_id` and rebuilds the
     graph from the source (`create_only` import, catch-up, barrier). `/ready`
     answers 503 until then; route traffic by `/ready`. The reference
     PostgreSQL feeder does this on its own (a new snapshot and slot; the old
     slot is dropped).
   - **Standalone:** the graph is empty; clients must load it again.
4. **Verify:** `GET /v1/info` (engine and settings), `GET /ready` returns
   200, graph counts in `GET /v1/stats` match the source, and the console's
   Overview shows the expected server.
5. **Rollback** is the same procedure with the previous version: nothing was
   persisted, so nothing has to be converted back.

Without downtime: start the new version as a second instance with its own
feeder (in the reference feeder, a distinct slot prefix), wait until it is
`ready`, switch the gateway's upstream to it, then stop the old instance.

The upgrade path is tested end to end by the PostgreSQL feeder's
`upgrade_from_previous_release` test: a server of the previous release is
fed and converged, stopped with SIGTERM, replaced by the current build on the
same port, and the feeder's rebuild converges again
([postgres-feeder workflow](../.github/workflows/postgres-feeder.yml)).

## Transition evidence

The transcript below observes these transitions:
- `uninitialized` → `ready` after feeding;
- `ready` → `stale` after feeder exit;
- restart → `uninitialized` → `ready`;
- `draining` → exit 0.

The remaining documented transitions are covered by tests in
`crates/knotrel-server/tests/`:

| Transition | Test |
|---|---|
| `loading` cannot be promoted by a barrier | `bootstrap.rs` `test_2_initial_loading_cannot_be_promoted_by_a_barrier` |
| Failed or cancelled import → `uninitialized` | `bootstrap.rs` `test_3_initial_import_failure_or_cancellation_reverts_loading_to_uninitialized` |
| Import → `catching_up` | `bootstrap.rs` `test_4_successful_import_remains_catching_up` |
| Heartbeat after expiry stays `stale`; a new epoch plus a barrier restores `ready` | `e2e_rebuild.rs` `test_07_lease_expiry_cannot_be_repaired_by_heartbeat_alone`; `bootstrap.rs` `test_13_heartbeat_after_expiry_does_not_restore_ready` |
| `desynchronized` → rebuild | `events.rs` `test_19_stop_event_application_after_desynchronization_and_rebuild_recovery`; `e2e_rebuild.rs` `test_23_recovery_helper_requires_rebuild_after_desynchronizing_rejection` |
| `unverifiable_duplicate` → replacement import | `e2e_rebuild.rs` `test_11_unverifiable_duplicate_requiring_safe_recovery` |
| `draining` cannot be reversed | `bootstrap.rs` `test_16_draining_cannot_be_reversed_by_feeder_requests` |

## Appendix: verification transcript

The full sequence is scripted in
[`check-deployment.sh`](../examples/container/check-deployment.sh):
- it uses bounded polls rather than fixed sleeps as evidence;
- it probes `/ready` immediately after each feeder exit;
- it exits non-zero on any unexpected outcome.

```sh
FEEDER=target/release/examples/external_feeder examples/container/check-deployment.sh
```

**Recorded 2026-11-12** on macOS arm64 with Docker Engine 29.1.2 (linux
aarch64 VM). Source: a working tree based on `cdb0e5b` with the Package 1/2
documentation and example files uncommitted.

This is **not** the release-candidate record. Per the closure plan, the
transcript must be repeated from the release-candidate SHA. It must also be
repeated after any later change to `examples/container/**`,
`crates/knotrel-server/examples/external_feeder.rs`,
`crates/knotrel-server/tests/common/feeder.rs` or the server sources.

For this run the feeder was built with a separate `CARGO_TARGET_DIR`, which
left the preserved measurement driver under `target/` untouched.

Image build: `docker build -f examples/container/Dockerfile -t knotrel-server:example .`
exited 0, producing image `sha256:ba5b4404dca6…`.

```text
== image and feeder
image=sha256:ba5b4404dca67c33b2c602fb3c4c0a88c70a8ee0c929b0e517ed7cf3576fc720 feeder=../knotrel-deploy-check/target/release/examples/external_feeder out=../knotrel-deploy-check/script-run
== 1. start container (source-managed, published on 127.0.0.1 only)
  PASS docker run: 0
  PASS container user: 10001:10001
  PASS uid inside container: 10001
[20:51:17.665] /health 200
  PASS docker HEALTHCHECK: healthy
== 2. readiness before any feeder request
  PASS /ready at startup: 503 {"status":"uninitialized"}
== 3. feeder run 1 (initial import, catch-up, barrier)
[20:51:23.066] feeder exit=0; /ready probed 0.045 s after exit -> 200 {"status":"ready"}
  PASS feeder exit status: 0
  PASS /ready immediately after feeder: 200 {"status":"ready"}
== 4. lease expiry after feeder exit (bounded 30 s poll)
[20:51:33.574] stale observed 10.496 s after feeder exit
  PASS /ready after lease expiry: 503 {"status":"stale"}
  PASS /health while stale: 200
== 5. restart (graph state is lost)
  PASS docker restart: 0
[20:51:33.934] /health 200
  PASS /ready after restart: 503 {"status":"uninitialized"}
== 6. feeder run 2 (authoritative rebuild)
[20:51:34.056] feeder exit=0; /ready probed 0.052 s after exit -> 200 {"status":"ready"}
  PASS feeder exit status: 0
  PASS /ready immediately after feeder: 200 {"status":"ready"}
== 7. stop (SIGTERM, drain)
  PASS docker stop: 0
  PASS container exit code: 0
  PASS OOM killed: false
2026-11-12T20:51:33.628259Z  INFO knotrel_server: shutdown requested
2026-11-12T20:51:33.628338Z  INFO knotrel_server::serve: draining open_connections=0 outstanding_jobs=0 timeout_ms=10000
2026-11-12T20:51:33.628464Z  INFO knotrel_server: shutdown complete
2026-11-12T20:51:34.092207Z  INFO knotrel_server: shutdown requested
2026-11-12T20:51:34.092236Z  INFO knotrel_server::serve: draining open_connections=0 outstanding_jobs=0 timeout_ms=10000
2026-11-12T20:51:34.092303Z  INFO knotrel_server: shutdown complete
== result: 0 failure(s); logs in ../knotrel-deploy-check/script-run
exit=0
```

Notes:
- The two drain sequences in the log come from the `docker restart` (first)
  and the final `docker stop` (second). Each drained with no outstanding work.
- Each feeder log shows 34 passed checks. In `--addr` mode the feeder's own
  restart step prints only its header and is skipped; the restart here is
  external.
- The server's startup warning still reads "v0.1.0 has no authentication…".
  That text is in production code and is intentionally left unchanged in this
  package.

**Rust 1.100.0 revalidation, 2026-11-12:** The updated container was built on
the same platform from the uncommitted working tree, using the digest-pinned
bootstrap image. The separately built Rust 1.100.0 feeder and image
`sha256:77895d533a3ab9a7176e9686d27299a7935a832435cd2cbb11b3ae94cf5292d2`
passed `check-deployment.sh` with zero failures. Startup was `uninitialized`;
readiness was `ready` immediately after each feeder exit; lease expiry produced
`stale`; restart returned to `uninitialized`, followed by a successful rebuild;
and shutdown exited 0 without an OOM kill. This remains a pre-RC check and does
not replace the required RC-SHA transcript.

**Candidate deployment check, 2026-11-13:** The image and reference feeder were
built from clean commit `a076c031a94e9e72c8b80b78e1dc512f51599220` on macOS
arm64 with Docker Engine 29.1.2 and a Linux arm64 VM. Both builds exited 0.
The image ID was
`sha256:4240c876759512a00c96d3118aa8275ccee66951b5602b6500a87181faa057fe`;
the feeder SHA-256 was
`3a95772ef9691393d621cffe8cbf07ac2e2ee2c86dec1f232be41f48aa0ea0bb`.
The [full transcript and logs](../../evidence/v0.3.0-package4-local-20261113T063235Z-a076c03/deployment/transcript.txt)
are retained in the maintainer workspace. Times below are UTC:

```text
06:33:40.202 /health 200; container user and uid 10001; HEALTHCHECK healthy
             /ready 503 {"status":"uninitialized"} before feeding
06:33:45.922 feeder 1 exit 0; /ready 200 {"status":"ready"} after 0.049 s
06:33:56.397 /ready 503 {"status":"stale"} after lease expiry; /health 200
06:33:56.749 restart exit 0; /ready 503 {"status":"uninitialized"}
06:33:56.867 feeder 2 exit 0; /ready 200 {"status":"ready"} after 0.050 s
             docker stop exit 0; container exit 0; OOMKilled false
             check-deployment.sh exit 0; 0 failures
```

The feeder stops heartbeating when it exits; the two readiness results apply
only at the recorded probe instants. This check applies to the recorded commit
or a documentation-only descendant. Changes to the container, feeder, feeder
helper or server sources require repeating it.
