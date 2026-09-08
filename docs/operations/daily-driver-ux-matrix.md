---
last_updated: 2026-09-08
revision: 8
summary: Daily-driver CLI routes, coordinated progress, typed first-use and CCS verification diagnostics, strict verification JSON, and focused output proof
---

# Daily-Driver UX Matrix

## Purpose

This matrix is the Goal 7 contract for daily operator wording. It keeps
common package-manager commands boring, testable, and honest after the
structural readiness goals. It does not expand support claims: when a workflow
still belongs to the native package manager, adoption refresh, explicit
takeover, generation activation, or conaryd, the CLI should say that directly.

## Command Matrix

| Command | Success Route | Refusal Or Unsupported Route | Operator Guidance Phrase | Focused Test Target |
|---|---|---|---|---|
| `install <pkg>` | Conary-owned package install or dry-run plan | Adopted package already belongs to native authority | `conary system adopt --refresh` before retry; `conary install <pkg> --ownership takeover --yes` for explicit package takeover; `conary system takeover --yes` for generation-level takeover | `cargo test -p conary --test cli_daily_ux adopted_install_refusal_routes_to_refresh_and_takeover` |
| `install <pkg> --dry-run` | Reports a would-be dependency-to-explicit promotion without changing installed state, even with `--yes` | Ambiguous installed variants require exact selection | Use `--version` and `--arch` to select the intended installed variant | `cargo test -p conary --lib commands::install::command::tests` |
| `remove <pkg>` | Conary-owned package removal; Debian residual conffiles are preserved | Adopted package removal without `--purge` | Use `--purge` to delete residual config state or externally owned adopted files; use `conary system unadopt <pkg> --yes` to stop adopted tracking without deleting files | `cargo test -p conary --test cli_daily_ux adopted_remove_refusal_routes_to_unadopt_or_purge` |
| `update [pkg]` | Conary-owned update or security update from trusted advisory metadata | Adopted package update remains externally owned, unsupported advisory source fails before mutation | Refresh adoption after external changes; use `--ownership takeover` only for explicit Conary takeover | `cargo test -p conary --test cli_daily_ux adopted_update_routes_to_native_pm_and_refresh` |
| `search <pattern>` | Repository search results from synced metadata | Empty or stale repository metadata | Run `conary repo sync` before assuming a package is unavailable | Existing query/search tests plus `cargo run -p conary -- search --help` |
| `list [pkg]` | Installed package identity, files, path owner, pinned state | Ambiguous installed package variants | Use `--version` and `--arch` to select a specific installed variant | Existing `cargo test -p conary --test query list_info_refuses_ambiguous_variants_until_selector_is_given` |
| `autoremove` | Removes Conary-owned orphaned dependency packages | Adopted orphaned packages remain native-PM owned | Native package-manager authority is preserved for adopted orphans | Existing `cargo test -p conary --test native_pm_daily_driver autoremove_dry_run_lists_conary_owned_orphans_and_skips_adopted` |
| `pin <pkg>` | Pins a selected installed variant | Ambiguous installed variants | Use `--version` and `--arch` to pin the intended variant | Existing `cargo test -p conary --test query pin_and_unpin_use_same_variant_selector` |
| `unpin <pkg>` | Releases a selected installed variant | Ambiguous installed variants | Use `--version` and `--arch` to unpin the intended variant | Existing `cargo test -p conary --test query pin_and_unpin_use_same_variant_selector` |

## Cross-Cutting Routes

- Live-host mutation refusal should offer three clear paths: use `--dry-run`
  for preview, rerun the specific apply command with `--yes` when mutating the
  real machine is intended, or use conaryd package jobs when the operator needs
  durable background execution with the same intent boundary.
- Every applied install, update, remove, autoremove, automation, CCS, and
  conaryd package operation executes the complete typed lifecycle graph. There
  is no script-suppression flag or daemon request field; `--dry-run` is the
  non-mutating planning route.
- Shell integration is verified by rendering completion output, not by visual
  review. Goal 7 requires at least:

```bash
cargo run -p conary -- system completions bash >/tmp/conary-completion.bash
cargo run -p conary -- system completions zsh >/tmp/conary-completion.zsh
```

- Generation guidance should stay in the generation command family. Daily
  package commands may point to `conary system generation build` or
  `conary system generation switch` only when the next user action is genuinely
  generation activation, rollback, or export.
- conaryd guidance is operator routing text for durable package jobs. It is not
  a new UI client and does not loosen the live-host mutation acknowledgement.

## Package Progress Contract

`apps/conary/src/commands/progress.rs` owns package phase wording;
`apps/conary/src/ui/progress.rs` owns terminal rendering and row lifetime.
Install, update, removal, and adoption share one terminal coordinator. Unknown
or single-package totals render one cyan spinner; larger nonzero totals render
an aggregate bar and one active status row. No placeholder bar is registered.

Completion and early errors clear transient rows. Install, update, and removal
leave durable result wording to their command summaries. Adoption emits its
completion count as durable output, including in pipes. UI messages suspend
redraw while writing, so nested package operations can retain their summaries.
Live redraw requires both output streams to be terminals and a missing or empty
`NO_COLOR`; pipes and nonempty `NO_COLOR` keep only durable messages.

The focused proof includes `cargo test -p conary --test cli_progress`, which
captures real terminals with `script -qec`, plus screen-state tests under
`cargo test -p conary --lib ui::progress`. The audited single-package frame had
an extra `░… 0/0` row and a completion spinner touching the following summary.
The renderer now shows one phase row, erases it, and leaves the command summary
on its own line. Tests assert row count, phase text, retained diagnostics,
nested cleanup, and no redraw after return. Concurrent fetching remains #535.
First-use diagnostic rendering is covered below.

