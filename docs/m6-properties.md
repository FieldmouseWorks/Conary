# M6: delivery plan for the remaining flow properties

The pipeline audit ([#294](https://github.com/FieldmouseWorks/Conary/issues/294))
turned what the steward taught us into properties Conary must have by
construction, and the stalls since added more
([m6-order.md](m6-order.md#what-the-pipeline-taught-since)). Some already
have an owner. This plan gives every remaining one a bounded single-PR task,
says which of those tasks must land before a repository switches to
conaryd, and links the owner of everything else. It also says, for each
steward issue whose substance belongs in the flow, what the flow takes and
what it doesn't. It plans only this group, not the rest of M6
([m6.md](m6.md)).

This plan implements nothing, deploys nothing and grants no authority beyond
[#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s. It was written
against Conary's main at `6f2a743` on 2026-11-14, and brought up to date
with main at `292aa71` on 2026-11-16, once
[#307](https://github.com/FieldmouseWorks/Conary/issues/307) had landed. The
issues linked here own the live state.

The lessons register is [lessons.toml](lessons.toml). Its seed and DESIGN's
Properties index are [#305](https://github.com/FieldmouseWorks/Conary/issues/305)'s,
its check is [#306](https://github.com/FieldmouseWorks/Conary/issues/306)'s,
and binding acceptance criteria to lesson IDs is
#307's ([m6.md](m6.md#lessons-each-task-is-held-to)). The tables below
name each property by its entry. This plan moves the entries it owns that
were `open` to `designed`, stated in DESIGN's
[Properties](DESIGN.md#properties) index. Each task names the entries it
moves, and its PR moves them to `built` or says why one doesn't apply.

## Ground rules for every task

These apply to every task below. The tasks don't repeat them.

- **Conary stands alone.** Each property works with `cargo install`, a token
  and a configuration file: no burrow, no Nix, no steward (finish-line
  item 7 in [DESIGN](DESIGN.md#12-roadmap)). Where burrow integrates a
  property (its settings source, its doors, its drain), that's burrow's own
  issue, linked from the task, and Conary's tests never need it.
- **No steward behavior the properties forbid is ported.** None of these
  survives any task: a model running a proof, a binary first-priority queue,
  a silent fallback, an off switch that needs a deploy, a merge or a running
  pipeline, paused or held work that keeps its capacity, and an owner asked
  for a call that isn't a one-way door.
- **Substance, not Python.** Where a task takes a steward issue's substance,
  it builds the property in the flow from that issue's stated behavior. No
  task ports `step.py`, `tasks.py` or any other steward Python, and none
  waits for the steward issue to land in burrow
  ([what M6 makes moot](m6-order.md#what-m6-makes-moot)).
- **One settings home.** Every option a task adds is a field of the flow
  configuration ([flow.md](flow.md)), with unknown keys rejected, and is
  declared in the schema [FP1](#fp1-flow-publish-the-configurations-schema-and-its-live-exceptions)
  adds. There's no second settings store in Conary, and burrow's settings
  source generates the file rather than re-declaring its fields.
- **A failing scenario first.** Each task's regression scenario runs against
  the simulated GitHub (`crates/conary-github/src/sim`) or the crate's own
  tests, and fails on main before the task's change. Its PR says so.
- **The cutover path goes first.** The PR of every task marked as a cutover
  prerequisite below carries the `first` label, as #313's and #314's do.

## Where each property stands

Every entry in the register, by its ID, with its owner. Rows that point at
another issue are not scheduled again here.

| Entry | Owner | State on main |
| --- | --- | --- |
| `no-pinned-engines`, `fresh-judge` | Built (DESIGN 6.1) | Built |
| `no-model-in-mechanical-step` | [#295](https://github.com/FieldmouseWorks/Conary/issues/295) (landed in #330), C3 (landed in #398), then [m6-plugins.md](m6-plugins.md)'s P | Contract and flow built; the proof plugin planned |
| `degraded-counts-as-failed` | [FP5](#fp5-ledger-and-flow-record-a-degraded-outcome-and-count-it-failed) (landed in #449, #517 and #519); C2's partial publication went with the flow's own finding marks (#461), and a fix's replies wait to be read back, so they record nothing | Built: the op, and the flow's unreadable records, answers and stop markers, evidence lookups, GitHub reads that fail or stop short, and marks that don't parse ([flow.md](flow.md#degraded-paths)) |
| `same-inputs-same-verdict` | DESIGN 4.3 rule 3 by tree ([#337](https://github.com/FieldmouseWorks/Conary/pull/337), landed), then [#291](https://github.com/FieldmouseWorks/Conary/issues/291) (#380 landed, #324 turns it on) for gate reuse and [#293](https://github.com/FieldmouseWorks/Conary/issues/293) for a judge's verdict | Wording and lookup built; reuse and carry open |
| `heavy-work-waits-for-settled-inputs` | [#292](https://github.com/FieldmouseWorks/Conary/issues/292): [#296](https://github.com/FieldmouseWorks/Conary/issues/296) to [#302](https://github.com/FieldmouseWorks/Conary/issues/302) | Open (#296 in PR #333) |
| `spenders-off-by-default` | [#378](https://github.com/FieldmouseWorks/Conary/issues/378) (landed in #381) for the defaults; [FP1](#fp1-flow-publish-the-configurations-schema-and-its-live-exceptions) holds every spender to it through the schema | Defaults built: zero proof slots, no machine's accounts; nothing marks which options spend |
| `one-settings-source` | [FP1](#fp1-flow-publish-the-configurations-schema-and-its-live-exceptions) for Conary's schema; [burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655) and [burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724) import and render it | Built: one schema of the flow's, conaryd's events and its gate-reuse configuration ([#428](https://github.com/FieldmouseWorks/Conary/issues/428)), every reader loading through it; burrow's import open |
| `live-settings` | [FP1](#fp1-flow-publish-the-configurations-schema-and-its-live-exceptions) | Built: every option of the three files read at the next decision, but conaryd's listen addresses, webhook secret and data directory, which the schema declares as needing a restart |
| `explicit-priority` | [FP4](#fp4-flow-order-admission-and-proof-slots-by-explicit-priority-then-age) | Not built: one binary `first_label` |
| `one-question-per-decision` | [FP10](#fp10-flow-ask-the-owner-only-for-a-named-one-way-door-and-once-per-decision) | Not built |
| `switches-show-their-effect` | [FP3](#fp3-flow-report-every-switch-in-effect-and-what-it-holds) | Not built |
| `limits-are-design-bugs` | [FP6](#fp6-flow-a-second-deadline-on-the-same-tree-is-a-stall-not-a-retry) and [FP12](#fp12-flow-size-a-proofs-deadline-from-its-own-history); burrow's watchdog baselines are [burrow#651](https://github.com/FieldmouseWorks/burrow/issues/651) | Not built: a timed-out job is retried without end |
| `off-switch-independent` | [FP2](#fp2-flow-pause-and-hold-vacate-and-resume-without-the-pipeline), then [#313](https://github.com/FieldmouseWorks/Conary/issues/313) for the landing owner | Not built: Conary has no pause |
| `reusable-modules` | Finish-line item 7 in [DESIGN](DESIGN.md#12-roadmap) | Designed |
| `pause-vacates` | [FP2](#fp2-flow-pause-and-hold-vacate-and-resume-without-the-pipeline); the plugins answer its cancellation ([m6-plugins.md](m6-plugins.md#restart-and-cancellation)) | Not built: a held PR's proof keeps its slot |
| `spend-only-where-needed` | [#291](https://github.com/FieldmouseWorks/Conary/issues/291) (the cheapest path: reuse), [#292](https://github.com/FieldmouseWorks/Conary/issues/292) (start only once review settles), [#320](https://github.com/FieldmouseWorks/Conary/issues/320) (one gate per batch), and [FP2](#fp2-flow-pause-and-hold-vacate-and-resume-without-the-pipeline) (stop when paused). No task of its own | Partly: the flow stops work answering for an earlier head before it starts more (`flow/decide.rs`) |
| `owner-only-for-doors` | [FP10](#fp10-flow-ask-the-owner-only-for-a-named-one-way-door-and-once-per-decision), then [#314](https://github.com/FieldmouseWorks/Conary/issues/314) and [#318](https://github.com/FieldmouseWorks/Conary/issues/318) | Not built: "Too much to read" and "Two jobs ended without their answer" list the owner |
| `waiting-costs-nothing` | [FP7](#fp7-flow-a-ci-wait-costs-nothing-and-its-queue-time-is-no-stall), then [#317](https://github.com/FieldmouseWorks/Conary/issues/317) | Not built: a queued check's wait counts toward `stall_after` |
| `read-sized-to-data` | [FP9](#fp9-conaryd-one-repositorys-slow-or-failing-read-delays-only-its-own-pass) | Partly: a failed pass books its own recovery (#351), but conaryd runs passes one at a time |
| `recover-by-kind` | [FP8](#fp8-flow-bring-back-a-stalled-stage-by-a-way-github-allows-and-quote-its-error) | Not built: a cancelled check waits for someone else to rerun it |
| `repair-ahead-of-red` | [#318](https://github.com/FieldmouseWorks/Conary/issues/318) | Open |
| `publish-only-news` | [#316](https://github.com/FieldmouseWorks/Conary/issues/316)'s U5 ([m6-upkeep.md](m6-upkeep.md)) | Designed |
| `scope-below-a-repo` | [#313](https://github.com/FieldmouseWorks/Conary/issues/313); FP2's pause leaves room for it | Open |

## Steward issues whose substance lands here

Each of these is a steward issue in burrow or remi. The flow takes the
behavior below, built as a property, and nothing else from it. The rest
closes with the steward ([what M6 makes moot](m6-order.md#what-m6-makes-moot)).
[review-loop.md](review-loop.md) stays the one owner of each review-loop
decision.

| Issue | The flow takes | The flow doesn't take |
| --- | --- | --- |
| [burrow#748](https://github.com/FieldmouseWorks/burrow/issues/748), review limits and judge triggers from settings | The limits and triggers are flow `Settings` fields (`rounds`, `judge_engines`, the fix-growth share), declared in FP1's schema and read at each decision | A second settings reader in the steward: burrow renders the flow's file ([burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724)) |
| [burrow#945](https://github.com/FieldmouseWorks/burrow/issues/945), a looping review goes to the stuck role | Its three triggers and the stuck-role handoff, in [FP11](#fp11-flow-break-a-review-loop-that-wont-settle), with [SJ](#sj-burrow-serve-the-stuck-role-through-the-job-kind) serving the stuck role's job | Nothing left out |
| [burrow#961](https://github.com/FieldmouseWorks/burrow/issues/961), looping expedited stages go to the stuck role before the owner | The order at an expired stage: mechanical recovery ([FP8](#fp8-flow-bring-back-a-stalled-stage-by-a-way-github-allows-and-quote-its-error)), then the stuck role (FP11), then the owner only for a door ([FP10](#fp10-flow-ask-the-owner-only-for-a-named-one-way-door-and-once-per-decision)). It holds for every PR | A separate expedite lane: priority is FP4's levels, not a second path |
| [burrow#955](https://github.com/FieldmouseWorks/burrow/issues/955), an expedited PR fixes only blockers | One follow-up issue per PR for the findings a fix answers, shared with FP11's handoff | Which findings a fix fixes: that's the fix brief's ([review-loop.md](review-loop.md#the-decisions), task B1), the same for every PR |
| [burrow#960](https://github.com/FieldmouseWorks/burrow/issues/960), a deadline for every stage, then branch or flake recovery | Every watched stage already books `overdue_at`; FP4 sizes it by priority level, FP7 leaves queue time out, and FP8 takes the branch update and the rerun a check's kind allows | A known-flake retry: Conary holds no flaky-test record. Flaky upkeep stays burrow's ([#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s "what lives where") |
| [remi#1248](https://github.com/FieldmouseWorks/remi/issues/1248), the escalation policy for held PRs | A hold past `hold_stall_after` is a stall the flow reports, never an owner listing; the owner listing names a door (FP10) | Routing to remi's operator: that's the caller's, on the flow's status |

## Cutover prerequisites

A repository switches to conaryd only once conaryd keeps every property the
steward is being replaced for, and asks the owner for nothing the steward
shouldn't have. So:

| Task | Before #313 (ownership switch) | Before #314 (remi) | Otherwise before |
| --- | --- | --- | --- |
| FP1 schema, spender defaults and live exceptions | Yes: #313 consumes the schema | Yes | |
| FP2 pause and hold vacate | | Yes: #314 verifies pause | |
| FP3 switches report their effect | | Yes | |
| FP4 explicit priority | | Yes: else the binary queue is ported | |
| FP5 degraded counts failed | | Yes: else silent fallbacks are ported | |
| FP6 repeated deadline stalls | | Yes | |
| FP7 a CI wait costs nothing | | Yes | |
| FP8 recovery by kind | | Yes: else a cancelled check waits forever | |
| FP9 per-repository reads | | | [S2f](m6-cutover.md#s2f-burrow) and the first `allow` of a paused repository conaryd holds: before that, conaryd passes only remi and Conary |
| FP10 owner only for doors, once per decision | | Yes: at #314 an owner call is a marked review | |
| FP11 review-loop breakers | | | [S2f](m6-cutover.md#s2f-burrow): elsewhere a loop past the cap waits for the judge plugin or the handback |
| FP12 history-sized deadlines | | | #321's exit check |

FP11 isn't a #314 prerequisite because the steward it replaces on remi has
no loop breaker either; FP6 and FP8 already stop a stage from repeating
without end, and no prerequisite waits on it: what FP8 and FP10 don't hand
on stays overdue until FP11 lands. FP12 isn't one because FP6 already stops a deadline from being
hit over and over; the configured `proof_timeout` stays the ceiling until
FP12 lands.

## The tasks

Each task is one PR in Conary, except SJ, which is one PR in burrow. Each
says what it adds, its regression
scenario, what it waits on and what it leaves alone.

### FP1: flow: publish the configuration's schema and its live exceptions

**Moves:** `one-settings-source`, `live-settings`, `spenders-off-by-default`.

**Crates:** `conary-github` (`flow/types.rs`, `flow/pass.rs`,
`events/runtime.rs`), `cn` (`flow.rs`), `conaryd`. `docs/flow.md`,
`docs/conaryd.md`.

**Work.**
- `cn flow settings --schema` prints a self-describing schema of the flow
  configuration and conaryd's events configuration: for each option its
  name, type, allowed values, default, a one-line description, whether it
  starts paid work (`spender`), and whether it's live. The schema is
  generated from the same types the loaders parse, so a field can't exist
  without its entry.
- A test reads the schema and holds every option marked `spender` to a
  default of off or zero. #381 set those defaults; this keeps a new spender
  from joining without one.
- Live is the default: an option takes effect at the next decision that
  reads it. Each option that can't (conaryd's `listen`, `secret_path`,
  `data`, and whatever else the task finds read only at start) is declared
  `live: false` with its reason and what a change needs (a restart).
  Anything the task can make live, such as the events `repos` list, it
  makes live instead.
- `cn flow settings --config PATH` prints the effective value of every
  option and where it came from (default or file), with the live exceptions
  listed apart.

**Regression scenario.** A test adds a field to `Settings` without a schema
entry and fails; one changes an option the schema says is live between two
passes and sees the second pass act on it; one shows a `live: false`
option listed as an exception; one marks a new option `spender` with a
non-zero default and fails. On main there's no schema to check.

**Leaves alone:** burrow's import of the schema and its rendered file
([burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655),
[burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724)). Conary
adds no write door for configuration.

### FP2: flow: pause and hold vacate, and resume, without the pipeline

**Moves:** `off-switch-independent`, `pause-vacates`.

**Crates:** `conary-github` (`flow/decide.rs`, `flow/proofs.rs`,
`flow/occupancy.rs`, `flow/persist.rs`), `cn` (`flow.rs`). `docs/flow.md`.

**Work.**
- `cn flow pause OWNER/REPO --reason TEXT` and `cn flow resume OWNER/REPO`
  write the pause to the flow's data directory. They need no running
  conaryd, no GitHub, no build and no deploy, and they're always allowed.
  Every pass reads the pause before its first decision and again before
  each effect (a plugin start, an admission, a merge).
- A paused repository vacates: its live review, fix, judge and proof
  workers are cancelled through the existing stop path, its proof slots and
  reservations are released, and nothing new starts. Its PRs keep their
  Memory, receipts and branches. A worker stopped by a pause is recorded as
  stopped by the pause, never as a failure, a strike or a lost job.
- The hold label vacates its PR the same way, through the same code path.
- Resume continues from the saved state: no finished review, fix or proof
  answer is redone, and a stopped job starts again from its request.
- The pause's record has a scope, so a narrower one
  (`scope-below-a-repo`, #313's) adds a variant rather than a second pause.

**Regression scenario.** `proof_slots = 1`, PR A's proof running and PR B
waiting. Holding A frees the slot and B's proof starts; on main A keeps the
slot. Pausing the repository cancels every worker and starts none, with
conaryd stopped; on main there's no pause. Resuming finishes A without
another review round.

**Leaves alone:** per-repository landing ownership, steward or conaryd,
which is [#313](https://github.com/FieldmouseWorks/Conary/issues/313);
burrow's pause door and scoped modes
([burrow#647](https://github.com/FieldmouseWorks/burrow/issues/647),
[burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655)), which
call `cn flow pause` rather than keep a copy of it.

### FP3: flow: report every switch in effect and what it holds

**Moves:** `switches-show-their-effect`.

**Crates:** `conary-github` (`flow/types.rs`, `flow/state.rs`,
`flow/compare.rs`), `cn` (`flow.rs`). `docs/flow.md`.

**Work.**
- Each PR's Next names the switches that stop it, and what each one is
  holding: a pause, a hold label, an omitted plugin kind, the train off, a
  missing person list, an unconfigured evidence verifier.
- `cn flow status OWNER/REPO` prints the repository's switches in effect
  (pause with its reason and time, holds, omitted plugins) and, for each,
  the PRs and workers it holds, from local state alone, so it answers while
  GitHub or conaryd is down.
- `cn flow compare` carries the same field, so the shadow tells a switch
  apart from a disagreement.

**Regression scenario.** A paused repository with a held PR and no review
plugin: status names all three and each PR they stop. On main, a held PR's
Next says only "On hold" and nothing reports the omitted plugin until a
review would start.

**Waits on:** FP2 (the pause it reports). **Leaves alone:** each
repository's effective landing owner, which #313 shows.

### FP4: flow: order admission and proof slots by explicit priority, then age

**Moves:** `explicit-priority`.

**Crates:** `conary-github` (`flow/types.rs`, `flow/proofs.rs`,
`flow/pass.rs`, `flow/train.rs`, `flow/state.rs`). `docs/flow.md`.

**Work.**
- A PR's priority is one of the levels `high`, `medium` and `low`, the
  levels burrow's drain uses ([burrow#652](https://github.com/FieldmouseWorks/burrow/issues/652)),
  read from the labels a setting maps to each level (`priority: high` and so
  on, with `first` mapped to the top level on a machine that keeps it). A PR
  with none takes the configured default level.
- Proof slots, train admission and a targeted kick's view of the others
  order by level, then age, then PR number. `first_label` and its binary
  ordering are deleted in the same PR.
- Each level has its own stall deadline (`stall_after` becomes per level),
  which takes burrow#960's shorter deadline for owner-expedited work.
- The PR's level is in its Next, and dry runs reserve virtual slots in the
  same order.

**Regression scenario.** One proof slot and three waiting PRs: an old
unlabelled one, a newer `priority: high` one and a newest `priority: low`
one. The high one goes first, then the old one, then the low one, and the
high one is overdue at its level's deadline. On main the `priority: high`
label is ignored and the oldest goes first.

**Deployment:** remi's configuration maps its labels before it pins this.

### FP5: ledger and flow: record a degraded outcome and count it failed

**Moves:** `degraded-counts-as-failed`.

**Crates:** `conary-core` (`op.rs`, `ledger`), `conary-github` (`flow/state.rs`,
`flow/occupancy.rs`, `flow/read.rs`, `flow/evidence.rs`). `docs/DESIGN.md`
section 4.6, `docs/flow.md`.

**Work.**
- A degraded outcome is a path that half-worked: a fallback taken, a cache
  or evidence lookup that couldn't tell, an unreadable record reserved
  conservatively, a read too large to finish. Each is appended to the ledger
  as an op naming what degraded, on which change and revision, and why. The
  op is never removed: what clears a degradation is a later op.
- A check whose answer came through a degraded path counts as failed for
  that check, never as passed, and the PR's Next names it as degraded.
- The PR lists every degraded path on main and converts each one. A path
  another open issue owns (#291's can't-tell lookup, #296's deferred
  checks) records through this op when it lands, rather than inventing its
  own.

**Regression scenario.** An unreadable proof ownership record: main
reserves capacity and reports an error on its own PR, with nothing in the
ledger, and once the record reads again nothing shows it happened. The
fixed flow appends one degraded op, the check counts failed, and status
names it. A second scenario does the same for an evidence lookup that
can't tell.

**Waits on:** #305's statement of the property in DESIGN. This task states
the op itself in DESIGN 4.6.

### FP6: flow: a second deadline on the same tree is a stall, not a retry

**Moves:** `limits-are-design-bugs`, with FP12.

**Crates:** `conary-github` (`flow/decide.rs`, `flow/state.rs`,
`flow/jobs.rs`). `docs/flow.md`.

**Work.**
- The flow remembers, per check and tree, how many times its invocation
  reached its deadline. The second time on the same tree, the flow doesn't
  start it again: the PR is overdue with a reason that names the check, the
  tree and both deadlines.
- A new tree, or a changed recipe, starts the count again. A deadline hit
  once still retries as today.

**Regression scenario.** A proof plugin that never answers: after its first
deadline the flow retries; after its second on the same tree main retries
again, without end, with backoff. The fixed flow stops and reports the
stall, and no third worker starts.

### FP7: flow: a CI wait costs nothing, and its queue time is no stall

**Moves:** `waiting-costs-nothing`.

**Crates:** `conary-github` (`flow/ci.rs`, `flow/state.rs`, `flow/decide.rs`),
`conaryd`'s booked waits. `docs/flow.md`.

**Work.**
- A PR waiting on CI starts no plugin and books no timed wake for CI. Its
  pass comes from the check's completion event, which ends the wait only
  when it carries the verdict the wait named.
- A check's stall clock starts when its run starts. Time queued for a
  runner isn't counted toward `overdue_at`; the queue time is in Next, as
  capacity.

**Regression scenario.** A required check queued for three hours with a
two-hour `stall_after`: main marks the PR overdue, and the fixed flow
doesn't until two hours after the run starts. An unrelated event on the PR
while CI runs starts no worker and leaves the wait as it was.

### FP8: flow: bring back a stalled stage by a way GitHub allows, and quote its error

**Moves:** `recover-by-kind`.

**Crates:** `conary-github` (`flow/ci.rs`, `flow/decide.rs`,
`flow/execute.rs`, `api.rs`, the simulated GitHub). `docs/flow.md`.

**Work.**
- At an expired stage deadline the flow takes the first recovery that
  applies, once per stall: update a head behind its base when it merges
  cleanly, then bring back a cancelled or timed-out check the way GitHub
  allows for its kind (a workflow run's rerun, a check suite's re-request),
  and never for a kind GitHub won't rerun.
- A refused recovery isn't tried again in that stall. The PR's status
  quotes the error GitHub gave, as given, instead of a guessed cause.
- After that the stall is overdue in the flow's status, and goes to
  FP11's stuck role once FP11 has landed. The owner is listed only where a
  door applies (FP10). Repeated events, wakes and restarts repeat no
  recovery.

**Regression scenario.** A cancelled workflow run with no replacement:
main shows "cancelled, to be run again" and waits forever. The fixed flow
asks GitHub for the rerun once. In a second case GitHub refuses it, and
the status carries GitHub's message and no second request is sent.

**Leaves alone:** known-flake retries, which need a flaky-test record that
Conary doesn't keep.

### FP9: conaryd: one repository's slow or failing read delays only its own pass

**Moves:** `read-sized-to-data`.

**Crates:** `conary-github` (`events/queue.rs`, `events/runtime.rs`,
`api.rs`, `flow/read.rs`). `docs/conaryd.md`.

**Work.**
- Each read is sized to what it reads: it pages to the end of the data, with
  no fixed cap that fails a pass, and has a deadline of its own.
- A read that fails, is refused or runs past its deadline delays only its
  repository's pass. That pass books its recovery as today, and the other
  repositories' passes go on at once. A refused read leaves the last facts
  in place for the PRs that need it, recorded through FP5's op.

**Regression scenario.** Two repositories, the first one's API hanging: on
main the second repository's kick waits behind it, since passes run one at
a time. In the fixed daemon the second repository's pass finishes in its
own time, and the first books its recovery.

**Waits on:** FP5 (the op a refused read records).

### FP10: flow: ask the owner only for a named one-way door, and once per decision

**Moves:** `owner-only-for-doors`, `one-question-per-decision`.

**Crates:** `conary-github` (`flow/types.rs`, `flow/decide.rs`,
`flow/state.rs`, `flow/approval.rs`, `flow/persist.rs`). `docs/flow.md`.

**Work.**
- `Step::ListOwner` carries the door it's for, from a closed set: an R2
  change, an outside contribution, a judge's `owner` verdict (with the
  judge's reason) and the owner label set by a person. A decision with no
  door doesn't list the owner. The flow makes the call itself, records it
  and shows it: a read too large to finish is FP5's degraded wait, and two
  jobs ending without their answer go to FP8's recovery, then stay overdue
  in the flow's status until FP11's stuck role takes them. A major update the dependency sweep held (#316's sweep) is listed
  only with the door the sweep names.
- A hold past `hold_stall_after` is a stall in the flow's status, never an
  owner listing (remi#1248's substance).
- Each owner request has a subject key: its door, repository, PR and the
  scope it binds (the intent fingerprint), stable across heads that don't
  change that scope. The flow keeps one open request per key. A later wait
  with the same key joins it and adds no new listing or notice, and the
  request lists every PR waiting on it and its current approval marker.
- Joining never widens an approval: R2 still needs the marked review on the
  exact head ([person approval](flow.md#person-approval)).

**Regression scenario.** A PR whose read is too large: main lists the owner
("Too much to read: the owner merges it"), and the fixed flow doesn't. An
R2 PR listed for the owner gets two base-merge heads: main has a new
`owner_approval` marker each time, with nothing tying it to the first, and
the fixed flow keeps one request with one key whose marker moves to the
current head.

**Waits on:** FP5 and FP8 for the calls the owner no longer gets. It doesn't
wait on FP11: until FP11 lands, a loop past its round limit and a stall FP8
couldn't recover stay overdue in the flow's status rather than listed, and
FP11 then puts the stuck role ahead of that.
**Leaves alone:** the owner issue the steward's upkeep keeps, which
[#316](https://github.com/FieldmouseWorks/Conary/issues/316) moves to Rust
and which joins on this key; burrow's question notices
([burrow#649](https://github.com/FieldmouseWorks/burrow/issues/649)).

### FP11: flow: break a review loop that won't settle

**Moves:** no entry of its own. It builds burrow#945's behavior, which [review-loop.md](review-loop.md) owns.

**Crates:** `conary-github` (`flow/review.rs`, `flow/reviews.rs`,
`flow/decide.rs`, `flow/types.rs`). `docs/flow.md`, `docs/review-loop.md`.

**Work.** This builds burrow#945's behavior in the flow, per
[review-loop.md](review-loop.md):
- the triggers (the round cap, fix growth past the configured share
  against the original diff, an answered finding raised again unchanged)
  hand the PR to the stuck role through the flow's `job` kind, with no
  further fix round, and findings on the stuck role's head go to the judge;
- a stall FP8 couldn't recover goes to the stuck role the same way, once
  per stall, before any owner listing (burrow#961's order);
- the stuck role's answered findings link one follow-up issue per PR,
  shared with what burrow#955 asked for;
- the disputed-P2 switch is one `Settings` field whose default is the
  behavior on main, read by the flow alone, never by a plugin.

**Regression scenario.** Each trigger's boundary, against the simulated
GitHub: three rounds with findings hand over and two don't; growth just
past the share hands over and just under doesn't; a changed finding isn't a
repeat; repeated wakes start one stuck-role job. On main none of them
hands the PR to the stuck role.

**Waits on:** m6-plugins.md's
[C2](m6-plugins.md#c2-flow-give-each-finding-a-thread-and-post-a-fixs-answers-on-it)
(the answer history it reads), FP8, and [SJ](#sj-burrow-serve-the-stuck-role-through-the-job-kind) (the plugin
that serves the stuck role's `job`). **Leaves alone:** the owner's open
question on [burrow#939](https://github.com/FieldmouseWorks/burrow/issues/939),
which the switch's default doesn't answer.

### FP12: flow: size a proof's deadline from its own history

**Moves:** `limits-are-design-bugs`, with FP6.

**Crates:** `conary-github` (`flow/proofs.rs`, `flow/jobs.rs`). `docs/flow.md`.

**Work.**
- Each completed proof run's duration is kept with its recipe in one
  durable home the task picks and documents (the ledger's evidence or the
  flow's own records, never both).
- A proof's deadline is sized from that recipe's recent successful runs on
  the repository, with a margin the task documents, never above the
  configured `proof_timeout` and never below a floor. With no history yet,
  the configured value applies.
- Next shows the deadline used and where it came from.

**Regression scenario.** A recipe whose runs take about ten minutes and
`proof_timeout` of three hours: a hung run is cut at the history-sized
deadline, not after three hours. A recipe with no history uses the
configured value. On main every proof's deadline is the configured value.

**Waits on:** FP6.

### SJ: burrow: serve the stuck role through the `job` kind

**Moves:** no entry of its own. It's the plugin behind FP11's handoff,
which no task in [m6-plugins.md](m6-plugins.md) serves: F answers `fix`,
and B1's briefs are review, fix and judge.

**Repo:** burrow, in the burrow M6 lane after J. **Depends on:**
[F](m6-plugins.md#f-fix-plugin) (its crate and broker), [B1](m6-plugins.md#b1-briefs-one-source-for-the-review-fix-and-judge-briefs)
(the briefs' crate) and
[burrow#969](https://github.com/FieldmouseWorks/burrow/issues/969) (the
job's record). It adds no crate, so fence 2 doesn't apply.

**Work.**
- `burrow-fix`'s broker also advertises the `job` kind
  ([plugins.md](plugins.md#the-job-kind)) and starts it as a job on the
  stuck role from the role map, or the request's `execution` when it names
  one. It waits on and stops the job through its record, as F does.
- A stuck-role brief joins B1's in `burrow-brief`: given the PR and the
  flow's reason (the round cap, fix growth, an answered finding raised
  again, or a stall FP8 couldn't recover), the job settles the PR, pushing
  a head or answering why it can't, and opens the one follow-up issue
  FP11 links for the findings it answers.
- At most one live stuck-role job per PR; a closed connection stops it.
- The unit is behind `burrow.autoland.plugins.job.enable`, off by default,
  and the flow's rendered configuration
  ([burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724))
  names it for `job` only when that's on.

**Negative verification.** `cn plugin check` passes with `job` advertised.
A request for each of the four reasons, sent through the configured
plugins table, starts one job on the stuck role with its brief; a second
concurrent request for the same PR is refused; a cancelled request leaves
no running job. Each fails before the change.

## Order

These tasks are in the Conary flow lane
([m6-order.md](m6-order.md#lanes)): they all touch `flow/` or
`docs/flow.md`, so they run one at a time, after C1 to C4 (all landed by
2026-11-16), in this order:

```
FP1 ──► FP2 ──► FP3 ──► FP4 ──► FP5 ──► FP6 ──► FP7 ──► FP8
  ──► FP10 ──► #313 ──► FP9 ──► FP11 ──► FP12
```

The cutover prerequisites come first, then #313. FP9 and FP11 follow it in
the lane, before the S2 groups that need them
([m6-cutover.md](m6-cutover.md#plugins-and-the-flow-properties-that-wait)),
and FP12 before #321. FP11 also waits on SJ, which runs in
the burrow M6 lane after J, beside these.

## Filing

Once this plan lands, each task is filed as an issue in Conary, as a child
of #256's list so the lane admits one at a time:

- The title starts with the task's ID.
- The body links its section here, names the lesson IDs it moves, and says
  `Refs FieldmouseWorks/Conary#312` and `#256`.
- The body carries the fence that it runs after the task before it in the
  order above.
- A cutover prerequisite's body says its PR carries the `first` label.
- SJ is filed in Conary too, and its body says its PR is in burrow and
  closes it by its full name ([fence 8](m6-order.md#fences)).

An issue grants no authority beyond #256's.
