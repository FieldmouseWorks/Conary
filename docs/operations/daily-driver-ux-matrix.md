---
last_updated: 2026-09-14
revision: 54
summary: Daily-driver CLI installed-list records, database preflight, repository readiness, typed details, and grouped results
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
| `system init` | Initializes the selected database and configures built-in source feeds and typed host interfaces | Database initialization failure or typed obsolete-schema refusal | Sync the same selected database; rebuild disposable retired state only with explicit discard and apply flags | `cargo test -p conary --test cli_initialization` |
| `repo sync [name]` | Reports each attempted source and its synchronized package-record count; preserves successful sources in a partial failure | Retains typed causes per failed source; unknown source names identify the selected database | Resolve the reported causes, then retry only failed sources against the same database | `cargo test -p conary --test cli_repository_sync` |
| `repo add`, `enable`, `disable`, `remove` | Reports the completed operation, selected database, and escaped source identity through the shared UI | Retains requested operation, source, database, and original error causes; failed enrollment preserves existing state | Inspect configured repositories in the same database for missing or conflicting sources | `cargo test -p conary --test cli_repository_enrollment` |
| `repo reset-trust` | Reports static trust reset and the disabled source | Existing static-only and mutation checks remain command-owned | Re-establish trust for the same source and database using root fingerprints verified out of band | `cargo test -p conary --lib commands::repo_static`; `cargo test -p conary --test cli_repository_enrollment` |
| `install <pkg>` | Conary-owned package install or dry-run plan | Adopted package already belongs to native authority | `conary system adopt --refresh` before retry; `conary install <pkg> --ownership takeover --yes` for explicit package takeover; `conary system takeover --yes` for generation-level takeover | `cargo test -p conary --test cli_daily_ux adopted_install_refusal_routes_to_refresh_and_takeover` |
| `install <pkg> --dry-run` | Reports a would-be dependency-to-explicit promotion without changing installed state, even with `--yes` | Ambiguous installed variants require exact selection | Use `--version` and `--arch` to select the intended installed variant | `cargo test -p conary --lib commands::install::command::tests` |
| `remove <pkg>` | Conary-owned package removal; Debian residual conffiles are preserved | Adopted package removal without `--purge` | Use `--purge` to delete residual config state or externally owned adopted files; use `conary system unadopt <pkg> --yes` to stop adopted tracking without deleting files | `cargo test -p conary --test cli_daily_ux adopted_remove_refusal_routes_to_unadopt_or_purge` |
| `update [pkg]` | Conary-owned update or security update from trusted advisory metadata | Adopted package update remains externally owned, unsupported advisory source fails before mutation | Refresh adoption after external changes; use `--ownership takeover` only for explicit Conary takeover | `cargo test -p conary --test cli_daily_ux adopted_update_routes_to_native_pm_and_refresh` |
| `search <pattern>` | Repository search results from synced metadata | Empty or stale repository metadata | Run `conary repo sync` before assuming a package is unavailable | Existing query/search tests plus `cargo run -p conary -- search --help` |
| `list [pkg]` | Installed package identity, files, path owner, pinned state | Ambiguous installed package variants | Use `--version`, `--release`, and `--arch` to select a specific installed variant | Existing `cargo test -p conary --test query list_info_refuses_ambiguous_variants_until_selector_is_given` |
| `autoremove` | Removes Conary-owned orphaned dependency packages | Adopted orphaned packages remain native-PM owned | Native package-manager authority is preserved for adopted orphans | Existing `cargo test -p conary --test native_pm_daily_driver autoremove_dry_run_lists_conary_owned_orphans_and_skips_adopted` |
| `pin <pkg>` | Pins a selected installed variant | Ambiguous installed variants | Use `--version`, `--release`, and `--arch` to pin the intended variant | Existing `cargo test -p conary --test query pin_and_unpin_use_same_variant_selector` |
| `unpin <pkg>` | Releases a selected installed variant | Ambiguous installed variants | Use `--version`, `--release`, and `--arch` to unpin the intended variant | Existing `cargo test -p conary --test query pin_and_unpin_use_same_variant_selector` |
| `system history` | Recorded changeset fields, rollback relationships, continued lifecycle failures, and deferred recovery guidance | Obsolete changeset metadata keeps its existing refusal | Publication retries use the selected database; history does not decide rollback eligibility | `cargo test -p conary --test cli_history` |

## Ordinary Installed Lists

