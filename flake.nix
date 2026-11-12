{
  description = "Conary: the ledger and landing engine for code that agents write";

  # Pinned by commit, so Renovate moves it with the workflows' pins on burrow
  # (burrow's templates/project/flake.nix).
  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=4e3957fa7d1153728283ed1c5544af01ba181e2e";

  # `nix develop` for the toolchains, `nix flake check` for the gate:
  # rustfmt, clippy and the workspace's tests, on rust-toolchain.toml's
  # toolchain (see burrow's lib/project.nix). The tests drive git.
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    checks = pkgs: {
      package-git = import ./checks/package-git.nix {
        inherit pkgs;
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };
    };
    rust = {
      testInputs = pkgs: [ pkgs.git ];
      runtimeInputs = pkgs: [ pkgs.git ];
    };
  };
}
