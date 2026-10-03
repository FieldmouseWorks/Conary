# scripts/test-agent-runner-sandbox.py
"""Focused controls for the credential-isolated Codex child launcher."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("agent_runner_sandbox.py")
SPEC = importlib.util.spec_from_file_location("agent_runner_sandbox", SCRIPT)
assert SPEC and SPEC.loader
sandbox = importlib.util.module_from_spec(SPEC)
import sys
sys.modules[SPEC.name] = sandbox
SPEC.loader.exec_module(sandbox)
FAKE_CODEX = """#!/usr/bin/python3
import json
import os
import pathlib
import signal
import subprocess
import sys
import time

argv = sys.argv[1:]
worktree = pathlib.Path(argv[argv.index('-C') + 1])
result = pathlib.Path(argv[argv.index('--output-last-message') + 1])
mode = os.environ.get('FAKE_MODE') or 'success'
# Mode comes from a control file in the writable worktree. No inherited env
# except the launcher's explicit allowlist is needed.
control = worktree / '.sandbox-test-mode'
if control.exists():
    mode = control.read_text().strip()
if mode == 'sleep':
    time.sleep(20)
if mode == 'signal':
    os.kill(os.getpid(), signal.SIGTERM)
if mode == 'exit':
    sys.exit(7)
if mode == 'model_error':
    print('unknown model', file=sys.stderr, flush=True)
    sys.exit(2)
if mode == 'toolchain':
    cargo = subprocess.run(['cargo', '--version'], capture_output=True, text=True)
    rustc = subprocess.run(['rustc', '--version'], capture_output=True, text=True)
    cc = subprocess.run(['cc', '--version'], capture_output=True, text=True)
    cxx = subprocess.run(['c++', '--version'], capture_output=True, text=True)
    if cargo.returncode or rustc.returncode or cc.returncode or cxx.returncode:
        sys.exit(9)
companion_ready = None
if mode == 'companion':
    helper = subprocess.run(['/opt/agent/codex-code-mode-host'],
                            capture_output=True, text=True)
    companion_ready = helper.returncode == 0 and helper.stdout.strip() == 'host-ready'
auth = json.loads((pathlib.Path(os.environ['CODEX_HOME']) / 'auth.json').read_text())
host_home_visible = pathlib.Path('__HOST_AUTH_PATH__').exists()
gh_env = any(key in os.environ for key in ('GH_TOKEN', 'GITHUB_TOKEN', 'GH_HOST', 'SSH_AUTH_SOCK'))
common = pathlib.Path(subprocess.check_output(
    ['git', 'rev-parse', '--path-format=absolute', '--git-common-dir'],
    cwd=worktree, text=True,
).strip())
try:
    (common / 'sandbox-write-probe').write_text('bad')
    git_read_only = False
except OSError:
    git_read_only = True
probe = worktree / '.sandbox-test-worktree-write'
git_pointer = worktree / '.git'
try:
    with git_pointer.open('a') as handle:
        handle.write('bad')
    git_pointer_read_only = False
except OSError:
    git_pointer_read_only = True
git_pointer_replace_blocked = None
if mode != 'readonly':
    replacement = worktree / '.sandbox-test-git-replacement'
    replacement.write_text('bad')
    try:
        os.replace(replacement, git_pointer)
        git_pointer_replace_blocked = False
    except OSError:
        git_pointer_replace_blocked = True
    finally:
        replacement.unlink(missing_ok=True)
push_blocked = None
if mode == 'push_probe':
    attempted = subprocess.run(
        ['git', 'push', '--dry-run', 'origin', 'HEAD:refs/heads/test'],
        cwd=worktree, capture_output=True, text=True,
    )
    push_blocked = attempted.returncode != 0 and 'transport' in attempted.stderr.lower()
probe_data = {
    'argv': argv,
    'home': os.environ['HOME'],
    'host_home_visible': host_home_visible,
    'gh_env': gh_env,
    'git_read_only': git_read_only,
    'git_pointer_read_only': git_pointer_read_only,
    'git_pointer_replace_blocked': git_pointer_replace_blocked,
    'git_allow_protocol': os.environ.get('GIT_ALLOW_PROTOCOL'),
    'push_blocked': push_blocked,
    'worktree_write': 'ok',
    'schema_keys': sorted(json.loads(pathlib.Path(
        argv[argv.index('--output-schema') + 1]
    ).read_text())['required']),
    'companion_ready': companion_ready,
}
try:
    probe.write_text(json.dumps(probe_data))
    worktree_read_only = False