Ordinary `list`, name-filtered lists, and successful explicit-selector lists
render through `ui/installed_list.rs`. Rows retain the exact version, separate
CCS release, architecture, and record type. Each stays on one visible line;
recorded controls are escaped. Absent release and architecture are `Unspecified`,
which does not establish identity equivalence or imply `noarch`.

The query still returns package, component, and collection records in its
existing order. The former closing `Total: N package(s)` counted every record as
a package. The replacement separates the total from the typed counts:

```text
Installed records:
  Database: /selected/conary.db
[info]     nginx  1.27.2  Type: package  Release: 1  Architecture: x86_64
[info]     nginx:runtime  1.27.2  Type: component  Release: Unspecified  Architecture: Unspecified
  Records: 2
  Packages: 1
  Components: 1
  Collections: 0
```

A requested name appears in a `Name` field even when no record matches. Empty
lists say `No installed records.`; empty name-filtered results say
`No matching installed records.` Both retain the selected database and zero
counts. A displayed requested name is not an installed result row. The two
post-unadoption `list curl` manifest assertions therefore check the empty
result and record count.

`cargo test -p conary --test cli_installed_list` proves mixed record types,
exact release variants and selectors, absent architecture, escaped names and
paths, empty results, and unchanged full database snapshots in terminal, pipe,
and `NO_COLOR` modes. Fixtures use the registered core connection and `Trove`
model. Query selection and mutation authority stay with their existing owners;
detail, file, path-owner, and pinned modes keep their own presentation paths.

## Installed Variant Selection

Installed package selection combines exact version, separate signed CCS release,
and architecture. `--release 3` selects the exact recorded release string;
`--release none` selects records without a CCS envelope release. Omitting
`--release` leaves releases unconstrained. The existing core release grammar
validates numeric inputs; selection does not split a source version or normalize
release strings.

The selector is available on `list`, `pin`, `unpin`, `remove`, single-package
`update`, `query scripts`, `query whatbreaks`, and `system adopt --convert`.
For example, `conary list demo --info --version 2.0-1 --arch x86_64 --release 3`
can inspect one of two installed CCS releases that share the source version
and architecture. Ambiguity output includes each separate release and names
these actual selection flags. `list` and installed script inspection retain
the selected release in human output; scripts JSON keeps its existing schema.

Selectors cannot be combined with list path/pinned queries, collection updates,
unscoped updates, or artifact script inspection. Adopt conversion selectors
require one package. Invalid release syntax fails in argument parsing, before
database access. Selection does not change candidate resolution, dependencies,
package ownership, lifecycle admission, or live-mutation confirmation.
`cargo test -p conary --test installed_release_selector` and the shared selector
unit tests prove numbered/absent releases, ambiguity, source version and
architecture filters, and selected pin state on disposable databases.
The parser in `apps/conary/src/commands/package_target/release.rs` is shared by
runtime argument parsing and generated manuals.

Update carries the selected installed snapshot through preview, full-artifact,
and retained-artifact installation. Root preparation revalidates its record ID, identity,
source observations, and pin state; batch execution repeats that validation
under the runtime mutation lock. Unexpectedly missing or changed targets and a conflicting incoming identity
refuse before mutation. Every target is validated in the initial private
projection. If an earlier admitted relation effect then removes a later target,
that later installation carries an explicit planned-absence guard: its original
record must remain absent, and it never replaces a surviving name-match.
Dependencies keep their own targets. The feature-enabled update summary
captures prove this relation sequence in preview and apply.
`cargo test -p conary --features test-hooks --test installed_release_update`
proves that updating the second same-version/architecture release preserves
its sibling and publishes the incoming payload ownership and CAS bytes. This
fixture uses the fenced test-only mount boundary; real-mount proof remains a
separate lifecycle gate.

`pin` and `unpin` acquire the same runtime mutation lock as package operations
before resolving their installed selector, and hold it through the pin-state
write. A command waiting behind another package mutation therefore selects the
current record after that operation completes. A removed selected release is
refused; another same-name release is not silently changed. Read-only pinned
listing does not acquire the mutation lock.
Removal rechecks the current pin state after acquiring that same lock, before
preparing payload ownership, lifecycle, or the selected root. A pin completed
while removal waited therefore prevents deletion.
The removal graph regressions live in
`apps/conary/src/commands/remove/native_graph/tests.rs`.

## Repository Discovery

