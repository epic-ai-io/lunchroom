#!/usr/bin/env python3
"""A scripted model for the card kits: one server, three wire formats, no model.

The harness (Claude Code, Codex, OpenClaw or Hermes) is the real client; this server only answers
its requests on 127.0.0.1:$MOCK_PORT (default 8099), so that a prompt typed to a session becomes the
tool call the card needs and the harness's real hooks run on it.

  Anthropic Messages   POST /v1/messages          (Claude Code)
  OpenAI Responses     POST /v1/responses         (Codex)
  OpenAI Chat          POST /v1/chat/completions  (OpenClaw, Hermes)

A user prompt holding `RUN <label>` makes the next answer one shell tool call whose command is the
exact text of $SESS/cmd-<label>.txt (the kit writes the file first). A label is `<session>-<n>`; the
part before the last `-` names the session the request belongs to. The shell tool is `Bash` for
Claude Code, `exec_command` for Codex, and for the chat format the first of exec, terminal, bash
that the request offers. Any other request is answered with the text `ok`.

Three records are kept, all under $SESS (agents.jsonl: one line per request, {"t", "session", "label",
"ua"}, the User-Agent of the client that made it):

  mock.log     one JSON line per tool result the harness gave back for a RUN call:
               {"label", "session", "t", "result"}. `result` is the text the model was given: the
               command's output, or the hook's refusal. This is what a sender hears by channel S.
  model.jsonl  one JSON line per part of a request that is new to its conversation:
               {"t", "session", "label", "role", "kind", "text"}. `kind` is `prompt` (a user or
               developer text), `tool_result`, or `other`. A part the harness repeats from an earlier
               request of the same conversation is not logged again, so a line that reaches the model
               once is logged once and a line that reaches it twice is logged twice. This is what the
               model was shown, by whichever channel carried it.

Nothing here names the program under test.
"""

import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SESS = os.environ.get("SESS", "/run/sess")
MOCK_LOG = os.path.join(SESS, "mock.log")
MODEL_LOG = os.path.join(SESS, "model.jsonl")
AGENT_LOG = os.path.join(SESS, "agents.jsonl")
MARKER = re.compile(r"\bRUN ([A-Za-z0-9_.]+(?:-[A-Za-z0-9_.]+)*)")
LOCK = threading.Lock()
COUNTER = [0]
LAST_PARTS = {}  # conversation key -> the parts of its previous request


def session_of(label):
    return label.rsplit("-", 1)[0] if "-" in label else label


def write_line(path, record):
    with LOCK:
        with open(path, "a") as f:
            f.write(json.dumps(record) + "\n")


def flatten(node, role, kind, out):
    """Appends (role, kind, text) for every text leaf of a request's messages."""
    if isinstance(node, str):
        if node.strip():
            out.append((role, kind, node))
    elif isinstance(node, list):
        for item in node:
            flatten(item, role, kind, out)
    elif isinstance(node, dict):
        t = node.get("type", "")
        r = node.get("role", role)
        k = kind
        if r == "tool" or t.endswith("_output") or t == "tool_result":
            k = "tool_result"
        elif t in ("function_call", "custom_tool_call", "tool_use", "reasoning"):
            k = "other"
        elif r in ("user", "developer", "system") and kind != "tool_result":
            k = "prompt"
        if t in ("function_call", "custom_tool_call", "tool_use"):
            # What the model itself asked for is not what it was shown.
            return
        for key in ("text", "output", "content", "input_text", "arguments"):
            if key in node:
                if key == "arguments":
                    continue
                flatten(node[key], r, k, out)


def parts_of(body, proto):
    out = []
    if proto == "anthropic":
        if body.get("system"):
            flatten(body["system"], "system", "other", out)
        for m in body.get("messages") or []:
            flatten(m.get("content"), m.get("role", "user"), "prompt" if m.get("role") == "user" else "other", out)
    elif proto == "responses":
        if isinstance(body.get("input"), str):
            out.append(("user", "prompt", body["input"]))
        for it in body.get("input") or []:
            if isinstance(it, dict):
                flatten(it, it.get("role", "user"), "other", out)
    else:
        for m in body.get("messages") or []:
            role = m.get("role", "user")
            kind = "tool_result" if role == "tool" else ("prompt" if role in ("user", "system", "developer") else "other")
            flatten(m.get("content"), role, kind, out)
    return out


