#!/bin/bash
# The proof of the Lunchroom console (1.1), run as root in the container of `Containerfile`
# (Ubuntu 24.04, real users, a real sudo, real pseudo-terminals), on an account whose sudo asks for
# a password. wide.sh proves the account with password-free sudo (spec section 17 item 8).
#
# What it does, in order (docs/spec/lunchroom-spec.md, section 17 C2):
#   1. makes the user `dev` and installs Lunchroom for `dev` with `lunchroom install claude-code`,
#      which sets the store wall up and the passphrase, through the program's own prompts on a
#      pseudo-terminal (item 2); the secrets are typed from files, never given as an argument or in
#      the environment;
#   2. runs `lunchroom install console` on the password account, which goes ahead without asking
#      (item 8), and checks every file it makes, the Ed25519 key pair and the delivery folders
#      (item 9);
#   3. starts the console as the user `lunchroom` on a 140 x 50 pseudo-terminal, as `dev` does,
#      through the sudoers rule (it asks for `dev`'s sudo password, then the Lunchroom
#      passphrase), and reads its screen: header, the six alert cells dark, the key line (item 10);
#   4. as `dev`, without the guard, tries each attack and records the kernel's answer (items 2, 9);
#   5. as `dev`, through the guard, feeds each attack to `lunchroom guard pretool` from a HELD
#      session, and expects every one DENIED (item 5, 7, 9);
#   6. drives one event of each kind and checks that the console's screen shows it within 2 s,
#      with each alert cell turning amber or red (item 10);
#   7. opens the detail panel of each kind of item by key and by click, and does each action of the
#      table of item 10, the passphrase-guarded ones with and without the passphrase (item 3);
#   8. approves a controller request end to end and checks the delivery file's Ed25519 signature
#      with python's `cryptography` and with a pure verifier (item 9);
#   9. leaves the console untouched for 15 minutes, with an event arriving meanwhile, and checks
#      that it locks and that the alert strip stays live (items 3 and 10);
#  10. prints the console's last screen, and searches the whole container for the passphrase and
#      the sudo password.
#
# It prints PASS or FAIL for each check, MEASURED for a fact it only records, and exits non-zero on
# any FAIL. SKIP_LOCK_WAIT=1 skips the 15 minutes of step 9 (a dry run; the lock is then unproven).

set -u
. /opt/proof/common.sh

# ---------------------------------------------------------------------------------------------
heading "facts of this system"
echo "kernel.yama.ptrace_scope=$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo absent)"
echo "dev.tty.legacy_tiocsti=$(cat /proc/sys/dev/tty/legacy_tiocsti 2>/dev/null || echo absent)"
echo "sudoers use_pty: $(grep -rh '^Defaults.*use_pty' /etc/sudoers /etc/sudoers.d 2>/dev/null | head -1)"
echo "sudo: $(sudo -V 2>/dev/null | head -1)"
echo "kernel: $(uname -r)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"
python3 -c 'import cryptography; print("python cryptography", cryptography.__version__)' 2>&1 | sed 's/^/MEASURED /' | head -1
python3 -c 'import pyte; print("python pyte", pyte.__version__)' 2>&1 | sed 's/^/MEASURED /' | head -1

# ---------------------------------------------------------------------------------------------
heading "1. the user dev, and Lunchroom installed for dev (item 2)"
make_dev sudo
# An earlier user-mode store, as a 1.0.x install leaves it: one guard call writes it.
as_dev 'python3 /opt/proof/hook.py prompt EARLY "hello" --typed | ~/lunchroom-dl guard prompt >/dev/null 2>&1'
[ -f /home/dev/.lunchroom/store/state.json ] \
    && measured "an earlier user-mode store exists at /home/dev/.lunchroom/store (the install must move it)" \
    || measured "no earlier user-mode store was made by the guard call; the move is not exercised"
HAD_OLD=$([ -f /home/dev/.lunchroom/store/state.json ] && echo yes || echo no)

pty install1 runuser -u dev -- env $DEVENV /home/dev/lunchroom-dl install claude-code
if [ -g "$PROGRAM" ] && grep -q "$PROGRAM" /home/dev/.claude/settings.json 2>/dev/null; then
    pass "install claude-code set the wall up and wrote the hooks to run $PROGRAM (item 2)"
else
    fail "install claude-code did not finish (see $RUN/install1.log)"
    python3 "$PROOF/plaintext.py" "$RUN/install1.log" | tail -25 | sed 's/^/    /'
fi
grep -q "$PROGRAM" /home/dev/.claude/settings.json && ! grep -q '.lunchroom/bin/lunchroom' /home/dev/.claude/settings.json \
    && pass "the Claude Code hooks run $PROGRAM and no other" \
    || fail "the Claude Code hooks do not all run $PROGRAM"
[ -f /var/lib/lunchroom/$DEVUID/store/state.json ] \
    && pass "the store is at /var/lib/lunchroom/$DEVUID" || fail "no store in /var/lib/lunchroom/$DEVUID"
if [ "$HAD_OLD" = yes ]; then
    ls -d /home/dev/.lunchroom/store.moved-* >/dev/null 2>&1 && [ ! -e /home/dev/.lunchroom/store ] \
        && pass "the old store was moved aside, not deleted" || fail "the old store was not moved aside"
    grep -q 'EARLY' "$(store_state)" \
        && pass "the old store's session EARLY is in the new store (the store was moved with its content)" \
        || fail "the old store's content is not in the new store"
fi
python3 "$PROOF/plaintext.py" "$RUN/install1.log" | grep -q "Start every session of your agents anew" \
    && pass "the install says which sessions to start anew" || fail "the install does not say to start sessions anew"

heading "2. lunchroom install console, on an account whose sudo asks for a password (items 1, 8, 9)"
SUM_BEFORE=""
pty install2 runuser -u dev -- env $DEVENV "$PROGRAM" install console
grep -q "The console is set up" "$RUN/install2.log" \
    && pass "install console says the console is set up" \
    || { fail "install console did not finish (see $RUN/install2.log)"; python3 "$PROOF/plaintext.py" "$RUN/install2.log" | tail -30; }
python3 "$PROOF/plaintext.py" "$RUN/install2.log" | grep -qi "does not hold on this account" \
    && fail "install console asked about a wide sudo rule on an account whose sudo asks for a password" \
    || pass "install console went ahead without asking: this account's sudo asks for a password (item 8)"
check_console_files "after install console"
PUB_BEFORE=$(sha256sum /etc/lunchroom/$DEVUID.console.pub | cut -d' ' -f1)
NEWEST=$(ls -t --time=ctime /etc/lunchroom/$DEVUID.console.pub /etc/lunchroom/$DEVUID.console.key /etc/sudoers.d/lunchroom \
    /var/lib/lunchroom-approvals/$DEVUID /var/lib/lunchroom/$DEVUID 2>/dev/null | head -1)