`search` and `query repquery` result lists, plus `repo list`, render through
`apps/conary/src/ui/repository.rs`. Pattern and unfiltered queries both read
packages from enabled repositories. Result-list fields retain version, release,
architecture (or `Unspecified`), and source identity; absent architecture does
not imply `noarch`. Empty results explicitly describe the cached metadata
searched, rather than claiming that a package is unavailable upstream.

`query repquery --info` renders a single cached candidate through the same UI
owner. Candidate and installed observations remain separate: every same-name
installed package record retains its trove ID, version, separate CCS release,
architecture, version scheme, source profile, and install source. Repository
provenance IDs are shown when recorded. Components, collections, and retained
configuration state do not count as installed packages. Missing release,
architecture, or source-profile metadata is `Unspecified`; it does not establish
identity equivalence or compatibility. Multiple candidate matches retain the
existing result-list behavior.

The former detail frame omitted the separate release and reported only the
first name-matched trove as `Status: Installed (<version>)`. The replacement
makes both observations inspectable, for example:

```text
Repository package:
[info]     demo
  Version: 2.0-1
  Release: 3
  Architecture: x86_64
  Version scheme: rpm
  Source profile: fedora-44
  Repository: fedora
...
Installed packages with this name:
[info]     demo
  Trove ID: 7
  Version: 2.0-1
  Release: 2
  Architecture: aarch64
  Version scheme: rpm
  Source profile: fedora-44
  Install source: repository
  Installed packages: 1
```

Detail reads resolve the source, installed records, and requirements before
printing. Database errors remain errors; package-provided controls are escaped
in all displayed metadata and requirement text. Empty requirements mean none
are recorded in cached metadata. `cargo test -p conary --test
cli_repository_details` proves release and architecture variants, other-version
and non-package matches, absent metadata, requirement text, four terminal/pipe/
`NO_COLOR` modes, and unchanged database contents.

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
Container and VM onboarding assertions inspect read-only repository state, so
these human output changes do not become a second source-state authority.

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

## Database Initialization And Recovery

`system init` renders the selected database as an escaped `Database` field.
Its follow-up metadata command retains `--db-path` for custom databases,
including paths with spaces or single quotes. The same configuration summary
follows an explicit `system rebuild-db`. For example:

```text
Initialized Conary database
  Database: <fixture>/state/conary.db
```

After source enrollment and typed host-interface discovery, the action is:

```text
note: Download metadata from every enabled Remi feed:
note: Run: conary repo sync --db-path='<fixture>/state/conary.db'
```

Initialization keeps the original core error and typed database/runtime-root
context through both the command and its earlier try-session database preflight.
The existing initialization target validation runs before preflight opens the
database. Non-canonical system aliases retain their canonical-path refusal,
even when the underlying system database has a retired schema; they never
receive a rebuild command that the same validation would reject.
The UI selects rebuild guidance from `SchemaRebuildRequired` and retains its
observed schema, supported epoch, and revision as separate facts:

```text
error: Database initialization requires a schema rebuild.
  Database: <fixture>/state/conary.db
  Database parent: <fixture>/state
  Runtime root: <fixture>/state
  Observed schema: retired migration-chain schema version 66
  Supported epoch: conary-current-v1
  Supported revision: <current revision>
note: Rebuilding replaces active Conary state after preserving a snapshot. Use it only when this state is disposable.
note: Run: conary system rebuild-db --discard-state --yes --db-path='<fixture>/state/conary.db'
```

Other database failures include the same location fields and the core `Cause`.
Bare relative database filenames identify the parent and runtime root as `.`.
Displayed paths escape terminal controls; command hints for such paths use
`--db-path <PATH> (use the same database path)` rather than silently changing
the argument. Rebuild success names the selected database and escaped retired
snapshot path. Rendering does not skip try-session, privilege, or discard/apply
gates and does not change schema or rebuild authority.

`cargo test -p conary --test cli_initialization` captures initialization success,
unusable-parent and retired-schema refusals in real TTY, pipe, and both
`NO_COLOR` modes. It checks quoted/control-character paths, retained refusal
state, and the printed rebuild action on disposable state with its retired
snapshot preserved. Command unit tests retain downcastable core errors through
additional context. The rest of the first-use walkthrough remains under #132
and #644.

The system-alias regression creates a retired system database on private
`/var/lib` tmpfs in a separate mount namespace, then proves all four refusal
frames and byte-preserved database state. It requires usable user/mount
namespaces or an isolated privileged invocation of that exact test.

## Common Database Preflight

