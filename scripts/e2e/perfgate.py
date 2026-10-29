#!/usr/bin/env python3
"""Route-complete performance gate. See docs/testing/performance-slo.md."""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import math
import shlex
import os
from pathlib import Path
import random
import re
import subprocess
import sys
import time
import urllib.parse

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from perf_support import ApiError, Client, login

# Milliseconds: idle p95 and p99.
SLO = {'W': (50, 150), 'D': (100, 250), 'L': (150, 400), 'S': (200, 500),
       'A': (300, 800), 'U': (100, 250), 'U*': (500, 1000), 'P': (800, 1500)}


def uncovered_routes(routes, cases):
    covered = {(case['surface'], case['route']) for case in cases if case.get('route')}
    return [route for route in routes if route['method'] == 'get'
            and (route['surface'], route['path']) not in covered]


def percentile(samples, fraction):
    ordered = sorted(samples)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)] if ordered else None


def evaluate(samples, codes, class_, mode, baseline_p95=None):
    p95, p99 = percentile(samples, .95), percentile(samples, .99)
    target95, target99 = SLO[class_]
    if mode == 'busy':
        target95 *= 2
    elif mode == 'cold':
        target95 = target99 * 2
    regression = ((p95 / baseline_p95 - 1) * 100
                  if p95 is not None and baseline_p95 and baseline_p95 > 0 else None)
    regressed = regression is not None and regression > 20 + 1e-9
    errors = sum(count for code, count in codes.items() if not 200 <= int(code) < 300)
    passed = bool(samples) and p95 <= target95 and not errors and not regressed
    if mode == 'idle':
        passed = passed and p99 <= target99
    return {'n': len(samples), 'p50': percentile(samples, .50), 'p95': p95, 'p99': p99,
            'max': max(samples) if samples else None, 'codes': dict(codes),
            'target_p95': target95, 'target_p99': target99 if mode == 'idle' else None,
            'regression_percent': regression, 'regressed': regressed, 'passed': passed}


def failure_code(error):
    return error.status if isinstance(error, ApiError) else 0


def validate_baseline(report, mode, selected):
    if report.get('complete') is not True or report.get('mode') != mode:
        raise ValueError('baseline must be complete and use the same measurement mode')
    cases = report['cases']
    missing = [case['id'] for case in selected if case['id'] not in cases]
    if missing:
        raise ValueError('baseline missing selected cases: ' + ', '.join(missing))
    return cases


def response_matches(case, payload):
    if 'expected_items' not in case:
        return True
    response = json.loads(payload)
    return len(response['Items']) == case['expected_items']


def prepare_heartbeat(client, fields):
    # A fresh binding per sample prevents session TTL and stale sequences turning
    # measured writes into HTTP 200 no-ops. This runs before the cold restart.
    target = fields['write_movie']
    session = client.call('/api/v1/playback/sessions', 'POST',
                          {'media_item_id': target, 'device_id': 'perfgate', 'start_ms': 0})
    fields.update(session_movie=target, play_session=session['play_session_id'],
                  session_file=session['file_id'], sequence=1)
    client.call('/api/v1/playback/progress', 'POST', {
        'media_item_id': target, 'device_id': 'perfgate',
        'play_session_id': fields['play_session'], 'file_id': fields['session_file'],
        'event': 'start', 'event_sequence': 0, 'position_ms': 0, 'duration_ms': 7200000})


def verify_heartbeat(client, fields):
    state = client.call('/api/v1/playback/resume?media_item_id=' + fields['session_movie'])
    return state['position_ms'] == fields['position']


def routes():
    result = subprocess.run([sys.executable, str(HERE / 'list-routes.py')], check=True,
                            text=True, capture_output=True)
    return json.loads(result.stdout)


def context(manifest, user, rng):
    movie, series = rng.choice(manifest['movies']), rng.choice(manifest['series'])
    fields = {'movie_library': manifest['libraries']['movie'],
              'tv_library': manifest['libraries']['tv'], 'movie': movie['media_item_id'],
              'series': series['media_item_id'], 'user': user['id'],
              'collection': rng.choice(user['collections']), 'login': user['login'],
              'password': os.environ.get('PERF_USER_PASSWORD', 'local-perf-user-password'),
              'write_movie': manifest['movies'][500]['media_item_id'],
              'jf_movie': 'w1_' + movie['media_item_id'].replace('-', ''),
              'jf_series': 'w1_' + series['media_item_id'].replace('-', ''),
              **manifest.get('resources', {}), **user.get('resources', {})}
    # These are actual source ids gathered from API responses during preparation.
    fields.update(manifest.get('samples', {}))
    if movie.get('poster_url'):
        fields['movie_file'] = movie['poster_url'].rsplit('/', 1)[-1]
    return fields


