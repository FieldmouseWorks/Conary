{
  description = "Conary: the ledger and landing engine for code that agents write";

  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-26.05&shallow=1";
    rust-overlay = {
      url = "git+https://github.com/oxalica/rust-overlay?shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "git+https://github.com/ipetkov/crane?shallow=1";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
    }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll =
        f:
        lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );

      rev = self.rev or self.dirtyRev or null;
      toolchainSpec = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
      toolchainFor =
        pkgs: extra:
        pkgs.rust-bin.fromRustupToolchain (
          toolchainSpec
          // {
            components = (toolchainSpec.components or [ ]) ++ extra;
          }
        );

      craneFor = pkgs: (crane.mkLib pkgs).overrideToolchain (toolchainFor pkgs [ ]);

      # Existing workspace members under crates/: listed individually in
      # rust-test's passthru.testFiles so a new crate added by a PR is not
      # treated as covered on the base commit during negative runs.
      workspaceMembers = builtins.filter (
        name:
        (builtins.readDir ./crates).${name} == "directory"
        && builtins.pathExists (./crates + "/${name}/Cargo.toml")
      ) (builtins.attrNames (builtins.readDir ./crates));

      # Secrets scan over tracked files with gitleaks' credential rules,
      # dropping path-based allowlists and the noisy generic-api-key rule.
      gitleaksConfig =
        pkgs:
        pkgs.runCommand "gitleaks.toml"
          {
            nativeBuildInputs = [
              pkgs.remarshal
              pkgs.jq
            ];
          }
          ''
            toml2json ${pkgs.gitleaks.src}/config/gitleaks.toml | jq '
              def unpathed: [ .[]
                | if has("paths") and ((.condition // "or") | ascii_downcase) == "and" then empty else del(.paths) end
                | select(has("regexes") or has("stopwords") or has("commits")) ];
              def allowlists: .allowlists = ((.allowlists // []) + [.allowlist // empty] | unpathed) | del(.allowlist);
              allowlists
              | .rules |= map(select(.id != "generic-api-key") | allowlists | if .allowlists == [] then del(.allowlists) end)
            ' > config.json
            jq -e '([.. | objects | select(has("paths") or has("allowlist"))] == []) and (.rules | length > 100)' config.json >/dev/null
            json2toml config.json $out
          '';

      secretsCheck =
        pkgs:
        pkgs.runCommand "secrets"
          {
            nativeBuildInputs = [
              pkgs.gitleaks
              pkgs.jq
            ];
            passthru.testFiles = [ ];
          }
          ''
            cd ${self}
            status=0
            gitleaks dir --no-banner --redact --config ${gitleaksConfig pkgs} --max-target-megabytes 5 \
              --exit-code 2 --report-format json --report-path "$TMPDIR/leaks.json" . >"$TMPDIR/gitleaks.log" 2>&1 || status=$?
            if grep -q 'Invalid .gitleaksignore entry' "$TMPDIR/gitleaks.log"; then
              echo ".gitleaksignore has lines that aren't a fingerprint (FILE:RULE:LINE), and they ignore nothing"
            fi
            if [ -e "$TMPDIR/leaks.json" ]; then
              jq -r '.[] | "\(.File):\(.StartLine): \(.RuleID) (fingerprint \(.Fingerprint))"' "$TMPDIR/leaks.json"
            fi
            case $status in
              0) jq -e 'length == 0' "$TMPDIR/leaks.json" >/dev/null ;;
              2) exit 1 ;;
              *)
                echo "gitleaks stopped (exit $status) before it finished. Its log isn't shown here; nix build this check with --keep-failed and read gitleaks.log in the build directory it keeps."
                exit 1
                ;;
            esac
            touch $out
          '';

      packageFor =
        pkgs:
        let
          craneLib = craneFor pkgs;
          src = craneLib.cleanCargoSource self;
          cargoArtifacts = craneLib.buildDepsOnly {
            pname = "conary";
            version = "0.0.0";
            inherit src;
            strictDeps = true;
            cargoExtraArgs = "--locked --workspace";
          };
        in
        craneLib.buildPackage {
          pname = "conary";
          version = "0.0.0";
          inherit src cargoArtifacts;
          strictDeps = true;
          cargoExtraArgs = "--locked --bin cn --bin conaryd --bin conary-mcp";
          doCheck = false;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            for p in $out/bin/*; do
              wrapProgram "$p" --prefix PATH : ${lib.makeBinPath [ pkgs.git ]}
            done
          '';
          passthru = { inherit rev; };
        };

      checksFor =
        pkgs:
        let
          craneLib = craneFor pkgs;
          # Full cleaned source so tests can read fixtures outside crates/
          # (e.g. docs/lessons.toml and JSON fixtures under crates/).
          src = lib.cleanSource self;
          common = {
            pname = "conary";
            version = "0.0.0";
            inherit src;
            strictDeps = true;
            cargoExtraArgs = "--locked --workspace";
            CARGO_PROFILE = "dev";
          };
          cargoArtifacts = craneLib.buildDepsOnly common;
        in
        {
          rust-fmt = craneLib.cargoFmt {
            pname = "conary";
            version = "0.0.0";
            inherit src;
            cargoExtraArgs = "--all";
            passthru.testFiles = [ ];
          };
          rust-clippy = craneLib.cargoClippy (
            common
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
              passthru.testFiles = [ ];
            }
          );
          rust-test = craneLib.cargoTest (
            common
            // {
              inherit cargoArtifacts;
              nativeBuildInputs = [ pkgs.git ];
              passthru.testFiles = [
                "crates/*"
                "docs/lessons.toml"
              ]
              ++ map (name: "crates/${name}/**") workspaceMembers;
            }
          );
          package-git = import ./checks/package-git.nix {
            inherit pkgs;
            package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          };
          secrets = secretsCheck pkgs;
        };
    in
    {
      packages = forAll (pkgs: {
        default = packageFor pkgs;
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (toolchainFor pkgs [
              "rust-src"
              "rust-analyzer"
            ])
            pkgs.git
          ];
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);

      checks = forAll checksFor;
    };
}
