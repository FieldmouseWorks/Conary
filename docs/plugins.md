# Executable plugins

This is the published **conary-plugin v1** contract. It is independent of
language, harness and engine. The Rust types and runner are in
`conary_core::plugin`. Flow integration and engine-specific implementations
are separate work. Plugin output alone is not attested ledger evidence.

## Transport and negotiation

Conary starts an executable directly, without arguments or a shell. It writes
one UTF-8 JSON object to stdin, followed by a newline, then closes stdin.
The executable writes one UTF-8 JSON object to stdout and exits zero. Only
JSON whitespace may surround the object; extra objects or log text fail.
Stderr is the plugin's log, inherited by the runner, never parsed as an answer.
Both directions carry `"contract":"conary-plugin v1"`.
Each invocation is a fresh process; no shared process state is assumed.
Plugins must not require stdin or stdout to be a terminal or a pipe.

Before work, the runner sends:

```json
{"contract":"conary-plugin v1","kind":"describe"}
```

A complete response is:

```json
{"contract":"conary-plugin v1","by":{"engine":"example","session":null},"kind":"describe","versions":["conary-plugin v1"],"kinds":["review","fix","judge","proof"]}
```

`versions` must contain this contract string. `kinds` is a nonempty list of
unique supported kinds from the four above. A plugin can serve a subset.
`by` is required on **every** response: `engine` is a nonblank stable name,
and `session` is a nonblank session identifier when available, otherwise
null or omitted. The engine must match describe; a work invocation may create
its own session. The runner requests only advertised kinds and accepts only
the kind it requested. A plugin returning an unadvertised kind fails.

All fields shown below are required unless explicitly optional. Enum strings
are case-sensitive. Unknown enum values fail. Plugins should send only the
specified fields; receivers need not accept unknown fields. Missing required
fields fail. `session` is the sole optional field. Arrays may be empty unless
otherwise stated. Commit and tree names are full Git object IDs (40 or 64 hex
digits), not branch names. Repo is `owner/name`; PR and line are positive
integers. Paths are relative to the repository, separated by `/`, and name
something inside it: absolute paths, backslashes, NULs, paths that traverse
above the repository root, and paths naming only the root fail. Validation
is lexical, not a checkout symlink resolver. Lines are one-based.

## Work requests and answers

Each request has `contract`, `kind`, and these kind-specific fields. Each
response has `contract`, `by`, `kind`, and the listed answer fields.

| Kind | Request fields | Answer fields |
| --- | --- | --- |
| `review` | `repo`, `pr`, `base`, `head`, `author` | `findings`: array of findings, empty for none |
| `fix` | `repo`, `head`, `problems` | `result`: a pushed head or an inability reason |
| `judge` | `repo`, `head`, `against`, `author` | `verdict`: `pass`, `fail` or `owner`; `why`: nonblank explanation |
| `proof` | `repo`, `tree`, `checks`: unique check names | `results`: exactly one result for every requested check |

`author` has the same shape as `by`: engine and optional session, supplied by
Conary from the head's authorship. `against` is a string describing the
policy or criteria the judgment must apply. A finding is:

```json
{"path":"src/lib.rs","line":12,"severity":"p1","text":"Explain the defect and its consequence"}
```

Severity is `p0`, `p1`, `p2` or `p3`, highest first. Path and text must be
nonblank, and line must be positive. A fix problem is one of:

```json
{"kind":"finding","finding":{"path":"src/lib.rs","line":12,"severity":"p1","text":"Defect"}}
{"kind":"red_check","check":"gate","text":"Check output or a log reference"}
{"kind":"conflict","base":"1111111111111111111111111111111111111111","text":"Conflict details"}
```

A fix's `result` is one of:

```json
{"status":"pushed","head":"2222222222222222222222222222222222222222"}
{"status":"unable","why":"A nonblank reason no fix was pushed"}
```

The plugin owns its permitted checkout and push mechanism; this interface
neither supplies credentials nor authorizes an otherwise forbidden action.
A pushed result reports the actual head it pushed. An unable result is a
valid answer, as are a judgment of fail or owner and a failed proof.

Each proof result has `check` (the requested name), `verdict` (`pass` or
`fail`) and a nonblank `why`. Results may be reordered, but none may be
missing, duplicated or added. A proof must run on the requested tree; a
plugin may not substitute another head or tree. A later evidence consumer
must independently verify tree binding and producer attestation.

## Independence and execution failures

The author never reviews or judges its own head. Before accepting a review
or judgment, Conary compares its `by` with the request's `author`. Equal known
sessions are refused even across different engines. For the same engine, if
either session is unknown, independence cannot be established and is refused. Distinct
known sessions of the same engine are allowed. Engine identity is not a
privileged allowlist: every implementation goes through this rule.
Identity is the configured plugin's assertion, not a cryptographic
attestation; deployment must trust that plugin to report its identity.

The caller supplies a wall-clock deadline and maximum stdout response size
in bytes, applying separately to describe and every work invocation. The
runner polls the process and stdout size, kills and reaps the executable on
a breach, and never parses more than the cap. File-backed stdio prevents
blocking on a plugin that doesn't consume its request or leaves a pipe open.
Size enforcement occurs at polling intervals; this is not a disk quota.
A deadline breach, size breach, nonzero exit (including a signal), malformed
JSON, wrong version, missing field, unexpected kind, invalid answer or
self-grading is a **plugin failure**, returned as an error, never a verdict.
The deadline covers process execution, not sandbox provisioning.

The runner does not create a sandbox, constrain stderr, isolate descendants
or verify signatures. The deployment must provide the plugin's own sandbox
and resource limits. In particular, don't run an engine without its sandbox.

## Conformance

Build and run the copyable deterministic Rust example:

```console
cargo build -p conary-cli --bins
cn plugin check ./target/debug/conary-example-plugin
```

Use the built `target/debug/cn` if cn is not installed. Caller limits are
`--deadline-ms` (default 30000) and `--response-bytes` (default 1048576).
Success exits zero and prints `by` and the `checked` kinds as JSON. Failure
exits nonzero with its diagnostic on stderr. CI's workspace tests run this
command on the example and exercise broken executables for each failure.
The example source is `crates/conary-core/examples/plugin.rs`; its answers
are illustrative, never evidence about real code.

A conforming plugin recognizes the reserved repository
`conary-test/plugin-conformance` without network access, pushing or running
checks. These canned requests exercise every advertised kind:

- Review: PR 1, base forty `1` digits, head forty `2` digits. Return no findings.
- Fix: head forty `2` digits; one `red_check` named `fixture`, with text
  `No writable repository is supplied`. Return unable with a nonblank reason.
- Judge: head forty `2` digits, against `fixture: accept`. Return pass with a
  nonblank reason.
- Proof: tree forty `1` digits, checks `fixture-pass` and `fixture-fail`.
  Return pass and fail respectively, each with a nonblank reason.

Review and judge name author engine `conary-conformance-author`, session
`fixture-author`; the grader must be independent of that identity. The
runner's public `fixtures()` builds these same requests. An advertised subset
is checked in review, fix, judge, proof order. A successful conformance run
checks the wire behavior and fixtures; it cannot prove a plugin's real-world
judgment quality, that a pushed head exists, or that a proof was executed.
