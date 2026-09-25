# Contributing to Conary

Thank you for your interest in contributing to Conary. Whether you are fixing a typo, reporting a bug, or building a major feature, your contribution matters. This guide covers everything you need to get started.

## Table of Contents

- [Getting Started](#getting-started)
- [Building from Source](#building-from-source)
- [Running Tests](#running-tests)
- [Code Style](#code-style)
- [Module Overview](#module-overview)
- [Development Workflow](#development-workflow)
- [Issue Reporting](#issue-reporting)
- [Architecture Decisions](#architecture-decisions)
- [Code of Conduct](#code-of-conduct)
- [License](#license)

## Getting Started

### Prerequisites

- **Rust 1.98.0+** (edition 2024) -- install via [rustup](https://rustup.rs/)
- **Git**
- **Linux** -- Conary uses Linux-specific APIs (namespaces, landlock, seccomp) and does not currently build on macOS or Windows

### Your First Contribution

Not sure where to start? Browse
[open issues](https://github.com/FieldmouseWorks/Conary/issues), especially
`good first issue` and `help wanted`. If the problem or desired outcome is
still unclear, open a thread in
[GitHub Discussions](https://github.com/FieldmouseWorks/Conary/discussions) and a
maintainer can help turn it into a real, bounded issue.

Small first contributions often include:
- Adding or improving unit tests (look for modules with low coverage)
- Fixing clippy warnings or improving error messages
- Documentation improvements in doc comments
- Small bug fixes in package parsers

Concrete first-wave validation tasks include:

- Extend `apps/conary/tests/live_host_mutation_safety.rs` with one more
  refusal-before-mutation case.
- Add a daily-driver CLI diagnostic assertion in
  `apps/conary/tests/cli_daily_ux.rs`.
- Clarify one checked behavior in `docs/SCRIPTLET_SECURITY.md` and verify it
  against `crates/conary-core/src/scriptlet/`.
- Improve the preview install path in `site/src/routes/install/+page.svelte`
  without adding unsupported claims.
- Add a narrow invariant to `scripts/check-doc-truth.sh` for a claim that has
  drifted before.
- Tighten one source-selection example in `docs/modules/source-selection.md`
  against `crates/conary-core/src/repository/effective_policy.rs`.

### Fork and Clone

```bash
# Fork the repository on GitHub, then:
git clone https://github.com/YOUR_USERNAME/Conary.git
cd Conary

# Add upstream remote
git remote add upstream https://github.com/FieldmouseWorks/Conary.git
```

### Using Coding Assistants

Conary explicitly welcomes contributors who work with an LLM or coding agent.
Use the tool that works for you; contributions are evaluated on the resulting
design, code, safety, tests, and review evidence, not on whether a human typed
every line. The repository is deliberately structured so a person and their
coding buddy can discover ownership and proof without private prompt lore.
Agent assistance does not weaken branch, review, security, or verification
requirements, and no proprietary assistant is required.

If you work with an LLM coding tool, read `AGENTS.md` and this contribution
workflow, then ask the repository router for the smallest relevant context:

```bash
bash scripts/agent-context.sh --list
bash scripts/agent-context.sh --feature <slug>
```

The selected packet points to its start-here files, owned paths, and focused
proof. Use `--brief` when only the route is needed.

Tool-specific files such as `CLAUDE.md`, `.agents/rules/conary.md`, and
`.github/copilot-instructions.md` are compatibility shims. Prefer the linked
canonical docs over copied instructions or stale local lore. Google agent work
uses Antigravity/`agy`.

For multi-step assisted work, use the
[agent execution workflow](docs/llms/agent-workflow.md) for task graphs,
delegation, authorization, evidence, interruption, and closeout. This guide
remains the owner of issue, branch, PR, review, and merge lifecycle.

## Building from Source

```bash
# Debug builds
cargo build -p conary
cargo build -p remi
cargo build -p conaryd

# Release build (optimized, slower to compile)
cargo build -p conary --release
```

For repeated builds, the repository-owned development environment shares
eligible compiler outputs across linked worktrees while leaving each
worktree's Cargo target isolated:

```bash
scripts/dev-build.sh cargo -- build -p conary
scripts/dev-build.sh cargo -- test -p remi
scripts/dev-build.sh iterate -- build -p conary
scripts/dev-build.sh status
```

The shared compiler cache lives under the common Git directory and is bounded
to 10 GiB by default. Existing `RUSTC_WRAPPER`, `CARGO_TARGET_DIR`,
`SCCACHE_DIR`, and `SCCACHE_CACHE_SIZE` values retain precedence. Use
`scripts/dev-build.sh clean --yes` only when intentionally clearing that
marked compiler cache; it never removes Cargo targets.

Use `iterate` for an optimized edit/build/run loop. It selects the repository's
no-LTO incremental `fast-release` profile and keeps the caller's isolated Cargo
target. That artifact is development-only: release, promotion, and final
performance evidence continue to use the exact `--release` profile.

The project root is a virtual Cargo workspace with eight members:
`apps/conary`, `apps/remi`, `apps/conaryd`, `apps/conary-test`,
`crates/conary-agent-contract`, `crates/conary-bootstrap`,
`crates/conary-core`, and `crates/conary-mcp`.
EROFS support uses `composefs-rs` directly in `crates/conary-core`.

## Running Tests

```bash
# conary CLI, matching CI's conary shard in .github/workflows/pr-gate.yml
cargo test -p conary --no-default-features --test test_hook_ownership --verbose
cargo test -p conary --features test-hooks --verbose

# Core library
cargo test -p conary-core

# Service-owned code
cargo test -p remi
cargo test -p conaryd

# Test harness
cargo test -p conary-test

# Run a specific integration-test target owned by a package
cargo test -p conary --test database

# Library tests only
cargo test --lib

# Workspace doctests
cargo test --doc --workspace
```

Some `conary` and `conary-core` suites depend on host privileges, UID, umask,
namespaces, or tools and fail on ordinary developer hosts (tracked in #733).
Report those by exact test name against that issue and rely on CI's provisioned
workspace shards; do not retry or skip them silently.

All tests must pass before submitting a PR. At minimum, run the verification path that matches the code you touched:

1. `cargo fmt --check` -- formatting
2. `cargo clippy --workspace --all-targets -- -D warnings` -- workspace lint gate
3. `cargo test -p conary --no-default-features --test test_hook_ownership --verbose` and
   `cargo test -p conary --features test-hooks --verbose` -- the conary CLI shard CI runs
4. `cargo test -p remi` -- when touching Remi/server/federation code
5. `cargo test -p conaryd` -- when touching daemon code

Run these locally before pushing to save CI round-trips:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p conary --no-default-features --test test_hook_ownership --verbose
cargo test -p conary --features test-hooks --verbose
```

If your change touches Remi, daemon code, federation, or service-owned shared types, also run:

```bash
cargo test -p remi
cargo test -p conaryd
```

## Code Style

### General Conventions

- **File headers**: Every Rust source file starts with its repo-relative path as a comment:
  ```rust
  // apps/conary/src/commands/example.rs
  ```
- **Database-first**: All runtime state lives in SQLite. No config files (INI, TOML, YAML, JSON) for runtime state.
- **CLI output**: Route user-facing status through `apps/conary/src/ui/` and
  use the guarded lowercase vocabulary such as `[ok]`, `[fail]`, and `[warn]`.
  Do not hand-roll status prefixes.
- **Clippy-clean**: All code must pass `cargo clippy --workspace --all-targets -- -D warnings`. Pedantic lints are encouraged.
- **Unit-test placement**: Keep small tests in an inline `#[cfg(test)] mod tests`.
  Once a Rust source file carries more than 300 inline unit-test lines, its unit
  tests belong in the sibling `<file>/tests.rs` behind `#[cfg(test)] mod tests;`.
  A new sibling extraction must reduce the parent, and its commit must state
  that reduction. Package-level integration tests remain under the package's
  top-level `tests/` directory.

### Rust Specifics

- Edition 2024, minimum supported Rust version 1.98.0
- Use `thiserror` for library/module error types
- Use `anyhow` for application-level error propagation
- Minimize `.unwrap()` in production code paths -- prefer `?` or explicit error handling
- Keep ownership explicit: service and daemon code live in `apps/remi` and `apps/conaryd`, not behind a root feature flag

### Maintainability Slices

Refactor and cleanup PRs are welcome when they make ownership clearer. Keep
them focused: name the current responsibility, the module or helper that should
own it, and the focused verification command that proves behavior is preserved
or intentionally changed.

`scripts/check-line-cap.sh` measures all lines outside syntax nodes whose combined
`cfg` predicates can hold with `test` enabled but cannot hold with it disabled
under any assignment of other cfg atoms. Items, fields, statements, and
expressions all participate. It caps the non-test portion at 1,000 lines and
the total inline test-only spans at 300 lines. A file named `tests.rs` or below
`tests/` is exempt only when its file-level gate, declaring contexts, or Cargo
integration-test target membership establish test-only compilation. Production
imports override test-target membership. Production macro invocations or
opaque attribute expansion make context-only exemptions unknown; unknown files
retain both caps. Extracted test files carry `#![cfg(test)]`, which the
compiler enforces even when another declaring site loads the file. Sanctioned
parent gates remain in place. The gate reads offline Cargo metadata v1,
honoring custom targets and disabled auto-discovery. A custom tree without a
manifest has no Cargo target evidence. Literal and literal-concatenated
`include!` paths participate in the graph; even apparently builtin include
calls need compiler name resolution, so they also invalidate context-only
exemptions. Loads outside scanned roots retain uncertainty; inert
compiler-owned attributes from the pinned Rust catalog preserve complete
contextual proof.

The declared production roots are the top-level `apps/` and `crates/` trees.
`third_party/` is excluded as vendored upstream source (aws-creds, rust-s3,
resolvo, and tantivy) patched into the build by path from `Cargo.toml`; it is
neither measured nor allowlist-eligible. Adding a top-level Rust source root
fails until its policy is recorded in `SOURCE_ROOTS` in
`crates/conary-xtask/src/line_cap.rs`. Under `--report`, the gate prints each
root's file count, every test filename as `TEST FILE:` with measured content
and context, and the number of files with unresolved expansion authority.
Cap-checked files also receive the ordinary row. Names ending in `_tests.rs`
retain their existing measurement rules; an over-cap production file under a
test filename still needs an owned allowlist exception. Parent rows attribute
proven test-only siblings as `attributed_test_lines`; compare base and head
reports to prove extraction reduced the parent. Current sibling size alone is
not a historical delta. Attributes whose final path segment is `test` also
mark inline tests, including those enabled by `cfg_attr` in a test build.

Once a Rust source file has more than 300 inline unit-test lines, move those
tests to sibling `<file>/tests.rs` behind `#[cfg(test)] mod tests;`. The
extraction must reduce the parent, and the commit states that reduction. Every
checked-in exception names the open issue that owns its decomposition. The
checked-in `scripts/line-cap-issue-state.txt` snapshot records each cited
issue's state and the canonical allowlist entries those states were read for;
the hermetic gate compares that entry set with the allowlist. Refresh the
snapshot with `scripts/refresh-line-cap-issue-state.sh` after any allowlist
edit. The gate performs no network I/O; the PR gate and
`.github/workflows/line-cap-issue-state.yml` run
`scripts/refresh-line-cap-issue-state.sh --check` against
`github.com/FieldmouseWorks/Conary`. That workflow runs on issue
closure, reopening, and deletion, every six hours, and by manual dispatch.
Lookup failures and closed owners fail without rewriting the snapshot. Add
behavior to an over-cap file only through the issue's ownership-based
reorganization. Thin registration, dispatch, and re-export wiring may remain
in a large hub only through an issue-linked exception.

Custom scans through `scripts/check-line-cap.sh` that override `--root` or
`--allowlist` must also supply `--issue-state`. An omitted snapshot is a usage
error; the wrapper never substitutes repository state for a custom scan.

Before changing behavior in a Rust file over 1,500 lines, name the ownership
boundary being preserved or improved. Files over 2,500 lines need a reviewed
decomposition path before major feature work unless the fix is urgent.

Large files are review signals. Use `scripts/line-count-report.sh` to refresh
the current hotspot list when planning broad maintenance work. Do not split a
file only to reduce line count; split when a responsibility has a clearer home.
Persisted state, package formats, trust metadata, and integration-test
manifests need explicit compatibility or migration decisions before they change.

Before a broad refactor or cleanup PR, run
`scripts/maintainability-drift-report.sh` for a warn-only view of changed-path
owner hints, focused proof commands, documentation-truth health, and current
Rust hotspots. Treat its output as review guidance, not as a substitute for
the feature card or the tests you actually ran.

### Fix The Class, Not The Instance

When a defect, review finding, or correction exposes a recurring class, fix the
class at the strongest rung that applies instead of patching only the reported
instance:

1. **Make it unrepresentable.** Prefer types, exhaustive matches without
   catch-alls, and one owning API. An exhaustive destructure that makes a new
   field fail to compile, or a typed enum instead of a boolean flag, is stronger
   than a check a future editor can forget.
2. **Enforce it statically.** Use the compiler, clippy, or a repository guard
   such as `apps/conary/tests/output_vocabulary_guard.rs`,
   `scripts/check-line-cap.sh`, or `scripts/check-doc-truth.sh`.
3. **Write guidance.** Put the rule in the affected feature card, module doc, or
   `AGENTS.md` when it cannot be enforced mechanically.
4. **Review alone.** Rely on review only when nothing stronger applies.

State which rung the change used in the PR. When an anti-pattern cannot be
removed in the current slice, add a guard that stops new instances first, then
file the cleanup. Comments state contracts and invariants; a comment that
justifies a deviation is a defect, not documentation.

### Feature Ownership And Verification

Use `docs/modules/feature-ownership.md` when a change is easier to describe as
a feature than as a crate or file. Each card names the files to read first, the
neighboring systems that can be affected, a focused proof command for small
edits, and a broader interaction gate for behavior that crosses subsystem
boundaries.

Small docs-only or module-local changes do not need full workspace validation by
default. Run the focused proof for the touched feature, then add the broader
gate when the card's neighboring systems are affected.

### Commit Messages

This project uses [Conventional Commits](https://www.conventionalcommits.org/). Every commit message must start with a type prefix:

| Prefix | When to use | Version bump |
|--------|-------------|-------------|
| `feat:` | New feature or capability | Minor |
| `fix:` | Bug fix | Patch |
| `docs:` | Documentation only | None |
| `refactor:` | Code restructure, no behavior change | None |
| `test:` | Test additions or changes | None |
| `chore:` | Build, tooling, dependencies | None |
| `security:` | Security fix | Patch |
| `perf:` | Performance improvement | Patch |

Add `!` after the type for breaking changes: `feat!: remove superseded API`.

Scopes are optional but encouraged: `feat(resolver): add SAT backtracking`.

Use the imperative mood in the subject line (e.g., "add sparse index support" not "added sparse index support"). Keep the subject under 72 characters. The release pipeline (`scripts/release.sh`, backed by `scripts/release-matrix.sh`) uses these prefixes to determine version bumps and generate changelogs.

## Module Overview

The project is a virtual Cargo workspace with eight members: four app crates
and four shared crates.

**`apps/conary`** -- CLI binary

| Module | Purpose |
|--------|---------|
| `apps/conary/src/cli/` | CLI definitions and argument parsing |
| `apps/conary/src/app.rs` | Startup/bootstrap wiring |
| `apps/conary/src/dispatch.rs` | Top-level command routing |
| `apps/conary/src/commands/` | Command implementations |

**`crates/conary-core`** -- Core library

| Module | Purpose |
|--------|---------|
| `crates/conary-core/src/db/` | SQLite current-schema definition, validation, and models |
| `crates/conary-core/src/packages/` | RPM/DEB/Arch package parsers unified through `PackageMetadata` |
| `crates/conary-core/src/compression/` | Unified decompression (Gzip, Xz, Zstd) with format detection |
| `crates/conary-core/src/repository/` | Remote repository metadata sync, mirror logic, and Remi client |
| `crates/conary-core/src/resolver/` | SAT-based dependency graph resolution |
| `crates/conary-core/src/filesystem/` | Content-addressable storage and file deployment |
| `crates/conary-core/src/delta/` | Binary delta updates |
| `crates/conary-core/src/version/` | Version parsing and constraint matching |
| `crates/conary-core/src/container/` | Scriptlet sandboxing via Linux namespace isolation |
| `crates/conary-core/src/trigger/` | Post-install trigger system |
| `crates/conary-core/src/scriptlet/` | Scriptlet execution with cross-distro support |
| `crates/conary-core/src/label.rs` | Package provenance labels |
| `crates/conary-core/src/flavor/` | Build variation specifications |
| `crates/conary-core/src/components/` | Component classification |
| `crates/conary-core/src/transaction/` | Composefs-native transaction pipeline and conflict preflight |
| `crates/conary-core/src/model/` | System Model and remote include handling |
| `crates/conary-core/src/ccs/` | CCS native package format (builder, policy engine, OCI export) |
| `crates/conary-core/src/recipe/` | Recipe system for building packages from source |
| `crates/conary-core/src/capability/` | Capability declarations, enforcement, and inference |
| `crates/conary-core/src/provenance/` | Package DNA and provenance tracking |
| `crates/conary-core/src/automation/` | Automated maintenance (security updates, orphan cleanup) |
| `crates/conary-core/src/bootstrap/` | Bootstrap a complete Conary system from scratch |
| `crates/conary-core/src/generation/` | EROFS generation building, composefs mounting, artifact export, CAS GC |
| `crates/conary-core/src/derivation/` | CAS-layered derivation engine for bootstrap |
| `crates/conary-core/src/trust/` | TUF supply chain trust |
| `crates/conary-core/src/canonical/` | Exact versioned cross-distro package-map contracts; AppStream/Repology discovery caches are non-authoritative |
| `crates/conary-core/src/self_update.rs` | Self-update version checking, download, atomic replacement |
| `crates/conary-core/src/hash.rs` | Multi-algorithm hashing (SHA-256, XXH128) |

**`crates/conary-bootstrap`** -- Shared binary bootstrap helpers

| Module | Purpose |
|--------|---------|
| `crates/conary-bootstrap/src/lib.rs` | Shared tracing, runtime, and exit-code helpers for workspace binaries |

**`crates/conary-agent-contract`** -- Shared agent contract types

| Module | Purpose |
|--------|---------|
| `crates/conary-agent-contract/src/lib.rs` | Versioned tool, resource, risk, and approval contract types for agent-facing Conary surfaces |

**`crates/conary-mcp`** -- Shared transport-agnostic MCP helpers

| Module | Purpose |
|--------|---------|
| `crates/conary-mcp/src/lib.rs` | MCP server plumbing shared across workspace apps |

**`apps/remi`** -- Remi server + federation service

| Module | Purpose |
|--------|---------|
| `apps/remi/src/server/` | Remi on-demand CCS conversion proxy, search, admin API, and MCP server |
| `apps/remi/src/federation/` | CAS federation -- peer discovery, chunk routing, allowlists, TLS pinning |

**`apps/conaryd`** -- conaryd daemon

| Module | Purpose |
|--------|---------|
| `apps/conaryd/src/daemon/` | conaryd REST API, SSE events, job queue, and systemd integration |

**`apps/conary-test`** -- Declarative test infrastructure (TOML manifests, container management)

| Module | Purpose |
|--------|---------|
| `apps/conary-test/src/config/` | TOML manifest and distro config parsing |
| `apps/conary-test/src/engine/` | Test suite, runner, assertions |
| `apps/conary-test/src/container/` | ContainerBackend trait and container lifecycle |
| `apps/conary-test/src/report/` | JSON output and SSE event streaming |
| `apps/conary-test/src/remi_client.rs` | Remi test-data API client and retained result-streaming path |
| `apps/conary-test/src/wal.rs` | Retained SQLite result buffer for the planned Remi streaming path |

## Development Workflow

GitHub is the day-to-day coordination surface for Conary work:

| Surface | What it owns |
|---------|--------------|
| Issue | Problem, in/out scope, acceptance criteria, current status, and follow-up work |
| Pull request | Proposed diff, review discussion, exact verification, and integration record |
| Roadmap | Ordered project state, blockers, and proof expectations across issues |
| Canonical docs and specs | Current product, operator, architecture, persisted-contract truth, and durable design decisions |

Issues and PRs link these surfaces together; they do not replace durable
repository documentation.

### 1. Start With An Issue

Search open and closed issues before creating a new one. Use the matching
GitHub issue type:

- **Bug** for behavior that differs from the supported contract.
- **Feature** for a new capability or user-visible improvement.
- **Task** for implementation slices, refactors, documentation, operations,
  releases, investigations with a deliverable, and project maintenance.

The issue should name the problem, in-scope and out-of-scope work, acceptance
criteria, owning subsystem or boundary, and expected proof. A broad initiative
may remain open across several focused PRs or use sub-issues; each PR still
names one primary issue.

Use Discussions while an idea is still exploratory. Pure read-only triage does
not need a new issue until it becomes proposed work. A truly trivial correction
such as spelling or a dead link may go straight to a focused PR, but `main`
still never receives a direct push. Report vulnerabilities through a private
security advisory, not a public issue.

### 2. Preserve The Project Gates

Run `bash scripts/agent-context.sh --feature <slug>` or
`bash scripts/agent-context.sh --path <file>` before feature-scoped work. Record
the owning boundary and focused proof in the issue, then preserve any design,
security, migration, compatibility, or maintainer-decision gate printed by the
feature card.

Create or update a tracked design or implementation plan only when the work
needs durable decision detail or spans multiple coordinated steps. Link it from
the issue. The issue owns work status; the document owns the durable decision
or execution detail. Completed planning is removed after its truth and resume
facts have moved to canonical owners, as described under Documentation Hygiene.

### 3. Branch From Current `main`

For a maintainer checkout:

```bash
git switch main
git pull --ff-only
git switch -c fix/42-rpm-parser-overflow
```

Contributors working from forks should refresh from `upstream/main` before
creating the branch. Include the issue number in a short descriptive name:

- `fix/42-rpm-parser-overflow`
- `feat/57-sparse-index`
- `docs/63-update-architecture` <!-- repo-path: hypothetical -->
- `chore/71-refresh-dependencies`

Do not commit or push repository changes directly to `main`.

### 4. Implement And Open A Pull Request

Keep each PR to one reviewable logical change. New features need focused tests;
bug fixes need a regression test that fails without the fix. Open a draft PR
early when the work is substantial or when review can help settle an active
decision.

Before pushing, run the focused proof and interaction gate selected by the
feature card. Broad changes should also run the full local CI path:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude conary-test --verbose
cargo test -p conary --no-default-features --test test_hook_ownership --verbose
cargo test -p conary --features test-hooks --verbose
cargo test -p conary-test --verbose
```

Add `cargo test -p remi` and `cargo test -p conaryd` when the change touches
service-owned code.

Hosted `pr-gate` (`.github/workflows/pr-gate.yml`) runs only for pull requests
targeting `main` (`on: pull_request: branches: [main]`, plus
`workflow_dispatch`). A stacked PR based on another branch therefore gets no
hosted checks; its proof is the full local gate above, which includes the conary
shard's `test-hooks` and `test_hook_ownership` runs, or a `workflow_dispatch`
run on its branch. When a non-required matrix cell fails, compare its failing
test ID and error against the known cause before treating it as pre-existing: a
suite stops at its first fatal failure, so a known-red cell can mask a new
regression behind it.

Every non-trivial PR names one primary issue:

- Use `Closes #42` only when merging the PR will satisfy the issue's acceptance
  criteria.
- Use `Refs #42` when the PR is one slice of a larger issue. Record the
  remaining work on the issue and leave it open.

The PR description explains the problem, scope, ownership boundary, user or
developer impact, exact verification commands and results, and any linked
roadmap, design, or plan.

### 5. Review, Merge, And Close

The default-branch ruleset requires changes to arrive through a PR, required CI
checks to pass, and every review thread to be resolved, including one an
automated reviewer re-posts for a finding already fixed. Reply on such a thread
with the fixing commit and resolve it rather than re-fixing; never merge over a
thread that is genuinely new. Respond to review feedback in the PR and keep its
verification section current after material changes.

The maintainer bypass is PR-only so an urgent merge still leaves an issue, diff,
discussion, and audit trail. Use it only when waiting for a required check would
cause more harm than merging, and record the reason plus replacement proof in
the issue and PR.

Merge through GitHub, then let GitHub delete the head branch. Confirm that the
issue closed when the PR used `Closes`; otherwise update the open issue with
what landed, what remains, and the next acceptance boundary.

## Issue Reporting

### Bug Reports

When filing a bug report, please include:

- Conary version (`conary --version`)
- Linux distribution and version
- Steps to reproduce
- Expected vs. actual behavior
- A reviewed support bundle when host state matters:
  `bash scripts/conary-support-bundle.sh target/conary-support-bundle`

The support bundle is allowlist-only and does not copy `conary.db`, raw logs,
environment dumps, shell history, private keys, SSH keys, `/etc/conary/trust`,
host-local access notes, or package payloads. Share raw `RUST_LOG=debug` output
only when a maintainer asks for it, and review/redact it before posting.

### Feature Requests

Feature requests are welcome. Please search existing issues first to avoid duplicates, and describe:

- The problem you are trying to solve
- Your proposed solution (if you have one)
- Any alternatives you considered

## Architecture Decisions

Conary has a few core design principles that inform how contributions should be structured. Understanding these will help your PR get accepted:

- **Database-first**: SQLite is the single source of truth for all package state. Do not introduce config files, caches outside the database, or in-memory-only state for data that should persist.
- **Content-addressable storage**: Files are stored by hash, enabling deduplication and efficient delta updates.
- **Explicit transaction and recovery boundaries**: Package operations record durable state in SQLite and use command-specific live-root journals or generation recovery paths. Keep failure handling fail-closed, and do not assume every host mutation has automatic rollback without focused proof.
- **Package-owned service surfaces**: Remi and conaryd live in their own app crates and should be built and tested directly with `cargo build -p remi`, `cargo build -p conaryd`, `cargo test -p remi`, and `cargo test -p conaryd`.

Before proposing significant architectural changes, please open an issue to discuss the approach. This helps avoid wasted effort and ensures alignment with the project direction.

## Documentation Hygiene

- Treat active docs as current-state references, not historical logs.
- Keep detailed roadmap state under `docs/roadmaps/`. Record durable design
  decisions in the architecture, module, or `docs/specs/` document that owns
  the affected surface. Track bounded multi-step execution in the primary
  issue and draft pull request; stable public or persisted contracts remain
  under `docs/specs/`.
- After canonical truth, proof, roadmap state, and resume facts are durable,
  delete completed, superseded, or abandoned planning. Use Git history for
  historical context; do not create a replacement planning archive.
- Run `bash scripts/check-doc-truth.sh` and the owning feature-card proof when
  editing a public claim or assistant-facing route.
- When editing files under `docs/`, update YAML frontmatter (`last_updated`, `revision`, `summary`) unless the file is intentionally exempt.

## Getting Help

If you have questions about contributing, feel free to start a thread in [GitHub Discussions](https://github.com/FieldmouseWorks/Conary/discussions) or open an issue on the [GitHub repository](https://github.com/FieldmouseWorks/Conary). We are happy to help newcomers find their way around the codebase.

## Code of Conduct

Participation in Conary project spaces is governed by the
[Conary Code of Conduct](CODE_OF_CONDUCT.md). Report sensitive conduct concerns
privately using the contact listed there.

## License

The Conary client and every library crate are dual-licensed under the [MIT License](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE); the Remi server under `apps/remi` is licensed under the [GNU AGPL v3 or later](apps/remi/LICENSE). By submitting a pull request, you agree that your contribution is licensed under the terms that apply to the files you change, and that contributions to the dual-licensed crates may be used under either license. `scripts/check-license-authority.sh` pins each crate to its declared license.
