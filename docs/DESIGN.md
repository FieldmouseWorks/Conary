# Conary design

Draft 0.2 · 2026-10-27 · MIT licensed

Software process is built on two assumptions that aren't true any more:
writing code is slow, and undoing it is expensive. Code review, approvals,
merge queues, up-to-date branches and release trains all exist because a bad
change cost days to write and days to unwind, so it was worth stopping
everything to check it first. With agents, a change takes minutes to write
and a revert takes minutes to land. Gates that cost more than the mistakes
they catch should go, and cheap, trustworthy undo is what lets them go.

Conary is that undo, and the landing engine that leans on it. It records why
every change exists, who made it, what proved it and what it would cost to get
wrong. It lands a change the moment its proof allows, and it can undo any
change, any agent session's work, or everything since a point in time. Only
what undo can't fix (R2: secrets, migrations, the owner's decisions) waits for
a person.

Conary works on top of git and GitHub. Trunk is the default branch on GitHub,
and agents keep using plain git. Every gate in section 2 can go on that
footing: none of them needed a new VCS, they needed undo that's cheap and a
record good enough to trust it. Section 10 names the places where a layer on
git would run out, and what Conary would have to own then.

Conary adds the record git doesn't keep and the questions git can't answer:

- **Why** does this line look like this? The change that last touched it, the
  task and session behind that change, and the evidence it landed with.
- **What did that agent session do**, across every repo, and undo all of it.
- **Is this proof still good**, on this head, against this base, from a runner
  the agent couldn't touch?

Draft 0.1 proposed a git replacement built on jj-lib. 0.2 puts that on hold.
Since 0.1 was written, the machine grew most of the landing pipeline on top of
git (the autoland steward), and nearly every agent works from a cloud session
that only reaches GitHub. What's missing is the record underneath, undo built
on it, and a Rust home for the steward once it settles.

## 1. Goals and non-goals

**Goals**

1. **No gate that undo makes cheap.** Every check that exists only because
   mistakes used to be expensive goes (section 2). What stays has a reason
   undo can't answer.
2. **One record of every change.** Intent, author and session, class,
   evidence and landing, for every PR in every repo the steward covers, kept
   on remi, append-only.
3. **`why`, from anywhere.** Any agent, including a cloud session that can
   only reach GitHub, can ask why a line or symbol is the way it is.
4. **Undo by change, by session and by time.** Every undo is a new change that
   lands through the same flow. Nothing is rewritten.
5. **The steward, in Rust.** Conary takes over the steward's landing flow one
   piece at a time, deleting the Python for each piece as it goes.
6. **Evidence that means something.** Agents don't grade their own work, new
   tests must fail on the old code, and evidence names exactly what it ran on.
7. **Agent-first interface.** Every call takes structured input and returns
   structured output. The CLI is a thin client over the same API.

**Non-goals**

- Replacing git, its object store or its wire format. Agents keep cloning,
  committing and pushing as they do today.
- Replacing GitHub as the place people and bots meet the code.
- Semantic merge and commutation (parked, with the condition that unparks
  them, in section 10).
