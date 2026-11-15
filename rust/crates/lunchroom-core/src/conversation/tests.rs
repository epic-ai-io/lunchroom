//! C3: the conversations of the console (§17.10).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)] // test code

use super::*;

fn p(harness: &str, session: &str, name: &str) -> Party {
    Party {
        harness: harness.to_owned(),
        session: session.to_owned(),
        name: name.to_owned(),
    }
}

fn msg(id: u64, at: u64, from: &Party, to: &Party, text: &str) -> MessageIn {
    MessageIn {
        id: format!("lr-{id:08}"),
        at,
        order: id,
        from: from.clone(),
        to: to.clone(),
        text: text.to_owned(),
    }
}

fn line(order: u64, at: u64, agents: &[&Party], text: &str) -> LineIn {
    LineIn {
        id: order,
        at,
        order,
        text: text.to_owned(),
        agents: agents.iter().map(|a| (*a).clone()).collect(),
    }
}

fn texts(c: &Conversation) -> Vec<&str> {
    c.entries.iter().map(|e| e.text.as_str()).collect()
}

fn find<'a>(all: &'a [Conversation], label: &str) -> &'a Conversation {
    all.iter().find(|c| c.label == label).unwrap_or_else(|| {
        panic!(
            "no conversation {label}: {:?}",
            all.iter().map(|c| &c.label).collect::<Vec<_>>()
        )
    })
}

// §17.10: one conversation per pair; messages of both directions and the lines stand in it.
#[test]
fn a_pair_has_one_conversation_of_both_directions() {
    let (k, c) = (
        p("OpenClaw", "k1", "Kimi"),
        p("Claude Code", "c1", "Claude"),
    );
    let all = conversations(
        &[msg(1, 10, &k, &c, "merge"), msg(2, 20, &c, &k, "ok")],
        &[line(1, 15, &[&c, &k], "Claude is HELD.")],
    );
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].label, "Claude and Kimi");
    assert_eq!(texts(&all[0]), ["merge", "Claude is HELD.", "ok"]);
    assert_eq!(all[0].entries[1].kind, EntryKind::Line);
}

// §17.10 (A04): a line naming one agent that has no pair is a conversation of that agent alone,
// listed with the name only, and nothing else is created.
#[test]
fn a_one_agent_line_with_no_pair_is_a_conversation_of_that_agent_alone() {
    let a = p("Codex", "x1", "Mistral");
    let all = conversations(&[], &[line(1, 5, &[&a], "YOLO until 14:00 UTC (Mistral)")]);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].label, "Mistral");
    assert_eq!(all[0].parties.len(), 1);
    assert_eq!(texts(&all[0]), ["YOLO until 14:00 UTC (Mistral)"]);
}

// §17.10: a line naming one agent shows in every pair that includes the agent, and in no singleton
// conversation once the agent has a pair; a pair of others does not show it.
#[test]
fn a_one_agent_line_shows_in_every_pair_of_that_agent_and_makes_no_singleton() {
    let (a, b, c, d) = (
        p("Codex", "a", "A"),
        p("Codex", "b", "B"),
        p("Codex", "c", "C"),
        p("Codex", "d", "D"),
    );
    let all = conversations(
        &[
            msg(1, 1, &a, &b, "m1"),
            msg(2, 2, &a, &c, "m2"),
            msg(3, 3, &c, &d, "m3"),
        ],
        &[line(1, 4, &[&a], "A yolo")],
    );
    assert_eq!(
        all.len(),
        3,
        "{:?}",
        all.iter().map(|x| &x.label).collect::<Vec<_>>()
    );
    assert_eq!(texts(find(&all, "A and B")), ["m1", "A yolo"]);
    assert_eq!(texts(find(&all, "A and C")), ["m2", "A yolo"]);
    assert_eq!(texts(find(&all, "C and D")), ["m3"]);
}

