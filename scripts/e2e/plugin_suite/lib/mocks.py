"""Local loopback mock servers for opaque-box E2E tests (Python 3.9+ stdlib).

Provides:
- MockQbitServer: stateful qBittorrent WebUI /api/v2/* emulator
- MockPtSiteServer: NexusPHP + M-Team API + redirect/SSRF/header-spy emulator
- MockObscuraCdpServer: HTTP /json/version + RFC 6455 WebSocket CDP emulator
"""
from __future__ import annotations

import base64
from email.parser import BytesParser
import hashlib
import html
import http.server
import json
from pathlib import Path
import socket
import socketserver
import struct
import sys
import threading
from typing import Any, Dict, List, Optional, Set, Tuple
import urllib.parse

from .fixtures import (
    TorrentFixture,
    bencode_decode,
    bencode_encode,
    build_torrent_bytes,
)


class _ThreadedHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128

    def handle_error(self, request: Any, client_address: Any) -> None:
        _, exc, _ = sys.exc_info()
        if isinstance(exc, (BrokenPipeError, ConnectionResetError, ConnectionAbortedError)):
            return
        super().handle_error(request, client_address)


def _parse_cookies(cookie_header: Optional[str]) -> Dict[str, str]:
    out: Dict[str, str] = {}
    if not cookie_header:
        return out
    for part in cookie_header.split(";"):
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def _parse_multipart_form(
    content_type: str, body: bytes
) -> Tuple[Dict[str, str], Dict[str, Tuple[str, bytes]]]:
    raw_msg = (
        f"Content-Type: {content_type}\r\nMIME-Version: 1.0\r\n\r\n".encode("utf-8")
        + body
    )
    msg = BytesParser().parsebytes(raw_msg)
    fields: Dict[str, str] = {}
    files: Dict[str, Tuple[str, bytes]] = {}
    if not msg.is_multipart():
        return fields, files
    for part in msg.get_payload():
        name = part.get_param("name", header="content-disposition")
        if not name:
            continue
        filename = part.get_filename()
        payload = part.get_payload(decode=True) or b""
        if filename is not None:
            files[name] = (filename, payload)
        else:
            fields[name] = payload.decode("utf-8", errors="replace")
    return fields, files


def _torrent_info_from_bytes(raw: bytes) -> Tuple[str, str, int]:
    """Return (info_hash, name, size) from .torrent bytes, falling back to sha1."""
    try:
        root = bencode_decode(raw)
        if isinstance(root, dict) and isinstance(root.get(b"info"), dict):
            info = root[b"info"]
            info_hash = hashlib.sha1(bencode_encode(info)).hexdigest()
            name_bytes = info.get(b"name", b"torrent")
            name = (
                name_bytes.decode("utf-8", errors="replace")
                if isinstance(name_bytes, bytes)
                else str(name_bytes)
            )
            if isinstance(info.get(b"length"), int):
                size = int(info[b"length"])
            elif isinstance(info.get(b"files"), list):
                size = sum(
                    int(item.get(b"length", 0))
                    for item in info[b"files"]
                    if isinstance(item, dict)
                )
            else:
                size = len(raw)
            return info_hash, name, size
    except Exception:
        pass
    return hashlib.sha1(raw).hexdigest(), "upload.torrent", len(raw)


def _hash_from_url(url: str) -> str:
    lower = url.lower()
    marker = "urn:btih:"
    if marker in lower:
        start = lower.index(marker) + len(marker)
        rest = lower[start:].split("&", 1)[0].strip()
        if len(rest) == 40:
            return rest
    return hashlib.sha1(url.encode("utf-8")).hexdigest()


def _resolve_real_save_dir(real_save_dir: Path, save_path: str) -> Path:
    base = real_save_dir.resolve()
    raw = save_path.replace("\x00", "_").replace("\\", "/")
    if raw.startswith("/downloads/"):
        raw = raw[len("/downloads/") :]
    elif raw == "/downloads":
        raw = ""
    parts = [p for p in raw.split("/") if p and p not in (".", "..")]
    candidate = (base.joinpath(*parts) if parts else base).resolve()
    if candidate != base and base not in candidate.parents:
        return base
    return candidate


def _sanitize_torrent_filename(name: str, fallback_hash: str) -> str:
    cleaned = name.replace("\x00", "_").replace("/", "_").replace("\\", "_")
    while ".." in cleaned:
        cleaned = cleaned.replace("..", "_")
    cleaned = cleaned.strip().strip(".")[:120]
    return cleaned or fallback_hash[:16]


# ---------------------------------------------------------------------------
# Mock qBittorrent WebUI Server (/api/v2/*)
# ---------------------------------------------------------------------------


