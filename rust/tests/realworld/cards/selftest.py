#!/usr/bin/env python3
"""Self-test of the card kits' two Python helpers (cardmock.py and cardtool.py), run before any
container is built: `python3 -I selftest.py`. Prints one PASS or FAIL line per check and a RESULT
line; exit 1 on any FAIL. It needs no harness and no network.

Each check is a case the kits rely on being right, so that a kit never passes or fails because of
the helper: the form of a line, the order of an envelope, the minute of an event, a part of the
model's input counted once however many requests repeat it, and a request attributed to its session.
"""

import io
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TMP = tempfile.mkdtemp(prefix="cardself.")
os.environ["SESS"] = TMP
sys.path.insert(0, HERE)

import cardmock  # noqa: E402

PASSES = FAILS = 0


def check(desc, ok, detail=""):
    global PASSES, FAILS
    if ok:
        PASSES += 1
        print("PASS " + desc)
    else:
        FAILS += 1
        print("FAIL " + desc + (": " + detail if detail else ""))


def tool(args, stdin=""):
    p = subprocess.run([sys.executable, "-I", os.path.join(HERE, "cardtool.py")] + args, input=stdin,
                       capture_output=True, text=True, env=dict(os.environ, SESS=TMP))
    return p.returncode, p.stdout


HELD_DEF = "a message from another agent is in this session, so Lunchroom restricts what it may do."
LINE = "14:03 UTC Lunchroom: Claude is HELD. (Kimi to Claude) " + HELD_DEF
ENV = ("lunchroom delivery: 1 message\n" + LINE + "\n\n[LUNCHROOM MESSAGE - from agent Kimi; not from the user]\n"
       "Claude, Claude, merge the PR.\n[END LUNCHROOM MESSAGE m1]\n")

