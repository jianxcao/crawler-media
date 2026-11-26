"""Tests for scripts/with-build-lock.py QoS, lock hygiene, fairness, and signals."""
import fcntl
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time
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

    def _isolated_env(self, *, no_bg_qos: bool = True) -> dict[str, str]:
        return {
            **os.environ,
            "PATH": f"{self.bin_dir}:{os.environ.get('PATH', '')}",
            "CM_BUILD_LOCK_DIR": self.lock_dir,
            "CM_NO_BG_QOS": "1" if no_bg_qos else "0",
            "CM_TRACE_LOG": self.trace_log,
        }

    @staticmethod
    def _kill_quietly(pid: int) -> None:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def test_lock_not_held_by_orphaned_grandchild(self) -> None:
        env = self._isolated_env()
        spawner = (
            "import subprocess, sys; "
            "p = subprocess.Popen("
            "[sys.executable, '-c', 'import time; time.sleep(30)'], "
            "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, "
            "close_fds=False, start_new_session=True); "
            "print(p.pid)"
        )
        res = subprocess.run(
            [sys.executable, "scripts/with-build-lock.py", sys.executable, "-c", spawner],
            env=env,
            capture_output=True,
            text=True,
            check=True,
        )
        grandchild_pid = int(res.stdout.strip())
        self.addCleanup(self._kill_quietly, grandchild_pid)

        start = time.monotonic()
        excl = subprocess.run(
            [
                sys.executable,
                "scripts/with-build-lock.py",
                "--exclusive",
                sys.executable,
                "-c",
                "pass",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=2.0,
            check=True,
        )
        elapsed = time.monotonic() - start
        self.assertEqual(excl.returncode, 0)
        self.assertLess(elapsed, 2.0)

    def test_exclusive_not_starved_by_single_slot_stream(self) -> None:
        env = self._isolated_env()
        holder_ready = os.path.join(self.tmp.name, "holder1.ready")
        release_holder = os.path.join(self.tmp.name, "holder1.release")
        order_log = os.path.join(self.tmp.name, "order.log")

        holder_code = (
            "import os, sys, time; "
            f"open({holder_ready!r}, 'w').close(); "
            f"deadline = time.monotonic() + 10.0\n"
            f"while not os.path.exists({release_holder!r}) and time.monotonic() < deadline:\n"
            "    time.sleep(0.02)\n"
            f"open({order_log!r}, 'a').write('holder1\\n')"
        )
        holder = subprocess.Popen(
            [sys.executable, "scripts/with-build-lock.py", sys.executable, "-c", holder_code],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)

        deadline = time.monotonic() + 5.0
        while not os.path.exists(holder_ready) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(os.path.exists(holder_ready), "slot 1 holder never became ready")

        excl_code = (
            "import time; "
            "time.sleep(0.05); "
            f"open({order_log!r}, 'a').write('exclusive\\n')"
        )
        excl = subprocess.Popen(
            [
                sys.executable,
                "scripts/with-build-lock.py",
                "--exclusive",
                sys.executable,
                "-c",
                excl_code,
            ],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(excl.wait)
        self.addCleanup(excl.kill)

        # Wait until --exclusive holds the intent gate (or briefly if absent).
        intent_path = os.path.join(self.lock_dir, "cm-cargo-intent.lock")
        gate_deadline = time.monotonic() + 0.35
        while time.monotonic() < gate_deadline:
            if os.path.exists(intent_path):
                with open(intent_path, "a", encoding="utf-8") as fh:
                    try:
                        fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        break
                    else:
                        fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
            time.sleep(0.02)

        late_code = f"open({order_log!r}, 'a').write('late_single\\n')"
        late = subprocess.Popen(
            [sys.executable, "scripts/with-build-lock.py", sys.executable, "-c", late_code],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(late.wait)
        self.addCleanup(late.kill)

        time.sleep(0.15)
        open(release_holder, "w", encoding="utf-8").close()

        self.assertEqual(holder.wait(timeout=5.0), 0)
        self.assertEqual(excl.wait(timeout=5.0), 0)
        self.assertEqual(late.wait(timeout=5.0), 0)

        with open(order_log, encoding="utf-8") as fh:
            events = [line.strip() for line in fh if line.strip()]
        self.assertIn("exclusive", events)
        self.assertIn("late_single", events)
        self.assertLess(
            events.index("exclusive"),
            events.index("late_single"),
            f"expected --exclusive before late single-slot task, got {events}",
        )

    def test_exit_code_and_sigterm_propagate(self) -> None:
        env = self._isolated_env(no_bg_qos=False)
        res = subprocess.run(
            [
                sys.executable,
                "scripts/with-build-lock.py",
                sys.executable,
                "-c",
                "import sys; sys.exit(42)",
            ],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(res.returncode, 42)

        pid_file = os.path.join(self.tmp.name, "child.pid")
        sleeper = os.path.join(self.tmp.name, "sleeper.py")
        write_exe(
            sleeper,
            f"#!{sys.executable}\n"
            "import os, time\n"
            f"with open({pid_file!r}, 'w') as fh:\n"
            "    fh.write(str(os.getpid()))\n"
            "time.sleep(30)\n",
        )
        write_exe(
            os.path.join(self.bin_dir, "cargo"),
            f'#!/bin/sh\nexec "{sleeper}" "$@"\n',
        )
        cmd = (
            ["cargo", "test", "-p", "domain"]
            if sys.platform == "darwin"
            else [sleeper]
        )
        wrapper = subprocess.Popen(
            [sys.executable, "scripts/with-build-lock.py", *cmd],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(wrapper.wait)
        self.addCleanup(wrapper.kill)

        deadline = time.monotonic() + 5.0
        child_pid = None
        while time.monotonic() < deadline:
            if os.path.exists(pid_file):
                with open(pid_file, encoding="utf-8") as fh:
                    raw = fh.read().strip()
                if raw:
                    child_pid = int(raw)
                    break
            time.sleep(0.02)
        self.assertIsNotNone(child_pid, "child never wrote its PID")
        assert child_pid is not None
        self.addCleanup(self._kill_quietly, child_pid)

        wrapper.send_signal(signal.SIGTERM)
        rc = wrapper.wait(timeout=3.0)
        self.assertEqual(rc, 128 + signal.SIGTERM)

        term_deadline = time.monotonic() + 1.5
        child_alive = True
        while time.monotonic() < term_deadline:
            try:
                os.kill(child_pid, 0)
            except ProcessLookupError:
                child_alive = False
                break
            time.sleep(0.02)
        self.assertFalse(child_alive, "child process survived SIGTERM sent to wrapper")

        excl = subprocess.run(
            [
                sys.executable,
                "scripts/with-build-lock.py",
                "--exclusive",
                sys.executable,
                "-c",
                "pass",
            ],
            env=self._isolated_env(),
            capture_output=True,
            text=True,
            timeout=2.0,
            check=True,
        )
        self.assertEqual(excl.returncode, 0)


if __name__ == "__main__":
    unittest.main()
