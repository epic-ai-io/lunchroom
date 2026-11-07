#!/usr/bin/env python3
"""A stand-in for the OpenAI Responses API on 127.0.0.1:8099, for the proofs that need the real
Codex to run a tool call (so its real hooks run).

The real `codex` program is the client; nothing about it is faked. This server only answers its
requests, with no model. A prompt holding one of these words makes the next answer a tool call:

    PATCHTEST <label>   apply_patch (a custom tool call, as GPT-5 models use it): add hello-<label>.txt
    PATCHPROT <label>   apply_patch: add a file inside Lunchroom's own folder
    DENYTEST <label>    exec_command: `codex queue other hi`
    CONTROLTEST <label> exec_command: `ls /opt/proof`

The output Codex gives back for the call (the command's output, or the hook's refusal) is appended
as one JSON line to $MOCK_LOG (default /run/sess/mock.log): {"label": ..., "tool": ..., "result": ...},
the same lines mockapi.py writes for Claude Code, so lib.sh's `probe` reads either.
Any other request is answered with the text "ok".
"""

import json
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("MOCK_LOG", "/run/sess/mock.log")
MARKER = re.compile(r"\b(PATCHTEST|PATCHPROT|DENYTEST|CONTROLTEST)\s+(\w+)")
COUNT = [0]


def texts(items):
    out = []
    for it in items or []:
        if isinstance(it, dict) and it.get("type") == "message" and it.get("role") == "user":
            c = it.get("content")
            if isinstance(c, list):
                out.extend(b.get("text", "") for b in c if isinstance(b, dict))
            elif isinstance(c, str):
                out.append(c)
    return out


def last_marker(items):
    for t in reversed(texts(items)):
        found = MARKER.search(t)
        if found:
            return found.group(1), found.group(2)
    return None


def flat(output):
    if isinstance(output, str):
        return output
    return json.dumps(output)


def sse(name, data):
    return ("event: %s\ndata: %s\n\n" % (name, json.dumps(data))).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
        except ValueError:
            body = {}
        items = body.get("input") or []
        last = items[-1] if items else {}
        marker = last_marker(items)
        COUNT[0] += 1
        n = COUNT[0]
        if last.get("type") in ("custom_tool_call_output", "function_call_output"):
            kind, label = marker or ("?", "?")
            with open(LOG, "a") as f:
                f.write(json.dumps({"label": label, "tool": kind, "result": flat(last.get("output"))}) + "\n")
            item = {"type": "message", "role": "assistant", "id": "msg_%d" % n,
                    "content": [{"type": "output_text", "text": "done"}]}
        elif marker and MARKER.search(" ".join(texts([m for m in items if m.get("role") == "user"][-1:]))):
            # The newest user message holds the marker: a new prompt, not an old one in the history.
            kind, label = marker
            if kind == "PATCHTEST":
                patch = "*** Begin Patch\n*** Add File: hello-%s.txt\n+hello\n*** End Patch\n" % label
                item = {"type": "custom_tool_call", "call_id": "call_%d" % n, "name": "apply_patch", "input": patch}
            elif kind == "PATCHPROT":
                patch = "*** Begin Patch\n*** Add File: /home/dev/.lunchroom/planted-%s.txt\n+planted\n*** End Patch\n" % label
                item = {"type": "custom_tool_call", "call_id": "call_%d" % n, "name": "apply_patch", "input": patch}
            else:
                cmd = "codex queue other hi" if kind == "DENYTEST" else "touch /tmp/control-ran-%s && ls /opt/proof" % label
                item = {"type": "function_call", "call_id": "call_%d" % n, "name": "exec_command",
                        "arguments": json.dumps({"cmd": cmd})}
        else:
            item = {"type": "message", "role": "assistant", "id": "msg_%d" % n,
                    "content": [{"type": "output_text", "text": "ok"}]}
        out = sse("response.created", {"type": "response.created", "response": {"id": "resp_%d" % n}})
        out += sse("response.output_item.done", {"type": "response.output_item.done", "item": item})
        out += sse("response.completed", {"type": "response.completed", "response": {"id": "resp_%d" % n,
                   "usage": {"input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
                             "output_tokens_details": None, "total_tokens": 2}}})
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


if __name__ == "__main__":
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", 8099), Handler).serve_forever()
    sys.exit(0)
