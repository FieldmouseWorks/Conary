# Conary

The ledger and landing engine for code that agents write, on top of git and
GitHub.

## Why it exists

Code review, approvals, merge queues and release freezes all assume two
things: writing a change is slow, and undoing one is expensive. A bad change
cost days to write and days to unwind, so it was worth stopping everything to
check it first. With agents writing the code, a change takes minutes, and so
does a revert. Most of those gates now cost more than the mistakes they catch.

What lets them go is undo you can trust, and undo needs a record git doesn't
keep. git knows what changed. It doesn't know why, which agent session made
the change, what checked it, or whether that check ran somewhere the agent
couldn't touch. Without that, "undo everything that session did, across four
repos" is an afternoon of `git revert` by hand, and "is this green check real"
is a guess.

Conary keeps that record and builds on it:

- **Every change has a why.** Each pull request becomes a change with its
  intent (the task, a summary), its author and session, its class, every head
  it was pushed at, the evidence it landed on, and the landing itself. The
  record is an append-only, signed op log, anchored on GitHub so a log that
  loses entries is refused.
- **`why` works from anywhere.** Ask why a line looks the way it does and get
  the change, its task, its session and its evidence. A fresh clone with no
  access to the ledger answers from git notes on the landed commits.
- **Evidence has to mean something.** Agents don't grade their own work: a
  result counts only when it came from outside the authoring session and its
  producer vouches for it. A new test has to fail on the old code. Evidence
  names the exact tree it ran on, and never stands in for another.
- **Changes land when their proof allows.** The train checks every change
  that's ready with one gate run instead of one per PR, and lands the green
  ones in order.
- **Anything can be undone.** One change, every change one agent session
  landed, or everything since a point in time. An undo is always a new
  change through the same flow; nothing is rewritten.
- **Only what undo can't fix waits for a person.** Leaked secrets, dropped
  data and the owner's own decisions (class R2) still need someone to say
  yes. Everything else lands on evidence.

Conary isn't a new version control system. Agents keep cloning, committing
and pushing with plain git, and trunk is the default branch on GitHub. An
early draft planned a VCS of its own; that's parked until something turns up
that can't be built on git, with the case written down. Nor does it run
builds or tests: CI and other runners produce evidence, and Conary records
it.

The full design, including what's parked and why, is
[docs/DESIGN.md](docs/DESIGN.md).

## Where it stands

Early. It runs on one machine, remi, over one owner's repositories, alongside
[burrow's steward](https://github.com/FieldmouseWorks/burrow/blob/main/docs/repo-baseline.md#the-pr-flow-the-steward),
the service that gets PRs reviewed and merges them there today. Conary takes the
steward's jobs over one at a time.

- **Done:** the ledger records every PR on remi and answers `why`, there and
  from any clone (M1, M2). conaryd serves it over a socket, and an MCP server
  offers it to agents.
- **In progress:** the train (M3). It has landed remi's PRs since
  2026-10-30. What's left is the evidence verifier, so a candidate counts
  only on attested results.
- **Next:** `undo` by change, session and time (M4), then class policy (M5),
  then Conary running the whole landing flow in place of the steward (M6).

The milestones and their exit checks are in
[DESIGN section 12](docs/DESIGN.md#12-milestones).

## Trying it

Conary records its own PRs, so this repository is the quickest look. With a
full clone and cargo:

    git clone https://github.com/FieldmouseWorks/Conary && cd Conary
    cargo run --release -p conary-cli -- why --human crates/conary-core/src/why.rs:1

`cn why` fetches the notes from `origin` and names the PR that last touched
that line, what it was for, who made it and what it landed on. After that,
`git log --notes=conary` shows every landed commit's note
([notes](docs/notes.md)).

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
