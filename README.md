# Conary

The ledger and landing engine for code that agents write, on top of git and GitHub.

Every change carries why it exists, who made it and what proved it. It lands when that proof is green, and any change, any agent session's work, or everything since a point in time can be undone, always as a new change.

Status: milestone M1, recording PRs and answering `why`. See
[docs/DESIGN.md](docs/DESIGN.md).

## Using it

`cn` keeps the ledger in `$XDG_STATE_HOME/conary` (or `--ledger`) and reads
GitHub with `GH_TOKEN`. The ledger is anchored at a ref in the repository
`CONARY_ANCHOR_REPO` names (the token writes it): `cn init` sets it up once,
and every write moves it on, so a log that loses ops is refused. It prints
JSON; add `--human` for people.

    cn init
    cn backfill FieldmouseWorks/burrow --reviewer <steward login>
    cn ingest FieldmouseWorks/burrow 61 --reviewer <steward login>
    cn log --human
    cn why -C ~/src/burrow modules/claude.nix:40 --human
    cn verify    # the log against the anchor on GitHub (--whole: its whole history)

`cn --help` and each command's `--help` list the rest, environment variables
included.

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
