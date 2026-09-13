# Conary resolvo patch

This directory vendors crates.io `resolvo` 0.12.1 under its BSD-3-Clause
license. Conary carries one diagnostic-only change in `src/conflict.rs`:
retain only conflict-graph nodes reachable from the synthetic request root
before rendering an unsatisfiable result. SAT decisions and provider semantics
remain upstream-owned; there is no persisted schema or public API change.

Resolvo's conflict evidence can include a clause for a candidate rejected before
the final root conflict. That node is not necessarily connected to the request's
causal diagnostic graph. Upstream asserts that every collected node is
root-reachable and panics while formatting such evidence. The patch filters
those unrelated nodes, preserving the root proof and returning a normal conflict.

## Reviewed upstream baseline

[Issue #930](https://github.com/FieldmouseWorks/Conary/issues/930) evaluated the
[0.12.1 release](https://docs.rs/crate/resolvo/0.12.1/source/) on 2026-09-13.
The crates.io archive SHA-256 is
`a5d0149e99c7af5382e3da8bb05b23089cf0398e66509f724453ff903004541e`;
its source commit is `d01ba112abbdfe70c1beee1ef806a1131b065d35`.
The upstream conflict implementation is byte-identical to 0.12.0.

The regression in `src/conflict/conary_tests.rs` preserves the original graph
fixture (root → causal → missing, plus unrelated → missing) and now calls
`Conflict::graph` and `display_user_friendly` through a snapshot provider. It
uses no Conary-only production helper, so it can run unchanged in an upstream
source tree. Copy that test file into the next extracted crate and append this
test-only declaration to its `src/conflict.rs`:

```rust
#[cfg(test)]
#[path = "conflict/conary_tests.rs"]
mod conary_tests;
```

Run `cargo test --manifest-path <crate>/Cargo.toml --lib
conflict_graph_discards_learned_branches_unreachable_from_root -- --nocapture`.
The unpatched 0.12.1 run failed at `src/conflict.rs:371:9`:

```text
assertion `left == right` failed
  left: 4
 right: 3
```

The baseline therefore advances to 0.12.1 with the diagnostic fix retained.
The upstream release's runtime optimizations and dependency updates are kept.
Every newer non-yanked release still trips the inventory's exit gate. Remove
this vendor patch only after that release passes the regression without the
production patch and Conary's resolver and candidate-survey proofs pass.
