# Gate reuse

Whether CI already attested a pass of a gate on exactly a tree, so a push
of that tree under another commit needn't run the gate again
([DESIGN section 4.3](DESIGN.md#43-evidence), rule 3). burrow's
`flake-check.yml` asks before a push to the default branch builds anything,
under the contract `burrow-gate-reuse/v1`: burrow's
`crates/burrow-flake-check/README.md`, "Reusing an attested pass", defines
the request, its token and the answer. This page is Conary's side: what it
counts as a hit, and how a machine runs it. The lookup is
`conary-github`'s `reuse`; conaryd serves it over HTTP.

## What's a hit

The answer is `hit` only when all of these hold, for one check run:

1. **The requester is the job asking.** The request carries the job's
   GitHub OIDC token, signed by GitHub, whose audience is
   `burrow-gate-reuse:v1:sha256:` and the request body's SHA-256, whose
   repository is the request's (one the machine names), whose `sha` is the
   request's commit, and whose `job_workflow_ref` and `job_workflow_sha`
   are the workflow its gate names, at the commit it names. The token
   hasn't expired. Otherwise the request is refused (HTTP 401), and the
   gate runs.
2. **The tree is the push's.** GitHub says the request's commit holds the
   request's tree, and the gate's workflow commit holds the workflow tree
   the gate names. Otherwise the lookup can't tell (`unknown`).
3. **The verdict is the tree's.** The gate's `source` is `tree`: burrow
   evaluated a checkout of the tree alone, so equal trees are the whole
   match for what it checked. Any other gate is a miss.
4. **CI attested a pass of it, as the train counts one.** The check run
   concluded `success`, and its job's attestation holds under every rule of
   [Checking one](ci-attestation.md#checking-one): GitHub's signature, the
   signer workflow the machine names (`signer`) at a commit on its
   default branch, a runner GitHub hosts or one started fresh for the job
   (`fresh`), the statement's digest as the token's audience, and the token
   naming this check run. Its statement says `pass` on exactly the
   request's tree, as a push of the check run's commit (`conary commit
   v1`) or as a PR revision whose head is that commit (`conary revision
   v1`, which ran on the head's merge with the base branch). A service's
   statement never makes one: only CI's own attestation says a gate passed
   on a tree.
5. **It's the same gate.** Its predicate's `gate` names the signer workflow
   at the commit its token names, that commit holds the workflow tree the
   gate says, and the gate equals the request's in everything but
   `workflow.commit`: the same workflow files (their tree), source and
   inputs. A changed workflow has another tree, so it never reuses an old
   pass. An attestation from before workflows named their gate is a miss.
6. **It skipped nothing this gate builds.** Each check its predicate says
   it skipped is one the request's gate skips too (`inputs.build-skip`).

The answer names that check run's job,
`{server}/{repository}/actions/runs/{run}/job/{job}`: the run that built and
attested the pass. A push that reused a pass attests nothing, so it never
answers a later request in its place.

**Where it looks.** The check runs that passed on: the head of each PR
GitHub links the push to (the PR it landed, whose last run checked its merge
with trunk), each candidate in the repository's train whose tree the ledger
says is the request's, and the push itself (an earlier attempt of its own
run). At most 32 passed check runs are read for one answer; past that it
says `unknown`.

**Miss and unknown.** `miss` means nothing it read is such a pass, and
says why in `why`. `unknown` means it couldn't tell: GitHub didn't answer
one of its reads, the ledger couldn't be read, the request's tree isn't its
commit's, or there was more to read than one answer reads. burrow runs the
whole gate on either; `unknown` also marks its lookup degraded. An
attestation whose artifact has expired is gone, so it counts as no
evidence: a miss, never a hit. A request that doesn't read is refused with
400, one for a repository the machine doesn't name with 403, and when
GitHub's signing keys can't be read the requester can't be checked: 503.

**What it never does.** It records nothing: an answer is neither evidence
nor an op, so every record stays on the revision or candidate that produced
it, as it was. Reuse skips only a push's gate run. It never stands in for a
`negative` record, a review or a judgment, and an R2 change still lands
only on its train candidate's own evidence.

**Limits.** A PR's own run of the workflow it changes signs nothing that
counts (rule 2 of [Checking one](ci-attestation.md#checking-one)), and
neither does a run of the signer at a PR's merge. So in the signer's own
repository, where PRs run the workflow from the PR, only pushes' and train
candidates' passes can be hits.

## Running it

conaryd serves the lookup when it's given `--gate-reuse-config PATH`
(`CONARY_GATE_REUSE_CONFIG`), a machine-owned JSON file whose unknown fields
are refused:

```json
{
  "listen": "127.0.0.1:8091",
  "repos": ["example/project"],
  "token_command": "/usr/local/bin/conary-token",
  "signer": "example/ci/.github/workflows/gate.yml",
  "fresh": null,
  "api": "https://api.github.com",
  "server": "https://github.com"
}
```

- `listen`: the HTTP address. Put TLS termination in front of it: burrow
  asks over https unless the lookup is on the runner's own machine.
- `repos`: the repositories it answers for.
- `token_command`: prints a repository's GitHub token, as `cn`'s does. It
  reads check runs, Actions jobs and artifacts, commits and PRs.
- `signer`: the workflow whose attestations count, as for the train
  (`CONARY_SIGNER_WORKFLOW`).
- `fresh`: what the names of self-hosted runners started fresh for one job
  start with, as for the train (`CONARY_FRESH_RUNNERS`); `null` counts
  GitHub's hosted runners alone.
- `api` and `server`: GitHub's API root and web root, github.com's when
  left out.

It reads the ledger conaryd serves, for the trains' candidates. Without
the option, no listener starts. A caller's workflow then names the
lookup's external URL as its `gate-reuse-lookup`, with `gate-reuse` on.
