#!/bin/bash
# Proof 7, inside the container: the real Codex CLI 0.166.0 on a pseudo-terminal, Lunchroom 1.1.0
# installed for it with `lunchroom install codex`, Codex's own hook review answered in Codex's own
# screen ("Trust all and continue"), and its API answered by codexmock.py so that a prompt becomes a
# real tool call whose real hooks run.
#
# What it measures:
#   1. the PreToolUse input Codex gives a hook for a real apply_patch call (a hook of the proof's own,
#      tee-hook.sh, keeps a copy), compared with docs/host-facts/codex-apply-patch.md section 1;
#   2. what Lunchroom's guard does with apply_patch calls (an ordinary file; a file inside Lunchroom's
#      own folder) and with a raw `codex queue`, and the control call that must run;
#   3. what the user sees on Codex's screen of Lunchroom's multi-line DENIED text.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "codex: $(codex --version 2>/dev/null | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
mkdir -p "$SESS"
chmod 777 "$SESS"
mkdir -p "$DEVHOME/.codex"
cat > "$DEVHOME/.codex/config.toml" <<'T'
model = "gpt-5.5"
model_provider = "mock"
approval_policy = "never"
# Codex's own sandbox cannot start inside this rootless container ("No usable execution environment
# is available"), so the container is the sandbox and Codex runs without its own.
sandbox_mode = "danger-full-access"

[model_providers.mock]
name = "mock"
base_url = "http://127.0.0.1:8099/v1"
wire_api = "responses"
requires_openai_auth = false
T
chown -R dev "$DEVHOME/.codex"
MOCK_LOG="$SESS/mock.log" python3 "$PROOF/codexmock.py" &
sleep 1
install_lunchroom codex
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install codex finished" \
    || { fail "lunchroom install codex did not finish"; finish; }

# The proof's own hook, beside Lunchroom's PreToolUse hook, before Codex first reads the file.
cp "$PROOF/tee-hook.sh" /usr/local/bin/tee-hook.sh
chmod 755 /usr/local/bin/tee-hook.sh
python3 - "$DEVHOME/.codex/hooks.json" <<'PY'
import json, sys
p = sys.argv[1]
h = json.load(open(p))
h["hooks"]["PreToolUse"].append({"hooks": [{"type": "command", "command": "/usr/local/bin/tee-hook.sh", "timeout": 5}]})
json.dump(h, open(p, "w"), indent=2)
PY
chown dev "$DEVHOME/.codex/hooks.json"

flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/cx.log" 140 50; }
key() { timeout 5 sh -c "printf '%s\n' \"\$1\" > $SESS/cx.keys" _ "$1"; }

heading "1. Codex starts; its own screens are answered"
mkfifo "$SESS/cx.keys"
chmod 666 "$SESS/cx.keys"
(
    cd "$DEVHOME" || exit 1
    as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/cx.log" --keys "$SESS/cx.keys" \
        --ready "$SESS/cx.ready" --timeout 1500 -- codex >/dev/null 2>&1
) &
n=0
QUIET=0
READY=no
while [ $n -lt 120 ]; do
    S=$(screen | flat)
    case "$S" in
        *"Hooks need review"*)
            echo "SCREEN: Codex asks about the hooks: Down, Enter = '2. Trust all and continue'"
            key "<DOWN>"; sleep 1; key "<ENTER>"
            sleep 3 ;;
        *"Meet GPT-6 Sol"*|*"Try new model"*)
            echo "SCREEN: Codex offers a new model: Down, Enter = '2. Use existing model'"
            key "<DOWN>"; sleep 1; key "<ENTER>"
            sleep 3 ;;
        *"Do you trust"*)
            echo "SCREEN: Codex asks whether to trust the folder: Enter = the first choice"
            key "<ENTER>"
            sleep 3 ;;
        *"hooks need review before they can run"*)
            echo "SCREEN: the hook list: t = trust each hook, Esc back"
            key "<RAW:t>"; sleep 1; key "<DOWN>"; sleep 1; key "<RAW:t>"; sleep 1; key "<ESC>"
            sleep 3 ;;
        *"Ask Codex to do anything"*)
            # The main screen is drawn before Codex's dialogs: it counts as ready only when it has
            # stood alone for 8 seconds.
            QUIET=$((QUIET + 1))
            [ "$QUIET" -ge 8 ] && { READY=yes; break; } ;;
    esac
    case "$S" in *"Hooks need review"*|*"Try new model"*|*"Do you trust"*|*"need review"*) QUIET=0 ;; esac
    sleep 1
    n=$((n + 1))
