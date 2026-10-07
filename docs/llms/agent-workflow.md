---
last_updated: 2026-10-07
revision: 10
summary: Define guided and bounded unattended execution, task graphs, evidence, authorization, recovery, and closeout for Conary work
---

# Agent Execution Workflow

This page owns the procedure for guided and bounded unattended, multi-step
execution. `AGENTS.md` owns concise project policy; `CONTRIBUTING.md` owns the
contribution lifecycle; the primary issue or established handoff owns live
task state; the PR owns the proposed diff, review, and verification receipts.
For runner-backed work, the issue body gives the graph overview and indexes
node comments; each indexed comment owns its live node state and dependency
prose.
`ROADMAP.md` and
`docs/roadmaps/` remain the project-state owners. Guided work follows this
procedure directly. Bounded unattended work uses the validated run envelope
and controller documented in [Agent Runner](agent-runner.md); only behavior
implemented and exercised by that controller may be described as automated.
Guided work may keep its graph in the primary issue or established handoff.
Bounded runner work uses the indexed issue comments described below.

## Execution Modes

**Guided mode** is the default: a primary integrator selects and advances graph
nodes, reviews results, and resumes work. Existing read-only CI intake may
surface failures; it does not select product work or dispatch a repair.

**Bounded unattended mode** requires a versioned, validated run envelope that
records the original authorization source and a finite run boundary. The
envelope points to one primary issue and its canonical graph, defines a finite
ordered candidate list and exclusions, names allowed remote writes, pins the
model and effort for this run, and supplies finite wall-clock and causal repair
limits. The issue body is a graph overview and contains an index marker for
each queued graph comment. The indexed comment is canonical for live node state
and dependency prose; its marker binds the task ID, state, base revision, and
acceptance hash. The controller trusts the integrator-authored `ready` marker
after dependency verification and does not parse dependency edges. The run
envelope also records the prompt hash, allowed local paths, acceptance
commands, and required hosted checks. The controller's exclusive local lock
limits a run to one active slice. The envelope records the source of
authority; it does not create authority. A task issue or runner config cannot
grant permission the user or session did not grant. Schema v1 sets `merge` to
false; merge and cleanup follow the separately recorded task authorization
and repository lifecycle rules.

The finite queue is an ordered set of candidate alternatives for one run, not
a sequence of slices. One `run_id` journals at most one task. A later graph
node needs its own validated envelope and new `run_id` after the prior task's
canonical record is updated and its next action is authorized.

The runner claims at most one eligible slice at a time. The indexed graph
comment owns the live node state and dependency prose; separate run checkpoint
comments record phases and evidence digests. Runner state is limited to lease
and recovery pointers such as the claimed node, attempt, worktree, candidate
revision, pull request and check identities, and the next reconciliation
action. It must not copy or replace the graph. A restart first reconciles those
pointers with the local repository and remote state. Unknown side effects,
expired or conflicting ownership, changed authorization, malformed graph or
receipt, and any head or tree mismatch stop the run. Remote writes are never
blindly replayed after interruption; inspect or read back the remote result
before deciding whether a write is still needed.

Unattended execution never weakens the normal review and merge gates. A draft
pull request must identify the exact candidate. Independent review, fresh
candidate-bound acceptance evidence, all required hosted checks for that head,
resolved review threads, and explicit merge-path authorization are required
before protected merge. Runner v1's read-only promotion check binds worker and
reviewer evidence, exact-head checks, live rules, resolved threads, and the PR
test-merge tree and parents to that candidate; it does not create a GitHub
approval, mark the draft ready, or merge. After merge, verify the merged tree
against the reviewed candidate, read the canonical issue/PR record back, and
clean up only resources owned by the run. If any gate is unknown or unavailable,
stop with a recoverable pointer and a concrete next action.

