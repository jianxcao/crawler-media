"""Build accuracy reports from independent manifest, labels and predictions."""
import argparse
import json
import os
import sys
from pathlib import Path

from report import build_report, format_markdown_report


def _load(path):
    with open(path, encoding="utf-8") as stream:
        return json.load(stream)


def main():
    parser = argparse.ArgumentParser(description="Generate adaptive fingerprint benchmark report")
    for name in ("results", "manifest", "labels", "predictions", "output"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    try:
        runs = [_load(os.path.join(args.results, name)) for name in sorted(os.listdir(args.results))
                if name.endswith(".json")]
        if any(not isinstance(run, dict) for run in runs):
            raise ValueError("run results must be JSON objects")
        report_data = build_report(runs, _load(args.manifest), _load(args.labels), _load(args.predictions))
        output = Path(args.output)
        json_output = output.with_suffix(".json")
        if output == json_output:
            raise ValueError("--output must name a Markdown file, not the JSON sidecar")
        output.write_text(format_markdown_report(report_data), encoding="utf-8")
        json_output.write_text(json.dumps(report_data, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    except (OSError, ValueError, TypeError) as error:
        print("Invalid report input: {}".format(error), file=sys.stderr)
        return 2
    print("Report generated at {}".format(args.output))
    return {"invalid": 2, "insufficient": 2, "failed": 1, "passed": 0}[report_data["accuracy"]["status"]]


if __name__ == "__main__":
    sys.exit(main())
