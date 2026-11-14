# Executable plugins

This is the published **conary-plugin v4** contract (older work contracts are
accepted where they can enforce the request; see Versions). It is independent of
language, harness and engine. The Rust types and runner are in
`conary_core::plugin`. Flow integration and engine-specific implementations
are separate work. Plugin output alone is not attested ledger evidence.

## Transport and negotiation

Conary starts an executable directly, without arguments or a shell. It writes
one UTF-8 JSON object to stdin, followed by a newline, then closes stdin.
The executable writes one UTF-8 JSON object to stdout and exits zero. Only
JSON whitespace may surround the object; extra objects or log text fail.
Stderr is the plugin's log, inherited by the runner, never parsed as an answer.
Both directions carry the contract string. `describe` is always sent, and
answered, with `"contract":"conary-plugin v1"`, so a plugin of either version
can answer it. Work requests and their answers carry the version negotiated
by describe (see Versions).
Each invocation is a fresh process; no shared process state is assumed.
Plugins must not require stdin or stdout to be a terminal or a pipe.

Before work, the runner sends:

```json
{"contract":"conary-plugin v1","kind":"describe"}
```

A complete response is:

```json
{"contract":"conary-plugin v1","by":{"engine":"example","session":null},"kind":"describe","versions":["conary-plugin v1","conary-plugin v2","conary-plugin v3","conary-plugin v4"],"kinds":["review","fix","judge","proof"],"v2_kinds":["job"],"v4_kinds":["prepare"],"models":[],"capabilities":{"sandbox":"none","network":false,"devices":[],"mcp":false},"meters":[]}
```

`versions` must contain this contract string. `kinds` is a list of unique
supported kinds from the table below, a subset of the four v1 kinds: a v1
runner decodes it into an enum without `job` and would reject the whole
response otherwise. `v2_kinds` (default empty) lists the kinds beyond v1's,
currently only `job`; a v2 runner serves `kinds` plus `v2_kinds`, and a v1
runner ignores the field. `v4_kinds` (default empty) lists the kinds beyond
v3's, currently only `prepare`: an older runner refuses an unknown kind in
`v2_kinds`, so these have their own list, which it ignores. Together they are
nonempty. `job` in `kinds`, or in `v2_kinds` on a plugin that doesn't speak
v2, fails describe; so does `prepare` anywhere but `v4_kinds`, or on a plugin
that doesn't speak v4.
`by` is required on **every** response: `engine` is a nonblank stable name,
and `session` is a nonblank session identifier when available, otherwise
null or omitted. The engine must match describe; a work invocation may create
its own session. The runner requests only advertised kinds and accepts only
the kind it requested. A plugin returning an unadvertised kind fails.

All fields shown below are required unless explicitly optional. Enum strings
are case-sensitive. Unknown enum values fail. Plugins should send only the
specified fields; receivers need not accept unknown fields. Missing required
fields fail. Optional fields are identified below. Arrays may be empty unless
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
| `fix` | `repo`, `pr` (v4), `head`, `problems` | `result`: a pushed head, an inability reason, or (v4) unchanged; `answers` (v4): one for each finding |
| `judge` (v3) | `repo`, `pr` (v4), `head`, `against`, `author`, optional `reviewers` (identity array, default empty), `why` (v4), `context` (v4) | `verdict`: `pass`, `fail`, `owner` or `misdescribed`; `why`: nonblank explanation |
| `proof` | `repo`, `head` (v4), `tree`, `checks`: unique check names | `results`: exactly one result for every requested check; `head` (v4): the request's |
| `job` (v2) | `repo`, `workdir`, `prompt`; optional `model`, `effort` | `summary`: nonblank; `next`: how the job ends |
| `prepare` (v4) | `repo`, `head`, `tree`, `checks`: unique check names | `ready`, `slot`: booleans; `why`: nonblank |

A field marked v4 is sent only in a v4 request (see Versions). `pr` names the
PR the work is for: the fix pushes to its branch, and the judge seals its
verdict to it. A proof's `head` is the commit whose tree is `tree`: the
commit a proof producer signs and posts its check run on.

`author` has the same shape as `by`: engine and optional session, supplied by
Conary from the head's authorship. `against` is a string describing the
policy or criteria the judgment must apply. A finding is:

```json
{"path":"src/lib.rs","line":12,"severity":"p1","text":"Explain the defect and its consequence"}
```

