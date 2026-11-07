#!/usr/bin/env python3
"""Runs a program on a real pseudo-terminal, as a person's terminal window does, and lets a proof
type into it.

    sessionpty.py --log FILE --keys FIFO [--ready FILE] [--cols N] [--rows N] [--timeout SECONDS]
                  -- COMMAND [ARGUMENT ...]

The command (Claude Code, in the proofs) runs with the slave side of a new pseudo-terminal as its
controlling terminal and its standard streams, so it is an attended, interactive session. Its output
is appended to FILE as it arrives. Each line written to the FIFO is typed into the terminal, ended
with a carriage return (what the Enter key sends); the line `<ESC>` types the escape key and the
line `<CTRL-C>` types Control-C. --ready names a file that gets the child's pid. The driver ends
when the command ends (exit status: the command's) or after --timeout seconds (124, the command
killed).
"""

import argparse
import fcntl
import os
import select
import signal
import struct
import sys
import termios
import time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", required=True)
    ap.add_argument("--keys", required=True)
    ap.add_argument("--ready")
    ap.add_argument("--cols", type=int, default=140)
    ap.add_argument("--rows", type=int, default=50)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    cmd = args.cmd[1:] if args.cmd and args.cmd[0] == "--" else args.cmd
    if not cmd:
        print("sessionpty: no command", file=sys.stderr)
        return 2

    master, slave_fd = os.openpty()
    fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
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
            f.write(f"{pid}\n")

    # O_RDWR keeps the FIFO open without a writer, so a reader never sees end-of-file.
    keys = os.open(args.keys, os.O_RDWR | os.O_NONBLOCK)
    log = open(args.log, "ab", buffering=0)
    typed = bytearray()
    deadline = time.time() + args.timeout
    while True:
        if time.time() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            return 124
        ready, _, _ = select.select([master, keys], [], [], 0.2)
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                data = b""
            if not data:
                break
            log.write(data)
        if keys in ready:
            try:
                typed += os.read(keys, 4096)
            except BlockingIOError:
                pass
            while b"\n" in typed:
                line, _, rest = bytes(typed).partition(b"\n")
                typed = bytearray(rest)
                if line == b"<ESC>":
                    os.write(master, b"\x1b")
                elif line == b"<CTRL-C>":
                    os.write(master, b"\x03")
                elif line == b"<CTRL-O>":
                    os.write(master, b"\x0f")
                elif line == b"<ENTER>":
                    os.write(master, b"\r")
                elif line == b"<DOWN>":
                    os.write(master, b"\x1b[B")
                elif line.startswith(b"<RAW:") and line.endswith(b">"):
                    # The characters between `<RAW:` and `>`, typed with no Enter after them.
                    os.write(master, line[5:-1])
                else:
                    os.write(master, line)
                    time.sleep(0.3)
                    os.write(master, b"\r")
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


if __name__ == "__main__":
    sys.exit(main())
