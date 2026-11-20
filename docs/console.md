# Knotrel Console

The Knotrel Console is a browser UI for operating one Knotrel server. It shows
liveness, readiness, configuration, graph counts, identity and version
metadata and synchronization status. It runs connectivity and lifecycle
queries and, on standalone servers, topology operations.

> **Status: unreleased.** Console 0.1.0 is prepared for the Operational
> Console milestone. No container image or archive is published yet.
> Commands that reference `ghcr.io/knotrel/knotrel-console:0.1.0` or a
> release archive work only after the first authorized release.

The console is a static React application. It is versioned independently of
the Rust crates and distributed in two forms:

| Distribution | Use | Status |
| --- | --- | --- |
| Container image `ghcr.io/knotrel/knotrel-console:<version>` | Run next to Knotrel, locally or on a server | Primary; not yet published |
| Static archive `knotrel-console-<version>.tar.gz` (+ `.sha256`) on the GitHub Release `console-v<version>` | Host behind an existing web server or gateway | Primary; not yet published |
| npm package with a local launcher | `npx` quick start | Deferred (separate increment) |

The server binary, the console image and the archive are independent
artifacts. Cargo builds and server installations never need Node.js, and the
Rust binary embeds no console assets.

## Choose a path

- **[Local start with Docker](#local-start-with-docker):** a few commands start
  a server and the console on loopback.
- **[Install in your environment](#install-in-your-environment):** serve the
  static archive from your gateway, with your TLS and access control.

## Local start with Docker

Requirements: Docker with Compose. From a checkout of this repository:

```sh
docker compose -f console/deploy/compose.yaml up --build
```

Open <http://127.0.0.1:7380/>. The example starts a standalone server with
lifecycle support enabled. Only the console port is published, on loopback;
the server is reachable only from the console container. Graph state is in
memory and lost when the server stops. Stop with Ctrl+C, then:

```sh
docker compose -f console/deploy/compose.yaml down
```

To use the console with a server you already run, start only the console
image (after the first release, or with an image built by
`docker build -t knotrel-console:dev console`):

```sh
docker run --rm -p 127.0.0.1:7380:8080 \
  -e KNOTREL_UPSTREAM=http://host.docker.internal:8080 \
  --read-only --tmpfs /tmp --cap-drop ALL \
  ghcr.io/knotrel/knotrel-console:0.1.0
```

The server must be reachable from the container. A server bound to the host's
loopback is not reachable through `host.docker.internal` on Linux. Prefer a
shared Docker network, as in the Compose example. Do not expose an
unauthenticated server just to reach it.

### Container configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_UPSTREAM` | required | Server base URL, `http(s)://host[:port][/path]`, no credentials, query or trailing slash. Fixed for the container's lifetime. |
| `KNOTREL_CONSOLE_SERVER_NAMES` | `localhost 127.0.0.1 [::1]` | Accepted `Host` names (space separated). Requests for any other host get 421. Add the public name when a gateway forwards to the container. |
| `KNOTREL_CONSOLE_SERVER_LABEL` | the upstream URL | Text shown as "Server address" in the UI. |
| `KNOTREL_UPSTREAM_TIMEOUT` | `35s` | Upstream send/read timeout. It exceeds the server's 30 s request timeout so that the server's `request_timeout` is reported when it applies. |

Invalid values stop the container at startup. The image runs nginx as uid
101, listens on port 8080, works with a read-only root filesystem and a tmpfs
at `/tmp`, and has a health check on `/healthz`. That endpoint reveals nothing
about the server.

## Install in your environment

1. Download `knotrel-console-<version>.tar.gz` and its `.sha256` from the
   GitHub Release `console-v<version>`, then verify and extract them:

   ```sh
   sha256sum -c knotrel-console-0.1.0.tar.gz.sha256
   tar -xzf knotrel-console-0.1.0.tar.gz
   (cd knotrel-console-0.1.0 && sha256sum -c SHA256SUMS)
   ```

2. Serve `knotrel-console-<version>/html/` as static files.
3. On the **same origin**, forward exactly these routes to one Knotrel server,
   stripping the `/api` prefix:

   | Console request | Server request |
   | --- | --- |
   | `GET /api/health` | `GET /health` |
   | `GET /api/ready` | `GET /ready` |
   | `GET /api/v1/info` | `GET /v1/info` |
   | `GET /api/v1/stats` | `GET /v1/stats` |
   | `POST /api/v1/operations` | `POST /v1/operations` |

   Forward nothing else: feeder endpoints, `/v1/batch` and `/metrics` must not be
   reachable through the console origin. Do not retry upstream requests
   (`proxy_next_upstream off` in nginx): a retried mutation could be applied twice.
4. Put TLS and access control in front of both the UI and `/api`.
5. Optionally serve `console-config.json` next to `index.html`, for example
   `{"serverLabel":"knotrel-prod-1"}`, to show which server the gateway uses.

The archive includes `nginx/knotrel-console.conf`, a commented nginx example.
The console uses the relative prefix `api/`, so the same build works at `/` or
under a sub-path, with no rebuild per installation.

### Optional gateway signal

The bundled container sets `X-Knotrel-Console-Proxy` on answers it generates
itself. Your gateway may set it too:

| Value | Meaning | How the console treats a mutation |
| --- | --- | --- |
| `upstream_unreachable` | No byte reached the server (connection refused or connect timeout). | Not delivered (rejected). |
| `upstream_failed` | The connection failed after the request may have been sent. | Outcome uncertain. |
| `upstream_timeout` | No answer within the gateway timeout. | Outcome uncertain. |
| `rejected` | The gateway refused the request (route, method, origin, size). | Rejected. |

Without the header, a gateway error is shown as its plain HTTP status. A 5xx
answer to a mutation is then treated as uncertain.

## Trust boundary

- **The console adds no authentication.** Neither does the Knotrel API. Anyone
  who can load the console can read the graph and, on a standalone server, change
  it. Expose it only through a gateway that provides TLS and access control, or
  keep it on loopback for one trusted user.
- **The upstream is fixed by operator configuration.** The browser cannot name a
  server. Only the allowlisted routes are forwarded, without cookies,
  `Authorization` or forwarding headers.
- **Browser-origin attacks.** Binding to loopback does not stop a malicious web
  page from sending requests to `127.0.0.1`. The container answers only requests
  whose `Host` is an accepted name (421 otherwise), which defeats DNS rebinding.
  It rejects API requests whose `Origin` names another host or whose
  `Sec-Fetch-Site` is not `same-origin`, which defeats cross-site request forgery.
  A strict Content Security Policy forbids inline scripts and framing.
- **Bounds.** Request bodies are limited to 64 KiB. Upstream connect (5 s), send
  and read timeouts are bounded. Upstream responses are not size-limited by the
  container; the server's own responses are small.
- **Logs.** The container logs client address, method, path without query string,
  status, size and duration. It never logs bodies, headers, cookies or
  credentials.
- **Server-provided text** (error messages) is always rendered as text, never
  as HTML.

## Capabilities and API mapping

Every value comes from these endpoints. Values the API does not expose are
shown as "Unavailable", with the reason; they are never inferred.

| Screen | Endpoint | Fields used |
| --- | --- | --- |
| Overview: connection | all reads | transport result, `X-Knotrel-Console-Proxy`, time of the last success and failure count |
| Overview: liveness | `GET /health` | HTTP status, `status` |
| Overview: readiness | `GET /ready` | HTTP status (200/503), `status` (all nine labels) |
| Overview: server | `GET /v1/info` | `engine`, `experimental`, `mode` (absent means standalone), `topology_lifecycle`, `max_pending_jobs`, `max_body_bytes`, `max_batch_operations` |
| Overview: graph | `GET /v1/stats` | `node_count`, `edge_count`, `active_node_count`, `enabled_edge_count`, `effective_edge_count`, `max_*`, `remaining_*`, `state_version`, `sync_status` |
| Overview: identity and version | `GET /v1/stats` headers | `X-Knotrel-Instance-Id`, `X-Knotrel-Generation`, `X-Knotrel-State-Version`, `X-Knotrel-Source-Position` (decoded), `X-Knotrel-Sync-Status` |
| Graph/Queries | `POST /v1/operations` | `connected`, `node_state`, `edge_state` outcomes and the version headers of the same response |
| Synchronization | `GET /v1/stats`, `GET /ready`, `GET /v1/info` | `sync_status`, `generation`, `source_position`, `instance_id`, readiness label |
| Operations | `POST /v1/operations` | `add_node`, `link`, `cut`, `set_node_active`, `set_edge_enabled`, `remove_node`; `changed`, error envelope, `X-Knotrel-State-Version` |

Not exposed by the HTTP API, so shown as unavailable: the server software
version, server execution time, source lag, feeder lease remaining time and
last-event age. The last two are Prometheus metrics when
`KNOTREL_METRICS_ENABLED=true`; the console does not read `/metrics`.

### Mode and capability rules

| Server | Queries | Topology operations |
| --- | --- | --- |
| Standalone, lifecycle off | `connected` | `add_node`, `link`, `cut` |
| Standalone, lifecycle on (`compact-bfs`) | `connected`, `node_state`, `edge_state` | all six, including permanent removal (typed confirmation) |
| Source-managed | `connected` (and `node_state`/`edge_state` with lifecycle) | none. Changes belong in the authoritative source and its feeder. The console never sends session, heartbeat, import, event, gap or barrier requests. |

Unsupported actions are disabled with an explanation. They are not hidden
silently.

## Behavior

- **Exact values.** Responses are parsed losslessly: JSON numbers keep their
  source text. Generations, counts and limits are shown as exact decimal strings,
  and node IDs and revisions are never converted to a JavaScript `number`. Node
  IDs must be ASCII digits up to 18446744073709551615. They are sent exactly as
  typed; the server normalizes leading zeroes, and the form says so.
- **Runtime validation.** Generated OpenAPI types describe shapes only. Every
  response is validated at runtime. Malformed or incompatible responses are
  reported as such. Unknown error codes are shown with their code, not dropped.
- **Independent reads.** The overview refreshes liveness, readiness,
  configuration and statistics as four independent requests, every 5 s while the
  page is visible. Each is shown with its own time and browser round-trip. Only
  `/v1/stats` and its headers form one coherent server snapshot; the page never
  claims that separate reads are atomic.
- **Bounded polling.** Refreshes never overlap. A manual refresh cancels the
  obsolete one. After errors the interval doubles up to 60 s. Polling pauses
  while the tab is hidden and refreshes immediately when it is shown again. Data
  older than three intervals, or from before a failed refresh, is marked stale.
- **Durations.** Displayed durations are browser round-trips. The API reports
  no server execution time.
- **Mutations.** Each mutation is sent once and never retried automatically.
  Outcomes are *effective* (`changed: true`, with the new state version), *no-op*,
  *rejected* (structured error or not delivered) or *uncertain*: a timeout,
  connection loss after sending, server `request_timeout` or an undecodable
  success. A timeout is not a rollback. For uncertain outcomes the console offers
  a read-back query and explains its limits. For example, connectivity does not
  prove that a specific edge exists.
- **Restarts.** A change of `X-Knotrel-Instance-Id` is announced. Earlier
  results describe the previous, in-memory instance.
- **History.** The last 50 operations of the browser session are kept in memory
  only.

## Compatibility

The console declares the server versions it supports in
`console/package.json` (`knotrel.supportedServer`, currently `>=0.7.0 <0.9.0`).
It also records the SHA-256 of the `docs/openapi.yaml` revision it was
generated from (`knotrel.openapiSha256`), which the archive's
`console-manifest.json` repeats. CI fails when the generated types or that
digest are stale. Browser tests run against the server built from the same
commit. The server exposes no software version, so the console cannot check
compatibility at runtime; it reports incompatible responses instead.

## Verified platforms and limitations

| Area | Coverage |
| --- | --- |
| Browsers | Playwright on Linux: Chromium and Firefox (CI matrix). WebKit/Safari is not tested. |
| Container | Linux runners (amd64); the release builds linux/amd64 and linux/arm64, and smoke-tests the amd64 image. |
| Static build and archive | Built and unit-tested on Linux, macOS and Windows. CI checks that the three archives are byte-identical. |
| Accessibility | axe-core checks (WCAG 2 A/AA, serious and critical findings fail) and keyboard-only flows. This is not a full manual audit. |

Out of scope: graph visualization, path reconstruction, multi-server
management, bulk import, log ingestion, historical metrics, authentication in
the server, persistence and multi-graph.

## Development

See [console/README.md](../console/README.md) for commands and
[docs/releases/console-release.md](releases/console-release.md) for the
release procedure.
