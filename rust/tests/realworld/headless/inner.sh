#!/bin/bash
# Proof 13 (headless), inside the container: the real Claude Code (Anthropic's apt package), Lunchroom
# 1.0.2 installed by `lunchroom install claude-code`, the API answered by mockapi.py. Three spellings of
# a headless run are each run twice with a 120 s timeout:
#   p      claude -p "say hi"
#   json   claude -p "say hi" --output-format json
#   print  claude --print "say hi"
# once as a control with Lunchroom's hooks removed from settings.json (the mock's answer must come back
# quickly), once with the hooks as installed. For each: the answer, the exit status, the seconds it
# took, and for the hooked runs the debug log lines for the hooks (`claude --debug-file <path>`,
# "Write debug logs to a specific file path (implicitly enables debug mode)" in `claude --help`).

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "claude: $(/usr/bin/claude --version 2>&1 | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"
/usr/bin/claude --help 2>&1 | grep -A1 -e '--debug-file' | sed 's/^/MEASURED claude --help: /'

make_user
export MOCK_API=1
seed_claude
install_lunchroom
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }
python3 - "$DEVHOME/.claude/settings.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
for event, groups in s["hooks"].items():
    for g in groups:
        for h in g["hooks"]:
            print("MEASURED installed hook:", event, "|", h["command"].split("/")[-2:], "|", {k: v for k, v in h.items() if k not in ("type", "command")})
PY
cp "$DEVHOME/.claude/settings.json" "$RUN/settings.with-lunchroom.json"
python3 - "$DEVHOME/.claude/settings.json" "$RUN/settings.no-hooks.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
s["hooks"] = {}
json.dump(s, open(sys.argv[2], "w"))
PY
chmod 644 "$RUN/settings.no-hooks.json"

# run_one LABEL ARGUMENT...: claude with these arguments (and --debug-file) as dev, 120 s timeout;
# sets P_OUT, P_CODE, P_SECS.
run_one() {
    local label=$1 start end
    shift
    rm -f "$SESS/$label.debug" "$SESS/$label.out"
    start=$(date +%s%N)
    ( cd "$DEVHOME" && as_dev ANTHROPIC_BASE_URL="$ANTHROPIC_BASE_URL" ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
        timeout 120 /usr/bin/claude "$@" --debug-file "$SESS/$label.debug" > "$SESS/$label.out" 2> "$SESS/$label.err" < /dev/null )
    P_CODE=$?
    end=$(date +%s%N)
    P_SECS=$(( (end - start) / 1000000 ))
    P_OUT=$(head -c 300 "$SESS/$label.out" | tr '\n' ' ')
}

# The three spellings.
spell_args() {
    case "$1" in
        p)     echo "-p|say hi" ;;
        json)  echo "-p|say hi|--output-format|json" ;;
        print) echo "--print|say hi" ;;
    esac
}

for spelling in p json print; do
    IFS='|' read -r -a ARGS <<< "$(spell_args "$spelling")"
    heading "spelling '$spelling': claude ${ARGS[*]}"

    cat "$RUN/settings.no-hooks.json" > "$DEVHOME/.claude/settings.json"; chown dev "$DEVHOME/.claude/settings.json"
    run_one "control-$spelling" "${ARGS[@]}"
    measured "control (no hooks): exit $P_CODE after ${P_SECS} ms; stdout: '$P_OUT'; stderr: '$(head -c 200 "$SESS/control-$spelling.err" | tr '\n' ' ')'"
    { [ "$P_CODE" -eq 0 ] && echo "$P_OUT" | grep -q ok; } \
        && pass "$spelling control: without hooks it prints the mock's answer ('ok' in the output) and exits 0, in ${P_SECS} ms" \
        || fail "$spelling control: without hooks it did not print the answer (exit $P_CODE after ${P_SECS} ms)"

    cat "$RUN/settings.with-lunchroom.json" > "$DEVHOME/.claude/settings.json"; chown dev "$DEVHOME/.claude/settings.json"
    run_one "hooked-$spelling" "${ARGS[@]}"
    measured "Lunchroom's hooks: exit $P_CODE after ${P_SECS} ms; stdout: '$P_OUT'; stderr: '$(head -c 200 "$SESS/hooked-$spelling.err" | tr '\n' ' ')'"
    echo "---- debug log lines for the hooks of '$spelling':"
    grep -E "SessionStart:startup \[|Hook SessionStart|Stop:" "$SESS/hooked-$spelling.debug" 2>/dev/null | cut -c1-230 | head -8 | sed 's/^/    | /'
    if [ "$P_CODE" -eq 124 ]; then
        fail "$spelling with Lunchroom's hooks did NOT finish: killed by the 120 s timeout (exit 124) after ${P_SECS} ms; stdout '$P_OUT'"
    elif echo "$P_OUT" | grep -q ok && [ "$P_CODE" -eq 0 ] && [ "$P_SECS" -lt 30000 ]; then
        pass "$spelling with Lunchroom's hooks printed the mock's answer and exited 0 in ${P_SECS} ms"
    else
        fail "$spelling with Lunchroom's hooks: exit $P_CODE after ${P_SECS} ms, stdout '$P_OUT'"
    fi
done

finish
