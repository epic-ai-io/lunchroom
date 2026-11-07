#!/bin/bash
# Proof 11, inside the container: with OpenClaw's own apply_patch containment left at its default
# (tools.exec.applyPatch.workspaceOnly NOT touched), does an apply_patch whose path is inside the
# workspace, but is a symlink to a file or folder of Lunchroom's store, get through?
#
# Two symlinks are made in the agent's workspace (~/.openclaw/workspace), as a person (or an agent
# with a shell) can make them:
#   link-file.json -> ~/.lunchroom/store/state.json     patch: Add File: link-file.json
#   link-dir       -> ~/.lunchroom/store                patch: Add File: link-dir/planted-<label>.txt
# The result is judged by the store file's hash before and after, and by the planted file's existence.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "openclaw: $(openclaw --version 2>/dev/null | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
mkdir -p "$SESS"
chmod 777 "$SESS"
export MOCK_LOG="$SESS/mock.log"
SHELL_TOOL=exec SHELL_ARG=command python3 "$PROOF/chatmock.py" &
sleep 1
install_lunchroom openclaw
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install openclaw finished" || { fail "install did not finish"; finish; }

OC=(as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token openclaw)
"${OC[@]}" config set models.providers.mock '{"baseUrl":"http://127.0.0.1:8099/v1","apiKey":"sk-mock","api":"openai-completions","models":[{"id":"mock-1","name":"Mock 1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":128000,"maxTokens":4096}]}' --strict-json >/dev/null 2>&1
"${OC[@]}" config set agents.defaults.model.primary mock/mock-1 >/dev/null 2>&1
"${OC[@]}" config set gateway.mode local >/dev/null 2>&1
measured "tools.exec.applyPatch.workspaceOnly (left at its default, not set by this proof): $("${OC[@]}" config get tools.exec.applyPatch.workspaceOnly 2>&1 | tail -1)"
echo y | "${OC[@]}" plugins install --link "$DEVHOME/.lunchroom/plugins/openclaw" --force --accept-capabilities > "$RUN/plugin-install.out" 2>&1
"${OC[@]}" plugins enable lunchroom > "$RUN/plugin-enable.out" 2>&1
"${OC[@]}" config set plugins.entries.lunchroom.hooks.allowConversationAccess true --strict-json >/dev/null 2>&1
grep -q "Enabled plugin \"lunchroom\"" "$RUN/plugin-enable.out" && pass "the Lunchroom plugin is enabled" || fail "the plugin is not enabled"
(cd "$DEVHOME" && "${OC[@]}" gateway run --port 18789 --bind loopback > "$SESS/gateway.log" 2>&1 &)
n=0
until (exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null || [ $n -ge 120 ]; do sleep 1; n=$((n + 1)); done
(exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null && pass "the gateway listens (after ${n} s)" || { fail "the gateway did not start"; finish; }
sleep 5
turn() { as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token timeout 120 openclaw agent --session-key "$1" --message "$2" 2>&1 | tail -3; }

turn agent:main:alpha "join alpha" >/dev/null
n=0; until status | grep -q "^alpha	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
status | grep "^alpha	" | grep -q AVAILABLE && pass "alpha joined and is AVAILABLE" || fail "alpha is not AVAILABLE"

WS=$DEVHOME/.openclaw/workspace
STORE=$DEVHOME/.lunchroom/store
measured "workspace: $WS ($( [ -d "$WS" ] && echo exists || echo missing))"
as_dev mkdir -p "$WS"
as_dev ln -s "$STORE/state.json" "$WS/link-file.json"
as_dev ln -s "$STORE" "$WS/link-dir"
ls -l "$WS" | grep link | sed 's/^/    /'
[ "$(readlink "$WS/link-file.json")" = "$STORE/state.json" ] && [ "$(readlink "$WS/link-dir")" = "$STORE" ] \
    && pass "the workspace holds link-file.json -> state.json and link-dir -> the store folder" \
    || fail "the symlinks were not made"

heading "1. control: an ordinary file in the workspace"
turn agent:main:alpha "PATCHTEST p0" >/dev/null
Q=$(await_result PATCHTEST p0)
measured "apply_patch of hello-p0.txt in the workspace: $(echo "$Q" | head -c 200)"
[ -f "$WS/hello-p0.txt" ] && pass "control: hello-p0.txt was created in the workspace (the tool path works with the default containment)" \
    || fail "control: hello-p0.txt was not created: ${Q:-nothing}"

heading "2. apply_patch through a symlink to the store's state file"
H0=$(sha256sum "$STORE/state.json" | cut -d' ' -f1)
turn agent:main:alpha "PATCHLINK l1" >/dev/null
L=$(await_result PATCHLINK l1)
H1=$(sha256sum "$STORE/state.json" 2>/dev/null | cut -d' ' -f1)
echo "MEASURED apply_patch 'Add File: link-file.json': the model was given back: $(echo "$L" | head -c 700)"
measured "state.json sha256 before: $H0"
measured "state.json sha256 after:  ${H1:-missing}"
measured "state.json now begins: $(head -c 80 "$STORE/state.json" 2>&1 | tr '\n' ' ')"
case "$L" in *DENIED*) measured "the refusal came from Lunchroom's guard" ;;
    *"escapes"*|*"sandbox"*|*"outside"*|*"symlink"*) measured "the refusal came from OpenClaw's own containment" ;; esac
# The store is live: Lunchroom's own hooks rewrite it on every turn, so its hash changes anyway
# (the first run of this proof compared hashes and flagged that wrongly). What the patch would
# write is the line `hello`, replacing the whole file; so the file must still be JSON, with no
# line `hello`.
if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$STORE/state.json" 2>/dev/null && ! grep -qx hello "$STORE/state.json"; then
    pass "the store file is still Lunchroom's JSON and does not hold the patch's line 'hello'"
else
    fail "the store file was overwritten by the apply_patch through the symlink"
fi

heading "3. apply_patch to a new file under a symlinked folder"
turn agent:main:alpha "PATCHLINKDIR l2" >/dev/null
M=$(await_result PATCHLINKDIR l2)
echo "MEASURED apply_patch 'Add File: link-dir/planted-l2.txt': the model was given back: $(echo "$M" | head -c 700)"
case "$M" in *DENIED*) measured "the refusal came from Lunchroom's guard" ;;
    *"escapes"*|*"sandbox"*|*"outside"*|*"symlink"*) measured "the refusal came from OpenClaw's own containment" ;; esac
[ -e "$STORE/planted-l2.txt" ] && fail "the file $STORE/planted-l2.txt EXISTS: an apply_patch through a workspace symlink wrote inside Lunchroom's folder" \
    || pass "no file was created inside Lunchroom's store folder"
status > "$RUN/status-after.out"
grep -q "^alpha	" "$RUN/status-after.out" && measured "status afterwards still lists alpha: $(grep '^alpha	' "$RUN/status-after.out" | cut -f3)" \
    || measured "status afterwards does not list alpha (the store was changed)"

finish
