"""Paired lifecycle wallclocks and independently complete input measurements."""
import math
from collections import defaultdict


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def distribution(values):
    if not values:
        return None
    values = sorted(values)
    def percentile(p):
        position = (len(values) - 1) * p
        low = int(position)
        return values[low] + (values[min(low + 1, len(values) - 1)] - values[low]) * (position - low)
    return {"p50": percentile(.5), "p95": percentile(.95)}


def compare_paired_runs(runs):
    result = {"status": "invalid", "errors": [], "pairs": [], "scenarios": {},
              "wall_time_saving_percent": None, "input_bytes_saving_percent": None,
              "measurement_complete": False}
    grouped = defaultdict(list)
    if not isinstance(runs, list) or not runs:
        result["errors"].append("runs must be a nonempty list")
        return result
    for row in runs:
        if not isinstance(row, dict) or not isinstance(row.get("pair_id"), str) or not row["pair_id"]:
            result["errors"].append("missing pair_id")
            continue
        grouped[row["pair_id"]].append(row)
    for pair_id, rows in grouped.items():
        if len(rows) != 2 or {r.get("variant") for r in rows} != {"baseline", "candidate"}:
            result["errors"].append(f"{pair_id}: requires exactly one baseline and candidate")
            continue
        baseline = next(r for r in rows if r["variant"] == "baseline")
        candidate = next(r for r in rows if r["variant"] == "candidate")
        valid = True
        for key in ("scenario", "corpus_version", "config_hash", "source_hashes"):
            if not baseline.get(key) or baseline[key] != candidate.get(key):
                result["errors"].append(f"{pair_id}: mismatched/missing {key}")
                valid = False
        for row in rows:
            elapsed = row.get("total_elapsed_ms")
            if row.get("wallclock_complete") is not True or row.get("terminal_status") != "completed" or not number(elapsed) or elapsed < 0:
                result["errors"].append(f"{pair_id}: incomplete successful lifecycle")
                valid = False
        if not valid or baseline.get("total_elapsed_ms", 0) <= 0:
            if valid:
                result["errors"].append(f"{pair_id}: baseline wallclock must be positive")
            continue
        known_bytes = all(r.get("measurement_complete") is True and number(r.get("input_bytes")) and r["input_bytes"] >= 0 for r in rows)
        result["pairs"].append({"pair_id": pair_id, "scenario": baseline["scenario"],
            "wall_time_saving_percent": 100 * (1 - candidate["total_elapsed_ms"] / baseline["total_elapsed_ms"]),
            "input_bytes_saving_percent": 100 * (1 - candidate["input_bytes"] / baseline["input_bytes"]) if known_bytes and baseline["input_bytes"] > 0 else None,
            "measurement_complete": known_bytes})
    if result["errors"]:
        return result
    scenarios = defaultdict(list)
    for pair in result["pairs"]:
        scenarios[pair["scenario"]].append(pair)
    for scenario, pairs in scenarios.items():
        byte_values = [p["input_bytes_saving_percent"] for p in pairs]
        result["scenarios"][scenario] = {
            "status": "measured" if len(pairs) >= 5 else "pilot", "pair_count": len(pairs),
            "wall_time_saving_percent": distribution([p["wall_time_saving_percent"] for p in pairs]),
            "input_bytes_saving_percent": distribution(byte_values) if all(v is not None for v in byte_values) else None}
    result["status"] = "measured" if all(s["status"] == "measured" for s in result["scenarios"].values()) else "pilot"
    result["measurement_complete"] = all(p["measurement_complete"] for p in result["pairs"])
    if len(result["scenarios"]) == 1:
        scenario = next(iter(result["scenarios"].values()))
        result["wall_time_saving_percent"] = scenario["wall_time_saving_percent"]
        result["input_bytes_saving_percent"] = scenario["input_bytes_saving_percent"]
    return result