done
CXPID=$(sed -n 1p "$SESS/cx.ready" 2>/dev/null)
if [ "$READY" = yes ]; then
    pass "Codex is up at its prompt ('Ask Codex to do anything')"
    screen | flat | grep -q "include this session" \
        && measured "Lunchroom's SessionStart text is on Codex's screen at start" \
        || measured "Lunchroom's SessionStart text is not on Codex's screen at start (Codex runs SessionStart hooks with the first turn)"
else
    fail "Codex did not reach its prompt; its screen follows"
    screen | grep -v '^\s*$' | sed 's/^/    | /' | tail -30
    finish
fi
measured "codex pid $CXPID; /proc/$CXPID/exe -> $(exe_of "$CXPID")"
grep -q "trusted_hash" "$DEVHOME/.codex/config.toml" \
    && pass "Codex wrote its own trust record for the hooks (trusted_hash in config.toml); Lunchroom wrote none" \
    || fail "config.toml holds no trusted_hash after 'Trust all'"
key "join cx"
n=0; until status | grep -q "^cx	" || [ $n -ge 60 ]; do sleep 0.5; n=$((n + 1)); done
status | sed 's/^/    /'
status | grep "^cx	" | grep -q AVAILABLE && pass "the Codex session joined and is AVAILABLE (guarded)" \
    || fail "the Codex session is not AVAILABLE"

heading "2. a real apply_patch call: what Codex gives the hook"
R=$(probe cx PATCHTEST p1)
measured "apply_patch of hello-p1.txt: Codex got back: ${R:-nothing}"
PT=$(python3 - <<'PY'
import json
for line in open("/run/sess/pretool-inputs.jsonl"):
    line = line.strip()
    if not line:
        continue
    d = json.loads(line)
    if d.get("tool_name") == "apply_patch":
        print(json.dumps(d, sort_keys=True))
        break
PY
)
if [ -n "$PT" ]; then
    pass "Codex ran a PreToolUse hook for apply_patch (the proof's hook kept the input)"
    echo "MEASURED the hook input, as received: $(echo "$PT" | head -c 900)"
    echo "$PT" | python3 -c '
import json, sys
d = json.load(sys.stdin)
print("fields:", sorted(d))
print("tool_input fields:", sorted(d.get("tool_input", {})))
' | sed 's/^/MEASURED /'
    echo "$PT" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if d["tool_name"]=="apply_patch" else 1)' \
        && pass "host facts section 1: tool_name is apply_patch" || fail "tool_name is not apply_patch"
    echo "$PT" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if sorted(d["tool_input"])==["command"] else 1)' \
        && pass "host facts section 1: tool_input is an object with one field, command" \
        || fail "tool_input does not have exactly one field, command"
    echo "$PT" | python3 -c '
import json, sys
d = json.load(sys.stdin)
c = d["tool_input"]["command"]
sys.exit(0 if c == "*** Begin Patch\n*** Add File: hello-p1.txt\n+hello\n*** End Patch\n" else 1)' \
        && pass "host facts section 1: command is the whole patch text, as the model wrote it, paths inside it" \
        || fail "command is not the patch text the model wrote"
else
    fail "no PreToolUse hook input for apply_patch was kept: Codex ran no hook for the call (or the call never came)"
fi
case "$R" in
    *DENIED*) fail "an ordinary apply_patch (hello-p1.txt) was DENIED: $R" ;;
    "") fail "no result came back for the ordinary apply_patch" ;;
    *) pass "Lunchroom let an ordinary apply_patch through (Codex's own result: $(echo "$R" | head -c 120))" ;;
esac
[ -f "$DEVHOME/hello-p1.txt" ] && pass "the file hello-p1.txt exists: the patch was applied" \
    || measured "hello-p1.txt does not exist in $DEVHOME (Codex result above)"

