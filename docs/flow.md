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
    "reviewers": ["reviewer"],
    "review_authors": ["landing-app[bot]"]
  },
  "plugins": {
    "review": "/absolute/path/to/reviewer",
    "fix": "/absolute/path/to/fixer",
    "judge": "/absolute/path/to/judge",
    "proof": "/absolute/path/to/proof"
  }
}
```

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

A plugin's proof response is **not signed CI evidence**. The producer must
publish its independently attested checks through the existing evidence
path; until those appear on GitHub, the flow waits. In particular, the
example's canned output cannot authorize a real landing. No plugin response
is inserted into the ledger as an attestation.

## Effects and retries

The executor rechecks the PR revision before effects, publishes the stage
label and `review-settled` status, then performs the selected step. It
reads back settled permission before merging the exact head. Review markers
bind the response to head, base, description and producer completion time.
PR pushes invalidate older scope. Branch updates also name their expected
head. Train admission and departure use the existing train module and its
ledger checks, with `--anchor-repo` required for train-enabled passes.

A refused write is saved as a failure observation for the next decision.
Retry steps perform no further writes until their deadline; three strikes
make the decision overdue. One PR's read, plugin or write failure is
reported on that PR and does not stop processing the others.

## Comparing decisions

```console
cn flow pass OWNER/REPO --config flow.json --policy policy.json --dry-run
cn flow kick OWNER/REPO 123 --config flow.json --policy policy.json --dry-run
```

Dry runs only read GitHub and existing local records. They create no data
directory, lock, worker or GitHub write, and collect no results by mutation.
Train dry runs read the verified local ledger without taking its writer lock.
Output is one JSON object per PR, sorted by PR number. Each successful read
includes `repo`, `pr` and all of Next: `stage`, `step`, `description`,
`overdue_at`, `proof`, `wait_on`, `on_usage`, and `owner_notice`;
`error` reports a per-PR failure. Null deadlines remain explicit. These
lines are suitable for comparing with the existing steward beside it,
before any repository is switched over.
