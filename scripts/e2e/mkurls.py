import json
import os
from pathlib import Path
import sys
import urllib.parse
import urllib.request


def _pid_is_alive(pid_file):
    try:
        raw = pid_file.read_text(encoding='utf-8', errors='ignore').strip()
        if not raw.isdigit():
            return False
        pid = int(raw)
        if pid <= 0:
            return False
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


def resolve_token(cli_token=None, repo_root=None, base_url=None):
    if cli_token and cli_token.strip():
        return cli_token.strip()
    for key in ('CRAWLER_MEDIA_TOKEN', 'TOKEN'):
        value = os.environ.get(key, '').strip()
        if value:
            return value
    repo = Path(repo_root).expanduser() if repo_root is not None else Path(__file__).resolve().parents[2]
    raw_data_dir = os.environ.get('DATA_DIR', '').strip()
    raw_test_root = os.environ.get('TEST_ROOT', '').strip()
    if not raw_data_dir and not raw_test_root and base_url is not None:
        raw_port = os.environ.get('BACKEND_PORT', '').strip()
        try:
            dev_port = int(raw_port) if raw_port else 18765
        except ValueError:
            dev_port = 18765
        parts = urllib.parse.urlsplit(base_url)
        target_port = parts.port or (443 if parts.scheme == 'https' else 80 if parts.scheme == 'http' else None)
        if target_port != dev_port:
            return 'changeme'
    test_root = Path(raw_test_root).expanduser() if raw_test_root else (Path.home() / 'Downloads/crawler-media')
    if not test_root.is_absolute():
        test_root = repo / test_root
    if raw_data_dir:
        data_dir = Path(raw_data_dir).expanduser()
        if not data_dir.is_absolute():
            data_dir = repo / data_dir
    else:
        data_dir = test_root / 'data'
    if not raw_data_dir or raw_test_root or data_dir == test_root / 'data':
        run_dir = test_root / 'run'
        active_token_file = run_dir / 'backend.token'
        try:
            if _pid_is_alive(run_dir / 'backend.pid') and active_token_file.is_file():
                active = active_token_file.read_text(encoding='utf-8', errors='ignore').strip()
                if active:
                    return active
        except OSError:
            pass
    token_file = data_dir / '.dev-api-token'
    try:
        if token_file.is_file():
            saved = token_file.read_text(encoding='utf-8', errors='ignore').strip()
            if saved:
                return saved
    except OSError:
        pass
    return 'changeme'


def main():
    phase = sys.argv[1]
    full = sys.argv[2] == 'full'
    B = 'http://127.0.0.1:18765/api/v1'
    H = {'Authorization': 'Bearer ' + resolve_token(base_url=B)}
    F = 'http://127.0.0.1:3334'
    g = lambda p: json.load(urllib.request.urlopen(urllib.request.Request(B + p, headers=H)))['data']
    libs = [l for l in g('/libraries') if not l['is_default']]
    urls = [('library', F + '/library')]
    for l in libs:
        its = g('/libraries/%s/items?limit=200' % l['id'])
        if full:
            urls.append(('lib-' + l['name'], F + '/library/' + l['id']))
        for it in (its if full else sorted(its, key=lambda x: -(x.get('episode_count') or 0))[:1]):
            urls.append(('detail-%s-%s' % (l['name'], it['title']), '%s/library/%s/item/%s' % (F, l['id'], it['media_item_id'])))
    if full:
        for p in [
            '/library/favorites',
            '/library/manage',
            '/library/customize',
            '/settings',
            '/tasks',
            '/activity',
            '/search',
            '/collections',
            '/subscriptions',
            '/my',
            '/discover/movie',
            '/health',
        ]:
            urls.append(('page' + p.replace('/', '-'), F + p))
    with open('/tmp/e2e/urls-%s.json' % phase, 'w') as out_file:
        json.dump({'phase': phase, 'urls': urls}, out_file, ensure_ascii=False)


if __name__ == '__main__':
    main()
