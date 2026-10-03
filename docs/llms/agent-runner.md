---
last_updated: 2026-10-03
revision: 2
summary: Specify the validated one-slice agent runner, indexed graph comments, recovery journal, and read-only promotion check
---

# Bounded Agent Runner

`scripts/agent-runner.py` runs one pre-authorized graph task per invocation.
For runner-backed work, the primary issue body gives the graph overview and
indexes its node comments. Each referenced comment owns the live node state
and dependency prose. The run envelope fixes the scope and authorization for
that invocation. The runner's private journal stores recovery metadata and
evidence locators; it is not a second graph or source of authorization.

The runner is invoked manually. There is no runner timer, queue service, or
automatic restart schedule. The separate [GitHub intake](agent-intake.md)
continues to use GET-only requests and does not dispatch work. Version 1 never
merges a pull request; `promotion-check` is read-only.

## Commands

Keep the versioned run specification and prompt outside the tracked checkout.
Both are user-owned regular files with mode `0600`; the specification contains
authorization references and local paths, never tokens, passwords, or private
keys. The validator rejects missing or extra schema fields, duplicate JSON
object keys, expired authority, and non-normalized local paths.

```bash
python3 scripts/agent-runner.py validate --spec /private/path/run-v1.json
python3 scripts/agent-runner.py dry-run --spec /private/path/run-v1.json
python3 scripts/agent-runner.py run --spec /private/path/run-v1.json
python3 scripts/agent-runner.py promotion-check --spec /private/path/run-v1.json --pr 123
```

`validate` checks the strict schema, expiry, and path shape. `dry-run` reads the
issue, branch, and pull-request state, verifies the selected prompt, UTF-8
encoding, and hash, and selects the one ready task; it launches no model and
makes no remote writes. `run` checks the prompt, UTF-8 encoding, and required
private model-auth file before its first remote write, then claims and executes
that task within its recorded bounds. `promotion-check` reads the specified
pull request and reports a JSON status with reasons; it does not merge or
change issue, pull-request, branch, or run-journal state. Every command except
`validate` takes the repository-wide lock in the shared Git common directory.
The lock file may be created by
`dry-run` or `promotion-check` too.

## Run Envelope Version 1

Version 1 accepts a finite ordered queue of 1 to 20 candidate tasks. The queue
lists first-claim alternatives, not a serial multi-slice plan. Each invocation
claims at most one ready task, choosing the first eligible entry in the
declared order, and its `run_id` journal binds to that one task. To run the next
slice, prepare a new envelope with a new `run_id` after updating and verifying
the graph. The runner does not discover or invent additional product work.
The strict JSON envelope records:

- `schema_version`, `run_id`, `repository`, `issue_number`, and
  `controller_actor`: version `1`, a unique run identity, canonical repository,
  primary issue (`1070` for the current migration pilot), and the authenticated
  GitHub account that performs controller writes.
- `authorization`: source, a UTC `expires_at` ending in `Z`, `remote_writes`
  chosen from `create_branch`, `update_branch`, `comment_issue`, and
  `create_draft_pr`, and `merge: false`. Schema v1 rejects merge authorization.
- `exclusions`: issue numbers outside this run; the current pilot excludes
  `1093`.
- `queue`: ordered task entries with graph comment ID and author, base SHA,
  branch, private prompt file and its SHA-256, allowed paths, acceptance
  commands and timeouts, and commit subject. The prompt file's content must
  match its declared hash.
- `models`: an explicit `allowlist`, plus worker and reviewer model IDs and
  reasoning effort for this invocation. These are run settings, not repository
  policy. An unavailable or unlisted model is a hard stop.
- `limits`: finite wall-clock seconds and causal repair count (zero is
  allowed). On the first claim, the journal records an absolute deadline no
  later than `wall_seconds` from that claim. Later invocations use the same
  deadline; neither the time budget nor repair count resets after a restart.
- `required_checks`: required hosted check contexts paired with their GitHub
  App IDs.
- `worktree_root` and `journal_dir`: absolute local locations for the isolated
  task worktree and private recovery journal. Both directories are user-owned
  mode `0700`, journal files are mode `0600`, their roots are disjoint, and
  private journal data is outside the candidate worktree.
- Optional `auth_file`: absolute path to the private Codex model-auth JSON
  supplied to the sandbox. It is required by `run`, but not by `validate`,
  `dry-run`, or `promotion-check`. It is not a GitHub credential and must
  remain a user-owned mode `0600` file outside the candidate worktree. For
  `run`, the controller rejects duplicate JSON keys and requires scannable
  credential values before making any GitHub API request; the sandbox checks
  duplicate keys again when it copies the file.

