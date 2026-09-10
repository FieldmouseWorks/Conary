---
last_updated: 2026-09-10
revision: 10
summary: Document conaryd authorization, package jobs, routes, and shared bounded HTTP response framing before JSON and SSE
---

# conaryd

`conaryd` is the local daemon for query routes, package job queueing, and SSE
events. It listens on the configured local socket and applies the same
apply-intent boundary as the CLI for package mutation jobs. Unimplemented
system-operation routes are absent rather than exposed as placeholders.

## Authorization

`GET /health` is outside the v1 auth gate so service managers can perform basic
liveness checks. `/v1/*` routes are behind the v1 gate. Query routes are
read-oriented. Package mutation and system operation routes require the daemon
authorization checks and still require explicit apply intent in request bodies
where the operation can mutate the host. Package requests send
`apply_intent: true`; removed acknowledgement aliases are rejected as unknown
request fields.

Root, the daemon identity, and members of the exact group passed through
`--socket-group` can perform daemon operations. At startup, conaryd resolves
that group once and fails if it does not exist; the same group owns the
mode-`0660` Unix socket and is checked against `SO_PEERCRED` plus the live
process supplementary-group list. With no configured group, the API is
root/daemon-only. There is no PolicyKit placeholder, broad authenticated-user
fallback, or implicit `sudo`/`wheel` distribution-group selection.

## Package Job Execution Boundary

Daemon install, remove, and update jobs are queued and tracked by `conaryd`, but
the package operation executor in `apps/conaryd/src/daemon/package_ops.rs`
currently calls the CLI command functions from the `conary` crate
(`cmd_install`, `cmd_remove`, and `cmd_update`). Changes to package-job behavior
therefore need both daemon-route/job proof and the owning CLI package-command
proof; `package_ops.rs` is the adapter boundary, not an independent package
manager implementation.

Apply-intent refusals retain `conary::live_host_safety::LiveMutationRefusal`,
including the exact command label and mutation class. Its plain display uses
the same diagnostic facts and guidance as the CLI without terminal escapes,
so direct package-job callers and persisted error messages retain the safe
preview/confirmation routes. This does not change job error schemas or the
apply-intent predicate.

Native runtime preflight and aggregated update failures retain the versioned
`conary.package.failure.v1` observation report in
`error.extensions.package_failure`. The report type lives in
`crates/conary-agent-contract/src/package_failure.rs`; the Conary command adapter
projects the original typed errors before the daemon converts its summary to
text. The ordinary error detail still includes the package, stage, and causal
summary for clients that do not inspect the extension. Persisted job errors,
job inspection, and `JobFailed` SSE use the same
extension. It distinguishes requested root/database scope from the disposable
execution root and reports observed earlier committed changesets for an update
selection. An absent commit list means observations were not supplied, not that
an enclosing job changed nothing. Unknown failure chains remain data and never
authorize a retry. Job status, socket authorization, and approval predicates
are unchanged; no database authority schema changes.

A mutating package operation is complete only when its exact selected-root
generation is published. If the package database commit succeeds but generation
publication leaves recoverable debt, the daemon reports the job as failed with
the persisted publication phase, failure detail, and exact retry command. It
must not label a database-only mutation as a completed package job. Package
publication never mutates the ambient root passed to the daemon; the new
generation is the execution result.

## Route Reference

The Unix-socket client checks response media types before consuming the body: ordinary
JSON results require `application/json`, structured daemon errors require
`application/problem+json`, and event streams require `text/event-stream`
before any event callback. Header names and media-type names are case
insensitive, and valid parameters are accepted through the media-type parser.
Missing, malformed, duplicate, or mismatched Content-Type fields produce bounded
protocol errors. Empty cancellation responses do not require a JSON media type.
The interpretation follows [RFC 9110, section 8.3](https://www.rfc-editor.org/rfc/rfc9110.html#section-8.3).
`apps/conaryd/src/daemon/client/response.rs` owns the shared header parser,
media-type validation, and header size and wall-clock deadline limits.
`apps/conaryd/src/daemon/client/body.rs` owns HTTP body framing before JSON or
SSE parsing, following [RFC 9112, sections 6 and 7](https://www.rfc-editor.org/rfc/rfc9112.html#section-6).
It supports exact Content-Length, connection-close bodies, and chunked coding.
Up to eight informational response heads may precede the final response under
the same header deadline; unsolicited protocol upgrades are rejected.
Repeated Content-Length values must agree; conflicting framing and unsupported
transfer codings fail before body interpretation. Chunk boundaries may split
JSON tokens, UTF-8 characters, or SSE lines without changing decoded content.
Fixed-length and completed chunked responses finish without waiting for socket
close. Premature EOF, invalid chunk syntax, and malformed trailers fail with
bounded diagnostics. Chunk metadata lines are limited to 8 KiB; trailer sections
are limited to 32 KiB and 128 fields. Trailers cannot redefine body framing or
Content-Type. `apps/conaryd/src/daemon/client/body_stream.rs` owns body read
deadlines, including every internal read used to assemble chunk metadata.
Ordinary response bodies share one configured body deadline. SSE validates the
remaining HTTP framing before its terminal callback or final job lookup, under
the lesser of the configured timeout and ten seconds. A socket timeout ends
the stream with an error instead of resuming a partially read frame.

The route list below is checked by `scripts/check-doc-truth.sh` against
`apps/conaryd/src/daemon/routes/{system,transactions,query,events}.rs`.

<!-- conaryd-routes:start -->
GET /health | Health check outside the v1 auth gate
GET /v1/version | Version and build metadata
GET /v1/metrics | Prometheus-style daemon metrics
GET /v1/transactions | List visible daemon jobs
POST /v1/transactions | Queue a daemon transaction job
POST /v1/transactions/dry-run | Preview a daemon transaction request
GET /v1/transactions/{id} | Get a visible daemon job
DELETE /v1/transactions/{id} | Cancel a visible daemon job
GET /v1/transactions/{id}/stream | Stream visible daemon job events
POST /v1/packages/install | Queue package install work
POST /v1/packages/remove | Queue package remove work
POST /v1/packages/update | Queue package update work
POST /v1/enhance | Queue enhancement work
GET /v1/packages | List packages
GET /v1/packages/{name} | Get package details
GET /v1/packages/{name}/files | List package files
GET /v1/search | Search package names
GET /v1/depends/{name} | List direct package dependencies
GET /v1/rdepends/{name} | List reverse package dependencies
GET /v1/history | List changeset history with publication status
GET /v1/events | Stream daemon events
<!-- conaryd-routes:end -->

Route implementation ownership: `apps/conaryd/src/daemon/routes.rs` is the
route hub; `daemon/config.rs` owns runtime configuration and canonical defaults;
`routes/router.rs` owns Axum assembly; `routes/types.rs` owns API DTOs;
`routes/errors.rs` owns API error conversion; `routes/auth.rs` owns route-level
auth and job/event visibility gates; `routes/db.rs` owns blocking DB query
plumbing; `routes/sse.rs` owns SSE connection guarding; and
`routes/{system,query,transactions,events}.rs` own endpoint declarations and
handlers.
