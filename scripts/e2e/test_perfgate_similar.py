"""Finite recommendation HTTP observations: readiness is not completion."""
import importlib.util
import json
from pathlib import Path
import time
import unittest

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