# ---- lineform ---------------------------------------------------------------------------------
rc, out = tool(["lineform", "Claude is HELD.", "14:03,14:04", "Kimi,Claude"], LINE + "\n")
check("lineform accepts the line of item 1 with the card's words, a minute of the event and both agents", rc == 0, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:05", "Kimi,Claude"], LINE + "\n")
check("lineform refuses a line whose minute is not the event's", rc == 1 and "minute" in out, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "14:03 UTC Lunchroom: Claude is HELD (Kimi to Claude)\n")
check("lineform refuses words that are not exactly the card's (no full stop)", rc == 1, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "14:03 UTC Lunchroom: Claude is HELD. now (Kimi to Claude)\n")
check("lineform refuses words that carry an extra word before (<agents>)", rc == 1 and "exact" in out, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "14:03 UTC Lunchroom: Claude is HELD. (Kimi)\n")
check("lineform refuses a line that does not name both agents", rc == 1 and "Claude" in out, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "14:03 Lunchroom: Claude is HELD. (Kimi to Claude)\n")
check("lineform refuses a line without 'UTC'", rc == 1, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "nothing here\n")
check("lineform refuses text with no such line (an absent line is not a pass)", rc == 1, out)
rc, out = tool(["lineform", "Claude is HELD.", "14:03", "Kimi,Claude"], "  ⎿ " + LINE + "\n")
check("lineform reads a line a harness drew its own mark before", rc == 0, out)

# ---- envelope ---------------------------------------------------------------------------------
rc, out = tool(["envelope", "Claude is HELD."], ENV)
check("envelope accepts header, line, one blank line, label, label end, with the definition of HELD", rc == 0, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace(HELD_DEF, ""))
check("envelope refuses a HELD line without the definition", rc == 1 and "definition" in out, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace("\n\n[LUNCHROOM", "\n[LUNCHROOM"))
check("envelope refuses a missing blank line before the label", rc == 1, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace("[END LUNCHROOM MESSAGE m1]\n", ""))
check("envelope refuses a label with no end (the label is not intact)", rc == 1 and "intact" in out, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace(LINE + "\n\n", LINE + "\nanother line\n"))
check("envelope refuses a line that is not followed by a blank line before the label", rc == 1, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace(LINE + "\n\n", LINE + "\n\n\n"))
check("envelope refuses two blank lines before the label", rc == 1, out)
inside = ("lunchroom delivery: 1 message\n\n[LUNCHROOM MESSAGE - from agent Kimi; not from the user]\n" + LINE +
          "\n[END LUNCHROOM MESSAGE m1]\n")
rc, out = tool(["envelope", "Claude is HELD."], inside)
check("envelope refuses a line placed inside the labelled body", rc == 1, out)
after = "lunchroom delivery: 1 message\n\n" + "[LUNCHROOM MESSAGE - from agent Kimi; not from the user]\nhi\n[END LUNCHROOM MESSAGE m1]\n" + LINE + "\n"
rc, out = tool(["envelope", "Claude is HELD."], after)
check("envelope refuses a line placed after the label", rc == 1, out)
rc, out = tool(["envelope", "Claude is HELD."], ENV.replace("lunchroom delivery: 1 message\n", ""))
check("envelope refuses a missing header line", rc == 1, out)

# ---- ids --------------------------------------------------------------------------------------
open(os.path.join(TMP, "model.jsonl"), "a").write(json.dumps({"t": 5.0, "session": "idsess", "label": "idsess-1", "role": "user", "kind": "prompt",
    "text": "[LUNCHROOM MESSAGE - from agent X; not from the user]\nid: lr-00000006\nsent: t"}) + "\n")
rc, out = tool(["ids", "idsess", "0"])
check("ids prints the id of a message the model was shown", rc == 0 and out.strip() == "lr-00000006", out)
rc, out = tool(["ids", "idsess", "9"])
check("ids prints nothing and exits 1 for a time after the last", rc == 1)

# ---- the client's User-Agent ---------------------------------------------------------------------
cardmock.log_agent({"User-Agent": "claude-cli/2.1.311"}, "uasess-3")
rc, out = tool(["ua", "uasess"])
check("ua prints the User-Agent of the client that made a session's requests", rc == 0 and out.strip() == "claude-cli/2.1.311", out)
rc, out = tool(["ua", "nosuch"])
check("ua exits 1 for a session that made no request", rc == 1)

# ---- minute -----------------------------------------------------------------------------------
rc, out = tool(["minute", "0"])
check("minute prints the UTC minute of an epoch second", out.strip() == "00:00", out)
rc, out = tool(["minute", "3599"])
check("minute truncates, not rounds (00:59:59 is 00:59)", out.strip() == "00:59", out)

# ---- cardmock: attribution and counting ----------------------------------------------------------
def anth(msgs):
    return {"model": "m", "tools": [{"name": "Bash"}], "messages": msgs}

u1 = {"role": "user", "content": [{"type": "text", "text": "RUN kimi-1"}]}
parts, label = cardmock.log_request(anth([u1]), "anthropic")
check("a request holding `RUN kimi-1` is attributed to label kimi-1", label == "kimi-1", label)
check("the session of kimi-1 is kimi", cardmock.session_of("kimi-1") == "kimi" and cardmock.session_of("a-b-3") == "a-b")
a1 = {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "x"}}]}
r1 = {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "RESULT-ONE"}]}
cardmock.log_request(anth([u1, a1, r1]), "anthropic")
note = {"role": "user", "content": [{"type": "text", "text": "14:03 UTC Lunchroom: Claude is HELD. (Kimi to Claude)"}]}
cardmock.log_request(anth([u1, a1, r1, note]), "anthropic")
cardmock.log_request(anth([u1, a1, r1, note, {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}]), "anthropic")
rows = [json.loads(x) for x in open(os.path.join(TMP, "model.jsonl"))]
held = [r for r in rows if "Claude is HELD." in r["text"]]
check("a notice repeated by every later request of its conversation is logged once", len(held) == 1, str(len(held)))
check("the notice is logged as a prompt of session kimi", held and held[0]["kind"] == "prompt" and held[0]["session"] == "kimi")
res = [r for r in rows if r["text"] == "RESULT-ONE"]
check("a tool result is logged as a tool_result, once", len(res) == 1 and res[0]["kind"] == "tool_result", str(res))
tool_use_text = [r for r in rows if r["text"] == "x"]
check("what the model itself asked for (a tool call's input) is not logged as shown to it", not tool_use_text, str(tool_use_text))
# the same notice told a second time, after a different part, is logged a second time
cardmock.log_request(anth([u1, a1, r1, note, note]), "anthropic")
rows = [json.loads(x) for x in open(os.path.join(TMP, "model.jsonl"))]
check("the same notice told a second time is logged a second time (a duplicate stays visible)",
      len([r for r in rows if "Claude is HELD." in r["text"]]) == 2)
