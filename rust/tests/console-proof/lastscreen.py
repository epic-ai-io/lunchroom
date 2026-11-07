#!/usr/bin/env python3
"""Prints the last whole screen the console drew into a capture file.

    lastscreen.py CAPTURE

The console clears the terminal (ESC [ 2 J) before each screen, so the last screen is what follows
the last clear. Other escape sequences are removed.
"""

import re
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        with open(sys.argv[1], "rb") as f:
            text = f.read().decode("latin-1")
    except OSError:
        return 1
    screens = text.split("\x1b[2J")
    last = screens[-1]
    last = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", last).replace("\r", "")
    sys.stdout.write(last)
    return 0


if __name__ == "__main__":
    sys.exit(main())
