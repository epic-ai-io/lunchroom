#!/bin/bash
# Card 6 (spec section 16 items 19 to 21; section 4 R3), sourced by inner.sh.
#
#   GLM:        "Mistral, please deploy fix to production."
#   Lunchroom:  "Mistral is UNGUARDED."
#   GLM:        "Blocked. I'll wait until guards are back."     (GLM's own session text)
#
# Item 19's test: Mistral on Codex, with a Lunchroom hook not trusted, is UNGUARDED; GLM on Claude Code
# sends the message. Item 20's test: repeated with GLM on each of Codex, OpenClaw and Hermes; once Mistral's
# hooks are trusted the message is DELIVERED once and its envelope carries the line. Item 21: the run of
# item 20 ends with that one delivery. Mistral does not hear the line while it is UNGUARDED.

C6_L1="Mistral, please deploy fix to production."
C6_UNG="Mistral is UNGUARDED."
C6_L3="Blocked. I'll wait until guards are back."

# start_cx_untrusted NAME: a Codex session whose Lunchroom hooks are not all trusted. The session is first
# started and trusted as a person would, ended, one hook's trust record is removed from Codex's own
# configuration (the state of a Codex whose hooks changed since they were trusted, which is what a
# Lunchroom upgrade does), and it is started again; at Codex's hook review it chooses to go on without
# trusting.
start_cx_untrusted() {
    local s=$1 n=0 quiet=0 ready=no scr
    start_cx "$s"
    pkill -u dev -f sessionpty.py 2>/dev/null
    pkill -u dev -f codex 2>/dev/null
    sleep 3
    python3 - "$DEVHOME/.codex/config.toml" <<'PY'
import re, sys
p = sys.argv[1]
lines = open(p).read().split("\n")
out, removed = [], 0
for ln in lines:
    if removed == 0 and re.match(r"\s*trusted_hash\s*=", ln):
        removed = 1
        continue
    out.append(ln)
open(p, "w").write("\n".join(out))
print("trusted_hash lines removed:", removed)
PY
    rm -f "$SESS/$s.keys" "$SESS/$s.log"
    mkfifo "$SESS/$s.keys"
    chmod 666 "$SESS/$s.keys"
    (
        cd "$DEVHOME" || exit 1
        as_dev python3 "$PROOF/sessionpty.py" --log "$SESS/$s.log" --keys "$SESS/$s.keys" \
            --ready "$SESS/$s.ready" --timeout 3000 -- codex >/dev/null 2>&1
    ) &
    while [ $n -lt 150 ]; do
        scr=$(screen_of "$s" | tr '\n' ' ' | tr -s ' ')
        case "$scr" in
            *"Hooks need review"*) measured "Codex's hook review: the first choice (go on without trusting) is taken"; key_to "$s" "<ENTER>"; sleep 3 ;;
            *"Meet GPT-6 Sol"*|*"Try new model"*) key_to "$s" "<DOWN>"; sleep 1; key_to "$s" "<ENTER>"; sleep 3 ;;
            *"Do you trust"*) key_to "$s" "<ENTER>"; sleep 3 ;;
            *"hooks need review before they can run"*) key_to "$s" "<ESC>"; sleep 3 ;;
            *"Ask Codex to do anything"*) quiet=$((quiet + 1)); [ "$quiet" -ge 8 ] && { ready=yes; break; } ;;
        esac
        case "$scr" in *"Hooks need review"*|*"Try new model"*|*"Do you trust"*|*"need review"*) quiet=0 ;; esac
        sleep 1
        n=$((n + 1))
    done
    [ "$ready" = yes ] || { fail "Codex ($s, untrusted) did not reach its prompt"; screen_of "$s" | grep -v '^\s*$' | tail -15 | sed 's/^/    | /'; return 1; }
}

# trust_cx_live NAME: the user trusts the hooks in the running Codex session, at Codex's own hook list
# (`/hooks`: t on each hook, Escape back), as the user is told to.
trust_cx_live() {
    local s=$1 i
    key_to "$s" "/hooks"
    sleep 4
    for i in 1 2 3 4; do
        key_to "$s" "<RAW:t>"
        sleep 1
        key_to "$s" "<DOWN>"
        sleep 1
    done
    key_to "$s" "<ESC>"
    sleep 3
}