heading "3. apply_patch into Lunchroom's own folder, and a raw codex queue"
R2=$(probe cx PATCHPROT p2)
echo "MEASURED apply_patch of ~/.lunchroom/planted-p2.txt: Codex got back: $(echo "$R2" | head -c 700)"
case "$R2" in
    *DENIED*) pass "an apply_patch that writes inside Lunchroom's own folder is DENIED" ;;
    "") fail "no result for the apply_patch into Lunchroom's folder" ;;
    *) fail "an apply_patch that writes inside Lunchroom's own folder was NOT denied: $R2" ;;
esac
[ -e "$DEVHOME/.lunchroom/planted-p2.txt" ] && fail "the file ~/.lunchroom/planted-p2.txt was created" \
    || pass "the file ~/.lunchroom/planted-p2.txt does not exist (and the folder does: $( [ -d "$DEVHOME/.lunchroom" ] && echo yes || echo no))"

C=$(probe cx CONTROLTEST c1)
measured "control exec_command (ls /opt/proof): Codex got back: $(echo "$C" | head -c 200)"
CH=$(python3 - <<'PY'
import json
for line in open("/run/sess/pretool-inputs.jsonl"):
    line = line.strip()
    if line and "control-ran-c1" in line:
        d = json.loads(line)
        print(json.dumps({"tool_name": d.get("tool_name"), "tool_input": d.get("tool_input")}, sort_keys=True))
        break
PY
)
measured "the hook input Codex gave for the control call: ${CH:-none kept}"
# The call form is the one Codex 0.166.0 runs for a shell command: a function_call named exec_command
# with arguments {"cmd": ...} (measured in a plain `codex exec`: the command's file was created, form A
# of exp/cx6; the forms `shell`, `shell_command` and local_shell_call were not run). The control
# command creates a file, so the control is proved by the file, not by Codex's answer.
if [ -e "/tmp/control-ran-c1" ]; then
    pass "the control ran: its command created /tmp/control-ran-c1 (the file exists)"
else
    fail "the control did NOT run: /tmp/control-ran-c1 does not exist (Codex's answer: ${C:-nothing})"
fi
case "$C" in
    *DENIED*|*"blocked by PreToolUse"*) fail "the control call was refused by the guard: $C" ;;
    "") fail "no result for the control call" ;;
    *) pass "the guard did not refuse the control call (Codex's answer: $(echo "$C" | head -c 100))" ;;
esac
[ -n "$CH" ] && pass "Codex's hook ran for the control call and the proof's hook kept its input" \
    || fail "the control call's hook input was not kept: no hook ran for it"
D=$(probe cx DENYTEST d1)
echo "MEASURED exec_command 'codex queue other hi': Codex got back: $(echo "$D" | head -c 900)"
case "$D" in *DENIED*) pass "a raw 'codex queue' is DENIED in the Codex session" ;;
    "") fail "no result for the raw codex queue" ;; *) fail "a raw 'codex queue' was not denied: $D" ;; esac

heading "4. what the user sees on Codex's screen"
sleep 3
screen > "$RUN/screen-codex.txt"
sed 's/^/    | /' "$RUN/screen-codex.txt" | grep -v '^    | *$' | tail -60
SF=$(flat < "$RUN/screen-codex.txt")
LASTWORDS=$(echo "$D" | flat | awk 'NF>=4 {n=NF; print $(n-3), $(n-2), $(n-1), $n}')
[ -n "$LASTWORDS" ] || { fail "there is no DENIED text to look for on the screen"; LASTWORDS="(none)"; }
for phrase in "DENIED" "What was DENIED" "Why:" "Ways forward"; do
    echo "$SF" | grep -qF -- "$phrase" \
        && pass "the user sees '$phrase' on Codex's screen" \
        || fail "the user does not see '$phrase' on Codex's screen"
done
echo "$SF" | grep -qF -- "$LASTWORDS" \
    && pass "the last words of the DENIED text ('$LASTWORDS') are on Codex's screen" \
    || fail "the last words of the DENIED text ('$LASTWORDS') are not on Codex's screen: it is cut off or folded"
LINES=$(grep -c . "$RUN/screen-codex.txt")
measured "Codex's screen holds $LINES non-blank lines (scrollback included)"

finish
