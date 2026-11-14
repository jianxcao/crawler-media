"""Evaluate independently labelled cases; missing evidence can never pass.

Only a frozen, explicitly supplied acceptance policy permits formal acceptance.
The default population targets describe the planned 600-source/60-Media corpus
and 300-source/30-Media holdout, not evidence that this corpus already exists.
"""
from confidence import cluster_bootstrap, zero_failure_upper_bound
from interval_metrics import distribution, iou, merge_intervals, overlap_ms, signed_distance

DEFAULT_POLICY = {
    "frozen": False, "min_sources": 600, "min_media_groups": 60,
    "min_holdout_sources": 300, "min_holdout_media_groups": 30,
    "required_kinds": ["intro", "outro"], "min_clear_fraction": 0.95,
    "min_precision": 0.99, "min_recall": 0.85, "min_boundary_2s": 0.95,
    "bootstrap_samples": 2000, "bootstrap_seed": 20261103,
}
CASE_FIELDS = {"case_id", "media_group", "split", "source_hash", "duration_ms", "kind"}
LABEL_FIELDS = {"case_id", "source_hash", "state", "safe_intervals_ms", "protected_intervals_ms",
                "start_range_ms", "end_range_ms", "reviewers", "adjudicated"}
PREDICTION_FIELDS = {"case_id", "source_hash", "state", "interval_ms", "reason", "run_id"}


def _text(value):
    return isinstance(value, str) and bool(value.strip())


def _interval(value, duration, allow_equal=False):
    return (isinstance(value, list) and len(value) == 2
            and all(type(x) is int for x in value)
            and 0 <= value[0] <= value[1] <= duration
            and (allow_equal or value[0] < value[1]))


def _index(rows, kind, fields, reasons):
    indexed = {}
    if not isinstance(rows, list):
        reasons.add("invalid_" + kind)
        return indexed
    for row in rows:
        if not isinstance(row, dict) or set(row) != fields or not _text(row.get("case_id")):
            reasons.add("invalid_" + kind)
            continue
        key = row["case_id"]
        if key in indexed:
            reasons.add("duplicate_" + kind)
        indexed[key] = row
    return indexed


def _validate_case(case, reasons):
    if (not all(_text(case[key]) for key in ("case_id", "media_group", "source_hash"))
            or case["split"] not in ("development", "validation", "holdout")
            or case["kind"] not in ("intro", "outro")
            or type(case["duration_ms"]) is not int or case["duration_ms"] <= 0):
        reasons.add("invalid_case")


def _validate_label(case, label, reasons):
    duration = case["duration_ms"]
    state = label["state"]
    safe, protected = label["safe_intervals_ms"], label["protected_intervals_ms"]
    if (state not in ("present", "absent", "ambiguous")
            or not isinstance(safe, list) or not isinstance(protected, list)
            or not all(_interval(x, duration) for x in safe + protected)
            or not isinstance(label["reviewers"], list)
            or not all(_text(x) for x in label["reviewers"])
            or len(set(label["reviewers"])) < 2 or type(label["adjudicated"]) is not bool):
        reasons.add("invalid_label")
        return
    if state == "present":
        if (not safe or not label["adjudicated"]
                or not _interval(label["start_range_ms"], duration, True)
                or not _interval(label["end_range_ms"], duration, True)
                or label["start_range_ms"][1] >= label["end_range_ms"][0]):
            reasons.add("invalid_label")
    elif (safe or label["start_range_ms"] is not None or label["end_range_ms"] is not None
          or (state == "absent" and not label["adjudicated"])):
        reasons.add("invalid_label")
    if any(overlap_ms(left, right) for left in safe for right in protected):
        reasons.add("overlapping_truth")


def _validate_prediction(case, prediction, reasons):
    if (prediction["state"] not in ("matched", "no_match", "uncertain", "error")
            or not _text(prediction["reason"]) or not _text(prediction["run_id"])):
        reasons.add("invalid_prediction")
    if prediction["state"] == "matched":
        if not _interval(prediction["interval_ms"], case["duration_ms"]):
            reasons.add("invalid_prediction")
    elif prediction["interval_ms"] is not None:
        reasons.add("invalid_prediction")