## First-Use Diagnostic Contract

`apps/conary/src/ui/diagnostics.rs` renders application failures as one `error:`
line, indented facts, and separate `note:` actions. `LiveMutationRefusal` retains
the exact command and mutation class through error context; the existing
`--dry-run` and `--yes` gate remains its authority. A refusal renders, for example:

```text
error: Confirmation is required before applying changes.
  Command: conary install
  Impact: May change packages, files, scriptlets, ownership, or the live Conary database.
  Root: Current --root or similar arguments are not sufficient isolation for this command yet.
note: Use --dry-run when available to preview first.
note: Rerun this command with --yes when you intend to apply it.
```

The previous frame joined these facts and actions into one paragraph. Custom
missing-database errors now put the exact path in a `Database` field without
Rust debug quotes and retain the custom-path initialization route. Unclassified
application errors retain their cause chain as separate `Cause` fields. Rendering
does not derive remedies by parsing error text; a generic conflict does not
recommend removing a package or using an unverified `--force` flag.

Pending publication prints one warning with its changeset, a retained failure
reason when present, and the existing exact retry command as a note. Its internal
tracing record is debug-level, so the default warning log no longer repeats the
same user-visible warning. Publication outcomes and retry authority are unchanged.

`cargo test -p conary --test cli_diagnostics` asserts exact terminal, pipe, and
`NO_COLOR` frames and proves first-use refusals create no database or other files.
`cargo test -p conary --lib ui::diagnostics` checks typed refusal downcasts,
unclassified cause retention, publication facts, and one default warning/retry.
CCS verification retains its typed trust-policy cause and input archive path
through install and verification contexts. Untrusted signer output labels the
package-provided key ID as a claim; control characters in displayed facts are
escaped to keep them on one visible line. The exact public key remains the trust-anchor
identity. A failed `ccs verify` emits no success preamble:

```text
error: CCS package signer is not trusted.
  Package: <fixture>/diagnostic-1.0.0-1.ccs
  Claimed key ID: fixture-signer
  Public key: <exact Ed25519 public key>
note: Verify the signing key through a trusted source and use a policy that authorizes this package.
```

This replaces repeated archive/context lines and `key_id=Some(...)` debug text.
Missing, malformed, future, and expired timestamps retain distinct causes;
expiry includes timestamp, age, and configured maximum. Invalid authority
retains every diagnostic code, field, path, and publisher-facing suggestion.
Host-capability preflight names the required interface, hook or affected path,
and the existing inventory-refresh action. UI rendering never authorizes a
signing key or changes a capability requirement.

The verification capture also proves that changing to a policy containing the
actual signing key allows the same intact archive to verify. Core tests prove
untrusted archives cannot commit payload objects into CAS and preserve all
validation diagnostics through both document and streaming readers.

`ccs verify --json` emits the versioned verification report shared with the local
MCP `conary.packaging.verify_artifact` tool. Verification failures preserve exit
status 1 and emit one JSON object without a duplicate human error. Raw typed
fields survive serialization independently of the human renderer's escaping.
See [CCS Verification Report V1](../specs/ccs-verification-report-v1.md) for the
strict schema, explicit-policy MCP boundary, and authority contract.

Remaining #644 work includes machine/refusal coverage on other command surfaces
and consistent fields, headings, and empty states.

## Ranked UI Slices

These slices change rendering and presentation, not package, publication,
query, download, or boot behavior. Each lands separately under #132 unless a
focused issue is created first. Proof for every slice includes before/after
evidence plus `cargo test -p conary --test output_vocabulary_guard` and
`cargo test -p conary --test cli_daily_ux`; snapshot changes also run
`cargo test -p conary --test cli_output_snapshots`.

1. **Fix TTY progress rendering** — For `install`, `update`, and `remove`, stop
   rendering zero-length bars. Single-package operations get one spinner line
   that clears to the final summary; bars appear only with known non-zero
   totals. Keep the primitive capable of a bounded aggregate-plus-worker layout
   for #535. Add a pty capture with `script -qec`.
2. **One warning/error voice** — Route deferred or stuck publication warnings
   once through `ui::warn`, retain tracing for logs rather than duplicate
   default output, render application failures through `ui::error_line`, align
   clap's visible vocabulary, and state each fact and remedy once. #534 owns
   publication behavior; this slice owns rendering.
3. **Transaction summary block** — Give `install --dry-run`, `install --yes`,
   `update`, and `remove` one shared summary renderer for install, upgrade, and
   remove groups; version, architecture, source format, file count, size, and
   disk delta; and a closing line that distinguishes planning from apply.
4. **Typed preflight rendering** — Render signature, authority, and preflight
   refusals from their fields: one `error:` line naming the cause, indented
   facts without debug wrappers or repeated paths, and one `note:` remedy.
5. **Field/heading unification and empty-state phrasing** — Route `list --info`,
   `ccs build`, `system history`, and list/search/update empty states through
   `ui::field` and `ui::heading`; preserve guarded ASCII tags and one phrasing
   pattern per empty state. Core returns typed CCS summary data for rendering at
   the application boundary. History drops hand-rolled tags and repeated retry
   prose. Update snapshots in the same slice.
6. **Structured refusal layout** — Keep the live-host refusal routes from this
   matrix, presented as a short cause plus `note:` next steps. Update
   `live_host_mutation_safety` expectations in the same slice.

## Release Honesty

Do not mark an unsupported route as implemented in docs unless the focused test
target above or the referenced integration suite proves it. Keep active docs
clear that native package managers remain authoritative for adopted packages
until the user chooses explicit takeover.