- Working-copy undo. Files on remi already have that: the guard's ZFS
  snapshots before anything deletes or rewrites them
  ([burrow's undo module](https://github.com/FieldmouseWorks/burrow/blob/main/modules/undo.nix)).
- Replacing the build or test system. Conary records evidence. Runners
  produce it.
- Windows.

## 2. Gates, and what replaces them

Each gate below exists because a mistake used to be expensive. For each one,
this is what makes it cheap now, and what's left of the gate.

| Gate | Why it existed | What replaces it | Where it stands |
| --- | --- | --- | --- |
| A person reviews and approves | A bad change cost days, so a second person checked it first | Agent reviewers; nothing below R2 waits for a person | Done (autoland) |
| A person presses merge | The merge was the commit point | `land` the moment evidence and class allow | Done (autoland) |
| Up-to-date branches, merge queues | A broken combination on trunk was slow to find and unwind | Trunk's own CI after each landing, and an `undo` or fix on red | Done 2026-10-26; Conary makes the revert an `undo` (M3) |
| Review rounds before landing | Catching a bug before trunk was cheaper than after | R0: lands on green CI and attested evidence; the reviewers read the landed change, a finding opens a fix change on its own, and a P1 that can't be fixed fast is reverted. R1 keeps review before landing until the ledger shows review after landing catches as much | Proposed (M5) |
| Release trains, freeze windows | Shipping a bad build was slow to roll back | R1 lands live and reverts as fast as it landed; remi's own switch keeps its dead man's switch and health check | Done for remi, the site and the profile |
| Permission prompts | Any command might be the one you can't take back | The guard asks only for R2 commands and snapshots before anything destructive | Done (burrow) |

Two gates stay, because undo doesn't make them cheap:

- **CI before landing.** A red trunk breaks every other agent's base at once,
  so R0 still lands only on green evidence. Conary makes it cheaper instead
  of skipping it: evidence reuse where a check's footprint is known, and only
  the checks a change can affect.
- **A person for R2.** Undo can't bring back a leaked secret or dropped data.
  R2 is kept as narrow as policy can make it.

## 3. What Conary grows out of

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

## 4. Core model

```
Op log    ──records every change to──▶  Ledger
Ledger    = { changes, revisions, evidence, landings, sessions }
Change    = stable id + intent + author + class + revisions + evidence + status
Revision  = one head of a change, on one base (git commit ids)
Evidence  = one result of one check on one revision
Landing   = trunk moving, and what moved it
```

### 4.1 Change

A change is one unit of work that lands or doesn't: today, a pull request.
Conary creates it the first time it sees the PR and keeps its ID until the
change lands, closes or is reverted.

| Field | Meaning |
| --- | --- |
| `id` | 128-bit random, shown as a short prefix (`kq7tmz`). Assigned by Conary, never by the author, and never changes. |
| `repo`, `pr` | Where it lives on GitHub. A PR is one change for its whole life, however often it's pushed, rebased or retargeted. |
| `intent` | Why it exists: the task (issue or project thread), a one-line summary, and the PR body. The full text lives in the ledger. |
| `author` | Who made it: actor kind (agent, person, bot), the session that opened it, the tool and the model when known. |
| `class` | R0, R1 or R2, set by policy (section 4.4). The author can raise it, never lower it. |
| `revisions` | Every head Conary saw, in order. |
| `status` | `open`, `landed`, `closed`, `reverted`. |
| `links` | `reverts`, `fixes` (a red trunk), `supersedes`, `depends-on`. |

### 4.2 Revision

A revision is one head of a change on one base: `(base commit, head commit)`.
Its ID is the hash of those two commit IDs. A git commit ID already hashes its
tree, parents and message, so the revision ID changes with anything about the
revision: a reworded commit, new ancestry, a rebase or a retarget all give a
new revision, and none of them overwrites an old one. Evidence binds to the
revision. Trees are named by their own git IDs, and two revisions sharing a
tree is a fact Conary can report, never a reason to count one's evidence for
the other.

For a GitHub PR the base is the merge base of the head and the base branch:
the tree its diff and its review are against, which trunk moving doesn't
change. A new head is a new revision, as is a retarget that moves the merge
base; trunk moving under an open PR is not.

### 4.3 Evidence

Evidence is one result of one check on one revision:

```
Evidence {
  change, revision
  check:     kind (ci | review | proof | judge | negative) and name
  result:    pass | fail | error, plus findings for a review
  env:       environment digest when the check reports one (the flake's lock)
  source:    what produced it: a check run id and app, a reviewer, a proof job
  attest:    the producer's own attestation that this result came from this
             run on this revision (section 9)
  log:       link to, or hash of, its output
  at:        when it finished
}
```

Three rules make it trustworthy:

1. **Agents don't grade their own work.** Evidence counts only when it was
   produced outside the authoring session's sandbox and carries its
   producer's attestation: CI on a configured runner, the steward's
   reviewers, or a proof or judge job Conary started. conaryd's signature on
   the op that records a result doesn't stand in for that attestation.
   Anything the authoring session posts itself is feedback, not evidence.
2. **New tests must fail on the old code.** A change that adds a test needs a
   `negative` record: the new test run against the base's code, failing. How
   a repo names its new tests is open (section 11).
3. **Evidence is about the tree it ran on, and says so.** A PR's evidence
   proves its revision: that head on that base. Landing squashes it onto a
   newer trunk, and the default branch is never required to be up to date
   first ([why](https://github.com/FieldmouseWorks/burrow/blob/main/docs/repo-baseline.md#settings-the-steward)).
   So Conary never records pre-land evidence as proof of the landed tree. The
   landed tree gets its own evidence from trunk's CI, recorded against the
   landing, and a red result there starts an undo or a fix (section 6). That
   is the owner's call for R0 and R1, where a revert undoes the damage. R2 is
   different: undo can't help, so an R2 change lands only on evidence for the
   candidate tree itself (its head merged onto the trunk commit it lands
   on), and if trunk moves before the merge, that evidence no longer counts
   and the checks run again.

### 4.4 Class

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

### 4.5 Landing

Trunk moves only through **`land`**: Conary (or, until M5, the steward)
merges a change whose evidence and class allow it. The landing records the
change, the revision that was checked, the trunk commit before and after, and
the evidence it landed on. An automation that writes trunk itself, like
remi-update pushing its lock bumps, lands through the same call with the
evidence its own gate produced, so its pushes are landings too.

Until M5, a merge is read as the steward's landing only when the steward's
own logins said `review-settled` success on that head after any retarget and
before GitHub merged it, and trunk's commit is the PR's own: GitHub's merge
of that head, or a commit GitHub ties to that PR. A word that came after the
merge can't have authorized it.

GitHub can't make that absolute: a repo admin can merge past the ruleset from
the PR page, and anyone with the owner's token can push. So anything else that
moves trunk is recorded as an **`outside`** op. An outside op is a violation,
never a second way in: it's listed for the owner straight away, its commits
get the class check and the evidence a landing would have needed, after the
fact, and an R2 path among them is the owner's at once. `why` says plainly
that the change never landed.

### 4.6 Op log

Every write to the ledger is one operation:

```
Op {
  id:      hash of the op's bytes exactly as written
  parent:  the previous op's id
  at:      timestamp
  body:    what happened (`Body` in crates/conary-core/src/op.rs): observe,
           intent, revision, evidence, land, outside, close, reopen; classify
           and undo join with M4 and M3
  sig:     the ledger key's signature over the id
}
```

The log is one hash-chained, append-only file, signed by the ledger's key.
It's the source of truth: everything else is rebuilt by replaying it, and an
op the ledger can't apply is refused before it's written. Nothing in the log
is ever edited or deleted. A correction is a new op that supersedes the old
one.

The log is refused whole if any line fails its id, its signature, its
parent or its schema, the last line included. A last line with no newline is
kept if it verifies. Only a line whose JSON stops short is a write that never
finished (its caller was never told it succeeded), so the next writer cuts it.
A writer whose append fails partway stops until the log is reopened and
verified again. A log cut short by whole lines still chains, so its head is
anchored off the box, at `refs/conary/anchor` in a repository the machine
names. Each move is a commit naming the ledger's key and its newest op.

- `cn init` sets a ledger up once: its key, then the ref. It's the ledger's
  identity. Run again it finishes a set-up that stopped partway, and with a
  ref already there it takes only the key that ref names, so a lost key or
  directory is refused until the key is back, never replaced by a new
  ledger. A writer never makes a key.
- Each write takes the ledger's lock, then reads the ref, and refuses another
  key's ref or a log that doesn't hold the ref's op, before it changes a byte
  of the log. After writing it moves the ref on, fast-forward only, and keeps
  a copy beside the log.
- Every move on the ref since that copy, and the move before it, must name
  the ledger's key, go forward and be held by the log, so a ref moved back,
  even to an op it named before, or a longer history in place of one the log
  acknowledged, is refused. A write stopped before it kept the copy is caught
  up by the next; a copy many moves behind is refused, and `cn init` without
  one checks the ref's whole history.
- Readers check that copy, so a read never waits on GitHub, and refuse a log
  with none. The copy is only as good as the box: whoever can rewrite the log
  can rewrite it too. The next write, or `cn verify` from anyone who can read
  the log, checks against GitHub itself.

## 5. Undo

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

## 6. The landing flow

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

## 7. Interfaces

| Interface | Used by | Notes |
| --- | --- | --- |
| `conary-core` and `conary-github` (Rust libraries) | conaryd, tests, tools | All logic lives here; `conary-github` reads GitHub into ledger ops. |
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

## 8. Storage

- **Op log:** one append-only, hash-chained file per host, the source of
  truth.
- **Index:** the ledger is replayed from the op log into memory when it's
  opened. SQLite in WAL mode (with FTS5 over intent text) takes over once
  replay stops being fast or intent search needs it, and is rebuilt from the
  op log when in doubt.
- **Code:** stays in git. Conary stores commit and tree IDs, never contents.
- **On remi:** the ledger is state that has to survive the root wipe, so remi
  persists its directory and backs it up with the rest of remi's state.
- **Anywhere else:** nothing in Conary depends on NixOS, ZFS or remi. It builds
  with plain cargo, and the ledger's directory, the socket and the keys are
  configuration. remi's Nix module is one thin wrapper over the binary, and a
  systemd unit or another distro's package is another.

Targets on remi, warm cache, a year of ledger across every repo:

| Operation | Target |
| --- | --- |
| Record an op | under 10 ms |
| `why` / `log.query` | under 50 ms |
| `undo` bookkeeping, excluding the PR it opens | under 200 ms |
| Notes push after a landing | under 5 s |

## 9. Security and identity

- Every op is signed by conaryd's key. The log's hash chain makes "what did
  this session do" a question with a trustworthy answer.
- Evidence counts by its producer's attestation, never by what it says about
  itself or by conaryd having recorded it:
  - CI on GitHub Actions: a signed attestation the job itself makes with its
    GitHub OIDC identity (Sigstore, keyless), over the result, the revision's
    head and base commits and the log hash. It binds the result to the
    workflow, the commit and the run that made it. Making it is a step to add
    to burrow's shared `flake-check.yml` (at M1), so every repo's gate makes
    one without its own change. GitHub's API
    record of the check run is where Conary finds it, never a substitute
    for it.
  - Jobs on remi (reviewers, proof and judge jobs, negative runs): a signature
    by that runner service's own key, which no authoring session or sandbox
    can read, over the result, the revision and the run's log hash.
  - Anything without a valid attestation doesn't count, whatever its source:
    a missing or unverifiable one fails closed.
- Today the steward acts on GitHub as the owner's own login, so any session
  holding that token could post a status that looks like the steward's. The
  judge's seals already guard verdicts against that. A GitHub App for Conary
  would let `review-settled` and landings be pinned to one integration ID
  instead (section 11, [#14](https://github.com/FieldmouseWorks/Conary/issues/14)).

## 10. Where a layer on git runs out

Each of these would need Conary to own more than a layer on git. None of them
is needed for the gates above, so each waits for a measured reason.

[Anthropic's Scion](https://www.anthropic.com/engineering/redesigning-source-control)
shows what the first row looks like at 30,000 merges a day in one monorepo: a
server that owns every commit, merge and query, with no merge queue, testing
after merge and an agent fixing what collides. It reached the same verdict on
merge queues that section 2 does, but its problem was scale. Conary's is
trust and undo across many small repos for one owner, which git serves fine.

| Idea | Parked because | Unparked when |
| --- | --- | --- |
| Conary as trunk's source of truth, GitHub a mirror | A PR per change is fine at today's rate, and GitHub is where cloud agents reach the code | PR overhead or the shared GitHub API quota starts limiting how fast changes land |
| Own object store, Conary as a standalone VCS | git does the storage fine, and every agent already speaks it | Something in this design can't be built on git, with the case written down |
| Semantic patches and commutation (tree-sitter diffs, Mergiraf) | The measured waits are review, CI and bots, not conflicts | The ledger shows conflict and rebase work costing real time each week |
| jj as the working-copy layer | Nothing here needs it, and cloud sessions use plain git | Agents need undo inside a working copy finer than the guard's ZFS snapshots (every tool call, by operation), or agents on remi want it for themselves |
| Offline laptops syncing a ledger | One ledger on remi covers how the work happens today | A second machine writes changes Conary has to record |

## 11. Open questions

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

## 12. Milestones

| Milestone | Delivers | Exit check |
| --- | --- | --- |
| M0 | Rust workspace, `rust = { };` in the flake, CI | `nix flake check` runs the workspace's tests in CI |
| M1 | `cn` on remi, recording every PR event the steward sees, read-only, plus a backfill of merged PRs. `cn log`, `cn why`. Each write takes the ledger's writer lock | `cn why` on a line in burrow returns the PR, its intent, session and evidence |
| M2 | conaryd serving the ledger over its socket, the notes ref and the MCP server | A cloud session answers `why` from a fresh clone |
| M3 | `undo` by change, session and time, through the landing flow | A deliberately bad session's changes in two repos are reverted by `undo --actor` and land with no person involved |
| M4 | Class policy in Conary; the steward reads classes from conaryd | `oneWay` is gone from burrow's autoland config |
| M5 | conaryd runs the landing flow, and R0 changes land before review, with review after landing | `steward.py` is deleted, and a week of PRs across every repo lands through conaryd, R0 ones with no wait on a reviewer |
| M6 | A written decision on each parked idea, from ledger data | The decision |
