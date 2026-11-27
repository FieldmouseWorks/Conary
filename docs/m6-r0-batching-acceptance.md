# M6 slices 6, 8 and exit: bounded plan for R0, batching and acceptance

This is [#427](https://github.com/FieldmouseWorks/Conary/issues/427)'s bounded
delivery plan (child of [#256](https://github.com/FieldmouseWorks/Conary/issues/256)).
It plans **only** the four existing authorized implementation issues below,
reusing them without creating duplicate feature owners:

- **Slice 6 — R0 post-landing review:**
  [#317](https://github.com/FieldmouseWorks/Conary/issues/317)
- **Slice 8 (part 1) — deterministic batch proposals:**
  [#319](https://github.com/FieldmouseWorks/Conary/issues/319)
- **Slice 8 (part 2) — one-gate batch execution through the train:**
  [#320](https://github.com/FieldmouseWorks/Conary/issues/320)
- **Milestone exit — M6 acceptance report:**
  [#321](https://github.com/FieldmouseWorks/Conary/issues/321)

This task only plans these four deliveries. It performs no repository cutover,
no live failure drill, no week-long observation, and no code implementation,
and it grants no authority beyond
[#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s boundaries
([m6.md](m6.md#boundaries)). Reconciled 2026-11-27 against Conary's `main` at
`44e82bd`, [DESIGN 12](DESIGN.md#12-roadmap), [m6.md](m6.md),
[m6-order.md](m6-order.md), [m6-red-trunk.md](m6-red-trunk.md), and
[lessons.toml](lessons.toml).

## Ground rules

These hold for every delivery below and are not repeated in each section.

- **One feature owner per existing issue.** `#317`, `#319`, `#320`, and `#321`
  remain the sole feature owners for R0 post-landing review, batch proposals,
  batch gating, and M6 acceptance. `#317`, `#319`, and `#320` each stay a
  single Conary PR. `#321` splits into two bounded children (`#321a` and
  `#321b`) under `#321` so report code and fixture tests land before the live
  seven-day observation window without duplicating ownership.
- **Keep `#319` strictly before `#320`.** Deterministic read-only batch
  proposals land and verify first; `#320` consumes them inside the existing
  train.
- **Reuse existing evidence and deferral workstreams.** `#320` reuses the
  shared exact-tree evidence lookup owned by
  [#291](https://github.com/FieldmouseWorks/Conary/issues/291) /
  [#324](https://github.com/FieldmouseWorks/Conary/issues/324) (Conary's lookup
  landed in [#380](https://github.com/FieldmouseWorks/Conary/pull/380),
  documented in [gate-reuse.md](gate-reuse.md)) and the deferred-check
  contract owned by
  [#296](https://github.com/FieldmouseWorks/Conary/issues/296)–[#302](https://github.com/FieldmouseWorks/Conary/issues/302)
  (Conary's core implementation merged in
  [#333](https://github.com/FieldmouseWorks/Conary/pull/333), documented in
  [flow.md](flow.md#deferred-checks)). Neither `#319` nor `#320` adds a
  competing gate scheduler.
- **Standalone without burrow or Nix (`reusable-modules`).** All logic lives in
  `conary-core` and `conary-github`; `conaryd` and `cn` stay thin wrappers.
  Every delivery works on an ordinary Linux host with `cargo install`, a GitHub
  token, a ledger, and generic JSON configuration
  ([DESIGN finish line item 7](DESIGN.md#12-roadmap)). Tests run against
  `conary-github::sim` with generic repositories, never machine-specific wiring.
- **Event-driven, never a polling timer or resident waiter (`waiting-costs-nothing`).**
  Post-landing review waits (`#317`), batch admission (`#320`), and the
  seven-day acceptance window (`#321`) advance on webhooks, worker exit kicks,
  persisted `events::Queue` deadlines, or one-shot event-based job resumption —
  never a cron loop or sleeping process.
- **Append-only op log; backward-compatible schemas.** No op is ever rewritten
  or deleted. Any new optional field on an existing op (such as `paths` on
  `Body::Classify` or `batch` on `Body::Candidate`) uses
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so historical
  op bytes, IDs, and signatures remain bit-for-bit unchanged.
- **Exact-tree independently attested evidence (`same-inputs-same-verdict`).**
  Evidence proves only the exact tree, check identity, and gate definition it
  ran on, backed by producer attestation (`Attest::GithubToken` or
  `Attest::Service`). R0 never replaces pre-landing evidence with post-landing
  testing. Equal trees never carry a review or judge verdict. R2 changes always
  ride alone on candidate-tree evidence bound to the exact trunk base.
- **Fail closed on degraded or missing observations (`degraded-counts-as-failed`).**
  Unreadable records, truncated GitHub listings, unverified lookups, or missing
  acceptance observations record degradation or report `incomplete` — never a
  quiet pass.
- **Spenders off by default; live settings schema (`spenders-off-by-default`,
  `one-settings-source`, `live-settings`, `pause-vacates`).** Every new toggle
  is declared in `flow/schema.rs`, defaults to `false` (off), takes effect at
  the next decision without deploy, and obeys `cn flow pause` immediately.
- **No model in mechanical steps (`no-model-in-mechanical-step`).** Batch
  grouping (`#319`), train gating/splitting (`#320`), post-landing R0
  eligibility/escalation (`#317`), and acceptance accounting (`#321`) make zero
  model calls. `#319` does not implement the optional intent-classification
  experiment from [DESIGN 12](DESIGN.md#12-roadmap).

## Original M6 exit criteria: fixtures and real observation sources

Every original exit criterion from the M6 row of
[DESIGN 12](DESIGN.md#12-roadmap) and
[#256](https://github.com/FieldmouseWorks/Conary/issues/256) is assigned below
to both deterministic verification fixtures and a real read-only observation
source, with explicit fail-closed / incomplete rules enforced by `#321`.

| Original M6 exit criterion | Owning deliveries | Verification fixtures | Real observation source | Incomplete / fail-closed rule |
| --- | --- | --- | --- | --- |
| **`steward.py` is deleted** (and active landing-flow wiring absent) | [#316](https://github.com/FieldmouseWorks/Conary/issues/316) ([m6-upkeep.md](m6-upkeep.md#d-delete-the-stewards-python-s4): `D1` `burrow#1310`, `D2` `remi#1463`, `D3` `#494`), checked by `#321` | **AC-1** (clean tree/unit/config snapshot passes); **AC-6** (negative: `steward.py`, `modules/autoland/*.py`, `autoland-steward` unit, `autoland-shadow@`, legacy judgment readers, or `settings.landing = steward` present -> `Fail`); **AC-4** (missing snapshot -> `Incomplete`) | Read-only repository scans of `burrow`, `remi`, and `Conary` HEAD commits (`rg` for `steward.py`, `modules/autoland/.*\.py`, `autoland-steward`, `autoland-shadow@`), plus live deployment probe of active systemd units, watchdog sources, and `settings.landing` across all discovered repos | Missing repo or live-probe snapshot -> `Incomplete`; any matching file, unit, watchdog source, or `steward` owner -> `Fail` |
| **A continuous week of PRs across every managed repository lands through `conaryd`** (with attested evidence for each landed tree) | [#313](https://github.com/FieldmouseWorks/Conary/issues/313), [#314](https://github.com/FieldmouseWorks/Conary/issues/314), [#315](https://github.com/FieldmouseWorks/Conary/issues/315) ([m6-cutover.md](m6-cutover.md): `S2a`–`S2f`), [#423](https://github.com/FieldmouseWorks/Conary/issues/423) (`FP12`), checked by `#321` | **AC-1** (complete 7-day multi-repo ledger + GitHub fixture, all `conaryd`-landed with attested evidence); **AC-2** (`< 168h` window -> `Incomplete`); **AC-3** (managed repo omitted -> `Incomplete`); **AC-4** (degraded/truncated GitHub read -> `Incomplete`); **AC-5** (`Outside` op, steward landing, or unattested landed tree -> `Fail`) | Anchored op log (`Body::Land`, `Body::Candidate`, `Body::CandidateEvidence`, `Body::Finish`, `Body::Evidence`, `Body::Outside`, `Body::Degraded`) joined with live managed-repository discovery (`U2` / `settings.landing`) and read-only GitHub PR, commit, and attestation records across one unbroken `[T_start, T_start + 7d]` UTC window after the last prerequisite deploy | Window `< 7 * 86400s`, any discovered repo missing, any degraded/truncated read, or `0` total landings -> `Incomplete`; any steward landing, unresolved `Outside` op, or unattested landed tree -> `Fail` |
| **R0 PRs land with no wait on a reviewer** (while keeping attested pre-landing evidence and durable post-landing review/repair) | [#317](https://github.com/FieldmouseWorks/Conary/issues/317), checked by `#321` | **R0-1..R0-10** in `#317` (R0 lands while review pending, non-train `deferred::start` dispatches without waiting on review, post-landing review resumes after close/restart, findings trigger `fix`, unfixed P1 escalates to `cn undo`, R1/R2/raised/unattested blocked); **AC-1** (valid R0 sample with zero pre-landing reviewer wait); **AC-7** (R0 waited on reviewer -> `Fail`; zero R0 landings in window -> `Incomplete`) | Anchored op log (`Body::Classify` with `class == R0`, `docs == false`, `raised` containing neither `review` nor `r2`; `Body::Land`; pre-landing attested `Body::Evidence`; post-landing `Body::Evidence { kind: Review }`; `Body::Undo` on P1 escalation) reconciled with `post-review.json` status and read-only GitHub review/PR timestamps for every landed R0 PR | Zero eligible R0 landings observed or missing review timestamps -> `Incomplete` (never substitute estimates); any eligible R0 PR that waited in `Stage::Review` before landing or landed without attested evidence -> `Fail` |
| **Fewer gate runs than PRs landed, counting PR, candidate, and trunk gate runs together** | [#319](https://github.com/FieldmouseWorks/Conary/issues/319) -> [#320](https://github.com/FieldmouseWorks/Conary/issues/320), reusing [#291](https://github.com/FieldmouseWorks/Conary/issues/291)/[#324](https://github.com/FieldmouseWorks/Conary/issues/324) and [#296](https://github.com/FieldmouseWorks/Conary/issues/296)–[#302](https://github.com/FieldmouseWorks/Conary/issues/302) (PR [#333](https://github.com/FieldmouseWorks/Conary/pull/333)), checked by `#321` | **BP-1..BP-8** in `#319`; **BG-1..BG-10** in `#320` (shared candidate gate, changed head/trunk, stale evidence, red split, restart, PR runs counted); **AC-1** (`total_gate_runs < prs_landed`); **AC-8** (`candidate + trunk < prs_landed` alone, but adding PR gate runs makes `total_gate_runs >= prs_landed` -> `Fail`) | Anchored op log (`Body::Land`, `Body::Candidate`, `Body::CandidateEvidence`, `Body::Finish`, `#320`'s gate execution/reuse provenance) reconciled with read-only GitHub Actions workflow runs and check runs across every managed repo in `[T_start, T_start + 7d]`: `total_gate_runs = pr_gate_runs + candidate_gate_runs + trunk_gate_runs` (verified zero-build exact-tree reuse hits counted separately, never hidden) | Truncated Actions pagination, unreadable repo runs, or `prs_landed == 0` -> `Incomplete`; `total_gate_runs >= prs_landed` -> `Fail` |
| **A deliberately broken landing is found and undone with no model job** (and no ordinary person intervention) | [#318](https://github.com/FieldmouseWorks/Conary/issues/318) ([m6-red-trunk.md](m6-red-trunk.md): `RT1`–`RT4`, reusing [#199](https://github.com/FieldmouseWorks/Conary/issues/199)), checked by `#321` | `RT4`'s automated full-stack drill (`crates/conary-github/tests/red_trunk_drill.rs` with real `conary-plugin v5` deterministic proof binary, Ed25519 producer, poisoned sentinel for `fix`/`judge`/`job`); **AC-1** (passing `verify-drill` JSON); **AC-9** (missing drill -> `Incomplete`; nonzero model calls, nonzero owner calls, or unverified culprit -> `Fail`) | Output of `cn flow red-trunk verify-drill OWNER/REPO --culprit <B>` against the anchored ledger after `RT4`'s controlled live drill on the dedicated R0 drill repository, backed by immutable `Body::Land`, verified `Attest::Service` `Body::TrunkEvidence`, `Body::Undo`, and zero model/owner invocations | Missing drill artifact or unverified ledger evidence -> `Incomplete`; failed drill predicate, nonzero model calls, or nonzero owner requests -> `Fail` |
| **`cn lessons exit M6` passes on `main`** (and every `designed` entry for an M6 component is listed with its building task) | [#305](https://github.com/FieldmouseWorks/Conary/issues/305) (`#379`), [#306](https://github.com/FieldmouseWorks/Conary/issues/306) (`#391`), checked by `#321` | `crates/conary-core/tests/lessons.rs`; **AC-1** (`open == []` with `designed` M6 entries listed alongside owning tasks -> `Pass`); **AC-10** (`open != []`, e.g., `scope-below-a-repo` while still `open` -> `Fail`) | Live execution of `cn lessons check` and `cn lessons exit M6` against `docs/lessons.toml` at `main` HEAD during `#321b` report generation | Register unreadable -> `Incomplete`; any `open` lesson applying to an M6 component -> `Fail` |

## Dependency order and reuse edges

### Dependency graph

```text
Existing landed / in-flight prerequisites              Planned deliveries (#427)
─────────────────────────────────────────              ─────────────────────────
main (44e82bd) ──────────────────────────────────────► #317 (R0 post-landing review, 1 PR) ──────────┐
                                                                                                     │
main (44e82bd) ──────────────────────────────────────► #319 (batch proposals, 1 PR, read-only)       │
                                                             │                                       │
#291 (#380 lookup merged) ─────────────────────────────┐     ▼                                       │
#296 (PR #333 merged; #297-#302 rollout open) ─────────┼──► #320 (one-gate batching, 1 PR) ──────────┼──► #321a (report code,
                                                       │                                             │           fixtures, runbook)
#199 + #318 (RT1-RT3) ──► RT4 (verify-drill contract) ─┼─────────────────────────────────────────────┘            │
                                                       │                                                          ▼
Live deployment prerequisites (before 7-day window):   │                                                 #321b (7-day live week,
  - Cutovers S1 (#314) + S2a-S2f (#315, #499-#505)     │                                                        live RT4 drill,
  - Python deletion D1-D3 (#316, burrow#1310,          │                                                        steward absent,
    remi#1463, #494)                                   │                                                        cn lessons exit M6)
  - FP12 (#423, closed) deployed live                  │                                                          │
  - #317, #318 (RT1-RT4), #320 deployed live           │                                                          ▼
  - #324 (#389) gate reuse & #297-#302 deferral active │                                                   M6 complete -> closes #321,
    where configured                                   └─────────────────────────────────────────────────► unblocks #256
```

### Verified status of referenced issues (2026-11-27)

Every external issue referenced by this plan is reused by its existing owner and
never re-owned here:

| Issue / PR | State (2026-11-27) | Role in this plan (reused, never duplicated) |
| --- | --- | --- |
| [#256](https://github.com/FieldmouseWorks/Conary/issues/256) | Open | Parent M6 tracker; defines boundaries and closes only when `#321` reports all criteria passing |
| [#291](https://github.com/FieldmouseWorks/Conary/issues/291), [#322](https://github.com/FieldmouseWorks/Conary/issues/322), [#323](https://github.com/FieldmouseWorks/Conary/issues/323), [#380](https://github.com/FieldmouseWorks/Conary/pull/380) | `#322`/`#323` closed, PR `#380` merged; `#291` open | Owns exact-tree gate reuse lookup (`conary_github::reuse`, [gate-reuse.md](gate-reuse.md)); consumed by `#320` for exact-tree batch/trunk evidence reuse |
| [#324](https://github.com/FieldmouseWorks/Conary/issues/324), [#389](https://github.com/FieldmouseWorks/Conary/issues/389) | Open | Owns live gate-reuse activation on remi (`#324`) and runner-hour audit (`#389`); avoids duplicate main-push gate builds counted by `#321` |
| [#292](https://github.com/FieldmouseWorks/Conary/issues/292), [#296](https://github.com/FieldmouseWorks/Conary/issues/296)–[#302](https://github.com/FieldmouseWorks/Conary/issues/302), PR [#333](https://github.com/FieldmouseWorks/Conary/pull/333) | PR `#333` merged (2026-11-23); `#292`, `#296`–`#302` open | Owns deferred heavy checks (`settings.deferred`, [flow.md](flow.md#deferred-checks)), burrow/repo rollout (`#297`–`#301`), and deferred-gate week report (`#302`); reused by `#317` and `#320` without adding a second gate scheduler |
| [#313](https://github.com/FieldmouseWorks/Conary/issues/313), [#314](https://github.com/FieldmouseWorks/Conary/issues/314), [#315](https://github.com/FieldmouseWorks/Conary/issues/315), [#504](https://github.com/FieldmouseWorks/Conary/issues/504), [#505](https://github.com/FieldmouseWorks/Conary/issues/505) | Closed | Per-repo ownership (`#313`), S1 remi (`#314`), S2 plan (`#315`), S2a Conary (`#505`), and S2b public paused repos (`#504`); note `scope-below-a-repo` on `#313` is still `open` in [lessons.toml](lessons.toml) and must resolve before `cn lessons exit M6` passes |
| [#499](https://github.com/FieldmouseWorks/Conary/issues/499), [#500](https://github.com/FieldmouseWorks/Conary/issues/500), [#501](https://github.com/FieldmouseWorks/Conary/issues/501), [#502](https://github.com/FieldmouseWorks/Conary/issues/502), [#503](https://github.com/FieldmouseWorks/Conary/issues/503) | Open | Remaining S2 cutovers (`S2c`–`S2f`); must finish and deploy before `#321b`'s continuous week starts |
| [#316](https://github.com/FieldmouseWorks/Conary/issues/316), [burrow#1310](https://github.com/FieldmouseWorks/burrow/issues/1310), [remi#1463](https://github.com/FieldmouseWorks/remi/issues/1463), [#494](https://github.com/FieldmouseWorks/Conary/issues/494) | `#316` plan closed; `D1`–`D3` open | Upkeep extraction and Python deletion (`D1`–`D3`); must deploy live before `#321b`'s continuous week starts |
| [#199](https://github.com/FieldmouseWorks/Conary/issues/199), [#318](https://github.com/FieldmouseWorks/Conary/issues/318), PR [#642](https://github.com/FieldmouseWorks/Conary/pull/642) | `#318` plan closed (PR `#642` merged); `#199` open; `RT1`–`RT4` to file | Deterministic red-trunk recovery ([m6-red-trunk.md](m6-red-trunk.md)); `#317` reuses `RT1` repair priority and `cn undo` on P1 escalation; `#321` consumes `RT4`'s `verify-drill` report |
| [#423](https://github.com/FieldmouseWorks/Conary/issues/423) (`FP12`), [#293](https://github.com/FieldmouseWorks/Conary/issues/293) | `#423` closed; `#293` open | `FP12` (history-sized proof deadlines) is landed on `main` and must be deployed live before `#321b`; `#293` (judge verdict carry) remains orthogonal and stays in its Conary flow slot before `#319` or alongside non-conflicting work per [m6-order.md](m6-order.md) |

## Delivery 1: `#317` — R0 post-landing review and durable repair

- **Existing owner:** [#317](https://github.com/FieldmouseWorks/Conary/issues/317)
  (M6 slice 6).
- **PR scope:** One Conary PR (no subdivision needed).
- **Implementation waits on:** None beyond `main` (uses existing `conary-plugin`
  `fix` kind and `conary_github::undo`).
- **Deployment waits on:** Per-repository opt-in after cutover
  (`r0_post_landing_review = true`).

### Design and durable state

1. **Live settings (`flow/types.rs`, `flow/schema.rs`):**
   - `r0_post_landing_review: bool` (default `false`, `x-spender = true`):
     enables policy-classified R0 landing before review and durable
     post-landing review tracking.
   - `r0_p1_repair_timeout: u64` (default `3600`, `x-spender = false`):
     seconds an unfixed post-landing P1 finding may spend in follow-up repair
     before escalating to `cn undo`.
2. **Strict pre-landing eligibility (`flow/review.rs`, `flow/decide.rs`, `flow/evidence.rs`):**
   A PR may bypass the pre-landing reviewer wait **only** when every condition
   below holds:
   - `settings.r0_post_landing_review == true`;
   - `f.classified.class == Class::R0` from policy (`Body::Classify` under the
     current policy hash, never set or lowered by the author);
   - `f.classified.docs == false` (docs-only changes already skip review
     entirely without post-landing review per [DESIGN 4.4](DESIGN.md#44-class));
   - `f.classified.raised` contains neither `"r2"` nor `"review"` (sticky
     across revisions: removing a label never lowers a raise);
   - `f.trusted == true`, not draft, not paused/held, no active owner door;
   - no pre-existing unresolved reviewer findings (`f.open_threads == 0` and
     `!f.changes_requested` and no `Verdict::Findings` already posted on the
     current head — known findings are fixed before landing);
   - all required pre-landing evidence is green and independently attested via
     `evidence::authorize` (`evidence.require` on direct head or train
     candidate, configured `settings.proofs`, `negative` when tests are added,
     and configured `settings.deferred`). Missing required evidence or
     `Trigger::NoCi` ALWAYS blocks landing: post-landing review never replaces
     pre-landing attested evidence.
   - **Deferred-check start path on non-train repos (`flow/review.rs`,
     `flow/decide.rs`, `flow/deferred.rs`, `docs/flow.md#deferred-checks`):**
     On a non-train repository (`settings.train == false`) with both
     `settings.deferred` and `settings.reviewers` configured, `deferred::start`
     runs inside `flow::decide::after_review` only after `flow::review::look`
     returns `Look::Settled`. `#317` therefore lets an eligible R0 PR (meeting
     every condition above, with `f.open_threads == 0`, `!f.changes_requested`,
     and no `Verdict::Findings` on the current head) exit `flow::review::look`
     as `Look::Settled { capped: false, changed: false }` (marking post-landing
     review required when any configured reviewer is not yet `Verdict::Clean`).
     `after_review` then handles red cheap CI first (`ci == Ci::Red` still goes
     to `Kind::Red`), and when cheap CI is not red, `ready` holds and
     `deferred::start` dispatches the configured deferred checks immediately
     while reviewer verdict is `None` or `Running`. Landing still blocks in
     `deferred::gate` and `evidence::authorize` until every deferred check has
     its attested pass on the head; `#317` updates
     [flow.md](flow.md#deferred-checks) ("Readiness is not permission") to
     document this R0 readiness rule.
   - `Class::R1`, `Class::R2`, and any `review`-/`r2`-raised change retain
     their full pre-landing review, judge, and person-approval gates unchanged.
3. **Durable post-landing review controller (`flow/post_review.rs`, `flow/pass.rs`, `events/runtime.rs`):**
   - Because `Pass::run` normally decides only on `Pulls::Open` PRs
     (`flow/pass.rs`), post-landing work is persisted atomically under the
     repository lock at `<data>/flow/<owner>/<repo>/post-review.json` (keyed by
     `change_id`, `pr`, `revision_id`, `landed_head`, `landed_commit`,
     `landed_tree`, `landed_at`) **during landing reconciliation** (both direct
     `Step::Merge` and `train::step` landings of eligible R0 changes).
   - Survives PR closure and `conaryd` crash/restart: `Pass::run` processes
     active `post-review.json` entries alongside open PRs, and
     `events::Runtime::restart` kicks every repo with active post-landing
     entries and re-watches any active repair worker's `.running` lock.
   - **State transitions (strictly event-driven via webhooks and `events::Queue`
     booked waits):**
     - `AwaitingReview`: If no reviewer has started after `QUIET` (or the head
       was pushed by an App), ask the configured reviewer through the existing
       `review_ask` / `settings.review_adapter` path referencing the landed PR
       and commit. Wait passively (`waiting-costs-nothing`). Record finished
       reviewer outcomes as unattested `Body::Evidence { kind: CheckKind::Review }`
       on the landed change's revision.
     - `Clean`: All configured reviewers report `Verdict::Clean` on the landed
       head -> terminal `Resolved { outcome: Clean }`.
     - `RepairRequested`: Any reviewer posts findings. Persist intent before
       invoking the configured `plugins.fix` (`fix` kind), asking it to open a
       follow-up repair PR against the default branch (never pushing to a
       merged PR branch or rewriting trunk). Track the worker's `.running` lock
       and resulting repair PR. When the repair PR lands ->
       `Resolved { outcome: Repaired { fix_pr } }`.
     - `EscalatedToUndo`: If any finding is parsed as `P1` by
       `settings.review_adapter` AND (`plugins.fix` is unset, the fix worker
       fails/breaks, or the follow-up repair PR has not landed by
       `findings_at + settings.r0_p1_repair_timeout`), invoke the established
       recovery path: `conary_github::undo::undo(store, forge, source, config,
       change_id, structured_reason)` (prompt-free operational reason citing
       the landed PR and finding reference; idempotent via `strays()` /
       `How::Found`; inherits `RT1` repair priority when trunk is affected).
       If the change was already reverted (e.g., by red-trunk recovery `#318`),
       reconcile cleanly to `Resolved { outcome: AlreadyReverted }`.
     - `Degraded`: Any failed/truncated review read records `Body::Degraded`
       (`degraded-counts-as-failed`), never marks `Clean`, and books bounded
       backoff retry.
   - **Pause/hold (`pause-vacates`):** `cn flow pause` immediately cancels any
     active post-landing `fix` worker while retaining `post-review.json` state
     so unpausing resumes without losing findings.

### Durable observations for `#321`

- Ledger ops: `Body::Classify` (`class == R0`, `docs == false`, `raised`),
  `Body::Land` (`at`), pre-landing attested `Body::Evidence` /
  `Body::CandidateEvidence`, post-landing `Body::Evidence { kind: Review }`,
  and `Body::Undo` (on P1 escalation).
- `cn flow status` exposes `post_review` entries (active, repairing,
  escalated, degraded, recent terminal outcomes) and records that eligible R0
  PRs incurred zero `Stage::Review` pre-landing waits.

### Verification fixtures (`#317`)

All run deterministically in `conary-github::sim` (failing before the fix):

| ID | Scenario | Expected outcome |
| --- | --- | --- |
| **R0-1** | Eligible policy-R0 PR with attested green CI/proofs/negative while reviewer verdict is `None` or `Running` | Lands immediately (direct or via train) with `0` pre-landing reviewer waits; creates durable `post-review.json` entry in `AwaitingReview` |
| **R0-2** | PR closes on merge, `conaryd` restarts before reviewer posts | On restart, `Pass::run` resumes `AwaitingReview` from `post-review.json`, asks reviewer after `QUIET`/App-push, and records `Clean` when clean review arrives |
| **R0-3** | Post-landing reviewer posts P2/P3 findings after PR merge + restart | Transitions to `RepairRequested`, invokes `plugins.fix` to open a follow-up repair PR against trunk, and resolves `Repaired` when it lands |
| **R0-4** | Post-landing reviewer posts P1 finding and fix fails or exceeds `r0_p1_repair_timeout` | Escalates via `cn undo` (idempotent across lost HTTP response/restart), lands revert PR, marks change `Reverted`, never rewrites trunk |
| **R0-5** | Eligible R0 PR missing required attested CI, proof, negative, or deferred check | Refused/blocked before landing (`evidence::authorize` fails); post-landing review never substitutes for pre-landing evidence |
| **R0-6** | Policy-R0 PR with sticky `review` label (even if label is later removed) or `r2` raise | Cannot bypass pre-landing review (`review`) or person approval + train-alone (`r2`) |
| **R0-7** | Policy-R1 and policy-R2 PRs with `r0_post_landing_review = true` | Keep full pre-landing review, judge, and R2 person/train-alone requirements |
| **R0-8** | Eligible R0 PR that already received `Verdict::Findings` before landing | Fixes known findings before landing; never lands over active pre-landing findings |
| **R0-9** | Default setting (`r0_post_landing_review = false`), repo pause mid-repair, and degraded review read | Off by default changes nothing; pause vacates active repair worker and preserves state; degraded read records `Body::Degraded` and never marks `Clean` |
| **R0-10** | Non-train repo with `settings.r0_post_landing_review = true`, `settings.deferred` configured, and `settings.reviewers` configured while reviewer verdict is `None`/`Running` | When cheap CI is not red and no findings exist, `flow::review::look` returns `Look::Settled` and `after_review` calls `deferred::start` immediately without waiting for review; landing still waits in `deferred::gate` / `evidence::authorize` until deferred checks attest green, then lands with `0` pre-landing reviewer waits and persists `post-review.json` (and red cheap CI or failed/unattested deferred checks block landing) |

## Delivery 2: `#319` — Deterministic batch proposals from paths and class

- **Existing owner:** [#319](https://github.com/FieldmouseWorks/Conary/issues/319)
  (M6 slice 8, part 1).
- **PR scope:** One Conary PR (no subdivision needed).
- **Implementation waits on:** None beyond `main`; **must merge before `#320`**.
- **Deployment waits on:** None (read-only command and library).

### Design and durable state

1. **Touched-path provenance (`conary-core/src/op.rs`, `ledger.rs`, `conary-github/src/class.rs`):**
   - Add an optional backward-compatible field to `Body::Classify`:
     `#[serde(default, skip_serializing_if = "Option::is_none")] paths: Option<ClassifiedPaths>`
     holding canonical sorted touched paths (including rename/copy `from`
     paths) and `cut: bool`. Historical ops deserialize without the field and
     keep identical IDs and signatures.
   - `class.rs` populates `paths` whenever it records a new `Body::Classify`
     op. For legacy classified revisions lacking recorded paths, the read-only
     proposal command either marks them `UnclassifiedPaths` (isolated singleton)
     or resolves them through read-only `Source::files` when a GitHub source is
     supplied — never mutating GitHub or the ledger.
2. **Pure deterministic batch-proposal kernel (`crates/conary-core/src/batch.rs`):**
   - Independent of GitHub I/O and burrow (`reusable-modules`), zero model calls
     (`no-model-in-mechanical-step`), and explicitly excludes the optional
     intent-classification experiment.
   - **Canonical input normalization:** Sort open changes for `repo` by
     `(pr, change_id)` (or `change_id`) before grouping so output is 100%
     invariant to input enumeration order.
   - **Effective class and sticky raises:** Resolve each change's current
     revision class from `Change::class_of(current)` combined with sticky
     `Change::raised()`.
   - **Hard isolation rules:**
     - **Unclassified or truncated (`cut == true`):** Isolated singleton
       (`alone: true`, reason `Unclassified` or `TruncatedPaths`).
     - **R2 isolation:** Every `Class::R2` change (from policy paths,
       `cut == true`, or sticky `r2` label raise) is strictly a singleton
       (`alone: true`, reason `R2Policy` or `R2Raised`), never grouped even
       with another R2 change touching identical paths.
     - **Class separation:** Non-R2 changes only group within the same
       effective class (`R0` with `R0`, `R1` with `R1`); `R0` changes carrying
       a sticky `review` raise record `review_raised: true` and are kept
       separate from review-bypassing R0 changes so `#320` never blurs review
       readiness.
   - **Deterministic path/scope connected components:**
     - Extract canonical path set (`{path} ∪ {from}` for renames/copies) and
       configurable generic scope prefixes (e.g., owning directory/crate prefix
       at configured `scope_depth`, defaulting to parent directory / top crate
       prefix with zero repo-specific constants).
     - Same-class non-R2 changes sharing any exact path or scope key connect
       into one deterministic batch (connected components), capped at
       `max_batch_size` (split deterministically in canonical member order when
       larger). Disjoint same-class changes remain separate path-based batches
       with reason `DisjointPaths`.
     - Compute deterministic `BatchId` as the BLAKE3 hash of canonical
       `(repo, class, alone, sorted[(change_id, revision_id)])` so any change
       in membership, head, or class yields a new `BatchId`.
3. **Read-only CLI (`cn batch propose OWNER/REPO [--json]`):**
   - Takes read-only ledger access (and optional read-only `Source`); compiles
     with **zero** `Write`/`Forge` trait dependency so GitHub writes are
     structurally impossible.
   - Emits machine-readable JSON: schema version, `repo`, `policy_digest`, and
     canonically sorted `batches` (`batch_id`, `class`, `alone`, `members` with
     `change_id`/`revision_id`/`pr`/`class`/`raised`/`docs`/`paths`,
     `shared_paths`, `shared_scopes`, and structured `reasons`).

### Verification fixtures (`#319`)

All run against generic simulated repositories in `conary-core` unit tests and
`conary-github::sim` / `cn` CLI tests:

| ID | Scenario | Expected outcome |
| --- | --- | --- |
| **BP-1** | Identical open PR set supplied in all permutations of enumeration order | Bit-for-bit identical `batches`, `batch_id`s, member order, and JSON output |
| **BP-2** | Overlapping exact paths and overlapping directory/crate scopes within `R0` (and within `R1`), including renames (`from` -> `path`) | Grouped into one deterministic batch citing exact `shared_paths`/`shared_scopes` |
| **BP-3** | Disjoint paths within the same class | Separate deterministic batches with reason `DisjointPaths` |
| **BP-4** | Mixed classes (`R0` and `R1`, or `R1` and `R2`) touching the exact same file | Strictly separated by class (`ClassMismatch` / `R2Isolated`) |
| **BP-5** | R2 isolation: policy R2 path, `cut == true` (>300 files), and sticky `r2` label raise (including after label removal) | Every R2 change is a singleton with `alone: true`, even when two R2 PRs touch the same file |
| **BP-6** | Sticky `review` label raise on an R0 change touching same path as an unraised R0 change | Preserves `review` raise and isolates it from unreviewed R0 batch |
| **BP-7** | Unclassified open PR or head change (`revision_id` moved) | Unclassified PR fails closed as singleton `Unclassified`; moving a head changes `batch_id` |
| **BP-8** | CLI `cn batch propose OWNER/REPO --json` against sim spy | Succeeds with structured JSON and records **0** GitHub write calls and **0** model calls |

## Delivery 3: `#320` — One-gate batch execution through the existing train

- **Existing owner:** [#320](https://github.com/FieldmouseWorks/Conary/issues/320)
  (M6 slice 8, part 2).
- **PR scope:** One Conary PR (no subdivision needed).
- **Implementation waits on:** `#319` merged, `#291`'s lookup (`#380`, merged),
  and `#296`'s deferred checks (`#333`, merged).
- **Deployment waits on:** Per-repository opt-in after cutover; live runner
  savings compose with `#324` (`#389`) and `#297`–`#302`.

### Design and reuse integration

1. **Live opt-in setting (`flow/types.rs`, `flow/schema.rs`):**
   - `train_batching: bool` (default `false`, `x-spender = false`): enables
     batch-aware candidate planning inside the existing train. Preserves the
     existing exclusivity rule between `settings.train` and `settings.deferred`
     ([flow.md](flow.md#deferred-checks)) so no repository runs competing
     schedulers.
2. **Batch-aware candidate planning inside the existing train (`conary-core/src/ledger/train.rs`, `conary-github/src/train.rs`, `flow/train.rs`):**
   - `Ledger::next` / train planner retains all existing safety invariants:
     1. Red-candidate halves (`train.splits`) ALWAYS go first, whole and
        intact.
     2. Red-trunk repairs (`RT1`) outrank ordinary batches.
     3. Every `R2` change (`alone == true`) rides in a 1-change candidate bound
        to its exact `base` trunk commit and candidate tree.
     4. Otherwise, when `train_batching` is enabled, seated ready changes are
        grouped through `#319`'s deterministic `batch::propose` so same-class
        related PRs enter one candidate together (up to `train_size`, respecting
        FP4 priority and age tiebreaks), gating the batch **once** on its
        single `conary/train/<id>` branch.
   - Optional backward-compatible `#[serde(default, skip_serializing_if = "Option::is_none")] batch: Option<BatchId>`
     on `Body::Candidate` links each batched candidate permanently to the exact
     `#319` proposal it executed without changing historical op bytes.
3. **Exact-tree evidence binding and reuse of `#291` / `#296`–`#302`:**
   - Candidate evidence (`Body::CandidateEvidence`) is bound strictly to
     `(candidate.commit, candidate.tree, check_name, gate_definition)` with
     verified producer attestation (`Attest::GithubToken` or
     `Attest::Service`).
   - Reuses `#291`'s exact-tree verification rules (`conary_github::reuse` /
     [gate-reuse.md](gate-reuse.md)):
     - When a finished batch candidate ends on `trunk_tree == candidate.tree`
       (`Candidate::same_tree()`), `#291`/`#324`'s lookup answers `hit` for the
       subsequent trunk push so trunk does not rebuild the exact tree the batch
       just proved.
     - Inside candidate evaluation, any exact-tree check reuse must match the
       exact `tree`, `check`, workflow tree, source, inputs, and skipped-check
       coverage under `reuse`'s rules; a different tree, modified gate
       definition, expired artifact, or `unknown`/degraded lookup ALWAYS runs
       the full candidate gate (`degraded-counts-as-failed`).
     - **R2 never reuses foreign evidence:** an R2 candidate lands only on
       attested evidence produced for its own candidate tree onto its exact
       `base` trunk commit; trunk movement drops and rebuilds it.
   - Reuses `#296`–`#302` (`flow/deferred.rs`, PR `#333`) for non-train
     deferred heavy checks without duplicating dispatch or cancellation logic;
     train-enabled repos rely on candidate gating + minimal cheap PR checks so
     PR pushes do not duplicate the full gate.
4. **Invalidation and red-batch splitting:**
   - **Changed membership / head / scope / raises:** Any member pushing a new
     head, closing, leaving, losing review/approval scope, or gaining a raise
     (`raised_since_join` / `below_class` to `R2`) invalidates the candidate
     (`State::Broken`) and computes a new `BatchId`. Evidence from the old
     candidate never carries to the new membership or tree.
   - **Trunk movement:** If trunk moves under a non-R2 candidate and final
     `Finish.tree != candidate.tree`, `Candidate::same_tree()` is `false` so
     trunk runs its own full gate.
   - **Red splitting:** Uses the existing train's bisection split
     (`train.splits`, older half first, then second half) after its single
     allowed full-workflow rerun (`train/retry.rs`). The green half lands; the
     single red PR leaves with `Held::Red` and `Mark::Red`.
5. **Durable gate-execution and reuse accounting (for `#321`):**
   - Provide a shared read-only accounting library in `conary-github` (and thin
     `cn train stats` / acceptance helper) that reconciles ledger ops
     (`Body::Land`, `Body::Candidate`, `Body::CandidateEvidence`,
     `Body::Finish`, `Body::Evidence`) with GitHub Actions workflow runs and
     check runs to report per-repo and fleet totals:
     - `prs_landed`
     - `pr_gate_runs` (full gate runs triggered by `pull_request` or
       `workflow_dispatch` deferred checks)
     - `candidate_gate_runs` (full gate runs on `conary/train/**`)
     - `trunk_gate_runs` (full gate runs on default branch pushes)
     - `total_gate_runs = pr_gate_runs + candidate_gate_runs + trunk_gate_runs`
     - `reused_gate_hits` (verified zero-build exact-tree reuse hits, reported
       separately with original run URL, tree ID, and workflow commit)
     - `degraded_lookups` (never counted as hits)

### Verification fixtures (`#320`)

All run deterministically in `conary-github::sim` (failing before the fix,
with demonstrated negative cases):

| ID | Scenario | Expected outcome |
| --- | --- | --- |
| **BG-1** | Three ready same-class PRs in one `#319` batch seated in train | Admitted together into one candidate, gated **once**, all three land, `Finish.tree == candidate.tree`, trunk push hits exact-tree reuse -> `total_gate_runs (1) < prs_landed (3)` |
| **BG-2** | Different tree negative case: candidate/trunk tree differs by 1 byte from previously attested tree | Exact-tree reuse refused (`miss`), full gate required, never grants permission from mismatched tree |
| **BG-3** | Changed gate definition negative case: workflow tree or `build-skip` changes on identical repo tree | Exact-tree reuse refused (`miss`), full gate required |
| **BG-4** | Changed membership / head mid-gating: member #2 pushes a new head or closes while candidate is gating | Old candidate stops (`State::Broken`), old evidence discarded, remaining members build a new candidate with a new `BatchId` and gate afresh |
| **BG-5** | Trunk moves while batch is gating or landing | Non-R2 `Finish.tree != candidate.tree` forces trunk full gate (`same_tree() == false`); R2 candidate drops immediately and rebuilds on new trunk |
| **BG-6** | Red batch splitting: 4-PR batch with 1 bad PR fails attempt 1 and attempt 2 | Splits via existing `train.splits` (older half first, then second half); 3 clean PRs land, 1 bad PR exits with `Held::Red` and `Mark::Red` |
| **BG-7** | R2 isolation + late `r2` raise mid-batch | R2 PR never joins a multi-PR batch (`alone: true`); a PR raised to `r2` after joining leaves via `raised_since_join` before landing and invalidates the batch |
| **BG-8** | Restart mid-gating and mid-landing | `conaryd` replays op log, resumes the in-flight candidate without duplicate gate runs or duplicate merges |
| **BG-9** | Degraded/unknown reuse lookup | Records degradation (`degraded-counts-as-failed`), refuses reuse, runs full candidate/trunk gate |
| **BG-10** | Gate-run accounting with PR runs included (negative test for omission) | Fixture with 2 PRs landed, 1 candidate run, 0 trunk runs (reused), and 2 PR-level full gate runs reports `total_gate_runs = 3` (`2 PR + 1 candidate + 0 trunk`), failing `< prs_landed` when PR runs push the total over |

## Delivery 4: `#321` — Milestone M6 acceptance report

- **Existing owner:** [#321](https://github.com/FieldmouseWorks/Conary/issues/321)
  (sole owner of M6 acceptance; no duplicate feature owner is created).
- **Bounded two-PR division under `#321`:**
  Building and verifying the acceptance report tool against deterministic
  fixtures should not sit open in an unmerged PR for a seven-day live
  observation window. `#321` therefore divides into two bounded children that
  both reference and deliver `#321`:
  1. **`#321a` (`acceptance: add M6 exit report evaluator, fixtures, and runbook` — 1 Conary PR):**
     - **Waits on:** `#317`, `#320`, and `RT4`'s `verify-drill` JSON schema
       ([m6-red-trunk.md](m6-red-trunk.md#rt4-tests-add-controlled-broken-landing-drill-and-exit-acceptance-check)).
     - **Delivers:** Pure evaluator in `conary-core` / `conary-github`
       (`crates/conary-github/src/acceptance.rs`), thin CLI
       (`cn flow m6-report`), deterministic fixture test suite (`AC-1`–`AC-10`),
       and operator runbook (`docs/m6-acceptance.md`).
     - **Boundary:** Performs no live observation and declares nothing complete.
  2. **`#321b` (`m6: collect live exit week, verify drill, and publish M6 acceptance report` — 1 Conary PR, closes `#321`):**
     - **Waits on ALL live prerequisites deployed** ([fence 12](m6-order.md#fences)):
       1. Completed cutovers across every managed repository: S1 (`#314`) +
          S2a–S2f (`#315`, `#499`–`#505`), with zero active handovers;
       2. Python deletion deployed live: `#316` (`D1` `burrow#1310`, `D2`
          `remi#1463`, `D3` `#494`);
       3. R0 post-landing review live: `#317`;
       4. Deterministic red-trunk recovery live + controlled live drill
          executed on the dedicated R0 drill repository: `#199` + `#318`
          (`RT1`–`RT4`);
       5. Batching + gate reuse + deferred checks live as configured: `#319` +
          `#320` (reusing `#291`/`#324`/`#389` and `#296`–`#302`);
       6. `FP12` (`#423`, closed) deployed live, and every `open` M6 lesson
          (including `scope-below-a-repo` on `#313`) resolved on `main`.
     - **Event-based seven-day observation window (no resident waiting process):**
       Record `T_start` (UTC timestamp of the last prerequisite deploy), exit
       immediately with zero sleeping/polling processes, and resume via a
       one-shot scheduled/event wakeup once `now >= T_start + 7 * 86400s` to
       collect the read-only bundle and run `cn flow m6-report`.
     - **Delivers:** Reproducible read-only queries, exact `[T_start, T_end]`
       UTC window, per-repository and fleet totals (no secrets or personal
       data), `RT4` `verify-drill` JSON output, absent-steward proof,
       `cn lessons exit M6` output on `main` (listing every remaining
       `designed` M6 entry and its building task), and updates to `docs/m6.md`
       and `docs/DESIGN.md` declaring M6 complete **only** when every criterion
       is `Pass`.

### Report contract and strict fail-closed semantics

Every criterion evaluates to `Pass`, `Fail`, or `Incomplete`. The overall M6
verdict is:
- `Pass` (exit `0`) **if and only if** every criterion is `Pass`;
- `Fail` (non-zero exit) if any criterion is `Fail`;
- `Incomplete` (non-zero exit) if no criterion failed but any criterion is
  `Incomplete`.

Missing data, truncated GitHub pagination, degraded ledger reads, zero-sample
R0 weeks, windows shorter than 168 continuous hours, or omitted managed
repositories **always** evaluate to `Incomplete` — never `Pass`, and estimates
may never substitute for measured observations.

| Criterion in `cn flow m6-report` | Pass condition | Fail condition | Incomplete condition |
| --- | --- | --- | --- |
| `window_continuity` | Single unbroken UTC window `T_end - T_start >= 7 * 86400s`, `T_start >= latest_prereq_deploy_at`, zero read gaps | `T_start < latest_prereq_deploy_at` (window overlaps pre-cutover or pre-deletion period) | `T_end - T_start < 7 * 86400s`, missing deploy timestamp, or degraded/truncated read |
| `managed_repo_coverage` | Every repository returned by live discovery (`U2` / `settings.landing`) is present in the report with `landing_owner == conaryd` (quiet repos explicitly recorded as `0 landings observed`) | Any managed repository still owned by `steward` or mid-handover | Discovery inventory missing or any managed repository absent from observation bundle |
| `conaryd_landings_attested` | `prs_landed > 0`; 100% of default-branch landings are `conaryd` `Body::Land` ops with verified producer-attested evidence for each landed tree; `0` unresolved `Body::Outside` ops | Any steward-authorized landing, unattested landed tree, or unresolved `Outside` op | `prs_landed == 0`, unreadable ledger segment, or unreadable attestation/check run |
| `r0_no_reviewer_wait` | `r0_eligible_landed > 0`; every eligible policy-R0 PR (`class == R0`, `!docs`, no `review`/`r2` raise) had `0` pre-landing reviewer waits, attested pre-landing evidence, and recorded post-landing review tracking | Any eligible R0 PR waited in `Stage::Review` before landing or landed without attested evidence | `r0_eligible_landed == 0` or missing PR/review timestamps |
| `gate_runs_below_landed` | `prs_landed > 0` and `(pr_gate_runs + candidate_gate_runs + trunk_gate_runs) < prs_landed`, with verified reuse hits listed separately | `total_gate_runs >= prs_landed` | Missing/truncated Actions run list for any managed repository, or `prs_landed == 0` |
| `steward_absent` | Zero matches for `steward.py`, `modules/autoland/*.py`, `autoland-steward`, `autoland-shadow@`, and legacy judgment readers across `burrow`, `remi`, `Conary`; zero active steward systemd units, watchdog sources, or `steward` owners | Any steward file, unit, watchdog source, legacy reader, or `steward` setting present | Missing repository scan or live unit/settings probe |
| `deterministic_recovery_drill` | Valid `RT4` `verify-drill` report from controlled live drill: culprit reverted, benign neighbors landed, `model_plugin_invocations == 0`, `owner_requests == 0`, bounded probes with verified `Body::TrunkEvidence` | Any drill assertion failed, nonzero model calls, or nonzero owner requests | Drill report missing or ledger references unverifiable |
| `lessons_exit_m6` | `cn lessons check` and `cn lessons exit M6` exit `0` on `main` (`open == []`); every remaining `designed` M6 entry listed with its building task | Any `open` lesson applies to an M6 component (e.g., `scope-below-a-repo` while `open`) | `docs/lessons.toml` unreadable or invalid |

### Verification fixtures (`#321a`)

| ID | Fixture case | Expected verdict |
| --- | --- | --- |
| **AC-1** | Complete 7-day multi-repo bundle (including one quiet repo with `0` landings), eligible R0 PRs with `0` reviewer wait, `total_gate_runs < prs_landed` (PR + candidate + trunk counted), absent steward, valid `RT4` drill, `lessons exit M6` clear with `designed` entries listed | `Pass` |
| **AC-2** | Window of `6d 23h 59m` (or `T_start` earlier than latest prerequisite deploy) | `Incomplete` (`window_continuity`) / `Fail` (overlap) |
| **AC-3** | One managed repository omitted from the observation bundle | `Incomplete` (`managed_repo_coverage`) |
| **AC-4** | Truncated GitHub Actions pagination or degraded ledger read on one repository | `Incomplete` (never `Pass`) |
| **AC-5** | Unattested landed tree, steward-authorized landing, or unresolved `Outside` op | `Fail` (`conaryd_landings_attested`) |
| **AC-6** | `steward.py` file, `autoland-steward` systemd unit, or `settings.landing = steward` present | `Fail` (`steward_absent`) |
| **AC-7** | (a) Eligible R0 PR waited on reviewer before landing; (b) zero eligible R0 PRs landed in window | (a) `Fail`; (b) `Incomplete` (`r0_no_reviewer_wait`) |
| **AC-8** | `candidate_gate_runs + trunk_gate_runs < prs_landed`, but including `pr_gate_runs` makes `total_gate_runs >= prs_landed` | `Fail` (`gate_runs_below_landed`) |
| **AC-9** | (a) `RT4` drill missing; (b) `RT4` drill shows `model_plugin_invocations > 0` or `owner_requests > 0` | (a) `Incomplete`; (b) `Fail` (`deterministic_recovery_drill`) |
| **AC-10** | `docs/lessons.toml` has an `open` entry on an M6 component (`scope-below-a-repo`) | `Fail` (`lessons_exit_m6`) |

## Lessons held by these deliveries

Per [m6.md](m6.md#lessons-each-task-is-held-to) and
[DESIGN acceptance](DESIGN.md#acceptance-lesson-by-lesson):

- **`#317` (slice 6):** `same-inputs-same-verdict` (`designed` — R0 still
  requires attested exact-tree pre-landing evidence), `degraded-counts-as-failed`
  (`built` — extended to post-landing review reads), `waiting-costs-nothing`
  (`built` — extended to passive post-landing review waits),
  `no-model-in-mechanical-step` (`designed` — R0 eligibility, tracking, and P1
  undo escalation use zero model calls; `plugins.fix` runs only on posted
  findings).
- **`#319` (slice 8):** `no-model-in-mechanical-step` (`designed` — pure
  path/class grouping with zero model calls), `same-inputs-same-verdict`
  (`designed` — identical inputs always propose identical batches),
  `reusable-modules` (`designed` — pure `conary-core` library with generic
  config, no burrow).
- **`#320` (slice 8):** `heavy-work-waits-for-settled-inputs` (`designed` —
  reuses `#296`–`#302` deferral and train-candidate gating),
  `same-inputs-same-verdict` (`designed` — reuses `#291` exact-tree lookup,
  invalidates changed membership/tree/gate), `spend-only-where-needed`
  (`designed` — gates each batch once through existing train),
  `degraded-counts-as-failed` (`built` — `unknown`/degraded reuse lookup runs
  full gate), `reusable-modules` (`designed` — standalone without burrow).
- **`#321` (M6 exit):** Invokes `cn lessons exit M6` on `main` across every
  M6 component and lists every remaining `designed` M6 entry with its building
  task. As of 2026-11-27, `scope-below-a-repo` (owned by `#313`) is the sole
  `open` M6 entry in [lessons.toml](lessons.toml) and will block `#321b` until
  its owner moves it to `designed` or `built`.

Because this PR is a plan and adds no runtime implementation tests, no lesson
entry moves to `built` here; every slice 6 and slice 8 lesson is already
`designed` or `built` and links back to this plan in
[lessons.toml](lessons.toml).

## Single-PR delivery summary

| Delivery | Existing owner | PRs | Implementation fence | Live / acceptance fence |
| --- | --- | --- | --- | --- |
| R0 post-landing review | [#317](https://github.com/FieldmouseWorks/Conary/issues/317) | 1 | `main` | Per-repo opt-in after cutover; R0 waits reported by `#321` |
| Deterministic batch proposals | [#319](https://github.com/FieldmouseWorks/Conary/issues/319) | 1 | `main` (**strictly before `#320`**) | Read-only; no deploy switch |
| One-gate batch execution | [#320](https://github.com/FieldmouseWorks/Conary/issues/320) | 1 | `#319`, `#291` (`#380`), `#296` (`#333`) | Per-repo opt-in after cutover; composes with `#324`/`#389` and `#297`–`#302`; gate count reported by `#321` |
| Acceptance report tool & runbook | [#321](https://github.com/FieldmouseWorks/Conary/issues/321) (`#321a`) | 1 | `#317`, `#320`, `RT4` CLI schema | None (offline fixtures only; never declares M6 complete) |
| Live exit week & final report | [#321](https://github.com/FieldmouseWorks/Conary/issues/321) (`#321b`) | 1 | `#321a` | All S1/S2 cutovers, `D1`–`D3` Python deletion, `#317`, `#318` (`RT1`–`RT4` + live drill), `#319`+`#320`, `FP12` (`#423`), continuous 7-day week, `cn lessons exit M6` clear |

These deliveries preserve [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s
original authorization boundaries and grant no additional authority.
