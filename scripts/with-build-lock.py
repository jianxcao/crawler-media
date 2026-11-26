"""Bound concurrent builds and tests on the shared development host."""
import fcntl
import os
import shutil
import signal
import subprocess
import sys
import time

LOCK_SLOTS = ("cm-cargo.lock", "cm-cargo-2.lock")
INTENT_LOCK = "cm-cargo-intent.lock"

active_proc = None
received_signal = None


def handle_signal(signum, _frame):
    global received_signal
    received_signal = signum
    proc = active_proc
    if proc is None:
        raise SystemExit(128 + signum)
    try:
        proc.send_signal(signum)
    except ProcessLookupError:
        pass


def open_lock_file(lock_dir, name):
    os.makedirs(lock_dir, exist_ok=True)
    fh = open(os.path.join(lock_dir, name), "a", encoding="utf-8")
    os.set_inheritable(fh.fileno(), False)
    return fh


def wait_intent_gate(lock_dir):
    fh = open_lock_file(lock_dir, INTENT_LOCK)
    try:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
    finally:
        fh.close()


def try_lock_slot(lock_dir, name):
    fh = open_lock_file(lock_dir, name)
    try:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        fh.close()
        return None
    except BaseException:
        fh.close()
        raise
    return fh


def lock_slot_blocking(lock_dir, name):
    fh = open_lock_file(lock_dir, name)
    try:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
    except BaseException:
        fh.close()
        raise
    return fh


def acquire_locks(lock_dir, exclusive):
    if exclusive:
        intent = open_lock_file(lock_dir, INTENT_LOCK)
        held = []
        try:
            fcntl.flock(intent.fileno(), fcntl.LOCK_EX)
            for name in LOCK_SLOTS:
                held.append(lock_slot_blocking(lock_dir, name))
        except BaseException:
            for fh in reversed(held):
                fh.close()
            raise
        finally:
            intent.close()
        return held

    while True:
        wait_intent_gate(lock_dir)
        for name in LOCK_SLOTS:
            fh = try_lock_slot(lock_dir, name)
            if fh is not None:
                return [fh]
        time.sleep(0.02)


def run_command(cmd, env):
    global active_proc
    if received_signal is not None:
        return 128 + received_signal
    proc = subprocess.Popen(cmd, env=env, close_fds=True)
    active_proc = proc
    try:
        if received_signal is not None:
            try:
                proc.send_signal(received_signal)
            except ProcessLookupError:
                pass
        rc = proc.wait()
    finally:
        active_proc = None
    if rc < 0:
        return 128 + (-rc)
    if received_signal is not None and rc == 0:
        return 128 + received_signal
    return rc


def main():
    exclusive = len(sys.argv) > 1 and sys.argv[1] == "--exclusive"
    command = sys.argv[2:] if exclusive else sys.argv[1:]
    if not command:
        raise SystemExit("usage: with-build-lock.py [--exclusive] COMMAND [ARG ...]")

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    lock_dir = os.environ.get("CM_BUILD_LOCK_DIR", "/tmp")
    held_locks = acquire_locks(lock_dir, exclusive)
    try:
        env = {**os.environ, "TMPDIR": "/tmp"}
        taskpolicy = None
        if sys.platform == "darwin" and os.environ.get("CM_NO_BG_QOS") != "1":
            taskpolicy = shutil.which("taskpolicy") or (
                "/usr/sbin/taskpolicy" if os.path.exists("/usr/sbin/taskpolicy") else None
            )

        is_cargo_test = (
            len(command) >= 2
            and os.path.basename(command[0]) == "cargo"
            and command[1] == "test"
        )
        is_cargo_nextest_run = (
            len(command) >= 3
            and os.path.basename(command[0]) == "cargo"
            and command[1] == "nextest"
            and command[2] == "run"
        )
        if taskpolicy is not None:
            cargo_args = (
                command[: command.index("--")] if "--" in command else list(command)
            )
            split_compile = (is_cargo_test or is_cargo_nextest_run) and (
                "--no-run" not in cargo_args
                and (not is_cargo_test or "--doc" not in cargo_args)
            )
            if split_compile:
                # Run the heavy compile+link pass in macOS Background QoS so desktop UI
                # stays responsive, then run compiled test binaries at normal QoS so
                # strict millisecond timing assertions are not tripped by throttling.
                compile_args = (
                    [a for a in cargo_args if a not in ("--fail-fast", "--no-fail-fast")]
                    if is_cargo_nextest_run
                    else cargo_args
                )
                rc = run_command([taskpolicy, "-b", *compile_args, "--no-run"], env)
                if rc != 0 or received_signal is not None:
                    return rc if rc != 0 else 128 + received_signal
            else:
                command = [taskpolicy, "-b", *command]

        return run_command(command, env)
    finally:
        for lock in reversed(held_locks):
            lock.close()


if __name__ == "__main__":
    raise SystemExit(main())
