#!/usr/bin/env python3
# scripts/test-remi-survey-request.py

"""Focused mutation tests for the Remi resolution-survey request contract."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
TOOL = REPO_ROOT / "scripts" / "remi-survey-request.py"
sys.dont_write_bytecode = True
TOOL_SPEC = importlib.util.spec_from_file_location("remi_survey_request", TOOL)
assert TOOL_SPEC is not None and TOOL_SPEC.loader is not None
REQUEST_TOOL = importlib.util.module_from_spec(TOOL_SPEC)
TOOL_SPEC.loader.exec_module(REQUEST_TOOL)

REPOSITORY = "FieldmouseWorks/Conary"
WORKFLOW_COMMIT = "a" * 40
SOURCE_HEAD = "b" * 40
ORACLE_RUN_ID = "300"
WORKFLOW_RUN_ID = "500"
WORKFLOW_RUN_ATTEMPT = "2"
SOURCE_RUN_ID = "400"
SOURCE_ATTEMPT = "3"
U64_MAX_TEXT = "18446744073709551615"
SURVEY_WORKFLOW_PATH = ".github/workflows/survey-remi-resolution.yml"


def canonical(value: object) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def run_metadata() -> dict[str, object]:
    return {
        "id": int(SOURCE_RUN_ID),
        "run_attempt": int(SOURCE_ATTEMPT),
        "event": "workflow_dispatch",
        "status": "completed",
        "conclusion": "failure",
        "head_branch": "main",
        "path": SURVEY_WORKFLOW_PATH,
        "repository": {"id": 1, "full_name": REPOSITORY},
        "head_repository": {"id": 1, "full_name": REPOSITORY},
        "head_sha": SOURCE_HEAD,
    }


class SurveyRequestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.counter = 0

    def base(self, operation: str = "survey") -> list[str]:
        return [
            "--operation", operation,
            "--repository", REPOSITORY,
            "--workflow-commit", WORKFLOW_COMMIT,
            "--workflow-run-id", WORKFLOW_RUN_ID,
            "--workflow-run-attempt", WORKFLOW_RUN_ATTEMPT,
            "--oracle-run-id", ORACLE_RUN_ID,
        ]

    def invoke(
        self, arguments: list[str], output: Path | None = None
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        self.counter += 1
        target = (
            output
            if output is not None
            else self.root / f"survey-request-{self.counter}.json"
        )
        result = subprocess.run(
            [sys.executable, str(TOOL), *arguments, "--output", str(target)],
            capture_output=True,
            text=True,
            check=False,
        )
        return result, target

    def reject(
        self, arguments: list[str], output: Path | None = None, contract: bool = True
    ) -> subprocess.CompletedProcess[str]:
        result, target = self.invoke(arguments, output)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        if contract:
            self.assertIn("remi survey request rejected:", result.stderr)
        self.assertFalse(target.exists(), "a refused request must not create its output")
        return result

    def metadata_file(self, value: object, name: str = "retained-run.json") -> Path:
        path = self.root / name
        path.write_bytes(canonical(value))
        return path

    def raw_metadata_file(self, data: bytes, name: str = "retained-run.json") -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def recovery(
        self,
        metadata: object | None = None,
        *,
        source_id: str = SOURCE_RUN_ID,
        attempt: str = SOURCE_ATTEMPT,
        metadata_path: Path | None = None,
    ) -> list[str]:
        arguments = self.base("recover") + [
            "--retained-survey-run-id", source_id,
            "--retained-survey-run-attempt", attempt,
        ]
        if metadata_path is None and metadata is not None:
            metadata_path = self.metadata_file(metadata)
        if metadata_path is not None:
            arguments += ["--retained-run", str(metadata_path)]
        return arguments

    def test_survey_request_is_exact_canonical_version1(self) -> None:
        result, target = self.invoke(self.base("survey"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        expected = {
            "schema_version": 1,
            "operation": "survey",
            "operator": {
                "workflow_commit_sha": WORKFLOW_COMMIT,
                "workflow_run_id": int(WORKFLOW_RUN_ID),
                "workflow_run_attempt": int(WORKFLOW_RUN_ATTEMPT),
            },
            "oracle_run_id": int(ORACLE_RUN_ID),
            "survey_id": f"survey-{ORACLE_RUN_ID}-{WORKFLOW_RUN_ID}-{WORKFLOW_RUN_ATTEMPT}",
            "chain_workflow_commit_sha": WORKFLOW_COMMIT,
            "retained_survey": None,
        }
        data = target.read_bytes()
        self.assertEqual(data, canonical(expected) + b"\n")
        parsed = json.loads(data)
        self.assertEqual(set(parsed), set(expected))
        self.assertEqual(set(parsed["operator"]), set(expected["operator"]))
        for value in (
            parsed["schema_version"],
            parsed["oracle_run_id"],
            parsed["operator"]["workflow_run_id"],
            parsed["operator"]["workflow_run_attempt"],
        ):
            self.assertIs(type(value), int)
            self.assertNotIsInstance(value, bool)

    def test_survey_accepts_explicit_default_retained_attempt(self) -> None:
        arguments = self.base("survey") + [
            "--retained-survey-run-id", "",
            "--retained-survey-run-attempt", "1",
        ]
        result, target = self.invoke(arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(target.read_bytes())["survey_id"],
            f"survey-{ORACLE_RUN_ID}-{WORKFLOW_RUN_ID}-{WORKFLOW_RUN_ATTEMPT}",
        )

    def test_survey_rejects_retained_run_id_metadata_and_attempt(self) -> None:
        metadata = self.metadata_file(run_metadata())
        cases = [
            ["--retained-survey-run-id", SOURCE_RUN_ID],
            ["--retained-run", str(metadata)],
            ["--retained-run", str(self.root / "absent.json")],
        ]
        cases += [
            ["--retained-survey-run-attempt", attempt]
            for attempt in ("2", "00", "01", "0", "", " 1", "1 ", "١")
        ]
        for extra in cases:
            with self.subTest(extra=extra):
                self.reject(self.base("survey") + extra)

    def test_recovery_failure_is_exact_canonical_request(self) -> None:
        result, target = self.invoke(self.recovery(run_metadata()))
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = {
            "schema_version": 1,
            "operation": "recover",
            "operator": {
                "workflow_commit_sha": WORKFLOW_COMMIT,
                "workflow_run_id": int(WORKFLOW_RUN_ID),
                "workflow_run_attempt": int(WORKFLOW_RUN_ATTEMPT),
            },
            "oracle_run_id": int(ORACLE_RUN_ID),
            "survey_id": f"survey-{ORACLE_RUN_ID}-{SOURCE_RUN_ID}-{SOURCE_ATTEMPT}",
            "chain_workflow_commit_sha": SOURCE_HEAD,
            "retained_survey": {
                "workflow_run_id": int(SOURCE_RUN_ID),
                "workflow_run_attempt": int(SOURCE_ATTEMPT),
                "workflow_commit_sha": SOURCE_HEAD,
                "conclusion": "failure",
            },
        }
        data = target.read_bytes()
        self.assertEqual(data, canonical(expected) + b"\n")
        parsed = json.loads(data)
        for value in (
            parsed["retained_survey"]["workflow_run_id"],
            parsed["retained_survey"]["workflow_run_attempt"],
        ):
            self.assertIs(type(value), int)

    def test_recovery_cancelled_is_accepted(self) -> None:
        metadata = run_metadata()
        metadata["conclusion"] = "cancelled"
        result, target = self.invoke(self.recovery(metadata))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(target.read_bytes())["retained_survey"]["conclusion"], "cancelled"
        )

    def test_recovery_ignores_unrelated_github_api_fields(self) -> None:
        metadata = run_metadata()
        metadata.update(
            {
                "display_title": "survey exact production candidate resolution",
                "run_number": 12,
                "workflow_id": 99,
                "html_url": f"https://github.com/{REPOSITORY}/actions/runs/{SOURCE_RUN_ID}",
                "created_at": "2026-09-13T08:00:00Z",
                "updated_at": "2026-09-13T09:00:00Z",
                "actor": {"login": "operator"},
                "head_commit": {"id": SOURCE_HEAD, "message": "recover"},
                "pull_requests": [],
                "artifacts_url": "https://api.github.com/repos/x/y/actions/runs/1/artifacts",
            }
        )
        result, _ = self.invoke(self.recovery(metadata))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_recovery_requires_metadata_document(self) -> None:
        self.reject(self.recovery(metadata_path=None))
        self.reject(self.recovery(metadata_path=self.root / "absent.json"))
        self.reject(self.recovery(metadata_path=self.root))
        link = self.root / "linked.json"
        link.symlink_to(self.metadata_file(run_metadata(), name="real.json"))
        self.reject(self.recovery(metadata_path=link))

    def test_recovery_rejects_source_equal_to_current_run(self) -> None:
        metadata = run_metadata()
        metadata["id"] = int(WORKFLOW_RUN_ID)
        self.reject(self.recovery(metadata, source_id=WORKFLOW_RUN_ID))

    def test_recovery_rejects_noncanonical_source_identities(self) -> None:
        metadata = self.metadata_file(run_metadata())
        for source_id in (
            "", "0", "007", "1_0", "1e2", "+400", "-400", " 400", "400 ",
            "٣٠٠", "18446744073709551616", "9" * 21,
        ):
            with self.subTest(source_id=source_id):
                self.reject(
                    self.recovery(source_id=source_id, metadata_path=metadata)
                )
        for attempt in ("", "0", "01", "3.0", "٣", "18446744073709551616"):
            with self.subTest(attempt=attempt):
                self.reject(self.recovery(attempt=attempt, metadata_path=metadata))

    def test_recovery_metadata_id_and_attempt_must_be_exact_integers(self) -> None:
        for key, value in (
            ("id", 401), ("id", True), ("id", False), ("id", 400.0),
            ("id", "400"), ("id", None), ("id", [400]),
            ("run_attempt", 4), ("run_attempt", True), ("run_attempt", 3.0),
            ("run_attempt", "3"), ("run_attempt", None),
        ):
            metadata = run_metadata()
            metadata[key] = value
            with self.subTest(key=key, value=value):
                self.reject(self.recovery(metadata))
        for key in ("id", "run_attempt", "event", "status", "conclusion", "head_branch",
                    "path", "repository", "head_repository", "head_sha"):
            metadata = run_metadata()
            del metadata[key]
            with self.subTest(missing=key):
                self.reject(self.recovery(metadata))

    def test_recovery_rejects_wrong_run_state_and_provenance(self) -> None:
        cases: list[tuple[str, object]] = [
            ("event", "schedule"), ("event", "workflow_dispatch "), ("event", None),
            ("status", "in_progress"), ("status", "queued"), ("status", "completed "),
            ("status", None),
            ("conclusion", "success"), ("conclusion", "timed_out"),
            ("conclusion", "action_required"), ("conclusion", ""), ("conclusion", None),
            ("conclusion", 5),
            ("head_branch", "recovery"), ("head_branch", "MAIN"), ("head_branch", None),
            ("path", ".github/workflows/other.yml"),
            ("path", SURVEY_WORKFLOW_PATH + " "),
            ("path", ".github/workflows/survey-remi-resolution.yaml"),
            ("path", None),
            ("head_sha", SOURCE_HEAD.upper()), ("head_sha", "b" * 39),
            ("head_sha", "b" * 41), ("head_sha", "g" * 40), ("head_sha", ""),
            ("head_sha", None),
        ]
        for key, value in cases:
            metadata = run_metadata()
            metadata[key] = value
            with self.subTest(key=key, value=value):
                self.reject(self.recovery(metadata))

    def test_recovery_rejects_wrong_repository_provenance(self) -> None:
        for key in ("repository", "head_repository"):
            for value in (
                {"full_name": "Other/Repository"},
                {"full_name": REPOSITORY + "/"},
                {"id": 1},
                {"full_name": None},
                "FieldmouseWorks/Conary",
            ):
                metadata = run_metadata()
                metadata[key] = value
                with self.subTest(key=key, value=value):
                    self.reject(self.recovery(metadata))

    def test_recovery_metadata_document_must_be_strict_json_object(self) -> None:
        for raw in (b"", b"not json", b"[]", b'"text"', b"null", b"400", b"{}"):
            with self.subTest(raw=raw):
                self.reject(
                    self.recovery(
                        metadata_path=self.raw_metadata_file(raw, name="raw-%d.json" % len(raw))
                    )
                )

    def test_recovery_rejects_duplicate_json_keys_and_nonfinite_constants(self) -> None:
        base = canonical(run_metadata())
        duplicate = base[:-1] + b',"status":"completed"}'
        self.assertIn(b'"status":"completed"', duplicate)
        self.reject(
            self.recovery(metadata_path=self.raw_metadata_file(duplicate, name="dup.json"))
        )
        for label, replacement in (
            ("nan", base.replace(b'"conclusion":"failure"', b'"conclusion":NaN')),
            ("id-infinity", base.replace(b'"id":400', b'"id":Infinity')),
            ("attempt-negative-infinity", base.replace(
                b'"run_attempt":3', b'"run_attempt":-Infinity'
            )),
        ):
            with self.subTest(constant=label):
                self.reject(
                    self.recovery(
                        metadata_path=self.raw_metadata_file(
                            replacement, name=f"constant-{label}.json"
                        )
                    )
                )

    def test_unknown_operation_is_rejected(self) -> None:
        for operation in ("inspect", "SURVEY", "recover "):
            with self.subTest(operation=operation):
                self.reject(
                    [argument if argument != "survey" else operation
                     for argument in self.base("survey")],
                    contract=False,
                )

    def test_workflow_identity_rejections(self) -> None:
        for run_id in (
            "", "0", "01", "-1", "1e3", "1 000", "500 ", " 500", "١٠", "500.0",
            "18446744073709551616", "9" * 21,
        ):
            arguments = [
                value if value != WORKFLOW_RUN_ID else run_id for value in self.base()
            ]
            with self.subTest(run_id=run_id):
                self.reject(arguments)
        for attempt in ("", "0", "01", "2.0", "٢"):
            arguments = [
                value if value != WORKFLOW_RUN_ATTEMPT else attempt
                for value in self.base()
            ]
            with self.subTest(attempt=attempt):
                self.reject(arguments)
        for oracle in ("", "0", "0300", "3e2", "三百"):
            arguments = [
                value if value != ORACLE_RUN_ID else oracle for value in self.base()
            ]
            with self.subTest(oracle=oracle):
                self.reject(arguments)

    def test_u64_ceiling_is_accepted_and_overflow_is_rejected(self) -> None:
        arguments = [
            value if value != WORKFLOW_RUN_ID else U64_MAX_TEXT for value in self.base()
        ]
        result, target = self.invoke(arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        request = json.loads(target.read_bytes())
        self.assertEqual(request["operator"]["workflow_run_id"], 2**64 - 1)
        self.assertEqual(
            request["survey_id"],
            f"survey-{ORACLE_RUN_ID}-{U64_MAX_TEXT}-{WORKFLOW_RUN_ATTEMPT}",
        )

    def test_workflow_commit_rejections(self) -> None:
        for commit in ("", "a" * 39, "a" * 41, "A" * 40, "g" * 40, " a" * 40):
            arguments = [
                value if value != WORKFLOW_COMMIT else commit for value in self.base()
            ]
            with self.subTest(commit=commit):
                self.reject(arguments)

    def test_repository_acceptance_and_rejections(self) -> None:
        arguments = [
            value if value != REPOSITORY else "Owner_1/Repo.js-x" for value in self.base()
        ]
        result, _ = self.invoke(arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        for repository in (
            "", "Conary", "/Conary", "Conary/", "a/b/c", "owner/..", "owner/.",
            "owner/...", "own er/name", "owner/naïve", "owner/na?me", "owner/name/",
            "owner\\name",
        ):
            arguments = [
                value if value != REPOSITORY else repository for value in self.base()
            ]
            with self.subTest(repository=repository):
                self.reject(arguments)

    def test_output_is_create_only(self) -> None:
        target = self.root / "survey-request.json"
        target.write_bytes(b"sentinel\n")
        result, _ = self.invoke(self.base("survey"), output=target)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("remi survey request rejected:", result.stderr)
        self.assertIn("create-only", result.stderr)
        self.assertEqual(target.read_bytes(), b"sentinel\n")

    def test_missing_output_directory_creates_nothing(self) -> None:
        target = self.root / "missing" / "survey-request.json"
        result, _ = self.invoke(self.base("survey"), output=target)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(target.exists())


class SurveyRequestHelperTests(unittest.TestCase):
    def test_derived_identity_bound(self) -> None:
        self.assertEqual(
            REQUEST_TOOL.require_identity("survey-300-500-1", "survey id"),
            "survey-300-500-1",
        )
        with self.assertRaises(REQUEST_TOOL.ValidationError):
            REQUEST_TOOL.require_identity("a" * 129, "survey id")
        with self.assertRaises(REQUEST_TOOL.ValidationError):
            REQUEST_TOOL.require_identity("Survey-300-500-1", "survey id")
        with self.assertRaises(REQUEST_TOOL.ValidationError):
            REQUEST_TOOL.require_identity("survey/300", "survey id")

    def test_canonical_decimal_helper(self) -> None:
        self.assertEqual(REQUEST_TOOL.require_canonical_decimal("1", "run id"), 1)
        self.assertEqual(
            REQUEST_TOOL.require_canonical_decimal(U64_MAX_TEXT, "run id"), 2**64 - 1
        )
        for value in ("", "0", "01", "1_0", "١٢٣", "1.0", "18446744073709551616"):
            with self.subTest(value=value):
                with self.assertRaises(REQUEST_TOOL.ValidationError):
                    REQUEST_TOOL.require_canonical_decimal(value, "run id")
        with self.assertRaises(REQUEST_TOOL.ValidationError):
            REQUEST_TOOL.require_canonical_decimal(1, "run id")

    def test_repository_and_commit_helpers(self) -> None:
        self.assertEqual(
            REQUEST_TOOL.require_repository("Owner_1/Repo.js-x", "repository"),
            "Owner_1/Repo.js-x",
        )
        for value in ("owner/..", "owner/...", "owner/.", "a/b/c", "/x", "x/"):
            with self.subTest(value=value):
                with self.assertRaises(REQUEST_TOOL.ValidationError):
                    REQUEST_TOOL.require_repository(value, "repository")
        self.assertEqual(REQUEST_TOOL.require_commit("a" * 40, "commit"), "a" * 40)
        for value in ("A" * 40, "a" * 39, "a" * 41, None):
            with self.subTest(value=value):
                with self.assertRaises(REQUEST_TOOL.ValidationError):
                    REQUEST_TOOL.require_commit(value, "commit")


if __name__ == "__main__":
    unittest.main()
