#!/bin/bash
# The proof of the Lunchroom console (1.1), run as root in the container of `Containerfile`
# (Ubuntu 24.04, real users, a real sudo, real pseudo-terminals).
#
# What it does, in order (the design, section 4):
#   1. makes the user `dev` and installs Lunchroom for `dev`, passphrase and all, through the
#      program's own prompts on a pseudo-terminal; the secrets are typed from files, never given
#      as an argument or in the environment;
#   2. runs `lunchroom install console`, and checks every file it makes;
#   3. starts the console as the user `lunchroom` on a pseudo-terminal, as `dev` does, through the
#      sudoers rule (it asks for `dev`'s sudo password);
#   4. as `dev`, without the guard, tries each attack and records the kernel's answer;
#   5. as `dev`, through the guard, feeds each attack to `lunchroom guard pretool` from a HELD
#      session, and expects every one DENIED;
#   6. drives one event of each kind and checks that the console's screen shows it within 2 s;
#   7. searches the whole container for the passphrase and the sudo password.
#
# It prints PASS or FAIL for each check, MEASURED for a fact it only records, and exits non-zero on
# any FAIL.

set -u

PROOF=/opt/proof
RUN=/run/proof
PROGRAM=/usr/local/lib/lunchroom/lunchroom
DEVENV="HOME=/home/dev PATH=/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin"
PASSES=0
FAILS=0

