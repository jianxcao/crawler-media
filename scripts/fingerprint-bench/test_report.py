import unittest
from report import build_report

class TestBenchmarkReport(unittest.TestCase):
    def test_unknown_network_bytes_do_not_produce_saving_percent(self):
        runs = [
            {
                "sampling_mode": "full_window",
                "input_bytes": None,
                "measurement_complete": False,
                "attempts": [{"success": True}],
                "accuracy_errors": []
            },
            {
                "sampling_mode": "adaptive",
                "input_bytes": 1000,
                "measurement_complete": True,
                "attempts": [{"success": True}],
                "accuracy_errors": []
            }
        ]
        rep = build_report(runs)
        self.assertIsNone(rep["input_bytes_saving_percent"])
        self.assertFalse(rep["measurement_complete"])

    def test_paired_report_includes_failures_retries_and_ground_truth_errors(self):
        runs = [
            {
                "sampling_mode": "full_window",
                "input_bytes": 10000,
                "measurement_complete": True,
                "attempts": [{"success": True}],
                "accuracy_errors": []
            },
            {
                "sampling_mode": "adaptive",
                "input_bytes": 5000,
                "measurement_complete": True,
                "attempts": [{"success": False}, {"success": False}, {"success": True}],
                "accuracy_errors": [{"boundary_error_ms": 3000}]
            }
        ]
        rep = build_report(runs)
        self.assertEqual(rep["input_bytes_saving_percent"], 50.0)
        self.assertTrue(rep["measurement_complete"])
        self.assertEqual(rep["failed_attempts"], 2)
        self.assertEqual(rep["max_boundary_error_ms"], 3000)
        self.assertFalse(rep["accuracy_gate_passed"])

if __name__ == "__main__":
    unittest.main()
