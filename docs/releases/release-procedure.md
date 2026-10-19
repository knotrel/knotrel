# Knotrel Release Procedure

This document specifies the reproducible verification, packaging, and publication protocol for Knotrel releases.

> [!IMPORTANT]
> **No implicit releases**: Creating Git tags, pushing tags, or publishing release artifacts requires explicit project maintainer authorization. Automated CI or pull requests must never publish releases.

---

## 1. Distribution Model & Prerequisites

- **Licensing**: All distributed source packages and binaries carry the **Elastic License 2.0 (ELv2)**.
- **Toolchain**: Build and test with the pinned compiler: **Rust 1.98.1** (`rust-toolchain.toml`).
- **Supported binary targets**:
  - `x86_64-unknown-linux-gnu` (Linux x86_64, ELF 64-bit)
  - `aarch64-apple-darwin` (macOS Apple Silicon, Mach-O 64-bit arm64)
  - `x86_64-pc-windows-msvc` (Windows x86_64, PE32+ executable)
- **Crates.io distribution**:
  - `knotrel-core`: Synchronous dynamic graph connectivity engine.
  - `knotrel-server`: HTTP API and standalone server for Knotrel.
- **Lineage Constraint**: Candidate commits for release must belong to the authorized release lineage (`origin/main`).

---

## 2. Authentication Decision & Secret Configuration

Knotrel uses an API-token-based publishing model with **ONE persistent GitHub Secret** and manual token rotation:

- Secret name: `CRATES_IO_TOKEN`
- Location: GitHub Environment `crates-io` (approval-protected)

Do not configure OIDC Trusted Publishing, separate bootstrap secrets, multiple authentication modes, or automatic rotation. The release workflow remains unchanged when the token is rotated.

### 2.1 Initial Token Setup (v0.1.0 Bootstrap)

Before the first release (v0.1.0), neither crate exists on crates.io. An initial token with new crate creation privileges is required:

