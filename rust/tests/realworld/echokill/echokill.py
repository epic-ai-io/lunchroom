#!/usr/bin/env python3
"""Runs a command on a pseudo-terminal this program keeps open, waits for its passphrase prompt,
sends a signal to the program, and reports whether the terminal's echo comes back.

    echokill.py --signal KILL|TERM|HUP --target ARG [--owner UID] [--wait SECONDS] -- COMMAND [ARGUMENT ...]

COMMAND is started with the pseudo-terminal's slave as its controlling terminal. This program keeps
the slave open itself, so the terminal's settings outlive the command: that is what lets it read the
ECHO flag after the command is dead. When the output ends in a passphrase prompt (the text
"assphrase" and a final colon) the ECHO flag is read, then SIGNAL is sent to the process whose
command line is the one that ends in --target (the program, not the user-switching wrapper above
it), and the ECHO flag is read again every 100 ms for --wait seconds (default 6).

It prints one line each:
    ECHO_BEFORE=off|on      the flag at the prompt (the proof is hollow unless this is off)
    TARGET=<pid>            the process signalled
    ECHO_AFTER=on|off AFTER=<seconds>   the flag once it came back, or at the end of the wait
    REMAINING=<pids>        processes named lunchroom other than the one signalled (a watchdog that
                            has not ended), 0.5 s after the echo came back or the wait ended
Exit status: 0 when it ran to the end (the proof judges the lines), 2 on a setup failure.

Spec section 7 A1.3 (docs/spec/lunchroom-spec.md): "Echo is turned off by the program itself ... and
turned back on before it exits, also when it is interrupted." Echo after SIGKILL is the lead's 1.1
decision (GAPS addendum 2: a watchdog process and a hidden subcommand restore it).
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


def echo_on(fd):
    return bool(termios.tcgetattr(fd)[3] & termios.ECHO)


def pids_with(arg):
    found = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open("/proc/%s/cmdline" % name, "rb") as f:
                words = f.read().split(b"\0")
        except OSError:
            continue
        words = [w for w in words if w]
        if words and words[-1].decode("utf-8", "replace") == arg and os.path.basename(words[0].decode("utf-8", "replace")) == "lunchroom":
            found.append(int(name))
    return found


def lunchroom_pids():
    """Every process whose name is lunchroom: the program, and any watchdog it started."""
    found = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open("/proc/%s/comm" % name) as f:
                if f.read().strip() == "lunchroom":
                    found.append(int(name))
        except OSError:
            continue
    return found


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--signal", required=True, choices=["KILL", "TERM", "HUP"])
    ap.add_argument("--target", required=True)
    ap.add_argument("--owner", type=int)
    ap.add_argument("--wait", type=float, default=6.0)
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    cmd = args.cmd[1:] if args.cmd and args.cmd[0] == "--" else args.cmd
    if not cmd:
        print("echokill: no command", file=sys.stderr)
        return 2
    master, slave = os.openpty()
    if args.owner is not None:
        os.chown(os.ttyname(slave), args.owner, -1)
    pid = os.fork()
    if pid == 0:
        os.close(master)
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        for fd in (0, 1, 2):
            os.dup2(slave, fd)
        if slave > 2:
            os.close(slave)
        os.execvp(cmd[0], cmd)
    seen = b""
    deadline = time.time() + 40
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if master in ready:
            try:
                data = os.read(master, 4096)
            except OSError:
                break
            if not data:
                break
            seen += data
            tail = seen.replace(b"\r", b"").rstrip()
            if b"assphrase" in seen and re.search(rb"assphrase[^\n]*:$", tail):
                break
    if not (b"assphrase" in seen):
        print("echokill: the command never asked for a passphrase; its output: %r" % seen[-300:], file=sys.stderr)
        return 2
    time.sleep(0.7)
    print("ECHO_BEFORE=%s" % ("on" if echo_on(slave) else "off"))
    targets = pids_with(args.target)
    if not targets:
        print("echokill: no process of the program was found (argument %r)" % args.target, file=sys.stderr)
        return 2
    victim = targets[0]
    print("TARGET=%d" % victim)
    sys.stdout.flush()
    os.kill(victim, getattr(signal, "SIG" + args.signal))
    start = time.time()
    after = None
    while time.time() - start < args.wait:
        if echo_on(slave):
            after = time.time() - start
            break
        time.sleep(0.1)
    print("ECHO_AFTER=%s AFTER=%.1f" % ("on" if after is not None else "off", after if after is not None else args.wait))
    time.sleep(0.5)
    leftover = [p for p in lunchroom_pids() if p != victim]
    print("REMAINING=%s" % " ".join(str(p) for p in leftover))
    for p in lunchroom_pids():
        try:
            os.kill(p, signal.SIGKILL)
        except OSError:
            pass
    try:
        os.kill(pid, signal.SIGKILL)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except OSError:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
