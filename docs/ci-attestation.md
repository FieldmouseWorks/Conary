# CI attestations

How a repo's CI attests its gate's result, so Conary can count it as evidence
([DESIGN section 9](DESIGN.md#9-security-and-identity)). burrow's shared
[`flake-check.yml`](https://github.com/FieldmouseWorks/burrow/blob/main/.github/workflows/flake-check.yml)
makes them, after `nix flake check`, on every push and same-repo PR. This page
is the format; the workflow follows it.

## v1

Each attestation is signed with the job's own GitHub OIDC identity: GitHub
signs the job a token naming the repo, the workflow that asked for it
(burrow's `flake-check.yml` at a commit), the commit the job checked and the
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
  --signer-workflow FieldmouseWorks/burrow/.github/workflows/flake-check.yml \
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

## What doesn't get one

A run with no OIDC token, such as a fork's PR or one Dependabot opened. Its
result is still CI, just not evidence: anything without a valid attestation
fails closed.

## Checking one

A verifier, Conary's at M3, takes an attestation as evidence only when all of
these hold:

1. GitHub signed it: the bundle verifies against Sigstore's public trust
   root, or the token against GitHub's published keys
   (`https://token.actions.githubusercontent.com/.well-known/jwks`, RS256),
   from the issuer `https://token.actions.githubusercontent.com`.
2. The signer workflow (the token's `job_workflow_ref` and
   `job_workflow_sha`, which the certificate carries too) is burrow's
   `flake-check.yml` at a commit on burrow's main, and its repo is the
   change's repo. A burrow branch, or a burrow PR's own run of the workflow
   it changes, can sign anything, so neither counts.
3. The subject's digest is the one for the revision (or the commit) asked
   about, and the predicate's `revision` says the same.
4. It's this page's `#v1`: the bundle's predicate type, or the token's
   audience.

A token also has to bind its statement to its run:

5. Its audience is the one for the statement it came with.
6. Its `check_run_id` is the check run being counted. That check run, as
   GitHub reports it, is on the subject's commit (a PR's is on its head), has
   completed, and concluded `success` when the result is `pass`. So a token
   taken from one run can't say another passed: the run's own conclusion has
   to agree.
7. For a push, its `sha` is the subject's commit too. (A PR's is GitHub's
   merge as the event found it, which the job doesn't check.)

A token expires minutes after the job asks for it; that only stops it being
used to sign in somewhere, and doesn't bear on checking it. GitHub can drop a
key from its published set later, so the verifier checks a token when it
records it, and the ledger keeps the token with the evidence.

The caller's own workflow can change what it passes in (`build-skip` among
them), and a PR's run takes that workflow from the PR. So the predicate says
what was skipped, and the policy that counts the evidence decides whether a
result with skipped checks is enough.
