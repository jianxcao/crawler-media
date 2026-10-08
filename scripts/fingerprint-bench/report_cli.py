import argparse
import sys
import json
import os
from report import build_report, format_markdown_report

def main():
    parser = argparse.ArgumentParser(description="Generate adaptive fingerprint benchmark report")
    parser.add_argument("--results", required=True, help="Directory containing run JSON results")
    parser.add_argument("--output", required=True, help="Path to write markdown report")
    args = parser.parse_args()

    runs = []
    if os.path.exists(args.results):
        for fname in os.listdir(args.results):
            if fname.endswith(".json"):
                fpath = os.path.join(args.results, fname)
                with open(fpath, "r", encoding="utf-8") as f:
                    runs.append(json.load(f))

    report_data = build_report(runs)
    md = format_markdown_report(report_data)

    with open(args.output, "w", encoding="utf-8") as f:
        f.write(md)

    print(f"Report generated at {args.output}")

if __name__ == "__main__":
    main()
