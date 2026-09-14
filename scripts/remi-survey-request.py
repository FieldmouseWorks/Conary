#!/usr/bin/env python3
# scripts/remi-survey-request.py

"""Build one strict version1 resolution-survey request for the protected survey workflow.

The request selects exactly one operator action:

* ``survey`` derives the survey identity for one fresh resolution survey whose
  operator is the current protected workflow dispatch.
* ``recover`` derives the survey identity for standalone reconstruction of one
  original failed or cancelled survey run, plus the retained-run provenance
  that re-verifies that original run.

This module owns neither Git authority nor artifact authentication. It never
invokes a helper or service, never derives or accepts an artifact digest, and
never inspects the local checkout. The protected survey workflow separately
verifies that the returned ``chain_workflow_commit_sha`` is an ancestor of
current protected ``main``; downstream ``build-input`` reauthentication must
reconstruct the exact source manifest that one recovery helper has to match.
A source run without an artifact stays legitimate: this module neither invents
nor requires a source artifact.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import stat
from typing import Any, NoReturn


SCHEMA_VERSION = 1
MAX_METADATA_BYTES = 16 * 1024 * 1024
U64_MAX = 2**64 - 1
CANONICAL_DECIMAL = re.compile(r"[1-9][0-9]*")
COMMIT = re.compile(r"[0-9a-f]{40}")
IDENTITY = re.compile(r"[a-z0-9][a-z0-9._-]{0,127}")
REPOSITORY_SEGMENT = re.compile(r"[A-Za-z0-9_.-]+")
SURVEY_WORKFLOW_PATH = ".github/workflows/survey-remi-resolution.yml"
SURVEY_WORKFLOW_EVENT = "workflow_dispatch"
SURVEY_BRANCH = "main"
RECOVERY_CONCLUSIONS = ("failure", "cancelled")
OPERATIONS = ("survey", "recover")


class ValidationError(ValueError):
    """An argument, metadata document, or derived request differs from the contract."""


def fail(message: str) -> NoReturn:
    raise ValidationError(message)


def require_canonical_decimal(value: Any, label: str) -> int:
    if not isinstance(value, str) or CANONICAL_DECIMAL.fullmatch(value) is None:
        fail(f"{label} must be one canonical positive decimal identity")
    if len(value) > 20:
        fail(f"{label} exceeds the unsigned 64-bit range")
    number = int(value)
    if number > U64_MAX:
        fail(f"{label} exceeds the unsigned 64-bit range")
    return number


def require_commit(value: Any, label: str) -> str:
    if not isinstance(value, str) or COMMIT.fullmatch(value) is None:
        fail(f"{label} must be one full lowercase 40-character commit digest")
    return value


def require_repository(value: Any, label: str) -> str:
    segments = value.split("/") if isinstance(value, str) else []
    if len(segments) != 2 or any(
        REPOSITORY_SEGMENT.fullmatch(segment) is None for segment in segments
    ):
        fail(f"{label} must be exactly one nonempty ASCII owner/name repository")
    if any(set(segment) == {"."} for segment in segments):
        fail(f"{label} must not carry a traversal or degenerate segment")
    return value


def require_identity(value: Any, label: str) -> str:
    if not isinstance(value, str) or IDENTITY.fullmatch(value) is None:
        fail(
            f"{label} must start with a lowercase ASCII letter or digit and carry "
            "at most 128 characters of lowercase letters, digits, '.', '_', or '-'"
        )
    return value


def exact_positive_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 < value <= U64_MAX:
        fail(f"{label} must be one unsigned 64-bit integer, never a bool, float, or string")
    return value


def require_text(value: Any, expected: str, label: str) -> str:
    if not isinstance(value, str) or value != expected:
        fail(f"{label} must be exactly {expected!r}")
    return value


def reject_duplicate_key(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            fail(f"retained run metadata repeats JSON key {key!r}")
        value[key] = item
    return value


def reject_constant(token: str) -> NoReturn:
    fail(f"retained run metadata carries the nonfinite JSON constant {token!r}")


def read_metadata(path: Path) -> dict[str, Any]:
    label = "retained run metadata"
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ValidationError(f"cannot inspect {label} {path}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
        fail(f"{label} {path} must be a regular file, never a symlink")
    if metadata.st_size <= 0 or metadata.st_size > MAX_METADATA_BYTES:
        fail(f"{label} {path} size is outside its bounded contract")
    data = path.read_bytes()
    if len(data) != metadata.st_size:
        fail(f"{label} {path} changed while being read")
    value = json.loads(
        data,
        object_pairs_hook=reject_duplicate_key,
        parse_constant=reject_constant,
    )
    if not isinstance(value, dict):
        fail(f"{label} {path} must be one JSON object")
    return value


def nested_object(value: dict[str, Any], key: str, label: str) -> dict[str, Any]:
    item = value.get(key)
    if not isinstance(item, dict):
        fail(f"{label} must be one JSON object")
    return item


def validate_recovery_source(
    metadata: dict[str, Any],
    repository: str,
    source_run_id: int,
    source_attempt: int,
) -> tuple[str, str]:
    """Return the validated ``(conclusion, head_sha)`` of one original survey run."""

    observed_id = exact_positive_int(metadata.get("id"), "retained run metadata id")
    if observed_id != source_run_id:
        fail("retained run metadata id does not match the requested source run ID")
    observed_attempt = exact_positive_int(
        metadata.get("run_attempt"), "retained run metadata run_attempt"
    )
    if observed_attempt != source_attempt:
        fail("retained run metadata run_attempt does not match the requested attempt")
    require_text(metadata.get("event"), SURVEY_WORKFLOW_EVENT, "retained run metadata event")
    require_text(metadata.get("status"), "completed", "retained run metadata status")
    conclusion = metadata.get("conclusion")
    if not isinstance(conclusion, str) or conclusion not in RECOVERY_CONCLUSIONS:
        fail("retained run metadata conclusion must be exactly 'failure' or 'cancelled'")
    require_text(metadata.get("head_branch"), SURVEY_BRANCH, "retained run metadata head_branch")
    require_text(metadata.get("path"), SURVEY_WORKFLOW_PATH, "retained run metadata path")
    require_text(
        nested_object(metadata, "repository", "retained run metadata repository").get(
            "full_name"
        ),
        repository,
        "retained run metadata repository.full_name",
    )
    require_text(
        nested_object(
            metadata, "head_repository", "retained run metadata head_repository"
        ).get("full_name"),
        repository,
        "retained run metadata head_repository.full_name",
    )
    head_sha = require_commit(metadata.get("head_sha"), "retained run metadata head_sha")
    return conclusion, head_sha


def build_request(arguments: argparse.Namespace) -> dict[str, Any]:
    repository = require_repository(arguments.repository, "repository")
    workflow_commit = require_commit(arguments.workflow_commit, "workflow commit")
    workflow_run_id = require_canonical_decimal(
        arguments.workflow_run_id, "workflow run ID"
    )
    workflow_run_attempt = require_canonical_decimal(
        arguments.workflow_run_attempt, "workflow run attempt"
    )
    oracle_run_id = require_canonical_decimal(arguments.oracle_run_id, "oracle run ID")
    retained = arguments.retained_survey_run_id
    retained_attempt = arguments.retained_survey_run_attempt

    if arguments.operation == "survey":
        if retained:
            fail("a normal survey request must not retain a survey run ID")
        if arguments.retained_run is not None:
            fail("a normal survey request must not carry retained run metadata")
        if retained_attempt != "1":
            fail("a normal survey request must use the canonical retained attempt 1")
        survey_id = f"survey-{oracle_run_id}-{workflow_run_id}-{workflow_run_attempt}"
        chain_workflow_commit_sha = workflow_commit
        retained_survey: dict[str, Any] | None = None
    elif arguments.operation == "recover":
        source_run_id = require_canonical_decimal(retained, "retained survey run ID")
        source_attempt = require_canonical_decimal(
            retained_attempt, "retained survey run attempt"
        )
        if arguments.retained_run is None:
            fail("recovery requires retained run metadata as a JSON file")
        if source_run_id == workflow_run_id:
            fail("recovery source run must differ from the current workflow run")
        conclusion, source_head = validate_recovery_source(
            read_metadata(arguments.retained_run),
            repository,
            source_run_id,
            source_attempt,
        )
        survey_id = f"survey-{oracle_run_id}-{source_run_id}-{source_attempt}"
        chain_workflow_commit_sha = source_head
        retained_survey = {
            "workflow_run_id": source_run_id,
            "workflow_run_attempt": source_attempt,
            "workflow_commit_sha": source_head,
            "conclusion": conclusion,
        }
    else:
        fail("operation must be exactly 'survey' or 'recover'")

    require_identity(survey_id, "derived survey ID")
    return {
        "schema_version": SCHEMA_VERSION,
        "operation": arguments.operation,
        "operator": {
            "workflow_commit_sha": workflow_commit,
            "workflow_run_id": workflow_run_id,
            "workflow_run_attempt": workflow_run_attempt,
        },
        "oracle_run_id": oracle_run_id,
        "survey_id": survey_id,
        "chain_workflow_commit_sha": chain_workflow_commit_sha,
        "retained_survey": retained_survey,
    }


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def write_request(path: Path, request: dict[str, Any]) -> None:
    data = canonical_json(request) + b"\n"
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    except FileExistsError as error:
        raise ValidationError(
            f"output {path} already exists; request output is create-only"
        ) from error
    except OSError as error:
        raise ValidationError(f"cannot create output {path}: {error}") from error
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
    except OSError as error:
        try:
            path.unlink()
        except OSError:
            pass
        raise ValidationError(f"cannot write output {path}: {error}") from error


def parse_arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="build one strict version1 Remi resolution-survey request",
        allow_abbrev=False,
    )
    parser.add_argument("--operation", choices=OPERATIONS, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--workflow-commit", required=True)
    parser.add_argument("--workflow-run-id", required=True)
    parser.add_argument("--workflow-run-attempt", required=True)
    parser.add_argument("--oracle-run-id", required=True)
    parser.add_argument("--retained-survey-run-id", default="")
    parser.add_argument("--retained-survey-run-attempt", default="1")
    parser.add_argument("--retained-run", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> None:
    arguments = parse_arguments(argv)
    try:
        request = build_request(arguments)
        write_request(arguments.output, request)
    except (OSError, ValueError) as error:
        raise SystemExit(f"remi survey request rejected: {error}") from error


if __name__ == "__main__":
    main()