[ "$NEWEST" = "/etc/lunchroom/$DEVUID.console.pub" ] \
    && pass "the public key was written last, once every other step had passed (item 9)" \
    || measured "the public key is not the most recently changed of the console's files (newest: $NEWEST); same-second writes cannot be ordered by ctime"
pty install3 runuser -u dev -- env $DEVENV "$PROGRAM" install console
[ "$(sha256sum /etc/lunchroom/$DEVUID.console.pub | cut -d' ' -f1)" = "$PUB_BEFORE" ] \
    && pass "a second install console kept the existing key pair (item 9)" \
    || fail "a second install console changed the public key"

pty doctor runuser -u dev -- env $DEVENV "$PROGRAM" doctor
python3 "$PROOF/plaintext.py" "$RUN/doctor.log" > "$RUN/doctor.txt"
grep -q "PASS.*console" "$RUN/doctor.txt" && pass "doctor reports the console set up (C1.4)" \
    || { fail "doctor does not report the console set up"; tail -30 "$RUN/doctor.txt"; }
grep -q "^FAIL" "$RUN/doctor.txt" && { measured "doctor has FAIL lines before the console runs:"; grep '^FAIL' "$RUN/doctor.txt" | sed 's/^/    /'; DOCTOR_CLEAN=no; } || DOCTOR_CLEAN=yes
# Right after a clean install nothing in sudo's rules has changed: doctor must not say they differ
# (cold first-time install, 2026-11-15: this was measured above and let through). The other FAIL
# lines here are this container's (dev's PATH as the proof runs it has neither ~/.local/bin nor claude).
grep -q "differ from the ones Lunchroom kept" "$RUN/doctor.txt" \
    && fail "doctor says sudo's rules changed right after a clean install (F2-AC38)" \
    || pass "doctor sees sudo's rules as kept right after a clean install (F2-AC38)"

# ---------------------------------------------------------------------------------------------
heading "3. the console, started as dev starts it (items 3 and 10)"
as_dev 'mkdir -p ~/proof-plugin'
start_console
if screen_has "CALLS WAIT"; then
    pass "the console asked for the passphrase and drew its screen"
else
    fail "the console did not draw its screen"
    python3 "$PROOF/plaintext.py" "$CONSOLE_LOG" | tail -c 1500
fi
echo "console pid $CPID, its terminal $CPTY, the outer terminal $OUTER"
[ "$(ps -o user= -p "$CPID" | tr -d ' ')" = "lunchroom" ] \
    && pass "the console runs as the user lunchroom" || fail "the console does not run as lunchroom"
[ "$(cut -d' ' -f1 /var/lib/lunchroom/$DEVUID/console.alive 2>/dev/null)" = "$CPID" ] \
    && pass "console.alive names the console's pid" || fail "console.alive does not name pid $CPID"
M1=$(stat -c %Y /var/lib/lunchroom/$DEVUID/console.alive); sleep 3.2; M2=$(stat -c %Y /var/lib/lunchroom/$DEVUID/console.alive)
[ "$M1" != "$M2" ] && pass "console.alive is rewritten every 2 s" || fail "console.alive was not rewritten in 3 s"
echo "---- the first screen ----"
screen_text
echo "--------------------------"
for word in "LUNCHROOM CONSOLE" "dev" "UTC" "HOLD MODE" "screen locks in"; do
    screen_has "$word" && pass "the header shows '$word' (item 10)" || fail "the header does not show '$word'"
done
screen_text | grep -qE 'screen locks in 1[45]:' \
    && pass "the header counts down to the lock from 15 minutes" || fail "the header does not show the minutes to the lock (about 15:00)"
for cell in "CALLS WAIT 0" "PROBING 0" "FAILED MSG 0" "UNGUARDED 0" "GUARD SLOW 0"; do
    screen_has "$cell" && pass "the alert strip shows '$cell'" || fail "the alert strip does not show '$cell'"
done
screen_has "CHECKS" && pass "the alert strip shows CHECKS" || fail "the alert strip does not show CHECKS"
for cell in "CALLS WAIT" "PROBING" "FAILED MSG" "UNGUARDED" "GUARD SLOW"; do
    [ "$(cell_color "$cell")" = dark ] && pass "a cell with nothing to report is dark: $cell" || fail "$cell is $(cell_color "$cell"), not dark, with nothing to report"
done
if [ "$DOCTOR_CLEAN" = yes ]; then
    screen_has "CHECKS OK" && [ "$(cell_color 'CHECKS OK')" = green ] \
        && pass "CHECKS is green while every doctor check passes" || fail "CHECKS is not green 'CHECKS OK' though doctor has no FAIL"
fi
for word in approve refuse release resend discard hold unregister mode yolo controller lock quit; do
    screen_has "$word" && pass "the key line shows '$word'" || fail "the key line does not show '$word'"
done

# ---------------------------------------------------------------------------------------------
heading "4. as dev, without the guard: what the kernel answers (items 2, 5, 9)"
# Every attack below needs its target to exist: a refusal of a file or process that is not there
# proves nothing. Without a running console, its terminal, the store folder and the setgid
# program, the attacks are not run and the proof fails.
S=/var/lib/lunchroom/$DEVUID
if [ -z "$CPID" ] || [ -z "$CPTY" ] || [ ! -d "$S" ] || [ ! -g "$PROGRAM" ]; then
    fail "the attacks need a running console (pid '$CPID', terminal '$CPTY'), the store folder $S and the setgid program $PROGRAM; not run"
    finish
fi
refused "dev cannot list the store folder" "ls $S"
refused "dev cannot read the store" "cat $S/store/state.json"
refused "dev cannot create a file in the store folder" "touch $S/planted"
refused "dev cannot write the liveness file" "echo x > $S/console.alive"
refused "dev cannot write the program" "echo x >> $PROGRAM"
refused "dev cannot write the passphrase check" "echo x > /etc/lunchroom/$DEVUID.verifier"
refused "dev cannot read the passphrase check" "cat /etc/lunchroom/$DEVUID.verifier"
refused "dev cannot read the console's private key (item 9)" "cat /etc/lunchroom/$DEVUID.console.key"
refused "dev cannot write the public key" "echo x > /etc/lunchroom/$DEVUID.console.pub"
refused "dev cannot write the delivery folder (item 9)" "touch /var/lib/lunchroom-approvals/$DEVUID/planted"
as_dev "ls /var/lib/lunchroom-approvals/$DEVUID >/dev/null && ls /var/lib/lunchroom-approvals/$DEVUID/requests >/dev/null" \
    && pass "dev can list the delivery folder and requests/ (item 9)" || fail "dev cannot list the delivery folder"
as_dev "echo '{}' > /var/lib/lunchroom-approvals/$DEVUID/requests/probe.tmp && rm /var/lib/lunchroom-approvals/$DEVUID/requests/probe.tmp" \
    && pass "dev can write in requests/ (the controller's request files, item 9)" || fail "dev cannot write in requests/"