def _policy(manifest, reasons):
    supplied = manifest.get("acceptance_policy", {})
    if not isinstance(supplied, dict) or set(supplied) - set(DEFAULT_POLICY):
        reasons.add("invalid_policy")
        return dict(DEFAULT_POLICY)
    policy = {**DEFAULT_POLICY, **supplied}
    if (type(policy["frozen"]) is not bool
            or any(type(policy[k]) is not int or policy[k] <= 0 for k in
                   ("min_sources", "min_media_groups", "min_holdout_sources",
                    "min_holdout_media_groups", "bootstrap_samples"))
            or type(policy["bootstrap_seed"]) is not int
            or any(type(policy[k]) not in (int, float) or not 0 < policy[k] <= 1 for k in
                   ("min_clear_fraction", "min_precision", "min_recall", "min_boundary_2s"))
            or not isinstance(policy["required_kinds"], list) or not policy["required_kinds"]
            or any(k not in ("intro", "outro") for k in policy["required_kinds"])
            or len(set(policy["required_kinds"])) != len(policy["required_kinds"])):
        reasons.add("invalid_policy")
    return policy


def _validate(manifest, labels, predictions):
    reasons = set()
    if (not isinstance(manifest, dict) or set(manifest) - {"corpus_version", "cases", "acceptance_policy", "evidence_kind"}
            or not _text(manifest.get("corpus_version")) or "cases" not in manifest):
        return {}, {}, {}, dict(DEFAULT_POLICY), {"invalid_manifest"}
    if "evidence_kind" not in manifest:
        reasons.add("missing_evidence_kind")
    elif manifest["evidence_kind"] not in ("synthetic", "human_reviewed", "public_annotation"):
        reasons.add("invalid_evidence_kind")
    cases = _index(manifest["cases"], "case", CASE_FIELDS, reasons)
    truth = _index(labels, "label", LABEL_FIELDS, reasons)
    predicted = _index(predictions, "prediction", PREDICTION_FIELDS, reasons)
    policy = _policy(manifest, reasons)
    for case in cases.values():
        _validate_case(case, reasons)
    if "invalid_case" in reasons:
        return cases, truth, predicted, policy, reasons
    group_splits, sources = {}, {}
    for key, case in cases.items():
        group = case["media_group"]
        if group in group_splits and group_splits[group] != case["split"]:
            reasons.add("media_split_leakage")
        group_splits[group] = case["split"]
        source = case["source_hash"]
        identity = (group, case["split"], case["duration_ms"])
        if source in sources and sources[source] != identity:
            reasons.add("inconsistent_source")
        sources[source] = identity
        for rows, name, validator in ((truth, "label", _validate_label),
                                      (predicted, "prediction", _validate_prediction)):
            if key not in rows:
                reasons.add("missing_" + name)
            elif rows[key]["source_hash"] != source:
                reasons.add("wrong_source")
            else:
                validator(case, rows[key], reasons)
    for rows, name in ((truth, "label"), (predicted, "prediction")):
        if set(rows) - set(cases):
            reasons.add("unexpected_" + name)
    identities = [(c["source_hash"], c["kind"]) for c in cases.values()]
    if len(identities) != len(set(identities)):
        reasons.add("duplicate_source_kind")
    return cases, truth, predicted, policy, reasons


