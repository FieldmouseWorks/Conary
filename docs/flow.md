# Explicit landing passes

`cn flow pass OWNER/REPO` reads the open PRs, calls `flow::next` on each
PR's current facts and saved observations, and executes its one returned
step. `cn flow kick OWNER/REPO [PR]` uses that same entry immediately.
Neither command installs an event source, schedules another pass, nor
switches a repository away from its steward.

Configuration is machine-owned JSON, supplied with `--config`
(`CONARY_FLOW_CONFIG`). Classification uses `--policy` (`CONARY_POLICY`),
the same policy as `cn class`. A minimal configuration is:

```json
{
  "trusted_authors": ["repository-owner"],
  "settings": {
    "people": ["maintainer"],
    "reviewers": ["reviewer"],
    "review_authors": ["landing-app[bot]"]
  },
  "plugins": {
    "review": "/absolute/path/to/reviewer",
    "fix": "/absolute/path/to/fixer",
    "judge": "/absolute/path/to/judge",
    "proof": "/absolute/path/to/proof"
  },
  "evidence": {
    "signer": "owner/ci/.github/workflows/gate.yml",
    "fresh": null,
    "require": ["check"],
    "negative": "attest / negative"
  }
}
```

Publishing settlement requires an anchored ledger (`--anchor-repo`) and the
`evidence` verifier configuration. Missing configuration leaves permission
pending. Passes always read the ledger, even when evidence is unconfigured,
so historical raises still apply. Dry runs use no writer and publish no anchor.

Omitted settings use `flow::Settings` defaults; unknown keys are rejected.
An omitted plugin makes its step wait, with the missing kind in its status.
The paths name executables, without shell parsing. They use the unchanged
[v1 plugin contract](plugins.md), including describe negotiation, bounded
responses, and refusal of the head's author as its reviewer or judge.
Engine trailers across the PR history supply its author engine. Unreadable
or mixed-engine histories wait rather than infer independence. Session
trailers are untrusted: only a completed fix's validated producer establishes
a known session for its reported push.

GitHub authentication uses `GH_TOKEN`, `GITHUB_TOKEN`, or
`--token-command` (`CONARY_TOKEN_COMMAND`), as other cn commands do.
`--api` overrides the API root. Nothing enables these commands automatically.

## Person approval

R2 and a judge's Owner decision require an APPROVED GitHub review from an
account in machine-owned `settings.people`. The account must be a GitHub User, not a
Bot. The review must name the exact head and include the exact marker returned
as `owner_approval` in pass/dry-run output. `flow::owner_approval` produces the
same marker for callers. It binds repo, PR, head, target branch and the current
intent fingerprint; an approval on another scope does not carry forward.
A later review or dismissal from that person supersedes their earlier review.

The owner label only lists work. Adding it, observing it in Memory, or taking
it off never grants permission. A valid approval allows progress while that
label remains. Permission verification rereads the person's approval before
admission, after publication and before use, including continued train grants.
An empty `settings.people` list leaves owner-required decisions waiting.

## State and workers

`--data` (`CONARY_DATA`, by default the ledger directory's `data`
subdirectory) holds mutable state under `flow/owner/repo/`. It is separate
from the append-only operation log. Keep it private to the controller.

Each PR has a JSON record containing its complete Memory, latest Next
(including owner digest notices), effect receipts, job ownership and a
monotonic invocation sequence. Only a ListOwner step adds the blocking owner
label; a digest notice keeps the PR moving. Composite map keys are
stored as ordered pairs. Atomic rename, file and directory sync protect each
record. A repository file lock serializes mutating passes and kicks.

The pass imports every raise from the append-only ledger before deciding and
appends any raise remembered locally before acting. Removing a label, resetting
local Memory or pushing a new revision never lowers the change's class.
Excluded PRs keep the same intentionally incomplete classification in the
initial read and scope checks; recording a complete ledger classification
must not prevent owner listing, waiting or cancellation from executing.

