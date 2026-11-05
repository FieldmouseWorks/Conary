{
  description = "Conary: the ledger and landing engine for code that agents write";

  # Pinned by commit, so Renovate moves it with the workflows' pins on burrow
  # (burrow's templates/project/flake.nix).
  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=28981ffbec693afc4a7096d9886f43ffb503cef0";

  # `nix develop` for the toolchains, `nix flake check` for the gate:
  # rustfmt, clippy and the workspace's tests, on rust-toolchain.toml's
  # toolchain (see burrow's lib/project.nix). The tests drive git.
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    rust.testInputs = pkgs: [ pkgs.git ];
  };
}