except OSError:
    worktree_read_only = True
if mode == 'event_type_reflect':
    print(json.dumps({'type': 'copy.' + auth['token']}), flush=True)
if mode == 'session_reflect':
    print(json.dumps({'type': 'thread.started', 'thread_id': 'id-' + auth['token']}), flush=True)
print(json.dumps({'type':'thread.started','thread_id':'123e4567-e89b-12d3-a456-426614174000'}), flush=True)
print(json.dumps({'type':'item.completed','item':{'text':auth['token']}}), flush=True)
print(auth['token'], file=sys.stderr, flush=True)
payload = {
    'schema_version': 1,
    'run_id': 'test-run',
    'task_id': 'T1',
    'status': 'ready',
    'branch': 'test-branch',
    'candidate_head': None,
    'candidate_tree': None,
    'evidence': [] if worktree_read_only else ['.sandbox-test-worktree-write'],
    'reason': 'read-only' if worktree_read_only else None,
}
if mode == 'reflect':
    payload['reason'] = auth['token']
if mode == 'result_missing':
    pass
elif mode == 'result_parse':
    result.write_text('{broken json')
elif mode == 'result_parse_secret':
    result.write_text('{broken json ' + auth['token'])
elif mode == 'result_oversize':
    result.write_text('x' * (256 * 1024 + 1))
elif mode == 'result_oversize_secret_tail':
    result.write_text('x' * (256 * 1024 + 1) + auth['token'])
elif mode == 'result_unreadable':
    result.symlink_to('/dev/null')
else:
    if mode == 'result_shape':
        payload['branch'] = None
    result.write_text(json.dumps(payload))