refused "dev cannot signal the console (kill -0)" "kill -0 $CPID"
refused "dev cannot kill the console" "kill -9 $CPID"
kill -0 "$CPID" 2>/dev/null && pass "the console is still running" || fail "the console died"
refused "dev cannot ptrace-attach to the console" \
    "python3 -c 'import ctypes,sys; l=ctypes.CDLL(None,use_errno=True); sys.exit(0 if l.ptrace(16,$CPID,0,0)==0 else 1)'"
# strace exits 1 when it cannot attach; a timeout (124) would mean it attached.
as_dev "timeout 5 strace -p $CPID -o /dev/null" >"$RUN/last.out" 2>&1
STRACE=$?
[ "$STRACE" -eq 1 ] && pass "strace cannot attach to the console ($(head -c 100 "$RUN/last.out" | tr '\n' ' '))" \
    || fail "strace -p on the console exited $STRACE: $(head -c 200 "$RUN/last.out")"
refused "dev cannot read the console's environment" "cat /proc/$CPID/environ"
refused "dev cannot read the console's memory" "head -c 1 /proc/$CPID/mem"
refused "dev cannot list the console's descriptors" "ls /proc/$CPID/fd"
refused "dev cannot read the console's maps" "cat /proc/$CPID/maps"
measured "the console's terminal $CPTY: $(stat -c '%U:%G mode %a' "$CPTY")"
refused "dev cannot read the console's terminal" "timeout 2 head -c 1 < $CPTY"
refused "dev cannot write the console's terminal" "echo x > $CPTY"
refused "dev cannot push a keystroke into the console's terminal (TIOCSTI)" \
    "python3 -c \"import fcntl,termios,os; fd=os.open('$CPTY',os.O_RDWR); fcntl.ioctl(fd,termios.TIOCSTI,b'x')\""
refused "dev cannot start the console without sudo's password" \
    "sudo -n -u lunchroom $PROGRAM console </dev/null"
refused "dev cannot become lunchroom with another command" "sudo -n -u lunchroom id"

# What dev can do: the program, through its own logic.
as_dev '"$LRP" status' >"$RUN/status.out" 2>&1 \
    && pass "dev's lunchroom status reads the store through the program" \
    || { fail "dev's lunchroom status failed"; head -5 "$RUN/status.out"; }
as_dev 'LUNCHROOM_HOME=/tmp/evil "$LRP" status' >"$RUN/status-evil.out" 2>&1
if [ ! -e /tmp/evil ] && cmp -s "$RUN/status.out" "$RUN/status-evil.out"; then
    pass "LUNCHROOM_HOME is ignored by the setgid program (same store, nothing made at /tmp/evil)"
else
    fail "LUNCHROOM_HOME changed what the setgid program did"
fi
# The setgid program is not dumpable: dev cannot read its environment while it waits for input.
as_dev 'sleep 8 | "$LRP" guard pretool >/dev/null 2>&1' &
WAITING=$!
sleep 1.5
# The program's own process, by its exact name: `pgrep -f` would also match the shell that started
# it, whose command line holds the same words and whose environment dev can read.
GPID=$(pgrep -u dev -x lunchroom | head -1)
measured "the setgid program's ids: $(grep -E '^(Uid|Gid)' /proc/$GPID/status 2>/dev/null | tr '\t\n' '  ')"
if [ -n "$GPID" ]; then
    refused "dev cannot read /proc/<setgid program>/environ (not dumpable)" "cat /proc/$GPID/environ"
    refused "dev cannot read /proc/<setgid program>/mem" "head -c 1 /proc/$GPID/mem"
else
    fail "could not find a running setgid program to test"
fi
wait "$WAITING"

# The outer terminal belongs to dev. A process of dev that opens it can read the keys typed on it.
# Digits only: what the reader does not take reaches the console as keys, and no digit is one of
# the console's keys (a letter would act: `a` approve, `c` the controller form, and so on).
printf '01234567890123456' > "$CKEYS" &
RACE=$(as_dev "timeout 2 head -c 17 < $OUTER" 2>/dev/null)
measured "outer terminal $OUTER ($(stat -c '%U:%G mode %a' "$OUTER")): a dev process reading it received: '$RACE' (item 6 of the spec states this limit; the guard's terminal rule covers guarded agents)"

