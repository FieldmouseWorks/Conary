---
last_updated: 2026-09-08
revision: 1
summary: Versioned observation contract for CCS verification through CLI JSON and local packaging MCP
---

# CCS Verification Report V1

`crates/conary-agent-contract/src/verification.rs` owns the transport-neutral
request, report, and derived JSON Schema. The schema discriminator is
`conary.ccs.verification.v1`. Decoders reject unknown versions, fields, and cause
variants. Verified and failed outcomes are disjoint: a failure cannot carry
verified facts.

The application service in `apps/conary/src/commands/ccs/verification.rs` calls
core verification once. Human output, JSON, and the local MCP adapter share this
service. `apps/conary/src/ui/diagnostics/verification.rs` maps original typed
failures to the contract and takes summary/next-step wording from the human
diagnostic owner. It never parses displayed fields or infers causes from text.

## CLI

```bash
conary ccs verify dist/package.ccs --policy trust.toml --json
```

For a dispatched verification request, stdout contains one JSON object. Verified
outcomes exit 0; failed outcomes exit 1 without an additional human error frame.
The object is independent of TTY, pipe, and `NO_COLOR`; optional tracing remains
on stderr. Command-line syntax and startup errors retain their existing handling.

The CLI keeps its existing optional policy selection: without `--policy`, it
uses an initialized local-dev key when available. That existing path may maintain
its lock and public-key mirror. The report's nullable `policy` field records
whether an explicit policy path was supplied.

## Local MCP

`conary.packaging.verify_artifact` on `conary mcp packaging` requires `package`
and `policy` strings. The explicit policy keeps this read-only operation separate
from local-dev key maintenance. The adapter runs the same verification service
on a blocking worker and advertises the contract's output schema.

The result carries the report in `structuredContent` with a JSON text fallback;
`isError` is true for a failed verification outcome. Expected verification
failures remain contract results. Transport or worker failures remain MCP errors.
No Remi endpoint or remote service is added.

## Fields And Meaning

Every report records the requested `package`, optional explicit `policy`, schema,
and `outcome`. Requested paths are input identities, not authenticated metadata.

A `verified` outcome contains package name, version, version scheme, release,
optional architecture, exact compressed archive SHA-256 and byte length, checked
file count, verified public key, claimed key ID, and signature timestamp. The
package-provided key ID remains a claim; trust is established by exact public-key
membership and core signature verification. Timestamp metadata retains its
existing policy semantics.

A `failed` outcome contains a typed `cause`, human summary/notes, and retained
archive/policy context paths when available. Causes preserve:

- missing signatures, malformed signature data, unsupported algorithms, and
  invalid signatures;
- empty or duplicate trust anchors, exact untrusted signer claims/public keys,
  and missing, malformed, future, or expired timestamps with numeric age limits;
- payload/structure failures, structural budget fields and limits, and every v3
  authority diagnostic's code, severity, message, field, path, invalid flag, and
  suggestion;
- missing input/policy conditions and unclassified failures with their original
  cause chains.

Raw strings survive JSON round trips. JSON escaping keeps terminal control bytes
out of the serialized stream; it does not replace the original diagnostic data
with the human renderer's escaped field text.

## Authority And Proof

A report is an observation of one verification attempt. Deserializing or caching
it cannot produce `VerifiedCcsArchive`, authorize a signing key, or permit install
or publication. Those operations still acquire current core verification
capabilities. This contract introduces no persisted authority schema change.

Proof lives in the agent-contract verification tests, CLI diagnostic integration
captures, UI projection tests, and the packaging MCP server's verification tests.
The tests cover strict decoding, original-field retention, exact archive identity,
status/exit agreement, default-policy failure, invalid policy/archive input,
TTY/pipe/`NO_COLOR`, and MCP structured failure adaptation.
