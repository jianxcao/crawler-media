import copy
import unittest

from confidence import cluster_bootstrap, zero_failure_upper_bound
from evaluate import evaluate_corpus, score_case

CASE = {"case_id": "m1-e1-intro", "media_group": "m1", "split": "holdout",
        "source_hash": "sha256:fixture", "duration_ms": 180000, "kind": "intro"}
LABEL = {"case_id": CASE["case_id"], "source_hash": CASE["source_hash"],
         "state": "present", "safe_intervals_ms": [[30000, 90000]],
         "protected_intervals_ms": [[0, 30000], [90000, 180000]],
         "start_range_ms": [30000, 30000], "end_range_ms": [90000, 90000],
         "reviewers": ["a", "b"], "adjudicated": True}
PREDICTION = {"case_id": CASE["case_id"], "source_hash": CASE["source_hash"],
              "state": "matched", "interval_ms": [30000, 90000],
              "reason": "verified", "run_id": "test"}


def inputs():
    return copy.deepcopy(({"corpus_version": "test", "evidence_kind": "synthetic", "cases": [CASE]}, [LABEL], [PREDICTION]))


class EvaluationTests(unittest.TestCase):
    def test_missing_truth_never_passes(self):
        result = evaluate_corpus({"corpus_version": "test", "evidence_kind": "synthetic", "cases": [CASE]}, [], [])
        self.assertNotEqual(result["status"], "passed")
        self.assertIn("missing_label", result["reasons"])

    def test_signed_boundary_error_and_protected_overlap(self):
        prediction = dict(PREDICTION, interval_ms=[20000, 90000])
        result = score_case(CASE, LABEL, prediction)
        self.assertEqual(result["start_error_ms"], -10000)
        self.assertEqual(result["protected_overlap_ms"], 10000)
        self.assertFalse(result["both_within_2s"])

    def test_detection_states_and_iou_threshold(self):
        for state, interval, label_state, expected in [
            ("matched", [30000, 60000], "present", (1, 0, 0)),
            ("matched", [30000, 59999], "present", (0, 1, 1)),
            ("uncertain", None, "present", (0, 0, 1)),
            ("no_match", None, "present", (0, 0, 1)),
            ("error", None, "present", (0, 0, 1)),
            ("matched", [30000, 90000], "absent", (0, 1, 0)),
        ]:
            with self.subTest(state=state, interval=interval, label_state=label_state):
                label = dict(LABEL, state=label_state)
                if label_state == "absent":
                    label.update(safe_intervals_ms=[], start_range_ms=None, end_range_ms=None)
                result = score_case(CASE, label, dict(PREDICTION, state=state, interval_ms=interval))
                self.assertEqual(tuple(result[key] for key in ("tp", "fp", "fn")), expected)

    def test_protected_union_and_best_safe_interval(self):
        label = dict(LABEL, protected_intervals_ms=[[0, 20000], [10000, 30000]])
        result = score_case(CASE, label, dict(PREDICTION, interval_ms=[10000, 90000]))
        self.assertEqual(result["protected_overlap_ms"], 20000)
        label = dict(LABEL, safe_intervals_ms=[[30000, 40000], [50000, 90000]],
                     protected_intervals_ms=[])
        self.assertEqual(score_case(CASE, label, dict(PREDICTION, interval_ms=[50000, 90000]))["iou"], 1)

    def test_pilot_and_all_abstain_are_insufficient(self):
        manifest, labels, predictions = inputs()
        self.assertEqual(evaluate_corpus(manifest, labels, predictions)["status"], "insufficient")
        predictions[0].update(state="uncertain", interval_ms=None)
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertIsNone(result["by_kind"]["intro"]["precision"])
        self.assertEqual(result["by_kind"]["intro"]["recall"], 0)
        self.assertEqual(result["counts"]["uncertain"], 1)

    def test_invalid_contracts_fail_closed(self):
        for mutation, reason in [
            (lambda m, l, p: p.clear(), "missing_prediction"),
            (lambda m, l, p: p.append(copy.deepcopy(p[0])), "duplicate_prediction"),
            (lambda m, l, p: l.append(copy.deepcopy(l[0])), "duplicate_label"),
            (lambda m, l, p: p[0].update(source_hash="wrong"), "wrong_source"),
            (lambda m, l, p: p[0].update(kind="outro"), "invalid_prediction"),
            (lambda m, l, p: p[0].update(interval_ms=[-1, 90000]), "invalid_prediction"),
            (lambda m, l, p: l[0].update(protected_intervals_ms=[[20000, 40000]]), "overlapping_truth"),
            (lambda m, l, p: m["cases"].append(dict(CASE, case_id="missing-episode")), "missing_prediction"),
            (lambda m, l, p: p[0].update(state="unknown"), "invalid_prediction"),
            (lambda m, l, p: p[0].update(interval_ms=[True, 90000]), "invalid_prediction"),
        ]:
            with self.subTest(reason=reason):
                manifest, labels, predictions = inputs()
                mutation(manifest, labels, predictions)
                result = evaluate_corpus(manifest, labels, predictions)
                self.assertEqual(result["status"], "invalid")
                self.assertIn(reason, result["reasons"])

    def test_ambiguous_is_listed_and_cannot_supply_precision(self):
        manifest, labels, predictions = inputs()
        labels[0].update(state="ambiguous", safe_intervals_ms=[], start_range_ms=None,
                         end_range_ms=None, adjudicated=False)
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["counts"]["ambiguous"], 1)
        self.assertEqual(result["ambiguous_cases"][0]["case_id"], CASE["case_id"])
        self.assertEqual(result["by_kind"]["intro"]["precision"], 0)
        self.assertIsNone(result["by_kind"]["intro"]["recall"])

    def test_absent_truth_has_null_recall(self):
        manifest, labels, predictions = inputs()
        labels[0].update(state="absent", safe_intervals_ms=[], start_range_ms=None, end_range_ms=None)
        self.assertIsNone(evaluate_corpus(manifest, labels, predictions)["by_kind"]["intro"]["recall"])

    def test_frozen_configurable_policy_and_holdout_gate(self):
        manifest, labels, predictions = inputs()
        manifest["evidence_kind"] = "human_reviewed"
        manifest["acceptance_policy"] = {
            "frozen": True, "min_sources": 1, "min_media_groups": 1,
            "min_holdout_sources": 1, "min_holdout_media_groups": 1,
            "required_kinds": ["intro"], "bootstrap_samples": 100, "bootstrap_seed": 42}
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["confidence"]["groups"], 1)
        predictions[0]["interval_ms"] = [20000, 90000]
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["status"], "failed")
        self.assertIn("severe_protected_overlap", result["reasons"])
        self.assertEqual(result["by_kind"]["intro"]["start_error_ms"]["p95"], -10000)
        self.assertEqual(result["by_kind"]["intro"]["start_absolute_error_ms"]["p95"], 10000)

    def test_evidence_provenance_is_required_and_synthetic_cannot_pass(self):
        manifest, labels, predictions = inputs()
        manifest["acceptance_policy"] = {
            "frozen": True, "min_sources": 1, "min_media_groups": 1,
            "min_holdout_sources": 1, "min_holdout_media_groups": 1,
            "required_kinds": ["intro"], "bootstrap_samples": 100}
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["status"], "insufficient")
        self.assertIn("synthetic_evidence", result["reasons"])
        del manifest["evidence_kind"]
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertNotEqual(result["status"], "passed")
        self.assertIn("missing_evidence_kind", result["reasons"])

    def test_population_counts_sources_once_and_requires_each_kind(self):
        manifest, labels, predictions = inputs()
        manifest["evidence_kind"] = "human_reviewed"
        manifest["acceptance_policy"] = {
            "frozen": True, "min_sources": 2, "min_media_groups": 1,
            "min_holdout_sources": 2, "min_holdout_media_groups": 1,
            "bootstrap_samples": 100}
        manifest["cases"].append(dict(CASE, case_id="m1-e1-outro", kind="outro"))
        labels.append(dict(LABEL, case_id="m1-e1-outro"))
        predictions.append(dict(PREDICTION, case_id="m1-e1-outro"))
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["status"], "insufficient")
        self.assertIn("insufficient_holdout_sources", result["reasons"])
        self.assertEqual(result["population"]["sources"], 1)
        self.assertEqual(result["population"]["cases"], 2)
        manifest["acceptance_policy"].update(min_sources=1, min_holdout_sources=1)
        predictions.pop()
        labels.pop()
        manifest["cases"].pop()
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertIn("missing_outro_cases", result["reasons"])

    def test_missing_label_only_is_insufficient(self):
        manifest, _, predictions = inputs()
        result = evaluate_corpus(manifest, [], predictions)
        self.assertEqual(result["status"], "insufficient")
        self.assertEqual(result["reasons"], ["missing_label"])

    def test_malformed_schema_never_raises_or_passes(self):
        for mutation in [lambda m, l, p: m.update(cases=None),
                         lambda m, l, p: l[0].update(reviewers=[["a"], "b"]),
                         lambda m, l, p: l[0].update(start_range_ms=[90000, 30000]),
                         lambda m, l, p: m.update(acceptance_policy={"required_kinds": [["intro"]]}),
                         lambda m, l, p: p[0].update(state="uncertain"),
                         lambda m, l, p: l[0].update(adjudicated=False)]:
            manifest, labels, predictions = inputs()
            mutation(manifest, labels, predictions)
            self.assertEqual(evaluate_corpus(manifest, labels, predictions)["status"], "invalid")

    def test_split_leakage_and_duplicate_source_kind_are_invalid(self):
        for extra, reason in [(dict(CASE, case_id="another", source_hash="other", split="development"), "media_split_leakage"),
                              (dict(CASE, case_id="another"), "duplicate_source_kind")]:
            manifest, labels, predictions = inputs()
            manifest["cases"].append(extra)
            labels.append(dict(LABEL, case_id="another", source_hash=extra["source_hash"]))
            predictions.append(dict(PREDICTION, case_id="another", source_hash=extra["source_hash"]))
            result = evaluate_corpus(manifest, labels, predictions)
            self.assertEqual(result["status"], "invalid")
            self.assertIn(reason, result["reasons"])

    def test_micro_macro_and_group_failure_denominators(self):
        manifest, labels, predictions = inputs()
        manifest["acceptance_policy"] = {"bootstrap_samples": 100}
        for number in range(2, 5):
            key = "case-{}".format(number)
            source = "sha256:{}".format(number)
            group = "m1" if number < 4 else "m2"
            manifest["cases"].append(dict(CASE, case_id=key, source_hash=source, media_group=group))
            labels.append(dict(LABEL, case_id=key, source_hash=source))
            predictions.append(dict(PREDICTION, case_id=key, source_hash=source))
        predictions[-1].update(interval_ms=[0, 10000])
        result = evaluate_corpus(manifest, labels, predictions)
        self.assertEqual(result["micro"]["precision"], 0.75)
        self.assertEqual(result["macro"]["precision"], 0.5)
        self.assertEqual(result["macro_by_kind"]["intro"]["precision"], 0.5)
        self.assertEqual(result["confidence"]["groups_with_severe_failure"], 1)
        self.assertIsNone(result["confidence"]["zero_severe_failure_group_upper_95"])

    def test_formal_abstention_and_pipeline_errors_fail(self):
        manifest, labels, predictions = inputs()
        manifest.update(evidence_kind="human_reviewed", acceptance_policy={
            "frozen": True, "min_sources": 1, "min_media_groups": 1,
            "min_holdout_sources": 1, "min_holdout_media_groups": 1,
            "required_kinds": ["intro"], "bootstrap_samples": 100})
        for state in ("uncertain", "error", "no_match"):
            predictions[0].update(state=state, interval_ms=None)
            result = evaluate_corpus(manifest, labels, predictions)
            self.assertEqual(result["status"], "failed")
            self.assertIn("intro_precision", result["reasons"])
            self.assertIn("intro_recall", result["reasons"])
            if state == "error":
                self.assertIn("holdout_pipeline_error", result["reasons"])

    def test_group_zero_failure_bound_and_cluster_bootstrap(self):
        self.assertAlmostEqual(zero_failure_upper_bound(30), 0.09503385, places=7)
        self.assertGreater(zero_failure_upper_bound(30), 0.01)
        self.assertLess(zero_failure_upper_bound(300), 0.01)
        with self.assertRaises(ValueError):
            zero_failure_upper_bound(0)
        # Repeated episodes in one group must be resampled together.
        rows = [{"media_group": group, "value": value} for group, value in
                [("a", 1), ("a", 1), ("b", 0)]]
        result = cluster_bootstrap(rows, lambda sample: sum(r["value"] for r in sample) / len(sample),
                                   samples=100, seed=42)
        self.assertEqual(result, cluster_bootstrap(rows, lambda sample: sum(r["value"] for r in sample) / len(sample),
                                                 samples=100, seed=42))
        self.assertEqual(result["groups"], 2)
        self.assertEqual(result["cases"], 3)
        self.assertEqual(result["lower"], 0)
        self.assertEqual(result["upper"], 1)


if __name__ == "__main__":
    unittest.main()
