#!/bin/bash
# Proof 10, inside the container: the real Claude Code 2.1.311 runs a Bash tool call (its API answered by
# mockapi.py) that a hook of the proof's own (variant-hook.sh) refuses, said four ways. After each, the
# screen a person sees (screen.py) is read as Claude Code shows it by default, and again with the
# tool output expanded (ctrl+o). Each form's lines carry that form's own letter, so the screen says
# which field was shown and which was folded.
#
#   a  exit 2, text on stderr
#   b  exit 0, JSON hookSpecificOutput.permissionDecision "deny" + permissionDecisionReason
#   c  exit 0, JSON with a top-level systemMessage AND a deny with its own, different reason
#   d  exit 0, JSON with a top-level systemMessage alone (no decision)
#
# Docs read (https://code.claude.com/docs/en/hooks.md): exit 2 "The message that comes with the block
# is your hook's stderr"; systemMessage "Warning message shown to the user".

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "claude: $(/usr/bin/claude --version 2>&1 | head -1)"

make_user
export MOCK_API=1
seed_claude
install_lunchroom
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }

cp "$PROOF/variant-hook.sh" /usr/local/bin/variant-hook.sh
chmod 755 /usr/local/bin/variant-hook.sh
python3 - "$DEVHOME/.claude/settings.json" <<'PY'
import json, sys
p = sys.argv[1]
s = json.load(open(p))
s["hooks"]["PreToolUse"].append({"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/variant-hook.sh", "timeout": 5}]})
json.dump(s, open(p, "w"), indent=2)
PY
chown dev "$DEVHOME/.claude/settings.json"
echo a > "$SESS/hookmode"; chmod 666 "$SESS/hookmode"

flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/s1.log" 140 50; }
start_session s1 /usr/bin/claude --permission-mode default --allowedTools Bash
sleep 3
status | grep -q "^s1	" && pass "the session joined as s1" || fail "the session did not join"

# Control: with the variant hook off, the call runs.
echo off > "$SESS/hookmode"
R=$(probe s1 VARTEST ctl)
case "$R" in *variant-ran*) pass "control, variant hook off: the Bash call ran (output: variant-ran)" ;; *) fail "control: the call did not run: '${R:-nothing}'" ;; esac

# measure MODE LETTER: one call refused in form MODE; what the screen shows by default and expanded.
measure() {
    local mode=$1 letter=$2 label=$3 got def exp toggled
    echo "$mode" > "$SESS/hookmode"
    got=$(probe s1 VARTEST "$label")
    sleep 3
    def=$(screen | flat)
    echo "<CTRL-O>" > "$SESS/s1.keys"; sleep 3
    exp=$(screen | flat)
    echo "<CTRL-O>" > "$SESS/s1.keys"; sleep 2
    echo "---- form $mode: what the model was given back: $(echo "$got" | head -c 500)"
    for k in 1 2 3; do
        for field in LINE REASON SYS; do
            m="${letter}-${field}${k}"
            if echo "$got $def $exp" | grep -qF -- "$m"; then
                d=no; x=no
                echo "$def" | grep -qF -- "$m" && d=yes
                echo "$exp" | grep -qF -- "$m" && x=yes
                measured "form $mode, marker $m: on the default screen: $d; after ctrl+o: $x; in the result the model got: $(echo "$got" | grep -qF -- "$m" && echo yes || echo no)"
                eval "SEEN_${mode}_${field}${k}_default=$d; SEEN_${mode}_${field}${k}_expanded=$x"
            fi
        done
    done
    echo "---- default screen after form $mode (non-blank lines, last 14):"
    screen | grep -v '^\s*$' | tail -14 | sed 's/^/    | /'
}
measure a A a1
measure b B b1
measure c C c1
measure d D d1

heading "summary: which field the default screen showed"
for pair in "a LINE1" "b LINE1" "c SYS1" "c REASON1" "d SYS1"; do
    set -- $pair
    eval "dv=\${SEEN_${1}_${2}_default:-absent}; xv=\${SEEN_${1}_${2}_expanded:-absent}"
    echo "form $1, ${2} first line: default screen = $dv, after ctrl+o = $xv"
done

# What each form must do (docs): a, b, c block the call (the model's result is the refusal); d does not.
echo a > "$SESS/hookmode"
finish
