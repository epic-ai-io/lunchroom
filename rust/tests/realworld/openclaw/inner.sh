#!/bin/bash
# Proof 8, inside the container: the real OpenClaw gateway 2026.10.7 (npm), Lunchroom 1.0.2 installed
# for it with `lunchroom install openclaw`, the plugin installed and enabled the way the install
# tells the user (`openclaw plugins install --link ... --force`, `plugins enable`, `config set ...
# allowConversationAccess true`), and the model's API answered by chatmock.py so that a typed turn
# becomes a real tool call whose real before_tool_call hook runs the plugin, which runs the guard.
#
# Measures: the plugin loads and registers its hooks; a session joins; a call the guard must DENY is
# blocked and what the model is given back; the control call runs; an apply_patch into Lunchroom's
# folder is blocked; the release OpenClaw reports through the plugin is read by status and doctor.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "node: $(node --version)"
echo "openclaw: $(openclaw --version 2>/dev/null | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
mkdir -p "$SESS"
chmod 777 "$SESS"
export MOCK_LOG="$SESS/mock.log"
SHELL_TOOL=exec SHELL_ARG=command python3 "$PROOF/chatmock.py" &
sleep 1
install_lunchroom openclaw
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install openclaw finished" \
    || { fail "lunchroom install openclaw did not finish"; finish; }

OC=(as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token openclaw)
"${OC[@]}" config set models.providers.mock '{"baseUrl":"http://127.0.0.1:8099/v1","apiKey":"sk-mock","api":"openai-completions","models":[{"id":"mock-1","name":"Mock 1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":128000,"maxTokens":4096}]}' --strict-json >/dev/null 2>&1
"${OC[@]}" config set agents.defaults.model.primary mock/mock-1 >/dev/null 2>&1
"${OC[@]}" config set gateway.mode local >/dev/null 2>&1
# OpenClaw's own apply_patch stays inside the agent's workspace unless this is false ("Path escapes
# sandbox root"). It is turned off here so that the only thing between the call and the file in
# Lunchroom's folder is Lunchroom's guard; the first run of this proof, with it on, showed OpenClaw's
# own error and so proved nothing about the guard.
"${OC[@]}" config set tools.exec.applyPatch.workspaceOnly false --strict-json >/dev/null 2>&1
measured "tools.exec.applyPatch.workspaceOnly = $("${OC[@]}" config get tools.exec.applyPatch.workspaceOnly 2>&1 | tail -1)"
echo y | "${OC[@]}" plugins install --link "$DEVHOME/.lunchroom/plugins/openclaw" --force --accept-capabilities > "$RUN/plugin-install.out" 2>&1
"${OC[@]}" plugins enable lunchroom > "$RUN/plugin-enable.out" 2>&1
"${OC[@]}" config set plugins.entries.lunchroom.hooks.allowConversationAccess true --strict-json >/dev/null 2>&1
grep -q "Enabled plugin \"lunchroom\"" "$RUN/plugin-enable.out" \
    && pass "OpenClaw installed (linked) and enabled the Lunchroom plugin" \
    || { fail "OpenClaw did not enable the plugin"; cat "$RUN/plugin-install.out" "$RUN/plugin-enable.out" | head -20; }