// §17.10: a line naming three agents shows in every pair of them, including a pair that has
// exchanged no message (the pair then exists); it shows in no pair outside them.
#[test]
fn a_three_agent_line_shows_in_every_pair_of_the_three() {
    let (a, b, c, d) = (
        p("Codex", "a", "A"),
        p("Codex", "b", "B"),
        p("Codex", "c", "C"),
        p("Codex", "d", "D"),
    );
    let all = conversations(
        &[msg(1, 1, &a, &b, "m1"), msg(2, 1, &a, &d, "m2")],
        &[line(1, 2, &[&a, &b, &c], "three")],
    );
    assert_eq!(texts(find(&all, "A and B")), ["m1", "three"]);
    assert_eq!(texts(find(&all, "A and C")), ["three"]);
    assert_eq!(texts(find(&all, "B and C")), ["three"]);
    assert_eq!(texts(find(&all, "A and D")), ["m2"]);
    assert_eq!(all.len(), 4);
}

// §17.10 (A10): two sessions of one name are two conversations, each labelled with the name and its
// harness, in every pair of harnesses; nothing of one shows in the other.
#[test]
fn two_sessions_of_one_name_are_two_conversations_labelled_with_the_harness() {
    let harnesses = ["Claude Code", "Codex", "OpenClaw", "Hermes"];
    for h1 in harnesses {
        for h2 in harnesses {
            if h1 == h2 {
                continue;
            }
            let (x1, x2, other) = (
                p(h1, "s1", "Claude"),
                p(h2, "s2", "Claude"),
                p("Codex", "o", "Kimi"),
            );
            let all = conversations(
                &[
                    msg(1, 1, &other, &x1, "to first"),
                    msg(2, 2, &other, &x2, "to second"),
                ],
                &[line(1, 3, &[&x1, &other], "only first")],
            );
            assert_eq!(all.len(), 2, "{h1} {h2}");
            let first = find(&all, &format!("Claude ({h1}) and Kimi"));
            let second = find(&all, &format!("Claude ({h2}) and Kimi"));
            assert_eq!(texts(first), ["to first", "only first"]);
            assert_eq!(texts(second), ["to second"]);
            assert_ne!(first.key, second.key);
        }
    }
}

// §17.10: two sessions of one name in the same harness are told apart by their session id.
#[test]
fn two_sessions_of_one_name_in_one_harness_are_told_apart() {
    let (x1, x2) = (
        p("Codex", "aaaaaaaa11", "Codex"),
        p("Codex", "bbbbbbbb22", "Codex"),
    );
    let o = p("Hermes", "o", "Kimi");
    let all = conversations(&[msg(1, 1, &o, &x1, "1"), msg(2, 2, &o, &x2, "2")], &[]);
    assert_eq!(all.len(), 2);
    assert_ne!(all[0].label, all[1].label);
    assert!(all.iter().any(|c| c.label.contains("aaaaaaaa")));
    assert!(all.iter().any(|c| c.label.contains("bbbbbbbb")));
}

// §17.10: kept by session identity: a session renamed stays one conversation, under its latest name.
#[test]
fn a_renamed_session_stays_one_conversation_under_its_latest_name() {
    let (k, c1, c2) = (
        p("Codex", "k", "Kimi"),
        p("Codex", "c", "Old"),
        p("Codex", "c", "New"),
    );
    let all = conversations(&[msg(1, 1, &k, &c1, "a"), msg(2, 9, &k, &c2, "b")], &[]);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].label, "Kimi and New");
    assert_eq!(texts(&all[0]), ["a", "b"]);
}

