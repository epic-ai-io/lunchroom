#!/bin/sh
# A PreToolUse hook of the proof's own, beside Lunchroom's, that says the same refusal in the output
# form named in /run/sess/hookmode. Each form carries lines marked with its own letter, so the proof
# can tell from the screen which field Claude Code showed. Lunchroom's own hook allows the call
# these variants judge (`echo variant-ran`).
cat > /dev/null
mode=$(cat /run/sess/hookmode 2>/dev/null)
case "$mode" in
a)  # exit 2 with the text on stderr
    printf 'A-LINE1 first line, on stderr\nA-LINE2 second line\nA-LINE3 third line\n' >&2
    exit 2 ;;
b)  # exit 0 with JSON: permissionDecision deny and permissionDecisionReason
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"B-LINE1 first line, in the reason\\nB-LINE2 second line\\nB-LINE3 third line"}}'
    exit 0 ;;
c)  # exit 0 with JSON: a top-level systemMessage and a deny with its own reason
    printf '{"systemMessage":"C-SYS1 first line, in systemMessage\\nC-SYS2 second line\\nC-SYS3 third line","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"C-REASON1 first line, in the reason\\nC-REASON2 second line\\nC-REASON3 third line"}}'
    exit 0 ;;
d)  # exit 0 with JSON: a top-level systemMessage alone (no decision: the call runs)
    printf '{"systemMessage":"D-SYS1 first line, in systemMessage alone\\nD-SYS2 second line\\nD-SYS3 third line"}'
    exit 0 ;;
*)  exit 0 ;;
esac
