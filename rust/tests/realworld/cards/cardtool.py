#!/usr/bin/env python3
"""Reads the records cardmock.py keeps and checks the form of a line of Lunchroom's.

    cardtool.py result LABEL [SECONDS]            the text the model was given back for the RUN call
                                                  LABEL (waits up to SECONDS, default 90); exit 1 if none
    cardtool.py heard SESSION NEEDLE SINCE [SECONDS] [KIND]
                                                  prints "KIND ROLE TIME" for each part the model of
                                                  SESSION was shown since SINCE (epoch seconds) that holds
                                                  NEEDLE; waits up to SECONDS (default 0); exit 1 if none.
                                                  KIND is prompt, tool_result or other (default: any)
    cardtool.py texts SESSION NEEDLE SINCE        prints each such part's whole text, parts separated by a
                                                  line of 20 equals signs
    cardtool.py dups SESSION NEEDLE SINCE         exit 1 when two parts of one kind hold the same text
                                                  with NEEDLE in it (the same notice told twice)
    cardtool.py lineform WORDS MINUTES NAMES      reads text on standard input; every line holding
                                                  "Lunchroom: WORDS" must be `HH:MM UTC Lunchroom: WORDS
                                                  (<agents>)` with HH:MM one of MINUTES (comma list) and
                                                  every name of NAMES (comma list) inside the brackets;
                                                  prints the lines; exit 1 if there is none or one is wrong
    cardtool.py envelope WORDS                    reads an envelope on standard input: the header line
                                                  `lunchroom delivery: N message(s)`, then the line of
                                                  WORDS, then one blank line, then the label
                                                  `[LUNCHROOM MESSAGE - from agent` and the label's end;
                                                  the line carries the definition of HELD when WORDS holds
                                                  HELD; exit 1 on the first thing wrong
    cardtool.py ids SESSION SINCE                 prints the message ids (`id: lr-00000007`) the model of
                                                  SESSION was shown since SINCE, oldest first; exit 1 if none
    cardtool.py ua SESSION                        the distinct User-Agents of the requests of SESSION
    cardtool.py minute EPOCH                     HH:MM in UTC of the second
"""

import json
import os
import re
import sys
import time

SESS = os.environ.get("SESS", "/run/sess")


def records(path):
    out = []
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        out.append(json.loads(line))
                    except ValueError:
                        pass
    except OSError:
        pass
    return out


def wait_for(fn, seconds):
    end = time.time() + seconds
    while True:
        got = fn()
        if got or time.time() >= end:
            return got
        time.sleep(0.5)


def matching(session, needle, since, kind=None):
    return [r for r in records(os.path.join(SESS, "model.jsonl"))
            if r.get("session") == session and r.get("t", 0) >= since and needle in r.get("text", "")
            and (kind is None or r.get("kind") == kind)]


def cmd_result(args):
    label = args[0]
    seconds = float(args[1]) if len(args) > 1 else 90

    def find():
        for r in records(os.path.join(SESS, "mock.log")):
            if r.get("label") == label:
                return r
        return None

    r = wait_for(find, seconds)
    if r is None:
        return 1
    sys.stdout.write(r.get("result", ""))
    return 0


def cmd_heard(args):
    session, needle, since = args[0], args[1], float(args[2])
    seconds = float(args[3]) if len(args) > 3 else 0
    kind = args[4] if len(args) > 4 and args[4] else None
    got = wait_for(lambda: matching(session, needle, since, kind), seconds)
    for r in got:
        print(r.get("kind"), r.get("role"), "%.1f" % r.get("t", 0))
    return 0 if got else 1


def cmd_texts(args):
    session, needle, since = args[0], args[1], float(args[2])
    got = matching(session, needle, since)
    print(("\n" + "=" * 20 + "\n").join(r.get("text", "") for r in got))
    return 0 if got else 1


def cmd_ids(args):
    session, since = args[0], float(args[1])
    found = []
    for r in matching(session, "id: lr-", since):
        for m in re.finditer(r"\bid: (lr-\d+)", r.get("text", "")):
            if m.group(1) not in found:
                found.append(m.group(1))
    print("\n".join(found))
    return 0 if found else 1


def cmd_ua(args):
    """Prints the distinct User-Agents of the requests of SESSION; exit 1 if there was none."""
    seen = []
    for r in records(os.path.join(SESS, "agents.jsonl")):
        if r.get("session") == args[0] and r.get("ua") not in seen:
            seen.append(r.get("ua"))
    print("\n".join(seen))
    return 0 if seen else 1