# ---------------------------------------------------------------------------------------------
heading "5. as dev, through the guard, from a HELD session (items 5, 7, 9)"
# Named, so the console lists it (it lists named agents; section 7e opens its panel).
pty registerA runuser -u dev -- env $DEVENV "$PROGRAM" register ATTACK openclaw:ATTACK
held ATTACK
echo "ATTACK is HELD: $(dev_status | grep -c 'HELD') HELD line(s) in status"
# Rule 1: another terminal (F1.4).
denied_call "rule 1: read the console's terminal" ATTACK "cat $CPTY"
denied_call "rule 1: write the console's terminal" ATTACK "echo x > $CPTY"
denied_call "rule 1: write the outer terminal of another process" ATTACK "echo x > $OUTER"
denied_call "rule 1: read a pseudo-terminal" ATTACK "cat /dev/pts/0"
denied_call "rule 1: write a virtual terminal" ATTACK "echo x > /dev/tty1"
denied_call "rule 1: read a virtual console" ATTACK "cat /dev/vcs1"
# Rule 2: signal the console.
denied_call "rule 2: kill the console by pid" ATTACK "kill -9 $CPID"
denied_call "rule 2: kill -TERM the console by pid" ATTACK "kill $CPID"
denied_call "rule 2: pkill lunchroom" ATTACK "pkill lunchroom"
denied_call "rule 2: pkill -f lunchroom" ATTACK "pkill -f lunchroom"
denied_call "rule 2: killall lunchroom" ATTACK "killall lunchroom"
# Rule 3: debuggers and other processes' secrets, by a pid the call writes (the pid is not in the
# agent's own subtree, so it is DENIED).
denied_call "rule 3: gdb the console by pid" ATTACK "gdb -p $CPID"
denied_call "rule 3: strace the console by pid" ATTACK "strace -p $CPID"
denied_call "rule 3: ltrace the console by pid" ATTACK "ltrace -p $CPID"
denied_call "rule 3: ptrace through python" ATTACK "python3 -c 'import ctypes; ctypes.CDLL(None).ptrace(16, $CPID, 0, 0)'"
denied_call "rule 3: ptrace through perl" ATTACK "perl -e 'syscall(101, 16, $CPID, 0, 0)'"
denied_call "rule 3: read the console's environment" ATTACK "cat /proc/$CPID/environ"
denied_call "rule 3: read the console's memory" ATTACK "head -c 1 /proc/$CPID/mem"
denied_call "rule 3: list the console's descriptors" ATTACK "ls /proc/$CPID/fd"
denied_call "rule 3: read the console's maps" ATTACK "cat /proc/$CPID/maps"
denied_call "rule 3: read init's memory map" ATTACK "cat /proc/1/maps"
# Rule 4: tmux and screen into a pane that is not the agent's own (F1.4).
denied_call "rule 4: tmux send-keys to another pane" ATTACK "tmux send-keys -t %99 'x' Enter"
denied_call "rule 4: tmux capture-pane of another pane" ATTACK "tmux capture-pane -p -t %99"
denied_call "rule 4: tmux pipe-pane of another pane" ATTACK "tmux pipe-pane -t %99 'cat > /tmp/p'"
denied_call "rule 4: screen stuff into another session" ATTACK "screen -S other -X stuff 'x'"
denied_call "rule 4: screen hardcopy of another session" ATTACK "screen -S other -X hardcopy /tmp/h"
protected_call "rule 4: tmux send-keys with no target (the agent's own pane) while HELD" ATTACK "tmux send-keys 'x' Enter"
# Rule 5: becoming lunchroom, and the stores.
denied_call "rule 5: sudo -u lunchroom" ATTACK "sudo -u lunchroom id"
denied_call "rule 5: sudo --user=lunchroom" ATTACK "sudo --user=lunchroom id"
denied_call "rule 5: sudo -g lunchroom (group lunchroom)" ATTACK "sudo -g lunchroom id"
denied_call "rule 5: su lunchroom" ATTACK "su lunchroom -c id"
denied_call "rule 5: runuser to lunchroom" ATTACK "runuser -u lunchroom -- id"
denied_call "rule 5: machinectl shell to lunchroom" ATTACK "machinectl shell lunchroom@ /bin/sh"
denied_call "rule 5: pkexec to lunchroom" ATTACK "pkexec --user lunchroom id"
denied_call "rule 5: doas to lunchroom" ATTACK "doas -u lunchroom id"
denied_call "rule 5: run0 to lunchroom" ATTACK "run0 --user=lunchroom id"
denied_call "rule 5: read the store" ATTACK "cat $S/store/state.json"
denied_call "rule 5: write the store folder" ATTACK "touch /var/lib/lunchroom/$DEVUID/planted"
denied_call "rule 5: write the passphrase check" ATTACK "echo x > /etc/lunchroom/$DEVUID.verifier"
denied_call "rule 5: write the program folder" ATTACK "cp /bin/sh /usr/local/lib/lunchroom/helper"
denied_call "rule 5: write the delivery folder (item 9)" ATTACK "echo x > /var/lib/lunchroom-approvals/$DEVUID/run-x.phase.json"
denied_call "rule 5: name the delivery folder (item 9)" ATTACK "ls /var/lib/lunchroom-approvals/$DEVUID"
denied_call "rule 5: read the private key (item 9)" ATTACK "cat /etc/lunchroom/$DEVUID.console.key"
denied_call "rule 5: name the public key (item 9)" ATTACK "cat /etc/lunchroom/$DEVUID.console.pub"
denied_call "rule 5: write the sudo rule file of item 8" ATTACK "echo x > /etc/sudoers.d/zz-lunchroom-$DEVUID"
denied_call "item 7: read /etc/lunchroom, where the verifiers are" ATTACK "cat /etc/lunchroom/$DEVUID.verifier"
# Rule 6: the console command.
denied_call "rule 6: lunchroom console" ATTACK "lunchroom console"
denied_call "rule 6: the installed program, console" ATTACK "$PROGRAM console"
# The neighbours these rules leave alone.
allowed_call "an ordinary command" ATTACK "echo hello"
allowed_call "lunchroom status" ATTACK "lunchroom status"
allowed_call "killing an unrelated name" ATTACK "pkill -0 no-such-program-xyz"
allowed_call "sudo for another user" ATTACK "sudo -n -u nobody true"
allowed_call "strace that starts a program of its own" ATTACK "strace -o /dev/null /bin/true"
# Item 5: gdb that starts a program of its own runs (decision of 2026-11-14: with -nx and only run, r,
# bt, backtrace, continue, c, quit or q as -ex, then --args and the program). Without -nx it reads
# a .gdbinit the guard does not: while HELD it is GATED (§5 P1.4) and goes to the user.
allowed_call "gdb -nx that starts a program of its own, while HELD (item 5: it runs)" ATTACK "gdb -nx --batch -ex run --args /bin/true"
protected_call "gdb without -nx, while HELD (GATED: it reads a .gdbinit)" ATTACK "gdb --batch -ex run --args /bin/true"
allowed_call "tmux with no pane named" ATTACK "tmux -V"
typed ATTACK
allowed_call "a command as the user's own turn (ATTACK typed in, no longer HELD)" ATTACK "echo ok"
allowed_call "gdb that starts a program of its own, not HELD (item 5: it runs)" ATTACK "gdb --batch -ex run --args /bin/true"
held ATTACK

# ---------------------------------------------------------------------------------------------
heading "6. one event of each kind, on the console's screen within 2 s, and its alert cell (item 10)"
pty register1 runuser -u dev -- env $DEVENV "$PROGRAM" register alpha codex:A1
pty register2 runuser -u dev -- env $DEVENV "$PROGRAM" register beta codex:B1
# The sender is the session a guard hook saw in the very process `send` descends from (I1.2), as
# in an OpenClaw gateway: the hook and the send run in one shell, which plays the gateway.
SEND_AS_S1='python3 /opt/proof/hook.py prompt S1 "hello" --typed | "$LRP" guard prompt >"$HOME/s1-prompt.out" 2>&1; LUNCHROOM_SESSION=openclaw:S1 OPENCLAW_SHELL=exec "$LRP" send'
# `; exit $?` keeps the shell alive under the send: bash runs the last command of `-c` in its own
# place (exec), which would take away the process the guard saw, the one the send descends from.
as_dev "$SEND_AS_S1 alpha 'first message'; exit \$?" >"$RUN/send1.out" 2>&1
expect "a sent message shows as QUEUED within 2 s" "QUEUED lr-" 2
expect_cell "a message QUEUED for an agent that does not run" "UNGUARDED 1" amber 3
# A message FAILED: sent to beta, then beta's name is taken away.
as_dev "$SEND_AS_S1 beta 'second message'; exit \$?" >"$RUN/send2.out" 2>&1
pty unregister runuser -u dev -- env $DEVENV "$PROGRAM" unregister beta
expect "a FAILED message shows within 2 s" "FAILED lr-" 2
expect_cell "a FAILED message" "FAILED MSG 1" amber 3
# A call waiting for approval: a HELD session, a PROTECTED call, and nobody to ask.
held WAITER1
guard_event pretool WAITER1 "git push origin main" --no-channel >"$RUN/waiting.out" 2>&1
expect "a call waiting for approval shows within 2 s" "WAITER1" 2
expect "the calls waiting have their heading" "CALLS WAITING FOR YOU" 2
expect_cell "a call waiting" "CALLS WAIT 1" amber 3
held WAITER2
guard_event pretool WAITER2 "git push origin develop" --no-channel >"$RUN/waiting2.out" 2>&1
held WAITER3
guard_event pretool WAITER3 "git push origin release" --no-channel >"$RUN/waiting3.out" 2>&1
expect "three calls wait" "CALLS WAIT 3" 3
# A DENIED burst: six DENIED calls from one session in a few seconds. Named, so the alert names it.
pty registerP runuser -u dev -- env $DEVENV "$PROGRAM" register PROBER openclaw:PROBER
for i in 1 2 3 4 5 6; do
    guard_event pretool PROBER "cat $S/store/state.json" >/dev/null 2>&1
