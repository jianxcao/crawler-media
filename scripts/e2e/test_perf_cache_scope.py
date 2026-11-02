"""Cache latency is gated; cache population stays visible without a fast SLO."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
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

    def run_fixture(self, mode, population_code, population_body=None, contract=None):
        events = []
        class Client:
            def __init__(self, *args):
                self.first = True
            def call(self, *args):
                return {'commit': 'fixture'}
            def raw(self, *args):
                events.append('request')
                code = population_code if self.first else 200
                elapsed = 1400 if self.first else 1
                body = population_body if self.first and population_body else b'{"data":{"sections":[{"items":[]}]}}'
                self.first = False
                return code, elapsed, body
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
            (root / 'cases.json').write_text(json.dumps({'cases': [case], 'exemptions': []}))
            (root / 'manifest.json').write_text(json.dumps({'users': [{'id': 'user', 'login': 'user'}]}))
            client = Client()
            args = ['perfgate', mode, '--n', '1', '--cases', str(root / 'cases.json'),
                    '--manifest', str(root / 'manifest.json'), '--out', str(root / 'report.json'),
                    '--restart-command', '/fake/restart']
            with patch('sys.argv', args), patch.object(perfgate, 'routes', return_value=[]), \
                    patch.object(perfgate, 'Client', Client), \
                    patch.object(perfgate, 'login', return_value=(client, {})), \
                    patch.object(perfgate, 'context', return_value={}), \
                    patch.object(perfgate.subprocess, 'run', side_effect=lambda *a, **kw: events.append('restart')):
                status = perfgate.main()
            result = json.loads((root / 'report.json').read_text())['cases']['catalog']
            return status, result, events


if __name__ == '__main__':
    unittest.main()
