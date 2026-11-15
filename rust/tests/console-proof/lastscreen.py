#!/usr/bin/env python3
"""Prints, or measures, the screen a terminal shows after a capture of the console's output.

    lastscreen.py CAPTURE [ROWSxCOLS]                  the whole screen as text (default 50x140)
    lastscreen.py CAPTURE [ROWSxCOLS] --find TEXT      "ROW COL" (1-based) of the first TEXT, or exit 1
    lastscreen.py CAPTURE [ROWSxCOLS] --cell TEXT      the colour of the alert-strip cell holding TEXT:
                                                       amber, red, green or dark; exit 1 if no TEXT
    lastscreen.py CAPTURE [ROWSxCOLS] --lines FROM TO  rows FROM..TO (1-based) only

1.1 (spec section 17 item 10): the console is a Ratatui screen that redraws only what changed, so
the capture is fed through a terminal emulator (pyte) and the screen read is the one a person
sees now. The colours are the screen's own (console/draw.rs: AMBER 255,176,0; RED 255,77,77;
GREEN 63,208,127): a cell is amber or red when its background is, green when its text is the
green, and dark otherwise. Colour is never the only sign, so a proof also reads the cell's word.
"""

import sys

import pyte

AMBER = "ffb000"
RED = "ff4d4d"
GREEN = "3fd07f"


def screen_of(path, rows, cols):
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    with open(path, "rb") as f:
        stream.feed(f.read())
    return screen


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    path = args.pop(0)
    rows, cols = 50, 140
    if args and "x" in args[0] and args[0].replace("x", "").isdigit():
        r, _, c = args.pop(0).partition("x")
        rows, cols = int(r), int(c)
    try:
        screen = screen_of(path, rows, cols)
    except OSError:
        return 1
    lines = [line.rstrip() for line in screen.display]
    if not args:
        sys.stdout.write("\n".join(lines).rstrip("\n") + "\n")
        return 0
    mode = args.pop(0)
    if mode == "--lines":
        lo, hi = int(args[0]), int(args[1])
        sys.stdout.write("\n".join(lines[lo - 1 : hi]) + "\n")
        return 0
    text = args[0]
    for y, line in enumerate(lines):
        x = line.find(text)
        if x < 0:
            continue
        if mode == "--find":
            print(y + 1, x + 1)
            return 0
        if mode == "--cell":
            char = screen.buffer[y][x]
            if char.bg == AMBER:
                print("amber")
            elif char.bg == RED:
                print("red")
            elif char.fg == GREEN:
                print("green")
            else:
                print("dark")
            return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
