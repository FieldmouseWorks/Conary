#!/usr/bin/env python3
"""Exercise the real readiness helper against deterministic elapsed intervals."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "deploy/remi-deploy-helper.sh"
FAKE = """#!/usr/bin/env python3
import json, os, sys
from decimal import Decimal
from pathlib import Path
p = Path(os.environ['READINESS_FIXTURE'])
s = json.loads(p.read_text())
operation = Path(sys.argv[0]).name
if operation == 'clock':
    print(f"{s['now_ms'] // 1000}.{s['now_ms'] % 1000:03d}")
elif operation == 'systemctl':
    assert sys.argv[1:] == ['start', 'remi']
    s['starts'] += 1
    s['now_ms'] += s['start_ms']
elif operation == 'curl':
    timeout = Decimal(sys.argv[sys.argv.index('--max-time') + 1])
    s['timeouts_ms'].append(int(timeout * 1000))
    index = len(s['timeouts_ms']) - 1
    assert index < len(s['probes']), 'unexpected probe'
    probe = s['probes'][index]
    s['now_ms'] += probe['duration_ms']
    p.write_text(json.dumps(s))
    sys.exit(probe['status'])
else:
    raise AssertionError(operation)
p.write_text(json.dumps(s))
"""


class ReadinessClockTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="remi-readiness-clock-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.state = self.root / "var/lib/conary-remi-deploy/readiness.json"
        self.state.parent.mkdir(parents=True, mode=0o700)
        self.state.write_text(json.dumps({"schema_version": 1, "last_ready_duration_seconds": 1}))
        self.fixture = self.root / "fixture.json"
        for name in ("clock", "systemctl", "curl"):
            path = self.root / name
            path.write_text(FAKE)
            path.chmod(0o700)

    def run_probe(self, phase_ms, start_ms, probes):
        fixture = {
            "now_ms": 100000 + phase_ms,
            "start_ms": start_ms,
            "starts": 0,
            "probes": probes,
            "timeouts_ms": [],
        }
        self.fixture.write_text(json.dumps(fixture))
        env = os.environ.copy()
        env.update(
            CONARY_REMI_DEPLOY_ROOT=str(self.root),
            CONARY_REMI_DEPLOY_SKIP_RESTART="0",
            CONARY_REMI_DEPLOY_TEST_CLOCK=str(self.root / "clock"),
            CONARY_REMI_DEPLOY_TEST_CURL=str(self.root / "curl"),
            READINESS_FIXTURE=str(self.fixture),
        )
        result = subprocess.run(
            ["bash", "-c", 'source "$1"; REMI_SYSTEMCTL="$2"; '
             'status=0; start_and_probe || status=$?; '
             'printf "%s\\n" "$READINESS_INSPECTION"; exit "$status"',
             "readiness-fixture", str(HELPER), str(self.root / "systemctl")],
            env=env, text=True, capture_output=True, timeout=10,
        )
        self.assertIn(result.returncode, (0, 1), result.stderr)
        inspection = json.loads(result.stdout)
        observed = json.loads(self.fixture.read_text())
        self.assertEqual(observed["starts"], 1)
        self.assertEqual(inspection["multiplier"], 2)
        self.assertEqual(inspection["ceiling_seconds"], 7200)
        self.assertEqual(inspection["systemctl_status"], 0)
        self.assertEqual(json.loads(self.state.read_text()), inspection)
        return result.returncode, inspection, observed

    def test_failed_probe_does_not_lose_remaining_time_at_a_clock_boundary(self):
        for phase in (0, 950):
            with self.subTest(phase_ms=phase):
                self.state.write_text(json.dumps({"schema_version": 1, "last_ready_duration_seconds": 1}))
                status, evidence, fixture = self.run_probe(phase, 100, [
                    {"duration_ms": 1000, "status": 22},
                    {"duration_ms": 0, "status": 0},
                ])
                self.assertEqual(status, 0, evidence)
                self.assertEqual(evidence["outcome"], "restored")
                self.assertEqual(evidence["budget_seconds"], 2)
                self.assertEqual(len(fixture["timeouts_ms"]), 2)
                self.assertEqual(evidence["restart_to_ready_seconds"], 2)
                self.assertEqual(evidence["elapsed_seconds"], 2)

    def test_response_at_exact_deadline_is_accepted(self):
        status, evidence, _ = self.run_probe(950, 0, [{"duration_ms": 2000, "status": 0}])
        self.assertEqual(status, 0, evidence)
        self.assertEqual(evidence["restart_to_ready_seconds"], 2)

    def test_response_after_deadline_is_rejected(self):
        status, evidence, _ = self.run_probe(0, 0, [{"duration_ms": 2001, "status": 0}])
        self.assertEqual(status, 1, evidence)
        self.assertEqual(evidence["reason"], "readiness_timeout")
        self.assertIsNone(evidence["restart_to_ready_seconds"])
        self.assertEqual(evidence["last_ready_duration_seconds"], 1)
        self.assertEqual(evidence["elapsed_seconds"], 3)

    def test_service_start_consumes_the_same_deadline(self):
        status, evidence, fixture = self.run_probe(950, 2000, [])
        self.assertEqual(status, 1, evidence)
        self.assertEqual(evidence["elapsed_seconds"], 2)
        self.assertEqual(fixture["timeouts_ms"], [])

    def test_probe_timeout_is_limited_to_fractional_remaining_budget(self):
        status, evidence, fixture = self.run_probe(950, 1500, [{"duration_ms": 500, "status": 0}])
        self.assertEqual(status, 0, evidence)
        self.assertEqual(fixture["timeouts_ms"], [500])
        self.assertEqual(evidence["budget_seconds"], 2)

    def test_completed_duration_rounds_up_before_next_budget_is_derived(self):
        status, first, _ = self.run_probe(950, 0, [{"duration_ms": 1500, "status": 0}])
        self.assertEqual(status, 0, first)
        self.assertEqual(first["last_ready_duration_seconds"], 2)
        status, second, _ = self.run_probe(0, 0, [{"duration_ms": 3000, "status": 0}])
        self.assertEqual(status, 0, second)
        self.assertEqual(second["basis_seconds"], 2)
        self.assertEqual(second["budget_seconds"], 4)
        self.assertEqual(second["last_ready_duration_seconds"], 3)


if __name__ == "__main__":
    unittest.main()
