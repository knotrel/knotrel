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
  `npm install --package-lock-only` in `console/`.

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
