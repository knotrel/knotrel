# Development requirements

- Use Rust 1.100.0, edition 2024, and the existing Cargo workspace. Preserve
  the `knotrel-core` MSRV of Rust 1.98.1 and verify it separately.
- Keep graph algorithms synchronous in `knotrel-core`; HTTP belongs in `knotrel-server` and uses Axum.
- Write code, comments, Rustdoc and project documentation in English.
- Document every public item, including structs, enums, variants, fields, traits, functions and methods. Explain errors and provide examples where useful.
- Document nontrivial private algorithms: invariants, correctness, time/space complexity, and whether bounds are worst-case, expected or amortized. Cite sources when implementing published algorithms.
- Inherit workspace lints in every crate. Do not suppress missing documentation or introduce unsafe code.
- Preserve Cargo.lock. Use stable Rust features and minimal dependencies.
- Verify formatting, Clippy with warnings denied, workspace tests/doctests, and Rustdoc with warnings denied.
- Keep benchmarks reproducible. Never present unmeasured performance claims or compare different consistency/result contracts.
- Do not commit or push unless the user explicitly asks for it.

## Before release

Follow [the release procedure](docs/releases/release-procedure.md); this list
covers what is easy to miss. Check every item on the exact candidate commit.

- **Crate docs (crates.io and docs.rs):** for every item under "What is new"
  in the release notes, update:
  - `crates/*/src/lib.rs` crate-level `//!` docs (the docs.rs landing page),
    with a doctest where an example helps;
  - `crates/*/README.md` (the crates.io page);
  - the main `README.md`, `docs/http-api.md` and `docs/openapi.yaml` for any
    new public type, `KNOTREL_*` variable or HTTP operation.
  Mark unreleased features as such until publication, and never link to
  release assets that do not exist yet.
- **Preview what will be published:** `cargo package --list -p <crate>` and
  `cargo doc --no-deps -p <crate>`; run `python3 scripts/check-crate-docs.py`
  (it checks version references only, not coverage).
- **Versions:** workspace version, the `knotrel-core` dependency version and
  `Cargo.lock` agree with the release.
- **User-facing performance pages:** add the release to
  `docs/performance-by-version.md` and the README performance section, with
  no claim beyond the qualification and its maintainer decisions.
- **Release notes:** the "Outstanding release gates" section reflects the
  current state; drop "draft" only at release.
- **Evidence:** release code of the final commit identical to the measured
  candidate (or re-measured); raw evidence stored with checksums.
- **CI and dry run:** all CI on all three platforms and the release dry run
  passed on the final commit; the deployment and rebuild smoke test repeated.
- **Tags:** signed tag (`git tag -s`, `git verify-tag`) in knotrel and the
  matching tag in knotrel-benchmarks.
- **After publication:** remove "unreleased" labels, update installation
  examples and the roadmap.

## Git & Naming Rulesets

- **Branch Naming**: All branches must follow the pattern:
  `^(feature|bugfix|fix|hotfix|docs|chore|refactor|test|ci|dependabot)/.+$`
  Allowed prefixes: `feature/`, `bugfix/`, `fix/`, `hotfix/`, `docs/`, `chore/`, `refactor/`, `test/`, `ci/`, `dependabot/`.
- **Commit Messages**: All commits and PR titles must follow Conventional Commits matching:
  `^(build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test)(\([a-z0-9_./-]+\))?!?: .+$`
  Examples: `feat(core): add pruning`, `fix(server): handle zero capacity`, `chore: update dependencies`.
