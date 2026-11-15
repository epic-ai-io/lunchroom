#!/bin/bash
# Proof 17 (echokill), inside the container: Lunchroom 1.1 is installed for dev (so dev has a
# passphrase), then `lunchroom passphrase` is run on a pseudo-terminal and killed at its prompt.
#
# Spec section 7 A1.3 (docs/spec/lunchroom-spec.md): "Echo is turned off by the program itself
# (`tcgetattr` and `tcsetattr` on the controlling terminal), and turned back on before it exits, also
# when it is interrupted." A process killed by SIGKILL runs no code of its own, so the 1.1 build
# starts a watchdog process that restores the terminal when the program ends (GAPS addendum 2, lead,
# 2026-11-05: "fixable with a watchdog process and a hidden subcommand"). The proof holds the
# pseudo-terminal's slave open itself (echokill.py), so the terminal's settings outlive the program,
# and reads the ECHO flag at the prompt and after each signal.
#
# Control: the flag must be OFF at the prompt, else the proof proves nothing.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
install_lunchroom
# 1.1, spec section 17 item 2: the install is finished when the setgid program and /var/lib/lunchroom/<uid> are in place.
lr_installed && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }
[ -e "/etc/lunchroom/$(id -u dev).verifier" ] && pass "dev has a passphrase (the program asks for the current one at its prompt)" \
    || { fail "no passphrase is set"; finish; }
DEVUID=$(id -u dev)

# case SIGNAL: `lunchroom passphrase` as dev on a terminal this proof owns, signalled at the prompt.
case_run() {
    local sig=$1 out
    out=$(python3 /opt/proof/echokill.py --signal "$sig" --target passphrase --owner "$DEVUID" --wait 6 -- \
        runuser -u dev -- env HOME=$DEVHOME TERM=xterm-256color PATH=/usr/local/bin:/usr/bin:/bin "$LR" passphrase 2>"$RUN/echokill-$sig.err")
    local code=$?
    echo "$out" | sed "s/^/MEASURED SIG$sig: /"
    [ -s "$RUN/echokill-$sig.err" ] && sed "s/^/    stderr: /" "$RUN/echokill-$sig.err" | head -5
    if [ $code -ne 0 ]; then
        fail "SIG$sig: the proof could not run to the end (exit $code)"
        return
    fi
    echo "$out" | grep -q '^ECHO_BEFORE=off' \
        && pass "SIG$sig control: echo is OFF at the passphrase prompt (the proof is not hollow)" \
        || { fail "SIG$sig control: echo is not off at the prompt: $(echo "$out" | grep ECHO_BEFORE)"; return; }
    echo "$out" | grep -q '^ECHO_AFTER=on' \
        && pass "SIG$sig: the terminal's echo is on again after the program was signalled ($(echo "$out" | grep ECHO_AFTER))" \
        || fail "SIG$sig: the terminal's echo stayed OFF after the program was signalled: $(echo "$out" | grep ECHO_AFTER)"
    REMAIN=$(echo "$out" | sed -n 's/^REMAINING=//p')
    [ -z "$REMAIN" ] && pass "SIG$sig: no watchdog or other lunchroom process is left running" \
        || fail "SIG$sig: lunchroom processes were left running: $REMAIN"
}

heading "1. SIGKILL at the passphrase prompt (the case no handler in the program can catch)"
case_run KILL
heading "2. SIGTERM at the passphrase prompt"
case_run TERM
heading "3. SIGHUP at the passphrase prompt"
case_run HUP

finish
