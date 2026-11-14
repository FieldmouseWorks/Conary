# M6: delivery plan for the remaining flow properties

The pipeline audit ([#294](https://github.com/FieldmouseWorks/Conary/issues/294))
turned what the steward taught us into properties Conary must have by
construction. Some of them already have an owner. This plan gives every
remaining one a bounded single-PR task, says which of those tasks must land
before a repository switches to conaryd, and links the owner of everything
else. It plans only this group, not the rest of M6 ([m6.md](m6.md)).

This plan implements nothing, deploys nothing and grants no authority beyond
[#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s. It was written
against Conary's main at `6f2a743` on 2026-11-14. The issues linked here own
the live state.

The lessons register is [lessons.toml](lessons.toml). Its seed and DESIGN's
Properties index are [#305](https://github.com/FieldmouseWorks/Conary/issues/305)'s,
its check is [#306](https://github.com/FieldmouseWorks/Conary/issues/306)'s,
and binding acceptance criteria to lesson IDs is
[#307](https://github.com/FieldmouseWorks/Conary/issues/307)'s. When this
was written the register held only `no-pinned-engines` and `fresh-judge`, so
the tables below name each property by #294's wording. #307 binds them to the
IDs #305 gives them. Each task's PR moves its entries to `built`, or says why
one doesn't apply.

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
  pipeline, and paused or held work that keeps its capacity.
- **One settings home.** Every option a task adds is a field of the flow
  configuration ([flow.md](flow.md)), with unknown keys rejected, and is
  declared in the schema [S1](#s1-flow-publish-the-configurations-schema-and-its-live-exceptions)
  adds. There's no second settings store in Conary, and burrow's settings
  source generates the file rather than re-declaring its fields.
- **A failing scenario first.** Each task's regression scenario runs against
  the simulated GitHub (`crates/conary-github/src/sim`) or the crate's own
  tests, and fails on main before the task's change. Its PR says so.
- **The cutover path goes first.** The PR of every task marked as a cutover
  prerequisite below carries the `first` label, as #313's and #314's do.

## Where each property stands

Every property #294 asked for, and the four it added, with its owner. Rows
that point at another issue are not scheduled again here.

| Property (#294's wording) | Owner | State on main |
| --- | --- | --- |
| No model in a mechanical step: a proof is a program | [#295](https://github.com/FieldmouseWorks/Conary/issues/295) (contract v2's proofs as programs, landed in #330), then [m6-plugins.md](m6-plugins.md)'s C3 and P | Contract built; plugin and slot split planned |
| Degraded is reported and counts as failed | [D1](#d1-ledger-and-flow-record-a-degraded-outcome-and-count-it-failed) | Not built |
| Same inputs, same verdict | DESIGN 4.3 rule 3 by tree ([#337](https://github.com/FieldmouseWorks/Conary/pull/337), landed), then [#291](https://github.com/FieldmouseWorks/Conary/issues/291) (#323, #324) for gate reuse and [#293](https://github.com/FieldmouseWorks/Conary/issues/293) for a judge's verdict | Wording built; reuse and carry open |
| Heavy work waits for settled inputs | [#292](https://github.com/FieldmouseWorks/Conary/issues/292): [#296](https://github.com/FieldmouseWorks/Conary/issues/296) to [#302](https://github.com/FieldmouseWorks/Conary/issues/302) | Open (#296 in PR #333) |
| Spenders off by default | [S2](#s2-flow-every-spender-defaults-off-and-no-default-names-a-machine) | Not held: `proof_slots` defaults to unlimited |
| One settings source, read and never copied | [S1](#s1-flow-publish-the-configurations-schema-and-its-live-exceptions) for Conary's schema; [burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655) imports it | Flow config is reread each pass; no schema |
| Explicit priority | [A1](#a1-flow-order-admission-and-proof-slots-by-explicit-priority-then-age) | Not built: one binary `first_label` |
| One question per decision | [A2](#a2-flow-ask-a-person-once-per-decision) | Not built |
| Switches report their effect | [O2](#o2-flow-report-every-switch-in-effect-and-what-it-holds) | Not built |
| A limit work keeps hitting is a design bug | [T1](#t1-flow-a-second-deadline-on-the-same-tree-is-a-stall-not-a-retry) and [T2](#t2-flow-size-a-proofs-deadline-from-its-own-history); burrow's watchdog baselines are [burrow#651](https://github.com/FieldmouseWorks/burrow/issues/651) | Not built: a timed-out job is retried without end |
| An off switch never depends on the pipeline it stops (remi#980) | [O1](#o1-flow-pause-and-hold-vacate-and-resume-without-the-pipeline) | Not built: Conary has no pause |
| Every option is live; needing a deploy is a declared, visible exception | [S1](#s1-flow-publish-the-configurations-schema-and-its-live-exceptions) | Flow config live; conaryd's events config read only at start, undeclared |
| Reusable modules or libraries, a machine holding only its values | Finish-line item 7 in [DESIGN](DESIGN.md#12-roadmap) | Designed |
| Pausing means vacating, and an unpause resumes (burrow#647) | [O1](#o1-flow-pause-and-hold-vacate-and-resume-without-the-pipeline) | Not built: a held PR's proof keeps its slot |
| `no-pinned-engines`, `fresh-judge` | Built (DESIGN 6.1) | Built |

One more flow change is scheduled here because [m6-plugins.md](m6-plugins.md#covered-elsewhere-not-scheduled-again)
and [review-loop.md](review-loop.md#the-decisions) hand it to #312: the
review loop's breakers from burrow#945, and the disputed-P2 switch.
That's [L1](#l1-flow-break-a-review-loop-that-wont-settle).

## Cutover prerequisites

A repository switches to conaryd only once conaryd keeps every property the
steward is being replaced for. So:

| Task | Before #313 (ownership switch) | Before #314 (remi) | Otherwise before |
| --- | --- | --- | --- |
| S1 schema and live exceptions | Yes: #313 consumes the schema | Yes | |
| S2 safe defaults | | Yes | |
| O1 pause and hold vacate | | Yes: #314 verifies pause | |
| O2 switches report their effect | | Yes | |
| A1 explicit priority | | Yes: else the binary queue is ported | |
| D1 degraded counts failed | | Yes: else silent fallbacks are ported | |
| T1 repeated deadline stalls | | Yes | |
| L1 review-loop breakers | | Yes, if burrow#945 is on burrow's main when #314 starts | #315 |
| A2 one question per decision | | | #316's extraction of the owner issue |
| T2 history-sized deadlines | | | #321's exit check |

A2 isn't a cutover prerequisite because the flow lists an owner decision
once per PR with one label, as the steward does; what A2 adds is the key
upkeep's owner listing joins on. T2 isn't one because T1 already stops a
deadline from being hit over and over; the configured `proof_timeout`
stays the ceiling until T2 lands.

## The tasks

Each task is one PR in Conary. Each says what it adds, its regression
scenario, what it waits on and what it leaves alone.

### S1: flow: publish the configuration's schema and its live exceptions

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
option listed as an exception. On main there's no schema to check.

**Waits on:** nothing. **Leaves alone:** burrow's import of the schema and
its generated file ([burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655),
[burrow#748](https://github.com/FieldmouseWorks/burrow/issues/748)); Conary
adds no write door for configuration.

### S2: flow: every spender defaults off, and no default names a machine

**Crates:** `conary-github` (`flow/types.rs`, `flow/proofs.rs`). `docs/flow.md`.

**Work.**
- Every option S1 marks as a spender defaults to off or zero: an omitted
  `proof_slots` starts no proof, the train stays off, and so on. Unlimited
  is something a machine writes down, never what an omission means.
- Defaults that name one machine's accounts, labels or engines
  (`review_authors`, `owner_label`, `judge_engines` and the like) become
  empty or neutral, so a standalone install isn't configured for remi.
  The machine's generated configuration supplies those values.
- A test reads the schema and holds every spender's default to off or zero.

**Regression scenario.** A proof plugin and a proof recipe configured,
`proof_slots` omitted, and three PRs needing that proof: main starts three
proof workers, and the fixed
flow starts none and says which setting turns them on.

**Waits on:** S1 (the `spender` mark). **Deployment:** remi's generated flow
configuration sets the values whose defaults change before it pins this,
so the shadow's decisions don't move.

### O1: flow: pause and hold vacate, and resume, without the pipeline

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

**Regression scenario.** `proof_slots = 1`, PR A's proof running and PR B
waiting. Holding A frees the slot and B's proof starts; on main A keeps the
slot. Pausing the repository cancels every worker and starts none, with
conaryd stopped; on main there's no pause. Resuming finishes A without
another review round.

**Waits on:** nothing. **Leaves alone:** per-repository landing ownership,
steward or conaryd, which is [#313](https://github.com/FieldmouseWorks/Conary/issues/313);
burrow's pause door and scoped modes
([burrow#647](https://github.com/FieldmouseWorks/burrow/issues/647),
[burrow#655](https://github.com/FieldmouseWorks/burrow/issues/655)), which
call `cn flow pause` rather than keep a copy of it.

### O2: flow: report every switch in effect and what it holds

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

**Waits on:** O1 (the pause it reports). **Leaves alone:** the owner's
effective landing owner per repository (#313 shows it).

### A1: flow: order admission and proof slots by explicit priority, then age

**Crates:** `conary-github` (`flow/types.rs`, `flow/proofs.rs`,
`flow/pass.rs`, `flow/train.rs`). `docs/flow.md`.

**Work.**
- A PR's priority is one of the levels `high`, `medium` and `low`, the
  levels burrow's drain uses ([burrow#652](https://github.com/FieldmouseWorks/burrow/issues/652)),
  read from the labels a setting maps to each level (`priority: high` and so
  on, with `first` mapped to the top level on a machine that keeps it). A PR
  with none takes the configured default level.
- Proof slots, train admission and a targeted kick's view of the others
  order by level, then age, then PR number. `first_label` and its binary
  ordering are deleted in the same PR.
- The PR's level is in its Next, and dry runs reserve virtual slots in the
  same order.

**Regression scenario.** One proof slot and three waiting PRs: an old
unlabelled one, a newer `priority: high` one and a newest `priority: low`
one. The high one goes first, then the old one, then the low one. On main
the `priority: high` label is ignored and the oldest goes first.

**Waits on:** nothing. It touches the slot ordering
[C3](m6-plugins.md#c3-flow-prepare-proofs-outside-the-proof-slots-and-send-no-model-run-check-to-a-proof-plugin)
keeps; whichever lands second rebases onto the other. **Deployment:** remi's
configuration maps its labels before it pins this.

### D1: ledger and flow: record a degraded outcome and count it failed

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
ledger, and once the record reads again nothing shows it happened. The fixed flow appends one degraded op, the check counts failed,
and status names it. A second scenario does the same for an evidence
lookup that can't tell.

**Waits on:** nothing. It states the op in DESIGN 4.6 in the same PR, after
#305 states the property.

### T1: flow: a second deadline on the same tree is a stall, not a retry

**Crates:** `conary-github` (`flow/decide.rs`, `flow/state.rs`,
`flow/jobs.rs`). `docs/flow.md`.

**Work.**
- The flow remembers, per check and tree, how many times its invocation
  reached its deadline. The second time on the same tree, the flow doesn't
  start it again: the PR is overdue with a reason that names the check, the
  tree and both deadlines, and it goes to the owner's listing.
- A new tree, or a changed recipe, starts the count again. A deadline hit
  once still retries as today.

**Regression scenario.** A proof plugin that never answers: after its first
deadline the flow retries; after its second on the same tree main retries
again, without end, with backoff. The fixed flow stops and reports the
stall, and no third worker starts.

**Waits on:** nothing.

### T2: flow: size a proof's deadline from its own history

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

**Waits on:** T1.

### A2: flow: ask a person once per decision

**Crates:** `conary-github` (`flow/types.rs`, `flow/state.rs`,
`flow/approval.rs`, `flow/persist.rs`). `docs/flow.md`.

**Work.**
- Each decision the flow needs a person for (an R2 approval, a judge's
  Owner verdict, an outside contribution, the owner label) has a subject
  key: its kind, repository, PR and the scope the decision binds (the
  intent fingerprint). The key is stable across heads that don't change
  that scope.
- The flow keeps one open request per key. A later wait with the same key
  joins it and adds no new listing or notice. The request lists every PR
  waiting on it, and its current approval marker.
- Joining never widens an approval: R2 still needs the marked review on the
  exact head ([person approval](flow.md#person-approval)).
- Next and `cn flow status` carry the key, so upkeep's owner listing and
  any notice joins on it.

**Regression scenario.** An R2 PR listed for the owner gets two base-merge
heads. On main each head is a new `owner_approval` marker with nothing
tying it to the first, and each owner notice is computed afresh per pass.
The fixed flow keeps one request, with one key, whose marker moves to the
current head.

**Waits on:** nothing. **Leaves alone:** the owner issue the steward's
upkeep keeps, which [#316](https://github.com/FieldmouseWorks/Conary/issues/316)
moves to Rust and which joins on this key; burrow's own question notices
([burrow#649](https://github.com/FieldmouseWorks/burrow/issues/649)).

### L1: flow: break a review loop that won't settle

**Crates:** `conary-github` (`flow/review.rs`, `flow/reviews.rs`,
`flow/decide.rs`, `flow/types.rs`). `docs/flow.md`, `docs/review-loop.md`.

**Work.** This ports the decisions burrow#945 puts on burrow's main, as
they are on the day it's written, per [review-loop.md](review-loop.md):
- the triggers (the round cap, fix growth past the configured share
  against the original diff, an answered finding raised again unchanged)
  hand the PR to the stuck role through the flow's `job` kind, with no
  further fix round, and findings on the stuck role's head go to the judge;
- the disputed-P2 switch is one `Settings` field whose default is the
  behavior on main, read by the flow alone, never by a plugin.

**Regression scenario.** Each trigger's boundary, against the simulated
GitHub: three rounds with findings hand over and two don't; growth just
past the share hands over and just under doesn't; a changed finding isn't a
repeat. On main none of them hands the PR to the stuck role.

**Waits on:** [burrow#945](https://github.com/FieldmouseWorks/burrow/issues/945)
on burrow's main, and m6-plugins.md's
[C2](m6-plugins.md#c2-flow-give-each-finding-a-thread-and-post-a-fixs-answers-on-it)
(the answer history it reads). **Leaves alone:** the owner's open question
on [burrow#939](https://github.com/FieldmouseWorks/burrow/issues/939),
which the switch's default doesn't answer; the stuck role's brief, which
is B1's.

## Order

```
S1 ──► S2
S1 ──► #313
O1 ──► O2
T1 ──► T2
burrow#945, C2 ──► L1
S1, S2, O1, O2, A1, D1, T1, (L1) ──► #314
A2 ──► #316's owner-issue extraction
T2 ──► #321
```

S1, O1, A1, D1, T1 and A2 start at once.

## Filing

Once this plan lands, each task is filed as an issue in Conary:

- The title starts with the task's ID.
- The body links its section here and says
  `Refs FieldmouseWorks/Conary#312` and `#256`.
- A cutover prerequisite's body says its PR carries the `first` label.

An issue grants no authority beyond #256's.
