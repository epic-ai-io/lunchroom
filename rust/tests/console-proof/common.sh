#!/bin/bash
# Shared by prove.sh (an account whose sudo asks for a password) and wide.sh (an account with
# password-free sudo): the proof of the Lunchroom 1.1 console, run as root in the container of
# `Containerfile`. Sourced, never run. It defines the counters, the account, the pseudo-terminal
# helpers, the keys and clicks that drive the console's Ratatui screen, the screen readers, and the
# guard helpers.
#
# Spec clauses (docs/spec/lunchroom-spec.md, section 17 C2): item 2 the store wall, item 3 the
# console's passphrase and 15-minute lock, item 5 the guard rules, item 8 the sudo rule, item 9
# controller approvals, item 10 the screen.

PROOF=/opt/proof
RUN=/run/proof
PROGRAM=/usr/local/lib/lunchroom/lunchroom
# LRP names the program for the code that runs as dev: the installed program, not a link.
DEVENV="HOME=/home/dev PATH=/usr/local/bin:/usr/bin:/bin PLUGIN_DIR=/home/dev/proof-plugin PROOF_CWD=/home/dev LRP=$PROGRAM"
ROWS=50
COLS=140
PASSES=0
FAILS=0

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

# Runs bash code as the user dev, with the code as the first argument and the rest as $1, $2, ...
as_dev() {
    local code=$1
    shift
    runuser -u dev -- env $DEVENV bash -c "$code" _ "$@"
}

# ---------------------------------------------------------------------------------------------
# Pseudo-terminals for the commands that ask a question (install, passphrase, doctor).
# The answers are typed from files, never given as an argument or in the environment.
AUTO=(
    --auto "New passphrase \\(at least [0-9]+ characters\\): ::$RUN/lr.pw"
    --auto "New passphrase again: ::$RUN/lr.pw"
    --auto "Current passphrase: ::$RUN/lr.pw"
    --auto "Passphrase: ::$RUN/lr.pw"
    --auto "\\[sudo\\] password for dev: ::$RUN/sudo.pw"
    --auto "Choose 1, 2 or 3 \\[2\\]: ::$RUN/enter"
    --auto "Type yes: ::$RUN/yes"
)
pty() {
    local name=$1
    shift
    python3 "$PROOF/ptyrun.py" --log "$RUN/$name.log" --timeout 300 "${AUTO[@]}" -- "$@"
}

# Makes the user dev, with a password of their own for sudo, and the secrets in files only root
# can read, in a folder that is gone when the container is. $1 is the extra groups (default none).
make_dev() {
    useradd -m -s /bin/bash ${1:+-G "$1"} dev
    DEVUID=$(id -u dev)
    mkdir -p "$RUN"
    chmod 700 "$RUN"
    umask 077
    head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/lr.pw"
    head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > "$RUN/sudo.pw"
    : > "$RUN/enter"
    echo yes > "$RUN/yes"
    echo no > "$RUN/no"
    LRPW=$(cat "$RUN/lr.pw")
    SUDOPW=$(cat "$RUN/sudo.pw")
    printf 'dev:%s\n' "$SUDOPW" | chpasswd
    umask 022
    as_dev 'mkdir -p ~/.claude ~/.local/bin && cp /opt/proof/lunchroom ~/lunchroom-dl && chmod 755 ~/lunchroom-dl'
}

# ---------------------------------------------------------------------------------------------
# The console: started as dev starts it, on a pseudo-terminal that dev owns, 140 columns by 50 rows.
CKEYS=$RUN/keys
CONSOLE_LOG=$RUN/console.log

