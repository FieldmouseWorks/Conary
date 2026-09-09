---
last_updated: 2026-09-09
revision: 25
summary: Daily-driver CLI routes, enabled-source discovery, grouped transaction results, scoped recovery, coordinated progress, and typed diagnostics
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

## Repository Discovery

`search` and `query repquery` result lists, plus `repo list`, render through
`apps/conary/src/ui/repository.rs`. Pattern and unfiltered queries both read
packages from enabled repositories. Result-list fields retain version, release,
architecture (or `Unspecified`), and source identity; absent architecture does
not imply `noarch`. Empty results explicitly describe the cached metadata
searched, rather than claiming that a package is unavailable upstream.

Discovery distinguishes no configured repositories, all sources disabled,
enabled sources without published metadata, and metadata checks due under the
core sync policy. Guidance appears even alongside matching cached packages.
A successful recent check with no matches needs no automatic retry advice.
`repo list --all` retains disabled sources with `[off]` and separate checked
and published timestamps. These facts do not establish package compatibility,
source authentication, or transaction readiness.

Recovery commands retain the selected database with shell-safe quoting. Disabled
source recovery offers actual quoted repository names after an option terminator;
control-containing names or database paths get explicit instructions rather than
a runnable placeholder. Missing
publication recommends a forced sync so a recent check alone cannot suppress
the requested refresh. Queries never refresh metadata or change source state
implicitly. `cargo test -p conary --test cli_repository_discovery` proves local
repository enrollment, sync, search, disable/enable, stale and empty catalogs,
execution of the printed recovery command, and terminal/pipe/`NO_COLOR` frames.
The fixture uses an isolated database and local JSON metadata; it makes no
claim about supported-host setup or native-source authentication.

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

## Collection Update Summary

`apps/conary/src/commands/update/outcome.rs` retains no-change, planned, and
applied outcomes from update selection and committed package observations. Download
counters do not establish an applied package count.
`apps/conary/src/ui/update_summary.rs` renders collection results from these
observations. Successful return alone is never counted as an applied update.
Package counts and request counts have distinct labels; rows retain the selected
member's name, installed version, and architecture.

A collection preview previously ended with `Collection update complete` and
`Updated: 1 package(s)` immediately after saying no updates were applied. The
summary now reads:

```text
Collection update preview
  Collection: base
  Planned packages: 1
  Unchanged requests: 0
  Failed requests: 0
[pending]  demo 1.0-1 [x86_64]  1 planned
Dry run: no updates were applied.
```

Before the request-result summary, collection selection renders its retained
reasons through `apps/conary/src/ui/update_summary/selection.rs`: selected,
pinned, externally managed, no eligible update, and not installed. The update
command records these observations at the existing branch decisions; rendering
does not select a package or change ownership policy. Counts distinguish collection
members from installed package variants. Zero-valued reason fields are omitted.

A pinned-only collection previously said `All members ... are up to date` after
skipping its packages. It now reports:

```text
Collection update selection
  Collection: base
  Members: 1
  Selected packages: 0
  Pinned packages: 1
[skip]     demo 1.0-1 [x86_64]  pinned; not checked
No eligible updates selected.
```

Security-only selection says `No eligible security updates selected`; it does
not imply skipped packages were checked. An empty collection says `Collection
has no members`. Missing members are explicitly not installed by an update.
External-owner rows retain the recorded manager's update guidance and the
adoption-refresh note, including when other members have eligible updates.
Mixed collections retain all selection reasons alongside the planned changes.
Unavailable security metadata remains an error before the selection summary or
any update execution; it never becomes an empty-selection success claim.

Apply uses `Collection update results` and `Applied packages`; a request that
finds no eligible update on re-selection is shown as `[skip]` with `no changes`.
Failed requests retain failure status and never imply that prior successful
members were rolled back. The closing note directs the operator to inspect
package state before retrying when a request failed during apply. Planning,
source selection, lifecycle execution, and publication authority are unchanged.

`cargo test -p conary --test cli_update_summary` captures terminal, pipe, and
`NO_COLOR` previews, empty/pinned/uninstalled/externally managed/mixed selections, and
security-metadata refusals, comparing every database table before and after.
The UI proof distinguishes member counts from installed variant counts. Update
unit tests prove no-change/preview/apply outcomes against real selection and
execution; UI tests cover mixed results and partial-failure wording. Remaining first-use fields and generation/recovery surfaces remain #132.

## Install And Update Results

