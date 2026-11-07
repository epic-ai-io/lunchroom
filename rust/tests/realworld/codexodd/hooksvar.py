#!/usr/bin/env python3
"""Writes the hooks.json of one configuration of the codexodd proof.

    hooksvar.py BASE.json OUT.json CONFIG

BASE.json is the hooks.json `lunchroom install codex` wrote. CONFIG is:
  a  no hooks at all
  b  only the proof's capture hook (tee-hook.sh, PreToolUse)
  c  all of Lunchroom's hooks, as the install wrote them
  d  only Lunchroom's PreToolUse hook
  e  only Lunchroom's PostToolUse hook
Every command is run through tee-wrap.sh, which keeps its stdin, stdout, stderr and exit code.
"""

import copy
import json
import sys

WRAP = "/usr/local/bin/tee-wrap.sh"


def wrap(group_list, event):
    out = copy.deepcopy(group_list)
    for group in out:
        for hook in group.get("hooks", []):
            hook["command"] = f"{WRAP} {event} {hook['command']}"
    return out


def main() -> int:
    base_path, out_path, config = sys.argv[1:4]
    base = json.load(open(base_path))["hooks"]
    if config == "a":
        hooks = {}
    elif config == "b":
        hooks = {"PreToolUse": [{"hooks": [{"type": "command", "command": f"{WRAP} capture /usr/local/bin/tee-hook.sh", "timeout": 5}]}]}
    elif config == "c":
        hooks = {event: wrap(groups, event) for event, groups in base.items()}
    elif config == "d":
        hooks = {"PreToolUse": wrap(base["PreToolUse"], "PreToolUse")}
    elif config == "e":
        hooks = {"PostToolUse": wrap(base["PostToolUse"], "PostToolUse")}
    else:
        return 2
    json.dump({"hooks": hooks}, open(out_path, "w"), indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
