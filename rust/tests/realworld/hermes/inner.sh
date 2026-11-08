#!/bin/bash
# Proof 9, inside the container: the real Hermes Agent (commit 9976f48), Lunchroom 1.1.0 installed for
# it with `lunchroom install hermes`, the plugin enabled the way the install tells the user, and the
# model's API answered by chatmock.py so that a one-shot turn becomes a real tool call whose real
# pre_tool_call hook runs the plugin, which runs the guard.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "python: $(python3 --version)"
echo "hermes: $(hermes --version 2>/dev/null | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
mkdir -p "$SESS"
chmod 777 "$SESS"
export MOCK_LOG="$SESS/mock.log"
SHELL_TOOL=terminal SHELL_ARG=command python3 "$PROOF/chatmock.py" &
sleep 1
install_lunchroom hermes
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install hermes finished" \
    || { fail "lunchroom install hermes did not finish"; sed 's/^/    /' "$RUN/install.log" | head; finish; }
sed 's/^/    install: /' < <(python3 "$PROOF/plaintext.py" "$RUN/install.log" | tail -14)

HM=(as_dev hermes)
"${HM[@]}" plugins enable lunchroom > "$RUN/enable.out" 2>&1
sed 's/^/    enable: /' "$RUN/enable.out" | head -5
"${HM[@]}" plugins list 2>&1 | grep -i lunchroom | sed 's/^/MEASURED plugins list: /'
"${HM[@]}" plugins list 2>&1 | grep -i lunchroom | grep -qi enabled \
    && pass "Hermes lists the Lunchroom plugin as enabled" || fail "Hermes does not list the Lunchroom plugin as enabled"

# turn TEXT: one one-shot turn through the mock API, with the terminal tool.
as_dev hermes config set model.provider custom > "$RUN/hconfig.out" 2>&1
as_dev hermes config set model.base_url http://127.0.0.1:8099/v1 >> "$RUN/hconfig.out" 2>&1
as_dev hermes config set model.default mock-1 >> "$RUN/hconfig.out" 2>&1
sed 's/^/    config: /' "$RUN/hconfig.out" | head -6
turn() {
    as_dev env OPENAI_API_KEY=sk-mock timeout 180 hermes chat --oneshot -q "$1" -t terminal,file --accept-hooks 2>&1 | tail -6
}

heading "1. a session joins"
turn "join hm" | sed 's/^/    turn: /'
n=0; until status | grep -q "^hm	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
status | sed 's/^/    /'
status | grep "^hm	" | grep -q AVAILABLE && pass "the Hermes session joined as hm and is AVAILABLE" \
    || fail "the Hermes session is not AVAILABLE"
doctor > "$RUN/doctor.out"
grep -i "hermes" "$RUN/doctor.out" | sed 's/^/    /'
grep -i "hermes" "$RUN/doctor.out" | grep -q "^ *FAIL" && fail "doctor has a FAIL line for Hermes" \
    || pass "doctor has no FAIL line for Hermes"

measured "install owner: $(stat -c '%U' /opt/hermes-agent) (root); Hermes runs as dev"
# The whole checkout as it stands, measured for the record. The plugin's trust check reads only the
# checkout's folder and its `.git` (root's, none group/other-writable, no link in `.git`).
measured "checkout entries: $(find /opt/hermes-agent | wc -l)"
measured "entries not owned by root: $(find /opt/hermes-agent ! -user root | wc -l) $(find /opt/hermes-agent ! -user root | head -3 | tr '\n' ' ')"
measured "entries writable by group or others (links aside): $(find /opt/hermes-agent ! -type l -perm /022 | wc -l) $(find /opt/hermes-agent ! -type l -perm /022 | head -3 | tr '\n' ' ')"
measured "links inside .git: $(find /opt/hermes-agent/.git -type l | wc -l)"

heading "2. tool calls through the plugin"
turn "DENYTEST d1" >/dev/null
D=$(await_result DENYTEST d1)
echo "MEASURED the terminal tool, 'codex queue other hi': the model was given back: $(echo "$D" | head -c 900)"
case "$D" in *DENIED*) pass "a raw 'codex queue' is blocked in the Hermes session (DENIED text reached the model)" ;;
    "") fail "no result for the raw codex queue" ;; *) fail "the raw codex queue was not denied: $D" ;; esac
turn "CONTROLTEST c1" >/dev/null
C=$(await_result CONTROLTEST c1)
measured "the terminal tool, 'ls /opt/proof': the model was given back: $(echo "$C" | head -c 200)"
case "$C" in *lib.sh*) pass "the control call ran (its output lists /opt/proof)" ;;
    *) fail "the control call did not run: '${C:-nothing}'" ;; esac
grep -h '"tools"' "$MOCK_LOG.requests" | head -1 | cut -c1-400 | sed 's/^/MEASURED the tools Hermes offered: /'

heading "3. the checkout owned by another user who is not root"
# Hermes keeps the version information it read once in the gateway process it starts for one-shot turns,
# so that process is stopped (SIGTERM) for the next turn to start a new one.
useradd -m other
chown -R other /opt/hermes-agent
measured "install owner now: $(stat -c '%U' /opt/hermes-agent)"
pkill -u dev -f "hermes" ; sleep 4
turn "join hm2" >/dev/null
n=0; until status | grep -q "^hm2	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
status | sed 's/^/    /'
status | grep "^hm2	" | grep -q UNGUARDED && pass "with the checkout owned by another non-root user, hm2 is UNGUARDED" \
    || fail "with the checkout owned by another non-root user, hm2 is not UNGUARDED"
status | grep -qF "give the checkout to the user who runs Hermes or to root" \
    && pass "status gives the reason: 'give the checkout to the user who runs Hermes or to root'" \
    || fail "status does not give the reason 'give the checkout to the user who runs Hermes or to root'"

finish