1. Log in to [crates.io](https://crates.io/) and navigate to **Account Settings > API Tokens**.
2. Click **New Token**:
   - **Name**: `knotrel-release-bootstrap`
   - **Expiration**: Short duration (e.g. 7–14 days)
   - **Scopes**: Select `publish-new`
3. Copy the generated token immediately.
4. In the GitHub repository (`knotrel/knotrel`):
   - Navigate to **Settings > Environments**.
   - Create a new environment named `crates-io`.
   - Under **Deployment protection rules**, enable **Required reviewers** and assign the repository owner / authorized release maintainers.
   - Under **Environment secrets**, add `CRATES_IO_TOKEN` and paste the token value.

### 2.2 Post-Publication Token Rotation (Mandatory after v0.1.0)

Once both `knotrel-core` and `knotrel-server` v0.1.0 are published on crates.io, the initial `publish-new` token must be rotated to a restricted token:

1. On [crates.io](https://crates.io/), navigate to **Account Settings > API Tokens**.
2. Click **New Token**:
   - **Name**: `knotrel-release-production`
   - **Expiration**: Standard rotation policy (e.g. 90 days or 1 year)
   - **Scopes**: Select `publish-update` only (uncheck `publish-new`)
   - **Crate restrictions**: Restrict access strictly to:
     - `knotrel-core`
     - `knotrel-server`
3. Copy the newly generated token.
4. In GitHub repository **Settings > Environments > `crates-io` > Environment secrets**:
   - Update `CRATES_IO_TOKEN` with the new restricted token value.
5. On [crates.io](https://crates.io/), immediately **Revoke** the old `knotrel-release-bootstrap` token.
6. Verify that the workflow file `.github/workflows/release.yml` requires **zero code changes** for token rotation.

---

## 3. Pre-Release Verification Protocol

Before initiating a release, execute the mandatory quality gates locally on a clean checkout of `main`:

### 3.1 Workspace Quality Gates

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

### 3.2 Isolated Source Distribution Verification

Validate that both crate packages build reproducibly offline without repository-local patches:

```sh
./scripts/verify-source-packages.sh
```

This verifies:
- Dependencies are pre-fetched (`cargo fetch --locked`).
- `cargo package --workspace --offline` packages and verifies both crates natively without repository-local Cargo patches.
- `knotrel-core-0.1.0.crate` and `knotrel-server-0.1.0.crate` archives are created.
- `LICENSE.md` exists in both archives and matches the Elastic License 2.0 text.
- Packages are extracted into an isolated sandbox outside the repository checkout.
- `cargo metadata` proves `knotrel-core` resolves exclusively from the isolated archive.
- `cargo check --offline --locked` and `cargo test --offline --locked --no-run` succeed.

### 3.3 Release Binary Build & Black-Box Smoke Test

Compile `knotrel-server` in release mode and execute black-box process tests:

```sh
# 1. Build release binary
cargo build --release --locked -p knotrel-server

# 2. Run automated black-box smoke tests (Unix)
./scripts/smoke-test-release-binary.sh target/release/knotrel-server

# On Windows (PowerShell):
# powershell -File ./scripts/smoke-test-release-binary.ps1 -BinaryPath target/release/knotrel-server.exe
```

---

## 4. GitHub Actions Release Workflow (Dry Run)

Always run a dry run before publishing:

1. Navigate to GitHub Actions > **Release**.
2. Trigger the workflow manually via `workflow_dispatch`:
   - `version`: `0.1.0`
   - `commit_sha`: (optional, defaults to current workflow commit)
   - `dry_run`: `true` (default)
3. The dry-run workflow executes:
   - `validate-inputs-and-metadata`: Validates version format (anchored `^[0-9]+\.[0-9]+\.[0-9]+$`), verifies commit SHA lineage against `origin/main`, verifies version against `Cargo.toml`.
   - `candidate-quality-gates`: Executes fmt, Clippy, Rustdoc, workspace tests, test-support tests, and all 3 examples on the candidate commit SHA before build jobs run.
   - `build-binaries`: Compiles release binaries, records build provenance receipts, validates executable architecture (ELF, Mach-O, PE32+), runs native smoke tests on Linux, macOS, and Windows, and packages release archives.
   - `build-source`: Runs `./scripts/verify-source-packages.sh`, executes dry-run crates.io verification (`cargo publish --dry-run -p knotrel-core`), and generates `.crate.sha256` files.
   - `assemble-release-bundle`: Verifies individual `.sha256` files, generates `SHA256SUMS`, asserts exact 14-asset inventory, and uploads the release bundle.
4. Security guarantee:
   - In dry-run mode (`dry_run: true`), `publish-release` and `publish-crates` jobs are **completely skipped**.
   - The `crates-io` environment and `CRATES_IO_TOKEN` secret are **never accessed**.
5. Download the assembled release bundle artifact (`knotrel-release-bundle-v0.1.0`) from the GitHub Actions run and verify:
   ```sh
   sha256sum -c SHA256SUMS
   ```

---

## 5. Final Review & Authorization Checklist

Before tagging and publishing:
- [ ] Working tree is clean and `git status` shows no uncommitted files.
- [ ] Version in `Cargo.toml`, `crates/knotrel-core/Cargo.toml`, and `crates/knotrel-server/Cargo.toml` is `0.1.0`.
- [ ] Release notes in `docs/releases/v0.1.0-release-notes.md` are accurate and complete.
- [ ] Candidate commit SHA is verified on `origin/main`.
- [ ] All CI and release dry-run quality gates passed on the exact candidate commit SHA.
- [ ] All three platform binaries passed native CI smoke tests and provenance verification.
- [ ] Source package verification passed.
- [ ] Elastic License 2.0 is included in all archives.
- [ ] `CRATES_IO_TOKEN` is configured in the `crates-io` environment with required reviewer protection.
- [ ] Explicit maintainer authorization has been granted.

---

## 6. Authorized Release Publication

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

3. Workflow execution order:
   - `validate-inputs-and-metadata`: Validates that remote tag `v0.1.0` exists and points exactly to `<commit-sha-of-v0.1.0-tag>`.
   - `candidate-quality-gates`: Validates fmt, Clippy, doc, tests, and examples.
   - `build-binaries` and `build-source`: Build and verify binary and source packages, including crates.io publish dry-run validation.
   - `assemble-release-bundle`: Verifies bundle checksums and asset inventory.
   - `publish-release`: Idempotently creates or verifies the draft GitHub Release with all 14 verified assets (`./scripts/create-or-verify-draft-release.sh`).
   - `publish-crates`: Runs inside the approval-protected `crates-io` environment:
     1. Prompts environment reviewer for approval.
     2. Validates `CARGO_REGISTRY_TOKEN` presence and verifies prospective packages match bundle checksums byte-for-byte.
     3. Preflights both `knotrel-core` and `knotrel-server` on crates.io before attempting any upload.
     4. Publishes `knotrel-core v0.1.0` (if absent) and polls until verified on REST API, sparse index, and resolvable via Cargo probe.
     5. Publishes `knotrel-server v0.1.0` (if absent) with full verification against published core, and polls until verified on crates.io.
   - `finalize-release`: Automatically finalizes the GitHub Release (`gh release edit --draft=false`) after confirming both crates are published and all 14 assets are verified on the draft.

4. Perform mandatory token rotation (see Section 2.2).

---

## 7. Partial Publication Failure Recovery & Rerun Guidelines

Because crates.io is an immutable package registry, a network glitch, timeout, or authorization failure could interrupt publication after `knotrel-core` has published but before `knotrel-server` completes, or before GitHub Release finalization.

### When to Rerun a Failed Job vs. Entire Workflow

- **Rerun Failed Job Only**:
  Use GitHub Actions "Re-run failed jobs" when:
  - A transient network or timeout error occurred during `publish-crates` readiness polling or `finalize-release`.
  - The candidate commit SHA, release version, and tag have not changed.
  - The idempotent scripts (`publish-crates.sh`, `create-or-verify-draft-release.sh`, and `finalize-release.sh`) will recognize already-verified artifacts and resume safely without duplicate uploads.
- **Rerun Entire Workflow**:
  Use "Re-run all jobs" or a new `workflow_dispatch` run when:
  - The workflow failed during early compilation or test stages before bundle assembly.
  - Any code, manifest, or dependency input was amended (requires a new commit SHA and re-tagging).

### Scenario A: `knotrel-core` published, `knotrel-server` failed

1. Inspect the workflow logs to identify why `knotrel-server` failed (e.g. index delay, transient network failure, token rate limit).
2. Do not delete or re-tag the release.
3. Re-dispatch the `release.yml` workflow with `dry_run: false`, `version: 0.1.0`, and the same `commit_sha` (or re-run failed jobs).
4. When `publish-release` runs: It verifies the existing draft release and reuses it without error.
5. When `publish-crates` runs:
   - It preflights both crates.
   - It queries `https://crates.io/api/v1/crates/knotrel-core/0.1.0` and matches the checksum against the local candidate artifact.
   - It reports: `Confirmed: Remote knotrel-core checksum matches local release candidate. Skipping upload.`
   - It proceeds directly to publishing and verifying `knotrel-server`.
6. When `finalize-release` runs: It revalidates both crates on crates.io and publishes the draft GitHub Release.

### Scenario B: Checksum mismatch detected

If `publish-crates.sh` discovers that a crate version already exists on crates.io with a different SHA256 checksum than the local candidate:
- The script immediately aborts with an error:
  `ERROR: Remote knotrel-... v0.1.0 checksum mismatch! Stopping for manual reconciliation.`
- Full release success is **not declared**.
- The maintainer must investigate whether a conflicting version was previously published. Because crates.io disallows overwrites, the release version must be bumped (e.g. to `0.1.1`) if the remote version cannot be used.

### Scenario C: Token authorization or scope error

If publication fails with HTTP 403 / unauthorized:
- For initial v0.1.0: Ensure the token has `publish-new` permission.
- For subsequent releases: Ensure the token has `publish-update` permission and includes both `knotrel-core` and `knotrel-server` in its crate scope.
- Update `CRATES_IO_TOKEN` in the `crates-io` environment and re-run the failed job.
