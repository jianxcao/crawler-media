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

    def test_relative_p95_under_one_millisecond_floor_passes(self):
        result = perfgate.evaluate([0.9] * 40, {200: 40}, 'L', 'idle', 0.4)
        self.assertGreater(result['regression_percent'], 20)
        self.assertFalse(result['regressed'])
        self.assertTrue(result['passed'])

    def test_relative_p95_at_or_above_floor_still_fails(self):
        under_floor = perfgate.evaluate([1.5] * 40, {200: 40}, 'L', 'idle', 0.5)
        self.assertEqual(under_floor['p95'] - 0.5, 1.0)
        self.assertGreater(under_floor['regression_percent'], 20)
        self.assertTrue(under_floor['regressed'])
        self.assertFalse(under_floor['passed'])
        above = perfgate.evaluate([1.5] * 40, {200: 40}, 'L', 'idle', 0.4)
        self.assertGreater(above['p95'] - 0.4, 1.0)
        self.assertTrue(above['regressed'])
        self.assertFalse(above['passed'])

    def test_absolute_slo_still_fails_when_relative_change_is_small(self):
        result = perfgate.evaluate([160.0] * 40, {200: 40}, 'L', 'idle', 159.0)
        self.assertLess(result['regression_percent'], 20)
        self.assertFalse(result['regressed'])
        self.assertFalse(result['passed'])

    def test_source_reads_prepare_cache_and_local_reads_do_not(self):
        import json
        cases = json.loads(Path(__file__).with_name('perf-cases.json').read_text())['cases']
        warmed = {case['id'] for case in cases if case.get('cache_read')}
        self.assertEqual(warmed, {
            'self-genres-kind',
            'self-libraries-routing-options',
            'self-settings-countries',
            'self-settings-languages',
            'self-media-tv-id-season-season',
            'self-discover-kind',
            'self-discover-kind-collection-id',
            'self-discover-kind-filtered',
            'self-media-douban-id',
            'self-media-person-id',
            'self-media-kind-id',
            'self-media-id-poster',
            'jellyfin-persons-name-images-primary',
            'self-search-titles',
        })
        genre = next(case for case in cases if case['id'] == 'self-genres-kind')
        health = next(case for case in cases if case['id'] == 'self-health')
        calls = []
        perfgate.prepare_cache(genre, lambda: calls.append('populate'))
        perfgate.prepare_cache(health, lambda: calls.append('populate'))
        self.assertEqual(calls, ['populate'])

    def test_raw_reuses_one_connection_and_retries_a_closed_socket(self):
        from perf_support import Client
        import socketserver
        from http.server import BaseHTTPRequestHandler

        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def setup(self):
                super().setup()
                self.connection.settimeout(2)

            def do_GET(self):
                if self.path == '/missing':
                    body, status = b'no', 404
                else:
                    body, status = b'ok', 200
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                if self.path == '/drop':
                    self.close_connection = True
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, fmt, *args):
                return

        class Server(socketserver.TCPServer):
            allow_reuse_address = True

            def __init__(self):
                self.accepts = 0
                super().__init__(('127.0.0.1', 0), Handler)

            def process_request(self, request, client_address):
                self.accepts += 1
                super().process_request(request, client_address)

        server = Server()
        client = Client('http://127.0.0.1:%d' % server.server_address[1], token='fixture-token')
        thread = __import__('threading').Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            first = client.raw('/ping')
            second = client.raw('/ping')
            self.assertEqual(first[0], 200)
            self.assertEqual(second[0], 200)
            self.assertEqual(first[2], b'ok')
            self.assertEqual(server.accepts, 1)
            missing = client.raw('/missing')
            self.assertEqual(missing[0], 404)
            self.assertEqual(missing[2], b'no')
            self.assertEqual(server.accepts, 1)
            closed = client.raw('/drop')
            self.assertEqual(closed[0], 200)
            again = client.raw('/ping')
            self.assertEqual(again[0], 200)
            self.assertGreaterEqual(server.accepts, 2)
        finally:
            client.close()
            server.shutdown()
            server.server_close()


