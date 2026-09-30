## Summary

<!-- Brief summary of what this PR does and why. -->

Fixes # <!-- Issue number if applicable -->

## Type of Change

- [ ] `feat`: New feature or user-visible enhancement
- [ ] `fix`: Bug fix
- [ ] `perf`: Performance optimization or memory improvement
- [ ] `docs`: Documentation updates or additions
- [ ] `test`: New tests or test refactoring
- [ ] `refactor`: Internal code refactoring without behavior changes
- [ ] `ci`: CI/CD workflows, build scripts, or tooling

## Affected Components

- [ ] `knotrel-core` (Default compact core)
- [ ] `knotrel-core` (HDT dynamic engine)
- [ ] `knotrel-core` (Euler-tour forest / ReferenceGraph)
- [ ] `knotrel-server` (Axum HTTP API, state, admission)
- [ ] Benchmarks / Workflows / Documentation

## Algorithmic & Invariant Impact

<!-- If modifying core algorithms:
- Did you preserve the independent connectivity oracle?
- What are the time and space complexity bounds?
- Are update sequences and corner cases tested?
-->

## Verification Checklist

- [ ] Pinned toolchain: Built and tested with Rust `1.98.1`
- [ ] Tests pass: `cargo test --workspace --locked`
- [ ] Formatted: `cargo fmt --all -- --check`
- [ ] Clippy clean: `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] Documentation updated: Exported items documented and `cargo doc --workspace --no-deps --locked` clean
- [ ] Coverage considered: Checked via `bash scripts/coverage.sh`
- [ ] Benchmarks verified (for performance changes)