(cd "$DEVHOME" && "${OC[@]}" gateway run --port 18789 --bind loopback > "$SESS/gateway.log" 2>&1 &)
n=0
until (exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null || [ $n -ge 120 ]; do sleep 1; n=$((n + 1)); done
(exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null && pass "the gateway listens on 127.0.0.1:18789 (after ${n} s)" \
    || { fail "the gateway did not start"; tail -20 "$SESS/gateway.log"; finish; }
sleep 5

INSPECT=$("${OC[@]}" plugins inspect lunchroom --runtime --json 2>&1)
measured "plugins inspect lunchroom --runtime: $(echo "$INSPECT" | tr -d '\n ' | head -c 400)"
echo "$INSPECT" | grep -q before_tool_call && pass "the plugin registered before_tool_call in the running gateway" \
    || fail "before_tool_call is not among the plugin's typed hooks"
echo "$INSPECT" | grep -q before_agent_run && pass "the plugin registered before_agent_run (conversation access granted)" \
    || fail "before_agent_run is not registered"

# turn KEY TEXT: one turn of the session KEY, started by the user (openclaw agent).
turn() { as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token timeout 120 openclaw agent --session-key "$1" --message "$2" 2>&1 | tail -5; }

heading "1. a session joins"
turn agent:main:alpha "join alpha" | sed 's/^/    turn: /'
n=0; until status | grep -q "^alpha	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
status | sed 's/^/    /'
status | grep "^alpha	" | grep -q AVAILABLE && pass "the OpenClaw session joined as alpha and is AVAILABLE (guarded)" \
    || fail "the OpenClaw session is not AVAILABLE"
doctor > "$RUN/doctor.out"
grep -i "openclaw" "$RUN/doctor.out" | sed 's/^/    /'
grep -i "openclaw" "$RUN/doctor.out" | grep -q "^ *FAIL" \
    && fail "doctor has a FAIL line for OpenClaw" || pass "doctor has no FAIL line for OpenClaw"
grep -iq "OpenClaw.*2026.10.7\|release 2026.10.7" "$RUN/doctor.out" \
    && pass "doctor shows the release the plugin reported: 2026.10.7" \
    || fail "doctor does not show the OpenClaw release 2026.10.7"

heading "2. tool calls through the plugin"
turn agent:main:alpha "DENYTEST d1" >/dev/null
D=$(await_result DENYTEST d1)
echo "MEASURED the shell tool, 'codex queue other hi': the model was given back: $(echo "$D" | head -c 900)"
case "$D" in *DENIED*) pass "a raw 'codex queue' is blocked in the OpenClaw session (DENIED text reached the model)" ;;
    "") fail "no result for the raw codex queue" ;; *) fail "the raw codex queue was not denied: $D" ;; esac
turn agent:main:alpha "CONTROLTEST c1" >/dev/null
C=$(await_result CONTROLTEST c1)
measured "the shell tool, 'ls /opt/proof': the model was given back: $(echo "$C" | head -c 200)"
case "$C" in *lib.sh*) pass "the control call ran (its output lists /opt/proof)" ;;
    *) fail "the control call did not run: '${C:-nothing}'" ;; esac
turn agent:main:alpha "PATCHPROT p1" >/dev/null
P=$(await_result PATCHPROT p1)
echo "MEASURED apply_patch of ~/.lunchroom/planted-p1.txt: the model was given back: $(echo "$P" | head -c 700)"
grep apply_patch_schema "$MOCK_LOG.requests" | head -1 | cut -c1-400 | sed 's/^/MEASURED the harness own apply_patch schema: /'
case "$P" in *"Lunchroom: DENIED"*) pass "an apply_patch into Lunchroom's own folder is DENIED by Lunchroom ('Lunchroom: DENIED' reached the model)" ;;
    "") fail "no result for the apply_patch into Lunchroom's folder" ;; *) fail "the apply_patch into Lunchroom's folder was not denied: $P" ;; esac
[ -e "$DEVHOME/.lunchroom/planted-p1.txt" ] && fail "the file ~/.lunchroom/planted-p1.txt was created" \
    || pass "the file ~/.lunchroom/planted-p1.txt does not exist"
turn agent:main:alpha "PATCHTEST p2" >/dev/null
Q=$(await_result PATCHTEST p2)
measured "apply_patch of hello-p2.txt: the model was given back: $(echo "$Q" | head -c 300)"
case "$Q" in *DENIED*) fail "an ordinary apply_patch was DENIED: $Q" ;; "") fail "no result for the ordinary apply_patch" ;;
    *) pass "an ordinary apply_patch was not stopped by the guard" ;; esac

heading "3. what the plugin left in the gateway's log"
grep -i "lunchroom" "$SESS/gateway.log" | tail -8 | cut -c1-240 | sed 's/^/    /'

finish