Common try-session preflight retains the selected database when opening it
fails before command dispatch. Repository, package, query, and system commands
therefore name the same `Database` field. Corrupt or inaccessible database errors
retain their original causes as escaped fields, without guessing a repair from
their text. An obsolete schema previously appeared as one long error sentence
with no database path. It now has a structured frame:

```text
error: Database requires a schema rebuild.
  Database: /selected/conary.db
  Observed schema: retired migration-chain schema version 66
  Supported epoch: conary-current-v1
  Supported revision: 56
note: Preserve existing Conary runtime state unless you have confirmed it is disposable.
note: Run: conary system rebuild-db --help
note: Any rebuild must select this same database with --db-path and satisfy the command's target and privilege checks.
```

The suggested command only displays help. Common preflight has not validated
rebuild privileges or canonical database aliases, so it does not supply an apply
command. Initialization retains its own earlier target checks and recovery
adapter. Missing-database pass-through, try-session checks, command risk checks,
and typed schema classification keep their existing order and behavior.

`cargo test -p conary --test cli_diagnostics database_preflight` captures the
repository, install-preview, history, and list entrypoints in terminal, pipe,
and `NO_COLOR` modes. It proves selected-path and schema-control escaping,
unchanged corrupt bytes, retained obsolete schema and rows, and executable
read-only help. Core database open still configures SQLite WAL before rejecting
an obsolete schema; these captures do not claim that the database file's bytes
remain identical in that case. The dispatch unit test also retains the original
typed SQLite error through additional caller context.

## Repository Enrollment And State Changes

Repository enrollment, enable, disable, remove, and static trust reset results
use `ui/repository/`. The requested operation, exact source name, and selected
database remain typed context around the original error. Failed database
inserts no longer flatten their causes into strings. Static enrollment uses
its TUF fields; JSON/Remi and native enrollment retain their respective trust,
source, strategy, and advisory fields. Repository trust descriptions now live
under `ui/repository/trust_display.rs`.

Static duplicate enrollment and missing-source trust reset use the same typed
conflict/not-found cases and database-scoped inspection guidance as the other
repository operations. Sync control-value fixtures now enroll their source
through the actual CLI instead of renaming it after enrollment.
Missing-database failures retain the shared initialization guidance and add the
affected repository identity; the selected database is named for both default
and custom paths.

```text
Repository added:
  Database: <fixture>/conary.db
[ok]       source
  Metadata URL: https://example.invalid/metadata
  Enabled: true
  Priority: 50
  Repository trust: typed JSON/Remi authority
  Security advisories: unknown
```

Successful results, including trust-reset recovery notes, stay on stdout;
failures stay on stderr. A conflicting enrollment keeps the existing source
intact and reports, for example:

```text
error: Repository enrollment failed.
  Database: <fixture>/conary.db
  Repository: source
  Cause: persist repository enrollment
  Cause: Conflict: repository 'source' already exists
note: Inspect the configured repositories:
note: Run: conary repo list --all --db-path='<fixture>/conary.db'
```

All displayed source names, metadata, descriptions, and paths escape terminal
control characters. Inspection commands quote the same selected database;
control-containing paths get instructions to reuse the exact path. Static
trust prompts keep the verified root-key set distinct from the displayed
description and retain the existing stale-root warning and explicit acceptance
question. Reset-trust recovery names the same source and database and requires
out-of-band root fingerprints before explicit re-enrollment; rendering does not
approve trust or enable the repository.

`cli_repository_enrollment` captures actual add/enable/disable/remove journeys,
duplicate and missing-source failures, quoted and option-like names, control
paths, static fingerprint enrollment, and trust reset in TTY/pipe with and
without `NO_COLOR`. It executes the printed inspection command and checks
persisted source/trust state. Command tests retain downcastable core errors,
prove insertion rollback, and preserve existing authority/refusal proofs.

## Repository Synchronization

`commands/repo/sync.rs` owns source selection and the sequential sync attempts.
`ui/repository/sync.rs` owns the shared transient progress adapter and durable
result frame; `ui/diagnostics/repository.rs` renders retained typed failures.
The command keeps every original core error and completed result, including
when a later attempt cannot open the database. Core repository code remains
the authority for refresh age, trust, metadata validation, and publication.

Durable results stay on stdout. A partial failure still reports successful
sources and returns a nonzero exit status; its cause and retry guidance stay
on stderr:

