# Knotrel Release Procedure

This document specifies the reproducible verification and publication protocol for Knotrel releases.

> [!IMPORTANT]
> **No implicit releases**: Creating Git tags, pushing tags, or publishing release artifacts requires explicit project maintainer authorization. Automated CI or pull requests must never publish releases.

---

## 1. Distribution Model & Prerequisites

- **Licensing**: All distributed source and binary packages must carry the **Elastic License 2.0 (ELv2)**.
- **Toolchain**: Build and test with the pinned compiler: **Rust 1.98.1** (`rust-toolchain.toml`).
- **Supported targets**:
  - `x86_64-unknown-linux-gnu` (Linux x86_64, ELF 64-bit)
  - `aarch64-apple-darwin` (macOS Apple Silicon, Mach-O 64-bit arm64)
  - `x86_64-pc-windows-msvc` (Windows x86_64, PE32+ executable)
- **Registry Independence**: Knotrel packages are source-available preview packages (`publish = false`) and are **not published to crates.io**.
- **Lineage Constraint**: Candidate commits for release must belong to the authorized release lineage (`origin/main`).

---

## 2. Pre-Release Verification Protocol

Before initiating a release, the maintainer executes the following gates on a clean checkout of `main`:

### 2.1 Workspace Quality Gates

```sh
# 1. Format check
cargo fmt --all -- --check

# 2. Strict Clippy check
cargo clippy --workspace --all-targets --locked -- -D warnings

# 3. Documentation check with denied warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

# 4. Full workspace tests
cargo test --workspace --locked

# 5. Fault-injection lifecycle tests (test-support feature)
cargo test --workspace --locked --features knotrel-server/test-support

# 6. Runnable examples
cargo run -p knotrel-core --example basic
cargo run -p knotrel-core --example bulk_import
cargo run -p knotrel-core --example engine_selection
```

### 2.2 Isolated Source Distribution Verification

Validate that both crate packages can be extracted into an isolated directory outside the repository and built offline without resolving to the repository checkout:

```sh
./scripts/verify-source-packages.sh
```

This verifies:
- Locked dependencies are pre-fetched (`cargo fetch --locked`) so verification is independent of pre-warmed Cargo caches.
- `knotrel-core-0.1.0.crate` and `knotrel-server-0.1.0.crate` are packaged offline.
- `LICENSE.md` exists in both archives and matches the Elastic License 2.0 text.
- An isolated patch `[patch.crates-io]` in `.cargo/config.toml` configures dependency resolution.
- `cargo metadata` proves the canonical manifest path of `knotrel-core` resolves exactly and exclusively from the isolated extracted sibling directory.
- `cargo check --offline --locked` and `cargo test --offline --locked --no-run` succeed.

### 2.3 Release Binary Build & Black-Box Smoke Test

Compile `knotrel-server` in release mode and execute black-box process tests:

```sh
# 1. Build release binary
cargo build --release --locked -p knotrel-server

# 2. Run automated black-box smoke tests (Unix)
./scripts/smoke-test-release-binary.sh target/release/knotrel-server

# On Windows (PowerShell):
# powershell -File ./scripts/smoke-test-release-binary.ps1 -BinaryPath target/release/knotrel-server.exe
```

This verifies:
- Loopback-only binding enforcement (non-loopback rejected at startup by default).
- Dynamic ephemeral port binding (`127.0.0.1:0`) without port collisions or TOCTOU races.
- Liveness (`/health`) returns `{"status":"ok"}`.
- Readiness (`/ready`) returns `{"status":"ready"}`.
- Engine info (`/v1/info`) reflects `compact-bfs`.
- Exact dynamic connectivity mutations (`link`, `cut`) and queries (`connected`).
- Documented error handling: self-loops (422 `self_loop`), missing nodes (404 `unknown_node`), bad content-type (415), malformed JSON (400 `invalid_request`).
- Idle graceful drain on `SIGTERM` (Unix) and clean exit code 0.
  *(Note: In-flight worker tracking and exit code 2 on deadline expiry are verified by `tests/process.rs` and `tests/lifecycle.rs`. On Windows, process termination is an immediate forced stop via `Process.Kill()`, as POSIX signals do not exist on Windows.)*
- Restart with fresh instance starts with empty graph (`node_count: 0`, `edge_count: 0`, `state_version: "0"`).

---

## 3. GitHub Actions Release Workflow (Dry Run)

1. Navigate to GitHub Actions > **Release**.
2. Trigger the workflow manually via `workflow_dispatch`:
   - `version`: `0.1.0`
   - `commit_sha`: (optional, defaults to current workflow commit)
   - `dry_run`: `true` (default)
3. The workflow executes:
   - `validate-inputs-and-metadata`: Validates version format (anchored `^[0-9]+\.[0-9]+\.[0-9]+$`), verifies commit SHA lineage against `origin/main`, verifies version against `Cargo.toml`, and locks candidate commit SHA.
   - `candidate-quality-gates`: Executes fmt, Clippy, Rustdoc, workspace tests, test-support tests, and all 3 examples on the candidate commit SHA before allowing any build or packaging jobs to run.
   - `build-binaries`: Builds release binaries, records verifiable build provenance receipts (linking binary digest, target, commit SHA, and rustc version), verifies binary file architecture (ELF, Mach-O, PE32+), runs native black-box smoke tests on `ubuntu-latest`, `macos-latest`, and `windows-latest`, and packages release archives.
   - `build-source`: Runs `./scripts/verify-source-packages.sh` on `ubuntu-latest` and packages `.crate` source archives.
   - `assemble-release-bundle`: Downloads all artifacts, verifies individual `.sha256` files, generates and verifies `SHA256SUMS`, asserts exact 14-asset inventory, and uploads the complete release bundle (with read-only permissions).
4. Download the assembled release bundle artifact (`knotrel-release-bundle-v0.1.0`) from the GitHub Actions run and verify:
   ```sh
   sha256sum -c SHA256SUMS
   ```

---

## 4. Final Review & Approval

Before tagging:
- [ ] Working tree is clean and `git status` shows no uncommitted files.
- [ ] Version in `Cargo.toml`, `crates/knotrel-core/Cargo.toml`, and `crates/knotrel-server/Cargo.toml` is `0.1.0`.
- [ ] Release notes in `docs/releases/v0.1.0-release-notes.md` are accurate and complete.
- [ ] Candidate commit SHA is verified to belong to `origin/main`.
- [ ] Candidate quality gates passed on the exact candidate commit SHA.
- [ ] All three platform binaries passed native CI smoke tests and provenance verification.
- [ ] Source package verification passed.
- [ ] Elastic License 2.0 is included in all archives.
- [ ] Explicit maintainer authorization has been granted.

---

## 5. Authorized Release Publication

Once explicitly approved:

1. Create and push the Git tag:
   ```sh
   git tag -a v0.1.0 -m "Knotrel v0.1.0 Preview"
   git push origin v0.1.0
   ```

2. Trigger the `release.yml` GitHub Actions workflow with:
   - `version`: `0.1.0`
   - `commit_sha`: `<commit-sha-of-v0.1.0-tag>`
   - `dry_run`: `false`

3. The workflow validates that the remote tag `v0.1.0` exists and points to `<commit-sha-of-v0.1.0-tag>` (`gh release create --verify-tag`), verifies that candidate quality gates and artifact assembly succeeded, and publishes the draft GitHub Release with all 14 verified assets.
4. Review the drafted release on GitHub and publish it.
