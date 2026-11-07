#!/bin/bash
# Proof 12 (codexodd), inside the container: why does Codex 0.166.0 tell the model "unsupported call:
# exec_command" for a command that ran, when Lunchroom's hooks are installed?
#
# The same control call (the mock's exec_command with {"cmd": "touch /tmp/control-ran-<label> && ls
# /opt/proof"}) is run in five configurations, each in the Codex TUI and in `codex exec`:
#   a no hooks   b only the proof's capture hook   c all Lunchroom's hooks   d Lunchroom's PreToolUse
#   alone   e Lunchroom's PostToolUse alone
# Every hook command is run through tee-wrap.sh, which keeps its stdin, stdout, stderr and exit code.
# For each run: what Codex gave the model back, whether the file exists, and each hook's exit code
# and output. Nothing is judged here; the table at the end is the result.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "codex: $(codex --version 2>/dev/null | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
mkdir -p "$SESS" "$SESS/hooks"
chmod 777 "$SESS" "$SESS/hooks"
mkdir -p "$DEVHOME/.codex"
BASECONF='model = "gpt-5.5"
model_provider = "mock"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.mock]
name = "mock"
base_url = "http://127.0.0.1:8099/v1"
wire_api = "responses"
requires_openai_auth = false
'
printf '%s' "$BASECONF" > "$DEVHOME/.codex/config.toml"
chown -R dev "$DEVHOME/.codex"
MOCK_LOG="$SESS/mock.log" python3 "$PROOF/codexmock.py" &
sleep 1
install_lunchroom codex
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install codex finished" || { fail "install did not finish"; finish; }
cp "$DEVHOME/.codex/hooks.json" "$RUN/hooks.base.json"
cp "$DEVHOME/.codex/config.toml" "$RUN/config.after-install.toml"
measured "config.toml after the install (features): $(grep -A2 '^\[features\]' "$DEVHOME/.codex/config.toml" | tr '\n' ' ')"
cp /opt/proof/tee-wrap.sh /usr/local/bin/tee-wrap.sh
cp /opt/proof/tee-hook.sh /usr/local/bin/tee-hook.sh
chmod 755 /usr/local/bin/tee-wrap.sh /usr/local/bin/tee-hook.sh

flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/cx.log" 140 50; }
key() { timeout 5 sh -c "printf '%s\n' \"\$1\" > $SESS/cx.keys" _ "$1"; }
RESULTS="$RUN/results.txt"
: > "$RESULTS"

# reset CONFIG: a fresh Codex configuration for CONFIG: the install's config.toml without any trust
# record, and the hooks.json of CONFIG.
reset_config() {
    cp "$RUN/config.after-install.toml" "$DEVHOME/.codex/config.toml"
    python3 - "$DEVHOME/.codex/config.toml" <<'PY'
import re, sys
p = sys.argv[1]
t = open(p).read()
# drop every [hooks.state...] table (Codex's trust records) so each configuration is reviewed afresh
t = re.sub(r"\n?\[hooks\.state\.[^\]]*\]\n(?:(?!\[).*\n?)*", "\n", t)
open(p, "w").write(t)
PY
    python3 /opt/proof/hooksvar.py "$RUN/hooks.base.json" "$DEVHOME/.codex/hooks.json" "$1"
    chown -R dev "$DEVHOME/.codex"
}

# hooks_report LABEL: the hooks that ran since the folder was emptied.
hooks_report() {
    local f any=no
    for f in $(ls "$SESS/hooks" 2>/dev/null | grep '\.code$' | sort); do
        any=yes
        b="$SESS/hooks/${f%.code}"
        printf '    hook %s: exit %s; stdout: %s; stderr: %s\n' "$(echo "${f%.code}" | sed 's/^[0-9]*-//')" "$(cat "$b.code")" \
            "$(head -c 160 "$b.out" | tr '\n' ' ')" "$(head -c 160 "$b.err" | tr '\n' ' ')"
    done
    [ $any = no ] && echo "    no hook ran"
}
hooks_summary() {
    local f out=""
    for f in $(ls "$SESS/hooks" 2>/dev/null | grep '\.code$' | sort); do
        out="$out $(echo "${f%.code}" | sed 's/^[0-9]*-//')=$(cat "$SESS/hooks/$f")"
    done
    echo "${out:- none}"
}