done
expect "a burst of DENIED calls shows PROBING within 2 s" "PROBING" 2
expect_cell "a burst of DENIED calls" "PROBING 1" red 3
expect "the probing alert has its line, with the agent and the count (the one-line form of item 10)" "PROBING PROBER" 3
# A call the guard could not decide: it cannot read its input, and records that it could not decide.
echo 'this is not json' | as_dev '"$LRP" guard pretool' >/dev/null 2>&1
if screen_shows "could not decide" 3; then
    measured "an undecided call is on the main screen"
else
    measured "an undecided call is not on the main screen (item 3 names the guard's timeouts and slow decisions, which the GUARD SLOW cell below shows, not undecided calls)"
fi
# The store held for longer than the guard's 10 s: a decision that timed out.
python3 "$PROOF/hold.py" "$S/store/state.flock" 14 "$RUN/held" >/dev/null 2>&1 &
HOLDER=$!
n=0; until [ -e "$RUN/held" ] || [ $n -ge 20 ]; do sleep 0.25; n=$((n + 1)); done
guard_event pretool SLOWPOKE "ls /home/dev" >"$RUN/slow.out" 2>&1
wait "$HOLDER" 2>/dev/null
expect_cell "a guard decision that ran past its time (timeout in the last hour)" "GUARD SLOW" red 15
# A failing check: the program's folder made writable by a group, then put back (decision of
# 2026-11-14, "Add the check": the program and its folder are root's, writable by no one else).
# The cell before: the FAILED message to beta, whose name was taken away above, is itself a failed
# check (§17.10 table: "discard a QUEUED or FAILED message to an agent no longer named"). The
# console runs the setup checks every 60 s, so each wait allows 90 s.
CHECKS_BEFORE=$(screen_text | grep -oE 'CHECKS (OK|[0-9]+ FAIL)' | head -1)
N_BEFORE=$(printf '%s' "$CHECKS_BEFORE" | grep -oE '[0-9]+' || echo 0)
measured "the CHECKS cell before the folder is made writable: '$CHECKS_BEFORE'"
[ -n "$CHECKS_BEFORE" ] || { fail "no CHECKS cell on the screen"; CHECKS_BEFORE="CHECKS-CELL-MISSING"; }
chmod 775 /usr/local/lib/lunchroom
expect "a failing doctor check shows in the strip as one more FAIL" "CHECKS $((N_BEFORE + 1)) FAIL" 90
expect_cell "a failing doctor check" "CHECKS" red 5
chmod 755 /usr/local/lib/lunchroom
expect "with the check passing again, CHECKS is as before ('$CHECKS_BEFORE')" "$CHECKS_BEFORE" 90
kill -0 "$CPID" 2>/dev/null && pass "the console survived every attack and event" \
    || fail "the console died during the proof"

# ---------------------------------------------------------------------------------------------
heading "7. detail panels and what can be done from them (items 3 and 10)"
# 7a. The main screen holds no message text; the panel holds it whole.
LONGMARK=END-OF-THE-LONG-MESSAGE-7c41
LONG=$(for i in $(seq 1 90); do printf 'line %s of a long message the panel must show whole\\n' "$i"; done)
as_dev "$SEND_AS_S1 alpha \"\$(printf '%b' '$LONG$LONGMARK')\"; exit \$?" >"$RUN/send3.out" 2>&1
# Its own id: alpha's first message (section 6) is QUEUED too, and its line comes first.
LONGID=$(grep -oE 'lr-[0-9]+' "$RUN/send3.out" | head -1)
measured "the long message is ${LONGID:-'(no id in the send output)'}: $(head -c 200 "$RUN/send3.out" | tr '\n' ' ')"
expect "the long message shows as QUEUED" "QUEUED ${LONGID:-lr-}" 3
screen_has "line 1 of a long message" && fail "the main screen shows message text (item 10: one line per item, no message text)" \
    || pass "the main screen holds no message text"
screen_has "git push origin main" && fail "the main screen shows a full command" || pass "the main screen holds no full command"
# 7b. Enter and a click each open the selected item's panel; Esc and a click outside close it.
click_text "QUEUED ${LONGID:-lr-}"
expect "a click on a QUEUED message opens its panel" "THE WHOLE TEXT" 3
expect "the panel shows the text from its first line" "line 1 of a long message" 2
press END
press_n DOWN 150
expect "a text longer than the screen scrolls and its last byte is shown" "$LONGMARK" 3
press ESC
expect_not "Esc closes the panel" "THE WHOLE TEXT" 2
press UP; press DOWN
n=0
until screen_has "THE WHOLE TEXT" || [ $n -ge 30 ]; do press ENTER; screen_has "THE WHOLE TEXT" || { press ESC; press DOWN; }; n=$((n + 1)); done
screen_has "THE WHOLE TEXT" && pass "the arrow keys and Enter open an item's panel" || fail "no item's panel opened with the arrow keys and Enter"
CLOSED=no
for pos in "2 2" "139 2" "2 49" "139 49"; do
    set -- $pos
    click "$1" "$2"
    screen_has "THE WHOLE TEXT" || { CLOSED=yes; break; }
done
[ "$CLOSED" = yes ] && pass "a click outside the panel closes it" || fail "a click outside the panel did not close it"
press ESC

# 7c. A call waiting: refuse it (the call is DENIED, and a retry says the user refused it).
click_text "WAITER1"
expect "a click on a call waiting opens its panel with the whole call" "git push origin main" 3
expect "the panel names the folder the call runs in" "folder" 2
press x
confirm_asked && pass "refusing asks y/N" || measured "refusing acted at once, with no y/N"
sleep 1
OUT=$(guard_event pretool WAITER1 "git push origin main" --no-channel 2>&1); CODE=$?
# §17.10 table: "a retry of that exact call is DENIED with the reason that the user declined this exact call".
{ [ $CODE -eq 2 ] && echo "$OUT" | grep -qi "declined this exact call"; } \
    && pass "after the user refused the call, a retry of that exact call is DENIED with the reason that the user declined it" \
    || fail "the retry of the refused call ($CODE): $(echo "$OUT" | head -c 300)"
# 7d. Approve: wrong passphrase approves nothing; the right one approves that one call.
click_text "WAITER2"
expect "the panel of the second call" "git push origin develop" 3
press a
expect "approve asks the passphrase" "Passphrase:" 2
type_wrong
expect "a wrong passphrase is said to be wrong" "The passphrase was wrong" 2
press ESC
OUT=$(guard_event pretool WAITER2 "git push origin develop" --no-channel 2>&1); CODE=$?
[ $CODE -ne 0 ] && pass "an approve with a wrong passphrase approved nothing (the retry still stops: $CODE)" \
    || fail "the retry ran after an approve with a wrong passphrase"