```text
Repository synchronization:
  Database: <fixture>/conary.db
[fail]     unavailable
[ok]       available
  Package records synchronized: 1
error: Repository metadata synchronization failed.
  Database: <fixture>/conary.db
  Repository: unavailable
  HTTP status: 404
  Metadata URL: http://127.0.0.1:<port>/unavailable/metadata.json
note: After resolving the reported causes, retry the failed repositories:
note: Run: conary repo sync --force --db-path='<fixture>/conary.db' -- 'unavailable'
```

Retry commands preserve the selected database and source, including quoted or
option-like names. Control-containing values are escaped for display and get
instructions to reuse the same values instead of an altered executable path.
`--force` bypasses the refresh-age check; all existing trust and publication
checks still apply. The UI does not claim that a retry repairs the cause.

If core policy says no selected source is due, the frame says
`No repository metadata checks are due.` With no enabled sources it says
`No enabled repositories to sync.` and offers the existing enrollment or
enable guidance. An explicitly named disabled source remains selectable.
An unknown name reports the database and source and offers `repo list --all`
against that database.

`cargo test -p conary --test cli_repository_sync` exercises successful and mixed
refreshes, multiple failures, preserved failed-source cache, freshness and
enabled-source selection, unknown names, escaped controls, and actual execution
of printed recovery commands. Each journey uses a disposable database and local
HTTP metadata, captured in a real terminal and pipe with and without `NO_COLOR`.
Progress uses the existing shared terminal coordinator and ends before durable
results print.

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

## Native Runtime Refusals

`commands/install/native_events/refusal.rs` retains the event owner, version,
architecture, source format, stage, program, and actual execution root at the
transaction-wide preflight boundary. Core returns typed missing-interpreter,
invalid-root, and timeout-range failures; other causes retain their complete
chain without text-derived remediation. `commands/package_failure.rs` projects
these observations into `conary-agent-contract`'s closed, versioned
`conary.package.failure.v1` report. `ui/diagnostics/package.rs` renders the same
report; daemon jobs retain it in `error.extensions.package_failure`.

Requested root/database scope stays separate from the materialized execution
root, which can be temporary. For example, a refused native install reports:

```text
error: Native transaction preflight refused.
  Package: summary-incoming
  Version: 2.0.0-1
  Architecture: x86_64
  Source format: rpm
  Stage: package-pre-install
  Event: Normal
  Root: <requested root>
  Database: <selected database>
  Execution root: <materialized transaction root>
  Entry: rpm:%pre
  Cause: Required interpreter is missing
  Interpreter: /missing/summary-interpreter
  Path state: Current selected root
note: Provide the required interpreter in the selected root at this lifecycle stage before retrying.
```

Update aggregation retains every underlying error and the changeset IDs returned
by earlier commits. Typed operation scope preserves the full ordinary error
summary, including package, stage, and cause, for library consumers; aggregation
also retains per-package detail in its ordinary display. It renders each failure once at the closing diagnostic,
after the applied table, with an explicit note that earlier commits remain
applied. Requested package and event owner are separate when dependencies,
relations, triggers, or recovery make them differ. Unknown errors retain every
cause; their text cannot establish a typed refusal or generate a retry action.

Native, CCS, and dependency-batch installs keep baseline selected-root preparation
inside a savepoint until preflight succeeds. A refused transaction therefore
does not leave baseline snapshot rows or advance the database mutation epoch.
Autoremove retains the native cause through its preflight wrapper and rolls back
preparation observations after each read-only check, on success as well as refusal.
The runtime mutation lock still precedes preparation; the savepoint closes before
lifecycle execution and does not combine independently committed updates.
Artifact acquisition and disposable root preparation are not package mutation.

Terminal/pipe/`NO_COLOR` captures cover standalone and batch native refusals, a
first CCS update refusal, and a later CCS update refusal with earlier applied
rows preserved. The standalone and batch fixtures compare every database table.
Core tests distinguish current-root and projected interpreter absence without
staging files. Strict report tests reject unknown schemas, variants, and fields.
Broader native contract classification and full install/update JSON result
surfaces remain under #644; this report is failure evidence, not apply authority.

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
update is planned against earlier successful effects. The planner retains its
established advertised-delta priority, then full-only targets, preserving order
within each group. Apply consumes that same ordered vector of admitted full
artifacts directly; it does not download or reconstruct a delta after full
admission, and artifact reuse cannot reorder package effects. If an earlier update removes a later target,
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
    Package           Version         CCS release  Architecture  Source format
    a-summary-update  1.0.0 -> 2.0.0  - -> 1       x86_64        rpm