# send_raw TEXT: types TEXT (printf %b escapes) into the console's terminal. Bounded: a write to a
# FIFO nobody reads (the console has ended) would block for ever.
send_raw() {
    timeout 5 bash -c 'printf "%b" "$1" > "$2"' _ "$1" "$CKEYS" || echo "(the console's terminal is gone: nothing was typed)" >&2
}
# press NAME: one key. Any other word is typed as the text it is.
press() {
    case "$1" in
        ENTER) send_raw '\r' ;;
        ESC) send_raw '\033' ;;
        TAB) send_raw '\t' ;;
        UP) send_raw '\033[A' ;;
        DOWN) send_raw '\033[B' ;;
        PGDN) send_raw '\033[6~' ;;
        PGUP) send_raw '\033[5~' ;;
        END) send_raw '\033[F' ;;
        HOME) send_raw '\033[H' ;;
        *) send_raw "$1" ;;
    esac
    sleep 0.5
}
# type_text TEXT: characters, no Enter.
type_text() { send_raw "$(printf '%s' "$1" | sed 's/\\/\\\\/g')"; sleep 0.4; }
# type_secret: the Lunchroom passphrase, from its file, then Enter. The passphrase is in no
# argument list: `cat` is given a path.
type_secret() {
    # Typed only into a passphrase prompt: typed into any other field, the screen would show it.
    if ! screen_has "Passphrase"; then
        echo "(no passphrase prompt on the screen: the passphrase was not typed)" >&2
        return 1
    fi
    timeout 5 sh -c 'cat "$1" > "$2"' _ "$RUN/lr.pw" "$CKEYS"
    sleep 0.3
    send_raw '\r'
    sleep 0.8
}
type_wrong() { type_text "not-the-passphrase-0000"; send_raw '\r'; sleep 0.8; }
# click COL ROW (1-based): a left click as the terminal sends it under SGR mouse reporting.
click() { send_raw "\\033[<0;$1;$2M\\033[<0;$1;$2m"; sleep 0.6; }

screen_text() { python3 "$PROOF/lastscreen.py" "$CONSOLE_LOG" "${ROWS}x${COLS}"; }
# True when the screen shows the text.
screen_has() { screen_text | grep -qF -- "$1"; }
# Waits up to $2 seconds (default 3) for the screen to show the text.
screen_shows() {
    local text=$1 limit=$(( ${2:-3} * 5 )) n=0
    while [ $n -lt $limit ]; do
        screen_has "$text" && return 0
        sleep 0.2
        n=$((n + 1))
    done
    return 1
}
# Waits up to $2 seconds (default 3) for the screen to stop showing the text.
screen_clears() {
    local text=$1 limit=$(( ${2:-3} * 5 )) n=0
    while [ $n -lt $limit ]; do
        screen_has "$text" || return 0
        sleep 0.2
        n=$((n + 1))
    done
    return 1
}
# expect DESC TEXT [SECONDS]: PASS when the screen shows TEXT within the time, else FAIL with the screen.
expect() {
    if screen_shows "$2" "${3:-3}"; then
        pass "$1"
    else
        fail "$1: the screen does not show '$2'"
        screen_text | grep -v '^\s*$' | tail -25 | sed 's/^/    | /'
    fi
}
expect_not() {
    if screen_clears "$2" "${3:-2}"; then
        pass "$1"
    else
        fail "$1: the screen still shows '$2'"
        screen_text | grep -v '^\s*$' | tail -25 | sed 's/^/    | /'
    fi
}
# cell_color TEXT: amber, red, green or dark: the colour of the alert-strip cell holding TEXT.
cell_color() { python3 "$PROOF/lastscreen.py" "$CONSOLE_LOG" "${ROWS}x${COLS}" --cell "$1"; }
# expect_cell DESC TEXT COLOUR [SECONDS]: the cell holding TEXT turns COLOUR within the time. The
# word is read as well as the colour: colour is never the only sign (item 10).
expect_cell() {
    local desc=$1 text=$2 want=$3 limit=$(( ${4:-3} * 5 )) n=0 got=""
    while [ $n -lt $limit ]; do
        got=$(cell_color "$text" 2>/dev/null)
        [ "$got" = "$want" ] && { pass "$desc: '$text' is $want"; return 0; }
        sleep 0.2
        n=$((n + 1))
    done
    fail "$desc: '$text' is '${got:-not on the screen}', not $want"
    screen_text | sed -n 1,8p | sed 's/^/    | /'
    return 1
}
# press_n NAME COUNT: the same key COUNT times in one write (a scroll).
press_n() {
    local seq="" i
    case "$1" in
        DOWN) seq='\033[B' ;;
        UP) seq='\033[A' ;;
        PGDN) seq='\033[6~' ;;
        *) seq="$1" ;;
    esac
    local all=""
    for i in $(seq 1 "$2"); do all="$all$seq"; done
    send_raw "$all"
    sleep 0.8
}
# reset_ui: two Escapes: whatever panel or question is open is closed.
reset_ui() { send_raw '\033'; sleep 0.4; send_raw '\033'; sleep 0.5; }
# confirm_asked: where the console asks "Do it? y/N", answers y and returns 0; else returns 1.
confirm_asked() {
    sleep 0.6
    if screen_has "Do it?"; then
        press y
        sleep 0.8
        return 0
    fi
    return 1
}
# open_by_arrows MARKER: Down and Enter through the items until the panel that shows MARKER opens.
open_by_arrows() {
    local marker=$1 n=0
    reset_ui
    while [ $n -lt 40 ]; do
        press DOWN
        press ENTER
        screen_has "$marker" && return 0
        press ESC
        n=$((n + 1))
    done
    return 1
}
# expect_re DESC REGEX [SECONDS]: PASS when the screen shows a line matching REGEX within the time.
expect_re() {
    local limit=$(( ${3:-3} * 5 )) n=0
    while [ $n -lt $limit ]; do
        screen_text | grep -qE -- "$2" && { pass "$1"; return 0; }
        sleep 0.2
        n=$((n + 1))
    done
    fail "$1: no line of the screen matches '$2'"
    screen_text | grep -v '^\s*$' | tail -25 | sed 's/^/    | /'
    return 1
}
# click_text TEXT: clicks on the first place the screen shows TEXT.
# Whatever panel or question was open is closed first (reset_ui), so a click always starts from
# the main screen.
click_text() {
    reset_ui
    click_on "$1"
}
# click_on TEXT: as click_text, without closing anything first (for a click inside a question).
click_on() {
    local pos
    pos=$(python3 "$PROOF/lastscreen.py" "$CONSOLE_LOG" "${ROWS}x${COLS}" --find "$1") || { echo "(no '$1' on the screen to click)" >&2; return 1; }
    set -- $pos
    click $(( $2 + 1 )) "$1"
}