def latest_label(parts):
    found = None
    for role, kind, text in parts:
        if kind == "prompt" and role == "user":
            m = MARKER.search(text)
            if m:
                found = m.group(1)
    return found


def log_request(body, proto):
    parts = parts_of(body, proto)
    label = latest_label(parts) or "none"
    session = session_of(label)
    # The kit writes a new number to $SESS/generation at the start of each cell, so a cell never shares
    # a conversation's history with the one before it.
    try:
        with open(os.path.join(SESS, "generation")) as f:
            generation = f.read().strip()
    except OSError:
        generation = ""
    key = (generation, session, bool(body.get("tools")))
    prev = LAST_PARTS.get(key, [])
    common = 0
    for a, b in zip(prev, parts):
        if a != b:
            break
        common += 1
    LAST_PARTS[key] = parts
    now = time.time()
    for role, kind, text in parts[common:]:
        write_line(MODEL_LOG, {"t": now, "session": session, "label": label, "role": role, "kind": kind, "text": text})
    return parts, label


def log_agent(headers, label):
    """Which program made the request: its User-Agent, kept so that a kit can show that the client is the
    harness itself and not a stand-in."""
    write_line(AGENT_LOG, {"t": time.time(), "session": session_of(label), "label": label,
                           "ua": headers.get("User-Agent", "")})


def command_for(label):
    try:
        with open(os.path.join(SESS, "cmd-%s.txt" % label)) as f:
            return f.read()
    except OSError:
        return "echo no-command-file-for-%s" % label


def pending_marker(parts):
    """The label of a RUN prompt that is the newest user prompt and has not yet had its tool call."""
    last_user = None
    for i, (role, kind, text) in enumerate(parts):
        if kind == "prompt" and role == "user" and MARKER.search(text):
            last_user = i
    return last_user


