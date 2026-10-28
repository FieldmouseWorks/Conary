# The MCP server

`conary-mcp` offers the ledger's calls as MCP tools to agents on the
ledger's box ([DESIGN section 7](DESIGN.md#7-interfaces)). It speaks MCP
over stdio and forwards each call to [conaryd](conaryd.md), so it holds no
ledger of its own and needs nothing but conaryd's socket (`--socket`, or
`CONARY_SOCKET`, defaulting as conaryd does). For Claude Code:

    claude mcp add conary -- conary-mcp

The tools are conaryd's calls, named with `_` for `.` (`change_get`,
`change_list`, `evidence_query`, `landings`), with the same params and
schemas ([`rpc.rs`](../crates/conary-core/src/rpc.rs), `METHODS`), and
`why`. `why` takes an absolute checkout `dir`, a `path` in it and a `line`.
It blames the line here, in the MCP server, as the agent that started it,
and asks conaryd only for the repository's `landings`: conaryd runs no git
for anyone ([why](conaryd.md#the-protocol)).

Every tool only reads. Each result is the call's JSON, as text. A call
conaryd refuses, a blame git refuses, or a call made while conaryd isn't
running comes back as the tool's error, for the agent to read. A dropped
connection is made again on the next call, so conaryd can restart, or
close an idle connection, under a running agent.
