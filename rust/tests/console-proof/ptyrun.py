#!/usr/bin/env python3
"""Runs a command on a real pseudo-terminal and types secrets into its prompts.

    ptyrun.py --log FILE [--auto 'REGEX::SECRETFILE'] ... [--keys FIFO] [--ready FILE]
              [--owner UID] [--timeout SECONDS] -- COMMAND [ARGUMENT ...]

The command runs with the slave side of a new pseudo-terminal as its controlling terminal. Output
is appended to FILE as it arrives. Each --auto rule waits for REGEX in the output after the
previous match, waits half a second (so the program has hidden the typed characters), and types
the contents of SECRETFILE and a newline: an empty file types just the newline. A secret is read
from a file, never taken from the command line or the environment, so it is in no process's
argument list or environment. With --keys, lines written to the FIFO are typed too (the console is
driven that way). --ready names a file that gets the child's pid and the slave's path. --owner
gives the slave to that user id, as a terminal a person opened is theirs. Exit status: the
command's, or 124 when --timeout passes.
"""

import argparse
import fcntl
import os
import re
import select
import signal
import sys
import termios
import time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", required=True)
    ap.add_argument("--auto", action="append", default=[])
    ap.add_argument("--keys")
    ap.add_argument("--ready")
    ap.add_argument("--owner", type=int)
    ap.add_argument("--timeout", type=int, default=180)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    cmd = args.cmd[1:] if args.cmd and args.cmd[0] == "--" else args.cmd
    if not cmd:
        print("ptyrun: no command", file=sys.stderr)
        return 2

    rules = []
    for rule in args.auto:
        regex, _, secret_file = rule.rpartition("::")
        rules.append((re.compile(regex), secret_file))

    # A new pseudo-terminal, given to the child as its controlling terminal. (Python has no
    # os.ptsname; the slave's path is read from the slave's own descriptor.)
    master, slave_fd = os.openpty()
    slave = os.ttyname(slave_fd)
    if args.owner is not None:
        os.chown(slave, args.owner, -1)
    pid = os.fork()
    if pid == 0:
        os.close(master)
        os.setsid()
        fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
        for fd in (0, 1, 2):
            os.dup2(slave_fd, fd)
        if slave_fd > 2:
            os.close(slave_fd)
        os.execvp(cmd[0], cmd)
    os.close(slave_fd)
    if args.ready:
        with open(args.ready, "w") as f:
            f.write(f"{pid}\n{slave}\n")

    keys = None
    if args.keys:
        keys = os.open(args.keys, os.O_RDONLY | os.O_NONBLOCK)
    log = open(args.log, "ab", buffering=0)
    seen = ""
    start = 0
    queue = []
    typed = bytearray()
    deadline = time.time() + args.timeout
    code = None
    while True:
        if time.time() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            return 124
        fds = [master] + ([keys] if keys is not None else [])
        ready, _, _ = select.select(fds, [], [], 0.2)
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                data = b""
            if not data:
                break
            log.write(data)
            seen += data.decode("latin-1")
            for k, (regex, secret_file) in enumerate(rules):
                m = regex.search(seen, start)
                if m:
                    time.sleep(0.5)
                    with open(secret_file, "rb") as f:
                        secret = f.read().rstrip(b"\n")
                    os.write(master, secret + b"\n")
                    start = m.end()
                    del secret
                    break
        if keys is not None and keys in ready:
            try:
                chunk = os.read(keys, 4096)
            except BlockingIOError:
                chunk = b""
            typed += chunk
            while b"\n" in typed:
                line, _, rest = bytes(typed).partition(b"\n")
                typed = bytearray(rest)
                os.write(master, line + b"\n")
    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    return code


if __name__ == "__main__":
    sys.exit(main())
