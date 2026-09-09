#!/usr/bin/env python3
# scripts/test-release-matrix-runner.py
"""Exercise mutation batching, failure attribution, and cleanup without the checker."""

import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import time
import unittest


SOURCE = Path(__file__).with_name("test-release-matrix.sh").read_text()
assert SOURCE.endswith('main "$@"\n')
LIBRARY = SOURCE.removesuffix('main "$@"\n')


class MutationRunnerTests(unittest.TestCase):
    def run_probe(self, jobs="4", fail_case="", records=None, interrupt=None, launch_interrupt=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            run_root = root / "run"
            run_root.mkdir()
            events = root / "events"
            record_file = root / "records"
            if records is None:
                records = b"".join(
                    b"\0".join([str(n).encode(), b"replace", b"file", b"old", b"new", b"expected"]) + b"\0"
                    for n in range(7)
                )
            record_file.write_bytes(records)
            lines = LIBRARY.splitlines()
            lines = [
                'REPO_ROOT=' + shlex.quote(str(root)) if line.startswith('REPO_ROOT=')
                else 'TEST_RUN_ROOT=' + shlex.quote(str(run_root)) if line.startswith('TEST_RUN_ROOT=')
                else line for line in lines
            ]
            shell = "\n".join(lines) + r'''
release_matrix_mutation_cases() { cat "$PROBE_RECORDS"; }
run_release_policy_mutation_case() {
    trap - EXIT INT TERM
    printf 'start %s\n' "$1" >> "$PROBE_EVENTS"
    if [[ "$1" == 0 ]]; then sleep 0.04; else sleep 0.15; fi
    [[ -d "$TEST_RUN_ROOT" ]] || { echo 'EARLY CLEANUP' >&2; exit 90; }
    printf 'done %s\n' "$1" >> "$PROBE_EVENTS"
    if [[ "$1" == "$PROBE_FAILURE" ]]; then
        echo "causal failure $1" >&2
        exit 7
    fi
    printf 'ok - %s\n' "$1"
}
run_release_policy_mutation_cases
'''
            if launch_interrupt:
                shell = shell.replace("\nrun_release_policy_mutation_cases\n", r'''
set -T
probe_sent=0
trap 'if [[ "$BASH_COMMAND" == "pid=\$!" && "$probe_sent" == 0 ]]; then probe_sent=1; kill -TERM "$BASHPID"; fi' DEBUG
run_release_policy_mutation_cases
''')
            env = dict(os.environ, CONARY_RELEASE_MATRIX_MUTATION_JOBS=jobs,
                       PROBE_RECORDS=str(record_file), PROBE_EVENTS=str(events),
                       PROBE_FAILURE=fail_case)
            probe = root / "probe.sh"
            probe.write_text(shell)
            process = subprocess.Popen(["bash", str(probe)], env=env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       text=True, start_new_session=True)
            if interrupt is not None:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if events.exists() and events.read_text().count("start") == 4:
                        break
                    time.sleep(0.005)
                else:
                    process.kill()
                    process.communicate()
                    self.fail("workers did not start")
                os.kill(process.pid, interrupt)
            try:
                stdout, stderr = process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
                raise
            history = events.read_text().splitlines() if events.exists() else []
            self.assertFalse(run_root.exists(), "parent did not clean fixture root")
            self.assertNotIn("EARLY CLEANUP", stdout + stderr)
            active = set()
            maximum = 0
            for event in history:
                operation, name = event.split()
                if operation == "start":
                    self.assertNotIn(name, active)
                    active.add(name)
                    maximum = max(maximum, len(active))
                else:
                    active.remove(name)
            self.assertFalse(active, "cleanup returned before all workers completed")
            return process.returncode, stdout, stderr, history, maximum

    def test_serial_and_parallel_preserve_exact_case_order(self):
        expected = "".join(f"ok - {n}\n" for n in range(7))
        for jobs in ("1", "4"):
            with self.subTest(jobs=jobs):
                code, out, err, history, maximum = self.run_probe(jobs)
                self.assertEqual((code, out, err), (0, expected, ""))
                self.assertEqual(len(history), 14)
                self.assertEqual(maximum, int(jobs))

    def test_failure_drains_batch_and_preserves_first_cause(self):
        code, out, err, history, _ = self.run_probe(fail_case="0")
        self.assertEqual(code, 1)
        self.assertEqual(out, "causal failure 0\n")
        self.assertIn("case 0 failed with exit status 7", err)
        self.assertEqual(len(history), 8)

    def test_signals_drain_before_cleanup(self):
        for sig, expected in ((signal.SIGINT, 130), (signal.SIGTERM, 143)):
            with self.subTest(signal=sig):
                code, _, _, history, _ = self.run_probe(interrupt=sig)
                self.assertEqual(code, expected)
                self.assertEqual(len(history), 8)

    def test_signal_between_launch_and_pid_registration_drains_worker(self):
        code, _, _, history, _ = self.run_probe(launch_interrupt=True)
        self.assertEqual(code, 143)
        self.assertEqual(history, ["start 0", "done 0"])

    def test_invalid_concurrency_never_starts_worker(self):
        for jobs in ("", "0", "5", "04", "oops"):
            with self.subTest(jobs=jobs):
                code, _, err, history, _ = self.run_probe(jobs=jobs)
                self.assertEqual(code, 1)
                self.assertIn("must be 1, 2, 3, or 4", err)
                self.assertEqual(history, [])

    def test_incomplete_records_never_start_worker(self):
        valid = b"0\0replace\0file\0old\0new\0expected\0"
        for records in (b"", valid[:-1], valid + b"trailing", valid + b"partial\0"):
            with self.subTest(records=records):
                code, _, _, history, _ = self.run_probe(records=records)
                self.assertEqual(code, 1)
                self.assertEqual(history, [])


if __name__ == "__main__":
    unittest.main()
