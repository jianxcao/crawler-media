#!/usr/bin/env python3
"""Route-complete performance gate. See docs/testing/performance-slo.md."""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
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
from perf_similar import measure_similar, similar_payload, similar_response, summarize_similar
import perf_business

# Milliseconds: idle p95 and p99.
SLO = {'W': (50, 150), 'D': (100, 250), 'L': (150, 400), 'S': (200, 500),
       'A': (300, 800), 'U': (100, 250), 'U*': (500, 1000), 'P': (800, 1500)}
# Relative p95 increases smaller than this are measurement noise.
ABSOLUTE_P95_FLOOR_MS = 1.0
RESERVED_REPORT = None


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
    comparable = p95 is not None and baseline_p95 and baseline_p95 > 0
    regression = ((p95 / baseline_p95 - 1) * 100 if comparable else None)
    # A sub-millisecond absolute gap stays inside client and suite noise.
    below_floor = comparable and (p95 - baseline_p95) < ABSOLUTE_P95_FLOOR_MS
    regressed = regression is not None and regression > 20 + 1e-9 and not below_floor
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


def prepare_cache(case, issue):
    """Record cache population separately, before timing or a cold restart."""
    if case.get('cache_read'):
        return issue()
    return None


def comparable_p95(case, previous):
    # Mixed first-origin/cache reports cannot establish a cache-read baseline.
    if case.get('cache_read') and previous.get('latency_scope') != 'cached':
        return None
    return previous.get('p95')


def baseline_invalid_reason(case, previous, expected_n):
    p95 = previous.get('p95')
    if (not isinstance(p95, (int, float)) or isinstance(p95, bool)
            or not math.isfinite(p95) or p95 <= 0):
        return 'baseline p95 invalid'
    n, codes = previous.get('n'), previous.get('codes')
    if (type(expected_n) is not int or expected_n <= 0 or type(n) is not int or n != expected_n
            or not isinstance(codes, dict) or not codes
            or any(not str(code).isdigit() or not 200 <= int(code) < 300
                   or type(count) is not int or count <= 0 for code, count in codes.items())
            or sum(codes.values()) < n):
        return 'baseline HTTP samples invalid'
    if (previous.get('errors') != [] or previous.get('precondition_codes') != {}
            or previous.get('response_verified') is not True
            or 'write_verified' not in previous
            or (previous['write_verified'] is not None and previous['write_verified'] is not True)
            or (case.get('prepare') == 'heartbeat' and previous['write_verified'] is not True)):
        return 'baseline correctness evidence invalid or missing'
    return None


def completion_p95(summary, expected_n, contract):
    if not isinstance(summary, dict) or summary.get('contract') != contract or summary.get('passed') is not True:
        return None
    observations = summary.get('observations')
    if not isinstance(observations, list) or len(observations) != expected_n or not observations:
        return None
    durations = []
    for observation in observations:
        if not isinstance(observation, dict):
            return None
        duration = observation.get('completion_ms')
        if (observation.get('passed') is not True or observation.get('terminal_code') != 200
                or observation.get('terminal_state') != 'ready' or isinstance(duration, bool)
                or not isinstance(duration, (int, float)) or not math.isfinite(duration) or duration <= 0):
            return None
        durations.append(duration)
    value = percentile(durations, .95)
    return value if summary.get('complete_body_p95_ms') == value else None


def busy_completion_summary(summary, attempts, per_sample):
    if not isinstance(summary, dict) or type(attempts) is not int or attempts <= 0:
        return None
    observations = summary.get('observations')
    accepted = summary.get('accepted_window')
    if (not isinstance(observations, list) or len(observations) != attempts * per_sample
            or not isinstance(accepted, dict)
            or any(not isinstance(item, dict) or type(item.get('in_window')) is not bool for item in observations)
            or accepted.get('observations') != [item for item in observations if item['in_window']]):
        return None
    return accepted


def close_clients(admin, clients):
    admin.close()
    for _, rest_client, protocol_client, *_ in clients:
        rest_client.close()
        protocol_client.close()


@contextmanager
def client_connections(admin, clients):
    try:
        yield
    finally:
        close_clients(admin, clients)


def validate_baseline(report, mode, selected):
    if report.get('complete') is not True or report.get('mode') != mode:
        raise ValueError('baseline must be complete and use the same measurement mode')
    cases = report['cases']
    missing = [case['id'] for case in selected if case['id'] not in cases]
    if missing:
        raise ValueError('baseline missing selected cases: ' + ', '.join(missing))
    return cases


