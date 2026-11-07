#!/usr/bin/env python3
"""Holds Lunchroom's store lock, as a Lunchroom process that never lets go does.

    hold.py LOCKFILE SECONDS READYFILE

Takes an exclusive flock on LOCKFILE (the store's state.flock), writes READYFILE, and holds the lock
for SECONDS seconds, or until it is killed.
"""

import fcntl
import sys
import time

path, seconds, ready = sys.argv[1], float(sys.argv[2]), sys.argv[3]
with open(path, "a") as f:
    fcntl.flock(f, fcntl.LOCK_EX)
    with open(ready, "w") as r:
        r.write("held\n")
    time.sleep(seconds)
