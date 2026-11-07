#!/usr/bin/env python3
"""A stand-in for the Anthropic Messages API on 127.0.0.1:8099, for the proofs that need the real
Claude Code to run a tool call (so its real hooks run).

The real `claude` program is the client; nothing about it is faked. This server only answers its
requests, with no model:

- a prompt holding `DENYTEST <label>` is answered with a Bash tool call `codex queue other hi`;
- a prompt holding `CONTROLTEST <label>` is answered with a Bash tool call `ls /opt/proof`;
- a request whose last message is the tool's result is answered with a short text, and the
  result Claude Code gave back (the output of the command, or the hook's refusal) is appended as
  one JSON line to $MOCK_LOG (default /run/sess/mock.log): {"label": ..., "tool": ..., "result": ...};
- any other request (a join, a title, a count) is answered with the text "ok".

Both streaming and non-streaming requests are answered.
"""

import json
import os
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("MOCK_LOG", "/run/sess/mock.log")
MARKER = re.compile(r"\b(DENYTEST|CONTROLTEST|VARTEST)\s+(\w+)")
LOCK = threading.Lock()
COUNTER = [0]


def text_of(content):
    if isinstance(content, str):
        return content
    out = []
    for block in content or []:
        if isinstance(block, dict):
            if block.get("type") == "text":
                out.append(block.get("text", ""))
            elif block.get("type") == "tool_result":
                c = block.get("content")
                out.append(text_of(c) if not isinstance(c, str) else c)
    return "\n".join(out)


def plan(body):
    """What to answer: ('tool', command) | ('text', string), and what to log."""
    messages = body.get("messages") or []
    if not messages:
        return ("text", "ok"), None
    last = messages[-1]
    content = last.get("content")
    if last.get("role") == "user" and isinstance(content, list) and any(
        isinstance(b, dict) and b.get("type") == "tool_result" for b in content
    ):
        label, tool = "?", "?"
        for m in reversed(messages[:-1]):
            t = text_of(m.get("content")) if m.get("role") == "user" else ""
            found = MARKER.search(t) if t else None
            if found:
                tool, label = found.group(1), found.group(2)
                break
        result = text_of([b for b in content if b.get("type") == "tool_result"])
        return ("text", "done"), {"label": label, "tool": tool, "result": result}
    if last.get("role") == "user":
        found = MARKER.search(text_of(content))
        if found:
            command = {"DENYTEST": "codex queue other hi", "VARTEST": "echo variant-ran"}.get(found.group(1), "ls /opt/proof")
            return ("tool", command), None
    return ("text", "ok"), None


def sse(event, data):
    return f"event: {event}\ndata: {json.dumps(data)}\n\n".encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except ValueError:
            body = {}
        if not self.path.startswith("/v1/messages") or "count_tokens" in self.path:
            reply = json.dumps({"input_tokens": 1}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return
        answer, record = plan(body)
        if record:
            with LOCK:
                with open(LOG, "a") as f:
                    f.write(json.dumps(record) + "\n")
        with LOCK:
            COUNTER[0] += 1
            n = COUNTER[0]
        model = body.get("model", "claude-fable-5-1")
        usage = {"input_tokens": 1, "output_tokens": 1}
        if answer[0] == "tool":
            blocks = [{"type": "tool_use", "id": f"toolu_mock{n:06d}", "name": "Bash",
                       "input": {"command": answer[1]}}]
            stop = "tool_use"
        else:
            blocks = [{"type": "text", "text": answer[1]}]
            stop = "end_turn"
        message = {"id": f"msg_mock{n:06d}", "type": "message", "role": "assistant", "model": model,
                   "content": blocks, "stop_reason": stop, "stop_sequence": None, "usage": usage}
        if not body.get("stream"):
            reply = json.dumps(message).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def chunk(data):
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")

        start = dict(message, content=[], stop_reason=None)
        chunk(sse("message_start", {"type": "message_start", "message": start}))
        for i, b in enumerate(blocks):
            if b["type"] == "tool_use":
                chunk(sse("content_block_start", {"type": "content_block_start", "index": i,
                          "content_block": {"type": "tool_use", "id": b["id"], "name": "Bash", "input": {}}}))
                chunk(sse("content_block_delta", {"type": "content_block_delta", "index": i,
                          "delta": {"type": "input_json_delta", "partial_json": json.dumps(b["input"])}}))
            else:
                chunk(sse("content_block_start", {"type": "content_block_start", "index": i,
                          "content_block": {"type": "text", "text": ""}}))
                chunk(sse("content_block_delta", {"type": "content_block_delta", "index": i,
                          "delta": {"type": "text_delta", "text": b["text"]}}))
            chunk(sse("content_block_stop", {"type": "content_block_stop", "index": i}))
        chunk(sse("message_delta", {"type": "message_delta",
                  "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 5}}))
        chunk(sse("message_stop", {"type": "message_stop"}))
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def do_GET(self):
        reply = b"{}"
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(reply)))
        self.end_headers()
        self.wfile.write(reply)


if __name__ == "__main__":
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", 8099), Handler).serve_forever()
    sys.exit(0)
