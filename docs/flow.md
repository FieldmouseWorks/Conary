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
    "reviewers": ["codex"],
    "review_authors": ["chatgpt-codex-connector[bot]"],
    "flow_authors": ["landing-app[bot]"]
  },
  "plugins": {
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
work, so a machine supplies its own: with no `review_authors` no review
counts, with no `flow_authors` neither its own `review-settled` nor the
train's status does, with no `owner_label` what the owner decides waits unlisted, and
`proof_slots` defaults to `0`, which starts no proof and no preparation
(`null` sets no limit).
Execution choices are optional entries in the same machine-owned configuration:

```json
{"execution":{"judge":{"engine":"example","model":null,"effort":null}}}
```

Keys are plugin kinds (`review`, `fix`, `judge`, `proof`, `job`); the landing
flow starts three of them (fix, judge and proof, a proof's preparation
included): the reviewer reviews on its own ([Review](#review)). Values select that configured executable's
engine and an offered model/effort; they do not install or choose a different
executable. Change `plugins` and `execution` together to switch engines.
Choices are copied into each durable invocation and verified by the worker.
Without an entry Conary supplies no execution policy: the plugin owns its
runtime defaults, and v3 responses still report what actually ran.
`settings.judge_engines` identifies *author* engines whose changes need a judge,
not engines chosen to do the judging.

Both the CLI and conaryd reread this file per pass, through `Config::load`;
updates apply to subsequent requests without a release or restart. A seal
key (`settings.judge_key`, `settings.major_key`) may be given instead as a
file holding it (`judge_key_file`, `major_key_file`), read at that same load,
so the configuration can sit where anyone reads it (a Nix store) while the key
is readable by the flow's user alone. One key given both ways is refused. Existing
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
nothing else: every review thread with its comments as they read now, and
every review body (`flow::review_bodies`), whoever wrote them and whether or
not a worker did. Each finding the reviewer posts and each answer a fix
gives is one of these, so the flow's own job records never go in, and a judge
producer gets the same digest from `flow::read_judging_context` over its own
read of the PR. Resolving a thread doesn't change it. An approval's body doesn't
count: it's a person's permission, and counting it would make the owner's
release of an owner verdict ask for another judgment. A plugin speaking an older contract gets none of these fields.
Engine trailers across the PR history supply its author engine, read once
with the PR (`flow::history::author`). A history no trailer names is the
account that opened the PR, a person or an App: a judge is independent by
its fresh session on any engine ([plugins.md](plugins.md#independence-and-execution-failures)),
so a workers App's PR is judged all the same. A history that didn't read in
full, or names more than one engine, waits for its judge, its status saying
which, until its history, or a fix's validated push, names one author. Session trailers are untrusted: only a completed fix's validated
producer establishes a known session for its reported push. A step that
fails is retried with what its effect said in its status and in the PR's
record (`failed_why`), never dropped.

GitHub authentication uses `GH_TOKEN`, `GITHUB_TOKEN`, or
`--token-command` (`CONARY_TOKEN_COMMAND`), as other cn commands do.
`--api` overrides the API root. Nothing enables these commands automatically.

## The settings schema

`cn flow settings --schema` prints the JSON Schema of this file (`flow`),
of conaryd's [events configuration](conaryd.md#verified-flow-events)
(`events`) and of its [gate-reuse configuration](gate-reuse.md#running-it)
(`gate_reuse`). `flow::schema` generates it from the types the loaders parse,
so an option can't exist without its entry. Each option has its type,
allowed values, default and a one-line description, and says three things
more:

- `x-spender`: whether it starts paid work. Every one that does defaults
  off or zero, and the schema refuses to generate if a new one doesn't.
- `x-live`: whether a change takes effect at the next decision that reads
  it. Every option of this file is live. One that isn't says in `x-needs`
  what a change needs, and why.
- `x-secret`: the seal keys, and conaryd's token commands, whose arguments
  may carry a credential. Conary never prints their values.

Every reader loads a file through `flow::schema::load`: typed, so a
duplicate or unknown key is refused (a key given twice in a map, such as
`proofs` or `priority_labels`, too), and with each section written as an
object, the one form the schema describes.

`cn flow settings --config PATH` (`CONARY_FLOW_CONFIG`) prints each option
of a configuration file with its effective value, as the loader reads it,
and whether the file or the default set it. The options of each section,
of a set `evidence` and of each `execution` entry are listed one by one.
`--events-config PATH` (`CONARY_EVENTS_CONFIG`) does the same for conaryd's
file, with the options that take effect only at a restart listed apart,
under `not_live`, and `--gate-reuse-config PATH`
(`CONARY_GATE_REUSE_CONFIG`) for its gate-reuse file. `--schema` reads
none of them. A settings source, such as
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

## Asking the owner

The flow asks the owner only for a one-way door it names
(`owner-only-for-doors`), and once for each decision
(`one-question-per-decision`). A `ListOwner` step carries its door, from a
closed set ([`flow::Door`](../crates/conary-github/src/flow/approval.rs)):
`r2`, an R2 change; `outside`, an outside contribution; `judge`, a judge's
`owner` verdict; `label`, the owner label set by a person. A decision with
no door doesn't list the owner: the flow makes the call itself and its
status shows it, overdue like any other wait.

- A read too large to finish waits at no stage, named as the degraded
  `snapshot` read ([Degraded paths](#degraded-paths)), until GitHub gives
  it in full.
- Two jobs on one revision that end without their answer start no third:
  the wait is overdue at once, gets its [recovery](#deadlines) where one
  applies (a timed-out run at the fix stage is run again), and then stays
  overdue in the status, with no digest notice however long the PR has
  been open.
- A major update the dependency sweep held waits for the sweep, listed only
  with a door the sweep names; today's sweep names none.
- A hold past `hold_stall_after` is a stall in the status, never a listing.

Each request has a subject key: its door, repository, PR and the intent
fingerprint it binds, the same across heads that keep that intent. The flow
keeps one open request per key in the PR's memory, and a later wait with
the same key joins it: no new listing or notice, though the label has come
off since. Next's `owner` is the request the decision lists or joins: its
`key`, `since` (when it was first asked), `prs` (every PR waiting on it,
the one its key names), its approval `marker` for the head now when the
door takes one, a judge's `reason` from the flow's own judge's answer
(model text, kept to the data directory and the status, never the ledger),
and whether it's `listed`. A new intent is a new decision: a request for
another one is closed. Joining never widens an approval: R2 still needs
the marked review on the exact head ([Person approval](#person-approval)).

The flow's own listing is no door. Memory's `labelled` says the flow put
the owner label on, until a pass reads it off. A label without that mark
is a person's door only when a person put it on. While it's on, each read
of the PR (a scope check's too) reads its timeline for its newest
`labeled` event: a `User` there is a person, a `Bot` an App (the steward,
or the flow before it kept its mark), which lists nothing of its own.
When that can't be told (the read, `owner_label`, fails, or names
neither) the PR waits at no stage, neither listed nor gone past. A
decision, or a switch's step, saved before a listing named its door
loads as a wait, and the next pass decides it again.

## State and workers

`--data` (`CONARY_DATA`, by default the ledger directory's `data`
subdirectory) holds mutable state under `flow/owner/repo/`. It is separate
from the append-only operation log. Keep it private to the controller.

Each PR has a JSON record containing its complete Memory, latest Next
(including owner digest notices), effect receipts, job ownership and a
monotonic invocation sequence. Only a ListOwner step adds the blocking owner
label, once for each request ([Asking the owner](#asking-the-owner)); a
digest notice keeps the PR moving. Composite map keys are
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
same invocation, and a check's second deadline on one tree is a
[stall](#deadlines). Cancellation creates a marker the runner checks before
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
at all. So `evidence.require` names only checks a producer can attest on
the tree that lands. For a PR that lands itself that's its head. For one that
lands through the merge train (`settings.train`, targeting the default
branch) it's the candidate: the train's gate (its step's `--require`,
these same checks, [the train](#the-train)) asks them of the candidate,
attested on the tree that lands, so joining the train and
waiting in it ask the head only for its proofs and negative, and a repo's
gate may run on candidates alone. Verification uses the CI reader's effective workflow
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

A review is not evidence ([DESIGN.md 4.3](DESIGN.md#43-evidence)): the
reviewer's clean word is the decision's to wait for, and nothing attests it,
so permission asks no producer for it. A post the decision read that's
rewritten before permission is used changes the decision's scope, which
refuses permission. The ledger records the reviewer's word as a `review`
result with no attestation (`ingest`, from the same reader), which never
counts.

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
flow stops starting them. A judge job that ended without its answer is waited for and
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
records that evidence through the same verifier. Direct merges apply it too,
a bot's included.

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
immediately before publication and validated afterward, including renewals
for PRs already seated in the train.

A paused PR, and a PR with the hold label, vacates. Each of its workers
that hasn't answered (review, fix, judge, preparation or proof) is asked to
stop at once, through the same stop path as obsolete work, its marker
saying `paused` or `held`. Cancellation runs independently of queued-merge
removal, train departure or a pending-landing wait. A repository pause also
cancels owned workers whose PRs have closed or merged, without changing
finished answers. A cancelled worker exiting during scope validation does
not interrupt non-granting cleanup or count as a failure; permission grants
still validate fresh worker state. Its proof slot frees when its worker ends, as
any cancelled proof's does, and nothing new starts. A stopped worker is
retired: never a failure, a strike or a lost job. Its PR keeps its Memory,
receipts and branch. A paused PR then waits with its reason, outside every
stage and never overdue; a held one waits where it always has.

Lifting either carries on from the saved state. What a worker finished
stays, so no review, fix, preparation or proof answer is redone, and a
stopped worker's request starts again if it's still wanted. A paused PR
leaves the train at its next decision, and a paused repository's train
isn't stepped ([the train](#the-train)).

## Switches

A switch is something a person or the configuration turned off: the
repository's pause, a PR's hold label, an omitted plugin (`plugins.review`,
`fix`, `judge` or `proof`), the train off (`settings.train`), no person list
(`settings.people`), and no evidence verifier (`evidence` unset, or one that
requires no gate). Each one shows while it's in effect, with what it holds.

Each PR's decision names every switch that stops it in `switches`, left out
when there are none. An entry is the switch (`"switch"`: `pause` with its
`reason` and `at`, `hold` with its `label`, `no_plugin` with its `kind`,
`train_off`, `no_people` or `no_evidence`), the `step` it keeps off, and the
`workers` of the PR it's stopping. A pause or a hold decides the PR's step
and so masks the others: the step they keep off is decided again with them
lifted, its memory thrown away, and the configuration's switches are judged
on that step. So a paused, held PR that needs a review with no review plugin
names all three. A configuration switch names a PR only at the step it
keeps off: an omitted plugin when the step is that plugin's work; the train
off when an R2 change would merge (an R2 change lands only through the
train); no person list when a landing step needs a person's approval, or
the PR waits on the owner for one (a person's owner label, an R2 change or a
judge's owner verdict); no evidence verifier at any
landing step. The PR's `description` stays the one its decision gives.

A held PR is read as fully as any other, so its decision can say what the
hold keeps off. The hold still decides it ahead of everything but what an
owner must hear of first, as before, and a read that fails doesn't keep it
from stopping the PR's work: the label the open list shows does.

```console
cn flow status OWNER/REPO --config flow.json
```

prints the repository's switches in effect, each with `prs`: every PR it
holds, the `step` it keeps off and the PR's `workers` that haven't
answered. It reads only the data directory and the configuration: no
GitHub, conaryd, token or lock, so it answers while they're down or a pass
holds the lock. The pause is read from its file now, and holds every open
PR it covers, whether or not a pass has seen it yet. The configuration's
switches are listed whenever they're in effect, holding no PR until a
decision names one. Holds are as the last pass read the labels. The open
PRs are the ones the last pass listed, saved as `open` in the repository's
directory with the time it read them (`read_at`); a record stays once its
PR closes, so a closed PR drops out at the next pass.

## Priority

Each PR has a priority level, `high`, `medium` or `low`, the levels
burrow's drain uses. `priority_labels` maps labels to levels (by default
`priority: high`, `priority: medium` and `priority: low`; a machine that
keeps an owner's `first` label maps it to `high`). A PR takes the highest
level its labels are mapped to, or `default_priority` (`medium`) with none.
A label maps to one level: it's a key of that map, and the loader refuses
a key given twice. A fix for the repository's red default branch is `high`
whatever its labels, so nobody has to label it: a trusted author's PR from
an `autoland/red-` branch in the repository itself into the default branch
([`flow::red_fix`](../crates/conary-github/src/flow/priority.rs), the same
test that lets it skip waiting on the red branch's own runs).

Proof slots, train admission and a targeted kick's view of the other PRs
go in one order ([`flow::Rank`](../crates/conary-github/src/flow/priority.rs)):
by level, then age (the oldest opened first), then PR number. A pass takes
its PRs in that order, so a dry run's virtual slots and the train's joins
follow it too. Each level has its own `stall_after`, the seconds a PR may
wait at one stage before it's overdue: `high` 1800, `medium` and `low`
7200 by default. A hold's and the train's own deadlines stay as they are. The level is in each PR's Next, as `priority`.

## Degraded paths

A degraded path is one that half-worked and went on without the answer it
was for. The flow records each one it meets, through one owner
([`flow::degraded`](../crates/conary-github/src/flow/degraded.rs)), as the
ledger's `degraded` op ([DESIGN 4.6](DESIGN.md#46-op-log)), counts it as its
check's failure, and names it in the PR's decision as `degraded`: each
entry's `path`, what it names, and `why`. Left out when there are none. The
paths it records:

- **A record that doesn't read** (`record`). Every pass reads each open PR's
  record, so a targeted kick meets every PR's too. That PR's workers are
  taken as holding every proof slot, and a PR waiting on one says whose
  work it is. Its own decision waits, naming it, and nothing acts for it or
  writes over the record.
- **A worker's answer that doesn't read** (`answer`, with its `job` and the
  `kind` of work): its answer file doesn't parse, or answers another kind
  of work than the worker was started for (the runner refuses that, so the
  file says something it never wrote), or the worker's stop marker says
  neither a stop nor its deadline. The flow writes a stop marker whole, so
  one half-written is never read. The worker is taken as still running, and holding its
  slot if it's a proof. The PR's decision waits on it at no stage, so even
  a seat in the train is granted nothing, unless it's stopping work. An
  answer that stops reading after the decision and before its permission is
  used withdraws the permission, as any other changed input does.
- **An evidence lookup that can't tell** (`lookup`, with its `check`): a
  read of GitHub that one check's attested result needed didn't answer:
  its runs, its tree or its attestation, the negative check's merge base
  and verification, or a review's PR and carry. The landing isn't granted,
  and the PR waits at the proof stage. A lookup that told no is no
  degradation: it's the check's own answer.
- **A mark on the PR that doesn't parse** (`mark`, with which `mark`): one
  the accounts the flow trusts for it left, which it can't read, so what
  it would have said counts for nothing. `verdict`, a judge's or the
  dependency sweep's sealed verdict whose issuance isn't one (it can't be
  authenticated, so it may be newer than any that can); `review`, the
  reviewer's summary whose state isn't JSON or whose code review isn't a
  review's state; `release_age`, Renovate's artifact comment with a block
  that ends in its release-age wait with no time after it (a block that
  ends in anything else is a real failure, as before). The PR waits on it
  at no stage, naming it, nothing acts for it, and the pass is booked
  again. A mark is read again, and cleared, each time the decision reads
  the comments it's in: Renovate's only while its artifacts are the only
  red check. A mark's text doesn't say which revision it's for, so one
  that stays broken holds every revision until it's edited or deleted.
- **A read of GitHub that fails or stops short** (`read`, with which `read`
  from a closed set). Each one counts as the failure of what it was for:
  - Reads the decision can't go on without (`pull`: the PR or its
    snapshot; `pulls`: the open PRs; `base`: the base branch's tip;
    `files`; `checks`: the head's runs and statuses; `cancelled`: a
    cancelled run's annotations; `pushes`; `reviews`: the PR's reviews and
    comments; `pause`: the repository's pause, which a writing pass meets
    first stopping work, as an error, so only a dry run's decision names
    it). The PR waits at no stage, naming it, and nothing acts for it. Its
    error books the pass again.
  - Reads it goes on past, each holding what it was for failed: `checks`
    when GitHub lists more check suites than one read holds, or a run's
    suite isn't in the snapshot (the check is pending); `fork`, the docs
    lane's fork point (the lane's word is stale); `cancelled`, a cancelled
    run whose suite isn't in the snapshot (taken as not replaced);
    `commits`, a comparison that fails or has a commit without its message,
    and `trailers` (authorship is unknown, and it waits); `commits`, a
    comparison GitHub stops short (a judge is asked, and the judge's word
    lands it); `merge_tree`, GitHub's test merge (what the merge changes is
    unknown, so it doesn't land; a train PR still joins, since the train's
    candidate merges it again and checks that); `diff` (the gates count as
    edited); `tree`, the head's tree, read once a check has reached its
    [deadline twice](#deadlines) on one tree (whether this is that tree is
    unknown, so nothing starts for it); `owner_label`, the timeline read
    for who put the owner label on (it waits, neither listed nor gone
    past: [Asking the owner](#asking-the-owner)); `default_branch`, no
    observation of the default branch's CI for a fix of it (the base is
    held); `snapshot`, a snapshot that stops short of the PR's reviews,
    comments, labels or threads (the PR waits at no stage, overdue in the
    status, until one reads in full). A stale test merge,
    or a base branch that's gone, is GitHub's answer, not a degradation.
  - Evidence's reads every check shares (`observations`: the head's checks,
    read again, failing or stopping short; `reviews`): the landing isn't
    granted, and the PR waits at the proof stage.
  - A PR the pass doesn't visit that holds a live permission keeps it only
    while its read says it's open: a read that fails withdraws it, as a
    closed PR's is, and records `pull`. A PR that's closed isn't read
    again, so that stays recorded.
  - Occupancy's read of each open PR, for its proof slots, and of the
    pause: what it meets is recorded for a PR the pass doesn't visit, and
    its slots are held as before. A visited PR's own decision reads it
    again, and records what that read meets.
  - A read that fails or stops short after the decision and before its
    permission is used withdraws the permission, as any other changed
    input does, and is recorded: the decision saved waits on it at no
    stage, naming it.

A pass records each path once while it lasts, against the PR's revision as
the ledger has it, and appends `restored` when the same path works again: a
record, answer or mark the next pass reads, a lookup that tells, a read that's
taken and answers in full (one that isn't taken clears nothing). A PR the
ledger hasn't seen has nothing to record against, and its decision still
names what degraded. A dry run, or a pass with no ledger store, records
nothing and names the same: a path the ledger still holds degraded that this
pass took again (a record or answer that reads, a lookup that tells, a read
that answers) isn't named, and any other it holds is, until it's taken
again.

`cn flow status` lists every open PR's degraded paths as `degraded`, each
with its `pr`: a record or answer that doesn't read now, read from the data
directory, and what its last decision named. A record it can't read is
listed, not an error.

## Effects and retries

The executor revokes previous permission before cosmetic stage-label writes.
It validates the current head, target branch and tip, description, draft state,
review threads, blocking labels and policy raises before granting permission.
It repeats scope and producer-evidence checks immediately before an expected-head
merge. Same-head edits invalidate permission too.
Branch updates pin their expected head; conflict requests carry the resolved
base commit, never a branch name.

Train admission and departure use the existing train module and its ledger
checks, with `--anchor-repo` required for train-enabled passes. Direct R2
landings are refused even after owner approval: R2 must use the train's exact
candidate-tree evidence and trunk binding.

A dependency bot never merges its own PR: Renovate merges nothing in a
repository conaryd lands (burrow#1318). So a settled Renovate PR lands like
any other, through the train where there is one and the flow's own
expected-head merge where there isn't, whatever its automerge setting says.

Proof slots count active proof workers across all open PRs, including workers outside
a targeted kick and admissions earlier in this pass. A preparation, and a
proof its preparation said needs no slot, hold none. Queued PRs keep their
[order](#priority). Dry runs reserve hypothetical slots for the decisions they
print, without starting workers, and only for a proof that needs one.
Cancellation does not release a slot before
the worker answers. An unreadable ownership record or proof answer reserves
the configured capacity conservatively, once per pass. Its error remains on
its own PR; unrelated reviews and fixes still proceed. Capacity is a numeric
reservation, never an invented worker ID or a cancellation target. Old local
records containing synthetic capacity cancellations are recovered without
dropping cancellation state for real owned workers.

### The train

Stepping is landing, so the flow steps the train of a repo it lands: a pass
of the whole repo with `settings.train` on ends with one step of its train
(`train::step`, the one `cn train step` runs), after every open PR's
decision, so the PRs it joined are in it, and still under the repository's
lock, so no two passes step at once and a quiet lock means no step is under
way. The step's `--require` is `evidence.require`, its attestations are
counted as `evidence` says, and its candidates hold at most
`settings.train_size` changes. With no `evidence`, or nothing required, the
step fails and nothing lands. It isn't taken in a dry run, in a pass of one
PR, while the repository is paused, or when the decision of a PR in the
train failed in this pass, as it may not be settled any more; nor
when the train has nothing in it or in flight. A step that changed what a PR
is (landed, red, refused, out) books that PR's next pass at once, since the
train's own statuses start none; a step that failed says so on each PR in
the train, which books it again. conaryd's passes are always of the whole
repo, and the events that move a train kick one: a check suite or workflow
run on a candidate's train branch, a merge's push to the default branch, a
PR's close, and conaryd's start.

## Review

The reviewer is Codex's GitHub connector, which reviews each push on its
own and posts on the PR (`flow::reviews`, as burrow's steward reads it). Its
word on a head is the newer of what its summary comment's
`codex-review-state:v1` mark says of that exact commit (`completed` with no
findings is clean, `completed` with findings is findings, anything else is
under way) and a review on the head whose `**Reviewed commit:**` receipt
names it (findings). Each review with a receipt, at any head, is a round.
Only posts by `settings.review_authors` count, so nobody posing as the
connector moves a PR. Its word binds only the commit: a PR retargeted or
whose description was edited at or after its head's push (or at all, when
GitHub can't say when the head was pushed: the earliest check suite on it)
goes to a judge with the reason `changed` instead of landing on it.

A head with no word yet waits in `review` for half an hour after the flow
first saw it, then the flow asks for a look (`AskReview`), at most once an
hour per head. The connector takes that ask from no App, so a configuration
can name a door that posts it as someone it does take it from:

```json
{"review_ask": {"command": ["/absolute/path/to/door"], "repos": ["owner/repo"]}}
```

In a listed repository the flow runs the command, without a shell, with the
repository, the PR's number and the head after its arguments, and with no
`GH_TOKEN` or `GITHUB_TOKEN` of its own; what it prints goes to the flow's
standard error. Exit 0 counts the head as asked; anything else isn't, and a
later pass runs it again. It runs in a process group of its own: with no
answer in three minutes, the group is killed, so nothing it started posts
an ask the flow didn't count. Acting as that
person is the door's alone: on a burrow-managed host it's
[burrow-review-ask](https://github.com/FieldmouseWorks/burrow/blob/main/crates/burrow-review-ask/src/lib.rs),
for the repositories in `autoland.askAsOwner.repos`. In any other repository
the ask is a PR comment of the flow's own: the bare `@codex review` (the
connector reads any text after it as a task and posts nothing, so the why
stays in the flow's record), counted as asked only once GitHub took it. A PR
an App opened (its author a `Bot` account) is asked for on the pass that first
sees each head, since the connector reviews none of its heads unasked. Two
hours after a head's first ask (or, never asked, after the flow first saw it)
the owner hears of it; asking again doesn't restart that clock. Findings
with open threads go to a fix; findings whose every thread is answered on
an unchanged head are asked for again once they've settled
(`settle_delay`). Past `settings.rounds` rounds and not clean, the flow stops
waiting for the reviewer: a judge decides with `round_limit`.

A fix is handed every open review thread (`flow::handoff`), whoever opened
it: the connector's, or anyone's, as burrow's steward hands them. Each is a
finding on the thread's file and line (the line it was opened on, once
outdated), at the severity the connector's badge gives it (P1 without one),
saying the thread's whole conversation under each comment's author; the fix
job's record keeps the thread's id, which its answer goes on. A thread on no
line, native change requests, judgment and description failures wait
explicitly: v1 cannot express their repair. Red checks and conflicts carry
their observed check or exact base commit.

A v4 fix's answers are posted on the head it left, pushed or `unchanged`,
and never on a head that moved past it. Each answer's `reply` goes up as a
reply on the thread it was handed, as the flow's own account
(`settings.flow_authors`), and the thread is then resolved. A thread that's
gone gets no reply. A lost write is reconciled by its exact body
before it's posted again: every write is saved in the job's record before
it's sent, and sent only while GitHub doesn't show it; one whose answer was
lost may have gone up, so it's waited for and sent again only after GitHub
has gone ten minutes without showing it, at most three times. The fix
is done only once every reply and resolution is read back; it then counts
as answered, never as a job that ended without its answer. Only the latest
fix's answers go up: an earlier invocation's are dropped. An `unchanged` fix posts its replies and pushes
nothing, and the reviewer is then asked to look again; the round is counted as before
([review-loop.md](review-loop.md) owns what the answers mean).

Before submitting a direct merge, flow persists its intent and the authenticated
permission it observed. The existing
GitHub ingestion path then records the actual revision and trunk before/after
commits as a Land operation. Later passes revisit pending records even when the
PR has left the open list, including lost responses and
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
other open PRs' [place in line](#priority) even before their first pass; dry
runs reserve virtual slots within the pass in the same order.

A refused write is saved as a failure observation for the next decision.
Retry passes still revoke permission while leaving the failed substantive
action until its deadline. Three strikes make the wait overdue, as a check
[stalled on its deadlines](#deadlines) does at once. One PR's
read, plugin or write failure is reported on that PR and does not stop
processing the others.

### Deadlines

A deadline work keeps hitting is a stall to fix, not a retry
(`limits-are-design-bugs` in [lessons.toml](lessons.toml)). Each
preparation and proof keeps the tree it was asked to run on, and one that
reaches its deadline without an answer (the plugin ran out its time, or
its worker was lost and a later pass said so for it; stopped work doesn't
count) counts once for each check it was asked for, against that check's
recipe and that tree. The flow's records of its work are the count's one
home, so a restart or a dry run reads the same count.

The first deadline a check reaches on a tree is retried as any failed job
is. At the second on the same tree, as its recipe stands, the flow starts
nothing more for it, neither preparation nor proof: the PR waits at the
proof stage, overdue at once, saying `Stalled:` with the check, the tree's
first twelve digits and both deadlines. The count is the check's, not its
invocation's: a preparation's deadline and a proof's add up. A new tree or
a changed recipe starts the count again; a new head on the same tree
doesn't, so once a check has reached its deadline twice on any tree, the
flow reads the head's tree before deciding. A read of it that fails is the
degraded `tree` read: whether the head is on the stalled tree can't be told,
so nothing starts until it reads. Work saved before trees were kept counts
toward no tree.

A wait on CI costs nothing (`waiting-costs-nothing`): it starts no plugin
and books no wake for the checks' result, which comes as their own events (a
run finishing, a commit status) and ends the wait only with a verdict. Only
a PR no check has reported for books a wake, five minutes on, to tell when
none ever will. What's booked is the stall deadline, and a check's stall
clock starts when its run starts. While every check the wait names is a run
GitHub says is queued for a runner, `overdue_at` is none, nothing is booked,
and Next's `queued` says since when, as capacity. Once one starts, the wait
is overdue `stall_after` from the earliest start GitHub gives, however long
it queued; a workflow job leaving the queue kicks the repository, since no
check run event says so. A check pending only behind the others (a job that
finished in a workflow run still going, or a machine proof waiting for CI)
has no clock of its own. Any other pending check that isn't a run known to
be queued or started (a commit status, a run GitHub gives no start for, a
cancelled run to be run again) keeps the wait's own clock, as before, and
a cancelled run's clock running out is what brings it back (below).

A wait past its deadline is brought back the way GitHub allows, once
(`recover-by-kind`). The pass at its deadline takes the first recovery that
applies and wasn't asked for on this head:

1. At a wait on CI or the merge, a head GitHub says is behind its base and
   merges cleanly is updated with it, as the merge's own update is. Not a
   dependency bot's PR, whose bot rebases it, nor one stacked on another.
2. A check run brought back by its kind, once the run's suite has
   finished: a GitHub Actions run by its workflow run's rerun (the token's
   Actions write permission), another App's by a re-request of its check
   suite (an App token's Checks write permission). At a wait on CI, that's
   a cancelled run nothing replaced. A run that timed out is red and goes
   to a fix as any failure does; at a stalled fix that no fix job or
   dependency sweep has (its fix can't be handed), it's run again. A
   commit status, a run in a suite the read didn't hold, or an Actions run
   whose workflow run it didn't name has no way GitHub allows, and is never
   asked for.

Retrying a known flake isn't among them: it needs a flaky-test record Conary
doesn't keep.

The ask is saved before it's made, and its answer after, in the PR's
memory, so whatever GitHub answers, no event, wake or restart asks again in
that stall or for that way on that head; a new head starts afresh. A rerun
that comes back cancelled again stays overdue. A refusal is quoted as
GitHub gave it, its words first in the status (cut to GitHub's 140
characters) and whole in Next's `recovery`, in the pass that got it, since
nothing on GitHub changes to bring a pass, and in every pass of the stall
after it, a fix that still can't be handed included. An answer that never came (GitHub
unavailable, or its answer lost) says so the same way, and isn't asked again
either. The wait itself, its `overdue_at` and its stall clock, stays what
was decided: after its recovery a stall is overdue in the flow's status. A
PR the owner hears of already (open over a day) has no deadline here, and
so no recovery, but for two lost jobs ([Asking the owner](#asking-the-owner)).

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
`overdue_at`, `proof`, `wait_on`, `on_usage`, `owner_notice`, `priority` and, when
any stop it, `switches` ([Switches](#switches)), when any path half-worked,
`degraded` ([Degraded paths](#degraded-paths)), when it lists the owner,
`owner` ([Asking the owner](#asking-the-owner)), and
`revision` (`head`, `base`, `description`), the revision it was decided on;
`error` reports a per-PR failure. Null deadlines remain explicit. These
lines are suitable for comparing with the existing steward beside it,
before any repository is switched over.

`cn flow compare OWNER/REPO [PR]` makes that comparison itself. It does the
same dry run, then reads what the steward running the repository says of
each PR now: the stage its `waiting: …` label names, and its newest
`review-settled` status on the head, counted only from
`settings.flow_authors` (from anyone when that's empty). It prints one
JSON line per PR with `repo`, `pr`, `head`, `agree`, `flow` (`stage`,
`step`, `description`, `switches`, `degraded`), `steward` (`stages`, `state`,
`description`) and
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
[#410](https://github.com/FieldmouseWorks/Conary/issues/410)), and
Conary's from 2026-11-21
([#505](https://github.com/FieldmouseWorks/Conary/issues/505)), was judged
against what GitHub shows for the PR. A kind where the flow was wrong is
fixed and isn't listed. These are the rest. A disagreement of one of these
kinds is accounted for; any other is a finding.

**The steward settles on work the flow doesn't count.** Both read the
reviewer's word the same way ([Review](#review)), but the flow counts a
judgment only as a sealed issuance with its basis, and a job only one it
started. The steward's judgments are `autoland-judgment:` markers with no
basis, and nothing attests them. So where the steward's judge settled a PR
the flow still asks a judge (remi#1301: the steward's judge said land).
Where that settle seated the PR in the steward's train, the flow takes it
out first (`LeaveTrain`) and asks its own judge on the next pass (Conary#539:
its description was edited after Codex's clean review). And while the
steward's own fix job has the branch, the flow sees no job of its own. A dry run has no memory of when it first saw a head, so where the
steward has already asked the reviewer for a look the flow still waits its
half hour; both say `review`.

**A switch the steward doesn't share.** A PR the flow's pause or a hold
stops waits outside every stage, and one no evidence verifier, no person
list or the train off stops waits short of landing, while a steward without
that switch carries on. Its line's `flow.switches` names the switch, so the
stage it gives isn't read as the two seeing the PR differently. An omitted
plugin leaves the stage as decided, so it explains no stage disagreement.

**The steward asks the owner where the flow doesn't.** The steward lists
the owner for a read too large to finish, two jobs lost and a held
major; the flow lists only a [door](#asking-the-owner) and
says the rest itself. Where the steward says `owner` and the flow waits on
one of those, overdue, the two agree on the PR and differ on who's told.

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
