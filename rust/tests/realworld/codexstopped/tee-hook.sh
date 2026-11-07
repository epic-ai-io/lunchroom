#!/bin/sh
# A hook of the proof's own, added beside Lunchroom's PreToolUse hook in Codex's hooks.json: it
# keeps a copy of exactly what Codex gives a PreToolUse hook (one JSON object per line), so the proof
# can compare the real input of an apply_patch call with the host facts. It decides nothing (exit 0).
cat >> /run/sess/pretool-inputs.jsonl
echo >> /run/sess/pretool-inputs.jsonl
exit 0
