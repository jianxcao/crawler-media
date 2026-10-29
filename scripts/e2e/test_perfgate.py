"""Offline tests for the performance gate; no server is required."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('perfgate', Path(__file__).with_name('perfgate.py'))
perfgate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perfgate)


class PerfGateTests(unittest.TestCase):
    def test_fixture_disables_only_optional_global_scrape(self):
        from perf_support import disable_auxiliary_scrape

        class Jobs:
            def __init__(self):
                self.rows = [{'id': kind, 'kind': kind, 'enabled': True}
                             for kind in ['scrape', 'probe', 'library_scan']]

            def call(self, path, method='GET', body=None):
                if method == 'GET':
                    return self.rows
                row = next(row for row in self.rows if path.endswith('/' + row['id']))
                row.update(body)
                return row

        jobs = Jobs()
        disable_auxiliary_scrape(jobs)
        self.assertEqual({row['kind']: row['enabled'] for row in jobs.rows},
                         {'scrape': False, 'probe': True, 'library_scan': True})

    def test_uncovered_get_route_fails(self):
        routes = [{'surface': 'self', 'method': 'get', 'path': '/covered'},
                  {'surface': 'self', 'method': 'get', 'path': '/missing'}]
        cases = [{'surface': 'self', 'route': '/covered'}]
        self.assertEqual(perfgate.uncovered_routes(routes, cases), [routes[1]])

    def test_p95_over_slo_fails(self):
        result = perfgate.evaluate([180.0] * 40, {200: 40}, 'L', 'idle')
        self.assertEqual(result['p95'], 180)
        self.assertFalse(result['passed'])

    def test_regression_over_20_percent_flagged(self):
        result = perfgate.evaluate([120.1] * 40, {200: 40}, 'L', 'idle', 100)
        self.assertTrue(result['regressed'])
        self.assertFalse(result['passed'])

    def test_any_5xx_in_busy_fails(self):
        result = perfgate.evaluate([1.0] * 40, {200: 39, 503: 1}, 'L', 'busy')
        self.assertFalse(result['passed'])

    def test_error_response_and_timeout_are_never_passed(self):
        for code in [0, 400, 401, 404, 500]:
            with self.subTest(code=code):
                self.assertFalse(perfgate.evaluate([1], {code: 1}, 'D', 'idle')['passed'])

    def test_busy_budget_is_twice_idle_and_cold_uses_p99(self):
        self.assertTrue(perfgate.evaluate([290], {200: 1}, 'L', 'busy')['passed'])
        self.assertFalse(perfgate.evaluate([301], {200: 1}, 'L', 'busy')['passed'])
        self.assertTrue(perfgate.evaluate([790], {200: 1}, 'L', 'cold')['passed'])

    def test_detail_bundle_includes_file_continuations_and_reports_failure(self):
        class FilePages:
            def raw(self, path, *args):
                if path.endswith('/detail'):
                    return 200, 1, b'{"ok":true,"data":{"next_file_cursor":"next page"}}'
                if path.endswith('/detail/files?after=next%20page'):
                    return 503, 1, b'{}'
                raise AssertionError('unexpected continuation URL')
        result = perfgate.page_request(FilePages(), {'url': '/detail', 'follow_file_pages': True}, {})
        self.assertEqual(result[0], 503)

    def test_body_placeholder_retains_numeric_type_and_urls_remain_strings(self):
        result = perfgate.interpolate({'position_ms': '{position}', 'path': '/items/{position}'},
                                     {'position': 123})
        self.assertEqual(result, {'position_ms': 123, 'path': '/items/123'})

    def test_successful_noop_heartbeat_fails_persisted_write_verification(self):
        class Resume:
            def call(self, path):
                return {'position_ms': 0}
        self.assertFalse(perfgate.verify_heartbeat(Resume(), {
            'session_movie': 'movie', 'position': 123456}))

    def test_heartbeat_verification_accepts_only_exact_persisted_position(self):
        class Resume:
            def call(self, path):
                return {'position_ms': 123456}
        self.assertTrue(perfgate.verify_heartbeat(Resume(), {
            'session_movie': 'movie', 'position': 123456}))

    def test_baseline_rejects_incomplete_wrong_mode_and_missing_cases(self):
        valid = {'complete': True, 'mode': 'idle', 'cases': {'detail': {'p95': 10}}}
        for report in [dict(valid, complete=False), dict(valid, mode='busy'),
                       dict(valid, cases={})]:
            with self.subTest(report=report), self.assertRaises(ValueError):
                perfgate.validate_baseline(report, 'idle', [{'id': 'detail'}])
        self.assertEqual(perfgate.validate_baseline(valid, 'idle', [{'id': 'detail'}]), valid['cases'])

    def test_episode_workload_is_detail_and_requires_seventy_items(self):
        import json
        cases = json.loads(Path(__file__).with_name('perf-cases.json').read_text())['cases']
        case = next(case for case in cases if case['id'] == 'jellyfin-shows-id-episodes')
        self.assertEqual((case['class'], case['expected_items']), ('D', 70))
        self.assertFalse(perfgate.response_matches(case, b'{"Items":[]}'))
        self.assertTrue(perfgate.response_matches(case, json.dumps({'Items': [{}] * 70}).encode()))
        self.assertFalse(any(case['id'] == 'jellyfin-items-last-played' for case in cases))

    def test_route_parser_qualified_chained_and_comments(self):
        spec = importlib.util.spec_from_file_location('list_routes', Path(__file__).with_name('list-routes.py'))
        extractor = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(extractor)
        source = '''// .route("/comment", get(fake))
        .route("/actual", axum::routing::post(handler).get(other))
        /* .route("/block-comment", get(fake)) */
        .route("/url", get(handler("https://example.test/a")))'''
        self.assertEqual(list(extractor.route_calls(source)),
                         [('get', '/actual'), ('post', '/actual'), ('get', '/url')])

    def test_precondition_failures_retain_http_status(self):
        from perf_support import ApiError
        self.assertEqual(perfgate.failure_code(ApiError(503, '/jobs', b'')), 503)
        self.assertEqual(perfgate.failure_code(ApiError(500, '/jobs', b'')), 500)
        self.assertEqual(perfgate.failure_code(RuntimeError('missing fixture')), 0)

    def test_busy_sampler_discards_only_successful_boundary_crossings(self):
        sampler = perfgate.BusySamples(40)
        sampler.record(200, 100, False)
        for _ in range(40):
            sampler.record(200, 10, True)
        self.assertTrue(sampler.complete)
        self.assertEqual((len(sampler.samples), sampler.attempts, sampler.outside), (40, 41, 1))
        self.assertEqual(sampler.samples, [10] * 40)
        self.assertFalse(sampler.failed)

    def test_busy_sampler_retains_503_outside_window(self):
        sampler = perfgate.BusySamples(1)
        sampler.record(503, 2, False)
        self.assertFalse(sampler.can_attempt)
        self.assertTrue(sampler.failed)
        self.assertEqual(sampler.codes[503], 1)

    def test_busy_sampler_stops_when_no_request_fits_scan_window(self):
        sampler = perfgate.BusySamples(2)
        for _ in range(4):
            sampler.record(200, 5, False)
        self.assertFalse(sampler.can_attempt)
        self.assertFalse(sampler.complete)
        self.assertTrue(sampler.failed)

    def test_percentile_uses_nearest_rank(self):
        self.assertEqual(perfgate.percentile(list(range(1, 41)), .95), 38)
        self.assertEqual(perfgate.percentile(list(range(1, 41)), .99), 40)


if __name__ == '__main__':
    unittest.main()
