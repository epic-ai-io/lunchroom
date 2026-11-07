#!/usr/bin/env python3
"""Prints what a terminal shows after a capture of its output, as a person would see it.

    screen.py CAPTURE [COLS ROWS]

The capture is fed through a terminal emulator (pyte) with 2000 lines of scrollback; the output is
the scrollback followed by the visible screen, trailing blanks removed, so a line that scrolled off
the top is still shown and a line the program overwrote is not.
"""

import sys

import pyte


def main() -> int:
    path = sys.argv[1]
    cols = int(sys.argv[2]) if len(sys.argv) > 2 else 140
    rows = int(sys.argv[3]) if len(sys.argv) > 3 else 50
    screen = pyte.HistoryScreen(cols, rows, history=2000, ratio=0.5)
    stream = pyte.ByteStream(screen)
    with open(path, "rb") as f:
        stream.feed(f.read())
    lines = []
    for row in screen.history.top:
        lines.append("".join(row[x].data for x in range(cols)).rstrip())
    lines.extend(line.rstrip() for line in screen.display)
    while lines and not lines[-1]:
        lines.pop()
    sys.stdout.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
