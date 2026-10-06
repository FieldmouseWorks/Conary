# scripts/test-agent-runner.py
"""Independent controls for the bounded workflow runner."""

import importlib.util
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("agent-runner.py")
SPEC = importlib.util.spec_from_file_location("agent_runner", SCRIPT)
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)

BASE = "a" * 40
HEAD = "b" * 40
TREE = "c" * 40


def journal_times(started=None):
    started = started or datetime.now(timezone.utc)
    return {
        "started_at": started.isoformat().replace("+00:00", "Z"),
        "deadline_at": (started + timedelta(seconds=3600)).isoformat().replace("+00:00", "Z"),
    }


def task():
    return {
        "id": "TNPM18-repository-row",
        "graph_comment_id": 42,
        "graph_author": "runner-test",
        "base_sha": BASE,
        "branch": "agent/1070-typed-update-repository-row",
        "prompt_sha256": "d" * 64,
        "allowed_paths": [
            "apps/conary/tests/integration/remi/manifests/phase4-native-daily-driver-corpus.toml"
        ],
        "checks": [
            {"id": "focused", "argv": ["cargo", "test", "-p", "conary-test"], "timeout_seconds": 900}
        ],
    }


def spec():
    return {
        "schema_version": 1,
        "repository": "FieldmouseWorks/Conary",
        "issue_number": 1070,
        "controller_actor": "runner-test",
        "queue": [task()],
        "required_checks": [
            {"context": context, "app_id": 15368}
            for context in ("fmt", "clippy", "workspace-tests", "docs-truth", "frontends")
        ],
        "models": {
            "allowlist": ["gpt-6-sol", "gpt-6-luna"],
            "worker": {"id": "gpt-6-sol", "reasoning_effort": "max"},
            "reviewer": {"id": "gpt-6-sol", "reasoning_effort": "max"},
        },
    }


def complete_spec():
    value = spec()
    value.update(
        {
            "run_id": "pilot-1070-1",
            "authorization": {
                "source": "Owner's 2026-10-03 bounded unattended workflow request",
                "expires_at": "2099-01-01T00:00:00Z",
                "remote_writes": ["create_branch", "update_branch", "comment_issue",
                                  "create_draft_pr"],
                "merge": False,
            },
            "exclusions": [1093],
            "models": {
                "allowlist": ["gpt-6-sol", "gpt-6-luna"],
                "worker": {"id": "gpt-6-sol", "reasoning_effort": "max"},
                "reviewer": {"id": "gpt-6-sol", "reasoning_effort": "max"},
            },
            "limits": {"wall_seconds": 3600, "causal_repairs": 2},
            "worktree_root": "/tmp/conary-runner-pilot/worktrees",
            "journal_dir": "/tmp/conary-runner-pilot/journal",
        }
    )
    value["queue"] = [
        {
            **task(),
            "prompt_file": "/tmp/conary-runner-pilot/prompt.txt",
            "commit_subject": "test(integration): pin update repository row",
        }
    ]
    return value


def graph_comment(state="ready", base=BASE, acceptance=None, task_id="TNPM18-repository-row"):
    if acceptance is None:
        acceptance = RUNNER.acceptance_sha256(task())
    marker = {
        "id": task_id,
        "state": state,
        "base_sha": base,
        "acceptance_sha256": acceptance,
        "next_action": "implement",
    }
    return {
        "id": 42,
        "user": {"login": "runner-test"},
        "body": "<!-- conary-agent-node:v1 " + json.dumps(marker, separators=(",", ":")) + " -->",
    }


def checkpoint_comment(phase="candidate", head=HEAD, tree=TREE):
    return {
        "id": 43,
        "user": {"login": "runner-test"},
        "body": RUNNER.run_marker("pilot-1070-1", task(), phase, head, tree,
                                  next_action="observe hosted checks"),
    }


def promotion_inputs():
    current = spec()
    checks = [
        {
            "id": index,
            "name": check["context"],
            "head_sha": HEAD,
            "status": "completed",
            "conclusion": "success",
            "app": {"id": check["app_id"]},
        }
        for index, check in enumerate(current["required_checks"], start=1)
    ]
    rules = [
        {
            "type": "required_status_checks",
            "parameters": {
                "required_status_checks": [
                    {"context": check["context"], "integration_id": check["app_id"]}
                    for check in current["required_checks"]
                ]
            },
        },
        {"type": "pull_request", "parameters": {"required_review_thread_resolution": True}},
    ]
    pr = {
        "number": 1184,
        "state": "open",
        "draft": False,
        "head": {"sha": HEAD},
        "base": {"sha": BASE, "ref": "main"},
        "mergeable": True,
        "mergeable_state": "clean",
        "test_merge_tree_sha": TREE,
        "test_merge_parents": [BASE, HEAD],
    }
    candidate = {
        "base_sha": BASE,
        "head_sha": HEAD,
        "tree_sha": TREE,
        "check_ids": [check["id"] for check in checks],
        "worker": {"session_id": "worker-1"},
        "review": {
            "approved": True,
            "head_sha": HEAD,
            "tree_sha": TREE,
            "model_id": "gpt-6-sol",
            "reasoning_effort": "max",
            "session_id": "review-1",
            "trace_sha256": "d" * 64,
            "result_sha256": "e" * 64,
        },
    }
    return current, pr, BASE, rules, checks, [{"isResolved": True}], candidate


class SelectionControls(unittest.TestCase):
    def test_matching_ready_node_is_selected(self):
        self.assertEqual(RUNNER.select_task(spec(), [graph_comment()]), task())

    def test_no_ready_node_does_not_dispatch(self):
        self.assertIsNone(RUNNER.select_task(spec(), [graph_comment(state="verified")]))

    def test_stale_base_and_changed_acceptance_stop(self):
        for comment in (
            graph_comment(base="e" * 40),
            graph_comment(acceptance="f" * 64),
        ):
            with self.subTest(comment=comment):
                with self.assertRaises(RUNNER.RunnerError):
                    RUNNER.select_task(spec(), [comment])

    def test_duplicate_ready_markers_stop(self):
        comments = [graph_comment(), {**graph_comment(), "id": 43}]
        with self.assertRaises(RUNNER.RunnerError):
            RUNNER.select_task(spec(), comments)

    def test_untrusted_graph_author_stops_dispatch(self):
        comment = graph_comment()
        comment["user"]["login"] = "random-commenter"
        with self.assertRaises(RUNNER.RunnerError):
            RUNNER.select_task(spec(), [comment])

    def test_working_node_on_approval_hold_does_not_resume(self):
        held = graph_comment(state="working")
        held["body"] = held["body"].replace('"next_action":"implement"',
                                            '"next_action":"await_approval"')
        self.assertIsNone(RUNNER.select_task(complete_spec(),
                                            [held, checkpoint_comment("claimed", None, None)]))

    def test_observed_task_can_be_verified_but_cannot_override_a_hold(self):
        value = complete_spec()
        observed = checkpoint_comment("observed")
        selected = RUNNER.select_task(value, [graph_comment(state="verified"), observed],
                                      observed_task_id=task()["id"])
        self.assertEqual(selected, value["queue"][0])
        for state, next_action in (("working", "await_approval"),
                                   ("working", "stop"),
                                   ("blocked", "await_approval")):
            held = graph_comment(state=state)
            held["body"] = held["body"].replace('"next_action":"implement"',
                                                f'"next_action":"{next_action}"')
            with self.subTest(state=state, next_action=next_action):
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    RUNNER.select_task(value, [held, observed],
                                       observed_task_id=task()["id"])
                self.assertEqual(caught.exception.code, "blocked_task")