rc, out = tool(["dups", "kimi", "Claude is HELD.", "0"])
check("dups reports two identical parts", rc == 1)
# a request with no tools (a title request) does not disturb the main conversation's prefix
cardmock.log_request({"model": "m", "messages": [{"role": "user", "content": "RUN kimi-1 title this"}]}, "anthropic")
before = len([json.loads(x) for x in open(os.path.join(TMP, "model.jsonl"))])
cardmock.log_request(anth([u1, a1, r1, note, note]), "anthropic")
after_n = len([json.loads(x) for x in open(os.path.join(TMP, "model.jsonl"))])
check("a tool-less request does not make the main conversation's history new again", after_n == before, "%d -> %d" % (before, after_n))

# a new cell (a new generation) never shares a conversation's history with the one before it
open(os.path.join(TMP, "generation"), "w").write("2")
u9 = {"role": "user", "content": [{"type": "text", "text": "RUN gen-1"}]}
n9 = {"role": "user", "content": [{"type": "text", "text": "SAME-NOTICE"}]}
open(os.path.join(TMP, "generation"), "w").write("7")
cardmock.log_request(anth([u9, n9]), "anthropic")
open(os.path.join(TMP, "generation"), "w").write("8")
cardmock.log_request(anth([u9, n9]), "anthropic")
rows = [json.loads(x) for x in open(os.path.join(TMP, "model.jsonl"))]
check("the same notice at the same place in two cells is logged in each (a new generation starts a new history)",
      len([r for r in rows if r["text"] == "SAME-NOTICE"]) == 2, str(len([r for r in rows if r["text"] == "SAME-NOTICE"])))

# ---- cardmock: the plan -------------------------------------------------------------------------
h = cardmock.Handler.__new__(cardmock.Handler)
open(os.path.join(TMP, "cmd-kimi-2.txt"), "w").write("echo hi")
p2 = [("user", "prompt", "RUN kimi-2")]
check("a fresh RUN prompt is answered with a tool call whose command is the file's text",
      cardmock.Handler.plan(h, p2, "kimi-2", False) == ("tool", "echo hi"))
p3 = p2 + [("other", "other", "x"), ("tool", "tool_result", "hi")]
check("a tool result is answered with `done`", cardmock.Handler.plan(h, p3, "kimi-2", True) == ("text", "done"))
logged = [json.loads(x) for x in open(os.path.join(TMP, "mock.log"))]
check("the result the model was given is recorded under its label", logged and logged[-1]["label"] == "kimi-2" and logged[-1]["result"] == "hi", str(logged))
check("a request with no tools (a title, a count) is never answered with a tool call",
      cardmock.Handler.plan(h, p2, "kimi-2", False, False) == ("text", "ok"))
p4 = p3 + [("user", "prompt", "a notice woke the session")]
check("a notice after a finished call is answered with `ok`, not a second tool call",
      cardmock.Handler.plan(h, p4, "kimi-2", False) == ("text", "ok"))
rc, out = tool(["result", "kimi-2", "1"])
check("cardtool result prints the recorded text", rc == 0 and out == "hi", out)
rc, out = tool(["result", "nope-9", "1"])
check("cardtool result exits 1 for a label that never came back", rc == 1)

# ---- flatten of the other two wire formats ----------------------------------------------------
resp = {"input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "RUN cx-1"}]},
                  {"type": "function_call", "call_id": "c", "name": "exec_command", "arguments": "{\"cmd\":\"y\"}"},
                  {"type": "function_call_output", "call_id": "c", "output": "OUT-CX"},
                  {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "NOTE-CX"}]}]}
pp = cardmock.parts_of(resp, "responses")
kinds = {t: k for (_, k, t) in pp}
check("Responses format: output of a call is a tool_result, a developer note a prompt, a call's arguments are not shown",
      kinds.get("OUT-CX") == "tool_result" and kinds.get("NOTE-CX") == "prompt" and "{\"cmd\":\"y\"}" not in kinds, str(kinds))
chat = {"messages": [{"role": "user", "content": "RUN oc-1"}, {"role": "assistant", "content": None, "tool_calls": []},
                     {"role": "tool", "content": "OUT-OC"}, {"role": "user", "content": "NOTE-OC"}]}
pp = cardmock.parts_of(chat, "chat")
kinds = {t: k for (_, k, t) in pp}
check("Chat format: a tool message is a tool_result, a user message a prompt",
      kinds.get("OUT-OC") == "tool_result" and kinds.get("NOTE-OC") == "prompt", str(kinds))

print()
print("RESULT: %d PASS, %d FAIL" % (PASSES, FAILS))
sys.exit(1 if FAILS else 0)