def cmd_dups(args):
    session, needle, since = args[0], args[1], float(args[2])
    seen = set()
    for r in matching(session, needle, since):
        key = (r.get("kind"), r.get("text"))
        if key in seen:
            return 1
        seen.add(key)
    return 0


LINE = re.compile(r"^(\d\d):(\d\d) UTC Lunchroom: (.*)$")


def cmd_lineform(args):
    words, minutes, names = args[0], args[1].split(","), [n for n in args[2].split(",") if n]
    text = sys.stdin.read()
    found = [ln.strip() for ln in text.splitlines() if ("Lunchroom: " + words) in ln]
    if not found:
        print("no line holding 'Lunchroom: %s'" % words)
        return 1
    bad = 0
    for ln in found:
        # A harness may draw its own mark before the line; the line starts at HH:MM.
        m = re.search(r"\d\d:\d\d UTC Lunchroom: .*$", ln)
        core = m.group(0) if m else ln
        m = LINE.match(core)
        ok = True
        why = ""
        if not m:
            ok, why = False, "not of the form HH:MM UTC Lunchroom: <words> (<agents>)"
        else:
            rest = m.group(3)
            if not rest.startswith(words + " ("):
                ok, why = False, "the words are not followed by ' (<agents>)': the card's words are not exact"
            else:
                inner = rest[len(words) + 2:].split(")")[0]
                if "%s:%s" % (m.group(1), m.group(2)) not in minutes:
                    ok, why = False, "time %s:%s is not the minute of the event (%s)" % (m.group(1), m.group(2), ",".join(minutes))
                missing = [n for n in names if n not in inner]
                if missing:
                    ok, why = False, "agents %r are not named in (%s)" % (missing, inner)
        print(("OK   " if ok else "BAD  ") + core + ("" if ok else "   <- " + why))
        bad += 0 if ok else 1
    return 1 if bad else 0


def cmd_envelope(args):
    words = args[0]
    lines = sys.stdin.read().split("\n")
    try:
        h = next(i for i, ln in enumerate(lines) if re.match(r"^lunchroom delivery: \d+ messages?$", ln.strip()))
    except StopIteration:
        print("no header line 'lunchroom delivery: N message'")
        return 1
    try:
        lab = next(i for i, ln in enumerate(lines) if ln.startswith("[LUNCHROOM MESSAGE - from agent"))
    except StopIteration:
        print("no label line '[LUNCHROOM MESSAGE - from agent'")
        return 1
    if lab <= h:
        print("the label comes before the header")
        return 1
    between = lines[h + 1:lab]
    if not between or between[-1].strip() != "":
        print("the lines are not followed by one blank line before the label: %r" % between[-3:])
        return 1
    block = [ln for ln in between[:-1]]
    if any(ln.strip() == "" for ln in block):
        print("a blank line stands between the header and the line")
        return 1
    mine = [ln for ln in block if ("Lunchroom: " + words) in ln]
    if not mine:
        print("the line 'Lunchroom: %s' is not between the header and the label; found %r" % (words, block))
        return 1
    if "HELD" in words and "a message from another agent is in this session" not in mine[0]:
        print("the line does not carry the definition of HELD after (<agents>): %s" % mine[0])
        return 1
    tail = "\n".join(lines[lab:])
    if "[END LUNCHROOM MESSAGE" not in tail:
        print("the label's end '[END LUNCHROOM MESSAGE' is missing: the label is not intact")
        return 1
    inside = "\n".join(lines[lab:lines.index(next(ln for ln in lines if ln.startswith('[END LUNCHROOM MESSAGE')))])
    if "Lunchroom: " + words in inside:
        print("the line stands inside the labelled body")
        return 1
    print("OK   header, line, one blank line, label, label end: " + mine[0])
    return 0


def main():
    cmds = {"result": cmd_result, "heard": cmd_heard, "texts": cmd_texts, "dups": cmd_dups, "ids": cmd_ids, "ua": cmd_ua,
            "lineform": cmd_lineform, "envelope": cmd_envelope}
    if len(sys.argv) >= 3 and sys.argv[1] == "minute":
        print(time.strftime("%H:%M", time.gmtime(int(float(sys.argv[2])))))
        return 0
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        print(__doc__)
        return 2
    return cmds[sys.argv[1]](sys.argv[2:])


if __name__ == "__main__":
    sys.exit(main())
