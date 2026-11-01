# AGENTS.md

Conary is the ledger and landing engine for code that agents write, on top of
git and GitHub: every change carries its intent and proof, lands when the proof
is green, and can always be undone. It's at the design stage;
[docs/DESIGN.md](docs/DESIGN.md) is the spec everything here builds toward.

<!-- house:rules v1 begin. Copied from FieldmouseWorks/burrow house/rules.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## House rules

These hold in every repo. This file's own sections add to them and win where
they say something more specific.

- Fix the whole cause. No workarounds, shims, fallbacks or half-migrations:
  when you replace something, delete the old path in the same change.
- One fact, one home. Each rule or fact lives in one place; link to it instead
  of restating it.
- Before you edit, say what done looks like and how it will be checked. Simple
  work gets a short plan, not a ceremony.
- Report only the checks you actually ran, and what they said. Skipped,
  unavailable or "should pass" is never a pass. Keep local and CI results
  apart.
- A new check comes with proof that it fails on the bug it's for.
- Split a code file before it passes 1,000 lines. A PR that grows one past
  that, or grows one already over it, splits it, or says why not on a line in
  its body starting `Why not split:`.
- Rust by default, shell included. New code that's meant to last starts in
  Rust unless its platform picks the language, and anything else takes a
  solid reason: a coin toss goes to Rust. New Python or shell says why it
  isn't Rust, on a line in its PR body starting `Why not Rust:` (a PR that
  adds Python, or grows Python and shell past a fix, needs one): a file a
  tool generates, glue that only runs another program, a throwaway script,
  or a tool still being tuned, ported once it settles. Port existing Python
  and shell whenever Rust buys anything, system and pipeline code first, and
  let the port redraw where the code lives and what owns it rather than copy
  it.
- Work starts from an event (a push, a webhook, a unit finishing), not a
  clock. A PR that adds a timer or a cron schedule says why on a line in its
  body starting `Why a timer:`.
- The code is the memory. When something goes wrong, make the code, a type, a
  test or a check stop it happening again; a new rule in AGENTS.md is the last
  resort. Say in the PR which one you did.
- Keep going on work you were asked to do. Don't stop to ask again for what's
  already been asked for.
- Never commit what the repo keeps private: secrets, tokens, personal data, and
  anything listed under this file's "Private" section.
- `Closes #N` only when the issue is fully done; otherwise `Refs #N`.
- Tracked files never name a model. Which model does what is the machine's
  business, and model names go stale in weeks. A line that has to (a recorded
  screen in a test) gets a line in the PR body starting `Why a model name:`.
- CI's house check holds every PR to the rules above that ask for a line in
  its body, with a reason of four words or more after it, and to its title's
  style. Editing the body or title runs it again.

### Commits

Subjects are `area: plain sentence`, where the area is the module, directory or
tool the change is about: `autoland: merge only the head that was checked`.
Lowercase after the colon, no trailing period, no `feat:`/`fix:` prefixes. The
body, when there is one, says why. A PR's title is the subject main gets when
it lands, so it's written the same way.

Keep the git identity your machine gives you. Where commits are signed, the key
belongs to that identity: a signed commit under someone else's name and email
shows as Unverified on GitHub, and CI fails it.
<!-- house:rules end -->

## Build and test

The gate is the flake, the same locally, in a sandbox and in CI:

    nix develop        # the toolchains
    nix flake check    # the gate: every check CI runs on the code

That covers rustfmt, clippy on all targets with warnings as errors, and the
tests of the Cargo workspace in `crates/`, on the toolchain
`rust-toolchain.toml` pins. The same checks run with plain cargo
([README](README.md#building)). Beside it, `.github/workflows/house.yml` holds
each PR to the house rules a machine can read, its title and body included.

All logic lives in the libraries, `conary-core` and `conary-github` (its GitHub
side); conaryd, the MCP server and the `cn` CLI stay thin layers over them.

Nix is how remi builds and checks Conary, never something the code needs.
Paths, the socket, keys and the service manager are configuration the binary
reads, so a systemd unit or another distro's package is a small wrapper, the
way remi's Nix module is.

<!-- house:prs v1 begin. Copied from FieldmouseWorks/burrow house/prs.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## Pull requests

Open PRs ready for review, never as drafts, and leave them to the steward: it
asks the review bot for a review, and once the bot finds nothing on your head,
every review thread is resolved and CI is green, it merges the PR. Don't merge
it yourself. Fix or answer every finding: push the fix and reply with the
commit, or reply with why it's wrong, then resolve the thread. A PR touching a
one-way path (secrets, CODEOWNERS) gets a judgment first: it lands unless it
can't be undone or is the owner's to decide.

A red default branch (CI failed on its tip) is the steward's to fix too: a
job takes it as soon as the steward sees it, and its fix comes from a
branch named `autoland/red-` and the commit (`gh pr list --search
head:autoland/red-`). Don't fix it yourself, in any repo: it needs one fix,
not several racing each other. Only if the pipeline's stuck issue says the
steward's jobs gave up on it, fix it, from a branch of that name, so the
steward knows it for the fix.
<!-- house:prs end -->

## Code Review Rules

For reviewers, Codex included. Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
- an op log entry, revision or evidence record that can be rewritten or
  deleted: undo is always a new operation, never a removal
- a change ID that can change after creation or that its author chooses, or a
  revision ID that isn't the hash of its base and head commits
- a change's class set or lowered by its author, rather than by policy (the
  author may only raise it)
- trunk moving by any path other than a `land` operation, or an `outside` op
  treated as anything but a violation for the owner; an R2 change landing
  without a person
- evidence counted when it was produced inside the authoring session's
  sandbox or lacks its producer's attestation (conaryd's signature on the op
  doesn't count); a new test accepted without a failing `negative` result
- evidence counted for a tree it didn't run on: a PR's evidence proves its
  revision, never the landed tree, and an R2 change lands only on evidence
  for the exact candidate tree
- a ledger note carrying prompt text, or anything the repo's own rules keep
  out of its history
