# Knotrel Release Procedure

This document specifies the reproducible verification, packaging, and publication protocol for Knotrel releases.

> [!IMPORTANT]
> **No implicit releases**: Creating Git tags, pushing tags, or publishing release artifacts requires explicit project maintainer authorization. Automated CI or pull requests must never publish releases.

---

## 1. Distribution Model & Prerequisites

- **Licensing**: All distributed source packages and binaries carry the **Elastic License 2.0 (ELv2)**.
- **Toolchain**: Build and test with the pinned compiler: **Rust 1.100.0** (`rust-toolchain.toml`). Verify the core MSRV separately with `cargo +1.98.1 test --locked -p knotrel-core`.
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

# 2. Strict Clippy checks for default and test-support configurations
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --features knotrel-server/test-support -- -D warnings

# 3. Documentation check with denied warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

# 4. Full workspace tests
cargo test --workspace --locked

# 5. Fault-injection lifecycle tests (test-support feature)
cargo test --workspace --locked --features knotrel-server/test-support

# 6. Core minimum supported Rust version
cargo +1.98.1 test --locked -p knotrel-core

# 7. Runnable examples
cargo run --locked -p knotrel-core --example basic
cargo run --locked -p knotrel-core --example bulk_import
cargo run --locked -p knotrel-core --example engine_selection
```

### 3.2 Isolated Source Distribution Verification

Validate that both crate packages build reproducibly offline without repository-local patches:

```sh
./scripts/verify-source-packages.sh
```

The script removes `target/package` before packaging. Preserve any existing
contents by running it in a disposable checkout of the candidate tree. Its
`--allow-dirty` packaging permits a local pre-commit check, but the final
release workflow must verify the committed RC.

This verifies:
- Dependencies are pre-fetched (`cargo fetch --locked`).
- `cargo package --workspace --offline` packages and verifies both crates natively without repository-local Cargo patches.
- `knotrel-core-0.5.0.crate` and `knotrel-server-0.5.0.crate` archives are created for this candidate.
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

### 3.4 Reproducible-build check (rc gate)

From v1.0.0, the Linux (`x86_64-unknown-linux-gnu`) and macOS
(`aarch64-apple-darwin`) release binaries must be rebuildable from the
candidate commit. The Windows binary is **not** covered (see the last bullet
below). The release workflow builds them without a build cache and with
`SOURCE_DATE_EPOCH` set to the commit time. After the release dry run on the
exact candidate commit (section 4), run **Actions > reproducible-build** with
these inputs:

- `ref`: the full candidate SHA
- `version`: for example `1.0.0`
- `dry_run_run_id`: the dry run's run ID
- `accept`: `byte`

The workflow rebuilds `knotrel-server` from a clean export on `ubuntu-latest`
(x86-64 Linux) and `macos-latest` (arm64 macOS), checks the bundle's checksums
and metadata, and compares the binaries
(`scripts/check-reproducible-build.sh`, which can also run locally).

Each platform gets two separate verdicts, reported in this order:

1. **Byte-identical**: yes or no.
2. **Equivalent after excluding named metadata**. For Mach-O exactly two
   ranges are excluded: the 16-byte `LC_UUID` and the ad-hoc
   `LC_CODE_SIGNATURE`. The signature hashes the file's pages, including the
   one holding the UUID, so it differs whenever the UUID differs. For ELF
   nothing is excluded, and the verdict is "not applicable".

A third, diagnostic line (`code`) compares only the code section
(`__TEXT,__text` or `.text`). It ignores data, strings, symbols and all other
sections.

- **Linux** must be byte-identical.
- **macOS** is expected to be byte-identical on the runner. If it is not, but
  it is equivalent after excluding `LC_UUID` and `LC_CODE_SIGNATURE`, the gate
  needs an explicit maintainer decision. Record both verdicts and the reason
  in the release notes, then rerun with `accept: equivalent`. Never describe
  such a result as "reproducible" without the qualifier.
- **Windows** (`x86_64-pc-windows-msvc`) is not covered and must not be
  described as reproducible. The workflow has no Windows job. MSVC's
  `cl.exe` ignores `SOURCE_DATE_EPOCH`, so mimalloc embeds the real compile
  time, and `link.exe` writes the link time into the PE header unless given
  `/Brepro`. State this limitation in the release notes.

A `code`-only result (code section identical, both verdicts "no") does
**not** pass the gate. Accepting it needs an explicit maintainer decision,
recorded in the release notes, that names the differing sections and explains
why they do not matter. Any other result blocks the release until it is
explained. Record the
verdicts and the workflow run in the release notes. After publication, run the
workflow again with `ref` set to the tag and no `dry_run_run_id`, which
compares against the GitHub Release.

The method, the excluded metadata, the macOS UUID cause and the v0.9.0 result are in
[Reproducible release builds](reproducible-build.md).

---

## 4. GitHub Actions Release Workflow (Dry Run)

Always run a dry run before publishing. Commit and push the intended RC first;
do not treat a run against an earlier commit as the final RC validation.

1. Navigate to GitHub Actions > **Release**.
2. Trigger the workflow manually via `workflow_dispatch`:
   - `version`: `0.5.0`
   - `commit_sha`: the full, committed RC SHA on `origin/main` (supply it explicitly)
   - `dry_run`: `true`
3. The dry-run workflow executes:
   - `validate-inputs-and-metadata`: Validates version format (anchored `^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[1-9][0-9]*)?$`: a release or a release candidate, see below), verifies commit SHA lineage against `origin/main`, verifies version against `Cargo.toml`. Then runs `./scripts/check-release-notes.sh <version>`: the notes `docs/releases/v<version>-release-notes.md` must exist and their status paragraph must start with `**Status: released` and not mention draft or unreleased, because the GitHub Release body is taken from the notes at the tagged commit and the tag cannot be changed.
   - `candidate-quality-gates`: Executes fmt, Clippy, Rustdoc, workspace tests, test-support tests, all 3 examples, and the release-publishing and recovery simulation test suites on the candidate commit SHA before build jobs run.
   - `build-binaries`: Compiles release binaries, records build provenance receipts, validates executable architecture (ELF, Mach-O, PE32+), runs native smoke tests on Linux, macOS, and Windows, and packages release archives.
   - `build-source`: Runs `./scripts/verify-source-packages.sh`, executes the required `knotrel-core` publish dry run, conditionally checks the server publish dry run, and generates `.crate.sha256` files.
   - `assemble-release-bundle`: Verifies individual `.sha256` files, generates `SHA256SUMS`, asserts exact 14-asset inventory, and uploads the release bundle.
4. Security guarantee:
   - In dry-run mode (`dry_run: true`), `publish-release`, `publish-crates` and `finalize-release` jobs are **completely skipped**.
   - The `crates-io` environment and `CRATES_IO_TOKEN` secret are **never accessed**.
5. Download the assembled release bundle artifact (`knotrel-release-bundle-v0.5.0`) from the GitHub Actions run and verify:
   ```sh
   sha256sum -c SHA256SUMS
   ```

`verify-dry-run-publish.sh` defers the server registry publish dry run if the
crates.io API confirms that `knotrel-core` 0.5.0 is absent (HTTP 404). The
offline source-package check still runs, but it does not prove that the server
can be published to the registry. A successful dry run does not guarantee
publication.

### 4.1 Release candidates (`X.Y.Z-rc.N`)

A release candidate goes through the same workflows with the version
`X.Y.Z-rc.N` (for example `1.0.0-rc.1`, tag `v1.0.0-rc.1`). It is the only
prerelease form: `release.yml`, `console-release.yml`,
`reproducible-build.yml` and the scripts they call reject any other suffix
(`1.0.0-alpha`, `1.0.0-rc.0`, `1.0.0-rc.01`). For a candidate:

- every version is `X.Y.Z-rc.N` in lockstep, and the server's `knotrel-core`
  requirement is exact (`=X.Y.Z-rc.N`, see `CONTRIBUTING.md`);
  `scripts/check-core-pin.py X.Y.Z-rc.N` checks it from `cargo metadata` in
  CI and in the release dry run (caret `^X.Y.Z` for a stable release);
- its notes are `docs/releases/vX.Y.Z-rc.N-release-notes.md`, separate from the
  notes of the final release, and `check-release-notes.sh X.Y.Z-rc.N` checks
  them; their status line is set to `**Status: released <date>**` by the last
  change before the candidate's tag, as for a release;
- `scripts/check-readme.py --release X.Y.Z-rc.N` requires the READMEs to keep
  naming the latest stable release (older than the candidate, with released
  notes) and allows "unreleased" labels; a README never names a candidate as
  its latest release;
- `create-or-verify-draft-release.sh` creates the GitHub Release as a
  **prerelease** (never "Latest"), and it and `finalize-release.sh` refuse an
  existing release whose prerelease flag does not match the version;
- crates.io publishes `X.Y.Z-rc.N`, which Cargo selects only for a
  requirement naming a prerelease (`knotrel-core = "=1.0.0-rc.1"`); the
  console image gets the tag `X.Y.Z-rc.N` and never `latest`.

---

## 5. Final Review & Authorization Checklist

Before tagging and publishing:
- [ ] The release performance diff against the previous published release is committed in `knotrel-benchmarks` and linked from the release notes (requirements below).
- [ ] The full release commit SHA in each repository is recorded, together with the measured product/tooling SHAs and any verified evidence-transfer justification.
- [ ] Matching signed version tags are planned for both `knotrel/knotrel` and `knotrel/knotrel-benchmarks`; neither existing tag will be moved or overwritten.
- [ ] Working tree is clean and `git status` shows no uncommitted files.
- [ ] Workspace and inherited crate versions are `0.5.0`, and the core dependency and lockfile agree.
- [ ] Release notes in `docs/releases/v0.5.0-release-notes.md` report the v0.5.0 performance qualification (existing behavior against v0.4.0, lifecycle cost and sustained resources), every disclosed or owner-accepted non-PASS result, and all operational limitations.
- [ ] `README.md` and `console/README.md` describe this release on the candidate commit: latest release line and links, download and asset names, the `knotrel-core = "MAJOR.MINOR"` snippet, the console status line and image tags, the performance summary, and no feature still marked "unreleased". `scripts/check-readme.py --release <version>` checks the mechanical part and runs in the release workflow (dry runs included); every pull request runs it without `--release`, so a README never names two different releases.
- [ ] The release notes status is final (`**Status: released <date>**`, the planned tag date) on the candidate commit: the GitHub Release body is taken from the notes at the tag. Post-publication details (digests, links) can follow on `main` and in the release body, but never a status change from draft.
- [ ] Candidate commit SHA is verified on `origin/main`.
- [ ] The v0.5.0 performance qualification ran on the exact RC commit (or its executed code was shown identical), and the deployment and rebuild smoke test was repeated on it.
- [ ] All CI and release dry-run quality gates passed on the exact candidate commit SHA.
- [ ] All three platform binaries passed native CI smoke tests and provenance verification.
- [ ] From v1.0.0: the `reproducible-build` workflow rebuilt the dry-run binaries from the candidate commit (Linux byte-identical; macOS byte-identical, or equivalent after excluding `LC_UUID` and `LC_CODE_SIGNATURE` with a recorded maintainer decision) and the result is in the release notes (section 3.4).
- [ ] Source package verification passed.
- [ ] Elastic License 2.0 is included in all archives.
- [ ] `CRATES_IO_TOKEN` is configured in the `crates-io` environment with required reviewer protection.
- [ ] The release tag will be created with `git tag -s` and checked with `git verify-tag` before it is pushed.
- [ ] Explicit maintainer authorization has been granted.

---

### Per-release performance diff and paired tags

For every release, retain a versioned performance comparison in
`knotrel-benchmarks` and link it from the product release notes. Include:

- Previous release and candidate versions, full commit SHAs, measured binary
  hashes, harness/collector revisions, host, toolchain and runtime configuration.
- Comparable workload cells with absolute baseline/candidate values, percentage
  changes, uncertainty intervals, approved thresholds and original verdicts.
- Missing coverage and changes in semantics or methodology that prevent direct
  comparison. Mark unmeasured cells explicitly; do not infer their performance.
- Regressions, inconclusive results and maintainer dispositions, preserved as
  such rather than relabelled PASS, with links to durable raw evidence and its
  checksum manifest. Private evidence must remain private.

Reuse valid qualification evidence; producing this report does not itself
require a new benchmark campaign. If the final commit differs from the measured
candidate, document why the evidence applies or perform the required
requalification before release.

Use the same release tag name (for example `v0.5.0`) in both repositories, each
pointing to its own recorded commit. The benchmark tag pins the report and
reproduction tooling; it does not imply that the benchmark crate has the same
package version. Record measured tooling revisions separately when they differ
from the report/tag commit. A sibling path dependency is not pinned by tagging:
reproduction instructions must specify both repository revisions explicitly.

After authorization, create, verify and push the signed benchmark tag first,
then the signed product tag using the publication procedure below. Check the
workflows triggered by each tag before pushing; the product tag can start
publication. Verify both remote tags resolve to the intended commits and record
both links in the release notes. If publication stops between tags, retain the
existing tag and resume on the same commits; do not move tags to hide the partial
state. Paired tags alone do not prove qualification or release completion.

## 6. Authorized Release Publication

Once explicitly approved:

1. Create a **signed** annotated tag on the candidate commit, verify it, then
   push it:
   ```sh
   git tag -s v0.5.0 -m "Knotrel v0.5.0" <commit-sha-of-v0.5.0-tag>
   git verify-tag v0.5.0
   git push origin v0.5.0
   ```
   Use `-s`, not `-a`. With `-a`, GitHub shows the tag object as unverified
   (`unsigned`) even when the commit it points to has a valid signature.
   `git verify-tag` must report a good signature from the maintainer's signing
   key before the push. After pushing, confirm that GitHub reports the tag as
   verified:
   ```sh
   gh api repos/knotrel/knotrel/git/refs/tags/v0.5.0 --jq .object.sha \
     | xargs -I{} gh api repos/knotrel/knotrel/git/tags/{} --jq .verification
   ```

   Do not delete or move a tag that a published GitHub Release uses.
   Deleting it turns the release back into a draft. Republishing it restores
   it, but GitHub records a new publication date. If such a tag must be
   recreated (for example to sign it), run `git tag -s` successfully first;
   then delete, recreate and push the tag on the same commit at once.
   Immediately republish the release with
   `gh release edit <tag> --draft=false --latest=<true|false>`, restoring its
   previous state, and check that its assets are unchanged.

2. Trigger the `release.yml` GitHub Actions workflow with:
   - `version`: `0.5.0`
   - `commit_sha`: `<commit-sha-of-v0.5.0-tag>`
   - `dry_run`: `false`

3. Workflow execution order:
   - `validate-inputs-and-metadata`: Validates that remote tag `v0.5.0` exists and points exactly to `<commit-sha-of-v0.5.0-tag>`.
   - `candidate-quality-gates`: Validates fmt, Clippy, doc, tests, and examples.
   - `build-binaries` and `build-source`: Build and verify binary and source packages, including crates.io publish dry-run validation.
   - `assemble-release-bundle`: Verifies bundle checksums and asset inventory.
   - `publish-release`: Idempotently creates or verifies the draft GitHub Release with all 14 verified assets (`./scripts/create-or-verify-draft-release.sh`).
   - `publish-crates`: Runs inside the approval-protected `crates-io` environment:
     1. Prompts environment reviewer for approval.
     2. Validates `CARGO_REGISTRY_TOKEN` presence and verifies prospective packages match bundle checksums byte-for-byte.
     3. Preflights both `knotrel-core` and `knotrel-server` on crates.io before attempting any upload.
     4. Publishes `knotrel-core v0.5.0` (if absent) and polls until verified on REST API, sparse index, and resolvable via Cargo probe.
     5. Publishes `knotrel-server v0.5.0` (if absent) with full verification against published core, and polls until verified on crates.io.
   - `finalize-release`: Automatically finalizes the GitHub Release (`gh release edit --draft=false`) after confirming both crates are published and all 14 assets are verified on the draft.

4. For the initial v0.1.0 bootstrap only, perform the mandatory token rotation
   described in Section 2.2. Subsequent releases use the restricted token.

5. Then release the console under the same tag (lockstep, same version): run
   the [console release workflow](console-release.md) with `dry_run` = false.
   It adds the console archive and checksum to this GitHub Release (the
   asset checks above accept exactly those two extra files) and publishes the
   image `ghcr.io/knotrel/knotrel-console:<version>`.

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
3. Re-dispatch the `release.yml` workflow with `dry_run: false`, `version: 0.5.0`, and the same `commit_sha` (or re-run failed jobs).
4. When `publish-release` runs: It verifies the existing draft release and reuses it without error.
5. When `publish-crates` runs:
   - It preflights both crates.
   - It queries `https://crates.io/api/v1/crates/knotrel-core/0.5.0` and matches the checksum against the local candidate artifact.
   - It reports: `Confirmed: Remote knotrel-core checksum matches local release candidate. Skipping upload.`
   - It proceeds directly to publishing and verifying `knotrel-server`.
