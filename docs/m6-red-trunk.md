# M6 slice 7: deterministic red-trunk recovery

This is [#318](https://github.com/FieldmouseWorks/Conary/issues/318)'s design
and test plan (child of [#256](https://github.com/FieldmouseWorks/Conary/issues/256)).
It specifies a bounded, deterministic recovery state machine that detects
persistent failures on a repository's default branch, bisects them over the
ledger's real landing order using deterministic proof plugins, and reverts the
culprit through `cn undo` without a model job or ordinary person intervention
([DESIGN 4.3](DESIGN.md#43-evidence), [DESIGN 12](DESIGN.md#12-roadmap)).

This plan implements nothing, deploys nothing, introduces no live failures, and
grants no authority beyond [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s
boundaries ([m6.md](m6.md#boundaries)). Written 2026-11-26 against Conary's
main at `1dddf09`.

## Ground rules

These apply to every task below and are not repeated in each task.

- **No model diagnoses or bisects the trunk (`no-model-in-mechanical-step`).**
  Failure confirmation, landing-order window construction, bisection probing,
  culprit attribution, and opening the revert PR are purely deterministic Rust
  logic plus deterministic proof plugins (`conary-plugin v4`/`v5` `prepare` and
  `proof`, which refuse any declared model in `describe`,
  [plugins.md](plugins.md#versions)). No `review`, `fix`, `judge`, or `job`
  plugin may be called to diagnose or bisect a red trunk.
- **Conary stands alone (`reusable-modules`).** Recovery lives in `conary-core`
  and `conary-github`; `conaryd` and `cn` stay thin layers over them. It runs
  with `cargo install`, a GitHub token, a ledger, and a flow configuration
  file, with no burrow and no Nix ([DESIGN finish line item 7](DESIGN.md#12-roadmap)).
- **Event-driven, never a timer.** Recovery advances on verified webhooks
  (`push`, `check_suite`, `check_run`, `workflow_run`, `status`), proof worker
  exit kicks, and persisted deadlines (`events/queue.rs`), never on a polling
  cron or timer. Waiting on CI starts no work and books no periodic wake
  (`waiting-costs-nothing`).
- **Append-only ledger; undo is a new change.** Trunk never rewrites or
  force-pushes. Every recovery rollback is a revert PR opened through
  `conary_github::undo` (`cn undo`) and recorded as an immutable `Undo` op with
  a permanent, structured, prompt-free operational reason
  ([DESIGN section 5](DESIGN.md#5-undo)).
- **Evidence must be independently attested on the exact tree
  (`same-inputs-same-verdict`).** Plugin stdout is a proposal, never evidence.
  A bisection verdict counts only when backed by a verified CI attestation
  ([ci-attestation.md](ci-attestation.md)) or service attestation
  ([service-attestation.md](service-attestation.md)) bound to the exact commit,
  exact git tree, check name, and configured recipe `definition`. Because a
  GitHub check run's `output.text` is mutable after recording, a stored
  `Attest::Service` verdict is never counted from the ledger or local cache
  alone: before reusing it, recovery must re-read and re-verify the reporting
  check run's current GitHub publication against current producer keys, check
  scope, and recipe `definition`, and halt fail closed if it was edited,
  removed, or no longer matches
  ([service-attestation.md](service-attestation.md#signed-statement-v1)). Only
  a kept CI attestation (`Attest::Ci`) may stand in for a recheck of the exact
  check run its token names while that run carries no service statement.
- **Degraded or inconclusive fails closed (`degraded-counts-as-failed`,
  `limits-are-design-bugs`).** Any unattested, malformed, crashed, timed-out,
  cancelled, `error`, or unreadable probe or GitHub read is recorded, never
  treated as a pass or clean fail, never retried on the same tree, and halts
  automatic recovery cleanly.
- **Spenders off by default; one live settings source (`spenders-off-by-default`,
  `one-settings-source`, `live-settings`, `off-switch-independent`,
  `pause-vacates`).** Every option recovery adds is a live field of
  `flow::Settings` declared in `flow/schema.rs` (`red_trunk_recovery` defaults
  to `false` with `x-spender = true`). Pausing a repository immediately cancels
  any active bisection proof worker, vacates its slot, and preserves completed
  attested state.
- **Owner only for one-way doors (`owner-only-for-doors`,
  `one-question-per-decision`).** R0 and R1 recoveries never list or wait on
  the owner. Only an R2 undo (`Door::R2`) or an `outside` trunk commit
  violation asks a person, once per decision. Operational halts surface in
  `cn flow status` and the stall watchdog, never as owner doors.
- **Repairs go first and never wait on red runs (`repair-ahead-of-red`).**
  Whatever repairs a red trunk (an automated recovery `conary/undo/...` PR or a
  trusted `autoland/red-...` fix PR) never waits on the red trunk's own pending
  or hung CI runs, never waits on its own base being brought up to date behind
  other work, and outranks every ordinary PR (including `priority: high`) for
  proof slots, pass order, and train admission.

## Reuse of #199

[#199](https://github.com/FieldmouseWorks/Conary/issues/199) fixes two bugs in
`crates/conary-github/src/undo.rs` (and its ledger helpers) that slice 7
depends on directly. Slice 7 reuses #199 and does not re-own or duplicate it:

1. **Real trunk landing order.** Today `Landing.at` is the op-ingestion
   timestamp (`Store::record`), so delayed sync, backfilled history, or
   same-millisecond timestamps scramble `group()`'s ordering. #199 establishes
   trunk's true commit order (`before -> after` commit chain / merge order)
   across delayed sync, backfill, existing ledger entries, and tied timestamps.
   Red-trunk bisection uses that exact trunk commit order to build its
   `[known_good, ..., red_commit]` window and never sorts landings by
   `Landing.at`.
2. **Net-effect undo chains.** Today `group()` filters only on
   `reverted_by.is_none()`, so repeating a time-based undo can pick the undo
   change itself and restore what it reverted. #199 computes the net effect of
   each original change together with its undo chain. Red-trunk attribution
   reuses that net-effect model so recovery:
   - targets the active landed change that actually caused the transition (for
     `A -> U1(A) -> U2(U1)` breaking at `U2`, it calls `undo(U2)`, never
     `undo(A)`, avoiding `UndoError::UndoneUndo`);
   - never re-undoes a change whose revert already landed;
   - never undoes a finished rollback `U1` when trunk is still red for another
     reason.

What remains slice 7's own responsibility (not #199's): bisection over
non-linear/grouped candidates, `Body::TrunkEvidence` and service-attested
commit `fail` verification, R2 class inheritance on revert PRs, the recovery
state machine, `repair-ahead-of-red`, and the controlled drill.

## Architecture and bounded state machine

### Where state lives

- **Append-only op log (`conary-core`):**
  - Existing ops: `Land`, `Outside`, `Candidate`, `Finish`, `Classify`, `Undo`,
    `Degraded`, `Restored`.
  - New immutable op (added in **RT3**): `Body::TrunkEvidence { repo, commit,
    tree, kind, check, result, source, attest }`. `Ledger::check` accepts it
    only for a `commit` that is the `after` of a recorded `Land` or `Outside`
    op in `repo`, with a verified producer attestation (`attest: Some(...)`
    checked before recording) and non-empty `check`/`source`. Like `Evidence`
    and `CandidateEvidence`, it is never edited or deleted. It preserves
    permanent audit provenance for trunk CI and bisection proof verdicts, and
    lets restarts and later incidents avoid re-running a proof plugin on the
    exact same tree (`same-inputs-same-verdict`,
    [DESIGN 4.3 rule 3](DESIGN.md#43-evidence)) — subject to live
    revalidation for `Attest::Service`: before counting any stored service
    verdict, recovery re-reads the check run named by `source` on GitHub and
    re-verifies its current published statement against current producer keys,
    check scope, and recipe `definition`, halting fail closed
    (`InconclusiveProof`) if the publication was edited, removed, or no longer
    matches ([service-attestation.md](service-attestation.md#signed-statement-v1)).
- **Mutable per-repository controller state (`conary-github`):**
  - Stored at `<data>/flow/<owner>/<repo>/red-trunk.json` alongside PR records
    ([flow.md](flow.md#state-and-workers)), protected by the repository file
    lock and written atomically (temp file, `fsync`, rename, directory sync)
    **before** spawning any proof worker or making any GitHub write.
  - Holds at most one active incident per `(repo, trunk_branch)`, its active
    proof worker invocation ID and absolute history-sized deadline, its
    incident-local tree verdict map, its consecutive-undo counter, and a
    bounded ring of recent terminal incidents (default last 16) for
    `cn flow status`.

### States and transitions

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Confirming : required check fails on trunk_tip
    Confirming --> Resolved : trunk_tip turns green
    Confirming --> Confirming : trunk_tip moves while not green (re-anchor to new tip)
    Confirming --> Halted : infra error, cancelled/stale, or read degraded
    Confirming --> Bisecting : persistent failure confirmed
    Bisecting --> Resolved : trunk_tip turns green (cancel active worker)
    Bisecting --> Confirming : trunk_tip moves while not green (cancel worker, re-confirm new tip)
    Bisecting --> Halted : inconclusive proof, known_good fails now, non-reproducible, broken ancestry, or no known_good
    Bisecting --> Attributed : last pass->fail transition verified (commits[g]=pass, commits[g+1..=k]=fail)
    Attributed --> Resolved : trunk_tip turns green
    Attributed --> Confirming : trunk_tip moves while not green (never execute stale attribution)
    Attributed --> Halted : culprit is Outside commit or already reverted
    Attributed --> Executing : trunk_tip == red_commit, culprit is active Land(C), intent persisted
    Executing --> Tracking : cn undo(C) returns Opened or Found
    Executing --> Confirming : trunk_tip moves while not green before PR open
    Executing --> Resolved : trunk_tip turns green before PR open (or UndoError::Nothing and trunk green)
    Executing --> Halted : unrecoverable undo refusal or budget exhausted
    Tracking --> Resolved : undo PR lands and trunk turns green
    Tracking --> Confirming : undo PR lands but trunk stays red (if budget remains)
    Tracking --> Halted : undo PR closed, undo checks fail persistently, or budget exhausted
    Tracking --> Resolved : another commit fixes trunk (withdraw/close undo)
    Halted --> Idle : trunk moves to new commit or explicit operator clear
    Resolved --> Idle : recorded in recent history
```

#### 1. `Idle`

No active red-trunk incident for `(repo, trunk_branch)`. On each repo pass (or
default-branch `push`, `check_suite`, `check_run`, `workflow_run`, `status`
kick), the pass reads `trunk_tip` and its required checks (`evidence.require`
plus configured deterministic proofs for the default branch). If all required
checks are green or pending with no failures, state stays `Idle`.

#### 2. `Confirming`

Entered when at least one required check concludes failed on `trunk_tip`.
Records `incident_id`, `trunk_branch`, `red_commit`, `red_tree`,
`failing_checks` (each check name, run ID, attempt), and `opened_at`.

- **Single allowed rerun (`recover-by-kind`):** Reuses `train/retry.rs`'s
  discipline. If a failing GitHub Actions workflow run is on `run_attempt == 1`
  and its first attempt has finished, recovery requests **one** full workflow
  rerun (never failed-jobs-only, which breaks attestations) and waits passively
  for the webhook (`waiting-costs-nothing`). GitHub's `run_attempt` is the sole
  record of the retry, so lost HTTP responses or restarts never request a
  second rerun.
- **Persistent failure confirmed:** When at least one required check fails
  definitively on `red_commit`:
  - For a CI check: `run_attempt >= 2` (or a check with no rerun allowed)
    concludes `failure` or `timed_out`, and carries **no** attested `error`
    (infrastructure failure, [ci-attestation.md](ci-attestation.md)).
  - For a deterministic proof check: a verified service attestation on
    `(red_commit, red_tree, check, recipe_digest)` reports `result == "fail"`
    with a failed check run.
- **Fail closed in `Confirming`:**
  - Any check ending `cancelled` (not timed out), `stale`, `neutral`,
    `skipped`, or carrying an attested `error` is **not** a confirmed code
    failure -> `Halted { reason: InconclusiveConfirm }`.
  - Any GitHub read failure or truncated page records degradation
    (`degraded-counts-as-failed`) and waits/halts rather than blaming a commit.
  - If `trunk_tip` turns green before confirmation -> `Resolved { by: TrunkGreen }`.
  - If `trunk_tip` moves to a newer commit while not green -> re-anchor
    `Confirming` to `red_commit = trunk_tip` and wait passively for the new
    tip's checks to either turn green (`Resolved { by: TrunkGreen }`) or
    confirm a persistent failure.

#### 3. `Bisecting`

Builds a bounded, verified trunk commit chain `[commits[0], ..., commits[k]]`
where `commits[k] == red_commit`, walking first-parent ancestry backwards at
most `settings.red_trunk_max_window` commits (default `16`, bounded) and
matching each step `(before -> after)` against the ledger's `Land` and
`Outside` ops (after running an `ingest` sync of `repo` first so freshly merged
landings are present).

- **Window validation (fail closed before any probe):**
  1. `red_commit` missing from the ledger after sync ->
     `Halted { reason: UnrecordedTrunkCommit }`.
  2. Any gap, non-matching parent, or rewritten history between `commits[i-1]`
     and `commits[i]` -> `Halted { reason: BrokenAncestry }`.
  3. No deterministic proof recipe configured for any confirmed failing check
     in `target_checks` (or `proof_slots == Some(0)` or `plugins.proof` unset)
     -> `Halted { reason: NoDeterministicProof }`.
  4. Select `known_good` (`commits[0]`): the newest commit in the window that
     already holds verified attested passing evidence for `target_checks` on
     its exact tree (a finished candidate's `same_tree()` evidence, a verified
     `TrunkEvidence` pass, or verified trunk CI/service attestation — with any
     service attestation revalidated against its reporting check run's current
     GitHub publication). If none exists within `max_window` ->
     `Halted { reason: NoKnownGoodWithinWindow }`.
- **Mandatory boundary verification (eliminates flakes and external outages):**
  Before probing the interior, the deterministic proof plugin must verify:
  - `commits[k]` (`red_commit`) deterministically **fails** now under
    `target_checks` (verified service attestation `result == "fail"` on
    `(red_commit, red_tree, check, definition)`). If it **passes**, the failure
    is non-reproducible -> `Halted { reason: NonReproducible }`.
  - `commits[0]` (`known_good`) deterministically **passes** now under
    `target_checks` (verified service attestation `result == "pass"` on
    `(known_good, good_tree, check, definition)`). An older historical pass
    from yesterday is not enough to rule out an external outage today (expired
    certificate, broken upstream registry, runner environment regression). If
    `known_good` **fails** now -> `Halted { reason: KnownGoodFailsNow }`.
  - If `k == 1`, boundary verification already proves `commits[0] == Pass` and
    `commits[1] == Fail` with zero interior commits -> transition directly to
    `Attributed { good: commits[0], bad: commits[1] }`.
- **Non-monotone-safe reverse-suffix search and strict probe bound:**
  - Midpoint binary search is sound only if verdicts are monotone (`verdict(i)
    == Fail` implies `verdict(j) == Fail` for all `j > i`). That assumption
    **does not hold** across unprobed intermediate commits: inside a grouped
    train candidate or after superseded trunk CI, `T0=pass, T1=fail, T2=pass,
    T3=fail` can occur, where midpoint search would probe `T1=fail` and
    falsely undo `T1` even though `T2` passed and `T3` introduced the current
    failure.
  - Because unprobed interior commits cannot be assumed monotone, recovery
    walks **backwards** from `i = k - 1` down to `1`, stopping at the first
    index `g` where `verdict(commits[g]) == Pass` (or `g = 0`, already verified
    `Pass`, if every `i` in `1..=k - 1` fails).
  - **Exact-tree reuse with live service revalidation
    (`same-inputs-same-verdict`):** If `tree(commits[i])` already has a
    recorded attested verdict for the same check and current `definition`,
    recovery avoids re-running the proof plugin **only after** verifying it is
    still valid: for `Attest::Service`, re-read the reporting check run named
    by `source` on GitHub and re-verify its currently published `output.text`
    against current producer keys, check scope, commit/tree, check name, and
    recipe `definition`. If that publication was edited, removed, or no longer
    matches the stored verdict, fail closed immediately to
    `Halted { reason: InconclusiveProof }` (never trust the cached verdict and
    never silently re-run over a tampered publication). Only a kept
    `Attest::Ci` verdict may stand in without re-reading check-run output, for
    the exact check run its token names while that run carries no service
    statement.
  - Otherwise, run **one** `prepare` (outside proof slots, per C3) + `proof`
    invocation on `(commits[i], tree(commits[i]), target_checks)` through
    the existing `flow/jobs.rs` worker launcher (`.running` lock + durable
    invocation record + absolute history-sized deadline capped by
    `proof_timeout`).
  - Only **one** bisection proof runs per repository at a time. Its maximum
    total probe count for a window of `k` steps (`k + 1` commits) is strictly
    bounded by `k + 1 <= red_trunk_max_window + 1` (at most `17` probes at the
    default `red_trunk_max_window = 16`; `2` probes for a single landing, `3`
    probes when `red_commit` itself broke trunk, and `<= 4` probes for a
    3-commit grouped candidate).
  - When the walk stops at `g`, **every** commit in `[g + 1, k]` is proven
    `Fail` and `commits[g]` is proven `Pass`, guaranteeing `commits[g + 1]` is
    the exact most-recent `pass -> fail` transition even on non-monotone
    histories -> transition to `Attributed { good: commits[g], bad:
    commits[g + 1] }`.
- **Re-anchoring on concurrent trunk movement:** Every pass re-reads
  `trunk_tip`. If `trunk_tip` turns green, cancel any active bisection proof
  worker immediately (`pause-vacates`) and transition to `Resolved { by:
  TrunkGreen }`. If `trunk_tip != red_commit` while not green, cancel any
  active worker for the superseded tip and transition back to `Confirming`
  anchored on the new `trunk_tip`. Recovery never finishes an old
  `[known_good, red_commit]` bisection to apply its attribution to a newer
  unprobed `trunk_tip`. Once the new `trunk_tip` confirms persistently red,
  `Bisecting` rebuilds and validates `[known_good, ..., trunk_tip]` within
  `red_trunk_max_window`, verifies the new `trunk_tip` deterministically fails
  now (halting `NonReproducible` if it passes), verifies `known_good` still
  passes now (reusing the current incident's live-revalidated boundary pass if
  `known_good` is unchanged, or probing if it changed), and runs the
  reverse-suffix walk from `trunk_tip - 1` reusing live-revalidated exact-tree
  attested verdicts for previously probed commits.

#### 4. `Attributed`

First re-read `trunk_tip`: if `trunk_tip` is green -> `Resolved { by:
TrunkGreen }`; if `trunk_tip != red_commit` -> never execute a superseded
attribution against a newer unprobed tip, and transition back to `Confirming`
anchored on the new `trunk_tip`. Otherwise, examine the ledger op that produced
`bad = commits[g + 1]`:

- **`Body::Outside`:** There is no `ChangeId` to undo, and an `outside` move is
  already a violation for the owner ([DESIGN 4.5](DESIGN.md#45-landing)). Halt
  fail closed as `Halted { reason: OutsideCommit { commit: bad } }` and surface
  the violation; never blame an adjacent `Land` op.
- **`Body::Land { change: C, .. }`:**
  - Check net-effect undo chain (#199):
    - If `C` is already `Status::Reverted` on current trunk -> `Halted {
      reason: CulpritAlreadyReverted { change: C } }` (never re-undo `C` or
      undo its revert).
    - If `C` is itself an undo (`U2` that undid `U1` of `A`), `C` is the exact
      active change that caused `good -> bad`; target `C = U2` directly.
  - Determine `class`: the highest class ever recorded for `C` across all its
    `Classify` ops (policy paths plus any `raised` labels — never lowerable).
    If `C` has no `Classify` op in the ledger, fail closed as `Halted { reason:
    UnclassifiedCulprit { change: C } }` rather than assuming R0/R1.
  - Check consecutive-undo cap: if `consecutive_undos >=
    settings.red_trunk_max_consecutive_undos` (default `3`), fail closed as
    `Halted { reason: RecoveryBudgetExhausted }`.
  - Persist `Executing { culprit: C, class, good, bad, checks, reason }` to
    `red-trunk.json` **before** calling GitHub.

#### 5. `Executing`

Re-checks `trunk_tip` and runs `strays()` + `refresh()` + `found()` before
opening any new PR:

- **Pre-open trunk-movement guard:** If an earlier attempt already opened an
  undo PR for `C` on GitHub (e.g. before a lost HTTP response or crash),
  `strays()` adopts it via `How::Found` into `Tracking` (where `Tracking`
  closes it immediately if `trunk_tip` is green, or requires full attested
  evidence on its landing tree against current `trunk_tip`). If no undo PR
  exists yet and `trunk_tip` is green -> `Resolved { by: TrunkGreen }` without
  opening a PR. If no undo PR exists yet and `trunk_tip != red_commit` ->
  abort `Executing` without opening a PR and return to `Confirming` anchored on
  the new `trunk_tip` so attribution is revalidated through `trunk_tip`.
- Otherwise, calls `conary_github::undo::undo(store, forge, source, config, C, reason)`.

- **Structured operational reason:** Permanent, prompt-free, under 1 KiB,
  containing no secrets, personal data, or model text:
  - Failing check name(s) and red trunk run URL (and first deterministic error
    line from the verified log if available, sanitized).
  - Red trunk commit `red_commit`, bisected transition `good -> bad`, and
    attested proof check run URLs / log SHA-256 digests.
- **Idempotency across lost responses and restarts:**
  - `undo()` always runs `strays()` + `refresh()` + `found()` first. If a prior
    attempt opened a PR on `conary/undo/<C>/<attempt>` and lost the HTTP
    response or crashed before appending `Body::Undo`, the retry finds that PR
    via its branch and `<!-- conary-undo: ... -->` marker, records `Body::Undo`
    with its original marker reason, and returns `How::Found` without opening a
    second PR.
  - If an undo PR for `C` was already open (even opened manually by an
    operator), `undo()` returns `How::Found` and recovery adopts it into
    `Tracking`.
- **R2 class inheritance:** Immediately after `Undone { change: U, pr, .. }` is
  recorded, if `C`'s effective class is `R2`, ensure `U` is classified as `R2`
  in the ledger (preserving `R2` even when `C` became `R2` via an `r2` label
  rather than `oneWay` paths).
- **Refusals and transient errors:**
  - `UndoError::Nothing(C)`: `trunk_tip` already removed `C`'s diff without a
    ledger `Undo` op -> re-read `trunk_tip`; if green -> `Resolved { by:
    TrunkGreen }`, else transition to `Halted { reason: CulpritAlreadyAbsent {
    change: C } }` (terminal for `red_commit` so recovery does not loop
    re-blaming `C` on the same tip, clearing automatically if `trunk_tip` moves
    to a newer commit).
  - `UndoError::Api` / `Store` / `Ingest`: stay in `Executing`, book a recovery
    retry via `events::Queue::fail` with bounded backoff.
  - Any other `UndoError::refused()` (`Taken`, `Reverted`, `UndoneUndo`,
    `NotLanded`): fail closed to `Halted { reason: UndoRefused(...) }`.

#### 6. `Tracking`

Monitors the open undo PR `U` (`#undo_pr`) through the landing flow:

- **Priority and non-blocking (`repair-ahead-of-red`):** `U` enters the flow in
  the dedicated repair tier above `Level::High` and is never held by
  `base_held` waiting on `red_commit`'s own pending or hung CI runs.
- **Clean R0/R1 automated path (no model job, no person):** When `class` is
  `R0` or `R1`, `conflicts == false`, and `U`'s current head commit equals the
  exact mechanical revert commit recorded by `Executing` (no subsequent push by
  any other actor):
  - `U` does **not** wait for a code review or judge model job (no new human or
    model code was authored; the tree is the mechanical inverse of `C`).
  - `U` still requires full attested CI / deterministic proof evidence on its
    landing tree (candidate tree in a train repo, or head tree in a direct
    repo) — recovery never bypasses attested evidence!
  - If anyone pushes a new commit to `U`'s branch, the mechanical-head match
    breaks and `U` falls back to the ordinary review/judge gates for that class.
- **Conflicting revert (`conflicts == true`):** `cn undo C` opens the bare
  revert PR per [DESIGN section 5](DESIGN.md#5-undo). Recovery records
  `conflicts: true` in `Tracking` and does **not** bypass review or invoke a
  model job to diagnose trunk; automatic zero-model landing halts until the
  conflict is resolved through normal flow or operator intervention.
- **R2 person requirements:** When `class == R2`:
  - `U` requires a scoped `conary-owner:v1` review from an account in
    `settings.people` (`Door::R2`, listed once under
    `one-question-per-decision`).
  - Automated recovery starts **no judge model** on `U`, so an R2 recovery undo
    can **never** land on a model judgment alone — a person must approve the
    exact head, target branch, and intent fingerprint. If `settings.people` is
    empty, `U` waits indefinitely (fail closed).
  - `U` rides **alone** in the merge train on exact candidate-tree evidence
    bound to the trunk commit it lands on; if trunk moves before merge, the
    candidate drops and rebuilds ([DESIGN 4.7, R2](DESIGN.md#47-the-train);
    direct R2 merges remain refused).
- **Completion and loop guards:**
  - `U` lands and new `trunk_tip` is green ->
    `Resolved { by: UndoLanded { undo_change: U, undo_pr } }`.
  - `U` lands and new `trunk_tip` is still red -> increment
    `consecutive_undos`; if below `red_trunk_max_consecutive_undos`, start a
    fresh `Confirming` incident on the new `trunk_tip`; otherwise
    `Halted { reason: RecoveryBudgetExhausted }`.
  - `U` is closed without landing -> `Halted { reason: UndoClosed { undo_pr } }`
    (never reopens in a loop).
  - `U`'s own required checks fail persistently ->
    `Halted { reason: UndoFailedChecks { undo_pr } }` (never spawns a model job
    to diagnose or fix trunk).
  - Trunk turns green by another commit while `U` is open -> cancel `U`'s
    active proofs / leave train / withdraw permission, close `U` with a note
    naming the fixing commit, and transition to `Resolved { by: TrunkGreen }`.

#### 7. `Halted` and `Resolved`

- **`Halted`** is terminal for `(red_commit, failing_checks)`: repeated passes
  and webhooks do not re-probe or re-open PRs while `trunk_tip == red_commit`.
  It clears automatically if `trunk_tip` moves to a new commit (resolving if
  green, or starting fresh evaluation if red), or via an explicit operator
  command (`cn flow red-trunk clear OWNER/REPO`).
- `cn flow status` reports `red_trunk` for every repository (state, incident
  ID, `red_commit`, `known_good`, tested commits, active worker or `undo_pr`,
  and exact `halt_reason` when `Halted`). `Halted` incidents are exported to
  the stall watchdog alongside overdue PR waits, never as owner doors unless
  the cause is `Door::R2` or `OutsideCommit`.

## How each required edge case is handled

| Edge case | Rule in the state machine |
| --- | --- |
| **Grouped train candidates** | A finished candidate `[A, B, C]` only has candidate-tree evidence for `T3 = after(C)` (and only if `tree(T3) == candidate.tree`). `T1 = after(A)` and `T2 = after(B)` have no candidate evidence of their own, so recovery walks the individual `Land` ops backwards from `T3` toward `T0` and probes intermediate commits directly, isolating `A`, `B`, or `C` (or a `State::Broken` partial landing) to the exact most-recent `pass -> fail` transition even if intermediate verdicts are non-monotone (`T0=pass, T1=fail, T2=pass, T3=fail`). |
| **Concurrent trunk movement** | Every pass re-reads `trunk_tip`. If trunk turns green, active workers are cancelled (`pause-vacates`) and the incident resolves. If trunk advances to a newer commit while not green during `Confirming`, `Bisecting`, `Attributed`, or `Executing` (before an undo PR exists on GitHub), recovery cancels any active worker on the superseded tip, never applies the old `[known_good, red_commit]` attribution to the newer unprobed tip, and transitions to `Confirming` anchored on the new `trunk_tip`. Once the new tip confirms persistently red, `Bisecting` re-anchors `[known_good, ..., trunk_tip]` within `red_trunk_max_window`, verifies the new `trunk_tip` deterministically fails now (halting `NonReproducible` if it passes), and walks backwards from `trunk_tip - 1` reusing live-revalidated exact-tree attested verdicts for previously probed trees. If `trunk_tip` already removed `C`'s diff without a ledger `Undo` op (`UndoError::Nothing`), recovery halts `CulpritAlreadyAbsent` (or resolves if green) instead of opening an empty PR. While a confirmed red incident is active, ordinary PRs do not land ahead of the repair. |
| **Lost responses** | (1) Rerun ask in `Confirming`: reconciled via GitHub's `run_attempt`. (2) Proof worker result: `.running` lock + durable answer file + independently signed service attestation on the check run. (3) `cn undo` PR creation: `Executing` persists intent before calling GitHub; retry runs `strays()` + `pulls_from()` on `conary/undo/<C>/<attempt>` and records the stray PR with its original marker reason (`How::Found`), never opening a duplicate. |
| **Daemon restart / crash** | Every transition atomically writes `red-trunk.json` under the repo lock before launching a worker or calling GitHub writes. On startup, `events::Runtime::restart` kicks every repo, replays the anchored op log, reloads `red-trunk.json`, and re-watches every outstanding proof worker's `.running` lock (bisection workers included). Completed attested probes are reused without re-running the proof plugin once any stored `Attest::Service` verdict passes live revalidation against its reporting check run's current publication. |
| **Inconclusive proofs** | Missing/invalid/expired/out-of-scope attestation, edited or removed service check-run publication on reuse, wrong commit or tree, stale `definition` digest, unsigned plugin stdout, plugin crash/malformed JSON/declared model, `error`/`cancelled`/`stale`/`neutral`/`skipped` result, worker hitting its history-sized deadline, or degraded GitHub read: recorded (`degraded` where applicable), never retried on that tree (`limits-are-design-bugs`), never advances the reverse-suffix search, and fails closed to `Halted { reason: InconclusiveProof }`. |
| **Undo chains (#199)** | Attribution walks trunk commit ancestry (`before -> after`), never `Landing.at`. If `A -> U1(A) -> U2(U1)` fails at `U2`, attribution names `U2` and calls `cn undo U2` (avoiding `UndoError::UndoneUndo`). If `A` is already reverted by `U1`, recovery never re-undoes `A` or undoes `U1`. An already-open undo PR is adopted (`How::Found`); a recovery undo PR closed without landing halts (`UndoClosed`) instead of looping. |
| **R2 person requirements** | Revert PR inherits `R2` from the reverted change's highest recorded class (paths or `raised` labels), opens one `Door::R2` owner request (`one-question-per-decision`), runs no model judge, requires a scoped `conary-owner:v1` review from `settings.people` on the exact head and intent, and lands only alone in the train with exact candidate-tree evidence bound to trunk. Empty `settings.people` fails closed. |
| **`repair-ahead-of-red`** | Both `autoland/red-...` fixes (by trusted authors on repo branches) and recovery `conary/undo/...` PRs rank in a dedicated repair tier strictly above `Level::High`, preempt ordinary PR proof slots (cancelling the lowest-priority ordinary proof when slots are full), preempt unlanded gating ordinary train candidates (never interrupting an already-started merge), and are never held in `Stage::Base` waiting on the red trunk's own pending or hung CI runs. Only a stale `autoland/red-...` PR targeting a trunk already confirmed green is held. |

## Simulated scenarios

Every scenario below runs deterministically against `conary-github/src/sim` (no
live network, no live failures, no model plugins). Each delivery PR adds the
scenarios in its column as failing-before-fix tests.

| ID | Scenario | Expected culprit | Expected recovery operations | Fail-closed / invariant checked | Owner |
| --- | --- | --- | --- | --- | --- |
| **S1** | Single R0 direct landing `T0(green) -> T1(A, red)` fails attempt 1, fails attempt 2 | `A` | Rerun requested once in `Confirming`; boundary probes verify `T1` fail + `T0` pass; `cn undo A` opens PR `#U`; lands on attested evidence without review/model/person -> `Resolved` | Zero model plugin calls; zero `ListOwner`; `A.status == Reverted` | RT2, RT3 |
| **S2** | Grouped train candidate `[A, B, C]` lands `T0 -> T1(A) -> T2(B) -> T3(C)`; only `T3` had candidate evidence, trunk check fails on `T3`; `T1` passes, `T2` fails | `B` | Reverse-suffix walk on `[T0, T1, T2, T3]` probes boundaries `T3`(fail), `T0`(pass), then `T2`(fail), `T1`(pass) (`<= 4` probes); `cn undo B` reverts `B` alone onto `T3`, keeping `A` and `C`; lands -> `Resolved` | Intermediate candidate commits probed directly; `A` and `C` remain `Landed`, `B` is `Reverted` | RT2, RT3 |
| **S3** | Two green PRs make a red trunk (`T0 -> T1(A) -> T2(B)`), `T2` also has a second hung/pending CI run, and an older `priority: high` PR holds the sole proof slot | `B` | `repair-ahead-of-red`: bisection and `cn undo B` do not wait on `T2`'s hung run (`base_held == false`); ordinary `high` PR's proof slot is vacated; `U` jumps ahead of `high` PR in proof slots and train, lands, restores `tree(T1)` | Exact reproduction of burrow#1102 / burrow#1108 class; `U` never enters `Stage::Base` while trunk is red | RT1, RT3 |
| **S4** | Delayed sync & tied timestamps (#199): `T0 -> T1(A) -> T2(B)` where `Landing(A).at >= Landing(B).at`, `B` broke trunk | `B` | Window built from `before -> after` commit ancestry, not `Landing.at`; probes `T1`(pass) -> blames `B` | Never inverts order under delayed sync, backfill, or equal timestamps | #199, RT2 |
| **S5** | Undo chain (#199): `T0 -> T1(A) -> T2(U1 reverts A, green) -> T3(U2 reverts U1, red)` | `U2` (not `A`) | Bisect finds `T2(pass) -> T3(fail)` -> attributes `U2`; calls `cn undo U2` (not `A`); `U3` lands, reverting `U2` and returning `A` to `Reverted` | Never fails with `UndoError::UndoneUndo`; net effect of `A` is reverted | #199, RT2, RT3 |
| **S6** | Already-reverted change (#199): `T1(A)` broke trunk, `T2(U1 reverts A)` already landed, `T3(B)` breaks trunk again | `B` (never `A` or `U1`) | Window starts at `T2` (green) -> blames `B`; calls `cn undo B`; `U1` is untouched | Never re-undoes `A` and never undoes finished rollback `U1` | #199, RT2, RT3 |
| **S7** | Concurrent trunk movement: while bisecting `T0..T4`, commit `T5` lands and turns trunk green | None (resolved) | Active bisection proof worker cancelled immediately (`pause-vacates`); no undo PR opened -> `Resolved { by: TrunkGreen }` | Cancelled worker signs no counted evidence; zero stale PRs | RT2, RT3 |
| **S8** | Concurrent trunk movement: while bisecting `[T0..T3]` for `gate` (`T3=fail`, `T0=pass`, `T2=fail` attested), trunk advances while still red on `gate`: (a) disjoint commit `D` lands at `T4` (deterministically fails `gate`); (b) trusted repair `D` at `T4` fixes `B` (`T4` passes `gate`) and `E` at `T5` breaks `gate` again (`T5` fails `gate`); (c) trusted repair `D` at `T4` fixes `B` so `T4` passes the deterministic proof (or removes `B`'s diff while `T4` still fails `gate` for another reason) | (a) `B`; (b) `E` (never `B`); (c) none | Recovery cancels any active worker on superseded `T3`, never applies stale `[T0..T3]` attribution to the newer tip, and re-confirms/re-probes through the new `trunk_tip`: (a) re-anchors `[T0..T4]`, probes `T4`(fail), reuses live-revalidated `T3`/`T2`/`T0` without re-running them, probes `T1`(pass), verifies `trunk_tip == T4`, opens and lands `cn undo B`; (b) re-anchors `[T0..T5]`, probes `T5`(fail) and `T4`(pass), stops and opens `cn undo E`; (c) boundary probe `T4`(pass) halts `NonReproducible` without calling `undo(B)` (or if `T4` still fails deterministically and removed `B`'s diff, `UndoError::Nothing(B)` halts `CulpritAlreadyAbsent` without opening an empty PR) | Old `[known_good, red_commit]` attribution is never applied to a newer unprobed tip; already-probed trees are reused only after live service revalidation; never opens empty undo PR | RT2, RT3 |
| **S9** | Lost HTTP response & restart during `Executing`: `open_pull` succeeds on GitHub (`#42` on `conary/undo/<B>/1`), then transport errors and `conaryd` restarts before `Body::Undo` is appended | `B` | On restart, `Executing` re-invokes `undo(B)` -> `strays()` finds `#42`, appends `Body::Undo` with original marker reason, returns `How::Found` -> `Tracking { undo_pr: 42 }` | Exactly one PR (`#42`) opened; original reason preserved | RT3 |
| **S10** | Daemon restart mid-bisection: (a) worker still running with `.running` lock; (b) worker finished and signed attestation while daemon was down | `B` | `Runtime::restart` re-watches `.running` lock / kicks repo; (a) waits for exit without launching a duplicate worker; (b) revalidates the reporting check run's current signed publication and advances the reverse-suffix search without re-running `T2` | Zero duplicate proof runs; `Body::TrunkEvidence` persists audit provenance and revalidates current check-run output before reuse | RT2, RT3 |
| **S11** | R2 culprit `C` (subcase (a) `oneWay` path; subcase (b) `r2` label raise on non-`oneWay` path) | `C` (`R2`) | `cn undo C` opens PR `#U` with inherited `R2` class; raises single `Door::R2` request (`one-question-per-decision`); runs no judge model; refuses direct merge and unapproved landing; after scoped `conary-owner:v1` review from `settings.people`, rides alone in train; if trunk moves mid-train, candidate drops and rebuilds | Never lands R2 undo without person approval and exact candidate-tree + trunk binding; empty `settings.people` waits closed | RT3 |
| **S12** | Fail-closed: inconclusive proof (unsigned stdout, invalid signature, edited or removed service check-run publication after recording `TrunkEvidence`, wrong tree, stale recipe `definition`, plugin crash, declared model, worker hits history-sized deadline, or `error` attestation) | None (fail closed) | Records degradation where applicable; does not advance the search; does not trust stale cached service verdicts or retry on that tree (`limits-are-design-bugs`); opens no PR; lists no owner -> `Halted { reason: InconclusiveProof }` | Strict fail-closed; tampered/removed service publications halt instead of attributing an undo; surfaced in `cn flow status` and watchdog | RT2, RT3 |
| **S13** | Fail-closed: external outage (`known_good` fails now) or non-reproducible flake (`red_commit` passes now) | None (fail closed) | Boundary probe catches both before bisecting interior; opens no PR -> `Halted { reason: KnownGoodFailsNow }` or `Halted { reason: NonReproducible }` | Never falsely blames an innocent landing during infra/upstream outage or flake | RT2 |
| **S14** | Fail-closed: culprit is `Body::Outside`, broken commit ancestry, or no `known_good` in `max_window` | `Outside` / none (fail closed) | `Outside` -> `Halted { reason: OutsideCommit }` + owner violation; broken ancestry -> `Halted { reason: BrokenAncestry }`; window exhausted -> `Halted { reason: NoKnownGoodWithinWindow }` | Never invents a `ChangeId`, never blames a neighbor of an `outside` commit, never scans unbounded history | RT2, RT3 |
| **S15** | Fail-closed: operator closes recovery undo PR `#U`, revert conflicts with later work (`conflicts: true`), or consecutive undo budget (`3`) exhausted | `B` (for closed/conflict) / none (budget) | Closed `#U` -> `Halted { reason: UndoClosed }` (never reopens); conflict -> opens bare revert PR with `conflicts: true`, does not bypass review or start a model diagnostic job; 3rd undo still red -> `Halted { reason: RecoveryBudgetExhausted }` | Strict loop bounds; no runaway unwind of trunk | RT3 |
| **S16** | `spenders-off-by-default` and `pause-vacates` | `B` (on resume) | With `red_trunk_recovery = false`, red trunk is observed in status only (zero proofs, zero PRs). With `true`, `cn flow pause` mid-bisection cancels active worker immediately, vacates slot, and resume continues from revalidated attested probes | Default config spends nothing; pause takes effect at next decision with no deploy | RT2, RT3 |
| **S17** | Non-monotone interior history: `T0(pass) -> T1(A, fail) -> T2(B, pass, no prior trunk pass recorded) -> T3(C, fail)` | `C` (never `A`) | Boundary probes verify `T3`(fail) and `T0`(pass); reverse-suffix search probes `T2`(pass) and stops immediately (`3` probes total) -> attributes `C` and opens `cn undo C`, never probing or blaming `A` | Sound on non-monotone sequences; `A` and `B` remain `Landed`, `C` is `Reverted` | RT2, RT3 |

## Single-PR delivery boundaries

Slice 7 delivers in **four** bounded single-PR tasks in Conary, after
prerequisite [#199](https://github.com/FieldmouseWorks/Conary/issues/199) (and
5c's proof plugin contract, already landed). Each task is one PR with its own
failing-before-fix tests, no stacks, and no live changes until deployment.

```text
#199 (landing order + undo chains) ──┬──► RT2 (detect + bisect) ──┐
                                     │                            ├──► RT3 (execute + attest + wire) ──► RT4 (drill)
RT1 (repair-ahead-of-red) ───────────┴────────────────────────────┘
```

### Prerequisite: #199 (already filed, reused)

- **Owner:** [#199](https://github.com/FieldmouseWorks/Conary/issues/199) (not
  re-owned here).
- **What slice 7 consumes:** Real trunk landing order (`before -> after` commit
  chain, independent of ingestion timestamps) and net-effect undo-chain
  resolution in `conary-core` / `conary-github::undo`.

### RT1: `flow: prioritize red-trunk repairs ahead of red runs and ordinary work`

Delivers the `repair-ahead-of-red` lesson across all red-trunk repairs
(`autoland/red-...` fixes and red-trunk recovery `conary/undo/...` PRs). Can
start immediately, in parallel with #199.

- **Files:** `crates/conary-github/src/flow/priority.rs`, `read.rs`,
  `decide.rs`, `train.rs`, `pass.rs`, `docs/flow.md`, `docs/lessons.toml`,
  `crates/conary-github/src/sim/tests/flow_priority.rs`,
  `crates/conary-github/src/sim/tests/flow_reader.rs` (update
  `a_red_fix_lands_only_on_the_default_tip_observed_red` and add new tests).
- **Changes:**
  1. Extend repair identification (`flow::red_repair`) to cover both trusted
     `autoland/red-...` PRs and repo-owned `conary/undo/...` PRs whose recorded
     `Undo` op is linked to an active red-trunk recovery (or targeting a red
     default branch).
  2. Add a repair rank strictly above `Level::High` in `flow::Rank` so repairs
     outrank even older `priority: high` PRs in pass order, proof slots, and
     train admission.
  3. Remove the burrow#1102 / burrow#1108 wait in `flow/read.rs`: a repair PR
     is never held in `Stage::Base` (`base_held = false`) while the default
     branch has any failing check or an active red-trunk incident, even when
     another check on the default branch is `pending`/hung or its snapshot lags
     `tip`. Hold a stale `autoland/red-...` PR only when `tip` is confirmed
     green with no active red incident.
  4. In `flow/pass.rs` and `flow/train.rs`, allow a repair PR to preempt
     ordinary PR proof slots (cancelling the lowest-priority ordinary proof
     when slots are full) and enter the front of the train (dropping an
     unlanded gating ordinary candidate on the red trunk, never interrupting an
     in-flight merge).
- **Lessons moved:** `repair-ahead-of-red` (`designed` -> `built`, pointing at
  RT1's sim tests).
- **Verification:** Failing-before-fix sim tests covering **S3**'s priority,
  hung-trunk-CI, proof-slot preemption, and train-front behavior, plus stale
  `autoland/red-` held once trunk is confirmed green.

### RT2: `recovery: detect persistent red-trunk failures and bisect over landing order`

Delivers the pure, bounded detection-and-bisection state machine and its
deterministic proof-plugin driver in observation/attribution mode (opens **no**
undo PRs yet and mutates no trunk/PR state on GitHub).

- **Waits on:** [#199](https://github.com/FieldmouseWorks/Conary/issues/199).
- **Files:** New pure kernel module `crates/conary-core/src/recovery.rs`
  (landing-order window builder from #199, reverse-suffix search state,
  bounds, fail-closed halt reasons); new driver
  `crates/conary-github/src/flow/red_trunk.rs` (`Confirming` + `Bisecting` +
  `Attributed` + `Halted`, `red-trunk.json` atomic persistence, `prepare` +
  `proof` worker dispatch via `flow/jobs.rs`, `pause-vacates` cancellation);
  unit tests in `conary-core` and sim tests in
  `crates/conary-github/src/sim/tests/red_trunk_bisect.rs`.
- **Changes:**
  1. Pure `conary-core` recovery kernel: constructs `[known_good, ...,
     red_commit]` strictly from trunk `before -> after` commit order, enforces
     `max_window`, enforces mandatory `red_commit` fail + `known_good` pass
     boundary checks, computes exact-tree verdict reuse (requiring live
     check-run revalidation for `Attest::Service`), walks backwards from
     `red_commit - 1` to `known_good` in `<= window.len()` probes so non-monotone
     histories always attribute the exact most-recent `pass -> fail` transition,
     re-anchors to `Confirming` when `trunk_tip` moves past `red_commit` (never
     applying a superseded attribution to an unprobed tip), resolves culprit
     (`Land` vs `Outside`, undo-chain net effect from #199, highest recorded
     `Class`), and enumerates every fail-closed `HaltReason`.
  2. `Confirming` stage reusing `train/retry.rs`'s single full workflow rerun
     on `run_attempt == 1`, rejecting `cancelled`/`stale`/attested-`error`
     infra failures as `InconclusiveConfirm`.
  3. `Bisecting` stage driving only `prepare` + `proof` plugins (refusing any
     model-declaring plugin), sized by `flow/durations.rs` history deadlines,
     with atomic `red-trunk.json` persistence before spawning and full
     `.running` lock re-watch on restart.
  4. Stops at `Attributed` (read-only attribution report; does not call
     `undo()` yet).
- **Lessons held:** `no-model-in-mechanical-step`, `same-inputs-same-verdict`,
  `degraded-counts-as-failed`, `limits-are-design-bugs`, `pause-vacates`.
- **Verification:** Pure unit tests for all window, reverse-suffix search
  (including non-monotone sequences and concurrent trunk movement
  re-anchoring), undo-chain, and halt-reason transitions, plus sim tests for
  **S1, S2, S4, S5, S6, S7, S8, S10, S12, S13, S14, S16, S17** (through
  attribution/halt).

### RT3: `recovery: execute authenticated red-trunk undo and wire conaryd`

Delivers authenticated verdict verification, `Body::TrunkEvidence`, `Executing`
+ `Tracking` via `cn undo`, R2 person-approval enforcement, settings schema,
`conaryd` wiring (off by default), and `cn flow status` / watchdog reporting.

- **Waits on:** **RT1**, **RT2**, and [#199](https://github.com/FieldmouseWorks/Conary/issues/199).
- **Files:** `crates/conary-core/src/op.rs` and `ledger.rs`
  (`Body::TrunkEvidence`); `crates/conary-github/src/attest/service.rs`
  (verify signed commit `fail` statements for bisection/trunk checks while
  keeping `pass_result` strictly `pass`-only for landing permission);
  `crates/conary-github/src/flow/red_trunk.rs` (`Executing`, `Tracking`, R2
  inheritance, clean-revert review exemption with mechanical-head check);
  `crates/conary-github/src/flow/types.rs`, `schema.rs`, `pass.rs`,
  `switches.rs`, `events/runtime.rs`; `crates/cn/src/flow.rs` (`cn flow status`
  `red_trunk` output + `cn flow red-trunk clear`); docs (`docs/flow.md`,
  `docs/service-attestation.md`, `docs/DESIGN.md`, `docs/lessons.toml`); sim
  tests in `crates/conary-github/src/sim/tests/red_trunk_exec.rs`.
- **Changes:**
  1. Add immutable `Body::TrunkEvidence` to `conary-core` (accepted only for a
     recorded `Land`/`Outside` trunk commit, with verified attestation).
  2. Extend `attest::service` with a dedicated `verdict_result` helper for
     bisection/trunk probes accepting signed `pass` or `fail` statements
     matching the check run conclusion, `repository`, `commit`, `tree`,
     `check`, and `definition == recipe_digest` — while keeping `pass_result`
     untouched so a `fail` attestation can never grant landing permission.
     Reject unsigned plugin stdout as `InconclusiveProof`. Require live
     revalidation of the reporting check run's current GitHub publication
     before reusing any stored `Attest::Service` `TrunkEvidence` verdict,
     halting fail closed (`InconclusiveProof`) if the publication was edited,
     removed, or no longer verifies under current producer keys or recipe
     `definition`.
  3. Implement `Executing` (guarding `trunk_tip == red_commit` before opening a
     new PR, persisting intent before `undo::undo`, handling lost responses via
     `strays()` + `How::Found`, handling `UndoError::Nothing`, inheriting `R2`
     class from the reverted change) and `Tracking` (clean mechanical R0/R1
     undo lands on attested evidence without review/judge model; conflicting
     undo keeps full review gate; R2 undo requires `Door::R2` `conary-owner:v1`
     person review + train-alone exact candidate-tree binding; closed/failed
     undo halts; consecutive-undo cap enforced).
  4. Add live settings to `flow::Settings` / `flow/schema.rs`:
     - `red_trunk_recovery: bool` (default `false`, `x-spender = true`);
     - `red_trunk_max_window: NonZeroUsize` (default `16`, `x-spender = false`);
     - `red_trunk_max_consecutive_undos: NonZeroUsize` (default `3`,
       `x-spender = false`).
  5. Wire `red_trunk::step` into `flow::Pass::run` and `events::Runtime::restart`
     (re-watching bisection proof `.running` locks), and expose `red_trunk` in
     `cn flow status` and the watchdog export (`C1`/`U6`).
- **Lessons moved / held:** `no-model-in-mechanical-step`,
  `same-inputs-same-verdict`, `degraded-counts-as-failed`,
  `limits-are-design-bugs`, `owner-only-for-doors`, `spenders-off-by-default`,
  `one-settings-source`, `live-settings`, `switches-show-their-effect`.
- **Verification:** Failing-before-fix sim tests completing **S1–S17** end to
  end, including signed vs tampered/unsigned/edited-after-record service
  attestations, non-monotone interior history (`T0=pass, T1=fail, T2=pass,
  T3=fail`), lost `open_pull` response + restart, R2 path and `r2`-label
  inheritance with person approval and trunk-move drop, and schema default
  tests confirming `red_trunk_recovery` defaults to `false`.

### RT4: `tests: add controlled broken-landing drill and exit acceptance check`

Delivers the repeatable end-to-end drill harness and live acceptance procedure
required by [#318](https://github.com/FieldmouseWorks/Conary/issues/318) and
the M6 exit check ([#321](https://github.com/FieldmouseWorks/Conary/issues/321)).

- **Waits on:** **RT3** (and for executing the live M6 exit drill on a machine,
  RT3 deployed with a deterministic proof plugin and producer key provisioned
  per [fence 12](m6-order.md#fences)).
- **Files:** `crates/conary-github/tests/red_trunk_drill.rs` (full-stack
  automated drill using a real deterministic `conary-plugin v5` fixture
  executable, real Ed25519 service producer signing, real anchored op log, real
  `flow::Pass` + `train` + `undo`, and `conary-github::sim`), `crates/cn/src/flow.rs`
  (`cn flow red-trunk verify-drill OWNER/REPO --culprit CHANGE_ID` read-only
  ledger/status report checker for `#321`), and drill documentation in
  `docs/flow.md`.
- **Automated CI drill (runs in `nix flake check` with zero live side effects):**
  1. Sets up a simulated train repository with a grouped candidate `[A, B, C]`
     where `B` introduces a deterministic proof failure on trunk, an unrelated
     older `priority: high` PR holding a proof slot, and a hung/pending second
     check on the red trunk commit.
  2. Configures a real `conary-plugin v5` deterministic proof binary (zero
     declared models, independent Ed25519 producer signer) and registers a
     poisoned sentinel executable for `fix`, `judge`, and `job` that panics/fails
     the test if ever invoked.
  3. Runs event-driven passes from the red-trunk webhook through confirmation,
     bisection, `cn undo B`, priority preemption, train landing of `U`, and
     green trunk verification.
  4. Asserts the acceptance predicate:
     - `culprit == B`, `U.reverts == [B]`, `B.status() == Status::Reverted`;
     - `A` and `C` remain `Status::Landed`;
     - `model_plugin_invocations == 0` and `owner_requests == 0`;
     - `proof_probes <= window.len()` (strictly bounded by
       `red_trunk_max_window + 1`), every counting probe backed by immutable
       `Body::TrunkEvidence` with verified `Attest::Service` (and live check-run
       revalidation on reuse);
     - `U` never waited in `Stage::Base` and preempted the older `high` PR.
  5. Runs companion negative drills in the same file: unattested proof halts
     fail closed, `known_good` failing now halts fail closed, `outside` culprit
     halts fail closed, and R2 culprit waits for scoped person approval without
     invoking a model.
- **Controlled live drill procedure (for [#321](https://github.com/FieldmouseWorks/Conary/issues/321),
  never run against production repos without an explicit drill target):**
  1. Target a dedicated R0 drill repository (never a live production repository
     such as `remi` or `burrow`) managed by `conaryd` with `red_trunk_recovery
     = true`, a deterministic proof plugin, and its trusted producer key.
  2. Land a benign change `A`, a deliberately broken R0 change `B` (passing a
     skipped/untested path or merged onto `A` so trunk's deterministic check
     fails), and a benign change `C` (or a grouped train candidate `[A, B, C]`).
  3. Observe without intervening: `conaryd` detects the persistent failure,
     bisects to `B`, opens `cn undo B`, lands the revert PR through the train
     ahead of any queued ordinary PR, and returns trunk to green with zero
     model jobs and zero person approvals.
  4. Run `cn flow red-trunk verify-drill OWNER/REPO --culprit <B>` against the
     anchored ledger and attach its JSON output to `#321`'s exit report.
  5. Abort / rollback guard: `cn flow pause OWNER/REPO` vacates any active
     proof worker immediately, and setting `red_trunk_recovery = false` disables
     automatic recovery on the next pass without a deploy.

## Lessons

#318 and its tasks are held to the six entries listed in
[m6.md](m6.md#lessons-each-task-is-held-to):

- **`repair-ahead-of-red`:** Stated in [DESIGN Properties](DESIGN.md#properties)
  and moved from `open` to `designed` in [lessons.toml](lessons.toml) by this
  plan; moved to `built` by **RT1** with its failing-before-fix sim tests.
- **`no-model-in-mechanical-step`:** Already `designed`. **RT2**, **RT3**,
  and **RT4** enforce zero model calls in detection, bisection, attribution,
  and undo execution (verified by RT4's poisoned model-plugin sentinel).
- **`same-inputs-same-verdict`:** Already `designed`. **RT2** reuses attested
  tree verdicts inside an incident and **RT3** persists immutable
  `Body::TrunkEvidence` keyed by exact commit and tree, revalidating any
  stored `Attest::Service` check run's current publication before counting it.
- **`degraded-counts-as-failed`:** Already `built` in the flow. **RT2** and
  **RT3** extend fail-closed degraded handling to every red-trunk read, lookup,
  and inconclusive proof verdict.
- **`limits-are-design-bugs`:** Already `built` in the flow. **RT2** sizes
  bisection proof deadlines from `flow/durations.rs` history, never retries a
  timed-out or inconclusive proof on the same tree, and bounds window size and
  consecutive undos explicitly.
- **`owner-only-for-doors`:** Already `built` in the flow. **RT3** lists the
  owner only for `Door::R2` (R2 undos) and `OutsideCommit` violations; R0/R1
  recoveries and operational halts never ask the owner.

## Covered elsewhere, not scheduled again

- **Landing order and undo-chain net effect:** [#199](https://github.com/FieldmouseWorks/Conary/issues/199).
- **Proof plugin contract (`prepare`/`proof`, zero models) and burrow's proof
  producer:** [m6-plugins.md](m6-plugins.md) (C1, C3, P).
- **Service attestation key provisioning and verifier base:** #309 (#344).
- **Watchdog publisher and stuck-issue upkeep (`U5`, `U6`) and deletion of
  `steward.py`'s `watch_default` (`D1`–`D3`):** [m6-upkeep.md](m6-upkeep.md).
  Task `D` waits on RT3 being live.
- **M6 final exit report:** [#321](https://github.com/FieldmouseWorks/Conary/issues/321),
  which consumes RT4's drill report.

## Filing

Once this plan lands, file **RT1**, **RT2**, **RT3**, and **RT4** as issues in
`FieldmouseWorks/Conary`:

- Each title matches its heading (`RT1:` through `RT4:`).
- Each body links its section here, lists its lesson IDs and simulated scenario
  IDs, states its fence (`RT2` waits on `#199`; `RT3` waits on `RT1`, `RT2`,
  and `#199`; `RT4` waits on `RT3`), and says `Refs #318` and `Refs #256`.
- Tasks whose dependencies have not yet landed carry `later` until their fence
  merges.

These tasks grant no authority beyond [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s.
