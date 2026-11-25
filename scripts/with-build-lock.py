"""Bound concurrent builds and tests on the shared development host."""
import fcntl
import os
import shutil
import subprocess
import sys
import time

exclusive = len(sys.argv) > 1 and sys.argv[1] == "--exclusive"
command = sys.argv[2:] if exclusive else sys.argv[1:]
if not command:
    raise SystemExit("usage: with-build-lock.py [--exclusive] COMMAND [ARG ...]")

lock_dir = os.environ.get("CM_BUILD_LOCK_DIR", "/tmp")
lock_names = ("cm-cargo.lock", "cm-cargo-2.lock")


def try_lock(name):
    lock = open(os.path.join(lock_dir, name), "a", encoding="utf-8")
    try:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        return None
    return lock


while True:
    if exclusive:
        first = try_lock(lock_names[0])
        if first is not None:
            second = try_lock(lock_names[1])
            if second is not None:
                held_locks = (first, second)
                break
            first.close()
    else:
        for name in lock_names:
            lock = try_lock(name)
            if lock is not None:
                held_locks = (lock,)
                break
        else:
            time.sleep(0.05)
            continue
        break
    time.sleep(0.05)

for lock in held_locks:
    os.set_inheritable(lock.fileno(), True)

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
if taskpolicy is not None and is_cargo_test:
    # Run the heavy compile+link pass in macOS Background QoS so desktop UI stays
    # responsive, then run compiled test binaries at normal QoS so strict
    # millisecond timing assertions are not tripped by E-core/IO throttling.
    cargo_args = command[: command.index("--")] if "--" in command else list(command)
    if "--no-run" not in cargo_args and "--doc" not in cargo_args:
        status = subprocess.run([taskpolicy, "-b", *cargo_args, "--no-run"], env=env)
        if status.returncode != 0:
            raise SystemExit(status.returncode)
elif taskpolicy is not None:
    command = [taskpolicy, "-b", *command]

os.execvpe(command[0], command, env)