// §17.10: time order: a message by its sent time, a line by its event time, whatever the order they
// were given in; equal times in the order recorded, one order over messages and lines (a line
// recorded before a message of the same second stands before it).
#[test]
fn entries_stand_in_time_order_and_equal_times_in_the_order_recorded() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(
        &[
            msg(3, 50, &a, &b, "m-late"),
            msg(1, 10, &a, &b, "m-early"),
            msg(6, 30, &b, &a, "m-30-after-lines"),
            msg(7, 30, &a, &b, "m-30-last"),
            msg(2, 30, &b, &a, "m-30-before-lines"),
        ],
        &[
            line(5, 30, &[&a, &b], "l-30-second"),
            line(4, 30, &[&a, &b], "l-30-first"),
            line(8, 5, &[&a, &b], "l-earliest"),
            line(9, 60, &[&a, &b], "l-latest"),
        ],
    );
    assert_eq!(
        texts(&all[0]),
        [
            "l-earliest",
            "m-early",
            "m-30-before-lines",
            "l-30-first",
            "l-30-second",
            "m-30-after-lines",
            "m-30-last",
            "m-late",
            "l-latest"
        ]
    );
}

// A tie of time and of recorded order (only a store that does not number) puts the message first.
#[test]
fn a_full_tie_puts_the_message_before_the_line() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(&[msg(1, 9, &a, &b, "m")], &[line(1, 9, &[&a, &b], "l")]);
    assert_eq!(texts(&all[0]), ["m", "l"]);
}

// §17.10: Lunchroom's lines stand between the messages they come after.
#[test]
fn a_line_stands_between_the_messages_it_comes_after() {
    let (k, c) = (p("OpenClaw", "k", "Kimi"), p("Claude Code", "c", "Claude"));
    let all = conversations(
        &[
            msg(1, 100, &k, &c, "Claude, merge the PR."),
            msg(2, 160, &c, &k, "Done."),
        ],
        &[
            line(1, 100, &[&c], "Claude is HELD."),
            line(2, 130, &[&c], "Claude is released."),
        ],
    );
    assert_eq!(
        texts(&all[0]),
        [
            "Claude, merge the PR.",
            "Claude is HELD.",
            "Claude is released.",
            "Done."
        ]
    );
}

// §16 item 1, §17.10: nothing is dropped: every message and every line the store holds is in some
// conversation, however many.
#[test]
fn no_message_and_no_line_is_dropped() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let msgs: Vec<MessageIn> = (1..=3_000)
        .map(|i| msg(i, i % 97, &a, &b, &format!("m{i}")))
        .collect();
    let lns: Vec<LineIn> = (1..=3_000)
        .map(|i| line(i, i % 89, &[&a, &b], &format!("l{i}")))
        .collect();
    let all = conversations(&msgs, &lns);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].entries.len(), 6_000);
    let keys: BTreeSet<&str> = all[0].entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys.len(), 6_000);
    assert!(all[0].entries.windows(2).all(|w| w[0].at <= w[1].at));
}

// A line naming no agent belongs to no conversation; one naming the same agent twice is one agent.
#[test]
fn a_line_with_no_agent_is_nowhere_and_a_repeated_agent_counts_once() {
    let a = p("Codex", "a", "A");
    assert_eq!(conversations(&[], &[line(1, 1, &[], "nobody")]), Vec::new());
    let all = conversations(&[], &[line(1, 1, &[&a, &a], "twice")]);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].label, "A");
    assert_eq!(texts(&all[0]), ["twice"]);
}

// A message to oneself is a conversation of that agent alone; a line naming it and another agent is
// not shown there.
#[test]
fn a_message_to_oneself_is_a_conversation_of_one() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(
        &[msg(1, 1, &a, &a, "note")],
        &[
            line(1, 2, &[&a, &b], "pair line"),
            line(2, 3, &[&a], "alone"),
        ],
    );
    assert_eq!(texts(find(&all, "A")), ["note", "alone"]);
    assert_eq!(texts(find(&all, "A and B")), ["pair line", "alone"]);
}

// The conversations are listed most recent first.
#[test]
fn conversations_are_listed_most_recent_first() {
    let (a, b, c) = (
        p("Codex", "a", "A"),
        p("Codex", "b", "B"),
        p("Codex", "c", "C"),
    );
    let all = conversations(&[msg(1, 10, &a, &b, "old"), msg(2, 90, &a, &c, "new")], &[]);
    assert_eq!(all[0].label, "A and C");
    assert_eq!(all[1].label, "A and B");
}

