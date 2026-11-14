"""Reports keep accuracy evidence separate from network instrumentation."""
from evaluate import evaluate_corpus
from paired import compare_paired_runs


def _validate_runs(runs):
    if not isinstance(runs, list):
        raise ValueError("run results must be a list")
    for index, run in enumerate(runs):
        if not isinstance(run, dict):
            raise ValueError("run {} must be a JSON object".format(index))
        attempts = run.get("attempts", [])
        if not isinstance(attempts, list):
            raise ValueError("run {} attempts must be a list".format(index))
        for attempt_index, attempt in enumerate(attempts):
            if not isinstance(attempt, dict) or type(attempt.get("success")) is not bool:
                raise ValueError("run {} attempt {} must be an object with boolean success".format(
                    index, attempt_index))
        input_bytes = run.get("input_bytes")
        if input_bytes is not None and (type(input_bytes) is not int or input_bytes < 0):
            raise ValueError("run {} input_bytes must be null or a nonnegative integer".format(index))
        if "measurement_complete" in run and type(run["measurement_complete"]) is not bool:
            raise ValueError("run {} measurement_complete must be boolean".format(index))


def build_report(runs, manifest, labels, predictions):
    _validate_runs(runs)
    accuracy = evaluate_corpus(manifest, labels, predictions)
    complete = bool(runs) and all(
        run.get("measurement_complete") is True and type(run.get("input_bytes")) is int
        and run["input_bytes"] >= 0 for run in runs)
    paired = compare_paired_runs(runs) if any("pair_id" in run for run in runs) else None
    return {
        "runs_count": len(runs),
        "input_bytes_saving_percent": None,
        "measurement_complete": complete,
        "performance_status": paired["status"] if paired else "pairing_pending",
        "paired": paired,
        "failed_attempts": sum(not attempt["success"]
                               for run in runs for attempt in run.get("attempts", [])),
        "accuracy_complete": accuracy["accuracy_complete"],
        "accuracy_gate_passed": accuracy["status"] == "passed",
        "accuracy": accuracy,
        "evidence_kind": manifest.get("evidence_kind") if isinstance(manifest, dict) else None,
    }


def format_markdown_report(report_data):
    accuracy = report_data["accuracy"]
    lines = ["# Adaptive Fingerprint Benchmark Report", "",
             "- Accuracy status: **{}**".format(accuracy["status"]),
             "- Evidence kind: {}".format(report_data["evidence_kind"]),
             "- Accuracy complete: {}".format(report_data["accuracy_complete"]),
             "- Reasons: {}".format(", ".join(accuracy["reasons"]) or "none"),
             "- Runs: {}".format(report_data["runs_count"]),
             "- Network measurement complete: {}".format(report_data["measurement_complete"]),
             "- Performance pairing: {}".format(report_data["performance_status"]), "",
             "Accuracy metrics describe this labelled corpus only; synthetic evidence cannot establish real-media acceptance.",
             "See the JSON sidecar for all denominators, Media groups, confidence intervals and ambiguous cases.", ""]
    if accuracy.get("by_kind"):
        lines += ["| Kind | Cases | TP | FP | FN | Precision | Recall | Both boundaries ±2s |",
                  "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
        for kind, values in accuracy["by_kind"].items():
            lines.append("| {} | {} | {} | {} | {} | {} | {} | {} |".format(
                kind, *(values[k] for k in ("cases", "tp", "fp", "fn", "precision", "recall", "boundary_2s"))))
    return "\n".join(lines) + "\n"
