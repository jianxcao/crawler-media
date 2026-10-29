#!/usr/bin/env python3
"""Create finite local-media fixtures for the isolated performance instance.

Database changes use public API only. Generated video/artwork/font/subtitle files
live under /tmp and never become repository fixtures.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.parse
from xml.sax.saxutils import escape

from perf_support import Client, items, write_private_json


PREFIX = '/api/v1'


def run(command):
    subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def inspect_video(path):
    return json.loads(subprocess.check_output([
        'ffprobe', '-v', 'error', '-show_streams', '-show_chapters', '-of', 'json', str(path)]))


def fixture_font():
    configured = os.environ.get('PERF_FONT_PATH')
    candidates = ([Path(configured)] if configured else []) + [
        Path('/System/Library/Fonts/SFArabicRounded.ttf'),
        Path('/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf')]
    for path in candidates:
        if path.is_file():
            return path
    raise ValueError('set PERF_FONT_PATH to an existing local TTF file')


def prepare_tree(root, actor=None):
    root.mkdir(parents=True, exist_ok=True)
    video = root / 'Perfgate Local Fixture (2001).mp4'
    if not video.exists():
        metadata = root / 'fixture.ffmetadata'
        metadata.write_text(';FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=2000\ntitle=Fixture chapter\n')
        try:
            run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
                 '-f', 'lavfi', '-i', 'color=c=blue:s=64x64:r=1:d=2',
                 '-f', 'lavfi', '-i', 'anullsrc=r=8000:cl=mono', '-f', 'ffmetadata', '-i', str(metadata),
                 '-map', '0:v', '-map', '1:a', '-map_metadata', '2', '-map_chapters', '2',
                 '-t', '2', '-c:v', 'mpeg4', '-threads', '1', '-c:a', 'aac', str(video)])
        finally:
            metadata.unlink(missing_ok=True)
    person = (f'<actor><name>{escape(actor["name"])}</name>'
              f'<thumb>{escape(actor["avatar_url"])}</thumb></actor>' if actor else '')
    video.with_suffix('.nfo').write_text(
        '<?xml version="1.0"?><movie><title>Perfgate local fixture</title>'
        '<year>2001</year><plot>Finite delivery route fixture</plot>'
        '<genre>Perfgate</genre><studio>Perfgate Studio</studio>' + person + '</movie>')
    video.with_suffix('.en.srt').write_text(
        '1\n00:00:00,000 --> 00:00:00,900\nPerformance fixture\n\n')
    fonts = video.with_name(video.stem + '.fonts')
    fonts.mkdir(exist_ok=True)
    shutil.copyfile(fixture_font(), fonts / 'fixture.ttf')
    image = video.with_name(video.stem + '.trickplay.jpg')
    if not image.exists():
        run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', str(video),
             '-frames:v', '1', '-threads', '1', str(image)])
    for suffix in ('-chapter_0.jpg', '-poster.jpg', '-fanart.jpg', '-still.jpg'):
        shutil.copyfile(image, video.with_name(video.stem + suffix))
    for name in ('poster.jpg', 'fanart.jpg'):
        shutil.copyfile(image, root / name)
    return video


def subtitle_resources(file_id, item_id, session):
    tracks = session['decision']['subtitles']
    external = next((track for track in tracks if track.get('is_external')), None)
    if external is None:
        raise ValueError('local fixture subtitle has not been advertised by probe')
    return {'subtitle_file': file_id, 'subtitle_item': item_id,
            'subtitle_index': external['index'], 'subtitle_codec': 'vtt'}


def prepare(client, manifest_path, root):
    initial = json.loads(manifest_path.read_text()).get('resources', {})
    actor = ({'name': initial['catalog_person_name'], 'avatar_url': initial['catalog_person_avatar']}
             if initial.get('catalog_person_name') and initial.get('catalog_person_avatar') else None)
    prepare_tree(root, actor)
    libraries = client.call(PREFIX + '/libraries')
    library = next((row for row in libraries if row['name'] == 'perfgate-local-media'), None)
    if library is None:
        library = client.call(PREFIX + '/libraries', 'POST', {
            'name': 'perfgate-local-media', 'kind': 'movie', 'root_paths': [str(root)],
            'detect_intros': False, 'enable_fingerprint': False, 'realtime_watch': False,
            'generate_thumbnails': False, 'extract_chapter_images': False,
            'exclude_from_home': True, 'auto_series_collections': False})
    library_id = library['id']
    scan = client.call(f'{PREFIX}/libraries/{library_id}/scan', 'POST', {})['operation_id']
    deadline = time.monotonic() + 300
    while time.monotonic() < deadline:
        status = client.call(f'{PREFIX}/libraries/{library_id}/scan-operations/{scan}')
        if status['status'] in ('completed', 'succeeded'):
            break
        if status['status'] in ('failed', 'cancelled'):
            raise ValueError('local fixture scan did not succeed')
        time.sleep(.2)
    else:
        raise TimeoutError('local fixture scan did not finish')
    rows = items(client, library_id)
    row = next(row for row in rows if row['title'] == 'Perfgate local fixture')
    media_id = row['media_item_id']
    files = client.call(f'{PREFIX}/libraries/{library_id}/items/{media_id}/files')['files']
    file_id = files[0]['file_id']
    item_id = 'w1_' + media_id.replace('-', '')
    resources = {'local_file': item_id, 'chapter_file': file_id,
                 'local_library': library_id, 'local_media': media_id,
                 'image_genre': 'Perfgate', 'image_studio': 'Perfgate Studio'}
    if actor:
        resources['image_person'] = actor['name']
        resources['studio'] = 'Perfgate Studio'
    selection = {'media_item_id': media_id, 'file_id': file_id,
                 'device_id': 'perfgate-media-fixture'}
    session = client.call(PREFIX + '/playback/sessions', 'POST', selection)
    # Mandatory metadata work is asynchronous; inspect its actual advertised
    # tracks before requesting the final session, never assume a subtitle index.
    deadline = time.monotonic() + 60
    while not session['decision']['subtitles'] and time.monotonic() < deadline:
        files = client.call(f'{PREFIX}/libraries/{library_id}/items/{media_id}/files')['files']
        if files[0]['subtitle_tracks']:
            session = client.call(PREFIX + '/playback/sessions', 'POST', selection)
            break
        if files[0]['probed'] and not files[0]['probe_queued']:
            break
        time.sleep(.2)
    try:
        resources.update(subtitle_resources(file_id, item_id, session))
        subtitle_error = None
    except ValueError:
        subtitle_error = 'No advertised external subtitle track; mandatory metadata probe may be queued'
    # Read immediately before writing to preserve other preparation helpers' keys.
    manifest = json.loads(manifest_path.read_text())
    manifest.setdefault('resources', {}).update(resources)
    failures = manifest.setdefault('resource_failures', {})
    if actor:
        for key in ('image_person', 'studio'):
            failures.pop(key, None)
    if subtitle_error:
        failures['subtitle_file'] = subtitle_error
    else:
        for key in ('subtitle_file', 'subtitle_item', 'subtitle_index', 'subtitle_codec'):
            failures.pop(key, None)
    write_private_json(manifest_path, manifest)
    return resources, subtitle_error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='http://127.0.0.1:18766')
    parser.add_argument('--manifest', type=Path, default=Path('/tmp/e2e-stress/perf-dataset.json'))
    parser.add_argument('--root', type=Path, default=Path('/tmp/e2e-stress/perf-media'))
    args = parser.parse_args()
    if args.base != 'http://127.0.0.1:18766':
        parser.error('media preparation requires isolated http://127.0.0.1:18766')
    if not args.root.resolve().is_relative_to(Path('/tmp').resolve()):
        parser.error('fixture files must live under /tmp')
    token = os.environ.get('CRAWLER_MEDIA_TOKEN')
    if not token:
        parser.error('CRAWLER_MEDIA_TOKEN is required')
    resources, error = prepare(Client(args.base, token), args.manifest, args.root)
    print('Prepared finite media resource keys: ' + ', '.join(sorted(resources)))
    if error:
        print(error, file=sys.stderr)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
