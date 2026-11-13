# AGENTS.md

Conary is the ledger and landing engine for code that agents write, on top of
git and GitHub: every change carries its intent and proof, lands when the proof
is green, and can always be undone. The ledger, the merge train and `cn undo`
run on remi today; [docs/DESIGN.md](docs/DESIGN.md) is the spec everything here
builds toward, and its [section 12](docs/DESIGN.md#12-roadmap) is the road to
done. Where the rest lives:

- [README](README.md): what it does today, and building it with plain cargo.
- [docs/ci-attestation.md](docs/ci-attestation.md): how CI attests evidence,
  and the negative runs.
- [docs/conaryd.md](docs/conaryd.md), [docs/mcp.md](docs/mcp.md): the
  ledger's calls over its socket and as MCP tools.
- [docs/notes.md](docs/notes.md): how a clone that can't reach the ledger
  reads it.

<!-- house:rules v1 begin. Copied from FieldmouseWorks/burrow house/rules.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## House rules

These hold in every repo. This file's own sections add to them and win where
they're more specific.

- Design the class, not the instance. When a bug, finding or stall shares a
  cause with others, fix the cause once, where it lives (one owning path, a
  type, a check), so none of its cases come back.
- Do work once. Look for what's already open or built first, extend the
  shared crate, client or job that does it rather than write another copy,
  and never run work twice that once would do.
- Fix the whole cause: no workarounds, shims, fallbacks or half-migrations.
  When you replace something, delete the old path in the same change.
- One fact, one home. Each rule or fact lives in one place; link to it
  instead of restating it.
- Pipeline options live in the settings source: change them through its
  door, read them with `proj settings show`, and link the
  [settings reference](https://github.com/FieldmouseWorks/burrow/blob/main/docs/settings-reference.md)
  rather than restate a value or where one is kept.
- Before you edit, say what done looks like and how it will be checked.
- Report only the checks you ran and what they said. Skipped, unavailable
  or "should pass" is never a pass. Keep local and CI results apart.
- A new check comes with proof that it fails on the bug it's for.
- The code is the memory. When something goes wrong, make the code, a type,
  a test or a check stop it happening again; a new rule here is the last
  resort. Say in the PR which one you did.
- Keep going on work you were asked to do: don't ask again for what's
  already been asked for.
- Never commit what the repo keeps private: secrets, tokens, personal data,
  and anything under this file's "Private" section.
- `Closes #N` only when the issue is fully done; otherwise `Refs #N`.

CI's house check holds every PR to its title's style and to the rules below,
each of which a PR can pass with a line in its body starting as shown and a
reason of four words or more. Editing the body or title runs it again.

- Rust by default, shell included: new code meant to last starts in Rust
  unless its platform picks the language, and a coin toss goes to Rust. A PR
  that adds Python, or grows Python and shell past a fix, says `Why not
  Rust:` (a file a tool generates, glue that only runs another program, a
  throwaway script, a tool still being tuned). Port existing Python and
  shell whenever Rust buys anything, system and pipeline code first.
- Work starts from an event (a push, a webhook, a unit finishing), not a
  clock. A PR that adds a timer or a cron schedule says `Why a timer:`.
- Split a code file before it passes 1,000 lines, or say `Why not split:`.
- AGENTS.md is a map: what the repo is, its own rules, and where things live
  (a feature map, the doc that owns each subject). Detail goes in the doc
  that owns it, linked from here. A PR that grows the repo's own part (the
  lines outside these blocks) past 100 lines says `Why a longer guide:`.
- Tracked files never name a model: which model does what is the machine's
  business, and names go stale in weeks. A line that has to (a recorded
  screen in a test) says `Why a model name:`.

### Commits

Subjects are `area: plain sentence`, the area being the module, directory or
tool the change is about (`autoland: merge only the head that was checked`):
lowercase after the colon, no trailing period, no `feat:`/`fix:`. The body,
if any, says why. A PR's title is the subject main gets, so it's written the
same way. Keep the git identity your machine gives you: a commit signed with
a key that isn't its author's shows as Unverified, and CI fails it.
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

One task, one PR: finish what the task set out to do, then open one PR, so
it goes through review and CI once. No PR per step or round, and no stacks.
A fix to the pipeline (the steward, CI, the machine) lands as soon as it's
ready, and so does a change to these rules that the work needs, in a PR of
its own: reviewers read the default branch's rules, not the PR's.

Open PRs ready for review, never as drafts, and leave them to the steward:
once the review bot finds nothing on your head, every thread is resolved and
CI is green, it merges. Don't merge it yourself. Answer every finding: push
the fix and reply with the commit, or reply with why it's wrong, then
resolve the thread. A PR touching a one-way path (secrets, CODEOWNERS) gets
a judgment first, and lands unless it can't be undone or is the owner's
call.

A red default branch is the steward's to fix, from a branch named
`autoland/red-` and the commit. Don't fix it yourself, in any repo, unless
the pipeline's stuck issue says the steward's jobs gave up; then fix it from
a branch of that name.
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
- evidence counted for a tree it didn't run on: evidence proves the tree it
  ran on and may count only for a landing with that exact tree, with the
  check's identity, definition and attestation intact; tree equality alone
  never carries a review or judgment; an R2 change lands only on evidence
  for the exact candidate tree and trunk binding
- a ledger note carrying prompt text, or anything the repo's own rules keep
  out of its history