# record CONFIG MODE LABEL: the result of the control call of this run.
record() {
    local config=$1 mode=$2 label=$3 got file
    got=$(await_result CONTROLTEST "$label")
    [ -e "/tmp/control-ran-$label" ] && file=yes || file=NO
    echo "---- config $config, $mode: Codex gave the model back: ${got:-nothing}"
    echo "     file /tmp/control-ran-$label exists: $file"
    hooks_report
    printf '%s|%s|%s|%s|%s\n' "$config" "$mode" "$(echo "$got" | head -c 70)" "$file" "$(hooks_summary)" >> "$RESULTS"
}

run_tui() {
    local config=$1 label=$2 n=0 quiet=0 s
    rm -f "$SESS/cx.keys" "$SESS/cx.log" "$SESS/cx.ready"
    mkfifo "$SESS/cx.keys"; chmod 666 "$SESS/cx.keys"
    ( cd "$DEVHOME" && as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/cx.log" --keys "$SESS/cx.keys" \
        --ready "$SESS/cx.ready" --timeout 400 -- codex >/dev/null 2>&1 ) &
    while [ $n -lt 100 ]; do
        s=$(screen 2>/dev/null | flat)
        case "$s" in
            *"Hooks need review"*) key "<DOWN>"; sleep 1; key "<ENTER>"; sleep 3; quiet=0 ;;
            *"hooks need review before they can run"*) key "<RAW:t>"; sleep 1; key "<DOWN>"; sleep 1; key "<RAW:t>"; sleep 1; key "<ESC>"; sleep 3; quiet=0 ;;
            *"Try new model"*|*"Meet GPT-6 Sol"*) key "<DOWN>"; sleep 1; key "<ENTER>"; sleep 3; quiet=0 ;;
            *"Do you trust"*) key "<ENTER>"; sleep 3; quiet=0 ;;
            *"Ask Codex to do anything"*) quiet=$((quiet + 1)); [ $quiet -ge 8 ] && break ;;
        esac
        sleep 1; n=$((n + 1))
    done
    if [ $quiet -lt 8 ]; then fail "config $config: the Codex TUI did not reach its prompt"; screen | grep -v '^\s*$' | tail -12 | sed 's/^/    | /'; return 1; fi
    if [ "$config" = c ]; then
        key "join cx"
        n=0; until status | grep -q "^cx	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
        status | grep "^cx	" | cut -c1-80 | sed 's/^/    joined: /'
    fi
    rm -f "$SESS/hooks"/*
    echo "$SESS/hooks" > "$SESS/hookdir"
    key "CONTROLTEST $label"
    return 0
}

stop_tui() {
    key "<CTRL-C>"; sleep 1; key "<CTRL-C>"; sleep 2
    pkill -u dev -f sessionpty.py 2>/dev/null; pkill -u dev -x codex 2>/dev/null
    sleep 2
}

for config in a b c d e; do
    heading "configuration $config"
    reset_config "$config"
    measured "hooks.json for $config: $(python3 -c 'import json; print(sorted(json.load(open("'"$DEVHOME"'/.codex/hooks.json"))["hooks"]))')"
    # TUI
    label="${config}_tui"
    if run_tui "$config" "$label"; then
        sleep 2
        record "$config" TUI "$label"
        stop_tui
    fi
    # codex exec, in the same configuration (hooks already trusted in the TUI above)
    label="${config}_exec"
    rm -f "$SESS/hooks"/*
    echo "$SESS/hooks" > "$SESS/hookdir"
    ( cd "$DEVHOME" && as_dev timeout 90 codex exec --skip-git-repo-check "CONTROLTEST $label" > "$SESS/exec-$config.out" 2>&1 )
    record "$config" exec "$label"
    sed 's/^/    exec output: /' "$SESS/exec-$config.out" | grep -v '^\s*$' | tail -6 | cut -c1-200
done

heading "table"
printf '%-6s %-5s %-5s %-70s %s\n' config mode file "what Codex gave the model" "hooks that ran (exit)"
while IFS='|' read -r c m g f h; do printf '%-6s %-5s %-5s %-70s %s\n' "$c" "$m" "$f" "$g" "$h"; done < <(awk -F'|' '{print $1"|"$2"|"$3"|"$4"|"$5}' "$RESULTS" | awk -F'|' '{print $1"|"$2"|"$3"|"$4"|"$5}')
echo "RESULTS_RAW_BEGIN"; cat "$RESULTS"; echo "RESULTS_RAW_END"

pass "all five configurations were run in the TUI and in codex exec; the table above is the result (nothing is judged here)"
finish
