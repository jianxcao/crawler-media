"""Cache latency is gated; cache population stays visible without a fast SLO."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import urllib.parse
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('perfgate', Path(__file__).with_name('perfgate.py'))
perfgate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perfgate)


class CacheScopeTests(unittest.TestCase):
    def test_catalog_cache_cases_are_explicit_and_local_reads_stay_strict(self):
        cases = json.loads(Path(__file__).with_name('perf-cases.json').read_text())['cases']
        by_id = {case['id']: case for case in cases}
        for name in ['self-discover-kind', 'self-media-douban-id', 'self-media-person-id',
                     'self-discover-kind-collection-id', 'self-discover-kind-filtered',
                     'self-media-kind-id', 'self-media-id-poster',
                     'jellyfin-persons-name-images-primary', 'self-search-titles',
                     'self-genres-kind', 'self-libraries-routing-options',
                     'self-settings-countries', 'self-settings-languages',
                     'self-media-tv-id-season-season']:
            self.assertTrue(by_id[name].get('cache_read'), name)
        for name in ['self-ledger', 'jellyfin-items-counts', 'jellyfin-genres',
                     'self-posters-ledger-id', 'self-fanart-ledger-id', 'self-stills-ledger-id']:
            self.assertFalse(by_id[name].get('cache_read'), name)
        self.assertFalse(perfgate.evaluate([1200] * 40, {200: 40}, 'A', 'idle')['passed'])

    def test_cached_title_search_targets_prepared_tmdb_query(self):
        cases = json.loads(Path(__file__).with_name('perf-cases.json').read_text())['cases']
        case = next(case for case in cases if case['id'] == 'self-search-titles')
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(case['url']).query)
        self.assertEqual(query, {'keyword': ['Toy Story'], 'provider': ['tmdb']})
        self.assertEqual(case.get('response_contract'), 'title_search')
        for case in cases:
            if case.get('route') == '/search/titles' and case.get('cache_read'):
                query = urllib.parse.parse_qs(urllib.parse.urlsplit(case['url']).query)
                self.assertEqual(query.get('provider'), ['tmdb'])

    def test_title_search_contract_rejects_degraded_or_empty_results(self):
        case = {'response_contract': 'title_search'}
        healthy = {'provider': 'tmdb', 'providers': [{'provider': 'tmdb', 'ok': True}],
                   'titles': [{'provider': 'tmdb', 'title': 'Toy Story'}]}
        self.assertTrue(perfgate.response_matches(case, json.dumps({'data': healthy})))
        invalid = [
            {}, {'providers': []}, {'providers': None}, {'providers': [{}]},
            {'providers': [{'provider': 'tmdb', 'ok': False}]},
            {'providers': [{'provider': 'tmdb', 'ok': 'true'}]},
            {'providers': [{'provider': 'douban', 'ok': True}]},
            {'providers': [{'provider': 'tmdb', 'ok': True},
                           {'provider': 'douban', 'ok': False}]},
            {'providers': [{'provider': 'tmdb', 'ok': True},
                           {'provider': 'douban', 'ok': True}]},
            {'provider': 'all'}, {'titles': []}, {'titles': None},
        ]
        for changes in invalid:
            data = dict(healthy, **changes)
            if changes == {}:
                del data['providers']
            with self.subTest(changes=changes):
                self.assertFalse(perfgate.response_matches(case, json.dumps({'data': data})))
        data = dict(healthy)
        del data['titles']
        self.assertFalse(perfgate.response_matches(case, json.dumps({'data': data})))

    def test_title_search_population_failure_is_not_a_fast_cache_success(self):
        failure = b'{"data":{"provider":"tmdb","providers":[{"provider":"tmdb","ok":false}],"titles":[]}}'
        status, result, _ = self.run_fixture('idle', 200, failure, 'title_search')
        self.assertEqual(status, 1)
        self.assertFalse(result['cache_preparation']['response_verified'])
        self.assertFalse(result['passed'])

    def test_cache_population_is_recorded_without_latency_gating(self):
        observation = {'code': 200, 'elapsed_ms': 1400, 'response_verified': True}
        self.assertEqual(perfgate.prepare_cache({'cache_read': True}, lambda: observation), observation)
        self.assertIsNone(perfgate.prepare_cache({}, lambda: self.fail('local read was primed')))

    def test_obsolete_origin_warmup_flag_is_removed(self):
        cases = json.loads(Path(__file__).with_name('perf-cases.json').read_text())['cases']
        self.assertTrue(all('origin_warmup' not in case for case in cases))

    def test_baselines_compare_only_equivalent_cache_conditions(self):
        case = {'cache_read': True}
        self.assertIsNone(perfgate.comparable_p95(case, {'p95': 2}))
        self.assertEqual(perfgate.comparable_p95(case, {'p95': 2, 'latency_scope': 'cached'}), 2)
        self.assertEqual(perfgate.comparable_p95({}, {'p95': 2}), 2)

    def test_population_failure_cannot_be_hidden_by_a_later_success(self):
        status, result, _ = self.run_fixture('idle', 503)
        self.assertEqual(status, 1)
        self.assertEqual(result['codes'], {'200': 1})
        self.assertEqual(result['cache_preparation']['code'], 503)
        self.assertEqual(result['precondition_codes'], {'503': 1})
        self.assertFalse(result['passed'])

    def test_discover_section_failure_cannot_be_hidden_by_a_later_success(self):
        failure = b'{"data":{"sections":[{"error":{"code":"discover.section_failed"}}]}}'
        status, result, _ = self.run_fixture('idle', 200, failure, 'discover')
        self.assertEqual(status, 1)
        self.assertFalse(result['cache_preparation']['response_verified'])
        self.assertFalse(result['passed'])

    def test_discover_timed_section_failure_is_rejected(self):
        case = {'response_contract': 'discover'}
        self.assertFalse(perfgate.response_matches(case, b'{"data":{"sections":[{"error":{"code":"discover.section_failed"}}]}}'))
        self.assertTrue(perfgate.response_matches(case, b'{"data":{"sections":[{"items":[]}]}}'))

    def test_cold_process_reads_persistent_cache_after_restart(self):
        status, result, events = self.run_fixture('cold', 200)
        self.assertEqual(events, ['request', 'restart', 'request'])
        self.assertEqual(status, 0)
        self.assertEqual(result['latency_scope'], 'cached')
        self.assertEqual(result['cache_preparation']['elapsed_ms'], 1400)
        self.assertEqual(result['p95'], 1)
        self.assertTrue(result['passed'])

    def test_invalid_baseline_evidence_cannot_pass_relative_gate(self):
        invalid = [
            {'codes': {'0': 1}},
            {'codes': {'200': 1, '503': 1}},
            {'response_verified': False},
            {'errors': ['preparation failed']},
            {'precondition_codes': {'503': 1}},
            {'write_verified': False},
            {'p95': float('nan')},
            {'n': 0},
            {'codes': {}},
        ]
        for changes in invalid:
            with self.subTest(changes=changes):
                status, result, _ = self.run_fixture('cold', 200, baseline=changes)
                self.assertEqual(status, 1)
                self.assertTrue(result['absolute_passed'])
                self.assertEqual(result['relative_status'], 'unavailable')
                self.assertIsNone(result['regression_percent'])
                self.assertFalse(result['passed'])

    def test_baseline_failed_only_latency_is_still_comparable(self):
        status, result, _ = self.run_fixture('cold', 200, baseline={'passed': False})
        self.assertEqual(status, 0)
        self.assertEqual(result['relative_status'], 'passed')
        self.assertTrue(result['passed'])

    def test_cache_scope_mismatch_keeps_absolute_only_exception(self):
        status, result, _ = self.run_fixture('cold', 200, baseline={'latency_scope': 'default'})
        self.assertEqual(status, 0)
        self.assertTrue(result['absolute_passed'])
        self.assertEqual(result['relative_status'], 'cache_scope_unavailable')
        self.assertIsNone(result['regression_percent'])
        self.assertTrue(result['passed'])

    def test_no_baseline_reports_absolute_only(self):
        status, result, _ = self.run_fixture('cold', 200)
        self.assertEqual(status, 0)
        self.assertTrue(result['absolute_passed'])
        self.assertEqual(result['relative_status'], 'not_requested')
        self.assertTrue(result['passed'])

    def test_heartbeat_baseline_requires_verified_write(self):
        status, result, _ = self.run_fixture('cold', 200, baseline={}, heartbeat=True)
        self.assertEqual(status, 1)
        self.assertTrue(result['absolute_passed'])
        self.assertEqual(result['relative_status'], 'unavailable')
        self.assertTrue(result['write_verified'])

    def test_valid_baseline_still_enforces_relative_regression(self):
        status, result, _ = self.run_fixture('cold', 200, baseline={'p95': 1}, elapsed=5)
        self.assertEqual(status, 1)
        self.assertTrue(result['absolute_passed'])
        self.assertEqual(result['relative_status'], 'failed')
        self.assertFalse(result['passed'])

    def test_invalid_current_response_has_no_relative_result(self):
        invalid = [(503, None), (200, b'{"data":{"sections":[{"error":{"code":"failed"}}]}}')]
        for code, body in invalid:
            with self.subTest(code=code, body=body):
                status, result, _ = self.run_fixture('cold', 200, contract='discover',
                                                     baseline={}, timed_code=code, timed_body=body)
                self.assertEqual(status, 1)
                self.assertFalse(result['absolute_passed'])
                self.assertEqual(result['relative_status'], 'unavailable')
                self.assertIsNone(result['regression_percent'])
                self.assertFalse(result['passed'])

    def test_incomplete_busy_baseline_samples_are_unavailable(self):
        status, result, _ = self.run_fixture('busy', 200, baseline={}, baseline_n=40)
        self.assertEqual(status, 1)
        self.assertTrue(result['absolute_passed'])
        self.assertEqual(result['relative_status'], 'unavailable')
        self.assertIsNone(result['regression_percent'])
        self.assertFalse(result['passed'])

    def run_fixture(self, mode, population_code, population_body=None, contract=None, baseline=None,
                    heartbeat=False, elapsed=1, timed_code=200, timed_body=None, baseline_n=1):
        events = []
        class Client:
            def __init__(self, *args):
                self.first = True
            def call(self, *args):
                return {'commit': 'fixture'}
            def raw(self, *args):
                events.append('request')
                code = population_code if self.first else timed_code
                duration = 1400 if self.first else elapsed
                body = (population_body if self.first else timed_body) or b'{"data":{"sections":[{"items":[]}]}}'
                self.first = False
                return code, duration, body
            def close(self):
                pass

        temporary_root = Path('/tmp/crawler-storage-fixes')
        temporary_root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary_root) as directory:
            root = Path(directory)
            case = {'id': 'catalog', 'surface': 'self', 'route': '/catalog',
                    'class': 'A', 'url': '/catalog', 'cache_read': True}
            if contract:
                case['response_contract'] = contract
            if heartbeat:
                case['prepare'] = 'heartbeat'
            (root / 'cases.json').write_text(json.dumps({'cases': [case], 'exemptions': []}))
            (root / 'manifest.json').write_text(json.dumps({'users': [{'id': 'user', 'login': 'user'}],
                                                            'libraries': {'tv': 'tv'}}))
            client = Client()
            args = ['perfgate', mode, '--n', '1', '--cases', str(root / 'cases.json'),
                    '--manifest', str(root / 'manifest.json'), '--out', str(root / 'report.json'),
                    '--restart-command', '/fake/restart']
            if baseline is not None:
                previous = {'p95': 10, 'n': 1, 'codes': {'200': 1}, 'errors': [],
                            'precondition_codes': {}, 'response_verified': True,
                            'write_verified': None, 'latency_scope': 'cached', 'passed': True}
                previous.update(baseline)
                (root / 'baseline.json').write_text(json.dumps(
                    {'complete': True, 'mode': mode, 'n': baseline_n, 'cases': {'catalog': previous}}))
                args.extend(['--baseline', str(root / 'baseline.json')])
            with patch('sys.argv', args), patch.object(perfgate, 'routes', return_value=[]), \
                    patch.object(perfgate, 'Client', Client), \
                    patch.object(perfgate, 'login', return_value=(client, {})), \
                    patch.object(perfgate, 'context', return_value={}), \
                    patch.object(perfgate, 'prepare_heartbeat'), \
                    patch.object(perfgate, 'verify_heartbeat', return_value=True), \
                    patch.object(perfgate, 'BusyScan') as busy, \
                    patch.object(perfgate.subprocess, 'run', side_effect=lambda *a, **kw: events.append('restart')):
                busy.return_value.running.return_value = True
                status = perfgate.main()
            result = json.loads((root / 'report.json').read_text())['cases']['catalog']
            return status, result, events


if __name__ == '__main__':
    unittest.main()