def sse(event, data):
    return ("event: %s\ndata: %s\n\n" % (event, json.dumps(data))).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send_json(self, code, obj):
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            self.send_json(200, {"object": "list", "data": [{"id": "mock-1", "object": "model"}]})
        else:
            self.send_json(200, {})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
        except ValueError:
            body = {}
        path = self.path.split("?")[0].rstrip("/")
        with LOCK:
            COUNTER[0] += 1
            count = COUNTER[0]
        if path.endswith("/messages/count_tokens") or "count_tokens" in path:
            self.send_json(200, {"input_tokens": 1})
        elif path.endswith("/v1/messages"):
            self.anthropic(body, count)
        elif path.endswith("/responses"):
            self.responses(body, count)
        elif path.endswith("/chat/completions"):
            self.chat(body, count)
        else:
            self.send_json(200, {})

    # -- what to answer, the same for all three formats -------------------------------------------
    def plan(self, parts, label, last_is_result, has_tools=True):
        """('tool', command) or ('text', words). A result that came back is recorded."""
        idx = pending_marker(parts)
        if last_is_result:
            results = [t for (r, k, t) in parts[idx:] if k == "tool_result"] if idx is not None else []
            if label and label != "none":
                write_line(MOCK_LOG, {"label": label, "session": session_of(label), "t": time.time(),
                                      "result": results[-1] if results else ""})
            return ("text", "done")
        if idx is not None and has_tools:
            # A request with no tools (a title or a count) is never answered with a tool call.
            after = parts[idx + 1:]
            # The newest RUN prompt, with no tool result and no later user prompt after it: a new turn.
            if all(k != "tool_result" and not (k == "prompt" and r == "user") for (r, k, _) in after):
                return ("tool", command_for(label))
        return ("text", "ok")

    # -- Anthropic Messages ---------------------------------------------------------------------
    def anthropic(self, body, count):
        parts, label = log_request(body, "anthropic")
        log_agent(self.headers, label)
        msgs = body.get("messages") or []
        last = msgs[-1] if msgs else {}
        c = last.get("content")
        last_is_result = last.get("role") == "user" and isinstance(c, list) and any(
            isinstance(b, dict) and b.get("type") == "tool_result" for b in c)
        answer = self.plan(parts, label, last_is_result, bool(body.get("tools")))
        model = body.get("model", "mock-model")
        if answer[0] == "tool":
            blocks = [{"type": "tool_use", "id": "toolu_mock%06d" % count, "name": "Bash", "input": {"command": answer[1]}}]
            stop = "tool_use"
        else:
            blocks = [{"type": "text", "text": answer[1]}]
            stop = "end_turn"
        message = {"id": "msg_mock%06d" % count, "type": "message", "role": "assistant", "model": model,
                   "content": blocks, "stop_reason": stop, "stop_sequence": None,
                   "usage": {"input_tokens": 1, "output_tokens": 1}}
        if not body.get("stream"):
            self.send_json(200, message)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def chunk(data):
            self.wfile.write(("%x\r\n" % len(data)).encode() + data + b"\r\n")

        chunk(sse("message_start", {"type": "message_start", "message": dict(message, content=[], stop_reason=None)}))
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
        chunk(sse("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
                  "usage": {"output_tokens": 5}}))
        chunk(sse("message_stop", {"type": "message_stop"}))
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    # -- OpenAI Responses ---------------------------------------------------------------------------
    def responses(self, body, count):
        parts, label = log_request(body, "responses")
        log_agent(self.headers, label)
        items = body.get("input") or []
        last = items[-1] if items and isinstance(items[-1], dict) else {}
        last_is_result = last.get("type") in ("custom_tool_call_output", "function_call_output")
        answer = self.plan(parts, label, last_is_result)
        if answer[0] == "tool":
            item = {"type": "function_call", "call_id": "call_%d" % count, "name": "exec_command",
                    "arguments": json.dumps({"cmd": answer[1]})}
        else:
            item = {"type": "message", "role": "assistant", "id": "msg_%d" % count,
                    "content": [{"type": "output_text", "text": answer[1]}]}
        out = sse("response.created", {"type": "response.created", "response": {"id": "resp_%d" % count}})
        out += sse("response.output_item.done", {"type": "response.output_item.done", "item": item})
        out += sse("response.completed", {"type": "response.completed", "response": {
            "id": "resp_%d" % count,
            "usage": {"input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
                      "output_tokens_details": None, "total_tokens": 2}}})
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    # -- OpenAI Chat Completions --------------------------------------------------------------------
    def chat(self, body, count):
        parts, label = log_request(body, "chat")
        log_agent(self.headers, label)
        msgs = body.get("messages") or []
        last = msgs[-1] if msgs else {}
        answer = self.plan(parts, label, last.get("role") == "tool")
        names = [t.get("function", {}).get("name") for t in body.get("tools") or []]
        tool = next((n for n in ("exec", "terminal", "bash") if n in names), "exec")
        arg = "command"
        tool_calls = None
        content = answer[1]
        if answer[0] == "tool":
            tool_calls = [{"index": 0, "id": "call_%d" % count, "type": "function",
                           "function": {"name": tool, "arguments": json.dumps({arg: answer[1]})}}]
        model = body.get("model", "mock-1")
        if not body.get("stream"):
            message = {"role": "assistant", "content": None if tool_calls else content}
            if tool_calls:
                message["tool_calls"] = [{k: v for k, v in tc.items() if k != "index"} for tc in tool_calls]
            self.send_json(200, {"id": "chatcmpl-%d" % count, "object": "chat.completion", "created": 1, "model": model,
                                 "choices": [{"index": 0, "message": message,
                                              "finish_reason": "tool_calls" if tool_calls else "stop"}],
                                 "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
            return

        def chunk(delta, finish=None):
            obj = {"id": "chatcmpl-%d" % count, "object": "chat.completion.chunk", "created": 1, "model": model,
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            return ("data: %s\n\n" % json.dumps(obj)).encode()

        out = chunk({"role": "assistant"})
        if tool_calls:
            out += chunk({"tool_calls": tool_calls})
            out += chunk({}, "tool_calls")
        else:
            out += chunk({"content": content})
            out += chunk({}, "stop")
        usage = {"id": "chatcmpl-%d" % count, "object": "chat.completion.chunk", "created": 1, "model": model,
                 "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
        out += ("data: %s\n\n" % json.dumps(usage)).encode()
        out += b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


if __name__ == "__main__":
    os.makedirs(SESS, exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", int(os.environ.get("MOCK_PORT", "8099"))), Handler).serve_forever()
    sys.exit(0)
