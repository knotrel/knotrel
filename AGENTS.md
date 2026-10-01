# Development requirements

- Use Rust 1.98.1, edition 2024, and the existing Cargo workspace.
- Keep graph algorithms synchronous in `knotrel-core`; HTTP belongs in `knotrel-server` and uses Axum.
- Write code, comments, Rustdoc and project documentation in English.
- Document every public item, including structs, enums, variants, fields, traits, functions and methods. Explain errors and provide examples where useful.
- Document nontrivial private algorithms: invariants, correctness, time/space complexity, and whether bounds are worst-case, expected or amortized. Cite sources when implementing published algorithms.
- Inherit workspace lints in every crate. Do not suppress missing documentation or introduce unsafe code.
- Preserve Cargo.lock. Use stable Rust features and minimal dependencies.
- Verify formatting, Clippy with warnings denied, workspace tests/doctests, and Rustdoc with warnings denied.
- Keep benchmarks reproducible. Never present unmeasured performance claims or compare different consistency/result contracts.
- Do not commit or push unless the user explicitly asks for it.

## Git & Naming Rulesets

- **Branch Naming**: All branches must follow the pattern:
  `^(feature|bugfix|fix|hotfix|docs|chore|refactor|test|ci|dependabot)/.+$`
  Allowed prefixes: `feature/`, `bugfix/`, `fix/`, `hotfix/`, `docs/`, `chore/`, `refactor/`, `test/`, `ci/`, `dependabot/`.
- **Commit Messages**: All commits and PR titles must follow Conventional Commits matching:
  `^(build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test)(\([a-z0-9_./-]+\))?!?: .+$`
  Examples: `feat(core): add pruning`, `fix(server): handle zero capacity`, `chore: update dependencies`.
