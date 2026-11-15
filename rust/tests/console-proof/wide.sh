#!/bin/bash
# The proof of the Lunchroom console (1.1) on an account with password-free sudo, run as root in the
# container of `Containerfile` with WIDE set to the case:
#
#   WIDE=yes    install console stops and asks; on `yes` the rule is in force, the console is set up
#               and every attack is refused (spec section 17 item 8)
#   WIDE=no     on `no` nothing is set up and doctor prints the FAIL with the reason
#   WIDE=later  a password-free rule that sudo reads after Lunchroom's file wins: the install removes
#               its file, sets nothing up, shows that rule, and doctor prints the FAIL
#
# Item 8 (docs/spec/lunchroom-spec.md, section 17): "The proof covers both accounts: one whose `sudo`
# asks for a password (every attack refused) and one with password-free `sudo` (install console
# stops and asks; on `yes` the rule is in force, the console is set up and every attack is refused;
# on `no` nothing is set up and doctor's FAIL is shown; an agent's `sudo systemctl` runs before the
# rule, and its `sudo` naming Lunchroom's paths is DENIED)". prove.sh is the password account.
#
# It prints PASS or FAIL for each check, MEASURED for a fact it only records, and exits non-zero on
# any FAIL.

set -u
. /opt/proof/common.sh

WIDE=${WIDE:-yes}
case "$WIDE" in yes | no | later) ;; *) echo "FAIL WIDE is '$WIDE', not yes, no or later"; exit 1 ;; esac

heading "facts of this system (WIDE=$WIDE)"
echo "sudo: $(sudo -V 2>/dev/null | head -1)"
echo "kernel: $(uname -r)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

heading "1. the user dev, with password-free sudo, and Lunchroom installed (item 8: install still sets the store wall up)"
make_dev
echo 'dev ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/dev
chmod 440 /etc/sudoers.d/dev
if [ "$WIDE" = later ]; then
    # A rule in /etc/sudoers after its @includedir line wins over every file in /etc/sudoers.d
    # (item 8: this computer, read 2026-11-05: `@includedir` at line 57, the rule at line 58).
    printf '\n# the rule the proof adds after the includedir line\ndev ALL=(ALL) NOPASSWD: ALL\n' >> /etc/sudoers
    visudo -c >/dev/null 2>&1 && measured "/etc/sudoers with the later rule: $(grep -n 'includedir\|^dev' /etc/sudoers | tr '\n' ' ')" \
        || fail "/etc/sudoers does not pass visudo -c after the later rule was added"
fi
as_dev 'sudo -n true' && pass "the account's sudo runs a command without a password (the case under test)" \
    || fail "dev's sudo asks for a password: this is not the password-free account"
S=/var/lib/lunchroom/$DEVUID
# 1.2.0: install claude-code goes on to set the console up, and on this account asks the rule
# question; the proof answers no here, so step 4 asks `install console` the question afresh.
python3 "$PROOF/ptyrun.py" --log "$RUN/install1.log" --timeout 300 "${AUTO[@]}" \
    --auto "sets nothing up: ::$RUN/no" \
    -- runuser -u dev -- env $DEVENV /home/dev/lunchroom-dl install claude-code
python3 "$PROOF/plaintext.py" "$RUN/install1.log" > "$RUN/install1.txt"
[ -g "$PROGRAM" ] && [ -d "$S" ] && pass "install claude-code still set the store wall up on this account (item 8)" \
    || { fail "install claude-code did not set the wall up"; tail -25 "$RUN/install1.txt" | sed 's/^/    /'; }
grep -q "The store wall does not hold on this account" "$RUN/install1.txt" \
    && pass "the install says plainly that the wall does not hold on this account until that rule is removed" \
    || { fail "the install does not say the wall does not hold"; tail -25 "$RUN/install1.txt" | sed 's/^/    /'; }
pty doctor1 runuser -u dev -- env $DEVENV "$PROGRAM" doctor
DOCTOR1_CODE=$?
python3 "$PROOF/plaintext.py" "$RUN/doctor1.log" > "$RUN/doctor1.txt"
grep -qi "does not hold\|without a password" "$RUN/doctor1.txt" \
    && pass "doctor says so on a line (C1.4)" || fail "doctor does not say the wall does not hold"
