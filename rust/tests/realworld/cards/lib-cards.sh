#!/bin/bash
# Shared by the six card kits, inside the container, as root. Sourced after /opt/proof/lib.sh (which
# gives pass, fail, measured, heading, finish, make_user, install_lunchroom, pty, status, as_dev,
# $LR, $DEVHOME, $RUN, $SESS) and never run.
#
# What a card kit does: it starts live sessions of the four harnesses (Claude Code, Codex, OpenClaw,
# Hermes) as the named agents of a card, makes each agent run the shell commands the card gives it
# (the harness is real, its hooks are real, Lunchroom is the installed program; only the model is
# scripted, by cardmock.py), and checks that every hearer of every Lunchroom line has the line by the
# channel the spec names (S: the output of `send`; D: a notice the harness tells; N: the envelope;
# U: the user's list, the user's terminal, the store and the console).
#
# Harness codes: cc Claude Code, cx Codex, oc OpenClaw, hm Hermes.
# Sessions are named by `join <Name>` with the card's names; the label of a model turn is
# `<name in lower case>-<n>` (cardmock.py).

export SESS

# A step whose outcome the spec does not fix for a harness is run with SOFT=1: what would be a PASS or a
# FAIL is printed as MEASURED and counts for neither, so the record shows it without deciding it.
SOFT=0
pass() {
    if [ "$SOFT" = 1 ]; then echo "MEASURED (would pass) $*"; else echo "PASS $*"; PASSES=$((PASSES + 1)); fi
}
fail() {
    if [ "$SOFT" = 1 ]; then echo "MEASURED (would fail) $*"; else echo "FAIL $*"; FAILS=$((FAILS + 1)); fi
}
TOOL=(python3 -I "$PROOF/cardtool.py")
NAMES_ALL="cc cx oc hm"
WITNESS=/var/tmp/witness
DEVUID=""
CELL=0

lc() { printf '%s' "$1" | tr 'A-Z' 'a-z'; }
now() { date +%s.%N; }
uid_dev() { [ -n "$DEVUID" ] || DEVUID=$(id -u dev); echo "$DEVUID"; }
harness_of() { cat "$SESS/h-$(sid "$1")" 2>/dev/null; }
harness_word() {
    case "$1" in cc) echo "Claude Code" ;; cx) echo Codex ;; oc) echo OpenClaw ;; hm) echo Hermes ;; *) echo "$1" ;; esac
}
# minutes_of T0 T1: the UTC minutes (HH:MM,HH:MM) of the two instants.
minutes_of() {
    local a b
    a=$("${TOOL[@]}" minute "$1")
    b=$("${TOOL[@]}" minute "$2")
    [ "$a" = "$b" ] && echo "$a" || echo "$a,$b"
}

# ---------------------------------------------------------------------------------------------
# One-time setup of the container: the user, the model, all four harnesses, Lunchroom installed for
# each by its own installer, the console set up. Prints the version of each harness.
setup_world() {
    heading "facts of this system"
    echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"
    echo "claude: $(claude --version 2>&1 | head -1)"
    echo "codex: $(codex --version 2>&1 | head -1)"
    echo "openclaw: $(openclaw --version 2>&1 | head -1)"
    echo "hermes: $(hermes --version 2>&1 | head -1)"
    echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"
    check_versions

    make_user
    mkdir -p "$SESS" "$WITNESS"
    chmod 777 "$SESS" "$WITNESS"
    : > "$SESS/mock.log"
    : > "$SESS/model.jsonl"
    : > "$SESS/agents.jsonl"
    chmod 666 "$SESS/mock.log" "$SESS/model.jsonl" "$SESS/agents.jsonl"
    SESS="$SESS" python3 -I "$PROOF/cardmock.py" &
    sleep 1

    seed_cards_claude
    mkdir -p "$DEVHOME/.codex"
    cat > "$DEVHOME/.codex/config.toml" <<'T'
model = "gpt-5.5"
model_provider = "mock"
approval_policy = "never"
# Codex's own sandbox cannot start inside a rootless container, so the container is the sandbox.
sandbox_mode = "danger-full-access"

[model_providers.mock]
name = "mock"
base_url = "http://127.0.0.1:8099/v1"
wire_api = "responses"
requires_openai_auth = false
T
    chown -R dev "$DEVHOME/.codex"

    local h
    for h in claude-code codex openclaw hermes; do
        install_lunchroom "$h"
        lr_installed && pass "setup: lunchroom install $h finished" || { fail "setup: lunchroom install $h did not finish"; finish; }
    done
    # The installer, not this kit, wrote each harness's hooks: the kit names no hook of its own.
    grep -q "$LR" "$DEVHOME/.claude/settings.json" && pass "setup: the Claude Code hooks run $LR (written by the installer)" \
        || fail "setup: the Claude Code settings do not run $LR"
    grep -q 'web_search *= *"disabled"' "$DEVHOME/.codex/config.toml" && pass "setup: Codex web_search is disabled by the installer (H1.4)" \
        || fail "setup: config.toml holds no web_search = \"disabled\""
    grep -q 'unified_exec_tty *= *false' "$DEVHOME/.codex/config.toml" \
        && pass "setup: Codex unified_exec_tty = false by the installer (H1.4)" \
        || measured "setup: config.toml holds no unified_exec_tty = false (the installer of this build writes none, or it is elsewhere)"

    setup_openclaw
    setup_hermes
    lr_default_mode_check
}