click_text "WAITER2"
press a
expect "approve asks the passphrase again" "Passphrase:" 2
press ENTER
OUT=$(guard_event pretool WAITER2 "git push origin develop" --no-channel 2>&1); CODE=$?
[ $CODE -ne 0 ] && pass "an approve with no passphrase approved nothing" || fail "the retry ran after an approve with no passphrase"
press ESC
press ESC
click_text "WAITER2"
press a
expect "approve asks the passphrase" "Passphrase:" 2
type_secret
OUT=$(guard_event pretool WAITER2 "git push origin develop" --no-channel 2>&1); CODE=$?
{ [ $CODE -eq 0 ] && ! echo "$OUT" | grep -q DENIED; } \
    && pass "with the passphrase, the exact call was approved: its retry runs" \
    || fail "the retry of the approved call ($CODE): $(echo "$OUT" | head -c 300)"
# A second approve, under a minute after the first, asks the passphrase again.
click_text "WAITER3"
press a
expect "a second approve, under a minute after the first, asks the passphrase again (item 3)" "Passphrase:" 2
press ESC
press ESC

# 7e. A HELD agent: its panel shows what holds it; releasing asks the passphrase.
click_text "ATTACK"
expect "an agent's panel shows what holds it" "HELD BY" 3
expect "the panel shows the message that holds the agent, whole" "hello from another agent" 2
press r
expect "release asks the passphrase" "Passphrase:" 2
type_wrong
press ESC
held_now ATTACK && pass "a release with a wrong passphrase left the agent HELD" \
    || fail "the agent was released by a wrong passphrase"
click_text "ATTACK"
press r
type_secret
sleep 1
held_now ATTACK \
    && fail "the agent is still HELD after the release with the passphrase" \
    || pass "the release with the passphrase ended the hold, as when the user types in that session"
press ESC

# 7f. An agent that is UNGUARDED or STOPPED: the panel shows the evidence and the repairing command, which it does not run.
click_text "alpha"
expect "an agent without guard evidence: the panel shows what is missing and why" "EVIDENCE MISSING, AND WHY" 3
expect "the panel shows the command that repairs it" "THE COMMAND THAT REPAIRS IT" 2
expect "the panel says the console shows it and does not run it" "does not run it" 2
press ESC

# 7g. The probing alert: the DENIED calls with their rules; hold the agent; dismiss.
click_text "PROBING PROBER"
expect "a probing alert's panel shows the agent's DENIED calls" "DENIED CALLS OF THE LAST 10 MINUTES" 3
expect "...each with its rule" "rule" 2
press h
confirm_asked
held_now PROBER \
    && pass "h held the probing agent (HELD by the user)" || fail "h did not hold the probing agent"
# ATTACK's burst of section 5 is still in the last 10 minutes: dismissing PROBER's alert takes the
# count down by one and its line away.
PROBING_BEFORE=$(screen_text | grep -oE 'PROBING [0-9]+' | head -1 | grep -oE '[0-9]+')
click_text "PROBING PROBER"
press d
# Dismissing asks y/N (console/ui.rs: Action::Dismiss, Need::Confirm).
confirm_asked && pass "dismissing the alert asks y/N" || fail "dismissing the alert did not ask y/N"
expect_cell "dismissing the probing alert takes its count away" "PROBING $(( ${PROBING_BEFORE:-1} - 1 ))" \
    "$([ "${PROBING_BEFORE:-1}" -gt 1 ] && echo red || echo dark)" 4
expect_not "the dismissed alert's line is gone" "PROBING PROBER" 3

# 7h. A FAILED message: resend (a new message that holds its recipient) and discard.
pty register3 runuser -u dev -- env $DEVENV "$PROGRAM" register beta codex:B2
click_text "FAILED lr-"
expect "a FAILED message's panel shows its whole text, sender, recipient and reason" "THE WHOLE TEXT" 3
expect "...with its reason" "reason" 2
press s
confirm_asked
held_now beta \
    && pass "a resent message holds its recipient" || measured "status after the resend: $(dev_status | grep -E '^beta' | head -c 200)"
click_text "QUEUED lr-"
expect "a QUEUED message's panel opens" "THE WHOLE TEXT" 3
press d
confirm_asked
expect_not "discard removes the message from the screen" "THE WHOLE TEXT" 3

# 7i. A failing doctor check: its panel shows the check and what it read.
chmod 775 /usr/local/lib/lunchroom
expect_re "the failing check is on the screen" 'CHECKS [0-9]+ FAIL' 30
open_by_arrows "WHAT IT READ" && pass "a failed check's panel shows what it read" \
    || fail "no panel with WHAT IT READ opened for the failing check"
press ESC
chmod 755 /usr/local/lib/lunchroom

# 7j. Unregister: within 15 minutes of the passphrase last typed it asks none.
click_text "beta"
press u
sleep 1
screen_has "Passphrase:" && fail "unregister within 15 minutes of the passphrase asked for it again" \
    || pass "unregister within 15 minutes of the passphrase last typed asks none (item 3)"
confirm_asked
sleep 1
dev_status | grep -q "^beta" && fail "beta is still registered after u" || pass "u unregistered the agent"
press ESC

# 7k. Hold mode and yolo: each loosening action asks the passphrase.
press m
expect "m opens the hold mode choice" "HOLD MODE" 2
press r
expect "relaxed (a loosening) asks the passphrase" "Passphrase:" 2
type_secret
sleep 1
dev_status | grep -qi "hold mode: Relaxed" && pass "the hold mode is now Relaxed" || measured "status's hold mode line: $(dev_status | grep -i 'hold mode' | head -c 120)"
press m
press s
sleep 1
screen_has "Passphrase:" && fail "strict (tightening) asked the passphrase" || pass "strict (tightening) asked no passphrase"
confirm_asked
sleep 1
dev_status | grep -qi "hold mode: Strict" && pass "the hold mode is now Strict" || measured "status's hold mode line: $(dev_status | grep -i 'hold mode' | head -c 120)"
press m
press b
expect "balanced (a loosening) asks the passphrase" "Passphrase:" 2
type_secret
press y
expect "y opens the yolo form" "YOLO" 2
type_text "alpha"
press TAB
type_text "23:59"
press ENTER
expect "yolo asks the passphrase" "Passphrase:" 2
type_secret
sleep 1
dev_status | grep -qi "yolo" && pass "yolo is in force for alpha until the time given" || measured "status shows no yolo line: $(dev_status | head -c 300)"

# 7l. The key line is a target for clicks.
click_text "controller"
expect "a click on the key line's 'controller' opens the controller approval" "CONTROLLER APPROVAL" 3
press ESC

