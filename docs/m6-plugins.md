# M6 slice 5c: delivery plan for the plugins and their producers

This plan covers burrow's Rust plugins for review, fix, judge and
deterministic proof, and the independent producers that sign the evidence
they produce. It splits that work into single-PR tasks. It plans only this
integration group, not the rest of M6 ([m6.md](m6.md)).

This plan deploys nothing, introduces no secret and grants no authority
beyond [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s. It
was written against Conary's main at `f4bd817` and burrow's main at
`fa6254e`, on 2026-11-14. The issues linked here own the live state.

The review loop's policy (severities, fixes, rounds, judging, escalation)
has one page, [review-loop.md](review-loop.md). The tasks below link it and
don't restate it.

## Ground rules for every task

These apply to every task below. The tasks don't repeat them.

- Every PR from this plan carries the `first` label.
- **Contract ownership.** The plugin contract changes only in
  [C1](#c1-contract-conary-plugin-v4-carries-what-the-plugins-need).
  C1 continues the contract work of
  [#295](https://github.com/FieldmouseWorks/Conary/issues/295), whose v2 and
  v3 landed in #330 and #353. The verdict vocabulary is
  [#303](https://github.com/FieldmouseWorks/Conary/issues/303)'s, landed in
  #339 (`pass`, `fail`, `owner`, `misdescribed`). No task here adds a word.
- **A plugin answer is a proposal, never evidence.** Only a producer's
  signed statement counts ([service-attestation.md](service-attestation.md)).
  The format and the verifier are fixed.
- **One key per producer.** Each producer has its own Ed25519 key and runs
  as its own system user. No authoring session, sandbox, plugin client or
  other producer can read that key.
- **No signing verb.** No producer has a way to sign an arbitrary statement.
  It signs only results it produced itself, after checking them against
  GitHub's state and its own record.
- **Proofs are programs.** A proof is a program. No model runs a proof.
  Preparing a proof's inputs never holds one of the scarce proof slots
  ([the proof-queue prerequisite](m6.md#the-proof-queue-prerequisite)).
- **Off until #314.** Each plugin and producer ships behind its own enable
  flag, off by default. Turning one on is
  [#314](https://github.com/FieldmouseWorks/Conary/issues/314) for remi and
  [#315](https://github.com/FieldmouseWorks/Conary/issues/315) for the other
  repositories.

## What's missing today

Each item below names what fills the gap.

- **Fix answers.** A fix result can't carry answers to findings.
  `Step::PostFix` fails with "v1 fix results cannot carry thread replies"
  (`flow/execute.rs`). But the fix policy answers every finding it doesn't
  fix ([review-loop.md](review-loop.md)). C1 and C2 fill this.
- **Fix and judge requests.** Neither request names its PR.
  - The fix plugin needs the PR to find the branch it pushes to.
  - The judge producer needs it to seal its verdict to `repo#pr` and to
    read the PR's base and description.
  - The judge request doesn't say why the flow asks for a judge. The brief
    depends on the reason: past the round limit, a dependency update, or no
    CI.

  C1 fills this.
- **Proof requests.** A proof request names a tree but no commit. A proof
  producer signs `revision.head` and posts its check run on a commit, so it
  needs that commit. C1 fills this.
- **Proof preparation.** Preparation holds a slot today. The flow sends one
  `proof` call per proof and holds a proof slot for all of it
  ([flow.md](flow.md#effects-and-retries)). The flow's
  Evaluate/Prebuild/Build/Stored plans (`flow/proofs.rs`) are filled only
  by the simulation, never by a pass. C1 and C3 fill this.
- **Model-run proofs.** A `Job` recipe is a model-run check, and the flow
  still sends it to the proof plugin (`flow::proofs::engine_job`). That
  contradicts "a proof is a program". C3 fills this.
- **Briefs.** The briefs live in two languages. The review brief is in Rust
  (`burrow-upkeep/src/brief.rs`). The fix and judge policy is in Python
  (`modules/autoland/tasks.py`). B1 fills this.
- **Signing.** Nothing in burrow signs anything today. B2 fills this.

## How a plugin and its producer fit together

```
conaryd's flow worker ──stdin/stdout──► plugin executable ──Unix socket──► producer service
   (Conary)                              (thin client)                     (own system user, own key,
                                                                            engine sandbox or program)
```

### The plugin

The plugin is the executable named in the flow configuration's `plugins`.
It's a thin client:

- It answers `describe` and the reserved conformance fixtures itself, with
  no network access.
- It forwards each work request to its producer, waits, and writes the
  producer's answer.
- Any producer error is a nonzero exit, which Conary treats as a plugin
  failure. It never becomes a verdict.

### The producer

The producer is a long-lived service. It runs under its own system user,
and its key arrives as a systemd credential. It does the work: it runs the
engine in its sandbox, or runs the program. It publishes what Conary reads
on GitHub and signs that publication.

The plugin and its producer are separate processes for three reasons:

- conaryd runs as the same user as authoring jobs today. A key held in the
  plugin's process would be readable by them.
- When a work invocation is cancelled, the runner kills the plugin process
  with SIGKILL and nothing else. That can't be trapped.
- The work outlives a short process: an engine session, or a build.

### Only the flow may ask, and GitHub must agree

A producer does what it's asked only when two things hold.

**The caller is the flow.** A producer accepts a connection only from the
controller's own system user, checked by the socket's peer credentials
(`SO_PEERCRED`).
- No authoring session runs as that user.
  [B3](#b3-ledger-run-the-flows-plugins-as-a-controller-user-no-authoring-session-shares)
  creates that user, because conaryd runs as the authoring user today.
- So an authoring session can't ask for a judgment early, choose the
  reasons it's judged for, re-roll a review, or start a proof.
- The judge's `why` and the review's prior history come from the flow, and
  only the flow.

**GitHub's state matches the request.** The producer checks every request
against GitHub before it works and again before it publishes:
- the head is the PR's current head;
- the tree is that head's tree;
- the description is the PR's current description.

**Every request is new work.** A producer never answers a request from a
recorded result. Each request runs a new look, judgment or proof, and its
answer carries that work's own completion. Records exist only so that
publication can be finished after a crash. They're never served as a fresh
answer, so a cached result can't appear newer than work done after it.
Re-asking is safe because only the flow can ask, and it asks only when its
decision needs new work.

A proof's record names the commit it ran on. Its statement is issued only
for that commit, never re-signed for another commit with the same tree.
Another commit gets its own run. Reusing a pass on the same tree is
Conary's consumer path ([#291](https://github.com/FieldmouseWorks/Conary/issues/291)),
which reads the original statement and leaves the producer out of it. An
R2 candidate still needs a statement for its own commit.

### Restart and cancellation

Conary's half of restart and cancellation exists and doesn't change
([flow.md](flow.md#state-and-workers)):

- an invocation is saved before its worker starts;
- a lost worker becomes a failed job at its deadline and is never started
  again;
- cancellation writes a marker, and the runner then kills the plugin.

Burrow's half is the same for every producer, and
[B2](#b2-producers-the-shared-plugin-and-producer-kit) builds it once.

- **A request lives only as long as its connection.** When the plugin dies
  (killed, deadline, cancelled), its connection closes. The producer then
  stops that request's work: it stops the engine sandbox or program cgroup,
  or the job.
- **Cancelled work publishes nothing.** A check run the producer started is
  completed as `cancelled`, never `success`.
- **A producer restart drops requests in flight.** Their plugins see the
  connection close and fail.
- **Publication is reconciled at startup.** A producer persists a result
  before it publishes it. At startup it finishes whatever a crash
  interrupted, from that record. It never re-runs the work.

## Who produces what

The receipts and check names are the existing ones
([flow.md](flow.md#producer-evidence)). `ROLE` is the review role as the
flow settings name it.

| Plugin | Producer posts | Signed check run, and its log | Key's check scope |
| --- | --- | --- | --- |
| Review | Nothing itself. The flow publishes the review (`flow::review_body`) as an account in `settings.review_authors`. The producer then signs that publication if it matches what the producer recorded | `conary/review/ROLE` on the review's head. The log is `review_receipt(head, body)` | `conary/review/ROLE` |
| Fix | Its commits, pushed to the PR's branch | None. A fix is authorship, not evidence. The next review grades it. Its session becomes known from the validated plugin answer ([flow.md](flow.md#explicit-landing-passes)), so a later judge can't be that session | No key |
| Judge | The sealed `autoland-judgment:v2` comment (`SealKey::judgment_by`), as an account in `settings.verdict_authors` | `conary/judgment` on the head. The log is `judgment_receipt_by(revision, generation, verdict, provenance)` | `conary/judgment` |
| Proof | A check run per proof, named as the proof is named in `settings.proofs` | That check run, with `definition` set to `recipe_digest(recipe)`. The log is the sha256 of the program's output | The exact names of the configured proofs |

Check runs go out through the steward's existing GitHub App, from burrow's
existing token door. That App isn't the trust root: the signed statement
is.

An authoring session using another App could post a later, unsigned run
under the same name. Conary reads only the latest run for a receipt, so
that run would make the decision wait. It could never make one count.

## Provisioning

Only the controller user is a task here (B3). Every other row has its own
owner.

| Need | For | Owner |
| --- | --- | --- |
| One Ed25519 private key each for the review, judge and proof producers | B2's signer | The owner and the operator, as secrets ([m6.md](m6.md#boundaries)). No task creates, prints or stores a private key |
| Each key delivered as a systemd credential, readable only by its producer's user | The unit wiring in R, J and P, off by default | The wiring is in R, J and P. Turning it on is #314, then #315 |
| Public keys and check scopes in the machine's producers file (`evidence.producers`), as in the table above | Conary's verifier | #314 for remi, #315 for the rest |
| The judge's seal key, shared by the flow's `settings.judge_key` and the judge producer, and by no authoring session | J | #314, from the steward's existing verdict key |
| The flow configuration's `plugins` paths and `execution` choices, generated from the settings source and its role map | All four plugins | Each plugin task exports its executable. [burrow#724](https://github.com/FieldmouseWorks/burrow/issues/724) renders the configuration, after B3, since both edit `modules/ledger.nix`. [#313](https://github.com/FieldmouseWorks/Conary/issues/313) consumes the configuration schema, and #314 sets the values |
| A controller user that runs the flow's plugins, which no authoring session shares | Every producer's peer-credential check | [B3](#b3-ledger-run-the-flows-plugins-as-a-controller-user-no-authoring-session-shares) builds it, off by default. #314 turns it on |
| The `first` label on every PR | All | Each task |

## The tasks

There are eleven tasks. Four are in Conary: C1, C2, C3 and C4. Seven are in
burrow: B1, B2, B3, R, F, J and P. Each task is one PR, in one repository.

### C1: contract: conary-plugin v4 carries what the plugins need

**Repo:** Conary. **Refs:** #295, #311. **Depends on:** nothing.

**Crates:**
- `conary-core`: `plugin`, its runner and its fixtures.
- `cn plugin check`.
- the example plugin.
- `docs/plugins.md`.
- The flow's request construction (`flow/execute.rs`), which fills in the
  new fields.

**The contract.** Add `conary-plugin v4`. The runner speaks the highest
version a plugin offers, so v1 to v3 plugins keep their current wire.

- **`fix` request.** Adds `pr`.
- **`fix` answer.** Adds `answers`: exactly one entry for each `finding`
  problem, in request order. Each entry is
  `{"fixed": bool, "reply": nonblank}`.
- **`fix` result.** Gains `{"status":"unchanged"}`. It's valid only when no
  answer says `fixed`.
- **`judge` request.** Adds `pr` and `why`. `why` is a nonempty, unique list
  of the flow's judge triggers, in a closed vocabulary that names
  `flow::decide`'s conditions: R2, author engine or gated paths, hidden
  files, no CI, asked, past the round limit, major dependency.
  - It also adds `context`: the flow's digest of the review findings and
    their answers that the judgment is on. C1 adds the exported helper
    that computes it, `flow::judging_context`, so the flow and the judge
    producer compute it the same way.
  - `against` stays the description being judged.
  - The verdict words don't change.
- **`proof` request.** Adds `head`: the commit whose tree is `tree`.
- **New kind, `prepare`.** It's for proof plugins only. A plugin that
  declares a model may not advertise it, as with `proof`.
  - Request: `repo`, `head`, `tree`, `checks`.
  - Answer: `ready` (bool), `slot` (bool: whether proving needs a scarce
    slot) and a nonblank `why`.
  - It's never a verdict.

**Conformance fixtures.** One for every new field and kind, all on the
reserved repository.

**Negative verification.** Each case is a runner test that fails before
the change. The runner refuses:
- a fix answer with a missing, extra or reordered answer;
- `unchanged` alongside a fixed answer;
- a judge request with an empty or unknown `why`;
- a proof answer without `head`;
- `prepare` advertised by a model plugin, or without `proof`;
- `prepare` answering with results.

A v3 plugin keeps passing `cn plugin check`. Run the repository gate.

### C2: flow: give each finding a thread, and post a fix's answers on it

**Repo:** Conary. **Depends on:** C1.

**Crates:** `conary-github`: `api` (review submission with comments, and
thread replies and resolution), and `flow` (`write.rs`, `execute.rs`,
`reviews.rs`, `handoff.rs`, the fix's observation). `docs/flow.md`.

Today `Api::review` submits only a body, so a flow-published finding has no
thread to answer, and `Step::PostFix` fails. This task owns both halves.

**Identity.** Every finding target and every answer carries a marker
that names the review issuance and the finding inside it. The issuance is
named by the digest of its receipt, `receipt_digest(review_receipt(head,
body))`, which is unique per publication because the body holds the
findings and `reviewed_at`. The finding is named by its index in that
review marker's `problems`, and the role and head are named too. An answer
also names the fix invocation it came from. So a second review on the same
head, a reordered list, two findings at one place, or a carried review
never share a marker, and a retry keeps its own.

**Publishing (`PostReview`).** This uses only supported GitHub operations:
- The review is created in one request (`POST /pulls/{n}/reviews`), with a
  line comment for each finding whose line is in the diff's hunks, placed
  from the diff before submitting.
- Every other finding gets a marked PR comment (`POST
  /issues/{n}/comments`): a line outside the hunks, or a file with no
  usable line position or outside the diff. Such a finding has no thread
  to resolve.

Publication is complete only once every finding's target has been
observed. A lost or partial publication is reconciled by its markers
before anything is sent again: the review by its exact body, and each
fallback comment by its marker. Nothing is duplicated, and nothing counts
as published early. The review body and its receipt don't change.

**Reading threads.** `flow::reviews` matches each finding to its target by
the marker, including on a review carried from an earlier head, where the
thread stays on the original review. Handoff treats a recognised finding
thread as that finding, not as an unstructured blocking thread. A fix
problem carries its thread.

**Posting answers (`PostFix`).** For each finding problem it handed out,
the flow posts the answer's `reply` on that finding's target, as the
flow's review account, and resolves the thread if there is one.
- A lost write is reconciled by its exact body before it's posted again.
- An `unchanged` result posts replies and pushes nothing.
- The reviewers then look again, and the flow counts the round as it does
  today. This task adds no policy: [review-loop.md](review-loop.md) owns
  what the answers mean.

**Negative verification**, against the simulated GitHub with no threads
made in advance, each case failing before the change:
- The whole path runs: `PostReview` makes the threads, a fix answers, and
  `PostFix` replies on and resolves exactly those threads.
- Two findings on one line, a line outside the hunks, a file outside the
  diff, and a carried review each reach their own target.
- A second review on the same head, after an `unchanged` fix, never
  matches the first review's answers, including on fallback PR comments.
- A retried or partial publication makes no duplicate targets, and isn't
  complete until every target is observed.
- A reply is never posted on a thread other than its finding's.
- A reply for a head that moved isn't posted, and a repeated pass posts
  each reply once.
- A fix answer from a superseded invocation posts nothing.

Run the repository gate.

### C3: flow: prepare proofs outside the proof slots, and send no model-run check to a proof plugin

**Repo:** Conary. **Depends on:** C1.

**Crates:** `conary-github` `flow` (`proofs.rs`, `pass.rs`, `execute.rs`,
`jobs.rs`, `occupancy.rs`). `docs/flow.md`.

**Work.** This replaces the Evaluate/Prebuild/Build/Stored plans, which
nothing outside the simulation fills, with two steps. That old path is
deleted in the same PR.

- **`prepare`.** It holds no proof slot and isn't counted in
  `proof_slots`. Its deadline is `proof_timeout`. One runs per head and
  recipe set. It's cancelled when the head or a selected recipe changes,
  as burrow#518's prebuild is.
- **`proof`.** It holds a slot, unless its `prepare` answered
  `slot: false`. Queue order and the `first` label work as they do now.

A `Job` recipe is never sent to a proof plugin. Its check waits, with a
reason that says a model-run check isn't a proof. Whether wend's
`proofJobs` check becomes a `Run` recipe, or wend stays on the steward, is
#315's to decide before wend's cutover.

**Negative verification**, against the simulated GitHub, each case failing
before the change:
- While a `prepare` runs, another PR's proof takes the only slot.
- A `prepare` answer can't post or count a verdict.
- A changed head cancels its `prepare`.
- A `Job` recipe starts no proof worker.
- Dry runs reserve virtual slots only for `proof`.

Run the repository gate.

### C4: flow: bind a judgment to its reasons and the findings it read

**Repo:** Conary. **Depends on:** C1. Coordinates with
[#293](https://github.com/FieldmouseWorks/Conary/issues/293), which carries
a verdict across heads. This task is about one revision.

**Crates:** `conary-github` `flow` (`markers.rs`, `evidence.rs`,
`decide.rs`, `observe.rs`). `docs/flow.md`.

**Work.** Today `flow::decide::after_review` accepts any pass for the same
`Revision`. The sealed issuance and its receipt name neither why it was
judged nor what it read. So findings answered without a push, or a newly
applicable reason, can leave an old pass standing.
- A new issuance seals, and `judgment_receipt_by` binds, the `why` and
  `context` it was judged under. The issued format is versioned. Earlier
  issuances stay as they are, and their ordering by generation is
  unchanged.
- A pass or owner verdict counts only when two things hold: its `why`
  includes every reason the flow requires now, and its `context` equals
  `flow::judging_context` on the threads and answers now. Otherwise the
  flow asks for a new judgment before granting permission. The old
  issuance is never removed.
- An issuance without `why` and `context` counts as insufficient.

**Negative verification**, against the simulated GitHub with an
authenticated pass already present, each case failing before the change:
- A finding answered without a push on the same revision makes the flow
  ask for a new judgment.
- A newly required reason, such as reaching the round limit, makes the flow
  ask too.
- A matching issuance still counts.
- A forged or edited `why` or `context` fails the seal or the receipt.
- Generations stay ordered.

Run the repository gate.

### B1: briefs: one source for the review, fix and judge briefs

**Repo:** burrow. **Depends on:** nothing.

**Crates:** a new `crates/burrow-brief`, a library plus a `burrow-brief`
CLI, plus the callers that move onto it:
- `crates/burrow-upkeep` (its `brief.rs` moves into the new crate);
- `modules/autoland/tasks.py`.

**Work.** Move each brief's policy text into `burrow-brief`, as it is on
main when the task is written:
- the review brief;
- `MUST_FIX` and `FINDINGS`;
- the judge's criteria: `REVIEWED`, `PAST_LIMIT`, `DEP_SEEN`, the
  land/owner rule, and what counts as failing.

Each caller keeps only how it's answered. The steward's judge posts a
sealed comment. A plugin's engine answers JSON.

`tasks.py` and `burrow-upkeep` read the text through the crate (the CLI
for the Python), and their copies are deleted. This changes no wording.
burrow#958 edits `brief.rs`: whichever lands second rebases onto the
other, and the text lands once.

**Negative verification:**
- A parity test shows each caller's rendered brief is byte-identical to
  main's for fixed inputs.
- A test fails when any policy sentence of a brief appears as a literal
  outside `burrow-brief`. Show it failing on main, where `tasks.py` and
  `brief.rs` hold the text.

Run burrow's affected tests and gate.

### B2: producers: the shared plugin and producer kit

**Repo:** burrow. **Depends on:** nothing.

**Crates:** a new `crates/burrow-producer` (library).

It depends on Conary's `conary-core` and `conary-github` crates as a
Cargo git dependency, pinned by `Cargo.lock`, for:
- the contract types and fixtures;
- `attest::service::{Statement, sign}`;
- the flow's receipt helpers.

None of these formats is copied.

**What the kit provides:**
- **The plugin side.** It reads one request and answers `describe` for the
  kinds it's built for. It answers the conformance fixtures locally,
  forwards work over the socket, and writes one answer. It exits nonzero on
  any producer error.
- **The producer side.**
  - A Unix-socket server that accepts only the configured controller user,
    checked by `SO_PEERCRED`.
  - Connection-scoped work: a closed connection stops the work's process
    group or unit, and publishes nothing.
  - Records kept for publication and recovery only, never returned as an
    answer to a later request.
  - A durable record of results, written atomically, kept beside the work.
  - Reconciliation at startup.
- **The signer.** It loads the key only from `$CREDENTIALS_DIRECTORY`. It
  exposes typed `review`, `judgment` and `proof` signing calls, each built
  from that producer's own record, and no call that takes a statement.
- **Publication.** It creates and completes the check run, with the
  statement as `output.text`, and completes it as `cancelled` when the
  work is abandoned.

**Negative verification:**
- Statements signed with a test key verify through Conary's verifier.
- The same statement is refused once its check, tree or log is altered.
- A closed connection leaves no running work and no `success` check run
  (fake GitHub).
- A key offered through the environment or arguments is refused.
- A connection from any user but the controller is refused.
- A repeated request runs new work, and its answer never carries a
  recorded result or its completion time.
- The crate's public API has no function that signs a caller-supplied
  statement, checked by an API-surface test.

Run burrow's gate.

### B3: ledger: run the flow's plugins as a controller user no authoring session shares

**Repo:** burrow. **Depends on:** nothing.

**Crates:** `modules/ledger.nix`, the flow's worker unit, and its check in
`checks/`.

conaryd runs as `burrow.user`, the user authoring jobs run as. A producer
that trusted that user would trust every authoring session. This task adds
a controller system user and runs the flow's passes and workers, and so
its plugins, as that user. It keeps access to the ledger for `cn` and the
steward through the socket directory's group.
- The controller user owns the flow's data directory.
- No authoring job, sandbox or `proj` run may run as it.
- It's behind `burrow.ledger.controllerUser`, off by default. #314 turns it
  on.

**Negative verification**, in the module's VM check:
- With the option on, a process running as `burrow.user` can't open a
  producer's socket. A flow worker can.
- `cn` still reads the ledger.
- With the option off, nothing changes.
- The first case fails before the change.

Run burrow's gate.

### R: review plugin and producer

**Repo:** burrow. **Depends on:** B1 and B2. It needs no contract change.

**Crate:** `crates/burrow-review`. It gains the `burrow-review-plugin`
binary and a signing path. It keeps its existing sandbox, egress bridge and
login handling. It's the one implementation of those, shared with the
steward's queue until #316 removes the queue.

**Request.** `review {repo, pr, base, head, author}`, with
`execution {choice, context: fresh}`.

- The producer checks `head` and `base` against the PR, and records its
  input: repo, PR, role, head, base, the description's fingerprint as
  `flow::fingerprint` takes it, and a digest of the earlier findings and
  answers it quotes.
- It builds the look from B1's review brief: the base's rules, the quoted
  description, and earlier findings with their replies, read from GitHub.
- It runs the engine in a fresh session. It doesn't resume the PR's
  session: the flow asks for `fresh`, and the answer must report the
  context it got.

**Answer.**
- `findings`: each one's `path` and `line`, `severity` lowercased (`p0` to
  `p2`), and `text` built as title, body, then "Covers:" and the covers
  field.
- `by`: the engine, and the engine's own session ID.
- `execution`: the model and effort that actually ran.

**Signing.** The flow publishes the review. When the producer sees that
review on the head (burrow-events' `pull_request_review`, plus a periodic
sweep), it checks the whole published record against its own:
- the review was posted by a `review_authors` account, on the recorded
  head;
- the `ROLE-review:v2` marker names the recorded role, `commit_sha`,
  `base` and `scope`;
- its `problems` equal the recorded findings, in order, and its `findings`
  count equals their number;
- `reviewed_at` is no earlier than the producer finished the look and no
  later than now;
- the PR's description and the quoted history still have the recorded
  digests.

Only then does it sign `review_receipt(head, body)` as
`conary/review/ROLE`. A published review that differs in any of these is
never signed.

**Restart and cancellation.** As in B2. A cancelled look records nothing.
Signing is idempotent per published review.

**Negative verification:**
- `cn plugin check` passes, and the conformance check runs in burrow's
  tests through Conary's `Plugin` and `fixtures()`.
- A request for a head that isn't the PR's head is refused.
- A request whose author session equals the producer's session is refused.
- A published body is never signed when it has a finding removed or
  added, a `findings` count of zero against recorded findings, another
  `base` or `scope`, a `reviewed_at` before the look ended, or a
  description edited since the look.
- Every request runs a new look. A request after a description changed
  and changed back still runs a new look, with its own `reviewed_at`.
- An old statement fails Conary's verifier against an edited body.
- Each case fails before the change.

**Unit.** `autoland-review` gains its credential and the plugin export,
behind `burrow.autoland.plugins.review.enable`, off by default.

### F: fix plugin

**Repo:** burrow. **Depends on:** C1, B1, B2 and
[burrow#969](https://github.com/FieldmouseWorks/burrow/issues/969). C2 is
needed before cutover, not before this PR.

**Crate:** a new `crates/burrow-fix`, with the plugin binary and the
broker service. The broker uses B2's connection-scoped work and record. It
has no signer and no key.

**Request.** `fix {repo, pr, head, problems}`, with
`execution {choice, context}`.

- The broker checks `head` against the PR's current head.
- It starts the fix as a job through the existing job runner, on the
  fixing role from the role map. The runner keeps job slots, engines,
  sandboxes, the acting App and usage overflow.
- It waits on that job and stops it through the job's record
  (`proj state --json`), which says when the job is overdue. It never
  reads `proj lifecycle`'s lines and starts no `--owned` job: both go with
  the steward
  ([proj's design](https://github.com/FieldmouseWorks/burrow/blob/main/docs/proj-design.md#conary)).
  burrow#969 delivers that record.
- The job gets the problems in order, with B1's fix brief. It writes one
  answer per finding problem, by index.
- At most one live fix runs per PR.

**Answer.**
- `result`: `pushed` with the head the PR actually has after the push,
  checked against GitHub; or `unchanged`; or `unable` with the reason.
- `answers`: one per finding problem.
- `by`: the job's engine and session.
- `execution`: what ran.

**Signing.** None (see [Who produces what](#who-produces-what)). The job
runs with no access to any producer's key or socket credential.

**Restart and cancellation.** The connection closing stops the job. A
broker restart stops jobs whose connection it lost. A push that already
happened stands, and the next pass reads the head that moved.

**Negative verification:**
- `cn plugin check` passes.
- A reported head that doesn't match the PR is refused.
- Answers that don't match the problems are refused.
- A second concurrent fix for one PR is refused.
- A cancelled request leaves no running job.
- Each case fails before the change.

**Unit.** Behind `burrow.autoland.plugins.fix.enable`, off by default.

### J: judge plugin and producer

**Repo:** burrow. **Depends on:** C1, C4, B1 and B2.

**Crate:** a new `crates/burrow-judge`, with the plugin binary and the
`autoland-judge` producer service.

**Request.** `judge {repo, pr, head, against, author, reviewers, why, context}`,
with `execution {choice, context: fresh}`.

- The producer reads the PR. The head, base and description must match
  the request and `against`. Only the flow can send the request (B3), so
  `why` is the flow's.
- `why` only adds to what's judged, never removes. The judge always reads
  every review thread and its answer, whatever `why` says.
- It allocates the next generation for this repo and PR and persists it
  before publishing anything.
- It runs the judging engine in a fresh session, on B1's judge brief for
  the given `why`, in a read-only checkout of the head. It runs the build
  and tests itself when `why` includes no CI.

**Independence.** Before it publishes, it refuses to judge if its session
equals the author's or any reviewer's. It uses Conary's own check, not a
copy.

**Answer.**
- `verdict`: one of #303's words.
- `why`, `by` and `execution`.

**Publication and signing.**
1. The sealed comment: `SealKey::judgment_by(repo, pr, verdict, revision,
   generation, provenance)`, with the `why` text, posted as a
   `verdict_authors` account.
2. Then `conary/judgment` over
   `judgment_receipt_by(revision, generation, verdict, provenance)`.

Every request is a new judgment under a new generation. The issuance
carries the request's `why` and `context` ([C4](#c4-flow-bind-a-judgment-to-its-reasons-and-the-findings-it-read)).
The producer recomputes `context` from GitHub, with Conary's own helper,
and refuses to judge if it differs from the request's.

**Restart and cancellation.**
- A closed connection stops the engine session.
- An issuance whose comment was posted is completed, with its check run, at
  startup reconciliation. It's never withdrawn.
- A generation persisted with nothing published is burned. The next
  request takes a new one.

**Negative verification:**
- `cn plugin check` passes.
- A judge session equal to the author's or a reviewer's is refused.
- A stale head, base or description is refused.
- A request whose `context` differs from the producer's own reading of
  the threads is refused.
- The sealed issuance and its receipt carry the request's `why` and
  `context`.
- A brief built for a `why` without "no CI" still includes every thread and
  answer.
- The sealed comment and its statement round-trip through Conary's public
  `SealKey` and service verifier. A statement for another generation,
  verdict or revision fails that verifier.
- A restart between the comment and the check run publishes the check run
  exactly once.
- Generations never repeat.
- Each case fails before the change.

**Unit.** Behind `burrow.autoland.plugins.judge.enable`, off by default.

### P: proof plugin and producer

**Repo:** burrow. **Depends on:** C1 and B2. C3 is needed before cutover,
not before this PR.

**Crate:** `crates/burrow-proof`. It gains the `burrow-proof-plugin` binary
and the `autoland-proof` producer service. It keeps `start`, `run` and
`end`, with their cgroups and `end.json`.

**Describe.** No models. Kinds `proof` and `prepare`. Each check's recipe
is read from the machine's flow settings (`settings.proofs`), never from
the request or the PR.

**`prepare {repo, head, tree, checks}`:**
- The producer checks that `tree` is `head`'s tree.
- **`Build` recipe.** It evaluates the derivation on the head and builds
  its inputs in a machine heavy slot. This is burrow#518's prebuild. It
  answers `slot: false` when every output is already valid in the store.
- **`Run` recipe.** It prepares the fresh worktree.
- Otherwise it answers `ready: true`.

**`proof {repo, head, tree, checks}`:**
- **`Build` recipe.** It realises the derivation.
- **`Run` recipe.** It runs `burrow-proof run` in its cgroup.
- The verdict is the program's recorded exit.
- A `Job` recipe is refused as a plugin failure.

**Records.** A run is recorded per repo, head, check and recipe digest. A
statement is signed only for the commit its run checked out. Another
commit with the same tree, such as a train candidate, gets its own run,
which for a `Build` recipe with valid outputs finishes at once. Reusing a
pass across commits is Conary's consumer path (#291), never re-signing.

**Signing.** On a program's recorded exit, it signs `{producer, repo,
revision {base: null, head}, tree, check, definition:
recipe_digest(recipe), result, log sha256}`. It posts the check run on
`head`, with conclusion `success` for a pass and `failure` for a fail, and
keeps the statement beside `end.json`.

**Restart and cancellation.** Cancelled or restarted work signs no pass. A
killed run signs no fail either, unless the program recorded its exit. The
started check run is completed as `cancelled`.

**Negative verification:**
- `cn plugin check` passes.
- A `tree` that isn't `head`'s tree is refused.
- A `Job` recipe is refused.
- Describe with a model fails.
- A recipe changed after signing makes the statement fail Conary's
  `definition` check.
- A killed run leaves no pass statement and no `success` run.
- A `prepare` never posts a check run.
- A request for a second commit with the same tree runs again and is
  signed for that commit. The first commit's statement is never re-issued
  under the second commit.
- Each case fails before the change.

These cover [#218's requirements of this producer](https://github.com/FieldmouseWorks/Conary/pull/338)
(its P2). The train's candidates ask this same plugin, with `head` set to
the candidate commit (#338's P3).

**Unit.** Behind `burrow.autoland.plugins.proof.enable`, off by default.

## Order

```
C1 ──► C2
C1 ──► C3
C1 ──► C4 ──► J
B3 ──────► #314
B1 ──┐
B2 ──┼──► R
C1 ──┼──► F   (C2 before cutover; burrow#969 before F)
     ├──► J
     └──► P   (C3 before cutover; P doesn't need B1)
R, F, J, P, C2, C3, C4, B3, and key provisioning ──► #313 ──► #314
```

C1, B1, B2 and B3 can start at once. R starts once B1 and B2 land. F, J
and P start once C1 and B2 land, plus B1 for F and J, C4 for J, and
burrow#969 for F. C2 and C3 run alongside the plugins.

This is the order these tasks need of each other.
[m6-order.md](m6-order.md) puts them beside burrow's proj and gate
redesigns, which share burrow's files with the seven burrow tasks, and
says which may be open together.

## Covered elsewhere, not scheduled again

- **The contract and the verdict vocabulary.** #295 owns the contract and
  #303 the verdict words. C1 is the only change to the contract.
- **Service evidence.** #309 built its format and verifier (#344). No task
  changes them.
- **The review-loop policy.** Its changes stay with their burrow issues,
  per [review-loop.md](review-loop.md). Porting the burrow#945 triggers,
  and the disputed-P2 switch, into the flow is #312's to schedule.
- **A judge's verdict on a head whose change didn't move.** That's #293.
- **Per-repository landing ownership and the switch.** That's #313 and
  #314, and #315 for the other repositories, including wend's `proofJobs`
  check.
- **The train's proof of candidates and status binding.** That's #338's
  P1 and P3, which consume P.
- **Negative-run evidence (`attest / negative`)** stays with CI.
- **Dependency-sweep decisions (`conary/major`)** and the upkeep the
  steward does outside the landing flow belong to #316.
- **The stuck role's job** is burrow#945 and burrow#961 on the steward,
  then the flow's `job` kind.

## Filing

Once this plan lands, each task is filed as an issue in its repository:

- The issue title starts with the task's ID.
- The issue body links its section here and says
  `Refs FieldmouseWorks/Conary#311` and `#256`.
- The issue body says its PR carries the `first` label.

An issue grants no authority beyond #256's.
