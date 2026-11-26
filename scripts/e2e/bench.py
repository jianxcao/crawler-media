import json
import os
from pathlib import Path
import statistics
import sys
import time
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
    N = int(sys.argv[2]) if len(sys.argv) > 2 else 15
    B = 'http://127.0.0.1:18765/api/v1'
    H = {'Authorization': 'Bearer ' + resolve_token(base_url=B)}

    def get(p, to=60):
        t = time.time()
        try:
            r = urllib.request.urlopen(urllib.request.Request(B + p, headers=H), timeout=to)
            b = r.read()
            c = r.status
        except urllib.error.HTTPError as e:
            b = e.read()
            c = e.code
        except Exception:
            b = b''
            c = 0
        return c, (time.time() - t) * 1000, b

    J = lambda p: json.loads(get(p)[2])['data']
    libs = {l['name']: l for l in J('/libraries')}
    lib = libs['tv-en']['id']
    its = J('/libraries/%s/items' % lib)
    it = its[0]
    ledger = it['poster_url'].split('/')[-1]
    jobs = J('/jobs')
    jl = jobs['items'] if isinstance(jobs, dict) else jobs
    sub = {
        '{id}': lib,
        '{item_id}': it['media_item_id'],
        '{ledger_id}': ledger,
        '{kind}': 'tv',
        '{index}': '0',
        '{season}': '1',
    }
    skip = ['stream', '/proxy', '{operation_id}', '{play_session_id}', '/export', '{collection_id}', '/users/removals']
    ext = ['/discover', '/media/', '/search/torrents', '/search/titles', '/genres', '/sites/boost', '/sites/catalog', '/subscriptions/automation']
    special = {
        '/media/{kind}/{id}': '/media/tv/1438',
        '/media/tv/{id}/season/{season}': '/media/tv/1438/season/1',
        '/media/person/{id}': '/media/person/31',
        '/media/douban/{id}': '/media/douban/1292052',
        '/jobs/{id}': '/jobs/' + str(jl[0]['id']),
        '/search/library-items': '/search/library-items?q=%E7%81%AB',
        '/search/titles': '/search/titles?q=wire',
        '/fs/browse': '/fs/browse?path=/tmp',
        '/media/{id}/poster': '/media/' + it['media_item_id'] + '/poster',
    }
    res = []
    with open('/tmp/e2e/routes.txt') as routes_file:
        for l in routes_file:
            ms, p = l.split()
            if 'get' not in ms or any(s in p for s in skip):
                continue
            u = special.get(p)
            if not u:
                u = p
                for k, v in sub.items():
                    u = u.replace(k, v)
                if '{' in u:
                    continue
            n = 3 if any(e in p for e in ext) else N
            ts = []
            codes = {}
            for _ in range(n):
                c, ms_, _ = get(u)
                ts.append(ms_)
                codes[c] = codes.get(c, 0) + 1
            ts.sort()
            res.append({
                'route': p,
                'n': n,
                'p50': round(statistics.median(ts), 1),
                'p95': round(ts[min(n - 1, int(n * 0.95))], 1),
                'max': round(ts[-1], 1),
                'codes': codes,
            })
            print(p, res[-1]['p50'], res[-1]['p95'], res[-1]['max'], codes, flush=True)
    with open('/tmp/e2e/bench-%s.json' % phase, 'w') as out_file:
        json.dump(res, out_file, ensure_ascii=False)


if __name__ == '__main__':
    main()