def interpolate(value, fields):
    if isinstance(value, str):
        match = re.fullmatch(r'\{(\w+)\}', value)
        return fields[match.group(1)] if match else value.format_map(fields)
    if isinstance(value, dict):
        return {key: interpolate(item, fields) for key, item in value.items()}
    if isinstance(value, list):
        return [interpolate(item, fields) for item in value]
    return value


class BusyScan:
    """Every measured request must overlap a running full TV scan."""
    def __init__(self, admin, library):
        self.admin, self.library, self.operation = admin, library, None

    def status(self):
        if not self.operation:
            return None
        return self.admin.call(f'/api/v1/libraries/{self.library}/scan-operations/{self.operation}')

    def running(self):
        status = self.status()
        return status and status.get('status') == 'running'

    def ensure(self):
        if self.running():
            return
        operation = self.admin.call(f'/api/v1/libraries/{self.library}/scan', 'POST')
        self.operation = operation['operation_id']
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            status = self.status()
            if status.get('status') == 'running':
                return
            if status.get('status') in ('failed', 'cancelled', 'completed', 'succeeded'):
                raise RuntimeError('TV scan finished before measurement; no valid busy window')
            time.sleep(.05)
        raise RuntimeError('TV scan did not enter running within 60 seconds')


class BusySamples:
    """Collect valid scan-window samples without discarding any request failure."""
    def __init__(self, required):
        self.required = required
        self.samples = []
        self.codes = Counter()
        self.invalid = []
        self.attempts = 0
        self.outside = 0

    @property
    def complete(self):
        return len(self.samples) >= self.required

    @property
    def can_attempt(self):
        return (not self.complete and self.attempts < self.required * 2
                and all(200 <= int(code) < 300 for code in self.codes))

    @property
    def failed(self):
        return (not self.complete or any(not 200 <= int(code) < 300 for code in self.codes))

    def record(self, code, elapsed, in_window):
        self.attempts += 1
        self.codes[code] += 1
        if in_window:
            self.samples.append(elapsed)
        else:
            self.outside += 1
            self.invalid.append({'code': code, 'elapsed_ms': elapsed})


def page_request(client, request, fields):
    url = interpolate(request['url'], fields)
    code, elapsed, payload = client.raw(url, request.get('method', 'GET'),
                                        interpolate(request.get('body'), fields),
                                        request.get('auth', True))
    if request.get('follow_file_pages') and 200 <= code < 300:
        cursor = json.loads(payload)['data']['next_file_cursor']
        seen = set()
        while cursor:
            if cursor in seen:
                return 0, elapsed, b''
            seen.add(cursor)
            code, duration, payload = client.raw(url + '/files?after=' +
                                                urllib.parse.quote(cursor, safe=''))
            elapsed += duration
            if not 200 <= code < 300:
                return code, elapsed, payload
            cursor = json.loads(payload)['data']['next_cursor']
    return code, elapsed, payload


