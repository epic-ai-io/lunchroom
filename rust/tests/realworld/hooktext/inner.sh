#!/bin/bash
# Proof 6 (hooktext), Claude Code part, inside the container: what the user sees, on the real
# Claude Code terminal screen, of Lunchroom's multi-line hook texts.
#
# The session is the real /usr/bin/claude of Anthropic's apt package, on a pseudo-terminal 140
# columns by 50 rows, its API answered by mockapi.py so that its real hooks run on a real Bash tool
# call. The terminal's output is fed through a terminal emulator (screen.py, pyte) to give the
# screen as a person sees it, scrollback included.
#
# Multi-line hook texts measured:
#   1. the DENIED text of a PreToolUse hook (section "What was DENIED / Why / Ways forward");
#   2. the text Lunchroom shows when the user types `join` (UserPromptSubmit hook);
#   3. the SessionStart text.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "claude: $(/usr/bin/claude --version 2>&1 | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
export MOCK_API=1
seed_claude
install_lunchroom
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install claude-code finished" \
    || { fail "lunchroom install claude-code did not finish"; finish; }

# flat TEXT: one line, spaces collapsed, so a phrase wrapped over lines can be found.
flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/$1.log" 140 50; }

start_session s1 /usr/bin/claude --permission-mode default --allowedTools Bash
sleep 3
screen s1 > "$RUN/screen-join.txt"

heading "1. the SessionStart text and the answer to 'join s1'"
sed 's/^/    | /' "$RUN/screen-join.txt"
flat < "$RUN/screen-join.txt" | grep -qF "Lunchroom: include this session? Type join, or join <name>. Anything else keeps it out." \
    && pass "the user sees the SessionStart text: 'Lunchroom: include this session? Type join, or join <name>. Anything else keeps it out.'" \
    || fail "the SessionStart text is not on the user's screen"
status | grep -q "^s1	" && pass "the session joined as s1" || fail "the session did not join"

heading "2. the DENIED text of a PreToolUse hook"
D=$(probe s1 DENYTEST scr)
sleep 3
screen s1 > "$RUN/screen-deny.txt"
echo "What the model was given back (one line):"
echo "    $D" | fold -w 150 -s
echo "What the user's screen shows after the call:"
sed 's/^/    | /' "$RUN/screen-deny.txt" | tail -40
LINES_ON_SCREEN=$(grep -c . "$RUN/screen-deny.txt")
measured "the screen holds $LINES_ON_SCREEN non-blank lines in all"
SF=$(flat < "$RUN/screen-deny.txt")
for phrase in "Lunchroom: DENIED" "What was DENIED" "Why:" "Ways forward"; do
    if echo "$D" | flat | grep -qF -- "$phrase"; then
        if echo "$SF" | grep -qF -- "$phrase"; then
            pass "the hook text has '$phrase' and the user sees it on the screen"
        else
            fail "the hook text has '$phrase' but the user does not see it on the screen"
        fi
    else
        measured "the text the model got has no '$phrase'"
    fi
done
if echo "$SF" | grep -qiE "ctrl\+o to expand|… \+[0-9]+ lines|\.\.\. \+[0-9]+ lines"; then
    measured "the screen shows a fold marker: $(echo "$SF" | grep -oiE '(…|\.\.\.) \+[0-9]+ lines[^)]*\)?|ctrl\+o to expand' | head -2 | tr '\n' ' ')"
else
    measured "the screen shows no fold marker (no '+N lines', no 'ctrl+o to expand')"
fi
# The full last sentence of the hook text: is the text complete or cut?
LASTWORDS=$(echo "$D" | flat | awk '{n=NF; print $(n-3), $(n-2), $(n-1), $n}')
echo "$SF" | grep -qF -- "$LASTWORDS" \
    && pass "the last words of the hook text ('$LASTWORDS') are on the user's screen: nothing is cut off" \
    || fail "the last words of the hook text ('$LASTWORDS') are not on the user's screen: the text is cut off"

heading "3. the same call, with the tool output expanded (ctrl+o)"
# Claude Code's default view folds a tool call to 'Ran 1 shell command'; ctrl+o shows the
# transcript with the tool results. Measured, not assumed: the screen is read again after ctrl+o.
: > "$SESS/s1.mark"
SIZE0=$(stat -c %s "$SESS/s1.log")
echo "<CTRL-O>" > "$SESS/s1.keys"
sleep 3
tail -c +$((SIZE0 + 1)) "$SESS/s1.log" > "$SESS/s1-after-ctrlo.log"
python3 "$PROOF/screen.py" "$SESS/s1.log" 140 50 > "$RUN/screen-expanded.txt"
sed 's/^/    | /' "$RUN/screen-expanded.txt" | grep -v '^    | *$' | tail -40
SX=$(flat < "$RUN/screen-expanded.txt")
for phrase in "DENIED" "What was DENIED" "Why:" "Ways forward"; do
    echo "$SX" | grep -qF -- "$phrase" \
        && pass "after ctrl+o the user sees '$phrase'" \
        || fail "even after ctrl+o the user does not see '$phrase'"
done
echo "$SX" | grep -qF -- "$LASTWORDS" \
    && pass "after ctrl+o the last words ('$LASTWORDS') are shown: the whole text is available to the user" \
    || fail "after ctrl+o the last words ('$LASTWORDS') are not shown"
echo "<CTRL-O>" > "$SESS/s1.keys"
sleep 1

finish
