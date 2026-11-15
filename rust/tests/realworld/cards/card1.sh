#!/bin/bash
# Card 1 (spec section 16 items 4 to 6), sourced by inner.sh.
#
#   Kimi:       "Claude, Claude, merge the PR."
#   Lunchroom:  "Claude is HELD."
#   Claude:     "Understood. Waiting for user."
#
# Item 4's test: Kimi on OpenClaw sends the first line to Claude on Claude Code. Item 5's test: Claude on
# each of the four harnesses and Kimi on each of the four (16 cells). Item 6's test: Claude on Claude
# Code asks to run `gh pr merge`; the merge does not run. Item 22: every hearer of every line, on every
# harness, by the channel item 2 gives.

L1="Claude, Claude, merge the PR."
HELD="Claude is HELD."
WILL="Claude will be HELD when it reads this."
L3="Understood. Waiting for user."

card1_cell() {
    local kh=$1 ch=$2 item6=$3 T0 T1 T2 T3 out out3 mins id
    reset_world
    cell_banner "card 1: Kimi on $(harness_word "$kh") -> Claude on $(harness_word "$ch")"
    start_agent "$kh" Kimi || return 0
    start_agent "$ch" Claude || return 0

    # Item 4, line 1: Kimi's `lunchroom send`.
    T0=$(now)
    out=$(agent_run Kimi "$LR send Claude \"$L1\"")
    T1=$(now)
    mins=$(minutes_of "$T0" "$T1")
    measured "Kimi's send printed (as Kimi's harness gave it back): $(echo "$out" | head -c 700 | tr '\n' '|')"

    # Item 4 outcome and item 2 channels D1.1 and N: Claude gets the message in its envelope; the envelope
    # has the line after the header and before the label, with the definition of HELD, and the label is
    # intact.
    LAST_ENVELOPE=""
    check_envelope Claude "$HELD" "$T0" "item 4/5 (Claude, N)" "$L1"
    if [ -n "$LAST_ENVELOPE" ]; then
        line_form_in "$LAST_ENVELOPE" "$HELD" "$(minutes_of "$T0" "$(now)")" "Kimi,Claude" "item 5 (Claude, N)"
    fi
    if [ "$ch" = cc ]; then
        printf '%s' "$LAST_ENVELOPE" | grep -q "lunchroom delivery:" \
            && pass "item 4: Claude's delivery prompt (Claude Code) carries the message" || fail "item 4: no delivery prompt"
    fi
    agent_state Claude | grep -q "HELD" && pass "item 4: Claude is HELD by the message (D1.5): $(agent_state Claude | tr '\t' ' ')" \
        || fail "item 4: status does not show Claude HELD: $(agent_state Claude | tr '\t' ' ')"

    # Item 5, Kimi: by S where Claude was HELD by the time `send` returned, else "will be HELD when it
    # reads this." from S and the line by D at the moment of the hold, by the channel of Kimi's harness.
    if printf '%s\n' "$out" | grep -qF "$HELD"; then
        line_form_in "$out" "$HELD" "$mins" "Kimi,Claude" "item 5 (Kimi, S)"
        case "$ch" in
            cc|hm) fail "item 5: Claude on $(harness_word "$ch") is not HELD when send returns, but send printed '$HELD' (item 2 S)" ;;
        esac
    elif printf '%s\n' "$out" | grep -qF "$WILL"; then
        pass "item 5 (Kimi, S): send printed '$WILL'"
        expect_heard Kimi "$HELD" "$T0" "item 5 (Kimi, D)"
        heard_line_form Kimi "$HELD" "Kimi,Claude" "$T0" "$(now)" "item 5 (Kimi, D)"
    else
        fail "item 5: Kimi's send printed neither '$HELD' nor '$WILL'"
    fi
    case "$ch" in
        cc|hm)
            printf '%s\n' "$out" | grep -qF "$WILL" && pass "item 2 (S): a $(harness_word "$ch") recipient is not HELD yet, and send printed '$WILL'" \
                || fail "item 2 (S): send did not print '$WILL' for a $(harness_word "$ch") recipient" ;;
    esac

    # Item 5, the user: the user's list, the console, and the prompt the user types.
    user_has "$HELD" "$T0" "item 5 (U)"
    console_shows "$HELD" "item 5 (console)"
    [ "$kh" = oc ] && user_prompt_shows "$HELD" "item 5 (U)"

    # A second message from Kimi while Claude is already HELD: S prints "Claude is HELD."
    T3=$(now)
    out3=$(agent_run Kimi "$LR send Claude \"Claude, are you there?\"")
    if printf '%s\n' "$out3" | grep -qF "$HELD"; then
        line_form_in "$out3" "$HELD" "$(minutes_of "$T3" "$(now)")" "Kimi,Claude" "item 2 (S, recipient already HELD)"
    else
        fail "item 2 (S): a recipient already HELD: Kimi's second send did not print '$HELD': $(echo "$out3" | head -c 300)"
    fi

    [ "$item6" = yes ] || return 0

    # Item 6, line 3 and the merge call.
    id=$("${TOOL[@]}" ids claude "$T0" | head -1)
    measured "the id of Kimi's first message, as Claude was shown it: ${id:-none}"
    T2=$(now)
    local reply=""
    [ -n "$id" ] && reply="--reply-to $id "
    out=$(agent_run Claude "$LR send Kimi ${reply}\"$L3\"")
    measured "Claude's reply printed: $(echo "$out" | head -c 400 | tr '\n' '|')"
    if printf '%s' "$out" | grep -q "DENIED"; then
        fail "item 6: Claude's reply inside the slip was DENIED: $(echo "$out" | head -c 300)"
    else
        pass "item 6: Claude's reply to the agent whose message holds it was not DENIED (inside the slip, no step of the user)"
    fi
    local envk
    envk=$(envelope_of Kimi "$T2")
    printf '%s' "$envk" | grep -qF "$L3" && pass "item 6: Kimi has Claude's words '$L3' in its envelope (D1.1)" \
        || fail "item 6: Kimi's envelope does not carry '$L3'"

    # The merge call. Item 6's test names Claude on Claude Code; on the other harnesses the notices are
    # recorded, not decided (the spec's test line is for Claude Code), but the merge must not run on any.
    local W0
    W0=$(witness_count gh)
    agent_run_async Claude "gh pr merge 1" >/dev/null
    sleep 15
    [ "$(witness_count gh)" = "$W0" ] && pass "item 6: Claude's merge call did not run (the fake gh left no record)" \
        || fail "item 6: Claude's merge call RAN (the fake gh recorded it)"
    [ "$ch" = cc ] || SOFT=1
    user_has "Claude has a call waiting for you (call" "$T2" "item 6 (U: the call waits)"
    expect_heard Kimi "Claude has a call waiting for the user (call" "$T2" "item 6 (Kimi, D1.8)"
    if "${TOOL[@]}" heard kimi "Your message $id holds Claude." "$T2" 0 >/dev/null; then
        pass "item 6: Kimi's notice says 'Your message $id holds Claude.'"
    else
        fail "item 6: Kimi's notice does not say 'Your message ${id:-?} holds Claude.'"
    fi
    no_text_in Kimi "gh pr merge" "$T2" "item 6 (D1.8: no call text in any notice)"
    # The user's notice is written for the user: no agent's model is shown it or the command it names.
    no_text_in Kimi "has a call waiting for you" "$T2" "item 6 (the user's notice is not shown to Kimi)"
    no_text_in Kimi "lunchroom approve" "$T2" "item 6 (Kimi is not told the user's command)"
    no_text_in Claude "lunchroom approve" "$T2" "item 6 (Claude is not told the user's command)"
    if [ "$ch" = cc ]; then
        screen_of claude | grep -qi "proceed\|Do you want" && pass "item 6: Claude Code's own prompt is open for the user" \
            || fail "item 6: Claude Code shows no permission prompt for the merge"
        key_to claude "<ESC>"
    fi
    SOFT=0
}

