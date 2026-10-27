# CI attestations

How a repo's CI attests its gate's result, so Conary can count it as evidence
([DESIGN section 9](DESIGN.md#9-security-and-identity)). burrow's shared
[`flake-check.yml`](https://github.com/FieldmouseWorks/burrow/blob/main/.github/workflows/flake-check.yml)
makes them, after `nix flake check`, on every push and same-repo PR of a
public repo. This page is the format; the workflow follows it.

## v1

Each attestation is a [Sigstore](https://www.sigstore.dev/) bundle that GitHub's
[`actions/attest`](https://github.com/actions/attest) makes with the job's own
OIDC identity and keeps in the repo's attestations API. Its certificate names
the repo, the workflow that signed (burrow's `flake-check.yml` at a commit),
the commit the job checked, and the run.

### Subject

The subject is a short text, and its SHA-256 is the subject's digest:

- for a PR, `conary revision v1\n<base>\n<head>\n`, named `conary-revision`,
  where `<base>` is the merge base of the head and the base branch and
  `<head>` is the PR's head: the revision as [section 4.2](DESIGN.md#42-revision)
  defines it;
- for a push, `conary commit v1\n<commit>\n`, named `conary-commit`, the commit
  pushed.

Commit ids are lowercase hex, and every line ends in `\n`. So Conary finds a
revision's attestations by hashing that text and asking
`GET /repos/{owner}/{repo}/attestations/sha256:{digest}`, and anyone can check
one with stock tools: write the text to a file and run

```sh
gh attestation verify <file> --repo <owner>/<repo> \
  --signer-workflow FieldmouseWorks/burrow/.github/workflows/flake-check.yml \
  --predicate-type https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1
```

(`gh` looks for SLSA provenance unless it's given the predicate type.)

### Predicate

Its type is `https://github.com/FieldmouseWorks/Conary/blob/main/docs/ci-attestation.md#v1`:

```json
{
  "result": "pass",
  "revision": { "base": "<merge base>", "head": "<PR head>" },
  "tree": "<tree id of the commit checked>",
  "skipped": ["agents"],
  "log": { "sha256": "<hex>", "artifact": "flake-log-1" }
}
```

- `result`: `pass` when the gate succeeded, otherwise `fail`. A cancelled run
  attests nothing.
- `revision`: the subject's revision, or `null` for a push.
- `tree`: the tree the gate ran on. For a PR that is GitHub's merge of the head
  onto the base branch's tip at the time, the commit in the certificate,
  never the head's own tree and never a landed one
  ([section 4.3](DESIGN.md#43-evidence), rule 3).
- `skipped`: checks the caller asked to evaluate but not build (its
  `build-skip`). They passed evaluation only, and their evidence comes from
  wherever the caller says they do run.
- `log`: the SHA-256 of the gate's output, kept as that run's artifact.

## What doesn't get one

- Private repos: GitHub makes attestations for private repos only on
  Enterprise Cloud. Their CI results don't count until remi's runner signs
  them with its own key (section 9).
- A run with no OIDC token: a fork's PR, or one Dependabot opened.

Either way the result is still CI, just not evidence: anything without a valid
attestation fails closed.

## Checking one

A verifier, Conary's at M3, takes an attestation as evidence only when all of
these hold:

1. The bundle verifies against Sigstore's public trust root.
2. The certificate's signer workflow is burrow's `flake-check.yml` at a
   commit on burrow's main, and its repo is the change's repo. A burrow
   branch, or a burrow PR's own run of the workflow it changes, can sign
   anything, so neither counts.
3. The subject's digest is the one for the revision (or the commit) asked
   about, and the predicate's `revision` says the same.
4. The predicate's type is this page's `#v1`.

The caller's own workflow can change what it passes in (`build-skip` among
them), and a PR's run takes that workflow from the PR. So the predicate says
what was skipped, and the policy that counts the evidence decides whether a
result with skipped checks is enough.
