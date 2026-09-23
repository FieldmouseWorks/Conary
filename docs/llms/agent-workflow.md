---
last_updated: 2026-09-23
revision: 1
summary: Define scoped agent execution, task graphs, evidence, authorization, resumption, and closeout for Conary work
---

# Agent Execution Workflow

This page owns the procedure for assisted, multi-step execution. `AGENTS.md`
owns concise project policy; `CONTRIBUTING.md` owns the contribution lifecycle;
the primary issue or established handoff owns live task state; the PR owns the
proposed diff, review, and verification receipts. `ROADMAP.md` and
`docs/roadmaps/` remain the project-state owners. This is manual guidance for
people and agents; it does not add an executor, framework, CI workflow,
automatic dispatch, restart, or enforcement.

## Scope Before Edits

Before changing files, observe the current success state and record the
requested outcome, its acceptance boundary, and known exclusions. Inspect the
smallest relevant context: user request and existing authorization, repository
instructions, routed owner packet, current issue/PR or handoff, current goal,
branch and dirty work, and required CI or proof rules. Preserve other people's
changes. Do not copy live status or raw output into durable orientation docs.

A simple, independent task gets a short plan. Substantive work with dependent
steps uses one canonical graph in its primary issue body; if an established
handoff already owns that graph, keep it there and link it from the issue and
PR. A PR links evidence and review but does not create a competing task graph.
Graph changes and ready-task selection remain with the primary integrator,
using the `gpt-6-astra` role when available.

The [workflow setup issue](https://github.com/FieldmouseWorks/Conary/issues/1063)
provides a worked graph with its own authorization, effort policy, and evidence.
Use closed work as historical evidence; each new outcome records its own scope.

Each graph node records:

- stable ID and concrete outcome;
- dependencies, model/effort, accountable owner, and file or task ownership;
- input references and an observable acceptance check;
- state, evidence locator, and bounded effort or repair policy.

Use these states consistently: **ready** means dependencies are verified and
the node can start; **working** means one named owner is doing it; **blocked**
means a specific missing condition prevents progress; **verified** means the
parent reviewed the actual artifact and acceptance evidence. A worker's
completion claim is a pointer to review, not verification. Do not unlock a
dependent node until its parent verifies the dependency. On batch failure,
inspect every completed child and keep independent results.

`AGENTS.md` defines the requested default roles and effort. Dispatch only
useful, bounded work; state exact inputs, acceptance, model and effort, owner,
and file ownership. Confirm concurrent owners do not overlap and tell them to
preserve existing edits. The primary integrator owns architecture, graph
changes, selection, review, and integration. If a requested model is
unavailable, report it without silent substitution; do not claim a model or
effort that the harness cannot show. DeepSeek is an optional supervised helper
only when active project/session authorization permits it; honor an existing
pause until explicitly re-enabled. If enabled, it never owns architecture or
integration. Record why it helps, exact model, independent verification,
results, corrections, and reliability in the existing work record. Do not
create evaluation-only tasks.

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
would distinguish failure from success; do not add checks that only mirror the
implementation. Routine bounded work defaults to at most two scoped repair
attempts per causal defect. After the second attempt, the parent reassesses
evidence, acceptance, and remaining scope, then records the next decision in
the same task record. Diagnose before repairing; do not repeat a failed command
against the same candidate and inputs. After a causal repair changes the
candidate or relevant inputs, rerun the affected proof and record a new receipt.
Do not reset the effort limit. User or harness limits take precedence. A
blocker names the missing condition and one next action; it does not silently
expand scope, effort, or authority.

## Work, Evidence, And Feedback

Choose the cheapest existing proof that establishes the acceptance property.
Prepare any missing project tool through authorized setup or isolated
dependencies. Run independent checks concurrently only when inputs and mutable
resources are isolated. When both local and hosted checks are ready and the PR
is authorized, they may run concurrently; separate their outputs, ports,
databases, and other mutable state. Keep Cargo targets isolated per worktree;
the shared development-build guidance in `CONTRIBUTING.md` explains which
compiler outputs may be reused.

Before expensive gates, freeze the exact reviewed candidate revision or tree.
Capture receipts from actual artifacts: command, commit/tree identity or
relevant input hash, timing when available, exit status, and output locator.
Link source claims to original or pinned documentation and source with a
locator; tie implementation claims to the exact revision and its actual proof
receipts. Review and redact raw evidence before sharing it; retain failures
with their causal leaf instead of replacing them with a later green run. A
relevant source, test, document, or gate-input change invalidates the affected
receipt.
Report local, default-branch, browser, and hosted results separately and state
their scope.
Label self-review and independent review honestly. Claim a speedup only from
completed measurements of comparable commands and environments.

When a check fails, inspect its exact revision, command, output, and affected
inputs, then create a scoped repair node in the same graph. Preserve successful
independent checks. A check against changed inputs is a new receipt, not a
rewrite of the earlier result. Never rerun the same failure against an
unchanged candidate and inputs.

If work is interrupted, resume by checking running processes, current branch
and revisions, dirty work, graph state, and receipt freshness before starting
anything again. Continue from the recorded next action; do not assume a prior
command is still running or restart work whose artifact is already present.
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
branches, processes, and temporary resources owned by this task.
If a merge, publication, or cleanup action is outside recorded authorization,
leave it as a concrete pending action and ask for that specific authority.

Close an outcome only after its scoped acceptance is verified, its authorized
integration is complete, and its evidence has been read back from the canonical
record. Archive the completed graph in the issue or PR closeout/history after
its truth and resume facts are recorded; then leave one next action. Do not
keep a second active tracker or start the next outcome while the previous graph
still claims live work.