class ColdConnectionTests(unittest.TestCase):
    def setUp(self):
        import socket
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def setup(self):
                super().setup()
                self.connection.settimeout(2)
                self.server.connections.add(self.connection)

            def do_GET(self):
                self.respond(b'{"ok":true,"data":{"commit":"fixture"}}')

            def do_POST(self):
                length = int(self.headers.get('Content-Length', '0'))
                self.rfile.read(length)
                self.server.writes.append(self.path)
                if self.path == '/drop-write':
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.close_connection = True
                    return
                self.respond(b'{"ok":true,"data":{"written":true}}')

            def respond(self, body):
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                return

        class Server(ThreadingHTTPServer):
            daemon_threads = True

            def __init__(self):
                self.accepts = 0
                self.connections = set()
                self.writes = []
                super().__init__(('127.0.0.1', 0), Handler)

            def process_request(self, request, client_address):
                self.accepts += 1
                super().process_request(request, client_address)

        self.server = Server()
        self.base = 'http://127.0.0.1:%d' % self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def test_client_close_discards_main_and_page_worker_connections(self):
        from concurrent.futures import ThreadPoolExecutor
        from perf_support import Client
        client = Client(self.base)
        with ThreadPoolExecutor(max_workers=1) as executor:
            self.assertEqual(client.raw('/before')[0], 200)
            self.assertEqual(executor.submit(client.raw, '/before').result()[0], 200)
            self.assertEqual(self.server.accepts, 2)
            client.close()
            try:
                self.assertEqual(client.raw('/after', 'POST', {})[0], 200)
                self.assertEqual(executor.submit(client.raw, '/after', 'POST', {}).result()[0], 200)
                self.assertEqual(self.server.accepts, 4,
                                 'planned restart must discard every thread socket')
                self.assertEqual(self.server.writes, ['/after', '/after'])
            finally:
                client.close()
                executor.submit(client.close).result()

    def test_cold_restart_keeps_each_write_on_a_fresh_connection_without_replay(self):
        import contextlib
        import io
        import json
        import socket
        import subprocess
        import sys
        import tempfile
        from unittest.mock import patch
        from perf_support import Client

        def restart(*args, **kwargs):
            for connection in list(self.server.connections):
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            return subprocess.CompletedProcess(args[0], 0)

        def login(base, username, password, jellyfin=False):
            return Client(base, token='fixture', jellyfin=jellyfin), 'user'

        with tempfile.TemporaryDirectory(dir='/tmp/crawler-storage-fixes') as directory:
            root = Path(directory)
            manifest = {'libraries': {'movie': 'movie', 'tv': 'tv'},
                        'movies': [{'media_item_id': 'movie'}] * 501,
                        'series': [{'media_item_id': 'series'}],
                        'users': [{'id': 'user', 'login': 'user', 'collections': ['collection']}]}
            config = {'exemptions': [], 'cases': [
                {'id': 'first-write', 'surface': 'self', 'class': 'W',
                 'url': '/first-write', 'method': 'POST', 'body': {}},
                {'id': 'second-write', 'surface': 'self', 'class': 'W',
                 'url': '/second-write', 'method': 'POST', 'body': {}}]}
            (root / 'manifest.json').write_text(json.dumps(manifest))
            (root / 'cases.json').write_text(json.dumps(config))
            argv = ['perfgate', 'cold', '--base-url', self.base,
                    '--manifest', str(root / 'manifest.json'),
                    '--cases', str(root / 'cases.json'), '--out', str(root / 'report.json'),
                    '--restart-command', 'fixture-restart']
            with patch.object(sys, 'argv', argv), patch.object(perfgate, 'routes', return_value=[]), \
                    patch.object(perfgate, 'login', side_effect=login), \
                    patch.object(perfgate.subprocess, 'run', side_effect=restart), \
                    contextlib.redirect_stdout(io.StringIO()):
                result = perfgate.main()
            report = json.loads((root / 'report.json').read_text())
            self.assertEqual(result, 0, report)
            self.assertTrue(report['passed'])
            self.assertEqual(self.server.writes, ['/first-write', '/second-write'],
                             'planned cold restarts must not lose or replay writes')

    def test_unexpected_write_disconnect_stays_a_failure_without_replay(self):
        from perf_support import Client
        client = Client(self.base)
        try:
            code, _, _ = client.raw('/drop-write', 'POST', {})
            self.assertEqual(code, 0)
            self.assertEqual(self.server.writes, ['/drop-write'])
        finally:
            client.close()


if __name__ == '__main__':
    unittest.main()
