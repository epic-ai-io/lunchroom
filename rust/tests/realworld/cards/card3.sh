#!/bin/bash
# Card 3 (spec section 16 items 10 to 12; section 2 R1.2, R1.3, R1.4, R1.8), sourced by inner.sh.
#
#   Claude:     "Codex, please review the merge."  (7 times)
#   Codex:      "Approved. Claude, please confirm."  (8 times)
#   Lunchroom:  "8-message loop. Conversation halted."
#
# Item 10's test: Claude on Claude Code and Codex on Codex exchange the 15 messages of R1.8. Item 22: the
# hearer of each line on every harness, so Claude and the agent Codex each on all four (16 cells). A third
# agent, Kimi on Claude Code, sends Claude a message afterwards: Claude's next envelope from any agent
# carries the line (item 12).

C3_A="Codex, please review the merge."
C3_B="Approved. Claude, please confirm."
C3_LOOP="8-message loop. Conversation halted."

card3_cell() {
    local ch=$1 xh=$2 i T0 T7 T8 T9 out out8 id mins halted_early=0 undelivered=0
    reset_world
    cell_banner "card 3: Claude on $(harness_word "$ch"), Codex on $(harness_word "$xh")"
    start_agent "$ch" Claude || return 0
    start_agent "$xh" Codex || return 0

    T0=$(now)
    # Claude's message, seven times. The first comes from an agent that is not HELD.
    for i in 1 2 3 4 5 6 7; do
        out=$(agent_run Claude "$LR send Codex \"$C3_A\"")
        printf '%s' "$out" | grep -qE "DENIED|HALTED|Conversation halted" && { fail "item 10: Claude's send $i of 7 was stopped: $(echo "$out" | head -c 300)"; halted_early=1; }
    done
    [ "$halted_early" = 0 ] && pass "item 10: all seven of Claude's sends were delivered, none stopped (the count is 7, short of 8)"
    # Codex reads what reached it (cx and oc read an envelope by `lunchroom read`; cc and hm are handed it).
    envelope_of Codex "$T0" >/dev/null
    id=$("${TOOL[@]}" ids codex "$T0" | tail -1)
    measured "the newest message id Codex was shown: ${id:-none}"
    local reply=""
    [ -n "$id" ] && reply="--reply-to $id "

    # Codex's reply, eight times. The eighth halts the pair.
    T7=$(now)
    for i in 1 2 3 4 5 6 7; do
        out=$(agent_run Codex "$LR send Claude ${reply}\"$C3_B\"")
        printf '%s' "$out" | grep -qE "DENIED|HALTED|Conversation halted" && { fail "item 11: Codex's send $i of 8 was stopped: $(echo "$out" | head -c 300)"; undelivered=1; }
    done
    [ "$undelivered" = 0 ] && pass "item 11: Codex's sends 1 to 7 were delivered and the pair was not halted (R1.2: the count is 7)"
    T8=$(now)
    out8=$(agent_run Codex "$LR send Claude ${reply}\"$C3_B\"")
    T9=$(now)
    mins=$(minutes_of "$T8" "$T9")
    measured "Codex's eighth send printed: $(echo "$out8" | head -c 600 | tr '\n' '|')"

    # Item 12, Codex by S: the output of its eighth send holds the line.
    line_form_in "$out8" "$C3_LOOP" "$mins" "Codex,Claude" "item 12 (Codex, S)"
    printf '%s' "$out8" | grep -q "DENIED" && fail "item 12: the eighth message was DENIED; it is to be delivered and then halt the pair (R1.3)" \
        || pass "item 12: the eighth message was not DENIED (R1.3: that message is delivered)"

    # Claude, who did not send the eighth, by D at the moment of the halt.
    expect_heard Claude "$C3_LOOP" "$T8" "item 12 (Claude, D)"
    heard_line_form Claude "$C3_LOOP" "Codex,Claude" "$T8" "$(now)" "item 12 (Claude, D)"

    # The pair is HALTED for reason loop; status shows it (C1.1).
    local st
    st=$(status)
    if printf '%s\n' "$st" | grep -i "HALTED" | grep -qi "codex" && printf '%s\n' "$st" | grep -i "HALTED" | grep -qi "claude"; then
        pass "item 12: status shows the HALTED pair: $(printf '%s\n' "$st" | grep -i HALTED | head -1 | tr '\t' ' ')"
    else
        fail "item 12: status does not show the pair HALTED: $(printf '%s\n' "$st" | head -8 | tr '\n' '|')"
    fi
    printf '%s\n' "$st" | grep -i "HALTED" | grep -qi "loop" && pass "item 12: the reason shown is loop" || fail "item 12: status does not name the reason loop"

    # Every later message between them is DENIED, with "Conversation halted.".
    out=$(agent_run Claude "$LR send Codex \"$C3_A\"")
    if printf '%s' "$out" | grep -q "DENIED" && printf '%s' "$out" | grep -qF "Conversation halted."; then
        pass "item 12 (R1.4): the ninth message, from Claude, is DENIED and the sender is told 'Conversation halted.'"
    else
        fail "item 12 (R1.4): the next message from Claude was not DENIED with 'Conversation halted.': $(echo "$out" | head -c 300)"
    fi
    out=$(agent_run Codex "$LR send Claude \"$C3_B\"")
    printf '%s' "$out" | grep -q "DENIED" && pass "item 12 (R1.4): the next message from Codex is DENIED too" \
        || fail "item 12 (R1.4): the next message from Codex was not DENIED: $(echo "$out" | head -c 300)"

    # The user: the list, the user's prompt, the console.
    user_has "$C3_LOOP" "$T8" "item 12 (U)"
    console_shows "$C3_LOOP" "item 12 (console)"
    user_prompt_shows "$C3_LOOP" "item 12 (U)"

    # Claude's next envelope from any agent: a third agent, Kimi on Claude Code, sends Claude a message.
    if true; then
        local T10
        start_agent cc Kimi || return 0
        T10=$(now)
        agent_run Kimi "$LR send Claude \"Claude, a different pair: how is the build?\"" >/dev/null
        LAST_ENVELOPE=""
        check_envelope Claude "$C3_LOOP" "$T10" "item 12 (Claude, N: its next envelope from any agent)" "how is the build?"
        [ -n "$LAST_ENVELOPE" ] && line_form_in "$LAST_ENVELOPE" "$C3_LOOP" "$(minutes_of "$T8" "$(now)")" "Codex,Claude" "item 12 (Claude, N)"
        # The loop is between Claude and Codex: Kimi, in no loop, was not halted.
        printf '%s\n' "$(status)" | grep -i HALTED | grep -qi kimi && fail "item 12: Kimi, who is in no loop, is shown HALTED" \
            || pass "item 12: the HALTED pair is Claude and Codex only (Kimi is not shown HALTED)"
    fi
}

card3_main() {
    setup_world
    local ch xh
    for ch in $(harness_list); do
        for xh in $(harness_list); do
            if base_only && ! { [ "$ch" = cc ] && [ "$xh" = cx ]; }; then continue; fi
            card3_cell "$ch" "$xh"
        done
    done
    finish
}
