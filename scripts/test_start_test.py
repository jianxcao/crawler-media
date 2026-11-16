"""Exercise startup paths through the real script and an isolated HTTP backend."""

import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BACKEND = """#!{python}
import json, os
from pathlib import Path
from http.server import BaseHTTPRequestHandler, HTTPServer

class Health(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
    def log_message(self, *args):
        pass

data = Path(os.environ["CRAWLER_MEDIA_DATA"])
logs = data / "logs"
logs.mkdir(parents=True, exist_ok=True)
(logs / "crawler-media.log.test").write_text("application log")
Path(os.environ["START_TEST_OBSERVED"]).write_text(json.dumps({{
    "data": str(data),
    "downloads": os.environ["CRAWLER_MEDIA_QB_PATH_MAP"],
    "pid": os.getpid(),
}}))
print("backend stdout", flush=True)
HTTPServer(("127.0.0.1", int(os.environ["BACKEND_PORT"])), Health).serve_forever()
"""


class StartupPathsTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="cm-start-test-", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.repo = self.directory / "repo"
        self.repo.mkdir()
        shutil.copy2(ROOT / "start-test.sh", self.repo / "start-test.sh")
        (self.repo / "scripts").mkdir()
        shutil.copy2(
            ROOT / "scripts/with-build-lock.py",
            self.repo / "scripts/with-build-lock.py",
        )
        # Build a tiny local crate; never compile or start the real application.
        (self.repo / "Cargo.toml").write_text(
            '[package]\nname="api"\nversion="0.0.0"\nedition="2024"\n'
            '[lib]\npath="lib.rs"\n'
        )
        (self.repo / "lib.rs").write_text("")
        binary = self.repo / "target/debug/crawler-media"
        binary.parent.mkdir(parents=True)
        binary.write_text(BACKEND.format(python=sys.executable))
        binary.chmod(0o755)
        self.runtime = self.directory / "runtime with spaces"
        self.observed = self.directory / "backend.json"
        self.env = {
            key: value
            for key, value in os.environ.items()
            if key not in {
                "TEST_ROOT", "DATA_DIR", "LOG_DIR", "QB_HOST_DOWNLOADS",
                "MOCK_SITE_PY", "CLEAN_LOGS", "CARGO_TARGET_DIR",
            }
        }
        self.env.update(
            TEST_ROOT=str(self.runtime),
            START_TEST_OBSERVED=str(self.observed),
            TOKEN="test-token",
            ADMIN_PASSWORD="test-password",
            QB_USER="test-user",
            QB_PASS="test-password",
            CARGO_BUILD_JOBS="4",
        )
        for name in ["BACKEND_PORT", "FRONTEND_PORT", "QB_PORT", "MOCK_PORT"]:
            port = socket.socket()
            port.bind(("127.0.0.1", 0))
            self.env[name] = str(port.getsockname()[1])
            if name == "BACKEND_PORT":
                port.close()
            else:
                self.addCleanup(port.close)
        self.addCleanup(self.stop_backend)

    def stop_backend(self):
        if self.observed.exists():
            pid = json.loads(self.observed.read_text())["pid"]
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass

    def invoke(self, action, **overrides):
        result = subprocess.run(
            ["bash", str(self.repo / "start-test.sh"), action],
            cwd=self.directory,
            env={**self.env, **overrides},
            text=True,
            capture_output=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_default_status_reports_the_downloads_layout_without_creating_it(self):
        self.env.pop("TEST_ROOT")
        base = Path.home() / "Downloads/crawler-media"
        before = {path: path.exists() for path in [base / "log", base / "run", base / "qb"]}
        output = self.invoke("status")
        self.assertIn(str(base / "data"), output)
        self.assertIn(str(base / "log"), output)
        self.assertIn(str(base / "qb"), output)
        self.assertEqual({path: path.exists() for path in before}, before)

    def test_restart_routes_database_logs_downloads_and_pid_to_the_test_root(self):
        self.invoke("restart")
        observed = json.loads(self.observed.read_text())
        self.assertEqual(observed["data"], str(self.runtime / "data"))
        self.assertEqual(observed["downloads"], f"/downloads={self.runtime}/qb")
        self.assertEqual(
            (self.runtime / "log/crawler-media.log.test").read_text(),
            "application log",
        )
        self.assertIn(
            "backend stdout", (self.runtime / "log/backend.log").read_text()
        )
        self.assertEqual(
            (self.runtime / "run/backend.pid").read_text().strip(),
            str(observed["pid"]),
        )
        self.assertFalse((self.repo / "data").exists())
        self.assertFalse((self.repo / ".test-logs").exists())

    def test_absolute_data_override_is_passed_unchanged(self):
        data = self.directory / "isolated data"
        self.invoke("restart", DATA_DIR=str(data))
        self.assertEqual(json.loads(self.observed.read_text())["data"], str(data))
        self.assertEqual(
            (self.runtime / "log/crawler-media.log.test").read_text(),
            "application log",
        )

    def test_relative_data_override_resolves_from_repository_not_callers_directory(self):
        self.invoke("restart", DATA_DIR="custom data")
        self.assertEqual(
            json.loads(self.observed.read_text())["data"],
            str(self.repo / "custom data"),
        )

    def test_existing_log_directory_is_preserved_and_startup_explains_the_conflict(self):
        old_logs = self.runtime / "data/logs"
        old_logs.mkdir(parents=True)
        sentinel = old_logs / "important.log"
        sentinel.write_text("existing log")
        result = subprocess.run(
            ["bash", str(self.repo / "start-test.sh"), "restart"],
            env=self.env, text=True, capture_output=True, timeout=30,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(old_logs), result.stdout + result.stderr)
        self.assertEqual(sentinel.read_text(), "existing log")
        self.assertFalse(self.observed.exists())

    def test_clean_logs_uses_the_runtime_log_directory_and_preserves_database(self):
        logs = self.runtime / "log"
        logs.mkdir(parents=True)
        for name in ["backend", "frontend", "mock"]:
            (logs / f"{name}.log").write_text("old output")
        database = self.runtime / "data/library.db"
        database.parent.mkdir()
        database.write_bytes(b"database sentinel")
        self.invoke("clean-logs")
        for name in ["backend", "frontend", "mock"]:
            self.assertEqual((logs / f"{name}.log").read_text(), "")
        self.assertEqual(database.read_bytes(), b"database sentinel")

    def logstats(self, *args):
        result = subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/e2e/logstats.py"), *args],
            cwd=self.directory, env=self.env,
            text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_logstats_uses_the_same_test_root(self):
        log = self.runtime / "log/backend.log"
        log.parent.mkdir(parents=True)
        log.write_text("Remote media read admitted waited_ms=19\n")
        self.assertIn("remote reads 1", self.logstats())

    def test_logstats_accepts_an_explicit_log_file(self):
        log = self.directory / "recorded backend.log"
        log.write_text("Remote media read admitted waited_ms=19\n" * 2)
        self.assertIn("remote reads 2", self.logstats(str(log)))

    def test_logstats_reads_colored_backend_output(self):
        log = self.directory / "colored backend.log"
        log.write_text(
            "HTTP 请求缓慢 \x1b[3mroute\x1b[0m\x1b[2m=\x1b[0m/api/v1/health "
            "\x1b[3mheaders_latency_ms\x1b[0m\x1b[2m=\x1b[0m1511\n"
        )
        self.assertIn("slow http 1", self.logstats(str(log)))


if __name__ == "__main__":
    unittest.main()