`apps/conary/src/commands/install/report.rs` carries planner and committed
transaction observations through native installs, CCS installs, dependency
batches, and updates. `apps/conary/src/ui/transaction_summary/install.rs` adapts
these observations to the same package table used for removal and rollback.
A preview uses `Planned package changes` with `Install`, `Update`, `Remove`, and
`Deconfigure` groups; committed results use `Applied package changes`. Update
rows retain before/after versions and CCS releases, plus architecture transitions
when those differ. Relation-driven removals retain their typed relation kind in
a `Reason` column. Update previews resolve and verify the exact selected artifacts
through `apps/conary/src/commands/update/package/preview.rs`, then call the install
planner with dry-run options. This includes relation removals, deconfigurations,
and dependencies; CCS releases come from verified artifacts even when repository
metadata leaves the release unspecified. Downloads and CAS objects used for the
preview live in a disposable directory. Unavailable or untrusted artifacts fail
the preview before a planned table or database mutation. CLI diagnostic tracing
uses color only on terminal stderr when `NO_COLOR` is absent. Apply reuses the
exact admitted artifacts from preview, retaining one full-artifact download.

`apps/conary/src/commands/install/preview.rs` owns a private database snapshot
that advances from prepared artifact identities, requirements, capabilities,
lifecycle contracts, declared payload paths, and typed relation effects. Native
install/batch lifecycle planning lives in
`apps/conary/src/commands/install/native_events/install.rs`; its preview path
view preserves later file-trigger planning without fabricating resolved owners.
Named payload ownership resolves during apply after pre-payload lifecycle
programs can create the declared accounts. Each later
update is planned against earlier successful effects, in the execution order
of deltas followed by full updates. Failed delta downloads, reconstruction, or
installation use the admitted full artifact immediately in that same position;
fallback does not reorder package effects. If an earlier update removes a later target,
that later row is an install. Dependency batches advance the same snapshot.
Lifecycle programs, selected-root mutation, and generation publication do not
run in this projection; the installed database and permanent CAS stay unchanged.
Native dependency acquisition reads prepared trust from the original runtime
keyring while planning against projected package state; it neither copies nor
relaxes that trust. Projected database effects follow the native graph payload
boundaries through `apps/conary/src/commands/install/preview/effects.rs`,
including typed repository enrollment transitions and last-owner dispositions.
A removed repository and its cached candidates disappear before later update
selection; retained and shared ownership follow the same enrollment authority
as apply. Debian payload completion, declared successful event state, and
trigger-state transitions use the native lifecycle state owner, retaining
config-files residual authority after removal and clearing it after disappearance.
Standalone native install previews also share this state between their
dependency stage and root planning. Native dependency preview and apply both
use the authenticated repository-batch preparation owner, including signed
CCS dependencies; the duplicate native-only preparation path is removed.
Atomic dependency previews run the existing incoming CCS hook preflight for
every prepared package with the caller's root context before adding planned rows
or advancing disposable state. Native and CCS dependency prompts carry the same
cancelled outcome through the enclosing update.

Previously, native/CCS install printed independent `Installed package` fields,
batches printed a separate success list, and update ended with artifact preparation counters.
A dry run could print its completion line before relation removals, while CCS
dependency selections were absent from that frame. Planner-backed dependency and
relation rows now precede the closing dry-run note. Declining the native
dependency prompt stops the enclosing install.

The update selection preview is:

```text
Planned package changes:
  Update (1):
    Package           Version         CCS release  Architecture
    a-summary-update  1.0.0 -> 2.0.0  - -> 1       x86_64
note: Dry run: no updates were applied.
```

A committed CCS upgrade renders:

```text
Applied package changes:
  Updated (1):
    Package           Version         CCS release  Architecture
    summary-incoming  1.0.0 -> 2.0.0  1 -> 1       x86_64
  Installed file records: 4
  Changeset: 1
  Generation: 0 published
note: Inspect history: conary system history --db-path='<fixture>/conary.db'
note: Request rollback of latest changeset: conary system state rollback 1 --yes --db-path='<fixture>/conary.db'
```

