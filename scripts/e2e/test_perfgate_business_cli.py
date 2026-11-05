"""Public CLI contracts for independently supplied business expectations.

These tests require the future business expectation reader. Run only after the
owner authorizes ending the quiet period; no production implementation here.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


HERE = Path(__file__).resolve().parent
MOVIE = 'w1_00000000000000000000000000000001'
OTHER = 'w1_00000000000000000000000000000002'
COUNTS = {'MovieCount': 1, 'SeriesCount': 0, 'EpisodeCount': 0, 'ItemCount': 1}
ZERO = {key: 0 for key in COUNTS}
GENRE = {'Id': str(uuid.uuid5(uuid.NAMESPACE_DNS, 'Drama')),
         'Name': 'Drama', 'Type': 'Genre', 'ChildCount': 1}
STUDIO = {'Id': str(uuid.uuid5(uuid.NAMESPACE_DNS, 'Studio')),
          'Name': 'Studio', 'Type': 'Studio', 'ChildCount': 1}
PRODUCT_COMMIT = 'a' * 40
PRODUCT_BINARY_SHA256 = 'b' * 64
BASELINE_FACTS_SHA256 = 'c' * 64


class BusinessCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.responses = {}
        self.requests = []
        self.hidden_views = set()
        self.fail_login = False
        self.fail_health = False
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def reply(self, value):
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                if outer.fail_login:
                    self.send_error(401)
                    return
                if self.path == '/api/v1/auth/login':
                    user = data['username']
                    self.reply({'ok': True, 'data': {'token': user, 'user': {'id': user}}})
                elif self.path == '/Users/AuthenticateByName':
                    user = data['Username']
                    self.reply({'AccessToken': user, 'User': {'Id': user}})
                else:
                    self.send_error(404)

            def do_GET(self):
                if self.path == '/api/v1/health':
                    if outer.fail_health:
                        self.send_error(503)
                        return
                    self.reply({'ok': True, 'data': {'commit': PRODUCT_COMMIT[:12]}})
                    return
                if self.path in ('/Users/alice/Views?IncludeHidden=true', '/Users/bob/Views?IncludeHidden=true'):
                    user = self.path.split('/')[2]
                    items = [] if user in outer.hidden_views else [{'Id': 'movies'}]
                    self.reply({'Items': items, 'TotalRecordCount': len(items)})
                    return
                auth = self.headers.get('Authorization', '')
                user = 'alice' if 'alice' in auth else 'bob' if 'bob' in auth else ''
                path = urlsplit(self.path).path
                outer.requests.append((user, path))
                if (user, path) not in outer.responses:
                    self.send_error(404)
                    return
                self.reply(outer.responses[user, path])

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = 'http://127.0.0.1:%d' % self.server.server_address[1]
        source_manifest = self.root / 'source-manifest.json'
        source_tree = self.root / 'source-tree.json'
        source_manifest.write_text('{"source":"fixture"}')
        source_tree.write_text('{"tree":"fixture"}')
        self.manifest = {
            'product': {'commit': PRODUCT_COMMIT, 'binary_sha256': PRODUCT_BINARY_SHA256},
            'source_snapshot': {'baseline_facts_sha256': BASELINE_FACTS_SHA256,
                                'source_manifest_path': str(source_manifest),
                                'source_manifest_sha256': hashlib.sha256(source_manifest.read_bytes()).hexdigest(),
                                'source_tree_manifest_path': str(source_tree),
                                'source_tree_manifest_sha256': hashlib.sha256(source_tree.read_bytes()).hexdigest()},
            'libraries': {'movie': 'movies', 'tv': 'tv'},
            'movies': [{'media_item_id': '00000000-0000-0000-0000-000000000001'}] * 501,
            'series': [{'media_item_id': '00000000-0000-0000-0000-000000000003'}],
            'users': [{'id': user, 'login': user, 'collections': ['collection']}
                      for user in ('alice', 'bob')],
        }
        tool_names = ('perfgate.py', 'perf_business.py', 'perf_business_oracle.py', 'perf_support.py',
                      'perf_similar.py', 'list-routes.py')
        mapping = {name: hashlib.sha256((HERE / name).read_bytes()).hexdigest() for name in tool_names}
        self.manifest['tool_snapshot_sha256'] = hashlib.sha256(
            json.dumps(mapping, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        self.expectations = {
            'version': 1, 'fixture_commit': PRODUCT_COMMIT,
            'users': {
                'alice': {'counts': COUNTS, 'movie_libraries': {'movies': [MOVIE]},
                          'genres': {'Drama': 1}, 'studios': {'Studio': 1}},
                'bob': {'counts': ZERO, 'movie_libraries': {'movies': []},
                        'genres': {}, 'studios': {}},
            },
        }
        for user in self.expectations['users'].values():
            for kind in ('counts', 'genres', 'studios'):
                user[kind] = {'status': 'verified', 'value': user[kind]}
            user['movie_libraries'] = {key: {'status': 'verified', 'value': value}
                                       for key, value in user['movie_libraries'].items()}

    def invoke(self, case_id, responses, *, stale=False, n=1, expect_http=True,
               baseline=None, existing_output=False, provenance_remove=None,
               provenance_override=None, mode='idle', case_update=None):
        self.responses = responses
        self.requests.clear()
        run_root = Path(tempfile.mkdtemp(dir=self.root))
        original = json.loads((HERE / 'perf-cases.json').read_text())
        case = next(case for case in original['cases'] if case['id'] == case_id)
        if case_update is not None:
            case.update(case_update)
        routes = json.loads(subprocess.run(
            [sys.executable, str(HERE / 'list-routes.py')], check=True,
            capture_output=True, text=True).stdout)
        # All other routes are exempt only in this isolated CLI invocation.
        config = {'cases': [case], 'exemptions': [
            {'surface': row['surface'], 'route': row['path']} for row in routes
            if row['method'] == 'get']}
        expected_path = run_root / 'expectations.json'
        canonical_manifest = {key: value for key, value in self.manifest.items()
                              if key != 'business_expectations'}
        canonical_bytes = json.dumps(canonical_manifest, sort_keys=True,
                                     separators=(',', ':'), ensure_ascii=False).encode()
        self.expectations['provenance'] = {
            'manifest_sha256': hashlib.sha256(canonical_bytes).hexdigest(),
            'product_commit': PRODUCT_COMMIT, 'binary_sha256': PRODUCT_BINARY_SHA256,
            'baseline_facts_sha256': BASELINE_FACTS_SHA256, 'oracle_version': 1,
            'contract_version': 1,
            'source_manifest_sha256': self.manifest['source_snapshot']['source_manifest_sha256'],
            'source_tree_manifest_sha256': self.manifest['source_snapshot']['source_tree_manifest_sha256'],
        }
        self.expectations['provenance']['tool_snapshot_sha256'] = self.manifest['tool_snapshot_sha256']
        if provenance_remove:
            self.expectations['provenance'].pop(provenance_remove, None)
        if provenance_override:
            self.expectations['provenance'].update(provenance_override)
        expected_bytes = json.dumps(self.expectations, sort_keys=True).encode()
        expected_path.write_bytes(expected_bytes)
        digest = hashlib.sha256(expected_bytes).hexdigest()
        self.manifest['business_expectations'] = {
            'version': 1, 'path': str(expected_path),
            'sha256': '0' * 64 if stale else digest,
        }
        for name, value in [('manifest', self.manifest), ('cases', config)]:
            (run_root / (name + '.json')).write_text(json.dumps(value))
        report_path = run_root / 'report.json'
        if existing_output:
            report_path.write_text('{"sentinel": true}')
        env = dict(os.environ, PERF_USER_PASSWORD='fixture', CRAWLER_MEDIA_TOKEN='fixture')
        command = [
            sys.executable, str(HERE / 'perfgate.py'), mode, '--n', str(n),
            '--only', case_id, '--base-url', self.base,
            '--manifest', str(run_root / 'manifest.json'),
            '--cases', str(run_root / 'cases.json'), '--out', str(report_path),
        ]
        if baseline is not None:
            baseline_path = run_root / 'baseline.json'
            baseline_path.write_text(json.dumps(baseline))
            command.extend(['--baseline', str(baseline_path)])
        result = subprocess.run(command, capture_output=True, text=True, env=env, timeout=30)
        self.assertTrue(report_path.exists(), result.stdout + result.stderr)
        report = json.loads(report_path.read_text())
        if existing_output:
            return result, report
        if expect_http:
            self.assertTrue(report['complete'], report)
            self.assertEqual(report['cases'][case_id]['codes'], {'200': n}, report)
            self.assertEqual(len(self.requests), n, self.requests)
            expected_requests = [(user, path) for user, path in responses]
            self.assertEqual(self.requests, expected_requests[:n])
        return result, report

    def assert_rejected(self, case, responses, **kwargs):
        result, report = self.invoke(case, responses, **kwargs)
        row = report['cases'][case]
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertFalse(row['response_verified'], row)
        self.assertFalse(row['passed'], row)
        self.assertFalse(row['absolute_passed'], row)

    def assert_accepted(self, case, responses, **kwargs):
        result, report = self.invoke(case, responses, **kwargs)
        row = report['cases'][case]
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(row['response_verified'], row)
        self.assertTrue(row['passed'], row)
        self.assertTrue(row['absolute_passed'], row)

    def test_genres_rejects_empty_when_independent_expectation_is_nonempty(self):
        self.assert_rejected('jellyfin-genres', {('alice', '/Genres'): {'Items': [], 'TotalRecordCount': 0}})

    def test_studios_rejects_empty_when_independent_expectation_is_nonempty(self):
        self.assert_rejected('jellyfin-studios', {('alice', '/Studios'): {'Items': [], 'TotalRecordCount': 0}})

    def test_studios_accepts_nonempty_and_proven_empty_for_two_users(self):
        self.assert_accepted('jellyfin-studios', {
            ('alice', '/Studios'): {'Items': [STUDIO], 'TotalRecordCount': 1},
            ('bob', '/Studios'): {'Items': [], 'TotalRecordCount': 0}}, n=2)
        self.assertEqual({user for user, _ in self.requests}, {'alice', 'bob'})

    def test_genres_accepts_matching_category(self):
        self.assert_accepted('jellyfin-genres', {('alice', '/Genres'): {'Items': [GENRE], 'TotalRecordCount': 1}})

    def test_counts_rejects_zero_for_nonempty_fixture(self):
        self.assert_rejected('jellyfin-items-counts', {('alice', '/Items/Counts'): ZERO})

    def test_counts_rejects_malformed_and_boolean_values(self):
        for body in ({'MovieCount': 1}, dict(COUNTS, MovieCount=True)):
            with self.subTest(body=body):
                self.assert_rejected('jellyfin-items-counts', {('alice', '/Items/Counts'): body})

    def test_user_counts_checks_each_users_own_expectation(self):
        self.assert_rejected('jellyfin-users-user-id-items-counts', {
            ('alice', '/Users/alice/Items/Counts'): COUNTS,
            ('bob', '/Users/bob/Items/Counts'): COUNTS}, n=2)

    def test_counts_accepts_real_zero_for_second_user(self):
        self.assert_accepted('jellyfin-users-user-id-items-counts', {
            ('alice', '/Users/alice/Items/Counts'): COUNTS,
            ('bob', '/Users/bob/Items/Counts'): ZERO}, n=2)

    def test_items_rejects_wrong_library_and_duplicate(self):
        for items in ([{'Id': OTHER, 'Type': 'Movie'}],
                      [{'Id': MOVIE, 'Type': 'Movie'}, {'Id': MOVIE, 'Type': 'Movie'}], []):
            with self.subTest(items=items):
                self.assert_rejected('jellyfin-users-user-id-items', {
                    ('alice', '/Users/alice/Items'): {'Items': items, 'TotalRecordCount': 1}})

    def test_items_rejects_duplicate_when_page_size_and_membership_match(self):
        self.expectations['users']['alice']['movie_libraries']['movies']['value'] = [MOVIE, OTHER]
        self.expectations['users']['alice']['counts']['value'] = dict(COUNTS, MovieCount=2, ItemCount=2)
        self.assert_rejected('jellyfin-users-user-id-items', {
            ('alice', '/Users/alice/Items'): {
                'Items': [{'Id': MOVIE, 'Type': 'Movie'}, {'Id': MOVIE, 'Type': 'Movie'}],
                'TotalRecordCount': 2}})

    def test_items_accepts_matching_work(self):
        self.assert_accepted('jellyfin-users-user-id-items', {
            ('alice', '/Users/alice/Items'): {'Items': [{'Id': MOVIE, 'Type': 'Movie'}], 'TotalRecordCount': 1}})

    def test_items_rejects_wrong_total(self):
        self.assert_rejected('jellyfin-users-user-id-items', {
            ('alice', '/Users/alice/Items'): {
                'Items': [{'Id': MOVIE, 'Type': 'Movie'}], 'TotalRecordCount': 2}})

    def test_latest_rejects_object_shape_and_empty_array(self):
        for body in ({'Items': [{'Id': MOVIE, 'Type': 'Movie'}]}, []):
            with self.subTest(body=body):
                self.assert_rejected('jellyfin-users-user-id-items-latest', {
                    ('alice', '/Users/alice/Items/Latest'): body})

    def test_latest_accepts_bare_array(self):
        self.assert_accepted('jellyfin-users-user-id-items-latest', {
            ('alice', '/Users/alice/Items/Latest'): [{'Id': MOVIE, 'Type': 'Movie'}]})

    def test_latest_rejects_duplicate_when_page_size_and_membership_match(self):
        self.expectations['users']['alice']['movie_libraries']['movies']['value'] = [MOVIE, OTHER]
        self.expectations['users']['alice']['counts']['value'] = dict(COUNTS, MovieCount=2, ItemCount=2)
        self.assert_rejected('jellyfin-users-user-id-items-latest', {
            ('alice', '/Users/alice/Items/Latest'): [
                {'Id': MOVIE, 'Type': 'Movie'}, {'Id': MOVIE, 'Type': 'Movie'}]})

    def test_stale_expectation_is_not_verified(self):
        result, report = self.invoke('jellyfin-items-counts',
                                     {('alice', '/Items/Counts'): COUNTS},
                                     stale=True, expect_http=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(report['passed'], report)
        self.assertEqual(report['business_status'], 'unverified', report)

    def test_missing_user_expectation_is_not_verified(self):
        del self.expectations['users']['alice']
        result, report = self.invoke('jellyfin-items-counts',
                                     {('alice', '/Items/Counts'): COUNTS},
                                     expect_http=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(report['passed'], report)

    def test_unknown_category_is_not_treated_as_empty(self):
        self.expectations['users']['alice']['studios'] = {
            'status': 'unverified', 'reason': 'metadata ambiguous'}
        result, report = self.invoke('jellyfin-studios',
                                     {('alice', '/Studios'): {'Items': [], 'TotalRecordCount': 0}},
                                     expect_http=False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(report['business_status'], 'unverified')

    def test_unrelated_unknown_category_does_not_block_counts(self):
        self.expectations['users']['alice']['studios'] = {
            'status': 'unverified', 'reason': 'metadata ambiguous'}
        self.assert_accepted('jellyfin-items-counts', {('alice', '/Items/Counts'): COUNTS})

    def test_baseline_without_business_provenance_is_not_admitted(self):
        responses = {('alice', '/Items/Counts'): COUNTS}
        _, baseline = self.invoke('jellyfin-items-counts', responses)
        baseline.pop('business_provenance', None)
        result, report = self.invoke('jellyfin-items-counts', responses, baseline=baseline)
        row = report['cases']['jellyfin-items-counts']
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(row['passed'], row)
        self.assertEqual(row['relative_status'], 'unavailable', row)

    def test_independently_bound_baseline_is_admitted(self):
        responses = {('alice', '/Items/Counts'): COUNTS}
        _, baseline = self.invoke('jellyfin-items-counts', responses)
        _, report = self.invoke('jellyfin-items-counts', responses, baseline=baseline)
        row = report['cases']['jellyfin-items-counts']
        self.assertIn(row['relative_status'], ('passed', 'failed'), row)

    def test_baseline_rejects_changed_case_configuration(self):
        responses = {('alice', '/Items/Counts'): COUNTS}
        _, baseline = self.invoke('jellyfin-items-counts', responses)
        for changed in ({'class': 'D'}, {'cache_read': True}):
            with self.subTest(changed=changed):
                # A cache-read case makes an untimed warmup request as well.
                _, report = self.invoke('jellyfin-items-counts', responses, baseline=baseline,
                                        case_update=changed, expect_http=False)
                row = report['cases']['jellyfin-items-counts']
                self.assertTrue(report['complete'], report)
                self.assertEqual(row['codes'], {'200': 1}, row)
                self.assertTrue(row['response_verified'], row)
                self.assertFalse(row['passed'], row)
                self.assertEqual(row['relative_status'], 'unavailable', row)
                self.assertIn('case', row['baseline_note'].lower())

    def test_existing_output_is_never_overwritten(self):
        result, report = self.invoke('jellyfin-items-counts', {}, expect_http=False,
                                     existing_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report, {'sentinel': True})

    def test_login_failure_writes_unverified_report(self):
        self.fail_login = True
        result, report = self.invoke('jellyfin-items-counts', {}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')
        self.assertFalse(report['passed'])

    def test_health_failure_writes_unverified_report(self):
        self.fail_health = True
        result, report = self.invoke('jellyfin-items-counts', {}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_missing_busy_library_writes_unverified_report(self):
        del self.manifest['libraries']['tv']
        result, report = self.invoke('jellyfin-items-counts', {}, mode='busy', expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_invalid_context_writes_unverified_report(self):
        self.manifest['users'][0]['collections'] = []
        result, report = self.invoke('jellyfin-items-counts', {}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_incomplete_baseline_writes_unverified_report(self):
        result, report = self.invoke('jellyfin-items-counts', {}, baseline={}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_root_counts_allows_user_without_movie_library_access(self):
        self.hidden_views.add('bob')
        self.assert_accepted('jellyfin-items-counts', {
            ('alice', '/Items/Counts'): COUNTS, ('bob', '/Items/Counts'): ZERO}, n=2)

    def test_items_rejects_sampled_user_without_movie_library_access(self):
        self.hidden_views.add('alice')
        result, report = self.invoke('jellyfin-users-user-id-items', {}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_changed_baseline_tool_identity_is_rejected(self):
        responses = {('alice', '/Items/Counts'): COUNTS}
        _, baseline = self.invoke('jellyfin-items-counts', responses)
        baseline['business_provenance']['tool_snapshot_sha256'] = '0' * 64
        result, report = self.invoke('jellyfin-items-counts', responses, baseline=baseline)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['cases']['jellyfin-items-counts']['relative_status'], 'unavailable')

    def test_missing_source_and_tool_provenance_is_unverified(self):
        for field in ('source_manifest_sha256', 'source_tree_manifest_sha256', 'tool_snapshot_sha256'):
            with self.subTest(field=field):
                result, report = self.invoke('jellyfin-items-counts',
                                             {('alice', '/Items/Counts'): COUNTS},
                                             provenance_remove=field, expect_http=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(report['business_status'], 'unverified')

    def test_wrong_actual_tool_hash_is_unverified(self):
        result, report = self.invoke('jellyfin-items-counts',
                                     {('alice', '/Items/Counts'): COUNTS},
                                     provenance_override={'tool_snapshot_sha256': '0' * 64},
                                     expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')

    def test_mutated_source_manifest_is_unverified(self):
        Path(self.manifest['source_snapshot']['source_manifest_path']).write_text('{"changed":true}')
        result, report = self.invoke('jellyfin-items-counts', {}, expect_http=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['business_status'], 'unverified')


if __name__ == '__main__':
    unittest.main()
