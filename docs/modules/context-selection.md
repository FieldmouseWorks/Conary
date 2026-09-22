---
last_updated: 2026-09-22
revision: 4
summary: Record both completed context comparisons, independent checks, fixed-model follow-up results, and the held-out evidence-completeness limitation
---

# Diagnostic Context Selection Corpus

The fresh follow-up is owned by [#1057](https://github.com/FieldmouseWorks/Conary/issues/1057)
and [Redshirt #24](https://github.com/FieldmouseWorks/redshirt/issues/24).
`scripts/fixtures/context-selection-v2.json` pins four new cases at the same
product revision as v1. The shared Rust runner owns BM25 task/identifier retrieval,
Jev selection, fixed-model diagnostics, budgets, receipts and replay. Conary
continues to own only corpus export and independent product checks.

| Fresh case | Split | Independent behavior |
| --- | --- | --- |
| c5 | calibration | Absent, unspecified and exact installed-release selectors have distinct resolution results. |
| c6 | calibration | Repeated promised-path claims deduplicate, cannot witness content, and conflict with shipped ownership. |
| c7 | held-out | Exact command-line prefixes, presence and argument order agree between Rust boot policy and the shell adapter. |
| c8 | held-out | Equal timestamps differ between static and generic trust modes; cached children remain hash checked. |

Each case offers six production excerpts and five diagnosis choices, including
insufficient evidence. Both arms retain the full pinned root policy and actual
router output, then select at most two excerpts under the same byte cap. The
pinned router has no owner-card match for c5's `package_target.rs`; its real
fallback is preserved verbatim in both arms. No owner packet is invented.
The separate routing repair is tracked in [#1058](https://github.com/FieldmouseWorks/Conary/issues/1058).
Cases, labels, ordering, prompt and baseline are frozen before any live call.
The first pilot's cases are unchanged and excluded from this follow-up.

Export v2 using a host-frozen Codex profile supplied to Redshirt's documented
version-2 contract:

```sh
python3 scripts/context-selection-corpus.py --check --spec scripts/fixtures/context-selection-v2.json
python3 scripts/context-selection-corpus.py --spec scripts/fixtures/context-selection-v2.json \
  --codex-profile /path/to/frozen-profile.json --output /path/to/new-inputs
cargo test --locked -p conary --lib commands::package_target::tests::selector_
cargo test --locked -p conary-core --lib repository::dependency_model::tests::
cargo test --locked -p conary-core --lib generation::verity_policy::tests::
cargo test --locked -p conary-core --lib trust::client::tests::
```

A v2 self-check without output uses a synthetic profile and makes zero provider
calls. Live export requires a real frozen profile; local executable/catalog and
ambient host captures remain outside Git. The completed allowance was one campaign,
at most four Jev selector calls (USD0.012 conservative ceiling) and eight Codex
diagnostic turns (60 seconds each). Both arms request `gpt-6-astra` at low effort
through the same pinned CLI binary/catalog. Codex uses existing authentication;
subscription billing is unknown and its token/cache usage is reported separately
from Jev estimates. CLI turns are not a measurement of internal HTTP attempts.
No retries, replacements or continuation after failure are permitted. These
diagnoses do not execute tools or modify the product. Results cannot establish
general coding-agent performance or justify changing production routing alone.

## 2026-09-22 Fixed-Model Follow-Up Result

One frozen campaign completed four Jev selection calls and eight Codex diagnostic
turns, without retries, replacement, fallback, unexpected tool events or failure.
Redshirt runtime/test revision was `9e9a5ed4eb15027a5510ea08bfc0b6b1e83faebe`;
Conary exporter/corpus revision was `79ce07ed20718e5b971b258c3b45ebc242b1a89b`.
Both arms requested `gpt-6-astra`, low effort, through CLI `0.154.0` with the same
frozen executable/catalog. This pins the request configuration, not an immutable
backend snapshot. Actual model prompts also include CLI/global instructions;
only their offline capture was audited, and temporary path/session fields vary.

| Metric | Rust BM25 baseline | Jev-selected context |
| --- | ---: | ---: |
| Resolved against frozen answer key | 2/4 | 3/4 |
| Calibration resolved | 1/2 | 2/2 |
| Held-out resolved | 1/2 | 1/2 |
| Insufficient-evidence answers | 2 | 1 |
| Declared essential excerpts retained | 6/8 | 8/8 |
| Mean encoded context bytes | 19,978.75 | 19,778 |
| Codex input / output tokens | 27,648 / 97 | 27,461 / 136 |
| Codex cached-input tokens | 0 | 0 |
| Median case elapsed time, including selection | 5.057 s | 4.832 s |
| Total observed provider time | 20.627 s | 22.290 s |

Jev added 30,739 input and 328 output tokens over four selector calls, estimated
at USD0.001291038. Its conservative reservation was USD0.011010048 under the
USD0.012 ceiling. Codex subscription billing and combined dollar cost remain
unknown. Campaign wall time was 43.871 s. Median latency fell slightly while
total provider time rose; four observations with uncontrolled service/startup
variation do not establish a latency or cache advantage.

The gain was c5: BM25 chose the resolver and a query caller; Jev selected the
resolver and release-matching implementation, changing insufficient evidence to
the correct diagnosis. Both arms selected identical actual packets for c6/c7
and resolved them. Both abstained on c8. Post-run review found that c8's frozen
`essential` list names b/e but omits c, the strict version-increase helper needed
to establish Generic equal-version behavior. The treatment packet therefore
still relies on an unshown helper despite meeting the declared evidence label.
This limits the fixture and the interpretation of 8/8 retention; the experiment
does not identify the model's internal reason for abstaining. No labels, cases
or grades were changed after collection. A fresh completeness audit is required
before another campaign, tracked in [#1060](https://github.com/FieldmouseWorks/Conary/issues/1060).
Jev remains experimental; there is no held-out gain
here and no production routing change.

The exact manifest SHA-256 was
`0e260354d3eca545c109af6e5a624ad1b369e6e718710f7e63de5a7d19eb6626`;
oracle `2ab5ea86b09c18684016c2ae3206ec74957f52b3a8b94dadcb976574aa2cb7e4`;
coding profile `80dc6800f2a2b5f1bca767d87fb0d04abba290782437e938879478e54b87e5ca`;
live report `af21ea01a85762c2d56742ba12a91181867f97ea885099140d55cfa749a44cb9`.
Raw host/CLI artifacts remain local. Rust replay and a separate Python audit
verified request binding, selections, mandatory policy, answers, grades, tokens
and reservations with zero model calls. The live allowance is closed.

Self-review: 41 all-feature and 39 default Redshirt tests, both clippy modes,
fmt/build, v1 live/mock replay, v2 mock replay, both exporter self-checks, router
tests and documentation truth passed. Fresh consumer backing proofs passed
11 selector, 17 capability, 2 boot-policy and 16 trust tests. Those product tests
ran in the preceding corpus checkout, whose app/core/packaging/Cargo/vendor
sources were verified identical to the pinned source and this checkout.
No delegated coding worker was used; the child CLI only supplied benchmark
diagnoses. Review and protected integration remain owned by the stacked PRs.

## First Pilot

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
documented in the verified shared [context comparison](https://github.com/FieldmouseWorks/redshirt/blob/51ccd7b4b0ce059d2bf0bb5bcbf7bf0e3abe149c/docs/CONTEXT-COMPARISON.md).

## 2026-09-22 Pilot Result

One frozen campaign completed all 12 calls, with no failure, retry, replacement,
fallback, or tuning. Redshirt runtime/test revision was
`51ccd7b4b0ce059d2bf0bb5bcbf7bf0e3abe149c`; the Conary exporter/corpus revision was
`9564efd458cdb8f039bb3209dd8162e71f5ba77b`. Public source remained pinned to
`180bd662516080b7523c9cee069396ae14b0f064`. Paired reviews are
[Redshirt #23](https://github.com/FieldmouseWorks/redshirt/pull/23) and
[Conary #1056](https://github.com/FieldmouseWorks/Conary/pull/1056).

| Measurement | Deterministic baseline | Jev selection plus diagnosis |
| --- | ---: | ---: |
| Calibration correct | 1/2 | 2/2 |
| Held-out correct | 2/2 | 2/2 |
| Insufficient-evidence answers | 1 | 0 |
| Labelled essential excerpts retained | 3/8 | 8/8 |
| Calls | 4 | 8 |
| Reported input tokens | 18,839 | 47,095 |
| Estimated provider cost, USD | 0.000791238 | 0.001977990 |
| Sum of provider time | 1.680 s | 2.264 s |
| Median provider time per case | 303.3 ms | 572.8 ms |
| Mean encoded diagnosis context | 16,193.5 bytes | 17,249 bytes |

Treatment cost and time include all four relevance calls. The entire campaign
took 4.675 s including local evidence processing and writes. The selector took
1.195 s in total; its downstream diagnoses took 1.069 s. Total reported usage was
65,934 input and 786 output tokens. At the published pinned-model
[input price](https://docs.typesafe.ai/models), the estimate is USD0.002769228;
actual billing and cache usage are unknown. The conservative reservation remains
USD0.033030144. All usage was known, all 32 typed answers passed validation, and
all probability totals were classified exact without normalization.

| Case | Baseline selection; answer | Jev selection; answer | Encoded bytes, baseline / Jev |
| --- | --- | --- | ---: |
| c1 | s3, s1; d2, correct | s3, s4; d2, correct | 14,834 / 16,190 |
| c2 | s1, s2; insufficient | s3, s1; d1, correct | 20,371 / 20,800 |
| c3 | s3, s1; d3, correct | s4, s5; d3, correct | 14,844 / 16,882 |
| c4 | s6, s1; d2, correct | s6, s2; d2, correct | 14,725 / 15,124 |

The extra correct diagnosis was c2, a calibration case: selected evidence added
the implementation that returns from dependency promotion without writing during
a dry run. Held-out accuracy tied. The selection arm retained every predeclared
essential excerpt, while the baseline omitted five. Required policy/owner packet
hashes matched across all three stages of every case, and excerpt hashes remained
valid. Rust replay re-rendered every request and reproduced selections, grades,
usage and reservations with zero provider calls; a separate offline calculation
confirmed the aggregate answers, evidence retention, token counts and costs.

Self-review proof before collection: eight CLI diagnostic tests, two test-hook
ownership tests and the dry-run promotion test passed. Export/source/label
checks, all 22 ownership cards, router self-tests and documentation truth checks
passed. Redshirt's all-feature suite passed 35 tests, with fmt/clippy passing;
injected tests exercised failures, interruption and evidence tampering. The
issue and PR retain exact commands and hosted-CI status separately.

The result supports a further controlled comparison. It does not establish a
general accuracy advantage: four hand-picked cases are too few, two share a
subsystem, the downstream model is Jev, and correct answers were sometimes
possible without all labelled evidence. The identical budget admitted about
6.5% more actual bytes in the treatment. Treatment cost was 2.5 times baseline
and median latency was higher. The first call was a baseline request;
connection/startup effects were not measured separately. No cache or causal
latency benefit is established.
Production routing is unchanged. A next experiment should use fresh cases,
a stronger deterministic baseline, and a fixed downstream coding model with its
own bounded allowance. This campaign's allowance is closed.

Raw receipts remain in consumer-owned local evidence. These SHA-256 identities
bind the retained artifacts without publishing host-local paths or credentials:

| Artifact | SHA-256 |
| --- | --- |
| Frozen manifest | `7b2e8c529c4b8138f739f6772c29f5b05f5e9ee895f2fc57313220bc548ed6e4` |
| Independent oracle | `8b173ea90221dd6b6a7a7815cea1fe9e58cd378b543fa4d92260001d1300fd89` |
| Exact call receipts | `5bd78e5952dd928fc66ff0b46cade50cee573a75d0d4ecb92c127b0bab3b7a7d` |
| Campaign report | `fefc8af873e51db54aae032693f2b1a0a63e556fcd8e110f58c7d7a7140ffb63` |
| Offline replay report | `1af5434d2b0525bc7bab190616dffd80e856cfae8e11808d7f578dbedd211365` |
