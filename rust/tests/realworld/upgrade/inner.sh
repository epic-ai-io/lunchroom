#!/bin/bash
# Proof 2, inside the container: Claude Code (Anthropic's apt package claude-code, installed at
# $OLD) runs as a real session; the package is then upgraded with Anthropic's documented command
# (`apt update && apt upgrade claude-code`) while the session runs. dpkg replaces /usr/bin/claude,
# so the running session's /proc/<pid>/exe reads "/usr/bin/claude (deleted)". Lunchroom 1.1.0 must
# then give the reason "program replaced while running", not "release cannot be read".

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

pkgver() { dpkg-query -W -f='${Version}' claude-code 2>/dev/null; }

heading "1. before the upgrade: claude-code $OLD installed, a real session running"
V0=$(pkgver)
[ "$V0" = "$OLD" ] && pass "dpkg: claude-code is at $OLD" || fail "dpkg: claude-code is at '$V0', not $OLD"
make_user
# The Anthropic API is answered by mockapi.py, so a typed prompt becomes a real Bash tool call in
# the real program and its real hooks judge it. Bash is allowed by the session's own flag, so a
# call the guard lets through runs without a permission prompt.
export MOCK_API=1
seed_claude
install_lunchroom
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install claude-code finished" \
    || { fail "lunchroom install claude-code did not finish"; finish; }
SESSION_FLAGS=(--permission-mode default --allowedTools Bash)
start_session before /usr/bin/claude "${SESSION_FLAGS[@]}"
PID1=$SESSION_PID
[ "$(exe_of "${PID1:-0}")" = "/usr/bin/claude" ] \
    && pass "session 'before' (pid $PID1) runs /usr/bin/claude; its exe is '$(exe_of "$PID1")'" \
    || fail "session 'before' exe is '$(exe_of "${PID1:-0}")', not /usr/bin/claude"
status | sed 's/^/    /'
status | grep "^before	" | grep -q AVAILABLE \
    && pass "before the upgrade the session is AVAILABLE (guarded)" \
    || fail "before the upgrade the session is not AVAILABLE"

# judge SESSION LABEL: a call the guard must DENY in every session (a raw `codex queue`), and an
# ordinary `ls /opt/proof` as the control that proves the tool path works. Both go through the real
# program's real PreToolUse hook.
judge() {
    local session=$1 label=$2 d c
    d=$(probe "$session" DENYTEST "$label")
    c=$(probe "$session" CONTROLTEST "$label")
    measured "$label: DENY call (codex queue other hi) -> Claude Code got: ${d:-nothing}"
    measured "$label: control call (ls /opt/proof) -> Claude Code got: ${c:-nothing}"
    case "$c" in
        *lib.sh*) pass "$label: the control call ran (its output lists /opt/proof): the hook path works" ;;
        *) fail "$label: the control call did not run: '${c:-nothing}'" ;;
    esac
    case "$d" in
        *DENIED*) pass "$label: the call that must be DENIED was DENIED by the session's hook" ;;
        "") fail "$label: no result for the call that must be DENIED" ;;
        *) fail "$label: the call that must be DENIED was NOT denied: $d" ;;
    esac
}
heading "1b. before the upgrade: the session's hooks judge a real tool call"
judge before pre

heading "2. the upgrade, with Anthropic's own command, while the session runs"
echo "COMMAND apt-get update && apt-get upgrade -y claude-code   (the page: 'sudo apt update && sudo apt upgrade claude-code')"
apt-get update >"$RUN/apt-update.out" 2>&1
apt-get upgrade -y claude-code >"$RUN/apt-upgrade.out" 2>&1
UPSTATUS=$?
grep -E "^(Setting up|Unpacking|Preparing to unpack)" "$RUN/apt-upgrade.out" | sed 's/^/    /'
[ $UPSTATUS -eq 0 ] && pass "apt-get upgrade claude-code exited 0" || fail "apt-get upgrade claude-code exited $UPSTATUS"
V1=$(pkgver)
measured "dpkg: claude-code $V0 -> $V1"
[ -n "$V1" ] && [ "$V1" != "$V0" ] && pass "the package is now $V1, not $V0 (the upgrade installed a different release)" \
    || fail "the package did not change ($V0 -> $V1): nothing was replaced"
kill -0 "$PID1" 2>/dev/null && pass "the session (pid $PID1) is still running" || fail "the session ended during the upgrade"
EXE1=$(exe_of "$PID1")
measured "/proc/$PID1/exe -> $EXE1"
case "$EXE1" in
    "/usr/bin/claude (deleted)") pass "/proc/$PID1/exe ends in ' (deleted)': the running program was replaced" ;;
    *) fail "/proc/$PID1/exe is '$EXE1', not '/usr/bin/claude (deleted)'" ;;
