"""Opaque-box HTTP client for crawler-media plugin E2E tests (Python 3.9+ stdlib)."""
from __future__ import annotations

from dataclasses import dataclass, field
import errno
import http.client
import json
import select
import socket
import ssl
import threading
import time
from typing import Any, Dict, List, Optional, Tuple
import urllib.parse
import uuid


_RETRY_EADDR_BACKOFFS = (0.05, 0.15, 0.35, 0.75)
_STALE_CONN_EXCS = (
    http.client.RemoteDisconnected,
    http.client.BadStatusLine,
    http.client.CannotSendRequest,
    http.client.ResponseNotReady,
    ConnectionResetError,
    ConnectionAbortedError,
    BrokenPipeError,
)
_IDEMPOTENT_METHODS = frozenset({"GET", "HEAD", "OPTIONS", "TRACE"})


def _is_conn_usable(conn: http.client.HTTPConnection) -> bool:
    sock = getattr(conn, "sock", None)
    if sock is None:
        return False
    try:
        r, _, _ = select.select([sock], [], [], 0.0)
    except (OSError, ValueError):
        return False
    if not r:
        return True
    if isinstance(sock, ssl.SSLSocket):
        return False
    try:
        sock.recv(1, socket.MSG_PEEK | socket.MSG_DONTWAIT)
    except (OSError, ValueError):
        pass
    return False


@dataclass
class HttpResponse:
    """Raw HTTP response without auto-redirect or hidden envelope coercion."""

    status: int
    reason: str
    headers: Dict[str, str]
    raw_headers: List[Tuple[str, str]]
    body: bytes
    elapsed_ms: float

    def text(self, encoding: str = "utf-8") -> str:
        return self.body.decode(encoding, errors="replace")

    def json(self) -> Any:
        if not self.body:
            return None
        return json.loads(self.body.decode("utf-8"))

    def header(self, name: str, default: Optional[str] = None) -> Optional[str]:
        return self.headers.get(name.lower(), default)

    def header_values(self, name: str) -> List[str]:
        target = name.lower()
        return [v for k, v in self.raw_headers if k.lower() == target]


class ApiError(RuntimeError):
    """Structured API/HTTP error preserving status, error code, fields, and response."""

    def __init__(
        self,
        status: int,
        path: str,
        code: Optional[str] = None,
        message: Optional[str] = None,
        fields: Optional[List[Dict[str, Any]]] = None,
        response: Optional[HttpResponse] = None,
    ) -> None:
        clean_path = path.split("?")[0]
        detail = f"{code}: {message}" if code else (message or f"HTTP {status}")
        super().__init__(f"HTTP {status} for {clean_path} ({detail})")
        self.status = status
        self.path = path
        self.code = code
        self.message = message
        self.fields = fields or []
        self.response = response


def _set_header_ci(
    hdrs: Dict[str, str],
    key: str,
    value: str,
    overwrite: bool = True,
) -> None:
    """Insert or replace an HTTP header using case-insensitive key deduplication."""
    target = key.lower()
    existing = [k for k in hdrs if k.lower() == target]
    if existing and not overwrite:
        return
    for k in existing:
        del hdrs[k]
    hdrs[key] = value


def _extract_mc_session(raw_headers: List[Tuple[str, str]]) -> Optional[str]:
    """Extract mc_session value from Set-Cookie headers."""
    session: Optional[str] = None
    for name, value in raw_headers:
        if name.lower() != "set-cookie":
            continue
        first_part = value.split(";", 1)[0].strip()
        if first_part.startswith("mc_session="):
            val = first_part[len("mc_session=") :]
            session = val if val else ""
    return session