def response_matches(case, payload, similar_contract='stateful'):
    if case.get('response_contract') == 'similar':
        return similar_payload(payload, similar_contract) is not None
    if case.get('response_contract') == 'title_search':
        data = json.loads(payload).get('data', {})
        providers, titles = data.get('providers'), data.get('titles')
        return (data.get('provider') == 'tmdb'
                and isinstance(providers, list) and len(providers) == 1
                and isinstance(providers[0], dict)
                and providers[0].get('provider') == 'tmdb'
                and providers[0].get('ok') is True
                and isinstance(titles, list) and bool(titles))
    if case.get('response_contract') == 'discover':
        response = json.loads(payload)
        sections = response.get('data', {}).get('sections')
        return isinstance(sections, list) and all(
            isinstance(section, dict) and not section.get('error') for section in sections)
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


def prepare_subtitle(client, fields, observations):
    item = urllib.parse.quote(str(fields['subtitle_item']), safe='')
    code, _, payload = client.raw('/Items/' + item + '/PlaybackInfo')
    observation = {'code': code, 'outside_timing': True, 'session_id_advertised': False, 'passed': False}
    observations.append(observation)
    if not 200 <= code < 300:
        raise RuntimeError(f'subtitle PlaybackInfo HTTP {code}')
    try:
        info = json.loads(payload)
    except (ValueError, TypeError) as error:
        raise RuntimeError(f'subtitle PlaybackInfo JSON invalid: {error}') from error
    if isinstance(info, dict):
        sid = info.get('PlaySessionId')
        observation['session_id_advertised'] = isinstance(sid, str) and bool(sid)
    if not isinstance(info, dict) or not isinstance(info.get('MediaSources'), list):
        raise RuntimeError('subtitle PlaybackInfo sources invalid')
    matches = []
    for source in info['MediaSources']:
        if not isinstance(source, dict) or not isinstance(source.get('MediaStreams'), list):
            raise RuntimeError('subtitle PlaybackInfo source invalid')
        if any(isinstance(track, dict) and track.get('Type') == 'Subtitle'
               and track.get('IsExternal') is True and type(track.get('Index')) is int
               and track['Index'] == fields['subtitle_index'] for track in source['MediaStreams']):
            source_id = source.get('Id')
            if not isinstance(source_id, str) or not source_id:
                raise RuntimeError('subtitle source ID missing')
            matches.append(source_id)
    if len(matches) != 1:
        raise RuntimeError('subtitle source is missing or ambiguous')
    fields['subtitle_source'] = urllib.parse.quote(matches[0], safe='')
    observation['passed'] = True


def verify_heartbeat(client, fields):
    state = client.call('/api/v1/playback/resume?media_item_id=' + fields['session_movie'])
    return state['position_ms'] == fields['position']


