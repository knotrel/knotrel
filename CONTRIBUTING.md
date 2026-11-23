# Contributing

Use the pinned Rust 1.100.0 toolchain and edition 2024. Preserve the core MSRV of
Rust 1.98.1, checked with `cargo +1.98.1 test --locked -p knotrel-core`. Keep
the core independent of networking and runtimes; implement HTTP concerns in the
Axum server.

Document every exported item in English. Document errors, mutation semantics and
examples where they help callers. Explain complex private code with invariants,
correctness reasoning, worst-case/expected/amortized time, space bounds and any
applicable paper or reference. A passing documentation lint does not establish
that the explanation is sufficient; review the content as well.

Tests should cover observable behavior. Algorithm changes must preserve the
independent connectivity oracle and include the update sequence that motivates
the change. Report benchmark methodology and results before claiming a speedup.

## Versioning

All artifacts use one version number (lockstep): `knotrel-core`,
`knotrel-server` and the console. A release bumps them together, even when a
component did not change; its release notes then say "no changes". To bump:

- `version` in `[workspace.package]` and the `knotrel-core` dependency
  version in the root `Cargo.toml`, then `cargo update -w` for `Cargo.lock`;
- `version` in `console/package.json`, then
  `npm install --package-lock-only` in `console/`;
- the reference feeder `integrations/postgres-feeder` (`version` in its
  `Cargo.toml`, then `cargo update -w` in that directory), which follows the
  same number although it is not published;
- `info.version` in `docs/openapi.yaml`, then `npm run generate:api` in
  `console/`.

**Release candidates.** The only prerelease form is `X.Y.Z-rc.N` (N from 1,
no leading zero, for example `1.0.0-rc.1`); the release workflows reject any
other suffix. A candidate carries the suffix everywhere above, in the tag
(`v1.0.0-rc.1`) and in its own release notes
(`docs/releases/v1.0.0-rc.1-release-notes.md`). The `knotrel-core`
requirement of the server is then exact, `version = "=1.0.0-rc.1"`: Cargo's
default caret requirement `1.0.0-rc.1` would also accept `1.0.0-rc.2` and
`1.0.0`, so a published server candidate could resolve a different core;
for a stable release it returns to the caret form (`version = "1.0.0"`). The
console's `knotrel.supportedServer` must name a prerelease of the same
`X.Y.Z` to admit it (npm range rules, see
[docs/console.md](docs/console.md#compatibility)).

Compatibility rules for a release (what may change, deprecation, the
console's supported server range) are in
[docs/compatibility.md](docs/compatibility.md); the stability class of each
public surface is in [docs/stability.md](docs/stability.md).

A change to the public Rust API (a new, changed or removed public item of
`knotrel-core` or `knotrel-server`) also updates, in the same change:
- the snapshot: `python3 scripts/public-api.py generate` rewrites
  `docs/api/public-api-knotrel-core.txt` and `docs/api/public-api-knotrel-server.txt`;
- `docs/api/public-api-classification.txt`: one line `<stable|experimental|internal> <key>`
  per new item and per new trait implementation (`impl <Trait> for <Type>`);
- `docs/api/public-api-policy.toml`, only with a maintainer decision: a new
  `#[non_exhaustive]` item, a narrowing (a stable item made experimental,
  internal or removed) or a cargo-semver-checks lint exception;
- `docs/stability.md`, if the item is at the crate root or its class differs
  from its type's.

`python3 scripts/public-api.py check` (CI job `public API`) fails otherwise.
It needs the workspace toolchain (`rust-toolchain.toml`) and compares the
classification with `origin/main` (or `--base <rev>`) to detect narrowings.

`python3 scripts/check-crate-docs.py` flags stale version references in the
crate documentation, and the console release workflow refuses a console
version that differs from the workspace version.

## Before submitting

Before submitting changes, run the four development commands in the README.
Keep Cargo.lock up to date and inherit workspace lints in any new crate. Avoid
adding dependencies without a concrete need. Do not change licensing implicitly.
