# Conary

The ledger and landing engine for code that agents write, on top of git and GitHub.

Every change carries why it exists, who made it and what proved it. It lands when that proof is green, and any change, any agent session's work, or everything since a point in time can be undone, always as a new change.

Status: design. See [docs/DESIGN.md](docs/DESIGN.md).

## Building

There's no code yet. The build and its checks run through Nix:

    nix develop        # a shell with the toolchains
    nix flake check    # the gate CI runs

## License

MIT licensed. See [LICENSE](LICENSE).
