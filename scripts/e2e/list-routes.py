#!/usr/bin/env python3
"""Extract Axum route methods from source, including chained methods and media routes."""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
METHOD = re.compile(r'(?<![\w:])(get|post|put|patch|delete|head|options|trace)\s*\(')


def route_calls(source):
    # Preserve quoted strings (including URLs) while dropping Rust comments.
    source = re.sub(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/',
                    lambda match: match.group() if match.group().startswith('"') else ' ', source)
    for match in re.finditer(r'\.route\s*\(\s*"([^"\n]+)"\s*,', source):
        depth, quote, escaped = 1, None, False
        end = match.end()
        for end in range(match.end(), len(source)):
            char = source[end]
            if quote:
                if escaped:
                    escaped = False
                elif char == '\\':
                    escaped = True
                elif char == quote:
                    quote = None
            elif char == '"':
                quote = char
            elif char == '(':
                depth += 1
            elif char == ')':
                depth -= 1
                if not depth:
                    break
        expression = source[match.end():end].replace('axum::routing::', '')
        for method in sorted(set(METHOD.findall(expression))):
            yield method, match.group(1)


def extract_routes(root=ROOT):
    groups = [('self', (root / 'crates/api/src/http').rglob('*.rs')),
              ('jellyfin', (root / 'crates/media-server/src').glob('routes*.rs')),
              ('jellyfin', (root / 'crates/media-server/src/routes').rglob('*.rs'))]
    routes = set()
    for surface, paths in groups:
        for path in paths:
            if 'tests' in path.parts or path.stem == 'tests' or path.stem.endswith('_tests'):
                continue
            # Test-only routers aren't production endpoints.
            source = path.read_text().split('#[cfg(test)]')[0]
            for method, route in route_calls(source):
                routes.add((surface, route, method))
    return [{'method': method, 'path': route, 'surface': surface}
            for surface, route, method in sorted(routes)]


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    print(json.dumps(extract_routes(args.root), ensure_ascii=False, indent=2))
