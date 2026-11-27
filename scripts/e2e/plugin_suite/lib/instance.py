"""Isolated crawler-media instance supervisor for opaque-box E2E tests."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from typing import Any, Dict, List, Optional, Sequence

from .client import ApiClient, ApiError


def find_repo_root(start: Optional[Path] = None) -> Path:
    """Walk upward from this file (or start) to find Cargo.toml workspace root."""
    cur = (start or Path(__file__)).resolve()
    if cur.is_file():
        cur = cur.parent
    for candidate in [cur] + list(cur.parents):
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    return Path.cwd().resolve()


def default_binary_path(repo_root: Optional[Path] = None) -> Path:
    root = repo_root or find_repo_root()
    return root / "target" / "debug" / "crawler-media"


def allocate_free_port(host: str = "127.0.0.1") -> int:
    """Bind an ephemeral loopback socket on port 0, return port, and close it."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((host, 0))
        return int(sock.getsockname()[1])
    finally:
        sock.close()


def resolve_e2e_tmp_root() -> str:
    """Return normalized temp root dir from CRAWLER_E2E_TMP_ROOT or '/tmp', ensuring it exists."""
    root = os.environ.get("CRAWLER_E2E_TMP_ROOT") or "/tmp"
    resolved = str(Path(root).expanduser().resolve())
    os.makedirs(resolved, exist_ok=True)
    return resolved


