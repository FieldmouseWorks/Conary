# Conary design

Draft 0.2 · 2026-10-27 · MIT licensed

Conary is the ledger and the landing engine for code that agents write. It
records why every change exists, who made it, what proved it and what it would
cost to get wrong. It lands a change when that proof is green, and it can undo
any change, any agent session's work, or everything since a point in time.

Conary works on top of git and GitHub. Trunk is the default branch on GitHub,
and agents keep using plain git. Conary adds the record git doesn't keep and
the questions git can't answer:

- **Why** does this line look like this? The change that last touched it, the
  task and session behind that change, and the evidence it landed with.
- **What did that agent session do**, across every repo, and undo all of it.
- **Is this proof still good**, on this head, against this base, from a runner
  the agent couldn't touch?

Draft 0.1 proposed a git replacement built on jj-lib. 0.2 drops that. jj
already exists, GitHub is where the agents live, and since 0.1 was written the
machine has grown most of the landing pipeline on top of git: the autoland
steward. What's missing is the record underneath it, and a Rust home for the
steward once it settles.

## 1. Goals and non-goals

**Goals**

1. **One record of every change.** Intent, author and session, class,
   evidence and landing, for every PR in every repo the steward covers, kept
   on remi, append-only.
2. **`why`, from anywhere.** Any agent, including a cloud session that can
   only reach GitHub, can ask why a line or symbol is the way it is.
3. **Undo by change, by session and by time.** Every undo is a new change that
   lands through the same flow. Nothing is rewritten.
4. **The steward, in Rust.** Conary takes over the steward's landing flow one
   piece at a time, deleting the Python for each piece as it goes.
5. **Evidence that means something.** Agents don't grade their own work, new
   tests must fail on the old code, and evidence names exactly what it ran on.
6. **Agent-first interface.** Every call takes structured input and returns
   structured output. The CLI is a thin client over the same API.

**Non-goals**

- Replacing git, its object store or its wire format. Agents keep cloning,
  committing and pushing as they do today.
- Replacing GitHub as the place people and bots meet the code.
- Semantic merge and commutation (parked, with the condition that unparks
  them, in section 9).
