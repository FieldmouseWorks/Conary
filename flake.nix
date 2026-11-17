{
  description = "Conary: the ledger and landing engine for code that agents write";

  # Pinned by commit, so Renovate moves it with the workflows' pins on burrow
  # (burrow's templates/project/flake.nix).
  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=61c1e8cd945f6a9093aa23a2ced76b1d49432d6a";

  # Conary's toolchain can be newer than burrow's. Own the overlay pin so
  # lock maintenance keeps it current instead of restoring burrow's lock.
  inputs.rust-overlay.url = "git+https://github.com/oxalica/rust-overlay?shallow=1";
  inputs.rust-overlay.inputs.nixpkgs.follows = "burrow/nixpkgs";
  inputs.burrow.inputs.rust-overlay.follows = "rust-overlay";

  # `nix develop` for the toolchains, `nix flake check` for the gate:
  # rustfmt, clippy and the workspace's tests, on rust-toolchain.toml's
  # toolchain (see burrow's lib/project.nix). The tests drive git.
  outputs = { self, burrow, ... }: burrow.lib.project {
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