"""


@unittest.skipUnless(shutil.which("bwrap"), "local bubblewrap is unavailable")
class SandboxTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="agent-runner-sandbox-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        seed = self.root / "seed"
        subprocess.run(["git", "init", "-q", str(seed)], check=True)
        (seed / "one.txt").write_text("one\n")
        (seed / ".gitignore").write_text("target/\n")
        subprocess.run(["git", "-C", str(seed), "add", "one.txt", ".gitignore"], check=True)
        subprocess.run([
            "git", "-C", str(seed), "-c", "user.name=Sandbox Test",
            "-c", "user.email=sandbox@example.invalid", "commit", "-qm", "seed",
        ], check=True)
        self.worktree = self.root / "worktree"
        subprocess.run(["git", "-C", str(seed), "worktree", "add", "-q", "--detach",
                        str(self.worktree), "HEAD"], check=True)
        subprocess.run(["git", "-C", str(seed), "remote", "add", "origin",
                        "https://example.invalid/private.git"], check=True)
        self.host_auth_path = self.root / "host-home" / ".codex" / "auth.json"
        self.host_auth_path.parent.mkdir(parents=True)
        self.host_auth_path.write_text("host-only fake auth\n", encoding="utf-8")
        self.fake = self.root / "codex"
        self.fake.write_text(FAKE_CODEX.replace("__HOST_AUTH_PATH__", str(self.host_auth_path)))
        self.fake.chmod(0o700)
        helper = self.root / "codex-code-mode-host"
        helper.write_text("#!/usr/bin/python3\nprint('host-ready')\n")
        helper.chmod(0o700)
        self.auth = self.root / "auth.json"
        self.secret = "fake-codex-credential-do-not-log"
        self.auth.write_text(json.dumps({"token": self.secret}))
        self.auth.chmod(0o600)
        self.mode = self.worktree / ".sandbox-test-mode"
        self.probe = self.worktree / ".sandbox-test-worktree-write"
        for path in (self.mode, self.probe):
            if path.exists():
                raise AssertionError(f"test control path already exists: {path}")
            self.addCleanup(path.unlink, missing_ok=True)

    def launch(self, *, mode: str = "success", timeout: int = 8,
               model: str = "gpt-6-sol", effort: str = "max", read_only: bool = False):
        self.mode.write_text(mode)
        trace = self.root / f"{mode}-{model}-trace.jsonl"
        result = self.root / f"{mode}-{model}-result.json"
        with patch.dict(os.environ, {"GH_TOKEN": self.secret, "GITHUB_TOKEN": self.secret,
                                     "SSH_AUTH_SOCK": "/tmp/host-agent.sock"}):
            outcome = sandbox.launch_codex(
                worktree=self.worktree, prompt="Return the requested JSON result.",
                model=model, timeout_seconds=timeout, trace_path=trace, result_path=result,
                codex_binary=self.fake, auth_file=self.auth,
                reasoning_effort=effort,
                read_only_worktree=read_only,
            )
        return outcome

    def test_private_home_model_effort_and_read_only_git(self) -> None:
        outcome = self.launch(model="gpt-6-luna")
        self.assertTrue(outcome.ok, outcome)
        self.assertEqual(outcome.session_id, "123e4567-e89b-12d3-a456-426614174000")
        self.assertEqual(outcome.reasoning_effort, "max")
        final = json.loads(outcome.result_path.read_text())
        probe = json.loads(self.probe.read_text())
        self.assertFalse(probe["host_home_visible"])
        self.assertFalse(probe["gh_env"])
        self.assertTrue(probe["git_read_only"])
        self.assertTrue(probe["git_pointer_read_only"])
        self.assertTrue(probe["git_pointer_replace_blocked"])
        self.assertEqual(probe["git_allow_protocol"], "file")
        self.assertEqual(probe["worktree_write"], "ok")
        self.assertEqual(probe["home"], "/home/agent")
        self.assertIn("gpt-6-luna", probe["argv"])
        self.assertIn("model_reasoning_effort=max", probe["argv"])
        self.assertIn("--output-schema", probe["argv"])
        self.assertEqual(probe["schema_keys"], sorted(sandbox.RESULT_KEYS))
        self.assertEqual(stat.S_IMODE(outcome.trace_path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(outcome.result_path.stat().st_mode), 0o600)
        trace = outcome.trace_path.read_text()
        self.assertNotIn(self.secret, trace)
        self.assertNotIn(self.secret, outcome.result_path.read_text())
        for line in trace.splitlines():
            json.loads(line)

    def test_explicit_nondefault_effort_reaches_child_unchanged(self) -> None:
        outcome = self.launch(effort="high")
        self.assertTrue(outcome.ok, outcome)
        self.assertEqual(outcome.reasoning_effort, "high")
        self.assertIn("model_reasoning_effort=high",
                      json.loads(self.probe.read_text())["argv"])

    def test_timeout_kills_child_and_preserves_failure_receipt(self) -> None:
        outcome = self.launch(mode="sleep", timeout=1)
        self.assertFalse(outcome.ok)
        self.assertTrue(outcome.timed_out)
        self.assertEqual(outcome.error, "timeout")
        self.assertFalse(outcome.result_path.exists())
        self.assertIn('"status":"timeout"', outcome.trace_path.read_text())

    def test_signal_and_nonzero_exit_are_failures(self) -> None:
        for mode in ("signal", "exit"):
            outcome = self.launch(mode=mode)
            self.assertFalse(outcome.ok)
            self.assertEqual(outcome.error, "child-exit")
            self.assertFalse(outcome.result_path.exists())

    def test_reflected_credential_rejects_final_without_logging_it(self) -> None:
        outcome = self.launch(mode="reflect")
        self.assertFalse(outcome.ok)
        self.assertEqual(outcome.error, "invalid-final-result")
        self.assertEqual(outcome.final_result_class, "secret")
        self.assertFalse(outcome.result_path.exists())
        self.assertNotIn(self.secret, outcome.trace_path.read_text())
        self.assertEqual(json.loads(outcome.trace_path.read_text().splitlines()[-1])
                         ["final_result_class"], "secret")

    def test_final_artifact_failure_classes_contain_no_raw_result(self) -> None:
        for mode, expected in (("result_missing", "missing"),
                               ("result_parse", "parse"),
                               ("result_shape", "shape"),
                               ("result_oversize", "oversize"),
                               ("result_unreadable", "unreadable")):
            with self.subTest(mode=mode):
                outcome = self.launch(mode=mode)
                self.assertFalse(outcome.ok)
                self.assertEqual(outcome.error, "invalid-final-result")
                self.assertEqual(outcome.final_result_class, expected)
                self.assertFalse(outcome.result_path.exists())
                trace = outcome.trace_path.read_text()
                self.assertEqual(json.loads(trace.splitlines()[-1])["final_result_class"],
                                 expected)
                self.assertNotIn(self.secret, trace)
                self.assertNotIn("broken json", trace)

    def test_malformed_artifact_with_credential_is_classified_as_secret(self) -> None:
        outcome = self.launch(mode="result_parse_secret")
        self.assertEqual(outcome.error, "invalid-final-result")
        self.assertEqual(outcome.final_result_class, "secret")
        self.assertFalse(outcome.result_path.exists())
        self.assertNotIn(self.secret, outcome.trace_path.read_text())

    def test_credential_bearing_event_metadata_is_rejected_before_trace(self) -> None:
        for mode in ("event_type_reflect", "session_reflect"):
            with self.subTest(mode=mode):
                outcome = self.launch(mode=mode)
                self.assertFalse(outcome.ok)
                self.assertEqual(outcome.error, "invalid-jsonl-events")
                self.assertFalse(outcome.result_path.exists())
                trace = outcome.trace_path.read_text()
                self.assertNotIn(self.secret, trace)
                self.assertEqual(json.loads(trace.splitlines()[-1])["invalid_events"], 1)

    def test_short_auth_token_is_also_rejected_from_session_id(self) -> None:
        self.secret = "short-secret"
        self.auth.write_text(json.dumps({"token": self.secret}))
        outcome = self.launch(mode="session_reflect")
        self.assertFalse(outcome.ok)
        self.assertEqual(outcome.error, "invalid-jsonl-events")
        self.assertFalse(outcome.result_path.exists())
        self.assertNotIn(self.secret, outcome.trace_path.read_text())

    def test_oversized_artifact_may_hide_a_credential_after_read_limit(self) -> None:
        outcome = self.launch(mode="result_oversize_secret_tail")
        self.assertFalse(outcome.ok)
        self.assertEqual(outcome.error, "invalid-final-result")
        self.assertEqual(outcome.final_result_class, "oversize")
        self.assertFalse(outcome.result_path.exists())
        self.assertNotIn(self.secret, outcome.trace_path.read_text())

    def test_model_unavailable_is_classified_without_stderr_contents(self) -> None:
        outcome = self.launch(mode="model_error")
        self.assertFalse(outcome.ok)
        self.assertEqual(outcome.error, "model-unavailable")
        trace = outcome.trace_path.read_text()
        self.assertNotIn("unknown model", trace)
        self.assertIn('"stderr_class":"model-unavailable"', trace)
        self.assertIn('"stderr_sha256":', trace)

    @unittest.skipUnless(shutil.which("rustup"), "Rust toolchain is unavailable")
    def test_rust_toolchain_available_without_host_home(self) -> None:
        outcome = self.launch(mode="toolchain")
        self.assertTrue(outcome.ok, outcome)

    def test_code_mode_companion_is_mounted_read_only(self) -> None:
        outcome = self.launch(mode="companion")
        self.assertTrue(outcome.ok, outcome)
        self.assertTrue(json.loads(self.probe.read_text())["companion_ready"])

    def test_git_remote_transport_is_disabled(self) -> None:
        outcome = self.launch(mode="push_probe")
        self.assertTrue(outcome.ok, outcome)
        self.assertTrue(json.loads(self.probe.read_text())["push_blocked"])

    def test_reviewer_worktree_is_read_only(self) -> None:
        outcome = self.launch(mode="readonly", read_only=True)
        self.assertTrue(outcome.ok, outcome)
        self.assertFalse(self.probe.exists())
        self.assertEqual(json.loads(outcome.result_path.read_text())["reason"], "read-only")

    def test_repository_credential_helper_is_rejected(self) -> None:
        common = Path(subprocess.check_output(
            ["git", "-C", str(self.worktree), "rev-parse", "--path-format=absolute",
             "--git-common-dir"], text=True,
        ).strip())
        subprocess.run(["git", "config", "--file", str(common / "config"),
                        "credential.helper", "unsafe"], check=True)
        with self.assertRaisesRegex(ValueError, "credential-capable"):
            self.launch()

    def test_weak_auth_permissions_are_rejected(self) -> None:
        self.auth.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "unsafe ownership or permissions"):
            self.launch()

    def test_duplicate_auth_keys_are_rejected_before_copy_to_child(self) -> None:
        self.auth.write_text(
            '{"tokens":{"access_token":"fake-first-secret-0123456789",'
            '"access_token":"fake-second-secret-0123456789"}}',
            encoding="utf-8",
        )
        destination = self.root / "copied-auth.json"
        with self.assertRaisesRegex(ValueError, "invalid"):
            sandbox._copy_auth(self.auth, destination)
        self.assertFalse(destination.exists())

    def test_proof_command_has_no_model_or_github_credentials(self) -> None:
        script = (
            "import json,os,pathlib; "
            "pathlib.Path('target/proof-sandbox-observation.json').write_text(json.dumps({"
            "'gh_env':any(k in os.environ for k in ('GH_TOKEN','GITHUB_TOKEN','SSH_AUTH_SOCK')),'"
            "auth_visible':pathlib.Path('/home/agent/.codex/auth.json').exists(),"
            f"'host_auth_visible':pathlib.Path({str(self.host_auth_path)!r}).exists(),"
            "'git_allow_protocol':os.environ.get('GIT_ALLOW_PROTOCOL')}))"
        )
        with patch.dict(os.environ, {"GH_TOKEN": self.secret, "GITHUB_TOKEN": self.secret,
                                     "SSH_AUTH_SOCK": "/tmp/host-agent.sock"}):
            outcome = sandbox.run_proof(
                worktree=self.worktree, proof_script=SCRIPT.with_name("agent-proof.py"),
                run_id="proof-test", argv=["/usr/bin/python3", "-c", script],
                timeout_seconds=8,
            )
        self.assertTrue(outcome.ok, outcome)
        receipt = json.loads((self.worktree / "target/agent-proof/proof-test/receipt.json").read_text())
        self.assertEqual(receipt["status"], "passed")
        observation = json.loads((self.worktree / "target/proof-sandbox-observation.json").read_text())
        self.assertFalse(observation["gh_env"])
        self.assertFalse(observation["auth_visible"])
        self.assertFalse(observation["host_auth_visible"])
        self.assertEqual(observation["git_allow_protocol"], "file")

    def test_proof_timeout_fails_closed(self) -> None:
        outcome = sandbox.run_proof(
            worktree=self.worktree, proof_script=SCRIPT.with_name("agent-proof.py"),
            run_id="proof-timeout", argv=["/usr/bin/python3", "-c", "import time;time.sleep(20)"],
            timeout_seconds=1,
        )
        self.assertFalse(outcome.ok)
        self.assertTrue(outcome.timed_out)
        self.assertEqual(outcome.error, "timeout")

    def test_missing_sandbox_fails_closed(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "bubblewrap is required"):
            sandbox.launch_codex(
                worktree=self.worktree, prompt="x", model="gpt-6-sol", timeout_seconds=1,
                trace_path=self.root / "missing-trace", result_path=self.root / "missing-result",
                codex_binary=self.fake, auth_file=self.auth,
                bubblewrap_binary=self.root / "missing-bwrap",
            )
        self.assertFalse((self.root / "missing-result").exists())

    def test_invalid_effort_and_worktree_evidence_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "invalid model or reasoning effort"):
            sandbox.launch_codex(
                worktree=self.worktree, prompt="x", model="gpt-6-sol", reasoning_effort="bogus",
                timeout_seconds=1, trace_path=self.root / "bad-trace",
                result_path=self.root / "bad-result", codex_binary=self.fake, auth_file=self.auth,
            )
        with self.assertRaisesRegex(ValueError, "outside the worktree"):
            sandbox.launch_codex(
                worktree=self.worktree, prompt="x", model="gpt-6-sol", timeout_seconds=1,
                trace_path=self.worktree / "bad-trace", result_path=self.root / "bad-result",
                codex_binary=self.fake, auth_file=self.auth,
            )

    def test_symlinked_evidence_and_auth_inside_worktree_are_rejected(self) -> None:
        shortcut = self.root / "journal-shortcut"
        shortcut.symlink_to(self.worktree, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "outside the worktree"):
            sandbox.launch_codex(
                worktree=self.worktree, prompt="x", model="gpt-6-sol", timeout_seconds=1,
                trace_path=shortcut / "trace.jsonl", result_path=self.root / "result.json",
                codex_binary=self.fake, auth_file=self.auth,
            )
        inside_auth = self.worktree / "auth.json"
        inside_auth.write_text(json.dumps({"token": self.secret}))
        inside_auth.chmod(0o600)
        self.addCleanup(inside_auth.unlink, missing_ok=True)
        with self.assertRaisesRegex(ValueError, "outside the child worktree"):
            sandbox.launch_codex(
                worktree=self.worktree, prompt="x", model="gpt-6-sol", timeout_seconds=1,
                trace_path=self.root / "trace.jsonl", result_path=self.root / "result.json",
                codex_binary=self.fake, auth_file=inside_auth,
            )


if __name__ == "__main__":
    unittest.main()
