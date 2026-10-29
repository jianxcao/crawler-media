"""Offline checks for safely preparing optional performance resource fixtures."""
import importlib.util
from pathlib import Path
import sys
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location('prepare_perf_resources', HERE / 'prepare-perf-resources.py')
resources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resources)


class MemoryApi:
    def __init__(self, existing=()):
        self.rows = list(existing)

    def call(self, path, method='GET', body=None):
        if method == 'GET':
            return self.rows
        row = {**body, 'id': 'created-resource'}
        self.rows.append(row)
        return row


class PrepareResourcesTests(unittest.TestCase):
    def test_poster_resource_uses_canonical_media_and_disables_fixture_jobs(self):
        class PosterApi:
            def __init__(self):
                self.subscription = None
                self.jobs = [{'id': 'fixture-search', 'enabled': True,
                              'payload': {'subscribe_id': 'poster-subscription'}},
                             {'id': 'fixture-catalog', 'enabled': True,
                              'payload': {'media_id': 'canonical-media-uuid'}},
                             {'id': 'unrelated', 'enabled': True, 'payload': {}}]

            def call(self, path, method='GET', body=None):
                if path == '/api/v1/subscriptions':
                    if method == 'GET':
                        return [self.subscription] if self.subscription else []
                    if body['media']['tmdb_id'] != 'observed-real-catalog-id':
                        raise ValueError('poster fixture must retain its verified catalog ID')
                    self.subscription = {'id': 'poster-subscription',
                                         'media': {'id': 'canonical-media-uuid', **body['media']},
                                         'tracking_state': 'active'}
                    return self.subscription
                if path == '/api/v1/subscriptions/poster-subscription':
                    self.subscription.update(body)
                    return self.subscription
                if path == '/api/v1/jobs':
                    return self.jobs
                for job in self.jobs:
                    if path == '/api/v1/jobs/' + job['id']:
                        job.update(body)
                        return job
                raise ValueError('unexpected fixture API route')

        client = PosterApi()
        manifest = {'resources': {'catalog_movie': 'observed-real-catalog-id'}}
        self.assertTrue(callable(getattr(resources, 'prepare_poster_media', None)),
                        'resource preparation must provide a real poster Media')
        resources.prepare_poster_media(client, manifest)
        self.assertEqual(manifest['resources']['poster_media'], 'canonical-media-uuid')
        self.assertEqual(client.subscription['tracking_state'], 'paused')
        self.assertFalse(client.jobs[0]['enabled'])
        self.assertFalse(client.jobs[1]['enabled'])
        self.assertTrue(client.jobs[2]['enabled'])
        resources.prepare_poster_media(client, manifest)
        self.assertEqual(manifest['resources']['poster_media'], 'canonical-media-uuid')

    def test_existing_named_fixture_is_reused(self):
        client = MemoryApi([{'id': 'existing', 'name': 'perfgate-fixture', 'enabled': False}])
        result = resources.ensure_named(client, '/sites', 'perfgate-fixture', {'enabled': False})
        self.assertEqual(result['id'], 'existing')
        self.assertEqual(len(client.rows), 1)

    def test_new_fixture_preserves_disabled_configuration(self):
        client = MemoryApi()
        result = resources.ensure_named(client, '/downloaders', 'perfgate-fixture',
                                        {'url': 'http://127.0.0.1:9', 'enabled': False})
        self.assertFalse(result['enabled'])
        self.assertEqual(client.rows[0]['url'], 'http://127.0.0.1:9')

    def test_failed_preparation_does_not_fabricate_id(self):
        manifest = {'resources': {'job': 'real-job'}}
        def fail():
            raise resources.ApiError(404, '/missing', b'sensitive upstream response')
        resources.record_resource(manifest, 'media_operation', fail)
        self.assertNotIn('media_operation', manifest['resources'])
        self.assertEqual(manifest['resources']['job'], 'real-job')
        self.assertEqual(manifest['resource_failures']['media_operation'], 'HTTP 404')

    def test_successful_preparation_records_server_issued_id(self):
        manifest = {'resources': {}}
        resources.record_resource(manifest, 'library_removal', lambda: 'server-operation')
        self.assertEqual(manifest['resources']['library_removal'], 'server-operation')

    def test_refuses_nonisolated_server(self):
        for target in ['https://example.com', 'http://127.0.0.1:18765', 'http://localhost:18766']:
            with self.subTest(target=target), self.assertRaises(ValueError):
                resources.validate_target(target)
        resources.validate_target('http://127.0.0.1:18766')

    def test_history_resource_reads_items_envelope(self):
        class HistoryApi:
            def call(self, path):
                if path.endswith('/search/history'):
                    return {'items': [{'id': 'real-snapshot', 'query': 'perfgate-fixture'}]}
                if 'save_history=true' not in path:
                    raise ValueError('the API requires explicit history opt-in')
                return {'items': []}
        self.assertEqual(resources.latest_history_entry(HistoryApi()), 'real-snapshot')

    def test_missing_optional_value_is_recorded_without_aborting(self):
        manifest = {'resources': {}}
        resources.record_resource(manifest, 'catalog_person', lambda: next(iter([])))
        self.assertNotIn('catalog_person', manifest['resources'])
        self.assertEqual(manifest['resource_failures']['catalog_person'], 'StopIteration')

    def test_catalog_choice_requires_observed_provider_and_kind(self):
        hits = [{'provider': 'douban', 'kind': 'movie', 'external_id': 'douban-real'},
                {'provider': 'tmdb', 'kind': 'tv', 'external_id': 'tv-real'},
                {'provider': 'tmdb', 'kind': 'movie', 'external_id': 'movie-real'}]
        self.assertEqual(resources.catalog_choice(hits, 'tmdb', 'movie'), 'movie-real')
        with self.assertRaises(StopIteration):
            resources.catalog_choice(hits, 'anilist', 'movie')

    def test_collection_resource_uses_verified_wall_section_identifier(self):
        self.assertEqual(resources.collection_identifier({
            'collection_id': 'popular', 'items': [{'id': 'real-item'}]}), 'popular')
        with self.assertRaises(ValueError):
            resources.collection_identifier({'collection_id': 'popular', 'items': []})

    def test_catalog_avatar_preserves_local_person_name(self):
        class CatalogApi:
            def call(self, path):
                if '/search/titles' in path:
                    return {'titles': [{'provider': 'tmdb', 'kind': 'movie', 'external_id': '1'},
                                       {'provider': 'tmdb', 'kind': 'tv', 'external_id': '2'}]}
                if '/collection/' in path:
                    return {'collection_id': 'popular', 'items': [{'id': 'real'}]}
                return {'info': {'cast': [{'tmdb_person_id': '3', 'name': 'Observed Person',
                                          'avatar_url': 'https://images/avatar.jpg'}]}}
        manifest = {'resources': {'image_person': 'Local Person'}}
        resources.prepare_catalog(CatalogApi(), manifest)
        self.assertEqual(manifest['resources']['image_person'], 'Local Person')
        self.assertEqual(manifest['resources']['catalog_person_avatar'], 'https://images/avatar.jpg')
        self.assertEqual(manifest['resources']['catalog_person_name'], 'Observed Person')


if __name__ == '__main__':
    unittest.main()
