# 2026-10-08 Fingerprint Sampling Protocol and Benchmark

## Protocol Overview
The adaptive fingerprint sampling protocol evaluates the performance, accuracy, and network/disk byte reduction achieved by adaptive window probing vs. full-window extraction across entire seasons.

### Invariants & Metrics
1. **Byte Measurement**:
   - `input_bytes` is populated only when exact origin/AVIO body bytes are reliably measured.
   - If bytes cannot be accurately verified (e.g. unknown network pipe), `input_bytes` is `None` / `null` and `measurement_complete = false`.
2. **Accuracy Gate**:
   - Boundary deviation from ground truth must be $\le 2000$ ms ($2$ seconds).
   - False positive rate must be $0$.
3. **Execution Commands**:
   - Python report evaluation:
     ```bash
     python3 scripts/fingerprint-bench/report_cli.py --results <results_dir> --output <report.md>
     ```
   - Rust unit tests:
     ```bash
     cargo test -p api --test adaptive_benchmark_report
     python3 -m unittest discover -s scripts/fingerprint-bench -p 'test_*.py'
     ```