check_versions() {
    local v
    v=$(claude --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
    [ "$(printf '%s\n%s\n' 2.1.309 "$v" | sort -V | head -1)" = 2.1.309 ] \
        && pass "version: Claude Code $v is 2.1.309 or later" || fail "version: Claude Code ${v:-unreadable} is older than 2.1.309"
    v=$(codex --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
    [ "$(printf '%s\n%s\n' 0.163.0 "$v" | sort -V | head -1)" = 0.163.0 ] \
        && pass "version: Codex $v is 0.163.0 or later" || fail "version: Codex ${v:-unreadable} is older than 0.163.0"
    openclaw --version 2>&1 | grep -q '^OpenClaw 2026\.10\.7 ' \
        && pass "version: OpenClaw is 2026.10.7" || fail "version: OpenClaw is not 2026.10.7"
    [ "$(git -C /opt/hermes-agent rev-parse HEAD 2>/dev/null)" = 9976f4857b72439d79016354d7b6e1a3ee952e2f ] \
        && pass "version: Hermes is at commit 9976f48" || fail "version: Hermes is not at commit 9976f48"
}

# Claude Code is told to use the model of cardmock.py, and its first-run questions are answered as
# a person who had answered them once would have it.
seed_cards_claude() {
    cat > "$DEVHOME/.claude.json" <<'J'
{"hasCompletedOnboarding": true, "theme": "dark", "numStartups": 5,
 "customApiKeyResponses": {"approved": ["sk-ant-mock"], "rejected": []},
 "projects": {"/home/dev": {"hasTrustDialogAccepted": true, "allowedTools": []}}}
J
    chown dev "$DEVHOME/.claude.json"
}

setup_openclaw() {
    local OC=(as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token openclaw)
    "${OC[@]}" config set models.providers.mock '{"baseUrl":"http://127.0.0.1:8099/v1","apiKey":"sk-mock","api":"openai-completions","models":[{"id":"mock-1","name":"Mock 1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":128000,"maxTokens":4096}]}' --strict-json >/dev/null 2>&1
    "${OC[@]}" config set agents.defaults.model.primary mock/mock-1 >/dev/null 2>&1
    "${OC[@]}" config set gateway.mode local >/dev/null 2>&1
    echo y | "${OC[@]}" plugins install --link "$DEVHOME/.lunchroom/plugins/openclaw" --force --accept-capabilities > "$RUN/plugin-install.out" 2>&1
    "${OC[@]}" plugins enable lunchroom > "$RUN/plugin-enable.out" 2>&1
    "${OC[@]}" config set plugins.entries.lunchroom.hooks.allowConversationAccess true --strict-json >/dev/null 2>&1
    grep -q "Enabled plugin \"lunchroom\"" "$RUN/plugin-enable.out" \
        && pass "setup: OpenClaw enabled the Lunchroom plugin with allowConversationAccess" \
        || { fail "setup: OpenClaw did not enable the plugin"; head -10 "$RUN/plugin-install.out" "$RUN/plugin-enable.out"; }
    (cd "$DEVHOME" && "${OC[@]}" gateway run --port 18789 --bind loopback > "$SESS/gateway.log" 2>&1 &)
    local n=0
    until (exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null || [ $n -ge 120 ]; do sleep 1; n=$((n + 1)); done
    (exec 3<>/dev/tcp/127.0.0.1/18789) 2>/dev/null && pass "setup: the OpenClaw gateway listens (after $n s)" \
        || { fail "setup: the OpenClaw gateway did not start"; tail -10 "$SESS/gateway.log"; }
    sleep 5
}

setup_hermes() {
    as_dev hermes plugins enable lunchroom > "$RUN/hm-enable.out" 2>&1
    as_dev hermes plugins list 2>&1 | grep -i lunchroom | grep -qi enabled \
        && pass "setup: Hermes lists the Lunchroom plugin as enabled" || fail "setup: Hermes does not list the plugin as enabled"
    as_dev hermes config set model.provider custom > "$RUN/hconfig.out" 2>&1
    as_dev hermes config set model.base_url http://127.0.0.1:8099/v1 >> "$RUN/hconfig.out" 2>&1
    as_dev hermes config set model.default mock-1 >> "$RUN/hconfig.out" 2>&1
}

# Item 3 of section 16: each card is played in the Balanced hold mode, with no `lunchroom mode` typed.
lr_default_mode_check() {
    local m
    m=$(status | grep -i -m1 'mode' )
    measured "setup: status names the hold mode: ${m:-no line with 'mode' in status}"
    if [ -n "$m" ]; then
        echo "$m" | grep -qi balanced && pass "setup: the default hold mode is Balanced, with no 'lunchroom mode' typed (item 3)" \
            || fail "setup: the default hold mode is not Balanced: $m"
    fi
}

# ---------------------------------------------------------------------------------------------
# Starting and ending sessions.

# reset_world: every session ends, the store is emptied (as root, standing in for the setgid program),
# the fake programs' witnesses are cleared. A cell of a card starts from this state.
reset_world() {
    pkill -u dev -f sessionpty.py 2>/dev/null
    pkill -u dev -x claude 2>/dev/null
    pkill -u dev -f 'codex' 2>/dev/null
    pkill -u dev -f 'hermes' 2>/dev/null
    sleep 2
    local store
    store="$STOREROOT/$(uid_dev)/store"
    if [ -d "$store" ]; then
        find "$store" -mindepth 1 -delete 2>/dev/null
    fi
    rm -f "$WITNESS"/* "$SESS"/h-* "$SESS"/sid-* "$SESS"/count-* "$SESS"/cmd-* "$SESS"/*.keys "$SESS"/*.ready 2>/dev/null
    : > "$SESS/mock.log"
    : > "$SESS/model.jsonl"
    : > "$SESS/agents.jsonl"
    CELL=$((CELL + 1))
    echo "$CELL" > "$SESS/generation"
    if status | grep -qE '^[A-Za-z]'; then
        local left
        left=$(status | grep -cE "AVAILABLE|HELD|UNGUARDED|STOPPED")
        [ "$left" -eq 0 ] && measured "reset: a clean store (status lists no agent)" \
            || fail "reset: status still lists $left agent(s) after the store was emptied"
    fi
}

# sid NAME: the label prefix and file stem of a session: the name in lower case, unless the session was
# started with a prefix of its own (a session that starts anew under the same name has its own).
sid() {
    if [ -f "$SESS/sid-$(lc "$1")" ]; then cat "$SESS/sid-$(lc "$1")"; else lc "$1"; fi
}

# live_check HARNESS NAME: the session is live: the harness's own program runs as the user, and the requests
# the model got came from that harness's own client (its User-Agent). A scripted model behind a stand-in
# client would fail this. Claude Code and Codex are decided; OpenClaw and Hermes are recorded.
live_check() {
    local h=$1 name=$2 s ua
    s=$(sid "$name")
    ua=$("${TOOL[@]}" ua "$s" 2>/dev/null | tr '\n' ' ')
    case "$h" in
        cc)
            pgrep -u dev -x claude >/dev/null && pass "live: $name: the claude program runs as the user" || fail "live: $name: no claude process runs as the user"
            case "$ua" in *claude-cli*|*claude-code*|*Claude*) pass "live: $name: the model's requests came from Claude Code's own client ($ua)" ;;
                *) fail "live: $name: the model's requests did not come from Claude Code's client (User-Agent: ${ua:-none})" ;; esac ;;
        cx)
            pgrep -u dev -f codex >/dev/null && pass "live: $name: the codex program runs as the user" || fail "live: $name: no codex process runs as the user"
            case "$ua" in *codex*|*Codex*) pass "live: $name: the model's requests came from Codex's own client ($ua)" ;;
                *) fail "live: $name: the model's requests did not come from Codex's client (User-Agent: ${ua:-none})" ;; esac ;;
        oc)
            pgrep -u dev -f openclaw >/dev/null && pass "live: $name: the openclaw gateway runs as the user" || fail "live: $name: no openclaw process runs as the user"
            measured "live: $name: OpenClaw's requests carried User-Agent: ${ua:-none}" ;;
        hm)
            measured "live: $name: Hermes's requests carried User-Agent: ${ua:-none}" ;;
    esac
}

# start_agent HARNESS NAME [SID [JOINNAME]]: a live session of the harness, joined as NAME, AVAILABLE, with one
# turn run so that every later request is attributed to it. SID is the session's label prefix when it must
# differ from the name (a session that starts anew); JOINNAME is the name typed to `join` when it is not NAME
# (a name too long to be a file name). Fails (and returns 1) when the session is not AVAILABLE.
start_agent() {
    local h=$1 name=$2 s n jn=${4:-$2}
    [ -n "${3:-}" ] && echo "$3" > "$SESS/sid-$(lc "$name")"
    s=$(sid "$name")
    echo "$h" > "$SESS/h-$s"
    case "$h" in
        cc) start_cc "$s" ;;
        cx) start_cx "$s" ;;
        oc|hm) : ;;
    esac
    case "$h" in
        cc|cx) echo "join $jn" > "$SESS/$s.keys" ;;
        oc) oc_turn "$s" "join $jn" >/dev/null 2>&1 ;;
        hm) hm_turn "$s" "join $jn" >/dev/null 2>&1 ;;
    esac
    n=0
    until status | grep -q "^$jn	" || [ $n -ge 60 ]; do sleep 0.5; n=$((n + 1)); done
    if status | grep "^$jn	" | grep -q AVAILABLE; then
        pass "$name ($(harness_word "$h")) joined as ${jn:0:40} and is AVAILABLE (guarded by evidence)"
    else
        fail "$name ($(harness_word "$h")) is not AVAILABLE after join as ${jn:0:40}"
        status | sed 's/^/    | /'
        return 1
    fi
    agent_run "$name" true >/dev/null
    live_check "$h" "$name"
    return 0
}

start_cc() {
    local s=$1
    mkfifo "$SESS/$s.keys"
    chmod 666 "$SESS/$s.keys"
    rm -f "$SESS/$s.log"
    (
        cd "$DEVHOME" || exit 1
        as_dev ANTHROPIC_BASE_URL=http://127.0.0.1:8099 ANTHROPIC_API_KEY=sk-ant-mock \
            python3 "$PROOF/sessionpty.py" --log "$SESS/$s.log" --keys "$SESS/$s.keys" \
            --ready "$SESS/$s.ready" --timeout 3000 -- /usr/bin/claude --permission-mode default \
            --allowedTools "Bash($LR:*)" >/dev/null 2>&1
    ) &
    local n=0
    until grep -q "include this session" "$SESS/$s.log" 2>/dev/null || [ $n -ge 90 ]; do sleep 0.5; n=$((n + 1)); done
}

# Codex's own screens (the hook review, the folder, a new model) are answered as a person answers them:
# "Trust all and continue" is the real choice.
start_cx() {
    local s=$1 n=0 quiet=0 ready=no scr
    mkfifo "$SESS/$s.keys"
    chmod 666 "$SESS/$s.keys"
    rm -f "$SESS/$s.log"
    (
        cd "$DEVHOME" || exit 1
        as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/$s.log" --keys "$SESS/$s.keys" \
            --ready "$SESS/$s.ready" --timeout 3000 -- codex >/dev/null 2>&1
    ) &
    while [ $n -lt 150 ]; do
        scr=$(screen_of "$s" | tr '\n' ' ' | tr -s ' ')
        case "$scr" in
            *"Hooks need review"*) key_to "$s" "<DOWN>"; sleep 1; key_to "$s" "<ENTER>"; sleep 3 ;;
            *"Meet GPT-6 Sol"*|*"Try new model"*) key_to "$s" "<DOWN>"; sleep 1; key_to "$s" "<ENTER>"; sleep 3 ;;
            *"Do you trust"*) key_to "$s" "<ENTER>"; sleep 3 ;;
            *"hooks need review before they can run"*) key_to "$s" "<RAW:t>"; sleep 1; key_to "$s" "<DOWN>"; sleep 1; key_to "$s" "<RAW:t>"; sleep 1; key_to "$s" "<ESC>"; sleep 3 ;;
            *"Ask Codex to do anything"*) quiet=$((quiet + 1)); [ "$quiet" -ge 8 ] && { ready=yes; break; } ;;
        esac
        case "$scr" in *"Hooks need review"*|*"Try new model"*|*"Do you trust"*|*"need review"*) quiet=0 ;; esac
        sleep 1
        n=$((n + 1))
    done
    [ "$ready" = yes ] || { fail "Codex ($s) did not reach its prompt"; screen_of "$s" | grep -v '^\s*$' | tail -20 | sed 's/^/    | /'; }
}

screen_of() { python3 "$PROOF/screen.py" "$SESS/$1.log" 140 50; }
key_to() { timeout 5 sh -c 'printf "%s\n" "$1" > "$2"' _ "$2" "$SESS/$1.keys"; }

# Turns of the one-shot harnesses. OpenClaw: a turn of the session key; Hermes: a one-shot turn.
oc_turn() {
    local s=$1 text=$2
    as_dev env OPENCLAW_GATEWAY_TOKEN=proof-token timeout 150 openclaw agent --session-key "agent:main:$s-$CELL" --message "$text" 2>&1 | tail -5
}
hm_turn() {
    local s=$1 text=$2
    as_dev env OPENAI_API_KEY=sk-mock timeout 180 hermes chat --oneshot -q "$text" -t terminal,file --accept-hooks 2>&1 | tail -6
}

# agent_prompt NAME TEXT [bg]: TEXT as a prompt of the session, typed by the proof as a person would
# (Claude Code, Codex) or given as a turn (OpenClaw, Hermes). With bg the turn runs in the background.
agent_prompt() {
    local name=$1 text=$2 bg=${3:-} s h
    s=$(sid "$name")
    h=$(harness_of "$name")
    case "$h" in
        cc|cx) key_to "$s" "$text" ;;
        oc) if [ -n "$bg" ]; then oc_turn "$s" "$text" >/dev/null 2>&1 & else oc_turn "$s" "$text" >/dev/null 2>&1; fi ;;
        hm) if [ -n "$bg" ]; then hm_turn "$s" "$text" >/dev/null 2>&1 & else hm_turn "$s" "$text" >/dev/null 2>&1; fi ;;
        *) fail "agent_prompt: $name has no harness" ;;
    esac
}

# agent_run_async NAME COMMAND: the agent runs COMMAND as one shell call (its model is scripted to ask
# for exactly it). Prints the label; the result comes with `await_result LABEL`.
agent_run_async() {
    local name=$1 cmd=$2 s n label
    s=$(sid "$name")
    n=$(($(cat "$SESS/count-$s" 2>/dev/null || echo 0) + 1))
    echo "$n" > "$SESS/count-$s"
    label="$s-$n"
    printf '%s' "$cmd" > "$SESS/cmd-$label.txt"
    chmod 644 "$SESS/cmd-$label.txt"
    agent_prompt "$name" "RUN $label" bg
    echo "$label"
}
# await_result LABEL [SECONDS]: what the model was given back for the call.
await_result() { "${TOOL[@]}" result "$1" "${2:-90}"; }
# agent_run NAME COMMAND: the call and its result.
agent_run() {
    local label
    label=$(agent_run_async "$1" "$2")
    await_result "$label" 120
}

# ---------------------------------------------------------------------------------------------
# The user: terminals outside every guarded session.

# user_pty NAME COMMAND...: the command as dev on a terminal, the passphrase and sudo password typed from
# files; prints the screen text of what was shown.
user_pty() {
    local name=$1
    shift
    rm -f "$RUN/$name.log"
    pty "$name" "${DEV[@]}" "$@"
    python3 "$PROOF/plaintext.py" "$RUN/$name.log"
}
user_status() { as_dev "$LR" status 2>&1; }
user_yolo() { user_pty yolo "$LR" yolo "$1" till "$2"; }
user_approve_list() { user_pty approvelist "$LR" approve; }
user_approve() { user_pty approve "$LR" approve "$1"; }

# store_text: everything in the user's store, read as root.
store_text() { grep -rIh -a . "$STOREROOT/$(uid_dev)" 2>/dev/null; }

# user_has NEEDLE SINCE DESC: the line is in the user's list: in `lunchroom status` run outside every
# session, and recorded in the store. (A line printed only where the user typed it is not enough.)
user_has() {
    local needle=$1 desc=$3 n=0
    until user_status | grep -qF -- "$needle" || [ $n -ge 20 ]; do sleep 1; n=$((n + 1)); done
    if user_status | grep -qF -- "$needle"; then
        pass "$desc: the user's list ('lunchroom status' outside every session) holds the line"
    else
        fail "$desc: the user's list ('lunchroom status' outside every session) does not hold '$needle'"
        user_status | head -12 | sed 's/^/    | /'
    fi
    if store_text | grep -qF -- "$needle"; then
        pass "$desc: the store records the line"
    else
        fail "$desc: the store does not record '$needle'"
    fi
}

# user_prompt_shows NEEDLE DESC: the user types a prompt in a session of their own (Claude Code, not
# joined) and the notice is shown to them (D1.8: in Claude Code and Codex at the next prompt a person types).
user_prompt_shows() {
    local needle=$1 desc=$2
    if [ ! -p "$SESS/usercc.keys" ]; then
        start_cc usercc
        echo cc > "$SESS/h-usercc"
    fi
    key_to usercc "hello from the user"
    local n=0
    until screen_of usercc | grep -qF -- "$needle" || [ $n -ge 20 ]; do sleep 1; n=$((n + 1)); done
    if screen_of usercc | grep -qF -- "$needle"; then
        pass "$desc: the next prompt the user types in a Claude Code session of their own shows the line"
    else
        fail "$desc: the user's own Claude Code prompt does not show '$needle'"
        screen_of usercc | grep -v '^\s*$' | tail -12 | sed 's/^/    | /'
    fi
}

# ---------------------------------------------------------------------------------------------
# The console, driven on a pseudo-terminal as in the console proof.
CKEYS=$RUN/ckeys
CONSOLE_LOG=$RUN/console.log
CROWS=50
CCOLS=140
console_ready=no

console_start() {
    [ "$console_ready" = yes ] && return 0
    user_pty install-console "$LR" install console >/dev/null
    grep -q "The console is set up" "$RUN/install-console.log" 2>/dev/null \
        && pass "setup: lunchroom install console set the console up" || fail "setup: lunchroom install console did not finish"
    rm -f "$CONSOLE_LOG" "$RUN/console.ready" "$CKEYS"
    mkfifo "$CKEYS"
    python3 "$PROOF/consolepty.py" --log "$CONSOLE_LOG" --ready "$RUN/console.ready" --owner "$(uid_dev)" \
        --size "${CROWS}x${CCOLS}" --keys "$CKEYS" --raw-keys --timeout 7200 \
        --auto "\\[sudo\\] password for dev: ::$RUN/sudo.pw" \
        --auto "Lunchroom passphrase: ::$RUN/lr.pw" \
        -- runuser -u dev -- env HOME=$DEVHOME PATH=/usr/local/bin:/usr/bin:/bin sudo -u lunchroom "$LR" console &
    local n=0
    until console_screen | grep -q "CALLS WAIT" || [ $n -ge 80 ]; do sleep 0.5; n=$((n + 1)); done
    if console_screen | grep -q "CALLS WAIT"; then
        console_ready=yes
        pass "setup: the console runs and drew its screen"
    else
        fail "setup: the console did not draw its screen"
    fi
}
console_screen() { python3 "$PROOF/lastscreen.py" "$CONSOLE_LOG" "${CROWS}x${CCOLS}"; }
console_keys() { timeout 5 bash -c 'printf "%b" "$1" > "$2"' _ "$1" "$CKEYS"; sleep 0.5; }

# console_shows NEEDLE DESC: the console shows NEEDLE on its main screen or in one of its panels (Down and
# Enter through the items, Escape back).
console_shows() {
    local needle=$1 desc=$2 i=0
    console_start
    [ "$console_ready" = yes ] || { fail "$desc: there is no console to look at"; return 1; }
    console_keys '\033'
    console_keys '\033'
    if console_screen | grep -qF -- "$needle"; then
        pass "$desc: the console's main screen shows the line"
        return 0
    fi
    while [ $i -lt 40 ]; do
        console_keys '\033[B'
        console_keys '\r'
        if console_screen | grep -qF -- "$needle"; then
            console_keys '\033'
            pass "$desc: the console shows the line (in a panel it opened)"
            return 0
        fi
        console_keys '\033'
        i=$((i + 1))
    done
    fail "$desc: the console shows no '$needle' on its screen or in any panel"
    console_screen | grep -v '^\s*$' | tail -15 | sed 's/^/    | /'
    return 1
}

# ---------------------------------------------------------------------------------------------
# Hearing.

# expect_heard NAME NEEDLE SINCE DESC: the model of the agent NAME was shown NEEDLE since SINCE, by the
# point its harness lets a hook add text. Claude Code: its listener wakes it, or the result of its next
# tool call carries it (either passes; which one is told). Codex: the result of its next tool call
# (PostToolUse), the Stop listener, or its next prompt. OpenClaw and Hermes: the start of the next turn,
# not before (K1.30). A turn the proof starts is made with `agent_run NAME true`.
expect_heard() {
    local name=$1 needle=$2 since=$3 desc=$4 s h out
    s=$(sid "$name")
    h=$(harness_of "$name")
    case "$h" in
        cc)
            if out=$("${TOOL[@]}" heard "$s" "$needle" "$since" 30); then
                pass "$desc: $name (Claude Code) heard it without a prompt, by its listener: $(echo "$out" | head -1)"
            else
                agent_run "$name" true >/dev/null
                if out=$("${TOOL[@]}" heard "$s" "$needle" "$since" 30); then
                    pass "$desc: $name (Claude Code) heard it by the result of its next tool call ($(echo "$out" | head -1)); the listener did not wake it first"
                else
                    fail "$desc: $name (Claude Code) never heard '$needle' (neither by its listener nor by its next tool result)"
                fi
            fi ;;
        cx)
            agent_run "$name" true >/dev/null
            if out=$("${TOOL[@]}" heard "$s" "$needle" "$since" 30); then
                pass "$desc: $name (Codex) heard it by its next tool call, its Stop or its prompt: $(echo "$out" | head -1)"
            else
                fail "$desc: $name (Codex) never heard '$needle'"
            fi ;;
        oc|hm)
            if "${TOOL[@]}" heard "$s" "$needle" "$since" 3 >/dev/null; then
                fail "$desc: $name ($(harness_word "$h")) heard it before its next turn started: no hook can add text to an idle $(harness_word "$h") session"
            else
                pass "$desc: $name ($(harness_word "$h")) has not heard it while idle (K1.30)"
            fi
            agent_run "$name" true >/dev/null
            if out=$("${TOOL[@]}" heard "$s" "$needle" "$since" 30); then
                pass "$desc: $name ($(harness_word "$h")) heard it at the start of its next turn: $(echo "$out" | head -1)"
            else
                fail "$desc: $name ($(harness_word "$h")) never heard '$needle' at the start of its next turn"
            fi ;;
        *) fail "$desc: $name has no harness" ;;
    esac
    if "${TOOL[@]}" dups "$s" "$needle" "$since"; then
        pass "$desc: $name was not told the same notice twice"
    else
        fail "$desc: $name was told the same notice twice"
    fi
}

# line_form_in TEXT WORDS MINUTES NAMES DESC: every line of Lunchroom's holding WORDS in TEXT has the form of
# item 1 with exactly those words, the event's minute and the agents' names.
line_form_in() {
    local text=$1 words=$2 minutes=$3 names=$4 desc=$5 out
    if out=$(printf '%s\n' "$text" | "${TOOL[@]}" lineform "$words" "$minutes" "$names"); then
        pass "$desc: the line has the form of item 1: $(echo "$out" | head -1 | sed 's/^OK   //')"
    else
        fail "$desc: the line is not of the form of item 1: $(echo "$out" | head -3 | tr '\n' ' ')"
    fi
}

# heard_line_form NAME WORDS NAMES SINCE T1 DESC: the line as the model of NAME was shown it.
heard_line_form() {
    local name=$1 words=$2 names=$3 since=$4 t1=$5 desc=$6 s text
    s=$(sid "$name")
    text=$("${TOOL[@]}" texts "$s" "Lunchroom: $words" "$since" 2>/dev/null)
    [ -n "$text" ] || { fail "$desc: $name was shown no line holding '$words'"; return 1; }
    line_form_in "$text" "$words" "$(minutes_of "$since" "$t1")" "$names" "$desc"
}

# envelope_of NAME SINCE: the envelope that reached the agent, by the route its harness has (D1.1): Claude
# Code, in its delivery prompt; Hermes, in the context of its next turn; Codex and OpenClaw, in the output
# of `lunchroom read <id>`, which the agent runs (the scripted model runs it for it). Prints the text.
envelope_of() {
    local name=$1 since=$2 s h id n=0 text=""
    s=$(sid "$name")
    h=$(harness_of "$name")
    case "$h" in
        cc|hm)
            # A Hermes agent is handed the envelope at the start of its next turn: the proof starts one.
            [ "$h" = hm ] && agent_run "$name" true >/dev/null
            until text=$("${TOOL[@]}" texts "$s" "lunchroom delivery:" "$since" 2>/dev/null) && [ -n "$text" ] || [ $n -ge 60 ]; do sleep 1; n=$((n + 1)); done ;;
        cx|oc)
            n=0
            until id=$(sed -n 's/.*lunchroom read \(lr-[0-9]*\).*/\1/p' <<< "$("${TOOL[@]}" texts "$s" "lunchroom read" "$since" 2>/dev/null)" | tail -1) && [ -n "$id" ] || [ $n -ge 60 ]; do sleep 1; n=$((n + 1)); done
            if [ -n "$id" ]; then
                text=$(agent_run "$name" "$LR read $id")
            fi ;;
    esac
    printf '%s' "$text"
}

# check_envelope NAME WORDS SINCE DESC [BODY]: the envelope holds the line between the header and the label,
# the label is intact, and (with BODY) the message's own words are in it.
check_envelope() {
    local name=$1 words=$2 since=$3 desc=$4 body=${5:-} env out
    env=$(envelope_of "$name" "$since")
    if [ -z "$env" ]; then
        fail "$desc: $name received no envelope"
        return 1
    fi
    if out=$(printf '%s\n' "$env" | "${TOOL[@]}" envelope "$words"); then
        pass "$desc: the envelope of $name has the line after the header and before the label: $(echo "$out" | sed 's/^OK   //' | cut -c1-120)"
    else
        fail "$desc: the envelope of $name is wrong: $out"
        printf '%s\n' "$env" | head -12 | sed 's/^/    | /'
    fi
    if [ -n "$body" ]; then
        printf '%s' "$env" | grep -qF -- "$body" && pass "$desc: the envelope carries the message's words '$body'" \
            || fail "$desc: the envelope does not carry '$body'"
    fi
    LAST_ENVELOPE=$env
}

# holds NAME: lunchroom status shows NAME as HELD.
agent_state() { status | grep "^$1	" | head -1; }

# witness_count NAME: how many times the fake program NAME ran (fake gh, alembic: /usr/local/bin).
witness_count() { [ -f "$WITNESS/$1.log" ] && wc -l < "$WITNESS/$1.log" || echo 0; }

# no_text_in NAME NEEDLE SINCE DESC: nothing the model of NAME was shown holds NEEDLE.
no_text_in() {
    local name=$1 needle=$2 since=$3 desc=$4
    if "${TOOL[@]}" heard "$(sid "$name")" "$needle" "$since" 0 >/dev/null; then
        fail "$desc: $name was shown '$needle'"
    else
        pass "$desc: $name was shown nothing holding '$needle'"
    fi
}

# Each cell of a grid: harness of one agent, harness of the other.
harness_list() {
    # CARD_HARNESSES (a list of cc cx oc hm) narrows the grid; the default is all four.
    echo "${CARD_HARNESSES:-$NAMES_ALL}"
}
# In a base run only the cell the spec's test line names is played.
base_only() { [ "${CARD_CELLS:-all}" = base ]; }

cell_banner() { heading "cell $CELL: $*"; }