# A line stands in one envelope only: after the delivery, a second `lunchroom read` of the same id prints
# the envelope without the line (item 2, N).
card1_once() {
    local id=$1 r1 r2
    r1=$(agent_run Claude "$LR read $id")
    r2=$(agent_run Claude "$LR read $id")
    measured "first read of $id: $(printf '%s' "$r1" | head -c 200 | tr '\n' '|')"
    if printf '%s' "$r2" | grep -qF "$HELD"; then
        fail "item 2 (N): a second 'lunchroom read $id' prints the line '$HELD' again; a line stands in one envelope only"
    else
        pass "item 2 (N): a second 'lunchroom read $id' prints the envelope without the line"
    fi
    [ -n "$r2" ] && pass "item 2 (N): the second read still prints the message" || fail "item 2 (N): the second read printed nothing"
}

# A session that starts anew is a new agent: it is told none of the lines recorded for its old session, and
# is told the definition of HELD again at its first HELD (V1.7, item 1).
card1_renew() {
    local pid T
    pid=$(sed -n 1p "$SESS/claude.ready" 2>/dev/null)
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
    sleep 3
    T=$(now)
    start_agent cc Claude claude2 || return 0
    no_text_in Claude "$HELD" "$T" "item 1 (a session that starts anew is told no line of its old session)"
    agent_state Claude | grep -q "HELD" && fail "item 1: the new session Claude is HELD at once, by the old session's message: $(agent_state Claude | tr '\t' ' ')" \
        || pass "item 1: the new session Claude is not HELD by the old session's message"
    T=$(now)
    agent_run Kimi "$LR send Claude \"Claude, are you back?\"" >/dev/null
    LAST_ENVELOPE=""
    check_envelope Claude "$HELD" "$T" "item 1 (V1.7: the definition of HELD again, to the new session at its first HELD)" "are you back?"
}

