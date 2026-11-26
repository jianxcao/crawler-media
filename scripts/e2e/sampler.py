import json
import os
from pathlib import Path
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
    B = 'http://127.0.0.1:18765/api/v1'
    H = {'Authorization': 'Bearer ' + resolve_token(base_url=B)}

    def get(p, to=30):
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

    out = open('/tmp/e2e/samples.jsonl', 'a')
    pick = {}
    last = 0
    while True:
        now = time.time()
        if now - last > 30:
            last = now
            c, _, b = get('/libraries')
            try:
                for l in json.loads(b)['data']:
                    if l['is_default']:
                        continue
                    c2, _, b2 = get('/libraries/%s/items?limit=200' % l['id'])
                    d = json.loads(b2)['data']
                    its = d['items'] if isinstance(d, dict) else d
                    if its:
                        big = max(its, key=lambda i: i.get('episode_count') or 0)
                        pick[l['name']] = (l['id'], big['media_item_id'], big.get('title'))
                    elif l['name'] in pick:
                        del pick[l['name']]
            except Exception:
                pass
            c, _, b = get('/jobs?active_only=true')
            try:
                data = json.loads(b)['data']
                rows = data['items'] if isinstance(data, dict) else data
                act = [j.get('kind') or j.get('name') for j in rows]
            except Exception:
                act = []
            out.write(json.dumps({'t': now, 'active': act}) + '\n')
        for n, (lid, iid, tt) in list(pick.items()):
            c, ms, _ = get('/libraries/%s/items/%s' % (lid, iid), 60)
            out.write(json.dumps({'t': time.time(), 'lib': n, 'title': tt, 'code': c, 'ms': round(ms, 1)}) + '\n')
        out.flush()
        time.sleep(2)


if __name__ == '__main__':
    main()