def _encode_multipart(
    fields: Dict[str, str],
    files: List[Tuple[str, str, bytes, str]],
) -> Tuple[bytes, str]:
    """Encode multipart/form-data body without external dependencies."""
    boundary = f"----CrawlerE2EBoundary{uuid.uuid4().hex}"
    chunks: List[bytes] = []
    for name, val in fields.items():
        chunks.append(f"--{boundary}\r\n".encode("ascii"))
        chunks.append(
            f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode("utf-8")
        )
        chunks.append(val.encode("utf-8"))
        chunks.append(b"\r\n")
    for field_name, filename, content, content_type in files:
        chunks.append(f"--{boundary}\r\n".encode("ascii"))
        chunks.append(
            (
                f'Content-Disposition: form-data; name="{field_name}"; '
                f'filename="{filename}"\r\n'
                f"Content-Type: {content_type}\r\n\r\n"
            ).encode("utf-8")
        )
        chunks.append(content)
        chunks.append(b"\r\n")
    chunks.append(f"--{boundary}--\r\n".encode("ascii"))
    return b"".join(chunks), f"multipart/form-data; boundary={boundary}"


class ApiClient:
    """Opaque-box HTTP client wrapping http.client.HTTPConnection."""

    def __init__(
        self,
        base_url: str,
        token: Optional[str] = None,
        session_cookie: Optional[str] = None,
        timeout: float = 30.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.session_cookie = session_cookie
        self.timeout = timeout

        parts = urllib.parse.urlsplit(self.base_url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            raise ValueError(f"Invalid base_url: {base_url!r}")
        self._scheme = parts.scheme
        self._host = parts.hostname
        self._port = parts.port or (443 if parts.scheme == "https" else 80)
        self._base_path = parts.path.rstrip("/")
        self._conn_lock = threading.Lock()
        self._conn: Optional[http.client.HTTPConnection] = None
        self._conn_timeout: Optional[float] = None

    def _new_connection(self, timeout: Optional[float] = None) -> http.client.HTTPConnection:
        effective_timeout = self.timeout if timeout is None else timeout
        if self._scheme == "https":
            return http.client.HTTPSConnection(
                self._host, self._port, timeout=effective_timeout
            )
        return http.client.HTTPConnection(
            self._host, self._port, timeout=effective_timeout
        )

    def _discard_conn_locked(self) -> None:
        conn = self._conn
        self._conn = None
        self._conn_timeout = None
        if conn is not None:
            try:
                conn.close()
            except OSError:
                pass

    def close(self) -> None:
        """Close any cached persistent HTTP/1.1 connection."""
        with self._conn_lock:
            self._discard_conn_locked()

    def __enter__(self) -> ApiClient:
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.close()

    def __del__(self) -> None:
        conn = getattr(self, "_conn", None)
        if conn is not None:
            self._conn = None
            try:
                conn.close()
            except OSError:
                pass

    def _resolve_target(self, path: str) -> str:
        if not path.startswith("/"):
            path = "/" + path
        return f"{self._base_path}{path}" if self._base_path else path

    def raw_request(
        self,
        method: str,
        path: str,
        body: Optional[bytes] = None,
        headers: Optional[Dict[str, str]] = None,
        no_bearer: bool = False,
        no_cookie: bool = False,
        bearer_token: Optional[str] = None,
        cookie_token: Optional[str] = None,
        timeout: Optional[float] = None,
    ) -> HttpResponse:
        """Send an HTTP request and return raw HttpResponse without auto-redirects."""
        req_headers: Dict[str, str] = {}
        effective_bearer = bearer_token if bearer_token is not None else self.token
        effective_cookie = cookie_token if cookie_token is not None else self.session_cookie

        if not no_bearer and effective_bearer:
            _set_header_ci(req_headers, "Authorization", f"Bearer {effective_bearer}")
        if not no_cookie and effective_cookie:
            _set_header_ci(req_headers, "Cookie", f"mc_session={effective_cookie}")
        if headers:
            for k, v in headers.items():
                _set_header_ci(req_headers, k, v, overwrite=True)

        target = self._resolve_target(path)
        upper_method = method.upper()
        effective_timeout = self.timeout if timeout is None else timeout
        conn_hdr = next(
            (v for k, v in req_headers.items() if k.lower() == "connection"),
            "",
        )
        force_close = "close" in conn_hdr.lower()
        started = time.perf_counter()

        if force_close:
            eaddr_retries = 0
            while True:
                conn = self._new_connection(timeout=effective_timeout)
                sent = False
                try:
                    conn.request(upper_method, target, body=body, headers=req_headers)
                    sent = True
                    resp = conn.getresponse()
                    raw_body = resp.read()
                    raw_hdrs = list(resp.getheaders())
                    hdr_map = {k.lower(): v for k, v in raw_hdrs}
                    return HttpResponse(
                        status=resp.status,
                        reason=resp.reason,
                        headers=hdr_map,
                        raw_headers=raw_hdrs,
                        body=raw_body,
                        elapsed_ms=(time.perf_counter() - started) * 1000.0,
                    )
                except OSError as exc:
                    if (
                        exc.errno in (errno.EADDRNOTAVAIL, errno.EADDRINUSE)
                        and not sent
                        and eaddr_retries < len(_RETRY_EADDR_BACKOFFS)
                    ):
                        time.sleep(_RETRY_EADDR_BACKOFFS[eaddr_retries])
                        eaddr_retries += 1
                        continue
                    raise
                finally:
                    try:
                        conn.close()
                    except OSError:
                        pass

        with self._conn_lock:
            stale_retries = 0
            eaddr_retries = 0
            while True:
                conn = self._conn
                reused = False
                if conn is not None:
                    if self._conn_timeout != effective_timeout or not _is_conn_usable(conn):
                        self._discard_conn_locked()
                    else:
                        reused = True
                if self._conn is None:
                    self._conn = self._new_connection(timeout=effective_timeout)
                    self._conn_timeout = effective_timeout
                    conn = self._conn
                    reused = False

                sent = False
                try:
                    conn.request(upper_method, target, body=body, headers=req_headers)
                    sent = True
                    resp = conn.getresponse()
                    raw_body = resp.read()
                    raw_hdrs = list(resp.getheaders())
                    hdr_map = {k.lower(): v for k, v in raw_hdrs}
                    if resp.will_close:
                        self._discard_conn_locked()
                    return HttpResponse(
                        status=resp.status,
                        reason=resp.reason,
                        headers=hdr_map,
                        raw_headers=raw_hdrs,
                        body=raw_body,
                        elapsed_ms=(time.perf_counter() - started) * 1000.0,
                    )
                except OSError as exc:
                    self._discard_conn_locked()
                    if (
                        exc.errno in (errno.EADDRNOTAVAIL, errno.EADDRINUSE)
                        and not sent
                        and eaddr_retries < len(_RETRY_EADDR_BACKOFFS)
                    ):
                        time.sleep(_RETRY_EADDR_BACKOFFS[eaddr_retries])
                        eaddr_retries += 1
                        continue
                    if reused and isinstance(
                        exc,
                        (ConnectionResetError, ConnectionAbortedError, BrokenPipeError),
                    ):
                        can_retry = (not sent) or (upper_method in _IDEMPOTENT_METHODS)
                        if can_retry and stale_retries < 1:
                            stale_retries += 1
                            continue
                    raise
                except _STALE_CONN_EXCS:
                    self._discard_conn_locked()
                    can_retry = (not sent) or (upper_method in _IDEMPOTENT_METHODS)
                    if reused and can_retry and stale_retries < 1:
                        stale_retries += 1
                        continue
                    raise
                except Exception:
                    self._discard_conn_locked()
                    raise

    def _build_plugin_headers(
        self,
        headers: Optional[Dict[str, str]] = None,
        if_match: Optional[str] = None,
        idempotency_key: Optional[str] = None,
        reauth_token: Optional[str] = None,
        plugin_package: Optional[str] = None,
        plugin_generation: Optional[Any] = None,
        request_id: Optional[str] = None,
    ) -> Dict[str, str]:
        merged: Dict[str, str] = {}
        if headers:
            for k, v in headers.items():
                _set_header_ci(merged, k, v, overwrite=True)
        if if_match is not None:
            if not (if_match.startswith('"') and if_match.endswith('"')):
                _set_header_ci(merged, "If-Match", f'"{if_match}"')
            else:
                _set_header_ci(merged, "If-Match", if_match)
        if idempotency_key is not None:
            _set_header_ci(merged, "Idempotency-Key", idempotency_key)
        if reauth_token is not None:
            _set_header_ci(merged, "X-Plugin-Reauth", reauth_token)
        if plugin_package is not None:
            _set_header_ci(merged, "X-Plugin-Package", plugin_package)
        if plugin_generation is not None:
            _set_header_ci(merged, "X-Plugin-Generation", str(plugin_generation))
        if request_id is not None:
            _set_header_ci(merged, "X-Request-Id", request_id)
        return merged

    @staticmethod
    def parse_envelope(resp: HttpResponse, path: str) -> Dict[str, Any]:
        """Validate standard API envelope and raise ApiError on HTTP>=400 or ok=false."""
        parsed: Any = None
        if resp.body:
            try:
                parsed = json.loads(resp.body.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                if 200 <= resp.status < 300:
                    raise RuntimeError(
                        f"Invalid JSON response from {path} (status={resp.status}): {exc}"
                    ) from exc
                raise ApiError(
                    resp.status,
                    path,
                    message=resp.text()[:256],
                    response=resp,
                ) from exc

        if not (200 <= resp.status < 300):
            if isinstance(parsed, dict) and parsed.get("ok") is False:
                err = parsed.get("error") or {}
                raise ApiError(
                    resp.status,
                    path,
                    code=err.get("code"),
                    message=err.get("message"),
                    fields=err.get("fields"),
                    response=resp,
                )
            raise ApiError(
                resp.status,
                path,
                message=resp.text()[:256] if resp.body else resp.reason,
                response=resp,
            )

        if not isinstance(parsed, dict) or parsed.get("ok") is not True or "data" not in parsed:
            raise RuntimeError(
                f"Invalid API envelope for {path} (status={resp.status}): {parsed!r}"
            )
        return parsed

    def request_envelope(
        self,
        method: str,
        path: str,
        json_body: Any = None,
        raw_body: Optional[bytes] = None,
        headers: Optional[Dict[str, str]] = None,
        if_match: Optional[str] = None,
        idempotency_key: Optional[str] = None,
        reauth_token: Optional[str] = None,
        plugin_package: Optional[str] = None,
        plugin_generation: Optional[Any] = None,
        request_id: Optional[str] = None,
        no_bearer: bool = False,
        no_cookie: bool = False,
        timeout: Optional[float] = None,
    ) -> Tuple[HttpResponse, Dict[str, Any]]:
        hdrs = self._build_plugin_headers(
            headers=headers,
            if_match=if_match,
            idempotency_key=idempotency_key,
            reauth_token=reauth_token,
            plugin_package=plugin_package,
            plugin_generation=plugin_generation,
            request_id=request_id,
        )
        body_bytes = raw_body
        if json_body is not None:
            body_bytes = json.dumps(json_body, ensure_ascii=False).encode("utf-8")
            _set_header_ci(hdrs, "Content-Type", "application/json", overwrite=False)
        resp = self.raw_request(
            method,
            path,
            body=body_bytes,
            headers=hdrs,
            no_bearer=no_bearer,
            no_cookie=no_cookie,
            timeout=timeout,
        )
        env = self.parse_envelope(resp, path)
        return resp, env

    def request(
        self,
        method: str,
        path: str,
        json_body: Any = None,
        headers: Optional[Dict[str, str]] = None,
        if_match: Optional[str] = None,
        idempotency_key: Optional[str] = None,
        reauth_token: Optional[str] = None,
        plugin_package: Optional[str] = None,
        plugin_generation: Optional[Any] = None,
        request_id: Optional[str] = None,
        no_bearer: bool = False,
        no_cookie: bool = False,
        timeout: Optional[float] = None,
    ) -> Any:
        _, env = self.request_envelope(
            method,
            path,
            json_body=json_body,
            headers=headers,
            if_match=if_match,
            idempotency_key=idempotency_key,
            reauth_token=reauth_token,
            plugin_package=plugin_package,
            plugin_generation=plugin_generation,
            request_id=request_id,
            no_bearer=no_bearer,
            no_cookie=no_cookie,
            timeout=timeout,
        )
        return env["data"]

    def get(self, path: str, **kwargs: Any) -> Any:
        return self.request("GET", path, **kwargs)

    def post(self, path: str, json_body: Any = None, **kwargs: Any) -> Any:
        return self.request("POST", path, json_body=json_body, **kwargs)

    def put(self, path: str, json_body: Any = None, **kwargs: Any) -> Any:
        return self.request("PUT", path, json_body=json_body, **kwargs)

    def patch(self, path: str, json_body: Any = None, **kwargs: Any) -> Any:
        return self.request("PATCH", path, json_body=json_body, **kwargs)

    def delete(self, path: str, json_body: Any = None, **kwargs: Any) -> Any:
        return self.request("DELETE", path, json_body=json_body, **kwargs)

    def head(self, path: str, no_bearer: bool = False, no_cookie: bool = False, headers: Optional[Dict[str, str]] = None) -> HttpResponse:
        return self.raw_request(
            "HEAD",
            path,
            headers=headers,
            no_bearer=no_bearer,
            no_cookie=no_cookie,
        )

    def post_multipart(
        self,
        path: str,
        fields: Optional[Dict[str, str]] = None,
        files: Optional[List[Tuple[str, str, bytes, str]]] = None,
        headers: Optional[Dict[str, str]] = None,
        if_match: Optional[str] = None,
        idempotency_key: Optional[str] = None,
        reauth_token: Optional[str] = None,
        expect_envelope: bool = True,
    ) -> Any:
        body_bytes, content_type = _encode_multipart(fields or {}, files or [])
        hdrs = self._build_plugin_headers(
            headers=headers,
            if_match=if_match,
            idempotency_key=idempotency_key,
            reauth_token=reauth_token,
        )
        _set_header_ci(hdrs, "Content-Type", content_type, overwrite=True)
        resp = self.raw_request("POST", path, body=body_bytes, headers=hdrs)
        if not expect_envelope:
            return resp
        return self.parse_envelope(resp, path)["data"]

    def get_asset(
        self,
        path: str,
        use_cookie: bool = True,
        no_bearer: bool = True,
        headers: Optional[Dict[str, str]] = None,
    ) -> HttpResponse:
        """Fetch raw UI asset bytes without JSON envelope parsing."""
        return self.raw_request(
            "GET",
            path,
            headers=headers,
            no_bearer=no_bearer,
            no_cookie=not use_cookie,
        )

    def head_asset(
        self,
        path: str,
        use_cookie: bool = True,
        no_bearer: bool = True,
        headers: Optional[Dict[str, str]] = None,
    ) -> HttpResponse:
        return self.raw_request(
            "HEAD",
            path,
            headers=headers,
            no_bearer=no_bearer,
            no_cookie=not use_cookie,
        )

    def read_sse(
        self,
        path: str,
        max_events: int = 5,
        timeout: float = 5.0,
        headers: Optional[Dict[str, str]] = None,
    ) -> List[Dict[str, Any]]:
        """Read up to max_events SSE messages from a streaming endpoint."""
        req_headers: Dict[str, str] = {"Accept": "text/event-stream"}
        if self.token:
            _set_header_ci(req_headers, "Authorization", f"Bearer {self.token}")
        if self.session_cookie:
            _set_header_ci(req_headers, "Cookie", f"mc_session={self.session_cookie}")
        if headers:
            for k, v in headers.items():
                _set_header_ci(req_headers, k, v, overwrite=True)

        events: List[Dict[str, Any]] = []
        target = self._resolve_target(path)
        eaddr_retries = 0
        while True:
            conn = self._new_connection(timeout=timeout)
            try:
                conn.request("GET", target, headers=req_headers)
                resp = conn.getresponse()
                break
            except OSError as exc:
                try:
                    conn.close()
                except OSError:
                    pass
                if (
                    exc.errno in (errno.EADDRNOTAVAIL, errno.EADDRINUSE)
                    and eaddr_retries < len(_RETRY_EADDR_BACKOFFS)
                ):
                    time.sleep(_RETRY_EADDR_BACKOFFS[eaddr_retries])
                    eaddr_retries += 1
                    continue
                raise
            except Exception:
                try:
                    conn.close()
                except OSError:
                    pass
                raise
        try:
            if not (200 <= resp.status < 300):
                body = resp.read()
                raise ApiError(
                    resp.status,
                    path,
                    message=body.decode("utf-8", errors="replace")[:256],
                )
            deadline = time.monotonic() + timeout
            current_event: Dict[str, Any] = {}
            data_lines: List[str] = []
            timed_out = False
            while len(events) < max_events:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    timed_out = True
                    break
                if conn.sock is not None:
                    try:
                        conn.sock.settimeout(remaining)
                    except OSError:
                        pass
                try:
                    raw_line = resp.readline()
                except (socket.timeout, TimeoutError):
                    timed_out = True
                    break
                if not raw_line:
                    break
                line = raw_line.decode("utf-8", errors="replace").rstrip("\r\n")
                if line == "":
                    if data_lines or current_event:
                        current_event["data"] = "\n".join(data_lines)
                        events.append(current_event)
                        current_event = {}
                        data_lines = []
                    continue
                if line.startswith(":"):
                    continue
                if ":" in line:
                    field_name, rest = line.split(":", 1)
                    if rest.startswith(" "):
                        rest = rest[1:]
                else:
                    field_name, rest = line, ""
                if field_name == "data":
                    data_lines.append(rest)
                elif field_name in ("event", "id", "retry"):
                    current_event[field_name] = rest
            if not timed_out and (data_lines or current_event):
                current_event["data"] = "\n".join(data_lines)
                events.append(current_event)
            return events
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def health(self) -> Dict[str, Any]:
        return self.get("/api/v1/health", no_bearer=True, no_cookie=True)

    def login(self, username: str, password: str) -> Dict[str, Any]:
        """Log in via /api/v1/auth/login, storing both Bearer token and mc_session cookie."""
        resp, env = self.request_envelope(
            "POST",
            "/api/v1/auth/login",
            json_body={"username": username, "password": password},
            no_bearer=True,
            no_cookie=True,
        )
        data = env["data"]
        if isinstance(data, dict) and "token" in data:
            self.token = data["token"]
        cookie_val = _extract_mc_session(resp.raw_headers)
        if cookie_val:
            self.session_cookie = cookie_val
        elif self.token and not self.session_cookie:
            # Fallback only if server omitted Set-Cookie in a minimal stub
            self.session_cookie = None
        return data

    def logout(self) -> Any:
        data = self.post("/api/v1/auth/logout")
        self.token = None
        self.session_cookie = None
        return data

    def me(self, no_bearer: bool = False, no_cookie: bool = False) -> Dict[str, Any]:
        return self.get("/api/v1/auth/me", no_bearer=no_bearer, no_cookie=no_cookie)

    def create_member(self, login_name: str, password: str) -> Dict[str, Any]:
        return self.post(
            "/api/v1/users",
            json_body={"login": login_name, "password": password},
        )

    def clone(
        self,
        token: Optional[str] = None,
        session_cookie: Optional[str] = None,
    ) -> ApiClient:
        return ApiClient(
            self.base_url,
            token=token,
            session_cookie=session_cookie,
            timeout=self.timeout,
        )

    def clone_cookie_only(self) -> ApiClient:
        """Return a client carrying only mc_session cookie (no Authorization header)."""
        return ApiClient(
            self.base_url,
            token=None,
            session_cookie=self.session_cookie,
            timeout=self.timeout,
        )