# A line is at most 1,024 bytes, a longer agent name being cut: names of 1,000, 1,030 and 2,000 bytes.
card1_long() {
    local len name out line bytes
    for len in 1000 1030 2000; do
        reset_world
        cell_banner "card 1, a line with an agent name of $len bytes"
        name=$(head -c "$len" /dev/zero | tr '\0' 'x')
        start_agent cc Kimi || return 0
        start_agent cc Longone longone "$name" || return 0
        out=$(agent_run Kimi "$LR send $name \"Look at the build.\"")
        line=$(printf '%s\n' "$out" | grep "Lunchroom:" | head -1)
        if [ -z "$line" ]; then
            fail "item 1 (long name $len): send printed no line of Lunchroom's: $(echo "$out" | head -c 200)"
            continue
        fi
        bytes=$(printf '%s' "$line" | wc -c)
        measured "item 1 (long name $len): the first line is $bytes bytes"
        [ "$bytes" -le 1024 ] && pass "item 1 (long name $len): the line is at most 1,024 bytes ($bytes)" || fail "item 1 (long name $len): the line is $bytes bytes, over 1,024"
        printf '%s' "$line" | grep -qE '[0-9]{2}:[0-9]{2} UTC Lunchroom: ' && pass "item 1 (long name $len): the cut line keeps its time, UTC and 'Lunchroom: '" \
            || fail "item 1 (long name $len): the cut line lost its prefix: $(printf '%s' "$line" | head -c 80)"
        printf '%s' "$line" | grep -qF " is HELD." && pass "item 1 (long name $len): the cut line keeps the card's words ' is HELD.'" \
            || fail "item 1 (long name $len): the cut line lost the words 'is HELD.'"
        printf '%s' "$line" | grep -q ')' && pass "item 1 (long name $len): the cut line keeps its agent list brackets" || fail "item 1 (long name $len): the agent list has no closing bracket"
    done
}