See [Agent Runner](agent-runner.md) for the runner's current interface, exact
stop classes, limits, state location, and exercised capabilities. Do not infer
that a local intake timer, a successful process launch, or an open pull request
means the workflow is scheduled, resumed, or complete.

## Scope Before Edits

Before changing files, observe the current success state and record the
requested outcome, its acceptance boundary, and known exclusions. Inspect the
smallest relevant context: user request and existing authorization, repository
instructions, routed owner packet, current issue/PR or handoff, current goal,
branch and dirty work, and required CI or proof rules. Preserve other people's
changes. Do not copy live status or raw output into durable orientation docs.

Record the continuation boundary: setup only, this assigned outcome, or a
specific explicitly authorized queue. Continue ready, authorized nodes inside
it without re-asking, then stop when that boundary is complete. Child nodes may
inherit common inputs and the sourced effort policy from their parent graph;
record those references instead of copying them.

For bounded unattended execution, validate the run envelope before claiming
work. It must cite the source of authorization and its expiry, identify the
primary issue and finite ordered candidate queue, name excluded work, list
permitted remote writes, and bind each candidate to its graph entry, owner,
branch, base revision, prompt hash, allowlisted paths, acceptance commands,
and required hosted checks. It also sets the model and effort for this run,
one active slice, a wall deadline, and a causal repair cap. Schema v1 sets
`merge: false`; merge authority must be separately sourced and verified before
any other protected merge path. A missing, expired, or contradictory field
fails validation before dispatch. Model choice remains session or run policy;
it does not create a repository model requirement.

A simple, independent task gets a short plan. Substantive work with dependent
steps uses one canonical graph in its primary issue or established handoff. In
bounded runner mode, the issue body provides an overview and index to canonical
node comments; each comment holds the live node state and dependency prose. A
PR links evidence and review but does not create a competing task graph. The
primary integrator authors the graph and any unattended queue and ordering. In
guided mode the primary selects ready nodes; in bounded mode the controller
selects only from the validated finite queue and trusts the indexed node's
integrator-authored `ready` marker. It does not interpret dependency edges.