The spec itself and every prompt file must also be user-owned regular files
with mode `0600` outside the candidate worktree. The runner rejects a spec,
prompt, or auth file under that worktree, and rejects overlapping worktree and
journal roots. Keep these private inputs outside tracked source as well.

For every queued graph comment ID, the issue body must contain an index marker
such as:

```html
<!-- conary-agent-graph-source:v1 comment=42 -->
```

The index is not the live node record. The referenced issue comment is
canonical for that node's state and dependency prose; its body carries the
integrator-authored machine marker:

```html
<!-- conary-agent-node:v1 {"id":"TNPM19","state":"ready","base_sha":"<40 hex characters>","acceptance_sha256":"<64 hex characters>","next_action":"implement"} -->
```

The marker binds the selected node to its base revision and acceptance
definition. Its GitHub author must match `graph_author`; run checkpoint
comments must come from `controller_actor`. `acceptance_sha256` is the SHA-256
of canonical JSON containing the task's `checks`, `allowed_paths`, and
`prompt_sha256`. The controller checks that every queued comment ID appears in
the issue-body index, rejects duplicate index IDs and node markers in the issue
body, and verifies the referenced comment's author and marker fields. The
controller trusts the integrator-authored `ready` state after dependency
verification; it does not parse dependency edges. The primary issue must remain
open. A closed or malformed issue blocks dispatch and every remote write.
Changed state, hash, base, exclusion, or authorization invalidates selection;
update the canonical graph comment and validate the envelope before a new run.

## Claim, Execution, And Recovery

The controller takes the repository-wide exclusive lock before operating. It
rechecks the ready marker and reconciles the issue, branch, and pull-request
head before each remote write. GitHub API requests explicitly pin
`--hostname github.com`; the controller also requires the repository's origin
fetch and push URLs to be the selected HTTPS `github.com` repository and rejects
local Git transport and filter overrides. Its issue checkpoints use the
`conary-agent-run:v1` marker and record the run, task, phase, base/head/tree,
check IDs, and next action.
Run checkpoints record the phases `claimed`, `candidate`, `draft`, `observed`,
or `blocked`; the indexed graph comment remains canonical for node state and
dependency prose, while run comments record execution status. The
mode-`0700` local journal stores run-bound recovery metadata and evidence
locators, not a second graph or authorization.

The worker runs in an isolated linked worktree and receives only the configured
Codex model credential in a temporary private home. The sandbox excludes host
GitHub credentials and CLI configuration, makes Git metadata read-only, and
disables non-file Git transport. The independent reviewer runs in a separate
session with a read-only worktree. Listed acceptance commands run through
`scripts/agent-proof.py` inside a credential-free sandbox in that fresh
worktree at the committed candidate HEAD. The controller checks each receipt's
argv, candidate-before/after identity, and receipt/log hashes against that
head and tree. Failed receipts and logs remain in the worktree for diagnosis;
they block acceptance. A successful process launch alone is never acceptance.

Before committing, the controller scans changed working and staged file bytes
for credential values extracted from the configured auth JSON and common
credential patterns, including GitHub token forms, `sk-` tokens, and private
key headers. This catches those exact values and patterns; it is not complete
exfiltration prevention, and transformed or split secrets may evade the scan.

Only the controller performs remote writes, limited to the actions listed in
`authorization.remote_writes` (branch create/update, issue checkpoint comments,
and draft-PR creation). The run does not push to `main`, bypass protections,
publish, mark the draft ready for review, or merge. It checks changed paths
against the task allowlist, runs acceptance commands, obtains an independent
review receipt, creates or reconciles the draft PR, then observes hosted checks
for the candidate head. A normal return is `draft_checks_passed` or
`already_observed`; neither status means merged or complete.

After a crash or duplicate invocation, the next invocation takes the shared
repository lock and reconciles journal data with the current issue marker,
local and remote branch, PR, candidate head, and check IDs before acting. It
does not blindly replay a write whose result is unknown. A journal at
`branch_intent` with an already-present remote branch is a hard
`unknown_outcome` stop because branch ownership is uncertain. After the
controller creates the branch and reads back its exact base, it records
`branch_created`; a restart at that phase can reconcile the base branch and
append the issue's `claimed` checkpoint. A journal at `worker_started` is also
a hard `unknown_outcome` stop: inspect the child trace and worktree manually;
the controller never relaunches that interrupted child. When it can identify
the task safely, it appends a `blocked` issue checkpoint with the manual
inspection action. That checkpoint is a human stop; no later invocation
automatically resumes the task. Conflicting ownership, an unknown remote
result, changed authorization, drifted head/tree, stale receipt, or malformed
state leaves the run stopped for explicit reconciliation. Some ambiguous
branch-creation outcomes cannot be checkpointed safely and remain hard stops.

