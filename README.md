# Conary

The ledger and landing engine for code that agents write, on top of git and GitHub.

Every change carries why it exists, who made it and what proved it. It lands when that proof is green, and any change, any agent session's work, or everything since a point in time can be undone, always as a new change.

Status: design, and the first crate. See [docs/DESIGN.md](docs/DESIGN.md).

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