pass() { echo "PASS $*"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL $*"; FAILS=$((FAILS + 1)); }
measured() { echo "MEASURED $*"; }
heading() { echo; echo "== $*"; }

# Runs bash code as the user dev, with the code as the first argument and the rest as $1, $2, ...
as_dev() {
    local code=$1
    shift
    runuser -u dev -- env $DEVENV bash -c "$code" _ "$@"
}

# True when the first argument (a screen) shows the text.
screen_has() { python3 "$PROOF/lastscreen.py" "$RUN/console.log" | grep -qF -- "$1"; }

# Waits up to 2 seconds for the console's screen to show the text.
screen_shows() {
    local text=$1 n=0
    while [ $n -lt 10 ]; do
        screen_has "$text" && return 0
        sleep 0.2
        n=$((n + 1))
    done
    return 1
}

# Runs a command on a pseudo-terminal, typing the prompts' answers from files.
AUTO=(
    --auto "New passphrase \\(at least [0-9]+ characters\\): ::$RUN/lr.pw"
    --auto "New passphrase again: ::$RUN/lr.pw"
    --auto "Current passphrase: ::$RUN/lr.pw"
    --auto "Passphrase: ::$RUN/lr.pw"
    --auto "\\[sudo\\] password for dev: ::$RUN/sudo.pw"
    --auto "Choose 1, 2 or 3 \\[2\\]: ::$RUN/enter"
)
pty() {
    local name=$1
    shift
    python3 "$PROOF/ptyrun.py" --log "$RUN/$name.log" "${AUTO[@]}" -- "$@"
}

# ---------------------------------------------------------------------------------------------
heading "facts of this system"
echo "kernel.yama.ptrace_scope=$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo absent)"
echo "dev.tty.legacy_tiocsti=$(cat /proc/sys/dev/tty/legacy_tiocsti 2>/dev/null || echo absent)"
echo "sudoers use_pty: $(grep -rh '^Defaults.*use_pty' /etc/sudoers /etc/sudoers.d 2>/dev/null | head -1)"
echo "sudo: $(sudo -V 2>/dev/null | head -1)"
echo "kernel: $(uname -r)"

heading "1. the user dev, and Lunchroom installed for dev"
useradd -m -s /bin/bash -G sudo dev
DEVUID=$(id -u dev)
mkdir -p "$RUN"
chmod 700 "$RUN"
umask 077
head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/lr.pw"
head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/sudo.pw"
: > "$RUN/enter"
LRPW=$(cat "$RUN/lr.pw")
SUDOPW=$(cat "$RUN/sudo.pw")
printf 'dev:%s\n' "$SUDOPW" | chpasswd
umask 022
as_dev 'mkdir -p ~/.claude ~/.local/bin && cp /opt/proof/lunchroom ~/lunchroom-dl && chmod 755 ~/lunchroom-dl'

pty passphrase runuser -u dev -- env $DEVENV /home/dev/lunchroom-dl passphrase
if grep -q "the passphrase is set" "$RUN/passphrase.log"; then
    pass "dev set a passphrase on the program's own prompt"
else
    fail "the passphrase was not set (see $RUN/passphrase.log)"
    sed 's/^/    /' "$RUN/passphrase.log" | tail -20
fi
if [ "$(stat -c '%U %a' /etc/lunchroom/$DEVUID.verifier 2>/dev/null)" = "root 644" ]; then
    pass "the passphrase check /etc/lunchroom/$DEVUID.verifier is root's"
else
    fail "the passphrase check is not root's, mode 644"
fi

pty install1 runuser -u dev -- env $DEVENV /home/dev/lunchroom-dl install claude-code
if as_dev 'test -x ~/.lunchroom/bin/lunchroom' && grep -q '.lunchroom/bin/lunchroom' /home/dev/.claude/settings.json; then
    pass "install claude-code put the hooks on the user-mode program"
else
    fail "install claude-code did not finish (see $RUN/install1.log)"
    tail -20 "$RUN/install1.log"
fi

heading "2. lunchroom install console"
pty install2 runuser -u dev -- env $DEVENV /home/dev/.local/bin/lunchroom install console
grep -q "The console is set up" "$RUN/install2.log" \
    && pass "install console says the console is set up" \
    || { fail "install console did not finish (see $RUN/install2.log)"; tail -30 "$RUN/install2.log"; }
[ "$(stat -c '%a %U:%G' $PROGRAM 2>/dev/null)" = "2755 root:lunchroom" ] \
    && pass "the program is $PROGRAM, root:lunchroom, mode 2755" \
    || fail "the program is not root:lunchroom 2755: $(stat -c '%a %U:%G' $PROGRAM 2>&1)"
[ "$(stat -c '%a %U:%G' /usr/local/lib/lunchroom)" = "755 root:root" ] \
    && pass "/usr/local/lib/lunchroom is root:root, mode 755" \
    || fail "/usr/local/lib/lunchroom is $(stat -c '%a %U:%G' /usr/local/lib/lunchroom)"
[ "$(stat -c '%a %U:%G' /var/lib/lunchroom/$DEVUID 2>/dev/null)" = "2770 root:lunchroom" ] \
    && pass "/var/lib/lunchroom/$DEVUID is root:lunchroom, mode 2770" \
    || fail "the store folder is $(stat -c '%a %U:%G' /var/lib/lunchroom/$DEVUID 2>&1)"
id -nG dev | tr ' ' '\n' | grep -qx lunchroom \
    && fail "dev is in the group lunchroom" \
    || pass "dev is not in the group lunchroom"
[ "$(stat -c '%a %U:%G' /etc/sudoers.d/lunchroom 2>/dev/null)" = "440 root:root" ] \
    && pass "/etc/sudoers.d/lunchroom is root:root, mode 440" \
    || fail "the sudoers drop-in is $(stat -c '%a %U:%G' /etc/sudoers.d/lunchroom 2>&1)"
visudo -cf /etc/sudoers.d/lunchroom >/dev/null 2>&1 \
    && pass "visudo -cf passes the drop-in" || fail "visudo -cf fails the drop-in"
grep -q NOPASSWD /etc/sudoers.d/lunchroom \
    && fail "the drop-in has NOPASSWD" || pass "the drop-in asks for dev's password (no NOPASSWD)"
[ "$(readlink /home/dev/.local/bin/lunchroom)" = "$PROGRAM" ] \
    && pass "~/.local/bin/lunchroom links to $PROGRAM" \
    || fail "~/.local/bin/lunchroom is $(readlink /home/dev/.local/bin/lunchroom)"
grep -q "$PROGRAM" /home/dev/.claude/settings.json && ! grep -q '.lunchroom/bin/lunchroom' /home/dev/.claude/settings.json \
    && pass "the Claude Code hooks run $PROGRAM and no other" \
    || fail "the Claude Code hooks do not all run $PROGRAM"
ls -d /home/dev/.lunchroom/store.moved-* >/dev/null 2>&1 && [ ! -e /home/dev/.lunchroom/store ] \
    && pass "the old store was moved aside, not deleted" \
    || fail "the old store was not moved aside"
[ -f /var/lib/lunchroom/$DEVUID/store/state.json ] \
    && pass "the store was copied to /var/lib/lunchroom/$DEVUID" \
    || fail "no store in /var/lib/lunchroom/$DEVUID"

pty doctor runuser -u dev -- env $DEVENV lunchroom doctor
grep -q "PASS the console is set up and the store at /var/lib/lunchroom/$DEVUID" "$RUN/doctor.log" \
    && pass "doctor reports console mode" \
    || { fail "doctor does not report console mode"; tail -c 2000 "$RUN/doctor.log"; }

# ---------------------------------------------------------------------------------------------
heading "3. the console, started as dev starts it"
mkfifo "$RUN/keys"
python3 "$PROOF/ptyrun.py" --log "$RUN/console.log" --ready "$RUN/console.ready" --owner "$DEVUID" \
    --keys "$RUN/keys" --timeout 1500 "${AUTO[@]}" --auto "Lunchroom passphrase: ::$RUN/lr.pw" \
    -- runuser -u dev -- env $DEVENV sudo -u lunchroom $PROGRAM console &
DRIVER=$!
n=0
until screen_has "AGENTS" || [ $n -ge 60 ]; do sleep 0.5; n=$((n + 1)); done
if screen_has "AGENTS"; then
    pass "the console asked for the passphrase and drew its screen"
else
    fail "the console did not draw its screen"
    tail -c 1500 "$RUN/console.log"
fi
CPID=$(pgrep -u lunchroom -x lunchroom | head -1)
OUTER=$(sed -n 2p "$RUN/console.ready")
# The console's controlling terminal, from ps (root in a rootless container lacks the capability
# to read another user's /proc/<pid>/fd).
CPTTY=$(ps -o tty= -p "$CPID" 2>/dev/null | tr -d ' ')
CPTY=${CPTTY:+/dev/$CPTTY}
echo "console pid $CPID, its terminal $CPTY, the outer terminal $OUTER"
[ "$(ps -o user= -p "$CPID" | tr -d ' ')" = "lunchroom" ] \
    && pass "the console runs as the user lunchroom" || fail "the console does not run as lunchroom"
[ "$(cut -d' ' -f1 /var/lib/lunchroom/$DEVUID/console.alive 2>/dev/null)" = "$CPID" ] \
    && pass "console.alive names the console's pid" || fail "console.alive does not name pid $CPID"
M1=$(stat -c %Y /var/lib/lunchroom/$DEVUID/console.alive); sleep 3.2; M2=$(stat -c %Y /var/lib/lunchroom/$DEVUID/console.alive)
[ "$M1" != "$M2" ] && pass "console.alive is rewritten every 2 s" || fail "console.alive was not rewritten in 3 s"

# ---------------------------------------------------------------------------------------------
heading "4. as dev, without the guard: what the kernel answers"
# Every attack below needs its target to exist: a refusal of a file or process that is not there
# proves nothing. Without a running console, its terminal, the store folder and the setgid
# program, the attacks are not run and the proof fails.
S=/var/lib/lunchroom/$DEVUID
if [ -z "$CPID" ] || [ -z "$CPTY" ] || [ ! -d "$S" ] || [ ! -g "$PROGRAM" ]; then
    fail "the attacks need a running console (pid '$CPID', terminal '$CPTY'), the store folder $S and the setgid program $PROGRAM; not run"
    echo
    echo "RESULT: $PASSES passed, $FAILS failed"
    exit 1
fi
refused() {
    local desc=$1 code=$2
    if as_dev "$code" >"$RUN/last.out" 2>&1; then
        fail "$desc: it worked: $(head -c 200 "$RUN/last.out")"
    else
        pass "$desc: refused ($(head -c 100 "$RUN/last.out" | tr '\n' ' '))"
    fi
}
S=/var/lib/lunchroom/$DEVUID
refused "dev cannot list the store folder" "ls $S"
refused "dev cannot read the store" "cat $S/store/state.json"
refused "dev cannot create a file in the store folder" "touch $S/planted"
refused "dev cannot write the liveness file" "echo x > $S/console.alive"
refused "dev cannot write the program" "echo x >> $PROGRAM"
refused "dev cannot write the passphrase check" "echo x > /etc/lunchroom/$DEVUID.verifier"
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
as_dev "lunchroom status" >"$RUN/status.out" 2>&1 \
    && pass "dev's lunchroom status reads the store through the program" \
    || { fail "dev's lunchroom status failed"; head -5 "$RUN/status.out"; }
as_dev "LUNCHROOM_HOME=/tmp/evil lunchroom status" >"$RUN/status-evil.out" 2>&1
if [ ! -e /tmp/evil ] && cmp -s "$RUN/status.out" "$RUN/status-evil.out"; then
    pass "LUNCHROOM_HOME is ignored by the setgid program (same store, nothing made at /tmp/evil)"
else
    fail "LUNCHROOM_HOME changed what the setgid program did"
fi
# The setgid program is not dumpable: dev cannot read its environment while it waits for input.
as_dev "sleep 8 | $PROGRAM guard pretool >/dev/null 2>&1" &
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
printf 'outer-race-probe\n' > "$RUN/keys"
RACE=$(as_dev "timeout 2 head -c 17 < $OUTER" 2>/dev/null)
measured "outer terminal $OUTER ($(stat -c '%U:%G mode %a' "$OUTER")): a dev process reading it received: '$RACE' (the same is true of sudo's own password prompt; the guard's terminal rule covers guarded agents)"

# ---------------------------------------------------------------------------------------------
heading "5. as dev, through the guard, from a HELD session"
guard_event() {
    # guard_event <pretool|prompt> <session> <text> [flag]: the guard's output, and its exit code.
    local event=$1 session=$2 text=$3 flag=${4:-}
    as_dev 'python3 /opt/proof/hook.py "$1" "$2" "$3" $4 | lunchroom guard "$1"' "$event" "$session" "$text" "$flag"
}
held() { guard_event prompt "$1" "hello from another agent" --untyped >/dev/null 2>&1; }
denied_call() {
    local desc=$1 session=$2 command=$3 out code
    out=$(guard_event pretool "$session" "$command" 2>&1)
    code=$?
    if [ $code -eq 2 ] && printf '%s' "$out" | grep -q "DENIED"; then
        pass "DENIED: $desc"
    else
        fail "not DENIED ($code): $desc: $command: $(printf '%s' "$out" | head -c 200)"
    fi
}
allowed_call() {
    local desc=$1 session=$2 command=$3 out code
    out=$(guard_event pretool "$session" "$command" 2>&1)
    code=$?
    if [ $code -eq 0 ] && ! printf '%s' "$out" | grep -q "DENIED"; then
        pass "allowed: $desc"
    else
        fail "refused ($code): $desc: $command: $(printf '%s' "$out" | head -c 200)"
    fi
}
held ATTACK
echo "ATTACK is HELD: $(as_dev 'lunchroom status' 2>&1 | grep -c 'HELD' ) HELD line(s) in status"
# Rule 1: another terminal.
denied_call "rule 1: read the console's terminal" ATTACK "cat $CPTY"
denied_call "rule 1: write the console's terminal" ATTACK "echo x > $CPTY"
denied_call "rule 1: write the outer terminal of another process" ATTACK "echo x > $OUTER"
denied_call "rule 1: read a virtual console" ATTACK "cat /dev/vcs1"
# Rule 2: signal the console.
denied_call "rule 2: kill the console by pid" ATTACK "kill -9 $CPID"
denied_call "rule 2: pkill lunchroom" ATTACK "pkill lunchroom"
denied_call "rule 2: killall lunchroom" ATTACK "killall lunchroom"
# Rule 3: debuggers and other processes' secrets.
denied_call "rule 3: gdb the console" ATTACK "gdb -p $CPID"
denied_call "rule 3: strace the console" ATTACK "strace -p $CPID"
denied_call "rule 3: ptrace through python" ATTACK "python3 -c 'import ctypes; ctypes.CDLL(None).ptrace(16, $CPID, 0, 0)'"
denied_call "rule 3: read the console's environment" ATTACK "cat /proc/$CPID/environ"
denied_call "rule 3: read init's memory map" ATTACK "cat /proc/1/maps"
# Rule 4: tmux and screen into a pane that is not the agent's own.
denied_call "rule 4: tmux send-keys to another pane" ATTACK "tmux send-keys -t %99 'x' Enter"
denied_call "rule 4: tmux capture-pane of another pane" ATTACK "tmux capture-pane -p -t %99"
denied_call "rule 4: screen stuff into another session" ATTACK "screen -S other -X stuff 'x'"
# Rule 5: becoming lunchroom, and the stores.
denied_call "rule 5: sudo -u lunchroom" ATTACK "sudo -u lunchroom id"
denied_call "rule 5: su lunchroom" ATTACK "su lunchroom -c id"
denied_call "rule 5: read the store" ATTACK "cat $S/store/state.json"
denied_call "rule 5: write the passphrase check" ATTACK "echo x > /etc/lunchroom/$DEVUID.verifier"
denied_call "rule 5: write the program folder" ATTACK "cp /bin/sh /usr/local/lib/lunchroom/helper"
# Rule 6: the console command.
denied_call "rule 6: lunchroom console" ATTACK "lunchroom console"
denied_call "rule 6: the installed program, console" ATTACK "$PROGRAM console"
# The neighbours these rules leave alone.
allowed_call "an ordinary command" ATTACK "echo hello"
allowed_call "lunchroom status" ATTACK "lunchroom status"
allowed_call "killing an unrelated name" ATTACK "pkill -0 no-such-program-xyz"
allowed_call "sudo for another user" ATTACK "sudo -n -u nobody true"

# ---------------------------------------------------------------------------------------------
heading "6. one event of each kind, on the console's screen within 2 s"
# A message, QUEUED: alpha is a name for a Codex session that does not run, so nothing is handed over.
pty register1 runuser -u dev -- env $DEVENV lunchroom register alpha codex:A1
pty register2 runuser -u dev -- env $DEVENV lunchroom register beta codex:B1
# The sender is the session a guard hook saw in the very process `send` descends from (I1.2), as
# in an OpenClaw gateway: the hook and the send run in one shell, which plays the gateway.
SEND_AS_S1='python3 /opt/proof/hook.py prompt S1 "hello" --typed | lunchroom guard prompt >"$HOME/s1-prompt.out" 2>&1; LUNCHROOM_SESSION=openclaw:S1 OPENCLAW_SHELL=exec lunchroom send'
# `; exit $?` keeps the shell alive under the send: bash runs the last command of `-c` in its own
# place (exec), which would take away the process the guard saw, the one the send descends from.
as_dev "$SEND_AS_S1 alpha 'first message'; exit \$?" >"$RUN/send1.out" 2>&1
screen_shows "QUEUED: lr-" && pass "a sent message shows as QUEUED within 2 s" \
    || { fail "the QUEUED message is not on the screen"; head -3 "$RUN/send1.out"; echo "prompt hook said:"; head -c 600 /home/dev/s1-prompt.out
         echo "-- diagnosis: the hook and the send in one shell, with every exit status"
         as_dev 'echo "shell pid $$"; python3 /opt/proof/hook.py prompt S1 "hello" --typed | lunchroom guard prompt; echo "prompt hook exit $?"; LUNCHROOM_SESSION=openclaw:S1 OPENCLAW_SHELL=exec lunchroom send alpha "diag"; echo "send exit $?"' 2>&1 | head -20
         echo "-- the store's record of S1 and of process sessions"
         python3 - "/var/lib/lunchroom/$DEVUID/store/state.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
for k in s:
    v = s[k]
    t = json.dumps(v)
    if "S1" in t or "process" in k:
        print(k, t[:600])
PY
       }
# A message FAILED: sent to beta, then beta's name is taken away.
as_dev "$SEND_AS_S1 beta 'second message'; exit \$?" >"$RUN/send2.out" 2>&1
pty unregister runuser -u dev -- env $DEVENV lunchroom unregister beta
screen_shows "FAILED: lr-" && pass "a FAILED message shows within 2 s" \
    || { fail "the FAILED message is not on the screen"; head -3 "$RUN/send2.out"; }
# A call waiting for approval: a HELD session, a PROTECTED call, and nobody to ask.
held WAITER
guard_event pretool WAITER "git push origin main" --no-channel >"$RUN/waiting.out" 2>&1
screen_shows "1. WAITER" && screen_has "CALLS WAITING FOR YOUR APPROVAL" \
    && pass "a call waiting for approval shows, numbered, within 2 s" \
    || { fail "the waiting call is not on the screen"; head -5 "$RUN/waiting.out"; }
# A DENIED burst: six DENIED calls from one session in a few seconds.
for i in 1 2 3 4 5 6; do
    guard_event pretool PROBER "cat $S/store/state.json" >/dev/null 2>&1
done
screen_shows "PROBING" && pass "a burst of DENIED calls shows PROBING within 2 s" \
    || fail "no probing alert on the screen"
screen_shows "It may be probing for a way round Lunchroom's guard" \
    && pass "the probing alert is a notice for the user" || fail "no probing notice on the screen"
# A timeout record: the guard cannot read its input, and records that it could not decide.
echo 'this is not json' | as_dev 'lunchroom guard pretool' >/dev/null 2>&1
screen_shows "could not decide" && pass "a call the guard could not decide shows within 2 s" \
    || fail "the guard's undecided call is not on the screen"
# The console is still alive after all of it.
kill -0 "$CPID" 2>/dev/null && pass "the console survived every attack and event" \
    || fail "the console died during the proof"
echo "---- the last screen ----"
python3 "$PROOF/lastscreen.py" "$RUN/console.log"
echo "-------------------------"

# ---------------------------------------------------------------------------------------------
heading "7. the passphrase and the sudo password: nowhere"
printf 'q\n' > "$RUN/keys"
wait "$DRIVER" 2>/dev/null
[ -e /var/lib/lunchroom/$DEVUID/console.alive ] \
    && fail "console.alive is still there after the console ended" \
    || pass "the console removed console.alive when it ended"
shred -u "$RUN"/*.pw 2>/dev/null
scan() {
    local what=$1 secret=$2 hits="" top
    # Every folder with files in it but the kernel's (/proc, /sys, /dev); a pattern read from a
    # pipe, so it is in no file and no argument list.
    for top in /bin /boot /etc /home /lib /media /mnt /opt /root /run /sbin /srv /tmp /usr /var; do
        [ -d "$top" ] || continue
        hits="$hits $(grep -rIl -D skip -F -f <(printf '%s\n' "$secret") "$top" 2>/dev/null | head -5)"
    done
    hits=$(echo $hits)
    for f in /proc/[0-9]*/environ /proc/[0-9]*/cmdline; do
        if tr '\0' '\n' < "$f" 2>/dev/null | grep -qF -f <(printf '%s\n' "$secret"); then
            hits="$hits $f"
        fi
    done
    if [ -z "$hits" ]; then
        pass "the $what is in no file, no /proc/*/environ and no /proc/*/cmdline"
    else
        fail "the $what was found in: $hits"
    fi
}
scan "Lunchroom passphrase" "$LRPW"
scan "sudo password" "$SUDOPW"

heading "result"
echo "RESULT: $PASSES PASS, $FAILS FAIL"
[ "$FAILS" -eq 0 ]
