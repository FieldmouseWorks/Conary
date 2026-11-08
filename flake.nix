{
  description = "Conary: the ledger and landing engine for code that agents write";

  # Pinned by commit, so Renovate moves it with the workflows' pins on burrow
  # (burrow's templates/project/flake.nix).
  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=aeb284d399c8f4c72b1d7135c4e7b4fa0e4a441b";

  # `nix develop` for the toolchains, `nix flake check` for the gate:
  # rustfmt, clippy and the workspace's tests, on rust-toolchain.toml's
  # toolchain (see burrow's lib/project.nix). The tests drive git.
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    rust.testInputs = pkgs: [ pkgs.git ];
  };
}