card6_cell() {
    local gh=$1 T0 T1 T2 T3 out out2 n=0 cnt
    reset_world
    cell_banner "card 6: GLM on $(harness_word "$gh") -> Mistral on Codex (a Lunchroom hook not trusted)"
    start_cx_untrusted mistral || return 0
    echo cx > "$SESS/h-mistral"
    echo "join Mistral" > "$SESS/mistral.keys"
    until status | grep -q "^Mistral	" || [ $n -ge 40 ]; do sleep 0.5; n=$((n + 1)); done
    if status | grep "^Mistral	" | grep -q "UNGUARDED"; then
        pass "item 19: Mistral on Codex, with a Lunchroom hook not trusted, is UNGUARDED: $(agent_state Mistral | tr '\t' ' ')"
    else
        fail "item 19: Mistral is not UNGUARDED with a hook untrusted: $(agent_state Mistral | tr '\t' ' ')"
        return 0
    fi
    start_agent "$gh" GLM || return 0

    # Item 19: GLM sends; the message stays QUEUED; nothing is delivered to Mistral.
    T0=$(now)
    out=$(agent_run GLM "$LR send Mistral \"$C6_L1\"")
    T1=$(now)
    measured "GLM's send printed: $(echo "$out" | head -c 900 | tr '\n' '|')"
    # Item 20, GLM by S, at once, followed by the reason in the form an agent may read.
    line_form_in "$out" "$C6_UNG" "$(minutes_of "$T0" "$T1")" "GLM,Mistral" "item 20 (GLM, S)"
    if [ "$(printf '%s\n' "$out" | grep -cv '^[[:space:]]*$')" -ge 2 ]; then
        pass "item 20 (R3.4): the output goes on past the line with a reason"
    else
        fail "item 20 (R3.4): the output has no reason after the line"
    fi
    status | grep -i "QUEUED" | grep -qi "mistral\|lr-" && pass "item 19: the message is QUEUED: $(status | grep -i QUEUED | head -1 | tr '\t' ' ')" \
        || fail "item 19: status shows no QUEUED message for Mistral"
    sleep 5
    no_text_in Mistral "deploy fix" "$T0" "item 19 (Mistral does not hear the message)"
    ps -eo args | grep -q "[c]odex queue" && fail "item 19: a 'codex queue' hand-over ran for an UNGUARDED recipient" \
        || pass "item 19: no 'codex queue' hand-over ran for the UNGUARDED recipient"
    # Item 20: Mistral hears nothing while UNGUARDED (it has no hook Lunchroom can trust to carry a line).
    agent_run Mistral true >/dev/null 2>&1
    no_text_in Mistral "$C6_UNG" "$T0" "item 20 (Mistral hears nothing while UNGUARDED)"
    no_text_in Mistral "$C6_L3" "$T0" "item 21 (Mistral does not hear GLM's own session text)"
    # Item 20, the user.
    user_has "$C6_UNG" "$T0" "item 20 (U)"
    console_shows "$C6_UNG" "item 20 (console)"
    # Each later send prints the line again (S); the user's list holds it once.
    T2=$(now)
    out2=$(agent_run GLM "$LR send Mistral \"Mistral, is the build ready?\"")
    printf '%s' "$out2" | grep -qF "$C6_UNG" && pass "item 20: a second send prints the line again (S)" || fail "item 20: a second send did not print '$C6_UNG'"

    # Item 20 and 21: the hooks are trusted; the message is DELIVERED once; its envelope carries the line.
    T3=$(now)
    trust_cx_live mistral
    agent_run Mistral true >/dev/null 2>&1
    n=0
    until status | grep "^Mistral	" | grep -qE "AVAILABLE|HELD" || [ $n -ge 60 ]; do sleep 1; n=$((n + 1)); done
    status | grep "^Mistral	" | grep -qE "AVAILABLE|HELD" && pass "item 20: with the hooks trusted Mistral is guarded again: $(agent_state Mistral | tr '\t' ' ')" \
        || { fail "item 20: Mistral is still UNGUARDED after the hooks were trusted: $(agent_state Mistral | tr '\t' ' ')"; return 0; }
    LAST_ENVELOPE=""
    check_envelope Mistral "$C6_UNG" "$T3" "item 20 (Mistral, N: the envelope of the message once guarded)" "$C6_L1"
    [ -n "$LAST_ENVELOPE" ] && line_form_in "$LAST_ENVELOPE" "$C6_UNG" "$(minutes_of "$T0" "$(now)")" "GLM,Mistral" "item 20 (Mistral, N)"
    cnt=$("${TOOL[@]}" texts mistral "$C6_L1" "$T0" 2>/dev/null | grep -cF "$C6_L1")
    [ "$cnt" = 1 ] && pass "item 21: the message was DELIVERED once (the model of Mistral was shown it once)" \
        || fail "item 21: the model of Mistral was shown the message $cnt times, not once"
}

card6_main() {
    setup_world
    local gh
    for gh in $(harness_list); do
        if base_only && [ "$gh" != cc ]; then continue; fi
        card6_cell "$gh"
    done
    finish
}
