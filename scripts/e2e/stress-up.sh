#!/bin/zsh
# Isolated release performance instance. Credentials are local test values; never printed.
set -eu
R=${1:-/tmp/e2e-stress}
P=${0:A:h}/../..
MODE=${2:-start}
mkdir -p "$R/data"
export CRAWLER_MEDIA_TOKEN=${CRAWLER_MEDIA_TOKEN:-changeme}
export CRAWLER_MEDIA_ADMIN_PASSWORD=${CRAWLER_MEDIA_ADMIN_PASSWORD:-local-perf-admin-password}
if [[ "$MODE" != restart && ${PERF_SKIP_BUILD:-0} != 1 ]]; then
  cargo build --manifest-path "$P/Cargo.toml" --release -j8 -p api
fi
if [[ -f "$R/pid" ]] && kill -0 "$(cat "$R/pid")" 2>/dev/null; then
  if [[ "$MODE" != restart ]]; then
    print -u2 'Performance instance already running; use restart mode or stop it explicitly.'
    exit 1
  fi
  kill "$(cat "$R/pid")"
  for _ in {1..100}; do
    kill -0 "$(cat "$R/pid")" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "$(cat "$R/pid")" 2>/dev/null; then
    print -u2 'Performance instance did not stop; refusing to start another process.'
    exit 1
  fi
fi
if [[ "$MODE" != restart ]]; then
  [[ -d "$R/media" ]] || python3 "$P/scripts/e2e/gen-stress.py" "$R/media"
  # Local shared artwork is hard-linked into the fixture; no upstream downloads.
  python3 - "$P" "$R" <<'PY'
import os
from pathlib import Path
import sys
repo, root = map(Path, sys.argv[1:])
import shutil
art = root / 'fixture-owned-art'
art.mkdir(exist_ok=True)
for kind in ('movie', 'tv'):
    destination = art / f'{kind}.jpg'
    if not destination.exists():
        shutil.copyfile(repo / 'crates/api/assets/covers' / f'{kind}.jpg', destination)
for kind in ('movie', 'tv'):
    for directory in (root / 'media' / kind).iterdir():
        for name in ('poster.jpg', 'fanart.jpg'):
            destination = directory / name
            if not destination.exists():
                os.link(art / f'{kind}.jpg', destination)
        if kind == 'tv':
            for episode in directory.rglob('*.strm'):
                destination = episode.with_name(episode.stem + '-thumb.jpg')
                if not destination.exists():
                    os.link(art / 'tv.jpg', destination)
PY
fi
CRAWLER_MEDIA_DATA="$R/data" CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE=1 \
CRAWLER_MEDIA_LISTEN=127.0.0.1:18766 nohup "$P/target/release/crawler-media" serve \
  > "$R/backend.log" 2>&1 &
print $! > "$R/pid"
python3 - "$P" "$R" "$MODE" <<'PY'
import json
import os
from pathlib import Path
import sys
import time
sys.path.insert(0, str(Path(sys.argv[1]) / 'scripts/e2e'))
from perf_support import Client, disable_auxiliary_scrape
repo, root, mode = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
client = Client('http://127.0.0.1:18766', os.environ['CRAWLER_MEDIA_TOKEN'])
deadline = time.monotonic() + 60
while client.raw('/api/v1/health')[0] != 200:
    if time.monotonic() >= deadline:
        raise SystemExit('Performance instance did not become healthy in 60 seconds')
    time.sleep(.1)
if mode == 'restart':
    raise SystemExit(0)
disable_auxiliary_scrape(client)
existing = {entry['name']: entry for entry in client.call('/api/v1/libraries')}
for kind in ('movie', 'tv'):
    name = f'stress-{kind}'
    if name not in existing:
        existing[name] = client.call('/api/v1/libraries', 'POST', {
            'name': name, 'kind': kind, 'root_paths': [str(root / 'media' / kind)],
            'detect_intros': False, 'enable_fingerprint': False, 'realtime_watch': False,
            'generate_thumbnails': False, 'extract_chapter_images': False})
for kind in ('movie', 'tv'):
    library = existing[f'stress-{kind}']['id']
    operation = client.call(f'/api/v1/libraries/{library}/scan', 'POST')['operation_id']
    deadline = time.monotonic() + 1800
    while True:
        status = client.call(f'/api/v1/libraries/{library}/scan-operations/{operation}')
        if status['status'] == 'completed':
            break
        if status['status'] in ('failed', 'cancelled', 'partial'):
            raise SystemExit(f'{kind} scan failed: {status["status"]}')
        if time.monotonic() > deadline:
            raise SystemExit(f'{kind} scan did not complete within 30 minutes')
        time.sleep(.5)
print('Isolated performance instance ready; scans completed', flush=True)
PY
if [[ "$MODE" != restart ]]; then
  python3 "$P/scripts/e2e/seed-activity.py" http://127.0.0.1:18766 --root "$R"
  python3 "$P/scripts/e2e/prepare-perf-resources.py" --base http://127.0.0.1:18766 --manifest "$R/perf-dataset.json" --root "$R/perf-resources"
  python3 "$P/scripts/e2e/prepare-perf-media.py" --base http://127.0.0.1:18766 --manifest "$R/perf-dataset.json" --root "$R/perf-media"
fi
