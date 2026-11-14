# Typed Constants Refactor Design

## Intent

Reduce error-prone magic strings and magic numbers in Knotrel's production
Rust sources without changing its HTTP API, configuration syntax, serialized
data, Prometheus exposition, runtime behavior, or supported Rust versions.

## Goals

- Represent closed internal domains with Rust enums and exhaustive matches.
- Give protocol values and repeated invariants one authoritative definition.
- Replace unexplained numeric literals with named constants where the name
  communicates a domain rule, wire-format layout, or operational limit.
- Preserve compile-time checks such as fixed-size metric arrays where useful.
- Keep tests readable: test fixture values and values whose meaning is obvious
  from the assertion are not production magic constants.

## Non-goals

- No wire-format, endpoint, metric-name, label-value, error-code, environment
  variable, default-value, or validation-rule changes.
- No edits to frozen baselines, evidence artifacts, or the separate benchmark
  repository.
- No attempt to replace every string or numeric literal. Human-readable log
  messages, error prose, mathematical identities, indices, test data, and
  one-off values that are self-explanatory remain literals.
- No broad redesign of public APIs or graph algorithms.

## Constraints

- Rust 1.100.0 for the workspace and Rust 1.98.1 for `knotrel-core`.
- Preserve `Cargo.lock` and add no dependency.
- Preserve the repository's missing-documentation, Clippy, Rustdoc, and unsafe
  code policies.
- Do not commit or push as part of this work.

## Approaches Considered

### Selected: native Rust enums and constants

Use focused enums with explicit `as_str`/iteration support for closed domains,
and shared typed constants for route paths, header names, protocol identifiers,
limits, bit fields, and other repeated invariants. This keeps mappings explicit,
supports exhaustive matching, preserves fixed-size arrays, and adds no runtime
or dependency cost.

### Rejected: derive mappings and iteration with `strum`

`strum` would remove some small `match` expressions and manually maintained
variant arrays, but those expressions are also the explicit audit point for
the externally stable strings. Several metric structures deliberately use
fixed-size arrays whose lengths encode invariants, so derive-based iteration
would not eliminate all associated bookkeeping. The limited boilerplate
reduction does not justify another dependency or its MSRV/upgrade surface.
This is a conscious project choice, not an omission: reconsider `strum` if the
number of closed string domains grows enough that manual mappings become a
demonstrable maintenance problem.

### Rejected: pervasive newtypes for all strings and numbers

Wrapping every route, header, error code, label, and numeric quantity would
maximize type distinctions but create widespread conversion noise and a much
larger compatibility risk. Newtypes are reserved for places where invalid
states otherwise remain representable, not used merely to eliminate literals.

## Design

1. Inventory production literals in `knotrel-core/src` and
   `knotrel-server/src`, classifying each as a closed domain, shared protocol
   constant, numeric invariant, algorithmic literal, or ordinary prose/data.
2. Consolidate HTTP paths, custom header names, the sole supported graph ID,
   and repeated error/metric label values at the narrowest shared module
   boundary that avoids dependency cycles.
3. Reuse existing domain enums (`Phase`, `SyncStatus`, metric classifications,
   API operation/result types) rather than maintaining parallel string lists.
   Add small private enums only where a real closed domain currently lacks one.
4. Name repeated validation limits and packed-state masks/shifts in
   `service.rs`; keep obvious zero/one arithmetic and algorithm-derived values
   inline.
5. Type configuration keys only if doing so removes duplicated matching and
   lookup strings without making parsing less clear; external environment names
   remain strings at the input boundary.
6. Add focused tests that fail before each refactor wherever a new mapping or
   shared invariant can regress externally visible output. Existing contract
   tests remain the main proof that behavior is unchanged.

## Acceptance Criteria

- Closed domains no longer require parallel handwritten string arrays where an
  existing enum can authoritatively produce the value.
- Repeated protocol strings and meaningful numeric limits have one definition.
- Public behavior and all documented external values remain byte-for-byte
  compatible.
- Formatting, Clippy with warnings denied, workspace tests and doctests, and
  Rustdoc with warnings denied pass; `knotrel-core` is also checked with its
  Rust 1.98.1 MSRV.
- A future maintainer can find the recorded rationale for not adding `strum`.
