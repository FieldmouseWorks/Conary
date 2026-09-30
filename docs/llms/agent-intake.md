---
last_updated: 2026-09-30
revision: 1
summary: Define the read-only daily GitHub workflow failure intake and its local state contract
---

# Daily GitHub Intake

`scripts/agent-intake.py` checks the current `main` commit and the current head
of each open pull request. It uses only explicit GitHub API `GET` requests to
read completed and in-progress workflow runs. For each workflow and head SHA,
it considers the newest run number and newest attempt. A newer success or run
in progress fences an older failure; failed, timed-out, startup-failed, and
approval-required results can be reported.

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