Starting long work saves the invocation and ownership before spawning the
hidden `cn flow-worker` entry. The pass returns without waiting for describe
or the task. A worker locks its invocation, runs the core plugin runner,
and atomically saves its validated response or failure. The next pass reads
that answer into the decision engine's review/job/proof observations.
Workers have file-backed logs and no inherited standard input or output.

The invocation has an absolute deadline: job timeout for review/fix/judge,
proof timeout for proof. Describe and work share that deadline. A later pass
turns a lost worker into a failed job at its deadline; it never relaunches the
same invocation. Cancellation creates a marker the runner checks before
accepting a response. A crash between saving ownership and launching waits
until that deadline, rather than risking two fixers. Deployment must retain
worker processes after the invoking pass exits, and supply plugin sandbox
and descendant resource limits as described in the contract.

## Producer evidence

Plugin v1 answers are proposals, not producer attestations. No local review,
judgment or proof result can grant landing permission. A missing evidence
configuration or missing verification makes the decision wait with a reason,
in dry runs as well as mutating passes.

The machine names its trusted signer, required checks and negative check in
`evidence`. All counted green checks must have a verified
[CI attestation](ci-attestation.md) for the head's exact tree from that signer
on its default branch. Verification uses the CI reader's effective workflow
and target selection: replaced cancellations, obsolete target suites and
skipped jobs are not reintroduced as evidence requirements. Producer receipts
verify the latest effective issuance, including across workflow changes,
against the exact selected result. Fresh observations must still match the
decision's check coverage and contain no blocking states. Configured proofs
remain verification requirements even if a job reports neutral or skipped.
An ordinary status cannot attest a result.
`fresh: null` accepts GitHub-hosted jobs only; a configured fresh-runner
prefix also accepts jobs on runners started for that job, as the existing
attestation verifier requires. A result from the authoring runner is refused.

For a review, the producer must attest the exact published review record.
The producer log is the canonical UTF-8 JSON returned by
`flow::review_receipt(head, body)`; its SHA-256 goes in the existing signed
commit predicate's `log.sha256`. The check name is
`conary/review/ROLE`. This binds the complete result, including the marker's
head, target branch, description scope and completion time. The exported
`review_body` helper owns that marker's formatting.

Judgments use `judgment_receipt(revision, generation, verdict)` under
`conary/judgment`, and dependency decisions use `major_receipt` under
`conary/major`. Their sealed GitHub issuances and independently attested
logs must agree. Only the independent producer allocates issuance generations.
A local invocation sequence has no judgment authority. Newer authenticated
issuances and same-generation conflicts or unsupported-verdict tombstones
survive both worker hydration and later deletion of a comment.

The signing producer must attest results it actually produced in its isolated
job, including checking grader independence; copying a controller-posted
proposal into a signing job is insufficient. Producers can use the existing
commit-attestation format and these log helpers without changing any v1
plugin request or response. Implementing deployment-specific producers is
separate from the pass runner.

Every permission path also verifies the revision's independently attested
negative result through the existing train verifier. It must say the new
tests fail on old code, or attest that no negative is needed. Train admission
records that evidence through the same verifier. Direct and delegated bot
merges apply it too.

The example's canned responses cannot authorize a real landing. The simulation
tests supply independent signed producer records separately. No plugin
response is inserted into the ledger as an attestation.

## Effects and retries

The executor revokes previous permission before cosmetic stage-label writes.
It validates the current head, target branch and tip, description, draft state,
review threads, blocking labels and policy raises before granting permission.
It repeats scope and producer-evidence checks immediately before an expected-head
merge or a delegated settlement. Same-head edits invalidate permission too.
Branch updates pin their expected head; conflict requests carry the resolved
base commit, never a branch name.

Train admission and departure use the existing train module and its ledger
checks, with `--anchor-repo` required for train-enabled passes. Direct and
delegated R2 landings are refused even after owner approval: R2 must use the
train's exact candidate-tree evidence and trunk binding.

