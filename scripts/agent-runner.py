#!/usr/bin/env python3
"""Bounded, issue-backed agent run controller.

The local spec is an authorization and selection envelope. GitHub issue
comments own task state; the local journal is a recovery pointer, never an
alternative task graph. This file intentionally does not install a scheduler.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import time
import urllib.parse


SCHEMA_VERSION = 1
GITHUB_API_VERSION = "2026-03-10"
SHA = re.compile(r"[0-9a-f]{40}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{1,79}\Z")
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
LOGIN = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}\Z")
BRANCH = re.compile(r"agent/[A-Za-z0-9][A-Za-z0-9._/-]{1,100}\Z")
NODE_MARKER = re.compile(r"<!-- conary-agent-node:v1 (\{[^\n]*\}) -->")
RUN_MARKER = re.compile(r"<!-- conary-agent-run:v1 (\{[^\n]*\}) -->")
ISSUE_SOURCE_MARKER = re.compile(
    r"<!-- conary-agent-graph-source:v1 comment=([1-9][0-9]*) -->"
)
WRITE_NAMES = {"create_branch", "update_branch", "comment_issue", "create_draft_pr"}
RUN_PHASES = {"claimed", "candidate", "draft", "observed", "blocked"}
TERMINAL_CONCLUSIONS = {
    "success", "failure", "cancelled", "timed_out", "action_required",
    "neutral", "skipped", "stale", "startup_failure",
}
HOSTED_STATUSES = {"queued", "in_progress", "completed", "waiting", "pending", "requested"}
REASONING_EFFORTS = {"minimal", "low", "medium", "high", "xhigh", "max", "ultra"}
KNOWN_RULES = {"deletion", "non_fast_forward", "required_status_checks", "pull_request"}
RETRYABLE_REVIEW_RESULT_CLASSES = {"missing", "parse", "shape"}
MAX_REVIEW_RESULT_RETRIES = 1
MAX_CANDIDATE_FILE = 64 * 1024 * 1024
COMMON_CREDENTIAL = re.compile(
    rb"(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|"
    rb"sk-[A-Za-z0-9_-]{20,}|-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----)"
)


class RunnerError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def require(condition, code, message):
    if not condition:
        raise RunnerError(code, message)


def fields(value, required, optional=(), label="object"):
    require(isinstance(value, dict), "invalid_spec", f"{label} must be an object")
    missing = set(required) - set(value)
    extra = set(value) - set(required) - set(optional)
    require(not missing and not extra, "invalid_spec",
            f"{label} fields mismatch (missing={sorted(missing)}, extra={sorted(extra)})")


def positive(value, label, allow_zero=False):
    require(type(value) is int and value >= (0 if allow_zero else 1),
            "invalid_spec", f"{label} must be a positive integer")


def nonempty(value, label, max_length=4096):
    require(isinstance(value, str) and 0 < len(value) <= max_length and
            all(ch.isprintable() for ch in value), "invalid_spec",
            f"{label} must be a printable nonempty string")


def fullmatch(pattern, value, label):
    require(isinstance(value, str) and pattern.fullmatch(value) is not None,
            "invalid_spec", f"{label} has an invalid value")


def absolute_path(value, label):
    require(isinstance(value, str) and value.startswith("/") and "\x00" not in value,
            "invalid_spec", f"{label} must be an absolute path")
    require(os.path.normpath(value) == value, "invalid_spec",
            f"{label} must be normalized")


def canonical_bytes(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False).encode("utf-8")


def reject_duplicate_keys(pairs, *, code="invalid_spec"):
    result = {}
    for key, value in pairs:
        require(key not in result, code, "JSON has a duplicate object key")
        result[key] = value
    return result


def acceptance_sha256(task):
    """Bind the graph node to the selected command and mutation boundary."""
    value = {
        "checks": task["checks"],
        "allowed_paths": task["allowed_paths"],
        "prompt_sha256": task["prompt_sha256"],
    }
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def parse_expiry(value, label="authorization.expires_at"):
    nonempty(value, label, 40)
    require(value.endswith("Z"), "invalid_spec", f"{label} must use UTC Z")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise RunnerError("invalid_spec", f"{label} is invalid") from error
    return parsed


def validate_spec(data):
    fields(data, {"schema_version", "run_id", "repository", "issue_number",
                  "authorization", "exclusions", "queue", "models", "limits",
                  "required_checks", "worktree_root", "journal_dir", "controller_actor"},
           optional={"auth_file"}, label="run spec")
    require(type(data["schema_version"]) is int and data["schema_version"] == SCHEMA_VERSION,
            "invalid_spec", "unsupported run spec version")
    fullmatch(IDENTIFIER, data["run_id"], "run_id")
    fullmatch(REPO, data["repository"], "repository")
    fullmatch(LOGIN, data["controller_actor"], "controller_actor")
    positive(data["issue_number"], "issue_number")
    fields(data["authorization"], {"source", "expires_at", "remote_writes", "merge"},
           label="authorization")
    nonempty(data["authorization"]["source"], "authorization.source")
    require(datetime.now(timezone.utc) < parse_expiry(data["authorization"]["expires_at"]),
            "expired_authorization", "run authorization has expired")
    writes = data["authorization"]["remote_writes"]
    require(isinstance(writes, list) and
            all(isinstance(name, str) and name in WRITE_NAMES for name in writes) and
            len(writes) == len(set(writes)),
            "invalid_spec", "authorization.remote_writes is invalid")
    require(data["authorization"]["merge"] is False, "invalid_spec",
            "schema v1 cannot authorize merge")
    require(isinstance(data["exclusions"], list) and
            all(type(value) is int and value > 0 for value in data["exclusions"]) and
            len(set(data["exclusions"])) == len(data["exclusions"]),
            "invalid_spec", "exclusions must be unique issue numbers")
    require(data["issue_number"] not in data["exclusions"], "invalid_spec",
            "primary issue cannot be excluded")
    models = data["models"]
    fields(models, {"allowlist", "worker", "reviewer"}, label="models")
    require(isinstance(models["allowlist"], list) and models["allowlist"] and
            all(isinstance(model, str) for model in models["allowlist"]) and
            len(models["allowlist"]) == len(set(models["allowlist"])),
            "invalid_spec", "models.allowlist is invalid")
    for model in models["allowlist"]:
        nonempty(model, "model id", 100)
    for role in ("worker", "reviewer"):
        value = models[role]
        fields(value, {"id", "reasoning_effort"}, label=f"models.{role}")
        require(value["id"] in models["allowlist"], "invalid_spec",
                f"{role} model is outside the explicit allowlist")
        require(isinstance(value["reasoning_effort"], str) and
                value["reasoning_effort"] in REASONING_EFFORTS, "invalid_spec",
                f"{role} reasoning effort is unsupported")
    fields(data["limits"], {"wall_seconds", "causal_repairs"}, label="limits")
    positive(data["limits"]["wall_seconds"], "limits.wall_seconds")
    positive(data["limits"]["causal_repairs"], "limits.causal_repairs", allow_zero=True)
    require(isinstance(data["queue"], list) and 0 < len(data["queue"]) <= 20,
            "invalid_spec", "queue must contain 1 to 20 ordered candidates")
    seen_ids, seen_branches = set(), set()
    for task in data["queue"]:
        fields(task, {"id", "graph_comment_id", "graph_author", "base_sha", "branch", "prompt_file",
                      "prompt_sha256", "allowed_paths", "checks", "commit_subject"},
               label="queue task")
        fullmatch(IDENTIFIER, task["id"], "task id")
        positive(task["graph_comment_id"], "graph_comment_id")
        fullmatch(LOGIN, task["graph_author"], "graph_author")
        fullmatch(SHA, task["base_sha"], "base_sha")
        fullmatch(BRANCH, task["branch"], "branch")
        require(".." not in task["branch"] and "//" not in task["branch"] and
                not task["branch"].endswith(("/", ".")), "invalid_spec",
                "branch is not a safe Git ref")
        absolute_path(task["prompt_file"], "prompt_file")
        fullmatch(DIGEST, task["prompt_sha256"], "prompt_sha256")
        nonempty(task["commit_subject"], "commit_subject", 100)
        require("\n" not in task["commit_subject"], "invalid_spec",
                "commit_subject must be one line")
        require(isinstance(task["allowed_paths"], list) and task["allowed_paths"] and
                all(isinstance(path, str) for path in task["allowed_paths"]) and
                len(task["allowed_paths"]) == len(set(task["allowed_paths"])),
                "invalid_spec", "allowed_paths must be unique")
        for path in task["allowed_paths"]:
            require(isinstance(path, str) and path and not path.startswith("/") and
                    ".." not in Path(path).parts and "\x00" not in path,
                    "invalid_spec", "allowed path is unsafe")
        require(isinstance(task["checks"], list) and task["checks"],
                "invalid_spec", "task needs focused checks")
        check_ids = set()
        for check in task["checks"]:
            fields(check, {"id", "argv", "timeout_seconds"}, label="task check")
            fullmatch(IDENTIFIER, check["id"], "check id")
            positive(check["timeout_seconds"], "check timeout")
            require(isinstance(check["argv"], list) and check["argv"] and
                    all(isinstance(arg, str) and arg and "\x00" not in arg
                        for arg in check["argv"]), "invalid_spec", "check argv is invalid")
            require(check["id"] not in check_ids, "invalid_spec", "duplicate check id")
            check_ids.add(check["id"])
        require(task["id"] not in seen_ids and task["branch"] not in seen_branches,
                "invalid_spec", "duplicate task or branch in queue")
        seen_ids.add(task["id"])
        seen_branches.add(task["branch"])
    require(isinstance(data["required_checks"], list), "invalid_spec",
            "required_checks must be a list")
    for item in data["required_checks"]:
        fields(item, {"context", "app_id"}, label="required check")
        nonempty(item["context"], "required check context", 200)
        positive(item["app_id"], "required check app_id")
    require(len(data["required_checks"]) == len({
        (item["context"], item["app_id"]) for item in data["required_checks"]
    }), "invalid_spec", "required_checks must be unique")
    absolute_path(data["worktree_root"], "worktree_root")
    absolute_path(data["journal_dir"], "journal_dir")
    if data.get("auth_file") is not None:
        absolute_path(data["auth_file"], "auth_file")
    worktree_root = Path(data["worktree_root"]).resolve()
    journal_dir = Path(data["journal_dir"]).resolve()
    require(not journal_dir.is_relative_to(worktree_root) and
            not worktree_root.is_relative_to(journal_dir), "invalid_spec",
            "journal and candidate worktree roots must be disjoint")
    if data.get("auth_file") is not None:
        require(not Path(data["auth_file"]).resolve().is_relative_to(worktree_root),
                "invalid_spec", "model auth must be outside the child worktree root")
    for task in data["queue"]:
        require(not Path(task["prompt_file"]).resolve().is_relative_to(worktree_root),
                "invalid_spec", "task prompt must be outside the child worktree root")
    return data


def _parse_marker(body, pattern, label):
    require(isinstance(body, str), "ambiguous_graph", f"{label} body is invalid")
    matches = pattern.findall(body)
    require(len(matches) <= 1, "ambiguous_graph", f"multiple {label} markers in one comment")
    if not matches:
        return None
    try:
        return json.loads(matches[0], object_pairs_hook=reject_duplicate_keys)
    except json.JSONDecodeError as error:
        raise RunnerError("ambiguous_graph", f"malformed {label} marker") from error


def select_task(spec, comments, observed_task_id=None):
    """Select a dispatchable node, or resolve one observed node for read-only review."""
    require(isinstance(comments, list), "ambiguous_graph", "issue comments are invalid")
    by_id = {}
    run_events = []
    node_ids = {}
    for comment in comments:
        require(isinstance(comment, dict) and type(comment.get("id")) is int and
                isinstance(comment.get("body"), str) and
                isinstance(comment.get("user"), dict) and
                isinstance(comment["user"].get("login"), str), "ambiguous_graph",
                "issue comment is malformed")
        require(comment["id"] not in by_id, "ambiguous_graph", "duplicate issue comment ID")
        by_id[comment["id"]] = comment
        node = _parse_marker(comment["body"], NODE_MARKER, "graph node")
        if node is not None:
            fields(node, {"id", "state", "base_sha", "acceptance_sha256", "next_action"},
                   label="graph node marker")
            fullmatch(IDENTIFIER, node["id"], "graph node ID")
            fullmatch(SHA, node["base_sha"], "graph node base SHA")
            fullmatch(DIGEST, node["acceptance_sha256"], "graph acceptance SHA-256")
            require(isinstance(node["state"], str) and
                    node["state"] in {"pending", "ready", "working", "blocked", "verified"} and
                    isinstance(node["next_action"], str), "ambiguous_graph",
                    "graph node state or next action is malformed")
            require(node["id"] not in node_ids, "ambiguous_graph",
                    "duplicate graph node ID")
            node_ids[node["id"]] = comment["id"]
        marker = _parse_marker(comment["body"], RUN_MARKER, "run")
        if marker is not None:
            require(comment["user"]["login"] == spec["controller_actor"],
                    "ambiguous_graph", "run checkpoint has an untrusted author")
            fields(marker, {"run_id", "task_id", "phase", "base_sha", "head_sha",
                            "tree_sha", "check_ids", "next_action"}, label="run marker")
            require(isinstance(marker["phase"], str) and
                    marker["phase"] in RUN_PHASES and
                    isinstance(marker["check_ids"], list), "ambiguous_graph",
                    "run marker has unknown phase or check IDs")
            fullmatch(IDENTIFIER, marker["run_id"], "run marker run ID")
            fullmatch(IDENTIFIER, marker["task_id"], "run marker task ID")
            fullmatch(SHA, marker["base_sha"], "run marker base SHA")
            for sha_key in ("head_sha", "tree_sha"):
                if marker[sha_key] is not None:
                    fullmatch(SHA, marker[sha_key], f"run marker {sha_key}")
            require(all(type(id_) is int and id_ > 0 for id_ in marker["check_ids"]),
                    "ambiguous_graph", "run marker check IDs are invalid")
            nonempty(marker["next_action"], "run marker next_action")
            run_events.append((comment["id"], marker))
    for task in spec["queue"]:
        graph = by_id.get(task["graph_comment_id"])
        require(graph is not None, "ambiguous_graph", "graph comment is missing")
        require(graph["user"]["login"] == task["graph_author"],
                "ambiguous_graph", "graph comment has an untrusted author")
        require(node_ids.get(task["id"]) == task["graph_comment_id"],
                "ambiguous_graph", "task graph marker has moved or is duplicated")
        node = _parse_marker(graph["body"], NODE_MARKER, "graph node")
        require(node is not None, "ambiguous_graph", "graph comment has no node marker")
        fields(node, {"id", "state", "base_sha", "acceptance_sha256", "next_action"},
               label="graph node marker")
        require(node["id"] == task["id"] and node["base_sha"] == task["base_sha"] and
                node["acceptance_sha256"] == acceptance_sha256(task),
                "ambiguous_graph", f"graph and spec disagree for {task['id']}")
        if observed_task_id is not None and task["id"] != observed_task_id:
            continue
        events = [(id_, event) for id_, event in run_events
                  if event["task_id"] == task["id"] and id_ > task["graph_comment_id"]]
        foreign = [event for _, event in events if event["run_id"] != spec["run_id"]]
        require(not foreign, "occupied_task", f"task {task['id']} has another run")
        same_run = [event for _, event in events if event["run_id"] == spec["run_id"]]
        if same_run and same_run[-1]["phase"] == "blocked":
            raise RunnerError("blocked_task", f"task {task['id']} has a blocked run checkpoint")
        if observed_task_id is not None:
            require(node["state"] in {"ready", "working", "verified"} and
                    node["next_action"] in {
                        "implement", "review_draft_and_run_promotion_check", "promote"
                    } and
                    same_run and same_run[-1]["phase"] == "observed",
                    "blocked_task", "observed task is blocked or has no current observed checkpoint")
            return task
        if ((node["state"] == "ready" and node["next_action"] == "implement") or
                (node["state"] == "working" and node["next_action"] == "implement"
                 and same_run)):
            return task
        require(node["state"] in {"pending", "working", "blocked", "verified"},
                "ambiguous_graph", f"task {task['id']} has unknown state")
    require(observed_task_id is None or
            observed_task_id in {task["id"] for task in spec["queue"]},
            "stale_evidence", "observed journal task is absent from the private queue")
    return None


def validate_issue_graph_index(spec, issue):
    require(isinstance(issue, dict) and issue.get("state") == "open" and
            issue.get("number") == spec["issue_number"] and
            "pull_request" not in issue and isinstance(issue.get("body"), str),
            "ambiguous_graph", "primary issue is closed or malformed")
    body = issue["body"]
    require(NODE_MARKER.search(body) is None, "ambiguous_graph",
            "issue body duplicates the live graph node")
    found = [int(value) for value in ISSUE_SOURCE_MARKER.findall(body)]
    require(len(found) == len(set(found)), "ambiguous_graph",
            "issue body has duplicate graph-source markers")
    require(all(task["graph_comment_id"] in found for task in spec["queue"]),
            "ambiguous_graph", "issue body does not index the selected graph comments")


def run_marker(run_id, task, phase, head_sha=None, tree_sha=None,
               check_ids=(), next_action=""):
    require(phase in RUN_PHASES, "internal", "invalid run phase")
    value = {
        "run_id": run_id, "task_id": task["id"], "phase": phase,
        "base_sha": task["base_sha"], "head_sha": head_sha,
        "tree_sha": tree_sha, "check_ids": list(check_ids),
        "next_action": next_action,
    }
    return f"<!-- conary-agent-run:v1 {canonical_bytes(value).decode('utf-8')} -->"


def _validate_hosted_state(item, label):
    status = item.get("status")
    conclusion = item.get("conclusion")
    require(status in HOSTED_STATUSES and
            ((status == "completed" and conclusion in TERMINAL_CONCLUSIONS) or
             (status != "completed" and conclusion is None)),
            "remote_unknown", f"{label} has an unknown or inconsistent state")


def _rule_checks(rules):
    require(isinstance(rules, list), "unknown_rules", "branch rules are malformed")
    required, pull_rule = None, None
    for rule in rules:
        require(isinstance(rule, dict) and isinstance(rule.get("type"), str) and
                rule["type"] in KNOWN_RULES,
                "unknown_rules", "unknown or malformed branch rule")
        if rule["type"] == "required_status_checks":
            require(required is None, "unknown_rules", "duplicate required-check rule")
            parameters = rule.get("parameters")
            require(isinstance(parameters, dict) and
                    isinstance(parameters.get("required_status_checks"), list),
                    "unknown_rules", "required-check rule is malformed")
            required = parameters["required_status_checks"]
        if rule["type"] == "pull_request":
            require(pull_rule is None, "unknown_rules", "duplicate pull-request rule")
            pull_rule = rule.get("parameters")
    require(required is not None and isinstance(pull_rule, dict), "unknown_rules",
            "missing required-check or pull-request rule")
    require(pull_rule.get("required_review_thread_resolution") is True,
            "unknown_rules", "review-thread resolution is not required by live rules")
    actual = set()
    for check in required:
        require(isinstance(check, dict) and isinstance(check.get("context"), str) and
                type(check.get("integration_id")) is int,
                "unknown_rules", "live required check is malformed")
        actual.add((check["context"], check["integration_id"]))
    require(len(actual) == len(required), "unknown_rules", "duplicate live required check")
    return actual


def evaluate_promotion(spec, pr, main_sha, rules, statuses, threads, candidate):
    """Return an exact-head read-only decision; unknown policy raises RunnerError."""
    live = _rule_checks(rules)
    configured = {(item["context"], item["app_id"])
                  for item in spec["required_checks"]}
    require(live == configured, "unknown_rules",
            "private required-check list disagrees with live main rules")
    require(isinstance(pr, dict) and isinstance(candidate, dict), "unknown_outcome",
            "pull request or candidate is malformed")
    for key in ("base_sha", "head_sha", "tree_sha"):
        fullmatch(SHA, candidate.get(key), f"candidate {key}")
    fullmatch(SHA, main_sha, "main_sha")
    require(isinstance(statuses, list) and isinstance(threads, list),
            "unknown_outcome", "hosted checks or review threads are malformed")
    reasons = []
    if pr.get("state") != "open" or pr.get("draft") is not False:
        reasons.append("pull request is not open and ready for review")
    if pr.get("merged_at") is not None:
        reasons.append("pull request is already merged")
    head = pr.get("head")
    base = pr.get("base")
    if not isinstance(head, dict) or head.get("sha") != candidate["head_sha"]:
        reasons.append("pull request head changed")
    if not isinstance(base, dict) or base.get("ref") != "main":
        reasons.append("pull request base is not main")
    if main_sha != candidate["base_sha"]:
        reasons.append("main moved after candidate proof")
    if pr.get("mergeable") is not True:
        reasons.append("GitHub has not confirmed mergeability")
    expected_ids = candidate.get("check_ids")
    require(isinstance(expected_ids, list) and
            all(type(id_) is int and id_ > 0 for id_ in expected_ids),
            "unknown_outcome", "candidate check IDs are invalid")
    review = candidate.get("review")
    if not isinstance(review, dict):
        reasons.append("independent review receipt is missing")
    else:
        expected_review = spec["models"]["reviewer"]
        if (review.get("approved") is not True or
                review.get("head_sha") != candidate["head_sha"] or
                review.get("tree_sha") != candidate["tree_sha"] or
                review.get("model_id") != expected_review["id"] or
                review.get("reasoning_effort") != expected_review["reasoning_effort"] or
                not isinstance(review.get("session_id"), str) or
                not review["session_id"] or
                not isinstance(review.get("trace_sha256"), str) or
                DIGEST.fullmatch(review["trace_sha256"]) is None or
                not isinstance(review.get("result_sha256"), str) or
                DIGEST.fullmatch(review["result_sha256"]) is None):
            reasons.append("independent review is not bound to this candidate")
        worker = candidate.get("worker")
        if (not isinstance(worker, dict) or
                not isinstance(worker.get("session_id"), str) or
                not worker["session_id"] or
                worker["session_id"] == review.get("session_id")):
            reasons.append("worker and reviewer sessions are not independent")
    if pr.get("test_merge_tree_sha") != candidate["tree_sha"]:
        reasons.append("PR test-merge tree differs from reviewed candidate")
    if pr.get("test_merge_parents") != [candidate["base_sha"], candidate["head_sha"]]:
        reasons.append("PR test-merge parents differ from reviewed base and head")
    observed = {}
    for item in statuses:
        require(isinstance(item, dict) and type(item.get("id")) is int and
                isinstance(item.get("name"), str) and
                isinstance(item.get("app"), dict) and
                type(item["app"].get("id")) is int and
                isinstance(item.get("head_sha"), str),
                "unknown_outcome", "hosted check is malformed")
        key = (item["name"], item["app"]["id"])
        require(item.get("status") in {"queued", "in_progress", "completed"} and
                (item.get("conclusion") is None or
                 item["conclusion"] in TERMINAL_CONCLUSIONS),
                "unknown_outcome", "hosted check has an unknown state")
        if (key not in live and item["head_sha"] == candidate["head_sha"] and
                (item["status"] != "completed" or item["conclusion"] != "success")):
            reasons.append(f"optional check {item['name']} is not successful")
        if key in live:
            require(item["head_sha"] == candidate["head_sha"], "unknown_outcome",
                    "required check is attached to the wrong head")
            observed.setdefault(key, []).append(item)
    for key in sorted(live):
        copies = observed.get(key, [])
        if len(copies) != 1:
            reasons.append(f"required check {key[0]} is missing or ambiguous")
            continue
        check = copies[0]
        if check["status"] != "completed" or check["conclusion"] != "success":
            reasons.append(f"required check {key[0]} has not succeeded")
        if check["id"] not in expected_ids:
            reasons.append(f"required check {key[0]} is absent from candidate receipt")
    if set(expected_ids) != {copies[0]["id"] for copies in observed.values()
                             if len(copies) == 1}:
        reasons.append("candidate check IDs disagree with live required checks")
    for thread in threads:
        require(isinstance(thread, dict) and type(thread.get("isResolved")) is bool,
                "unknown_outcome", "review thread is malformed")
        if not thread["isResolved"]:
            reasons.append("unresolved review thread")
    return {"ready": not reasons, "reasons": reasons}


def load_spec(path):
    """Read a private, user-owned regular JSON file without following a symlink."""
    source = Path(path)
    require(source.is_absolute(), "invalid_spec", "spec path must be absolute")
    try:
        fd = os.open(source, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    except OSError as error:
        raise RunnerError("invalid_spec", "cannot open private spec") from error
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and
                info.st_mode & 0o077 == 0, "invalid_spec",
                "spec must be an owned 0600 regular file")
        require(info.st_size <= 1024 * 1024, "invalid_spec", "spec is too large")
        with os.fdopen(fd, "rb") as handle:
            fd = -1
            data = json.load(handle, object_pairs_hook=reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RunnerError("invalid_spec", "spec is not valid UTF-8 JSON") from error
    finally:
        if fd != -1:
            os.close(fd)
    validate_spec(data)
    require(not source.resolve().is_relative_to(Path(data["worktree_root"]).resolve()),
            "invalid_spec", "private spec must be outside the child worktree root")
    require(datetime.now(timezone.utc) < parse_expiry(data["authorization"]["expires_at"]),
            "expired_authorization", "run authorization has expired")
    return data


def _private_file(path, expected_hash=None):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    except OSError as error:
        raise RunnerError("missing_input", f"cannot read private input {path}") from error
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and
                info.st_mode & 0o077 == 0, "missing_input",
                f"private input {path} must be owned and mode 0600")
        require(info.st_size <= 1024 * 1024, "missing_input", "private input is too large")
        with os.fdopen(fd, "rb") as handle:
            fd = -1
            payload = handle.read()
    finally:
        if fd != -1:
            os.close(fd)
    if expected_hash is not None:
        require(hashlib.sha256(payload).hexdigest() == expected_hash,
                "stale_input", "prompt hash changed")
    return payload


def _prompt_text(task):
    payload = _private_file(task["prompt_file"], task["prompt_sha256"])
    try:
        return payload.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RunnerError("invalid_spec", "task prompt is not UTF-8") from error


def _json_line(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


class GitHub:
    """Small REST adapter; mutations are named and called only by Controller."""

    def __init__(self, repository, binary="gh"):
        self.repository = repository
        self.binary = binary
        self.remaining = None

    def authenticated_actor(self):
        try:
            result = subprocess.run([self.binary, "api", "--hostname", "github.com",
                                     "user", "--jq", ".login"],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    check=False, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RunnerError("remote_unknown", "cannot identify GitHub actor") from error
        require(result.returncode == 0, "remote_unknown", "cannot identify GitHub actor")
        try:
            login = result.stdout.decode("utf-8").strip()
        except UnicodeDecodeError as error:
            raise RunnerError("remote_unknown", "GitHub actor name is malformed") from error
        fullmatch(LOGIN, login, "authenticated GitHub actor")
        return login

    def request(self, method, route, payload=None, missing_ok=False):
        endpoint = f"repos/{self.repository}/{route}"
        argv = [self.binary, "api", "--hostname", "github.com", "-i", "--method", method,
                "--header", f"X-GitHub-Api-Version: {GITHUB_API_VERSION}",
                endpoint]
        if payload is not None:
            argv[2:2] = ["--input", "-"]
        try:
            timeout = 60
            if method != "GET" and self.remaining is not None:
                timeout = min(timeout, self.remaining())
            result = subprocess.run(argv, input=canonical_bytes(payload) if payload is not None
                                    else None, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    check=False, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RunnerError("remote_unknown", f"GitHub request could not complete: {method} {route}") from error
        raw = result.stdout.replace(b"\r\n", b"\n")
        header, separator, body = raw.partition(b"\n\n")
        match = re.match(rb"HTTP/[^ ]+ ([0-9]{3})", header)
        require(bool(separator and match), "remote_unknown",
                f"GitHub returned no parseable HTTP response for {route}")
        status = int(match.group(1))
        if status == 404 and missing_ok:
            return None
        require(200 <= status < 300 and result.returncode == 0, "remote_unknown",
                f"GitHub {method} {route} returned HTTP {status}")
        if not body.strip():
            return None
        try:
            return json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RunnerError("remote_unknown", f"GitHub returned invalid JSON for {route}") from error

    def list(self, route):
        values = []
        for page in range(1, 12):
            join = "&" if "?" in route else "?"
            part = self.request("GET", f"{route}{join}per_page=100&page={page}")
            require(isinstance(part, list), "remote_unknown", f"GitHub list {route} is malformed")
            values.extend(part)
            if len(part) < 100:
                return values
        raise RunnerError("remote_unknown", f"GitHub list {route} exceeded pagination cap")

    def issue_comments(self, number):
        comments = self.list(f"issues/{number}/comments")
        for comment in comments:
            require(isinstance(comment, dict) and
                    isinstance(comment.get("issue_url"), str) and
                    comment["issue_url"].endswith(f"/issues/{number}"),
                    "remote_unknown", "issue comments contain wrong issue")
        return comments

    def issue(self, number):
        return self.request("GET", f"issues/{number}")

    def main_sha(self):
        branch = self.request("GET", "branches/main")
        commit = branch.get("commit") if isinstance(branch, dict) else None
        value = commit.get("sha") if isinstance(commit, dict) else None
        fullmatch(SHA, value, "main SHA")
        return value

    def branch_sha(self, branch):
        route = "git/ref/heads/" + urllib.parse.quote(branch, safe="/")
        ref = self.request("GET", route, missing_ok=True)
        if ref is None:
            return None
        obj = ref.get("object") if isinstance(ref, dict) else None
        value = obj.get("sha") if isinstance(obj, dict) else None
        fullmatch(SHA, value, "remote branch SHA")
        return value

    def pulls_for_branch(self, branch):
        owner = self.repository.split("/", 1)[0]
        head = urllib.parse.quote(f"{owner}:{branch}", safe="")
        pulls = self.list(f"pulls?state=all&head={head}")
        matched = []
        for pr in pulls:
            require(isinstance(pr, dict), "remote_unknown", "GitHub pull request is malformed")
            head_object = pr.get("head")
            if isinstance(head_object, dict) and head_object.get("ref") == branch:
                matched.append(pr)
        require(len(matched) <= 1, "remote_unknown", "multiple PRs claim the same branch")
        return matched[0] if matched else None

    def create_branch(self, branch, base_sha):
        return self.request("POST", "git/refs", {"ref": f"refs/heads/{branch}", "sha": base_sha})

    def comment(self, number, body):
        result = self.request("POST", f"issues/{number}/comments", {"body": body})
        require(isinstance(result, dict) and type(result.get("id")) is int,
                "remote_unknown", "GitHub did not return a comment ID")
        return result["id"]

    def draft_pr(self, task, body):
        return self.request("POST", "pulls", {
            "title": task["commit_subject"], "head": task["branch"],
            "base": "main", "draft": True, "body": body,
        })

    def required_rules(self):
        return self.request("GET", "rules/branches/main")

    def check_runs(self, sha):
        result = []
        for page in range(1, 12):
            payload = self.request("GET", f"commits/{sha}/check-runs?per_page=100&page={page}")
            require(isinstance(payload, dict) and isinstance(payload.get("check_runs"), list) and
                    type(payload.get("total_count")) is int, "remote_unknown",
                    "check-run response is malformed")
            result.extend(payload["check_runs"])
            if len(result) == payload["total_count"]:
                return result
            require(len(result) < payload["total_count"], "remote_unknown",
                    "check-run pagination count changed")
        raise RunnerError("remote_unknown", "check-run pagination exceeded cap")

    def pr(self, number):
        return self.request("GET", f"pulls/{number}")

    def _pr_graphql(self, number, query, cursor=None):
        owner, name = self.repository.split("/", 1)
        argv = [self.binary, "api", "graphql", "--hostname", "github.com",
                "-f", f"query={query}",
                "-f", f"owner={owner}", "-f", f"name={name}",
                "-F", f"number={number}"]
        if cursor is not None:
            argv.extend(("-f", f"cursor={cursor}"))
        try:
            result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    check=False, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RunnerError("remote_unknown", "cannot query GitHub pull request") from error
        require(result.returncode == 0, "remote_unknown", "GitHub pull-request query failed")
        try:
            value = json.loads(result.stdout, object_pairs_hook=lambda pairs:
                               reject_duplicate_keys(pairs, code="remote_unknown"))
        except (UnicodeDecodeError, ValueError) as error:
            raise RunnerError("remote_unknown", "GitHub pull-request response is malformed") from error
        require(isinstance(value, dict) and "errors" not in value,
                "remote_unknown", "GitHub pull-request query returned errors")
        return value

    def potential_merge_sha(self, number):
        query = ("query($owner:String!,$name:String!,$number:Int!){"
                 "repository(owner:$owner,name:$name){nameWithOwner "
                 "pullRequest(number:$number){number potentialMergeCommit{oid}}}}")
        value = self._pr_graphql(number, query)
        data = value.get("data")
        repository = data.get("repository") if isinstance(data, dict) else None
        require(isinstance(repository, dict) and
                isinstance(repository.get("nameWithOwner"), str) and
                repository["nameWithOwner"].casefold() == self.repository.casefold(),
                "remote_unknown", "test-merge query returned a foreign repository")
        pr = repository.get("pullRequest")
        require(isinstance(pr, dict) and type(pr.get("number")) is int and
                pr["number"] == number,
                "remote_unknown", "test-merge query returned a foreign pull request")
        commit = pr.get("potentialMergeCommit")
        require(isinstance(commit, dict), "remote_unknown",
                "GitHub test-merge commit is unavailable")
        sha = commit.get("oid")
        require(isinstance(sha, str) and SHA.fullmatch(sha) is not None,
                "remote_unknown", "GitHub test-merge commit SHA is malformed")
        return sha

    def commit_data(self, sha):
        commit = self.request("GET", f"git/commits/{sha}")
        require(isinstance(commit, dict) and commit.get("sha") == sha,
                "remote_unknown", "remote commit identity differs from test-merge SHA")
        tree = commit.get("tree")
        value = tree.get("sha") if isinstance(tree, dict) else None
        fullmatch(SHA, value, "remote commit tree SHA")
        parents = commit.get("parents")
        require(isinstance(parents, list), "remote_unknown", "remote commit parents are malformed")
        parent_shas = []
        for parent in parents:
            parent_sha = parent.get("sha") if isinstance(parent, dict) else None
            fullmatch(SHA, parent_sha, "remote commit parent SHA")
            parent_shas.append(parent_sha)
        return {"tree_sha": value, "parents": parent_shas}

    def review_threads(self, number):
        query = ("query($owner:String!,$name:String!,$number:Int!,$cursor:String){"
                 "repository(owner:$owner,name:$name){pullRequest(number:$number){"
                 "reviewThreads(first:100,after:$cursor){nodes{isResolved}"
                 "pageInfo{hasNextPage endCursor}}}}}")
        threads = []
        cursor = None
        for _ in range(11):
            value = self._pr_graphql(number, query, cursor)
            try:
                block = value["data"]["repository"]["pullRequest"]["reviewThreads"]
                nodes, page = block["nodes"], block["pageInfo"]
            except (KeyError, TypeError) as error:
                raise RunnerError("remote_unknown", "review-thread response is malformed") from error
            require(isinstance(nodes, list) and isinstance(page, dict) and
                    type(page.get("hasNextPage")) is bool,
                    "remote_unknown", "review-thread page is malformed")
            threads.extend(nodes)
            if not page["hasNextPage"]:
                return threads
            cursor = page.get("endCursor")
            require(isinstance(cursor, str) and cursor, "remote_unknown",
                    "review-thread cursor is missing")
        raise RunnerError("remote_unknown", "review-thread pagination exceeded cap")

    def workflow_runs(self, sha):
        result = []
        for page in range(1, 12):
            payload = self.request("GET", f"actions/runs?head_sha={sha}&per_page=100&page={page}")
            require(isinstance(payload, dict) and isinstance(payload.get("workflow_runs"), list) and
                    type(payload.get("total_count")) is int,
                    "remote_unknown", "workflow-run response is malformed")
            result.extend(payload["workflow_runs"])
            if len(result) == payload["total_count"]:
                return result
            require(len(result) < payload["total_count"], "remote_unknown",
                    "workflow-run pagination count changed")
        raise RunnerError("remote_unknown", "workflow-run pagination exceeded cap")

    def workflow_jobs(self, run_id):
        result = []
        for page in range(1, 12):
            payload = self.request("GET", f"actions/runs/{run_id}/jobs?per_page=100&page={page}")
            require(isinstance(payload, dict) and isinstance(payload.get("jobs"), list) and
                    type(payload.get("total_count")) is int,
                    "remote_unknown", "workflow-job response is malformed")
            result.extend(payload["jobs"])
            if len(result) == payload["total_count"]:
                return result
            require(len(result) < payload["total_count"], "remote_unknown",
                    "workflow-job pagination count changed")
        raise RunnerError("remote_unknown", "workflow-job pagination exceeded cap")


def git(cwd, *args, input_bytes=None, timeout=120, credentialed=False):
    safe_env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
        "HOME": "/nonexistent", "GIT_TERMINAL_PROMPT": "0",
    }
    argv = ["git", "-C", str(cwd),
            "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
            "-c", "commit.gpgsign=false", *args]
    if credentialed:
        gh_path = shutil.which("gh")
        require(gh_path is not None, "missing_input", "gh CLI is required for Git transport")
        argv[3:3] = ["-c", f"credential.https://github.com.helper=!"
                           f"{shlex.quote(gh_path)} auth git-credential"]
        safe_env["HOME"] = os.environ.get("HOME", "/nonexistent")
        safe_env["GIT_ALLOW_PROTOCOL"] = "https"
        for name in ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN", "XDG_CONFIG_HOME"):
            if name in os.environ:
                safe_env[name] = os.environ[name]
    try:
        result = subprocess.run(argv, input=input_bytes,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                check=False, timeout=timeout,
                                env=safe_env)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RunnerError("local_unknown", f"git {args[0]} did not complete") from error
    require(result.returncode == 0, "local_unknown",
            f"git {args[0]} failed with exit {result.returncode}")
    return result.stdout


def git_sha(cwd, expression):
    value = git(cwd, "rev-parse", expression).decode("ascii").strip()
    fullmatch(SHA, value, f"git {expression}")
    return value


def changed_paths(cwd):
    tracked = git(cwd, "diff", "--name-only", "-z", "HEAD").split(b"\0")
    untracked = git(cwd, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
    try:
        return sorted({os.fsdecode(value) for value in tracked + untracked if value})
    except UnicodeDecodeError as error:
        raise RunnerError("local_unknown", "candidate has non-UTF-8 paths") from error


def assert_worktree_pointer(source_root, worktree):
    """Check the Git indirection before host Git touches a child-writable tree."""
    pointer = Path(worktree) / ".git"
    try:
        info = pointer.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and
                info.st_size < 4096, "local_unknown", "worktree Git pointer is unsafe")
        content = pointer.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeDecodeError) as error:
        raise RunnerError("local_unknown", "cannot inspect worktree Git pointer") from error
    require(content.startswith("gitdir: "), "local_unknown",
            "worktree Git pointer is malformed")
    destination = Path(content.removeprefix("gitdir: ")).resolve()
    common = Path(git(source_root, "rev-parse", "--path-format=absolute",
                      "--git-common-dir").decode().strip()).resolve()
    require(destination.parent == common / "worktrees" and destination.is_dir(),
            "local_unknown", "worktree Git pointer escapes the common repository")
    actual = Path(git(worktree, "rev-parse", "--path-format=absolute",
                      "--git-common-dir").decode().strip()).resolve()
    require(actual == common and
            Path(git(worktree, "rev-parse", "--show-toplevel").decode().strip()).resolve()
            == Path(worktree).resolve(), "local_unknown",
            "worktree Git metadata differs from expected repository")


def path_allowed(path, allowed):
    return any(path == prefix.rstrip("/") or path.startswith(prefix.rstrip("/") + "/")
               for prefix in allowed)


def _owned_directory(path):
    destination = Path(path)
    destination.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = destination.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and
            info.st_mode & 0o077 == 0, "local_unknown",
            f"private directory {destination} is not owned mode 0700")
    return destination


@contextmanager
def run_lock(journal_dir, source_root=None):
    base = _owned_directory(journal_dir)
    if source_root is not None:
        common = Path(git(source_root, "rev-parse", "--path-format=absolute",
                          "--git-common-dir").decode().strip()).resolve()
        require(common.is_dir(), "local_unknown", "repository common Git dir disappeared")
        lock_file = common / "agent-runner.lock"
    else:
        lock_file = base / ".lock"
    fd = os.open(lock_file, os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW,
                 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and
                info.st_mode & 0o077 == 0, "local_unknown", "runner lock is unsafe")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RunnerError("busy", "another runner owns the singleton lock") from error
        yield
    finally:
        os.close(fd)


def journal_path(spec):
    return Path(spec["journal_dir"]) / f"{spec['run_id']}.json"


def read_journal(spec):
    _owned_directory(spec["journal_dir"])
    path = journal_path(spec)
    if not path.exists():
        return None
    raw = _private_file(path)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RunnerError("local_unknown", "runner journal is malformed") from error
    require(isinstance(value, dict) and value.get("schema_version") == 1 and
            value.get("run_id") == spec["run_id"] and
            value.get("spec_sha256") == hashlib.sha256(canonical_bytes(spec)).hexdigest(),
            "local_unknown", "runner journal disagrees with the private spec")
    started = parse_expiry(value.get("started_at"), "journal.started_at")
    deadline = parse_expiry(value.get("deadline_at"), "journal.deadline_at")
    require(started <= deadline and
            deadline - started <= timedelta(seconds=spec["limits"]["wall_seconds"]),
            "local_unknown", "journal wall deadline exceeds the authorized budget")
    return value


def write_journal(spec, value):
    destination = journal_path(spec)
    base = _owned_directory(spec["journal_dir"])
    value = dict(value)
    value["schema_version"] = 1
    value["run_id"] = spec["run_id"]
    value["spec_sha256"] = hashlib.sha256(canonical_bytes(spec)).hexdigest()
    temporary = base / f".{spec['run_id']}.{os.getpid()}.tmp"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
                 0o600)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(canonical_bytes(value) + b"\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, destination)
        directory_fd = os.open(base, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if temporary.exists():
            temporary.unlink()


def _check_authorized(spec, name):
    require(name in spec["authorization"]["remote_writes"],
            "unauthorized_write", f"authorization does not include {name}")
    require(datetime.now(timezone.utc) < parse_expiry(spec["authorization"]["expires_at"]),
            "expired_authorization", "run authorization expired before remote write")


def _latest_run_event(comments, run_id, task_id):
    result = None
    for comment in comments:
        marker = _parse_marker(comment["body"], RUN_MARKER, "run")
        if marker is not None and marker.get("run_id") == run_id and marker.get("task_id") == task_id:
            if result is None or comment["id"] > result[0]:
                result = (comment["id"], marker)
    return result


def optional_pretest_image_failure(jobs):
    """Classify one known hosted setup failure for reporting; never waive it."""
    if not isinstance(jobs, list) or not jobs:
        return None
    failed = [job for job in jobs if isinstance(job, dict) and
              job.get("conclusion") == "failure"]
    if len(failed) != 2:
        return None
    opensuse = next((job for job in failed if
                     job.get("name") == "native-cross-source-lifecycle (opensuse-tumbleweed)"),
                    None)
    aggregate = next((job for job in failed if
                      job.get("name") == "native-cross-source-lifecycle"), None)
    if opensuse is None or aggregate is None:
        return None
    if not isinstance(opensuse.get("steps"), list):
        return None
    steps = {step.get("number"): step for step in opensuse["steps"]
             if isinstance(step, dict)}
    cache = steps.get(5)
    if (not isinstance(cache, dict) or
            cache.get("name") != "Run ./.github/actions/cache-base-image" or
            cache.get("conclusion") != "failure" or
            any(steps.get(number, {}).get("conclusion") != "skipped"
                for number in range(6, 13))):
        return None
    aggregate_steps = aggregate.get("steps")
    if not isinstance(aggregate_steps, list) or not any(
            isinstance(step, dict) and
            step.get("name") == "Require every distro lifecycle job" and
            step.get("conclusion") == "failure" for step in aggregate_steps):
        return None
    if any(not isinstance(job, dict) or job.get("status") != "completed"
           for job in jobs):
        return None
    ids = (opensuse.get("id"), aggregate.get("id"))
    if any(type(value) is not int or value < 1 for value in ids):
        return None
    return {"opensuse_job_id": ids[0], "aggregate_job_id": ids[1],
            "failed_step": 5, "product_steps_skipped": list(range(6, 13))}


def _validate_remote_checkpoint(spec, task, comments, journal):
    event = _latest_run_event(comments, spec["run_id"], task["id"])
    if journal is None:
        require(event is None, "ambiguous_graph", "run checkpoint exists without local journal")
        return None
    require(journal.get("task_id") == task["id"], "local_unknown",
            "journal names a different task")
    if event is None:
        require(journal.get("phase") in {"branch_intent", "branch_created"}, "ambiguous_graph",
                "issue checkpoint disappeared")
    else:
        require(event[1].get("base_sha") == task["base_sha"], "ambiguous_graph",
                "issue checkpoint base differs from the selected task")
        phase = event[1].get("phase")
        allowed_journal = {
            "claimed": {"branch_intent", "branch_created", "claimed", "worker_started", "candidate",
                        "push_intent", "draft_intent", "draft", "observed"},
            "candidate": {"candidate", "push_intent", "draft_intent", "draft", "observed"},
            "draft": {"draft", "observed"},
            "observed": {"observed"},
            "blocked": {"blocked"},
        }
        require(phase in allowed_journal and journal.get("phase") in allowed_journal[phase],
                "ambiguous_graph", "issue checkpoint is ahead of or conflicts with local phase")
        if event[1].get("phase") in {"candidate", "draft", "observed"}:
            candidate = journal.get("candidate")
            require(isinstance(candidate, dict) and
                    event[1].get("head_sha") == candidate.get("head_sha") and
                    event[1].get("tree_sha") == candidate.get("tree_sha"),
                    "ambiguous_graph", "issue checkpoint disagrees with local candidate")
    return event


def _load_sandbox():
    source = Path(__file__).with_name("agent_runner_sandbox.py")
    require(source.is_file(), "missing_input", "sandbox launcher is missing")
    module_spec = importlib.util.spec_from_file_location("agent_runner_sandbox", source)
    require(module_spec is not None and module_spec.loader is not None,
            "missing_input", "cannot load sandbox launcher")
    module = importlib.util.module_from_spec(module_spec)
    sys.modules[module_spec.name] = module
    module_spec.loader.exec_module(module)
    return module


def _digest_file(path):
    digest = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as handle:
        require(stat.S_ISREG(os.fstat(handle.fileno()).st_mode), "stale_evidence",
                "evidence is not a regular file")
        while block := handle.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def _plain_relative_file(root, locator):
    require(isinstance(locator, str) and locator and not locator.startswith("/") and
            ".." not in Path(locator).parts, "stale_evidence",
            "evidence path is invalid")
    path = Path(root)
    for part in Path(locator).parts:
        path = path / part
        require(not path.is_symlink(), "stale_evidence",
                "evidence path contains a symlink")
    require(path.is_file(), "stale_evidence", "evidence file disappeared")
    return path


def _auth_secret_values(auth_file):
    try:
        document = json.loads(
            _private_file(auth_file),
            object_pairs_hook=lambda pairs: reject_duplicate_keys(pairs, code="missing_input"),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RunnerError("missing_input", "model auth JSON is malformed") from error
    require(isinstance(document, dict), "missing_input", "model auth JSON must be an object")
    values = set()
    sensitive_keys = {"token", "access_token", "refresh_token", "id_token",
                      "api_key", "secret", "client_secret", "password", "private_key"}

    def visit(value, key=""):
        if isinstance(value, dict):
            for child_key, child in value.items():
                if isinstance(child_key, str):
                    visit(child, child_key.lower())
        elif isinstance(value, list):
            for child in value:
                visit(child, key)
        elif isinstance(value, str) and (
                len(value) >= 20 or
                ((key in sensitive_keys or key.endswith("_token")) and len(value) >= 4)):
            raw = value.encode("utf-8")
            values.add(raw)
            values.add(json.dumps(value, ensure_ascii=True)[1:-1].encode("utf-8"))

    visit(document)
    require(values, "missing_input", "model auth JSON has no scannable credential value")
    return values


def _scan_candidate_bytes(payload, auth_values):
    require(len(payload) <= MAX_CANDIDATE_FILE, "out_of_scope",
            "candidate file exceeds the secret-scan size limit")
    require(not COMMON_CREDENTIAL.search(payload) and
            not any(secret in payload for secret in auth_values),
            "secret_in_candidate", "candidate contains a credential value")


def _working_payload(worktree, path):
    target = Path(worktree)
    parts = Path(path).parts
    require(parts and ".." not in parts and not Path(path).is_absolute(),
            "out_of_scope", "candidate path is invalid")
    for part in parts[:-1]:
        target = target / part
        require(target.is_dir() and not target.is_symlink(), "out_of_scope",
                "candidate path crosses a symlink")
    target = target / parts[-1]
    try:
        info = target.lstat()
    except FileNotFoundError:
        return None  # staged deletion
    if stat.S_ISLNK(info.st_mode):
        return os.fsencode(os.readlink(target))
    require(stat.S_ISREG(info.st_mode) and info.st_size <= MAX_CANDIDATE_FILE,
            "out_of_scope", "candidate contains an unsupported or oversized file")
    fd = os.open(target, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as handle:
        require(stat.S_ISREG(os.fstat(handle.fileno()).st_mode), "out_of_scope",
                "candidate file changed type during scan")
        return handle.read(MAX_CANDIDATE_FILE + 1)


def _staged_payload(worktree, path):
    listing = git(worktree, "ls-files", "--stage", "-z", "--", path)
    rows = [row for row in listing.split(b"\0") if row]
    if not rows:
        return None  # staged deletion
    require(len(rows) == 1, "out_of_scope", "candidate has unresolved index stages")
    metadata, separator, name = rows[0].partition(b"\t")
    fields_ = metadata.split()
    require(separator and len(fields_) == 3 and fields_[2] == b"0" and
            name == os.fsencode(path) and
            fields_[0] in {b"100644", b"100755", b"120000"},
            "out_of_scope", "candidate index entry is unsupported")
    sha = fields_[1].decode("ascii")
    fullmatch(SHA, sha, "staged blob SHA")
    size_raw = git(worktree, "cat-file", "-s", sha).decode("ascii").strip()
    require(size_raw.isdigit() and int(size_raw) <= MAX_CANDIDATE_FILE,
            "out_of_scope", "staged blob exceeds the secret-scan size limit")
    return git(worktree, "cat-file", "blob", sha)


class Controller:
    def __init__(self, spec, github=None, source_root=None, sandbox=None):
        self.spec = spec
        self.gh = github or GitHub(spec["repository"])
        if isinstance(self.gh, GitHub):
            self.gh.remaining = self._remaining
        self.source_root = Path(source_root or Path(__file__).resolve().parents[1])
        self.sandbox = sandbox
        self.started = time.monotonic()

    def _remaining(self):
        value = self.spec["limits"]["wall_seconds"] - (time.monotonic() - self.started)
        journal = read_journal(self.spec)
        if journal is not None:
            deadline = parse_expiry(journal["deadline_at"])
            value = min(value, (deadline - datetime.now(timezone.utc)).total_seconds())
        require(value > 0, "wall_budget", "run wall-clock budget expired")
        return int(max(1, value))

    def _remote_state(self):
        issue = self.gh.issue(self.spec["issue_number"])
        validate_issue_graph_index(self.spec, issue)
        comments = self.gh.issue_comments(self.spec["issue_number"])
        task = select_task(self.spec, comments)
        if task is None:
            return {"task": None, "comments": comments}
        return {
            "task": task, "comments": comments,
            "main_sha": self.gh.main_sha(),
            "branch_sha": self.gh.branch_sha(task["branch"]),
            "pr": self.gh.pulls_for_branch(task["branch"]),
        }

    def _check_actor(self):
        require(self.gh.authenticated_actor() == self.spec["controller_actor"],
                "unauthorized_write", "authenticated GitHub actor differs from private spec")

    def _check_private_mount_boundaries(self):
        common = Path(git(self.source_root, "rev-parse", "--path-format=absolute",
                          "--git-common-dir").decode().strip()).resolve()
        private_paths = [self.spec["journal_dir"]]
        if self.spec.get("auth_file"):
            private_paths.append(self.spec["auth_file"])
        private_paths.extend(task["prompt_file"] for task in self.spec["queue"])
        require(not any(Path(path).resolve().is_relative_to(common) for path in private_paths),
                "invalid_spec", "private runner inputs overlap Git metadata mounted to child")

    def _before_write(self, task, name, branch_sha, journal):
        self._remaining()
        _check_authorized(self.spec, name)
        self._check_actor()
        state = self._remote_state()
        require(state["task"] is not None and state["task"]["id"] == task["id"],
                "ambiguous_graph", "graph task changed before remote write")
        _validate_remote_checkpoint(self.spec, task, state["comments"], journal)
        require(state["main_sha"] == task["base_sha"], "stale_base",
                "main moved after task selection")
        require(state["branch_sha"] == branch_sha, "remote_unknown",
                "task branch changed before remote write")
        pr = state["pr"]
        if name in {"create_branch", "update_branch", "create_draft_pr"}:
            require(pr is None, "occupied_task",
                    "task branch has a PR before the controller's draft step")
        elif pr is not None:
            self._assert_owned_pr(task, journal, pr)
        self._remaining()
        return state

    def _assert_owned_pr(self, task, journal, pr):
        candidate = journal.get("candidate") if isinstance(journal, dict) else None
        require(isinstance(candidate, dict) and
                isinstance(candidate.get("head_sha"), str), "occupied_task",
                "PR exists before a candidate was recorded")
        marker = (f"<!-- conary-agent-pr:v1 run={self.spec['run_id']} "
                  f"task={task['id']} head={candidate['head_sha']} -->")
        head = pr.get("head") if isinstance(pr, dict) else None
        base = pr.get("base") if isinstance(pr, dict) else None
        actor = pr.get("user") if isinstance(pr, dict) else None
        require(isinstance(pr, dict) and type(pr.get("number")) is int and
                (journal.get("pr_number") is None or
                 pr["number"] == journal["pr_number"]) and
                pr.get("state") == "open" and pr.get("draft") is True and
                isinstance(actor, dict) and
                actor.get("login") == self.spec["controller_actor"] and
                isinstance(pr.get("body"), str) and marker in pr["body"] and
                isinstance(head, dict) and head.get("ref") == task["branch"] and
                head.get("sha") == candidate["head_sha"] and
                isinstance(base, dict) and base.get("ref") == "main" and
                base.get("sha") == task["base_sha"],
                "occupied_task", "existing PR is not the controller-owned draft")

    def dry_run(self):
        state = self._remote_state()
        task = state["task"]
        if task is None:
            return {"status": "no_ready_task", "run_id": self.spec["run_id"]}
        _prompt_text(task)
        journal = read_journal(self.spec)
        if journal is not None and journal.get("phase") != "observed":
            self._remaining()
        _validate_remote_checkpoint(self.spec, task, state["comments"], journal)
        require(state["main_sha"] == task["base_sha"], "stale_base",
                "main differs from selected task base")
        if journal is None:
            require(state["branch_sha"] is None and state["pr"] is None,
                    "occupied_task", "task branch or PR exists without this run's journal")
        return {
            "status": "ready", "run_id": self.spec["run_id"],
            "task_id": task["id"], "base_sha": task["base_sha"],
            "branch_sha": state["branch_sha"],
            "pr_number": state["pr"].get("number") if state["pr"] else None,
            "journal_phase": journal.get("phase") if journal else None,
        }

    def _append_checkpoint(self, task, phase, journal, next_action,
                           head_sha=None, tree_sha=None, check_ids=()):
        branch_sha = self.gh.branch_sha(task["branch"])
        self._before_write(task, "comment_issue", branch_sha, journal)
        candidate = journal.get("candidate") or {}
        proof_hashes = [item.get("receipt_sha256") for item in candidate.get("proof", [])]
        review_hash = (candidate.get("review") or {}).get("result_sha256")
        evidence_note = (f"Focused receipt SHA-256: `{', '.join(proof_hashes)}`. "
                         f"Independent review result SHA-256: `{review_hash}`.\n\n"
                         if proof_hashes and review_hash else "")
        body = (f"Agent run `{self.spec['run_id']}` task `{task['id']}`: {phase}.\n\n"
                f"Base `{task['base_sha']}`; head `{head_sha or 'pending'}`; "
                f"tree `{tree_sha or 'pending'}`. Next: {next_action}.\n\n"
                f"{evidence_note}"
                f"{run_marker(self.spec['run_id'], task, phase, head_sha, tree_sha, check_ids, next_action)}")
        self._remaining()
        return self.gh.comment(self.spec["issue_number"], body)

    def _worktree(self, task, journal):
        root = _owned_directory(self.spec["worktree_root"])
        path = root / self.spec["run_id"]
        if not path.exists():
            self._verify_origin()
            git(self.source_root, "fetch", "--no-tags",
                f"https://github.com/{self.spec['repository']}.git", "main", timeout=180,
                credentialed=True)
            require(git_sha(self.source_root, "FETCH_HEAD") == task["base_sha"],
                    "stale_base", "fetched main differs from task base")
            git(self.source_root, "worktree", "add", "--detach", str(path), task["base_sha"])
        require(path.is_dir() and not path.is_symlink(), "local_unknown",
                "run worktree is not a plain directory")
        assert_worktree_pointer(self.source_root, path)
        expected = (journal.get("candidate") or {}).get("head_sha") or task["base_sha"]
        require(git_sha(path, "HEAD") == expected, "local_unknown",
                "run worktree HEAD differs from journal")
        require(not changed_paths(path), "local_unknown",
                "run worktree has uncheckpointed changes")
        return path

    def _verify_origin(self):
        expected = f"https://github.com/{self.spec['repository']}"
        acceptable = {expected, expected + ".git"}
        fetch = git(self.source_root, "remote", "get-url", "origin").decode().strip()
        push = git(self.source_root, "remote", "get-url", "--push", "origin").decode().strip()
        require(fetch in acceptable and push in acceptable, "remote_unknown",
                "origin fetch or push URL differs from selected GitHub repository")
        names = git(self.source_root, "config", "--local", "--list", "--name-only").decode(
            "utf-8").splitlines()
        forbidden = ("url.", "filter.", "include.", "includeif.", "credential.", "http.")
        require(not any(name.lower().startswith(forbidden) for name in names),
                "remote_unknown", "repository config contains a transport or filter override")

    def _launch(self, task, worktree, role, attempt, candidate=None, prior_failure=None,
                review_retry=0):
        if self.sandbox is None:
            self.sandbox = _load_sandbox()
        model = self.spec["models"][role]
        run_dir = _owned_directory(Path(self.spec["journal_dir"]) / self.spec["run_id"])
        basename = f"{role}-{attempt}"
        if review_retry:
            require(role == "reviewer" and 0 < review_retry <= MAX_REVIEW_RESULT_RETRIES,
                    "internal", "invalid reviewer result retry")
            basename += f"-retry-{review_retry}"
        trace = run_dir / f"{basename}.jsonl"
        result_file = run_dir / f"{basename}.json"
        if role == "worker":
            assignment = _prompt_text(task)
            excluded = ", ".join(f"#{number}" for number in self.spec["exclusions"])
            prompt = (
                f"Implement one bounded Conary task. Run ID {self.spec['run_id']}; "
                f"task ID {task['id']}; branch {task['branch']}. "
                f"Excluded issue ownership: {excluded or 'none'}. Do not take that work. "
                f"Edit only {task['allowed_paths']}. Do not run gh, push, commit, or write "
                "GitHub. The controller owns remote actions and commits. "
                "Return one JSON object with exact keys schema_version, run_id, task_id, "
                "status, branch, candidate_head, candidate_tree, evidence, reason. "
                "For this implementation response, candidate_head and candidate_tree are null. "
                "Use status ready only when edits are complete; blocked with a concrete reason otherwise.\n\n"
                f"Task:\n{assignment}\n")
            if prior_failure:
                prompt += f"\nRepair the causal failure from this prior attempt: {prior_failure}\n"
        else:
            require(candidate is not None, "internal", "review requires a candidate")
            require(all(isinstance(item, dict) and
                        isinstance(item.get("receipt_path"), str) and
                        Path(item["receipt_path"]).is_relative_to(worktree)
                        for item in candidate.get("proof", [])),
                    "stale_evidence", "review receipts escaped the proof worktree")
            receipt_summaries = [
                {"check_id": item["check_id"],
                 "receipt_path": str(Path(item["receipt_path"]).relative_to(worktree)),
                 "receipt_sha256": item["receipt_sha256"]}
                for item in candidate.get("proof", [])
            ]
            prompt = (
                f"Independently review Conary task {task['id']} run {self.spec['run_id']}. "
                f"The logical task branch is {json.dumps(task['branch'])}; the proof "
                "worktree is detached, so do not infer the branch from Git HEAD. "
                f"Candidate HEAD {candidate['head_sha']} tree {candidate['tree_sha']}; "
                f"base {task['base_sha']}. Inspect the diff and receipts. Do not edit any file, "
                "run gh, commit, or push. Return exactly one JSON object, without Markdown, "
                "with these nine keys and no others: schema_version, run_id, task_id, status, "
                "branch, candidate_head, candidate_tree, evidence, reason. "
                f"Set schema_version to 1, run_id to {json.dumps(self.spec['run_id'])}, "
                f"task_id to {json.dumps(task['id'])}, branch to {json.dumps(task['branch'])}, "
                f"candidate_head to {json.dumps(candidate['head_sha'])}, and candidate_tree "
                f"to {json.dumps(candidate['tree_sha'])}. Use status ready only with no "
                "actionable findings; otherwise use blocked and give a concise reason. "
                "Evidence must be an array of worktree-relative POSIX paths (no absolute "
                "paths or '..' components); use [] if there are no supporting files. "
                "Set reason to null for ready or a concise string for blocked. "
                "The controller ran the configured checks in this fresh proof worktree. "
                f"Read these validated receipts and their logs: {receipt_summaries}. "
                "Inspect the candidate diff and whether its tests establish the task property.\n")
            if review_retry:
                prompt += ("A previous reviewer session completed but its final JSON artifact "
                           "was rejected. Review this same frozen candidate and proof again, "
                           "then follow the exact result contract above.\n")
        before = (git_sha(worktree, "HEAD"), git_sha(worktree, "HEAD^{tree}"),
                  changed_paths(worktree))
        timeout = min(self._remaining(), 3600)
        try:
            result = self.sandbox.launch_codex(
                worktree=worktree, prompt=prompt, model=model["id"],
                reasoning_effort=model["reasoning_effort"], timeout_seconds=timeout,
                trace_path=trace, result_path=result_file,
                auth_file=Path(self.spec["auth_file"]) if self.spec.get("auth_file") else None,
                read_only_worktree=(role == "reviewer"),
            )
        except (OSError, RuntimeError, ValueError) as error:
            raise RunnerError("model_unavailable", f"{role} sandbox launch failed") from error
        assert_worktree_pointer(self.source_root, worktree)
        require(result.model == model["id"] and
                result.reasoning_effort == model["reasoning_effort"],
                "model_unavailable", "launcher changed the requested model or effort")
        if role == "reviewer":
            require((git_sha(worktree, "HEAD"), git_sha(worktree, "HEAD^{tree}"),
                     changed_paths(worktree)) == before,
                    "stale_evidence", "review changed the frozen candidate")
            if result.error == "invalid-final-result":
                result_class = getattr(result, "final_result_class", None)
                if (result.ok is False and result.exit_code == 0 and not result.timed_out and
                        result.session_id and
                        result_class in RETRYABLE_REVIEW_RESULT_CLASSES):
                    require(trace.is_file() and not trace.is_symlink() and
                            trace.stat().st_size < 16 * 1024 * 1024 and
                            not result_file.exists(), "unknown_outcome",
                            "invalid reviewer result lacks a clean trace boundary")
                    try:
                        rows = [json.loads(line) for line in
                                trace.read_text(encoding="utf-8").splitlines()]
                    except (UnicodeDecodeError, json.JSONDecodeError) as error:
                        raise RunnerError("unknown_outcome",
                                          "invalid reviewer result trace is malformed") from error
                    require(len(rows) >= 3 and all(isinstance(row, dict) for row in rows) and
                            rows[0].get("kind") == "launch" and
                            rows[0].get("model") == model["id"] and
                            rows[0].get("reasoning_effort") == model["reasoning_effort"] and
                            rows[0].get("read_only_worktree") is True and
                            any(row.get("kind") == "codex_event" and
                                row.get("type") == "thread.started" and
                                row.get("session_id") == result.session_id for row in rows) and
                            any(row.get("kind") == "codex_event" and
                                row.get("type") == "turn.completed" for row in rows) and
                            rows[-1].get("kind") == "complete" and
                            rows[-1].get("status") == "invalid-final-result" and
                            rows[-1].get("session_id") == result.session_id and
                            rows[-1].get("exit_code") == 0 and
                            rows[-1].get("timed_out") is False and
                            rows[-1].get("invalid_events") == 0 and
                            rows[-1].get("final_result_class") == result_class,
                            "unknown_outcome", "invalid reviewer result trace is inconsistent")
                    raise RunnerError("invalid_reviewer_result",
                                      f"reviewer final artifact is {result_class}")
                raise RunnerError("unknown_outcome",
                                  "reviewer final artifact has a non-retryable rejection")
        require(result.ok and not result.timed_out and result.session_id,
                "model_unavailable", f"{role} launch did not complete: {result.error}")
        require(trace.is_file() and result_file.is_file(), "local_unknown",
                "launcher omitted its trace or final result")
        try:
            final = json.loads(result_file.read_text(encoding="utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RunnerError("local_unknown", "child final result is malformed") from error
        fields(final, {"schema_version", "run_id", "task_id", "status", "branch",
                       "candidate_head", "candidate_tree", "evidence", "reason"},
               label="child result")
        require(final["schema_version"] == 1 and final["run_id"] == self.spec["run_id"] and
                final["task_id"] == task["id"] and final["branch"] == task["branch"] and
                final["status"] in {"ready", "blocked"} and
                isinstance(final["evidence"], list), "local_unknown",
                "child result disagrees with selected task")
        if role == "worker":
            require(final["candidate_head"] is None and final["candidate_tree"] is None,
                    "local_unknown", "worker claimed an uncommitted candidate")
        else:
            require(final["candidate_head"] == candidate["head_sha"] and
                    final["candidate_tree"] == candidate["tree_sha"] and
                    (git_sha(worktree, "HEAD"), git_sha(worktree, "HEAD^{tree}"),
                     changed_paths(worktree)) == before,
                    "stale_evidence", "review changed or mismatched frozen candidate")
        require(final["status"] == "ready", "child_blocked",
                f"{role} reported blocked: {final['reason']}")
        return {
            "session_id": result.session_id, "model_id": result.model,
            "reasoning_effort": result.reasoning_effort,
            "trace_sha256": _digest_file(trace),
            "result_sha256": _digest_file(result_file),
            "trace_path": str(trace), "result_path": str(result_file),
        }

    def _commit(self, task, worktree):
        assert_worktree_pointer(self.source_root, worktree)
        paths = changed_paths(worktree)
        require(paths and all(path_allowed(path, task["allowed_paths"]) for path in paths),
                "out_of_scope", "worker changed no files or a path outside task ownership")
        auth_values = _auth_secret_values(self.spec["auth_file"])
        for path in paths:
            payload = _working_payload(worktree, path)
            if payload is not None:
                _scan_candidate_bytes(payload, auth_values)
        git(worktree, "add", "-A", "--", ".")
        staged = [os.fsdecode(item) for item in
                  git(worktree, "diff", "--cached", "--name-only", "-z").split(b"\0") if item]
        require(staged and all(path_allowed(path, task["allowed_paths"]) for path in staged),
                "out_of_scope", "staged paths differ from task ownership")
        for path in staged:
            payload = _staged_payload(worktree, path)
            if payload is not None:
                _scan_candidate_bytes(payload, auth_values)
        safe_env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
            "HOME": "/nonexistent", "GIT_TERMINAL_PROMPT": "0",
        }
        commit_argv = ["git", "-C", str(worktree),
                       "-c", "core.hooksPath=/dev/null",
                       "-c", "core.fsmonitor=false",
                       "-c", "commit.gpgsign=false",
                       "-c", "user.name=Conary Agent Runner",
                       "-c", "user.email=agent-runner@users.noreply.github.com",
                       "commit", "-m", task["commit_subject"]]
        try:
            committed = subprocess.run(commit_argv, env=safe_env, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, check=False, timeout=180)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RunnerError("local_unknown", "controller commit did not complete") from error
        require(committed.returncode == 0, "local_unknown",
                "controller commit failed with hooks and signing disabled")
        assert_worktree_pointer(self.source_root, worktree)
        require(not changed_paths(worktree), "stale_evidence",
                "candidate is dirty after controller commit")
        return {"head_sha": git_sha(worktree, "HEAD"),
                "tree_sha": git_sha(worktree, "HEAD^{tree}"),
                "base_sha": task["base_sha"]}

    def _proof(self, task, worktree, candidate, attempt):
        receipts = []
        if self.sandbox is None:
            self.sandbox = _load_sandbox()
        for check in task["checks"]:
            self._remaining()
            receipt_id = hashlib.sha256(
                f"{self.spec['run_id']}:{task['id']}:{attempt}:{check['id']}".encode()
            ).hexdigest()[:24]
            try:
                process = self.sandbox.run_proof(
                    worktree=worktree, proof_script=self.source_root / "scripts/agent-proof.py",
                    run_id=receipt_id, argv=check["argv"],
                    timeout_seconds=min(check["timeout_seconds"], self._remaining()),
                )
            except (OSError, RuntimeError, ValueError) as error:
                raise RunnerError("proof_failed", f"check {check['id']} did not finish") from error
            receipt_locator = f"target/agent-proof/{receipt_id}/receipt.json"
            receipt_file = _plain_relative_file(worktree, receipt_locator)
            try:
                receipt = json.loads(receipt_file.read_text(encoding="utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise RunnerError("proof_failed", f"check {check['id']} receipt is malformed") from error
            require(receipt.get("argv") == check["argv"] and
                    receipt.get("candidate_before") == receipt.get("candidate_after") and
                    receipt.get("candidate_before", {}).get("head") == candidate["head_sha"] and
                    receipt.get("candidate_before", {}).get("head_tree") == candidate["tree_sha"],
                    "stale_evidence", f"check {check['id']} receipt is stale")
            for stream in ("stdout", "stderr"):
                log = receipt.get(stream, {})
                locator = log.get("path")
                require(isinstance(locator, str) and not locator.startswith("/") and
                        ".." not in Path(locator).parts,
                        "stale_evidence", f"check {check['id']} log path is invalid")
                file_path = _plain_relative_file(worktree, locator)
                require(
                        file_path.stat().st_size == log.get("bytes") and
                        _digest_file(file_path) == log.get("sha256") and
                        log.get("verified") is True,
                        "stale_evidence", f"check {check['id']} log changed")
            receipts.append({"check_id": check["id"], "receipt_sha256": _digest_file(receipt_file),
                             "receipt_path": str(receipt_file)})
            require(process.ok and process.exit_code == 0 and receipt.get("status") == "passed" and
                    receipt.get("command_exit_code") == 0 and
                    receipt.get("output_complete") is True,
                    "proof_failed", f"check {check['id']} failed; receipt {receipt_file}")
        return receipts

    def _fresh_candidate_worktree(self, candidate, attempt, purpose):
        self._remaining()
        root = _owned_directory(self.spec["worktree_root"])
        path = root / f"{self.spec['run_id']}-{purpose}-{attempt}"
        require(not path.exists() and not path.is_symlink(), "unknown_outcome",
                f"{purpose} worktree already exists; inspect interrupted attempt")
        git(self.source_root, "worktree", "add", "--detach", str(path),
            candidate["head_sha"], timeout=min(120, self._remaining()))
        assert_worktree_pointer(self.source_root, path)
        require(git_sha(path, "HEAD") == candidate["head_sha"] and
                git_sha(path, "HEAD^{tree}") == candidate["tree_sha"] and
                not changed_paths(path) and not (path / "target").exists(),
                "stale_evidence", f"fresh {purpose} worktree is not clean")
        return path

    def _verify_evidence_worktree(self, candidate, key):
        raw = candidate.get(key)
        require(isinstance(raw, str) and Path(raw).is_absolute(),
                "stale_evidence", f"{key} is missing")
        path = Path(raw)
        require(path.resolve().is_relative_to(Path(self.spec["worktree_root"]).resolve()) and
                path.is_dir() and not path.is_symlink(),
                "stale_evidence", f"{key} escaped the owned worktree root")
        assert_worktree_pointer(self.source_root, path)
        require(git_sha(path, "HEAD") == candidate["head_sha"] and
                git_sha(path, "HEAD^{tree}") == candidate["tree_sha"] and
                not changed_paths(path), "stale_evidence",
                f"{key} changed after evidence collection")
        return path

    def _verify_frozen_review_inputs(self, task, worktree, candidate):
        require(isinstance(candidate, dict) and
                candidate.get("base_sha") == task["base_sha"] and
                git_sha(worktree, "HEAD") == candidate.get("head_sha") and
                git_sha(worktree, "HEAD^{tree}") == candidate.get("tree_sha") and
                not changed_paths(worktree), "stale_evidence",
                "local candidate differs from checkpoint")
        git(worktree, "merge-base", "--is-ancestor", task["base_sha"], "HEAD")
        require(candidate["head_sha"] != task["base_sha"], "stale_evidence",
                "candidate has no commit after base")
        proof_worktree = self._verify_evidence_worktree(candidate, "proof_worktree")
        self._verify_proof_evidence(task, candidate, proof_worktree)

    def _verify_candidate(self, task, worktree, candidate):
        self._verify_frozen_review_inputs(task, worktree, candidate)
        review = candidate.get("review")
        worker = candidate.get("worker")
        require(isinstance(review, dict) and isinstance(worker, dict) and
                review.get("session_id") and worker.get("session_id") and
                review["session_id"] != worker["session_id"],
                "stale_evidence", "independent review session is absent")
        for record in (worker, review):
            for kind in ("trace", "result"):
                path = record.get(f"{kind}_path")
                expected = record.get(f"{kind}_sha256")
                require(isinstance(path, str) and isinstance(expected, str) and
                        DIGEST.fullmatch(expected) is not None and
                        Path(path).is_file() and not Path(path).is_symlink() and
                        _digest_file(path) == expected,
                        "stale_evidence", f"{kind} record changed after review")
        self._verify_launch_evidence(task, worker, "worker", candidate)
        self._verify_launch_evidence(task, review, "reviewer", candidate)

    def _verify_proof_evidence(self, task, candidate, proof_worktree):
        receipts = candidate.get("proof")
        require(isinstance(receipts, list) and len(receipts) == len(task["checks"]),
                "stale_evidence", "focused proof receipt count differs")
        for record in receipts:
            path = record.get("receipt_path") if isinstance(record, dict) else None
            expected = record.get("receipt_sha256") if isinstance(record, dict) else None
            require(isinstance(path, str) and isinstance(expected, str) and
                    DIGEST.fullmatch(expected) is not None and
                    Path(path).is_relative_to(proof_worktree),
                    "stale_evidence", "focused proof receipt path is invalid")
            receipt_path = _plain_relative_file(
                proof_worktree, str(Path(path).relative_to(proof_worktree)))
            require(_digest_file(receipt_path) == expected,
                    "stale_evidence", "focused proof receipt changed")
            try:
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
                raise RunnerError("stale_evidence", "focused proof receipt is malformed") from error
            require(isinstance(receipt, dict) and receipt.get("status") == "passed" and
                    receipt.get("candidate_before") == receipt.get("candidate_after") and
                    receipt.get("candidate_before", {}).get("head") == candidate["head_sha"] and
                    receipt.get("candidate_before", {}).get("head_tree") == candidate["tree_sha"],
                    "stale_evidence", "focused proof no longer binds candidate")
            for stream in ("stdout", "stderr"):
                log = receipt.get(stream)
                require(isinstance(log, dict), "stale_evidence",
                        "focused proof log metadata is malformed")
                locator = log.get("path")
                require(isinstance(locator, str) and not locator.startswith("/") and
                        ".." not in Path(locator).parts,
                        "stale_evidence", "focused proof log path is invalid")
                log_path = _plain_relative_file(proof_worktree, locator)
                require(
                        log_path.stat().st_size == log.get("bytes") and
                        _digest_file(log_path) == log.get("sha256"),
                        "stale_evidence", "focused proof log changed")

    def _verify_launch_evidence(self, task, record, role, candidate):
        model = self.spec["models"][role]
        require(record.get("model_id") == model["id"] and
                record.get("reasoning_effort") == model["reasoning_effort"],
                "stale_evidence", f"{role} model differs from private spec")
        trace = Path(record["trace_path"])
        require(trace.stat().st_size < 16 * 1024 * 1024, "stale_evidence",
                f"{role} trace is oversized")
        try:
            rows = [json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines()]
            final = json.loads(Path(record["result_path"]).read_text(encoding="utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RunnerError("stale_evidence", f"{role} trace or result is malformed") from error
        require(len(rows) >= 3 and all(isinstance(row, dict) for row in rows) and
                rows[0].get("kind") == "launch" and
                rows[0].get("model") == model["id"] and
                rows[0].get("reasoning_effort") == model["reasoning_effort"] and
                rows[0].get("read_only_worktree") is (role == "reviewer") and
                rows[-1].get("kind") == "complete" and
                rows[-1].get("status") == "ok" and
                rows[-1].get("session_id") == record["session_id"] and
                any(row.get("kind") == "codex_event" and
                    row.get("type") == "thread.started" and
                    row.get("session_id") == record["session_id"] for row in rows),
                "stale_evidence", f"{role} trace lacks verified model/session completion")
        require(isinstance(final, dict) and final.get("schema_version") == 1 and
                final.get("run_id") == self.spec["run_id"] and
                final.get("task_id") == task["id"] and
                final.get("branch") == task["branch"] and
                final.get("status") == "ready", "stale_evidence",
                f"{role} final result differs from task")
        if role == "worker":
            require(final.get("candidate_head") is None and
                    final.get("candidate_tree") is None, "stale_evidence",
                    "worker claimed a candidate before commit")
        else:
            require(final.get("candidate_head") == candidate["head_sha"] and
                    final.get("candidate_tree") == candidate["tree_sha"] and
                    record.get("approved") is True and
                    record.get("head_sha") == candidate["head_sha"] and
                    record.get("tree_sha") == candidate["tree_sha"],
                    "stale_evidence", "review result does not approve exact candidate")

    def _build_candidate(self, task, journal, worktree):
        failure = None
        worker = None
        for attempt in range(self.spec["limits"]["causal_repairs"] + 1):
            self._remaining()
            journal["phase"] = "worker_started"
            journal["attempt"] = attempt
            write_journal(self.spec, journal)
            worker = self._launch(task, worktree, "worker", attempt, prior_failure=failure)
            candidate = self._commit(task, worktree)
            candidate["worker"] = worker
            candidate["repair_count"] = attempt
            try:
                proof_worktree = self._fresh_candidate_worktree(candidate, attempt, "proof")
                candidate["proof_worktree"] = str(proof_worktree)
                candidate["proof"] = self._proof(task, proof_worktree, candidate, attempt)
                self._verify_evidence_worktree(candidate, "proof_worktree")
                for review_retry in range(MAX_REVIEW_RESULT_RETRIES + 1):
                    self._remaining()
                    self._verify_frozen_review_inputs(task, worktree, candidate)
                    try:
                        reviewer = self._launch(task, proof_worktree, "reviewer", attempt,
                                                candidate=candidate,
                                                review_retry=review_retry)
                        break
                    except RunnerError as error:
                        if (error.code != "invalid_reviewer_result" or
                                review_retry >= MAX_REVIEW_RESULT_RETRIES):
                            raise
                candidate["review"] = {
                    **reviewer, "approved": True,
                    "head_sha": candidate["head_sha"], "tree_sha": candidate["tree_sha"],
                }
                self._verify_candidate(task, worktree, candidate)
                return candidate
            except RunnerError as error:
                if (error.code not in {"proof_failed", "child_blocked"} or
                        attempt >= self.spec["limits"]["causal_repairs"]):
                    raise
                failure = f"{error.code}: {error}"
                # A repair must produce a new commit. The next pass never reruns
                # unchanged failed proof on the same HEAD.
        raise RunnerError("internal", "repair loop ended without a decision")

    def _issue_candidate_checkpoint(self, task, journal):
        candidate = journal["candidate"]
        comments = self.gh.issue_comments(self.spec["issue_number"])
        event = _latest_run_event(comments, self.spec["run_id"], task["id"])
        if event and event[1]["phase"] in {"candidate", "draft", "observed"}:
            require(event[1]["head_sha"] == candidate["head_sha"] and
                    event[1]["tree_sha"] == candidate["tree_sha"],
                    "ambiguous_graph", "issue candidate differs from local checkpoint")
            return
        self._append_checkpoint(task, "candidate", journal, "push_draft_pr",
                                candidate["head_sha"], candidate["tree_sha"])

    def _push_candidate(self, task, journal, worktree):
        assert_worktree_pointer(self.source_root, worktree)
        self._verify_origin()
        candidate = journal["candidate"]
        remote = self.gh.branch_sha(task["branch"])
        if remote == candidate["head_sha"]:
            return
        require(remote == task["base_sha"], "remote_unknown",
                "task branch moved before candidate push")
        self._before_write(task, "update_branch", task["base_sha"], journal)
        journal["phase"] = "push_intent"
        write_journal(self.spec, journal)
        require(self.gh.pulls_for_branch(task["branch"]) is None, "occupied_task",
                "a PR appeared on the task branch before push")
        timeout = min(180, self._remaining())
        lease = f"--force-with-lease=refs/heads/{task['branch']}:{task['base_sha']}"
        git(worktree, "push", lease,
            f"https://github.com/{self.spec['repository']}.git",
            f"HEAD:refs/heads/{task['branch']}", timeout=timeout, credentialed=True)
        require(self.gh.branch_sha(task["branch"]) == candidate["head_sha"],
                "remote_unknown", "task branch did not reach candidate HEAD")
        journal["phase"] = "candidate"
        write_journal(self.spec, journal)

    def _ensure_draft(self, task, journal):
        candidate = journal["candidate"]
        pr = self.gh.pulls_for_branch(task["branch"])
        marker = (f"<!-- conary-agent-pr:v1 run={self.spec['run_id']} "
                  f"task={task['id']} head={candidate['head_sha']} -->")
        if pr is None:
            self._before_write(task, "create_draft_pr", candidate["head_sha"], journal)
            body = (f"Refs #{self.spec['issue_number']}\n\n"
                    f"Bounded task `{task['id']}` from the issue graph. "
                    f"Candidate `{candidate['head_sha']}`, tree `{candidate['tree_sha']}`. "
                    "The issue records proof and review digests; the private run journal "
                    "holds their local locators.\n\n"
                    f"{marker}")
            journal["phase"] = "draft_intent"
            write_journal(self.spec, journal)
            self._remaining()
            pr = self.gh.draft_pr(task, body)
        require(isinstance(pr, dict) and type(pr.get("number")) is int and
                pr.get("state") == "open" and pr.get("draft") is True and
                isinstance(pr.get("user"), dict) and
                pr["user"].get("login") == self.spec["controller_actor"] and
                isinstance(pr.get("body"), str) and marker in pr["body"] and
                pr.get("head", {}).get("sha") == candidate["head_sha"] and
                pr.get("base", {}).get("ref") == "main",
                "remote_unknown", "draft PR is missing or differs from this run")
        journal["pr_number"] = pr["number"]
        if journal.get("phase") != "observed":
            journal["phase"] = "draft"
        write_journal(self.spec, journal)
        comments = self.gh.issue_comments(self.spec["issue_number"])
        event = _latest_run_event(comments, self.spec["run_id"], task["id"])
        if event is None or event[1]["phase"] not in {"draft", "observed"}:
            self._append_checkpoint(task, "draft", journal, "observe_hosted_checks",
                                    candidate["head_sha"], candidate["tree_sha"])
        return pr

    def _ensure_observed_checkpoint(self, task, journal):
        candidate = journal["candidate"]
        comments = self.gh.issue_comments(self.spec["issue_number"])
        event = _latest_run_event(comments, self.spec["run_id"], task["id"])
        if event is not None and event[1]["phase"] == "observed":
            require(event[1]["head_sha"] == candidate["head_sha"] and
                    event[1]["tree_sha"] == candidate["tree_sha"] and
                    event[1]["check_ids"] == candidate["check_ids"],
                    "ambiguous_graph", "observed issue checkpoint differs from candidate")
            return
        require(event is not None and event[1]["phase"] == "draft",
                "ambiguous_graph", "draft checkpoint is missing before observed checkpoint")
        self._append_checkpoint(task, "observed", journal,
                                "review_draft_and_run_promotion_check",
                                candidate["head_sha"], candidate["tree_sha"],
                                candidate["check_ids"])

    def _observe(self, task, journal):
        candidate = journal["candidate"]
        sha = candidate["head_sha"]
        rules = self.gh.required_rules()
        required = {(item["context"], item["app_id"])
                    for item in self.spec["required_checks"]}
        require(_rule_checks(rules) == required,
                "unknown_rules", "required checks changed since run spec was approved")
        while True:
            self._remaining()
            runs = self.gh.workflow_runs(sha)
            checks = self.gh.check_runs(sha)
            current = {}
            for run in runs:
                require(isinstance(run, dict) and type(run.get("workflow_id")) is int and
                        type(run.get("id")) is int and type(run.get("run_number")) is int and
                        run.get("head_sha") == sha,
                        "remote_unknown", "hosted workflow run is malformed")
                _validate_hosted_state(run, "hosted workflow run")
                key = run["workflow_id"]
                prior = current.get(key)
                if prior is None or (run["run_number"], run.get("run_attempt", 0)) > (
                        prior["run_number"], prior.get("run_attempt", 0)):
                    current[key] = run
            for check in checks:
                require(isinstance(check, dict) and type(check.get("id")) is int and
                        isinstance(check.get("name"), str) and
                        check.get("head_sha") == sha and
                        isinstance(check.get("app"), dict) and
                        type(check["app"].get("id")) is int,
                        "remote_unknown", "hosted check is malformed")
                _validate_hosted_state(check, "hosted check")
            matching = {}
            for check in checks:
                if not isinstance(check, dict) or check.get("head_sha") != sha:
                    raise RunnerError("remote_unknown", "hosted check has a different head")
                app = check.get("app")
                require(isinstance(app, dict) and type(app.get("id")) is int,
                        "remote_unknown", "hosted check app is malformed")
                key = (check.get("name"), app["id"])
                if key in required:
                    matching.setdefault(key, []).append(check)
            failed = [run for run in current.values()
                      if run.get("status") == "completed" and
                      run.get("conclusion") != "success"]
            if any(run.get("conclusion") == "action_required" for run in failed):
                raise RunnerError("action_required",
                                  "hosted workflow requires an authorized action")
            for check in checks:
                if check.get("conclusion") == "action_required":
                    key = (check["name"], check["app"]["id"])
                    kind = "required" if key in required else "optional"
                    raise RunnerError("action_required",
                                      f"{kind} check {key[0]} requires action")
            for key, copies in matching.items():
                require(len(copies) == 1, "remote_unknown", "required check is ambiguous")
            for key, copies in matching.items():
                check = copies[0]
                if check.get("status") == "completed" and check.get("conclusion") != "success":
                    raise RunnerError("hosted_failure", f"required check {key[0]} failed")
            required_success = (set(matching) == required and
                                all(copies[0].get("status") == "completed" and
                                    copies[0].get("conclusion") == "success"
                                    for copies in matching.values()))
            if not required_success:
                time.sleep(min(20, self._remaining()))
                continue
            if failed:
                if len(failed) == 1:
                    detail = optional_pretest_image_failure(
                        self.gh.workflow_jobs(failed[0]["id"]))
                    if detail is not None:
                        raise RunnerError(
                            "optional_pretest_image_failure",
                            f"openSUSE image acquisition failed in job {detail['opensuse_job_id']} "
                            f"step {detail['failed_step']}; product steps were skipped; "
                            f"aggregate job {detail['aggregate_job_id']} failed")
                raise RunnerError("hosted_failure",
                                  "hosted workflow failed or requires action on candidate head")
            if not current or any(run["status"] != "completed" for run in current.values()):
                time.sleep(min(20, self._remaining()))
                continue
            for check in checks:
                key = (check["name"], check["app"]["id"])
                if key not in required and check.get("status") == "completed" and \
                        check.get("conclusion") != "success":
                    raise RunnerError("hosted_failure", "optional hosted check failed")
            ready = (bool(current) and
                     all(run.get("status") == "completed" for run in current.values()) and
                     all(check.get("status") == "completed" and
                         check.get("conclusion") == "success" for check in checks))
            if ready:
                ids = sorted(copies[0]["id"] for copies in matching.values())
                candidate["check_ids"] = ids
                journal["phase"] = "observed"
                write_journal(self.spec, journal)
                self._ensure_observed_checkpoint(task, journal)
                return {"status": "draft_checks_passed", "run_id": self.spec["run_id"],
                        "task_id": task["id"], "pr_number": journal["pr_number"],
                        "head_sha": sha, "tree_sha": candidate["tree_sha"],
                        "check_ids": ids}
            time.sleep(min(20, self._remaining()))

    def _run_inner(self):
        # All cheap local preflight happens before the first GitHub write.
        require(self.spec.get("auth_file") is not None, "missing_input",
                "sandboxed model run needs an explicit private auth_file")
        _auth_secret_values(self.spec["auth_file"])
        require(shutil.which("bwrap") and shutil.which("codex"), "missing_input",
                "bubblewrap or Codex CLI is unavailable")
        self.sandbox = self.sandbox or _load_sandbox()
        self._check_actor()
        state = self._remote_state()
        task = state["task"]
        require(task is not None, "no_ready_task", "no ready issue graph task matches the queue")
        _prompt_text(task)
        require(state["main_sha"] == task["base_sha"], "stale_base",
                "main moved after graph task was prepared")
        journal = read_journal(self.spec)
        _validate_remote_checkpoint(self.spec, task, state["comments"], journal)
        if journal is None:
            require(state["branch_sha"] is None and state["pr"] is None,
                    "occupied_task", "task branch or PR exists without owned recovery pointer")
            started = datetime.now(timezone.utc)
            deadline = started + timedelta(seconds=self.spec["limits"]["wall_seconds"])
            journal = {"task_id": task["id"], "phase": "branch_intent",
                       "started_at": started.isoformat().replace("+00:00", "Z"),
                       "deadline_at": deadline.isoformat().replace("+00:00", "Z")}
            write_journal(self.spec, journal)
        if journal["phase"] == "branch_intent":
            require(state["branch_sha"] is None, "unknown_outcome",
                    "branch appeared after an interrupted create; inspect owner before resuming")
            self._before_write(task, "create_branch", None, journal)
            self._remaining()
            self.gh.create_branch(task["branch"], task["base_sha"])
            require(self.gh.branch_sha(task["branch"]) == task["base_sha"],
                    "remote_unknown", "new task branch is not at selected base")
            journal["phase"] = "branch_created"
            write_journal(self.spec, journal)
        if journal["phase"] == "branch_created":
            require(self.gh.branch_sha(task["branch"]) == task["base_sha"],
                    "remote_unknown", "claimed branch changed before issue checkpoint")
            event = _latest_run_event(self.gh.issue_comments(self.spec["issue_number"]),
                                      self.spec["run_id"], task["id"])
            if event is None:
                self._append_checkpoint(task, "claimed", journal, "implement")
            else:
                require(event[1]["phase"] == "claimed", "ambiguous_graph",
                        "unexpected checkpoint while claiming task")
            journal["phase"] = "claimed"
            write_journal(self.spec, journal)
        if journal["phase"] == "worker_started":
            raise RunnerError("unknown_outcome",
                              "child attempt was interrupted; inspect trace and worktree before resuming")
        worktree = self._worktree(task, journal)
        if journal["phase"] == "claimed":
            candidate = self._build_candidate(task, journal, worktree)
            journal["candidate"] = candidate
            journal["phase"] = "candidate"
            write_journal(self.spec, journal)
        else:
            candidate = journal.get("candidate")
            self._verify_candidate(task, worktree, candidate)
        self._issue_candidate_checkpoint(task, journal)
        self._push_candidate(task, journal, worktree)
        self._ensure_draft(task, journal)
        if journal["phase"] == "observed":
            self._ensure_observed_checkpoint(task, journal)
            return {"status": "already_observed", "run_id": self.spec["run_id"],
                    "task_id": task["id"], "pr_number": journal["pr_number"]}
        return self._observe(task, journal)

    def run(self):
        try:
            self._check_private_mount_boundaries()
            return self._run_inner()
        except RunnerError as error:
            self._record_bounded_stop(error)
            raise

    def _record_bounded_stop(self, error):
        recordable = {
            "child_blocked", "model_unavailable", "proof_failed", "hosted_failure",
            "optional_pretest_image_failure", "wall_budget", "out_of_scope",
            "unknown_outcome", "action_required", "secret_in_candidate",
            "invalid_reviewer_result",
        }
        if error.code not in recordable:
            return
        try:
            journal = read_journal(self.spec)
            if journal is None or journal.get("phase") in {"branch_intent", "blocked", "observed"}:
                return
            task = next((item for item in self.spec["queue"]
                         if item["id"] == journal.get("task_id")), None)
            if task is None:
                return
            candidate = journal.get("candidate") or {}
            next_action = ("inspect_worker_attempt" if error.code == "unknown_outcome"
                           else "await_authorized_action" if error.code == "action_required"
                           else f"investigate_{error.code}")
            self._append_checkpoint(task, "blocked", journal,
                                    next_action,
                                    candidate.get("head_sha"), candidate.get("tree_sha"),
                                    candidate.get("check_ids", []))
            journal["phase"] = "blocked"
            journal["blocked_code"] = error.code
            journal["blocked_reason"] = str(error)
            write_journal(self.spec, journal)
        except RunnerError:
            # Preserve the causal error; an ambiguous checkpoint remains a
            # supervised recovery stop rather than an automatic retry.
            return

    def promotion_check(self, number):
        positive(number, "pull request number")
        journal = read_journal(self.spec)
        require(journal is not None and journal.get("phase") == "observed" and
                journal.get("pr_number") == number,
                "stale_evidence", "observed candidate checkpoint is missing")
        issue = self.gh.issue(self.spec["issue_number"])
        validate_issue_graph_index(self.spec, issue)
        comments = self.gh.issue_comments(self.spec["issue_number"])
        task = select_task(self.spec, comments, observed_task_id=journal.get("task_id"))
        require(task is not None, "stale_evidence", "observed task is absent from issue graph")
        _validate_remote_checkpoint(self.spec, task, comments, journal)
        event = _latest_run_event(comments, self.spec["run_id"], task["id"])
        candidate = journal.get("candidate")
        require(event is not None and event[1]["phase"] == "observed" and
                event[1]["head_sha"] == candidate.get("head_sha") and
                event[1]["tree_sha"] == candidate.get("tree_sha") and
                event[1]["check_ids"] == candidate.get("check_ids"),
                "stale_evidence", "issue lacks exact observed candidate receipt")
        worktree = self._worktree(task, journal)
        self._verify_candidate(task, worktree, candidate)
        pr = self.gh.pr(number)
        require(isinstance(pr, dict) and pr.get("head", {}).get("ref") == task["branch"] and
                isinstance(pr.get("user"), dict) and
                pr["user"].get("login") == self.spec["controller_actor"] and
                isinstance(pr.get("body"), str) and
                f"<!-- conary-agent-pr:v1 run={self.spec['run_id']} task={task['id']} "
                f"head={candidate['head_sha']} -->" in pr["body"],
                "remote_unknown", "pull request identity differs from run")
        merge_sha = self.gh.potential_merge_sha(number)
        require(isinstance(merge_sha, str) and SHA.fullmatch(merge_sha) is not None,
                "remote_unknown", "GitHub test-merge commit SHA is malformed")
        if "merge_commit_sha" in pr:
            rest_sha = pr["merge_commit_sha"]
            require(isinstance(rest_sha, str) and SHA.fullmatch(rest_sha) is not None,
                    "remote_unknown", "REST test-merge commit SHA is malformed")
            require(rest_sha == merge_sha, "remote_unknown",
                    "REST and GraphQL test-merge commits disagree")
        merge_data = self.gh.commit_data(merge_sha)
        pr["test_merge_tree_sha"] = merge_data["tree_sha"]
        pr["test_merge_parents"] = merge_data["parents"]
        rules = self.gh.required_rules()
        statuses = self.gh.check_runs(candidate["head_sha"])
        threads = self.gh.review_threads(number)
        decision = evaluate_promotion(self.spec, pr, self.gh.main_sha(), rules,
                                      statuses, threads, candidate)
        return {
            "status": "ready" if decision["ready"] else "blocked",
            "run_id": self.spec["run_id"], "task_id": task["id"],
            "pr_number": number, "head_sha": candidate["head_sha"],
            "tree_sha": candidate["tree_sha"], "reasons": decision["reasons"],
        }


def _cli():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "dry-run", "run", "promotion-check"))
    parser.add_argument("--spec", required=True, help="absolute path to a private 0600 JSON spec")
    parser.add_argument("--pr", type=int, help="pull request number for promotion-check")
    args = parser.parse_args()
    try:
        spec = load_spec(args.spec)
        source_root = Path(__file__).resolve().parents[1]
        common = Path(git(source_root, "rev-parse", "--path-format=absolute",
                          "--git-common-dir").decode().strip()).resolve()
        require(not Path(args.spec).resolve().is_relative_to(common), "invalid_spec",
                "private spec overlaps Git metadata mounted to child")
        if args.command == "validate":
            Controller(spec)._check_private_mount_boundaries()
            result = {"status": "valid", "run_id": spec["run_id"],
                      "queue_length": len(spec["queue"])}
        else:
            with run_lock(spec["journal_dir"], source_root):
                controller = Controller(spec)
                controller._check_private_mount_boundaries()
                if args.command == "dry-run":
                    result = controller.dry_run()
                elif args.command == "run":
                    result = controller.run()
                else:
                    require(args.pr is not None, "invalid_spec",
                            "promotion-check requires --pr")
                    result = controller.promotion_check(args.pr)
        print(_json_line(result))
        return 0 if result["status"] not in {"blocked", "no_ready_task"} else 2
    except RunnerError as error:
        print(_json_line({"status": "blocked", "code": error.code,
                          "reason": str(error)}))
        return 2


if __name__ == "__main__":
    sys.exit(_cli())
