"""In-process opaque HTTP stub server strictly for `run.py --self-test` mode."""
from __future__ import annotations

import hashlib
import http.server
import json
import socketserver
import sys
import threading
from typing import Any, Dict, List, Optional, Tuple
import urllib.parse
import uuid

from .mocks import _parse_cookies, _parse_multipart_form


VALID_REAUTH_PURPOSES = frozenset(
    {
        "trust-key:create",
        "trust-key:revoke",
        "developer-mode:enable",
        "plugin:upgrade",
        "plugin:rollback",
        "plugin:state-reset",
        "plugin:upload",
        "plugin-ui-trust",
    }
)


class _ThreadedStubHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128

    def handle_error(self, request: Any, client_address: Any) -> None:
        _, exc, _ = sys.exc_info()
        if isinstance(exc, (BrokenPipeError, ConnectionResetError, ConnectionAbortedError)):
            return
        super().handle_error(request, client_address)


class SelfTestStubServer:
    """Opaque-box HTTP stub bound to 127.0.0.1:0 for --self-test verification."""

    def __init__(
        self,
        admin_password: str = "e2e-admin-password-secret",
        cli_token: str = "e2e-cli-token-secret",
    ) -> None:
        self.admin_password = admin_password
        self.cli_token = cli_token

        self._lock = threading.Lock()
        self.users: Dict[str, Dict[str, Any]] = {
            "admin": {
                "id": "00000000-0000-0000-0000-000000000001",
                "login": "admin",
                "role": "admin",
                "enabled": True,
                "password": admin_password,
            }
        }
        self.tokens: Dict[str, str] = {cli_token: "admin"}
        self.trust_keys: List[Dict[str, Any]] = []
        self.uploads: Dict[str, Dict[str, Any]] = {}
        self.dev_mode: bool = False
        self.dev_rev: int = 1
        self.plugins: List[Dict[str, Any]] = [
            {
                "id": "org.example.stub-policy",
                "name": "Stub Policy",
                "version": "1.0.0",
                "lifecycle": "enabled",
                "revision": 1,
            }
        ]

        self._server: Optional[_ThreadedStubHTTPServer] = None
        self._thread: Optional[threading.Thread] = None
        self.port: Optional[int] = None

    @property
    def url(self) -> str:
        if self.port is None:
            raise RuntimeError("SelfTestStubServer is not running")
        return f"http://127.0.0.1:{self.port}"

    def _resolve_user(self, headers: Any) -> Optional[Dict[str, Any]]:
        auth = headers.get("Authorization", "")
        token: Optional[str] = None
        if auth.startswith("Bearer "):
            token = auth[len("Bearer ") :].strip()
        if not token:
            cookies = _parse_cookies(headers.get("Cookie"))
            token = cookies.get("mc_session")
        if not token:
            return None
        with self._lock:
            login = self.tokens.get(token)
            if not login:
                return None
            u = self.users.get(login)
            return dict(u) if u else None

    def _resolve_cookie_user(self, headers: Any) -> Optional[Dict[str, Any]]:
        cookies = _parse_cookies(headers.get("Cookie"))
        token = cookies.get("mc_session")
        if not token:
            return None
        with self._lock:
            login = self.tokens.get(token)
            if not login:
                return None
            u = self.users.get(login)
            return dict(u) if u else None

    def _make_handler(self) -> Any:
        parent = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: Any) -> None:
                return

            def _read_body(self) -> bytes:
                length = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(length) if length > 0 else b""

            def _send_raw(
                self,
                status: int,
                body: bytes,
                content_type: str,
                extra_headers: Optional[List[Tuple[str, str]]] = None,
                head_only: bool = False,
            ) -> None:
                req_id = self.headers.get("X-Request-Id") or str(uuid.uuid4())
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("X-Request-Id", req_id)
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                if not head_only:
                    self.wfile.write(body)

            def _send_json(
                self,
                status: int,
                payload: Any,
                extra_headers: Optional[List[Tuple[str, str]]] = None,
            ) -> None:
                self._send_raw(
                    status,
                    json.dumps(payload).encode("utf-8"),
                    "application/json; charset=utf-8",
                    extra_headers=extra_headers,
                )

            def _send_ok(
                self,
                data: Any,
                status: int = 200,
                extra_headers: Optional[List[Tuple[str, str]]] = None,
                next_cursor: Optional[str] = None,
            ) -> None:
                body: Dict[str, Any] = {"ok": True, "data": data}
                if next_cursor is not None:
                    body["next_cursor"] = next_cursor
                self._send_json(status, body, extra_headers=extra_headers)

            def _send_err(self, status: int, code: str, message: str) -> None:
                self._send_json(
                    status,
                    {"ok": False, "error": {"code": code, "message": message}},
                )

            def _require_member(self) -> Optional[Dict[str, Any]]:
                u = parent._resolve_user(self.headers)
                if u is None:
                    self._send_err(401, "auth.required", "Authentication required")
                    return None
                return u

            def _require_admin(self) -> Optional[Dict[str, Any]]:
                u = self._require_member()
                if u is None:
                    return None
                if u.get("role") != "admin":
                    self._send_err(403, "auth.forbidden", "Admin required")
                    return None
                return u

            def do_HEAD(self) -> None:
                self._handle_get(head_only=True)

            def do_GET(self) -> None:
                self._handle_get(head_only=False)

            def _handle_get(self, head_only: bool = False) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                path = parsed.path

                if path == "/api/v1/health":
                    self._send_ok(
                        {
                            "status": "ok",
                            "version": "0.1.0-stub",
                            "commit": "self-test",
                            "read_only": False,
                        }
                    )
                    return

                if path == "/api/v1/auth/me":
                    u = self._require_member()
                    if u is None:
                        return
                    self._send_ok(
                        {
                            "id": u["id"],
                            "login": u["login"],
                            "role": u["role"],
                            "enabled": u["enabled"],
                        }
                    )
                    return

                if path == "/api/v1/jobs/stream":
                    if self._require_member() is None:
                        return
                    sse = (
                        b"event: ready\n"
                        b"data: {}\n\n"
                        b"event: job\n"
                        b"id: 1\n"
                        b'data: {"id":"job-1","count":1,"state":"succeeded"}\n\n'
                    )
                    self._send_raw(
                        200,
                        sse,
                        "text/event-stream; charset=utf-8",
                        head_only=head_only,
                    )
                    return

                if path == "/api/v1/plugins":
                    if self._require_admin() is None:
                        return
                    with parent._lock:
                        items = list(parent.plugins)
                    self._send_ok(items, next_cursor=None)
                    return

                if path == "/api/v1/settings/plugins":
                    if self._require_admin() is None:
                        return
                    with parent._lock:
                        self._send_ok(
                            {"developer_mode": parent.dev_mode, "revision": parent.dev_rev}
                        )
                    return

                if path == "/api/v1/plugin-trust-keys":
                    if self._require_admin() is None:
                        return
                    with parent._lock:
                        keys = list(parent.trust_keys)
                    self._send_ok(keys)
                    return

                if path == "/api/v1/plugin-hook-catalog":
                    if self._require_admin() is None:
                        return
                    self._send_ok(
                        {
                            "items": [
                                {"id": "search.before", "failure_policy": "fail-open"},
                                {"id": "search.after", "failure_policy": "fail-open"},
                                {"id": "download.before", "failure_policy": "fail-closed"},
                            ]
                        }
                    )
                    return

                if path == "/api/v1/plugin-ui-contract":
                    if self._require_admin() is None:
                        return
                    self._send_ok(
                        {
                            "contract": "1.0.0",
                            "react": "19.3.0",
                            "react_dom": "19.3.0",
                            "sdk_methods": ["query", "command", "runTask"],
                        }
                    )
                    return

                if path.startswith("/api/v1/plugin-ui-assets/"):
                    u = parent._resolve_cookie_user(self.headers)
                    if u is None:
                        self._send_err(401, "auth.required", "Cookie session required")
                        return
                    if u.get("role") != "admin":
                        self._send_err(403, "plugin.forbidden", "Admin required")
                        return
                    asset_bytes = b"export const stubRemote = true;\n"
                    self._send_raw(
                        200,
                        asset_bytes,
                        "application/javascript; charset=utf-8",
                        extra_headers=[
                            ("X-Content-Type-Options", "nosniff"),
                            ("Cache-Control", "private, no-store"),
                        ],
                        head_only=head_only,
                    )
                    return

                # Deleted legacy boost routes must be 404
                if path == "/api/v1/sites/boost-stats" or (
                    path.startswith("/api/v1/sites/") and path.endswith("/boost")
                ):
                    self._send_err(404, "not_found", "Legacy boost route removed")
                    return

                # Phase 2 deferred routes must be 404
                if (
                    path.startswith("/api/v1/plugin-repos")
                    or path.startswith("/api/v1/list-subscriptions")
                    or "/endpoint-keys" in path
                    or "/endpoint-operations" in path
                    or path.endswith("/widgets")
                    or path.endswith("/state/exports")
                    or (path.startswith("/api/v1/plugin-operations/") and path.endswith("/export"))
                ):
                    self._send_err(404, "not_found", "Phase 2 deferred route")
                    return

                # Jellyfin namespace collision guard vs /plugin-apps/:id/:surfaceId
                if path.startswith("/plugins/"):
                    self._send_json(404, {"error": "not found"})
                    return

                if path.startswith("/plugin-apps/"):
                    self._send_raw(
                        200,
                        b"<!doctype html><html><body><div id='root'></div></body></html>",
                        "text/html; charset=utf-8",
                        head_only=head_only,
                    )
                    return

                self._send_err(404, "not_found", f"No stub route for {path}")

            def do_POST(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                path = parsed.path
                raw_body = self._read_body()

                if path == "/api/v1/auth/login":
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    username = req.get("username", "")
                    password = req.get("password", "")
                    with parent._lock:
                        u = parent.users.get(username)
                        if not u or u.get("password") != password:
                            self._send_err(401, "auth.invalid", "Invalid credentials")
                            return
                        tok = f"stub-session-{uuid.uuid4().hex}"
                        parent.tokens[tok] = username
                        user_view = {
                            "id": u["id"],
                            "login": u["login"],
                            "role": u["role"],
                            "enabled": u["enabled"],
                        }
                    self._send_ok(
                        {"token": tok, "user": user_view},
                        extra_headers=[
                            (
                                "Set-Cookie",
                                f"mc_session={tok}; Path=/; HttpOnly; SameSite=Lax",
                            )
                        ],
                    )
                    return

                if path == "/api/v1/auth/logout":
                    auth = self.headers.get("Authorization", "")
                    tok = auth[len("Bearer ") :].strip() if auth.startswith("Bearer ") else None
                    ck = _parse_cookies(self.headers.get("Cookie")).get("mc_session")
                    with parent._lock:
                        if tok:
                            parent.tokens.pop(tok, None)
                        if ck:
                            parent.tokens.pop(ck, None)
                    self._send_ok(
                        None,
                        extra_headers=[
                            (
                                "Set-Cookie",
                                "mc_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
                            )
                        ],
                    )
                    return

                if path == "/api/v1/users":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    login_name = req["login"]
                    password = req["password"]
                    with parent._lock:
                        uid = str(uuid.uuid4())
                        user_obj = {
                            "id": uid,
                            "login": login_name,
                            "role": "member",
                            "enabled": True,
                            "password": password,
                        }
                        parent.users[login_name] = user_obj
                    self._send_ok(
                        {
                            "id": uid,
                            "login": login_name,
                            "role": "member",
                            "enabled": True,
                        },
                        status=201,
                    )
                    return

                if path == "/api/v1/plugin-trust-keys/prepare":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    pub_key = str(req.get("public_key", ""))
                    if not pub_key.strip():
                        self._send_err(400, "trust_key.invalid", "Missing public_key")
                        return
                    fp = hashlib.sha256(pub_key.encode("utf-8")).hexdigest()
                    self._send_ok(
                        {
                            "public_key": pub_key,
                            "fingerprint": fp,
                            "consent_token": f"consent-{uuid.uuid4().hex}",
                        }
                    )
                    return

                if path == "/api/v1/plugin-trust-keys":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    if not req.get("reauth_token"):
                        self._send_err(401, "auth.reauth_required", "Missing reauth_token")
                        return
                    pub_key = str(req.get("public_key", ""))
                    fp = str(req.get("fingerprint") or hashlib.sha256(pub_key.encode("utf-8")).hexdigest())
                    label = str(req.get("label") or "stub-key")
                    key_id = fp[:16].upper()
                    with parent._lock:
                        entry = {
                            "id": str(uuid.uuid4()),
                            "key_id": key_id,
                            "fingerprint": fp,
                            "public_key": pub_key,
                            "label": label,
                            "status": "trusted",
                            "revision": 1,
                            "associated_package_count": 0,
                        }
                        parent.trust_keys.append(entry)
                    self._send_ok(entry, status=201)
                    return

                if path == "/api/v1/plugin-reauth":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    if req.get("password") != parent.admin_password:
                        self._send_err(401, "auth.invalid", "Invalid admin password")
                        return
                    purpose = req.get("purpose")
                    if purpose not in VALID_REAUTH_PURPOSES:
                        self._send_err(400, "auth.invalid", f"Invalid reauth purpose: {purpose!r}")
                        return
                    self._send_ok(
                        {
                            "reauth_token": f"reauth-{uuid.uuid4().hex}",
                            "purpose": purpose,
                            "expires_at": "2099-01-01T00:00:00Z",
                        }
                    )
                    return

                if path == "/api/v1/settings/plugins/developer/enable":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    if not req.get("reauth_token"):
                        self._send_err(403, "plugin.reauth_required", "Missing reauth_token")
                        return
                    with parent._lock:
                        parent.dev_mode = True
                        parent.dev_rev += 1
                        self._send_ok(
                            {"developer_mode": parent.dev_mode, "revision": parent.dev_rev}
                        )
                    return

                if path == "/api/v1/settings/plugins/developer/disable":
                    if self._require_admin() is None:
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    if "reauth_token" in req:
                        self._send_err(
                            400,
                            "plugin.invalid_input",
                            "developer/disable does not accept reauth_token",
                        )
                        return
                    with parent._lock:
                        parent.dev_mode = False
                        parent.dev_rev += 1
                        self._send_ok(
                            {"developer_mode": parent.dev_mode, "revision": parent.dev_rev}
                        )
                    return

                if path == "/api/v1/plugin-uploads":
                    if self._require_admin() is None:
                        return
                    ct = self.headers.get("Content-Type", "")
                    fields, files = _parse_multipart_form(ct, raw_body)
                    upload_id = fields.get("upload_id") or str(uuid.uuid4())
                    pkg_bytes = files.get("file", ("", b""))[1]
                    pkg_hash = hashlib.sha256(pkg_bytes).hexdigest()
                    sig_status = "verified" if "signature" in files else "unsigned"
                    self._send_ok(
                        {
                            "upload_id": upload_id,
                            "hash": pkg_hash,
                            "signature_status": sig_status,
                            "expires_at": "2099-01-01T00:00:00Z",
                        },
                        status=201,
                    )
                    return

                self._send_err(404, "not_found", f"No stub POST route for {path}")

            def do_PATCH(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                self._read_body()
                self._send_err(404, "not_found", f"No stub PATCH route for {parsed.path}")

        return Handler

    def start(self) -> str:
        if self._server is not None:
            return self.url
        self._server = _ThreadedStubHTTPServer(("127.0.0.1", 0), self._make_handler())
        self.port = int(self._server.server_address[1])
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            kwargs={"poll_interval": 0.05},
            daemon=True,
        )
        self._thread.start()
        return self.url

    def stop(self) -> None:
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            self._thread = None
        self.port = None

    def __enter__(self) -> SelfTestStubServer:
        self.start()
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.stop()