grep -i "does not hold\|without a password" "$RUN/doctor1.txt" | grep -q '^FAIL' \
    && fail "doctor's line about the password-free rule is a FAIL: it must leave the exit code as it was" \
    || pass "doctor's line about the password-free rule is not a FAIL"
measured "doctor's exit code on this account before the console: $DOCTOR1_CODE"

heading "2. before any rule: what the account and an agent's sudo can do (item 8)"
as_dev "sudo -n cat $S/store/state.json >/dev/null" \
    && pass "the wall does not hold here: dev reads the store through sudo (stated by item 8; the install said so)" \
    || fail "dev could not read the store through sudo on the password-free account (the wall held where item 8 says it does not)"
typed AG
allowed_call "an agent's sudo systemctl runs (agents keep their own use of sudo)" AG "sudo systemctl restart some-unit"
allowed_call "an agent's sudo -u another user runs" AG "sudo -u nobody ls"
allowed_call "an agent's sudo ls runs" AG "sudo ls"
denied_call "an agent's sudo -u lunchroom ls is DENIED" AG "sudo -u lunchroom ls"
denied_call "an agent's sudo cat of the verifier is DENIED" AG "sudo cat /etc/lunchroom/$DEVUID.verifier"
denied_call "an agent's sudo rm -r /var/lib/lunchroom is DENIED" AG "sudo rm -r /var/lib/lunchroom"
denied_call "an agent's command naming the rule file of item 8 is DENIED" AG "cat /etc/sudoers.d/zz-lunchroom-$DEVUID"

heading "3. a yes typed by a process with no controlling terminal confirms nothing (A1.3, item 8)"
as_dev 'setsid -w sh -c "printf \"yes\n\" | \"$LRP\" install console" ' > "$RUN/noterm.out" 2>&1
NOTERM=$?
measured "install console with no terminal and 'yes' on its input: exit $NOTERM: $(head -c 300 "$RUN/noterm.out" | tr '\n' ' ')"
[ "$NOTERM" -ne 0 ] && pass "install console with no controlling terminal failed (exit $NOTERM)" || fail "install console with no controlling terminal exited 0"
ls /etc/sudoers.d/zz-lunchroom-* >/dev/null 2>&1 \
    && fail "a rule file was written for a yes typed by a process with no terminal" \
    || pass "no rule file was written for it"
[ ! -e /etc/lunchroom/$DEVUID.console.pub ] && pass "nothing of the console was set up for it" || fail "the console's public key exists"

heading "4. lunchroom install console: it stops, says the wall does not hold, and asks (item 8)"
if [ "$WIDE" = no ]; then ANSWER="$RUN/no"; else ANSWER="$RUN/yes"; fi
python3 "$PROOF/ptyrun.py" --log "$RUN/install2.log" --timeout 300 "${AUTO[@]}" \
    --auto "sets nothing up: ::$ANSWER" \
    -- runuser -u dev -- env $DEVENV "$PROGRAM" install console
INSTALL2=$?
python3 "$PROOF/plaintext.py" "$RUN/install2.log" > "$RUN/install2.txt"
grep -q "the console's wall does not hold on this account" "$RUN/install2.txt" \
    && pass "install console stopped before it set anything up and said the console's wall does not hold on this account" \
    || { fail "install console did not stop and say so"; tail -30 "$RUN/install2.txt" | sed 's/^/    /'; }
grep -q "Type yes to write the rule" "$RUN/install2.txt" \
    && pass "it asked whether to make sudo ask this account for its password" || fail "it did not ask"
RULE=/etc/sudoers.d/zz-lunchroom-$DEVUID

