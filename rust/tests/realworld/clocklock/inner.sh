#!/bin/bash
# Proof 5, inside the container: stamps from a clock that stepped back, and a store lock that
# another Lunchroom process never lets go.
#
# Clock (spec section 11 D1.4 item 4, docs/spec/lunchroom-spec.md:494-499): "a stamp more than 5
# seconds ahead of the checking program's clock is a stamp from a clock that stepped back, and that
# attempt has run out"; an offer unconfirmed for 60 seconds, or a hand-over unconfirmed for 10
# minutes, is a failed attempt. A rootless container cannot set the wall clock, so the stamps are
# planted in the store (msg.py) and the real program judges them on the real clock.
#
# Lock (store.rs LOCK_WAIT 10 s; spec section 13 items 5 and 8): a store another process holds is
# reported busy after 10 seconds; the guard never locks the user out; a decision it cannot record
# is not dropped.

set -u
. /opt/proof/lib.sh

LR=$DEVHOME/.lunchroom/bin/lunchroom
STATE=$DEVHOME/.lunchroom/store/state.json
FLOCK=$DEVHOME/.lunchroom/store/state.flock

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"
# CAP_SYS_TIME is capability 25. The clock is not namespaced, so even a container that has the bit
# must not be used to set it: the proof never tries.
CAPEFF=$(grep '^CapEff' /proc/self/status | cut -f2)
measured "this container's effective capabilities: 0x$CAPEFF; CAP_SYS_TIME (bit 25) is $( [ $(( 0x$CAPEFF >> 25 & 1 )) -eq 1 ] && echo present || echo absent ); the proof does not set the clock either way"

make_user
install_lunchroom
[ -x "$LR" ] && pass "lunchroom install claude-code finished" || { fail "lunchroom install did not finish"; finish; }
mkdir -p "$DEVHOME/proof-plugin"
echo "const PROGRAM = \"$LR\";" > "$DEVHOME/proof-plugin/index.mjs"
chown -R dev "$DEVHOME/proof-plugin"
PLUGIN_ENV="PLUGIN_DIR=$DEVHOME/proof-plugin"

# guard_call EVENT SESSION TEXT [flag]: a hook input from an OpenClaw-shaped plugin, run through the
# guard as dev; prints the guard's output and sets GUARD_CODE and GUARD_SECS.
guard_call() {
    local event=$1 session=$2 text=$3 flag=${4:-} start end mode
    mode=$event
    [ "$event" = readtool ] && mode=pretool
    python3 /opt/proof/hook.py "$event" "$session" "$text" $flag > "$RUN/in-$session.json"
    chmod 644 "$RUN/in-$session.json"
    start=$(date +%s%N)
    GUARD_OUT=$(as_dev env "$PLUGIN_ENV" sh -c "$LR guard $mode < $RUN/in-$session.json" 2>&1)
    GUARD_CODE=$?
    end=$(date +%s%N)
    GUARD_SECS=$(( (end - start) / 1000000 ))
}

heading "1. a message that waits, to plant stamps in"
mkdir -p "$SESS"
chmod 777 "$SESS"
pty register "${DEV[@]}" "$LR" register alpha codex:A1
as_dev env "$PLUGIN_ENV" sh -c "python3 /opt/proof/hook.py prompt S1 hello --typed | $LR guard prompt >/dev/null 2>&1; LUNCHROOM_SESSION=openclaw:S1 OPENCLAW_SHELL=exec $LR send alpha 'a message that waits' > $SESS/send.out 2>&1; exit \$?"
sed 's/^/    /' "$SESS/send.out"
MSG=$(python3 -c 'import json; print(next(iter(json.load(open("'"$STATE"'"))["messages"]), ""))')
[ -n "$MSG" ] && pass "a message exists in the store: $MSG" || { fail "no message was queued"; finish; }
as_dev python3 /opt/proof/msg.py "$STATE" "$MSG" | sed 's/^/    /'

