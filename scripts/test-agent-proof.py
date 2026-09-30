#!/usr/bin/env python3
"""Behavioral controls for the local proof receipt wrapper."""

import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from datetime import datetime


SCRIPT = Path(__file__).with_name("agent-proof.py")


def inherited_pipe_child():
    code = (
        "import json,os,time; pid=os.fork(); "
        "time.sleep(30) if pid==0 else print(json.dumps([os.getpid(),pid]), flush=True); "
        "os._exit(0)"
    )
    return [sys.executable, "-c", code]


def process_running(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except FileNotFoundError:
        return False
    return state != "Z"


def wait_stopped(pid):
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        if not process_running(pid):
            return True
        time.sleep(0.05)
    return not process_running(pid)


def initialize_repo(root):
    root.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    (root / ".gitignore").write_text("/target/\n")
    (root / "tracked.txt").write_text("initial\n")
    subprocess.run(["git", "-C", str(root), "add", ".gitignore", "tracked.txt"], check=True)
    subprocess.run(
        ["git", "-C", str(root), "-c", "user.name=Proof Test", "-c", "user.email=proof@example.test", "commit", "-qm", "initial"],
        check=True,
    )


class AgentProofTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name) / "repo"
        initialize_repo(self.repo)

    def invoke(self, run_id, child, repo=None):
        root = repo or self.repo
        command = [sys.executable, str(SCRIPT), "--run-id", run_id, "--", *child]
        return subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False), command[5:]

    def receipt(self, run_id, repo=None):
        root = repo or self.repo
        return json.loads((root / "target" / "agent-proof" / run_id / "receipt.json").read_text())

    def spawn_pipe_test(self, run_id):
        process = subprocess.Popen(
            [sys.executable, str(SCRIPT), "--run-id", run_id, "--", *inherited_pipe_child()],
            cwd=self.repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.assertTrue(select.select([process.stdout], [], [], 3)[0], "child did not report its process IDs")
        leader, grandchild = json.loads(process.stdout.readline())
        return process, leader, grandchild

    def stop_pipe_test(self, process, leader, grandchild):
        if process.poll() is None:
            process.kill()
        if process_running(grandchild):
            try:
                os.killpg(leader, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.communicate(timeout=2)

    def test_dirty_tracked_untracked_and_index_inputs_have_distinct_identities(self):
        (self.repo / "tracked.txt").write_text("edited\n")
        draft = self.repo / "draft with spaces.txt"
        draft.write_text("one\n")
        marker = self.repo / "must-not-exist"
        literal = f"$(touch {marker})"
        child = [
            sys.executable, "-c",
            "import json,sys; print(json.dumps(sys.argv[1:])); print('diagnostic', file=sys.stderr)",
            "two words", literal, f"; touch {marker}",
        ]
        result, argv = self.invoke("dirty-one", child)
        self.assertEqual(result.returncode, 0, result.stderr)
        first = self.receipt("dirty-one")
        self.assertEqual(first["status"], "passed")
        self.assertEqual(first["schema_version"], 1)
        self.assertEqual(first["argv"], argv)
        self.assertEqual(json.loads(result.stdout), child[3:])
        self.assertIn(b"diagnostic", result.stderr)
        self.assertFalse(marker.exists(), "literal shell syntax was executed")
        self.assertEqual(first["candidate_before"], first["candidate_after"])
        self.assertEqual(first["candidate_before"]["state"], "dirty")
        self.assertEqual(first["candidate_before"]["untracked_paths"], 1)
        self.assertEqual(first["candidate_before"]["head"], first["candidate_after"]["head"])
        self.assertEqual(first["cwd"], ".")
        self.assertGreaterEqual(first["elapsed_seconds"], 0)
        self.assertLessEqual(datetime.fromisoformat(first["started_at"]), datetime.fromisoformat(first["ended_at"]))
        for stream in ("stdout", "stderr"):
            locator = first[stream]["path"]
            self.assertFalse(Path(locator).is_absolute())
            data = (self.repo / locator).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), first[stream]["sha256"])
            self.assertEqual(len(data), first[stream]["bytes"])

        draft.write_text("two\n")
        second_result, _ = self.invoke("dirty-two", [sys.executable, "-c", "pass"])
        self.assertEqual(second_result.returncode, 0, second_result.stderr)
        second = self.receipt("dirty-two")
        self.assertNotEqual(first["candidate_before"]["source_sha256"], second["candidate_before"]["source_sha256"])
        self.assertEqual(first["candidate_before"]["index_sha256"], second["candidate_before"]["index_sha256"])

        subprocess.run(["git", "-C", str(self.repo), "add", "tracked.txt"], check=True)
        staged_result, _ = self.invoke("dirty-staged", [sys.executable, "-c", "pass"])
        self.assertEqual(staged_result.returncode, 0, staged_result.stderr)
        staged = self.receipt("dirty-staged")
        self.assertEqual(second["candidate_before"]["source_sha256"], staged["candidate_before"]["source_sha256"])
        self.assertNotEqual(second["candidate_before"]["index_sha256"], staged["candidate_before"]["index_sha256"])

    def test_failed_child_retains_exact_exit_and_output(self):
        child = [sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); sys.exit(17)"]
        result, _ = self.invoke("failure", child)
        receipt = self.receipt("failure")
        self.assertEqual(result.returncode, 17)
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual(receipt["command_exit_code"], 17)
        self.assertEqual(receipt["exit_code"], 17)
        self.assertEqual((self.repo / receipt["stdout"]["path"]).read_bytes(), b"out\n")
        self.assertEqual((self.repo / receipt["stderr"]["path"]).read_bytes(), b"err\n")
        self.assertTrue(receipt["stdout"]["verified"])
        self.assertTrue(receipt["stderr"]["verified"])

    def test_replaced_log_cannot_pass_with_forged_bytes(self):
        code = (
            "from pathlib import Path; "
            "p=Path('target/agent-proof/replaced-log/stdout.log'); "
            "print('genuine', flush=True); p.unlink(); p.write_bytes(b'forged\\n')"
        )
        result, _ = self.invoke("replaced-log", [sys.executable, "-c", code])
        receipt = self.receipt("replaced-log")
        self.assertEqual(result.returncode, 124)
        self.assertEqual(receipt["status"], "evidence_error")
        self.assertEqual(receipt["command_exit_code"], 0)
        self.assertFalse(receipt["output_complete"])
        self.assertFalse(receipt["stdout"]["verified"])
        self.assertTrue(receipt["stderr"]["verified"])
        self.assertEqual(receipt["stdout"]["sha256"], hashlib.sha256(b"genuine\n").hexdigest())
        self.assertEqual((self.repo / receipt["stdout"]["path"]).read_bytes(), b"forged\n")

    def test_in_place_log_overwrite_preserves_failed_child_code(self):
        code = (
            "from pathlib import Path\nimport sys,time\n"
            "p=Path('target/agent-proof/in-place/stdout.log')\n"
            "print('genuine', flush=True)\n"
            "for _ in range(300):\n"
            "    if p.read_bytes() == b'genuine\\n': break\n"
            "    time.sleep(0.01)\n"
            "else: sys.exit(91)\n"
            "p.write_bytes(b'forged\\n')\nsys.exit(17)\n"
        )
        result, _ = self.invoke("in-place", [sys.executable, "-c", code])
        receipt = self.receipt("in-place")
        self.assertEqual(result.returncode, 17)
        self.assertEqual(receipt["status"], "evidence_error")
        self.assertEqual(receipt["command_exit_code"], 17)
        self.assertFalse(receipt["stdout"]["verified"])
        self.assertEqual(receipt["stdout"]["sha256"], hashlib.sha256(b"genuine\n").hexdigest())
        self.assertEqual((self.repo / receipt["stdout"]["path"]).read_bytes(), b"forged\n")

    def test_symlink_log_replacement_cannot_pass(self):
        code = (
            "from pathlib import Path; "
            "p=Path('target/agent-proof/symlink-log/stdout.log'); "
            "print('genuine', flush=True); p.unlink(); p.symlink_to('stderr.log')"
        )
        result, _ = self.invoke("symlink-log", [sys.executable, "-c", code])
        receipt = self.receipt("symlink-log")
        self.assertEqual(result.returncode, 124)
        self.assertEqual(receipt["status"], "evidence_error")
        self.assertFalse(receipt["stdout"]["verified"])
        self.assertTrue((self.repo / receipt["stdout"]["path"]).is_symlink())

    def test_index_flags_invalidate_candidate(self):
        for run_id, flag, undo in (
            ("skip-worktree", "--skip-worktree", "--no-skip-worktree"),
            ("assume-unchanged", "--assume-unchanged", "--no-assume-unchanged"),
        ):
            result, _ = self.invoke(run_id, ["git", "update-index", flag, "tracked.txt"])
            receipt = self.receipt(run_id)
            self.assertEqual(result.returncode, 125, result.stderr)
            self.assertEqual(receipt["status"], "stale")
            self.assertEqual(receipt["command_exit_code"], 0)
            self.assertEqual(receipt["candidate_before"]["source_sha256"], receipt["candidate_after"]["source_sha256"])
            self.assertNotEqual(receipt["candidate_before"]["index_sha256"], receipt["candidate_after"]["index_sha256"])
            subprocess.run(["git", "-C", str(self.repo), "update-index", undo, "tracked.txt"], check=True)

    def test_deleted_nested_tracked_path_has_stable_dirty_identity(self):
        nested = self.repo / "nested" / "deeper"
        nested.mkdir(parents=True)
        leaf = nested / "leaf.txt"
        leaf.write_text("tracked\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "nested/deeper/leaf.txt"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "-c", "user.name=Proof Test", "-c", "user.email=proof@example.test", "commit", "-qm", "nested"],
            check=True,
        )
        leaf.unlink()
        nested.rmdir()
        nested.parent.rmdir()
        result, _ = self.invoke("deleted-nested", [sys.executable, "-c", "pass"])
        receipt = self.receipt("deleted-nested")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(receipt["candidate_before"]["state"], "dirty")
        self.assertEqual(receipt["candidate_before"], receipt["candidate_after"])

    def test_tracked_mutation_cannot_produce_passed_receipt(self):
        child = [sys.executable, "-c", "from pathlib import Path; Path('tracked.txt').write_text('mutated\\n'); print('child succeeded')"]
        result, _ = self.invoke("mutation", child)
        receipt = self.receipt("mutation")
        self.assertEqual(result.returncode, 125)
        self.assertEqual(receipt["status"], "stale")
        self.assertEqual(receipt["command_exit_code"], 0)
        self.assertNotEqual(receipt["candidate_before"]["source_sha256"], receipt["candidate_after"]["source_sha256"])
        self.assertIn(b"child succeeded", result.stdout)

        failed_child = [
            sys.executable, "-c",
            "from pathlib import Path; import sys; Path('tracked.txt').write_text('again'); sys.exit(23)",
        ]
        result, _ = self.invoke("failed-mutation", failed_child)
        receipt = self.receipt("failed-mutation")
        self.assertEqual(result.returncode, 23)
        self.assertEqual(receipt["status"], "stale")
        self.assertEqual(receipt["command_exit_code"], 23)

    def test_restored_content_and_new_untracked_file_are_stale(self):
        restored = [
            sys.executable, "-c",
            "from pathlib import Path; p=Path('tracked.txt'); old=p.read_bytes(); p.write_bytes(b'changed'); p.write_bytes(old)",
        ]
        result, _ = self.invoke("restored", restored)
        receipt = self.receipt("restored")
        self.assertEqual(result.returncode, 125)
        self.assertEqual(receipt["status"], "stale")
        self.assertEqual(receipt["candidate_before"]["source_sha256"], receipt["candidate_after"]["source_sha256"])
        self.assertNotEqual(receipt["candidate_before"]["source_metadata_sha256"], receipt["candidate_after"]["source_metadata_sha256"])

        added = [sys.executable, "-c", "from pathlib import Path; Path('new-source.txt').write_text('new')"]
        result, _ = self.invoke("new-untracked", added)
        receipt = self.receipt("new-untracked")
        self.assertEqual(result.returncode, 125)
        self.assertEqual(receipt["status"], "stale")
        self.assertNotEqual(receipt["candidate_before"]["source_sha256"], receipt["candidate_after"]["source_sha256"])

    def test_existing_run_and_symlinks_cannot_be_overwritten_or_executed(self):
        proof_root = self.repo / "target" / "agent-proof"
        prior = proof_root / "prior"
        prior.mkdir(parents=True)
        (prior / "sentinel").write_text("keep")
        marker = self.repo / "executed"
        child = [sys.executable, "-c", "from pathlib import Path; Path('executed').write_text('yes')"]

        result, _ = self.invoke("prior", child)
        self.assertEqual(result.returncode, 2)
        self.assertEqual((prior / "sentinel").read_text(), "keep")
        self.assertFalse(marker.exists())

        outside = Path(self.temporary.name) / "outside"
        outside.mkdir()
        (proof_root / "linked").symlink_to(outside, target_is_directory=True)
        result, _ = self.invoke("linked", child)
        self.assertEqual(result.returncode, 2)
        self.assertFalse(marker.exists())
        self.assertEqual(list(outside.iterdir()), [])

        another = Path(self.temporary.name) / "another"
        initialize_repo(another)
        (another / "target").symlink_to(outside, target_is_directory=True)
        result, _ = self.invoke("target-link", child, repo=another)
        self.assertEqual(result.returncode, 2)
        self.assertFalse((another / "executed").exists())
        self.assertEqual(list(outside.iterdir()), [])

    def test_child_signal_is_interrupted(self):
        child = [sys.executable, "-c", "import os,signal; os.kill(os.getpid(), signal.SIGTERM)"]
        result, _ = self.invoke("child-signal", child)
        receipt = self.receipt("child-signal")
        self.assertEqual(result.returncode, 128 + signal.SIGTERM)
        self.assertEqual(receipt["status"], "interrupted")
        self.assertEqual(receipt["command_signal"], signal.SIGTERM)
        self.assertIsNone(receipt["command_exit_code"])

    def test_wrapper_signal_is_interrupted_and_forwarded(self):
        child = [sys.executable, "-c", "import time; print('ready', flush=True); time.sleep(10)"]
        process = subprocess.Popen(
            [sys.executable, str(SCRIPT), "--run-id", "wrapper-signal", "--", *child],
            cwd=self.repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            self.assertEqual(process.stdout.readline(), b"ready\n")
            os.kill(process.pid, signal.SIGTERM)
            process.communicate(timeout=5)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        receipt = self.receipt("wrapper-signal")
        self.assertEqual(process.returncode, 128 + signal.SIGTERM)
        self.assertEqual(receipt["status"], "interrupted")
        self.assertEqual(receipt["wrapper_signal"], signal.SIGTERM)

    def test_inherited_pipe_is_bounded_and_never_passed(self):
        process, leader, grandchild = self.spawn_pipe_test("inherited-pipe")
        try:
            process.communicate(timeout=5)
            receipt = self.receipt("inherited-pipe")
            self.assertEqual(process.returncode, 124)
            self.assertEqual(receipt["status"], "incomplete")
            self.assertEqual(receipt["command_exit_code"], 0)
            self.assertFalse(receipt["output_complete"])
            self.assertTrue(wait_stopped(grandchild), "inherited-pipe descendant kept running")
        finally:
            self.stop_pipe_test(process, leader, grandchild)

    def test_signal_after_leader_exit_kills_inherited_pipe_descendant(self):
        process, leader, grandchild = self.spawn_pipe_test("inherited-signal")
        try:
            self.assertTrue(wait_stopped(leader), "command leader did not exit")
            os.kill(process.pid, signal.SIGTERM)
            process.communicate(timeout=5)
            receipt = self.receipt("inherited-signal")
            self.assertEqual(process.returncode, 128 + signal.SIGTERM)
            self.assertEqual(receipt["status"], "interrupted")
            self.assertEqual(receipt["wrapper_signal"], signal.SIGTERM)
            self.assertFalse(receipt["output_complete"])
            self.assertTrue(wait_stopped(grandchild), "signalled descendant kept running")
        finally:
            self.stop_pipe_test(process, leader, grandchild)

    def test_ignored_termination_signal_escalates(self):
        child = [
            sys.executable, "-c",
            "import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print(os.getpid(),flush=True); time.sleep(30)",
        ]
        process = subprocess.Popen(
            [sys.executable, str(SCRIPT), "--run-id", "ignored-term", "--", *child],
            cwd=self.repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.assertTrue(select.select([process.stdout], [], [], 3)[0])
        child_pid = int(process.stdout.readline())
        try:
            os.kill(process.pid, signal.SIGTERM)
            process.communicate(timeout=5)
            receipt = self.receipt("ignored-term")
            self.assertEqual(process.returncode, 128 + signal.SIGTERM)
            self.assertEqual(receipt["status"], "interrupted")
            self.assertEqual(receipt["wrapper_signal"], signal.SIGTERM)
            self.assertEqual(receipt["command_signal"], signal.SIGKILL)
            self.assertTrue(wait_stopped(child_pid), "SIGTERM-ignoring child kept running")
        finally:
            if process.poll() is None:
                process.kill()
            if process_running(child_pid):
                try:
                    os.killpg(child_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.communicate(timeout=2)


if __name__ == "__main__":
    unittest.main()