// Names that hold hostile text are carried as they are: showing them safely is the console's.
#[test]
fn hostile_names_are_carried_unchanged() {
    let name = "x\u{1b}[2J\u{202e}\\\u{a0}".repeat(500);
    let (a, b) = (p("Codex", "a", &name), p("Codex", "b", "B"));
    let all = conversations(&[msg(1, 1, &a, &b, "t")], &[]);
    assert_eq!(all.len(), 1);
    assert!(all[0].label.contains(&name));
}

// ---- C3 (Codex's list) ----

// C3-003: a message each way is one unordered conversation.
#[test]
fn c3_pair_is_unordered() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(&[msg(1, 1, &a, &b, "x"), msg(2, 2, &b, &a, "y")], &[]);
    assert_eq!(all.len(), 1);
    assert_eq!(texts(&all[0]), ["x", "y"]);
}

// C3-006, C3-007: four agents are six pairs and no more; a line is counted once in each pair.
#[test]
fn c3_line_four_agents_six_pairs_counted_once_each() {
    let ps: Vec<Party> = ["A", "B", "C", "D"]
        .iter()
        .map(|n| p("Codex", n, n))
        .collect();
    let all = conversations(&[], &[line(1, 5, &ps.iter().collect::<Vec<_>>(), "four")]);
    assert_eq!(all.len(), 6);
    assert!(
        all.iter()
            .all(|c| c.entries.len() == 1 && c.parties.len() == 2)
    );
    let three = conversations(&[], &[line(1, 5, &[&ps[0], &ps[1], &ps[2]], "three")]);
    assert_eq!(three.len(), 3);
    assert!(three.iter().all(|c| c.entries.len() == 1));
}

// C3-008: the time of the last is the greatest event time, whatever the order given.
#[test]
fn c3_last_time_is_max_event_time() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(
        &[msg(1, 20, &a, &b, "x"), msg(2, 10, &a, &b, "y")],
        &[line(3, 30, &[&a, &b], "l")],
    );
    assert_eq!(all[0].last(), 30);
}

// C3-009, C3-010, C3-244: placed by the time sent, ties by the store's one recorded order, never by
// a number guessed from the message id.
#[test]
fn conversation_order_uses_store_sequence() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let mut late_id = msg(9, 100, &a, &b, "recorded-first");
    late_id.order = 1;
    late_id.id = "zzz".to_owned();
    let mut early_id = msg(1, 100, &b, &a, "recorded-third");
    early_id.order = 3;
    let all = conversations(
        &[early_id, late_id],
        &[line(2, 100, &[&a, &b], "recorded-second")],
    );
    assert_eq!(
        texts(&all[0]),
        ["recorded-first", "recorded-second", "recorded-third"]
    );
}

// C3-011, C3-012: 100,001 entries are all there, first and last, in order; the same input twice is
// the same output (nothing is consumed).
#[test]
fn c3_100k_lines_bounded_and_read_only() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let msgs: Vec<MessageIn> = (1..=100_001u64)
        .map(|i| msg(i, i, &a, &b, &format!("m{i}")))
        .collect();
    let first = conversations(&msgs, &[]);
    assert_eq!(first[0].entries.len(), 100_001);
    assert_eq!(first[0].entries[0].text, "m1");
    assert_eq!(first[0].entries[100_000].text, "m100001");
    assert_eq!(conversations(&msgs, &[]), first);
}

// C3-251: a label is made of names only.
#[test]
fn conversation_label_has_only_names() {
    let (a, b) = (p("Codex", "a", "A"), p("Codex", "b", "B"));
    let all = conversations(
        &[msg(1, 1, &a, &b, "UNIQUE-SECRET-1")],
        &[line(2, 2, &[&a, &b], "UNIQUE-SECRET-2")],
    );
    assert!(!all[0].label.contains("UNIQUE"));
}