def run_case(case, fields, self_client, jf_client, admin, executor):
    client = jf_client if case['surface'] == 'jellyfin' else self_client
    if case.get('admin'):
        client = admin
    if case.get('requests'):
        started = time.perf_counter()
        futures = [executor.submit(page_request, admin if request.get('admin') else client, request, fields)
                   for request in case['requests']]
        results = [future.result() for future in futures]
        code = next((result[0] for result in results if not 200 <= result[0] < 300), 200)
        return code, (time.perf_counter() - started) * 1000, True
    url = interpolate(case['url'], fields)
    code, elapsed, payload = client.raw(url, case.get('method', 'GET'),
                                      interpolate(case.get('body'), fields), case.get('auth', True))
    matches = not 200 <= code < 300 or response_matches(case, payload)
    return code, elapsed, matches


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['idle', 'busy', 'cold'])
    parser.add_argument('--n', type=int, default=40)
    parser.add_argument('--only', default='')
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--base-url', default=os.environ.get('PERF_BASE_URL', 'http://127.0.0.1:18766'))
    parser.add_argument('--manifest', type=Path, default=Path('/tmp/e2e-stress/perf-dataset.json'))
    parser.add_argument('--cases', type=Path, default=HERE / 'perf-cases.json')
    parser.add_argument('--restart-command', help='Argument string executed without a shell before each cold case')
    args = parser.parse_args()
    config = json.loads(args.cases.read_text())
    missing = uncovered_routes(routes(), config['cases'] + config['exemptions'])
    if missing:
        print(json.dumps({'uncovered_get_routes': missing}, ensure_ascii=False, indent=2))
        return 1
    print(f'Coverage OK: {sum(r["method"] == "get" for r in routes())} GET routes; '
          f'{len(config["cases"])} cases, {len(config["exemptions"])} exemptions', flush=True)
    if args.dry_run:
        return 0
    if args.mode == 'cold' and not args.restart_command:
        parser.error('cold requires --restart-command to restart the process before every case')
    if args.n < 1:
        parser.error('--n must be positive')
    manifest = json.loads(args.manifest.read_text())
    password = os.environ.get('PERF_USER_PASSWORD', 'local-perf-user-password')
    admin = Client(args.base_url, os.environ.get('CRAWLER_MEDIA_TOKEN', 'changeme'))
    rng = random.Random(20261029)
    selected = [case for case in config['cases'] if case['id'].startswith(args.only)]
    if not selected:
        parser.error('--only selected no cases')
    baseline = validate_baseline(json.loads(args.baseline.read_text()), args.mode, selected) if args.baseline else {}
    clients = []
    for user in manifest['users']:
        self_client, _ = login(args.base_url, user['login'], password)
        jf_client, _ = login(args.base_url, user['login'], password, jellyfin=True)
        clients.append((user, self_client, jf_client))
    busy = BusyScan(admin, manifest['libraries']['tv']) if args.mode == 'busy' else None
    results = {}
    commit = admin.call('/api/v1/health').get('commit')
    with ThreadPoolExecutor(max_workers=8) as executor:
        for case in selected:
            samples, codes, outside_busy, errors = [], Counter(), 0, []
            writes_verified = []
            responses_verified = []
            precondition_codes = Counter()
            count = 1 if args.mode == 'cold' else args.n
            sampler = BusySamples(count) if busy else None
            index = 0
            # Bounded replacement is only for scan-window crossings, never to hide errors.
            while sampler.can_attempt if sampler else index < count:
                user, self_client, jf_client = clients[(index + len(results)) % len(clients)]
                fields = context(manifest, user, rng)
                fields['sequence'] = 1
                fields['position'] = 120000 + index + len(results) * args.n
                matches = False
                measured = False
                try:
                    if case.get('prepare') == 'heartbeat' or case['id'] == 'self-playback-chapters':
                        prepare_heartbeat(self_client, fields)
                    if args.mode == 'cold':
                        subprocess.run(shlex.split(args.restart_command), check=True,
                                       stdout=subprocess.DEVNULL)
                    if busy:
                        busy.ensure()
                    code, elapsed, matches = run_case(case, fields, self_client, jf_client, admin, executor)
                    measured = True
                except KeyError as error:
                    code, elapsed = 0, 0
                    errors.append(f'dataset resource {error.args[0]} not prepared')
                except (RuntimeError, subprocess.CalledProcessError) as error:
                    code, elapsed = failure_code(error), 0
                    precondition_codes[code] += 1
                    errors.append(str(error))
                in_window = measured
                if busy:
                    try:
                        if not busy.running():
                            outside_busy += 1
                            in_window = False
                    except RuntimeError as error:
                        outside_busy += 1
                        in_window = False
                        precondition_codes[failure_code(error)] += 1
                        errors.append(str(error))
                if case.get('prepare') == 'heartbeat':
                    try:
                        verified = 200 <= code < 300 and verify_heartbeat(self_client, fields)
                    except (RuntimeError, KeyError, ValueError) as error:
                        precondition_codes[failure_code(error)] += 1
                        verified = False
                    writes_verified.append(verified)
                    if not verified:
                        errors.append('heartbeat did not persist the expected position')
                responses_verified.append(matches)
                if sampler:
                    sampler.record(code, elapsed, in_window)
                    samples, codes = sampler.samples, sampler.codes
                else:
                    samples.append(elapsed)
                    codes[code] += 1
                index += 1
            result = evaluate(samples, codes, case['class'], args.mode,
                              baseline.get(case['id'], {}).get('p95'))
            result.update({'class': case['class'], 'surface': case['surface'],
                           'outside_busy': outside_busy, 'errors': sorted(set(errors)),
                           'attempts': sampler.attempts if sampler else count,
                           'invalid_samples': sampler.invalid if sampler else [],
                           'precondition_codes': dict(precondition_codes),
                           'response_verified': all(responses_verified),
                           'write_verified': all(writes_verified) if writes_verified else None})
            if (sampler and sampler.failed) or errors or not all(responses_verified) or (writes_verified and not all(writes_verified)):
                result['passed'] = False
            results[case['id']] = result
            if args.out:
                args.out.parent.mkdir(parents=True, exist_ok=True)
                args.out.write_text(json.dumps({'mode': args.mode, 'commit': commit,
                    'complete': False, 'cases': results, 'passed': False},
                    ensure_ascii=False, indent=2) + '\n')
            print(f'{case["id"]}: p50={result["p50"]} p95={result["p95"]} '
                  f'p99={result["p99"]} max={result["max"]}ms '
                  f'codes={dict(codes)} {"PASS" if result["passed"] else "FAIL"}', flush=True)
    report = {'mode': args.mode, 'complete': True, 'base_url': args.base_url, 'n': args.n,
              'commit': commit,
              'timestamp': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'cases': results, 'passed': all(result['passed'] for result in results.values())}
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (RuntimeError, OSError, ValueError) as error:
        print(f'perfgate: {error}', file=sys.stderr)
        sys.exit(1)
