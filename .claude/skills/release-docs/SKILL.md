---
name: release-docs
description: Prepare the user-facing documentation for a Knotrel release. Use during release preparation (or after a release, when pages still describe an older one) to run the release-surface check, propose wording updates for every failing surface from the release notes and the benchmarks decision record, verify links and doctests, and open a pull request for review. Never tags, publishes, merges or pushes without explicit maintainer instruction.
---

# Release documentation

The maintainer invokes this skill while preparing release `X.Y.Z` (or to
bring pages up to the latest release). The deterministic check decides what
is stale; this skill only drafts the text that fixes it, for review.

## Hard limits

- Never create or push a tag, publish a GitHub Release, crate or image,
  merge a pull request, or push to `main`. Pushing the documentation branch
  and opening its pull request is allowed only when the maintainer asked for a
  PR in this invocation; otherwise stop after the local commit (or before it,
  if they did not ask for commits) and show the diff.
- Never change a verdict, decision, number, interval or classification. Copy
  them from the release notes and the benchmarks decision record; if a
  surface disagrees with them, the surface is wrong.
- Keep every limitation, exception, non-PASS result and "not qualified"
  statement visible where it was, in at least the same strength. Do not
  summarize "accepted with exceptions" as "accepted" or claim a speedup that
  the decision record does not state (AGENTS.md: no unmeasured claims, no
  comparison of different contracts).
- Do not decide whether a release is performance-qualified. If the latest
  release has no release-to-release qualification, ask the maintainer whether
  to add it to `[performance] not_qualified` in
  `docs/releases/release-surfaces.toml`; that is their decision.
- Follow AGENTS.md: English, Conventional Commits, branch prefix `docs/`,
  every item of "Before release" that the change touches.

## Steps

1. **Run the check.** From the knotrel checkout, with knotrel-benchmarks next
   to it if available:

   ```bash
   python3 scripts/check-release-surfaces.py --release X.Y.Z
   ```

   Without `--release` it checks against the latest released notes (what CI
   runs). Collect every failing surface. If the knotrel-benchmarks checkout is
   missing, say so: its surfaces were not checked. Also run
   `python3 scripts/check-readme.py --release X.Y.Z` for the READMEs.

2. **Read the sources of truth**, and nothing else, for the new wording:
   - `docs/releases/vX.Y.Z-release-notes.md` (status, "What is new",
     "Performance qualification", outstanding gates, limitations);
   - in knotrel-benchmarks, `results/vX.Y.Z-qualification/DECISION.md` and the
     report it links (`docs/releases/X.Y.Z.html`, `docs/data/releases.json`);
   - the previous wording of each surface, to keep its structure and links.

3. **Propose the edits**, one surface at a time, smallest change that makes
   the surface name the right version and describe the current evidence:
   - release surfaces: the version, links to the release notes and release
     assets that exist (never to assets that do not exist yet);
   - performance surfaces: the new comparison first
     (`## vX.Y.Z compared with vW` in `docs/performance-by-version.md`), the
     previous one moved to `docs/performance-history.md` unchanged;
   - remove "unreleased" / "X.Y candidate" labels for the released version
     only; keep labels for features still unreleased.
   Show the maintainer each surface's diff with the source sentence it is
   based on. If a pattern in `release-surfaces.toml` no longer fits wording
   that is correct, update the pattern in the same change and say why.

4. **Verify.**
   - `python3 scripts/check-release-surfaces.py --release X.Y.Z` and
     `python3 scripts/check-readme.py --release X.Y.Z` pass;
     `python3 tests/test_release_surfaces.py` and
     `python3 tests/test_readme_check.py` pass.
   - Relative links in every edited Markdown file resolve to existing files
     and headings; external links point to pages that exist (release notes at
     the tag, the benchmarks report).
   - If crate docs or READMEs with code changed:
     `cargo test --workspace --doc --locked --all-features` and
     `python3 scripts/check-crate-docs.py`.
   - The documentation site prepares: `python3 scripts/prepare-docs-site.py <dir>`.

5. **Open the pull request** (only if the maintainer asked for one): branch
   `docs/vX.Y.Z-release-surfaces`, Conventional Commit such as
   `docs(release): describe vX.Y.Z on the release surfaces`. In the body, list
   each surface changed, the source of each statement, and anything left for
   the maintainer (not-qualified decision, knotrel-benchmarks PR, surfaces not
   checked). Changes to knotrel-benchmarks go in a separate PR in that
   repository. Then stop: tagging and publishing follow
   `docs/releases/release-procedure.md` with the maintainer.