def score_case(case, label, prediction):
    """Score one validated case; callers using this seam also get validation."""
    reasons = set()
    if not isinstance(case, dict) or set(case) != CASE_FIELDS:
        raise ValueError("invalid_case")
    _validate_case(case, reasons)
    if reasons:
        raise ValueError(",".join(sorted(reasons)))
    for row, fields, validator in ((label, LABEL_FIELDS, _validate_label),
                                   (prediction, PREDICTION_FIELDS, _validate_prediction)):
        if not isinstance(row, dict) or set(row) != fields:
            raise ValueError("invalid_row")
        if row["case_id"] != case["case_id"] or row["source_hash"] != case["source_hash"]:
            raise ValueError("identity_mismatch")
        validator(case, row, reasons)
    if reasons:
        raise ValueError(",".join(sorted(reasons)))
    matched = prediction["state"] == "matched"
    present = label["state"] == "present"
    interval = prediction["interval_ms"]
    best_iou = max((iou(interval, safe) for safe in label["safe_intervals_ms"]), default=0) if matched else 0
    tp = int(matched and present and best_iou >= 0.5)
    start = signed_distance(interval[0], label["start_range_ms"]) if matched and present else None
    end = signed_distance(interval[1], label["end_range_ms"]) if matched and present else None
    protected = sum(overlap_ms(interval, x) for x in merge_intervals(label["protected_intervals_ms"])) if matched else 0
    return {**case, "label_state": label["state"], "prediction_state": prediction["state"],
            "tp": tp, "fp": int(matched and label["state"] != "ambiguous" and not tp),
            "fn": int(present and not tp), "matched": int(matched), "iou": best_iou,
            "start_error_ms": start, "end_error_ms": end, "protected_overlap_ms": protected,
            "both_within_2s": start is not None and max(abs(start), abs(end)) <= 2000,
            "both_within_1s": start is not None and max(abs(start), abs(end)) <= 1000}


def _ratio(numerator, denominator):
    return numerator / denominator if denominator else None


def _metrics(rows):
    tp = sum(r["tp"] for r in rows)
    matched = sum(r["matched"] for r in rows)
    positive = sum(r["label_state"] == "present" for r in rows)
    result = {"cases": len(rows), "media_groups": len({r["media_group"] for r in rows}),
              "tp": tp, "fp": sum(r["fp"] for r in rows), "fn": sum(r["fn"] for r in rows),
              "predicted_positive": matched, "labelled_positive": positive,
              "precision": _ratio(tp, matched), "recall": _ratio(tp, positive),
              "clear_fraction": _ratio(sum(r["label_state"] != "ambiguous" for r in rows), len(rows)),
              "boundary_2s": _ratio(sum(r["tp"] and r["both_within_2s"] for r in rows), tp),
              "boundary_1s": _ratio(sum(r["tp"] and r["both_within_1s"] for r in rows), tp),
              "severe_protected_overlap": sum(r["protected_overlap_ms"] > 2000 for r in rows),
              "any_protected_overlap": sum(r["protected_overlap_ms"] > 0 for r in rows),
              "error": sum(r["prediction_state"] == "error" for r in rows)}
    for name in ("start", "end"):
        values = [r[name + "_error_ms"] for r in rows if r["tp"]]
        result[name + "_error_ms"] = distribution(values)
        result[name + "_absolute_error_ms"] = distribution([abs(v) for v in values])
    return result


def _confidence(rows, policy):
    groups = {r["media_group"] for r in rows}
    failed_groups = {r["media_group"] for r in rows if r["protected_overlap_ms"] > 2000}
    zero_failure = bool(groups) and not failed_groups
    intervals = {}
    for metric in ("precision", "recall", "boundary_2s", "boundary_1s"):
        intervals[metric] = cluster_bootstrap(rows, lambda sample: _metrics(sample)[metric],
                                             samples=policy["bootstrap_samples"], seed=policy["bootstrap_seed"])
    return {"groups": len(groups), "cases": len(rows), "intervals": intervals,
            "groups_with_severe_failure": len(failed_groups),
            "zero_severe_failure_group_upper_95": zero_failure_upper_bound(len(groups)) if zero_failure else None,
            "assumption": "Representative independent Media groups; episodes within a Media are dependent."}


def _population(cases, policy):
    reasons = set()
    if not policy["frozen"]:
        reasons.add("policy_not_frozen")
    for prefix, rows in (("", cases), ("holdout_", [c for c in cases if c["split"] == "holdout"])):
        if len({c["media_group"] for c in rows}) < policy["min_" + prefix + "media_groups"]:
            reasons.add("insufficient_" + prefix + "media_groups")
        if len({c["source_hash"] for c in rows}) < policy["min_" + prefix + "sources"]:
            reasons.add("insufficient_" + prefix + "sources")
        for kind in policy["required_kinds"]:
            if any(not any(c["kind"] == kind and c["source_hash"] == source for c in rows)
                   for source in {c["source_hash"] for c in rows}):
                reasons.add("missing_" + prefix + kind + "_cases")
    return reasons


