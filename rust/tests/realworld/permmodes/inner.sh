#!/bin/bash
# Proof 15 (permmodes), inside the container: a real Lunchroom DENY (`codex queue other hi`) in the real
# Claude Code 2.1.311, in each permission mode (--permission-mode default, acceptEdits,
# bypassPermissions, dontAsk, plan), the Bash tool pre-allowed (--allowedTools Bash) so that Claude
# Code's own permission rules are not what stops the call. For each mode:
#   1. the call must not run (the result the model gets is Lunchroom's refusal, not a shell's);
#   2. the default screen must show "Lunchroom: DENIED" and "Ways forward" (the systemMessage);
#   3. the model must get the reason ("What was DENIED", "Why:", "Ways forward").

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "claude: $(/usr/bin/claude --version 2>&1 | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
export MOCK_API=1
seed_claude
install_lunchroom
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }

# bypassPermissions asks "WARNING: Claude Code running in Bypass Permissions mode ... No, exit / Yes, I
# accept" at start. The setting that records the user's acceptance is set, as a person who accepted once
# would have it (documented setting skipDangerousModePermissionPrompt); the proof does not click it.
python3 - "$DEVHOME/.claude/settings.json" <<'PY'
import json, sys
p = sys.argv[1]
s = json.load(open(p))
s["skipDangerousModePermissionPrompt"] = True
json.dump(s, open(p, "w"), indent=2)
PY
chown dev "$DEVHOME/.claude/settings.json"

flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/$1.log" 140 50; }

for mode in default acceptEdits bypassPermissions dontAsk plan; do
    heading "permission mode: $mode"
    name="m_$mode"
    start_session "$name" /usr/bin/claude --permission-mode "$mode" --allowedTools Bash
    sleep 3
    # The session must be up in this mode before the call is judged.
    S=$(screen "$name" | flat)
    echo "$S" | grep -q "include this session" && measured "$mode: the SessionStart text is on the screen" \
        || measured "$mode: the SessionStart text is not on the screen (the screen starts: $(echo "$S" | head -c 200))"
    R=$(probe "$name" DENYTEST d1)
    sleep 3
    D=$(screen "$name" | flat)
    echo "---- $mode: what the model was given back: $(echo "$R" | head -c 600)"
    echo "---- $mode: default screen (non-blank lines, last 16):"
    screen "$name" | grep -v '^\s*$' | tail -16 | sed 's/^/    | /'
    # 1. the call did not run: the result is Lunchroom's refusal; a shell would say "command not found".
    case "$R" in
        *DENIED*) echo "$R" | grep -q "command not found" \
            && fail "$mode: the result holds both DENIED and 'command not found'" \
            || pass "$mode: the call did not run (the model's result is Lunchroom's refusal, no shell output)" ;;
        "") fail "$mode: no result came back for the call" ;;
        *) fail "$mode: the call was not refused by Lunchroom (the result: $(echo "$R" | head -c 200))" ;;
    esac
    # 2. the default screen
    echo "$D" | grep -qF "Lunchroom: DENIED" && pass "$mode: the default screen shows 'Lunchroom: DENIED'" \
        || fail "$mode: the default screen does not show 'Lunchroom: DENIED'"
    echo "$D" | grep -qF "Ways forward" && pass "$mode: the default screen shows 'Ways forward'" \
        || fail "$mode: the default screen does not show 'Ways forward'"
    # 3. the reason reaches the model
    for phrase in "What was DENIED" "Why:" "Ways forward"; do
        echo "$R" | grep -qF -- "$phrase" && pass "$mode: the model's result has '$phrase'" \
            || fail "$mode: the model's result lacks '$phrase'"
    done
    pkill -u dev -f sessionpty.py 2>/dev/null; pkill -u dev -x claude 2>/dev/null; sleep 2
done

finish
