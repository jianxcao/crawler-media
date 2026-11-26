"""Tests for scripts/with-build-lock.py QoS and --no-run handling."""
import os
import stat
import subprocess
import sys
import tempfile
import unittest


FAKE_SCRIPT = """#!/bin/sh
echo "ARGV:$0:$*" >> "${CM_TRACE_LOG}"
"""


def write_exe(path: str, content: str) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(content)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)


class WithBuildLockTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.bin_dir = os.path.join(self.tmp.name, "bin")
        self.lock_dir = os.path.join(self.tmp.name, "locks")
        self.trace_log = os.path.join(self.tmp.name, "trace.log")
        os.makedirs(self.bin_dir, exist_ok=True)
        os.makedirs(self.lock_dir, exist_ok=True)
        write_exe(
            os.path.join(self.bin_dir, "taskpolicy"),
            '#!/bin/sh\necho "TASKPOLICY:$*" >> "${CM_TRACE_LOG}"\nshift\nexec "$@"\n',
        )
        write_exe(
            os.path.join(self.bin_dir, "cargo"),
            '#!/bin/sh\necho "CARGO:$*" >> "${CM_TRACE_LOG}"\n',
        )

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_lock(self, *args: str) -> list[str]:
        env = {
            **os.environ,
            "PATH": f"{self.bin_dir}:{os.environ.get('PATH', '')}",
            "CM_BUILD_LOCK_DIR": self.lock_dir,
            "CM_NO_BG_QOS": "0",
            "CM_TRACE_LOG": self.trace_log,
        }
        res = subprocess.run(
            [sys.executable, "scripts/with-build-lock.py", *args],
            env=env,
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertEqual(res.returncode, 0)
        with open(self.trace_log, encoding="utf-8") as fh:
            return [line.strip() for line in fh if line.strip()]

    def test_cargo_test_splits_no_run_compile_under_bg_qos(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("taskpolicy is macOS-only")
        lines = self.run_lock("cargo", "test", "-j4", "-p", "domain", "--", "--test-threads=4")
        self.assertEqual(
            lines,
            [
                "TASKPOLICY:-b cargo test -j4 -p domain --no-run",
                "CARGO:test -j4 -p domain --no-run",
                "CARGO:test -j4 -p domain -- --test-threads=4",
            ],
        )

    def test_cargo_nextest_run_splits_no_run_compile_under_bg_qos(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("taskpolicy is macOS-only")
        lines = self.run_lock(
            "cargo", "nextest", "run", "--build-jobs", "4", "--test-threads", "4", "-p", "domain"
        )
        self.assertEqual(
            lines,
            [
                "TASKPOLICY:-b cargo nextest run --build-jobs 4 --test-threads 4 -p domain --no-run",
                "CARGO:nextest run --build-jobs 4 --test-threads 4 -p domain --no-run",
                "CARGO:nextest run --build-jobs 4 --test-threads 4 -p domain",
            ],
        )

    def test_cargo_nextest_run_strips_fail_fast_during_no_run(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("taskpolicy is macOS-only")
        lines = self.run_lock(
            "cargo",
            "nextest",
            "run",
            "--build-jobs",
            "8",
            "--test-threads",
            "8",
            "--workspace",
            "--no-fail-fast",
        )
        self.assertEqual(
            lines,
            [
                "TASKPOLICY:-b cargo nextest run --build-jobs 8 --test-threads 8 --workspace --no-run",
                "CARGO:nextest run --build-jobs 8 --test-threads 8 --workspace --no-run",
                "CARGO:nextest run --build-jobs 8 --test-threads 8 --workspace --no-fail-fast",
            ],
        )

    def test_cargo_nextest_non_run_or_no_run_flag_does_not_double_compile(self) -> None:
        if sys.platform != "darwin":
            self.skipTest("taskpolicy is macOS-only")
        lines_list = self.run_lock("cargo", "nextest", "list", "-p", "domain")
        self.assertEqual(
            lines_list,
            [
                "TASKPOLICY:-b cargo nextest list -p domain",
                "CARGO:nextest list -p domain",
            ],
        )
        with open(self.trace_log, "w", encoding="utf-8"):
            pass
        lines_norun = self.run_lock("cargo", "nextest", "run", "--no-run", "-p", "domain")
        self.assertEqual(
            lines_norun,
            [
                "TASKPOLICY:-b cargo nextest run --no-run -p domain",
                "CARGO:nextest run --no-run -p domain",
            ],
        )


if __name__ == "__main__":
    unittest.main()
