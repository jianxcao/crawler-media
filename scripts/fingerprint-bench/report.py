import os
import json
from typing import List, Dict, Any, Optional

def build_report(runs: List[Dict[str, Any]]) -> Dict[str, Any]:
    """
    Builds a structured benchmark report from raw run dictionaries.
    """
    if not runs:
        return {
            "runs_count": 0,
            "input_bytes_saving_percent": None,
            "measurement_complete": False,
            "failed_attempts": 0,
            "max_boundary_error_ms": 0,
            "accuracy_gate_passed": False,
            "summary": "No runs provided",
        }

    total_attempts = 0
    failed_attempts = 0
    measurement_complete = True
    max_boundary_error_ms = 0
    full_window_bytes = 0
    adaptive_bytes = 0
    has_full_window_bytes = False
    has_adaptive_bytes = False

    for run in runs:
        attempts = run.get("attempts", [])
        total_attempts += len(attempts)
        for att in attempts:
            if not att.get("success", False):
                failed_attempts += 1
        
        if not run.get("measurement_complete", False):
            measurement_complete = False

        errors = run.get("accuracy_errors", [])
        for err in errors:
            err_ms = err.get("boundary_error_ms", 0)
            if err_ms > max_boundary_error_ms:
                max_boundary_error_ms = err_ms

        mode = run.get("sampling_mode")
        in_bytes = run.get("input_bytes")
        if mode == "full_window" and in_bytes is not None:
            full_window_bytes += in_bytes
            has_full_window_bytes = True
        elif mode == "adaptive" and in_bytes is not None:
            adaptive_bytes += in_bytes
            has_adaptive_bytes = True

    saving_percent: Optional[float] = None
    if measurement_complete and has_full_window_bytes and has_adaptive_bytes and full_window_bytes > 0:
        saving_percent = round((1.0 - (adaptive_bytes / full_window_bytes)) * 100.0, 2)

    accuracy_gate_passed = max_boundary_error_ms <= 2000

    return {
        "runs_count": len(runs),
        "input_bytes_saving_percent": saving_percent,
        "measurement_complete": measurement_complete,
        "failed_attempts": failed_attempts,
        "max_boundary_error_ms": max_boundary_error_ms,
        "accuracy_gate_passed": accuracy_gate_passed,
    }

def format_markdown_report(report_data: Dict[str, Any]) -> str:
    md = [
        "# Adaptive Fingerprint Benchmark Report",
        "",
        f"- **Runs count**: {report_data.get('runs_count')}",
        f"- **Measurement Complete**: {report_data.get('measurement_complete')}",
        f"- **Input Bytes Saving Percent**: {report_data.get('input_bytes_saving_percent')}%",
        f"- **Failed Attempts**: {report_data.get('failed_attempts')}",
        f"- **Max Boundary Error (ms)**: {report_data.get('max_boundary_error_ms')}",
        f"- **Accuracy Gate Passed**: {report_data.get('accuracy_gate_passed')}",
        ""
    ]
    return "\n".join(md)