note: Dry run: no updates were applied.
```

A committed CCS upgrade renders:

```text
Applied package changes:
  Updated (1):
    Package           Version         CCS release  Architecture  Source format
    summary-incoming  1.0.0 -> 2.0.0  1 -> 1       x86_64        ccs
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
keeps every remaining target unchanged, and records no applied package effect
for the declined package. Acquisition counts still include all prepared full artifacts.
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

The `Source format` column reports the prepared package's source kind: `rpm`,
`deb`, `arch`, `eopkg`, or `ccs`. A converted CCS retains its native lifecycle
source kind. A CCS package without a native lifecycle bundle reports `ccs`,
including when its package version uses a native grammar. Version grammar,
file extensions, package names, repository names, and distro names do not supply
this observation. Update rows describe the incoming source kind.

`ObservedPackage` in `apps/conary/src/commands/install/report.rs` carries that
optional fact separately from `PackageIdentity`; applied-target matching and
deduplication retain their existing exact identity rules. Stored native package
identities supply an observed format when available. Other stored rows display
`-`; the presentation path does not add a source lookup or invent a default.
The capture proof includes actual RPM-to-CCS conversion and native-free CCS with
RPM version grammar. Reporting and renderer tests cover known and absent
stored-native observations.

Per-package artifact size, disk-delta, and other first-use fields remain under
#132 and #644; the table does not infer values the operation did not return.

## Removal And Changeset Rollback Results

`apps/conary/src/ui/transaction_summary.rs` groups committed removals and
restores into one table. Each row preserves package name, version, separate CCS
release, architecture, and any exact retained native source format; absent optional
identity fields and source observations use `-`. Displayed
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
    Package          Version  CCS release  Architecture  Source format
    summary-fixture  2.0.0    7            x86_64        -
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

## File Ownership Query Failures

`list --path <PATH>` preserves errors from the file and owner readers. An
exact-path owner-read failure does not fall through to pattern search. A
pattern-query owner-read failure is not silently omitted from a successful
answer: the command exits nonzero with the original error. Valid absence,
exact matches, grouped pattern matches, and exact-path `--info` keep their
existing selection behavior. These queries do not change stored state.

`cargo test -p conary --test cli_path_query` establishes a valid file record
and failed typed owner read before invoking the actual CLI. Invalid owner type
and version grammar exercise both paths; separate valid-query controls and
database snapshots prove existing read-only behavior.

## Installed Package Details

`apps/conary/src/commands/query/package.rs::show_package_info` resolves the
selected trove, prepared authority label, repository name, payload ownership,
typed dependency/provide entries, and components before emitting any detail
output. A failed observation returns its existing error without a partial
package frame. `apps/conary/src/ui/installed_info.rs` owns the resulting
`Installed package:` frame and performs no lookups or classification.

`Name`, `Version`, and `CCS release` stay separate; an absent CCS release is
`-`. `Install source` identifies the recorded installation origin, while
`Version scheme` reports the recorded grammar; neither is inferred to be a
source package format. Optional architecture, source profile, repository,
description, installation timestamp, and selection reason appear only when
observed. Type and install reason use their typed labels. `File records` counts
the selected payload ownership entries; `Payload size` sums their recorded
content bytes, without claiming a compressed size or physical disk usage.
Typed dependencies, provides, and component observations retain their input
order and multiplicity. Component installation state uses explicit yes/no
fields. Recorded controls are escaped inside fields and relation lines.

The `Provides` section is owned by `apps/conary/src/ui/installed_provides.rs`.
Each record reports its exact capability name and typed kind, separate provider
version and relation (`-` when absent), version grammar, architecture qualifier,
and provenance. Capability-specific version, grammar, and architecture labels
keep these observations separate from the package fields. An exact qualifier keeps its literal architecture token: an
exact `native` token is distinct from implicit and wildcard qualifiers. Source
provenance reports its recorded source format and, for a declaration, its exact
source record index. Rendering does not infer a source format from the package
or capability version grammar, renumber source records, or turn promised paths
into shipped-file observations. The installed-info command orders its loaded
records by ascending persisted ID before rendering; shared provides queries
retain their existing read behavior. The renderer preserves that insertion
order and multiplicity without sorting by capability or source record index.

