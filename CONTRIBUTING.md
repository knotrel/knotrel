# Contributing

Use the pinned Rust 1.98.1 toolchain and edition 2024. Keep the core independent
of networking and runtimes; implement HTTP concerns in the Axum server.

Document every exported item in English. Document errors, mutation semantics and
examples where they help callers. Explain complex private code with invariants,
correctness reasoning, worst-case/expected/amortized time, space bounds and any
applicable paper or reference. A passing documentation lint does not establish
that the explanation is sufficient; review the content as well.

Tests should cover observable behavior. Algorithm changes must preserve the
independent connectivity oracle and include the update sequence that motivates
the change. Report benchmark methodology and results before claiming a speedup.

Before submitting changes, run the four development commands in the README.
Keep Cargo.lock up to date and inherit workspace lints in any new crate. Avoid
adding dependencies without a concrete need. Do not change licensing implicitly.