6. When `finalize-release` runs: It revalidates both crates on crates.io and publishes the draft GitHub Release.

### Scenario B: Checksum mismatch detected

If `publish-crates.sh` discovers that a crate version already exists on crates.io with a different SHA256 checksum than the local candidate:
- The script immediately aborts with an error:
  `ERROR: Remote knotrel-... v0.5.0 checksum mismatch! Stopping for manual reconciliation.`
- Full release success is **not declared**.
- The maintainer must investigate whether a conflicting version was previously published. Because crates.io disallows overwrites, the release version must be bumped (e.g. to `0.3.1`) if the remote version cannot be used.

### Scenario C: Token authorization or scope error

If publication fails with HTTP 403 / unauthorized:
- For initial v0.1.0: Ensure the token has `publish-new` permission.
- For subsequent releases: Ensure the token has `publish-update` permission and includes both `knotrel-core` and `knotrel-server` in its crate scope.
- Update `CRATES_IO_TOKEN` in the `crates-io` environment and re-run the failed job.

## Crate documentation gate

CI and candidate validation check the crate introductions for stale release
numbers, run all-feature doctests, and build warning-denied Rustdoc including
private items. `scripts/check-crate-docs.py` checks only crate README files and
module-level `//!` comments in `src/lib.rs`; it deliberately excludes release
notes and migration history. Prefer version-independent wording and `cargo add`
examples. An intentional historical comparison in an introduction must be moved
to the historical documentation or the check explicitly refined and reviewed;
this check does not prove factual accuracy of the prose.

Before approval, verify that documentation included in both source packages
accurately describes the candidate's capabilities and limitations. In particular,
check examples, readiness, supported modes and experimental guarantees.

After publication, the workflow checks docs.rs for the exact released version of
both crates. Build failure fails the documentation-status job; a queued, missing
or unreachable status produces a warning and leaves documentation closure pending.
It does not undo or retry publication. To finish the release checklist, run:

```sh
python3 scripts/check-docs-rs.py VERSION
```

Exit 0 means both builds succeeded; 1 means a build failed; 2 means status is still
unknown or pending. Inspect the linked build logs for failures, and rerun this
read-only check after queue processing or service recovery. Never republish the
same crate version to repair docs.rs. Record the successful exact-version check
before declaring documentation complete; a successful release workflow with a
pending warning alone is insufficient.

The documentation check also rejects LaTeX math delimiters in crate README
files and line-style Rustdoc comments under `src/`. Use inline code such as
`O(V + E)` or `O(log² V)`; the generated documentation needs no math renderer.