case "$WIDE" in
yes)
    [ "$INSTALL2" -eq 0 ] && pass "on yes the install finished (exit 0)" || fail "on yes the install exited $INSTALL2"
    [ "$(stat -c '%U:%G %a' $RULE 2>/dev/null)" = "root:root 440" ] \
        && pass "$RULE is root's with mode 0440" || fail "$RULE is $(stat -c '%U:%G %a' $RULE 2>&1)"
    [ "$(cat $RULE 2>/dev/null)" = "#$DEVUID ALL=(ALL:ALL) PASSWD: ALL" ] \
        && pass "the file holds the one rule '#$DEVUID ALL=(ALL:ALL) PASSWD: ALL'" \
        || fail "the file holds: $(cat $RULE 2>&1 | head -c 200)"
    visudo -c -f "$RULE" >/dev/null 2>&1 && pass "visudo -c -f passes the rule file" || fail "visudo -c -f fails the rule file"
    visudo -c >/dev/null 2>&1 && pass "visudo -c passes the whole configuration" || fail "visudo -c fails the whole configuration"
    ls /etc/sudoers.d | grep -F '.' | grep -q 'lunchroom' \
        && fail "a temporary file with a dot is left in /etc/sudoers.d: $(ls /etc/sudoers.d | grep -F '.' | tr '\n' ' ')" \
        || pass "no temporary file of the install is left in /etc/sudoers.d"
    as_dev 'sudo -k; sudo -n true' >"$RUN/sudo-n.out" 2>&1 \
        && fail "after sudo -k, sudo -n true still runs without a password: the rule is not in force" \
        || pass "after sudo -k, sudo -n true is refused: the rule is in force ($(head -c 80 "$RUN/sudo-n.out" | tr '\n' ' '))"
    check_console_files "wide account, after yes"
    pty doctor2 runuser -u dev -- env $DEVENV "$PROGRAM" doctor
    python3 "$PROOF/plaintext.py" "$RUN/doctor2.log" > "$RUN/doctor2.txt"
    grep -q "PASS.*console" "$RUN/doctor2.txt" && pass "doctor reports the console set up" \
        || { fail "doctor does not report the console set up"; tail -25 "$RUN/doctor2.txt" | sed 's/^/    /'; }
    # Every attack of the password account is refused now. The console runs, and dev cannot reach it.
    as_dev 'mkdir -p ~/proof-plugin'
    start_console
    screen_has "CALLS WAIT" && pass "the console asked for the passphrase and drew its screen" || fail "the console did not draw its screen"
    if [ -n "$CPID" ] && [ -n "$CPTY" ]; then
        refused "dev cannot read the store through sudo (the rule asks the password)" "sudo -n cat $S/store/state.json"
        refused "dev cannot read the store" "cat $S/store/state.json"
        refused "dev cannot list the store folder" "ls $S"
        refused "dev cannot read the passphrase check" "cat /etc/lunchroom/$DEVUID.verifier"
        refused "dev cannot read the console's private key" "cat /etc/lunchroom/$DEVUID.console.key"
        refused "dev cannot write the delivery folder" "touch /var/lib/lunchroom-approvals/$DEVUID/planted"
        refused "dev cannot kill the console" "kill -9 $CPID"
        refused "dev cannot signal the console (kill -0)" "kill -0 $CPID"
        refused "dev cannot read the console's environment" "cat /proc/$CPID/environ"
        refused "dev cannot ptrace-attach to the console" \
            "python3 -c 'import ctypes,sys; l=ctypes.CDLL(None,use_errno=True); sys.exit(0 if l.ptrace(16,$CPID,0,0)==0 else 1)'"
        refused "dev cannot read the console's terminal" "timeout 2 head -c 1 < $CPTY"
        refused "dev cannot write the console's terminal" "echo x > $CPTY"
        refused "dev cannot become lunchroom with sudo" "sudo -n -u lunchroom id"
        refused "dev cannot become root with sudo without the password" "sudo -n id"
        refused "an agent's process of dev cannot use sudo without the password (K1.10)" "sudo -n true"
        kill -0 "$CPID" 2>/dev/null && pass "the console is still running after every attack" || fail "the console died"
    else
        fail "no running console to attack (pid '$CPID', terminal '$CPTY')"
    fi
    press q
    wait "$DRIVER" 2>/dev/null
    # uninstall leaves the rule file in place and names it; the public key goes first.
    pty uninstall runuser -u dev -- env $DEVENV "$PROGRAM" uninstall console
    python3 "$PROOF/plaintext.py" "$RUN/uninstall.log" > "$RUN/uninstall.txt"
    [ -f "$RULE" ] && pass "lunchroom uninstall left the rule file in place" || fail "lunchroom uninstall removed the rule file"
    grep -qF "zz-lunchroom-$DEVUID" "$RUN/uninstall.txt" \
        && pass "lunchroom uninstall names the rule file" || fail "lunchroom uninstall does not name the rule file"
    [ ! -e /etc/lunchroom/$DEVUID.console.pub ] \
        && pass "the public key (the controller's signal that the console is installed) is gone after uninstall (item 9)" \
        || fail "the public key is still there after uninstall"
    ;;
