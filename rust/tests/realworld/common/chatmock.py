#!/usr/bin/env python3
"""A stand-in for an OpenAI-compatible Chat Completions API on 127.0.0.1:8099, for the proofs that need
a real OpenClaw (or Hermes) to run a tool call, so that its real plugin hooks run.

The real harness is the client; nothing about it is faked. This server only answers its requests, with
no model. A user message holding one of these words makes the next answer a tool call:

    DENYTEST <label>    the shell tool: `codex queue other hi`
    CONTROLTEST <label> the shell tool: `ls /opt/proof`

The shell tool's name is $SHELL_TOOL (OpenClaw: exec; Hermes: terminal) and its command argument
$SHELL_ARG (command). The result the harness gives back for the call (the command's output, or the
plugin's refusal) is appended as one JSON line to $MOCK_LOG (default /run/sess/mock.log):
{"label": ..., "tool": ..., "result": ...}, the lines mockapi.py writes, so lib.sh's `probe` reads them.
Any other request is answered with the text "ok". Every request's tool names are appended to
$MOCK_LOG.requests for the proof to print.
"""

import json
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("MOCK_LOG", "/run/sess/mock.log")
SHELL_TOOL = os.environ.get("SHELL_TOOL", "exec")
SHELL_ARG = os.environ.get("SHELL_ARG", "command")
MARKER = re.compile(r"\b(DENYTEST|CONTROLTEST|PATCHTEST|PATCHPROT|PATCHLINK|PATCHLINKDIR|PATCHWSUPD|PATCHWSADD)\s+(\w+)")
COUNT = [0]


def text_of(content):
    if isinstance(content, str):
        return content
    return "\n".join(b.get("text", "") for b in content or [] if isinstance(b, dict))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            self.reply(200, {"object": "list", "data": [{"id": "mock-1", "object": "model"}]})
        else:
            self.reply(200, {})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
        except ValueError:
            body = {}
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self.reply(200, {})
            return
        messages = body.get("messages") or []
        with open(LOG + ".requests", "a") as f:
            f.write(json.dumps({"path": self.path, "tools": [t.get("function", {}).get("name") for t in body.get("tools") or []],
                                "roles": [m.get("role") for m in messages]}) + "\n")
        last = messages[-1] if messages else {}
        user_marker = None
        for m in reversed(messages):
            if m.get("role") == "user":
                user_marker = MARKER.search(text_of(m.get("content")))
                break
        COUNT[0] += 1
        n = COUNT[0]
        tool_calls = None
        content = "ok"
        if last.get("role") == "tool":
            kind, label = (user_marker.group(1), user_marker.group(2)) if user_marker else ("?", "?")
            with open(LOG, "a") as f:
                f.write(json.dumps({"label": label, "tool": kind, "result": text_of(last.get("content"))}) + "\n")
            content = "done"
        elif last.get("role") == "user" and user_marker:
            kind, label = user_marker.group(1), user_marker.group(2)
            if kind in ("PATCHTEST", "PATCHPROT", "PATCHLINK", "PATCHLINKDIR", "PATCHWSUPD", "PATCHWSADD"):
                # The harness's own apply_patch tool; the argument's name is read from the tool's schema
                # in this very request: `input` if it has one, else its first required property.
                # PATCHLINK: a path inside the workspace that is a symlink to a file of Lunchroom's
                # store (link-file.json); PATCHLINKDIR: a new file under a symlink to its folder (link-dir).
                path = {"PATCHTEST": "hello-%s.txt" % label,
                        "PATCHPROT": "/home/dev/.lunchroom/planted-%s.txt" % label,
                        "PATCHLINK": "link-file.json",
                        "PATCHLINKDIR": "link-dir/planted-%s.txt" % label,
                        "PATCHWSUPD": "ws-link", "PATCHWSADD": "ws-dir/new.txt"}[kind]
                if kind == "PATCHWSADD":
                    path = "ws-dir/new.txt"
                if kind == "PATCHWSUPD":
                    # An Update File hunk replacing the file's first line (state.json is one line), read
                    # from $UPDATE_TARGET at the moment of the call.
                    try:
                        first = open(os.environ.get("UPDATE_TARGET", "/nonexistent")).readline().rstrip("\n")
                    except OSError:
                        first = "{}"
                    patch = "*** Begin Patch\n*** Update File: ws-link\n@@\n-%s\n+hello\n*** End Patch\n" % first
                else:
                    patch = "*** Begin Patch\n*** Add File: %s\n+hello\n*** End Patch\n" % path
                schema = next((t.get("function", {}).get("parameters", {}) for t in body.get("tools") or []
                               if t.get("function", {}).get("name") == "apply_patch"), {})
                props = list((schema.get("properties") or {}).keys())
                arg = "input" if "input" in props else ((schema.get("required") or props or ["input"])[0])
                with open(LOG + ".requests", "a") as f:
                    f.write(json.dumps({"apply_patch_schema": schema}) + "\n")
                name, args = "apply_patch", {arg: patch}
            else:
                cmd = "codex queue other hi" if kind == "DENYTEST" else "ls /opt/proof"
                name, args = SHELL_TOOL, {SHELL_ARG: cmd}
            tool_calls = [{"index": 0, "id": "call_%d" % n, "type": "function",
                           "function": {"name": name, "arguments": json.dumps(args)}}]
        model = body.get("model", "mock-1")
        if not body.get("stream"):
            message = {"role": "assistant", "content": None if tool_calls else content}
            if tool_calls:
                message["tool_calls"] = [{k: v for k, v in tc.items() if k != "index"} for tc in tool_calls]
            self.reply(200, {"id": "chatcmpl-%d" % n, "object": "chat.completion", "created": 1, "model": model,
                             "choices": [{"index": 0, "message": message,
                                          "finish_reason": "tool_calls" if tool_calls else "stop"}],
                             "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
            return

        def chunk(delta, finish=None):
            obj = {"id": "chatcmpl-%d" % n, "object": "chat.completion.chunk", "created": 1, "model": model,
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            return ("data: %s\n\n" % json.dumps(obj)).encode()

        out = chunk({"role": "assistant"})
        if tool_calls:
            out += chunk({"tool_calls": tool_calls})
            out += chunk({}, "tool_calls")
        else:
            out += chunk({"content": content})
            out += chunk({}, "stop")
        usage = {"id": "chatcmpl-%d" % n, "object": "chat.completion.chunk", "created": 1, "model": model,
                 "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
        out += ("data: %s\n\n" % json.dumps(usage)).encode()
        out += b"data: [DONE]\n\n"
        self.reply(200, out, "text/event-stream")


if __name__ == "__main__":
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", 8099), Handler).serve_forever()
    sys.exit(0)
