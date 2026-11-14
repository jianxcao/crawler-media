"""Process-level contract tests for the shared build wrapper."""

import fcntl
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest


WRAPPER = Path(__file__).with_name("with-build-lock.py")
HOLD_COMMAND = """
import os
from pathlib import Path
import signal
import sys
import time

entered, release, ignore_term = sys.argv[1:]
if ignore_term == "yes":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
Path(entered).touch()
while not Path(release).exists():
    time.sleep(0.01)
"""


class BuildLockTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.processes = []
        self.releases = []
        self.addCleanup(self.finish_processes)

    def wrapper_env(self):
        return {
            **os.environ,
            "CM_BUILD_LOCK_DIR": str(self.directory),
            "PYTHONDONTWRITEBYTECODE": "1",
        }

    def start_hold(self, name, *, exclusive=False, ignore_term=False):
        entered = self.directory / f"{name}.entered"
        release = self.directory / f"{name}.release"
        args = [sys.executable, str(WRAPPER)]
        if exclusive:
            args.append("--exclusive")
        args += [
            sys.executable,
            "-c",
            HOLD_COMMAND,
            str(entered),
            str(release),
            "yes" if ignore_term else "no",
        ]
        process = subprocess.Popen(args, env=self.wrapper_env())
        self.processes.append(process)
        self.releases.append(release)
        return process, entered, release

    def wait_for(self, path):
        deadline = time.monotonic() + 5
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(path.exists(), f"command did not enter: {path.name}")

    def assert_waiting(self, path):
        time.sleep(0.25)
        self.assertFalse(path.exists(), f"command entered too early: {path.name}")

    def finish_processes(self):
        for release in self.releases:
            release.touch()
        for process in self.processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    def test_two_ordinary_commands_enter_while_third_waits(self):
        _, first_entered, first_release = self.start_hold("first")
        self.wait_for(first_entered)
        _, second_entered, _ = self.start_hold("second")
        self.wait_for(second_entered)
        _, third_entered, _ = self.start_hold("third")
        self.assert_waiting(third_entered)
        first_release.touch()
        self.wait_for(third_entered)

    def test_existing_slot_one_holder_counts_toward_limit(self):
        with open(self.directory / "cm-cargo.lock", "a", encoding="utf-8") as external:
            fcntl.flock(external.fileno(), fcntl.LOCK_EX)
            _, second_entered, _ = self.start_hold("second")
            self.wait_for(second_entered)
            _, third_entered, _ = self.start_hold("third")
            self.assert_waiting(third_entered)
            fcntl.flock(external.fileno(), fcntl.LOCK_UN)
            self.wait_for(third_entered)

    def test_exclusive_waits_for_both_slots_and_blocks_ordinary(self):
        _, first_entered, first_release = self.start_hold("first")
        self.wait_for(first_entered)
        _, second_entered, second_release = self.start_hold("second")
        self.wait_for(second_entered)
        _, exclusive_entered, exclusive_release = self.start_hold(
            "exclusive", exclusive=True
        )
        self.assert_waiting(exclusive_entered)
        first_release.touch()
        self.assert_waiting(exclusive_entered)
        second_release.touch()
        self.wait_for(exclusive_entered)
        _, ordinary_entered, _ = self.start_hold("ordinary")
        self.assert_waiting(ordinary_entered)
        exclusive_release.touch()
        self.wait_for(ordinary_entered)

    def test_exit_code_and_forced_tmpdir(self):
        completed = subprocess.run(
            [
                sys.executable,
                str(WRAPPER),
                sys.executable,
                "-c",
                "import os, sys; print(os.environ['TMPDIR']); sys.exit(23)",
            ],
            env={**self.wrapper_env(), "TMPDIR": str(self.directory)},
            capture_output=True,
            text=True,
            timeout=5,
        )
        self.assertEqual(completed.returncode, 23)
        self.assertEqual(completed.stdout.strip(), "/tmp")

    def test_termination_keeps_slot_while_command_survives(self):
        _, first_entered, _ = self.start_hold("first")
        self.wait_for(first_entered)
        second, second_entered, second_release = self.start_hold(
            "second", ignore_term=True
        )
        self.wait_for(second_entered)
        second.send_signal(signal.SIGTERM)
        self.assertIsNone(second.poll(), "command should still be alive")
        _, third_entered, _ = self.start_hold("third")
        self.assert_waiting(third_entered)
        second_release.touch()
        self.wait_for(third_entered)


if __name__ == "__main__":
    unittest.main()