# ---------------------------------------------------------------------------------------------
heading "8. a controller approval, end to end (item 9)"
REQ=/var/lib/lunchroom-approvals/$DEVUID/requests
DEL=/var/lib/lunchroom-approvals/$DEVUID
new_run() { echo "run-$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"; }
new_hash() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }
write_request() {
    # write_request RUN PHASE HASH: the controller's request file, as dev (the agent's and the controller's user) writes it.
    as_dev 'printf "{\"run_id\":\"%s\",\"phase\":\"%s\",\"manifest_hash\":\"%s\"}" "$1" "$2" "$3" > "$4/$1.$2.json" && chmod 640 "$4/$1.$2.json"' "$1" "$2" "$3" "$REQ"
}
RUN1=$(new_run); HASH1=$(new_hash); PHASE1=deploy-prod
write_request "$RUN1" "$PHASE1" "$HASH1"
# Requests that must not be listed: another field, a name that disagrees with its content.
as_dev 'printf "{\"run_id\":\"%s\",\"phase\":\"x\",\"manifest_hash\":\"%s\",\"extra\":1}" "$1" "$2" > "$3/$1.x.json"' "$RUN1" "$HASH1" "$REQ"
as_dev 'printf "{\"run_id\":\"%s\",\"phase\":\"other\",\"manifest_hash\":\"%s\"}" "$1" "$2" > "$3/$1.mismatch.json"' "$RUN1" "$HASH1" "$REQ"
press c
expect "c lists the request with its run id, phase and the whole plan hash" "$RUN1" 3
expect "...the phase" "$PHASE1" 2
expect "...the plan hash" "$HASH1" 2
expect "requests that are not valid are counted, not listed" "not valid requests" 2
# N, an empty answer: nothing written.
press ENTER
expect "choosing it shows the run id, the phase and the plan hash and asks y/N" "Approve this phase? y/N" 3
press n
sleep 1
[ ! -e "$DEL/$RUN1.$PHASE1.json" ] && pass "N writes nothing" || fail "N wrote an approval"
press c
press ENTER
press y
expect "y asks the passphrase, for every phase" "Passphrase:" 2
type_wrong
sleep 1
[ ! -e "$DEL/$RUN1.$PHASE1.json" ] && pass "a wrong passphrase writes nothing" || fail "a wrong passphrase wrote an approval"
press ESC
press ESC
press c
press ENTER
press y
press ENTER
sleep 1
[ ! -e "$DEL/$RUN1.$PHASE1.json" ] && pass "an empty passphrase writes nothing" || fail "an empty passphrase wrote an approval"
press ESC
press ESC
press c
press ENTER
press y
type_secret
n=0; until [ -e "$DEL/$RUN1.$PHASE1.json" ] || [ $n -ge 20 ]; do sleep 0.5; n=$((n + 1)); done
[ -f "$DEL/$RUN1.$PHASE1.json" ] && pass "the approval file $RUN1.$PHASE1.json was written" \
    || { fail "no approval file was written"; screen_text | tail -15 | sed 's/^/    | /'; }
[ "$(stat -c '%a %U' "$DEL/$RUN1.$PHASE1.json" 2>/dev/null)" = "640 lunchroom" ] \
    && pass "the approval file is owned by lunchroom, mode 0640" || fail "the approval file is $(stat -c '%a %U' "$DEL/$RUN1.$PHASE1.json" 2>&1)"
as_dev "cat $DEL/$RUN1.$PHASE1.json >/dev/null" && pass "dev can read the approval (the controller reads it as dev)" || fail "dev cannot read the approval"
as_dev "echo x >> $DEL/$RUN1.$PHASE1.json" 2>/dev/null && fail "dev can write the approval file" || pass "dev cannot write the approval file"
cp "$DEL/$RUN1.$PHASE1.json" "$RUN/approval1.json"
cat > "$RUN/check_approval.py" <<'PY'
import base64, json, re, subprocess, sys
path, run_id, phase, mhash, pub_path = sys.argv[1:6]
doc = json.load(open(path))
bad = []
if sorted(doc) != ["record", "signature"]:
    bad.append("the file holds %s, not exactly record and signature" % sorted(doc))
rec = doc["record"]
fields = ["issued_at", "manifest_hash", "nonce", "operator", "phase", "run_id", "source"]
if sorted(rec) != fields:
    bad.append("the record holds %s, not exactly the seven fields" % sorted(rec))
