#!/usr/bin/env python3
"""Prints a terminal capture as plain text: escape sequences removed, carriage returns dropped.

    plaintext.py CAPTURE
"""

import re
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        with open(sys.argv[1], "rb") as f:
            text = f.read().decode("utf-8", "replace")
    except OSError:
        return 1
    text = re.sub(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", "", text)
    text = re.sub(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]", "", text)
    text = re.sub(r"\x1b[()][A-Za-z0-9]", "", text)
    sys.stdout.write(text.replace("\r\n", "\n").replace("\r", "\n"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
