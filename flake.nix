{
  description = "Conary: version control built for coding agents";

  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow";

  # `nix develop` for the toolchains, `nix flake check` for the gate. No
  # languages yet: the Rust workspace (M0 in docs/DESIGN.md) adds `rust = { };`
  # here in the same change (see burrow's lib/project.nix).
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
  };
}