The [workflow setup issue](https://github.com/FieldmouseWorks/Conary/issues/1063)
provides a worked graph with its authorization, effort policy, and evidence.
The completed setup graph is historical evidence; each new outcome records its
current scope and effort policy. Historical effort limits do not carry forward.

Each graph node records:

- stable ID and concrete outcome;
- dependencies, model/effort, accountable owner, and file or task ownership;
- input references and an observable acceptance check;
- state, evidence locator, and applicable effort or repair policy, including
  any explicit user or harness budget or waiver.

Use these states consistently: **pending** means dependencies are not yet
verified; **ready** means dependencies are verified and the node can start;
**working** means one named owner is doing it; **blocked** names specific
missing evidence, authority, or another condition; **verified** means the
parent reviewed the actual artifact and acceptance evidence. A worker's
completion claim is a pointer to review, not verification. Do not unlock a
dependent node until its parent verifies the dependency. On batch failure,
inspect every completed child and keep independent results.

Conary names no required model or agent tool; each contributor's session
chooses its own. Guidance speaks of roles, and a session maps them to whatever
it runs: the **integrator** owns the graph, selection, review, and integration;
an **implementer** makes a judgment-heavy change from a fixed brief; a
**reviewer** reads an exact candidate independently and edits nothing; a
**worker** does bounded work a command can accept; a **verifier** runs named
proof and writes receipts; a **scout** answers read-only questions of fact.
Route by how a node's acceptance is decided, not by how hard it looks.
Dispatch only useful, bounded work; state exact inputs,
acceptance, owner, and file ownership. Confirm concurrent owners do not overlap
and tell them to preserve existing edits. The primary integrator owns
architecture, graph changes, queue and selection policy, review, and
integration. In bounded mode, the controller applies only the recorded
selection policy; delegated helpers never alter it. If a requested model is
unavailable, report it without silent substitution; do not claim a model or
effort that the harness cannot show.
Record which role did delegated work, its independent verification, results,
and corrections in the existing work record; model identifiers stay out of
tracked records. Do not create evaluation-only
tasks.

## Read-Only Intake

The optional [GitHub failure intake](agent-intake.md) selects new failures for
current main and open pull-request heads. Its first run is a quiet baseline,
and newer workflow results fence old failures. Scheduling is host-local; the
selector writes local reports and does not dispatch graph nodes or implementation.

## Authorization And Effort

User and session instructions authorize actions; an issue or repository rule
can narrow scope but cannot grant permission beyond that authorization. Before
external or consequential steps, the task record states the exact source and
scope, remote writes, merge path, cleanup ownership, deployment or publication,
paid services or accounts, and protected source inputs or secrets included.
An authorization from another repository grants nothing here. Do not ask again
when the same action and scope are already authorized. The workflow itself is
not a user waiver and grants no permission.

Proceed with independent, authorized work when another action is blocked. If a
real authorization gap prevents completion, finish all work that does not
depend on it, prepare the concrete reviewable result, then ask only about the
specific missing action. Do not infer permission for a bypass, direct push to
`main`, unrelated work, deployment, release, publication, a new paid service,
or changes to protected inputs or secrets.

Use the existing focused proof first. For a new correctness check, record the
concrete observed defect or property and a meaningful negative control that
would distinguish failure from success; do not add checks that only mirror
the implementation. In guided mode, there is no default numeric
repair-attempt limit; continue evidence-driven repairs within the authorized
outcome until acceptance is verified or a specific blocker prevents progress.
Bounded unattended mode requires a finite wall-clock deadline and causal repair
cap in its envelope. Neither mode may reset an explicit user or harness limit.

Classify failures before taking another action. A required check failure keeps
its original receipt and blocks dependent work; diagnose it and create a
bounded repair node. A review finding blocks merge until a concrete repair and
independent re-review pass. A stale or candidate-unbound receipt is not
acceptance evidence; obtain a new receipt only after binding it to the exact
candidate and inputs. `action_required`, missing approval, unknown status,
malformed state, or uncertain remote-write outcome stops for human review.
An optional image acquisition failure before product tests is never a
product-test pass. Runner v1 recognizes one exact openSUSE cache-image failure
shape for diagnosis; it still blocks the run and promotion. Every other failed
optional hosted check is a hosted failure. See [Agent Runner](agent-runner.md)
for the classifier's exact conditions.
Never rerun an unchanged failure against the same candidate and inputs. After
a causal repair changes the candidate or relevant inputs, rerun the affected
proof and record a new receipt. A blocker names the missing condition and one
next action; it does not silently expand scope or authority.

## Work, Evidence, And Feedback

Choose the cheapest existing proof that establishes the acceptance property.
Prepare any missing project tool through authorized setup or isolated
dependencies. Run independent checks concurrently only when inputs and mutable
resources are isolated. When both local and hosted checks are ready and the PR
is authorized, they may run concurrently; separate their outputs, ports,
databases, and other mutable state. Keep Cargo targets isolated per worktree;
the shared development-build guidance in `CONTRIBUTING.md` explains which
compiler outputs may be reused.

Verify through repository entry points: `scripts/*` (for example
`scripts/dev-build.sh` or `scripts/agent-context.sh --feature <slug> --run
focused`), `conary-test` suites, and package tests. Do not use ad-hoc scripts
written for one session. A check needed twice belongs in `scripts/` or a test
with its own proof, so every later session runs the same verification. Local
proof must use the same commands, features, and flags CI runs; see the
[Running Tests section](../../CONTRIBUTING.md#running-tests).

Before expensive gates, freeze the exact reviewed candidate revision or tree.
Capture receipts from actual artifacts: command, commit/tree identity or
relevant input hash, timing when available, exit status, and output locator.
Link source claims to original or pinned documentation and source with a
locator; tie implementation claims to the exact revision and its actual proof
receipts. Review and redact raw evidence before sharing it; retain failures
with their causal leaf instead of replacing them with a later green run. A
relevant source, test, document, or gate-input change invalidates the affected
receipt.

For an optional local receipt around one canonical command, run
`python3 scripts/agent-proof.py -- <command> <arguments...>`. It records the
wrapped command's argv, exit code or signal, and stdout/stderr logs under the
ignored `target/agent-proof/<run-id>/` directory. The candidate snapshot binds
`HEAD`, the Git index, and tracked plus nonignored untracked working-tree files,
including existing dirty edits, before and after the command. A receipt can
report `passed` only when that snapshot is unchanged and the process exits zero;
a changed candidate is reported stale on normal completion.
Ignored files, external tools, and the environment are outside that snapshot.
This is local process evidence, not a CI receipt or environment attestation.
The helper verifies saved log identities and hashes before writing its receipt;
these user-writable files are not sealed. Before reusing a receipt, recheck
candidate freshness and each regular log file's size and SHA-256 against it.
The helper records the process it directly runs: when exact leaf status matters,
wrap the packet's canonical command itself. `agent-context.sh --run` executes
commands sequentially and reports an inner failure as wrapper exit 1, so a
receipt around it cannot recover the inner command's exact process status.

Report local, default-branch, browser, and hosted results separately and state
their scope.
Label self-review and independent review honestly. Claim a speedup only from
completed measurements of comparable commands and environments.

When a check fails, inspect its exact revision, command, output, and affected
inputs, then create a scoped repair node in the same graph. Preserve successful
independent checks. A check against changed inputs is a new receipt, not a
rewrite of the earlier result. Never rerun the same failure against an
unchanged candidate and inputs.

If guided work is interrupted, resume by checking running processes, current
branch and revisions, dirty work, graph state, and receipt freshness before
starting anything again. Continue from the recorded next action; do not assume
a prior command is still running or restart work whose artifact is already
present. For bounded runs, reacquire the repository-wide controller lock and
reconcile the private journal pointers with the canonical issue marker, local
and remote branch, PR, candidate head, and check IDs. A journal at
`worker_started` is an explicit `unknown_outcome` stop: inspect its trace and
worktree, then require a human to choose any recovery action. The controller
appends a `blocked` issue checkpoint with the inspection action when the task
can be identified safely. That checkpoint requires manual reconciliation; no
later invocation resumes the same task automatically, and the controller does
not relaunch the child. For accepted, non-blocked phases, another invocation
may continue only after reconciliation under the same unexpired authorization
and run budget. Unknown side effects, conflicting ownership, or changed
authorization stop for human review; never blindly replay a remote write.
Record an observed workflow defect in the same issue as its problem, smallest
proposed change, review, and proof. This does not reset task scope, effort, or
permission.

## Review And Closeout

The parent reviews actual diffs, artifacts, and proof receipts, including every
completed delegated node. Resolve review conversations and follow the existing
protected PR policy in `CONTRIBUTING.md`. This workflow adds no bypass
authority or relaxation of required gates; any exception follows that policy
and retains its reason and replacement proof. Verify the exact merged revision
on `main`, read back the issue/PR evidence from its owner, and clean up only
branches, processes, and temporary resources owned by this task. The stash
stack is shared by every worktree of a repository; do not use `git stash` to
set work aside in a linked worktree. Commit to the task branch or use a separate
worktree.
If a merge, publication, or cleanup action is outside recorded authorization,
leave it as a concrete pending action and ask for that specific authority.

Close an outcome only after its scoped acceptance is verified, its authorized
integration is complete, and its evidence has been read back from the canonical
record. Archive the completed graph in the issue or PR closeout/history after
its truth and resume facts are recorded; then leave one next action. Do not
keep a second active tracker or start the next outcome while the previous graph
still claims live work.
