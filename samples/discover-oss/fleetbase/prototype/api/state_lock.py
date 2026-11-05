"""Exclusive local guard for the experimental Fleetbase state directory."""
import fcntl
import os


def acquire_state_lock(root):
    """Lock the state directory itself until the returned descriptor is closed.

    Every process accessing the state must cooperate with this advisory lock.
    It does not protect against unrelated processes or replace Runner fencing.
    """
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BaseException:
        os.close(fd)
        raise
    return fd
