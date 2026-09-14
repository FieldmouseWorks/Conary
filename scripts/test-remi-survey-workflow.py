#!/usr/bin/env python3
# scripts/test-remi-survey-workflow.py

"""Execute the actual survey workflow shell with confined transport doubles."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "conversion_policy", ROOT / "scripts/check-remi-conversion-workflow.py"
)
assert spec and spec.loader
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
WORKFLOW = policy.yaml.load(
    (ROOT / ".github/workflows/survey-remi-resolution.yml").read_text(),
    Loader=policy.GitHubWorkflowLoader,
)
STEPS = {step["name"]: step for step in WORKFLOW["jobs"]["survey"]["steps"]}
HELPER_STEP = STEPS["Run through the fixed production helper"]
REQUEST_STEP = STEPS["Bind the exact survey or retained recovery request"]
SHA = "a" * 40
OLD_SHA = "b" * 40
DIGEST = "c" * 64
SURVEY = "survey-11-22-1"
EXPORT = "slice6-33-44-1"

# These doubles model only external process boundaries. The workflow's shell,
# traps, jq predicates, timing, and artifact receipts execute without rewriting.
DOUBLE = r'''
import json
import os
from pathlib import Path
import sys

def record_failure(kind, value, traceback):
    with open("double-failures.txt", "a") as failures:
        failures.write("%s: %s\n" % (kind.__name__, value))
    sys.__excepthook__(kind, value, traceback)

sys.excepthook = record_failure
name = Path(sys.argv[0]).name
args = sys.argv[1:]
with open("calls.jsonl", "a") as log:
    log.write(json.dumps([name, args]) + "\n")
scenario = os.environ["SCENARIO"]
if name == "git":
    if args[0] == "show":
        sys.stdout.buffer.write((Path(os.environ["SOURCE_ROOT"]) / "deploy/remi-deploy-helper.sh").read_bytes())
    elif args[0] == "rev-parse":
        print("f" * 40 if scenario == "main_advanced" else os.environ["WORKFLOW_SHA"])
    elif args[0] == "merge-base":
        sys.exit(1 if scenario == "unmerged_source" else 0)
    else:
        assert args == ["fetch", "--no-tags", "origin", "main"], args
elif name in {"ssh", "scp"}:
    assert args[:2] == ["-F", os.environ["REMI_SSH_CONFIG"]], args
    assert Path(os.environ["REMI_SSH_KEY_PATH"]).is_file(), "SSH key deleted before remote cleanup"
    assert Path(os.environ["REMI_SSH_KNOWN_HOSTS_PATH"]).is_file(), "host pins deleted before remote cleanup"
    if name == "scp":
        if scenario == "stage_failed":
            sys.exit(6)
        if os.environ["OPERATION"] == "recover":
            assert "remi-resolution-survey-helper-" in args[-1], args
    else:
        command = args[-1]
        if " export-resolution-survey-evidence " in command:
            assert command.endswith("'%s' '%s' '%s'" % (
                os.environ["SURVEY_ID"], os.environ["EXPORT_ID"], "c" * 64)), command
            if scenario == "export_failed":
                print("private recovery transport failure", file=sys.stderr)
                sys.exit(8)
            sys.stdout.buffer.write(b"authenticated transport fixture")
        elif " survey-resolution " in command:
            assert os.environ["OPERATION"] == "survey", "recovery invoked a survey"
            sys.exit(7)
        elif command.startswith("rm -f -- "):
            if os.environ["OPERATION"] == "recover":
                expected = "/tmp/remi-resolution-survey-helper-%s-%s-%s" % (
                    os.environ["SURVEY_ID"], os.environ["GITHUB_RUN_ID"], os.environ["GITHUB_RUN_ATTEMPT"])
                assert command == "rm -f -- '%s'" % expected, "recovery deleted original transports"
        else:
            assert command.startswith("sudo -n /usr/local/sbin/conary-remi-deploy "), command
            assert command.split()[3] in {"verify-access", "install-helper"}, command
elif name == "python3":
    if args[0] == "scripts/remi-survey-request.py":
        os.execv(os.environ["REAL_PYTHON"], [os.environ["REAL_PYTHON"],
            str(Path(os.environ["SOURCE_ROOT"]) / args[0]), *args[1:]])
    assert args[:2] == ["scripts/remi-resolution-survey-transport.py", "verify-recovery"], args
    flags = dict(zip(args[2::2], args[3::2], strict=True))
    assert flags == {
        "--survey-id": os.environ["SURVEY_ID"], "--export-id": os.environ["EXPORT_ID"],
        "--input-evidence": "resolution-survey-input-verification.json",
        "--transport": str(Path(os.environ["RUNNER_TEMP"]) / "resolution-survey-recovery.tar"),
        "--output": "resolution-survey-recovery",
    }, flags
    assert json.loads(Path(flags["--input-evidence"]).read_text())["manifest_sha256"] == "c" * 64
    assert Path(flags["--transport"]).read_bytes() == b"authenticated transport fixture"
    if scenario == "verification_failed":
        sys.exit(9)
    output = Path(flags["--output"])
    output.mkdir()
    evidence = {"authority": "diagnostic_only", "availability": "retained", "input_binding": "verified"}
    if scenario == "not_retained":
        evidence["availability"] = "not_retained"
    elif scenario == "input_unbound":
        evidence["input_binding"] = "unavailable"
    elif scenario == "wrong_authority":
        evidence["authority"] = "survey"
    (output / "recovery-verification.json").write_text(json.dumps(evidence))
elif name == "gh":
    assert args == ["api", "-H", "X-GitHub-Api-Version: 2026-03-10",
        "repos/FieldmouseWorks/Conary/actions/runs/22/attempts/1"], args
    print(Path("source-run.json").read_text())
else:
    raise AssertionError(name)
'''


class SurveyWorkflowTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        runner = self.path / "runner"
        runner.mkdir()
        commands = self.path / "bin"
        commands.mkdir()
        for name in ("git", "ssh", "scp", "python3", "gh"):
            command = commands / name
            command.write_text(f"#!{sys.executable}\n" + DOUBLE)
            command.chmod(0o755)
        self.env = {
            **os.environ, "PATH": f"{commands}:{os.environ['PATH']}",
            "REAL_PYTHON": sys.executable, "SOURCE_ROOT": str(ROOT),
            "RUNNER_TEMP": str(runner), "WORKFLOW_SHA": SHA,
            "OPERATION": "recover", "ORACLE_RUN_ID": "11",
            "SURVEY_ID": SURVEY, "EXPORT_ID": EXPORT,
            "GITHUB_REPOSITORY": "FieldmouseWorks/Conary",
            "GITHUB_RUN_ID": "55", "GITHUB_RUN_ATTEMPT": "1",
            "RETAINED_SURVEY_RUN_ID": "22", "RETAINED_SURVEY_RUN_ATTEMPT": "1",
            "REMI_SSH_TARGET": "fixture", "SCENARIO": "success",
        }
        for variable in ("REMI_SSH_KEY_PATH", "REMI_SSH_KNOWN_HOSTS_PATH",
                         "REMI_SSH_CONFIG", "INPUT_TRANSPORT", "GITHUB_OUTPUT"):
            path = runner / variable.lower()
            path.write_text("")
            self.env[variable] = str(path)
        self.write_json("resolution-survey-input-verification.json", {"manifest_sha256": DIGEST})
        self.write_json("resolution-survey-request.json", {"survey_id": SURVEY})
        self.write_json("source-run.json", {
            "id": 22, "run_attempt": 1, "event": "workflow_dispatch",
            "status": "completed", "conclusion": "cancelled", "head_branch": "main",
            "path": ".github/workflows/survey-remi-resolution.yml", "head_sha": OLD_SHA,
            "repository": {"full_name": "FieldmouseWorks/Conary"},
            "head_repository": {"full_name": "FieldmouseWorks/Conary"},
        })

    def write_json(self, name, value):
        (self.path / name).write_text(json.dumps(value))

    def read_json(self, name):
        return json.loads((self.path / name).read_text())

    def run_step(self, step=HELPER_STEP, scenario="success"):
        self.env["SCENARIO"] = scenario
        result = subprocess.run(["bash", "-c", step["run"]], cwd=self.path,
                                env=self.env, capture_output=True, text=True, timeout=30)
        self.calls = [json.loads(line) for line in (self.path / "calls.jsonl").read_text().splitlines()]
        self.outputs = Path(self.env["GITHUB_OUTPUT"]).read_text()
        failures = self.path / "double-failures.txt"
        self.assertFalse(failures.exists(), failures.read_text() if failures.exists() else "")
        self.assertNotIn("AssertionError", result.stderr, result.stderr)
        return result

    def assert_export_count(self, expected):
        exports = [args for name, args in self.calls
                   if name == "ssh" and " export-resolution-survey-evidence " in args[-1]]
        self.assertEqual(len(exports), expected)

    def assert_cleaned(self):
        self.assertFalse(Path(self.env["REMI_SSH_KEY_PATH"]).exists())
        self.assertFalse(Path(self.env["REMI_SSH_KNOWN_HOSTS_PATH"]).exists())
        commands = [args[-1] for name, args in self.calls if name == "ssh"]
        self.assertTrue(commands[-1].startswith("rm -f -- "), commands)

    def test_recovery_returns_before_survey_and_preserves_source_transports(self):
        result = self.run_step()
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = self.read_json("resolution-survey-recovery-operator.json")
        self.assertEqual(receipt["service_operation"], "none")
        self.assertEqual(receipt["original_survey_outcome"], "unchanged")
        self.assertEqual(receipt["input_manifest_sha256"], DIGEST)
        timing = receipt["timing"]
        self.assertEqual(timing["outcome"], "retrieved")
        self.assertGreaterEqual(timing["export_over_ssh_ms"], 0)
        self.assertGreaterEqual(timing["independent_verification_ms"], 0)
        self.assertEqual(timing["elapsed_ms"], timing["export_over_ssh_ms"] + timing["independent_verification_ms"])
        self.assertEqual(self.outputs, "recovery_outcome=retrieved\n")
        self.assert_export_count(1)
        self.assert_cleaned()

    def test_rejected_recovery_never_retries_or_publishes_success(self):
        for scenario in ("export_failed", "verification_failed", "not_retained",
                         "input_unbound", "wrong_authority"):
            with self.subTest(scenario=scenario):
                # Each attempt gets a clean runner, as Actions does.
                case = SurveyWorkflowTest()
                case.setUp()
                try:
                    result = case.run_step(scenario=scenario)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("private recovery transport failure", result.stderr)
                    self.assertFalse((case.path / "resolution-survey-recovery-operator.json").exists())
                    self.assertEqual(case.outputs, "")
                    case.assert_export_count(1)
                    case.assert_cleaned()
                finally:
                    case.doCleanups()

    def test_stale_operator_fails_before_ssh(self):
        result = self.run_step(scenario="main_advanced")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(name in {"ssh", "scp"} for name, _ in self.calls))
        self.assert_export_count(0)

    def test_recovery_stage_failure_cleans_only_its_stage_without_export(self):
        result = self.run_step(scenario="stage_failed")
        self.assertNotEqual(result.returncode, 0)
        self.assert_export_count(0)
        self.assertEqual(self.outputs, "")
        self.assert_cleaned()

    def test_normal_survey_failure_retains_original_status_and_recovers_once(self):
        self.env["OPERATION"] = "survey"
        result = self.run_step()
        self.assertNotEqual(result.returncode, 0)
        evidence = self.read_json("resolution-survey-helper.json")
        self.assertEqual(evidence["status"], 7)
        self.assertEqual(evidence["outcome"], "helper_failed")
        self.assertEqual(evidence["recovery"], "retrieved")
        self.assertEqual(self.outputs, "helper_outcome=helper_failed\n")
        self.assert_export_count(1)
        self.assert_cleaned()

    def test_request_authenticates_exact_attempt_then_checks_ancestry(self):
        (self.path / "resolution-survey-request.json").unlink()
        result = self.run_step(REQUEST_STEP)
        self.assertEqual(result.returncode, 0, result.stderr)
        request = self.read_json("resolution-survey-request.json")
        self.assertEqual(request["chain_workflow_commit_sha"], OLD_SHA)
        self.assertEqual(request["survey_id"], SURVEY)
        self.assertIn(["git", ["merge-base", "--is-ancestor", OLD_SHA, "origin/main"]], self.calls)
        self.assertIn(f"chain_workflow_commit={OLD_SHA}\n", self.outputs)

    def test_unmerged_source_cannot_publish_request_outputs(self):
        (self.path / "resolution-survey-request.json").unlink()
        result = self.run_step(REQUEST_STEP, "unmerged_source")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.outputs, "")

    def test_normal_request_uses_current_operator_without_source_lookup(self):
        (self.path / "resolution-survey-request.json").unlink()
        self.env.update(OPERATION="survey", RETAINED_SURVEY_RUN_ID="")
        result = self.run_step(REQUEST_STEP)
        self.assertEqual(result.returncode, 0, result.stderr)
        request = self.read_json("resolution-survey-request.json")
        self.assertEqual(request["chain_workflow_commit_sha"], SHA)
        self.assertEqual(request["survey_id"], "survey-11-55-1")
        self.assertFalse(any(name == "gh" for name, _ in self.calls))

    def test_dispatch_and_output_authorities_are_separate(self):
        inputs = WORKFLOW["on"]["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["operation"]["options"], ["survey", "recover"])
        self.assertEqual(inputs["operation"]["default"], "survey")
        self.assertEqual(WORKFLOW["jobs"]["survey"]["timeout-minutes"], 360)
        for name in ("Independently reopen sanitized survey transport", "Upload exact resolution survey",
                     "Record resolution survey counts and histograms", "Require successful Remi restoration after survey upload"):
            self.assertEqual(STEPS[name]["if"], "steps.request.outputs.operation == 'survey'")
        upload = STEPS["Upload independently recovered retained survey"]
        self.assertEqual(upload["if"], "steps.request.outputs.operation == 'recover' && steps.survey.outputs.recovery_outcome == 'retrieved'")
        self.assertIn("resolution-survey-recovery-operator.json", upload["with"]["path"])
        self.assertIn("resolution-survey-recovery-timing.json", upload["with"]["path"])
        build = next(step for step in STEPS.values() if "remi-resolution-survey-transport.py build-input" in step.get("run", ""))
        self.assertEqual(build["env"]["CHAIN_WORKFLOW_SHA"], "${{ steps.request.outputs.chain_workflow_commit }}")
        self.assertEqual(build["env"]["REQUEST_SURVEY_ID"], "${{ steps.request.outputs.survey_id }}")
        self.assertIn('--workflow-commit "$CHAIN_WORKFLOW_SHA"', build["run"])
        self.assertIn('survey_id="$REQUEST_SURVEY_ID"', build["run"])


if __name__ == "__main__":
    unittest.main()