heading "2. stamps from a clock that stepped back (D1.4 item 4)"
# case REL HANDED EXPECT WHY: plant presented_at = now+REL, run one check (`lunchroom status`),
# and read the message's failed_attempts.
case_run() {
    local rel=$1 handed=$2 expect=$3 why=$4 got
    as_dev python3 /opt/proof/msg.py "$STATE" "$MSG" plant "$rel" "$handed" >/dev/null
    as_dev "$LR" status >/dev/null 2>&1
    got=$(as_dev python3 /opt/proof/msg.py "$STATE" "$MSG" | grep -o '"failed_attempts": [0-9]*' | cut -d' ' -f2)
    measured "planted presented_at=now($rel s) handed_over=$handed -> after one check: failed_attempts=$got ($(as_dev python3 /opt/proof/msg.py "$STATE" "$MSG"))"
    if [ "$got" = "$expect" ]; then
        pass "$why: failed_attempts = $expect"
    else
        fail "$why: failed_attempts is '$got', expected $expect"
    fi
}
case_run -30   0 0 "an offer 30 s old is within its 60 s (control)"
case_run -61   0 1 "an offer 61 s old has run out (control)"
case_run -580  1 0 "a hand-over 580 s old is within its 600 s (control)"
case_run -620  1 1 "a hand-over 620 s old has run out (control)"
case_run +4    0 0 "an offer stamped 4 s ahead of the clock is not a stepped-back stamp (slack 5 s)"
case_run +10   0 1 "an offer stamped 10 s ahead is a stamp from a clock that stepped back: it has run out"
case_run +3600 0 1 "an offer stamped 1 hour ahead (the clock stepped back an hour): it has run out"
case_run +3600 1 1 "a hand-over stamped 1 hour ahead: it has run out"

heading "3. the store lock held by another process (LOCK_WAIT 10 s)"
as_dev env "$PLUGIN_ENV" sh -c "python3 /opt/proof/hold.py $FLOCK 75 $SESS/held >/dev/null 2>&1 &"
n=0; until [ -e "$SESS/held" ] || [ $n -ge 20 ]; do sleep 0.25; n=$((n + 1)); done
[ -e "$SESS/held" ] && pass "a process of dev holds $FLOCK (exclusive flock) for 75 s" || { fail "the lock holder did not start"; finish; }

start=$(date +%s%N)
as_dev "$LR" status > "$RUN/busy.out" 2>&1
code=$?
secs=$(( ($(date +%s%N) - start) / 1000000 ))
measured "lunchroom status with the store held: exit $code after ${secs} ms: $(head -c 300 "$RUN/busy.out" | tr '\n' ' ')"
[ "$code" -ne 0 ] && pass "status with the store held exits non-zero ($code), not success" || fail "status with the store held exited 0"
[ "$secs" -lt 15000 ] && pass "status with the store held finished in ${secs} ms (under 15 s)" \
    || fail "status waited ${secs} ms; expected one LOCK_WAIT of 10 s (store.rs:31, 'no command, the user's own included, waits on a process that never lets go'); several sequential waits of 10 s add up"
grep -qi "held" "$RUN/busy.out" && pass "status says the store is held (another Lunchroom process has held the store)" \
    || fail "status does not say the store is held"