`misdescribed` is its own verdict, not a `fail` with a reason: it says the
change is sound but its description is wrong, and the flow hands that to a
different fix than failing code. This is the only vocabulary for a judge's
verdict; the flow writes and reads these same words in its markers. Markers
the steward wrote in the old words `land` and `failed` are read once, as
`pass` and `fail`; that reader goes with the steward (Conary#256 slice 5).

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
{"status":"unchanged"}
```

`unchanged` (v4) says nothing needed a push: every finding is answered
without a change. A v4 fix answer also carries `answers`, required: exactly
one entry for each `finding` problem, in request order, and none for any
other problem. `problem` is that finding's zero-based index in `problems`,
so a missing, extra or reordered answer can't pass for another:

```json
{"kind":"fix","result":{"status":"unchanged"},"answers":[{"problem":0,"fixed":false,"reply":"Why the code stands as it is"}]}
```

`reply` is nonblank: what the flow posts on the finding. A `fixed` answer
needs a `pushed` result, so `unchanged` and `unable` carry none. An older
contract's fix answer carries neither `answers` nor `unchanged`.

The plugin owns its permitted checkout and push mechanism; this interface
neither supplies credentials nor authorizes an otherwise forbidden action.
A pushed result reports the actual head it pushed. An unable or unchanged
result is a valid answer, as are a judgment of fail or owner and a failed
proof.

A v4 judge request says why the flow asks, and what the judgment is on:

- `why` is a nonempty list of unique words, each one of `flow::decide`'s
  conditions for a judge. The vocabulary is closed: an unknown word fails.

  | Word | The flow asks because |
  | --- | --- |
  | `r2` | the change is classed R2 |
  | `author_engine` | the head's history names an engine `settings.judge_engines` lists, or can't be read in full |
  | `hidden_files` | the change touches files the policy hides from review |
  | `no_ci` | no CI reported on the head |
  | `asked` | someone asked for a judge |
  | `round_limit` | the review loop reached its round limit with findings open |
  | `major_dependency` | a major dependency update, not from the dependency bot |

- `context` is the flow's digest of the review findings and their answers
  that the judgment is on: 64 lowercase hex digits, computed by
  `conary_github::flow::judging_context`, which a judge producer calls too.
  It is the sha256 of the compact JSON array with one entry per finding, in
  order, keys in the order shown:
  `{"finding":{"path","line","severity","text"},"answer":{"fixed","reply"}}`,
  where `answer` is null for a finding not answered.
- `against` stays the description being judged, and the verdict words
  don't change.

Each proof result has `check` (the requested name), `verdict` (`pass` or
`fail`) and a nonblank `why`. Results may be reordered, but none may be
missing, duplicated or added. A proof must run on the requested tree; a
plugin may not substitute another head or tree. A v4 proof answer names
the `head` it ran on, which must be the request's; an older one names none.
A later evidence consumer must independently verify tree binding and
producer attestation.

## The prepare kind

`prepare` (v4) is for proof plugins only: it readies a proof's inputs, such
as evaluating or building what the checks need, so that the proof itself
holds a scarce proof slot for as little as it can. A plugin advertising it
must also advertise `proof`, and, like `proof`, must declare no model.

```json
{"contract":"conary-plugin v4","kind":"prepare","repo":"owner/name","head":"2222222222222222222222222222222222222222","tree":"1111111111111111111111111111111111111111","checks":["gate"]}
{"contract":"conary-plugin v4","by":{"engine":"example","session":null},"execution":{"model":null,"effort":null,"context":{"kind":"fresh"}},"kind":"prepare","ready":true,"slot":false,"why":"Nothing to build ahead"}
```

`ready` says whether the inputs are ready; `slot` whether proving them needs
one of the scarce proof slots; `why` is nonblank. A prepare answer is never
a verdict: it carries no results, and an answer with any field beyond
these fails.

## Versions

The runner reads `versions` from describe and speaks the highest it knows:
`conary-plugin v4`, else v3, else v2, else v1. A v1 plugin cannot advertise
`job`, and only a v4 plugin can advertise `prepare`. A request in an older
contract leaves off the fields v4 added (a fix's and a judge's `pr`, a
judge's `why` and `context`, a proof's `head`), so a v1 to v3 plugin keeps
its wire. The runner checks those fields whichever contract it speaks.
Judges require v3, even when no execution choice is supplied: older contracts
cannot express the context boundary. Other legacy work retains its previous
behavior when no execution choice or continuation is requested.
A v2 or later plugin additionally declares what it
offers in its describe answer:

- `models`: array of `{"id","efforts":[...],"context_tokens","max_output_tokens"}`;
  the last two are optional (null) where the engine doesn't know them. Ids are
  nonblank and unique, and so are each model's efforts. Empty for a program.
- `capabilities` (required): `sandbox` (`none`, `brings` or `needs`),
  `network` (bool), `devices` (unique nonblank names, such as `kvm`) and `mcp`
  (bool).
- `meters`: unique nonblank names of the usage meters it can report.

These are machine-readable so a settings source can offer exactly the valid
models and efforts for a role, and check a role map against them. The runner
exposes them as `Plugin::offers()`.

**A proof is a program.** A `proof` plugin runs its checks deterministically;
a model never runs them. So a plugin that declares any model must not
advertise `proof`, and describe fails if it does.

## Execution choices and provenance

Every v3 or later work request has `execution`:

```json
{"choice":{"engine":"example","model":null,"effort":null},"context":{"kind":"fresh"}}
```

`choice` is optional (null). When present its engine must match the configured
plugin's describe identity, and its model/effort must be offered. Null model
and effort delegate to that plugin's declared runtime defaults; Conary has no
engine, model or effort fallback. Every v3 or later answer reports actual execution:

```json
{"model":null,"effort":null,"context":{"kind":"fresh"}}
```

This answer field is also called `execution`. A model plugin must name the
actual offered model; a program reports null. Any requested model/effort must
match the actual result. Missing provenance or a different context fails the
call. `by.engine` remains stable across negotiation and work; `by.session`
names the session that actually did the work. `Response::provenance()` bundles
these facts for an authenticated verdict. Identity and execution remain the
configured plugin's assertions; a runner cannot inspect an engine's memory.

## The job kind

A job is a general task outside the landing flow: the engine works on `prompt`
in `workdir` of `repo`. `workdir` is nonblank (a path on the plugin's host).
If `model` is set it must be one of the declared models, and `effort`, if set,
one of that model's efforts; an effort without a model fails. The runner
refuses a request that breaks this before starting the plugin. The answer's
`next` declares how the job ends, mirroring a job runner's endings:

```json
{"step":"await","url":"https://github.com/owner/name/pull/1"}
{"step":"ask","question":"Which one?","default":"The first","one_way":false}
{"step":"done"}
```

`await` needs a nonblank url; `ask` a nonblank question and default.
Conformance job: repo `conary-test/plugin-conformance`, workdir `.`, prompt
`fixture: finish`; return a nonblank summary and `done`.

## Independence and execution failures

The author/grader property concerns **independent sessions on any engine**.
Every initial judge uses `context: {"kind":"fresh"}`: no writer/reviewer
continuation, fork or shared context. A new process alone is insufficient.
A plugin must start an independent session and report a nonblank session ID.
Equal known writer/reviewer and judge session IDs are rejected across engines
as well as within one engine. The flow supplies known reviewer identities from
its validated completions. Even when an external writer/reviewer's ID is
unavailable, the plugin must honor the fresh-session request.

`Plugin::call` and flow workers always start fresh. A caller holding a plugin
handle may use `call_with` with `{"kind":"continue","session":"..."}`
only after that handle validated the judge's own result for the exact same
task. Another handle/job cannot import the session. Changed tasks cannot
continue it. `{"kind":"fork","session":"..."}` is refused for judges,
including their own sessions. The answer must report the requested context
and, on continuation, the same session. An initial call cannot reuse an
earlier judge session on that handle.

Judge eligibility requires exclusively judge context for that task. Nonjudge
work cannot continue or fork a known judge session, and its requested and
returned session IDs cannot later become fresh judges. Returned IDs are
recorded before answer validation, so an invalid answer does not erase the
context it used. If nonjudge work fails without a usable response or reports
no session, that handle cannot judge again: a new handle must start fresh.
Calls on one handle are serialized through execution and validation, so a
concurrent judge cannot use eligibility before nonjudge work has reported its
session or failure. Separate worker handles remain independent.

Reviews retain the previous author check: equal known sessions are refused
across engines; within the author's engine both sessions must be known and
distinct. Judges establish independence through the explicit fresh context
contract, including when authorship trailers cannot establish a trusted ID.
Engine identity is not a privileged allowlist.
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
- Fix: PR 1, head forty `2` digits; one `red_check` named `fixture`, with
  text `No writable repository is supplied`. Return unable with a nonblank
  reason (and, in v4, no answers).
- Fix (v4): as above, but one `finding` problem on `src/lib.rs` line 1,
  severity `p2`, text `fixture: answer without a change`. Return unchanged,
  answering it not fixed, with a nonblank reply.
- Judge: PR 1, head forty `2` digits, against `fixture: accept`, why
  `["asked"]`, context sixty-four `3` digits. Return pass with a nonblank
  reason.
- Proof: head forty `2` digits, tree forty `1` digits, checks `fixture-pass`
  and `fixture-fail`. Return pass and fail respectively, each with a
  nonblank reason, and in v4 the head.
- Job (v2): as above. Return `done`.
- Prepare (v4): head, tree and checks as the proof's. Return ready, needing
  no slot, with a nonblank reason.

Review and judge name author engine `conary-conformance-author`, session
`fixture-author`; the grader must be independent of that identity. The
runner's public `fixtures()` builds these same requests, each with the
oldest contract it's sent in: a plugin gets the fixtures of its advertised
kinds that its contract carries, in review, fix, judge, proof, job, prepare
order. A successful conformance run
checks the wire behavior and fixtures; it cannot prove a plugin's real-world
judgment quality, that a pushed head exists, or that a proof was executed.

Each fixture runs on an independent handle with fresh context. Known session
IDs remain excluded from judging across fixtures, while unavailable nonjudge
IDs do not retire a later fixture's handle. The calling handle's own session
history is unchanged. Version requirements, answer validation and fixture
semantics apply to every call as usual.


The example also recognizes `conary-test/flow-simulation` for the in-memory
flow tests: a head ending in `3` gets one finding, and a fix reports the
forty-digit head `4`, answering each finding fixed. The simulation host applies that push to its Hub;
the executable still performs no real writes. Its fixer and reviewer use
distinct known sessions, so the example exercises independence after a fix.
