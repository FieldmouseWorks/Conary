#!/usr/bin/env python3
"""Prove launch-state completion selection and shared release/candidate wiring."""
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / ".github/actions/deploy-remi-bundle/resolve-completion.py"
spec = importlib.util.spec_from_file_location("completion", POLICY)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def launch(state="blocked"):
    return {"schema_version": 1, "gates": {"public_universe": {
        "state": state, "promotion_threshold": "zero_exclusions"}}}


class CompletionTests(unittest.TestCase):
    def test_no_universe_proves_private_candidates(self):
        self.assertEqual(module.resolve("launch-authority", launch()), "private-candidates")

    def test_active_universe_requires_repopulation(self):
        self.assertEqual(module.resolve("launch-authority", launch("passed")), "active-repopulation")

    def test_manual_candidate_mode_is_explicit(self):
        for mode in ("private-candidates", "active-repopulation"):
            self.assertEqual(module.resolve(mode, launch()), mode)

    def test_unknown_or_incomplete_authority_never_defaults(self):
        invalid = [None, {}, {"schema_version": True}, {"schema_version": 2},
                   launch("pending"), launch("unknown"), launch(None)]
        weak = launch()
        weak["gates"]["public_universe"]["promotion_threshold"] = "some_exclusions"
        invalid.append(weak)
        for value in invalid:
            with self.subTest(value=value), self.assertRaises((ValueError, KeyError, TypeError)):
                module.resolve("launch-authority", value)

    def test_unknown_mode_fails(self):
        with self.assertRaises(ValueError):
            module.resolve("automatic-ish", launch())

    def test_cli_has_one_machine_readable_result(self):
        for state, expected in (("blocked", "private-candidates"), ("passed", "active-repopulation")):
            result = subprocess.run(["python3", str(POLICY), "launch-authority"],
                                    input=json.dumps(launch(state)), capture_output=True,
                                    text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, expected + "\n")

    def test_cli_invalid_json_has_no_authorizing_stdout(self):
        result = subprocess.run(["python3", str(POLICY), "launch-authority"], input="{",
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("invalid deployment launch authority", result.stderr)

    def test_checked_in_launch_authority_resolves(self):
        status = json.loads((ROOT / "docs/roadmaps/launch-status.json").read_text())
        self.assertIn(module.resolve("launch-authority", status),
                      ("private-candidates", "active-repopulation"))


if __name__ == "__main__":
    unittest.main()