class InstanceSupervisor:
    """Manages an isolated crawler-media server process with its own /tmp DATA_DIR."""

    def __init__(
        self,
        bin_path: Optional[Path] = None,
        web_dir: Optional[Path] = None,
        cli_token: str = "e2e-cli-token-secret",
        admin_password: str = "e2e-admin-password-secret",
        lease_ttl_ms: int = 2000,
        cors_origins: Optional[str] = None,
        qb_url: Optional[str] = None,
        qb_user: Optional[str] = None,
        qb_pass: Optional[str] = None,
        qb_category: Optional[str] = None,
        qb_path_map: Optional[str] = None,
        browser_enabled: Optional[bool] = None,
        allow_private_network: Optional[bool] = None,
        extra_env: Optional[Dict[str, str]] = None,
        keep_tmp: bool = False,
        startup_timeout: float = 20.0,
    ) -> None:
        if cli_token == admin_password:
            raise ValueError("CRAWLER_MEDIA_ADMIN_PASSWORD must strictly differ from CRAWLER_MEDIA_TOKEN")
        if not cli_token or not admin_password:
            raise ValueError("cli_token and admin_password must be non-empty")

        self.repo_root = find_repo_root()
        self.bin_path = (
            Path(bin_path).resolve()
            if bin_path
            else default_binary_path(self.repo_root).resolve()
        )
        self.web_dir = Path(web_dir).resolve() if web_dir else None
        self.cli_token = cli_token
        self.admin_password = admin_password
        self.lease_ttl_ms = lease_ttl_ms
        self.cors_origins = cors_origins
        self.qb_url = qb_url
        self.qb_user = qb_user
        self.qb_pass = qb_pass
        self.qb_category = qb_category
        self.qb_path_map = qb_path_map
        self.browser_enabled = browser_enabled
        self.allow_private_network = allow_private_network
        self.extra_env = dict(extra_env or {})
        self.keep_tmp = keep_tmp
        self.startup_timeout = startup_timeout

        self.root_dir = Path(tempfile.mkdtemp(prefix="crawler-e2e-", dir=resolve_e2e_tmp_root()))
        self.data_dir = self.root_dir / "data"
        self.logs_dir = self.root_dir / "logs"
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.logs_dir.mkdir(parents=True, exist_ok=True)

        self.host = "127.0.0.1"
        self.port: Optional[int] = None
        self.proc: Optional[subprocess.Popen] = None
        self._stdout_handle: Optional[Any] = None
        self._stderr_handle: Optional[Any] = None

    @property
    def base_url(self) -> str:
        if self.port is None:
            raise RuntimeError("Instance is not started yet; port is unset")
        return f"http://{self.host}:{self.port}"

    @property
    def stdout_log(self) -> Path:
        return self.logs_dir / "backend.stdout.log"

    @property
    def stderr_log(self) -> Path:
        return self.logs_dir / "backend.stderr.log"

    def _build_serve_env(
        self,
        port: int,
        extra_env: Optional[Dict[str, str]] = None,
    ) -> Dict[str, str]:
        env = os.environ.copy()
        # Strip any ambient CRAWLER_MEDIA_* variables so tests stay completely isolated.
        for key in list(env.keys()):
            if key.startswith("CRAWLER_MEDIA_"):
                env.pop(key, None)

        env["CRAWLER_MEDIA_DATA"] = str(self.data_dir)
        env["CRAWLER_MEDIA_LISTEN"] = f"{self.host}:{port}"
        env["CRAWLER_MEDIA_TOKEN"] = self.cli_token
        env["CRAWLER_MEDIA_ADMIN_PASSWORD"] = self.admin_password
        env["CRAWLER_MEDIA_LEASE_TTL_MS"] = str(self.lease_ttl_ms)
        env["CRAWLER_MEDIA_LEASE_RETRY_MS"] = "250"
        env["CRAWLER_MEDIA_SLOW_REQUEST_MS"] = "0"

        if self.web_dir is not None:
            env["CRAWLER_MEDIA_UI"] = str(self.web_dir)
        if self.cors_origins is not None:
            env["CRAWLER_MEDIA_CORS_ORIGINS"] = self.cors_origins
        if self.qb_url is not None:
            env["CRAWLER_MEDIA_QB_URL"] = self.qb_url
        if self.qb_user is not None:
            env["CRAWLER_MEDIA_QB_USER"] = self.qb_user
        if self.qb_pass is not None:
            env["CRAWLER_MEDIA_QB_PASS"] = self.qb_pass
        if self.qb_category is not None:
            env["CRAWLER_MEDIA_QB_CATEGORY"] = self.qb_category
        if self.qb_path_map is not None:
            env["CRAWLER_MEDIA_QB_PATH_MAP"] = self.qb_path_map
        if self.browser_enabled is not None:
            env["CRAWLER_MEDIA_BROWSER"] = "1" if self.browser_enabled else "0"
        if self.allow_private_network is not None:
            env["CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK"] = (
                "1" if self.allow_private_network else "0"
            )

        env.update(self.extra_env)
        if extra_env:
            env.update(extra_env)
        return env

    def _close_log_handles(self) -> None:
        for attr in ("_stdout_handle", "_stderr_handle"):
            handle = getattr(self, attr, None)
            if handle is not None:
                try:
                    handle.close()
                except OSError:
                    pass
                setattr(self, attr, None)

    def _read_tail_logs(self, max_chars: int = 2000) -> str:
        parts: List[str] = []
        for label, path in (("stdout", self.stdout_log), ("stderr", self.stderr_log)):
            if path.is_file():
                try:
                    text = path.read_text(encoding="utf-8", errors="replace")
                    if text:
                        parts.append(f"[{label}]\n{text[-max_chars:]}")
                except OSError:
                    pass
        return "\n".join(parts)

    def start(
        self,
        max_bind_retries: int = 3,
        extra_env: Optional[Dict[str, str]] = None,
        port: Optional[int] = None,
    ) -> str:
        """Start crawler-media serve with an ephemeral loopback port and wait until healthy."""
        if self.proc is not None and self.proc.poll() is None:
            raise RuntimeError("Instance is already running")
        if not self.bin_path.is_file():
            raise FileNotFoundError(
                f"crawler-media binary not found at {self.bin_path}; pass --bin or build it first"
            )

        last_err: Optional[Exception] = None
        for attempt in range(max(1, max_bind_retries)):
            chosen_port = port if (port is not None and attempt == 0) else allocate_free_port(self.host)
            self.port = chosen_port
            env = self._build_serve_env(chosen_port, extra_env=extra_env)

            self._close_log_handles()
            self._stdout_handle = open(self.stdout_log, "ab")
            self._stderr_handle = open(self.stderr_log, "ab")
            self.proc = subprocess.Popen(
                [str(self.bin_path), "serve"],
                cwd=str(self.root_dir),
                env=env,
                stdout=self._stdout_handle,
                stderr=self._stderr_handle,
            )
            try:
                self.wait_ready(timeout=self.startup_timeout)
                return self.base_url
            except Exception as exc:
                last_err = exc
                self.stop(timeout=2.0)
                if port is not None:
                    break
        raise RuntimeError(f"Failed to start crawler-media after retries: {last_err}")

    def wait_ready(
        self,
        timeout: Optional[float] = None,
        allow_read_only: bool = False,
    ) -> Dict[str, Any]:
        """Poll GET /api/v1/health until status==200, data.status=='ok', and writable unless allow_read_only."""
        effective_timeout = self.startup_timeout if timeout is None else timeout
        deadline = time.monotonic() + effective_timeout
        client = ApiClient(self.base_url, timeout=2.0)
        last_exc: Optional[Exception] = None
        try:
            while time.monotonic() < deadline:
                if self.proc is not None:
                    rc = self.proc.poll()
                    if rc is not None:
                        logs = self._read_tail_logs()
                        raise RuntimeError(
                            f"crawler-media exited prematurely with code {rc}\n{logs}"
                        )
                try:
                    data = client.health()
                    if isinstance(data, dict) and data.get("status") == "ok":
                        if allow_read_only or data.get("read_only") is False:
                            return data
                        last_exc = RuntimeError(f"Instance still read_only: {data!r}")
                except (OSError, ApiError, RuntimeError) as exc:
                    last_exc = exc
                time.sleep(0.15)
        finally:
            client.close()

        logs = self._read_tail_logs()
        raise TimeoutError(
            f"crawler-media did not become healthy within {effective_timeout}s "
            f"(last error: {last_exc})\n{logs}"
        )

    def stop(self, timeout: float = 10.0) -> Optional[int]:
        """Gracefully terminate via SIGTERM, escalating to SIGKILL after timeout."""
        if self.proc is None:
            self._close_log_handles()
            return None

        rc = self.proc.poll()
        if rc is None:
            try:
                self.proc.send_signal(signal.SIGTERM)
            except OSError:
                pass
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                rc = self.proc.poll()
                if rc is not None:
                    break
                time.sleep(0.05)
            if rc is None:
                try:
                    self.proc.kill()
                except OSError:
                    pass
                rc = self.proc.wait()

        self.proc = None
        self._close_log_handles()
        return rc

    def kill(self) -> Optional[int]:
        """Immediately send SIGKILL for crash-recovery E2E testing."""
        if self.proc is None:
            self._close_log_handles()
            return None
        rc = self.proc.poll()
        if rc is None:
            try:
                self.proc.kill()
            except OSError:
                pass
            rc = self.proc.wait()
        self.proc = None
        self._close_log_handles()
        return rc

    def restart(
        self,
        extra_env: Optional[Dict[str, str]] = None,
        reuse_port: bool = False,
    ) -> str:
        """Stop (if running) and start again reusing the exact same DATA_DIR."""
        old_port = self.port if reuse_port else None
        self.stop(timeout=10.0)
        return self.start(extra_env=extra_env, port=old_port)

    def cleanup(self, force: bool = False) -> None:
        """Stop process and remove isolated /tmp root directory unless keep_tmp=True."""
        self.stop(timeout=10.0)
        if (force or not self.keep_tmp) and self.root_dir.exists():
            shutil.rmtree(self.root_dir, ignore_errors=True)

    def client(self, use_cli_token: bool = False) -> ApiClient:
        return ApiClient(
            self.base_url,
            token=self.cli_token if use_cli_token else None,
        )

    def run_cli(
        self,
        args: Sequence[str],
        env: Optional[Dict[str, str]] = None,
        timeout: float = 30.0,
        cwd: Optional[Path] = None,
    ) -> subprocess.CompletedProcess:
        """Execute <bin> <args...> with isolated CRAWLER_MEDIA_* environment."""
        if not self.bin_path.is_file():
            raise FileNotFoundError(f"crawler-media binary not found at {self.bin_path}")
        merged_env = os.environ.copy()
        for key in list(merged_env.keys()):
            if key.startswith("CRAWLER_MEDIA_"):
                merged_env.pop(key, None)
        merged_env["CRAWLER_MEDIA_DATA"] = str(self.data_dir)
        merged_env["CRAWLER_MEDIA_TOKEN"] = self.cli_token
        if self.port is not None:
            merged_env["CRAWLER_MEDIA_URL"] = self.base_url
        if env:
            merged_env.update(env)
        return subprocess.run(
            [str(self.bin_path)] + list(args),
            cwd=str(cwd or self.root_dir),
            env=merged_env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )

    def __enter__(self) -> InstanceSupervisor:
        try:
            self.start()
        except BaseException:
            self.cleanup()
            raise
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.cleanup()