no)
    [ "$INSTALL2" -ne 0 ] && pass "on no the install did not finish as a success (exit $INSTALL2)" || fail "on no the install exited 0"
    ls /etc/sudoers.d/zz-lunchroom-* >/dev/null 2>&1 && fail "on no a rule file was written" || pass "on no no rule file was written"
    [ ! -e /etc/lunchroom/$DEVUID.console.pub ] && [ ! -e /etc/lunchroom/$DEVUID.console.key ] \
        && pass "on no no key pair was made" || fail "on no a key exists"
    [ ! -e /etc/sudoers.d/lunchroom ] && pass "on no the console's own sudoers file was not written" || fail "on no /etc/sudoers.d/lunchroom was written"
    [ ! -e /var/lib/lunchroom-approvals/$DEVUID ] && pass "on no no delivery folder was made" || fail "on no a delivery folder exists"
    pty doctor2 runuser -u dev -- env $DEVENV "$PROGRAM" doctor
    DOCTOR2_CODE=$?
    python3 "$PROOF/plaintext.py" "$RUN/doctor2.log" > "$RUN/doctor2.txt"
    sed 's/^/    doctor: /' "$RUN/doctor2.txt"
    grep -i 'console' "$RUN/doctor2.txt" | grep -q '^FAIL' \
        && pass "doctor prints the console's FAIL (C1.4)" || fail "doctor does not print a FAIL for the console"
    grep -qi "was not yes" "$RUN/doctor2.txt" \
        && pass "the FAIL gives the reason: the answer was not yes" || fail "the FAIL does not give the reason"
    [ "$DOCTOR2_CODE" -eq 1 ] && pass "doctor exits 1 with that FAIL" || fail "doctor exits $DOCTOR2_CODE, not 1"
    ;;
later)
    [ "$INSTALL2" -ne 0 ] && pass "with a later rule the install did not finish as a success (exit $INSTALL2)" || fail "the install exited 0 with a later rule"
    ls /etc/sudoers.d/zz-lunchroom-* >/dev/null 2>&1 \
        && fail "the install left its rule file though a later rule wins" \
        || pass "the install removed its own file: a rule sudo reads later still wins"
    [ ! -e /etc/lunchroom/$DEVUID.console.pub ] && [ ! -e /etc/lunchroom/$DEVUID.console.key ] \
        && pass "nothing of the console was set up" || fail "a console key exists"
    grep -q "NOPASSWD" "$RUN/install2.txt" \
        && pass "the install shows the rule that wins, as sudo -n -l prints it" || fail "the install does not show the later rule"
    grep -q "/etc/sudoers" "$RUN/install2.txt" \
        && pass "the install says the change is the user's own to make in /etc/sudoers" || fail "the install does not name /etc/sudoers"
    pty doctor2 runuser -u dev -- env $DEVENV "$PROGRAM" doctor
    python3 "$PROOF/plaintext.py" "$RUN/doctor2.log" > "$RUN/doctor2.txt"
    grep -i 'console' "$RUN/doctor2.txt" | grep -q '^FAIL' \
        && pass "doctor prints the console's FAIL (C1.4)" || fail "doctor does not print a FAIL for the console"
    ;;
esac
shred -u "$RUN"/*.pw 2>/dev/null

finish
