# AGENTS.md

Conary is version control built for coding agents: changesets that carry their
intent and proof, land on their own when the proof is green, and can always be
undone. It's at the design stage; [docs/DESIGN.md](docs/DESIGN.md) is the spec
everything here builds toward.

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
- Touching a file over about 1,000 lines? Split it in that change.
- The code is the memory. When something goes wrong, make the code, a type, a
  test or a check stop it happening again; a new rule in AGENTS.md is the last
  resort. Say in the PR which one you did.
- Keep going on work you were asked to do. Don't stop to ask again for what's
  already been asked for.
- Never commit what the repo keeps private: secrets, tokens, personal data, and
  anything listed under this file's "Private" section.
- `Closes #N` only when the issue is fully done; otherwise `Refs #N`.
- Tracked files never name a model. Which model does what is the machine's
  business, and model names go stale in weeks.

### Commits

Subjects are `area: plain sentence`, where the area is the module, directory or
tool the change is about: `autoland: merge only the head that was checked`.
Lowercase after the colon, no trailing period, no `feat:`/`fix:` prefixes. The
body, when there is one, says why.
<!-- house:rules end -->

## Build and test

The gate is the flake, the same locally, in a sandbox and in CI:

    nix develop        # the toolchains
    nix flake check    # the gate; CI runs only this

There's no code yet. The Rust workspace (milestone M0 in the design) adds
`rust = { };` to `flake.nix` in the same change, which brings rustfmt, clippy
with warnings as errors, and the workspace's tests into the gate.

## Pull requests

Open PRs as drafts and leave them to the steward: it asks Codex for a review,
and once Codex finds nothing on your head, every review thread is resolved and
CI is green, it marks the PR ready and merges it. Fix or answer every finding:
push the fix and reply with the commit, or reply with why it's wrong, then
resolve the thread. Don't merge or mark ready yourself.

## Code Review Rules

For reviewers, Codex included. Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
- an operation log entry, revision or evidence record that can be rewritten or
  deleted: undo is always a new operation, never a removal
- a changeset ID that can change after creation, or a revision ID that isn't
  the hash of its contents
- a changeset's class set or lowered by its author, rather than by policy
  (the author may only raise it)
- trunk moving by any path other than a `land` operation (the one-time
  baseline git import aside), or an R2 change landing without a person
- evidence counted when it was produced inside the agent's sandbox or isn't
  signed by a runner; a new test accepted without a failing `negative` result
- evidence reused on a new base when anything in the check's footprint may
  have changed: when in doubt, it runs again
- git export that isn't deterministic (the same Conary state must give the
  same SHAs)
