{
  description = "Conary: the ledger and landing engine for code that agents write";

  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow";

  # `nix develop` for the toolchains, `nix flake check` for the gate:
  # rustfmt, clippy and the workspace's tests, on rust-toolchain.toml's
  # toolchain (see burrow's lib/project.nix).
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    rust = { };
  };
}
