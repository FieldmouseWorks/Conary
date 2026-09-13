#!/usr/bin/env python3
"""Exercise the real root helper with bounded fake-root inspection and HTTP ingress."""
import copy
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

HELPER = Path(__file__).resolve().parents[1] / 'deploy/remi-deploy-helper.sh'


def inspection_record(catalog="retained"):
    return {
        "catalog": catalog, "schema_epoch": "test-v1", "schema_revision": 1,
        "configured_profiles": 3, "populated_profiles": 3, "candidate_profiles": 3,
        "profiles": [], "candidates": [], "candidate_verification": {
            "mode": "full_reopen", "completed_after": None, "elapsed_micros": 10,
            "catalog_files_reopened": 3, "catalog_bytes_hashed": 100,
            "catalog_bytes_integrity_checked": 100,
        },
    }


class WaitTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name in ('conary/site', 'etc/conary', 'usr/local/bin'):
            (self.root / name).mkdir(parents=True)
        (self.root / 'conary').chmod(0o750)
        (self.root / 'etc/conary/remi.toml').write_text('')
        (self.root / 'conary/site/index.html').write_bytes(b'home\n')
        (self.root / 'conary/site/install-conary-preview.sh').write_bytes(b'installer\n')
        self.bad_ingress = False
        self.ingress_delay = 0
        test = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if test.ingress_delay:
                    time.sleep(test.ingress_delay)
                data = b'installer\n' if self.path.endswith('.sh') else b'home\n'
                if test.bad_ingress:
                    data = b'wrong deployed bytes'
                self.send_response(200)
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_args):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        base = f'http://127.0.0.1:{self.server.server_port}'
        self.env = dict(os.environ, CONARY_REMI_DEPLOY_ROOT=str(self.root),
                        CONARY_REMI_DEPLOY_SITE_HOME_URL=base+'/',
                        CONARY_REMI_DEPLOY_SITE_INSTALLER_URL=base+'/install-conary-preview.sh')
        self.write_inspection(inspection_record())

    def write_inspector(self, body):
        path = self.root / 'usr/local/bin/remi'
        path.write_text('#!/bin/bash\n'+body)
        path.chmod(0o755)

    def write_inspection(self, value, status=0, delay=0):
        body = f"sleep {delay}\n" if delay else ""
        # JSON is supplied as a quoted here-document, never interpreted by Bash.
        self.write_inspector(body + "cat <<'INSPECTION_JSON'\n" + json.dumps(value) +
                             f"\nINSPECTION_JSON\nexit {status}\n")

    def run_wait(self, budget=2):
        started = time.monotonic()
        result = subprocess.run(['/bin/bash', str(HELPER), 'wait-remi-repopulation', str(budget)],
                                env=self.env, capture_output=True, text=True, timeout=8)
        self.assertLess(time.monotonic()-started, budget+2)
        return result, json.loads(result.stdout)

    def test_complete_proves_inspection_and_exact_ingress(self):
        result, evidence = self.run_wait()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(evidence['catalog'], 'retained')
        wait = evidence['repopulation_wait']
        self.assertEqual(wait['outcome'], 'complete')
        self.assertIsNone(wait['reason'])
        self.assertLessEqual(wait['elapsed_ms'], wait['budget_ms'])
        self.assertEqual([a['operation'] for a in wait['attempts']], ['inspect', 'ingress'])
        self.assertTrue(all(a['status'] == 0 and not a['timed_out'] for a in wait['attempts']))

    def test_typed_pending_keeps_last_inspection_and_caps_sleep(self):
        self.write_inspection(inspection_record("pending"), status=1)
        result, evidence = self.run_wait(1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(evidence['catalog'], 'pending')
        self.assertEqual(evidence['repopulation_wait']['reason'], 'deadline_exceeded')
        self.assertEqual(len(evidence['repopulation_wait']['attempts']), 1)

    def test_blocked_inspection_kills_descendants_after_leader_exit(self):
        pid_file = self.root / 'child.pid'
        # The helper's pipeline leader can exit before this descendant closes stdout.
        self.write_inspector('sleep 30 &\nprintf "%s\\n" "$!" > "'+str(pid_file)+'"\nexit 0\n')
        result, evidence = self.run_wait(1)
        self.assertNotEqual(result.returncode, 0)
        wait = evidence['repopulation_wait']
        self.assertEqual(wait['reason'], 'deadline_exceeded')
        self.assertTrue(wait['attempts'][0]['timed_out'])
        pid = int(pid_file.read_text())
        stat = Path(f'/proc/{pid}/stat')
        if stat.exists():
            # A reparented dead child may briefly remain a zombie until init reaps it.
            self.assertEqual(stat.read_text().rsplit(')', 1)[1].split()[0], 'Z')

    def test_invalid_json_fails_without_retry_or_ingress(self):
        self.write_inspector('printf "not-json\\n"\n')
        result, evidence = self.run_wait()
        self.assertNotEqual(result.returncode, 0)
        # inspect-remi's own jq rejects invalid JSON, so this is a command failure.
        self.assertEqual(evidence['repopulation_wait']['reason'], 'inspection_failed')
        self.assertEqual(len(evidence['repopulation_wait']['attempts']), 1)

    def test_empty_success_is_invalid_inspection(self):
        self.write_inspector('exit 0\n')
        result, evidence = self.run_wait()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(evidence['repopulation_wait']['reason'], 'invalid_inspection')

    def test_failed_inspection_without_json_fails_immediately(self):
        self.write_inspector('exit 7\n')
        result, evidence = self.run_wait()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(evidence['repopulation_wait']['reason'], 'inspection_failed')
        self.assertLess(evidence['repopulation_wait']['elapsed_ms'], 1000)

    def test_wrong_ingress_bytes_never_complete(self):
        self.bad_ingress = True
        result, evidence = self.run_wait()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(evidence['catalog'], 'retained')
        self.assertEqual(evidence['repopulation_wait']['reason'], 'ingress_failed')

    def test_ingress_uses_remaining_shared_deadline(self):
        self.ingress_delay = 2
        result, evidence = self.run_wait(1)
        self.assertNotEqual(result.returncode, 0)
        wait = evidence['repopulation_wait']
        self.assertEqual(wait['reason'], 'deadline_exceeded')
        self.assertEqual(wait['attempts'][-1]['operation'], 'ingress')
        self.assertTrue(wait['attempts'][-1]['timed_out'])

    def test_malformed_inspection_objects_fail_without_polling(self):
        valid = inspection_record()
        malformed = [{}, {"schema_epoch": "partial"}]
        fields = [(name,) for name in (
            "schema_epoch", "schema_revision", "configured_profiles", "populated_profiles",
            "candidate_profiles", "profiles", "candidates", "candidate_verification")]
        fields += [("candidate_verification", name) for name in (
            "mode", "elapsed_micros", "catalog_files_reopened", "catalog_bytes_hashed",
            "catalog_bytes_integrity_checked")]
        for path in fields:
            for replacement in ("missing", True):
                value = copy.deepcopy(valid)
                parent = value if len(path) == 1 else value[path[0]]
                if replacement == "missing":
                    del parent[path[-1]]
                else:
                    parent[path[-1]] = replacement
                malformed.append(value)
        for path, replacement in (("mode", "unknown"), ("completed_after", "yesterday")):
            value = copy.deepcopy(valid)
            value["candidate_verification"][path] = replacement
            malformed.append(value)
        for status in (0, 7):
            for value in malformed:
                with self.subTest(status=status, value=value):
                    self.write_inspection(value, status=status)
                    result, evidence = self.run_wait(1)
                    self.assertNotEqual(result.returncode, 0)
                    wait = evidence['repopulation_wait']
                    self.assertEqual(wait['reason'],
                                     'invalid_inspection' if status == 0 else 'inspection_failed')
                    self.assertLess(wait['elapsed_ms'], 1000)
                    self.assertEqual(len(wait['attempts']), 1)
                    self.assertNotIn('schema_epoch', evidence)

    def test_inspection_time_is_deducted_from_ingress_budget(self):
        self.write_inspection(inspection_record(), delay=1)
        self.ingress_delay = 0.75
        result, evidence = self.run_wait(2)
        self.assertNotEqual(result.returncode, 0)
        wait = evidence['repopulation_wait']
        self.assertEqual(wait['reason'], 'deadline_exceeded')
        self.assertGreaterEqual(wait['attempts'][0]['duration_ms'], 900)
        self.assertEqual(wait['attempts'][-1]['operation'], 'ingress')
        self.assertTrue(wait['attempts'][-1]['timed_out'])

    def test_invalid_budget_never_starts_inspection(self):
        marker = self.root / 'started'
        self.write_inspector('touch "'+str(marker)+'"\n')
        for budget in ('0', '-1', '01', '+1', '1.5', '3601', '99999', '1;id'):
            with self.subTest(budget=budget):
                result = subprocess.run(['/bin/bash', str(HELPER), 'wait-remi-repopulation', budget],
                                        env=self.env, capture_output=True, text=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(marker.exists())


if __name__ == '__main__':
    unittest.main()
