# Console release procedure

This procedure releases the Knotrel Console: the container image
`ghcr.io/knotrel/knotrel-console:<version>` and the static archive
`knotrel-console-<version>.tar.gz` (+ `.sha256`), added to the GitHub Release
`v<version>`. Versions are lockstep: one signed tag `v<version>` covers the
crates, the server binaries and the console. The Rust release
([release-procedure.md](release-procedure.md)) runs first and creates the
GitHub Release; this workflow then adds the console files to it. The two
workflows stay separate, so a console retry never republishes crates and a
crate retry never republishes the console.

Workflow: [`.github/workflows/console-release.yml`](../../.github/workflows/console-release.yml)
(manual `workflow_dispatch`; **dry run by default**).

## Versioning

- The console version in `console/package.json` and
  `console/package-lock.json` equals the Rust workspace version (lockstep, see
  `CONTRIBUTING.md`): console, `knotrel-server` and `knotrel-core` are released
  together with the same number. The first console release is `0.8.0`.
- The release workflow refuses a console version that differs from the
  workspace version in `Cargo.toml` at the same commit, prerelease suffix
  included: a release candidate carries `-rc.N` in both (for example
  `1.0.0-rc.1`).
- Release candidates use `X.Y.Z-rc.N`. They become GitHub prereleases and never
  move `latest`.
- `knotrel.supportedServer` in `console/package.json` states the compatible
  server versions. Update it, and the documentation, when the API contract
  changes.
- There is no separate console tag: the release uses the signed tag
  `v<version>` (`git tag -s`) on a commit of `main`, the same tag as the crates.

## What a maintainer sets up once

These steps happen in GitHub settings, outside the repository. Nothing in this
repository performs them.

1. **Container package name.** The first publication creates the package
   `knotrel-console` under the `knotrel` organization in GHCR, as an
   organization-owned package. Before the first real run, confirm that the
   organization allows members to publish packages (Organization settings →
   Packages) and that no package of that name already exists with other
   ownership.
2. **Workflow token permissions.** The publish job requests `packages: write`,
   `contents: write`, `id-token: write` and `attestations: write` for itself
   only. Organization or repository settings must not cap the
   `GITHUB_TOKEN` below these (Settings → Actions → General → Workflow
   permissions; per-job permissions still apply).
3. **Environment `console-release`.** Create it under Settings → Environments.
   Add required reviewers (the publish job waits for approval) and restrict
   deployments to protected branches or tags. Required reviewers are available
   on public repositories and on paid plans for private repositories; if they are
   unavailable, the environment still documents intent, but approval is not
   enforced. No secrets are needed: publication uses the job's `GITHUB_TOKEN`.
4. **Tag protection (recommended).** Add a ruleset that restricts creation and
   deletion of `v*` tags to maintainers.
5. **After the first publication:** make the package public (Package settings →
   Change visibility) and link it to the `knotrel/knotrel` repository if GitHub
   did not do so from the image's `org.opencontainers.image.source` label. Under
   "Manage Actions access", grant this repository write access. New GHCR packages
   start **private**, so `docker pull` fails for other users until this is done.

No Docker Hub account, npm organization or long-lived registry token is
needed.

## Dry run (no publication)

Run it for every candidate commit before tagging:

1. Actions → `console-release` → Run workflow, on `main`.
2. Inputs: `version` = `console/package.json` version, `commit_sha` = full SHA on
   `main`, `dry_run` = true (default), `update_latest` = false.
3. The workflow:
   - verifies the version in `package.json` and the lockfile, and that the
     commit is on `main`;
   - runs formatting, ESLint, TypeScript, the OpenAPI drift check, unit tests
     with coverage gates, `npm audit` and the production build;
   - runs the Chromium browser tests against a server built from the same
     commit;
   - builds the static archive with that commit's time as `SOURCE_DATE_EPOCH`,
     then verifies it;
   - builds the linux/amd64 + linux/arm64 image once into an OCI archive;
   - copies the amd64 image from that archive and smoke-tests it against a
     real server container, including a check that the image serves files
     byte-identical to the archive's `html/`;
   - records the archive SHA-256, the image index digest and the OCI archive
     SHA-256 in `release-manifest.json`;
   - uploads everything as the artifact `console-release-<version>`.

Download the artifact and inspect it if needed:

```sh
sha256sum -c knotrel-console-0.8.0.tar.gz.sha256
node console/scripts/package-static.mjs --verify knotrel-console-0.8.0.tar.gz
cat release-manifest.json
```

Equivalent local commands (no publication):

```sh
cd console
npm ci && npm run check
npm run archive -- --out .artifacts
node scripts/package-static.mjs --verify .artifacts/knotrel-console-0.8.0.tar.gz
docker build -t knotrel-console:dev --build-arg VERSION=0.8.0 .
docker build -f ../examples/container/Dockerfile -t knotrel-server:example ..
scripts/smoke-container.sh knotrel-console:dev knotrel-server:example dist
```