class MockQbitServer:
    """Stateful qBittorrent WebUI API v2 emulator bound to 127.0.0.1:0."""

    def __init__(
        self,
        username: str = "admin",
        password: str = "adminadmin",
        require_auth: bool = True,
        disk_free_bytes: Optional[int] = 100 * 1024 ** 3,
        disk_endpoint_error: bool = False,
        real_save_dir: Optional[Path] = None,
    ) -> None:
        self.username = username
        self.password = password
        self.require_auth = require_auth
        self._sid_seq = 1
        self.sid: Optional[str] = "e2e-qbit-sid-0001"
        self.force_403_once = False
        self.reject_next_add = False
        self.drop_response_on_next_add = False
        self.drop_response_on_next_delete = False
        self.disk_free_bytes = disk_free_bytes
        self.disk_endpoint_error = disk_endpoint_error
        self.real_save_dir = Path(real_save_dir) if real_save_dir is not None else None

        self._lock = threading.Lock()
        self.torrents: Dict[str, Dict[str, Any]] = {}
        self.files: Dict[str, List[Dict[str, Any]]] = {}
        self._real_files: Dict[str, Path] = {}
        self.preferences: Dict[str, Any] = {"dl_limit": 0, "up_limit": 0}
        self.deleted_log: List[Dict[str, Any]] = []
        self.request_log: List[Dict[str, Any]] = []

        self._server: Optional[_ThreadedHTTPServer] = None
        self._thread: Optional[threading.Thread] = None
        self.port: Optional[int] = None

    @property
    def url(self) -> str:
        if self.port is None:
            raise RuntimeError("MockQbitServer is not running")
        return f"http://127.0.0.1:{self.port}"

    def trigger_relogin_on_next_call(self) -> None:
        with self._lock:
            if self.require_auth:
                self.sid = None
            else:
                self.force_403_once = True

    def seed_foreign_torrent(
        self,
        info_hash: str,
        name: str = "foreign-release",
        size: int = 1048576,
        tags: str = "",
        category: str = "",
        save_path: str = "/downloads/foreign",
        state: str = "uploading",
        progress: float = 1.0,
        uploaded: int = 0,
        files: Optional[List[Dict[str, Any]]] = None,
    ) -> None:
        with self._lock:
            self.torrents[info_hash.lower()] = {
                "hash": info_hash.lower(),
                "name": name,
                "size": size,
                "progress": progress,
                "save_path": save_path,
                "tags": tags,
                "category": category,
                "uploaded": uploaded,
                "state": state,
                "downloaded": int(size * progress),
                "dlspeed": 0,
                "upspeed": 0,
            }
            self.files[info_hash.lower()] = files or [
                {"index": 0, "name": f"{name}.mkv", "progress": progress, "priority": 1}
            ]

    def _make_handler(self) -> Any:
        parent = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: Any) -> None:
                return

            def _read_body(self) -> bytes:
                length = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(length) if length > 0 else b""

            def _send(
                self,
                status: int,
                body: bytes,
                content_type: str = "text/plain; charset=utf-8",
                extra_headers: Optional[List[Tuple[str, str]]] = None,
            ) -> None:
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def _check_auth(self) -> bool:
                with parent._lock:
                    if parent.force_403_once:
                        parent.force_403_once = False
                        self._send(403, b"Forbidden")
                        return False
                    if not parent.require_auth:
                        return True
                    cookies = _parse_cookies(self.headers.get("Cookie"))
                    if parent.sid is None or cookies.get("SID") != parent.sid:
                        self._send(403, b"Forbidden")
                        return False
                return True

            def _record(self, method: str, path: str, detail: Any = None) -> None:
                with parent._lock:
                    parent.request_log.append(
                        {"method": method, "path": path, "detail": detail}
                    )

            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                qs = urllib.parse.parse_qs(parsed.query)
                if not self._check_auth():
                    return

                if parsed.path == "/api/v2/torrents/info":
                    self._record("GET", parsed.path, qs)
                    hashes_filter = None
                    if "hashes" in qs and qs["hashes"][0]:
                        hashes_filter = {
                            h.strip().lower() for h in qs["hashes"][0].split("|") if h.strip()
                        }
                    tag_filter = qs.get("tag", [None])[0]
                    cat_filter = qs.get("category", [None])[0]

                    with parent._lock:
                        result = []
                        for t in parent.torrents.values():
                            if hashes_filter is not None and t["hash"].lower() not in hashes_filter:
                                continue
                            if tag_filter:
                                tags = {x.strip() for x in t.get("tags", "").split(",") if x.strip()}
                                if tag_filter not in tags:
                                    continue
                            if cat_filter is not None and t.get("category", "") != cat_filter:
                                continue
                            result.append(dict(t))
                    self._send(
                        200,
                        json.dumps(result).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/api/v2/torrents/files":
                    h = qs.get("hash", [""])[0].lower()
                    self._record("GET", parsed.path, {"hash": h})
                    with parent._lock:
                        items = list(parent.files.get(h, []))
                    self._send(
                        200,
                        json.dumps(items).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/api/v2/app/preferences":
                    self._record("GET", parsed.path)
                    with parent._lock:
                        prefs = dict(parent.preferences)
                    self._send(
                        200,
                        json.dumps(prefs).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/api/v2/sync/maindata":
                    self._record("GET", parsed.path, qs)
                    with parent._lock:
                        err = parent.disk_endpoint_error
                        free_bytes = parent.disk_free_bytes
                    if err:
                        self._send(500, b"Internal Server Error")
                        return
                    server_state: Dict[str, Any] = {}
                    if free_bytes is not None:
                        server_state["free_space_on_disk"] = free_bytes
                    self._send(
                        200,
                        json.dumps({"rid": 1, "full_update": True, "server_state": server_state}).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                self._send(404, b"Not Found")

            def do_POST(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                raw_body = self._read_body()
                ct = self.headers.get("Content-Type", "")

                if parsed.path == "/api/v2/auth/login":
                    form = urllib.parse.parse_qs(raw_body.decode("utf-8", errors="replace"))
                    user = form.get("username", [""])[0]
                    pw = form.get("password", [""])[0]
                    self._record("POST", parsed.path, {"username": user})
                    if user == parent.username and pw == parent.password:
                        with parent._lock:
                            parent._sid_seq += 1
                            parent.sid = f"e2e-qbit-sid-{parent._sid_seq:04d}"
                            issued_sid = parent.sid
                        self._send(
                            200,
                            b"Ok.",
                            extra_headers=[
                                ("Set-Cookie", f"SID={issued_sid}; Path=/; HttpOnly")
                            ],
                        )
                    else:
                        self._send(200, b"Fails.")
                    return

                if not self._check_auth():
                    return

                if parsed.path == "/api/v2/torrents/add":
                    if "multipart/form-data" in ct:
                        fields, files = _parse_multipart_form(ct, raw_body)
                    else:
                        qs_form = urllib.parse.parse_qs(
                            raw_body.decode("utf-8", errors="replace")
                        )
                        fields = {k: v[0] for k, v in qs_form.items()}
                        files = {}

                    with parent._lock:
                        if parent.reject_next_add:
                            parent.reject_next_add = False
                            parent.request_log.append(
                                {"method": "POST", "path": parsed.path, "detail": {"rejected": True}}
                            )
                            self._send(200, b"Fails.")
                            return

                        should_drop = parent.drop_response_on_next_add
                        if should_drop:
                            parent.drop_response_on_next_add = False

                        paused = fields.get("paused") == "true" or fields.get("stopped") == "true"
                        tags = fields.get("tags", "")
                        category = fields.get("category", "")
                        save_path = fields.get("savepath", "/downloads")

                        added_hashes: List[str] = []
                        if "torrents" in files:
                            fname, tbytes = files["torrents"]
                            ih, name, size = _torrent_info_from_bytes(tbytes)
                            self._add_torrent_locked(
                                ih, name or fname, size, tags, category, save_path, paused
                            )
                            added_hashes.append(ih)
                        elif "urls" in fields:
                            for url in fields["urls"].splitlines():
                                u = url.strip()
                                if not u:
                                    continue
                                ih = _hash_from_url(u)
                                self._add_torrent_locked(
                                    ih, u[:64], 1048576, tags, category, save_path, paused
                                )
                                added_hashes.append(ih)

                    self._record(
                        "POST",
                        parsed.path,
                        {
                            "fields": fields,
                            "file_keys": list(files.keys()),
                            "added_hashes": added_hashes,
                        },
                    )
                    if should_drop:
                        self.close_connection = True
                        try:
                            self.connection.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                        self.connection.close()
                        return
                    self._send(200, b"Ok.")
                    return

                form = urllib.parse.parse_qs(raw_body.decode("utf-8", errors="replace"))

                if parsed.path == "/api/v2/torrents/filePrio":
                    h = form.get("hash", [""])[0].lower()
                    ids = {int(x) for x in form.get("id", [""])[0].split("|") if x.strip()}
                    prio = int(form.get("priority", ["0"])[0])
                    self._record("POST", parsed.path, {"hash": h, "ids": sorted(ids), "priority": prio})
                    with parent._lock:
                        for f in parent.files.get(h, []):
                            if f["index"] in ids:
                                f["priority"] = prio
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/addTags":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    new_tags = [t.strip() for t in form.get("tags", [""])[0].split(",") if t.strip()]
                    self._record("POST", parsed.path, {"hashes": hashes, "tags": new_tags})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if not t:
                                continue
                            existing = [x.strip() for x in t.get("tags", "").split(",") if x.strip()]
                            for nt in new_tags:
                                if nt not in existing:
                                    existing.append(nt)
                            t["tags"] = ",".join(existing)
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/removeTags":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    rem_tags = {t.strip() for t in form.get("tags", [""])[0].split(",") if t.strip()}
                    self._record("POST", parsed.path, {"hashes": hashes, "tags": sorted(rem_tags)})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if not t:
                                continue
                            existing = [
                                x.strip()
                                for x in t.get("tags", "").split(",")
                                if x.strip() and x.strip() not in rem_tags
                            ]
                            t["tags"] = ",".join(existing)
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/setCategory":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    category = form.get("category", [""])[0]
                    self._record("POST", parsed.path, {"hashes": hashes, "category": category})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if t:
                                t["category"] = category
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/setDownloadLimit":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    limit = int(form.get("limit", ["0"])[0])
                    self._record("POST", parsed.path, {"hashes": hashes, "limit": limit})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if t:
                                t["dl_limit"] = limit
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/setUploadLimit":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    limit = int(form.get("limit", ["0"])[0])
                    self._record("POST", parsed.path, {"hashes": hashes, "limit": limit})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if t:
                                t["up_limit"] = limit
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/delete":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    delete_files = form.get("deleteFiles", ["false"])[0].lower() == "true"
                    self._record("POST", parsed.path, {"hashes": hashes, "deleteFiles": delete_files})
                    with parent._lock:
                        should_drop = parent.drop_response_on_next_delete
                        if should_drop:
                            parent.drop_response_on_next_delete = False
                        parent.deleted_log.append(
                            {"hashes": hashes, "deleteFiles": delete_files}
                        )
                        for h in hashes:
                            removed = parent.torrents.pop(h, None)
                            parent.files.pop(h, None)
                            tracked_file = parent._real_files.pop(h, None)
                            if delete_files and removed and parent.real_save_dir is not None:
                                target_file = tracked_file
                                if target_file is None:
                                    target_dir = _resolve_real_save_dir(
                                        parent.real_save_dir, removed.get("save_path", "")
                                    )
                                    safe_name = _sanitize_torrent_filename(
                                        removed.get("name", ""), h
                                    )
                                    target_file = target_dir / f"{safe_name}.mkv"
                                if target_file.is_file():
                                    target_file.unlink()
                    if should_drop:
                        self.close_connection = True
                        try:
                            self.connection.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                        self.connection.close()
                        return
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/pause":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    self._record("POST", parsed.path, {"hashes": hashes})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if t:
                                t["state"] = "pausedUP" if t.get("progress", 0.0) >= 1.0 else "pausedDL"
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/torrents/resume":
                    hashes = [h.strip().lower() for h in form.get("hashes", [""])[0].split("|") if h.strip()]
                    self._record("POST", parsed.path, {"hashes": hashes})
                    with parent._lock:
                        for h in hashes:
                            t = parent.torrents.get(h)
                            if t:
                                t["state"] = "uploading" if t.get("progress", 0.0) >= 1.0 else "downloading"
                    self._send(200, b"")
                    return

                if parsed.path == "/api/v2/app/setPreferences":
                    raw_json = form.get("json", ["{}"])[0]
                    payload = json.loads(raw_json)
                    self._record("POST", parsed.path, payload)
                    with parent._lock:
                        parent.preferences.update(payload)
                    self._send(200, b"")
                    return

                self._send(404, b"Not Found")

            def _add_torrent_locked(
                self,
                info_hash: str,
                name: str,
                size: int,
                tags: str,
                category: str,
                save_path: str,
                paused: bool,
            ) -> None:
                key = info_hash.lower()
                if key in parent.torrents:
                    return
                if parent.real_save_dir is not None:
                    real_dir = _resolve_real_save_dir(parent.real_save_dir, save_path)
                    real_dir.mkdir(parents=True, exist_ok=True)
                    safe_name = _sanitize_torrent_filename(name, key)
                    target_file = real_dir / f"{safe_name}.mkv"
                    target_file.write_bytes(b"e2e-torrent-data")
                    parent._real_files[key] = target_file
                    eff_save_path = str(real_dir)
                else:
                    eff_save_path = save_path
                parent.torrents[key] = {
                    "hash": key,
                    "name": name,
                    "size": size,
                    "progress": 0.0,
                    "save_path": eff_save_path,
                    "tags": tags,
                    "category": category,
                    "uploaded": 0,
                    "state": "pausedDL" if paused else "downloading",
                    "downloaded": 0,
                    "dlspeed": 0,
                    "upspeed": 0,
                }
                parent.files[key] = [
                    {"index": 0, "name": f"{name}.mkv", "progress": 0.0, "priority": 1}
                ]

        return Handler

    def start(self) -> str:
        if self._server is not None:
            return self.url
        self._server = _ThreadedHTTPServer(("127.0.0.1", 0), self._make_handler())
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

    def __enter__(self) -> MockQbitServer:
        self.start()
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.stop()


# ---------------------------------------------------------------------------
# Mock PT Site Server (NexusPHP + M-Team API + Redirect / SSRF Spy)
# ---------------------------------------------------------------------------


class MockPtSiteServer:
    """Emulates NexusPHP HTML/RSS/Check-in, M-Team API, and redirect/SSRF targets."""

    def __init__(
        self,
        expected_cookie: Optional[str] = None,
        expected_api_key: Optional[str] = None,
    ) -> None:
        self.expected_cookie = expected_cookie
        self.expected_api_key = expected_api_key
        self.auth_expired = False
        self.challenge_mode = False
        self.checkin_mode = "success"  # success | already | failure | challenge
        self.checkin_rotate_cookie: Optional[str] = None
        self.drop_response_on_next_checkin = False
        self.downgrade_target_url: Optional[str] = None
        self.webhook_fail_next = 0

        self._lock = threading.Lock()
        self.default_torrent: TorrentFixture = build_torrent_bytes(
            name="E2E.Demo.Release.1080p",
            length=1048576,
        )
        self.torrent_payloads: Dict[str, bytes] = {}
        self.torrents: List[Dict[str, Any]] = [
            {
                "id": "101",
                "title": "E2E.Demo.Release.1080p.WEB-DL",
                "size": "1.00 GiB",
                "size_bytes": 1073741824,
                "seeders": 24,
                "leechers": 2,
                "snatched": 15,
                "upload_time": "2026-11-20 12:00:00",
                "category": "Movie",
                "free": True,
                "hr": False,
            }
        ]
        self.spy_requests: List[Dict[str, Any]] = []
        self.request_log: List[Dict[str, Any]] = []
        self.checkin_post_count = 0
        self.checkin_requests: List[Dict[str, Any]] = []
        self.webhook_events: List[Dict[str, Any]] = []

        self._server: Optional[_ThreadedHTTPServer] = None
        self._thread: Optional[threading.Thread] = None
        self.port: Optional[int] = None

    @property
    def url(self) -> str:
        if self.port is None:
            raise RuntimeError("MockPtSiteServer is not running")
        return f"http://127.0.0.1:{self.port}"

    def _is_cookie_valid(self, headers: Any) -> bool:
        if self.auth_expired:
            return False
        if not self.expected_cookie:
            return True
        expected_pairs = _parse_cookies(self.expected_cookie)
        actual_pairs = _parse_cookies(headers.get("Cookie"))
        if not expected_pairs:
            return False
        return all(actual_pairs.get(k) == v for k, v in expected_pairs.items())

    def _make_handler(self) -> Any:
        parent = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: Any) -> None:
                return

            def _read_body(self) -> bytes:
                length = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(length) if length > 0 else b""

            def _send(
                self,
                status: int,
                body: bytes,
                content_type: str = "text/html; charset=utf-8",
                extra_headers: Optional[List[Tuple[str, str]]] = None,
            ) -> None:
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def _record(self, method: str, path: str) -> None:
                with parent._lock:
                    parent.request_log.append(
                        {
                            "method": method,
                            "path": path,
                            "headers": {k.lower(): v for k, v in self.headers.items()},
                        }
                    )

            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                qs = urllib.parse.parse_qs(parsed.query)
                self._record("GET", self.path)

                if parsed.path == "/spy":
                    with parent._lock:
                        parent.spy_requests.append(
                            {
                                "method": "GET",
                                "path": self.path,
                                "headers": {k.lower(): v for k, v in self.headers.items()},
                            }
                        )
                    self._send(
                        200,
                        json.dumps({"spied": True}).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path.startswith("/redirect/"):
                    tail = parsed.path[len("/redirect/") :]
                    if tail == "to":
                        target = qs.get("url", [f"{parent.url}/download.torrent"])[0]
                        self._send(302, b"", extra_headers=[("Location", target)])
                        return
                    try:
                        hops = int(tail)
                    except ValueError:
                        self._send(404, b"Bad redirect hop")
                        return
                    nxt = (
                        f"{parent.url}/redirect/{hops - 1}"
                        if hops > 1
                        else f"{parent.url}/download.torrent"
                    )
                    self._send(302, b"", extra_headers=[("Location", nxt)])
                    return

                if parsed.path == "/downgrade":
                    target = parent.downgrade_target_url or f"{parent.url}/spy"
                    self._send(302, b"", extra_headers=[("Location", target)])
                    return

                if parsed.path == "/oversized-response":
                    oversized_body = b"A" * (5 * 1024 * 1024 + 1024)
                    self._send(
                        200,
                        oversized_body,
                        content_type="text/plain; charset=utf-8",
                    )
                    return

                if parsed.path == "/torrents/oversized.torrent":
                    with parent._lock:
                        payload = parent.torrent_payloads.get("oversized")
                    if payload is None:
                        payload = b"x" * (10 * 1024 * 1024 + 1024)
                    self._send(
                        200,
                        payload,
                        content_type="application/x-bittorrent",
                    )
                    return

                if (
                    parsed.path == "/download.torrent"
                    or parsed.path.endswith(".torrent")
                    or parsed.path.startswith("/download/")
                ):
                    tid = parsed.path.rsplit("/", 1)[-1].replace(".torrent", "")
                    with parent._lock:
                        payload = parent.torrent_payloads.get(
                            tid, parent.default_torrent.bytes
                        )
                    self._send(
                        200,
                        payload,
                        content_type="application/x-bittorrent",
                    )
                    return

                if parsed.path == "/api/search":
                    if not parent._is_cookie_valid(self.headers):
                        self._send(403, b"Forbidden")
                        return
                    self._send(200, b"E2E.Demo.Release")
                    return

                if parsed.path in ("/", "/index.php"):
                    if parent.challenge_mode:
                        self._send(
                            200,
                            b"<html><body><div class='cf-chl-widget'>Checking your browser</div></body></html>",
                        )
                        return
                    if not parent._is_cookie_valid(self.headers):
                        self._send(
                            200,
                            b"<html><body><form action='login.php'><input type='password' name='password'/></form></body></html>",
                        )
                        return
                    self._send(
                        200,
                        b"<html><body><a href='logout.php'>Logout</a><p>Welcome</p></body></html>",
                    )
                    return

                if parsed.path == "/torrents.php":
                    if not parent._is_cookie_valid(self.headers):
                        self._send(
                            200,
                            b"<html><body><form action='login.php'><input type='password'/></form></body></html>",
                        )
                        return
                    kw = qs.get("search", [""])[0].lower()
                    rows: List[str] = []
                    with parent._lock:
                        items = list(parent.torrents)
                    for item in items:
                        if kw and kw not in item["title"].lower():
                            continue
                        free_img = "<img class='pro_free' alt='Free'/>" if item.get("free") else ""
                        if item.get("free_until"):
                            free_img += f"<span class='free_until' data-free-until='{html.escape(str(item['free_until']))}'></span>"
                        hr_img = "<img class='hitandrun' alt='H&R'/>" if item.get("hr") else ""
                        dl_href = item.get("magnet") or f"{parent.url}/torrents/{item['id']}.torrent"
                        rows.append(
                            f"<tr class='torrent'>"
                            f"<td class='category'>{html.escape(item['category'])}</td>"
                            f"<td><a class='name' title='{html.escape(item['title'])}' href='details.php?id={item['id']}'>{html.escape(item['title'])}</a>"
                            f"{free_img}{hr_img}"
                            f"<a class='download' href='{html.escape(dl_href)}'>DL</a></td>"
                            f"<td class='upload_time'>{html.escape(item['upload_time'])}</td>"
                            f"<td class='size'>{html.escape(item['size'])}</td>"
                            f"<td class='seeders'>{item['seeders']}</td>"
                            f"<td class='leechers'>{item['leechers']}</td>"
                            f"<td class='snatched'>{item['snatched']}</td>"
                            f"</tr>"
                        )
                    doc = (
                        "<html><body><a href='logout.php'>Logout</a>"
                        f"<table class='torrents'>{''.join(rows)}</table></body></html>"
                    )
                    self._send(200, doc.encode("utf-8"))
                    return

                if parsed.path == "/rss.php":
                    items_xml: List[str] = []
                    with parent._lock:
                        items = list(parent.torrents)
                    for item in items:
                        dl_url = item.get("magnet") or f"{parent.url}/torrents/{item['id']}.torrent"
                        enc_type = "application/x-bittorrent"
                        free_until_tag = (
                            f"<freeUntil>{html.escape(str(item['free_until']))}</freeUntil>"
                            if item.get("free_until")
                            else ""
                        )
                        items_xml.append(
                            f"<item>"
                            f"<title>{html.escape(item['title'])}</title>"
                            f"<guid>{html.escape(item['id'])}</guid>"
                            f"<enclosure url='{html.escape(dl_url)}' length='{item['size_bytes']}' type='{enc_type}'/>"
                            f"<seeders>{item['seeders']}</seeders>"
                            f"<free>{1 if item.get('free') else 0}</free>"
                            f"<hr>{1 if item.get('hr') else 0}</hr>"
                            f"{free_until_tag}"
                            f"</item>"
                        )
                    rss = (
                        "<?xml version='1.0' encoding='utf-8'?>"
                        f"<rss version='2.0'><channel><title>E2E Mock PT</title>{''.join(items_xml)}</channel></rss>"
                    )
                    self._send(200, rss.encode("utf-8"), content_type="application/rss+xml; charset=utf-8")
                    return

                self._send(404, b"Not Found")

            def do_POST(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                raw_body = self._read_body()
                self._record("POST", self.path)

                if parsed.path == "/spy":
                    with parent._lock:
                        parent.spy_requests.append(
                            {
                                "method": "POST",
                                "path": self.path,
                                "headers": {k.lower(): v for k, v in self.headers.items()},
                                "body": raw_body.decode("utf-8", errors="replace"),
                            }
                        )
                    self._send(
                        200,
                        json.dumps({"spied": True}).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/webhook/events":
                    with parent._lock:
                        if parent.webhook_fail_next > 0:
                            parent.webhook_fail_next -= 1
                            self._send(500, b'{"ok":false}', content_type="application/json")
                            return
                        try:
                            evt = json.loads(raw_body.decode("utf-8") or "{}")
                        except ValueError:
                            evt = {"raw": raw_body.decode("utf-8", errors="replace")}
                        parent.webhook_events.append(evt)
                    self._send(200, b'{"received":true}', content_type="application/json")
                    return

                if parsed.path == "/attendance.php":
                    if not parent._is_cookie_valid(self.headers):
                        self._send(
                            200,
                            b"<html><body><form action='login.php'><input type='password'/></form></body></html>",
                        )
                        return
                    with parent._lock:
                        parent.checkin_post_count += 1
                        parent.checkin_requests.append(
                            {
                                "path": self.path,
                                "headers": {k.lower(): v for k, v in self.headers.items()},
                                "body": raw_body.decode("utf-8", errors="replace"),
                            }
                        )
                        should_drop = parent.drop_response_on_next_checkin
                        if should_drop:
                            parent.drop_response_on_next_checkin = False
                    if should_drop:
                        self.close_connection = True
                        try:
                            self.connection.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                        self.connection.close()
                        return
                    extra_headers: List[Tuple[str, str]] = []
                    if parent.checkin_rotate_cookie:
                        extra_headers.append(
                            ("Set-Cookie", f"{parent.checkin_rotate_cookie}; Path=/; HttpOnly")
                        )
                    mode = parent.checkin_mode
                    if mode == "challenge":
                        body = "<html><body><div class='cf-chl-'>Just a moment</div></body></html>"
                    elif mode == "already":
                        body = "<html><body><div class='text'>您今天已经签到过了</div></body></html>"
                    elif mode == "failure":
                        body = "<html><body><div class='text'>签到失败</div></body></html>"
                    else:
                        body = "<html><body><div class='text'>签到成功，获得100魔力值</div></body></html>"
                    self._send(200, body.encode("utf-8"), extra_headers=extra_headers)
                    return

                if parsed.path == "/api/torrent/search":
                    api_key = self.headers.get("x-api-key")
                    if parent.expected_api_key and api_key != parent.expected_api_key:
                        self._send(
                            401,
                            json.dumps({"code": "401", "message": "Invalid API key"}).encode("utf-8"),
                            content_type="application/json",
                        )
                        return
                    req = json.loads(raw_body.decode("utf-8") or "{}")
                    kw = str(req.get("keyword", "")).lower()
                    with parent._lock:
                        items = list(parent.torrents)
                    data_items = []
                    for item in items:
                        if kw and kw not in item["title"].lower():
                            continue
                        status_obj: Dict[str, Any] = {
                            "seeders": str(item["seeders"]),
                            "leechers": str(item["leechers"]),
                            "snatched": str(item["snatched"]),
                            "discount": "FREE" if item.get("free") else "NORMAL",
                            "hr": bool(item.get("hr", False)),
                        }
                        if item.get("free_until"):
                            status_obj["discountEndTime"] = str(item["free_until"])
                        data_items.append(
                            {
                                "id": str(item["id"]),
                                "name": item["title"],
                                "size": str(item["size_bytes"]),
                                "createdDate": item["upload_time"],
                                "imageList": [],
                                "status": status_obj,
                            }
                        )
                    payload = {"code": "0", "message": None, "data": {"data": data_items}}
                    self._send(
                        200,
                        json.dumps(payload).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/api/torrent/genDlToken":
                    api_key = self.headers.get("x-api-key")
                    if parent.expected_api_key and api_key != parent.expected_api_key:
                        self._send(
                            401,
                            json.dumps({"code": "401", "message": "Invalid API key"}).encode("utf-8"),
                            content_type="application/json",
                        )
                        return
                    form = urllib.parse.parse_qs(raw_body.decode("utf-8", errors="replace"))
                    tid = form.get("id", ["101"])[0]
                    dl_url = f"{parent.url}/torrents/{tid}.torrent"
                    payload = {"code": "0", "message": None, "data": dl_url}
                    self._send(
                        200,
                        json.dumps(payload).encode("utf-8"),
                        content_type="application/json",
                    )
                    return

                if parsed.path == "/api/search":
                    if not parent._is_cookie_valid(self.headers):
                        self._send(403, b"Forbidden")
                        return
                    self._send(200, b"E2E.Demo.Release")
                    return

                self._send(404, b"Not Found")

        return Handler

    def start(self) -> str:
        if self._server is not None:
            return self.url
        self._server = _ThreadedHTTPServer(("127.0.0.1", 0), self._make_handler())
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

    def __enter__(self) -> MockPtSiteServer:
        self.start()
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.stop()


# ---------------------------------------------------------------------------
# Mock Obscura CDP Server (HTTP /json/version + RFC 6455 WebSocket)
# ---------------------------------------------------------------------------

_WS_MAGIC = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
MAX_WS_FRAME_BYTES = 16 * 1024 * 1024


class _WsFrameError(ValueError):
    """Raised on malformed or oversized RFC 6455 WebSocket frames."""

    def __init__(self, close_code: int, message: str) -> None:
        super().__init__(message)
        self.close_code = close_code


def _ws_recv_frame(
    sock: socket.socket,
    max_frame_bytes: int = MAX_WS_FRAME_BYTES,
) -> Optional[Tuple[bool, int, bytes]]:
    """Read one RFC 6455 WebSocket frame from socket; return (fin, opcode, payload) or None on EOF."""
    def _recv_exact(n: int) -> Optional[bytes]:
        if n < 0:
            raise _WsFrameError(1002, f"Negative WS frame length: {n}")
        if n > max_frame_bytes:
            raise _WsFrameError(1009, f"WS frame length {n} exceeds max {max_frame_bytes}")
        buf = bytearray()
        while len(buf) < n:
            chunk = sock.recv(min(65536, n - len(buf)))
            if not chunk:
                return None
            buf.extend(chunk)
        return bytes(buf)

    hdr = _recv_exact(2)
    if hdr is None:
        return None
    b1, b2 = hdr[0], hdr[1]
    fin = (b1 & 0x80) != 0
    opcode = b1 & 0x0F
    masked = (b2 & 0x80) != 0
    length = b2 & 0x7F
    if length == 126:
        ext = _recv_exact(2)
        if ext is None:
            return None
        length = struct.unpack("!H", ext)[0]
    elif length == 127:
        ext = _recv_exact(8)
        if ext is None:
            return None
        length = struct.unpack("!Q", ext)[0]
        if length >= (1 << 63):
            raise _WsFrameError(1002, "RFC 6455 64-bit length MSB must be 0")

    if length > max_frame_bytes:
        raise _WsFrameError(1009, f"WS frame length {length} exceeds max {max_frame_bytes}")

    mask_key = b""
    if masked:
        mk = _recv_exact(4)
        if mk is None:
            return None
        mask_key = mk

    payload = _recv_exact(length) if length > 0 else b""
    if payload is None:
        return None
    if masked:
        payload = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    return fin, opcode, payload


def _ws_send_frame(sock: socket.socket, opcode: int, payload: bytes) -> None:
    header = bytearray([0x80 | (opcode & 0x0F)])
    n = len(payload)
    if n < 126:
        header.append(n)
    elif n < 65536:
        header.append(126)
        header.extend(struct.pack("!H", n))
    else:
        header.append(127)
        header.extend(struct.pack("!Q", n))
    sock.sendall(bytes(header) + payload)


class MockObscuraCdpServer:
    """Emulates Obscura HTTP /json/version + RFC 6455 WebSocket CDP JSON-RPC."""

    def __init__(
        self,
        incompatible_mode: bool = False,
        post_nav_timeout: bool = False,
        html_content: str = "<html><body><a href='logout.php'>Logout</a><p>签到成功</p></body></html>",
        required_cdp_token: Optional[str] = None,
    ) -> None:
        self.incompatible_mode = incompatible_mode
        self.post_nav_timeout = post_nav_timeout
        self.html_content = html_content
        self.required_cdp_token = required_cdp_token
        self.ws_url_override: Optional[str] = None

        self._lock = threading.Lock()
        self._seq = 0
        self.cookies: List[Dict[str, Any]] = []
        self.rpc_log: List[Dict[str, Any]] = []
        self.eval_overrides: Dict[str, Any] = {}
        self.active_contexts: Set[str] = set()
        self.disposed_contexts: Set[str] = set()
        self.navigated_urls: List[str] = []

        self._server: Optional[_ThreadedHTTPServer] = None
        self._thread: Optional[threading.Thread] = None
        self.port: Optional[int] = None

    @property
    def url(self) -> str:
        if self.port is None:
            raise RuntimeError("MockObscuraCdpServer is not running")
        return f"http://127.0.0.1:{self.port}"

    @property
    def ws_url(self) -> str:
        if self.port is None:
            raise RuntimeError("MockObscuraCdpServer is not running")
        return self.ws_url_override or f"ws://127.0.0.1:{self.port}/ws/browser"

    def _next_id(self, prefix: str) -> str:
        with self._lock:
            self._seq += 1
            return f"{prefix}-{self._seq}"

    def _handle_cdp_rpc(
        self, req: Any, send_json: Any
    ) -> None:
        if not isinstance(req, dict):
            send_json(
                {
                    "id": None,
                    "error": {"code": -32600, "message": "Invalid Request"},
                }
            )
            return
        msg_id = req.get("id")
        method = req.get("method", "")
        raw_params = req.get("params")
        params = raw_params if isinstance(raw_params, dict) else {}
        session_id = req.get("sessionId")

        with self._lock:
            self.rpc_log.append(req)

        if method == "Target.createBrowserContext":
            if self.incompatible_mode:
                send_json(
                    {
                        "id": msg_id,
                        "error": {"code": -32601, "message": "Method not found"},
                    }
                )
                return
            ctx_id = self._next_id("ctx")
            with self._lock:
                self.active_contexts.add(ctx_id)
            send_json({"id": msg_id, "result": {"browserContextId": ctx_id}})
            return

        if method == "Target.createTarget":
            send_json({"id": msg_id, "result": {"targetId": self._next_id("tgt")}})
            return

        if method == "Target.attachToTarget":
            send_json({"id": msg_id, "result": {"sessionId": self._next_id("sid")}})
            return

        if method == "Target.disposeBrowserContext":
            ctx_id = params.get("browserContextId")
            if isinstance(ctx_id, str) and ctx_id:
                with self._lock:
                    self.active_contexts.discard(ctx_id)
                    self.disposed_contexts.add(ctx_id)
            send_json({"id": msg_id, "result": {}})
            return

        if method in (
            "Target.closeTarget",
            "Network.enable",
            "Page.enable",
            "Page.setLifecycleEventsEnabled",
        ):
            send_json({"id": msg_id, "result": {}})
            return

        if method == "Network.setCookies":
            raw_cookies = params.get("cookies", [])
            if isinstance(raw_cookies, list):
                with self._lock:
                    for ck in raw_cookies:
                        if isinstance(ck, dict):
                            self.cookies.append(dict(ck))
            send_json({"id": msg_id, "result": {}})
            return

        if method in ("Network.getCookies", "Network.getAllCookies"):
            with self._lock:
                cks = [dict(c) for c in self.cookies]
            send_json({"id": msg_id, "result": {"cookies": cks}})
            return

        if method == "Page.navigate":
            url_arg = params.get("url")
            if isinstance(url_arg, str) and url_arg:
                with self._lock:
                    self.navigated_urls.append(url_arg)
            loader_id = self._next_id("loader")
            send_json(
                {
                    "id": msg_id,
                    "result": {"frameId": "frame-1", "loaderId": loader_id},
                }
            )
            if not self.post_nav_timeout:
                evt: Dict[str, Any] = {
                    "method": "Page.lifecycleEvent",
                    "params": {"name": "load", "loaderId": loader_id},
                }
                if session_id is not None:
                    evt["sessionId"] = session_id
                send_json(evt)
            return

        if method == "Runtime.evaluate":
            expr = str(params.get("expression", ""))
            if expr in self.eval_overrides:
                val = self.eval_overrides[expr]
            elif "readyState" in expr:
                val = "complete"
            elif "outerHTML" in expr:
                val = self.html_content
            else:
                val = ""
            send_json(
                {
                    "id": msg_id,
                    "result": {"result": {"type": "string", "value": val}},
                }
            )
            return

        send_json(
            {
                "id": msg_id,
                "error": {"code": -32601, "message": f"Unsupported method: {method}"},
            }
        )

    def _make_handler(self) -> Any:
        parent = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: Any) -> None:
                return

            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                if parsed.path == "/json/version":
                    body = json.dumps(
                        {
                            "Browser": "Obscura/v0.2.3 Chrome/145",
                            "webSocketDebuggerUrl": parent.ws_url,
                        }
                    ).encode("utf-8")
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return

                if parsed.path == "/ws/browser":
                    if parent.required_cdp_token is not None:
                        qs = urllib.parse.parse_qs(parsed.query)
                        auth_hdr = self.headers.get("Authorization", "")
                        token_hdr = self.headers.get("X-Obscura-Token") or self.headers.get("X-CDP-Token")
                        qs_token = qs.get("token", [None])[0]
                        bearer = auth_hdr[7:].strip() if auth_hdr.startswith("Bearer ") else None
                        if parent.required_cdp_token not in (bearer, token_hdr, qs_token):
                            try:
                                self.send_response(401)
                                self.send_header("Content-Length", "0")
                                self.end_headers()
                            except (OSError, ValueError):
                                self.close_connection = True
                            return
                    ws_key = self.headers.get("Sec-WebSocket-Key")
                    if not ws_key:
                        try:
                            self.send_response(400)
                            self.send_header("Content-Length", "0")
                            self.end_headers()
                        except (OSError, ValueError):
                            self.close_connection = True
                        return

                    try:
                        ws_key_bytes = ws_key.encode("ascii")
                    except UnicodeEncodeError:
                        try:
                            self.send_response(400)
                            self.send_header("Content-Length", "0")
                            self.end_headers()
                        except (OSError, ValueError):
                            self.close_connection = True
                        return

                    self.close_connection = True
                    try:
                        accept = base64.b64encode(
                            hashlib.sha1(ws_key_bytes + _WS_MAGIC).digest()
                        ).decode("ascii")
                        self.send_response(101, "Switching Protocols")
                        self.send_header("Upgrade", "websocket")
                        self.send_header("Connection", "Upgrade")
                        self.send_header("Sec-WebSocket-Accept", accept)
                        self.end_headers()
                        self.wfile.flush()

                        sock = self.connection
                        send_lock = threading.Lock()

                        def send_json(obj: Dict[str, Any]) -> None:
                            data = json.dumps(obj).encode("utf-8")
                            with send_lock:
                                _ws_send_frame(sock, 0x1, data)

                        frag_opcode: Optional[int] = None
                        frag_chunks: List[bytes] = []
                        frag_bytes = 0
                        while True:
                            try:
                                frame = _ws_recv_frame(sock)
                            except _WsFrameError as exc:
                                try:
                                    with send_lock:
                                        _ws_send_frame(sock, 0x8, struct.pack("!H", exc.close_code))
                                except OSError:
                                    pass
                                break
                            if frame is None:
                                break
                            fin, opcode, payload = frame
                            if opcode == 0x8:  # Close
                                with send_lock:
                                    _ws_send_frame(sock, 0x8, b"")
                                break
                            if opcode == 0x9:  # Ping
                                with send_lock:
                                    _ws_send_frame(sock, 0xA, payload)
                                continue
                            if opcode == 0xA:  # Pong
                                continue
                            if opcode in (0x1, 0x2):
                                if frag_opcode is not None:
                                    with send_lock:
                                        _ws_send_frame(sock, 0x8, struct.pack("!H", 1002))
                                    break
                                if fin:
                                    full_payload = payload
                                    msg_opcode = opcode
                                else:
                                    frag_opcode = opcode
                                    frag_chunks = [payload]
                                    frag_bytes = len(payload)
                                    continue
                            elif opcode == 0x0:
                                if frag_opcode is None:
                                    with send_lock:
                                        _ws_send_frame(sock, 0x8, struct.pack("!H", 1002))
                                    break
                                frag_bytes += len(payload)
                                if frag_bytes > MAX_WS_FRAME_BYTES:
                                    with send_lock:
                                        _ws_send_frame(sock, 0x8, struct.pack("!H", 1009))
                                    break
                                frag_chunks.append(payload)
                                if not fin:
                                    continue
                                full_payload = b"".join(frag_chunks)
                                msg_opcode = frag_opcode
                                frag_opcode = None
                                frag_chunks = []
                                frag_bytes = 0
                            else:
                                with send_lock:
                                    _ws_send_frame(sock, 0x8, struct.pack("!H", 1003))
                                break

                            if msg_opcode == 0x1:  # Text
                                try:
                                    req = json.loads(full_payload.decode("utf-8"))
                                except ValueError:
                                    with send_lock:
                                        _ws_send_frame(sock, 0x8, struct.pack("!H", 1007))
                                    break
                                if not isinstance(req, dict):
                                    send_json(
                                        {
                                            "id": None,
                                            "error": {"code": -32600, "message": "Invalid Request"},
                                        }
                                    )
                                    continue
                                parent._handle_cdp_rpc(req, send_json)
                    except (OSError, ValueError, OverflowError):
                        self.close_connection = True
                    finally:
                        try:
                            sock = self.connection
                            old_timeout = sock.gettimeout()
                            try:
                                sock.settimeout(0.0)
                                for _ in range(4):
                                    if not sock.recv(65536):
                                        break
                            except OSError:
                                pass
                            finally:
                                sock.settimeout(old_timeout)
                        except OSError:
                            pass
                    return

                try:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                except (OSError, ValueError):
                    self.close_connection = True

        return Handler

    def start(self) -> str:
        if self._server is not None:
            return self.url
        self._server = _ThreadedHTTPServer(("127.0.0.1", 0), self._make_handler())
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

    def __enter__(self) -> MockObscuraCdpServer:
        self.start()
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.stop()
