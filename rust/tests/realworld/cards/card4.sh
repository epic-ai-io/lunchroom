#!/bin/bash
# Card 4 (spec section 16 items 13 to 15; section 7 A1.2, section 8 A2), sourced by inner.sh.
#
#   Claude:     "Tests pass. Codex, please merge."
#   User:       "Approved. Merge it."        (an approval of Codex's call, with the passphrase)
#   Lunchroom:  "Granted. Codex can merge."
#
# Item 13's test: Claude on Claude Code sends the first line to Codex on Codex, which is AVAILABLE.
# Item 14's test: Codex asks to run `gh pr merge`; the call waits; the user approves it with `lunchroom
# approve` and the passphrase; nothing is typed in Codex's session; Codex stays HELD. Item 15's test: after
# the approval Codex's `lunchroom read <id>` prints the line; Claude's notices carry it; the call changed
# by one byte is DENIED and a second use is DENIED. Item 22: the agent that merges on each of the four
# harnesses (Claude stays on Claude Code, as the card's test says).

C4_L1="Tests pass. Codex, please merge."
C4_GRANT="Granted. Codex can merge."

card4_cell() {
    local xh=$1 T0 T1 T2 T3 out id aid W0 apr list n ran
    reset_world
    cell_banner "card 4: Claude on Claude Code -> Codex on $(harness_word "$xh")"
    start_agent cc Claude || return 0
    start_agent "$xh" Codex || return 0

    # Item 13: line 1, delivered to Codex by D1.1; the message holds Codex.
    T0=$(now)
    out=$(agent_run Claude "$LR send Codex \"$C4_L1\"")
    measured "Claude's send printed: $(echo "$out" | head -c 500 | tr '\n' '|')"
    LAST_ENVELOPE=""
    check_envelope Codex "Codex is HELD." "$T0" "item 13 (Codex, D1.1)" "$C4_L1"
    agent_state Codex | grep -q HELD && pass "item 13: Codex is HELD by the message" || fail "item 13: Codex is not HELD: $(agent_state Codex | tr '\t' ' ')"
    id=$("${TOOL[@]}" ids codex "$T0" | tail -1)

    # Item 14: Codex asks to run `gh pr merge`; the call waits. (On Codex the guard keeps the call for the
    # user: exit 2 with the reason, kept for `lunchroom approve`. On the other harnesses the harness's own
    # prompt answers it, and these steps are recorded, not decided: item 14 names Codex.)
    W0=$(witness_count gh)
    T1=$(now)
    agent_run_async Codex "gh pr merge 1" >/dev/null
    sleep 15
    [ "$(witness_count gh)" = "$W0" ] && pass "item 14: Codex's merge call waits: the fake gh has not run" \
        || fail "item 14: Codex's merge call ran before anyone approved it"
    [ "$xh" = cx ] || SOFT=1
    user_has "Codex has a call waiting for you (call" "$T1" "item 14 (U: the call waits)"
    expect_heard Claude "Codex has a call waiting for the user (call" "$T1" "item 14 (Claude, D1.8)"
    no_text_in Claude "gh pr merge" "$T1" "item 14 (D1.8: no call text in any notice)"
    # The user's notice and the user's command are not shown to an agent.
    no_text_in Claude "lunchroom approve" "$T1" "item 14 (Claude is not told the user's command)"
    no_text_in Claude "has a call waiting for you" "$T1" "item 14 (the user's notice is not shown to Claude)"
    SOFT=0
    if [ "$xh" = cc ]; then
        # On Claude Code the harness's own prompt answers the call: the user answers it on the terminal (the
        # default choice is yes). Claude Code gives no line by A1.2, so Codex's agent hears by D, and Claude,
        # who asked, by D1.8.
        key_to codex "<ENTER>"
        sleep 8
        [ "$(witness_count gh)" = $((W0 + 1)) ] && pass "item 15: after the user's yes the merge ran once" || fail "item 15: the merge did not run once after the user's yes"
        expect_heard Claude "$C4_GRANT" "$T1" "item 15 (Claude, D1.8)"
        expect_heard Codex "$C4_GRANT" "$T1" "item 15 (the agent that merges, on Claude Code: by D)"
        heard_line_form Claude "$C4_GRANT" "Codex" "$T1" "$(now)" "item 15 (Claude)"
        user_has "$C4_GRANT" "$T1" "item 15 (U)"
        console_shows "$C4_GRANT" "item 15 (console)"
        return 0
    fi
    if [ "$xh" != cx ]; then
        measured "item 15: the user's answer path of $(harness_word "$xh") (its approval surface) is not driven by this kit; the waiting notices above are recorded and nothing more"
        return 0
    fi

    # The user approves with `lunchroom approve` and the passphrase, in a terminal outside every session.
    list=$(user_approve_list)
    measured "lunchroom approve lists: $(echo "$list" | head -c 500 | tr '\n' '|')"
    n=$(printf '%s\n' "$list" | sed -n 's/^[^0-9]*\([0-9][0-9]*\)[^0-9].*/\1/p' | head -1)
    [ -n "$n" ] && pass "item 14: 'lunchroom approve' lists the waiting call (number $n)" || { fail "item 14: 'lunchroom approve' lists no waiting call"; return 0; }
    printf '%s' "$list" | grep -q "gh pr merge" && pass "item 14: the user's own list shows the call" || fail "item 14: the user's list does not show the call"
    T2=$(now)
    apr=$(user_approve "$n")
    T3=$(now)
    measured "lunchroom approve $n printed: $(echo "$apr" | head -c 600 | tr '\n' '|')"
    agent_state Codex | grep -q HELD && pass "item 14: Codex stays HELD after the approval, and nothing was typed in its session" \
        || fail "item 14: Codex is no longer HELD after the approval"

    # Item 15: the line, to each hearer by its channel.
    expect_heard Claude "$C4_GRANT" "$T2" "item 15 (Claude, D1.8)"
    heard_line_form Claude "$C4_GRANT" "Codex" "$T2" "$(now)" "item 15 (Claude)"
    user_has "$C4_GRANT" "$T2" "item 15 (U)"
    console_shows "$C4_GRANT" "item 15 (console)"
    user_prompt_shows "$C4_GRANT" "item 15 (U)"
    # Codex: A1.2's line, through `codex queue`, which names only the approval's id; `lunchroom read <id>`
    # prints the line first, then what the approval covers.
    aid=$(sed -n 's/.*lunchroom read \(lr-[0-9]*\).*/\1/p' <<< "$("${TOOL[@]}" texts codex "lunchroom read" "$T2" 2>/dev/null)" | tail -1)
    if [ -n "$aid" ]; then
        local rd
        rd=$(agent_run Codex "$LR read $aid")
        measured "Codex's lunchroom read $aid printed: $(echo "$rd" | head -c 600 | tr '\n' '|')"
        line_form_in "$rd" "$C4_GRANT" "$(minutes_of "$T2" "$(now)")" "Codex" "item 15 (Codex, A1.2)"
        printf '%s' "$rd" | grep -qF "10 minutes" && pass "item 15: the text says the approval covers the call once, within 10 minutes" \
            || fail "item 15: the text does not say '10 minutes'"
        printf '%s' "$rd" | grep -qi "once" && pass "item 15: the text says 'once'" || fail "item 15: the text does not say 'once'"
        printf '%s' "$rd" | grep -q "gh pr merge" && fail "item 15: the text carries the call" || pass "item 15: the text does not carry the call"
    else
        fail "item 15: no 'lunchroom read <id>' reached Codex after the approval (codex queue did not name an id)"
    fi
    ps -eo args 2>/dev/null | grep -q "codex queue.*gh pr merge" && fail "item 15: the call text was on a command line" \
        || pass "item 15: the call text is on no command line"

    # The call changed by one byte is DENIED; the exact call runs once; a second use is DENIED.
    out=$(agent_run Codex "gh pr merge 2")
    if printf '%s' "$out" | grep -q "DENIED" || [ "$(witness_count gh)" = "$W0" ]; then
        pass "item 15: the call changed by one byte ('gh pr merge 2') did not run: $(echo "$out" | head -c 120 | tr '\n' ' ')"
    else
        fail "item 15: the call changed by one byte ran"
    fi
    agent_run Codex "gh pr merge 1" >/dev/null
    sleep 3
    ran=$(witness_count gh)
    [ "$ran" = $((W0 + 1)) ] && pass "item 15: the approved call ran once (A2.3)" || fail "item 15: the approved call ran $((ran - W0)) times, not once"
    out=$(agent_run Codex "gh pr merge 1")
    sleep 2
    if [ "$(witness_count gh)" = "$ran" ]; then
        pass "item 15: a second use of the approval did not run (A2.6): $(echo "$out" | head -c 120 | tr '\n' ' ')"
    else
        fail "item 15: a second use of the approval ran"
    fi
    agent_state Codex | grep -q HELD && pass "item 15: Codex stays HELD after the merge" || fail "item 15: Codex is not HELD after the merge"
}

card4_main() {
    setup_world
    local xh
    for xh in $(harness_list); do
        if base_only && [ "$xh" != cx ]; then continue; fi
        card4_cell "$xh"
    done
    finish
}
