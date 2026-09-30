# Knotrel Copilot & AI Assistant Guidelines

When modifying or generating code for Knotrel, strictly adhere to the following principles:

## 1. Toolchain & Dependencies
- Pin strictly to Rust `1.98.1` and Rust edition `2024`.
- Never introduce new external dependencies to `knotrel-core`. It must remain zero-dependency (core/std only).
- Keep HTTP networking and async runtime concerns strictly within `knotrel-server` (Axum/Tokio).

## 2. Invariants & Complexity
- Graphs are undirected; self-loops are disallowed.
- Link and cut operations must remain idempotent.
- Maintain independent connectivity oracle verification: changes to algorithms must be tested against reference implementations or property matrices.
- The default core is `CompactAdjacency`: queries are $O(V + E)$, updates are amortized $O(\log V + \text{deg}(u) + \text{deg}(v))$.

## 3. Code Standards & Checks
- Always verify changes with:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked`
  - `cargo doc --workspace --no-deps --locked` (with `RUSTDOCFLAGS="-D warnings"`)
- Document all public items with docstrings explaining mutation semantics, error states, and complexity bounds.