Installed file records count the selected payload descriptors, including
directories; they do not measure physical writes or disk savings. Multiple
committed transactions share one result table and list their changeset IDs;
the closing generation reflects the last transaction's returned publication
outcome. The report survives a later command error, so partially completed
updates retain their committed rows without claiming the failed package changed.
Preview rows describe planned successful effects. Apply preflights each transaction
against its then-current selected root before that transaction runs lifecycle or
payload mutation; an update selection spans separate committed transactions.
A later runtime preflight failure retains earlier committed rows and leaves the
failing package unchanged. Dependency cancellation stops the enclosing update,
keeps every remaining target unchanged, and records no applied delta for the
declined package. Acquisition counts still include all prepared full artifacts.
A publication delegated to an enclosing selected-root operation is explicitly
labeled as such rather than assigned a generation. Only a successful top-level
install/update command offers the latest changeset's rollback request. Nested
install and collection-member calls leave that guidance to their enclosing
operation. A multi-transaction rollback request reverses only the named changeset.

Pending publication retains the existing warning and same-database retry.
Native install snapshot follow-ups also retain that database. Empty update
selections say `No eligible updates selected` (or the security-specific form)
rather than declaring skipped packages current. Existing collection selection
reasons and security-metadata failure semantics remain intact.

`cargo test -p conary --features test-hooks --lib commands::install::report`
captures native/CCS install, upgrade, preview, pending publication, duplicate
refusal, batch results, and dependency cancellation in terminal, pipe, and
`NO_COLOR` modes. `cargo test -p conary --features test-hooks --lib summary_capture`
proves update preview, apply, relation removal and dependent deconfiguration,
pending publication, ordered co-selected replacement, mixed lifecycle failure,
and pinned no-op output. Preview/refusal/cancellation captures compare every
persisted database table. The verified CCS dependency fixture additionally
proves that its batch preview includes the same two package identities without
changing database state. Cross-source lifecycle fixtures assert exact grouped
preview rows before their existing payload, native lifecycle, and rollback proof.

Broader source-format, size, disk-delta, and first-use fields remain under #132
and #644; the transaction table does not infer values the operation did not return.

## Removal And Changeset Rollback Results

`apps/conary/src/ui/transaction_summary.rs` groups committed removals and
restores into one table. Each row preserves package name, version, separate CCS
release, and architecture; absent optional identity fields use `-`. Displayed
identity values escape control characters. Removal statistics describe selected-root
file and directory changes, including Debian’s separate conffile purge stage, while rollback's restored file count describes database
records, not physical writes or recovered disk space.

The old removal frame began `Removed package: ...`; rollback scattered removed
and restored identities around `Rollback complete`, even when publication remained
pending. The applied frame now distinguishes the reversed forward mutation from
the compensating changeset and reports only the returned publication outcome:

```text
Applied package changes:
  Restored (1):
    Package          Version  CCS release  Architecture
    summary-fixture  2.0.0    7            x86_64
  Reversed changeset: 1
  Restored file records: 0
  Changeset: 2
  Generation: 1 published
note: Inspect history: conary system history --db-path='<fixture>/conary.db'
```

A pending outcome instead says `Generation: publication pending`, followed by the
single existing warning with its cause and publication retry. Persisted deferred
follow-ups use the same scoped retry renderer. History regenerates publication
guidance for the database it opened, ignoring obsolete stored retry text;
`system generation pending` uses that same context. Retry and history
commands retain the selected database, with shell quoting for ordinary paths;
control-containing paths require the original path in an explicit placeholder.
Top-level CLI removal also gives the changeset-rollback request, after publication
when pending. Nested removals used by autoremove, model apply, and automation
retain their result and history link but leave final rollback guidance to the
enclosing operation; later mutations can make an earlier removal ineligible.
The rollback command retains its eligibility checks; a compensating rollback row
is never offered as another forward mutation to reverse. A published generation
is not a claim that the running system has activated it.

`cargo test -p conary --lib ui::transaction_summary --features test-hooks` captures
real removal and rollback command execution in terminals, pipes, and `NO_COLOR`,
including forced publication failure, precommit refusals, and a two-package
autoremove that must not advertise stale per-package rollback commands. Pending captures also
verify persisted retry guidance and read-only history/pending output for the same
database. It checks resulting
installed state, rollback lineage, publication debt, exact identity rows, and the
absence of applied summaries on refused operations. Pure rendering tests cover
mixed remove/restore groups, control characters, missing generation facts, and
shell argument preservation. Removal preview remains #642; other generation command frames and broader first-use
fields remain #132 and #644.

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

Update artifact results count all selected full artifacts admitted for preview,
including targets whose later apply fails. They do not claim delta bandwidth
savings after those full artifacts were acquired. Persisted delta success rate
uses successful and failed delta attempts; full-artifact preparation is a
separate count. Committed package rows remain the applied-result authority.
