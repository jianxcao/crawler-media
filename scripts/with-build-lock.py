"""Bound concurrent builds and tests on the shared development host."""
import fcntl
import os
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
os.execvpe(command[0], command, {**os.environ, "TMPDIR": "/tmp"})