`cargo test -p conary --lib ui::installed_provides` proves field preservation,
controls, missing observations, qualifier distinctions, and provenance.
`cargo test -p conary --test cli_installed_info` captures literal versioned,
unversioned, qualified, declared, derived-file, and promised-path records in
TTY, pipe, and `NO_COLOR` modes while proving the database remains unchanged.
Its complete-section assertion checks the relative order of all six records
against insertion order, including capability names and source record indexes
that would sort differently.

This hard-cuts the former hand-padded detail labels and debug enum output.
Installed-list and file-list layouts, installed variant selection, repository
fallback, and persisted authority are unchanged.

The frames below are captured from actual `list --info` executables against the
same disposable database fixture, with a fixed recorded installation timestamp.
They compare the preceding installed-info layout with the complete recorded
capability fields.

Before:

```text
Installed package:
  Name: nginx
  Version: 1.24.0
  CCS release: 7
  Type: package
  Authority: conary-owned
  Install source: repository
  Source profile: fedora-44
  Version scheme: rpm
  Repository: recorded-repository
  Architecture: x86_64
  Description: High performance web server
  Installed: 2026-01-01 00:00:00
  Selection reason: explicitly selected fixture
  Install reason: explicit
  Pinned: yes
  File records: 6
  Payload size: 1026048 bytes

Dependencies (1):
  openssl>= 3.0.0

Provides (2):
  nginx
  webserver

Components (2):
  Component: :config
  Installed: no
  Component: :runtime
  Installed: yes
```

After:

```text
Installed package:
  Name: nginx
  Version: 1.24.0
  CCS release: 7
  Type: package
  Authority: conary-owned
  Install source: repository
  Source profile: fedora-44
  Version scheme: rpm
  Repository: recorded-repository
  Architecture: x86_64
  Description: High performance web server
  Installed: 2026-01-01 00:00:00
  Selection reason: explicitly selected fixture
  Install reason: explicit
  Pinned: yes
  File records: 6
  Payload size: 1026048 bytes

Dependencies (1):
  openssl>= 3.0.0

Provides (2):
  Capability: nginx
  Kind: package
  Capability version: 1.24.0
  Capability version relation: =
  Capability version scheme: conary
  Architecture qualifier: implicit
  Provenance: exact-identity

  Capability: webserver
  Kind: package
  Capability version: -
  Capability version relation: -
  Capability version scheme: conary
  Architecture qualifier: implicit
  Provenance: exact-identity

Components (2):
  Component: :config
  Installed: no
  Component: :runtime
  Installed: yes
```

`cargo test -p conary --test cli_installed_info` checks actual TTY/pipe and
color/`NO_COLOR` output, optional observations, escaped controls, unchanged
database snapshots, and a late component-read failure without partial output.
Installed-release selector tests continue to prove exact selection and
ambiguity refusal.

## Changeset History

`apps/conary/src/commands/query/history.rs` reads recorded changesets,
recoverable publication observations, and ordered lifecycle events. It parses
each changeset's metadata once and classifies deferred work through the existing
command-owned contract. Generation-publication retry guidance is regenerated
with the selected `--db-path`; other follow-ups retain their recorded guidance.
`apps/conary/src/ui/history.rs` owns the shared headings, fields, and warning
rows. Rendering performs no lookups or recovery decisions.

Each record has a `Changeset N:` heading, `Description`, typed `Kind`, and typed
`Status`. `Created`, `Applied`, and `Rolled back` appear only when their recorded
timestamps exist. `Reverses changeset` and `Reversed by changeset` report exact
stored relationships. A matching recoverable publication adds its recorded
status; absence supplies no publication-success claim. Deferred records retain
their order and multiplicity, with one retry note per supplied command.
Continued lifecycle failures use the shared `[warn]` row and retain package,
version, entry, failure kind, phase, requested/effective sandbox, and reason.
Control characters in recorded text are escaped inside their fields.

The footer reports `Total changesets: N`. Empty history is exactly
`No changeset history.`. This is a hard cut of the human display: old `[N]`,
`[deferred]`, and publication suffix markers are removed. Repository rollback
fixtures extract the numeric `Changeset N:` heading. No persisted schema or
rollback eligibility contract changes.

Before and after frames below come from the existing `pending_remove`
command capture, which invokes `cmd_history` after a forced deferred publication
on a disposable database and compares database rows before/after inspection.
Fixture roots and recorded timestamps are redacted. The publication row's
`failed` status and the follow-up record's `pending` status remain separate
recorded facts.

Before:

