import unittest

from paired import compare_paired_runs


def run(pair="p1", variant="baseline", **updates):
    value = {
        "pair_id": pair, "variant": variant, "scenario": "cold",
        "corpus_version": "pilot-1", "config_hash": "c1",
        "source_hashes": ["s1"], "total_elapsed_ms": 1000,
        "input_bytes": None, "measurement_complete": False,
        "wallclock_complete": True, "terminal_status": "completed",
    }
    value.update(updates)
    return value


class PairedRunTests(unittest.TestCase):
    def test_missing_candidate_cannot_claim_a_speedup(self):
        result = compare_paired_runs([run()])
        self.assertEqual(result["status"], "invalid")
        self.assertIsNone(result["wall_time_saving_percent"])

    def test_wallclock_is_independent_of_input_byte_completeness(self):
        rows = [r for i in range(5) for r in (
            run(str(i)), run(str(i), "candidate", total_elapsed_ms=600))]
        result = compare_paired_runs(rows)
        self.assertEqual(result["status"], "measured")
        self.assertAlmostEqual(result["wall_time_saving_percent"]["p50"], 40)
        self.assertIsNone(result["input_bytes_saving_percent"])
        self.assertFalse(result["measurement_complete"])

    def test_positive_elapsed_cannot_prove_completed_lifecycle(self):
        for updates in ({"wallclock_complete": False},
                        {"terminal_status": "failed"},
                        {"terminal_status": None}):
            with self.subTest(updates=updates):
                result = compare_paired_runs([run(), run(variant="candidate", **updates)])
                self.assertEqual(result["status"], "invalid")
                self.assertIsNone(result["wall_time_saving_percent"])

    def test_identity_mismatches_and_duplicate_variants_are_invalid(self):
        for field, other in (("config_hash", "c2"), ("source_hashes", ["s2"]),
                             ("corpus_version", "other"), ("scenario", "warm"),
                             ("variant", "baseline")):
            with self.subTest(field=field):
                candidate = run(variant="candidate")
                candidate[field] = other
                result = compare_paired_runs([run(), candidate])
                self.assertEqual(result["status"], "invalid")

    def test_less_than_five_pairs_is_only_a_pilot_and_bytes_require_both_sides(self):
        result = compare_paired_runs([
            run(input_bytes=100, measurement_complete=True),
            run(variant="candidate", input_bytes=20, measurement_complete=True,
                total_elapsed_ms=500)])
        self.assertEqual(result["status"], "pilot")
        self.assertEqual(result["input_bytes_saving_percent"]["p50"], 80)
        self.assertEqual(len(result["pairs"]), 1)

    def test_scenarios_are_reported_separately(self):
        result = compare_paired_runs([
            run(), run(variant="candidate", total_elapsed_ms=500),
            run("p2", scenario="refresh"),
            run("p2", "candidate", scenario="refresh", total_elapsed_ms=2000)])
        self.assertIsNone(result["wall_time_saving_percent"])
        self.assertEqual(set(result["scenarios"]), {"cold", "refresh"})
        self.assertEqual(result["scenarios"]["cold"]["wall_time_saving_percent"]["p50"], 50)


if __name__ == "__main__":
    unittest.main()
