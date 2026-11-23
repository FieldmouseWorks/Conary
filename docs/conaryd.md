# conaryd

The ledger's calls over a Unix socket ([DESIGN section 7](DESIGN.md#7-interfaces)),
for anything on the ledger's box: [the MCP server](mcp.md), Claude jobs, scripts. One
per host. Its socket reads the ledger: `cn` and the steward record ops, and
conaryd reads the log again whenever it or its anchor copy has moved, so an
answer is never older than the last op written before it was asked.

    conaryd --ledger ~/.local/state/conary --socket "$XDG_RUNTIME_DIR/conary/conaryd.sock"

Both default as `cn` does: `CONARY_LEDGER`, or `conary` in the XDG state
directory, and `CONARY_SOCKET`, or `conary/conaryd.sock` in
`$XDG_RUNTIME_DIR` (`/run` when that's unset, for a system service). It
needs the ledger set up (`cn init`) and verifies the log against its key and
anchor before it listens. It holds a lock beside the socket
(`conaryd.sock.lock`) for as long as it runs, so of two started at once
one listens and the other is refused, and a socket left by a conaryd
that's gone is taken over.

Anyone who can open the socket can read the whole ledger, intents included,
so what guards it is the socket's directory: conaryd makes it if it's
missing and leaves its mode to the umask, or to the service that made it.

## The protocol

JSON-RPC 2.0, one request per line and one response per line, in order on
each connection; a notification (no `id`) gets no response. Each connection
is served on its own, and a call reads its own snapshot of the ledger, so a
slow one holds up no one else. What one conaryd takes on is bounded
(`Limits` in [`lib.rs`](../crates/conaryd/src/lib.rs)): 64 connections at
once, one more refused; a connection that sends nothing for a minute, or
won't take its response for ten seconds, closed; a line over 1 MiB refused,
and its connection closed.

    {"jsonrpc":"2.0","id":1,"method":"change.get","params":{"repo":"FieldmouseWorks/burrow","pr":61}}
    {"jsonrpc":"2.0","id":1,"result":{"id":"...","url":"https://github.com/FieldmouseWorks/burrow/pull/61","status":"landed",...}}

The calls are `change.get`, `change.list`, `evidence.query` and `landings`,
and `at`, which answers any of the others as the ledger stood at a time
(`{"time": MS, "method": "change.list", "params": {...}}`; [DESIGN
section 5](DESIGN.md#5-undo) says what it reads). An `at` reads the log on
its own, holding up no other call. Their params, as JSON Schema, are in
[`rpc.rs`](../crates/conary-core/src/rpc.rs) (`METHODS`), the one list every
transport offers. A change that isn't in the ledger is `null`, not an error.
`change.get` includes an undo's immutable `undo.reason`; it is `null` for
historical undo operations recorded before reasons were required.
Errors are JSON-RPC's own codes, and `-32000` for a call that was well formed
and failed: the log didn't verify, say.

Every call reads the ledger and nothing else. In particular conaryd runs no
git: `why` needs a blame in the caller's checkout, and git runs whatever
that checkout's config says to, as whoever runs git. So `why` is answered
where the caller is: `cn why` blames in its own process, and the MCP server
in its, asking conaryd only for the repository's `landings`, the
[notes](notes.md) of its landed changes with nothing left out.

## Gate reuse

The optional `--gate-reuse-config PATH` (`CONARY_GATE_REUSE_CONFIG`) starts
another HTTP listener, which answers whether CI already attested a pass of a
gate on a tree ([gate reuse](gate-reuse.md) says what counts and how it's
configured). It reads GitHub, as the train does, and the ledger, and
records nothing. Its configuration is read again at each lookup, all but
its address live. Its requests are bounded like the webhook's: a head of
16 KiB, a body of 64 KiB, five seconds for the whole request, 16 at once.

## Verified flow events

The optional `--events-config PATH` (`CONARY_EVENTS_CONFIG`) enables a separate
HTTP listener and flow worker. Without it, no listener or flow worker starts.
Nothing configures GitHub or switches a repository automatically. This is the
receiver for M6 slice 4; repository cutover remains slice 5.

The file is machine-owned JSON, with unknown fields rejected:

```json
{
  "listen": "127.0.0.1:8090",
  "secret_path": "/etc/conary/webhook-secret",
  "repos": ["example/project"],
  "data": "/var/lib/conary/data",
  "flow_config": "/etc/conary/flow.json",
  "policy": "/etc/conary/policy.json",
  "token_command": "/usr/local/bin/conary-token",
  "anchor_repo": "example/ledger",
  "worker": "/usr/local/bin/cn"
}
```

conaryd reads this file again at each delivery and each pass
(`events::Live`), so a change to an option takes effect at the next one:
adding a repository to `repos` takes its next event, and taking one off
retires its mailbox without a pass. `listen`, `secret_path` and `data` are
the exceptions: conaryd binds, reads and opens them once, as it starts, and
keeps the values it started with until it restarts. A change to one of
those is logged once, by its name, as waiting for a restart. The
[settings schema](flow.md#the-settings-schema) declares them so, and
`cn flow settings --events-config PATH` lists them apart.

`flow_config` is the [pass configuration](flow.md): one file every listed
repository's pass reads, as above, or an object giving each listed
repository its own, so repositories with different evidence or trains land
through one conaryd:

```json
"flow_config": {
  "example/project": "/etc/conary/flow/example/project.json",
  "example/other": "/etc/conary/flow/example/other.json"
}
```

A pass reads its own repository's file, at the pass, through the loader
`cn flow settings --config` uses, and no other: one repository's broken file
fails only its passes. The events file is refused, as conaryd starts and at
the next delivery or pass, when the object leaves a listed repository out,
names one `repos` doesn't list, names one twice in any case, or has a key
that isn't `OWNER/REPO`; `cn flow settings --events-config` refuses it the
same way and lists which file each repository reads.

`worker` names the installed
`cn` executable that runs owned plugin jobs. `api` optionally overrides the
GitHub API root. The token command has the same contract as `cn`'s token command.
The ledger is the daemon's `--ledger`; `anchor_repo` may be null only when no
repository's flow requires evidence or train operations. Keep data, policy, config
and the secret file private to the service user. The secret file contains the
exact secret bytes, without a trailing newline; its value never appears in
configuration. Put TLS termination in front of the configured HTTP address.

When a repository is ready to switch, its GitHub webhook uses the externally
reachable receiver URL, content type **application/json**, and the secret from
that file. Select pull requests (including labeled/unlabeled actions), pull
request reviews, pull request review comments, pull request review threads,
issue comments, check suites, check runs, workflow runs, workflow jobs, commit
statuses and pushes. GitHub's
`label` event changes label definitions; PR label attachments arrive through
`pull_request`. Only pushes to the default branch kick the repo's open PRs.
Issue comments without a PR and events outside this table are acknowledged
without work. Check events and workflow runs without associated PR numbers
(a train candidate's, among them), commit statuses and a workflow job's start
(its stall clock's, [flow](flow.md#deadlines)) kick the repository so
the pass can correlate current PR heads and step the train
([flow](flow.md#the-train)). Status echoes
are queued with their GitHub status ID. Before dispatch, the worker drops only
IDs recorded from its own successful status-write responses, including both
pending and success writes and train statuses. Delivery can precede the write
response: the worker classifies a repository's echoes after its running pass
finishes. Receipts
survive restart and are retained for 24 hours. External writes, including writes
to Conary's own status contexts, still kick a pass and recheck permission.
Stage labels already present are read without posting them again.

Only POST is accepted. `X-Hub-Signature-256` must contain the HMAC-SHA256 of the
exact request body, verified with a constant-time tag comparison before JSON
parsing or queue access. Missing or incorrect authentication returns 401;
SHA-1 alone is refused. Headers are capped at 16 KiB and bodies at 1 MiB;
oversized bodies return 413. The receiver logs no secrets, signatures or body
text. Signed malformed JSON returns 400, unlisted repos are ignored, and
accepted deliveries return 202 after durable enqueueing, without waiting for
a GitHub read or pass. Delivery IDs are remembered for 24 hours, including
across restart; replays are acknowledged and dropped.

Passes of different repositories run at once, one at a time per repository:
each repository's pass starts on a thread of its own as soon as it's due,
whatever another repository's pass is doing, so one whose GitHub reads are
slow or hang holds up only itself, but for the reads a merge or a train step
makes under the ledger's writer lock ([flow](flow.md#state-and-workers)). Kicks coalesce at repository scope because
each pass observes shared occupancy and train state. A burst queued before a
pass runs makes one pass; kicks during a running pass make one additional
pass, after it. A running repository's status events, due bookings and
recovery likewise wait for its pass to end. The existing repository file lock still guards execution. A separate
data-directory lock prevents two event workers from owning the same mailbox.
Every pass running when conaryd stopped runs again as it starts.

A flow worker's answer (a review's, fix's, judge's, preparation's or
proof's) is read only by a pass, and most publish nothing GitHub sends an
event for, so conaryd kicks the worker's repository as the worker exits, however it ended: answered, failed or cancelled. Starting, it
kicks every listed repository once, so an answer saved while it was down is
read by that first pass. A worker that outlives the daemon that launched it
still wakes the next one: each worker holds a lock, taken before it starts,
until it exits, and a starting daemon waits on the lock of each worker with
no answer yet and kicks its repository once it's free, its own cancellation
of that worker included. A worker's deadline stays booked as the backstop for
one whose exit nobody saw.

Each read of GitHub goes to the end of its data: a list a page of 100 at a
time until GitHub has no more, and a PR's snapshot with every check suite on
its head, the rest after its first query's page read a page at a time on
that same head. No count of pages fails a read. Each call is cut off after a
minute, and each read, all its pages, has a deadline of its own, five
minutes (`READ_FOR` in [`api.rs`](../crates/conary-github/src/api.rs)), each
page's call cut off at what's left of it. A read past its deadline fails as
GitHub unavailable, like any other read that fails: what the flow does with
one is [flow](flow.md#degraded-paths)'s, and its pass books its recovery as
below.

A whole-repository pass that fails before its reports (GitHub unavailable, a
token command failing) books a durable recovery for that repository instead of
stopping the worker: 60 seconds after the first failure, counted from when the
pass gave up however long it ran, doubling per failure
in a row up to 64 minutes, and cleared by the next pass that finishes. Until its
recovery time the repository is held: kicks and status events arriving
meanwhile stay queued, and its PR bookings stay unspent, and all of them run
in that one retry. Other repositories go on advancing, and the ledger socket
keeps answering. The recovery, the held kicks and the bookings survive restart.
The log names the repository, its failures in a row and its retry time, never
the error's text. Only a mailbox that can't be written stops the worker, once
the passes running end, and the daemon with it, so the service manager can restart it. Errors never print
webhook content.

Each report's `Next.wait_until` names its explicit wait or owned worker deadline;
`overdue_at` names the stall deadline. The mailbox books the earliest future
time per PR, replacing earlier bookings after a new repo pass and removing
closed PRs. A per-PR error, even one without a decision, books a durable recovery
wait: 60 seconds initially, doubling per failed report up to 64 minutes. Other
PRs keep their successful bookings. A successful report cancels that PR's retry;
an absent report after a successful repository read retires its closed PR.
These are failure-triggered bookings, not periodic repository scans. Explicit
waits name the first second their predicate permits progress (the quiet handoff
requires the second after its inclusive quiet period).

The worker sleeps on a condition variable until a kick (a webhook's, a flow
worker's exit, or its own start), a pass ending or that booked time, consumes
the booking once, and starts a pass. There is no periodic
scan or polling timer. Already elapsed deadlines are not rebooked by an
unchanged report. The mailbox, status receipts, failure backoffs, repository recoveries and delivery cache are mutable JSON files in
`data`, beside `flow/` memory, with atomic rename and file/directory sync;
they are separate from the append-only ledger.