## Release candidate, then stable

1. Set the version in `console/package.json` (equal to the workspace version,
   see `CONTRIBUTING.md`). Run `npm install --package-lock-only` in
   `console/`, then merge the change to `main`.
2. Dry run this workflow and the Rust release workflow on the merge commit.
3. Follow the Rust release procedure: it creates and pushes the signed tag
   `v<version>` on that commit, publishes the crates and creates the GitHub
   Release `v<version>`.
4. Run this workflow with `dry_run` = false, the same version and commit, and
   `update_latest` = false. It refuses to run if the tag or the GitHub Release
   `v<version>` is missing. Approve the `console-release` environment when
   asked.
5. The publish job:
   - re-verifies the downloaded artifacts against the SHA-256 values recorded
     by the build job;
   - pushes the OCI archive unchanged (`skopeo copy --all --preserve-digests`)
     to `ghcr.io/knotrel/knotrel-console:<version>` and checks the pushed digest;
   - attests build provenance for the image and the archive;
   - uploads the archive and its checksum to the GitHub Release `v<version>`
     (only missing files; existing files are never replaced) and appends a
     `### Console` section with the image reference and digest to the release
     notes (once: skipped if the notes already have that heading);
   - moves `latest` only for a stable version with `update_latest` = true.
6. Perform the post-publication checks.
7. For the stable release, repeat from step 1 with `0.8.0` and choose
   `update_latest` = true if this should become the default stable reference.

## Post-publication checks

```sh
docker pull ghcr.io/knotrel/knotrel-console:0.8.0
docker buildx imagetools inspect ghcr.io/knotrel/knotrel-console:0.8.0   # index digest = release-manifest.json
gh attestation verify oci://ghcr.io/knotrel/knotrel-console:0.8.0 --repo knotrel/knotrel
gh release download v0.8.0 --pattern 'knotrel-console-*' --repo knotrel/knotrel
sha256sum -c knotrel-console-0.8.0.tar.gz.sha256
gh attestation verify knotrel-console-0.8.0.tar.gz --repo knotrel/knotrel
```

Then start the published image with the Compose example: replace the console
`build:` with `image: ghcr.io/knotrel/knotrel-console:0.8.0`. Open the UI and
check Overview, a query and, on standalone, one operation.

The image is pulled from GHCR, not served by it. Publishing the image and the
archive does not host the console anywhere.

## Partial publication and recovery

The publish steps run in this order: image, attestations, release assets,
`latest`. A failure leaves an earlier prefix published. The job summary lists
every completed step.

| State after failure | Safe resume |
| --- | --- |
| Nothing pushed | Re-run the workflow (same inputs). |
| Image pushed, a later step failed (attestation, release assets) | Run the workflow again with the same inputs, `dry_run` = false and `resume_run_id` = the failed run's number (fix the workflow on `main` first if the failure was in the workflow). It does not rebuild: it publishes that run's tested artifacts after checking the manifest's version and commit and every SHA-256, sees the image tag already at the identical digest, and continues; it uploads only missing release assets and never replaces existing ones. A plain re-run would rebuild, and the image build is not byte-reproducible, so it would refuse to overwrite the tag. |
| Image tag exists with a **different** digest | The workflow refuses to overwrite it. Do not delete and re-push the tag. Publish a new version (the next patch or RC, for all artifacts in lockstep) instead. |
| Console files added but checks failed | Edit the release notes to warn about the console files, fix forward and release a new version. Do not replace published files. |

Versions are immutable once published. Do not delete a published image tag
or release to reuse its version. To withdraw a bad version:

1. Mark the GitHub Release as not recommended: edit its notes, and remove the
   "latest" marking if it has it.
2. If `latest` points to it, move `latest` back to the previous good digest:

   ```sh
   skopeo copy --all --preserve-digests docker://ghcr.io/knotrel/knotrel-console@<good-digest> docker://ghcr.io/knotrel/knotrel-console:latest
   ```

   Run this with credentials that may write the package.
3. Release a fixed version and document the issue in its notes.

## Release checklist

- [ ] `console/package.json` and `package-lock.json` at the release version on `main`.
- [ ] Console compatibility verified before setting the range (mandatory from v0.9.0): browser tests against the release candidate's server and the previous release's server; then `knotrel.supportedServer` and `docs/console.md` updated to exactly the versions verified ([compatibility policy](../compatibility.md#5-console-and-server-compatibility)).
- [ ] Console CI (`console-status`) green on the commit; Sonar and Codecov reviewed (not a gate if skipped).
- [ ] Dry run passed on the exact commit; `release-manifest.json` kept.
- [ ] Signed `v<version>` tag on that commit and the Rust release done (GitHub Release `v<version>` exists).
- [ ] Publication approved; console files and image reference on the GitHub Release; post-publication checks done.
- [ ] After the first release: package public and linked; docs' "unreleased" labels removed.
