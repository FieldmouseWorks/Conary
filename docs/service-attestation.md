# Service producer attestations

The machine can trust a review, proof, judgment or negative-run service's
Ed25519 key alongside [CI attestations](ci-attestation.md). Flow and train
use the same verifier. A plugin answer or a check's success alone grants
no permission. This implements the service-key path in
[DESIGN section 9](DESIGN.md#9-security-and-identity).

## Machine configuration

The machine supplies a JSON file of public keys:

```json
[
  {
    "name": "review-service",
    "key": "BASE64URL_ED25519_PUBLIC_KEY",
    "checks": ["conary/review/*"]
  }
]
```

`key` is the 32-byte public key encoded as unpadded base64url. Each key
belongs to exactly one producer. Multiple keys can name the same producer
while rotating keys. `checks` is a nonempty list of exact check names or
prefixes ending in `*`; `*` alone is refused. Unknown fields, invalid or
weak keys and duplicate keys are errors.

For `cn train step` and `cn train join --negative`, supply
`--producer-keys /etc/conary/producers.json` or `CONARY_PRODUCER_KEYS`.
For [flow](flow.md#producer-evidence), set machine-owned
`evidence.producers` to that file's path. The existing CI signer and runner
configuration still applies to CI evidence. An omitted producers file
trusts no service; a configured file that is missing or invalid fails
closed. Configuration is read for each pass or train invocation. Removing
a key or its check scope prevents previously recorded service evidence
from counting again, without changing or deleting its ledger record.

Keep both the configuration and the signing service outside author control.
An authoring session, its sandbox and the code under test must not be able
to read the private key, replace the public-key configuration, or ask the
service to sign arbitrary statements. A producer name in an unsigned plugin
answer is no identity. Trust comes from the machine's key assignment and
check scope, not from a key supplied by a PR. Provisioning, storing and
isolating private keys is outside this change; no key is installed by
Conary. This interface uses plain files and Rust APIs and requires neither
burrow nor Nix.

## Signed statement v1

A service posts the following exact UTF-8 bytes as the reporting check run's
`output.text`, including the final newline:

```text
conary service v1
{"producer":"review-service","repository":"owner/repo","revision":{"base":null,"head":"COMMIT"},"tree":"TREE","check":"conary/review/reviewer","definition":null,"result":"pass","log":{"sha256":"LOG_SHA256"}}
ed25519 PUBLIC_KEY SIGNATURE
```

The signature covers the header and the single JSON predicate line, each
ending in a newline. `PUBLIC_KEY` is the configured key; `SIGNATURE` is the
64-byte Ed25519 signature, also unpadded base64url. The verifier checks the
exact statement bytes rather than reserializing them. The library exports
`attest::service::{Statement, Revision, Log, sign}` to construct this format;
`Statement::text` returns the bytes for a service using another signer.
Unknown versions and predicate fields are refused, and `definition` is
always present, `null` included. Input is limited to
64 KiB. A service statement that fails verification never falls back to CI.

The signed producer must match the key's configured identity, and the key
must be trusted for the signed check. The check must match the reporting
check run's name, and `definition` the check's definition as the machine
selects it now: for a [flow proof](flow.md#producer-evidence), the
lowercase hexadecimal SHA-256 of its recipe's JSON as the flow settings
hold it (`{"Build":"…"}`, `{"Run":["…"]}` or `{"Job":"…"}`;
`flow::recipe_digest`), and `null` for every other check, the train's
gates and negatives included. A pass signed for a recipe the machine no
longer runs under that name proves nothing. A CI attestation names no
recipe, so a check the machine defines counts only from a service. Repository names compare without case; commits and trees
compare exactly. For a commit result, `base` is null, `head` is the tested
commit, `tree` is that commit's exact tree and `result` must be `pass`, with
a successful, completed check run. The log hash is a lowercase hexadecimal
SHA-256 digest. For a review or decision, it must equal the hash of the exact
[producer receipt](flow.md#producer-evidence) selected by flow. Ordinary
proof logs are bound by their signed digest; Conary does not download them.

For a negative run, `revision.base` and `revision.head` name the PR's merge
base and head. `tree` identifies the old code with the head's tests applied:
`fail` or `pass` requires a tree ID; `none` requires null. `fail` and `none`
have a successful check conclusion, while `pass` has a failure conclusion.
Only `fail` or `none` satisfies admission. A producer must report `none`
only when there are no new tests to run; a failure to run tests is no pass.

The verified statement, public key and signature are retained verbatim in
`Attest::Service` on the immutable evidence operation. They do not replace
conaryd's operation signature. A check run's output can be rewritten after
it is read, so the train never counts a kept service attestation in place
of the run's current output: each gate step verifies the statement the run
publishes then. Only a kept CI attestation stands in for a recheck, and
only for the very check run its token names while that run carries no
service statement. Train verification asks about the candidate's
own commit and tree, including R2 candidates: a PR-head statement cannot
prove a different candidate, and service evidence grants no person approval
or exemption from the train's trunk binding.

## Verification

`cargo test -p conary-github service` exercises the shared verifier, simulated
flow and train. It accepts independent service evidence beside CI and
rejects altered results, logs, revisions and trees, wrong repositories,
checks and check definitions, rewritten or removed publications, unknown or author-held keys, malformed signatures, out-of-scope keys
and invalid configuration. It also checks receipt binding, negative results,
key removal and R2 candidate scope. The repository gate runs these tests.
