import collections
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
    B = 'http://127.0.0.1:18765'
    H = {'Authorization': 'Bearer ' + resolve_token(base_url=B)}

    def raw(u):
        if u.startswith('/') and not u.startswith('/api'):
            u = '/api/v1' + u
        url = u if u.startswith('http') else B + u
        try:
            r = urllib.request.urlopen(urllib.request.Request(url, headers=H), timeout=30)
            b = r.read(16)
            return r.status, b
        except urllib.error.HTTPError as e:
            return e.code, b''
        except Exception:
            return 0, b''

    def img_ok(u):
        c, b = raw(u)
        return c == 200 and (
            b[:3] == b'\xff\xd8\xff'
            or b[:8] == b'\x89PNG\r\n\x1a\n'
            or b[8:12] == b'WEBP'
            or b[:4] in (b'GIF8',)
        ), c

    J = lambda p: json.load(urllib.request.urlopen(urllib.request.Request(B + '/api/v1' + p, headers=H)))['data']
    miss = collections.defaultdict(list)
    cnt = collections.Counter()
    bad = []
    for l in J('/libraries'):
        if l['is_default']:
            continue
        for it in J('/libraries/%s/items?limit=500' % l['id']):
            t = it['title']
            d = J('/libraries/%s/items/%s' % (l['id'], it['media_item_id']))
            cnt['items'] += 1
            for f in ['poster_url', 'backdrop_url', 'overview']:
                if not d.get(f):
                    miss['item.' + f].append(t)
                elif f.endswith('url'):
                    ok, c = img_ok(d[f])
                    cnt['img'] += 1
                    if not ok:
                        bad.append((t, f, c))
            if not d.get('cast'):
                miss['item.cast'].append(t)
            for a in (d.get('cast') or [])[:5]:
                cnt['cast'] += 1
                if not a.get('avatar_url'):
                    miss['cast.avatar'].append('%s/%s' % (t, a['name']))
                else:
                    ok, c = img_ok(a['avatar_url'])
                    cnt['img'] += 1
                    if not ok:
                        bad.append((t, 'avatar ' + a['name'], c))
            if not d.get('genres'):
                miss['item.genres'].append(t)
            cnt['seasons'] += len(d.get('seasons') or [])
            if it['kind'] == 'tv':
                eps = J('/libraries/%s/items/%s/episodes' % (l['id'], it['media_item_id']))
                for e in eps:
                    cnt['episodes'] += 1
                    n = '%s S%sE%s' % (t, e['season_number'], e['episode_number'])
                    if not e.get('overview'):
                        miss['ep.overview'].append(n)
                    if not e.get('name'):
                        miss['ep.name'].append(n)
                    if not e.get('still_url'):
                        miss['ep.still'].append(n)
                    else:
                        ok, c = img_ok(e['still_url'])
                        cnt['img'] += 1
                        if not ok:
                            bad.append((n, 'still', c))
    print(json.dumps(cnt))
    for k, v in miss.items():
        print(k, len(v), v[:5])
    print('bad images', len(bad), bad[:10])
    if len(sys.argv) > 1:
        with open(sys.argv[1], 'w') as out_file:
            json.dump({'cnt': cnt, 'miss': miss, 'bad': bad}, out_file, ensure_ascii=False)


if __name__ == '__main__':
    main()
