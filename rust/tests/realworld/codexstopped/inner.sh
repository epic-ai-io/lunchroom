#!/bin/bash
# Proof 14 (codexstopped), inside the container: a session joined in the real Codex TUI showed STOPPED
# in `lunchroom status` right after "join cx" (seen in codexodd1). This joins a Codex TUI session and
# runs `lunchroom status` at 0 s, 5 s and 30 s, recording each time: the state, the Codex processes
# (pid, parent, name, /proc/<pid>/exe, command line, start time), the process the store's record of the
# session names, and what `lunchroom doctor` says of the agent. If the session is running and status
# says STOPPED, that is the defect.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
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
[ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && pass "lunchroom install codex finished" || { fail "install did not finish"; finish; }

flat() { tr '\n' ' ' | tr -s ' '; }
screen() { python3 "$PROOF/screen.py" "$SESS/cx.log" 140 50; }
key() { timeout 5 sh -c "printf '%s\n' \"\$1\" > $SESS/cx.keys" _ "$1"; }

mkfifo "$SESS/cx.keys"; chmod 666 "$SESS/cx.keys"
( cd "$DEVHOME" && as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/cx.log" --keys "$SESS/cx.keys" \
    --ready "$SESS/cx.ready" --timeout 900 -- codex >/dev/null 2>&1 ) &
n=0; quiet=0
while [ $n -lt 120 ]; do
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
[ $quiet -ge 8 ] && pass "Codex is up at its prompt" || { fail "Codex did not reach its prompt"; finish; }

# snapshot LABEL: everything the question needs, at this moment.
snapshot() {
    local label=$1 pid
    echo "---- $label ($(date +%H:%M:%S))"
    echo "  lunchroom status:"
    status | sed 's/^/    /'
    echo "  processes of user dev (pid, ppid, comm, start ticks, exe, cmdline):"
    for pid in $(pgrep -u dev); do
        comm=$(cat /proc/$pid/comm 2>/dev/null)
        case "$comm" in codex*|node|sessionpty*|python3) ;; *) continue ;; esac
        printf '    %s ppid=%s comm=%s start=%s exe=%s\n      cmdline=%s\n' "$pid" "$(awk '{print $4}' /proc/$pid/stat 2>/dev/null)" "$comm" \
            "$(awk '{print $22}' /proc/$pid/stat 2>/dev/null)" "$(exe_of "$pid")" \
            "$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null | cut -c1-160)"
    done
    echo "  the store's record of the session (sessions / processes):"
    python3 - "$DEVHOME/.lunchroom/store/state.json" <<'PY' | sed 's/^/    /'
import json, sys
s = json.load(open(sys.argv[1]))
for k, v in s.get("sessions", {}).items():
    print("session", k, json.dumps(v)[:200])
for k, v in s.get("processes", {}).items():
    print("process", k, "->", json.dumps(v)[:160])
PY
    echo "  codex is alive per /proc: $(pgrep -u dev -x codex | tr '\n' ' ' || true)"
}

key "join cx"
# t = 0 s: as soon as Lunchroom lists the session at all (the join prompt's hook has run).
n=0; until status | grep -q "^cx	" || [ $n -ge 60 ]; do sleep 0.25; n=$((n + 1)); done
snapshot "t=0 s (the session has just been listed)"
S0=$(status | grep "^cx	" | cut -f3)
sleep 5
snapshot "t=5 s"
S5=$(status | grep "^cx	" | cut -f3)
sleep 25
snapshot "t=30 s"
S30=$(status | grep "^cx	" | cut -f3)
measured "state at 0 s: $S0; at 5 s: $S5; at 30 s: $S30"

echo "---- lunchroom doctor (the agent's line)"
doctor > "$RUN/doctor.out"
grep -i "cx\|codex" "$RUN/doctor.out" | sed 's/^/    /'

