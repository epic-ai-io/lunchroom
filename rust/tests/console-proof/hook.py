#!/usr/bin/env python3
"""Prints the input an OpenClaw plugin gives Lunchroom's guard for one event of one session.

    hook.py pretool SESSION 'COMMAND' [--no-channel]    a Bash-like `exec` call
    hook.py prompt  SESSION 'TEXT' [--typed|--untyped]  a prompt (untyped holds the session)

The plugin file named in the input holds `const PROGRAM = "<the installed program>";`, as the
plugin `lunchroom install openclaw` writes does. The text goes to the guard on its standard input.
"""

import json
import os
import sys

PROGRAM = "/usr/local/lib/lunchroom/lunchroom"
OPENCLAW_VERSION = "2026.10.7"


def main() -> int:
    if len(sys.argv) < 4 or sys.argv[1] not in ("pretool", "prompt"):
        print(__doc__, file=sys.stderr)
        return 2
    event, session, text = sys.argv[1], sys.argv[2], sys.argv[3]
    flags = sys.argv[4:]
    plugin_dir = os.environ.get("PLUGIN_DIR", "/home/dev/proof-plugin")
    plugin_file = os.path.join(plugin_dir, "index.mjs")
    if not os.path.exists(plugin_file):
        os.makedirs(plugin_dir, exist_ok=True)
        with open(plugin_file, "w") as f:
            f.write(f'const PROGRAM = "{PROGRAM}";\n')
    body = {
        "harness": "openclaw",
        "session_id": session,
        "cwd": os.environ.get("PROOF_CWD", "/home/dev"),
        "plugin_file": plugin_file,
        "harness_version": OPENCLAW_VERSION,
    }
    if event == "pretool":
        body.update(
            hook_event_name="PreToolUse",
            tool_name="exec",
            tool_input={"command": text},
            tool_use_id="t1",
            approval_channel="--no-channel" not in flags,
        )
    else:
        body.update(
            hook_event_name="UserPromptSubmit",
            prompt=text,
            prompt_id="p1",
            user_typed="--untyped" not in flags,
        )
    print(json.dumps(body))
    return 0


if __name__ == "__main__":
    sys.exit(main())
