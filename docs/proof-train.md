# Proofs on the machine: delivery plan for #218

This maps every requirement of
[#218](https://github.com/FieldmouseWorks/Conary/issues/218) (count proof
runs on the machine as evidence, so PSIV can ride a train) onto the work that
already exists, and specifies the rest as single-PR tasks in dependency order.
It plans only. It changes no behavior, activates nothing and grants no
authority beyond [#48](https://github.com/FieldmouseWorks/Conary/issues/48)'s
and [#256](https://github.com/FieldmouseWorks/Conary/issues/256)'s. It is a
snapshot of main on 2026-11-12; the issues it links own the live state.

## Two things kept apart

- **#48's exit check** is remi's: for a day of remi's PRs, fewer candidate gate
  runs than PRs landed. It is measured by
  [#326](https://github.com/FieldmouseWorks/Conary/issues/326)'s report, on
  remi, and nothing in this plan substitutes for it or relaxes it.
- **PSIV batching** is supplementary. It shows whether a queue of 3 to 5
  settled PRs gains from a train, which remi's arrival rate can't. Its numbers
  are labelled supplementary wherever they appear and never count toward the
  exit check (task P7).

## Rules every task below keeps

- **Producer independence.** A proof's statement is signed by the proof
  producer's own key, which no authoring session, sandbox, reviewer or judge
  producer can read, and each producer has its own key and identity (DESIGN
  section 9). Trusted public keys come from machine-owned configuration, never
  from a change under test. An author-controlled producer never counts.
- **Fail closed.** A missing, malformed, unknown-key or mismatched statement
  is not evidence. A status alone carries no attestation.
- **Exact binding.** A statement binds repo, revision, tree, check, result and
  log hash. A candidate's evidence counts for that candidate's tree only, and
  an R2 change lands only on evidence for its exact candidate tree. Evidence
  stays immutable; undo is a new operation.
- **Negative results.** A new test still needs its failing negative result;
  nothing here relaxes it. Each task below carries its own negative cases.
- **R2 isolation.** R2 changes ride alone, as now. Proof on the machine adds
  no path by which an R2 change lands without a person.
- **No keys in the repo.** Private-key provisioning (the new sops secret the
  steward's unit reads) is the owner's and the operator's, outside every
  implementation task here. Tasks consume public keys and a signer
  interface; they never create, print or store a private key.
- **Conary runs with no Nix and no burrow.** Machine values arrive as
  configuration; tests use the simulated GitHub with a generic config.

## #218's requirements, one owner each

| # | #218 requirement | Owner | State |
| --- | --- | --- | --- |
| 1a | The signed statement `(repo, commit, tree, check, result, log sha256)`: its format and verification | [#309](https://github.com/FieldmouseWorks/Conary/issues/309) | open |
| 1b | The producer that signs when a proof run ends, keeps the statement beside `end.json`, and reads its key only in the steward's unit | burrow's proof producers, planned by [#311](https://github.com/FieldmouseWorks/Conary/issues/311); requirements in P2 | planning open |
| 1c | The signing key itself | owner and operator; no task | outside authority |
| 2a | A new `Attest` variant checked against configured public keys | #309 | open |
| 2b | A required *status* counts on a candidate through that evidence (`train/gate.rs`) | **P1** | uncovered |
| 2c | A "Proofs on the machine" section in ci-attestation.md | #309 (format) and P1 (status binding) | open |
| 3 | `conary/train/` candidates get the repo's proof check started and its status posted | **P3** | uncovered |
| 3b | Per-PR proofs drop to something cheap | repo configuration, in P6 | uncovered |
| 4 | Public repos' hosted checks can be required on a candidate | existing token verifier; **P4** (documentation and regression coverage), **P5** (burrow publication) | verifier implemented; producer publication uncovered |
| 5 | PSIV `train = true`, `trainRequire = ["gate"]`, DEVELOPMENT.md | **P6** | uncovered, owner-sequenced |
| M3 | "fewer gate runs than PRs" on remi | #326, unchanged | open |
| M3b | PSIV batching measured | **P7** | uncovered, supplementary |

Reused as they are, not rescheduled:

- [#311](https://github.com/FieldmouseWorks/Conary/issues/311) already plans
  the Rust proof plugin and its independent signing producer, with key
  provisioning as a named dependency. P2 is not a task; it is the list of what
  #218 asks of #311's output, so the planner of #311 doesn't need to rediscover
  it.
- [#295](https://github.com/FieldmouseWorks/Conary/issues/295) (contract
  changes, "a proof is a program") and
  [#303](https://github.com/FieldmouseWorks/Conary/issues/303) (verdict
  vocabulary) stay the only owners of the plugin contract. A proof answer is
  the existing `results` per requested check; nothing here adds to it.
- PSIV#346 (two-lane gate) cuts each run's wall time and stays PSIV's; it
  stacks with batching and is not a prerequisite.
- [#291](https://github.com/FieldmouseWorks/Conary/issues/291) and its burrow
  child #322 reuse an attested pass on the same tree. They overlap with the
  candidate's proof in that a candidate tree later landing on main is a hit; P3
  must not reimplement the lookup, and may use it once it exists.
- burrow#728 and burrow#740 (one pinned `cn attest` step in
  `flake-check.yml`) are the prerequisite for P5.
- The proof-queue change (a proof's inputs prepared outside the scarce slot) is
  the steward's side of #256 slice 5; P3 requires it, because it is what makes
  proof execution program-only.

## P2: what #218 asks of #311's producer

Not a separate PR. #311's plan must show these, and its conformance checks
must fail without them:

- The producer signs only a program's recorded exit, never a model's answer.
  Preparation happens before the proof slot.
- The statement is #309's, versioned, signed with the producer's own key and
  stored next to the run's `end.json`, so it can be handed to Conary later.
- A cancelled or restarted run produces no pass statement. A killed run is
  not a failure statement either unless the program recorded one.
- The producer knows its signing key only through the unit's credential and
  has no verb that signs an arbitrary statement.

## The uncovered tasks, in dependency order

Each is one PR. They are specified here; they aren't filed as issues until
their dependencies are close to landing, so none becomes a tracker.

### P1: a required status counts through service evidence (Conary)

Depends on #309.

Wire the train's gate (`train/gate.rs`, which today says "a status carries no
attestation") so a required check that is a status counts when a verified
service statement for the candidate's commit and tree, check name and a
passing result is found, using #309's shared verifier, not a second one. Define
where the statement is read from, as machine-owned configuration (a directory
or a ledger-ingested record), so conaryd works on a box without burrow. Add
the "Proofs on the machine" section to ci-attestation.md for the status
binding.

Check, against the simulated GitHub: a green status with a valid statement
counts; the same status with no statement, an altered result, log, commit,
tree or check name, an unknown key, an author-controlled producer or a
statement for another candidate does not, and the candidate waits or fails
closed. A statement for the PR head does not count for the candidate's tree.
Negative regression cases fail before the change. Run the repository gate.

### P3: the train proves its candidates (Conary, flag off)

Depends on P1, #311's proof plugin and the proof-queue change.

`cn train`, after it builds `conary/train/ID`, asks the configured proof plugin
for the repo's machine-proof checks on that candidate's tree (the contract's
`proof` kind: repo, tree, checks), ingests the signed statements, and posts the
status P1 reads. Scheduling lives in Conary's train, not in the Python
steward, which #256 slice 5 deletes; porting it there would be doing the work
twice. It is configuration, off by default. A repo with none behaves exactly as
now.

Check: with the simulated GitHub and a fake proof plugin, one candidate runs
each configured check once (no duplicate on retry or restart); a failed or
absent result splits or holds the candidate as a red gate does; a slow or lost
plugin answer leaves the candidate waiting, never green; a plugin that
returns a result for an unrequested check, or for another tree, is rejected.
Landing behavior with the flag off is unchanged, shown by the existing train
tests. Run the repository gate.

### P4: document and cover existing public-token evidence (Conary)

No implementation prerequisite. Can land independently of #309, P1 and P5;
P5 does not wait for it.

#218 offers two ways: read Sigstore bundles, or let public repos' jobs keep
the token artifact private repos keep. Take the second. It reuses the verifier
that already counts private repos' evidence:
[`attest::attested`](../crates/conary-github/src/attest.rs) reads token artifacts
without consulting repository visibility, and
[`train/gate.rs`](../crates/conary-github/src/train/gate.rs) already uses it for
required candidate checks. Public and private artifacts follow the same trust
checks. Once a producer publishes the artifact, no verifier change is needed;
there is no new trust root or crypto dependency. Update ci-attestation.md's
"Public repos" to describe this existing support and distinguish token
verification by the train from `gh attestation verify`'s bundle verification
for a person. Reading bundles in the train is not scheduled.

Check: review the existing verifier and candidate-gate tests and extend only
missing regression coverage. For both public and private token artifacts, a
candidate counts with a valid artifact and not with an altered subject, a token
from another run, another repo or workflow, a self-hosted job not on a fresh
runner, or no artifact. GitHub-hosted runners retain their existing acceptance;
required statuses without attestation retain their existing rejection. These
checks exercise the shared verifier, not a new visibility-specific path. Run
the applicable tests; this documentation and coverage task changes no verifier
behavior and does not gate producer publication.

### P5: public repos keep the token artifact (burrow)

Depends only on burrow#728/#740, not on P4 or #309. One PR in burrow, flag off.

In the shared `cn attest` step, public repos also keep the token artifact. No
second copy of the format, and no change for private repos. Both use Conary's
existing verifier and trust checks; publication needs no verifier implementation.

Check: workflow tests fail before the change; public with the flag on keeps the
artifact and still makes the bundle, off or private is as before; the artifact
names the digest of what it attests. Run burrow's applicable gate.

### P6: PSIV opts in (remi and PSIV, owner-sequenced deployment)

Depends on P1, P3, P5, a provisioned key (owner and operator), the
trusted-key configuration on remi, and PSIV's 1.0. Not an implementation
child: it activates a service and changes where PSIV's merge bar runs, so it is
issued only when the owner sequences it. Two PRs in two repos at most: PSIV
(`train = true`, `trainRequire = ["gate"]`, per-PR proof reduced to hosted CI
or `gate.py --fast`, and DEVELOPMENT.md's "Gate and coverage" saying the full
gate runs per batch) and remi's pin and configuration. PSIV's ruleset change
follows the remi pattern (#48's fourth slice).

Check: one real batch lands with each PR's last landing on its candidate's
signed evidence, and the first failing proof is shown to hold the batch.

### P7: supplementary PSIV batching report (Conary)

Depends on P6 and #326's report.

Run #326's report over a PSIV window, through its existing interfaces, with the
output labelled supplementary and carrying a field that makes #48's check
refuse it. No new scheduler or verifier.

Check: a test shows the exit check rejecting a PSIV report; one real window is
published with its operation, commit and run references. If PSIV's queue never
reaches a batch, the report says so and nothing is manufactured.

## Order

```
#309 ──► P1 ────────────────────► P3
#311 (proof plugin, P2) ─────────► P3
#256 slice 5 (proof queue) ──────► P3
burrow#728/#740 ──► P5 ──────────► P6
P1, P3 ─────────────────────────► P6
key provisioning, trusted config, PSIV 1.0 ──► P6
P6, #326 ───────────────────────► P7

P4: independent documentation and regression coverage of existing support
```

#326 (remi's exit report) is independent of all of it and goes first or in
parallel; nothing here gates it.

## Nothing is left over

Every line of #218's "Shape" appears in the table, and the "M3 exit relevance"
note is carried by the two-measurements split above. No tracker or ongoing
workstream is an implementation child: #256, #48, #218 and #311 are not tasks;
P1, P3, P5 and P7 are implementation PRs; P4 is a documentation and regression
coverage PR, P2 is #311's requirements, and P6 waits for the owner.