esac
[ "$(/usr/bin/claude --version | head -1)" != "" ] && measured "the program now on disk says: $(/usr/bin/claude --version | head -1)"

heading "3. what Lunchroom 1.1.0 says now"
status > "$RUN/status.out"
sed 's/^/    /' "$RUN/status.out"
grep "^before	" "$RUN/status.out" | grep -q UNGUARDED \
    && pass "the session is UNGUARDED after the program was replaced" \
    || fail "the session is not UNGUARDED after the program was replaced"
grep -qF "was replaced while this session was running" "$RUN/status.out" \
    && pass "status: the reason is 'program replaced while running' (\"was replaced while this session was running\")" \
    || fail "status does not give the reason 'was replaced while this session was running'"
grep -qF "release cannot be read" "$RUN/status.out" \
    && fail "status says 'release cannot be read'" \
    || pass "status does not say 'release cannot be read'"
grep -qF "start the Claude Code session anew" "$RUN/status.out" \
    && pass "status tells the user what to do: start the Claude Code session anew" \
    || fail "status does not tell the user to start the session anew"
doctor > "$RUN/doctor.out"
sed 's/^/    /' "$RUN/doctor.out"
grep -qF "nothing it does is checked" "$RUN/status.out" "$RUN/doctor.out" \
    && fail "the UNGUARDED reason text (status or doctor) contains 'nothing it does is checked'" \
    || pass "the UNGUARDED reason text (status and doctor) does not contain 'nothing it does is checked'"
grep -qF "was replaced while this session was running" "$RUN/doctor.out" \
    && pass "doctor: the same reason" || fail "doctor does not give the reason 'was replaced while this session was running'"
grep -qF "release cannot be read" "$RUN/doctor.out" \
    && fail "doctor says 'release cannot be read'" || pass "doctor does not say 'release cannot be read'"

heading "3b. the UNGUARDED session's own hooks, still running, judge a real tool call"
# The session is marked UNGUARDED, but its hooks are the same Lunchroom hooks. Whether they still
# DENY is measured here, not assumed.
judge before post
kill "$PID1" 2>/dev/null
n=0; while kill -0 "$PID1" 2>/dev/null && [ $n -lt 20 ]; do sleep 0.5; n=$((n + 1)); done
start_session after /usr/bin/claude "${SESSION_FLAGS[@]}"
PID2=$SESSION_PID
[ "$(exe_of "${PID2:-0}")" = "/usr/bin/claude" ] && pass "session 'after' (pid $PID2) runs /usr/bin/claude (not deleted)" \
    || fail "session 'after' exe is '$(exe_of "${PID2:-0}")'"
status > "$RUN/status2.out"
sed 's/^/    /' "$RUN/status2.out"
grep "^after	" "$RUN/status2.out" | grep -q AVAILABLE \
    && pass "the new session is AVAILABLE (guarded)" || fail "the new session is not AVAILABLE"
doctor > "$RUN/doctor2.out"
grep -qF "PASS Claude Code: release $V1, read from /usr/bin/claude." "$RUN/doctor2.out" \
    && pass "doctor: release $V1, read from /usr/bin/claude" \
    || { fail "doctor does not show 'release $V1, read from /usr/bin/claude.'"; sed 's/^/    /' "$RUN/doctor2.out"; }

heading "5. a reinstall of the same release under a running session"
echo "COMMAND apt-get install --reinstall -y claude-code"
apt-get install --reinstall -y claude-code >"$RUN/apt-reinstall.out" 2>&1
[ $? -eq 0 ] && pass "apt-get install --reinstall claude-code exited 0" || fail "apt-get install --reinstall claude-code failed"
V2=$(pkgver)
[ "$V2" = "$V1" ] && pass "the release is the same, $V2" || fail "the release changed from $V1 to $V2"
EXE2=$(exe_of "$PID2")
measured "/proc/$PID2/exe -> $EXE2"
[ "$EXE2" = "/usr/bin/claude (deleted)" ] && pass "/proc/$PID2/exe ends in ' (deleted)' after the reinstall" \
    || fail "/proc/$PID2/exe is '$EXE2' after the reinstall"
status > "$RUN/status3.out"
sed 's/^/    /' "$RUN/status3.out"
grep -qF "was replaced while this session was running" "$RUN/status3.out" \
    && pass "after the reinstall: the reason is 'program replaced while running'" \
    || fail "after the reinstall: status does not give 'was replaced while this session was running'"
grep -qF "release cannot be read" "$RUN/status3.out" \
    && fail "after the reinstall: status says 'release cannot be read'" \
    || pass "after the reinstall: status does not say 'release cannot be read'"

finish
