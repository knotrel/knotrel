# Security Policy

## Supported Versions

Security fixes are released for the latest minor release line only, as a
patch release. The server, the `knotrel-core` and `knotrel-server` crates and
the console share one version number (lockstep, see
[CONTRIBUTING.md](CONTRIBUTING.md)), so this table applies to all of them.
Unreleased code on `main` is not a supported version.

| Version  | Supported          |
| -------- | ------------------ |
| 0.9.x    | :white_check_mark: |
| < 0.9    | :x:                |

When 1.0.0 is released, 1.0.x replaces 0.9.x and 0.9.x stops receiving fixes.

## Supported exposure

Knotrel has no authentication, authorization or TLS of its own. It is
supported on loopback, on a network whose every host is trusted, or behind a
gateway that provides TLS and access control. Exposure to untrusted clients is
unsupported. Behavior that follows from this trust model (for example that any
client inside the boundary can mutate the graph or take over the feeder
session) is documented, not a vulnerability. See the
[threat model](docs/threat-model.md), the
[trust model](docs/http-api.md#trust-model) and the
[deployment guide](docs/deployment.md#gateway-with-tls-and-authentication).

Reports are welcome for anything that breaks the documented mitigations, for
example: a request that bypasses the body, batch, graph or connection limits;
an error or log line that leaks request content; a way around feeder session
fencing by a stale feeder; a console route outside its allowlist; or a
weakness in a published artifact.

## Reporting a Vulnerability

Please do **not** open a public issue, pull request or discussion for a
suspected vulnerability.

Report it privately through
[GitHub Private Vulnerability Reporting](https://github.com/knotrel/knotrel/security/advisories/new)
(the repository's **Security** tab, **Report a vulnerability**). Include:

- the affected version or commit and the configuration (mode, engine,
  relevant `KNOTREL_*` settings, exposure);
- the type of issue (for example denial of service, memory exhaustion, panic
  through HTTP, information disclosure);
- minimal steps or a payload to reproduce it;
- your assessment of the impact.

You can expect an initial acknowledgment within 48 hours and regular updates
on triage and mitigation. Fixes are published as a patch release of the
supported line with a GitHub security advisory.