```text
Changeset history:
  [1] <recorded timestamp> - Remove summary-fixture-2.0.0 (Applied) [deferred] [publication-failed]
      deferred generation_publication pending: generation publication is pending Retry: conary system generation publish --yes --db-path='<fixture>/conary.db'

Total: 1 changeset(s)
```

After:

```text
Changeset history:
Changeset 1:
  Description: Remove summary-fixture-2.0.0
  Kind: mutation
  Status: applied
  Created: <recorded timestamp>
  Applied: <recorded timestamp>
  Generation publication: failed
Deferred work (1):
  Kind: generation_publication
  Status: pending
  Reason: generation publication is pending
note: Retry: conary system generation publish --yes --db-path='<fixture>/conary.db'
  Total changesets: 1
```

`cargo test -p conary --features test-hooks --lib ui::` covers the recovery
frame through TTY, pipe, and `NO_COLOR` modes.
`cargo test -p conary --test cli_history` runs the actual executable in all four
TTY/pipe and color/`NO_COLOR` combinations. It covers empty history, pending and
rolled-back records, exact rollback relationships, lifecycle failure fields,
scoped versus recorded retry guidance, escaped controls, unchanged database
rows, and the existing obsolete-metadata refusal.

## Package Build Results

`apps/conary/src/ui/ccs_build.rs` renders the core-owned `BuildResult` and
`LossReport`. The build summary uses shared headings and fields, separates the
exact `Version` from `CCS release`, and reports the manifest architecture only
when present. `File records` includes every recorded payload node; `Payload
sources` counts regular-file content descriptors. `Payload size` and component
sizes describe the builder's payload records, not a compressed archive or an
installed disk delta. Native export's returned archive size remains a separate
field beside its written path.

The former summary rendered `Package: summary-build v2.0.0`, omitted its CCS
release field, and labeled payload bytes `Total size`. The same fixture now
renders:

```text
Package build summary:
  Package: summary-build
  Version: 2.0.0
  CCS release: 7
  Architecture: noarch
  File records: 2
  Payload size: 8 bytes
  Payload sources: 1 regular file

Chunking:
  Chunked files: 0
  Whole files: 1
  Total chunks: 0
  Unique chunks: 0

Components:
  Component  File records  Payload size
  runtime    2             8 bytes
```

Its preview is:

```text
Planned package build:
  Package: summary-build
  Version: 2.0.0
  CCS release: 7

Planned artifacts:
  ccs: <fixture>/preview/summary-build-2.0.0-7.ccs
note: Dry run: no package artifacts were written.
```

Components appear in sorted name order; chunk counts and intra-package savings
come from the builder's optional chunking statistics. Conversion notes retain
their typed unsupported-feature, hook, and dependency categories. Dynamic
identity, component, note, and path values escape terminal controls.

A dry run shows `Planned package build` and `Planned artifacts`, followed by
`Dry run: no package artifacts were written.` It does not print a completed
build summary, claim payload measurements, create its output directory, or
initialize a signing key. A completed command reports each created path after
its writer returns and ends with `Built <package>`. Local-development signing
retains its release-publish restriction as one note. These frames do not change
authoring admission, signing, native export, or publication behavior.

`cargo test -p conary --test cli_ccs_build` captures real build and dry-run
commands in terminal, pipe, and `NO_COLOR` modes, with chunking enabled and
disabled. It checks exact version/release fields, reopens the written CCS to
compare file counts, and proves that previews and missing-manifest refusals
leave output and key state absent. The missing-manifest next step uses the
current `conary ccs init` command. Build and verification behavior retains the
`packaging_m4b` and `packaging_m4e` proofs.

## Release Honesty

Do not mark an unsupported route as implemented in docs unless the focused test
target above or the referenced integration suite proves it. Keep active docs
clear that native package managers remain authoritative for adopted packages
until the user chooses explicit takeover.

Update artifact results count all selected full artifacts admitted for preview,
including targets whose later apply fails. The closing artifact frame reports
full artifacts prepared; it has no delta-attempt or bandwidth-savings lines.
New updates fetch no delta after full admission and persist zero delta attempts
and savings. Historical delta statistics remain readable and compute their
success rate from actual recorded attempts. Committed package rows remain the
applied-result authority. `cargo test -p conary --features test-hooks --test
update_artifact_acquisition -- --nocapture` counts signed full-artifact and
advertised valid/invalid delta response payloads in cold and warm CAS fixtures;
the durable guard is described in `docs/performance/README.md`.
