# Explicit landing passes

`cn flow pass OWNER/REPO` reads the open PRs, calls `flow::next` on each
PR's current facts and saved observations, and executes its one returned
step. `cn flow kick OWNER/REPO [PR]` uses that same entry immediately.
[conaryd](conaryd.md#verified-flow-events) can call the same pass entry from
verified webhooks and persist the report's booked waits. Neither CLI command
installs an event source, schedules another pass, nor
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
The defaults name no machine, account, label or engine, and start no paid
work, so a machine supplies its own: with no `review_authors` no review is
queued, with no `owner_label` what the owner decides waits unlisted, and
`proof_slots` defaults to `0`, which starts no proof and no preparation
(`null` sets no limit).
Execution choices are optional entries in the same machine-owned configuration:

```json
{"execution":{"judge":{"engine":"example","model":null,"effort":null}}}
```

Keys are plugin kinds (`review`, `fix`, `judge`, `proof`, `job`); the landing
flow starts its four work kinds. Values select that configured executable's
engine and an offered model/effort; they do not install or choose a different
executable. Change `plugins` and `execution` together to switch engines.
Choices are copied into each durable invocation and verified by the worker.
Without an entry Conary supplies no execution policy: the plugin owns its
runtime defaults, and v3 responses still report what actually ran.
`settings.judge_engines` identifies *author* engines whose changes need a judge,
not engines chosen to do the judging.

Both the CLI and conaryd reread this file per pass, through `Config::load`;
updates apply to subsequent requests without a release or restart. Existing
invocations retain the choices they started with. On a burrow-managed host,
generate this configuration from the
[shared settings source](https://github.com/FieldmouseWorks/burrow/blob/main/docs/settings-reference.md).
Conary does not create a second settings database; standalone installations
can supply this JSON directly, without Nix or burrow.
An omitted plugin makes its step wait, with the missing kind in its status.
The paths name executables, without shell parsing. They use the negotiated
[plugin contract](plugins.md), including bounded responses and session
independence. Judges require v3 and every new flow invocation requests a
fresh context. Known reviewer sessions accompany the judge request.
Fix and judge requests name the PR, and a proof or prepare request the head
whose tree it proves. A judge request carries the decision's reasons as `why`, in the
contract's words. One asked again only because a judge already started on
this head and base carries that judge's reasons, even if the description
changed since, or `started` when that judge predates recorded reasons. Its `context` is
`flow::judging_context` over the PR's discussion as GitHub shows it, and
nothing else: every review thread with its comments as they read now; every
review body (`flow::review_bodies`); and every PR comment carrying a finding's
or an answer's marker (`flow::conversation`), whoever wrote them and whether
or not a worker did. Each finding the flow publishes and each answer a fix
gives is one of these, so the flow's own job records never go in, and a judge
producer gets the same digest from `flow::read_judging_context` over its own
read of the PR. Resolving a thread doesn't change it. An approval's body doesn't
count: it's a person's permission, and counting it would make the owner's
release of an owner verdict ask for another judgment. A plugin speaking an older contract gets none of these fields.
Engine trailers across the PR history supply its author engine. Unreadable
or mixed-engine histories wait rather than infer independence. Session
trailers are untrusted: only a completed fix's validated producer establishes
a known session for its reported push.

GitHub authentication uses `GH_TOKEN`, `GITHUB_TOKEN`, or
`--token-command` (`CONARY_TOKEN_COMMAND`), as other cn commands do.
`--api` overrides the API root. Nothing enables these commands automatically.

## The settings schema

`cn flow settings --schema` prints the JSON Schema of this file (`flow`) and
of conaryd's [events configuration](conaryd.md#verified-flow-events)
(`events`). `flow::schema` generates it from the types the loaders parse,
so an option can't exist without its entry. Each option has its type,
allowed values, default and a one-line description, and says three things
more:

- `x-spender`: whether it starts paid work. Every one that does defaults
  off or zero, and the schema refuses to generate if a new one doesn't.
- `x-live`: whether a change takes effect at the next decision that reads
  it. Every option of this file is live. One that isn't says in `x-needs`
  what a change needs, and why.
- `x-secret`: the seal keys, and conaryd's token command, whose arguments
  may carry a credential. Conary never prints their values.

Every reader loads a file through `flow::schema::load`: typed, so a
duplicate or unknown key is refused, and with each section written as an
object, the one form the schema describes.

`cn flow settings --config PATH` (`CONARY_FLOW_CONFIG`) prints each option
of a configuration file with its effective value, as the loader reads it,
and whether the file or the default set it. The options of each section,
of a set `evidence` and of each `execution` entry are listed one by one.
`--events-config PATH` (`CONARY_EVENTS_CONFIG`) does the same for conaryd's
file, with the options that take effect only at a restart listed apart,
under `not_live`. `--schema` reads neither file. A settings source, such as
burrow's, imports this schema rather than declaring the options again.

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
that answer into the decision engine's review/job/proof observations. A
preparation's answer is read as readiness only, never as a verdict: anything
but a `prepare` answer is a preparation that failed.
Workers have file-backed logs and no inherited pipes. A worker's standard
input, which it never reads, is its `.running` lock, taken before it starts and
held until it exits, so a daemon that didn't launch it can wait for its exit.

The invocation has an absolute deadline: job timeout for review/fix/judge,
proof timeout for prepare and proof. Describe and work share that deadline. A later pass
turns a lost worker into a failed job at its deadline; it never relaunches the
same invocation. Cancellation creates a marker the runner checks before
accepting a response. A crash between saving ownership and launching waits
until that deadline, rather than risking two fixers. Deployment must retain
worker processes after the invoking pass exits, and supply plugin sandbox
and descendant resource limits as described in the contract.

## Producer evidence

Plugin answers are proposals, not producer attestations. No local review,
judgment or proof result can grant landing permission. A missing evidence
configuration or missing verification makes the decision wait with a reason,
in dry runs as well as mutating passes.

The machine names its trusted signer, required checks and negative check in
`evidence`, and can set `evidence.producers` to its
[service producer keys file](service-attestation.md#machine-configuration).
Every check on the head must be green, but only the evidence the machine
names must have a verified attestation for the head's exact tree: the checks
in `evidence.require`, every spelling of each configured proof among the
head's checks, and the negative. Each needs a [CI attestation](ci-attestation.md) from that
signer on its default branch, or a [service attestation](service-attestation.md)
from a producer key trusted for that check. A docs-only PR needs no proofs,
and is docs-only only when its docs lane is green and the machine's class
policy names every path it touches documentation: anyone's job can carry the
docs lane's name on a change to code. Any other green check counts as
green alone, the way the train counts its candidate's checks beyond its
`require`: a PR's own CI can't attest its head, since a `pull_request` job's
token names GitHub's test merge and not the head, and a status can't attest
at all. So `evidence.require` names only checks a producer can attest for a
PR's head. Verification uses the CI reader's effective workflow
and target selection: replaced cancellations, obsolete target suites and
skipped jobs are not reintroduced as evidence requirements. Producer receipts
verify the latest effective issuance, including across workflow changes,
against the exact selected result. Fresh observations must still match the
decision's check coverage and contain no blocking states. Configured proofs
remain verification requirements even if a job reports neutral or skipped,
and count only from a service statement signed for the recipe the flow
selects now ([definition](service-attestation.md#signed-statement-v1)), so
changing a proof's recipe under its name needs a new pass.
An ordinary status cannot attest a result.
`fresh: null` accepts GitHub-hosted jobs only; a configured fresh-runner
prefix also accepts jobs on runners started for that job, as the existing
attestation verifier requires. A result from the authoring runner is refused.

For a review, the producer must attest the exact published review record.
The producer log is the canonical UTF-8 JSON returned by
`flow::review_receipt(head, body)`; its SHA-256 goes in the existing signed
commit predicate's or service statement's `log.sha256`. The check name is
`conary/review/ROLE`. This binds the complete result, including the marker's
head, target branch, description scope and completion time. The exported
`review_body` helper owns that marker's formatting.

New judging producers use `Response::provenance()` with `SealKey::judgment_by`
and `judgment_receipt_by(revision, generation, verdict, provenance, basis)`
under `conary/judgment`. Actual engine, session, model, effort and context are
inside both the sealed issuance and independently attested receipt, and so is
its `Basis`: the request's `why` and `context` it was judged under.
`judgment_by` issues these as `autoland-judgment:v3`, whose seal covers its
version, and refuses a basis a judge request couldn't carry. v2 issuances stay
as they are, in one generation order with v3 ones; an authenticated v3
issuance without its basis is a tombstone.

A fail or misdescribed verdict is acted on as it stands. A pass or owner
verdict counts only while its `why` holds every reason `flow::decide` requires
now, except `asked`, which the judgment's own existence makes hold, and its
`context` equals `flow::judging_context` over the discussion now. Otherwise the
flow asks a judge again before granting anything; the old issuance stays. A
judge's job is answered once an issuance of its revision, read on GitHub or
remembered, seals what it was asked on (any issuance, for a request from before
v4). Memory keeps it answered: a removed comment, a newer generation or a
restart never makes it owed again, and an answer that comes late takes it out
of the unanswered count. An answer that no
longer counts was superseded, never lost: the flow asks again at once, and it
doesn't count toward the two jobs that end without their answer before the
owner is told. A judge job that ended without its answer is waited for and
counted as before. A v2
issuance says neither, so its pass or owner verdict never counts. The worker also
retains the complete response with its verdict in the durable completion.
Provenance survives high-water memory; a same-generation provenance conflict
is a tombstone just like conflicting verdict words. Legacy issuances retain
their original receipt format and unknown provenance; no actual model is
inferred from today's settings. Dependency decisions use `major_receipt` under
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

## Pause and hold

`cn flow pause OWNER/REPO --reason TEXT` pauses a repository and
`cn flow resume OWNER/REPO` lifts it. They write and remove one file,
`pause`, in the repository's directory under `--data`, and need no conaryd,
GitHub, token, configuration, build or deploy. They take no lock, so a pass
in progress doesn't keep them off; a second pause replaces the first, and
resuming what isn't paused does nothing. The pause names its scope, the
whole repository; a narrower scope is another kind of the same pause,
never a second one.

Every pass reads the pause before each PR's decision, so one written during
a pass stops the PRs after it, and again before each effect: a plugin
start, a grant of landing permission, a merge or a train admission. An
effect a pause keeps off is no failure and no strike. Only what takes work
off goes ahead while paused: stopping a worker, leaving the train and
taking GitHub's queued merge off. A pause that can't be read stops every
decision with its error; it never reads as no pause. Permission is checked
immediately before publication and validated afterward, including delegated
merges and renewals for PRs already seated in the train.

A paused PR, and a PR with the hold label, vacates. Each of its workers
that hasn't answered (review, fix, judge, preparation or proof) is asked to
stop at once, through the same stop path as obsolete work, its marker
saying `paused` or `held`. Cancellation runs independently of queued-merge
removal, train departure or a pending-landing wait. A repository pause also
cancels owned workers whose PRs have closed or merged, without changing
finished answers. Its proof slot frees when its worker ends, as
any cancelled proof's does, and nothing new starts. A stopped worker is
retired: never a failure, a strike or a lost job. Its PR keeps its Memory,
receipts and branch. A paused PR then waits with its reason, outside every
stage and never overdue; a held one waits where it always has.

Lifting either carries on from the saved state. What a worker finished
stays, so no review, fix, preparation or proof answer is redone, and a
stopped worker's request starts again if it's still wanted. The merge
train's own stepping (`cn train step`) isn't a pass and doesn't read the
pause; a paused PR leaves the train at its next decision.

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

Proof slots count active proof workers across all open PRs, including workers outside
a targeted kick and admissions earlier in this pass. A preparation, and a
proof its preparation said needs no slot, hold none. Queued first-priority PRs
keep their order. Dry runs reserve hypothetical slots for the decisions they
print, without starting workers, and only for a proof that needs one.
Cancellation does not release a slot before
the worker answers. An unreadable ownership record or proof answer reserves
the configured capacity conservatively, once per pass. Its error remains on
its own PR; unrelated reviews and fixes still proceed. Capacity is a numeric
reservation, never an invented worker ID or a cancellation target. Old local
records containing synthetic capacity cancellations are recovered without
dropping cancellation state for real owned workers.

Reviews published by flow include their structured findings, and each
finding gets one target. The review goes up in one request
(`POST /pulls/{n}/reviews`) with a line comment for each finding whose line
is in one of the diff's hunks, placed from the PR's diff before it's sent.
Every other finding (a line outside the hunks, a file outside the diff, or
a diff GitHub won't give) gets a PR comment and has no thread to resolve.
Each target ends in a `conary-finding:v1` marker naming the role, the
reviewed head, the review's issuance (`flow::issuance`, the
`receipt_digest` of its `review_receipt`) and the finding's index in the
review marker's `problems`. So a second review on the same head, two
findings on one line, or a carried review never share a marker. The review
body and its receipt don't change. A lost or partial publication is
reconciled before anything is sent again: the review by its exact body,
each PR comment by its marker. Which findings get a line comment is
planned once, before the review is first sent, and kept in the job's
record. Every write is saved there before it's sent, and sent only while
GitHub doesn't show it. One GitHub acknowledged is never sent again. One
whose answer was lost, or that a crash left unanswered, may have gone up:
it's waited for, and sent again only after GitHub has gone ten minutes
without showing it, at most three times in all, after which the step fails
instead. One GitHub refused went nowhere and may be sent again. Publication is
complete only once the review and every finding's target are read back,
and nothing is handed on before that. Waiting for GitHub to show a write
spends no review attempt. A v2 review body sent twice counts as one
round.

Only the flow's review accounts' markers count. A finding thread is
recognised by its first comment's marker, on whichever review it started,
including one carried from an earlier head. A finding is answered when its
thread is resolved, or, with no thread, when an answer to its own marker is
posted; a review is answered when all its findings are. Fixer requests use
the selected current or carried GitHub review's unanswered findings, and any
finding thread still open, rather than saved local review answers. Each
problem keeps its finding's marker and target in the fix job's record.
Unstructured reviews, threads the flow didn't start, native change requests,
judgment and description failures wait explicitly: v1 cannot express their
repair. Red checks and conflicts carry their observed check or exact base
commit.

A v4 fix's answers are posted on the head it left, pushed or `unchanged`,
and never on a head that moved past it. Each answer's `reply` goes on its
finding's target with a `conary-answer:v1` marker that also names the fix
invocation, as the flow's review account: a reply on the thread, which is
then resolved, or a PR comment. A thread whose first comment no longer names
that finding gets no reply. A lost write is reconciled by its exact body
before it's posted again, and is sent under the same rules as a review's
writes. Every thread of the finding is resolved, a duplicate's too. The fix
is done only once every reply and resolution is read back; it then counts
as answered, never as a job that ended without its answer. Only the latest
fix's answers go up: an earlier invocation's are dropped. An `unchanged` fix posts its replies and pushes
nothing, and the reviewers then look again; the round is counted as before
([review-loop.md](review-loop.md) owns what the answers mean).

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

A proof is a program, and goes in two steps, both to the proof plugin
(`flow::proofs`). The flow selects the pending checks whose recipe is a
program, a `Build` before a `Run`, and asks once per head and selected
recipe set:

- **`prepare`** readies their inputs. It holds no proof slot and isn't
  counted against `proof_slots`; its deadline is `proof_timeout`. One runs
  at a time per head and recipe set, and it's stopped once the head or a
  selected recipe changes. A preparation that failed, or answered that the
  inputs aren't ready, is asked again after fifteen minutes.
- **`proof`** then runs the checks. It takes a slot, in the queue order
  above, unless its preparation answered `slot: false`.

A `Job` recipe is a model-run check, not a proof, and is never sent to a
proof plugin: its check waits, with that reason. A targeted kick reads the
other open PRs' queue priority even before their first pass; dry runs
reserve virtual slots within the pass using the same ordering.

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
`overdue_at`, `proof`, `wait_on`, `on_usage`, and `owner_notice`, and
`revision` (`head`, `base`, `description`), the revision it was decided on;
`error` reports a per-PR failure. Null deadlines remain explicit. These
lines are suitable for comparing with the existing steward beside it,
before any repository is switched over.

`cn flow compare OWNER/REPO [PR]` makes that comparison itself. It does the
same dry run, then reads what the steward running the repository says of
each PR now: the stage its `waiting: …` label names, and its newest
`review-settled` status on the head, counted only from
`settings.review_authors` (from anyone when that's empty). It prints one
JSON line per PR with `repo`, `pr`, `head`, `agree`, `flow` (`stage`,
`step`, `description`), `steward` (`stages`, `state`, `description`) and
`error`: disagreements first, then errors, then agreements, each by PR
number. A last line tallies the repository: `prs`, `agree`, `disagree`,
`errors`. The stages agree when the steward's labels name exactly the
flow's stage, or none for the null stage. A PR whose head, base, title or
body changed between the dry pass and that read is an error, never a
comparison. It writes nothing, as a dry run.
Run right after the steward's own pass, it shows where the two part before
any repository switches over.

### Where the shadow differs from the steward

Each kind of disagreement remi's shadow showed (2026-11-13 to 11-16,
[#410](https://github.com/FieldmouseWorks/Conary/issues/410)) was judged
against what GitHub shows for the PR. A kind where the flow was wrong is
fixed and isn't listed. These are the rest. A disagreement of one of these
kinds is accounted for; any other is a finding.

**The steward settles on work the flow doesn't count.** The flow counts a
review only by its scoped marker (head, base branch, title and body, and the
review's completion time), and grants nothing on it without the producer's
attestation ([Producer evidence](#producer-evidence)); a judgment only as a
sealed issuance with its basis; and a job only one it started. The steward's
reviews are `codex-review:v1` markers, whose fingerprint covers the body
alone and which carry no completion time; its judgments are
`autoland-judgment:` markers with no basis; nothing attests either. So where
the steward has settled a PR on them, the flow still wants a review or a
judge: stage `review`, step `LeaveTrain` for a PR in the train. Example:
remi#1355 at f97d267, where Codex found nothing, every thread was resolved,
`review-settled` was green and the steward had it gating in the train, and
the flow said `review`/`LeaveTrain`. The same holds in the steward's other
stages (remi#1314: steward `ci`, flow `review`), for its judge (remi#1301:
the steward's judge said land, the flow asks a judge), and while the
steward's own fix job has the branch. A dry run has no memory of when it
first saw a head, so it gives this as "Waiting for the head or answered
findings to settle" where a live pass would queue the review. It ends at S1,
when the flow's own reviewer and judge publish what it counts; a PR still
open then gets one fresh review.

**The steward is a pass behind.** Not a difference: the steward's next pass
says what the flow said. Right after the steward posts a review with
findings, its status can still say "Findings answered; review round N next"
while the flow reads the new thread and says `fix` (remi#1311: the review at
04:03:49Z, that status at 04:04:10Z, "1 open review thread" at 04:06:12Z).
After a fix job's answers go up the steward says `fix`, "reading the PR
again", while the flow already says `review` (remi#1311 at 04:18Z). It's
harmless because the comparison acts on nothing and the steward acts on its
own next read. The comparison can't mark it: each side reads GitHub once,
and nothing there says the steward's word is a pass old. A lag is a
disagreement that the next line about the PR doesn't repeat.
