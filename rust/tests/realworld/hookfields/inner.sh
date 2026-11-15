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
# 1.1, spec section 17 item 2: the install is finished when the setgid program and /var/lib/lunchroom/<uid> are in place.
lr_installed && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }

# 1.1, spec section 13 item 5 (H1.5): the tool-call hook is `<program> guard pretool || exit 2` and,
# in Claude Code, carries "onFailure": "block"; the prompt hook carries neither.
python3 - "$DEVHOME/.claude/settings.json" "$LR" <<'PY' && pass "PreToolUse is '<program> guard pretool || ... exit 2' with onFailure block; UserPromptSubmit has neither (H1.5 item 5)" || fail "the hooks do not have the fail-closed form of H1.5 item 5"
import json, sys
s = json.load(open(sys.argv[1]))
pre = [h for g in s["hooks"]["PreToolUse"] for h in g["hooks"] if sys.argv[2] in h["command"]]
prompt = [h for g in s["hooks"]["UserPromptSubmit"] for h in g["hooks"] if sys.argv[2] in h["command"]]
ok = bool(pre) and bool(prompt)
ok = ok and all("guard pretool" in h["command"] and "exit 2" in h["command"] and h.get("onFailure") == "block" for h in pre)
ok = ok and all("exit 2" not in h["command"] and "onFailure" not in h for h in prompt)
sys.exit(0 if ok else 1)
PY

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

heading "the tool-call hook fails closed: with Lunchroom's program gone, a call is blocked (1.1, section 13 item 5)"
# Spec: "with the guard's program removed ... a tool call is blocked in Claude Code and in Codex and the
# blocked call's text names `lunchroom doctor`, while a prompt the user types still reaches the agent".
echo off > "$SESS/hookmode"
mv "$LR" "$LR.gone"
F=$(probe s1 CONTROLTEST fc1)
mv "$LR.gone" "$LR"
echo "MEASURED control call with the program removed: Claude Code gave back: $(echo "$F" | head -c 600)"
case "$F" in
    *lib.sh*) fail "the control call RAN with Lunchroom's program gone: the hook failed open" ;;
    "") fail "no result came back for the call made with the program gone" ;;
    *"lunchroom doctor"*) pass "the call was blocked and its text names 'lunchroom doctor'" ;;
    *) fail "the call did not run, but its text does not name 'lunchroom doctor': $(echo "$F" | head -c 300)" ;;
esac
G=$(probe s1 CONTROLTEST fc2)
case "$G" in *lib.sh*) pass "with the program put back the control call runs again" ;; *) fail "with the program put back the control call did not run: '${G:-nothing}'" ;; esac

# What each form must do (docs): a, b, c block the call (the model's result is the refusal); d does not.
echo a > "$SESS/hookmode"
finish