- Working-copy undo. Files on remi already have that: the guard's ZFS
  snapshots before anything deletes or rewrites them
  ([burrow's undo module](https://github.com/FieldmouseWorks/burrow/blob/main/modules/undo.nix)).
- Replacing the build or test system. Conary records evidence. Runners
  produce it.
- Windows.

## 2. What Conary grows out of

Most of 0.1's model already runs, spread across burrow's modules. Conary owns
the record of it first, then takes over the jobs one at a time.

| 0.1 idea | What does it today | What Conary does with it |
| --- | --- | --- |
| Landing queue | The autoland steward ([PR flow](https://github.com/FieldmouseWorks/burrow/blob/main/docs/repo-baseline.md#the-pr-flow-the-steward)) | Records every step (M1), then becomes it (M5) |
| Evidence | CI check runs, the reviewers' verdicts, proof jobs, judge verdicts, `review-settled` | One evidence record per result, keyed to head and base |
| R0/R1/R2 classes | The guard's R2 commands, the steward's `oneWay` paths | Owns the class of a change (commands stay with the guard) |
| R1 auto-revert | The red-default-branch fix job (PR flow step 8) | Records it, then runs it as `undo` |
| Intent | PR title and body, the linked issue, the Claude session link | One intent record per change |
| Undo | `git revert` by hand, ZFS snapshots for files | `undo` by change, session or time, through the landing flow |

Until Conary takes a job over, that job's home stays where it is, and this
doc links to it rather than restating it. When Conary takes one over, the old
path is deleted in the same change.

## 3. Core model

```
Op log    ──records every change to──▶  Ledger
Ledger    = { changes, revisions, evidence, landings, sessions }
Change    = stable id + intent + author + class + revisions + evidence + status
Revision  = one head of a change, on one base (git commit ids)
Evidence  = one result of one check on one revision
Landing   = trunk moving, and what moved it
```

### 3.1 Change

A change is one unit of work that lands or doesn't: today, a pull request.
Conary creates it the first time it sees the PR and keeps its ID until the
change lands, closes or is reverted.

| Field | Meaning |
| --- | --- |
| `id` | 128-bit random, shown as a short prefix (`kq7tmz`). Assigned by Conary, never by the author, and never changes. |
| `repo`, `pr` | Where it lives on GitHub. A PR is one change for its whole life, however often it's pushed, rebased or retargeted. |
| `intent` | Why it exists: the task (issue or project thread), a one-line summary, and the PR body. The full text lives in the ledger. |
| `author` | Who made it: actor kind (agent, person, bot), the session that opened it, the tool and the model when known. |
| `class` | R0, R1 or R2, set by policy (section 3.4). The author can raise it, never lower it. |
| `revisions` | Every head Conary saw, in order. |
| `status` | `open`, `landed`, `closed`, `reverted`. |
| `links` | `reverts`, `fixes` (a red trunk), `supersedes`, `depends-on`. |

### 3.2 Revision

A revision is one head of a change on one base: `(base commit, head commit)`,
plus the trees they point at. Its ID is the hash of the two tree IDs, so it
names exactly the content that was checked, and two revisions with the same
trees are the same revision however they were reached. Every push, rebase or
retarget adds a revision.

### 3.3 Evidence

Evidence is one result of one check on one revision:

```
Evidence {
  change, revision
  check:     kind (ci | review | proof | judge | negative) and name
  result:    pass | fail | error, plus findings for a review
  env:       environment digest when the check reports one (the flake's lock)
  source:    what produced it: a check run id and app, a reviewer, a proof job
  signer:    the identity Conary trusts it from (section 8)
  log:       link to, or hash of, its output
  at:        when it finished
}
```

Three rules make it trustworthy:

1. **Agents don't grade their own work.** Evidence counts only when it comes
   from a source the agent's session couldn't produce: CI on a configured
   runner, the steward's reviewers, or a proof or judge job Conary started.
   Anything the authoring session posts itself is feedback, not evidence.
2. **New tests must fail on the old code.** A change that adds a test needs a
   `negative` record: the new test run against the base's code, failing. How
   a repo names its new tests is open (section 10).
3. **Evidence is about the tree it ran on, and says so.** A PR's evidence
   proves its revision: that head on that base. Landing squashes it onto a
   newer trunk, and the default branch is never required to be up to date
   first ([why](https://github.com/FieldmouseWorks/burrow/blob/main/docs/repo-baseline.md#settings-the-steward)).
   So Conary never records pre-land evidence as proof of the landed tree. The
   landed tree gets its own evidence from trunk's CI, recorded against the
   landing, and a red result there starts an undo or a fix (section 5).

### 3.4 Class

| Class | Meaning | Examples |
| --- | --- | --- |
| R0 | Landing changes nothing outside the repo. A revert undoes it fully. | Most code in a repo that doesn't deploy on merge |
| R1 | Landing goes live, but a revert undoes it. | remi (deploys main within 15 minutes), the website, the profile |
| R2 | Landing can't be fully undone, or is the owner's to decide. | Secrets, migrations, CODEOWNERS, nomos decisions, consolebook ADRs |

Policy sets the class from the repo and the paths a change touches, renames
and copies included. The policy starts as today's `oneWay` patterns and
per-repo settings, moved into Conary (M4), and grows rules only when a real
change needs one. The guard's R2 command list stays where it is: it classifies
commands an agent runs, not changes that land.

### 3.5 Landing

Trunk moves in one of two ways, and the ledger records both:

- **`land`**: Conary (or, until M5, the steward) merges a change whose
  evidence and class allow it. The landing records the change, the revision
  that was checked, the trunk commit before and after, and the evidence it
  landed on.
- **`outside`**: anything else that moves trunk: a person merging from the PR
  page, a repo with `directPush` automation, a force-push. Conary can't stop
  these on GitHub. It records them, and `why` says so plainly.

### 3.6 Op log

Every write to the ledger is one operation:

```
Op {
  id:      hash of (parent op, kind, payload, at)
  parent:  the previous op
  kind:    observe | intent | evidence | classify | land | outside | undo | backfill
  payload: kind-specific
  at:      timestamp
  sig:     conaryd's key
}
```

The log is one hash-chained, append-only file, signed by conaryd. It's the
source of truth: the SQLite tables are an index of it and can be rebuilt by
replaying it. Nothing in the log is ever edited or deleted. A correction is a
new op that supersedes the old one.

## 4. Undo

Every undo is a new change: a revert PR that goes through the same landing
flow as any other, under the class of what it reverts. Conary never rewrites
trunk.

| Call | What it does |
| --- | --- |
| `undo <change>` | Opens a PR that reverts one landed change. |
| `undo --actor <session>` | Reverts every change one agent session landed, newest first, one PR per repo. |
| `undo --since <time> --repo <repo>` | Reverts everything that landed on one repo's trunk after that time. |
| `at <time> <query>` | Answers any read-only query as the ledger stood at that time. |

A revert that doesn't apply cleanly, because later work built on what it
undoes, still opens its PR with the conflict, and a Claude job resolves it
like any other finding. Undo of an R2 change waits for the owner like the
change did.

## 5. The landing flow

The flow itself is burrow's: [the PR flow](https://github.com/FieldmouseWorks/burrow/blob/main/docs/repo-baseline.md#the-pr-flow-the-steward)
says what happens from a ready PR to a merge, and stays the one home of that
until conaryd takes it over. What Conary adds before then:

- every step is an op in the ledger, so "why did this land" and "why is this
  stuck" have one answer;
- trunk's CI after each landing is evidence on that landing, and the
  red-default-branch job's revert or fix links back to the change that broke
  trunk.

When conaryd runs the flow (M5), the flow's description moves here, burrow's
doc links to it, and `steward.py` goes. conaryd keeps the steward's rules on
event-triggered passes: it acts on webhooks and on waits it booked itself,
never on a timer.

## 6. Interfaces

| Interface | Used by | Notes |
| --- | --- | --- |
| `conary-core` (Rust library) | conaryd, tests, tools | All logic lives here. |
| `conaryd` (daemon on remi, JSON-RPC over a Unix socket) | The steward, Claude jobs, the CLI | One per host. Serialises writes, serves reads concurrently, streams events. |
| MCP server | Agents on remi | The same calls as tools, with schemas. |
| `cn` CLI | People and scripts | JSON output by default, `--human` for people. |
| `refs/notes/conary` on GitHub | Any clone, cloud sessions included | The public-safe slice of the ledger, pushed after every op. |

The notes ref is how a cloud session reads the ledger: it can't reach remi,
but it can fetch a git ref. Each landed trunk commit carries a note with its
change ID, intent summary, class, session, and links to its evidence. Notes
hold no prompt text, and nothing a repo's own rules keep out of its history
(LoK-web refuses session links, for one), so a note is never more public than
the repo it's in. `cn why` reads the notes when
conaryd isn't reachable.

The core calls:

```
change.get(id | repo#pr) / change.list(repo?, session?, status?, class?, since?)
evidence.add / evidence.query(change | revision)
land(change)                      // M5; until then the steward lands and reports
undo(change | actor | since) / at(time, query)
log.query(repo?, path?, symbol?, session?, intent~text?, since?, class?)
why(repo, path, line | symbol)    // the change, its intent, session and evidence
events.subscribe(land | outside | red | undo | owner)
```

## 7. Storage

- **Op log:** one append-only, hash-chained file per host, the source of
  truth.
- **Index:** SQLite in WAL mode for changes, revisions, evidence, landings and
  sessions, with FTS5 over intent text. Rebuilt from the op log when in doubt.
- **Code:** stays in git. Conary stores commit and tree IDs, never contents.
- **On remi:** the ledger is state that has to survive the root wipe, so remi
  persists its directory and backs it up with the rest of remi's state.

Targets on remi, warm cache, a year of ledger across every repo:

| Operation | Target |
| --- | --- |
| Record an op | under 10 ms |
| `why` / `log.query` | under 50 ms |
| `undo` bookkeeping, excluding the PR it opens | under 200 ms |
| Notes push after a landing | under 5 s |

## 8. Security and identity

- Every op is signed by conaryd's key. The log's hash chain makes "what did
  this session do" a question with a trustworthy answer.
- Evidence counts by its source, never by what it says about itself: a check
  run from a configured app on a configured runner, a reviewer the steward ran,
  a proof or judge job Conary started and sealed.
- Today the steward acts on GitHub as the owner's own login, so any session
  holding that token could post a status that looks like the steward's. The
  judge's seals already guard verdicts against that. A GitHub App for Conary
  would let `review-settled` and landings be pinned to one integration ID
  instead (section 10).

## 9. Parked, and what unparks it

| Idea | Parked because | Unparked when |
| --- | --- | --- |
| Own object store, Conary as a standalone VCS | git does the storage fine, and every agent already speaks it | Something in this design can't be built on git, with the case written down |
| Semantic patches and commutation (tree-sitter diffs, Mergiraf) | The measured waits are review, CI and bots, not conflicts | The ledger shows conflict and rebase work costing real time each week |
| jj as the working-copy layer | Nothing here needs it, and cloud sessions use plain git | Agents on remi want it for themselves; Conary never depends on it |
| Offline laptops syncing a ledger | One ledger on remi covers how the work happens today | A second machine writes changes Conary has to record |

## 10. Open questions

1. **A GitHub App for Conary.** It would make the settle status and landings
   unforgeable by a session with the owner's token. It costs a setup step the
   owner has to do on GitHub. Worth it at M5 at the latest.
2. **Naming new tests.** The negative-control rule needs to know which tests a
   change added. Per-language detection (new `#[test]` functions, new test
   files) or a list the PR declares and a judge checks?
3. **How much intent to keep.** The session link and PR body are always
   there. A session's own prompt usually isn't readable by the steward. Is the
   link enough, or should a Claude job's prompt be recorded when Conary
   started the job?
4. **Backfill depth.** Every merged PR since autoland went live is cheap. All
   of git history is possible, but most of it has no intent or evidence to
   record.
5. **CLI name.** `cn` is short and doesn't clash with common Unix tools.
   Check before M1.

## 11. Milestones

| Milestone | Delivers | Exit check |
| --- | --- | --- |
| M0 | Rust workspace, `rust = { };` in the flake, CI | `nix flake check` runs the workspace's tests in CI |
| M1 | conaryd on remi, recording every PR event the steward sees, read-only, plus a backfill of merged PRs. `cn log`, `cn why` | `cn why` on a line in burrow returns the PR, its intent, session and evidence |
| M2 | The notes ref and the MCP server | A cloud session answers `why` from a fresh clone |
| M3 | `undo` by change, session and time, through the landing flow | A deliberately bad session's changes in two repos are reverted by `undo --actor` and land with no person involved |
| M4 | Class policy in Conary; the steward reads classes from conaryd | `oneWay` is gone from burrow's autoland config |
| M5 | conaryd runs the landing flow | `steward.py` is deleted, and a week of PRs across every repo lands through conaryd |
| M6 | A written decision on each parked idea, from ledger data | The decision |
