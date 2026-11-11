{ pkgs, package }:
pkgs.runCommand "conary-package-git" {
  nativeBuildInputs = [ pkgs.git ];
  passthru.testFiles = [ "checks/package-git.nix" ];
} ''
  export HOME="$TMPDIR/home"
  mkdir -p "$HOME"
  git init repo
  cd repo
  git config user.name Fixture
  git config user.email fixture@example.invalid
  echo carried > file
  git add file
  git commit -m fixture
  git remote add origin https://github.com/fixture/repo.git
  # Keep the GitHub-shaped origin local, including the notes lookup.
  git config url."$PWD".insteadOf https://github.com/fixture/repo.git
  PATH= ${package}/bin/cn --ledger "$TMPDIR/no-ledger" why file:1 -C "$PWD" > result
  grep -q '"source":"notes"' result
  # All entry points, including the daemon, carry the same pinned git.
  for program in cn conaryd conary-mcp; do
    grep -q '${pkgs.git}/bin' ${package}/bin/$program
  done
  touch $out
''
