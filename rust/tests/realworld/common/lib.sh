#!/bin/bash
# Shared by the real-world proofs of Lunchroom 1.0.2. Sourced as root inside a container.
#
# It prints PASS or FAIL for a check, MEASURED for a fact it only records; `finish` prints the
# counts and exits non-zero on any FAIL. Every check that counts a refusal or an absence proves
# first that its target exists.

PASSES=0
FAILS=0
PROOF=/opt/proof
RUN=/run/proof
SESS=/run/sess
DEVHOME=/home/dev

pass() { echo "PASS $*"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL $*"; FAILS=$((FAILS + 1)); }
measured() { echo "MEASURED $*"; }
heading() { echo; echo "== $*"; }
finish() {
    heading "result"
    echo "RESULT: $PASSES PASS, $FAILS FAIL"
    [ "$FAILS" -eq 0 ]
    exit $?
}

# The words that run a command as the user dev, with a clean environment (runuser where util-linux
# has it, su-exec on Alpine).
if command -v runuser >/dev/null 2>&1; then
    DEV=(runuser -u dev -- env HOME=$DEVHOME TERM=xterm-256color PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin)
else
    DEV=(su-exec dev env HOME=$DEVHOME TERM=xterm-256color PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin)
fi
as_dev() { "${DEV[@]}" "$@"; }

# Makes the user dev, who may use sudo with a password of their own (kept in a file only root can
# read, in a folder that is gone when the container is).
make_user() {
    mkdir -p "$RUN"
    chmod 755 "$RUN"
    if command -v useradd >/dev/null 2>&1; then
        useradd -m -s /bin/bash dev
    else
        adduser -D -s /bin/bash dev
    fi
    umask 077
    head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/lr.pw"
    head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/sudo.pw"
    : > "$RUN/enter"
    echo yes > "$RUN/yes"
    printf 'dev:%s\n' "$(cat "$RUN/sudo.pw")" | chpasswd || echo "chpasswd failed: $?"
    umask 022
    # The password must be usable for sudo: check what was stored (the hash's scheme, never the hash).
    echo "MEASURED dev's password: $(passwd -S dev 2>&1 | cut -d' ' -f1-2); hash scheme $(grep '^dev:' /etc/shadow | cut -d: -f2 | cut -c1-3)"
    mkdir -p /etc/sudoers.d
    if [ "${SUDO_NOPASSWD:-0}" = 1 ]; then
        # Measured on Fedora in a rootless container: sudo refuses a correct password ("Sorry, try
        # again", with a yescrypt or a SHA-512 hash alike), so there dev's rule is NOPASSWD. The
        # proofs that use it are about the package database, not about sudo's password.
        echo "MEASURED sudo rule for dev: NOPASSWD (sudo's password check fails in this container)"
        echo 'dev ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/dev
    else
        echo 'dev ALL=(ALL) ALL' > /etc/sudoers.d/dev
    fi
    chmod 440 /etc/sudoers.d/dev
}

# Runs a command on a pseudo-terminal, typing the answers to Lunchroom's prompts from files.
pty() {
    local name=$1
    shift
    python3 "$PROOF/ptyrun.py" --log "$RUN/$name.log" --timeout 120 \
        --auto "Type yes: ::$RUN/yes" \
        --auto "Choose 1, 2 or 3 \\[2\\]: ::$RUN/enter" \
        --auto "New passphrase \\(at least [0-9]+ characters\\): ::$RUN/lr.pw" \
        --auto "New passphrase again: ::$RUN/lr.pw" \
        --auto "Passphrase: ::$RUN/lr.pw" \
        --auto "\\[sudo\\] password for dev: ::$RUN/sudo.pw" \
        -- "$@" >/dev/null 2>&1
}

# Puts Lunchroom 1.0.2 (the static program in $PROOF) on dev's account for Claude Code, then sets
# the passphrase, through the program's own prompts. Both run on a pseudo-terminal, as the program
# requires; the answers are typed from files.
install_lunchroom() {
    local harness=${1:-claude-code}
    cp "$PROOF/lunchroom" "$DEVHOME/lunchroom-dl"
    chmod 755 "$DEVHOME/lunchroom-dl"
    chown dev "$DEVHOME/lunchroom-dl"
    pty install "${DEV[@]}" "$DEVHOME/lunchroom-dl" install "$harness"
    # With sudo present the install sets the passphrase itself; otherwise it is set here.
    [ -e "/etc/lunchroom/$(id -u dev).verifier" ] \
        || pty passphrase "${DEV[@]}" "$DEVHOME/.lunchroom/bin/lunchroom" passphrase
}

# Claude Code is told to use a key and a server that never answers, and its first-run questions are
# answered in its own configuration file, as a person who had answered them once would have it.
seed_claude() {
    cat > "$DEVHOME/.claude.json" <<'J'
{"hasCompletedOnboarding": true, "theme": "dark", "numStartups": 5,
 "customApiKeyResponses": {"approved": ["ke-key-for-the-proof"], "rejected": []},
 "projects": {"/home/dev": {"hasTrustDialogAccepted": true, "allowedTools": []}}}
J
    chown dev "$DEVHOME/.claude.json"
    export ANTHROPIC_BASE_URL=http://127.0.0.1:8099
    # A stand-in key for the mock API (short, so it is not credential-shaped).
    export ANTHROPIC_API_KEY=sk-ant-mock
    mkdir -p "$SESS"
    chmod 777 "$SESS"
    if [ "${MOCK_API:-0}" = 1 ]; then
        # mockapi.py answers the real program's requests, so its hooks run on a real tool call.
        pgrep -f "$PROOF/mockapi.py" >/dev/null || { MOCK_LOG="$SESS/mock.log" python3 "$PROOF/mockapi.py" & sleep 1; }
    else
        pgrep -f "$PROOF/hang.py" >/dev/null || { python3 "$PROOF/hang.py" & sleep 1; }
    fi
}

# start_session NAME PROGRAM: starts PROGRAM (Claude Code) on a pseudo-terminal of its own as dev,
# waits for it to draw its prompt, types `join NAME`, and waits until Lunchroom lists it. The
# program's pid is left in SESSION_PID.
start_session() {
    local name=$1 program=$2
    shift 2
    mkdir -p "$SESS"
    chmod 777 "$SESS"
    rm -f "$SESS/$name.keys" "$SESS/$name.ready" "$SESS/$name.log"
    mkfifo "$SESS/$name.keys"
    chmod 666 "$SESS/$name.keys"
    (
        cd "$DEVHOME" || exit 1
        as_dev ANTHROPIC_BASE_URL="$ANTHROPIC_BASE_URL" ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
            python3 "$PROOF/sessionpty.py" --log "$SESS/$name.log" --keys "$SESS/$name.keys" \
            --ready "$SESS/$name.ready" --timeout 900 -- "$program" "$@" >/dev/null 2>&1
    ) &
    local n=0
    until grep -q "include this session" "$SESS/$name.log" 2>/dev/null || [ $n -ge 60 ]; do
        sleep 0.5
        n=$((n + 1))
    done
    SESSION_PID=$(sed -n 1p "$SESS/$name.ready" 2>/dev/null)
    echo "join $name" > "$SESS/$name.keys"
    n=0
    until status | grep -q "^$name	"; do
        sleep 0.5
        n=$((n + 1))
        [ $n -ge 40 ] && break
    done
}

# probe SESSION KIND LABEL: types "KIND LABEL" (KIND is DENYTEST or CONTROLTEST) into the session's
# terminal; mockapi.py answers with a Bash tool call, which Claude Code's own hooks judge; waits up
# to 90 s for the result Claude Code gave back, and prints it on one line (empty if none came).
probe() {
    local session=$1 kind=$2 label=$3
    # A write to a FIFO nobody reads would block for ever (the session may have ended): bounded.
    timeout 10 sh -c 'echo "$1 $2" > "$3"' _ "$kind" "$label" "$SESS/$session.keys" || echo "(the session's terminal is gone: nothing was typed)" >&2
    await_result "$kind" "$label"
}

# await_result KIND LABEL: waits up to 90 s for the mock server's record of the result of the tool call
# the prompt "KIND LABEL" led to, and prints it on one line (empty if none came).
await_result() {
    local kind=$1 label=$2 n=0 out=""
    while [ $n -lt 180 ]; do
        out=$(python3 - "$SESS/mock.log" "$kind" "$label" <<'PY'
import json, sys
try:
    for line in open(sys.argv[1]):
        r = json.loads(line)
        if r["tool"] == sys.argv[2] and r["label"] == sys.argv[3]:
            print(" ".join(r["result"].split()))
            break
except OSError:
    pass
PY
)
        [ -n "$out" ] && break
        sleep 0.5
        n=$((n + 1))
    done
    echo "$out"
}

# exe_of PID: where /proc/PID/exe points, read as dev (root in a rootless container may not read
# another user's /proc/<pid>/exe).
exe_of() { as_dev readlink "/proc/$1/exe" 2>/dev/null; }

# status: Lunchroom's status as dev (it needs no terminal).
status() { as_dev "$DEVHOME/.lunchroom/bin/lunchroom" status 2>&1; }

# doctor: Lunchroom's doctor as dev, on a pseudo-terminal (it runs only for a user at one).
doctor() {
    rm -f "$RUN/doctor.log"
    pty doctor "${DEV[@]}" "$DEVHOME/.lunchroom/bin/lunchroom" doctor
    python3 "$PROOF/plaintext.py" "$RUN/doctor.log"
}
