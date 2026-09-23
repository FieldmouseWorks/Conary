# Repository Guidelines

## Orientation And Proof

Conary is a virtual Rust workspace: CLI in `apps/conary/`, package behavior in
`crates/conary-core/`, Remi in `apps/remi/`, conaryd in `apps/conaryd/`, and
integration harness in `apps/conary-test/`. Shared bootstrap, agent-contract,
and MCP helpers live under `crates/`; packaging and deployment assets live in
`packaging/` and `deploy/`.

Before feature work, route once with:

```bash
bash scripts/agent-context.sh --feature <slug>
bash scripts/agent-context.sh --path <file>
```

Read the selected packet's start-here files and canonical owners; do not preload
the full feature map or broad subsystem docs.
Use `--run focused` for its narrow proof and `--run gate` only when its stated
interaction condition applies. `docs/llms/README.md` is the assistant entrypoint.

Build or test the owning package (`conary`, `conary-core`, `remi`, `conaryd`, or
`conary-test`) before broadening proof. Repository gates are:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Verification means the exact command ran. Preserve failure evidence, identify
the causal leaf, and create a new head before rerunning an unchanged failed
gate.

## Issues, Branches, And Assistant Work

Follow `CONTRIBUTING.md` for issue, branch, PR, review, and closeout. Non-trivial
work uses one primary GitHub issue and issue-linked branch; never push directly
to `main`. Open substantial changes as draft PRs, resolve review threads, and
use the protected merge path. Use `Closes #...` only when acceptance is met;
otherwise use `Refs #...`. Report security issues privately. Preserve unrelated
dirty work and avoid destructive Git commands in shared worktrees.

Use [the agent workflow](docs/llms/agent-workflow.md) for multi-step execution,
delegation, evidence, authorization, repair limits, interruption, and closeout.
For substantive work, keep one live graph in its primary issue or established
handoff; unlock dependent work only after the parent verifies its artifact and
acceptance evidence. Finish only after authorized integration, evidence
read-back, and cleanup of resources owned by the task. Default model roles,
when available, are `gpt-6-astra` at `max` as primary conversation owner for
architecture, planning, task selection, review, and integration; `gpt-6-sol`
at `max` for complex implementation, refactoring, and debugging; `gpt-6-luna` at `max`
for bounded exploration, documentation, tests, and routine edits. Request the
role and effort explicitly when dispatching useful work, name its inputs,
acceptance check, and file ownership, and protect concurrent edits. If the
requested model is unavailable, report that fact without silent substitution.
These are defaults for the available agent environment, not a contribution
requirement; other tools and human contributors remain welcome. Codex-specific
session controls are described in `docs/llms/openai-codex.md`.

Read-only scoping remains read-only until a change is requested.

## Product And Authority Contract

Conary is pre-alpha until a durable roadmap milestone says otherwise. Make
issue-backed hard cuts: replace the current schema or interface, state rebuild
impact, and remove superseded migrations, adapters, routes, flags, and
compatibility paths in the same slice. Never run old and new authorities in
parallel. Every reader of persisted data must classify an obsolete form as
typed non-authority (rebuild or fencing state), never deserialize it for
compatibility or block the rebuild path with an error.

Cross-distribution package installation is the primary path. The source format
owns lifecycle ABI, dependencies, versions, payload, and configuration;
Conary owns install, update, remove, rollback, and generation publication; the
target supplies typed capabilities. Runtime conversion must not invoke the
source package manager or its database. Adoption/takeover is the sole
migration-continuity exception; see
`docs/specs/foreign-package-lifecycle-contracts.md`.

Derive package behavior from pinned upstream documentation and source, then
encode it as typed grammars, state machines, and conformance tests. Heuristics,
regexes, substring matching, curated tokens, distro-name gates, and silent
defaults may aid diagnostics, redaction, discovery, or prioritization; they may
not establish compatibility, mutation, publication, security, or event
authority. A typed preflight failure is a defect to engineer, not a permanent
unsupported class or human-review queue.

Agent operations use versioned, typed, inspectable resources and plan/apply
results through `conary-agent-contract`; MCP adapts that contract and is never
a second authority. Do not weaken agent trust or approvals, or make an
essential operation available only through ad hoc shell or free-form output.
Redshirt owns reusable experiment tooling in Rust; Conary owns package rules
and independent checks. Follow the
[Redshirt workflow](CONTRIBUTING.md#working-with-redshirt).

## Defects And Maintainability

Fix defects, duplicated authority, and half-implementations found in scope; if
the defect belongs elsewhere, file an exact-evidence issue rather than routing
around it. Prove the cause and contract. Treat intermittent or unexplained
failures as defects.

Rust source files have a 1,000 non-test-line cap and 300 inline test-line cap.
The ownership, exception, extraction, and source-root policy lives in
[`CONTRIBUTING.md`'s maintainability section](CONTRIBUTING.md#maintainability-slices).
Refactors name what moves, its new owner, persisted/public impact, and focused
proof. Update the subsystem map or module doc when the look-here-first path
changes. Meta work remains limited to factual drift, a touched path, or a
failing gate and one meta slice per four product slices until the first
external tester milestone.

## Rust, CLI, And Documentation

Use standard Rust formatting and naming, four-space indentation, `thiserror`
for library errors, and `anyhow` at application boundaries. Every Rust source
file starts with its repo-relative path comment. Use short imperative
Conventional Commit subjects such as `security(federation): pin https peer identity`.

For `apps/conary`, route user-facing status through `apps/conary/src/ui/`; do
not hand-roll status prefixes. `apps/conary/tests/output_vocabulary_guard.rs`
enforces the guarded tags `[ok]`, `[fail]`, `[warn]`, `[skip]`, `[info]`,
`[off]`, `[missing]`, and `[pending]`. Internal tracing is not primary user
output. Logging defaults to `warn`; top-level `--verbose`, `--quiet`, and
`RUST_LOG` keep their documented precedence.

`AGENTS.md` is concise repo policy; `CONTRIBUTING.md` owns contribution
lifecycle; `docs/llms/README.md` routes assistants; feature cards own paths and
proof. Add nested `AGENTS.md` only for genuinely different subtree rules.
Update canonical truth and YAML frontmatter when behavior changes. For public
claims, command help, routes, or agent surfaces, run
`bash scripts/check-doc-truth.sh` plus the owning feature proof. Remove
superseded planning after truth and resume facts move to canonical owners; Git
history is the archive.

Keep credentials, private paths, raw review artifacts, host-local state, and
personal notes out of tracked guidance and public evidence. Use ignored files
such as `docs/operations/LOCAL_ACCESS.md`. <!-- repo-path: local --> Do not weaken HTTPS fingerprint
pinning or other trust defaults casually; Remi or conaryd service changes run
their owning package tests.