heading "4. the guard, with the store held: it never locks the user out (section 13 item 5)"
guard_call pretool G1 "ls /home/dev"
measured "ordinary read 'ls /home/dev': exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
# Spec section 13 item 6: "a guard past its time sends even a read to the user". Waiting 10 s for
# the lock is being past its time, so the call goes to the user through the approval channel
# (permissionDecision "ask"), it is not run unasked and not DENIED.
{ [ "$GUARD_CODE" -eq 0 ] && echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "a guard past its time sends even a read to the user (permissionDecision ask, exit 0)" \
    || fail "a read with the store held was not sent to the user (exit $GUARD_CODE)"
[ "$GUARD_SECS" -le 12000 ] && pass "...and the guard answered within its 10 s (${GUARD_SECS} ms)" || fail "the guard took ${GUARD_SECS} ms"

guard_call pretool G2 "codex queue other hi"
measured "'codex queue other hi': exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 0 ] && echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "a fence is sent to the user, not run, when the guard is past its time (item 6: the second judging is not made past the time)" \
    || fail "a fence with the store held was neither sent to the user nor DENIED as the spec says (exit $GUARD_CODE)"

guard_call pretool G3 "git push origin main" --no-channel
measured "'git push origin main' with no approval channel: exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 2 ] && echo "$GUARD_OUT" | grep -q DENIED; } \
    && pass "a call the guard cannot decide is DENIED where there is no approval channel (A1.2)" \
    || fail "an undecided call with no approval channel was not DENIED (exit $GUARD_CODE)"

guard_call prompt G4 "a prompt the user typed" --typed
measured "the user's typed prompt: exit $GUARD_CODE after ${GUARD_SECS} ms"
[ "$GUARD_CODE" -eq 0 ] && pass "the user's prompt reaches the agent with the store held (exit 0)" \
    || fail "the user's prompt was blocked with the store held (exit $GUARD_CODE)"

heading "5. after the lock is let go"
pkill -u dev -f "hold.py" 2>/dev/null
n=0; while pgrep -u dev -f "hold.py" >/dev/null && [ $n -lt 20 ]; do sleep 0.25; n=$((n + 1)); done
pgrep -u dev -f "hold.py" >/dev/null && fail "the lock holder is still running" || pass "the lock holder has ended"
start=$(date +%s%N)
as_dev "$LR" status > "$RUN/free.out" 2>&1
code=$?
secs=$(( ($(date +%s%N) - start) / 1000000 ))
[ "$code" -eq 0 ] && [ "$secs" -lt 3000 ] && pass "status works again at once (exit 0, ${secs} ms)" || fail "status after the release: exit $code, ${secs} ms"
guard_call pretool G5 "ls /home/dev"
[ "$GUARD_CODE" -eq 0 ] && pass "the next guard call decides normally" || fail "the next guard call exited $GUARD_CODE"
doctor > "$RUN/doctor.out"
grep -i "could not decide\|undecided\|did not finish" "$RUN/doctor.out" | sed 's/^/    /' | head -8
grep -qi "could not decide\|undecided\|did not finish\|took longer" "$RUN/doctor.out" \
    && pass "doctor shows the calls the guard could not decide while the store was held (no record dropped)" \
    || { fail "doctor shows no record of the calls the guard could not decide"; sed 's/^/    /' "$RUN/doctor.out" | head -40; }

heading "6. 20 writers at once (no lost update)"
for i in $(seq 1 20); do
    python3 /opt/proof/hook.py prompt "W$i" "hello $i" --typed > "$RUN/w$i.json"
    chmod 644 "$RUN/w$i.json"
done
for i in $(seq 1 20); do
    ( as_dev env "$PLUGIN_ENV" sh -c "$LR guard prompt < $RUN/w$i.json >/dev/null 2>&1"; echo $? > "$RUN/w$i.code" ) &
done
wait
BAD=0; for i in $(seq 1 20); do [ "$(cat "$RUN/w$i.code" 2>/dev/null)" = 0 ] || BAD=$((BAD + 1)); done
[ "$BAD" -eq 0 ] && pass "all 20 concurrent guard calls exited 0" || fail "$BAD of 20 concurrent guard calls failed"
SEEN=$(python3 -c 'import json; s=json.load(open("'"$STATE"'")); print(sum(1 for i in range(1,21) if any(k.endswith(":W%d" % i) for k in s["seen"])))')
[ "$SEEN" = 20 ] && pass "the store records all 20 sessions (no update lost)" || fail "the store records $SEEN of 20 sessions"
python3 -c 'import json; json.load(open("'"$STATE"'"))' && pass "state.json is valid JSON after the contention" || fail "state.json is not valid JSON"

heading "7. a store the guard cannot use (not a lock: unreadable state), section 13 items 5 and 6"
# Item 6: "with a store the guard cannot use, a read inside the shipped slip runs, a fence is DENIED and a
# reply is DENIED by `lunchroom send`". The state file is replaced by text that is not JSON (the
# program keeps a copy beside it, state.json.prev; both are replaced), then put back.
cp "$STATE" "$RUN/state.good"
cp "$STATE.prev" "$RUN/state.prev.good" 2>/dev/null
# Control, with the good store: the same read from a HELD session (the rules that need no record
# are those of a HELD agent) must run.
guard_call prompt C1 "text from another agent" --untyped
# Spec P1.1 (section 5 item 1): a slip holds calls that read and one command, the reply; any other call
# that changes a file or runs a command is never inside a slip. So the read here is OpenClaw's own
# `read` tool (a call that reads); an exec `cat` is a command, which is not inside the slip.
guard_call readtool C1 "/etc/hostname"
measured "control, good store, HELD session, read tool on /etc/hostname: exit $GUARD_CODE: $(echo "$GUARD_OUT" | head -c 200 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 0 ] && ! echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "control: with a good store a HELD session's read-tool call on /etc/hostname runs (inside the slip)" \
    || fail "control: with a good store a HELD session's read-tool call did not run (exit $GUARD_CODE)"
guard_call pretool C1 "cat /etc/hostname"
measured "control, good store, HELD session, exec 'cat /etc/hostname': exit $GUARD_CODE: $(echo "$GUARD_OUT" | head -c 200 | tr '\n' ' ')"
HOLDMODE=$(status | sed -n 's/^hold mode: \([A-Za-z]*\).*/\1/p')
measured "hold mode in effect: $HOLDMODE"
# P1.6 (docs/spec/lunchroom-spec.md:294): "Balanced: a HELD agent's calls run, except its PROTECTED calls
# (item 4), which go to the user." An exec `cat /etc/hostname` is not PROTECTED, so with a good store it runs.
{ [ "$HOLDMODE" = Balanced ] && [ "$GUARD_CODE" -eq 0 ] && ! echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "P1.6 Balanced (the hold mode in effect): a HELD agent's exec 'cat /etc/hostname' is not PROTECTED, so it runs" \
    || fail "P1.6 Balanced: the HELD agent's exec 'cat /etc/hostname' did not run (mode '$HOLDMODE', exit $GUARD_CODE)"
as_dev sh -c "echo 'this is not json' > $STATE; echo 'this is not json either' > $STATE.prev"
guard_call pretool H0 "ls /home/dev"
measured "unusable store, 'ls /home/dev' (a folder that holds Lunchroom's own): exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
guard_call readtool H1 "/etc/hostname"
measured "unusable store, read tool on /etc/hostname: exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 0 ] && ! echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "a read-tool call inside the slip runs with a store the guard cannot use" \
    || fail "a read-tool call inside the slip did not run with an unusable store (exit $GUARD_CODE)"
guard_call pretool H1x "cat /etc/hostname"
measured "unusable store, exec 'cat /etc/hostname': exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 0 ] && echo "$GUARD_OUT" | grep -q '"permissionDecision":"ask"'; } \
    && pass "H1.5 item 5 (section 13): judged by the rules that need no record, 'a call inside the slip as shipped runs'; an exec command is outside the slip (P1.1), so it goes to the user, unlike the good-store case under P1.6 Balanced" \
    || fail "an exec command with an unusable store was neither run-unasked-excluded nor asked as the spec says (exit $GUARD_CODE)"
guard_call pretool H2 "codex queue other hi"
measured "unusable store, 'codex queue other hi': exit $GUARD_CODE after ${GUARD_SECS} ms: $(echo "$GUARD_OUT" | head -c 250 | tr '\n' ' ')"
{ [ "$GUARD_CODE" -eq 2 ] && echo "$GUARD_OUT" | grep -q DENIED; } \
    && pass "a fence is DENIED with a store the guard cannot use" \
    || fail "a fence was not DENIED with an unusable store (exit $GUARD_CODE)"
guard_call prompt H3 "a prompt the user typed" --typed
[ "$GUARD_CODE" -eq 0 ] && pass "the user's prompt reaches the agent with an unusable store" \
    || fail "the user's prompt was blocked with an unusable store (exit $GUARD_CODE)"
cat "$RUN/state.good" > "$STATE"
[ -f "$RUN/state.prev.good" ] && cat "$RUN/state.prev.good" > "$STATE.prev"
as_dev "$LR" status > "$RUN/restored.out" 2>&1 && pass "with the state file put back, status works again" \
    || { fail "status fails after the state file was put back; its output follows"; sed 's/^/    /' "$RUN/restored.out" | head -12
         ls -l "$DEVHOME/.lunchroom/store/" | sed 's/^/    /'; }

finish
