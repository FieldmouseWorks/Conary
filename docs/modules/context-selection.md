---
last_updated: 2026-09-22
revision: 1
summary: Own the frozen Conary diagnostic corpus, deterministic context baseline, independent labels, and proof for the bounded Redshirt context-selection pilot
---

# Diagnostic Context Selection Corpus

Conary supplies four read-only diagnostic cases to the external Redshirt Rust
context comparison. This does not change production assistant routing, package
behavior, or the suite runner. Consumer execution and exact results belong to
[#1055](https://github.com/FieldmouseWorks/Conary/issues/1055); the shared runner
belongs to [Redshirt #22](https://github.com/FieldmouseWorks/redshirt/issues/22).

`scripts/fixtures/context-selection-v1.json` pins public source revision
`180bd662516080b7523c9cee069396ae14b0f064`, excerpt ranges/hashes, diagnostic
choices, independent labels, and the existing regression tests supporting them.
`scripts/context-selection-corpus.py` exports two separate inputs:

- `manifest.json`: the task, complete diagnosis choices, full root policy,
  actual pinned owner packet, optional source excerpts, and baseline order.
- `oracle.json`: the manifest digest, expected answers, essential evidence IDs,
  and independent proof commands. Redshirt excludes this file from requests.

The exporter runs the pinned router against the pinned ownership map. Required
root policy and the owner packet are never truncated. The deterministic baseline
orders the task's concrete source path first, then the packet's read-first paths,
then other candidate excerpts by path and source line. Redshirt packs up to two
whole optional excerpts under a 30,000-byte context limit. This is a budgeted
expansion of the existing packet, not a measurement of an unrestricted coding
agent that can read more files. The semantic arm uses the same pool and limits.

| Case | Split | Independent behavior |
| --- | --- | --- |
| c1 | calibration | Corrupt selected databases preserve the path and underlying cause in preflight diagnostics. |
| c2 | calibration | Dry-run dependency promotion preserves stored reasons even with `--yes`. |
| c3 | held-out | Obsolete schema state is retained; preflight offers rebuild help without authorizing destructive recovery. |
| c4 | held-out | Ordinary binaries refuse test-hook environment variables before CLI parsing. |

The two database cases share a subsystem. All four are hand-selected and use
closed diagnostic choices; they do not establish general coding-agent accuracy.
Some answers can be inferred from the task or mandatory policy. Essential-chunk
retention is reported separately from answer correctness and cannot prove which
evidence a model used. Prompts, ordering, labels and policy are frozen before any
live call. There is no tuning on the held-out cases or replacement after failure.

## Export And Proof

```sh
python3 scripts/context-selection-corpus.py --check
python3 scripts/context-selection-corpus.py --output /path/to/new-inputs
cargo test --locked -p conary --test cli_diagnostics
cargo test --locked -p conary --test test_hook_ownership
cargo test --locked -p conary --lib commands::install::command::tests::dry_run_preserves_dependency_promotion_state_even_with_yes
```

Use `scripts/dev-build.sh` for the normal shared compiler-cache environment.
The exporter checks source hashes, pinned proof functions, determinism, label
isolation, and rejection of a changed excerpt hash. It makes no network/model
call. Redshirt then performs its own input preflight and injected-transport
protocol check before a separately bounded live campaign. Keep raw artifacts in
the consumer's evidence location; the issue records their digest and outcome.

The pilot uses `jev-1.13.0` for both diagnostic arms and treatment relevance
scoring: at most 12 calls and a $0.04 conservative reservation, with no retry or
replacement. These are protocol ceilings, not reusable authorization for future
campaigns. Redshirt records selector plus diagnostic cost/latency, reservations,
unknown usage, incomplete cases, and zero-provider replay. Its CLI contract is
documented in the shared [context comparison](https://github.com/FieldmouseWorks/redshirt/blob/main/docs/CONTEXT-COMPARISON.md).
