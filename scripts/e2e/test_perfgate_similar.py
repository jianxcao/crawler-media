"""Finite recommendation HTTP observations: readiness is not completion."""
import importlib.util
import json
import contextlib
import io
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('perfgate', Path(__file__).with_name('perfgate.py'))
perfgate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perfgate)

CASE = {'id': 'similar', 'response_contract': 'similar'}
PENDING = {'ok': True, 'data': {'state': 'pending', 'retry_after_ms': 1}}
READY = {'ok': True, 'data': {'state': 'ready', 'items': [{'title': 'Full recommendation', 'tmdb_id': '42'}]}}


class FiniteSource:
    def __init__(self, replies):
        self.replies = iter(replies)

    def raw(self, path, *args):
        assert path == '/similar'
        code, body = next(self.replies)
        started = time.perf_counter()
        time.sleep(.005)
        return code, (time.perf_counter() - started) * 1000, json.dumps(body).encode()


class SimilarPerfTests(unittest.TestCase):
    def test_busy_cli_replacement_compares_only_in_window_completions(self):
        class Client:
            fail_first = False

            def __init__(self, *args):
                self.requests = 0

            def raw(self, *args):
                self.requests += 1
                if self.fail_first and self.requests == 1:
                    return 200, 1.0, json.dumps({'ok': False}).encode()
                return 200, 1.0, json.dumps(READY).encode()

            def call(self, *args, **kwargs):
                return {'commit': 'fixture'}

            def close(self):
                pass

        class BusyWindow:
            def __init__(self, *args):
                self.windows = iter((False, True))

            def ensure(self):
                pass

            def running(self):
                return next(self.windows)

        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            root = Path(directory)
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({'users': [{'id': 'user', 'login': 'user'}],
                                            'libraries': {'tv': 'tv'}}))
            for bundle, fail_boundary, fault in ((False, False, None), (True, False, None),
                    (False, True, None), (True, True, None), (False, False, 'subset'),
                    (False, False, 'marker'), (False, False, 'count')):
                Client.fail_first = fail_boundary
                case = {'id': 'detail' if bundle else 'similar', 'surface': 'self', 'class': 'L',
                        'route': '/similar'}
                if bundle:
                    case['requests'] = [{'url': '/similar', 'response_contract': 'similar'}]
                else:
                    case.update(url='/similar', response_contract='similar')
                cases = root / 'cases.json'
                cases.write_text(json.dumps({'cases': [case], 'exemptions': []}))
                baseline = root / 'baseline.json'
                observation = {'passed': True, 'completion_ms': 100.0, 'terminal_code': 200,
                               'terminal_state': 'ready', 'in_window': True}
                previous = {'p95': 100.0, 'n': 1, 'attempts': 1, 'codes': {'200': 1}, 'errors': [],
                            'precondition_codes': {}, 'response_verified': True, 'write_verified': None,
                            'recommendations': {'contract': 'legacy-list', 'passed': True,
                                'complete_body_p95_ms': 100.0, 'observations': [observation]}}
                previous['recommendations']['accepted_window'] = dict(previous['recommendations'])
                if fault == 'subset':
                    previous['recommendations']['accepted_window']['observations'] = []
                elif fault == 'marker':
                    del observation['in_window']
                elif fault == 'count':
                    previous['attempts'] = 2
                baseline.write_text(json.dumps({'mode': 'busy', 'complete': True, 'n': 1,
                                                'similar_contract': 'legacy-list', 'cases': {case['id']: previous}}))
                out = root / f'{bundle}-{fail_boundary}-{fault}.json'
                argv = ['perfgate', 'busy', '--n', '1', '--cases', str(cases), '--manifest', str(manifest),
                        '--baseline', str(baseline), '--out', str(out)]
                with patch.object(sys, 'argv', argv), patch.object(perfgate, 'routes', return_value=[
                        {'method': 'get', 'surface': 'self', 'path': '/similar'}]), \
                     patch.object(perfgate, 'Client', Client), patch.object(perfgate, 'BusyScan', BusyWindow), \
                     patch.object(perfgate, 'login', side_effect=lambda *a, **k: (Client(), 'user')), \
                     patch.object(perfgate, 'context', return_value={}), contextlib.redirect_stdout(io.StringIO()):
                    result = perfgate.main()
                row = json.loads(out.read_text())['cases'][case['id']]
                self.assertEqual(result, 1 if fail_boundary or fault else 0, row)
                self.assertEqual(row['attempts'], 2)
                self.assertEqual(len(row['invalid_samples']), 1)
                self.assertEqual(len(row['recommendations']['observations']), 2)
                self.assertEqual([item['in_window'] for item in row['recommendations']['observations']], [False, True])
                self.assertEqual(len(row['recommendations']['accepted_window']['observations']), 1)
                self.assertEqual(row['relative_status'], 'unavailable' if fail_boundary or fault else 'passed')
                self.assertEqual(row['response_verified'], not fail_boundary)
                self.assertIsNotNone(row['relative_current_p95_ms'])

    def test_cli_cross_contract_uses_completed_result_for_relative_gate(self):
        class Client:
            def __init__(self, *args):
                self.pending = False

            def raw(self, *args):
                if not self.pending:
                    self.pending = True
                    return 202, 1.0, json.dumps(PENDING).encode()
                time.sleep(.18)
                return 200, 180.0, json.dumps(READY).encode()

            def call(self, *args, **kwargs):
                return {'commit': 'fixture'}

            def close(self):
                pass

        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            root = Path(directory)
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({'users': [{'id': 'user', 'login': 'user'}], 'libraries': {}}))
            for bundle in (False, True):
                case = {'id': 'detail' if bundle else 'similar', 'surface': 'self', 'class': 'L',
                        'route': '/similar'}
                if bundle:
                    case['requests'] = [{'url': '/similar', 'response_contract': 'similar'}]
                else:
                    case.update(url='/similar', response_contract='similar')
                cases = root / 'cases.json'
                cases.write_text(json.dumps({'cases': [case], 'exemptions': []}))
                for old_ms, fault in ((100.0, None), (300.0, None), (100.0, 'contract'),
                                      (100.0, 'completion'), (100.0, 'count')):
                    baseline = root / 'baseline.json'
                    old_observation = {'passed': True, 'completion_ms': old_ms, 'terminal_code': 200,
                                       'terminal_state': 'ready'}
                    previous = {'p95': old_ms, 'n': 1, 'codes': {'200': 1}, 'errors': [],
                                'precondition_codes': {}, 'response_verified': True, 'write_verified': None,
                                'recommendations': {'contract': 'legacy-list', 'passed': True,
                                    'complete_body_p95_ms': old_ms, 'observations': [old_observation]}}
                    if fault == 'completion':
                        old_observation['passed'] = False
                    if fault == 'count':
                        previous['recommendations']['observations'] = []
                    baseline_data = {'mode': 'idle', 'complete': True, 'n': 1,
                                     'similar_contract': 'legacy-list', 'cases': {case['id']: previous}}
                    if fault == 'contract':
                        del baseline_data['similar_contract']
                    baseline.write_text(json.dumps(baseline_data))
                    out = root / f'{bundle}-{old_ms}-{fault}.json'
                    argv = ['perfgate', 'idle', '--n', '1', '--cases', str(cases), '--manifest', str(manifest),
                            '--baseline', str(baseline), '--out', str(out)]
                    with patch.object(sys, 'argv', argv), patch.object(perfgate, 'routes', return_value=[
                            {'method': 'get', 'surface': 'self', 'path': '/similar'}]), \
                         patch.object(perfgate, 'Client', Client), \
                         patch.object(perfgate, 'login', side_effect=lambda *a, **k: (Client(), 'user')), \
                         patch.object(perfgate, 'context', return_value={}), contextlib.redirect_stdout(io.StringIO()):
                        result = perfgate.main()
                    row = json.loads(out.read_text())['cases'][case['id']]
                    self.assertTrue(row['absolute_passed'])
                    self.assertEqual(row['relative_metric'], 'complete_body_p95_ms')
                    self.assertEqual(row['initial_relative_status'],
                                     'unavailable_contract_missing' if fault == 'contract' else 'unavailable_contract_changed')
                    self.assertEqual(row['relative_baseline_p95_ms'], old_ms if fault is None else None)
                    self.assertGreaterEqual(row['relative_current_p95_ms'], 180)
                    self.assertEqual(row['relative_status'], 'unavailable' if fault else
                                     'failed' if old_ms == 100 else 'passed')
                    self.assertEqual(result, 1 if fault or old_ms == 100 else 0)

    def test_cli_legacy_list_is_complete_and_preserves_absolute_slo(self):
        class Client:
            def __init__(self, *args):
                pass

            def raw(self, *args):
                return 200, 180.0, json.dumps({'ok': True, 'data': [{'id': 'one'}]}).encode()

            def call(self, *args, **kwargs):
                return {'commit': 'fixture'}

            def close(self):
                pass

        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            root = Path(directory)
            cases = root / 'cases.json'
            cases.write_text(json.dumps({'cases': [dict(CASE, surface='self', url='/similar', route='/similar',
                                                       **{'class': 'L'})], 'exemptions': []}))
            manifest = root / 'manifest.json'
            manifest.write_text(json.dumps({'users': [{'id': 'user', 'login': 'user'}], 'libraries': {}}))
            for contract in ('legacy-list', 'stateful', 'default'):
                out = root / (contract + '.json')
                argv = ['perfgate.py', 'idle', '--n', '1', '--cases', str(cases), '--manifest',
                        str(manifest), '--out', str(out)]
                if contract != 'default':
                    argv += ['--similar-contract', contract]
                with patch.object(sys, 'argv', argv), patch.object(perfgate, 'routes', return_value=[
                        {'method': 'get', 'surface': 'self', 'path': '/similar'}]), \
                     patch.object(perfgate, 'Client', Client), \
                     patch.object(perfgate, 'login', side_effect=lambda *a, **k: (Client(), 'user')), \
                     patch.object(perfgate, 'context', return_value={}), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(perfgate.main(), 1)
                report = json.loads(out.read_text())
                self.assertEqual(report['similar_contract'], 'stateful' if contract == 'default' else contract)
                case = report['cases']['similar']
                self.assertFalse(case['absolute_passed'])
                observation = case['recommendations']['observations'][0]
                self.assertEqual(observation['passed'], contract == 'legacy-list')
                if contract == 'legacy-list':
                    self.assertEqual(observation['items_count'], 1)
                    self.assertEqual(observation['pending_responses'], 0)
                    self.assertEqual(observation['completion_ms'], 180.0)
                    self.assertTrue(case['response_verified'])

    def test_legacy_list_rejects_stateful_errors_and_wrong_status(self):
        for code, body in [(202, {'ok': True, 'data': []}), (200, PENDING), (200, READY),
                           (200, {'ok': False, 'data': []}), (200, {'ok': True, 'data': {}})]:
            with self.subTest(code=code, body=body):
                observed = perfgate.measure_similar(FiniteSource([(code, body)]), '/similar',
                                                   contract='legacy-list')
                self.assertFalse(observed['passed'])
                self.assertEqual(observed['terminal_state'], 'error')

    def test_page_bundle_uses_explicit_legacy_contract(self):
        from concurrent.futures import ThreadPoolExecutor
        source = FiniteSource([(200, {'ok': True, 'data': []})])
        case = {'id': 'page-detail', 'surface': 'self', 'requests': [
            {'url': '/similar', 'response_contract': 'similar'}]}
        with ThreadPoolExecutor(max_workers=1) as executor:
            _, _, matches = perfgate.run_case(case, {}, source, source, source, executor,
                                              similar_contract='legacy-list')
        self.assertTrue(matches)

    def test_pending_is_a_distinct_explicit_contract_not_a_successful_empty_list(self):
        self.assertTrue(perfgate.response_matches(CASE, json.dumps(PENDING).encode()))
        self.assertTrue(perfgate.response_matches(CASE, json.dumps(READY).encode()))
        for body in [
            {'ok': True, 'data': []},
            {'ok': True, 'data': {'state': 'ready'}},
            {'ok': True, 'data': {'state': 'unknown', 'items': []}},
            {'ok': True, 'data': {'state': 'pending', 'retry_after_ms': 1, 'items': []}},
            {'ok': True, 'data': {'state': 'pending', 'retry_after_ms': 0}},
        ]:
            with self.subTest(body=body):
                self.assertFalse(perfgate.response_matches(CASE, json.dumps(body).encode()))

    def test_pending_observation_reports_first_response_and_full_completion_separately(self):
        source = FiniteSource([(202, PENDING), (202, PENDING), (200, READY)])
        observation = perfgate.measure_similar(source, '/similar', deadline_seconds=1)
        self.assertEqual(observation['initial_code'], 202)
        self.assertEqual(observation['initial_state'], 'pending')
        self.assertEqual(observation['terminal_code'], 200)
        self.assertEqual(observation['terminal_state'], 'ready')
        self.assertEqual(observation['items_count'], 1)
        self.assertEqual(observation['pending_responses'], 2)
        self.assertEqual(observation['ready_responses'], 1)
        self.assertGreaterEqual(observation['completion_ms'], 15)
        self.assertGreater(observation['completion_ms'], observation['initial_ms'])
        self.assertTrue(observation['passed'])

    def test_immediate_ready_has_no_invented_pending_or_missing_body(self):
        observation = perfgate.measure_similar(FiniteSource([(200, READY)]), '/similar', deadline_seconds=1)
        self.assertEqual(observation['initial_state'], 'ready')
        self.assertEqual(observation['pending_responses'], 0)
        self.assertEqual(observation['ready_responses'], 1)
        self.assertEqual(observation['items_count'], 1)
        self.assertGreaterEqual(observation['completion_ms'], observation['initial_ms'])
        self.assertTrue(observation['passed'])

    def test_terminal_error_after_pending_fails_instead_of_becoming_empty_ready(self):
        source = FiniteSource([(202, PENDING), (502, {'ok': False, 'error': {'code': 'catalog.unavailable'}})])
        observation = perfgate.measure_similar(source, '/similar', deadline_seconds=1)
        self.assertEqual(observation['terminal_code'], 502)
        self.assertEqual(observation['terminal_state'], 'error')
        self.assertIsNone(observation['items_count'])
        self.assertEqual(observation['ready_responses'], 0)
        self.assertFalse(observation['passed'])

    def test_http_status_must_match_explicit_state(self):
        for reply in [(200, PENDING), (202, READY)]:
            with self.subTest(reply=reply):
                observation = perfgate.measure_similar(FiniteSource([reply]), '/similar', deadline_seconds=1)
                self.assertFalse(observation['passed'])
                self.assertNotEqual(observation['terminal_state'], 'ready')

    def test_forever_pending_has_finite_failed_completion_without_ready_result(self):
        class Pending:
            def raw(self, path, *args):
                return 202, .1, json.dumps(PENDING).encode()
        observation = perfgate.measure_similar(Pending(), '/similar', deadline_seconds=.02)
        self.assertEqual(observation['initial_state'], 'pending')
        self.assertEqual(observation['terminal_state'], 'timeout')
        self.assertEqual(observation['terminal_code'], 0)
        self.assertIsNone(observation['items_count'])
        self.assertEqual(observation['ready_responses'], 0)
        self.assertFalse(observation['passed'])


    def test_ordinary_case_keeps_initial_202_separate_from_observed_complete_body(self):
        from concurrent.futures import ThreadPoolExecutor
        source = FiniteSource([(202, PENDING), (200, READY)])
        deferred = []
        case = dict(CASE, surface='self', url='/similar')
        with ThreadPoolExecutor(max_workers=1) as executor:
            code, elapsed, matches = perfgate.run_case(case, {}, source, source, source,
                                                      executor, similar_requests=deferred)
        self.assertEqual(code, 202)
        self.assertTrue(matches)
        self.assertEqual(len(deferred), 1)
        client, url, initial, started = deferred[0]
        observed = perfgate.measure_similar(client, url, initial=initial, started_at=started)
        self.assertEqual(observed['initial_code'], code)
        self.assertEqual(observed['initial_ms'], elapsed)
        self.assertEqual(observed['terminal_code'], 200)
        self.assertGreater(observed['completion_ms'], elapsed)
        self.assertTrue(observed['passed'])

    def test_page_bundle_records_pending_and_rejects_mismatched_status(self):
        from concurrent.futures import ThreadPoolExecutor
        for reply, expected in [((202, PENDING), True), ((200, PENDING), False),
                                ((202, READY), False)]:
            with self.subTest(reply=reply):
                source = FiniteSource([reply])
                deferred = []
                case = {'id': 'page-detail', 'surface': 'self', 'requests': [
                    {'url': '/similar', 'response_contract': 'similar'}]}
                with ThreadPoolExecutor(max_workers=1) as executor:
                    _, _, matches = perfgate.run_case(case, {}, source, source, source,
                                                      executor, similar_requests=deferred)
                self.assertEqual(matches, expected)
                self.assertEqual(len(deferred), 1)
                self.assertEqual(deferred[0][2][0], reply[0])

    def test_report_keeps_terminal_failure_and_completion_distribution(self):
        pending = perfgate.measure_similar(FiniteSource([(202, PENDING), (200, READY)]), '/similar')
        failed = perfgate.measure_similar(FiniteSource([(502, {'ok': False})]), '/similar')
        summary = perfgate.summarize_similar([pending, failed], perfgate.percentile)
        self.assertEqual(summary['initial_pending'], 1)
        self.assertEqual(summary['initial_ready'], 0)
        self.assertEqual(summary['completed_ready'], 1)
        self.assertEqual(summary['terminal_codes'], {200: 1, 502: 1})
        self.assertEqual(summary['complete_body_p95_ms'], max(pending['completion_ms'], failed['completion_ms']))
        self.assertFalse(summary['passed'])


if __name__ == '__main__':
    unittest.main()
