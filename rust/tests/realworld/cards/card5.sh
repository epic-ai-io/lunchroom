#!/bin/bash
# Card 5 (spec section 16 items 16 to 18; section 3 R2), sourced by inner.sh.
#
#   Kimi:       "DeepSeek, migration is ready. Run it."
#   Lunchroom:  "YOLO authorized until 14:00 UTC."     (the user typed `lunchroom yolo deepseek till 14:00`)
#   DeepSeek:   "Running the migration now."
#
# Item 16's test: Kimi on Codex sends the first line to DeepSeek on OpenClaw. Item 17's test: the line is in
# the terminal, in DeepSeek's notices, in its next envelope, in the output of Kimi's `send`, in the user's
# list and on the console, for DeepSeek on OpenClaw and repeated on each of Claude Code, Codex and Hermes.
# Item 18's test: DeepSeek asks to run a migration under the YOLO; the migration runs and no call waits;
# with no YOLO the same call goes to the user and waits.
#
# Where `lunchroom yolo` refuses the agent (its adapter has not passed the test of turning the harness's
# YOLO on and off, R2.2), the cell says so with the program's own refusal and FAILS item 17 for that
# harness: item 17 asks for the line for DeepSeek on each of the four.

C5_L1="DeepSeek, migration is ready. Run it."
C5_L3="Running the migration now."

card5_cell() {
    local kh=$1 dh=$2 T0 T1 T2 T3 out yout end words id mins W0 envd
    reset_world
    cell_banner "card 5: Kimi on $(harness_word "$kh") -> DeepSeek on $(harness_word "$dh")"
    start_agent "$kh" Kimi || return 0
    start_agent "$dh" DeepSeek || return 0

    # Item 16: line 1, delivered, holds DeepSeek.
    T0=$(now)
    agent_run Kimi "$LR send DeepSeek \"$C5_L1\"" >/dev/null
    LAST_ENVELOPE=""
    check_envelope DeepSeek "DeepSeek is HELD." "$T0" "item 16 (DeepSeek, D1.1)" "$C5_L1"
    agent_state DeepSeek | grep -q HELD && pass "item 16: DeepSeek is HELD by the message" || fail "item 16: DeepSeek is not HELD: $(agent_state DeepSeek | tr '\t' ' ')"
    id=$("${TOOL[@]}" ids deepseek "$T0" | tail -1)

    # Item 17: the user types `lunchroom yolo deepseek till <HH:MM>` in a terminal outside every session.
    end=$(date -u -d '+90 minutes' +%H:%M)
    words="YOLO authorized until $end UTC."
    T1=$(now)
    yout=$(user_yolo DeepSeek "$end")
    T2=$(now)
    measured "lunchroom yolo printed: $(echo "$yout" | head -c 600 | tr '\n' '|')"
    if ! printf '%s' "$yout" | grep -qF "$words"; then
        fail "item 17: 'lunchroom yolo deepseek till $end' on DeepSeek ($(harness_word "$dh")) did not print '$words'; the program printed: $(echo "$yout" | head -c 400 | tr '\n' ' ')"
        return 0
    fi
    pass "item 17: the terminal where the user typed it shows the line"
    line_form_in "$yout" "$words" "$(minutes_of "$T1" "$T2")" "DeepSeek" "item 17 (terminal)"

    # The line reaches DeepSeek by D at the moment the YOLO is set, and by N in its next envelope.
    expect_heard DeepSeek "$words" "$T1" "item 17 (DeepSeek, D)"
    heard_line_form DeepSeek "$words" "DeepSeek" "$T1" "$(now)" "item 17 (DeepSeek, D)"
    T3=$(now)
    out=$(agent_run Kimi "$LR send DeepSeek \"DeepSeek, the backup is done.\"")
    # Kimi, by S in the output of each send to DeepSeek while the YOLO lasts.
    line_form_in "$out" "$words" "$(minutes_of "$T3" "$(now)")" "DeepSeek" "item 17 (Kimi, S: output of a send while the YOLO lasts)"
    LAST_ENVELOPE=""
    check_envelope DeepSeek "$words" "$T3" "item 17 (DeepSeek, N: its next envelope)" "the backup is done."
    # The user: the list and the console.
    user_has "$words" "$T1" "item 17 (U)"
    console_shows "$words" "item 17 (console)"
    user_prompt_shows "$words" "item 17 (U)"
    status | grep -qi "yolo" && pass "item 17: status shows the YOLO and its end (R2.6): $(status | grep -i yolo | head -1 | tr '\t' ' ')" \
        || fail "item 17: status does not show the YOLO"

    # Item 18, line 3: DeepSeek's reply inside the slip; the migration runs under the YOLO with no call waiting.
    local reply=""
    [ -n "$id" ] && reply="--reply-to $id "
    out=$(agent_run DeepSeek "$LR send Kimi ${reply}\"$C5_L3\"")
    printf '%s' "$out" | grep -q "DENIED" && fail "item 18: DeepSeek's reply was DENIED: $(echo "$out" | head -c 300)" \
        || pass "item 18: DeepSeek's reply inside the slip was not DENIED"
    W0=$(witness_count alembic)
    agent_run_async DeepSeek "alembic upgrade head" >/dev/null
    local n=0
    until [ "$(witness_count alembic)" -gt "$W0" ] || [ $n -ge 40 ]; do sleep 1; n=$((n + 1)); done
    [ "$(witness_count alembic)" -gt "$W0" ] && pass "item 18: the migration ran under the YOLO (the fake alembic left its record)" \
        || fail "item 18: the migration did not run under the YOLO"
    if user_approve_list | grep -q "alembic"; then
        fail "item 18: a call waits for the user although the YOLO covers DeepSeek"
    else
        pass "item 18: no call waits for the user"
    fi
}

