# Conary

The ledger and landing engine for code that agents write, on top of git and GitHub.

Every change carries why it exists, who made it and what proved it. It lands when that proof is green, and any change, any agent session's work, or everything since a point in time can be undone, always as a new change.

Status: M1 and M2 are done. `cn` records every PR on the box with the
ledger and answers `why` there, any clone answers it from the
[notes](docs/notes.md), and conaryd and its MCP server serve the ledger to
everything else on the box. Landing and undo, above, are where it's going:
the train, which gates every ready change with one run, is M3, next, then
`undo` at M4, and Conary landing changes itself, in place of the steward,
is M6. The milestones are in
[docs/DESIGN.md](docs/DESIGN.md#12-milestones).

## Using it

`cn` keeps the ledger in `$XDG_STATE_HOME/conary` (or `--ledger`) and reads
GitHub with `GH_TOKEN`, or with each repository's own token from a command
(`CONARY_TOKEN_COMMAND`), as a GitHub App's are: one owner's each. The ledger
is anchored at a ref in the repository `CONARY_ANCHOR_REPO` names (the token
writes it): `cn init` sets it up once, and every write moves it on, so a log
that loses ops is refused. It prints JSON; add `--human` for people.

    cn init
    cn backfill FieldmouseWorks/burrow --reviewer <steward-app>[bot]
    cn ingest FieldmouseWorks/burrow 61 --reviewer <steward-app>[bot]
    cn log --human
    cn why -C ~/src/burrow modules/claude.nix:40 --human
    cn verify    # the log against the anchor on GitHub, all of its history

Each write also brings the repository's notes up to date, and `cn why` in a
clone with no ledger beside it answers from them ([notes](docs/notes.md)).

`cn train` runs a repository's train ([the
train](docs/DESIGN.md#47-the-train)) for the steward, as its App: `join` puts
a settled PR in at its head, and `step`, on each of the repository's events,
builds the next candidate, reads its gate, and lands a green one's PRs in
order. `--require` names the checks the gate waits for even before they
report. `verdict` records one `step` can't tell: `dropped`, for a gate that
never finished.

    cn train join FieldmouseWorks/remi 174 --head <sha>
    cn train step FieldmouseWorks/remi --require gate
    cn train verdict FieldmouseWorks/remi <candidate> dropped
    cn train show FieldmouseWorks/remi --human

`cn --help` and each command's `--help` list the rest, environment variables
included.

`conaryd` serves the same ledger over a Unix socket, as JSON-RPC, for
anything on the box ([conaryd](docs/conaryd.md)), and `conary-mcp` offers it
to agents as MCP tools ([the MCP server](docs/mcp.md)).

## Building

The Rust workspace is in `crates/`. It needs only rustup and cargo, on any
Linux: rustup reads `rust-toolchain.toml` and installs the toolchain it pins.

    cargo build
    cargo test
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings

With Nix, the flake runs the same checks in a sandbox. That's the gate CI runs:

    nix develop        # a shell with the toolchains
    nix flake check    # rustfmt, clippy and the tests, as above

## License

MIT licensed. See [LICENSE](LICENSE).