heading "2. the same join, with each Lunchroom hook run through a wrapper shell (what codexodd1 did)"
# codexodd1 ran every hook through tee-wrap.sh, a shell script, so the hook's parent was `sh`, not
# `codex`. harness_process (crates/lunchroom/src/claude.rs) takes a Codex hook's parent "counted only
# when it is a `codex` process". This repeats the join with that wrapper, to see whether it, and not
# Codex or Lunchroom, produced the STOPPED seen there.
key "<CTRL-C>"; sleep 1; key "<CTRL-C>"; sleep 2
pkill -u dev -f sessionpty.py 2>/dev/null; pkill -u dev -x codex 2>/dev/null; sleep 3
cp "$DEVHOME/.codex/hooks.json" "$RUN/hooks.base.json"
cp /opt/proof/tee-wrap.sh /usr/local/bin/tee-wrap.sh; chmod 755 /usr/local/bin/tee-wrap.sh
python3 - "$DEVHOME/.codex/config.toml" <<'PY'
import re, sys
p = sys.argv[1]
t = open(p).read()
t = re.sub(r"\n?\[hooks\.state\.[^\]]*\]\n(?:(?!\[).*\n?)*", "\n", t)
open(p, "w").write(t)
PY
python3 /opt/proof/hooksvar.py "$RUN/hooks.base.json" "$DEVHOME/.codex/hooks.json" c
chown -R dev "$DEVHOME/.codex"
echo "$SESS/hooks" > "$SESS/hookdir"; mkdir -p "$SESS/hooks"; chmod 777 "$SESS/hooks"
rm -f "$SESS/cx.keys" "$SESS/cx.log" "$SESS/cx.ready"
mkfifo "$SESS/cx.keys"; chmod 666 "$SESS/cx.keys"
( cd "$DEVHOME" && as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/cx.log" --keys "$SESS/cx.keys" \
    --ready "$SESS/cx.ready" --timeout 400 -- codex >/dev/null 2>&1 ) &
n=0; quiet=0
while [ $n -lt 120 ]; do
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
key "join cx2"
n=0; until status | grep -q "^cx2	" || [ $n -ge 60 ]; do sleep 0.25; n=$((n + 1)); done
snapshot "wrapped hooks, t=0 s"
W0=$(status | grep "^cx2	" | cut -f3)
WHY0=$(status | grep -A1 "^cx2	" | sed -n 2p)
sleep 30
W30=$(status | grep "^cx2	" | cut -f3)
WHY30=$(status | grep -A1 "^cx2	" | sed -n 2p)
measured "with the wrapper: state at 0 s: $W0; at 30 s: $W30"
echo "  hook processes seen by the wrapper (parent chain is the point): the wrapped hook's parent is sh, run by codex"

if pgrep -u dev -x codex >/dev/null; then pass "the Codex process is running (pid $(pgrep -u dev -x codex | tr '\n' ' '))"; else fail "no Codex process is running"; fi
for pair in "0 s:$S0" "5 s:$S5" "30 s:$S30"; do
    t=${pair%%:*}; st=${pair#*:}
    case "$st" in
        AVAILABLE|HELD) pass "at $t status says $st for the running Codex session" ;;
        *) fail "at $t status says '$st' for a Codex session whose process is running" ;;
    esac
done

# Spec R3.1: a Codex session is guarded only when every Lunchroom hook is in its hooks file; a wrapped
# command is not Lunchroom's hook, so UNGUARDED with the hooks-file reason is the right answer. What
# must not happen is STOPPED for a running session.
for pair in "0 s:$W0:$WHY0" "30 s:$W30:$WHY30"; do
    t=${pair%%:*}; rest=${pair#*:}; st=${rest%%:*}; why=${rest#*:}
    case "$st" in
        STOPPED) fail "with each hook run through a wrapper shell, status says STOPPED at $t for a running Codex session" ;;
        UNGUARDED)
            echo "$why" | grep -q "hooks.json" \
                && pass "with each hook run through a wrapper shell, status says UNGUARDED at $t, with a reason naming hooks.json" \
                || fail "UNGUARDED at $t, but the reason does not name hooks.json: '$why'" ;;
        *) fail "with each hook run through a wrapper shell, status says '$st' at $t (expected UNGUARDED with the hooks.json reason)" ;;
    esac
done

finish
