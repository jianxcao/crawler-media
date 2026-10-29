#!/usr/bin/env python3
"""Prepare optional route fixtures on the isolated :18766 performance instance.

All database changes use public APIs. Credentials and upstream error bodies are
never printed. Unsupported resources remain explicit failed preconditions.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import time
import urllib.parse
import uuid

from perf_support import ApiError, Client, items, write_private_json


PREFIX = '/api/v1'


def validate_target(base):
    target = urllib.parse.urlsplit(base)
    if (target.scheme, target.hostname, target.port, target.path) != (
            'http', '127.0.0.1', 18766, '') or target.username or target.password:
        raise ValueError('resource preparation requires http://127.0.0.1:18766')


def ensure_named(client, path, name, body):
    for row in client.call(path):
        if row.get('name') == name or row.get('login') == name:
            return row
    return client.call(path, 'POST', {**body, 'login' if path.endswith('/users') else 'name': name})


def record_resource(manifest, key, prepare):
    try:
        value = prepare()
        if value is None or value == '':
            raise ValueError('resource unavailable')
        manifest.setdefault('resources', {})[key] = value
        manifest.setdefault('resource_failures', {}).pop(key, None)
    except (ApiError, OSError, ValueError, KeyError, IndexError, StopIteration) as error:
        manifest.setdefault('resources', {}).pop(key, None)
        reason = f'HTTP {error.status}' if isinstance(error, ApiError) else type(error).__name__
        manifest.setdefault('resource_failures', {})[key] = reason


def latest_history_entry(client):
    client.call(PREFIX + '/search/torrents?keyword=perfgate-fixture&save_history=true')
    entries = client.call(PREFIX + '/search/history')['items']
    return next(row['id'] for row in entries if row.get('query') == 'perfgate-fixture')


def catalog_choice(hits, provider, kind):
    return next(str(row['external_id']) for row in hits
                if row.get('provider') == provider and row.get('kind') == kind
                and row.get('external_id'))


def collection_identifier(response):
    if not response.get('items'):
        raise ValueError('wall section did not return catalog items')
    return response['collection_id']


def prepare_poster_media(client, manifest):
    """Create a real catalog-backed Media, pausing all fixture-owned jobs."""
    resources = manifest['resources']
    catalog_movie = resources['catalog_movie']
    subscription = next((row for row in client.call(PREFIX + '/subscriptions')
                         if row.get('id') == resources.get('poster_subscription')
                         or (row.get('library_id') is None
                             and row.get('media', {}).get('tmdb_id') == catalog_movie)), None)
    if subscription is None:
        subscription = client.call(PREFIX + '/subscriptions', 'POST', {
            'media': {'kind': 'movie', 'title': 'Perfgate Poster Fixture',
                      'tmdb_id': catalog_movie},
            'coverage': {'kind': 'movie'}, 'fetch_mode': 'search'})
    subscription_id = subscription['id']
    media_id = subscription['media']['id']
    # Creation always seeds active jobs; the create tracking_state field is
    # ignored by the server. Explicitly pause and disable both seeded jobs.
    client.call(f'{PREFIX}/subscriptions/{subscription_id}', 'PATCH',
                {'tracking_state': 'paused'})
    for job in client.call(PREFIX + '/jobs'):
        payload = job.get('payload', {})
        if (payload.get('subscribe_id') == subscription_id
                or payload.get('media_id') == media_id):
            client.call(f'{PREFIX}/jobs/{job["id"]}', 'PATCH', {'enabled': False})
    resources.update(poster_subscription=subscription_id, poster_media=media_id)
    return media_id


def prepare_catalog(client, manifest, api_key=None):
    """Opt-in real catalog IDs; neither credentials nor upstream bodies are logged."""
    if api_key:
        if not re.fullmatch(r'[A-Za-z0-9]+', api_key):
            raise ValueError('invalid catalog key format')
        client.call(PREFIX + '/settings/metadata', 'PUT', {'tmdb_api_key': api_key})
    hits = client.call(PREFIX + '/search/titles?keyword=Toy%20Story&provider=tmdb')['titles']
    record_resource(manifest, 'catalog_movie', lambda: catalog_choice(hits, 'tmdb', 'movie'))
    hits = client.call(PREFIX + '/search/titles?keyword=Game%20of%20Thrones&provider=tmdb')['titles']
    record_resource(manifest, 'catalog_tv', lambda: catalog_choice(hits, 'tmdb', 'tv'))
    movie = manifest['resources'].get('catalog_movie')
    if movie:
        detail = client.call(f'{PREFIX}/media/movie/{movie}')
        cast = detail.get('info', {}).get('cast', [])
        record_resource(manifest, 'catalog_person', lambda: next(
            str(row['tmdb_person_id']) for row in cast if row.get('tmdb_person_id')))
        record_resource(manifest, 'catalog_person_avatar', lambda: next(
            row['avatar_url'] for row in cast if row.get('avatar_url')))
        record_resource(manifest, 'catalog_person_name', lambda: next(
            row['name'] for row in cast if row.get('tmdb_person_id') and row.get('avatar_url')))
    hits = client.call(PREFIX + '/search/titles?keyword=Toy%20Story&provider=douban')['titles']
    record_resource(manifest, 'catalog_douban', lambda: catalog_choice(hits, 'douban', 'movie'))
    record_resource(manifest, 'catalog_collection', lambda: collection_identifier(client.call(
        PREFIX + '/discover/movie/collection/popular?source=tmdb')))


def operation_id(receipt):
    value = receipt.get('operation_id') or receipt.get('id')
    if not value:
        raise ValueError('operation receipt has no identifier')
    return value


def wait_operation(client, path, timeout=300):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = client.call(path)
        state = status.get('status') or status.get('state')
        if state in ('succeeded', 'completed', 'failed', 'cancelled'):
            return status
        time.sleep(.1)
    raise TimeoutError('fixture operation did not settle')


def safe_library(client, root, name):
    root.mkdir(parents=True, exist_ok=True)
    return ensure_named(client, PREFIX + '/libraries', name, {
        'kind': 'movie', 'root_paths': [str(root)], 'detect_intros': False,
        'enable_fingerprint': False, 'realtime_watch': False,
        'generate_thumbnails': False, 'extract_chapter_images': False,
        'exclude_from_home': True, 'auto_series_collections': False,
    })


def prepare_operations(client, manifest, root):
    resources = manifest['resources']
    if 'scan_operation' not in resources:
        library = manifest['libraries']['movie']
        receipt = client.call(f'{PREFIX}/libraries/{library}/scan', 'POST', {})
        scan = operation_id(receipt)
        resources.update(scan_library=library, scan_operation=scan)
        wait_operation(client, f'{PREFIX}/libraries/{library}/scan-operations/{scan}')
    if 'library_operation' not in resources:
        library = safe_library(client, root / 'operation', 'perfgate-operation')['id']
        receipt = client.call(f'{PREFIX}/libraries/{library}/integrity-checks', 'POST', {})
        operation = operation_id(receipt)
        resources.update(operation_library=library, library_operation=operation)
        wait_operation(client, f'{PREFIX}/libraries/{library}/operations/{operation}')
    if 'library_removal' not in resources:
        library = safe_library(client, root / 'removal', 'perfgate-removal')['id']
        receipt = client.call(f'{PREFIX}/libraries/{library}', 'DELETE')
        resources['library_removal'] = operation_id(receipt)
        wait_operation(client, f'{PREFIX}/library-removals/{resources["library_removal"]}')
    if 'user_removal' not in resources:
        user = ensure_named(client, PREFIX + '/users', 'perfgate-disposable', {
            'password': os.environ.get('PERF_USER_PASSWORD', 'local-perf-user-password')})
        receipt = client.call(f'{PREFIX}/users/{user["id"]}', 'DELETE')
        resources['user_removal'] = operation_id(receipt)
        wait_operation(client, f'{PREFIX}/users/removals/{resources["user_removal"]}')


def prepare_merge(client, manifest, root):
    """Merge only two newly generated fixture Media; never a stress Media."""
    resources = manifest['resources']
    existing = resources.get('media_operation')
    if existing:
        client.call(f'{PREFIX}/media/operations/{existing}')
        return existing
    merge_root = root / 'merge'
    merge_root.mkdir(parents=True, exist_ok=True)
    for suffix in ('A', 'B'):
        name = f'Perfgate Merge {suffix} (2001)'
        directory = merge_root / name
        directory.mkdir(exist_ok=True)
        (directory / f'{name}.strm').write_text('http://127.0.0.1:9/perfgate.mkv\n')
        (directory / f'{name}.nfo').write_text(
            f'<?xml version="1.0"?><movie><title>Perfgate Merge {suffix}</title>'
            '<year>2001</year><plot>Isolated route fixture</plot></movie>')
    library = safe_library(client, merge_root, 'perfgate-merge')['id']
    scan = operation_id(client.call(f'{PREFIX}/libraries/{library}/scan', 'POST', {}))
    wait_operation(client, f'{PREFIX}/libraries/{library}/scan-operations/{scan}')
    rows = items(client, library)
    if len(rows) != 2:
        raise ValueError('merge requires two independently indexed fixture Media')
    request = {'request_id': str(uuid.uuid5(uuid.NAMESPACE_URL, str(merge_root))),
               'survivor': rows[0]['media_item_id'], 'loser': rows[1]['media_item_id'],
               'survivor_revision': 0, 'loser_revision': 0}
    # The public merge surface uses optimistic revisions. A version conflict
    # reports its current revision; pin each of the two fixture revisions once.
    for _ in range(3):
        try:
            return operation_id(client.call(PREFIX + '/media/merges', 'POST', request))
        except ApiError as error:
            if error.status != 409:
                raise
            body = json.loads(error.body)
            message = body.get('error', {}).get('message', '')
            conflict = re.search(r'merge (survivor|loser).*revision mismatch: expected \d+, got (\d+)', message)
            if not conflict:
                raise
            request[conflict.group(1) + '_revision'] = int(conflict.group(2))
    raise ValueError('fixture revisions changed repeatedly')


def prepare(client, manifest, root):
    resources = manifest.setdefault('resources', {})
    record_resource(manifest, 'downloader', lambda: ensure_named(
        client, PREFIX + '/downloaders', 'perfgate-disabled-downloader', {
            'kind': 'qbittorrent', 'url': 'http://127.0.0.1:9',
            'enabled': False, 'is_default': False})['id'])

    def site():
        profiles = client.call(PREFIX + '/sites/catalog')
        profile = next(row['profile_id'] for row in profiles if row['profile_id'] != 'mteam')
        return ensure_named(client, PREFIX + '/sites', 'perfgate-disabled-site', {
            'url': 'http://127.0.0.1:9', 'profile_id': profile,
            'enabled': False, 'maintenance_via': 'http'})['id']
    record_resource(manifest, 'site', site)

    record_resource(manifest, 'history_entry', lambda: latest_history_entry(client))

    try:
        prepare_operations(client, manifest, root)
    except (ApiError, OSError, ValueError, KeyError, TimeoutError) as error:
        reason = f'HTTP {error.status}' if isinstance(error, ApiError) else type(error).__name__
        manifest.setdefault('resource_failures', {})['operations'] = reason
    record_resource(manifest, 'media_operation', lambda: prepare_merge(client, manifest, root))

    # People/Studio here are genuine NFO names, not guessed provider identifiers.
    studios = [row['studio'] for row in manifest['movies'] if row.get('studio')]
    if studios:
        resources['studio'] = studios[0]
    for key in ('catalog_douban', 'catalog_person', 'catalog_collection', 'studio'):
        if key not in resources:
            manifest.setdefault('resource_failures', {})[key] = 'No matching catalog identifier in API dataset'
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='http://127.0.0.1:18766')
    parser.add_argument('--manifest', type=Path, default=Path('/tmp/e2e-stress/perf-dataset.json'))
    parser.add_argument('--root', type=Path, default=Path('/tmp/e2e-stress/perf-resources'))
    args = parser.parse_args()
    validate_target(args.base)
    if not args.root.resolve().is_relative_to(Path('/tmp').resolve()):
        parser.error('fixture files must live under /tmp')
    token = os.environ.get('CRAWLER_MEDIA_TOKEN')
    if not token:
        parser.error('CRAWLER_MEDIA_TOKEN is required')
    manifest = json.loads(args.manifest.read_text())
    prepare(Client(args.base, token), manifest, args.root)
    if os.environ.get('PERF_TMDB_API_KEY'):
        prepare_catalog(Client(args.base, token), manifest, os.environ['PERF_TMDB_API_KEY'])
    if manifest['resources'].get('catalog_movie'):
        record_resource(manifest, 'poster_media', lambda: prepare_poster_media(
            Client(args.base, token), manifest))
    write_private_json(args.manifest, manifest)
    print('Prepared resource keys: ' + ', '.join(sorted(manifest['resources'])))
    for key, reason in manifest.get('resource_failures', {}).items():
        print(f'Unprepared {key}: {reason}', file=sys.stderr)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
