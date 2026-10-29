"""HTTP and dataset helpers shared by the local performance tools (stdlib only)."""
import json
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request


class ApiError(RuntimeError):
    def __init__(self, status, path, body):
        # Paths may have auth query parameters; never echo headers, bodies or query strings.
        super().__init__(f'HTTP {status} for {path.split("?")[0]}')
        self.status, self.body = status, body


class Client:
    def __init__(self, base, token=None, jellyfin=False, timeout=30):
        self.base, self.token, self.jellyfin, self.timeout = base.rstrip('/'), token, jellyfin, timeout

    def raw(self, path, method='GET', body=None, auth=True):
        headers = {}
        if auth and self.token:
            headers['Authorization'] = (f'MediaBrowser Token="{self.token}"' if self.jellyfin
                                        else f'Bearer {self.token}')
        data = None
        if body is not None:
            data = json.dumps(body, ensure_ascii=False).encode()
            headers['Content-Type'] = 'application/json'
        req = urllib.request.Request(self.base + urllib.parse.quote(path, safe='/?&=%:,+'),
                                     data=data, headers=headers, method=method)
        started = time.perf_counter()
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as response:
                payload, code = response.read(), response.status
        except urllib.error.HTTPError as error:
            payload, code = error.read(), error.code
        except (OSError, TimeoutError):
            payload, code = b'', 0
        return code, (time.perf_counter() - started) * 1000, payload

    def call(self, path, method='GET', body=None, envelope=True):
        code, _, payload = self.raw(path, method, body)
        if not 200 <= code < 300:
            raise ApiError(code, path, payload)
        result = json.loads(payload) if payload else None
        if envelope:
            if not isinstance(result, dict) or result.get('ok') is not True:
                raise RuntimeError(f'Invalid API envelope for {path.split("?")[0]}')
            return result['data']
        return result


def login(base, username, password, jellyfin=False):
    client = Client(base, jellyfin=jellyfin)
    if jellyfin:
        result = client.call('/Users/AuthenticateByName', 'POST',
                             {'Username': username, 'Pw': password}, envelope=False)
        return Client(base, result['AccessToken'], jellyfin=True), result['User']['Id']
    result = client.call('/api/v1/auth/login', 'POST', {'username': username, 'password': password})
    return Client(base, result['token']), result['user']['id']


def disable_auxiliary_scrape(client):
    """Keep the fixture's optional global Scrape separate from measured scans."""
    for job in client.call('/api/v1/jobs'):
        if job.get('kind') == 'scrape':
            client.call(f'/api/v1/jobs/{job["id"]}', 'PATCH', {'enabled': False})


def items(client, library):
    result, cursor = [], None
    while True:
        path = f'/api/v1/libraries/{library}/items?limit=500'
        if cursor:
            path += '&cursor=' + urllib.parse.quote(cursor, safe='')
        response = client.call(path, envelope=False)
        batch = response['data']
        result.extend(batch)
        cursor = response['next_cursor']
        if not cursor or not batch:
            return result


def write_private_json(path, value):
    import os
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, 'w') as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)