class EnvelopeControls(unittest.TestCase):
    def test_explicit_bounded_envelope_is_valid(self):
        self.assertEqual(RUNNER.validate_spec(complete_spec())["schema_version"], 1)

    def test_reasoning_effort_is_pinned_by_run_not_repository_policy(self):
        value = complete_spec()
        value["models"]["worker"]["reasoning_effort"] = "high"
        self.assertEqual(RUNNER.validate_spec(value)["models"]["worker"]["reasoning_effort"],
                         "high")
        value["models"]["worker"]["reasoning_effort"] = "bogus"
        with self.assertRaises(RUNNER.RunnerError):
            RUNNER.validate_spec(value)

    def test_expired_or_merge_grant_cannot_dispatch(self):
        for change in (
            lambda value: value["authorization"].update(expires_at="2000-01-01T00:00:00Z"),
            lambda value: value["authorization"].update(merge=True),
        ):
            value = deepcopy(complete_spec())
            change(value)
            with self.subTest(value=value["authorization"]):
                with self.assertRaises(RUNNER.RunnerError):
                    RUNNER.validate_spec(value)

    def test_missing_model_effort_or_bounded_time_cannot_dispatch(self):
        for change in (
            lambda value: value["models"]["worker"].pop("reasoning_effort"),
            lambda value: value["limits"].update(wall_seconds=0),
        ):
            value = deepcopy(complete_spec())
            change(value)
            with self.assertRaises(RUNNER.RunnerError):
                RUNNER.validate_spec(value)

    def test_path_escape_cannot_be_authorized(self):
        value = complete_spec()
        value["queue"][0]["allowed_paths"] = ["../outside-repository"]
        with self.assertRaises(RUNNER.RunnerError):
            RUNNER.validate_spec(value)

    def test_private_spec_loader_rejects_group_readable_and_symlink(self):
        with tempfile.TemporaryDirectory(prefix="runner-spec-control-") as root:
            path = Path(root) / "run.json"
            path.write_text(json.dumps(complete_spec()), encoding="utf-8")
            path.chmod(0o600)
            self.assertEqual(RUNNER.load_spec(path)["schema_version"], 1)
            path.chmod(0o640)
            with self.assertRaises(RUNNER.RunnerError):
                RUNNER.load_spec(path)
            path.chmod(0o600)
            link = Path(root) / "link.json"
            link.symlink_to(path)
            with self.assertRaises(RUNNER.RunnerError):
                RUNNER.load_spec(link)

    def test_private_spec_rejects_duplicate_authority_keys(self):
        with tempfile.TemporaryDirectory(prefix="runner-duplicate-spec-") as root:
            path = Path(root) / "run.json"
            valid = json.dumps(complete_spec())
            for duplicate in (
                valid.replace('"repository": "FieldmouseWorks/Conary",',
                              '"repository": "FieldmouseWorks/Conary", '
                              '"repository": "other/Conary",', 1),
                valid.replace('"merge": false', '"merge": false, "merge": true', 1),
            ):
                with self.subTest(duplicate=duplicate[:100]):
                    path.write_text(duplicate, encoding="utf-8")
                    path.chmod(0o600)
                    with self.assertRaises(RUNNER.RunnerError) as caught:
                        RUNNER.load_spec(path)
                    self.assertEqual(caught.exception.code, "invalid_spec")

    def test_gh_api_pins_public_host_despite_ambient_gh_host(self):
        invocations = []

        def fake_run(argv, **_kwargs):
            invocations.append(argv)
            if "graphql" in argv:
                if any("potentialMergeCommit" in arg for arg in argv):
                    body = {"data": {"repository": {"nameWithOwner": "FieldmouseWorks/Conary",
                                                    "pullRequest": {"number": 1,
                                                                    "potentialMergeCommit": {
                                                                        "oid": "f" * 40}}}}}
                else:
                    body = {"data": {"repository": {"pullRequest": {"reviewThreads": {
                        "nodes": [], "pageInfo": {"hasNextPage": False}
                    }}}}}
                return SimpleNamespace(returncode=0, stdout=json.dumps(body).encode())
            if "user" in argv:
                return SimpleNamespace(returncode=0, stdout=b"runner-test\n")
            return SimpleNamespace(returncode=0, stdout=b"HTTP/2 200 OK\n\n{}")

        with patch.dict(RUNNER.os.environ, {"GH_HOST": "enterprise.invalid"}), \
                patch.object(RUNNER.subprocess, "run", side_effect=fake_run):
            gh = RUNNER.GitHub("FieldmouseWorks/Conary")
            self.assertEqual(gh.authenticated_actor(), "runner-test")
            self.assertEqual(gh.request("GET", "issues/1070"), {})
            self.assertEqual(gh.review_threads(1), [])
            self.assertEqual(gh.potential_merge_sha(1), "f" * 40)
        self.assertEqual(len(invocations), 4)
        for argv in invocations:
            self.assertEqual(argv[argv.index("--hostname") + 1], "github.com")

    def test_graphql_test_merge_oid_requires_exact_pr_identity_and_valid_sha(self):
        def response(repository="FieldmouseWorks/Conary", number=99, oid="f" * 40):
            return {"data": {"repository": {"nameWithOwner": repository,
                                            "pullRequest": {"number": number,
                                                            "potentialMergeCommit":
                                                                None if oid is None else {"oid": oid}}}}}

        cases = [
            response(oid=None),
            response(oid="not-a-sha"),
            response(repository="other/Conary"),
            response(number=100),
            {"errors": [{"message": "merge calculation failed"}], **response()},
            {"data": {"repository": None}},
        ]
        for body in cases:
            with self.subTest(body=body):
                raw = json.dumps(body).encode()
                with patch.object(RUNNER.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stdout=raw)) as run:
                    with self.assertRaises(RUNNER.RunnerError) as caught:
                        RUNNER.GitHub("FieldmouseWorks/Conary").potential_merge_sha(99)
                self.assertEqual(caught.exception.code, "remote_unknown")
                argv = run.call_args.args[0]
                self.assertEqual(argv[:4], ["gh", "api", "graphql", "--hostname"])
                self.assertEqual(argv[4], "github.com")
                self.assertNotIn("--method", argv)

        duplicate = b'{"data":{"repository":{"nameWithOwner":"FieldmouseWorks/Conary",' \
                    b'"pullRequest":{"number":99,"potentialMergeCommit":{"oid":"' \
                    + b'f' * 40 + b'","oid":"' + b'e' * 40 + b'"}}}}}'
        with patch.object(RUNNER.subprocess, "run", return_value=SimpleNamespace(
                returncode=0, stdout=duplicate)):
            with self.assertRaises(RUNNER.RunnerError) as caught:
                RUNNER.GitHub("FieldmouseWorks/Conary").potential_merge_sha(99)
        self.assertEqual(caught.exception.code, "remote_unknown")

    def test_commit_lookup_binds_response_to_graphql_oid(self):
        gh = RUNNER.GitHub("FieldmouseWorks/Conary")
        commit = {"sha": "e" * 40, "tree": {"sha": TREE},
                  "parents": [{"sha": BASE}, {"sha": HEAD}]}
        with patch.object(gh, "request", return_value=commit) as request:
            with self.assertRaises(RUNNER.RunnerError) as caught:
                gh.commit_data("f" * 40)
        self.assertEqual(caught.exception.code, "remote_unknown")
        request.assert_called_once_with("GET", "git/commits/" + "f" * 40)

    def test_private_inputs_and_journal_cannot_overlap_child_mount(self):
        base = complete_spec()
        root = base["worktree_root"]
        for change in (
            lambda value: value.update(journal_dir=f"{root}/journal"),
            lambda value: value.update(journal_dir="/tmp/conary-runner-pilot"),
            lambda value: value.update(auth_file=f"{root}/auth.json"),
            lambda value: value["queue"][0].update(prompt_file=f"{root}/prompt.txt"),
        ):
            value = deepcopy(base)
            change(value)
            with self.subTest(value=value):
                with self.assertRaises(RUNNER.RunnerError):
                    RUNNER.validate_spec(value)

    def test_private_spec_file_cannot_be_loaded_from_child_mount(self):
        with tempfile.TemporaryDirectory(prefix="runner-spec-location-") as root:
            value = complete_spec()
            value["worktree_root"] = str(Path(root) / "worktrees")
            value["journal_dir"] = str(Path(root) / "journal")
            mount = Path(value["worktree_root"])
            mount.mkdir()
            private_spec = mount / "run.json"
            private_spec.write_text(json.dumps(value), encoding="utf-8")
            private_spec.chmod(0o600)
            with self.assertRaises(RUNNER.RunnerError):
                RUNNER.load_spec(private_spec)


