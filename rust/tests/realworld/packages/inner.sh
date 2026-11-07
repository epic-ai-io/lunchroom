#!/bin/bash
# Proof 1, inside one container: Claude Code installed from Anthropic's own apt, dnf or apk
# repository (the Containerfile of the distro), a real session of it running, and what Lunchroom
# 1.0.2 says about that session. $DISTRO is apt, dnf or apk (set by the Containerfile).
#
# The session is the real /usr/bin/claude the package installed, started on a pseudo-terminal as
# a person's terminal window starts it, with the user typing `join NAME` in it. It points at a
# server that never answers, so it waits like a session whose prompt is being worked on: no
# account, no network use beyond the installation itself.

set -u
. /opt/proof/lib.sh

heading "facts of this system ($DISTRO)"
echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "kernel: $(uname -r)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

# The package manager's own answer to "which package owns /usr/bin/claude, at which version".
case "$DISTRO" in
    apt)
        DB="dpkg"
        owner() { dpkg -S /usr/bin/claude 2>/dev/null | sed 's/: .*//'; }
        pkgver() { dpkg-query -W -f='${Version}' claude-code 2>/dev/null; }
        ;;
    dnf)
        DB="rpm"
        owner() { rpm -qf --qf '%{NAME}' /usr/bin/claude 2>/dev/null; }
        # Lunchroom asks rpm for %{NAME} and %{VERSION}; the release (-1) is a separate field.
        pkgver() { rpm -q --qf '%{VERSION}' claude-code 2>/dev/null; }
        ;;
    apk)
        DB="apk"
        owner() { apk info -W /usr/bin/claude 2>/dev/null | sed 's/.*is owned by //; s/-[0-9][0-9.]*-r[0-9]*$//'; }
        pkgver() { apk list -I claude-code 2>/dev/null | sed -n 's/^claude-code-\([^ ]*\) .*/\1/p'; }
        ;;
    *) echo "FAIL DISTRO is '$DISTRO', not apt, dnf or apk"; exit 1 ;;
esac

heading "1. Claude Code from Anthropic's $DISTRO repository"
if [ -x /usr/bin/claude ]; then
    pass "/usr/bin/claude exists and is executable"
else
    fail "/usr/bin/claude does not exist: the package did not install"
    finish
fi
OWNER=$(owner)
PKGVER=$(pkgver)
BINVER=$(/usr/bin/claude --version 2>&1 | head -1)
measured "$DB says /usr/bin/claude is owned by '$OWNER', release '$PKGVER'"
measured "the program itself says: $BINVER"
[ "$OWNER" = "claude-code" ] && pass "$DB: the package claude-code owns /usr/bin/claude" \
    || fail "$DB: /usr/bin/claude is owned by '$OWNER', not claude-code"
[ -n "$PKGVER" ] && pass "$DB records a release of claude-code: $PKGVER" \
    || fail "$DB records no release of claude-code"
[ "$DISTRO" = "dnf" ] && DNFURL="rpm" || DNFURL="$DISTRO"
grep -rqs "downloads.claude.ai/claude-code/$DNFURL/stable" /etc/apk/repositories /etc/yum.repos.d /etc/apt/sources.list.d \
    && pass "the repository file names downloads.claude.ai/claude-code/$DNFURL/stable" \
    || fail "the repository file does not name downloads.claude.ai/claude-code/$DNFURL/stable"

heading "2. Lunchroom 1.0.2 installed for a user, and a real session"
make_user
seed_claude
install_lunchroom
if [ -x "$DEVHOME/.lunchroom/bin/lunchroom" ] && grep -q '.lunchroom/bin/lunchroom guard prompt' "$DEVHOME/.claude/settings.json"; then
    pass "lunchroom install claude-code put the program and the hooks on dev's account"
else
    fail "lunchroom install claude-code did not finish (see $RUN/install.log)"
    finish
fi
if [ -e "/etc/lunchroom/$(id -u dev).verifier" ]; then
    pass "the passphrase check is set"
else
    fail "the passphrase check is not set; the program's output follows"
    python3 "$PROOF/plaintext.py" "$RUN/passphrase.log" | tail -15 | sed 's/^/    /'
fi

start_session standin /usr/bin/claude
n=0
until [ "$(exe_of "${SESSION_PID:-0}")" = "/usr/bin/claude" ] || [ $n -ge 20 ]; do
    sleep 0.5; n=$((n + 1))
done
EXE=$(exe_of "${SESSION_PID:-0}")
measured "the session is pid ${SESSION_PID:-none}; /proc/${SESSION_PID:-none}/exe -> ${EXE:-unreadable}"
[ "$EXE" = "/usr/bin/claude" ] \
    && pass "the session process runs the installed /usr/bin/claude (its /proc exe is that path, not marked deleted)" \
    || fail "the session's /proc exe is '${EXE:-unreadable}', not /usr/bin/claude"