# The same call with no YOLO goes to the user and waits.
card5_noyolo() {
    local dh=$1 T0 T1 W0
    reset_world
    cell_banner "card 5, no YOLO: Kimi on Codex -> DeepSeek on $(harness_word "$dh")"
    start_agent cx Kimi || return 0
    start_agent "$dh" DeepSeek || return 0
    T0=$(now)
    agent_run Kimi "$LR send DeepSeek \"$C5_L1\"" >/dev/null
    envelope_of DeepSeek "$T0" >/dev/null
    W0=$(witness_count alembic)
    T1=$(now)
    agent_run_async DeepSeek "alembic upgrade head" >/dev/null
    sleep 15
    [ "$(witness_count alembic)" = "$W0" ] && pass "item 18: with no YOLO the migration did not run" || fail "item 18: with no YOLO the migration ran"
    [ "$dh" = cx ] || SOFT=1
    user_has "DeepSeek has a call waiting for you (call" "$T1" "item 18 (U: the call goes to the user and waits)"
    SOFT=0
}

# The Balanced hold mode with and without a covering YOLO, for a merge, a deploy and a migration: while an
# agent is HELD each goes to the user; under a YOLO that covers the agent each runs with no call waiting.
# Claude Code is the agent: the only one `lunchroom yolo` covers (R2.2).
card5_matrix() {
    local kind cmd prog W0 T n end words T1 yout row
    reset_world
    cell_banner "card 5, the matrix: merge, deploy, migration without and with a covering YOLO (DeepSeek on Claude Code)"
    start_agent cx Kimi || return 0
    start_agent cc DeepSeek || return 0
    T=$(now)
    agent_run Kimi "$LR send DeepSeek \"$C5_L1\"" >/dev/null
    envelope_of DeepSeek "$T" >/dev/null
    for row in "merge|gh pr merge 1|gh" "deploy|terraform apply|terraform" "migration|alembic upgrade head|alembic"; do
        IFS='|' read -r kind cmd prog <<< "$row"
        W0=$(witness_count "$prog")
        T1=$(now)
        agent_run_async DeepSeek "$cmd" >/dev/null
        sleep 12
        [ "$(witness_count "$prog")" = "$W0" ] && pass "item 3: a $kind while DeepSeek is HELD, with no YOLO, did not run: it goes to the user" \
            || fail "item 3: a $kind while DeepSeek is HELD, with no YOLO, ran"
        user_has "DeepSeek has a call waiting for you (call" "$T1" "item 3 ($kind with no YOLO: the user is told it waits)"
        key_to deepseek "<ESC>"
        sleep 3
    done
    end=$(date -u -d '+90 minutes' +%H:%M)
    words="YOLO authorized until $end UTC."
    yout=$(user_yolo DeepSeek "$end")
    if ! printf '%s' "$yout" | grep -qF "$words"; then
        fail "item 17: the YOLO for DeepSeek on Claude Code did not print '$words': $(echo "$yout" | head -c 300 | tr '\n' ' ')"
        return 0
    fi
    for row in "merge|gh pr merge 1|gh" "deploy|terraform apply|terraform" "migration|alembic upgrade head|alembic"; do
        IFS='|' read -r kind cmd prog <<< "$row"
        W0=$(witness_count "$prog")
        agent_run_async DeepSeek "$cmd" >/dev/null
        n=0
        until [ "$(witness_count "$prog")" -gt "$W0" ] || [ $n -ge 40 ]; do sleep 1; n=$((n + 1)); done
        [ "$(witness_count "$prog")" -gt "$W0" ] && pass "item 18: a $kind under the covering YOLO ran with no prompt" \
            || fail "item 18: a $kind under the covering YOLO did not run"
        user_approve_list | grep -q "$prog" && fail "item 18: a $kind waits for the user under the covering YOLO" \
            || pass "item 18: no $kind waits for the user under the covering YOLO"
    done
}

# The line of an event with three agents: `A, B and C` (item 1). One YOLO names three Claude Code agents.
card5_three() {
    local end T line
    reset_world
    cell_banner "card 5, a YOLO that names three agents (the form A, B and C)"
    start_agent cc Kimi || return 0
    start_agent cc GLM || return 0
    start_agent cc Claude || return 0
    end=$(date -u -d '+90 minutes' +%H:%M)
    T=$(now)
    line=$(user_yolo "Kimi,GLM,Claude" "$end" | grep -F "YOLO authorized until $end UTC." | head -1)
    measured "the line for three agents: $line"
    if printf '%s' "$line" | grep -qE '\(([A-Za-z]+, )+[A-Za-z]+ and [A-Za-z]+\)'; then
        pass "item 1: the three-agent line has the form (A, B and C)"
    else
        fail "item 1: the three-agent line is not of the form (A, B and C): $line"
    fi
    for n in Kimi GLM Claude; do
        printf '%s' "$line" | grep -qF "$n" && pass "item 1: the three-agent line names $n" || fail "item 1: the three-agent line does not name $n"
    done
}

card5_main() {
    setup_world
    local kh dh
    for kh in $(harness_list); do
        for dh in $(harness_list); do
            if base_only && ! { [ "$kh" = cx ] && [ "$dh" = oc ]; }; then continue; fi
            card5_cell "$kh" "$dh"
        done
    done
    for dh in oc cc; do
        card5_noyolo "$dh"
    done
    card5_matrix
    card5_three
    finish
}
