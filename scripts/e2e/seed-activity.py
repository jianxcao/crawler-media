#!/usr/bin/env python3
"""Seed the performance dataset entirely through public APIs; up to eight users at once."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import sys
import threading
import time

from perf_support import Client, ApiError, items, login, write_private_json


DURATION = 7_200_000


def targets(movies, series):
    # Exactly 300 units: 60 movies and four episodes from each of 60 TV Media.
    return ([{'media_item_id': item['media_item_id']} for item in movies[:60]] +
            [{'media_item_id': item['media_item_id'], 'season_number': 1, 'episode_number': episode}
             for item in series[:60] for episode in range(1, 5)])


def playback(client, target, device, completed, serial):
    session = client.call('/api/v1/playback/sessions', 'POST', {**target, 'device_id': device})
    common = {**target, 'file_id': session['file_id'], 'device_id': device,
              'play_session_id': session['play_session_id'], 'duration_ms': DURATION,
              'client': 'perfgate', 'device_name': 'Local performance fixture'}
    client.call('/api/v1/playback/progress', 'POST',
                {**common, 'event': 'start', 'event_sequence': serial * 2,
                 'position_ms': 0})
    client.call('/api/v1/playback/progress', 'POST',
                {**common, 'event': 'stop', 'event_sequence': serial * 2 + 1,
                 'position_ms': DURATION if completed else DURATION // 3})
    return session


def seed_user(client, user, movies, series, checkpoint_dir):
    checkpoint = checkpoint_dir / f'{user["id"]}.json'
    progress = json.loads(checkpoint.read_text()) if checkpoint.exists() else {}
    units = targets(movies, series)
    # 400 logs, all confined to those 300 units. The last 300 overwrite progress
    # to exactly 100 completed and 200 partial units, distributed over 60 TV Media.
    schedule = units[:100] + units
    completed_units = set(range(40)) | {60 + index * 4 for index in range(60)}
    for index in range(progress.get('logs', 0), 400):
        target_index = index if index < 100 else index - 100
        playback(client, schedule[index], f'perf-seed-{user["id"]}',
                 target_index in completed_units, index)
        progress['logs'] = index + 1
        write_private_json(checkpoint, progress)
    for index in range(progress.get('favorites', 0), 100):
        client.call('/api/v1/playback/marks', 'POST',
                    {'media_item_id': movies[index]['media_item_id'], 'favorite': True})
        progress['favorites'] = index + 1
        write_private_json(checkpoint, progress)
    collections = {entry['name']: entry['id'] for entry in client.call('/api/v1/collections')}
    collection_ids = []
    for collection_index in range(5):
        name = f'perf-{collection_index + 1}'
        collection_id = collections.get(name)
        if not collection_id:
            collection_id = client.call('/api/v1/collections', 'POST', {'name': name})['id']
        collection_ids.append(collection_id)
        key = f'collection-{collection_index}'
        for item_index in range(progress.get(key, 0), 50):
            client.call(f'/api/v1/collections/{collection_id}/items', 'POST',
                        {'media_item_id': movies[collection_index * 50 + item_index]['media_item_id']})
            progress[key] = item_index + 1
            write_private_json(checkpoint, progress)
    return {**user, 'collections': collection_ids}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base_url')
    parser.add_argument('admin_token', nargs='?', help='Prefer CRAWLER_MEDIA_TOKEN to avoid process-list exposure')
    parser.add_argument('--root', type=Path, default=Path('/tmp/e2e-stress'))
    parser.add_argument('--users', type=int, default=50)
    args = parser.parse_args()
    token = args.admin_token or os.environ.get('CRAWLER_MEDIA_TOKEN')
    if not token:
        parser.error('Set CRAWLER_MEDIA_TOKEN or supply admin_token')
    admin = Client(args.base_url, token)
    password = os.environ.get('PERF_USER_PASSWORD', 'local-perf-user-password')
    libraries = {entry['name']: entry for entry in admin.call('/api/v1/libraries')}
    movie_library, tv_library = libraries['stress-movie'], libraries['stress-tv']
    movies, series = items(admin, movie_library['id']), items(admin, tv_library['id'])
    if len(movies) < 3000 or len(series) < 300:
        raise RuntimeError(f'Incomplete dataset: {len(movies)} movie Media and {len(series)} TV Media')
    existing_users = {user['login']: user for user in admin.call('/api/v1/users')}
    users = []
    for index in range(args.users):
        name = f'perf-user-{index + 1:02d}'
        user = existing_users.get(name)
        if user is None:
            user = admin.call('/api/v1/users', 'POST', {'login': name, 'password': password})
        users.append({'id': user['id'], 'login': name})
    checkpoint_dir = args.root / 'seed-checkpoints'
    checkpoint_dir.mkdir(parents=True, exist_ok=True)
    # Password hashing has its own bounded admission: authenticate sequentially,
    # then parallelize only independent API writes. No retries hide overload.
    authenticated = []
    for user in users:
        client, actual_id = login(args.base_url, user['login'], password)
        if actual_id != user['id']:
            raise RuntimeError('Seed login resolved to a different User')
        authenticated.append((user, client))
    seeded = []
    with ThreadPoolExecutor(max_workers=8) as executor:
        futures = [executor.submit(seed_user, client, user, movies, series, checkpoint_dir)
                   for user, client in authenticated]
        for future in as_completed(futures):
            seeded.append(future.result())
            print(f'activity seeded: {len(seeded)}/{len(users)} users', flush=True)
    seeded.sort(key=lambda user: user['login'])
    subscriptions = admin.call('/api/v1/subscriptions')
    subscription = next((entry for entry in subscriptions if entry.get('library_id') == tv_library['id']), None)
    if subscription is None:
        entry = series[0]
        subscription = admin.call('/api/v1/subscriptions', 'POST', {
            'media': {'kind': 'tv', 'title': entry['title'], 'tmdb_id': '9500000'},
            'coverage': {'kind': 'tv', 'season': 1, 'episode_from': 1, 'episode_to': 70},
            'library_id': tv_library['id'], 'fetch_mode': 'search'})
    files = admin.call(f'/api/v1/libraries/{movie_library["id"]}/items/{movies[0]["media_item_id"]}/files')
    file_id = files['files'][0]['file_id']
    tv_files = admin.call(f'/api/v1/libraries/{tv_library["id"]}/items/{series[0]["media_item_id"]}/files')
    tv_file_id = tv_files['files'][0]['file_id']
    facets = admin.call(f'/api/v1/libraries/{movie_library["id"]}/facets')
    genre = facets['genres'][0]
    genre = genre.get('value', genre.get('name')) if isinstance(genre, dict) else genre
    resources = {'subscription': subscription['id'], 'movie_file': file_id, 'tv_file': tv_file_id,
                 'genre': genre, 'person': '演员001', 'catalog_movie': '9000000', 'catalog_tv': '9500000'}
    jobs = admin.call('/api/v1/jobs')
    jobs = jobs.get('items', []) if isinstance(jobs, dict) else jobs
    if jobs:
        resources['job'] = jobs[0]['id']
    filters = admin.call('/api/v1/rule-sets')
    if filters:
        resources['filter'] = filters[0]['id']
    manifest = {'libraries': {'movie': movie_library['id'], 'tv': tv_library['id']},
                'movies': movies, 'series': series, 'users': seeded, 'resources': resources,
                'spec': {'files': 24000, 'users': args.users, 'progress_per_user': 300,
                         'completed_per_user': 100, 'partial_per_user': 200,
                         'favorites_per_user': 100, 'collections_per_user': 5,
                         'items_per_collection': 50, 'logs_per_user': 400}}
    write_private_json(args.root / 'perf-dataset.json', manifest)
    print('Performance dataset manifest written (no credentials stored)', flush=True)


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, KeyError, ValueError) as error:
        print(f'seed-activity: {error}', file=sys.stderr)
        sys.exit(1)
