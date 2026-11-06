# CI attestations

How a repo's CI attests its gate's result, so Conary can count it as evidence
([DESIGN section 9](DESIGN.md#9-security-and-identity)). burrow's shared
[`flake-check.yml`](https://github.com/FieldmouseWorks/burrow/blob/main/.github/workflows/flake-check.yml)
makes them, after `nix flake check`, on every push and same-repo PR. This page
is the format; the workflow follows it.

## v1

Each attestation is signed with the job's own GitHub OIDC identity: GitHub
signs the job a token naming the repo, the workflow that asked for it
(the signer workflow, such as burrow's `flake-check.yml`, at a commit), the
commit the job checked and the
run. A public repo's job turns that into a [Sigstore](https://www.sigstore.dev/)
bundle; a private repo's signs with the token itself (both below). Either way
it attests the same subject and predicate.

### Subject

The subject is a short text, and its SHA-256 is the subject's digest:

- for a PR, `conary revision v1\n<base>\n<head>\n`, named `conary-revision`,
  where `<base>` is the merge base of the head and the base branch and
  `<head>` is the PR's head: the revision as [section 4.2](DESIGN.md#42-revision)
  defines it;
- for a push, `conary commit v1\n<commit>\n`, named `conary-commit`, the commit
  pushed.

Commit ids are lowercase hex, and every line ends in `\n`.

### Predicate

Its type is `https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1`:

```json
{
  "result": "pass",
  "revision": { "base": "<merge base>", "head": "<PR head>" },
  "tree": "<tree id of the commit checked>",
  "skipped": ["agents"],
  "log": { "sha256": "<hex>", "artifact": "flake-log-<uuid>" }
}
```

- `result`: `pass` when the gate succeeded, otherwise `fail`. A cancelled run
  attests nothing.
- `revision`: the subject's revision, or `null` for a push.
- `tree`: the tree the gate ran on. For a PR that is the head merged onto the
  base branch's tip as the job found it (the workflow's merge step), never
  the head's own tree and never a landed one
  ([section 4.3](DESIGN.md#43-evidence), rule 3).
- `skipped`: checks the caller asked to evaluate but not build (its
  `build-skip`). They passed evaluation only, and their evidence comes from
  wherever the caller says they do run.
- `log`: the SHA-256 of the gate's output, and the name of the run's
  artifact that keeps it, its own for each job.

### Public repos: a Sigstore bundle

GitHub's [`actions/attest`](https://github.com/actions/attest) makes the
bundle and keeps it in the repo's attestations API. Its certificate carries
the token's claims. So Conary finds a revision's attestations by hashing the
subject and asking `GET /repos/{owner}/{repo}/attestations/sha256:{digest}`,
and anyone can check one with stock tools: write the subject to a file and
run

```sh
gh attestation verify <file> --repo <owner>/<repo> \
  --signer-workflow "$CONARY_SIGNER_WORKFLOW" \
  --predicate-type https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1
```

(`gh` looks for SLSA provenance unless it's given the predicate type.)

### Private repos: the job's own token

A private repo gets no Sigstore bundle
([section 9](DESIGN.md#9-security-and-identity) says why, and why no key on
the runner signs instead), so its job signs with the token itself: a token's
audience is any text the job asks for, and the job asks for one that names
the digest of what it attests.

The statement is the subject, then the predicate as one line of JSON, then
`\n`. The job asks GitHub for a token whose audience is
`conary-attestation:v1:sha256:<hex>`, the statement's SHA-256 in lowercase
hex, and keeps the statement and the token, in that order, as the file
`conary-attestation` in an artifact of the run named
`conary-attestation-<uuid>`, the same `<uuid>` as its log's. The token is the
file's last line.

## Negative runs

[Rule 2](DESIGN.md#43-evidence) asks a change that adds a test for a
`negative` record: its new tests run on the old code, failing.
`flake-check.yml`'s `negative` job makes it on every same-repo PR, train
repos' included, and attests it the way the gate's result is attested
above, with a subject and a predicate of its own.

- **Test files.** A path is a test file when it matches one of the patterns
  in the workflow itself (`tests/**`, `**/tests.rs`, `**/tests/**`,
  `**/test_*.py`, `**/*_test.*`, `**/*.test.ts`, `src/test/**`). They aren't
  an input: the caller's workflow comes from the PR, and a PR mustn't choose
  its own judge.
- **Whether it needs one.** The PR's changes, from its merge base to its
  head, renames as the path left and the path made: if they add or change a
  test file and change anything else too, it needs a negative. Otherwise
  (tests alone, no tests, or test files only deleted) it needs none, and the
  job attests that. A change that tidies tests beside code no test of it
  catches is two changes: the tidy alone needs none.
- **The negative tree.** The merge base's tree with the head's version of
  each test file the PR changes: added, changed, or deleted.
- **What it builds.** The flake's checks whose derivation on the negative
  tree differs from the merge base's: those that read a changed test file,
  and any that take the whole tree as their source rather than a filtered
  one. Every other check builds what it built on the base, which passed
  there. If any of those checks fails (a test failing, a check that doesn't
  evaluate, or the tests not compiling against the old code), the new tests
  failed on the old code. A check the caller's `build-skip` names isn't
  built here.
- **What runs a test.** Reading a test file isn't running it: a formatter,
  a lint or a secrets scan reads every file. A check says which tests it
  runs as its `testFiles`, git glob pathspecs from the repo's root, in the
  base's flake (burrow's `lib/project.nix` says how); one that doesn't say
  runs every test it reads. If every check built passes, a new test passed
  on the old code only where one of them runs it. A new test none of them
  runs is `unrun`: the negative sees only the flake, and where that test
  fails, if anywhere, is for the gate that runs it to say.

The subject is `conary negative v1\n<base>\n<head>\n`, named
`conary-negative`: the PR's revision, under a name of its own, so a negative
can never be read as the gate's result for that revision. The predicate's
type is `https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#negative-v1`:

```json
{
  "result": "fail",
  "revision": { "base": "<merge base>", "head": "<PR head>" },
  "tree": "<the negative tree>",
  "tests": ["crates/conary-core/src/ledger/tests.rs"],
  "checks": ["crates"],
  "skipped": [],
  "unrun": [],
  "log": { "sha256": "<hex>", "artifact": "negative-log-<uuid>" }
}
```

- `result`: `fail` when a check failed on the negative tree, which is what
  rule 2 asks for; `pass` when every one passed and each new test is run by
  one of them; `skipped` when every one built passed but some that read the
  tests weren't built (`build-skip`) or some new tests none of them runs
  (`unrun`), so where they'd fail is for wherever those are built or run;
  `none` when the PR needs no negative (`tree` is then `null`, and the
  lists empty).
- `tests`: the test files the PR changes.
- `checks`: the checks it built (or that didn't evaluate), those whose
  derivation differs from the base's.
- `skipped`: those it would have built but `build-skip` names.
- `unrun`: the new tests no check that passed runs.

The job's check run is named `negative`, and concludes `success` when the
result is `fail`, `skipped` or `none`, and `failure` when it's `pass`: a red
check that says the new tests don't test the change. Only `fail` and `none`
count as a change's negative. A private repo signs the
statement with the job's token as above, under the same audience format.
Checking one is as for the gate's result, with this subject and predicate,
and rule 7's conclusion reads `success` for `fail`, `skipped` or `none`.

A train asked for negatives (`cn train join --negative CHECK`, CHECK being
the caller's job and then `negative`) lets a change join only once the
latest run of that check on its head is attested this way for the revision
joining, and says `fail` or `none`. What it checked is recorded as that
revision's `negative` evidence, with its attestation, and counted from the
ledger after: `fail` and `pass` as what the new tests did on the old code,
`none` as itself.

## What doesn't get one

A run with no OIDC token, such as a fork's PR or one Dependabot opened. Its
result is still CI, just not evidence: anything without a valid attestation
fails closed.

## Checking one

A verifier takes an attestation as evidence only when all of these hold.
Conary's train checks a private repo's token this way (`conary-github`'s
`attest`): on its candidate's commit, and on a PR's negative run when it's
asked to. Nothing checks a bundle, or a PR's revision's gate, yet.

1. GitHub signed it: the bundle verifies against Sigstore's public trust
   root, or the token against GitHub's published keys
   (`https://token.actions.githubusercontent.com/.well-known/jwks`, RS256),
   from the issuer `https://token.actions.githubusercontent.com`.
2. The signer workflow (the token's `job_workflow_ref` and
   `job_workflow_sha`, which the certificate carries too) is the one the
   machine running the train names, at a commit on its repo's default
   branch, and the token's repo is the change's repo. The machine names it
   as `owner/repo/.github/workflows/FILE`, the way `gh attestation verify
   --signer-workflow` takes it (`CONARY_SIGNER_WORKFLOW`, or `cn train
   --signer-workflow`), with no default; remi names burrow's
   `flake-check.yml`. A branch of the signer's repo, or a PR's own run of
   the workflow it changes, can sign anything, so neither counts. What the ledger kept counts again (a gate's pass read on a
   later step, a revision's negative when it joins again) only while its
   token names the signer the machine names now; a green verdict already
   recorded stands as it was decided.
3. The subject's digest is the one for the revision (or the commit) asked
   about, and the predicate's `revision` says the same.
4. It's this page's `#v1`: the bundle's predicate type, or the token's
   audience.
5. The job ran on a runner started fresh for it, which nothing the
   candidate controls ran on before it or runs beside it as its user:
   - a GitHub-hosted runner, a VM of its own for each job (the token's
     `runner_environment`, which the certificate carries too, is
     `github-hosted`); or
   - a self-hosted runner registered just in time for that one job, in a
     user and a state directory nothing ran in before, whose name starts
     with what the machine running the train names its fresh runners by
     (`CONARY_FRESH_RUNNERS`, or `cn train step --fresh-runners`). GitHub's
     record of the job
     (`GET /repos/{owner}/{repo}/actions/jobs/{job}`) says which runner took
     it, and only the launcher that starts fresh runners registers ones with
     such a name: registering a runner takes the repo's or the
     organization's runner administration, which no workflow has.

   A runner that takes job after job runs each as the same user. Any job
   before the signing one, such as one the candidate's own workflow adds,
   can leave a process behind that reads the next job's token request
   credentials from its environment and asks for a token in that job's name
   with any audience, or change files the next job runs, or read the
   runner's own credentials and take its jobs. Nothing in the token tells
   those apart from the job's own, so nothing from such a runner counts.

   That is all a fresh runner guards against: a stale or poisoned CI job.
   It doesn't hold against a session on the runner's machine that wants to
   cheat, wherever that session can become the launcher's user (on remi
   every session can, with passwordless sudo): such a session can register
   its own runner under such a name, or take a registration
   before its runner does, and sign whatever it likes. Root on the machine
   is outside what any attestation from it proves
   ([section 9](DESIGN.md#9-security-and-identity)).

A token also has to bind its statement to its run:

6. Its audience is the one for the statement it came with.
7. Its `check_run_id` is the check run being counted. That check run, as
   GitHub reports it, is on the subject's commit (a PR's is on its head), has
   completed, and concluded `success` when the result is `pass`. So a token
   taken from one run can't say another passed: the run's own conclusion has
   to agree.
8. For a push, its `sha` is the subject's commit too. (A PR's is GitHub's
   merge as the event found it, which the job doesn't check.)

Inside the signing job, nothing of the candidate's runs as the job's user:
burrow's `flake-check.yml` evaluates its flake and builds it in nix-daemon's
sandbox, as Nix's build users, and runs only burrow's own steps and actions
pinned to commits. That is part of what rule 2's signer workflow
guarantees, so a signer that ran the candidate's code as the job's user
would break rule 5's reason for counting it, and a machine names only a
workflow that keeps to this.

A token expires minutes after the job asks for it; that only stops it being
used to sign in somewhere, and doesn't bear on checking it. GitHub can drop a
key from its published set later, so the verifier checks a token when it
records it, and the ledger keeps the token with the evidence.

The caller's own workflow can change what it passes in (`build-skip` among
them), and a PR's run takes that workflow from the PR. So the predicate says
what was skipped, and the policy that counts the evidence decides whether a
result with skipped checks is enough. The train's counts none.
