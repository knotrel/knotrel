# Reproducible release builds

This page covers the reproducible-build check of the `knotrel-server` release
binary (readiness criterion 6, rc gate in
[the release procedure](release-procedure.md#34-reproducible-build-check-rc-gate)).
It describes what the check proves, how to run it, and the result for v0.9.0.

## Scope: Linux and macOS only

The gate covers two of the three release binaries:
`x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`. It does **not** cover
`x86_64-pc-windows-msvc`. The `reproducible-build` workflow has no Windows
job, and this page makes no claim that the Windows binary is reproducible.
Nobody has tested a Windows rebuild. There are two known obstacles:

- **mimalloc's build date.** MSVC's `cl.exe` ignores `SOURCE_DATE_EPOCH`, so
  the `__DATE__`/`__TIME__` that mimalloc embeds is always the actual compile
  time.
- **PE link timestamps.** The PE file header has a `TimeDateStamp` field,
  which `link.exe` fills with the link time unless it is given `/Brepro`. The
  release build does not pass that flag explicitly.

Covering Windows would need its own investigation and changes to the Windows
build. Until then, release notes must say that the reproducible-build check
covers Linux and macOS only.

## Two verdicts, never merged

Every comparison prints two separate verdicts for each platform, always in
this order and on separate lines:

1. **Byte-identical**: yes or no. The rebuilt file has the same SHA-256 as the
   reference. Only this verdict may be called *reproducible* without a
   qualifier.
2. **Equivalent after excluding named metadata**: yes, no, or not applicable.
   The files are equal once exactly the ranges listed below are zeroed in both
   files, and nothing else. The verdict line names every excluded range with
   its offset and length. A "yes" here is always reported as "not
   byte-identical; equivalent after excluding `LC_UUID` and
   `LC_CODE_SIGNATURE`", and it never replaces verdict 1.

| Platform | Excluded for verdict 2 | Why |
|---|---|---|
| macOS (Mach-O) | the 16-byte `LC_UUID` payload | `ld` hashes the debug map, which names the absolute path of the Rust toolchain, into the UUID; see [the UUID difference](#the-macos-uuid-difference) |
| macOS (Mach-O) | the `LC_CODE_SIGNATURE` blob (the whole signature, not just one slot) | the linker's ad-hoc signature (no certificate) holds a SHA-256 hash of each 4 KiB page of the file. The first page contains the load commands, including `LC_UUID`, so a different UUID always changes that page's hash. The signature differs *because of* the UUID; no other input affects it. If the UUIDs are equal and all other bytes are equal, the signatures are equal too. The comparer does not just assume this: it recomputes every page hash of both files, checks them against the stored hashes, and requires the two signatures to be identical apart from those hashes (same identifier, flags and limits). Otherwise the verdict is "no" |
| Linux (ELF) | nothing (verdict 2 is "not applicable") | ELF has no comparable field; `.note.gnu.build-id` is derived from the content and is compared like any other section |

### The diagnostic `code` level

A third line, `VERDICT code section identical`, compares only the bytes of a
single section:

- Mach-O: the contents of `__TEXT,__text`.
- ELF: the contents of `.text`.

The level is reached when those bytes are identical and both verdicts above
are "no". It says nothing about the following:

- data and string constants (`__cstring`, `__const`, `.rodata`, `.data`),
  including embedded paths and timestamps;
- unwind tables and exception data;
- symbol tables, relocations and fixups, and load commands or program
  headers;
- any other section.

These can change behavior (a constant, a table, the dynamic linker's work)
even when the instructions match. The level only helps locate a difference.
**It cannot close the gate.** Accepting a `code` result needs an explicit,
recorded maintainer decision that names the differing sections and explains
why they do not matter.

## What the check does

[`scripts/check-reproducible-build.sh`](../../scripts/check-reproducible-build.sh)
takes a release tag or candidate commit and does the following:

1. Gets the reference assets: the GitHub Release by default, or a release
   dry-run bundle. It checks the archive's `.sha256`, its `SHA256SUMS` entry,
   the commit in `-metadata.json` and the `binary_sha256` of the extracted
   binary before using them.
2. Exports the commit with `git archive`, so no untracked or ignored files are
   included.
3. Runs the release workflow's build command,
   `cargo build --release --locked --target <target> -p knotrel-server`, with
   the toolchain from the commit's `rust-toolchain.toml`. That toolchain must
   equal the `rust_toolchain` recorded in the metadata. The script clears
   `RUSTFLAGS`, wrappers and `CARGO_PROFILE_*` overrides.
4. Compares the binaries with
   [`scripts/compare-binaries.py`](../../scripts/compare-binaries.py). It
   prints a per-section table, the verdicts, and the exact offsets and lengths
   of the excluded ranges. With `ACCEPT=byte` (the default) only a
   byte-identical rebuild passes. `ACCEPT=equivalent` also accepts verdict 2
   and prints "NOT byte-identical" in its PASS line.

The script fixes two build inputs that do not depend on the source:

- **`SOURCE_DATE_EPOCH`.** `libmimalloc-sys` compiles mimalloc's C sources,
  which embed `__DATE__` and `__TIME__` (the "built on" banner in
  `options.c`). GCC and Clang read both from `SOURCE_DATE_EPOCH`. From v1.0.0
  the release workflow sets it to the commit time
  (`git log -1 --format=%ct`), and the script uses the same default.
- **Dependency source paths.** `rustc` embeds the absolute paths of crates
  under `CARGO_HOME` in panic locations (`/home/runner/.cargo/...` on Linux,
  `/Users/runner/.cargo/...` on macOS). Workspace paths are relative. When the
  local `CARGO_HOME` differs from the runner's, the script adds
  `--remap-path-prefix=<local CARGO_HOME>=<runner CARGO_HOME>`. On a
  GitHub-hosted runner the paths already match, so the build gets no extra
  flag.

Some inputs come from the host and must match the release build: the C
compiler, the linker, the SDK, the system libraries, and on macOS the path of
the Rust toolchain (see below). The `reproducible-build` workflow runs on the
same runner images as the release and logs `ImageOS`/`ImageVersion`.

Windows is not covered; see [Scope](#scope-linux-and-macos-only).

## Running it

Locally, on Linux x86-64 or macOS arm64, from a checkout with tags:

```sh
scripts/check-reproducible-build.sh v1.0.0                  # against the GitHub Release
PUBLISHED_DIR=<extracted dry-run bundle> RELEASE_VERSION=1.0.0 \
  scripts/check-reproducible-build.sh <candidate-sha>        # rc gate, before tagging
```

In CI, run **Actions > reproducible-build** (`workflow_dispatch`) with these
inputs:

- `ref`: a tag or a full commit SHA
- `version`: needed for a commit
- `dry_run_run_id`: the release dry-run run whose bundle is the reference;
  leave it empty to use the GitHub Release
- `source_date_epoch`: only for releases up to v0.9.0
- `accept`: default `byte`

The workflow rebuilds on `ubuntu-latest` (`x86_64-unknown-linux-gnu`) and
`macos-latest` (`aarch64-apple-darwin`) without a build cache, and uploads the
comparison log. GitHub only offers `workflow_dispatch` for a workflow file
that exists on the default branch, so the workflow can run only after this
change is merged into `main`.

## The macOS UUID difference

`LC_UUID` identifies a Mach-O image to crash reporters, symbolication and
`dyld`. Apple's `ld` computes it as a content hash of the output it writes. The
Rust release profile here has `debug = false`, so Cargo's default
`strip = "debuginfo"` applies: after linking, `rustc` strips the debug map (the
`N_OSO` stabs that point a debugger at the input objects by absolute path).
So `ld` computes the UUID over an output that still contains those paths, and
the shipped binary no longer contains them.

The paths include the Rust toolchain's own libraries. A build with stripping
disabled showed `N_OSO` entries for
`<RUSTUP_HOME>/toolchains/1.100.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcompiler_builtins-509456429c759ecd.rlib`.
That rlib is linked from the sysroot as is, never through LTO.

The evidence comes from local rebuilds of v0.9.0, all with the same source,
flags and `SOURCE_DATE_EPOCH`:

| Rebuild variation | UUID | Everything else |
|---|---|---|
| different target directory, `CARGO_HOME` path, or copy of the SDK at another path | unchanged (`40EB2970-…`) | unchanged |
| byte-for-byte copy of the toolchain at another path (rustc reports the new sysroot) | **changed** (`4361D6FF-…`) | identical (all sections, symbols, fixups and load commands) |
| both toolchain paths, final link with `-Wl,-S` (no debug map) | **equal** for both paths (`6BBB2034-…`) | (diagnostic only) |

Result: on macOS the UUID depends on the absolute path of the Rust toolchain,
through the debug map. Nothing else about the build or the host showed this
effect. The release was linked with the toolchain at
`/Users/runner/.rustup/toolchains/1.100.0-aarch64-apple-darwin`, and a local
rebuild uses `~/.rustup/...`. A local rebuild cannot use the runner's path
without creating `/Users/runner`, so locally the verdicts are "not
byte-identical; equivalent after excluding `LC_UUID` and
`LC_CODE_SIGNATURE`". A rebuild on `macos-latest` uses the same toolchain
path, so byte identity is expected there. That is **not yet demonstrated**: it
needs the CI run.

Making the UUID independent of the path would change the release binary
(for example `-C link-arg=-Wl,-S`, or `-Wl,-random_uuid`, which goes the other
way). It is not proposed for v1.0.0. The runner gives the same path anyway.

## Result for v0.9.0 (2026-11-22)

**Reference:** GitHub Release `v0.9.0`, commit
`223f04c10b0af663b54e269e0970ee10ac1dc6b2`, built with
`rustc 1.100.0 (0ef1bc386 2026-11-09)`. Before the rebuild, the archive
checksums (`.sha256` and `SHA256SUMS`) and the metadata `binary_sha256` were
verified for macOS arm64 and Linux x86-64.

### macOS arm64: rebuilt locally

The host was Darwin 25.5 with Apple clang 21.0.0, `ld-1267` and SDK 26.5; the
published binary records the same SDK and linker version. The rebuild used
Rust 1.100.0 on a `git archive` export of the tag.

| Rebuild | Byte-identical | Equivalent after excluding `LC_UUID`, `LC_CODE_SIGNATURE` | Code section | Remaining difference |
|---|---|---|---|---|
| Plain (local `CARGO_HOME`) | no | no | different | dependency paths in strings, constants, unwind info |
| Path remap to `/Users/runner/.cargo` | no | no | identical | mimalloc `__TIME__` string (`13:14:50` vs local) |
| Remap and `SOURCE_DATE_EPOCH=1795353290` (2026-11-22 13:14:50 UTC, the time embedded in the published binary) | **no** | **yes** | identical | 47 bytes, all inside `LC_UUID` (offset 2024) and `LC_CODE_SIGNATURE` (offset 4117504). In both signatures, all 1006 page hashes match their file, and the signatures are identical apart from those hashes |

**Verdicts for macOS:**

- **Byte-identical: no** (local rebuild).
- **Equivalent after excluding `LC_UUID` and `LC_CODE_SIGNATURE`: yes.** The
  UUID differs because of the toolchain path, as explained above.

Local rebuilds are deterministic: repeated clean rebuilds gave the same file
(`b858415abb47f8d92593cb74480674d93efad9504cab85bcf9f3528f5349ce69`).

### Workflow runs on GitHub's runners (2026-11-22)

The two v0.9.0 binaries embed different C build times, and
`source_date_epoch` applies to both jobs of a run, so each run counts for
one platform only; the other job is expected to differ.

| Platform | Run | `source_date_epoch` | Byte-identical | Equivalent after excluding named metadata | Code section |
|---|---|---|---|---|---|
| Linux x86-64 | [45058546623](https://github.com/knotrel/knotrel/actions/runs/45058546623) (`main`, `accept: byte`) | `1795244644` (2026-11-21 07:04:04 UTC, embedded in the published binary) | **yes** | not applicable (ELF: nothing excluded) | identical |
| macOS arm64 | [45058870559](https://github.com/knotrel/knotrel/actions/runs/45058870559) (this fix's branch, `accept: equivalent`) | `1795353290` (2026-11-22 13:14:50 UTC) | **no** | **yes**: `LC_UUID` (16 bytes at offset 2024) and `LC_CODE_SIGNATURE` (32,320 bytes at offset 4117504) excluded; 46 differing bytes in total; both ad-hoc signatures verified (1006 page hashes each), equal apart from the page hashes | identical |

The first macOS run on `main`
([45058549512](https://github.com/knotrel/knotrel/actions/runs/45058549512))
stopped before building: `CARGO_HOME` did not exist yet on the macOS runner.
The script now creates it.

**Verdicts for v0.9.0:**

- **Linux x86-64: byte-identical** on the runner, from the tag and the
  embedded C build time.
- **macOS arm64: not byte-identical; equivalent after excluding `LC_UUID`
  and `LC_CODE_SIGNATURE`**, on the runner as locally.

### Release-process finding

The v0.9.0 binaries contain objects from earlier workflow runs. The release
build restored `Swatinem/rust-cache`, so it recompiled only `knotrel-core`
and `knotrel-server`, and the mimalloc objects came from the cache:

- the macOS binary embeds `Nov 22 2026 13:14:50`, a time inside the v0.9.0
  dry-run build job (13:14:10–13:15:42 UTC), not the publishing run
  (13:24:25–13:25:19 UTC);
- the Linux binary embeds `Nov 21 2026 07:04:04`, the day before, during the
  v0.8.0 release dry run (Linux build job 07:03:34–07:04:53 UTC).

This does not change the code: Cargo reuses a cached dependency only when its
fingerprint (version, features, flags) matches, and the macOS rebuild shows
identical code. But a release binary should be derivable from its tag alone.
From v1.0.0 the `build-binaries` job builds without a cache and sets
`SOURCE_DATE_EPOCH` to the commit time.

### Gate status

The gate is **open**. Steps:

1. ~~The Linux v0.9.0 rebuild~~: done on the runner, byte-identical.
2. ~~The workflow run on `v0.9.0`~~: done (Linux byte-identical; macOS
   equivalent after excluding `LC_UUID` and `LC_CODE_SIGNATURE`).
3. The run on the v1.0.0 release candidate against its dry-run bundle, the
   first release built without cache and with `SOURCE_DATE_EPOCH`; whether a
   macOS result that is equivalent but not byte-identical is accepted needs
   the recorded maintainer decision of the release procedure.

Even when closed, the gate covers Linux and macOS only; Windows remains
untested.
