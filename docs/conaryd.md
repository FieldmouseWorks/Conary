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

`flow_config` is the [pass configuration](flow.md). `worker` names the installed
`cn` executable that runs owned plugin jobs. `api` optionally overrides the
GitHub API root. The token command has the same contract as `cn`'s token command.
The ledger is the daemon's `--ledger`; `anchor_repo` may be null only when the
flow does not require evidence or train operations. Keep data, policy, config
and the secret file private to the service user. The secret file contains the
exact secret bytes, without a trailing newline; its value never appears in
configuration. Put TLS termination in front of the configured HTTP address.

When a repository is ready to switch, its GitHub webhook uses the externally
reachable receiver URL, content type **application/json**, and the secret from
that file. Select pull requests (including labeled/unlabeled actions), pull
request reviews, pull request review comments, pull request review threads,
issue comments, check suites, check runs, commit statuses and pushes. GitHub's
`label` event changes label definitions; PR label attachments arrive through
`pull_request`. Only pushes to the default branch kick the repo's open PRs.
Issue comments without a PR and events outside this table are acknowledged
without work. Check events without associated PR numbers and commit statuses
kick the repository so the pass can correlate current PR heads. Status echoes
are queued with their GitHub status ID. Before dispatch, the worker drops only
IDs recorded from its own successful status-write responses, including both
pending and success writes and train statuses. Delivery can precede the write
response: the worker classifies echoes after its active pass finishes. Receipts
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

Kicks coalesce at repository scope because each pass observes shared occupancy
and train state. A burst queued before a pass runs makes one pass; kicks during
a running pass make one additional pass. The existing repository file lock
still guards execution. A separate data-directory lock prevents two event
workers from owning the same mailbox. In-flight work is recovered on restart.
A fatal worker error stops the daemon with work still durable; the service
manager can restart it. Errors never print webhook content.

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

The worker sleeps on a condition variable until an incoming kick or that
booked time, consumes the booking once, and runs a pass. There is no periodic
scan or polling timer. Already elapsed deadlines are not rebooked by an
unchanged report. The mailbox, status receipts, failure backoffs and delivery cache are mutable JSON files in
`data`, beside `flow/` memory, with atomic rename and file/directory sync;
they are separate from the append-only ledger.
