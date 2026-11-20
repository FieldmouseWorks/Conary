# M6: the steward's upkeep to Rust, then the Python deleted

This is [#316](https://github.com/FieldmouseWorks/Conary/issues/316)'s plan.
It covers the steward's Python in burrow: about 7,100 lines in
`modules/autoland/*.py` and `modules/updates/major-seal.py`, plus 13,000
lines of tests in `tests/autoland/`. For each thing that Python does, the
plan names one owner. Work outside the landing flow moves into burrow's
existing Rust crates, `burrow-upkeep` and `burrow-renovate`. Each move is a
bounded one-PR task with parity tests. Landing decisions are Conary's flow,
already built or owned by an issue, and none is ported back into burrow. The
plan ends with the deletion PR, S4 in [m6-order.md](m6-order.md#switches).

The inventory was taken on 2026-11-20 from burrow's main at `143cd3e`,
Conary's at `350d051`, remi's at `a416a33`, and the configuration deployed
on remi (`/etc/burrow/autoland.json`, `proj settings show`). The issues
linked here own the live state.

## Where it stands

- **remi lands through conaryd.** The live setting is
  `settings.landing {"fieldmouseworks/remi": "conaryd"}`, and
  [#314](https://github.com/FieldmouseWorks/Conary/issues/314) is still
  open. Every other repository is the steward's until
  [#315](https://github.com/FieldmouseWorks/Conary/issues/315)'s groups
  switch.
- **The steward still runs upkeep for repos it doesn't land.** For a repo
  conaryd holds, a pass restarts the repo when it was remade under a deleted
  repo's name, settles its settings and hook, starts its baseline job, and
  runs upkeep. Then it stops (`steward.py:324-332`). That leaves gaps for
  such a repo, remi among them today:
  - its branches aren't cleaned;
  - a red default branch isn't watched;
  - no PR stall reaches the watchdog;
  - renovate-sweep reads it as uncovered.
- **Rust already does most of the work. The Python decides when.**
  `burrow-upkeep` has a subcommand for each piece: `token`, `settle`,
  `hooks`, `branches`, `watch`, `claims`, `fetch`, `commits`, `ci` and
  `verdict`. The steward's pass is the only caller of every one of them
  (`common.upkeep`). So most extraction tasks below add a driver and delete
  Python. They don't write a second implementation.

## The gate

The final deletion (task D) waits on all of the following:

- **Every repository lands through conaryd.** #315 inventories the
  repositories and files their cutovers. D's own check is mechanical:
  `settings.landing` names `conaryd` for every repository the steward's
  discovery returns, and no handover is under way.
- **Red-trunk recovery is live.** A red default branch is recognised and
  repaired today only by the steward's `watch_default` (`repos.py:176-523`),
  and only in repos the steward lands. DESIGN gives it to Conary
  ([what lives where](https://github.com/FieldmouseWorks/Conary/issues/256)),
  owned by [#318](https://github.com/FieldmouseWorks/Conary/issues/318), and
  it isn't built. It isn't ported to Rust here. Until #318's recovery is
  live, a conaryd repo whose default branch goes red is caught by its own
  train, if it has one, and by its main-push CI. Nothing repairs it, so D
  doesn't delete the only repair. remi is in that gap now.
- **Every task below has landed and is live**
  ([fence 12](m6-order.md#fences)).

## Inventory

Each row ends in one owner: **Rust** (already done; the Python is deleted
with D), a **task** filed here, an **issue** that already owns it, or
**dropped**, with the reason. "Flow" means conaryd's flow decides it. The
third column names the Conary file, or the issue for a gap.

### Landing flow: Conary's, deleted with D

| Python | What it does | Owner |
| --- | --- | --- |
| `step.py`, `step_actions.py`, `step_context.py`, `step_jobs.py`, `dependency_step.py`, `aside.py`, `stages.py` | Each PR's step: pause, owner label, CI, conflicts, the dependency route, review, fix, judge, proofs and merge | Flow: `flow/decide.rs`, `flow/execute.rs`. The gaps are already owned: FP8 (CI recovery), FP11 and [SJ](m6-properties.md#sj-burrow-serve-the-stuck-role-through-the-job-kind) (lost jobs and loops), #315 (`proofJobs`). The fail and misdescribed verdict handoffs wait ([flow.md](flow.md)) |
| `prs.py` | PR reads, CI state, the status and stage label, the merge | Flow: `flow/read.rs`, `flow/ci.rs`, `api/flow_write.rs`. The CI rerun is FP8's |
| `touches.py`, `review_step.py`, `reviewers.py` | Gate edits, dependency kind, bot provenance; review asks through the owner's door | Flow: `flow/gates.rs`, `flow/history.rs`, `flow/review.rs`, `flow/ask.rs`. The door itself is `burrow-review-ask`, and it stays |
| `tasks.py` | Fix, judge, proof and red-main briefs; v1 verdict markers | Flow, with `burrow-brief`, `burrow-fix` and `burrow-judge`. The red-main brief goes to #318. The v1 markers are deleted with D |
| `train.py` | `cn class`, train join, leave and step | Flow: `flow/train.rs`. Its token command is task U1's |
| `proofs.py`, `proof_state.py` | Proof slots, prebuilds, `autoland-steward build` and `realise` units | Flow, with the `autoland-proof` producer (`proof.nix`). proj's `proofBuildDriver` (`proj.nix:85`, `policy_proofs.rs`) is deleted with D |
| `batch.py` | Time slices, cursors, continuations, coverage | Dropped: conaryd schedules its own passes, and FP9 owns a slow repository. burrow#1103 closes with D. Coverage is task N3's |
| `claims.py`, `stopping.py`, `mirror.py` | Fix-job push identity, stopping stale work, the steward's mirror | Flow, with `burrow-fix`'s broker. The `claims`, `fetch` and `commits` subcommands go with D |
| `landing.py`, `admission.py` (`held`, `handed`, `spend*`, `landing`) | Handing a repo over between the steward and conaryd; the steward's landing gate | Dropped with D. Once no repository is the steward's, there's nothing to hand over. See [the way back](#the-way-back) |
| `admission.py` (`decision`, `paused`) | Reads the admission decision and the pause | Rust: `burrow-settings decision` and `allows`, and proj's own check at slot time. Only glue is Python |
| `common.py`'s verdict seal and markers (`seal`, `_key`, `marker`, `quote`, `fingerprint`) | The judge's v1 seal, and reading the v1 major and judgment markers | Dropped with D. The judge seals v3 (`burrow-judge`). Major verdicts are task N1's |
| `steward.py`'s pass (`run_pass`, `main`, `kick`, `continue`, `status`) | One pass over every repo, and `autoland-steward status` | Flow: conaryd's passes, and `cn flow status` for the status. Each piece of upkeep goes to the task in the next table |

### Outside the landing flow: moved to Rust

| Python | What it does | Owner |
| --- | --- | --- |
| `steward.py token`, `common.token` | Prints the App's token. conaryd's `token_command` (`autoland.nix:684`, `landing.nix:56`), the shadow (`shadow.nix:136`) and `cn` in the train (`train.py:69`) all call it | **U1** |
| `repos.discover` | Lists each owner's repos (non-fork, non-archived, minus `exclude`) with their id, privacy, default branch and creation time | **U2**. house-sync's copy of the query (`house-sync/src/github.rs`) moves into `burrow-github`, so there's one |
| `repos.settle_repo`, `repos.hooks`, `steward.upkeep` (dashboard and flaky jobs), `common.come_back`, `after_limit`, `wake` for these | Drives `settle`, `hooks` and `watch`, starts the jobs `watch` asks for, and books the next look | **U2** |
| `waits.py`'s `failed` and `passed` for a repo's operations | A repo's failed upkeep goes to the stall watchdog | **U2**: a watchdog source of its own |
| `repos.restart`, `reset`, `retire_checkouts`; `common.follow_renames`, `retiring`, `kept_state`, `replaced`; `steward.retire_deleted`; the baseline job (`known.json`, `baseline-failed.json`); `common.taken` | A new repo's baseline job; a renamed repo's clones; a repo remade under a deleted repo's name, retired before anything acts on it | **U3** |
| `steward.branches` | Deletes the branches nothing needs, given every open PR's head and base | **U4**. It runs for every repo, which closes today's gap for conaryd repos |
| `repos.publish`, `issue`, `known_by`, `stuck_brief`, `inbox_off`, `job_questions`; `waits.escalated` | The owner issue ("PRs waiting for you") and the stuck issue ("Stuck in the pipeline") | **U5**, held to `publish-only-news` |
| Nothing (a gap) | conaryd's overdue PRs and owner notices reach neither the watchdog nor those issues | **C1** exports them; **U6** publishes them |
| `waits.note`, `overdue`, `read`, `publish` (`watch.json`) for PRs; hold stalls (`holdStallAfter`) | Each steward PR's wait and deadline, for the watchdog | Flow: FP7, FP8, FP10 (a hold past `hold_stall_after` is a stall). U6 carries them to the watchdog. The steward's own `watch.json` and its `autoland` source go with D |
| `modules/updates/major-seal.py`, `seal.py` (`key`, `mac`, `major`) | Signs and checks renovate-sweep's verdict on a major | **N1**. N1 also writes the v2 form Conary reads |
| `lock_wait.py` as the `lock-wait` program (`when`, `ripe`, `--limits`) | When a refresh Renovate refused for release age can go again, and the sweep's limits | **N2** |
| `lock_wait.py` as a library (`requested`, `refresh`, `interval`, `drain`, `recovery`, `guard`) | The steward's lock-file wait and its watchdog grace | Flow: `flow/gates.rs`, `flow/state.rs`. Deleted with D. Conary leaves out `interval`'s `+20` seconds: **C2** |
| `common.coverage`, `machine` (`repos.json`) | Tells renovate-sweep which repos have their PRs seen to, and how their proofs run | **N3** |
| `common.job`, `jobs`, `stop_job`, `lifecycle`, `at_work`, `forget_issues` | Starting, reading and stopping proj jobs | Rust already does this: `burrow-upkeep`'s `pass::Proj` (`proj run`, `find`, `stop`). U2 and U3 use it. Reading `proj lifecycle` lines and `--owned` jobs go with D, then proj's stage 8 |
| `common.gh`, `graphql`, `remote`, `limited`, `pass_deadline`, `load`, `save` | Transport, deadlines, state files | Rust already does this: `burrow-github`, `burrow-upkeep::pass` |
| `common.fresh`, `compare`, `rest_ours`, `rank`, `GIT_AUTH` | Nothing calls them | Dropped with D |

### Services, options and other callers

| What | Who uses it | Owner |
| --- | --- | --- |
| Units `autoland`, `autoland@`, `autoland-continue`, `autoland-continue@`; the `autoland` and `autoland-continue` event handlers; the transient `autoland-proof-*` and `autoland-ask-*` units the Python starts | The steward | Deleted with D. The `autoland-ask-*` door launcher stays, because conaryd's `review_ask` uses it |
| `autoland-shadow@` and its `OnSuccess` | Runs `cn flow compare` after the steward's pass | Deleted with D. There's nothing left to compare |
| `burrow.watchdog.sources.autoland` (`watch.json`), `stewardKick = "autoland"` (`watchdog.nix:38`), `escalated.json` | Stall watchdog | U2 adds the `upkeep` source. U5 reads `escalated.json`, and the watchdog kicks U5's publisher instead of the steward. D deletes the `autoland` source |
| `autoland-steward` package, `/etc/burrow/autoland.json`, `stewardSrc`, `stewardImports`, `checks/autoland*.nix`, `tests/autoland/` | The steward and its checks | Deleted with D. Each U and N task ports the cases it needs as Rust parity tests first |
| `proj.nix`'s `proofBuildDriver` and `proofsState`; `burrow-proj/src/policy_proofs.rs` | proj resumes a paused steward proof build with `steward.py build` | Deleted with D |
| `crates/lock-age/src/lib.rs:256` (`include_str!` of `lock_wait.py`), the flake shipping `modules/autoland` to that build (`flake.nix:101-123`) | lock-age's test pins its words to the steward's reader | N2 points it at the Rust reader |
| `modules/updates.nix`'s `majorSealSrc`, `lockWaitSrc`, `tools.seal`, `tools.lockWait`; `tests/renovate-sweep.py`, `tests/renovate-checks.nix` | renovate-sweep | N1 and N2 |
| remi's `checks.undo` (`flake.nix:147`), which greps `repos.py` for `cn undo` | remi's gate | Deleted in D's paired remi PR. #318 owns red-trunk undo then |
| `probe.nix`'s `autoland*.service` globs; `autoland.nix`'s `machineMd` (`autoland-steward status`); `docs/front-door.md`, `docs/repo-baseline.md`, `docs/landing.md` | Docs and probes | D changes each to say what replaced it |
| Options `baselineJobs`, `baselinePerPass`, `baselineTask` | `steward.py:310` | U3 moves them to the upkeep config, under the same names, through the settings source |
| Options `train*`, `proof*`, `shadow`, `flow`, `labels`, `repos.*` | The steward, and the flow configuration `shadow.nix` renders | They stay: conaryd's configuration is rendered from them (burrow#724). D deletes only `dryRun` and `continuationKick`, which only the steward reads |
| Conary's legacy judgment readers ([#303](https://github.com/FieldmouseWorks/Conary/issues/303), #339): `legacy_word` and the v1 `autoland-judgment` reader in `flow/markers.rs`, their tests (`sim/tests/flow_scope.rs`, `flow_findings.rs`, `persist/tests.rs`), and `docs/plugins.md`'s sentence on them | Read the steward's old verdicts | Deleted in D's Conary PR. `SealKey::seal` stays, because `issued_tag` uses it |

`burrow-upkeep`'s `verdict` and `ci` modules stay as libraries:
`burrow-review-ask` and `burrow-workflows` link them. D deletes only the
subcommands nothing runs any more.

## Found on the way

None of these is a new extraction task.

- **conaryd repos have gaps today: majors, stalls, coverage, branches.**
  Four rows above are live for remi:
  - renovate-sweep's major verdicts are v1, and Conary reads only
    `major-verdict:v2` (`flow/markers.rs:248`). No `major_key` is
    configured.
  - No overdue PR reaches the watchdog.
  - The sweep reads remi as uncovered.
  - remi's branches aren't cleaned.

  N1, N3, U4, and U6 with C1 close them. Each goes as early in its lane as
  what it builds on allows.
- **Conary hard-codes the stage label names, and the owner's name with
  them.** `Stage::label` in `flow/write.rs` has `"waiting: Peter"`. burrow
  names that label `waiting: ${owner}` (`labels.nix`). That's a machine
  value in a library (`reusable-modules`). **C3** takes it.
- **Conary's lock-file grace is 140 seconds short.** C2 says why.

## The tasks

Every task is one PR, in burrow unless it says otherwise, and every one
carries the `first` label. Each one:

- ports the Python tests that cover what it moves as Rust tests, named in
  its issue, and runs them against the Rust;
- shows in its PR that each ported test fails when the moved code is broken;
- switches what it moves on in the same PR, and deletes the Python or nix
  wiring it replaced.

**Python edits.** A task may edit the steward's Python only to delete a call
it replaces, or to point one at the Rust. That's the one exception to
[m6-order.md](m6-order.md#lanes)'s rule that the steward's Python takes no
edits: deleting a call ports nothing twice, and it keeps each move small
enough to roll back on its own. The Python the landing flow still imports
stays until D.

**Lanes.** The tasks run in two new lanes, each one task at a time:

- **burrow upkeep** (`crates/burrow-upkeep`, except what the burrow M6 lane
  owns, and the upkeep wiring in `modules/autoland.nix`): U1, U5, U2, U4,
  U3, U6. U5 goes early because it ends a live cost: the stuck issue's
  comments wake a job each time.
- **burrow renovate** (`crates/burrow-renovate`, `crates/lock-age`,
  `modules/updates.nix`, `modules/updates/`): N1, N3, N2.

C1 to C3 join the Conary flow lane.

**Switches.** Each deploy that carries a U or N task is a switch on
[the calendar](m6-order.md#switches). It's a small one: it moves no landing
decision. Before it merges, its PR shows the new code's `--dry` output
against the live repos, matching what the steward last did. Its measured
acceptance is one day of normal work with no stall traced to it. These
switches go in the gaps between S1, S2's groups and S4. Each lands as soon
as one opens; none waits on S2.

**Start now: every task but U6 and D.** None of them is in the landing
path, and none waits on a cutover. U6 waits on C1 and U5. D waits on
[the gate](#the-gate).

### U1: upkeep: the App's token for conaryd and cn, without the steward

Issue: [burrow#1301](https://github.com/FieldmouseWorks/burrow/issues/1301).

`burrow-upkeep` gains a token command for `CONARY_TOKEN_COMMAND`:
`token OWNER/REPO --bare`. It prints the bare token, mints it as `token`
does, and exits 75 when GitHub can't be asked. The flow's `token`
(`autoland.nix:684`), the shadow's (`shadow.nix:136`) and `train.py`'s `cn`
call all point at it. `steward.py token` is then deleted.

**Parity:** a test that the bare token is the one `token` mints, and one that
exit 75 passes through. The test that `cn` fetches a token through the new
command comes from `tests/autoland/app.py`.

### U2: upkeep: its own pass over every repo (discovery, settings, hooks, Renovate dashboard and flaky jobs)

Issue: [burrow#1303](https://github.com/FieldmouseWorks/burrow/issues/1303).

`burrow-upkeep CONFIG pass [OWNER/REPO]`:

- discovers the repos with the one query in `burrow-github`, house-sync's
  moved there with `databaseId`, `isPrivate`, `createdAt` and empty repos;
- runs `settle` and `watch` on each, and `hooks` at the end of a full pass,
  with the full discovered list as `seen`;
- starts the jobs `watch` asks for through `pass::Proj`.

It's started by its own `burrow.events` handler (`upkeep`), on `repository`
events, the dashboard's and flaky tests' `issues` events, and its own
wake-ups. It has its own state file and its own watchdog source, `upkeep`,
for a repo whose upkeep fails. A rate limit defers it through
`burrow-github`'s `deferred`.

The steward's calls to `settle`, `watch` and `hooks` are deleted. The
steward keeps `discover` for its own landing pass until D.

**Parity:** the settle, hook and upkeep cases of `tests/autoland/upkeeping.py`
and `app.py`, `limits.py`'s rate-limit case, and `passes.py`'s cases for
kicks and wake-ups.

**Fences:** it adds a handler to `modules/events.nix`, so it lands after any
open burrow proj-lane PR that edits that file.

### U3: upkeep: a repo's lifecycle (baseline job, renames, a repo remade under a deleted one's name)

Issue: [burrow#1305](https://github.com/FieldmouseWorks/burrow/issues/1305).

U2's pass takes over:

- `known.json`, `baseline-failed.json` and the baseline job (the
  `baselineJobs`, `baselinePerPass` and `baselineTask` options move to the
  upkeep config);
- `follow_renames`;
- `restart`, `reset` and `retire_checkouts` (clones moved aside,
  `proj-inbox forget`, the old repo's jobs stopped);
- `taken.json`;
- `retiring.json`.

Until D, the steward refuses to start a job in a repo `retiring.json` names,
through the same file as today, so nothing acts on a remade repo first.

**Parity:** `tests/autoland/upkeep.py`, `clones.py`, and `paused.py`'s
baseline case.

### U4: upkeep: delete the branches nothing needs, in every repo

Issue: [burrow#1304](https://github.com/FieldmouseWorks/burrow/issues/1304).

U2's pass runs `branches` itself. It reads every open PR's head and base with
one paged REST list, forks' and drafts' PRs included, as `in_use`. That means
conaryd's repos get it too. The steward's `branches` call and its `in_use`
bookkeeping are deleted.

**Parity:** `upkeeping.py`'s branch cases, and a new case: a conaryd-held
repo's merged branch is deleted. It fails on main.

### U5: upkeep: the owner and stuck issues, kept in place, notifying only for news

Issue: [burrow#1302](https://github.com/FieldmouseWorks/burrow/issues/1302).

`burrow-upkeep publish` keeps both issues in `notify.repo`. It reads:

- the steward's `digest-repos.json`, which the steward still writes until D;
- the watchdog's `escalated.json`;
- `proj _questions`;
- the upkeep pass's own lines;
- U6's lines, once U6 lands.

It runs when one of those inputs changes, through a path unit, not a timer.
The watchdog's `stewardKick` points at it.

It follows `publish-only-news`. This is the only deliberate change from
parity:

- **The stuck issue's body is edited in place.** It holds the whole current
  list. Today's stuck issue is the opposite: its body never changes, and
  every change posts the whole list as a new comment. That was done because
  proj's inbox can't trust a body whose edit history GitHub has pruned
  (burrow `e524bfd`). U5 removes that need:
  - the stuck issue's job reads the current list from the machine
    (`burrow-upkeep publish --show`), never from the body or a comment;
  - the inbox doesn't wake on the issue's edits.
- **A comment goes up only for a new item someone can act on.** It names
  that item alone. A cleared item, a reworded item, or a PR moving within
  its stage edits the body and posts nothing.
- **Items are keyed by failure, not by PR.** Two PRs stuck on the same
  failing check are one item that names both
  ([burrow#1244](https://github.com/FieldmouseWorks/burrow/issues/1244)'s
  substance).
- **Everything else matches today:**
  - the owner issue is assigned to the owner;
  - with no inbox, everything goes to the owner issue;
  - an empty list closes the issue;
  - nothing is recorded as published until every write succeeded.

The steward's `publish` and `escalated` are deleted.

**Parity:** `tests/autoland/digest.py`. New cases, each failing on main:

- clearing an item posts nothing;
- a new item posts one comment naming only that item;
- two PRs with one failure make one item;
- the job's brief carries the list from the machine.

**Closes:** burrow#1046 and burrow#1244. Its PR names each in full, under
[fence 8](m6-order.md#fences).

### U6: upkeep: conaryd's stalls and owner notices, to the watchdog and the two issues

Issue: [burrow#1306](https://github.com/FieldmouseWorks/burrow/issues/1306).

**Waits on:** C1 and U5.

For each repo conaryd holds, U2's pass reads C1's export:

- each overdue PR becomes an item in the `upkeep` watchdog source, with
  conaryd's kick for the repo;
- each owner notice becomes a line for U5, joined on FP10's subject key once
  FP10 has landed.

That replaces `waits.publish` for the repos the steward no longer lands. It
is "the caller's routing" that
[m6-properties.md](m6-properties.md#steward-issues-whose-substance-lands-here)
leaves to burrow for remi#1248.

**Parity:** `tests/autoland/watched.py`'s deadline and kick cases, run
against a conaryd-held repo's export.

### N1: renovate: major verdicts sealed in Rust, and in the form Conary reads

Issue: [burrow#1307](https://github.com/FieldmouseWorks/burrow/issues/1307).

`burrow-renovate` signs and checks major verdicts itself:

- it uses Conary's `SealKey`, the same MAC as `seal.py`, and creates or
  repairs the key as `seal.key()` does;
- for a repo conaryd holds it writes `major-verdict:v2`, by the landing
  record, and the v1 marker for one the steward holds.

The flow configuration gets `major_key`. That goes in the file that renders
it at the time, which is burrow#724's renderer if that has landed.

`modules/updates/major-seal.py`, `majorSealSrc`, `tools.seal` and the seal
half of `tests/renovate-sweep.py` are deleted. `seal.py` stays for the
steward until D, and its v1 writer is deleted with D.

**Parity:** `tests/renovate-sweep.py`'s seal cases as Rust tests. A v2 marker
round-trips through Conary's verifier. A conaryd-held major is landed on the
sweep's `ok` in the sim, and it fails on main.

### N2: renovate: the lock-file wait in Rust

Issue: [burrow#1309](https://github.com/FieldmouseWorks/burrow/issues/1309).

`burrow-renovate` reads when an age-refused refresh may go again, and its own
limits, itself. That's `when`, `ripe` and `--limits`, ported. The `lock-wait`
program, `lockWaitSrc` and `tools.lockWait` are deleted. lock-age's
`include_str!` test points at the Rust reader's words. The steward's library
use of `lock_wait.py` stays until D.

**Parity:** `tests/renovate-sweep.py:87-133`, including the `+20` in
`interval`.

### N3: renovate: coverage from the landing record, not `repos.json`

Issue: [burrow#1308](https://github.com/FieldmouseWorks/burrow/issues/1308).

renovate-sweep counts a repo as covered when the landing record gives it to
conaryd, and takes its proofs from the flow configuration. For repos the
steward holds, it reads `repos.json` until D. D deletes that reader and the
steward's writer.

**Parity:** the sweep's coverage tests, plus a new case: a conaryd-held repo
is covered. It fails on main.

### C1: flow: export each PR's overdue stage and owner notice (Conary)

Issue: [#490](https://github.com/FieldmouseWorks/Conary/issues/490).

`cn flow status OWNER/REPO --json` adds, for each open PR, the overdue stage
with its `since` and `due`, and its owner notice. It reads only the data
directory, as `cn flow status` does now. When FP10 lands, its subject key is
added. Lane: Conary flow, after the tasks open there.

### C2: flow: the lock-file grace includes the sweep's 20-second gap (Conary)

Issue: [#491](https://github.com/FieldmouseWorks/Conary/issues/491).

In `flow/state.rs`, the grace while a refresh is pending is `3600 + run`.
`lock_wait.interval` has `3600 + 20 + run`. The flow's deadline, and with it
an overdue lock-file PR, therefore comes 140 seconds early. One test fixes
the value to what `tests/renovate-sweep.py` pins.

### C3: flow: stage label names from the configuration (Conary)

Issue: [#492](https://github.com/FieldmouseWorks/Conary/issues/492).

`Stage::label`'s names, the owner's `waiting: Peter` among them, come from
the flow configuration. burrow renders them from `labels.nix`, so they're no
longer hard-coded. The defaults name no person.

### D: delete the steward's Python (S4)

Issue: [burrow#1310](https://github.com/FieldmouseWorks/burrow/issues/1310).

**Waits on:** [the gate](#the-gate). It's three PRs, each landing in this
order, one after the other.

**1. burrow.** Delete:

- `modules/autoland/*.py`, `tests/autoland/`, `checks/autoland.nix`,
  `checks/autoland-path.nix`;
- the steward's units, handlers and watchdog source, `stewardSrc`,
  `stewardImports` and the `autoland-steward` package;
- `autoland-shadow@`;
- proj's `proofBuildDriver`, `proofsState` and its `policy_proofs.rs` path;
- the steward side of `landing.nix` and `burrow-landing`: the release
  marker, and `settings landing … steward`;
- `burrow-upkeep`'s `claims`, `fetch` and `commits` subcommands, and every
  other subcommand nothing runs;
- the `dryRun` and `continuationKick` options;
- the v1 judge goldens in `burrow-brief`.

Update `probe.nix`, `machineMd`, `docs/front-door.md`,
`docs/repo-baseline.md` and `docs/landing.md` to say what replaced each
piece.

**2. remi.** Drop the `repos.py` caller from `checks.undo`, and the options
burrow no longer has.

**3. Conary.** Delete the legacy judgment readers listed above, and update
[m6.md](m6.md) and this page.

**Check:** `steward.py` is absent, which is #321's check. Also, `rg` finds no
`autoland-steward`, `steward.py` or `modules/autoland/.*\.py` in burrow,
remi or Conary.

**Closes**, each by its full name, or with a comment saying where its
substance went:

| Issue | What happens to it |
| --- | --- |
| burrow#1103, burrow#1055, burrow#947, burrow#760, burrow#880 | Moot once the steward is gone |
| burrow#964, burrow#953 | Their substance is FP4's priority levels |
| burrow#820 | Its substance is R0, #317 |

burrow#1046 has already closed with U5. PRs burrow#1049 and burrow#899 are
already closed.

**What waits on D:**

- proj's stage 8, where `lifecycle` and `--owned` go (burrow#983,
  burrow#668);
- the gate's stage 5, where `build-skip` goes (burrow#998).

### The way back

Until D, handing a repository back to the steward is one setting, with no
deploy. D removes that: once the steward is gone, the way back from conaryd
is a pause (`cn flow pause`), which is FP2's. Reverting D's three PRs is a
deploy, and it can be undone. D's PR says this in its body. It's the one
change in this plan the owner should hear about before it lands, and
nothing in it can't be undone.

## Lessons

- **`publish-only-news`:** U5 builds it. It moves to `designed` with this
  plan, and to `built` with U5's tests.
- **`reusable-modules`:** each task keeps machine values in configuration,
  never in code. U3's baseline options, N1's key path and C3's label names
  are the ones it moves.