Proof slots count active workers across all open PRs, including workers outside
a targeted kick and admissions earlier in this pass. Queued first-priority PRs
keep their order. Dry runs reserve hypothetical slots for the decisions they
print, without starting workers. Cancellation does not release a slot before
the worker answers. An unreadable ownership record or proof answer reserves
the configured capacity conservatively, once per pass. Its error remains on
its own PR; unrelated reviews and fixes still proceed. Capacity is a numeric
reservation, never an invented worker ID or a cancellation target. Old local
records containing synthetic capacity cancellations are recovered without
dropping cancellation state for real owned workers.

Fixer requests use the selected current or carried GitHub review's structured
findings, including its review ID and original head, rather than saved local
review answers. Reviews published by flow include those structured findings.
Unstructured reviews, blocking threads, native change requests, judgment and
description failures wait explicitly: v1 cannot express their repair. Red
checks and conflicts carry their observed check or exact base commit.

Before submitting a direct merge, flow persists its intent and the authenticated
permission it observed. The existing
GitHub ingestion path then records the actual revision and trunk before/after
commits as a Land operation. Later passes revisit pending records even when the
PR has left the open list, including delegated bot merges, lost responses and
lagging merge metadata. The saved permission is reauthenticated against GitHub status history when
cleanup and merging share a timestamp. It applies only to the submitted head,
target, intent and original trunk tip; ordinary withdrawals still block it.
Cleanup cannot erase this request's historical permission, and a known refused
request discards its receipt. A known accepted submission is never resubmitted.
An unsent or uncertain submission may retry only after reading the open PR
and the original target's complete comparison history without finding its
merge; this includes non-first-parent ancestry and has no local 300-commit
scan limit. A superseding head fences the original request even if that
comparison is incomplete. Every new attempt revalidates permission and pins
the expected head. Original scope observations are retained separately while
retrying on another head or target, until the PR's terminal state is recorded.
A merge into another branch closes without recording a trunk landing.

Every possibly accepted permission write is recorded before sending it. A
failed write or a failed validation immediately withdraws permission. If that
cleanup fails, the persisted record is retried before any further PR action,
even for a closed PR. Valid live permissions are stored separately: a targeted
kick preserves an excluded open PR's valid grant while retrying uncertain
cleanup for every PR. A selected PR withdraws its live grant even if its next
read fails. Validation re-reads review coverage and findings, sealed
judgment and dependency generations (including conflicts and tombstones),
local worker results, checks and policy inputs both after publication and
before using the permission.

The v1 proof task runs the whole requested proof. Every full plugin worker
therefore takes a repository proof slot, including Build's initial Evaluate
plan and Run/Job recipes. A targeted kick reads the other open PRs' queue
priority even before their first pass; dry runs reserve virtual slots within
the pass using the same ordering.

A refused write is saved as a failure observation for the next decision.
Retry passes still revoke permission while leaving the failed substantive
action until its deadline. Three strikes make the wait overdue. One PR's
read, plugin or write failure is reported on that PR and does not stop
processing the others.

## Comparing decisions

```console
cn flow pass OWNER/REPO --config flow.json --policy policy.json --dry-run
cn flow kick OWNER/REPO 123 --config flow.json --policy policy.json --dry-run
```

Dry runs only read GitHub and existing local records. They create no data
directory, lock, worker or GitHub write, and collect no results by mutation.
All dry runs read the verified local ledger without taking its writer lock.
The library also refuses to mutate a supplied writable Store in dry-run mode;
it uses only that Store's ledger observations.
Output is one JSON object per PR, sorted by PR number. Each successful read
includes `repo`, `pr` and all of Next: `stage`, `step`, `description`,
`overdue_at`, `proof`, `wait_on`, `on_usage`, and `owner_notice`;
`error` reports a per-PR failure. Null deadlines remain explicit. These
lines are suitable for comparing with the existing steward beside it,
before any repository is switched over.
