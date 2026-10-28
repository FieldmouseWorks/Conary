# conaryd

The ledger's calls over a Unix socket ([DESIGN section 7](DESIGN.md#7-interfaces)),
for anything on the ledger's box: the MCP server, Claude jobs, scripts. One
per host. It only reads: `cn` and the steward still record the ops, and
conaryd reads the log again whenever it or its anchor copy has moved, so an
answer is never older than the last op written before it was asked.

    conaryd --ledger ~/.local/state/conary --socket "$XDG_RUNTIME_DIR/conary/conaryd.sock"

Both default as `cn` does: `CONARY_LEDGER`, or `conary` in the XDG state
directory, and `CONARY_SOCKET`, or `conary/conaryd.sock` in
`$XDG_RUNTIME_DIR` (`/run` when that's unset, for a system service). It
needs the ledger set up (`cn init`) and verifies the log against its key and
anchor before it listens. A socket left by a conaryd that's gone is taken
over; one another conaryd still answers on is refused.

Anyone who can open the socket can read the whole ledger, intents included,
so what guards it is the socket's directory: conaryd makes it if it's
missing and leaves its mode to the umask, or to the service that made it.

## The protocol

JSON-RPC 2.0, one request per line and one response per line, in order on
each connection; a notification (no `id`) gets no response. Each connection
is served on its own, and a call reads its own snapshot of the ledger, so a
slow `why` holds up no one else. A line over 1 MiB is refused and the
connection closed.

    {"jsonrpc":"2.0","id":1,"method":"change.get","params":{"repo":"FieldmouseWorks/burrow","pr":61}}
    {"jsonrpc":"2.0","id":1,"result":{"id":"...","url":"https://github.com/FieldmouseWorks/burrow/pull/61","status":"landed",...}}

The calls are `change.get`, `change.list`, `evidence.query` and `why`. Their
params, as JSON Schema, are in
[`rpc.rs`](../crates/conary-core/src/rpc.rs) (`METHODS`), the one list every
transport offers. A change that isn't in the ledger is `null`, not an error.
Errors are JSON-RPC's own codes, and `-32000` for a call that was well formed
and failed: the log didn't verify, or git couldn't blame the line.
