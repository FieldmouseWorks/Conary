# M6, proj and the gate: one order

Three redesigns are in flight on one machine. Each has a plan of its own:

- **M6**, where conaryd runs the landing flow and the steward is deleted:
  [m6.md](m6.md), [m6-plugins.md](m6-plugins.md), tracker
  [#256](https://github.com/FieldmouseWorks/Conary/issues/256).
- **proj**, burrow's job runner and inbox:
  [proj-design.md](https://github.com/FieldmouseWorks/burrow/blob/main/docs/proj-design.md),
  tracker [burrow#974](https://github.com/FieldmouseWorks/burrow/issues/974).
- **The gate**, burrow's shared CI:
  [gate-design.md](https://github.com/FieldmouseWorks/burrow/blob/main/docs/gate-design.md),
  tracker [burrow#985](https://github.com/FieldmouseWorks/burrow/issues/985).

Each plan orders its own stages. None orders itself against the other two,
and seven of M6's tasks are burrow PRs. This page owns what no one plan can:
the order between the three, the fences between their files, the switches
that must not overlap, and what the pipeline has taught since the plans were
written. What a stage is stays with its plan and isn't restated here.

Written 2026-11-14 against Conary's main at `6f2a743`, burrow's at `fa6254e`
and remi's at `ec6b9f5`. The issues linked here own the live state.

## The rule

**Build in parallel, switch one at a time.**

- A **build** lands off by default, in shadow, or as a change no reader can
  see. Builds from different lanes land side by side.
- A **switch** is the moment something live starts deciding differently.
  One switch is open at a time on the whole machine. Each is followed by a
  settle period, in which no other switch opens and no deploy goes out that
  changes one of proj's units or what a gate run decides.

Both burrow designs already say this of themselves ("two engines swapped on
one machine in one week is how you can't tell which one broke it"). This
page makes it one calendar for all three.

## Lanes

A lane is a set of files with one task open at a time. Four of the five
already have a task list that admits its next child when the one before
lands, so a lane is that list, not a new mechanism.

| Lane | Its list | Its files |
| --- | --- | --- |
| Conary flow | The 5c tasks C1 to C4, then [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s children | `crates/conary-github/src/flow/`, `crates/conary-core/src/plugin/`, `docs/flow.md`, `docs/plugins.md` |
| Conary side | [#294](https://github.com/FieldmouseWorks/Conary/issues/294)'s children, then the Conary children of burrow#985 | `docs/lessons.toml`, `docs/DESIGN.md`, `docs/m6*.md`, the `attest` modules and the `cn` commands over them |
| burrow M6 | The 5c tasks B1 to B3, R, F, J and P, then the tasks [#316](https://github.com/FieldmouseWorks/Conary/issues/316) files | New crates `burrow-brief`, `burrow-producer`, `burrow-fix` and `burrow-judge`; `burrow-review`; `burrow-proof`; `burrow-upkeep/src/brief.rs`; `modules/ledger.nix`; the `plugins` options in `modules/autoland.nix` |
| burrow proj | [burrow#974](https://github.com/FieldmouseWorks/burrow/issues/974) | `crates/burrow-proj/`, `crates/burrow-watchdog/` at stage 3, `modules/proj.nix` from stage 4 |
| burrow gate | [burrow#985](https://github.com/FieldmouseWorks/burrow/issues/985) | `.github/workflows/flake-*.yml`, `burrow-flake-check`, `burrow-keep`, `burrow-workflows`, new `burrow-builds`, `modules/builds.nix` |

remi has no lane. It takes one configuration PR per switch, and the values
for a build that needs them (the build store's shadow).

No lane edits another lane's files. The steward's Python
(`modules/autoland/*.py`) belongs to no lane: it takes B1's one edit, and
otherwise only a fix for breakage that blocks a lane
([what M6 makes moot](#what-m6-makes-moot)).

## Fences

Where two tasks would meet, one goes first. Each fence is carried in the
body of the later task when it's filed.

1. **proj's record before the fix plugin.** Task F starts a job, waits on it
   and stops it. The steward does that today with an `--owned` job, read
   through `proj lifecycle`'s lines. proj's design deletes both with the
   steward, and gives callers the job's record instead. So
   [burrow#969](https://github.com/FieldmouseWorks/burrow/issues/969) (one
   record per job) lands before F is written, or F is built twice. The same
   holds for J if its task runs the judge's engine as a job.
2. **One new crate at a time.** B1, B2, F, J and burrow#986 each add a
   member to burrow's workspace, so each edits `crates/Cargo.toml`,
   `Cargo.lock` and the flake's crate list. One such PR is open at a time,
   in this order: B2, B1, burrow#986, F, J. Renovate's lock-file PRs in
   burrow stay held while that queue has an entry.
3. **`modules/ledger.nix` has two tasks.** B3 (the controller user) goes
   first. Then [burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724)
   (the flow's configuration rendered from the settings source, PR
   burrow#785), which [#313](https://github.com/FieldmouseWorks/Conary/issues/313)
   consumes.
4. **The review brief has two tasks.**
   [burrow#958](https://github.com/FieldmouseWorks/burrow/issues/958) edits
   `brief.rs` and B1 moves it. burrow#958 goes first, so the text moves once.
5. **The flow's files have four tasks.** C1 to C4 all touch
   `flow/execute.rs` or `docs/flow.md`. They run one at a time: C1, C4 (J
   needs it), C2, C3. The tasks [#312](https://github.com/FieldmouseWorks/Conary/issues/312)
   files come after them, then #313.
6. **#312 waits for #307.** #256's list admits #312 as soon as #311's PR has
   landed, but #312 is written against the lesson IDs that #305, #306 and
   #307 deliver. It stays `later` until #307 lands.
7. **The inbox's files have two tasks.** The fix for a list that stalls on a
   child landed elsewhere
   ([burrow#1110](https://github.com/FieldmouseWorks/burrow/issues/1110))
   and [burrow#975](https://github.com/FieldmouseWorks/burrow/issues/975)
   (one record per issue) are both in `crates/burrow-proj/src/inbox/`. The
   fix goes first: the lists in the table above run on it.
8. **A child whose PR is in another repo says so.** burrow#993, burrow#739
   and burrow#995 are children of burrow's list delivered by a Conary PR,
   and burrow#994 by a remi PR. Each PR's body closes its child by its full
   name (`Closes FieldmouseWorks/burrow#993`), or the list never sees it
   land.
9. **The proof plugin and the gate meet late.** Task P builds a proof with
   `burrow-proof`, as the steward does. The gate's last stage
   ([burrow#998](https://github.com/FieldmouseWorks/burrow/issues/998)) has
   the proof call the gate's program instead. That stage is after the
   steward is deleted, so P is written once now and changed once then.
10. **Two green PRs can make a red trunk.** On 2026-11-14 burrow's main went
    red on two PRs that each passed alone, and the repair waited for hours.
    Lanes landing side by side make that likelier, and the merge train is
    the fix. burrow can't ride one yet
    ([remi#1077](https://github.com/FieldmouseWorks/remi/issues/1077)): its
    train run skips the `agents` check, the train counts a skipped check
    only under a signed machine proof of the same candidate
    ([#218](https://github.com/FieldmouseWorks/Conary/issues/218)), and
    nothing signs one until task P and its producer are live with their
    key. burrow's own module also refuses a train beside `proofBuilds`. So
    burrow joins its train as the first step of its part of S2, with that
    refusal lifted in the same change, and no earlier. Until then three
    things stand in for it. Fences 2 and 11 keep the PRs in flight few. A
    burrow PR that reaches past its lane's own files (the lock file,
    `flake.nix`, `checks/`, a test that reads the whole workspace) merges
    main in before its last review round, so its checks ran on the trunk
    it lands on. And main's own run is the backstop: if it goes red on two
    PRs together a second time before S2, burrow drops to one PR out of
    `hold` at a time until its train is on.
11. **Runners are shared.** A burrow head starts about fifteen hosted jobs.
    At most two burrow PRs are in CI at once, with Conary's beside them. M6
    and proj carry `first`; the gate's lane yields.
12. **A stage is done when it's live.** A burrow merge reaches the machine
    only when remi's pin moves and deploys. A task whose successor reads
    what the machine runs (anything in shadow, anything a switch waits on)
    counts from its deploy, not its merge.

## Order

### Before anything is built

1. **burrow's door.** burrow's automated work is paused, and a paused repo
   starts no review, proof or job. A PR handed over with `settings land`
   gets its merge and nothing it still needs, so only a PR that was already
   reviewed and proven can use that door. Nothing new can land there. The
   owner's call (2026-11-14) is "tracks only": every open burrow PR and
   ready issue outside the three lists is held (`hold` on a PR, `later` on
   an issue), the jobs parked for them are stopped, the labels that were on
   are written to an undo list, and then burrow is allowed. One
   `settings pause` puts it back. It opens in two moves. The first, done
   on 2026-11-14, held every burrow PR but this plan's own (burrow#1113),
   the tracks' PRs included, so the plan could be reviewed before anything
   was built on it. The second, here, takes `hold` off the tracks' PRs and
   `later` off their issues, a lane at a time and inside the fences. The
   holds outlast the hold watchdog's usual patience, so remi#1290
   lengthened it; that change is reverted when the last hold comes off.
2. **The stalled lists.** #294's list waits on its closed child #304, and
   [#291](https://github.com/FieldmouseWorks/Conary/issues/291)'s on #322,
   whose work landed in burrow. Both are released through the author's door
   (`proj plan done`). Fence 7's fix stops it happening again.
3. **The eleven 5c tasks are filed**, as [m6-plugins.md](m6-plugins.md#filing)
   says, each with its fences from this page.
4. **In-flight burrow PRs in a lane's files.** One that's settled (review
   clean, CI green) lands now. One that isn't is held until the stage that
   rewrites its ground has landed, then rebased or closed. Four go first
   whatever their state: burrow#1104 (the gate's wait gets a deadline), the
   open PRs for proj's stage 0, and burrow#1053 (a job waiting on CI wakes
   only on its verdict).

### Builds, by lane

Each lane runs top to bottom. The edges between lanes are the fences above
and the dependencies in [m6-plugins.md](m6-plugins.md#order).

| Conary flow | Conary side | burrow M6 | burrow proj | burrow gate |
| --- | --- | --- | --- | --- |
| C1 | #305 | B2 | Stage 0: burrow#970, #943, #1110 | burrow#1104 |
| C4 | burrow#993 | burrow#958, then B1 | burrow#969 | burrow#986 (after burrow#993) |
| C2 | #306 | B3, then burrow#724 | burrow#975 | burrow#988 |
| C3 | #307 | R | burrow#972 | burrow#994 (remi's values, shadow on) |
| #312's tasks | #312 (the plan) | P | burrow#971 | |
| #313 | | F (after burrow#969) | | |
| #319 | | J | | |

Everything in this table is a build. Every lane then stops at a switch.

### Switches

A switch opens when its row's conditions hold and the row above has
settled. A switch has settled when its owner's measured acceptance is met
([m6.md](m6.md#what-remains) has that column). Where the owner names none,
it's two days of normal work with nothing traced to the switch.

| | Switch | Owner | Opens when |
| --- | --- | --- | --- |
| S1 | remi lands through conaryd | [#314](https://github.com/FieldmouseWorks/Conary/issues/314) | Every 5c task is live on remi, #313 has landed, the producers' keys are provisioned, and the shadow has agreed for two days with every disagreement accounted for |
| S2 | Every other repository, burrow among them, a group at a time | [#315](https://github.com/FieldmouseWorks/Conary/issues/315) and the tasks it files | S1 has settled. burrow's part starts by turning its train on (fence 10) |
| S3 | Hosted runs substitute from the build store, and the kept builds go | [burrow#989](https://github.com/FieldmouseWorks/burrow/issues/989) | burrow's part of S2 has settled, and the store's shadow has twenty good reports |
| S4 | The steward's Python is deleted | The tasks #316 files | Every repository lands through conaryd |
| S5 | One plan a run | [burrow#990](https://github.com/FieldmouseWorks/burrow/issues/990) | S3 has settled |
| S6 | proj's one pass | [burrow#976](https://github.com/FieldmouseWorks/burrow/issues/976), split by burrow#978 | S2 has settled, and the pass's own shadow has been quiet |

S3 goes before S6 because it's the cheaper one to undo: it touches hosted
CI only, and a run that can't reach the store builds cold. S6 replaces the
scheduler on the machine.

After these, each of the following is a switch of its own, in its plan's
order: proj's stages 5 to 8, the gate's stages 3 to 5, and M6's slices 6 to
8 (#317, #318, #320), then #321's measured week. Stages that wait for the
steward to be gone (proj's 8, the gate's 5) wait for S4.

No switch has a date. On the day this was written the eleven 5c tasks
weren't filed and burrow was shut, so S1 was not days away.

## What M6 makes moot

Work on any of these before the steward is deleted is work done twice.

- **The steward's own code.** A burrow issue that changes the steward's
  step, pass, stuck list or review loop is fixed only when the breakage
  blocks a lane. Otherwise it closes with S4. Its lesson goes in
  [lessons.toml](lessons.toml) and its policy stays where
  [review-loop.md](review-loop.md) puts it.
- **What proj keeps for the steward.** `proj lifecycle`'s lines, `--owned`
  jobs and the kicks proj sends the steward go at proj's stage 8.
- **What the gate keeps for the steward.** `build-skip`, and the check-run
  names the steward parses, go at the gate's stages 4 and 5.
- **Settings the steward reads.** A judge's and a reviewer's engine, model
  and effort (burrow#825, burrow#826) arrive in the plugin request's
  `execution`, so tasks J and R deliver them. The review limits and judge
  triggers (burrow#748) are the flow's settings and its `why`.
- **Model-run proofs.** wend's proof job (burrow#646) is C3's and #315's.
- **`needs-peter` on a PR.** At S1 an owner call is a GitHub review that
  carries the flow's marker ([flow.md](flow.md#person-approval)), not a
  label someone takes off.

## What the pipeline taught since

Each row is a failure seen after the three plans were written, or one they
didn't reckon with. "By design" means the plan already ends the class. A
"gap" is closed by an amendment: to burrow's designs in
[burrow#1113](https://github.com/FieldmouseWorks/burrow/pull/1113), or as an
open entry in [lessons.toml](lessons.toml), which
[#306](https://github.com/FieldmouseWorks/Conary/issues/306)'s check won't
let a milestone ship past.

| What happened | Seen in | Where it ends |
| --- | --- | --- |
| A repair waited on the thing it repaired: the fix for a red trunk sat behind the red trunk's own hung run | burrow#1102, burrow#1108 | The gate, by design at S3: no run waits on another. Gap: a leg could still hang for hours; amended, a leg fails and never hangs. For the flow, lesson `repair-ahead-of-red`, owed by #318 |
| A list stalled on a child whose work landed in another repo or another issue | burrow#1108, burrow#1110 | proj's `Plan` wait. Gap: it didn't say what a landing is; amended, and its fix pulled forward to stage 0 (fence 7) |
| The owner was asked for calls that aren't one-way doors | remi#1241, burrow#1108 | proj's `Answer` wait. Gap: it didn't say who may answer; amended. For the flow, lesson `owner-only-for-doors`, owed by #312 |
| Jobs spent model turns finding CI still running, and a slow runner queue read as a stall | remi#1238, burrow#1043 | proj's wait on a PR. Gap: it ended on any change; amended to end on a named condition, with queue time left out of the deadline. For the flow, lesson `waiting-costs-nothing`, owed by #312 |
| The stuck list re-posted itself and woke a job each time | remi#1238, burrow#1046 | Lesson `publish-only-news`, owed by #316, which moves that list to Rust |
| One oversized or refused read of GitHub failed a whole pass | burrow#1040, burrow#1103 | proj, by design: a refused read leaves the old facts and delays only what needs them. For the flow, lesson `read-sized-to-data`, owed by #312 |
| A cancelled check GitHub won't rerun was retried anyway, and the status guessed at the cause | burrow#1062 | Lesson `recover-by-kind`, owed by #312. Not proj's or the gate's |
| Hosted runners filled, and each burrow head joined the queue many times | burrow#798, burrow#937 | The gate, by design: legs by cost at S5, nothing rebuilt cold at S3. Gap: nothing recorded queue time; amended, the result file does. Until S3, fence 11 |
| Two green PRs made a red trunk | remi#1078 | The train, by design. burrow can't ride it before its part of S2; fence 10 says why, and what stands in until then |
| A pause couldn't be scoped below a repository, and a PR handed through it got no review | burrow#1070, step 1 above | proj's `Policy` wait. Gap: a rule named only a repo; amended with a focus. For the flow, lesson `scope-below-a-repo`, owed by #313 |
| A fix that had merged wasn't live while the fault it fixed kept costing | burrow#957 | Fence 12. How a pin bump travels is its own redesign, not started and not this page's |
| A list knew only "the step before it", not a fence | Fence 6, and S3 and S6 above | proj, by design at stage 7: a dependency is a wait on a record. Until then, whoever runs this plan keeps `later` on a fenced child |

Out of scope for all three plans: queued runs GitHub never gave a job
(harmless), remote sessions going stale, and standing-authority commands
that still ask for the owner's words
([remi#1242](https://github.com/FieldmouseWorks/remi/issues/1242)). This
plan doesn't lean on any of them: every step in it goes through labels or
the settings door.

Two checks ride on M6's tasks without growing them. When R and J land,
does the machine's usage total see a producer's engine session? And does a
pause stop a producer's work? The second should fall out of the kit's
connection-scoped work once the flow has a pause of its own. If either
answer is no, it's one follow-up issue, not a wider task.

## What needs the owner

- The door on burrow (step 1). Decided 2026-11-14.
- `proj plan done` for the two stalled lists (step 2), until fence 7's fix
  is live.
- The producers' private keys, before S1
  ([m6.md](m6.md#boundaries)). One sitting, when S1's other conditions hold.

Nothing else here waits on a person.