# start_console: the console as dev starts it (through sudo's rule, asking dev's sudo password,
# then the Lunchroom passphrase), with its terminal owned by dev. Leaves DRIVER, CPID, CPTY, OUTER.
start_console() {
    rm -f "$CONSOLE_LOG" "$RUN/console.ready" "$CKEYS"
    mkfifo "$CKEYS"
    python3 "$PROOF/ptyrun.py" --log "$CONSOLE_LOG" --ready "$RUN/console.ready" --owner "$DEVUID" \
        --size "${ROWS}x${COLS}" --keys "$CKEYS" --raw-keys --timeout 7200 \
        --auto "\\[sudo\\] password for dev: ::$RUN/sudo.pw" \
        --auto "Lunchroom passphrase: ::$RUN/lr.pw" \
        -- runuser -u dev -- env $DEVENV sudo -u lunchroom "$PROGRAM" console &
    DRIVER=$!
    local n=0
    until screen_has "CALLS WAIT" || [ $n -ge 80 ]; do sleep 0.5; n=$((n + 1)); done
    CPID=$(pgrep -u lunchroom -x lunchroom | head -1)
    OUTER=$(sed -n 2p "$RUN/console.ready" 2>/dev/null)
    # The console's controlling terminal, from ps (root in a rootless container lacks the
    # capability to read another user's /proc/<pid>/fd).
    CPTTY=$(ps -o tty= -p "$CPID" 2>/dev/null | tr -d ' ')
    CPTY=${CPTTY:+/dev/$CPTTY}
}