def routes(root):
    result = subprocess.run([sys.executable, str(HERE / 'list-routes.py'), '--root', str(root)], check=True,
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


def run_case(case, fields, self_client, jf_client, admin, executor, similar_requests=None,
             business_expectation=None, response_received=None, similar_contract='stateful'):
    client = jf_client if case['surface'] == 'jellyfin' else self_client
    if case.get('admin'):
        client = admin
    if case.get('requests'):
        started = time.perf_counter()
        futures = [executor.submit(page_request, admin if request.get('admin') else client, request, fields)
                   for request in case['requests']]
        results = [future.result() for future in futures]
        elapsed = (time.perf_counter() - started) * 1000
        matches = True
        for request, result in zip(case['requests'], results):
            if request.get('response_contract') == 'similar':
                matches = matches and similar_response(result[0], result[2], similar_contract) is not None
                if similar_requests is not None:
                    source = admin if request.get('admin') else client
                    similar_requests.append((source, interpolate(request['url'], fields), result, started))
        code = next((result[0] for result in results if not 200 <= result[0] < 300), 200)
        return code, elapsed, matches
    url = interpolate(case['url'], fields)
    started = time.perf_counter()
    code, elapsed, payload = client.raw(url, case.get('method', 'GET'),
                                      interpolate(case.get('body'), fields), case.get('auth', True))
    if response_received is not None:
        response_received()
    if case['id'] in perf_business.CASES:
        matches = (not 200 <= code < 300 or perf_business.matches(
            perf_business.CASES[case['id']], payload, business_expectation))
    else:
        matches = not 200 <= code < 300 or response_matches(case, payload, similar_contract)
    if case.get('response_contract') == 'similar':
        matches = similar_response(code, payload, similar_contract) is not None
        if similar_requests is not None:
            similar_requests.append((client, url, (code, elapsed, payload), started))
    return code, elapsed, matches


def main():
    global RESERVED_REPORT
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
    parser.add_argument('--similar-contract', choices=['stateful', 'legacy-list'], default='stateful')
    parser.add_argument('--route-root', type=Path, default=HERE.parents[1])
    args = parser.parse_args()
    route_root = args.route_root.resolve()
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        with args.out.open('x') as output:
            json.dump({'mode': args.mode, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'complete': False, 'cases': {}, 'passed': False,
                       'business_status': 'unverified', 'business_reason': 'run did not complete'}, output)
        RESERVED_REPORT = args.out
    cases_bytes = args.cases.read_bytes()
    config = json.loads(cases_bytes)
    inventory = routes(route_root)
    get_count = sum(r['method'] == 'get' for r in inventory)
    if args.out:
        args.out.write_text(json.dumps({'mode': args.mode, 'similar_contract': args.similar_contract,
            'route_root': str(route_root), 'get_route_count': get_count, 'complete': False,
            'cases': {}, 'passed': False, 'business_status': 'unverified',
            'business_reason': 'zero GET routes' if not get_count else 'route coverage not established'}) + '\n')
    if not get_count:
        print('perfgate: route inventory contains zero GET routes', file=sys.stderr)
        return 1
    missing = uncovered_routes(inventory, config['cases'] + config['exemptions'])
    if missing:
        print(json.dumps({'uncovered_get_routes': missing}, ensure_ascii=False, indent=2))
        return 1
    print(f'Coverage OK: {get_count} GET routes; '
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
    baseline_report = json.loads(args.baseline.read_text()) if args.baseline else None
    baseline = validate_baseline(baseline_report, args.mode, selected) if baseline_report is not None else {}
    baseline_n = (1 if args.mode == 'cold' else baseline_report.get('n')) if baseline_report else None
    business_cases = [case for case in selected if case['id'] in perf_business.CASES]
    case_contract = perf_business.case_contract(cases_bytes, [case['id'] for case in selected]) if business_cases else None
    artifact, business_provenance = None, None
    if business_cases:
        try:
            artifact, business_provenance = perf_business.load(manifest)
            for case in business_cases:
                perf_business.verify_case(case)
        except perf_business.Unverified as error:
            report = {'mode': args.mode, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'get_route_count': get_count, 'complete': False, 'cases': {}, 'passed': False,
                      'business_status': 'unverified', 'business_reason': str(error)}
            if args.out:
                args.out.parent.mkdir(parents=True, exist_ok=True)
                args.out.write_text(json.dumps(report, indent=2) + '\n')
            return 1
    clients = []
    try:
        for user in manifest['users']:
            try:
                self_client, self_id = login(args.base_url, user['login'], password)
            except (RuntimeError, OSError, ValueError, KeyError) as error:
                raise perf_business.Unverified('self login failed') from error
            try:
                jf_client, jf_id = login(args.base_url, user['login'], password, jellyfin=True)
            except (RuntimeError, OSError, ValueError, KeyError) as error:
                self_client.close()
                raise perf_business.Unverified('Jellyfin login failed') from error
            clients.append((user, self_client, jf_client, (self_id, jf_id)))
        busy = BusyScan(admin, manifest['libraries']['tv']) if args.mode == 'busy' else None
    except Exception:
        close_clients(admin, clients)
        raise
    results = {}
    with client_connections(admin, clients), ThreadPoolExecutor(max_workers=8) as executor:
        try:
            commit = admin.call('/api/v1/health').get('commit')
        except (RuntimeError, OSError, ValueError, KeyError, AttributeError) as error:
            raise perf_business.Unverified('health admission failed') from error
        if business_cases and (not isinstance(commit, str) or len(commit) < 7
                               or not business_provenance['product_commit'].startswith(commit)):
            report = {'mode': args.mode, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'get_route_count': get_count, 'complete': False, 'cases': {}, 'passed': False,
                      'business_status': 'unverified', 'business_reason': 'live product commit mismatch'}
            if args.out:
                args.out.parent.mkdir(parents=True, exist_ok=True)
                args.out.write_text(json.dumps(report, indent=2) + '\n')
            return 1
        visible_movies = {}
        if business_cases:
            try:
                for user, _, jf_client, login_ids in clients:
                    if not isinstance(user.get('id'), str) or any(value != user['id'] for value in login_ids):
                        raise perf_business.Unverified('authenticated user does not match manifest')
                    path = '/Users/' + urllib.parse.quote(user['id'], safe='') + '/Views?IncludeHidden=true'
                    views = jf_client.call(path, envelope=False)
                    items = views.get('Items') if isinstance(views, dict) else None
                    if not isinstance(items, list):
                        raise perf_business.Unverified('user views invalid')
                    visible_movies[user['id']] = any(isinstance(item, dict)
                            and item.get('Id') == manifest['libraries']['movie'] for item in items)
            except (perf_business.Unverified, RuntimeError, KeyError, ValueError) as error:
                report = {'mode': args.mode, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'get_route_count': get_count, 'complete': False, 'cases': {}, 'passed': False,
                          'business_status': 'unverified', 'business_reason': 'user views admission failed'}
                if args.out:
                    args.out.parent.mkdir(parents=True, exist_ok=True)
                    args.out.write_text(json.dumps(report, indent=2) + '\n')
                return 1
        for case in selected:
            samples, codes, outside_busy, errors = [], Counter(), 0, []
            writes_verified = []
            responses_verified = []
            business_assertions = []
            similar_observations = []
            accepted_similar_observations = []
            preparation_observations = []
            precondition_codes = Counter()
            count = 1 if args.mode == 'cold' else args.n
            sampler = BusySamples(count) if busy else None
            index = 0

            def issue_warmup():
                # Same resolved URL as the first timed sample, without shifting later choices.
                saved = rng.getstate()
                user, self_client, jf_client, login_ids = clients[len(results) % len(clients)]
                fields = context(manifest, user, rng)
                rng.setstate(saved)
                fields['sequence'] = 1
                fields['position'] = 120000
                try:
                    expected = None
                    if case['id'] in perf_business.CASES:
                        perf_business.verify_context(manifest, user, fields, login_ids)
                        expected = perf_business.expectation(artifact, case['id'], user['id'], fields['movie_library'])
                    code, elapsed, matches = run_case(case, fields, self_client, jf_client, admin, executor,
                                                      business_expectation=expected, similar_contract=args.similar_contract)
                    return {'code': code, 'elapsed_ms': elapsed, 'response_verified': matches}
                except (KeyError, RuntimeError, subprocess.CalledProcessError, OSError, ValueError) as error:
                    return {'code': failure_code(error), 'elapsed_ms': None,
                            'response_verified': False, 'error': type(error).__name__}

            population = prepare_cache(case, issue_warmup)
            if population and (not 200 <= population['code'] < 300 or not population['response_verified']):
                precondition_codes[population['code']] += 1
                errors.append('cache preparation failed')
            # Bounded replacement is only for scan-window crossings, never to hide errors.
            while sampler.can_attempt if sampler else index < count:
                user, self_client, jf_client, login_ids = clients[(index + len(results)) % len(clients)]
                matches = False
                measured = False
                deferred_similar = []
                window = []
                try:
                    fields = context(manifest, user, rng)
                    fields['sequence'] = 1
                    fields['position'] = 120000 + index + len(results) * args.n
                    expected = None
                    if case['id'] in perf_business.CASES:
                        perf_business.verify_context(manifest, user, fields, login_ids)
                        if perf_business.CASES[case['id']] in ('items', 'latest') and not visible_movies[user['id']]:
                            raise perf_business.Unverified('movie library absent from user views')
                        expected = perf_business.expectation(artifact, case['id'], user['id'], fields['movie_library'])
                    if case.get('prepare') == 'heartbeat' or case['id'] == 'self-playback-chapters':
                        prepare_heartbeat(self_client, fields)
                    if case.get('prepare') == 'subtitle':
                        prepare_subtitle(jf_client, fields, preparation_observations)
                    if args.mode == 'cold':
                        # This is an intentional connection boundary, not a
                        # retry: writes must never reuse a pre-restart socket.
                        close_clients(admin, clients)
                        subprocess.run(shlex.split(args.restart_command), check=True,
                                       stdout=subprocess.DEVNULL)
                    if busy:
                        busy.ensure()
                    code, elapsed, matches = run_case(case, fields, self_client, jf_client, admin, executor,
                                                      similar_requests=deferred_similar, business_expectation=expected,
                                                      response_received=(lambda: window.append(bool(busy.running())))
                                                      if busy and case['id'] in perf_business.CASES else None,
                                                      similar_contract=args.similar_contract)
                    measured = True
                    if case['id'] in perf_business.CASES:
                        valid = 200 <= code < 300 and matches
                        business_assertions.append({'name': perf_business.CASES[case['id']],
                                                    'status': 'verified' if valid else 'failed',
                                                    'reason': None if valid else 'response mismatch or HTTP failure'})
                except perf_business.Unverified as error:
                    code, elapsed = 0, 0
                    errors.append('business expectation unavailable')
                    business_assertions.append({'name': perf_business.CASES[case['id']],
                                                'status': 'unverified', 'reason': str(error)})
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
                        if not (window[0] if window else busy.running()):
                            outside_busy += 1
                            in_window = False
                    except RuntimeError as error:
                        outside_busy += 1
                        in_window = False
                        precondition_codes[failure_code(error)] += 1
                        errors.append(str(error))
                # Busy eligibility belongs to the initial response window. Poll
                # full recommendations afterwards, with a separate wall clock.
                for source, url, initial, started in deferred_similar:
                    observed = measure_similar(source, url, initial=initial, started_at=started,
                                               contract=args.similar_contract)
                    observed['in_window'] = in_window
                    similar_observations.append(observed)
                    if in_window:
                        accepted_similar_observations.append(observed)
                    matches = matches and observed['passed']
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
            previous = baseline.get(case['id'], {})
            invalid_baseline = baseline_invalid_reason(case, previous, baseline_n) if args.baseline else None
            if args.baseline and case['id'] in perf_business.CASES and not invalid_baseline:
                invalid_baseline = perf_business.baseline_reason(baseline_report, business_provenance, case_contract)
                if not invalid_baseline and previous.get('business_status') != 'verified':
                    invalid_baseline = 'baseline business assertion unverified'
            baseline_p95 = comparable_p95(case, previous) if args.baseline and not invalid_baseline else None
            result = evaluate(samples, codes, case['class'], args.mode, baseline_p95)
            result.update({'class': case['class'], 'surface': case['surface'],
                           'latency_scope': 'cached' if case.get('cache_read') else 'default',
                           'cache_preparation': population,
                           'outside_busy': outside_busy, 'errors': sorted(set(errors)),
                           'attempts': sampler.attempts if sampler else count,
                           'invalid_samples': sampler.invalid if sampler else [],
                           'precondition_codes': dict(precondition_codes),
                           'response_verified': all(responses_verified),
                           'write_verified': all(writes_verified) if writes_verified else None})
            if case.get('prepare') == 'subtitle':
                result['preparation'] = {'type': 'subtitle', 'attempts': len(preparation_observations),
                                         'observations': preparation_observations}
            if case['id'] in perf_business.CASES:
                result['business_assertions'] = business_assertions
                statuses = [row['status'] for row in business_assertions]
                result['business_status'] = ('unverified' if 'unverified' in statuses or not statuses
                                             else 'failed' if 'failed' in statuses else 'verified')
                if perf_business.CASES[case['id']] == 'latest':
                    result['latest_order'] = {'status': 'unverified', 'reason': 'independent ordering unavailable'}
            if similar_observations:
                result['recommendations'] = summarize_similar(similar_observations, percentile, args.similar_contract)
                result['recommendations']['accepted_window'] = summarize_similar(
                    accepted_similar_observations, percentile, args.similar_contract)
            correctness_failed = ((sampler and sampler.failed) or errors or not all(responses_verified)
                                  or (writes_verified and not all(writes_verified)))
            absolute = evaluate(samples, codes, case['class'], args.mode)
            result['absolute_passed'] = bool(absolute['passed'] and not correctness_failed)
            similar_per_sample = (1 if case.get('response_contract') == 'similar' else
                                  sum(request.get('response_contract') == 'similar'
                                      for request in case.get('requests', [])))
            baseline_contract = baseline_report.get('similar_contract') if baseline_report else None
            contract_unknown = bool(args.baseline and similar_per_sample and
                                    baseline_contract not in ('legacy-list', 'stateful'))
            contract_changed = bool(args.baseline and similar_per_sample and not contract_unknown and
                                    baseline_contract != args.similar_contract)
            if contract_unknown or contract_changed:
                result['initial_relative_status'] = 'unavailable_contract_changed' if contract_changed else 'unavailable_contract_missing'
                result['relative_metric'] = 'complete_body_p95_ms'
                old_summary = previous.get('recommendations')
                if args.mode == 'busy':
                    old_summary = busy_completion_summary(old_summary, previous.get('attempts'), similar_per_sample)
                old_completion = completion_p95(old_summary, baseline_n * similar_per_sample,
                                                baseline_contract) if type(baseline_n) is int else None
                current_summary = (busy_completion_summary(result.get('recommendations'), result['attempts'], similar_per_sample)
                                   if args.mode == 'busy' else result.get('recommendations', {}).get('accepted_window'))
                current_completion = completion_p95(current_summary, len(samples) * similar_per_sample,
                                                    args.similar_contract)
                result['relative_baseline_p95_ms'] = old_completion
                result['relative_current_p95_ms'] = current_completion
                if contract_unknown or old_completion is None or current_completion is None:
                    invalid_baseline = invalid_baseline or 'similar completion or contract evidence invalid'
                    result['regression_percent'] = None
                    result['regressed'] = False
                else:
                    comparison = evaluate([current_completion], {200: 1}, case['class'], args.mode, old_completion)
                    result['regression_percent'] = comparison['regression_percent']
                    result['regressed'] = comparison['regressed']
                result['passed'] = result['absolute_passed'] and not result['regressed'] and not invalid_baseline
            if not args.baseline:
                result['relative_status'] = 'not_requested'
            elif not samples or any(not 200 <= int(code) < 300 for code in codes) or correctness_failed:
                result['relative_status'] = 'unavailable'
                result['baseline_note'] = 'current correctness evidence invalid'
                result['regression_percent'] = None
                result['regressed'] = False
            elif invalid_baseline:
                result['relative_status'] = 'unavailable'
                result['baseline_note'] = invalid_baseline
            elif baseline_p95 is None and not contract_changed:
                result['relative_status'] = 'cache_scope_unavailable'
                result['baseline_note'] = 'Cache-read scope requires a cache-read baseline; absolute SLO enforced'
            else:
                result['relative_status'] = 'failed' if result['regressed'] else 'passed'
            if correctness_failed or invalid_baseline:
                result['passed'] = False
            results[case['id']] = result
            if args.out:
                args.out.parent.mkdir(parents=True, exist_ok=True)
                args.out.write_text(json.dumps({'mode': args.mode, 'commit': commit, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'get_route_count': get_count,
                    'complete': False, 'cases': results, 'passed': False},
                    ensure_ascii=False, indent=2) + '\n')
            print(f'{case["id"]}: p50={result["p50"]} p95={result["p95"]} '
                  f'p99={result["p99"]} max={result["max"]}ms '
                  f'codes={dict(codes)} {"PASS" if result["passed"] else "FAIL"}', flush=True)
    report = {'mode': args.mode, 'complete': True, 'base_url': args.base_url, 'n': args.n, 'similar_contract': args.similar_contract, 'route_root': str(route_root), 'get_route_count': get_count,
              'commit': commit,
              'timestamp': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'cases': results, 'passed': all(result['passed'] for result in results.values())}
    if business_cases:
        statuses = [results[case['id']]['business_status'] for case in business_cases]
        report['business_status'] = ('unverified' if 'unverified' in statuses
                                     else 'failed' if 'failed' in statuses else 'verified')
        report['business_provenance'] = business_provenance
        report['business_manifest_path'] = str(args.manifest.resolve())
        report['case_contract'] = case_contract
        report['case_config_path'] = str(args.cases.resolve())
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, IndexError) as error:
        if RESERVED_REPORT is not None:
            reason = str(error) if isinstance(error, perf_business.Unverified) else type(error).__name__
            report = json.loads(RESERVED_REPORT.read_text())
            report.update(complete=False, passed=False, business_status='unverified', business_reason=reason)
            RESERVED_REPORT.write_text(json.dumps(report) + '\n')
        print(f'perfgate: {error}', file=sys.stderr)
        sys.exit(1)
