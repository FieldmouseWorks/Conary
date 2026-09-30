---
last_updated: 2026-09-30
revision: 2
summary: Define the read-only daily GitHub workflow failure intake and its local state contract
---

# Daily GitHub Intake

`scripts/agent-intake.py` checks the current `main` commit and the current head
of each open pull request. It uses only explicit GitHub API `GET` requests to
read workflow runs. For each workflow and head SHA,
it considers the newest run number and newest attempt. A newer success or run
in progress fences an older failure; failed, timed-out, startup-failed, and
approval-required results can be reported.

The request pins REST API version `2026-03-10`. Workflow-run status and
conclusion fields must match the recognized GitHub vocabulary: completed runs
require a known non-null conclusion, and unfinished runs require a null
conclusion. Unknown or inconsistent outcomes fail without advancing state.
For each `head_sha` search, the tool also checks page `total_count` consistency
and unique run IDs; GitHub caps such searches at 1,000 results, so an
at-cap or incomplete result fails closed. The contract follows GitHub's
[workflow-runs endpoint](https://docs.github.com/en/rest/actions/workflow-runs?apiVersion=2026-03-10#list-workflow-runs-for-a-repository),
[check suite status and conclusion values](https://docs.github.com/en/rest/guides/using-the-rest-api-to-interact-with-checks?apiVersion=2026-03-10#about-check-suites),
and [REST API version header guidance](https://docs.github.com/en/rest/about-the-rest-api/api-versions?apiVersion=2026-03-10).

The first run records current failures as a quiet baseline. Later runs print
JSON only for newly observed failures. `--report-existing` explicitly prints
currently active failures, including previously seen ones. Output names the
run URL, head SHA, workflow, run ID, attempt, branch or pull request, and an
absolute local report path. The JSON report is stored under `reports/` for a
separate local reader.

Run it with an absolute state directory outside the checkout:

```bash
python3 scripts/agent-intake.py --repo OWNER/REPO --state-dir /absolute/user-local/state-dir
```

The directory is private to the current user and contains a lock, state, and
content-addressed JSON reports. API or state errors exit nonzero without
advancing state. The tool does not create a schedule, invoke a model, or write
to GitHub. Any schedule and downstream analysis remain host-local choices.
