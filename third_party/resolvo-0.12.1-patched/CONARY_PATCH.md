# Conary resolvo patch

This directory vendors crates.io `resolvo` 0.12.1 under its BSD-3-Clause
license. Conary carries two scoped fixes:

- `src/conflict.rs` retains only conflict-graph nodes reachable from the synthetic
  request root before rendering an unsatisfiable result.
- `src/solver/encoding.rs` groups forbid-multiple clauses by each provider's
  concrete name when a virtual capability has mixed-name candidates. This is
  backported from [upstream PR #293](https://github.com/prefix-dev/resolvo/pull/293),
  commit `dfb403fce20c989a150ef5bbf89c2d2c1e2b3199`, still a draft at review time.

There is no persisted schema or public API change.

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

The first Conary resolver run with only that patch passed 149 tests, ignored one,
and failed `root_with_and_without_require_same_provider_facts` at
`src/solver/encoding.rs:441`: `all candidates in a version set must have the same
package name`. Upstream PR #293 confirms virtual providers are valid and corrects
this new 0.12.1 assumption. In release builds the same bug puts unrelated
packages in one at-most-one bucket, incorrectly rejecting valid selections.

`src/solver/conary_tests.rs` backports upstream's regression through its public
snapshot provider. It requires a virtual capability and both concrete providers.
To test a future upstream release, copy this file and append
`#[cfg(test)] mod conary_tests;` to `src/solver/mod.rs`, alongside the conflict
test wiring above. Run both library modules with `cargo test --manifest-path
<crate>/Cargo.toml --lib conary_tests`, and repeat with `--release` to cover SAT
correctness with debug assertions disabled.

The baseline therefore advances to 0.12.1 with both fixes retained. The remaining
upstream runtime optimizations and dependency updates are kept.
Every newer non-yanked release still trips the inventory's exit gate. Remove
this vendor patch only after that release passes both regressions without the
production patches and Conary's resolver and candidate-survey proofs pass.