## Stops And Promotion

| Observation | Runner action |
| --- | --- |
| Missing, expired, malformed, or contradictory run envelope | Reject before claiming a task. |
| Closed or malformed primary issue; missing, unindexed, duplicated, or mismatched graph comment | Stop before dispatch or remote writes. |
| Issue node is absent, not `ready`, excluded, or differs from the envelope's base or acceptance hash | Stop before dispatch. The controller trusts the integrator's ready marker and does not parse dependency edges. |
| Prompt hash mismatch or invalid UTF-8 | Stop during preflight, before the first remote write. |
| Another invocation holds the local lock | Do not launch a second worker. Reconcile after the active invocation exits. |
| Required command fails | Preserve its receipt and stop or use only the remaining causal repair budget on a changed candidate and inputs. Never repeat an unchanged failure. |
| Review finding | Keep the PR unpromoted; repair within the recorded cap and request a fresh review, or stop when the cap is spent. |
| Stale/unbound evidence, `action_required`, unknown check state, or uncertain remote write | Stop. Do not interpret unknown as success or retry the write blindly. |
| Unknown or internally inconsistent hosted workflow/check state | Fail closed immediately; do not treat it as pending or keep polling. Valid queued or in-progress states may continue to be observed. |
| `unknown_outcome` with a `blocked` checkpoint | Manual stop. Inspect and reconcile the trace, worktree, issue, and remote state; the task is not automatically resumed. |
| Optional image acquisition fails before product tests | The narrow diagnostic below applies; it remains blocking. Other optional hosted failures are `hosted_failure`. |

`optional_pretest_image_failure` is returned only when required checks have
succeeded and one current workflow run failed. The workflow's failed-job set
must contain exactly two completed jobs: `native-cross-source-lifecycle
(opensuse-tumbleweed)` and `native-cross-source-lifecycle`. In the openSUSE job,
step 5, `Run ./.github/actions/cache-base-image`, must have failed and steps
numbered 6 through 12 must be skipped. In the aggregate job, step `Require
every distro lifecycle job` must have failed. This label describes the
pre-test failure; it does not waive it or turn it into a product-test pass.

For listed repairable failures, the controller may spend only the remaining
causal repair count on a changed candidate and then commits, reproves, and
obtains a fresh independent review. It does not repeat an unchanged failed
proof. Failures outside that narrow local repair class stop. For known bounded
stop codes (`child_blocked`, `model_unavailable`, `proof_failed`,
`hosted_failure`, `optional_pretest_image_failure`, `wall_budget`,
`out_of_scope`, `secret_in_candidate`, `action_required`, and
`unknown_outcome`), the controller tries
to append a `blocked` issue checkpoint when the journal identifies the task
safely. It skips the checkpoint for ambiguous graph or remote-write state, or
when the journal is already at `branch_intent`, `blocked`, or `observed`.

`promotion-check` is read-only and evaluates a specific PR against the observed
candidate. It resolves the task by the journal's task ID even when the graph
node has moved beyond dispatch readiness, then requires the indexed graph
comment, author, and latest issue checkpoint to match the observed candidate.
The node may be `ready`, `working`, or `verified`; its next action must be
`implement`, `review_draft_and_run_promotion_check`, or `promote`. A `stop` or
unknown action, blocked state, or blocked/foreign run checkpoint stops the
check. It rehashes worker and
reviewer traces/results plus focused proof receipts and logs; requires
distinct worker/reviewer sessions and the configured
reviewer model/effort; binds the PR head to the candidate; requires a ready,
open PR based on `main`, unchanged `main`, and GitHub mergeability; compares
live required context/App ID pairs with the envelope; requires exact-head
required-check successes and recorded check IDs, successful candidate-head
optional checks, resolved review threads, and a PR test-merge tree equal to the
candidate tree with parents `[base, head]`. Unknown or malformed policy is a
typed error; known mismatches return `blocked` with reasons.

A `ready` result is a read-only candidate evaluation, not merge clearance. The
runner does not create a GitHub approving review, mark the draft ready, or
merge. The ordinary protected review and merge path still applies, with
separately sourced merge authorization, exact merged-tree verification, and
issue/PR read-back. Schema v1 requires `merge: false`. Runner and sandbox
controls exercise this contract, but no live run or real PR promotion check has
been exercised in the pilot.

This command is not a scheduler and does not watch or retry. For phases that
the runner can reconcile, a later invocation uses the same unexpired envelope
and recorded run budget after checking the next action and current state. An
`unknown_outcome` blocked checkpoint requires human reconciliation and an
explicit canonical-record update before any successor work; it is never an
automatic resume. See [Agent Execution Workflow](agent-workflow.md)
for the authorization, task graph, evidence, and closeout rules.