cat "/proc/${SESSION_PID:-0}/comm" 2>/dev/null | grep -qx claude \
    && pass "the process name is claude" || fail "the process name is not claude"

heading "3. what 'lunchroom status' says"
status > "$RUN/status.out"
sed 's/^/    /' "$RUN/status.out"
LINE=$(grep "^standin	" "$RUN/status.out")
[ -n "$LINE" ] && pass "status lists the session 'standin'" || fail "status does not list the session 'standin'"
echo "$LINE" | grep -q "AVAILABLE" && pass "the session is AVAILABLE (guarded)" \
    || fail "the session is not AVAILABLE: '$LINE'"
grep -q "UNGUARDED" "$RUN/status.out" && fail "status says UNGUARDED" || pass "status does not say UNGUARDED"
grep -q "release cannot be read" "$RUN/status.out" && fail "status says the release cannot be read" \
    || pass "status gives no 'release cannot be read' reason"

heading "4. what 'lunchroom doctor' says"
doctor > "$RUN/doctor.out"
sed 's/^/    /' "$RUN/doctor.out"
grep -qF "PASS Claude Code: release $PKGVER, read from /usr/bin/claude." "$RUN/doctor.out" \
    && pass "doctor: release $PKGVER (the $DB record's version), read from /usr/bin/claude" \
    || fail "doctor does not show 'release $PKGVER, read from /usr/bin/claude.'"
grep -q "FAIL.*Claude Code" "$RUN/doctor.out" && fail "doctor has a FAIL line for Claude Code" \
    || pass "doctor has no FAIL line for Claude Code"
grep -q "UNGUARDED" "$RUN/doctor.out" && fail "doctor says UNGUARDED" || pass "doctor does not say UNGUARDED"
if [ "$DISTRO" = "apt" ] || [ "$DISTRO" = "apk" ]; then
    # The package string carries a packaging suffix (-1, -r1) that the program's own --version never
    # prints, so a release shown with it came from the package database, not from the binary.
    case "$PKGVER" in
        *-*) case "$BINVER" in *"$PKGVER"*) fail "the program's own version contains '$PKGVER'" ;;
                 *) pass "'$PKGVER' carries a packaging suffix the program never prints: doctor read it from $DB" ;; esac ;;
        *) fail "the $DB release '$PKGVER' has no packaging suffix to tell it from the program's version" ;;
    esac
fi

heading "5. control: the same program bytes, not owned by any package"
mkdir -p /opt/notpkg
cp /usr/bin/claude /opt/notpkg/claude
chmod 755 /opt/notpkg/claude
case "$DISTRO" in
    apt) dpkg -S /opt/notpkg/claude >/dev/null 2>&1 && NOTOWNED=no || NOTOWNED=yes ;;
    dnf) rpm -qf /opt/notpkg/claude >/dev/null 2>&1 && NOTOWNED=no || NOTOWNED=yes ;;
    apk) [ -n "$(apk info -W /opt/notpkg/claude 2>/dev/null | grep 'is owned by')" ] && NOTOWNED=no || NOTOWNED=yes ;;
esac
[ "$NOTOWNED" = yes ] && pass "$DB: /opt/notpkg/claude is owned by no package" \
    || fail "$DB: /opt/notpkg/claude is owned by a package (the control is void)"
cmp -s /usr/bin/claude /opt/notpkg/claude && pass "/opt/notpkg/claude is byte for byte the installed program" \
    || fail "/opt/notpkg/claude differs from the installed program"
start_session copy /opt/notpkg/claude
CEXE=$(exe_of "${SESSION_PID:-0}")
[ "$CEXE" = "/opt/notpkg/claude" ] && pass "the control session runs /opt/notpkg/claude" \
    || fail "the control session's /proc exe is '${CEXE:-unreadable}'"
status > "$RUN/status2.out"
sed 's/^/    /' "$RUN/status2.out"
grep "^copy	" "$RUN/status2.out" | grep -q UNGUARDED \
    && pass "the copy outside the package database is UNGUARDED" \
    || fail "the copy outside the package database is not UNGUARDED"
grep -q "release cannot be read from the install of its program (/opt/notpkg/claude)" "$RUN/status2.out" \
    && pass "its reason is 'release cannot be read ... (/opt/notpkg/claude)': it is the package record that makes the first session guarded" \
    || fail "the copy's reason is not 'release cannot be read ... (/opt/notpkg/claude)'"
grep "^standin	" "$RUN/status2.out" | grep -q AVAILABLE \
    && pass "the packaged session is still AVAILABLE beside it" || fail "the packaged session is no longer AVAILABLE"

finish
