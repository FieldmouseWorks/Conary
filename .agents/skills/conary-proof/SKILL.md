---
name: conary-proof
description: Route to and run focused Conary repository proof, with optional local command receipts.
---

# Conary proof

Route the changed feature or path with `bash scripts/agent-context.sh
--feature <slug>` or `--path <file>`. Read the returned owner packet and run its
focused proof; run its interaction gate only when the stated condition applies.
For integration-suite contracts, use `docs/INTEGRATION-TESTING.md` and the
`conary-test` card in `docs/modules/feature-ownership.md`.

When a local command-bound receipt is useful, run
`python3 scripts/agent-proof.py -- <canonical command> <arguments...>`. Read the
[workflow's receipt contract](../../../docs/llms/agent-workflow.md#work-evidence-and-feedback)
for its scope and stale-result behavior. The helper records only the process it
runs; invoke the selected proof command directly when the exact leaf exit status
matters instead of wrapping `agent-context.sh --run`.