checks = [
    (rec.get("run_id") == run_id, "run_id as typed"),
    (rec.get("phase") == phase, "phase as typed"),
    (rec.get("manifest_hash") == mhash, "manifest_hash is the plan hash shown"),
    (rec.get("operator") == "dev", "operator is the developer's login name"),
    (rec.get("source") == "console", "source is console"),
    (re.fullmatch(r"[0-9a-f]{32}", rec.get("nonce", "")) is not None, "nonce is 32 lowercase hex digits"),
    (re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", rec.get("issued_at", "")) is not None, "issued_at is YYYY-MM-DDTHH:MM:SSZ"),
]
for ok, what in checks:
    print(("PASS " if ok else "FAIL ") + "the record: " + what)
for b in bad:
    print("FAIL " + b)
canonical = json.dumps(rec, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
open(path + ".canonical", "wb").write(canonical)
pub = base64.b64decode(open(pub_path).read().strip(), validate=True)
sig = base64.b64decode(doc["signature"], validate=True)
print("MEASURED signature %d bytes, public key %d bytes, canonical bytes %d" % (len(sig), len(pub), len(canonical)))
print(("PASS " if len(sig) == 64 else "FAIL ") + "the signature is 64 bytes")
try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    Ed25519PublicKey.from_public_bytes(pub).verify(sig, canonical)
    print("PASS the signature verifies over the canonical JSON with python's cryptography (an Ed25519 verifier other than Lunchroom's)")
except ImportError:
    print("MEASURED python's cryptography is not in this container; the pure verifier is used")
except Exception as e:
    print("FAIL the signature does not verify with python's cryptography: %r" % (e,))
PY
python3 "$RUN/check_approval.py" "$RUN/approval1.json" "$RUN1" "$PHASE1" "$HASH1" /etc/lunchroom/$DEVUID.console.pub | sed 's/^/    /' > "$RUN/check1.out"
cat "$RUN/check1.out" | sed 's/^    //'
PASSES=$((PASSES + $(grep -c '^ *PASS ' "$RUN/check1.out"))); FAILS=$((FAILS + $(grep -c '^ *FAIL ' "$RUN/check1.out")))
if python3 -I "$PROOF/ed25519_verify.py" --selftest; then
    python3 -I "$PROOF/ed25519_verify.py" "$(cat /etc/lunchroom/$DEVUID.console.pub)" \
        "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["signature"])' "$RUN/approval1.json")" "$RUN/approval1.json.canonical" \
        && pass "the signature verifies with the pure RFC 8032 verifier too (self-tested on RFC 8032 test 1)" \
        || fail "the pure RFC 8032 verifier rejects the signature"
else
    measured "the pure verifier's self-test failed: it is not used"
fi
# The canonical bytes equal what the controller's `canonical` gives for the same record.
python3 - "$RUN/approval1.json" "$RUN/approval1.json.canonical" <<'PY' && pass "the signed bytes equal json.dumps(sort_keys, (',',':'), ensure_ascii=False).encode('utf-8'), the controller's canonical" || fail "the signed bytes differ from the controller's canonical form"
import json, sys
rec = json.load(open(sys.argv[1]))["record"]
want = json.dumps(rec, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
sys.exit(0 if open(sys.argv[2], "rb").read() == want else 1)
PY
# A second approval of another phase, under a minute after the first, asks the passphrase again;
# two approvals carry different nonces.
RUN2=$RUN1; PHASE2=release; HASH2=$(new_hash)
write_request "$RUN2" "$PHASE2" "$HASH2"
press c
# By its plan hash, which only its line holds ("release" is also a word of the key line). A click
# on a request chooses it, as Enter does on the selected one: the y/N question follows.
click_on "$HASH2"
press y
expect "a second approval, under a minute after the first, asks the passphrase again" "Passphrase:" 2
type_secret
n=0; until [ -e "$DEL/$RUN2.$PHASE2.json" ] || [ $n -ge 20 ]; do sleep 0.5; n=$((n + 1)); done
[ -f "$DEL/$RUN2.$PHASE2.json" ] && pass "the second approval was written" || fail "the second approval was not written"
N1=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["record"]["nonce"])' "$DEL/$RUN1.$PHASE1.json" 2>/dev/null)
N2=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["record"]["nonce"])' "$DEL/$RUN2.$PHASE2.json" 2>/dev/null)
[ -n "$N1" ] && [ -n "$N2" ] && [ "$N1" != "$N2" ] && pass "two approvals carry different nonces" || fail "the two approvals' nonces are '$N1' and '$N2'"
# Malformed keys: refused, nothing written (typed into the controller screen).
BEFORE=$(ls "$DEL" | wc -l)
for bad in "a-controller ${RUN1}0 deploy" "a-controller RUN-UPPER deploy" "a-controller $RUN1 .hidden" "a-controller $RUN1" "a-controller $RUN1 deploy extra"; do
    press c
    type_text "$bad"
    press ENTER
    sleep 0.5
    screen_has "Approve this phase?" && fail "a malformed key '$bad' reached the y/N question"
    press ESC
    press ESC
done
[ "$(ls "$DEL" | wc -l)" = "$BEFORE" ] && pass "a malformed run id or phase writes nothing" || fail "a malformed key wrote a file"
# A request that is missing: the key names a run and phase with no request file.
press c
type_text "a-controller $(new_run) deploy"
press ENTER
sleep 0.5
screen_has "Approve this phase?" && fail "a key with no request file reached the y/N question" || pass "an approval request that is missing is refused and nothing is written"
press ESC
press ESC
# An agent's command naming the delivery folder or either key is DENIED (checked in section 5).

# ---------------------------------------------------------------------------------------------
heading "9. the console is left alone for 15 minutes (items 3 and 10)"
if [ "${SKIP_LOCK_WAIT:-0}" = 1 ]; then
    measured "SKIP_LOCK_WAIT=1: the 15-minute lock is not exercised in this run"
else
    press ESC
    T0=$(date +%s)
    HEAD0=$(screen_text | grep -o 'screen locks in [0-9:]*' | head -1)
    measured "no key from now (T0). The header at T0: $HEAD0"
    sleep 420
    screen_text | grep -qE 'screen locks in 0[6-8]:' \
        && pass "7 minutes on, the header shows the minutes left to the lock (06:xx to 08:xx)" \
        || fail "7 minutes on, the header does not count down: $(screen_text | grep -o 'screen locks in [0-9:]*' | head -1)"
    pty reg4 runuser -u dev -- env $DEVENV "$PROGRAM" register gamma codex:G1
    as_dev "$SEND_AS_S1 gamma 'a message that fails while the console is unattended'; exit \$?" >"$RUN/send4.out" 2>&1
    pty unreg4 runuser -u dev -- env $DEVENV "$PROGRAM" unregister gamma
    expect "a FAILED message arrives within the unattended time and is shown (the screen is live)" "FAILED MSG" 3
    while [ $(( $(date +%s) - T0 )) -lt 910 ]; do sleep 5; done
    expect "after 15 minutes without a key the console is locked" "LOCKED" 5
    expect "the locked screen asks the passphrase" "Passphrase to unlock" 2
    screen_has "alpha" && fail "the locked screen still shows an agent's name: the details are not hidden" \
        || pass "the details are hidden while the console is locked"
    expect "the alert strip stays drawn while locked" "CALLS WAIT" 2
    BEFORE_CELL=$(screen_text | grep -o 'FAILED MSG [0-9]*' | head -1)
    pty reg5 runuser -u dev -- env $DEVENV "$PROGRAM" register delta codex:D1
    as_dev "$SEND_AS_S1 delta 'a message that fails while the console is locked'; exit \$?" >"$RUN/send5.out" 2>&1
    pty unreg5 runuser -u dev -- env $DEVENV "$PROGRAM" unregister delta
    sleep 3
    AFTER_CELL=$(screen_text | grep -o 'FAILED MSG [0-9]*' | head -1)
    [ -n "$AFTER_CELL" ] && [ "$AFTER_CELL" != "$BEFORE_CELL" ] \
        && pass "the alert strip changed while locked ('$BEFORE_CELL' to '$AFTER_CELL'): it is live" \
        || fail "the alert strip did not change while locked ('$BEFORE_CELL', '$AFTER_CELL')"
    press ENTER
    press a
    click 20 20
    screen_has "THE WHOLE TEXT" || screen_has "APPROVE CALL" && fail "a panel or modal opened while locked" || pass "when locked, no panel opens and no action runs"
    type_wrong
    expect "a wrong passphrase leaves it locked" "The passphrase was wrong" 3
    expect "...still locked" "LOCKED" 2
    type_secret
    expect_not "the passphrase unlocks the console" "LOCKED" 5
    screen_has "AGENTS" && pass "after the passphrase the details are shown again" || fail "the details did not come back after unlocking"
    press l
    expect "l locks the console at once" "LOCKED" 3
    type_secret
    expect_not "the passphrase unlocks it again" "LOCKED" 5
fi
kill -0 "$CPID" 2>/dev/null && pass "the console is still running" || fail "the console is gone"
echo "---- the console's last screen ----"
screen_text
echo "-----------------------------------"

# ---------------------------------------------------------------------------------------------
heading "10. the passphrase and the sudo password: nowhere"
press q
wait "$DRIVER" 2>/dev/null
[ -e /var/lib/lunchroom/$DEVUID/console.alive ] \
    && fail "console.alive is still there after the console ended" \
    || pass "the console removed console.alive when it ended"
shred -u "$RUN"/*.pw 2>/dev/null
scan "Lunchroom passphrase" "$LRPW"
scan "sudo password" "$SUDOPW"

finish
