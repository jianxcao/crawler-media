import copy
import json
import os
import subprocess
import sys
import tempfile
import unittest

from report import build_report, format_markdown_report
from test_evaluate import inputs


class TestBenchmarkReport(unittest.TestCase):
    def test_unlabelled_run_cannot_pass_accuracy(self):
        manifest, _, _ = inputs()
        rep = build_report([{"accuracy_errors": [], "measurement_complete": True}], manifest, [], [])
        self.assertFalse(rep["accuracy_gate_passed"])
        self.assertNotEqual(rep["accuracy"]["status"], "passed")

    def test_negative_self_reported_error_cannot_pass_accuracy(self):
        manifest, labels, predictions = inputs()
        predictions[0]["interval_ms"] = [20000, 90000]
        rep = build_report([{"accuracy_errors": [{"boundary_error_ms": -10000}]}],
                           manifest, labels, predictions)
        self.assertFalse(rep["accuracy_gate_passed"])
        self.assertEqual(rep["accuracy"]["micro"]["severe_protected_overlap"], 1)

    def test_unknown_network_bytes_do_not_produce_saving_percent(self):
        runs = [{"sampling_mode": "full_window", "input_bytes": None, "measurement_complete": False},
                {"sampling_mode": "adaptive", "input_bytes": 1000, "measurement_complete": True}]
        rep = build_report(runs, *inputs())
        self.assertIsNone(rep["input_bytes_saving_percent"])
        self.assertFalse(rep["measurement_complete"])
        self.assertTrue(rep["accuracy_complete"])

    def test_attempts_are_recorded_but_unpaired_performance_is_pending(self):
        runs = [{"sampling_mode": "full_window", "input_bytes": 10000, "measurement_complete": True,
                 "attempts": [{"success": True}]},
                {"sampling_mode": "adaptive", "input_bytes": 5000, "measurement_complete": True,
                 "attempts": [{"success": False}, {"success": False}, {"success": True}]}]
        rep = build_report(runs, *inputs())
        self.assertTrue(rep["measurement_complete"])
        self.assertIsNone(rep["input_bytes_saving_percent"])
        self.assertEqual(rep["performance_status"], "pairing_pending")
        self.assertEqual(rep["failed_attempts"], 2)
        self.assertEqual(rep["accuracy"]["status"], "insufficient")
        self.assertIn("insufficient", format_markdown_report(rep))

    def test_unknown_network_does_not_invalidate_frozen_accuracy(self):
        manifest, labels, predictions = inputs()
        manifest.update(evidence_kind="human_reviewed")
        manifest["acceptance_policy"] = {"frozen": True, "min_sources": 1, "min_media_groups": 1,
            "min_holdout_sources": 1, "min_holdout_media_groups": 1,
            "required_kinds": ["intro"], "bootstrap_samples": 100}
        rep = build_report([{"input_bytes": None}], manifest, labels, predictions)
        self.assertTrue(rep["accuracy_gate_passed"])
        self.assertFalse(rep["measurement_complete"])

    def test_cli_exit_codes_and_json_sidecar(self):
        manifest, labels, predictions = inputs()
        frozen = copy.deepcopy(manifest)
        frozen.update(evidence_kind="human_reviewed")
        frozen["acceptance_policy"] = {"frozen": True, "min_sources": 1, "min_media_groups": 1,
            "min_holdout_sources": 1, "min_holdout_media_groups": 1,
            "required_kinds": ["intro"], "bootstrap_samples": 100}
        early = copy.deepcopy(predictions)
        early[0]["interval_ms"] = [20000, 90000]
        for expected_status, expected_code, m, p in [
            ("insufficient", 2, manifest, predictions), ("invalid", 2, manifest, []),
            ("passed", 0, frozen, predictions), ("failed", 1, frozen, early)]:
            with self.subTest(status=expected_status), tempfile.TemporaryDirectory() as directory:
                results = os.path.join(directory, "runs")
                os.mkdir(results)
                for name, value in (("manifest", m), ("labels", labels), ("predictions", p)):
                    with open(os.path.join(directory, name + ".json"), "w") as stream:
                        json.dump(value, stream)
                output = os.path.join(directory, "report.md")
                completed = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "report_cli.py"),
                    "--results", results, "--manifest", os.path.join(directory, "manifest.json"),
                    "--labels", os.path.join(directory, "labels.json"), "--predictions",
                    os.path.join(directory, "predictions.json"), "--output", output], capture_output=True, text=True)
                self.assertEqual(completed.returncode, expected_code, completed.stderr)
                with open(os.path.join(directory, "report.json")) as stream:
                    self.assertEqual(json.load(stream)["accuracy"]["status"], expected_status)

    def test_cli_rejects_malformed_nested_run_records_as_invalid_input(self):
        bad_runs = [
            {"attempts": [None]}, {"attempts": ["bad"]}, {"attempts": [[]]},
            {"attempts": [1]}, {"attempts": None}, {"attempts": {}},
            {"attempts": "bad"}, {"attempts": 1},
            {"attempts": [{"success": "false"}]}, {"attempts": [{}]},
            {"input_bytes": -1}, {"input_bytes": "100"}, {"input_bytes": True},
            {"measurement_complete": "true"},
        ]
        for run in bad_runs:
            with self.subTest(run=run), tempfile.TemporaryDirectory() as directory:
                results = os.path.join(directory, "runs")
                os.mkdir(results)
                with open(os.path.join(results, "run.json"), "w") as stream:
                    json.dump(run, stream)
                manifest, labels, predictions = inputs()
                for name, value in (("manifest", manifest), ("labels", labels), ("predictions", predictions)):
                    with open(os.path.join(directory, name + ".json"), "w") as stream:
                        json.dump(value, stream)
                output = os.path.join(directory, "report.md")
                completed = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "report_cli.py"),
                    "--results", results, "--manifest", os.path.join(directory, "manifest.json"),
                    "--labels", os.path.join(directory, "labels.json"), "--predictions",
                    os.path.join(directory, "predictions.json"), "--output", output], capture_output=True, text=True)
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertIn("Invalid report input:", completed.stderr)
                self.assertNotIn("Traceback", completed.stderr)
                self.assertFalse(os.path.exists(output))


if __name__ == "__main__":
    unittest.main()
