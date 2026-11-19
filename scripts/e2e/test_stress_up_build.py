"""Exercise stress startup with private build slots and no real services."""

import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
FAKE_CARGO = """#!{python}
import json, os, sys
from pathlib import Path
Path(os.environ["STRESS_BUILD_OBSERVED"]).write_text(json.dumps({{
    "args": sys.argv[1:], "tmpdir": os.environ.get("TMPDIR"),
}}))
raise SystemExit(37)
"""


class StressUpBuildTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="cm-stress-build-", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.repo = self.directory / "repo with spaces"
        scripts = self.repo / "scripts"
        (scripts / "e2e").mkdir(parents=True)
        self.script = scripts / "e2e/stress-up.sh"
        shutil.copy2(ROOT / "scripts/e2e/stress-up.sh", self.script)
        self.wrapper = scripts / "with-build-lock.py"
        shutil.copy2(ROOT / "scripts/with-build-lock.py", self.wrapper)
        commands = self.directory / "bin"
        commands.mkdir()
        cargo = commands / "cargo"
        cargo.write_text(FAKE_CARGO.format(python=sys.executable))
        cargo.chmod(0o755)
        self.locks = self.directory / "locks"
        self.locks.mkdir()
        self.runtime = self.directory / "runtime with spaces"
        self.runtime.mkdir()
        self.observed = self.directory / "cargo.json"
        self.env = {
            **os.environ,
            "PATH": f"{commands}{os.pathsep}{os.environ['PATH']}",
            "CM_BUILD_LOCK_DIR": str(self.locks),
            "STRESS_BUILD_OBSERVED": str(self.observed),
            "TMPDIR": str(self.directory),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        self.env.pop("PERF_SKIP_BUILD", None)

    def start(self, *, mode=None, exclusive=False, **overrides):
        args = ["zsh", str(self.script), str(self.runtime)]
        if mode is not None:
            args.append(mode)
        if exclusive:
            args = [sys.executable, str(self.wrapper), "--exclusive", *args]
        process = subprocess.Popen(
            args, cwd=self.directory, env={**self.env, **overrides},
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )
        self.addCleanup(self.finish, process)
        return process

    @staticmethod
    def finish(process):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=5)

    def test_default_build_waits_for_either_occupied_slot(self):
        for name in ("cm-cargo.lock", "cm-cargo-2.lock"):
            with self.subTest(slot=name):
                self.observed.unlink(missing_ok=True)
                with (self.locks / name).open("a") as held:
                    fcntl.flock(held, fcntl.LOCK_EX)
                    process = self.start()
                    # An independent ordinary wrapper still admits work in the
                    # free slot, while the exclusive build must remain queued.
                    probe = subprocess.run(
                        [sys.executable, str(self.wrapper), sys.executable, "-c", "pass"],
                        env=self.env, capture_output=True, timeout=5,
                    )
                    self.assertEqual(probe.returncode, 0, probe.stderr)
                    time.sleep(0.25)
                    self.assertIsNone(process.poll(), "build bypassed the occupied slot")
                    self.assertFalse(self.observed.exists(), "cargo ran before slot release")
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 37, stdout + stderr)
                observed = json.loads(self.observed.read_text())
                self.assertEqual(observed["args"][:2], ["build", "--manifest-path"])
                self.assertEqual(
                    Path(observed["args"][2]).resolve(),
                    (self.repo / "Cargo.toml").resolve(),
                )
                self.assertEqual(observed["args"][3:], ["--release", "-j8", "-p", "api"])
                self.assertEqual(observed["tmpdir"], "/tmp")
                self.assertFalse((self.runtime / "pid").exists())
                self.assertFalse((self.runtime / "media").exists())

    def assert_skips_build_under_exclusive_lock(self, *, mode=None, **overrides):
        # Use only a PID owned and reaped by this test. The next-stage guard
        # refuses a duplicate instance before any service or fixture is started.
        sleeper = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)"],
            start_new_session=True,
        )
        self.addCleanup(self.finish, sleeper)
        (self.runtime / "pid").write_text(str(sleeper.pid))
        if mode == "restart":
            # Restart must pass the build stage while the outer wrapper owns
            # both slots; stop at a harmless Python sentinel before HTTP work.
            python = Path(self.env["PATH"].split(os.pathsep)[0]) / "python3"
            python.write_text(
                f"#!{sys.executable}\nimport os, sys\n"
                "if sys.argv[1:2] == ['-']:\n    raise SystemExit(43)\n"
                "os.execv(sys.executable, [sys.executable, *sys.argv[1:]])\n"
            )
            python.chmod(0o755)
            backend = self.repo / "target/release/crawler-media"
            backend.parent.mkdir(parents=True)
            backend.write_text("#!/bin/sh\nexit 0\n")
            backend.chmod(0o755)
            sleeper.terminate()
            sleeper.wait(timeout=5)
        process = self.start(mode=mode, exclusive=True, **overrides)
        stdout, stderr = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 43 if mode == "restart" else 1, stdout + stderr)
        self.assertFalse(self.observed.exists(), "skip path invoked cargo")
        self.assertFalse((self.runtime / "media").exists())
        if mode != "restart":
            self.assertIn("Performance instance already running", stderr)

    def test_restart_skips_build_under_exclusive_lock(self):
        self.assert_skips_build_under_exclusive_lock(mode="restart")

    def test_explicit_skip_build_does_not_acquire_another_slot(self):
        self.assert_skips_build_under_exclusive_lock(PERF_SKIP_BUILD="1")


if __name__ == "__main__":
    unittest.main()