class RecoveryControls(unittest.TestCase):
    def test_closed_or_unindexed_issue_cannot_dispatch(self):
        class FakeGitHub:
            def __init__(self, state, body):
                self.state = state
                self.body = body
                self.comments_read = 0

            def issue(self, _number):
                return {"number": 1070, "state": self.state, "body": self.body}

            def issue_comments(self, _number):
                self.comments_read += 1
                return [graph_comment()]

        indexed = "<!-- conary-agent-graph-source:v1 comment=42 -->"
        for state, body in (("closed", indexed), ("open", "No graph index")):
            with self.subTest(state=state, body=body):
                github = FakeGitHub(state, body)
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    RUNNER.Controller(complete_spec(), github=github)._remote_state()
                self.assertEqual(caught.exception.code, "ambiguous_graph")
                self.assertEqual(github.comments_read, 0)

    def test_invalid_utf8_prompt_stops_in_preflight_without_remote_write(self):
        class FakeGitHub:
            def __init__(self, node):
                self.node = node
                self.writes = 0

            def issue(self, _number):
                return {"number": 1070, "state": "open", "body":
                        "<!-- conary-agent-graph-source:v1 comment=42 -->"}

            def issue_comments(self, _number):
                return [self.node]

            def main_sha(self):
                return BASE

            def branch_sha(self, _branch):
                return None

            def pulls_for_branch(self, _branch):
                return None

            def create_branch(self, _branch, _sha):
                self.writes += 1

            def comment(self, _number, _body):
                self.writes += 1

        with tempfile.TemporaryDirectory(prefix="runner-prompt-utf8-") as root:
            value = complete_spec()
            prompt = Path(root) / "prompt.txt"
            prompt.write_bytes(b"task: \xff\n")
            prompt.chmod(0o600)
            value["queue"][0]["prompt_file"] = str(prompt)
            value["queue"][0]["prompt_sha256"] = hashlib.sha256(prompt.read_bytes()).hexdigest()
            github = FakeGitHub(graph_comment(
                acceptance=RUNNER.acceptance_sha256(value["queue"][0])))
            with self.assertRaises(RUNNER.RunnerError) as caught:
                RUNNER.Controller(value, github=github).dry_run()
            self.assertEqual(caught.exception.code, "invalid_spec")
            self.assertEqual(github.writes, 0)

    def test_malformed_model_auth_stops_before_remote_reads_or_writes(self):
        class NoGitHubUse:
            def __getattr__(self, name):
                raise AssertionError(f"GitHub {name} called before auth preflight")

        with tempfile.TemporaryDirectory(prefix="runner-auth-preflight-") as root:
            value = complete_spec()
            auth = Path(root) / "auth.json"
            value["auth_file"] = str(auth)
            for payload in (
                "{malformed",
                '{"access_token":"fake-first-secret-0123456789",'
                '"access_token":"fake-second-secret-0123456789"}',
            ):
                with self.subTest(payload=payload[:12]):
                    auth.write_text(payload, encoding="utf-8")
                    auth.chmod(0o600)
                    controller = RUNNER.Controller(value, github=NoGitHubUse(),
                                                   sandbox=object())
                    with self.assertRaises(RUNNER.RunnerError) as caught:
                        controller._run_inner()
                    self.assertEqual(caught.exception.code, "missing_input")

    def test_unknown_hosted_state_stops_instead_of_polling_wall_budget(self):
        class FakeGitHub:
            def __init__(self, unknown_kind):
                self.unknown_kind = unknown_kind

            def required_rules(self):
                return promotion_inputs()[3]

            def workflow_runs(self, sha):
                return [{"workflow_id": 1, "id": 9, "run_number": 1,
                         "run_attempt": 1, "head_sha": sha,
                         "status": "mystery" if self.unknown_kind == "workflow" else "completed",
                         "conclusion": "success"}]

            def check_runs(self, sha):
                checks = deepcopy(promotion_inputs()[4])
                for check in checks:
                    check["head_sha"] = sha
                if self.unknown_kind == "check":
                    checks[0]["status"] = "mystery"
                return checks

        with tempfile.TemporaryDirectory(prefix="runner-unknown-hosted-") as root:
            value = complete_spec()
            value["journal_dir"] = str(Path(root) / "journal")
            for kind in ("workflow", "check"):
                with self.subTest(kind=kind), \
                        patch.object(RUNNER.time, "sleep",
                                     side_effect=AssertionError("unknown state polled")):
                    controller = RUNNER.Controller(value, github=FakeGitHub(kind))
                    with self.assertRaises(RUNNER.RunnerError) as caught:
                        controller._observe(task(), {"candidate": {"head_sha": HEAD},
                                             "phase": "draft"})
                    self.assertIn(caught.exception.code, {"remote_unknown", "unknown_outcome"})

    def test_duplicate_wake_cannot_take_the_same_local_lock(self):
        with tempfile.TemporaryDirectory(prefix="runner-lock-control-") as root:
            journal_dir = Path(root) / "journal"
            with RUNNER.run_lock(journal_dir):
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    with RUNNER.run_lock(journal_dir):
                        pass
            self.assertEqual(caught.exception.code, "busy")

    def test_checkpoint_without_journal_stops_instead_of_replaying_a_write(self):
        comments = [graph_comment(), checkpoint_comment()]
        with self.assertRaises(RUNNER.RunnerError) as caught:
            RUNNER._validate_remote_checkpoint(complete_spec(), task(), comments, None)
        self.assertEqual(caught.exception.code, "ambiguous_graph")

    def test_drifted_candidate_checkpoint_stops_recovery(self):
        journal = {"task_id": task()["id"], "phase": "candidate",
                   "candidate": {"head_sha": HEAD, "tree_sha": TREE}}
        comments = [graph_comment(), checkpoint_comment(head="e" * 40)]
        with self.assertRaises(RUNNER.RunnerError) as caught:
            RUNNER._validate_remote_checkpoint(complete_spec(), task(), comments, journal)
        self.assertEqual(caught.exception.code, "ambiguous_graph")

    def test_remote_observed_checkpoint_cannot_replay_older_candidate_phase(self):
        journal = {"task_id": task()["id"], "phase": "candidate",
                   "candidate": {"head_sha": HEAD, "tree_sha": TREE}}
        comments = [graph_comment(), checkpoint_comment(phase="observed")]
        with self.assertRaises(RUNNER.RunnerError) as caught:
            RUNNER._validate_remote_checkpoint(complete_spec(), task(), comments, journal)
        self.assertEqual(caught.exception.code, "ambiguous_graph")

    def test_local_journal_is_bound_to_the_exact_run_envelope(self):
        with tempfile.TemporaryDirectory(prefix="runner-journal-control-") as root:
            value = complete_spec()
            value["journal_dir"] = str(Path(root) / "journal")
            RUNNER.write_journal(value, {"task_id": task()["id"], "phase": "branch_intent",
                                         **journal_times()})
            self.assertEqual(RUNNER.read_journal(value)["phase"], "branch_intent")
            changed = deepcopy(value)
            changed["queue"][0]["base_sha"] = "e" * 40
            with self.assertRaises(RUNNER.RunnerError) as caught:
                RUNNER.read_journal(changed)
            self.assertEqual(caught.exception.code, "local_unknown")

    def test_persisted_wall_deadline_still_stops_after_restart(self):
        with tempfile.TemporaryDirectory(prefix="runner-deadline-control-") as root:
            value = complete_spec()
            value["journal_dir"] = str(Path(root) / "journal")
            started = datetime.now(timezone.utc) - timedelta(seconds=7200)
            RUNNER.write_journal(value, {"task_id": task()["id"], "phase": "claimed",
                                         **journal_times(started)})
            controller = RUNNER.Controller(value)
            with self.assertRaises(RUNNER.RunnerError) as caught:
                controller._remaining()
            self.assertEqual(caught.exception.code, "wall_budget")

    def test_crash_after_claim_does_not_create_a_second_branch_or_checkpoint(self):
        class FakeGitHub:
            def __init__(self, first_comment):
                self.comments = [first_comment]
                self.branch = None
                self.branch_creations = 0
                self.checkpoints = 0

            def issue(self, _number):
                return {"number": 1070, "state": "open", "body":
                        "<!-- conary-agent-graph-source:v1 comment=42 -->"}

            def authenticated_actor(self):
                return "runner-test"

            def issue_comments(self, _number):
                return deepcopy(self.comments)

            def main_sha(self):
                return BASE

            def branch_sha(self, _branch):
                return self.branch

            def pulls_for_branch(self, _branch):
                return None

            def create_branch(self, _branch, base_sha):
                self.branch_creations += 1
                self.branch = base_sha

            def comment(self, _number, body):
                self.checkpoints += 1
                self.comments.append({"id": 42 + self.checkpoints,
                                      "user": {"login": "runner-test"}, "body": body})
                return 42 + self.checkpoints

        class StopAfterClaim(RUNNER.Controller):
            def _worktree(self, _task, _journal):
                raise RUNNER.RunnerError("test_stop", "simulated crash after claim")

        with tempfile.TemporaryDirectory(prefix="runner-claim-control-") as root:
            value = complete_spec()
            prompt = Path(root) / "prompt.txt"
            prompt.write_text("A bounded test task.\n", encoding="utf-8")
            prompt.chmod(0o600)
            auth = Path(root) / "auth.json"
            auth.write_text(
                json.dumps({"tokens": {"access_token": "fake-crash-auth-value-0123456789"}}),
                encoding="utf-8")
            auth.chmod(0o600)
            value["auth_file"] = str(auth)
            value["journal_dir"] = str(Path(root) / "journal")
            value["queue"][0]["prompt_file"] = str(prompt)
            value["queue"][0]["prompt_sha256"] = hashlib.sha256(prompt.read_bytes()).hexdigest()
            first_comment = graph_comment(
                acceptance=RUNNER.acceptance_sha256(value["queue"][0]))
            github = FakeGitHub(first_comment)
            for _ in range(2):
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    StopAfterClaim(value, github=github).run()
                self.assertEqual(caught.exception.code, "test_stop")
            self.assertEqual(github.branch_creations, 1)
            self.assertEqual(github.checkpoints, 1)
            self.assertEqual(RUNNER.read_journal(value)["phase"], "claimed")

    def test_controller_uses_the_isolated_proof_launcher(self):
        class IsolatedProof:
            def run_proof(self, **_kwargs):
                raise RUNNER.RunnerError("isolated_proof_called", "proof stayed in sandbox")

        with tempfile.TemporaryDirectory(prefix="runner-proof-control-") as root:
            controller = RUNNER.Controller(complete_spec(), sandbox=IsolatedProof())
            with self.assertRaises(RUNNER.RunnerError) as caught:
                controller._proof(task(), Path(root), {"head_sha": HEAD}, 0)
            self.assertEqual(caught.exception.code, "isolated_proof_called")

    def test_foreign_pr_on_owned_branch_blocks_before_head_update(self):
        class ForeignPRGitHub:
            def authenticated_actor(self):
                return "runner-test"

            def issue(self, _number):
                return {"number": 1070, "state": "open", "body":
                        "<!-- conary-agent-graph-source:v1 comment=42 -->"}

            def issue_comments(self, _number):
                return [graph_comment(), checkpoint_comment("claimed", None, None)]

            def main_sha(self):
                return BASE

            def branch_sha(self, _branch):
                return BASE

            def pulls_for_branch(self, branch):
                return {"number": 99, "state": "open", "draft": True,
                        "user": {"login": "foreign-user"}, "body": "Refs #1070",
                        "head": {"sha": BASE, "ref": branch},
                        "base": {"sha": BASE, "ref": "main"}}

        value = complete_spec()
        journal = {"task_id": task()["id"], "phase": "claimed"}
        controller = RUNNER.Controller(value, github=ForeignPRGitHub())
        with self.assertRaises(RUNNER.RunnerError) as caught:
            controller._before_write(task(), "update_branch", BASE, journal)
        self.assertEqual(caught.exception.code, "occupied_task")

    def test_reconciliation_cannot_consume_wall_deadline_before_remote_write(self):
        class SlowGitHub:
            def authenticated_actor(self):
                return "runner-test"

            def issue(self, _number):
                return {"number": 1070, "state": "open", "body":
                        "<!-- conary-agent-graph-source:v1 comment=42 -->"}

            def issue_comments(self, _number):
                return [graph_comment(), checkpoint_comment("claimed", None, None)]

            def main_sha(self):
                return BASE

            def branch_sha(self, _branch):
                return BASE

            def pulls_for_branch(self, _branch):
                return None

        with tempfile.TemporaryDirectory(prefix="runner-reconcile-clock-") as root:
            value = complete_spec()
            value["journal_dir"] = str(Path(root) / "journal")
            value["limits"]["wall_seconds"] = 1
            ticks = iter([100.0, 100.1])
            with patch.object(RUNNER.time, "monotonic", side_effect=lambda: next(ticks, 101.1)):
                controller = RUNNER.Controller(value, github=SlowGitHub())
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    controller._before_write(task(), "update_branch", BASE,
                                             {"task_id": task()["id"], "phase": "claimed"})
            self.assertEqual(caught.exception.code, "wall_budget")

    def test_candidate_cannot_commit_exact_model_auth_value(self):
        with tempfile.TemporaryDirectory(prefix="runner-auth-leak-") as root:
            root = Path(root)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "README").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(source), "add", "README"], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=runner-test",
                            "-c", "user.email=runner-test@example.invalid",
                            "commit", "-qm", "seed"], check=True)
            base = subprocess.check_output(
                ["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            worktree = root / "worker"
            subprocess.run(["git", "-C", str(source), "worktree", "add", "-q", "--detach",
                            str(worktree), base], check=True)
            secret = "fake-model-auth-token-0123456789"
            auth = root / "auth.json"
            auth.write_text(json.dumps({"tokens": {"access_token": secret}}), encoding="utf-8")
            auth.chmod(0o600)
            (worktree / "README").write_text(f"candidate {secret}\n", encoding="utf-8")
            value = complete_spec()
            value["auth_file"] = str(auth)
            owned_task = {**value["queue"][0], "allowed_paths": ["README"]}
            with self.assertRaises(RUNNER.RunnerError) as caught:
                RUNNER.Controller(value, source_root=source)._commit(owned_task, worktree)
            self.assertEqual(caught.exception.code, "secret_in_candidate")
            current = subprocess.check_output(
                ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True).strip()
            self.assertEqual(current, base)


class ControllerFlowControls(unittest.TestCase):
    def _exercise_flow(self, review_modes=(), drift_on_invalid=None,
                       exercise_merge_cases=False):
        sandbox = RUNNER._load_sandbox()

        class FakeSandbox:
            def __init__(self):
                self.launches = []
                self.review_modes = list(review_modes)
                self.review_inputs = []
                self.trace_paths = []
                self.result_paths = []
                self.proof_calls = 0

            def launch_codex(self, **kwargs):
                worktree = kwargs["worktree"]
                review = kwargs["read_only_worktree"]
                role = "reviewer" if review else "worker"
                self.launches.append(role)
                self.trace_paths.append(kwargs["trace_path"])
                self.result_paths.append(kwargs["result_path"])
                if not review:
                    (worktree / "README").write_text("candidate change\n", encoding="utf-8")
                    (worktree / "target").mkdir()
                    (worktree / "target" / "poison").write_text("ignored worker artifact\n",
                                                                 encoding="utf-8")
                else:
                    receipts = list((worktree / "target" / "agent-proof").glob("*/receipt.json"))
                    if len(receipts) != 1:
                        raise AssertionError("reviewer cannot inspect focused proof receipt")
                    proof_receipt = json.loads(receipts[0].read_text(encoding="utf-8"))
                    if not (worktree / proof_receipt["stdout"]["path"]).is_file():
                        raise AssertionError("reviewer cannot inspect focused proof log")
                    if (worktree / "target" / "poison").exists():
                        raise AssertionError("reviewer inherited worker ignored artifact")
                    if ('branch to "agent/flow-test"' not in kwargs["prompt"] or
                            "worktree-relative POSIX paths" not in kwargs["prompt"]):
                        raise AssertionError("reviewer prompt omitted the logical branch or evidence contract")
                head = subprocess.check_output(["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True).strip()
                tree = subprocess.check_output(["git", "-C", str(worktree), "rev-parse", "HEAD^{tree}"], text=True).strip()
                if review:
                    self.review_inputs.append((head, tree, hashlib.sha256(receipts[0].read_bytes()).hexdigest()))
                    mode = self.review_modes.pop(0) if self.review_modes else "valid"
                    if mode == "invalid" and drift_on_invalid == "candidate":
                        (worktree / "README").write_text("review drift\n", encoding="utf-8")
                    if mode == "invalid" and drift_on_invalid == "proof":
                        receipts[0].write_text("altered receipt\n", encoding="utf-8")
                    if mode == "interrupt":
                        raise KeyboardInterrupt()
                else:
                    mode = "valid"
                rejected_class = {"invalid": "shape", "unknown": "unreadable",
                                  "oversize": "oversize"}.get(mode)
                session = f"{role}-session-{len(self.launches)}"
                trace = kwargs["trace_path"]
                trace.write_text("\n".join(json.dumps(row) for row in (
                    {"kind": "launch", "model": kwargs["model"],
                     "reasoning_effort": kwargs["reasoning_effort"],
                     "read_only_worktree": review},
                    {"kind": "codex_event", "type": "thread.started", "session_id": session},
                    {"kind": "codex_event", "type": "turn.completed"},
                    {"kind": "complete", "status": "invalid-final-result"
                     if rejected_class else "ok",
                     "session_id": session, "exit_code": 0, "timed_out": False,
                     "invalid_events": 0,
                     "final_result_class": rejected_class},
                )) + "\n", encoding="utf-8")
                if rejected_class:
                    return SimpleNamespace(ok=False, timed_out=False, exit_code=0,
                                           session_id=session, model=kwargs["model"],
                                           reasoning_effort=kwargs["reasoning_effort"],
                                           error="invalid-final-result",
                                           final_result_class=rejected_class)
                kwargs["result_path"].write_text(json.dumps({
                    "schema_version": 1, "run_id": "flow-test", "task_id": "FlowTask",
                    "status": "ready", "branch": "agent/flow-test",
                    "candidate_head": head if review else None,
                    "candidate_tree": tree if review else None,
                    "evidence": [], "reason": None,
                }), encoding="utf-8")
                return SimpleNamespace(ok=True, timed_out=False, exit_code=0, session_id=session,
                                       model=kwargs["model"],
                                       reasoning_effort=kwargs["reasoning_effort"], error=None)

            def run_proof(self, **kwargs):
                self.proof_calls += 1
                return sandbox.run_proof(**kwargs)

        class FakeGitHub:
            def __init__(self, base, first_comment, required):
                self.base = base
                self.comments = [first_comment]
                self.branch = None
                self.pull = None
                self.merge_tree = None
                self.merge_parents = None
                self.merge_sha = "f" * 40
                self.potential_merge = self.merge_sha
                self.read_only = False
                self.created = 0
                self.required = required

            def issue(self, _number):
                return {"number": 1070, "state": "open", "body":
                        "<!-- conary-agent-graph-source:v1 comment=42 -->"}

            def authenticated_actor(self):
                return "runner-test"

            def issue_comments(self, _number):
                return deepcopy(self.comments)

            def main_sha(self):
                return self.base

            def branch_sha(self, _branch):
                return self.branch

            def pulls_for_branch(self, _branch):
                return deepcopy(self.pull)

            def create_branch(self, _branch, base):
                if self.read_only:
                    raise AssertionError("promotion-check attempted a remote write")
                self.created += 1
                self.branch = base

            def comment(self, _number, body):
                if self.read_only:
                    raise AssertionError("promotion-check attempted a remote write")
                self.comments.append({"id": 42 + len(self.comments),
                                      "user": {"login": "runner-test"}, "body": body})
                return self.comments[-1]["id"]

            def draft_pr(self, task_value, body):
                if self.read_only:
                    raise AssertionError("promotion-check attempted a remote write")
                self.pull = {"number": 99, "state": "open", "draft": True,
                             "user": {"login": "runner-test"}, "body": body,
                             "head": {"sha": self.branch, "ref": task_value["branch"]},
                             "base": {"sha": self.base, "ref": "main"},
                             "mergeable": True, "merge_commit_sha": "f" * 40}
                return deepcopy(self.pull)

            def pr(self, number):
                self.assert_pr_number(number)
                return deepcopy(self.pull)

            def assert_pr_number(self, number):
                if number != 99:
                    raise AssertionError("wrong test PR")

            def commit_data(self, _sha):
                if _sha != self.merge_sha:
                    raise RUNNER.RunnerError("remote_unknown", "foreign test-merge commit")
                return {"tree_sha": self.merge_tree,
                        "parents": self.merge_parents or [self.base, self.branch]}

            def potential_merge_sha(self, _number):
                return self.potential_merge

            def review_threads(self, _number):
                return []

            def required_rules(self):
                return promotion_inputs()[3]

            def workflow_runs(self, sha):
                return [{"workflow_id": 1, "id": 100, "run_number": 1,
                         "run_attempt": 1, "head_sha": sha,
                         "status": "completed", "conclusion": "success"}]

            def check_runs(self, sha):
                return [{"id": index, "name": item["context"], "head_sha": sha,
                         "status": "completed", "conclusion": "success",
                         "app": {"id": item["app_id"]}}
                        for index, item in enumerate(self.required, 1)]

        class LocalPush(RUNNER.Controller):
            def _push_candidate(self, task_value, journal, _worktree):
                if self.gh.branch == journal["candidate"]["head_sha"]:
                    return
                self._before_write(task_value, "update_branch", task_value["base_sha"], journal)
                self.gh.branch = journal["candidate"]["head_sha"]

        with tempfile.TemporaryDirectory(prefix="runner-flow-control-") as root:
            root = Path(root)
            source = root / "source"
            source.mkdir()
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            (source / "scripts").mkdir()
            shutil.copy2(Path(__file__).with_name("agent-proof.py"), source / "scripts" / "agent-proof.py")
            (source / ".gitignore").write_text("target/\n", encoding="utf-8")
            (source / "README").write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=runner-test",
                            "-c", "user.email=runner-test@example.invalid", "commit", "-qm", "seed"], check=True)
            base = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            value = complete_spec()
            value.update(run_id="flow-test", worktree_root=str(root / "worktrees"),
                         journal_dir=str(root / "journal"), auth_file=str(root / "auth.json"))
            value["queue"] = [{**task(), "id": "FlowTask", "base_sha": base,
                               "branch": "agent/flow-test", "allowed_paths": ["README"],
                               "commit_subject": "test: exercise bounded flow",
                               "checks": [{"id": "focused", "argv": ["python3", "-c",
                                           "from pathlib import Path; assert not Path('target/poison').exists()"],
                                           "timeout_seconds": 30}],
                               "prompt_file": str(root / "prompt.txt")}]
            (root / "auth.json").write_text(
                json.dumps({"tokens": {"access_token": "fake-flow-auth-value-0123456789"}}),
                encoding="utf-8")
            (root / "auth.json").chmod(0o600)
            (root / "prompt.txt").write_text("Edit README.\n", encoding="utf-8")
            (root / "prompt.txt").chmod(0o600)
            value["queue"][0]["prompt_sha256"] = hashlib.sha256((root / "prompt.txt").read_bytes()).hexdigest()
            (root / "worktrees").mkdir(mode=0o700)
            worktree = root / "worktrees" / "flow-test"
            subprocess.run(["git", "-C", str(source), "worktree", "add", "-q", "--detach",
                            str(worktree), base], check=True)
            node = graph_comment(base=base,
                                 acceptance=RUNNER.acceptance_sha256(value["queue"][0]),
                                 task_id="FlowTask")
            gh = FakeGitHub(base, node, value["required_checks"])
            child = FakeSandbox()
            try:
                with RUNNER.run_lock(value["journal_dir"]):
                    result = LocalPush(value, github=gh, source_root=source, sandbox=child).run()
            except KeyboardInterrupt:
                with RUNNER.run_lock(value["journal_dir"]):
                    with self.assertRaises(RUNNER.RunnerError) as caught:
                        LocalPush(value, github=gh, source_root=source, sandbox=child).run()
                error = caught.exception
                return {"error": error.code, "journal": RUNNER.read_journal(value),
                        "launches": child.launches, "review_inputs": child.review_inputs,
                        "trace_paths": child.trace_paths, "result_paths": child.result_paths,
                        "proof_calls": child.proof_calls,
                        "branch": gh.branch, "base": base, "pr": gh.pull}
            except RUNNER.RunnerError as error:
                return {"error": error.code, "journal": RUNNER.read_journal(value),
                        "launches": child.launches, "review_inputs": child.review_inputs,
                        "trace_paths": child.trace_paths, "result_paths": child.result_paths,
                        "proof_calls": child.proof_calls,
                        "branch": gh.branch, "base": base, "pr": gh.pull}
            self.assertEqual(result["status"], "draft_checks_passed")
            self.assertEqual(gh.created, 1)
            self.assertEqual(gh.pull["draft"], True)
            self.assertEqual(RUNNER.read_journal(value)["phase"], "observed")
            with RUNNER.run_lock(value["journal_dir"]):
                resumed = LocalPush(value, github=gh, source_root=source, sandbox=child).run()
            self.assertEqual(resumed["status"], "already_observed")
            self.assertEqual(gh.created, 1)
            gh.merge_tree = subprocess.check_output(
                ["git", "-C", str(worktree), "rev-parse", "HEAD^{tree}"], text=True).strip()
            gh.read_only = True
            draft_gate = LocalPush(value, github=gh, source_root=source,
                                   sandbox=child).promotion_check(99)
            self.assertEqual(draft_gate["status"], "blocked")
            gh.pull["draft"] = False
            gh.comments[0]["body"] = gh.comments[0]["body"].replace(
                '"state":"ready"', '"state":"verified"')
            ready_gate = LocalPush(value, github=gh, source_root=source,
                                   sandbox=child).promotion_check(99)
            self.assertEqual(ready_gate["status"], "ready")
            if exercise_merge_cases:
                controller = LocalPush(value, github=gh, source_root=source, sandbox=child)
                before_journal = RUNNER.read_journal(value)
                before_comments = deepcopy(gh.comments)
                before_branch = gh.branch

                del gh.pull["merge_commit_sha"]
                self.assertEqual(controller.promotion_check(99)["status"], "ready")
                gh.pull["merge_commit_sha"] = gh.merge_sha

                for bad_oid in (None, "malformed", "e" * 40):
                    gh.potential_merge = bad_oid
                    with self.subTest(oid=bad_oid), self.assertRaises(RUNNER.RunnerError) as caught:
                        controller.promotion_check(99)
                    self.assertEqual(caught.exception.code, "remote_unknown")
                gh.potential_merge = gh.merge_sha

                gh.pull["merge_commit_sha"] = "e" * 40
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    controller.promotion_check(99)
                self.assertEqual(caught.exception.code, "remote_unknown")
                gh.pull["merge_commit_sha"] = None
                with self.assertRaises(RUNNER.RunnerError) as caught:
                    controller.promotion_check(99)
                self.assertEqual(caught.exception.code, "remote_unknown")
                gh.pull["merge_commit_sha"] = gh.merge_sha

                gh.merge_tree = "e" * 40
                wrong_tree = controller.promotion_check(99)
                self.assertEqual(wrong_tree["status"], "blocked")
                self.assertIn("PR test-merge tree differs from reviewed candidate",
                              wrong_tree["reasons"])
                gh.merge_tree = before_journal["candidate"]["tree_sha"]
                gh.merge_parents = [gh.branch, gh.base]
                wrong_parents = controller.promotion_check(99)
                self.assertEqual(wrong_parents["status"], "blocked")
                self.assertIn("PR test-merge parents differ from reviewed base and head",
                              wrong_parents["reasons"])
                gh.merge_parents = None

                self.assertEqual(RUNNER.read_journal(value), before_journal)
                self.assertEqual(gh.comments, before_comments)
                self.assertEqual(gh.branch, before_branch)
            return {"error": None, "journal": RUNNER.read_journal(value),
                    "launches": child.launches, "review_inputs": child.review_inputs,
                    "trace_paths": child.trace_paths, "result_paths": child.result_paths,
                    "proof_calls": child.proof_calls,
                    "branch": gh.branch, "base": base, "pr": gh.pull}

    def test_one_slice_reaches_draft_and_observed_checks_without_duplicate_dispatch(self):
        result = self._exercise_flow()
        self.assertIsNone(result["error"])
        self.assertEqual(result["launches"], ["worker", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)

    def test_promotion_test_merge_sources_and_read_only_boundary(self):
        self.assertIsNone(self._exercise_flow(exercise_merge_cases=True)["error"])

    def test_invalid_reviewer_result_retries_only_review_on_same_candidate_and_proof(self):
        result = self._exercise_flow(review_modes=("invalid", "valid"))
        self.assertIsNone(result["error"])
        self.assertEqual(result["launches"], ["worker", "reviewer", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)
        self.assertEqual(result["review_inputs"][0], result["review_inputs"][1])
        self.assertNotEqual(result["trace_paths"][1], result["trace_paths"][2])
        self.assertNotEqual(result["result_paths"][1], result["result_paths"][2])
        self.assertEqual(result["journal"]["candidate"]["repair_count"], 0)
        self.assertEqual(result["journal"]["candidate"]["head_sha"], result["branch"])

    def test_exhausted_reviewer_result_retry_stops_before_push_or_pr(self):
        result = self._exercise_flow(review_modes=("invalid", "invalid"))
        self.assertEqual(result["error"], "invalid_reviewer_result")
        self.assertEqual(result["launches"], ["worker", "reviewer", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)
        self.assertEqual(result["review_inputs"][0], result["review_inputs"][1])
        self.assertEqual(result["journal"]["phase"], "blocked")
        self.assertEqual(result["branch"], result["base"])
        self.assertIsNone(result["pr"])

    def test_reviewer_result_retry_rejects_candidate_drift(self):
        result = self._exercise_flow(review_modes=("invalid",), drift_on_invalid="candidate")
        self.assertEqual(result["error"], "stale_evidence")
        self.assertEqual(result["launches"], ["worker", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)
        self.assertEqual(result["branch"], result["base"])
        self.assertIsNone(result["pr"])

    def test_reviewer_result_retry_rejects_proof_drift(self):
        result = self._exercise_flow(review_modes=("invalid",), drift_on_invalid="proof")
        self.assertEqual(result["error"], "stale_evidence")
        self.assertEqual(result["launches"], ["worker", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)
        self.assertEqual(result["branch"], result["base"])
        self.assertIsNone(result["pr"])

    def test_non_retryable_reviewer_result_is_a_manual_stop(self):
        for mode in ("unknown", "oversize"):
            with self.subTest(mode=mode):
                result = self._exercise_flow(review_modes=(mode,))
                self.assertEqual(result["error"], "unknown_outcome")
                self.assertEqual(result["launches"], ["worker", "reviewer"])
                self.assertEqual(result["journal"]["phase"], "blocked")
                self.assertEqual(result["branch"], result["base"])
                self.assertIsNone(result["pr"])

    def test_interrupted_review_does_not_relaunch_on_resume(self):
        result = self._exercise_flow(review_modes=("interrupt",))
        self.assertEqual(result["error"], "unknown_outcome")
        self.assertEqual(result["launches"], ["worker", "reviewer"])
        self.assertEqual(result["proof_calls"], 1)
        self.assertEqual(result["journal"]["phase"], "blocked")
        self.assertEqual(result["branch"], result["base"])
        self.assertIsNone(result["pr"])


class ObservationControls(unittest.TestCase):
    class SequencedGitHub:
        def __init__(self, snapshots, jobs=()):
            self.snapshots = snapshots
            self.jobs = jobs
            self.polls = 0
            self.job_queries = []

        def required_rules(self):
            return promotion_inputs()[3]

        def workflow_runs(self, sha):
            assert sha == HEAD
            if self.polls >= len(self.snapshots):
                raise AssertionError("observation polled past the supplied sequence")
            return deepcopy(self.snapshots[self.polls][0])

        def check_runs(self, sha):
            assert sha == HEAD
            checks = deepcopy(self.snapshots[self.polls][1])
            self.polls += 1
            return checks

        def workflow_jobs(self, run_id):
            self.job_queries.append((run_id, self.polls))
            return deepcopy(self.jobs)

    @staticmethod
    def workflow(conclusion="failure"):
        return {"workflow_id": 17, "id": 91, "run_number": 1, "run_attempt": 1,
                "head_sha": HEAD, "status": "completed", "conclusion": conclusion}

    @staticmethod
    def checks():
        return deepcopy(promotion_inputs()[4])

    @staticmethod
    def pretest_jobs():
        return [
            {"id": 92, "name": "native-cross-source-lifecycle (opensuse-tumbleweed)",
             "status": "completed", "conclusion": "failure", "steps": [
                 {"number": 5, "name": "Run ./.github/actions/cache-base-image",
                  "conclusion": "failure"},
                 *({"number": number, "name": f"product step {number}",
                    "conclusion": "skipped"} for number in range(6, 13)),
             ]},
            {"id": 93, "name": "native-cross-source-lifecycle", "status": "completed",
             "conclusion": "failure", "steps": [
                 {"number": 2, "name": "Require every distro lifecycle job",
                  "conclusion": "failure"},
             ]},
        ]

    def observe(self, snapshots, jobs=()):
        with tempfile.TemporaryDirectory(prefix="runner-observe-order-") as root:
            value = complete_spec()
            value["journal_dir"] = str(Path(root) / "journal")
            github = self.SequencedGitHub(snapshots, jobs)
            controller = RUNNER.Controller(value, github=github)
            journal = {"candidate": {"head_sha": HEAD, "tree_sha": TREE},
                       "phase": "draft", "pr_number": 99}
            with patch.object(RUNNER.time, "sleep") as sleep, \
                    patch.object(RUNNER, "write_journal"), \
                    patch.object(controller, "_ensure_observed_checkpoint"):
                try:
                    result = controller._observe(task(), journal)
                    error = None
                except RUNNER.RunnerError as caught:
                    result, error = None, caught
            return result, error, github.polls, github.job_queries, sleep.call_count, journal

    def test_optional_pretest_waits_for_missing_and_pending_required_checks(self):
        missing = self.checks()[:-1]
        pending = self.checks()
        pending[-1]["status"] = "in_progress"
        pending[-1]["conclusion"] = None
        optional = {"id": 99, "name": "native-cross-source-lifecycle",
                    "head_sha": HEAD, "status": "completed", "conclusion": "failure",
                    "app": {"id": 15368}}
        snapshots = [([self.workflow()], checks)
                     for checks in (missing + [optional], pending + [optional],
                                    self.checks() + [optional])]
        result, error, polls, queries, sleeps, _ = self.observe(snapshots, self.pretest_jobs())
        self.assertIsNone(result)
        self.assertEqual(error.code, "optional_pretest_image_failure")
        self.assertEqual((polls, sleeps), (3, 2))
        self.assertEqual(queries, [(91, 3)])

    def test_optional_check_waits_for_required_checks_then_fails(self):
        optional = {"id": 99, "name": "native-cross-source-lifecycle",
                    "head_sha": HEAD, "status": "completed", "conclusion": "failure",
                    "app": {"id": 15368}}
        pending = self.checks()
        pending[-1]["status"] = "queued"
        pending[-1]["conclusion"] = None
        running = self.workflow()
        running["status"] = "in_progress"
        running["conclusion"] = None
        snapshots = [([self.workflow("success")], pending + [optional]),
                     ([running], self.checks() + [optional]),
                     ([self.workflow("success")], self.checks() + [optional])]
        _, error, polls, queries, sleeps, _ = self.observe(snapshots)
        self.assertEqual(error.code, "hosted_failure")
        self.assertEqual((polls, sleeps), (3, 2))
        self.assertEqual(queries, [])

    def test_failed_optional_check_waits_for_workflow_pretest_diagnosis(self):
        optional = {"id": 99, "name": "native-cross-source-lifecycle",
                    "head_sha": HEAD, "status": "completed", "conclusion": "failure",
                    "app": {"id": 15368}}
        running = self.workflow()
        running["status"] = "in_progress"
        running["conclusion"] = None
        snapshots = [([running], self.checks() + [optional]),
                     ([self.workflow()], self.checks() + [optional])]
        _, error, polls, queries, sleeps, _ = self.observe(snapshots, self.pretest_jobs())
        self.assertEqual(error.code, "optional_pretest_image_failure")
        self.assertEqual((polls, sleeps), (2, 1))
        self.assertEqual(queries, [(91, 2)])

    def test_other_optional_workflow_waits_then_stays_hosted_failure(self):
        pending = self.checks()
        pending[-1]["status"] = "pending"
        pending[-1]["conclusion"] = None
        snapshots = [([self.workflow()], pending),
                     ([self.workflow()], self.checks())]
        _, error, polls, queries, sleeps, _ = self.observe(snapshots, jobs=[])
        self.assertEqual(error.code, "hosted_failure")
        self.assertEqual((polls, sleeps), (2, 1))
        self.assertEqual(queries, [(91, 2)])

    def test_required_failure_stops_before_optional_diagnosis(self):
        checks = self.checks()
        checks[0]["conclusion"] = "failure"
        checks[-1]["status"] = "in_progress"
        checks[-1]["conclusion"] = None
        _, error, polls, queries, sleeps, _ = self.observe(
            [([self.workflow()], checks)], self.pretest_jobs())
        self.assertEqual(error.code, "hosted_failure")
        self.assertEqual((polls, sleeps), (1, 0))
        self.assertEqual(queries, [])

    def test_action_required_stops_while_required_checks_are_pending(self):
        pending = self.checks()
        pending[-1]["status"] = "pending"
        pending[-1]["conclusion"] = None
        cases = [([self.workflow("action_required")], pending)]
        optional = {"id": 99, "name": "optional", "head_sha": HEAD,
                    "status": "completed", "conclusion": "action_required",
                    "app": {"id": 15368}}
        cases.append(([self.workflow()], pending + [optional]))
        required = deepcopy(pending)
        required[0]["conclusion"] = "action_required"
        cases.append(([self.workflow()], required))
        for runs, checks in cases:
            with self.subTest(runs=runs, checks=checks):
                _, error, polls, queries, sleeps, _ = self.observe([(runs, checks)])
                self.assertEqual(error.code, "action_required")
                self.assertEqual((polls, sleeps), (1, 0))
                self.assertEqual(queries, [])

    def test_unknown_or_duplicate_required_state_stops_before_polling(self):
        bad_run = self.workflow()
        bad_run["status"] = "mystery"
        bad_run["conclusion"] = None
        bad_check = self.checks()
        bad_check[-1]["status"] = "mystery"
        bad_check[-1]["conclusion"] = None
        duplicate = self.checks()
        duplicate.append({**duplicate[0], "id": 99})
        wrong_head = self.checks()
        wrong_head[-1]["head_sha"] = "e" * 40
        cases = [([bad_run], self.checks()), ([self.workflow()], bad_check),
                 ([self.workflow()], duplicate), ([self.workflow()], wrong_head)]
        for runs, checks in cases:
            with self.subTest(runs=runs, checks=checks):
                _, error, polls, queries, sleeps, _ = self.observe([(runs, checks)])
                self.assertEqual(error.code, "remote_unknown")
                self.assertEqual((polls, sleeps), (1, 0))
                self.assertEqual(queries, [])

    def test_pending_required_checks_can_finish_successfully(self):
        pending = self.checks()
        pending[-1]["status"] = "in_progress"
        pending[-1]["conclusion"] = None
        snapshots = [([self.workflow("success")], pending),
                     ([self.workflow("success")], self.checks())]
        result, error, polls, queries, sleeps, journal = self.observe(snapshots)
        self.assertIsNone(error)
        self.assertEqual(result["status"], "draft_checks_passed")
        self.assertEqual((polls, sleeps), (2, 1))
        self.assertEqual(queries, [])
        self.assertEqual(journal["candidate"]["check_ids"], [1, 2, 3, 4, 5])


class PromotionControls(unittest.TestCase):
    def evaluate(self, inputs):
        return RUNNER.evaluate_promotion(*inputs)

    def test_exact_candidate_required_checks_and_resolved_threads_are_ready(self):
        self.assertTrue(self.evaluate(promotion_inputs())["ready"])

    def test_new_main_or_new_head_stops_promotion(self):
        inputs = list(promotion_inputs())
        inputs[2] = "e" * 40
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[1]["head"]["sha"] = "e" * 40
        self.assertFalse(self.evaluate(inputs)["ready"])

    def test_wrong_check_origin_or_unobserved_check_stops_promotion(self):
        inputs = list(promotion_inputs())
        inputs[4][0]["app"]["id"] = 7
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[6]["check_ids"].remove(inputs[4][0]["id"])
        self.assertFalse(self.evaluate(inputs)["ready"])

    def test_missing_review_or_wrong_test_merge_tree_stops_promotion(self):
        inputs = list(promotion_inputs())
        del inputs[6]["review"]
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[6]["review"]["model_id"] = "gpt-6-luna"
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[1]["test_merge_tree_sha"] = "e" * 40
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[1]["test_merge_parents"] = [HEAD, BASE]
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[6]["review"]["session_id"] = inputs[6]["worker"]["session_id"]
        self.assertFalse(self.evaluate(inputs)["ready"])

    def test_required_failure_and_unresolved_thread_stop_promotion(self):
        inputs = list(promotion_inputs())
        inputs[4][0]["conclusion"] = "failure"
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[4][0]["conclusion"] = "action_required"
        self.assertFalse(self.evaluate(inputs)["ready"])
        inputs = list(promotion_inputs())
        inputs[5][0]["isResolved"] = False
        self.assertFalse(self.evaluate(inputs)["ready"])

    def test_unknown_rule_and_optional_failure_stop_promotion(self):
        inputs = list(promotion_inputs())
        inputs[3].append({"type": "mystery_rule", "parameters": {}})
        with self.assertRaises(RUNNER.RunnerError):
            self.evaluate(inputs)
        inputs = list(promotion_inputs())
        inputs[4].append(
            {
                "id": 99,
                "name": "native-cross-source-lifecycle",
                "head_sha": HEAD,
                "status": "completed",
                "conclusion": "failure",
                "app": {"id": 15368},
            }
        )
        self.assertFalse(self.evaluate(inputs)["ready"])

    def test_optional_image_failure_requires_skipped_product_steps(self):
        opensuse = {
            "id": 91,
            "name": "native-cross-source-lifecycle (opensuse-tumbleweed)",
            "status": "completed",
            "conclusion": "failure",
            "steps": [
                {"number": 5, "name": "Run ./.github/actions/cache-base-image",
                 "conclusion": "failure"},
                *({"number": number, "name": f"product step {number}",
                   "conclusion": "skipped"} for number in range(6, 13)),
            ],
        }
        aggregate = {
            "id": 92,
            "name": "native-cross-source-lifecycle",
            "status": "completed",
            "conclusion": "failure",
            "steps": [{"number": 2, "name": "Require every distro lifecycle job",
                       "conclusion": "failure"}],
        }
        jobs = [opensuse, aggregate]
        self.assertEqual(
            RUNNER.optional_pretest_image_failure(jobs)["product_steps_skipped"],
            list(range(6, 13)),
        )
        product_ran = deepcopy(jobs)
        product_ran[0]["steps"][2]["conclusion"] = "success"
        self.assertIsNone(RUNNER.optional_pretest_image_failure(product_ran))
        extra_failure = deepcopy(jobs)
        extra_failure.append({"id": 93, "name": "other", "status": "completed",
                              "conclusion": "failure", "steps": []})
        self.assertIsNone(RUNNER.optional_pretest_image_failure(extra_failure))


if __name__ == "__main__":
    unittest.main()