# ---------------------------------------------------------------------------------------------
# The guard, fed the input an OpenClaw plugin gives it (hook.py), run as dev.
guard_event() {
    # guard_event <pretool|prompt> <session> <text> [flag]: the guard's output; its exit code is the status.
    local event=$1 session=$2 text=$3 flag=${4:-}
    as_dev 'python3 /opt/proof/hook.py "$1" "$2" "$3" $4 | "$LRP" guard "$1"' "$event" "$session" "$text" "$flag"
}
held() { guard_event prompt "$1" "hello from another agent" --untyped >/dev/null 2>&1; }
typed() { guard_event prompt "$1" "hello from the user" --typed >/dev/null 2>&1; }
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
    if [ $code -eq 0 ] && ! printf '%s' "$out" | grep -q "DENIED" && ! printf '%s' "$out" | grep -q '"permissionDecision":"ask"'; then
        pass "allowed: $desc"
    else
        fail "not allowed ($code): $desc: $command: $(printf '%s' "$out" | head -c 200)"
    fi
}
# protected_call: a PROTECTED call of a HELD session goes to the user (the channel's "ask").
protected_call() {
    local desc=$1 session=$2 command=$3 out code
    out=$(guard_event pretool "$session" "$command" 2>&1)
    code=$?
    if [ $code -eq 0 ] && printf '%s' "$out" | grep -q '"permissionDecision":"ask"'; then
        pass "PROTECTED (goes to the user): $desc"
    else
        fail "not PROTECTED ($code): $desc: $command: $(printf '%s' "$out" | head -c 200)"
    fi
}

# refused DESC CODE: CODE, run as dev, must fail.
refused() {
    local desc=$1 code=$2
    if as_dev "$code" >"$RUN/last.out" 2>&1; then
        fail "$desc: it worked: $(head -c 200 "$RUN/last.out")"
    else
        pass "$desc: refused ($(head -c 100 "$RUN/last.out" | tr '\n' ' '))"
    fi
}

# store_json: the store's state file, read as root (the container's root stands in for the setgid
# program; dev cannot read it).
store_state() { echo "/var/lib/lunchroom/$DEVUID/store/state.json"; }
# dev_status: lunchroom status, as dev, through the installed program.
dev_status() { as_dev '"$LRP" status' 2>&1; }
# held_now NAME: status lists a hold under the agent NAME (its "HELD by ..." lines). The agent's own
# line shows its state, which is STOPPED for a held agent whose session has ended (R3.3), so the
# hold is read from the lines under it.
held_now() {
    dev_status | awk -v n="$1" '$1 == n && /\t/ { f = 1; next } f && /^  / { print; next } { f = 0 }' \
        | grep -q 'HELD by'
}