def _population_counts(rows):
    return {"cases": len(rows), "sources": len({r["source_hash"] for r in rows}),
            "media_groups": len({r["media_group"] for r in rows})}


def _macro(rows):
    groups = [_metrics([r for r in rows if r["media_group"] == group])
              for group in sorted({r["media_group"] for r in rows})]
    return {key: _ratio(sum(m[key] for m in groups if m[key] is not None),
                        sum(m[key] is not None for m in groups))
            for key in ("precision", "recall", "boundary_2s", "boundary_1s")}


def evaluate_corpus(manifest, labels, predictions):
    cases, truth, predicted, policy, reasons = _validate(manifest, labels, predictions)
    result = {"status": "invalid", "reasons": sorted(reasons), "accuracy_complete": False,
              "counts": {"expected": len(cases), "labels": len(truth), "predictions": len(predicted)},
              "by_kind": {}, "by_media": {}, "acceptance_policy": policy}
    if reasons:
        # Missing truth is insufficient; other contract errors retain invalid priority.
        if reasons <= {"missing_label"}:
            result["status"] = "insufficient"
        return result
    rows = [score_case(c, truth[key], predicted[key]) for key, c in cases.items()]
    holdout = [r for r in rows if r["split"] == "holdout"]
    by_media = {group: _metrics([r for r in rows if r["media_group"] == group])
                for group in sorted({r["media_group"] for r in rows})}
    result.update(accuracy_complete=True, case_scores=rows, micro=_metrics(rows), macro=_macro(rows),
                  macro_by_kind={k: _macro([r for r in rows if r["kind"] == k]) for k in ("intro", "outro")},
                  by_kind={k: _metrics([r for r in rows if r["kind"] == k]) for k in ("intro", "outro")},
                  by_media=by_media, confidence=_confidence(holdout, policy),
                  ambiguous_cases=[{"case": cases[r["case_id"]], "label": truth[r["case_id"]],
                                    "prediction": predicted[r["case_id"]], "case_id": r["case_id"]}
                                   for r in rows if r["label_state"] == "ambiguous"])
    result["population"] = {**_population_counts(rows), "holdout": _population_counts(holdout)}
    result["counts"].update({state: sum(r["prediction_state"] == state for r in rows)
                             for state in ("matched", "uncertain", "error", "no_match")})
    result["counts"].update(ambiguous=sum(r["label_state"] == "ambiguous" for r in rows),
                            processing_coverage=_ratio(len(rows), len(cases)))
    reasons = _population(list(cases.values()), policy)
    if manifest["evidence_kind"] == "synthetic":
        reasons.add("synthetic_evidence")
    if not cases:
        reasons.add("empty_corpus")
    acceptance = {k: _metrics([r for r in holdout if r["kind"] == k]) for k in policy["required_kinds"]}
    result["holdout_by_kind"] = acceptance
    result["confidence_by_kind"] = {k: _confidence([r for r in holdout if r["kind"] == k], policy)
                                    for k in policy["required_kinds"]}
    if reasons:
        result.update(status="insufficient", reasons=sorted(reasons))
        return result
    failures = set()
    if any(r["prediction_state"] == "error" for r in holdout):
        failures.add("holdout_pipeline_error")
    if any(r["protected_overlap_ms"] > 2000 for r in holdout):
        failures.add("severe_protected_overlap")
    for kind, measured in acceptance.items():
        for metric, minimum in (("clear_fraction", "min_clear_fraction"), ("precision", "min_precision"),
                                ("recall", "min_recall"), ("boundary_2s", "min_boundary_2s")):
            if measured[metric] is None or measured[metric] < policy[minimum]:
                failures.add(kind + "_" + metric)
    result.update(status="failed" if failures else "passed", reasons=sorted(failures))
    return result
