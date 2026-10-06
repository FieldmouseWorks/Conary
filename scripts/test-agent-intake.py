#!/usr/bin/env python3
"""Behavioral controls for the read-only daily GitHub intake."""

import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("agent-intake.py")
FAKE_GH = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
with open(os.environ["GH_CALL_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps(args) + "\n")

if not args or args[0] != "api" or "--method" not in args:
    sys.exit(90)
method = args[args.index("--method") + 1]
if method != "GET":
    sys.exit(91)
if "--header" not in args or args[args.index("--header") + 1] != "X-GitHub-Api-Version: 2026-03-10":
    sys.exit(94)
route = next((item for item in args if item.startswith("repos/")), None)
if route is None:
    sys.exit(92)
fields = {}
index = 0
while index < len(args):
    if args[index] in ("-F", "--field"):
        key, value = args[index + 1].split("=", 1)
        fields[key] = value
        index += 2
    else:
        index += 1

fixture = json.loads(Path(os.environ["GH_FIXTURE"]).read_text(encoding="utf-8"))
if route in fixture.get("fail_routes", []):
    print("fixture API failure", file=sys.stderr)
    sys.exit(17)
if route.endswith("/commits/main"):
    response = fixture["main"]
elif route.endswith("/pulls"):
    response = fixture.get("pulls", [])
elif route.endswith("/actions/runs"):
    response = fixture.get("runs_by_sha", {}).get(fields.get("head_sha"), {"total_count": 0, "workflow_runs": []})
else:
    sys.exit(93)

if isinstance(response, dict) and "__raw__" in response:
    sys.stdout.write(response["__raw__"])
elif isinstance(response, dict) and "__pages__" in response:
    for page in response["__pages__"]:
        sys.stdout.write(json.dumps(page) + "\n")
else:
    sys.stdout.write(json.dumps(response) + "\n")
'''


def sha(number):
    return f"{number:040x}"


def workflow_run(run_id, run_number, head_sha, *, workflow_id=1, attempt=1,
                 status="completed", conclusion="failure", name="Build"):
    return {
        "id": run_id,
        "run_attempt": attempt,
        "workflow_id": workflow_id,
        "run_number": run_number,
        "head_sha": head_sha,
        "head_branch": "feature/test",
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "html_url": f"https://github.com/test/repo/actions/runs/{run_id}",
    }


def run_page(*runs, total_count=None):
    return {"total_count": len(runs) if total_count is None else total_count, "workflow_runs": list(runs)}


class AgentIntakeTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.state = self.root / "local-state"
        self.state.mkdir(mode=0o700)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake = self.bin / "gh"
        fake.write_text(FAKE_GH, encoding="utf-8")
        fake.chmod(0o700)
        self.fixture_path = self.root / "fixture.json"
        self.call_log = self.root / "gh-calls.jsonl"

    def invoke(self, fixture, *extra):
        self.fixture_path.write_text(json.dumps(fixture), encoding="utf-8")
        environment = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ.get('PATH', '')}",
            "GH_FIXTURE": str(self.fixture_path),
            "GH_CALL_LOG": str(self.call_log),
        }
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--repo", "test/repo", "--state-dir", str(self.state), *extra],
            cwd=SCRIPT.parents[1], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )

    def calls(self):
        if not self.call_log.exists():
            return []
        return [json.loads(line) for line in self.call_log.read_text(encoding="utf-8").splitlines()]

    def fixture(self, main_sha, *, pulls=(), runs=None, fail_routes=()):
        return {
            "main": {"sha": main_sha},
            "pulls": {"__pages__": [[*pulls]]},
            "runs_by_sha": runs or {},
            "fail_routes": list(fail_routes),
        }

    def state_json(self):
        return json.loads((self.state / "state.json").read_text(encoding="utf-8"))

    def test_first_run_seeds_current_failures_quietly_and_report_existing_is_explicit(self):
        main_sha = sha(1)
        pr_sha = sha(2)
        fixture = self.fixture(
            main_sha,
            pulls=[{"number": 41, "head": {"sha": pr_sha, "ref": "feature/change"}}],
            runs={
                main_sha: {"__pages__": [
                    run_page(workflow_run(100, 10, main_sha), total_count=2),
                    run_page(workflow_run(102, 1, main_sha, workflow_id=3), total_count=2),
                ]},
                pr_sha: run_page(workflow_run(101, 4, pr_sha, workflow_id=2)),
            },
        )
        first = self.invoke(fixture)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, b"")
        state = self.state_json()
        self.assertEqual(len(state["seen"]), 3)
        self.assertEqual(len(state["watermarks"]), 3)
        self.assertFalse((self.state / "reports").exists())

        explicit = self.invoke(fixture, "--report-existing")
        self.assertEqual(explicit.returncode, 0, explicit.stderr)
        output = json.loads(explicit.stdout)
        self.assertEqual(len(output["failures"]), 3)
        self.assertTrue(Path(output["report_path"]).is_file())
        self.assertEqual(Path(output["report_path"]).read_text(encoding="utf-8").count('"url"'), 3)
        self.assertTrue(all(item["report_path"] == output["report_path"] for item in output["failures"]))
        self.assertTrue(any("--paginate" in call for call in self.calls()))
        self.assertTrue(all(call[call.index("--method") + 1] == "GET" for call in self.calls()))

    def test_unchanged_failures_stay_quiet_after_baseline(self):
        main_sha = sha(3)
        fixture = self.fixture(main_sha, runs={main_sha: run_page(workflow_run(110, 5, main_sha))})
        first = self.invoke(fixture)
        self.assertEqual(first.returncode, 0, first.stderr)
        state_before = (self.state / "state.json").read_bytes()
        second = self.invoke(fixture)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(second.stdout, b"")
        self.assertEqual((self.state / "state.json").read_bytes(), state_before)

    def test_new_current_failure_emits_machine_readable_report_and_locator(self):
        main_sha = sha(4)
        pr_sha = sha(5)
        pull = {"number": 55, "head": {"sha": pr_sha, "ref": "feature/new"}}
        baseline = self.fixture(main_sha, pulls=[pull], runs={main_sha: run_page(), pr_sha: run_page()})
        first = self.invoke(baseline)
        self.assertEqual(first.returncode, 0, first.stderr)
        current = self.fixture(
            main_sha,
            pulls=[pull],
            runs={
                main_sha: run_page(),
                pr_sha: run_page(workflow_run(120, 8, pr_sha, workflow_id=4, name="Lint")),
            },
        )
        second = self.invoke(current)
        self.assertEqual(second.returncode, 0, second.stderr)
        output = json.loads(second.stdout)
        self.assertEqual(len(output["failures"]), 1)
        failure = output["failures"][0]
        self.assertEqual(failure["sha"], pr_sha)
        self.assertEqual(failure["workflow"], "Lint")
        self.assertEqual(failure["run_id"], 120)
        self.assertEqual(failure["attempt"], 1)
        self.assertEqual(failure["url"], "https://github.com/test/repo/actions/runs/120")
        self.assertEqual(failure["branches"], ["PR #55 (feature/new)"])
        self.assertTrue(Path(failure["report_path"]).is_file())
        self.assertEqual(failure["report_path"], output["report_path"])

    def test_stale_heads_pending_newer_runs_and_successful_reruns_fence_old_failures(self):
        main_sha = sha(6)
        current_pr_sha = sha(7)
        stale_sha = sha(8)
        pull = {"number": 61, "head": {"sha": current_pr_sha, "ref": "feature/moved"}}
        baseline = self.fixture(
            main_sha,
            pulls=[pull],
            runs={
                main_sha: {"__pages__": [run_page(
                    workflow_run(130, 10, main_sha),
                    workflow_run(131, 11, main_sha, status="in_progress", conclusion=None),
                    workflow_run(132, 12, stale_sha),
                )]},
                current_pr_sha: run_page(
                    workflow_run(140, 8, current_pr_sha, workflow_id=2),
                    workflow_run(141, 9, current_pr_sha, workflow_id=2, conclusion="success"),
                ),
            },
        )
        first = self.invoke(baseline)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, b"")
        self.assertEqual(len(self.state_json()["watermarks"]), 2)

        # The newer in-progress main run and successful PR rerun may disappear
        # from the API response; persisted watermarks still fence old failures.
        stale_only = self.fixture(
            main_sha,
            pulls=[pull],
            runs={
                main_sha: run_page(workflow_run(130, 10, main_sha)),
                current_pr_sha: run_page(workflow_run(140, 8, current_pr_sha, workflow_id=2)),
            },
        )
        second = self.invoke(stale_only)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(second.stdout, b"")
        queried = [call for call in self.calls() if call[-1].endswith("/actions/runs")]
        self.assertFalse(any("head_sha=" + stale_sha in call for call in queried))

    def test_attempt_is_part_of_dedup_key_and_newer_success_stays_quiet(self):
        main_sha = sha(9)
        self.assertEqual(self.invoke(self.fixture(main_sha)).returncode, 0)

        attempt_one = self.fixture(main_sha, runs={main_sha: run_page(workflow_run(150, 20, main_sha, workflow_id=8))})
        first_failure = self.invoke(attempt_one)
        self.assertEqual(first_failure.returncode, 0, first_failure.stderr)
        self.assertEqual(json.loads(first_failure.stdout)["failures"][0]["attempt"], 1)

        attempt_two = self.fixture(main_sha, runs={main_sha: run_page(
            workflow_run(150, 20, main_sha, workflow_id=8, attempt=2),
        )})
        second_failure = self.invoke(attempt_two)
        self.assertEqual(second_failure.returncode, 0, second_failure.stderr)
        self.assertEqual(json.loads(second_failure.stdout)["failures"][0]["attempt"], 2)
        self.assertEqual(len(self.state_json()["seen"]), 2)

        successful_rerun = self.fixture(main_sha, runs={main_sha: run_page(
            workflow_run(150, 20, main_sha, workflow_id=8, attempt=2, conclusion="success"),
        )})
        resolved = self.invoke(successful_rerun)
        self.assertEqual(resolved.returncode, 0, resolved.stderr)
        self.assertEqual(resolved.stdout, b"")

    def test_api_failure_and_malformed_state_preserve_last_state(self):
        main_sha = sha(10)
        baseline = self.fixture(main_sha, runs={main_sha: run_page()})
        seeded = self.invoke(baseline)
        self.assertEqual(seeded.returncode, 0, seeded.stderr)
        state_path = self.state / "state.json"
        before = state_path.read_bytes()

        failed_api = self.fixture(main_sha, fail_routes=["repos/test/repo/actions/runs"])
        failed = self.invoke(failed_api)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(state_path.read_bytes(), before)

        malformed_api = self.fixture(main_sha, runs={main_sha: {"__raw__": "{malformed"}})
        malformed_response = self.invoke(malformed_api)
        self.assertNotEqual(malformed_response.returncode, 0)
        self.assertEqual(state_path.read_bytes(), before)

        state_path.write_bytes(b"{malformed\n")
        malformed_before = state_path.read_bytes()
        malformed = self.invoke(baseline)
        self.assertNotEqual(malformed.returncode, 0)
        self.assertEqual(state_path.read_bytes(), malformed_before)

    def test_unknown_or_inconsistent_conclusions_preserve_last_state(self):
        main_sha = sha(13)
        baseline = self.fixture(main_sha, runs={main_sha: run_page()})
        seeded = self.invoke(baseline)
        self.assertEqual(seeded.returncode, 0, seeded.stderr)
        state_path = self.state / "state.json"
        before = state_path.read_bytes()

        unknown = workflow_run(160, 1, main_sha, conclusion="future_failure")
        missing = workflow_run(161, 2, main_sha)
        missing.pop("conclusion")
        null_completed = workflow_run(162, 3, main_sha, conclusion=None)
        unfinished_with_conclusion = workflow_run(163, 4, main_sha, status="in_progress", conclusion="failure")
        for invalid in (unknown, missing, null_completed, unfinished_with_conclusion):
            result = self.invoke(self.fixture(main_sha, runs={main_sha: run_page(invalid)}))
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(state_path.read_bytes(), before)

    def test_head_sha_result_cap_and_incomplete_pagination_preserve_state(self):
        main_sha = sha(14)
        baseline = self.fixture(main_sha, runs={main_sha: run_page()})
        self.assertEqual(self.invoke(baseline).returncode, 0)
        state_path = self.state / "state.json"
        before = state_path.read_bytes()

        for count in (1000, 1001):
            capped = self.fixture(main_sha, runs={main_sha: run_page(
                workflow_run(170, 1, main_sha), total_count=count,
            )})
            result = self.invoke(capped)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(state_path.read_bytes(), before)

        incomplete = self.fixture(main_sha, runs={main_sha: run_page(
            workflow_run(171, 2, main_sha), total_count=2,
        )})
        result = self.invoke(incomplete)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state_path.read_bytes(), before)

        inconsistent_pages = self.fixture(main_sha, runs={main_sha: {"__pages__": [
            run_page(workflow_run(172, 3, main_sha), total_count=2),
            run_page(workflow_run(173, 4, main_sha), total_count=3),
        ]}})
        result = self.invoke(inconsistent_pages)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state_path.read_bytes(), before)

        duplicate_id = workflow_run(174, 5, main_sha)
        duplicate_pages = self.fixture(main_sha, runs={main_sha: {"__pages__": [
            run_page(duplicate_id, total_count=2),
            run_page(duplicate_id, total_count=2),
        ]}})
        result = self.invoke(duplicate_pages)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state_path.read_bytes(), before)

    def test_complete_subcap_result_is_accepted(self):
        main_sha = sha(15)
        runs = [workflow_run(1000 + number, number, main_sha) for number in range(1, 1000)]
        fixture = self.fixture(main_sha, runs={main_sha: run_page(*runs, total_count=999)})
        result = self.invoke(fixture)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(len(self.state_json()["seen"]), 1)

    def test_first_api_failure_does_not_create_state(self):
        main_sha = sha(11)
        fixture = self.fixture(main_sha, fail_routes=["repos/test/repo/pulls"])
        failed = self.invoke(fixture)
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse((self.state / "state.json").exists())

    def test_exclusive_lock_prevents_api_calls_and_state_update(self):
        lock_path = self.state / "intake.lock"
        with lock_path.open("w", encoding="utf-8") as lock:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            main_sha = sha(12)
            result = self.invoke(self.fixture(main_sha))
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.calls(), [])
            self.assertFalse((self.state / "state.json").exists())

    def test_relative_or_repository_local_state_directory_is_rejected(self):
        environment = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ.get('PATH', '')}",
            "GH_FIXTURE": str(self.fixture_path),
            "GH_CALL_LOG": str(self.call_log),
        }
        relative = subprocess.run(
            [sys.executable, str(SCRIPT), "--repo", "test/repo", "--state-dir", "relative"],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
        self.assertNotEqual(relative.returncode, 0)
        local = subprocess.run(
            [sys.executable, str(SCRIPT), "--repo", "test/repo", "--state-dir", str(SCRIPT.parents[1] / "target" / "intake-test")],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )
        self.assertNotEqual(local.returncode, 0)
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