# The store of lines passing 64 MiB: below, at and above the size; doctor's added line is not a FAIL and the exit
# code is as it was. The file of the lines is found by its content (it holds a line of Lunchroom's).
card1_doctor64() {
    local f sz target base_out base_rc out rc new
    reset_world
    cell_banner "card 1, the store of lines at 64 MiB"
    start_agent cc Kimi || return 0
    start_agent cc Claude || return 0
    agent_run Kimi "$LR send Claude \"$L1\"" >/dev/null
    sleep 3
    f=$(grep -rl "UTC Lunchroom" "$STOREROOT/$(uid_dev)" 2>/dev/null | head -1)
    if [ -z "$f" ]; then
        fail "item 1 (64 MiB): no file of the store holds a line of Lunchroom's"
        return 0
    fi
    measured "item 1 (64 MiB): the file of the lines is $f, $(stat -c %s "$f") bytes"
    base_out=$(user_pty doctor64a "$LR" doctor)
    pty doctor64b "${DEV[@]}" "$LR" doctor
    base_rc=$?
    for target in $((64 * 1024 * 1024 - 1)) $((64 * 1024 * 1024)) $((64 * 1024 * 1024 + 1)); do
        sz=$(stat -c %s "$f")
        [ "$target" -gt "$sz" ] && head -c $((target - sz)) /dev/zero | tr '\0' '\n' >> "$f"
        out=$(user_pty doctor64c "$LR" doctor)
        pty doctor64d "${DEV[@]}" "$LR" doctor
        rc=$?
        new=$(diff <(printf '%s\n' "$base_out") <(printf '%s\n' "$out") | grep '^>' | sed 's/^> //')
        measured "item 1 (64 MiB): at $target bytes doctor exits $rc (before: $base_rc); new lines: $(printf '%s' "$new" | head -c 300 | tr '\n' '|')"
        [ "$rc" = "$base_rc" ] && pass "item 1 (64 MiB, $target bytes): doctor's exit code is as it was ($rc)" || fail "item 1 (64 MiB, $target bytes): doctor's exit code changed from $base_rc to $rc"
        printf '%s\n' "$new" | grep -q '^FAIL' && fail "item 1 (64 MiB, $target bytes): doctor prints a FAIL line for the size: $(printf '%s' "$new" | head -c 200)" \
            || pass "item 1 (64 MiB, $target bytes): doctor prints no FAIL line for the size"
        if [ "$target" -gt $((64 * 1024 * 1024)) ]; then
            [ -n "$new" ] && pass "item 1 (64 MiB): past 64 MiB doctor prints a line it did not print before" || fail "item 1 (64 MiB): past 64 MiB doctor prints no new line"
        elif [ "$target" -lt $((64 * 1024 * 1024)) ]; then
            [ -z "$new" ] && pass "item 1 (64 MiB): below 64 MiB doctor prints no new line" || fail "item 1 (64 MiB): below 64 MiB doctor prints a new line: $(printf '%s' "$new" | head -c 200)"
        fi
    done
}

card1_main() {
    setup_world
    local kh ch
    for kh in $(harness_list); do
        for ch in $(harness_list); do
            if base_only && ! { [ "$kh" = oc ] && [ "$ch" = cc ]; }; then continue; fi
            card1_cell "$kh" "$ch" yes
        done
    done
    # The cases that need a cell of their own (Claude Code on both sides).
    reset_world
    cell_banner "card 1, a line stands in one envelope only; a session that starts anew"
    if start_agent cc Kimi && start_agent cc Claude; then
        local T0 id
        T0=$(now)
        agent_run Kimi "$LR send Claude \"$L1\"" >/dev/null
        envelope_of Claude "$T0" >/dev/null
        id=$("${TOOL[@]}" ids claude "$T0" | head -1)
        if [ -n "$id" ]; then card1_once "$id"; else fail "item 2 (N): no message id reached Claude"; fi
        card1_renew
    fi
    card1_long
    card1_doctor64
    finish
}