# Runs a check of the whole container for a secret: every folder with files in it but the kernel's.
scan() {
    local what=$1 secret=$2 hits="" top f
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

# What the install of the console must have made (spec section 17 items 2 and 9), with the modes
# and owners the spec states. $1 is a label.
check_console_files() {
    local label=$1
    [ "$(stat -c '%a %U:%G' $PROGRAM 2>/dev/null)" = "2755 root:lunchroom" ] \
        && pass "$label: the program is $PROGRAM, root:lunchroom, mode 2755 (item 2)" \
        || fail "$label: the program is not root:lunchroom 2755: $(stat -c '%a %U:%G' $PROGRAM 2>&1)"
    [ "$(stat -c '%a %U:%G' /usr/local/lib/lunchroom)" = "755 root:root" ] \
        && pass "$label: /usr/local/lib/lunchroom is root:root, mode 755" \
        || fail "$label: /usr/local/lib/lunchroom is $(stat -c '%a %U:%G' /usr/local/lib/lunchroom)"
    [ "$(stat -c '%a %U:%G' /var/lib/lunchroom/$DEVUID 2>/dev/null)" = "2770 root:lunchroom" ] \
        && pass "$label: /var/lib/lunchroom/$DEVUID is root:lunchroom, mode 2770 (item 2)" \
        || fail "$label: the store folder is $(stat -c '%a %U:%G' /var/lib/lunchroom/$DEVUID 2>&1)"
    id -nG dev | tr ' ' '\n' | grep -qx lunchroom \
        && fail "$label: dev is in the group lunchroom" \
        || pass "$label: dev is not in the group lunchroom"
    [ "$(stat -c '%a %U:%G' /etc/lunchroom/$DEVUID.verifier 2>/dev/null)" = "640 root:lunchroom" ] \
        && pass "$label: the passphrase check is root:lunchroom, mode 640 (A1.3)" \
        || fail "$label: the passphrase check is $(stat -c '%a %U:%G' /etc/lunchroom/$DEVUID.verifier 2>&1)"
    [ "$(stat -c '%a %U:%G' /etc/sudoers.d/lunchroom 2>/dev/null)" = "440 root:root" ] \
        && pass "$label: /etc/sudoers.d/lunchroom is root:root, mode 440" \
        || fail "$label: the sudoers drop-in is $(stat -c '%a %U:%G' /etc/sudoers.d/lunchroom 2>&1)"
    visudo -cf /etc/sudoers.d/lunchroom >/dev/null 2>&1 \
        && pass "$label: visudo -cf passes the drop-in" || fail "$label: visudo -cf fails the drop-in"
    grep -q NOPASSWD /etc/sudoers.d/lunchroom \
        && fail "$label: the drop-in has NOPASSWD" || pass "$label: the drop-in asks for dev's password (no NOPASSWD)"
    # Item 9: the key pair and the delivery folders.
    [ "$(stat -c '%a %U' /etc/lunchroom/$DEVUID.console.key 2>/dev/null)" = "400 lunchroom" ] \
        && pass "$label: the private key /etc/lunchroom/$DEVUID.console.key is owned by lunchroom, mode 0400 (item 9)" \
        || fail "$label: the private key is $(stat -c '%a %U' /etc/lunchroom/$DEVUID.console.key 2>&1)"
    [ "$(stat -c '%a %U' /etc/lunchroom/$DEVUID.console.pub 2>/dev/null)" = "644 root" ] \
        && pass "$label: the public key /etc/lunchroom/$DEVUID.console.pub is root's, mode 0644 (item 9)" \
        || fail "$label: the public key is $(stat -c '%a %U' /etc/lunchroom/$DEVUID.console.pub 2>&1)"
    if [ "$(wc -l < /etc/lunchroom/$DEVUID.console.pub 2>/dev/null)" = 1 ] \
        && [ "$(base64 -d < /etc/lunchroom/$DEVUID.console.pub 2>/dev/null | wc -c)" = 32 ]; then
        pass "$label: the public key is one line of standard base64 holding 32 bytes"
    else
        fail "$label: the public key is not one base64 line of 32 bytes"
    fi
    [ "$(stat -c '%a %U:%G' /var/lib/lunchroom-approvals 2>/dev/null)" = "755 root:root" ] \
        && pass "$label: /var/lib/lunchroom-approvals is root:root, mode 0755" \
        || fail "$label: /var/lib/lunchroom-approvals is $(stat -c '%a %U:%G' /var/lib/lunchroom-approvals 2>&1)"
    [ "$(stat -c '%a %U:%G' /var/lib/lunchroom-approvals/$DEVUID 2>/dev/null)" = "2750 lunchroom:dev" ] \
        && pass "$label: the delivery folder is lunchroom:dev (dev's primary group), mode 2750" \
        || fail "$label: the delivery folder is $(stat -c '%a %U:%G' /var/lib/lunchroom-approvals/$DEVUID 2>&1)"
    [ "$(stat -c '%a %U:%G' /var/lib/lunchroom-approvals/$DEVUID/requests 2>/dev/null)" = "2750 dev:lunchroom" ] \
        && pass "$label: requests/ is dev:lunchroom, mode 2750" \
        || fail "$label: requests/ is $(stat -c '%a %U:%G' /var/lib/lunchroom-approvals/$DEVUID/requests 2>&1)"
}
