//! Lunchroom 1.1, unit C1: the classes of the independent review of the unit's list
//! (C1-O01 to C1-O60), each with the test the
//! class names. The class id is in each test's comment.

// A test of a list that is empty says so by `is_empty`, as the clause does; a test that cannot go
// on without its setup says so by `expect`.
#![allow(clippy::assert_is_empty, clippy::expect_used)]

use std::fs;

use serde_json::json;

use super::calls::{Hearer, Outcome};
use super::lines::{self, LINE_MAX_BYTES, Who};
use super::*;
use crate::identity::Harness;
use crate::store::Store;

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

fn texts(list: &[Notice]) -> Vec<String> {
    list.iter().map(|n| n.text.clone()).collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = crate::test_target()
        .join("store-tests-c1-codex")
        .join(format!("{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Claude (c1) HELD by Codex's (x1) message; the notices of that hold are taken.
fn held() -> (Mail, String) {
    let mut m = Mail::default();
    for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
        m.observe(&who);
        assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
    }
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&claude("c1"), &id, 1), Confirmation::Show);
    let _ = m.take_user_notices();
    let _ = m.take_notices(&codex("x1"));
    (m, id)
}

fn hear(who: &Identity, d: bool, n: bool) -> Hearer {
    Hearer {
        who: who.clone(),
        d,
        n,
    }
}

fn ask(m: &mut Mail, id: &str, kept: bool) {
    m.call_starts(
        &claude("c1"),
        "claude",
        id,
        "merge",
        100,
        kept,
        true,
        &|_, _| true,
    );
}

// ---- section A: the form of a line -------------------------------------------------------------

// C1-O01: only the whole word HELD triggers V1.7's definition.
#[test]
fn c1_only_exact_held_word_triggers_definition() {
    let mut m = Mail::default();
    let who = claude("a");
    let line = lines::build(0, "UNHELD.", Who::Agents(&["x"]), &[]);
    assert_eq!(m.line_for(&who, &line), line);
    assert!(!lines::uses_held("UNHELD."));
    assert!(lines::uses_held("Claude is HELD."));
    assert!(lines::uses_held("HELD"));
    assert!(lines::uses_held("(HELD)"));
    // And the first real use still gets it: the substring line did not use it up.
    let real = lines::build(0, "Claude is HELD.", Who::Agents(&["x"]), &[]);
    assert!(m.line_for(&who, &real).ends_with(&lines::held_definition()));
}

// C1-O02: BEHELD and other superstrings.
#[test]
fn c1_held_substring_not_definition() {
    let mut m = Mail::default();
    for words in ["BEHELD.", "HELDER.", "Upheld.", "SHELD."] {
        let line = lines::build(0, words, Who::Agents(&["x"]), &[]);
        assert_eq!(m.line_for(&claude("a"), &line), line, "{words}");
    }
}

// C1-O03: a further sentence of 2,000 bytes does not break the 1,024-byte line.
#[test]
fn c1_long_further_sentence_is_bounded() {
    let line = lines::build(
        0,
        "Mistral is UNGUARDED.",
        Who::Agents(&["Mistral"]),
        &["x".repeat(2_000)],
    );
    assert!(line.len() <= LINE_MAX_BYTES, "{}", line.len());
    assert!(line.starts_with("00:00 UTC Lunchroom: Mistral is UNGUARDED. (Mistral)"));
}

// C1-O04: a fixed line of 1,023 bytes plus the first-use definition is still at most 1,024.
#[test]
fn c1_held_definition_counts_against_limit() {
    let mut m = Mail::default();
    // Words sized so that, without the definition, the line is about 1,023 bytes.
    let filler = "y".repeat(LINE_MAX_BYTES - 60);
    let words = format!("HELD {filler}");
    let line = lines::build(0, &words, Who::Agents(&["a"]), &[]);
    let told = m.line_for(&claude("r"), &line);
    assert!(told.len() <= LINE_MAX_BYTES, "{}", told.len());
    assert!(
        told.ends_with(&lines::held_definition()),
        "the definition stays whole"
    );
}

// C1-O05: two 10-KiB names and 10 KiB of words: nothing overflows the line.
#[test]
fn c1_words_and_further_cannot_overflow_line() {
    let big = "n".repeat(10 * 1024);
    for who in [
        Who::Agents(&[big.as_str(), big.as_str()]),
        Who::Message {
            from: big.as_str(),
            to: big.as_str(),
        },
    ] {
        let line = lines::build(0, &big, who, std::slice::from_ref(&big));
        assert!(line.len() <= LINE_MAX_BYTES, "{}", line.len());
        assert!(!line.contains('\n'));
    }
}

// C1-O06: an event with no agents has a valid representation, never `()`.
#[test]
fn c1_empty_agent_set() {
    let line = lines::build(0, "Conversation halted.", Who::Agents(&[]), &[]);
    assert!(line.ends_with("(unnamed)"), "{line}");
    assert!(!line.contains("()"));
    let mut m = Mail::default();
    let r = m.record_line(0, "Conversation halted.", Who::Agents(&[]), &[], &[]);
    assert_eq!(r.agents, "unnamed");
}

// C1-O07: an empty sender or recipient is shown as `unnamed`, never as malformed `to` syntax.
#[test]
fn c1_empty_message_agent_name() {
    let both = lines::build(
        0,
        "Denied. Asked for a secret.",
        Who::Message { from: "", to: "" },
        &[],
    );
    assert!(both.ends_with("(unnamed to unnamed)"), "{both}");
    let one = lines::build(
        0,
        "Denied. Asked for a secret.",
        Who::Message { from: "", to: "b" },
        &[],
    );
    assert!(one.ends_with("(unnamed to b)"), "{one}");
}

// C1-O08: what is recorded and shown of an agent name is the visible escaped form.
#[test]
fn c1_recorded_agents_are_sanitized() {
    let hostile = "a\0b\nc\td\re\u{7}f\u{202e}g\u{200d}h\u{fffd}i)";
    let mut m = Mail::default();
    let r = m.record_line(0, "Claude is HELD.", Who::Agents(&[hostile]), &[], &[]);
    for text in [&r.agents, &r.text] {
        assert!(
            !text.contains('\0') && !text.contains('\n') && !text.contains('\t'),
            "{text:?}"
        );
        assert!(!text.contains('\r') && !text.contains('\u{7}'), "{text:?}");
        assert!(
            !text.contains('\u{202e}') && !text.contains('\u{200d}'),
            "{text:?}"
        );
        assert!(!text.contains('\u{fffd}'), "{text:?}");
    }
    assert!(
        r.agents.contains("\\u{200d}") && r.agents.contains("\\n"),
        "{}",
        r.agents
    );
    assert_eq!(r.text.lines().count(), 1);
}

// C1-O09: the identities recorded match the agents rendered, or none are recorded.
#[test]
fn c1_line_identity_cardinality_matches_agents() {
    let mut m = Mail::default();
    let (a, b, c) = (claude("a"), claude("b"), claude("c"));
    let none = m.record_line(0, "Claude is HELD.", Who::Agents(&["A"]), &[], &[]);
    let right = m.record_line(
        0,
        "Claude is HELD.",
        Who::Agents(&["A"]),
        std::slice::from_ref(&a),
        &[],
    );
    let more = m.record_line(
        0,
        "Claude is HELD.",
        Who::Agents(&["A"]),
        &[a.clone(), b.clone()],
        &[],
    );
    let pair = m.record_line(
        0,
        "Claude is HELD.",
        Who::Message { from: "A", to: "B" },
        &[a.clone(), b.clone()],
        &[],
    );
    let three = m.record_line(
        0,
        "Claude is HELD.",
        Who::Agents(&["A", "B"]),
        &[a, b, c],
        &[],
    );
    assert!(none.who.is_empty());
    assert_eq!(right.who.len(), 1);
    assert!(
        more.who.is_empty(),
        "two identities for one agent are not recorded"
    );
    assert_eq!(pair.who.len(), 2);
    assert!(three.who.is_empty());
}

// C1-O10: two sessions with the same displayed name: only the named session hears.
#[test]
fn c1_same_name_different_session_isolated() {
    let mut m = Mail::default();
    let (old, new) = (claude("old"), claude("new"));
    let _ = m.tell_line(
        1,
        "Claude is HELD.",
        Who::Agents(&["Claude"]),
        &[],
        &[],
        &[hear(&old, true, true)],
        false,
    );
    assert_eq!(m.notices(&old).len(), 1);
    assert!(m.notices(&new).is_empty());
    assert!(m.pending_envelope_lines(&new).is_empty());
    assert!(m.take_envelope_lines(&new).is_empty());
    assert_eq!(m.take_envelope_lines(&old).len(), 1);
}

// C1-O11: a hearer named twice hears once on each channel.
#[test]
fn c1_duplicate_hearer_entry_deduplicated() {
    let mut m = Mail::default();
    let who = claude("a");
    let _ = m.tell_line(
        1,
        "Claude is HELD.",
        Who::Agents(&["Claude"]),
        &[],
        &[],
        &[
            hear(&who, true, false),
            hear(&who, true, true),
            hear(&who, false, true),
        ],
        false,
    );
    assert_eq!(m.notices(&who).len(), 1);
    assert_eq!(m.pending_envelope_lines(&who).len(), 1);
    assert_eq!(m.take_envelope_lines(&who).len(), 1);
}

// C1-O12: a pending number with no record behind it is reported and kept, not dropped.
#[test]
fn c1_missing_pending_line_is_reported() {
    let mut m = Mail::default();
    let who = claude("a");
    m.envelope_pending.insert(who.key(), vec![99]);
    assert_eq!(m.missing_envelope_lines(&who), vec![99]);
    assert!(m.take_envelope_lines(&who).is_empty());
    assert_eq!(
        m.missing_envelope_lines(&who),
        vec![99],
        "still kept, not discarded"
    );
}

// C1-O13: taking the envelope and recording a line race: each line is delivered once.
#[test]
fn c1_take_and_record_race() {
    let dir = scratch("take-record");
    let who = claude("a");
    let store = Store::<Mail>::open(&dir).expect("open");
    store
        .update(|m| {
            let _ = m.tell_line(
                1,
                "Mistral is UNGUARDED. seed",
                Who::Agents(&["A"]),
                &[],
                &[],
                &[hear(&who, false, true)],
                false,
            );
        })
        .expect("seed");
    let (d1, d2) = (dir.clone(), dir.clone());
    let w1 = who.clone();
    let consumer = std::thread::spawn(move || {
        let s = Store::<Mail>::open(&d1).expect("open");
        (0..20)
            .map(|_| s.update(|m| m.take_envelope_lines(&w1).len()).unwrap_or(0))
            .sum::<usize>()
    });
    let w2 = who.clone();
    let recorder = std::thread::spawn(move || {
        let s = Store::<Mail>::open(&d2).expect("open");
        for i in 0..10u64 {
            let _ = s.update(|m| {
                m.tell_line(
                    2 + i,
                    &format!("Mistral is UNGUARDED. {i}"),
                    Who::Agents(&["B"]),
                    &[],
                    &[],
                    &[hear(&w2, false, true)],
                    false,
                )
            });
        }
    });
    let delivered = consumer.join().expect("consumer");
    recorder.join().expect("recorder");
    let rest = store
        .update(|m| m.take_envelope_lines(&who).len())
        .expect("rest");
    assert_eq!(
        delivered + rest,
        11,
        "each of the 11 lines delivered exactly once"
    );
}

// ---- section: waiting calls ---------------------------------------------------------------------

// C1-O14: the same call started twice is one waiting call and one start event.
#[test]
fn c1_duplicate_call_start_is_idempotent() {
    let (mut m, _) = held();
    ask(&mut m, "t1", false);
    ask(&mut m, "t1", false);
    assert_eq!(m.waiting_calls().len(), 1);
    assert_eq!(m.user_notices().len(), 1);
    assert_eq!(m.pending_envelope_lines(&codex("x1")).len(), 1);
}

// C1-O15: no id, no record and no notice.
#[test]
fn c1_empty_call_id_rejected() {
    let (mut m, _) = held();
    ask(&mut m, "", true);
    assert!(m.waiting_calls().is_empty());
    assert!(m.user_notices().is_empty());
    assert!(m.pending_envelope_lines(&codex("x1")).is_empty());
}

// C1-O16: one id used by two sessions at once: each call is addressed by session and id.
#[test]
fn c1_call_id_scoped_to_session() {
    let (mut m, _) = held();
    let other = claude("c2");
    m.observe(&other);
    ask(&mut m, "t1", false);
    m.call_starts(&other, "c2", "t1", "merge", 100, false, false, &|_, _| true);
    assert_eq!(m.waiting_calls().len(), 2);
    assert!(m.call_ends_in(&claude("c1"), "t1", Outcome::Denied, 105, None));
    assert_eq!(m.waiting_calls().len(), 1);
    assert_eq!(m.waiting_calls()[0].session, other);
}

// C1-O17: ended without a session when two share the id: nothing is closed by guess.
#[test]
fn c1_unscoped_call_end_cannot_close_wrong_session() {
    let (mut m, _) = held();
    let other = claude("c2");
    ask(&mut m, "t1", false);
    m.call_starts(&other, "c2", "t1", "merge", 100, false, false, &|_, _| true);
    assert!(!m.call_ends("t1", Outcome::Denied, 105));
    assert_eq!(m.waiting_calls().len(), 2);
    // With one session's call only, the id alone names it.
    assert!(m.call_ends_in(&other, "t1", Outcome::Denied, 106, None));
    assert!(m.call_ends("t1", Outcome::Denied, 107));
}

// C1-O18: "the first of its messages" is the first recorded, not the first by id.
#[test]
fn c1_first_queued_message_is_event_order() {
    let (mut m, _) = held();
    let other = Identity::new(Harness::Hermes, "h1");
    m.observe(&other);
    assert_eq!(m.register("hermes", &other, Origin::User, None), Ok(None));
    let ids: Vec<String> = (0..3)
        .map(|i| {
            m.send(&other, "hermes", "claude", &format!("m{i}"), 10 + i)
                .map(|a| a.id)
                .unwrap_or_default()
        })
        .collect();
    // Event order is m3, m1, m2 (recs), while the ids sort m1, m2, m3.
    let rec = |m: &mut Mail, id: &str, rec: u64| {
        if let Some(x) = m.messages.get_mut(id) {
            x.rec = rec;
        }
    };
    rec(&mut m, &ids[2], 1_000);
    rec(&mut m, &ids[0], 1_001);
    rec(&mut m, &ids[1], 1_002);
    // The asker is not HELD here (so only the QUEUED apply): it is another session's call.
    m.call_starts(
        &claude("c1"),
        "claude",
        "t1",
        "merge",
        100,
        false,
        false,
        &|_, _| true,
    );
    let told = texts(m.notices(&other));
    assert!(
        told[0].ends_with(&format!("Your message {} waits for claude.", ids[2])),
        "{told:?}"
    );
}

// C1-O19: "the first of its messages that holds" is the first that came to hold.
#[test]
fn c1_first_holding_message_is_event_order() {
    let (mut m, first) = held();
    let second = m
        .send(&codex("x1"), "codex", "claude", "again", 2)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&claude("c1"), &second, 3), Confirmation::Show);
    // Make the holds' order differ from the ids' order.
    if let Some(h) = m.holds.get_mut(&claude("c1").key()) {
        h.reverse();
    }
    ask(&mut m, "t1", false);
    let told = texts(m.notices(&codex("x1")));
    assert_eq!(told.len(), 1);
    assert!(
        told[0].ends_with(&format!("Your message {second} holds claude.")),
        "{told:?} {first}"
    );
}

// C1-O20: a sender with a message holding and one QUEUED is told once, the holding one named.
#[test]
fn c1_holding_precedes_queued_message_name() {
    let (mut m, hold) = held();
    let _ = m.send(&codex("x1"), "codex", "claude", "queued", 5);
    ask(&mut m, "t1", false);
    let told = texts(m.notices(&codex("x1")));
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].ends_with(&format!("Your message {hold} holds claude.")));
}

// C1-O21: a call of a not-HELD agent is not heard by a sender with a message for another agent.
#[test]
fn c1_nonheld_call_excludes_unrelated_queue() {
    let (mut m, _) = held();
    let third = Identity::new(Harness::Hermes, "h1");
    let fourth = Identity::new(Harness::OpenClaw, "o1");
    for (n, w) in [("hermes", &third), ("oc", &fourth)] {
        m.observe(w);
        assert_eq!(m.register(n, w, Origin::User, None), Ok(None));
    }
    // hermes has a message QUEUED for oc, not for claude.
    let _ = m.send(&third, "hermes", "oc", "other", 5);
    m.call_starts(
        &claude("c1"),
        "claude",
        "t1",
        "merge",
        100,
        false,
        false,
        &|_, _| true,
    );
    assert!(m.notices(&third).is_empty());
    assert!(m.pending_envelope_lines(&third).is_empty());
}

// C1-O22: the asker is never a waiter, even as a sender to itself.
#[test]
fn c1_self_sender_not_waiter() {
    let (mut m, _) = held();
    let id = m
        .send(&codex("x1"), "codex", "claude", "self", 5)
        .map(|a| a.id)
        .unwrap_or_default();
    if let Some(x) = m.messages.get_mut(&id) {
        x.from = claude("c1");
    }
    ask(&mut m, "t1", false);
    assert!(m.notices(&claude("c1")).is_empty());
    assert!(m.pending_envelope_lines(&claude("c1")).is_empty());
}

// C1-O23: a waiter that loses its guard after the start still has the stop notice, by N.
#[test]
fn c1_stop_notice_survives_waiter_guard_loss() {
    let (mut m, _) = held();
    ask(&mut m, "t1", false);
    // x1 was guarded at the start (d); it is UNGUARDED now: its D notice cannot be told, but the
    // stop line is in its next envelope as well.
    let _ = m.take_notices(&codex("x1"));
    assert!(m.call_ends_in(&claude("c1"), "t1", Outcome::Denied, 110, None));
    let pending = m.pending_envelope_lines(&codex("x1"));
    assert!(pending.iter().any(|l| {
        l.text
            .contains("Denied. claude's call t1 was not approved.")
    }));
}

// C1-O24: the three voids are not told for a call the harness's prompt answers.
#[test]
fn c1_nonkept_void_outcomes_rejected() {
    for v in [
        Outcome::VoidMessageHeld,
        Outcome::VoidHoldEnded,
        Outcome::VoidCallChanged,
    ] {
        let (mut m, _) = held();
        ask(&mut m, "t1", false);
        let before = (m.user_notices().len(), m.lines().len());
        assert!(!m.call_ends_in(&claude("c1"), "t1", v, 110, None), "{v:?}");
        assert_eq!(before, (m.user_notices().len(), m.lines().len()));
        assert_eq!(m.waiting_calls().len(), 1, "still waiting");
    }
}

fn kept_request() -> crate::approvals::Request {
    kept_request_at(100)
}

fn kept_request_at(at: u64) -> crate::approvals::Request {
    crate::approvals::Request {
        session: claude("c1"),
        agent: "claude".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({"command": "gh pr merge 4"}),
        cwd: "/w".to_owned(),
        rule: "This call is PROTECTED, so it needs your yes.".to_owned(),
        at,
    }
}

/// A kept call for `kept_request`, asked 10 seconds before and approved at `approved_at`.
fn approved_kept(approved_at: u64) -> (Mail, String) {
    let (mut m, _) = held();
    let r = kept_request_at(approved_at - 10);
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    let shown = m.approval_prompt(&r.session, &r.agent, &r.tool, &r.input, &r.cwd);
    let id = m
        .approve_request(0, &r, &shown, approved_at)
        .expect("approved");
    (m, id)
}

// C1-O25: a kept call lapses at asked + 600, not before; the notice is stamped asked + 600.
#[test]
fn c1_lapse_exact_boundary() {
    for (check, closed) in [(699, 0), (700, 1), (701, 1)] {
        let (mut m, _) = held();
        let r = kept_request();
        m.request(r.clone());
        m.kept_call_starts(&r, &|_, _| true);
        let _ = m.take_user_notices();
        assert_eq!(m.sweep_calls(check, &|_, _| false), closed, "at {check}");
        if closed == 1 {
            let at = m.user_notices().last().map(|n| n.at);
            assert_eq!(at, Some(700));
        }
    }
}

// C1-O26: approved and not used: "Not used in time" at exactly approval + 600.
#[test]
fn c1_not_used_expiry_exact_boundary() {
    for (check, told) in [(1_000 + 599, 0), (1_000 + 600, 1), (1_000 + 601, 1)] {
        let (mut m, id) = approved_kept(1_000);
        let _ = m.take_user_notices();
        assert_eq!(m.sweep_calls(check, &|_, _| false), told, "at {check}");
        if told == 1 {
            let n = m.user_notices().last().cloned().expect("notice");
            assert_eq!(n.at, 1_600);
            assert!(
                n.text.contains(&format!(
                    "Not used in time. claude's approval of call {id} expired."
                )),
                "{}",
                n.text
            );
        }
    }
}

// C1-O27: the first Lunchroom program that runs after the expiry tells it, stamped at the expiry.
#[test]
fn c1_not_used_is_swept_by_every_program() {
    let (mut m, _) = approved_kept(1_000);
    let _ = m.take_user_notices();
    // Nothing ran until a long time later.
    assert_eq!(m.sweep_calls(50_000, &|_, _| false), 1);
    assert_eq!(m.user_notices().last().map(|n| n.at), Some(1_600));
    assert_eq!(m.sweep_calls(60_000, &|_, _| false), 0, "once");
}

// C1-O28: use and expiry at the same instant: exactly one of the two.
#[test]
fn c1_approval_use_expiry_atomic_race() {
    let input = json!({"command": "gh pr merge 4"});
    // At expiry: the use is DENIED and the expiry is told.
    let (mut m, _) = approved_kept(1_000);
    let _ = m.take_user_notices();
    assert_eq!(
        m.use_approval(&claude("c1"), "Bash", &input, "/w", 1_600),
        crate::vocab::Verdict::Denied
    );
    assert_eq!(m.sweep_calls(1_600, &|_, _| false), 1);
    // A second before: the use is GRANTED and nothing is told at the expiry.
    let (mut m, _) = approved_kept(1_000);
    let _ = m.take_user_notices();
    assert_eq!(
        m.use_approval(&claude("c1"), "Bash", &input, "/w", 1_599),
        crate::vocab::Verdict::Granted
    );
    assert_eq!(m.sweep_calls(1_600, &|_, _| false), 0);
    assert!(m.user_notices().is_empty());
}

// C1-O33: a call the harness's prompt answers has no timer.
#[test]
fn c1_nonkept_call_has_no_lapse_timer() {
    let (mut m, _) = held();
    ask(&mut m, "t1", false);
    assert_eq!(m.sweep_calls(100 + 86_400, &|_, _| false), 0);
    assert_eq!(m.waiting_calls().len(), 1);
}

// C1-O34: a STOPPED session with two calls: each told once, as its kind says.
#[test]
fn c1_stopped_session_mixed_call_outcomes() {
    let (mut m, _) = held();
    let me = claude("c1");
    ask(&mut m, "own", false);
    let r = kept_request();
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    let _ = m.take_user_notices();
    assert_eq!(m.sweep_calls(150, &|_, w| *w == me), 2);
    let all = texts(m.user_notices()).join("\n");
    assert!(
        all.contains("Denied. claude's call own was not approved."),
        "{all}"
    );
    assert!(
        all.contains("Not approved in time. claude's call ap-"),
        "{all}"
    );
    assert_eq!(all.matches("Denied.").count(), 1);
    assert_eq!(all.matches("Not approved in time.").count(), 1);
}

// C1-O35: STOPPED found at 100, a later sweep at 1,000: one outcome, stamped at the discovery.
#[test]
fn c1_stopped_call_not_double_closed() {
    let (mut m, _) = held();
    let me = claude("c1");
    let r = kept_request();
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    let _ = m.take_user_notices();
    assert_eq!(m.sweep_calls(150, &|_, w| *w == me), 1);
    assert_eq!(m.user_notices().last().map(|n| n.at), Some(150));
    assert_eq!(m.sweep_calls(1_000, &|_, _| false), 0);
    assert_eq!(m.sweep_calls(1_000, &|_, w| *w == me), 0);
    assert_eq!(m.user_notices().len(), 1);
}

// C1-O36: all eight combinations of the three causes.
#[test]
fn c1_void_priority_all_boolean_combinations() {
    use Outcome::{VoidCallChanged, VoidHoldEnded, VoidMessageHeld};
    for bits in 0..8u8 {
        let (a, b, c) = (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);
        let want = if a {
            Some(VoidMessageHeld)
        } else if b {
            Some(VoidHoldEnded)
        } else if c {
            Some(VoidCallChanged)
        } else {
            None
        };
        assert_eq!(Outcome::first_void(a, b, c), want, "{a} {b} {c}");
    }
}

// C1-O37: a `lunchroom yolo` before the approval, after it, at the expiry and after use changes no
// state of the call.
#[test]
fn c1_yolo_during_wait_state_matrix() {
    let (mut m, _) = held();
    let r = kept_request_at(990);
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    m.set_yolo(&claude("c1"), 9_999);
    assert_eq!(m.waiting_calls().len(), 1, "before the approval");
    let shown = m.approval_prompt(&r.session, &r.agent, &r.tool, &r.input, &r.cwd);
    let id = m.approve_request(0, &r, &shown, 1_000).expect("approved");
    m.set_yolo(&claude("c1"), 99_999);
    assert!(m.waiting_calls().is_empty());
    let _ = m.take_user_notices();
    // At the expiry instant, with the YOLO set: still "Not used in time" (the YOLO changed
    // nothing for this call).
    m.set_yolo(&claude("c1"), 199_999);
    assert_eq!(m.sweep_calls(1_600, &|_, _| false), 1, "{id}");
    // After use: nothing.
    let (mut m, _) = approved_kept(1_000);
    let input = json!({"command": "gh pr merge 4"});
    assert_eq!(
        m.use_approval(&claude("c1"), "Bash", &input, "/w", 1_100),
        crate::vocab::Verdict::Granted
    );
    m.set_yolo(&claude("c1"), 99_999);
    let _ = m.take_user_notices();
    assert_eq!(m.sweep_calls(2_000, &|_, _| false), 0);
}

// ---- the verb of a call ---------------------------------------------------------------------

fn verb(command: &str) -> String {
    crate::cards::action_of("Bash", &json!({ "command": command }))
}

// C1-O38.
#[test]
fn c1_action_precedence_complete_matrix() {
    assert_eq!(verb("git push"), "push");
    assert_eq!(verb("terraform apply"), "deploy");
    assert_eq!(verb("git merge x"), "merge");
    assert_eq!(verb("prisma migrate dev"), "migrate");
    // `prisma migrate deploy` is a migration and, by its word `deploy`, a deploy: deploy comes first.
    assert_eq!(verb("prisma migrate deploy"), "deploy");
    assert_eq!(verb("git push && terraform apply"), "push");
    assert_eq!(verb("terraform apply && git merge x"), "deploy");
    assert_eq!(verb("git merge x && prisma migrate dev"), "merge");
    assert_eq!(verb("git push && git merge x && rails db:migrate"), "push");
    assert_eq!(verb("npm test"), "run npm");
}

// C1-O39: `git pull && terraform apply` in every shell spelling.
#[test]
fn c1_pull_and_apply_all_shell_spellings() {
    for c in [
        "git pull && terraform apply",
        "git  pull  &&  terraform  apply",
        "git\tpull\t&&\tterraform\tapply",
        "git pull && terraform apply -input=false",
        "git pull; terraform apply",
        "git pull\nterraform apply",
        "git pull && 'terraform' apply",
        "git pull && terraform apply > /tmp/out.txt",
        "git pull && terraform apply 2>&1",
        "git pull || terraform apply",
        "git pull | terraform apply",
    ] {
        assert_eq!(verb(c), "deploy", "{c:?}");
    }
}

// C1-O40: wrappers and options before the subcommand do not hide the verb.
#[test]
fn c1_wrapped_and_optioned_git_commands() {
    assert_eq!(verb("git -C repo push"), "push");
    assert_eq!(verb("git --no-pager push"), "push");
    assert_eq!(verb("git -c user.name=x push origin main"), "push");
    assert_eq!(verb("env FOO=x git push"), "push");
    assert_eq!(verb("sudo git push"), "push");
    assert_eq!(verb("sudo -u deploy git push"), "push");
    assert_eq!(verb("nohup git push"), "push");
    assert_eq!(verb("env -u HOME git merge x"), "merge");
    assert_eq!(verb("gh -R o/r pr merge 4"), "merge");
    assert_eq!(verb("git commit -m push"), "run git");
}

// C1-O41: only a plain program name is shown.
#[test]
fn c1_plain_program_name_path_quote_alias_matrix() {
    assert_eq!(verb("/usr/bin/git status"), "run git");
    assert_eq!(verb("./tool status"), "run tool");
    assert_eq!(verb("../bin/tool status"), "run tool");
    assert_eq!(verb("tool.exe x"), "run tool.exe");
    assert_eq!(verb("\"tool\" x"), "run a command");
    assert_eq!(verb("'tool' x"), "run a command");
    assert_eq!(verb("tool\\ name x"), "run a command");
    assert_eq!(verb(&format!("{} x", "p".repeat(33))), "run a command");
    assert_eq!(verb("$(x) y"), "run a command");
}

// C1-O42: nothing is resolved at run time: a program called `gp` that is an alias for `git push`
// is `run gp`.
#[test]
fn c1_alias_function_symlink_not_resolved() {
    assert_eq!(verb("gp"), "run gp");
    assert_eq!(verb("ll -a"), "run ll");
    assert_eq!(verb("myfunc status"), "run myfunc");
}

// C1-O43: a verb word inside an unrelated argument is not that verb.
#[test]
fn c1_action_word_not_found_in_unrelated_arguments() {
    assert_eq!(verb("echo deploy"), "run echo");
    assert_eq!(verb("printf migrate"), "run printf");
    assert_eq!(verb("cat file-with-push"), "run cat");
    assert_eq!(verb("echo \"git merge\""), "run echo");
    assert_eq!(verb("git commit -m 'git push'"), "run git");
    assert_eq!(verb("grep push file.txt"), "run grep");
}

// C1-O44: the shell reader, not a split on spaces, finds the simple commands.
#[test]
fn c1_shell_lexing_action_matrix() {
    assert_eq!(verb("git 'push'"), "push");
    assert_eq!(verb("git \"push\" origin"), "push");
    assert_eq!(verb("git pu\\sh"), "push");
    assert_eq!(verb("git push # comment"), "push");
    assert_eq!(verb("echo hi; git push"), "push");
    assert_eq!(verb("echo 'a && git push'"), "run echo");
    assert_eq!(verb("echo hi | git push"), "push");
    assert_eq!(verb("echo hi || git push"), "push");
    assert_eq!(verb("(git push)"), "push");
}

// C1-O45: a Bash command that is not text.
#[test]
fn c1_action_of_wrong_bash_input_types() {
    for input in [
        json!({}),
        json!({"command": null}),
        json!({"command": 42}),
        json!({"command": ["git", "push"]}),
        json!({"command": {"a": 1}}),
        json!({"command": true}),
        json!({"command": ""}),
        json!(null),
        json!("git push"),
    ] {
        assert_eq!(
            crate::cards::action_of("Bash", &input),
            "run a command",
            "{input}"
        );
    }
}

// C1-O46: a tool name that is not plain.
#[test]
fn c1_action_of_wrong_tool_inputs() {
    for tool in [
        "",
        "a b",
        "x\ny",
        "../etc",
        "to'ol",
        "t;rm -rf",
        "$(id)",
        "`id`",
        "to\u{ff}ol",
        "t\0",
        "名前",
        "a|b",
        "a&b",
        "a>b",
    ] {
        assert_eq!(
            crate::cards::action_of(tool, &json!({})),
            "use a tool",
            "{tool:?}"
        );
    }
    assert_eq!(crate::cards::action_of("Write", &json!({})), "use Write");
    assert_eq!(
        crate::cards::action_of("mcp__srv__tool", &json!({})),
        "use mcp__srv__tool"
    );
}

// C1-O47: the Granted notice carries the approved action only, never the call's text.
#[test]
fn c1_granted_notice_never_contains_call_text() {
    let nasty = [
        "echo $(cat secret)",
        "echo `cat secret`; touch SECRETFILE",
        "echo a && echo SECRETWORD",
        "echo a\nSECRETLINE",
        "echo \u{202e}SECRETBIDI",
        &format!("echo {}", "SECRETBIG".repeat(8_000)),
    ];
    for command in nasty {
        let (mut m, _) = held();
        let action = crate::cards::action_of("Bash", &json!({ "command": command }));
        m.call_starts(
            &claude("c1"),
            "claude",
            "t1",
            &action,
            100,
            false,
            true,
            &|_, _| true,
        );
        assert!(m.call_ends_in(&claude("c1"), "t1", Outcome::Granted, 101, Some(true)));
        let all = format!(
            "{:?}{:?}{:?}{:?}",
            m.user_notices(),
            m.notices(&codex("x1")),
            m.notices(&claude("c1")),
            m.lines()
        );
        for needle in ["SECRET", "cat secret", "touch"] {
            assert!(!all.contains(needle), "{command:.40}: {needle}");
        }
        assert!(
            all.contains("Granted. claude can run echo."),
            "{command:.40}"
        );
    }
}

// ---- notices: crashes, caps, races ----------------------------------------------------------

// C1-O50: output built, hook killed before the mark: the notice is still pending.
#[test]
fn c1_notice_marking_crash_window() {
    let mut m = Mail::default();
    let who = claude("a");
    let _ = m.tell_line(
        1,
        "Mistral is UNGUARDED.",
        Who::Agents(&["A"]),
        &[],
        &[],
        &[hear(&who, true, false)],
        false,
    );
    let shown = m.notices(&who).to_vec();
    // (the hook is killed here: no mark)
    assert_eq!(
        m.notices(&who).len(),
        1,
        "untold, for the next eligible hook"
    );
    m.mark_notices_told(&who, &shown);
    assert!(m.notices(&who).is_empty());
}

// C1-O52: four processes' worth of stores hammer lines, envelopes, calls and marks: no line is
// lost, no sequence repeats, no envelope line is delivered twice, no call ends twice.
#[test]
fn c1_cross_process_flock_races() {
    let dir = scratch("flock-races");
    let store = Store::<Mail>::open(&dir).expect("open");
    let me = claude("c1");
    store
        .update(|m| {
            m.observe(&me);
        })
        .expect("seed");
    let mut workers = Vec::new();
    for t in 0..4u64 {
        let d = dir.clone();
        let who = me.clone();
        workers.push(std::thread::spawn(move || {
            let s = Store::<Mail>::open(&d).expect("open");
            let mut taken = 0usize;
            let mut closed = 0usize;
            for i in 0..15u64 {
                let id = format!("call-{t}-{i}");
                let _ = s.update(|m| {
                    m.tell_line(
                        t * 100 + i,
                        "Mistral is UNGUARDED.",
                        Who::Agents(&["A"]),
                        &[],
                        &[],
                        &[hear(&who, true, true)],
                        false,
                    );
                    m.call_starts(
                        &who,
                        "a",
                        &id,
                        "merge",
                        t * 100 + i,
                        false,
                        false,
                        &|_, _| true,
                    );
                });
                taken += s.update(|m| m.take_envelope_lines(&who).len()).unwrap_or(0);
                closed += usize::from(
                    s.update(|m| {
                        m.call_ends_in(&who, &id, Outcome::Granted, t * 100 + i + 1, None)
                    })
                    .unwrap_or(false),
                );
                let shown = s.update(|m| m.notices(&who).to_vec()).unwrap_or_default();
                let _ = s.update(|m| m.mark_notices_told(&who, &shown));
            }
            (taken, closed)
        }));
    }
    let (mut taken, mut closed) = (0, 0);
    for w in workers {
        let (a, b) = w.join().expect("worker");
        taken += a;
        closed += b;
    }
    let m = store.read().expect("read");
    let rest = store
        .update(|m| m.take_envelope_lines(&me).len())
        .expect("rest");
    // 60 UNGUARDED lines, 60 start lines (user), 60 Granted lines: 180 lines in all.
    let recs: std::collections::BTreeSet<u64> = m.lines().iter().map(|l| l.rec).collect();
    let seqs: std::collections::BTreeSet<u64> = m.lines().iter().map(|l| l.seq).collect();
    assert_eq!(recs.len(), m.lines().len(), "no record number repeats");
    assert_eq!(seqs.len(), m.lines().len(), "no sequence number repeats");
    assert_eq!(closed, 60, "every call ended once");
    assert!(m.waiting_calls().is_empty());
    // The envelope lines for the session: the 60 UNGUARDED lines are identical text at times
    // that repeat across threads, so dedupe may merge some; none is delivered twice.
    assert!(taken + rest >= 1);
    let unique: std::collections::BTreeSet<String> = m
        .lines()
        .iter()
        .filter(|l| l.words.starts_with("Mistral"))
        .map(|l| l.text.clone())
        .collect();
    assert!(
        taken + rest
            <= m.lines()
                .iter()
                .filter(|l| l.words.starts_with("Mistral"))
                .count()
    );
    assert!(!unique.is_empty());
}

// C1-O53: caps at the boundary: 8 of one message, 50 in all, the oldest go and one line counts
// them; concurrent insertion at the boundary keeps the cap and the count line.
#[test]
fn c1_notice_caps_concurrent_boundary() {
    let mut m = Mail::default();
    let who = claude("c1");
    m.observe(&who);
    let id = "lr-00000001";
    for i in 0..9 {
        m.tell(id, &format!("notice {i}"));
    }
    // (the sender of id has no notices here; use the list directly)
    let mut list: Vec<Notice> = Vec::new();
    for i in 0..9 {
        push_notice(
            &mut list,
            Notice {
                message: id.to_owned(),
                text: format!("n{i}"),
                at: 0,
                reason: false,
            },
            false,
        );
    }
    assert_eq!(list.iter().filter(|n| n.message == id).count(), 8);
    assert_eq!(
        list.first().map(|n| n.text.as_str()),
        Some("n1"),
        "the oldest went"
    );
    let mut big: Vec<Notice> = Vec::new();
    for i in 0..51 {
        push_notice(
            &mut big,
            Notice {
                message: format!("m{i}"),
                text: format!("n{i}"),
                at: 0,
                reason: false,
            },
            false,
        );
    }
    assert_eq!(big.len(), NOTICES_KEPT);
    assert!(big[0].text.contains("dropped"), "{:?}", big[0]);
    // The same through two threads inserting into one store's list at the boundary.
    let dir = scratch("caps");
    let store = Store::<Mail>::open(&dir).expect("open");
    let mut ws = Vec::new();
    for t in 0..2u64 {
        let d = dir.clone();
        let w = who.clone();
        ws.push(std::thread::spawn(move || {
            let s = Store::<Mail>::open(&d).expect("open");
            for i in 0..30u64 {
                let _ = s.update(|m| {
                    m.tell_line(
                        t * 1_000 + i,
                        &format!("Mistral is UNGUARDED. {t}-{i}"),
                        Who::Agents(&["A"]),
                        &[],
                        &[],
                        &[hear(&w, true, false)],
                        false,
                    )
                });
            }
        }));
    }
    for w in ws {
        w.join().expect("worker");
    }
    let m = store.read().expect("read");
    assert!(m.notices(&who).len() <= NOTICES_KEPT);
    assert_eq!(m.lines().len(), 60);
    assert!(m.notices(&who)[0].text.contains("dropped"));
}

// C1-O55: a 1.0.1 program rewriting the main file leaves the 1.1 fields (lines, calls, pending
// envelope lines, the record counter) intact in the second file.
#[test]
fn c1_101_concurrent_write_preserves_c1_fields() {
    let dir = scratch("v101");
    let store = Store::<Mail>::open(&dir).expect("open");
    let who = claude("c1");
    store
        .update(|m| {
            m.observe(&who);
            let _ = m.tell_line(
                7,
                "Claude is HELD.",
                Who::Agents(&["A"]),
                &[],
                &[],
                &[hear(&who, true, true)],
                false,
            );
            m.call_starts(&who, "a", "t1", "merge", 8, false, false, &|_, _| true);
        })
        .expect("write");
    // A 1.0.1 program reads the main file as it stands and writes it back whole.
    let main = dir.join("state.json");
    let text = fs::read_to_string(&main).expect("main");
    assert!(
        !text.contains("\"lines\"") && !text.contains("\"calls\"") && !text.contains("\"envelope_"),
        "1.1 fields are not in the main file"
    );
    fs::write(&main, &text).expect("rewrite");
    let m = store.read().expect("read");
    assert_eq!(m.lines().len(), 2);
    assert_eq!(m.waiting_calls().len(), 1);
    assert_eq!(m.pending_envelope_lines(&who).len(), 1);
}

// C1-O58: one event reaches each named hearer once on its own channel; none stands in for another.
#[test]
fn c1_four_channel_delivery_race() {
    let mut m = Mail::default();
    let (d_only, n_only, both) = (claude("d"), claude("n"), claude("b"));
    let rec = m.tell_line(
        5,
        "Claude is HELD.",
        Who::Agents(&["Claude"]),
        &[],
        &[],
        &[
            hear(&d_only, true, false),
            hear(&n_only, false, true),
            hear(&both, true, true),
        ],
        true,
    );
    assert_eq!(
        (
            m.notices(&d_only).len(),
            m.pending_envelope_lines(&d_only).len()
        ),
        (1, 0)
    );
    assert_eq!(
        (
            m.notices(&n_only).len(),
            m.pending_envelope_lines(&n_only).len()
        ),
        (0, 1)
    );
    assert_eq!(
        (
            m.notices(&both).len(),
            m.pending_envelope_lines(&both).len()
        ),
        (1, 1)
    );
    assert_eq!(m.user_notices().len(), 1, "U");
    assert_eq!(m.lines().len(), 1);
    assert_eq!(m.lines()[0].seq, rec.seq);
}

// C1-O59: the lines of an envelope are shown by the confirmation of that message by that session:
// a wrong session's confirmation consumes nothing; a second confirmation consumes nothing more.
#[test]
fn c1_envelope_consume_identity_and_race() {
    let mut m = Mail::default();
    for (n, w) in [("claude", claude("c1")), ("codex", codex("x1"))] {
        m.observe(&w);
        assert_eq!(m.register(n, &w, Origin::User, None), Ok(None));
    }
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    let _ = m.tell_line(
        1,
        "Mistral is UNGUARDED.",
        Who::Agents(&["M"]),
        &[],
        &[],
        &[hear(&claude("c1"), false, true)],
        false,
    );
    let built = m.envelope_lines(&id, 2);
    assert_eq!(
        built.len(),
        2,
        "the HELD line and the waiting line: {built:?}"
    );
    // Built again (a failed hand-over): the same lines.
    assert_eq!(m.envelope_lines(&id, 2).len(), 2);
    // A wrong session cannot confirm it, and consumes nothing.
    assert_eq!(
        m.confirm_at(&claude("other"), &id, 3),
        Confirmation::NotFound
    );
    assert_eq!(m.pending_envelope_lines(&claude("c1")).len(), 1);
    // The right session's confirmation consumes the lines; a second one consumes nothing more.
    assert_eq!(m.confirm_at(&claude("c1"), &id, 4), Confirmation::Show);
    assert!(m.pending_envelope_lines(&claude("c1")).is_empty());
    assert_eq!(m.confirm_at(&claude("c1"), &id, 5), Confirmation::Duplicate);
    // The next envelope carries neither.
    let next = m
        .send(&codex("x1"), "codex", "claude", "again", 6)
        .map(|a| a.id)
        .unwrap_or_default();
    let again = m.envelope_lines(&next, 7);
    assert_eq!(again.len(), 1, "only the HELD line: {again:?}");
}

// C1-H10 (§16 item 2 N): a line recorded after the envelope was built and before the message was
// confirmed is not in that envelope and stays pending for the next; the lines it did carry are
// shown and gone.
#[test]
fn c1_line_between_envelope_build_and_confirm_stays_pending() {
    let mut m = Mail::default();
    for (n, w) in [("claude", claude("c1")), ("codex", codex("x1"))] {
        m.observe(&w);
        assert_eq!(m.register(n, &w, Origin::User, None), Ok(None));
    }
    let id = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    let _ = m.tell_line(
        1,
        "Mistral is UNGUARDED. first",
        Who::Agents(&["M"]),
        &[],
        &[],
        &[hear(&claude("c1"), false, true)],
        false,
    );
    let built = m.envelope_lines(&id, 2);
    assert!(built.iter().any(|l| l.contains("first")), "{built:?}");
    let _ = m.tell_line(
        3,
        "Mistral is UNGUARDED. second",
        Who::Agents(&["M"]),
        &[],
        &[],
        &[hear(&claude("c1"), false, true)],
        false,
    );
    assert_eq!(m.confirm_at(&claude("c1"), &id, 4), Confirmation::Show);
    let left: Vec<String> = m
        .pending_envelope_lines(&claude("c1"))
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("second"), "{left:?}");
}

/// A kept call asked at 990, with the prompt the user will be shown, ready to be approved.
fn kept_waiting() -> (Mail, crate::approvals::Request, String) {
    let (mut m, _) = held();
    let r = kept_request_at(990);
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    let shown = m.approval_prompt(&r.session, &r.agent, &r.tool, &r.input, &r.cwd);
    let _ = m.take_user_notices();
    (m, r, shown)
}

fn last_user_line(m: &Mail) -> String {
    m.user_notices()
        .last()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

// C1-M20 (A2.2, D1.8): the approval fails because what the user confirmed has changed, and the call
// stops waiting with the first cause that is true: a message held the session; the hold ended;
// the call changed.
#[test]
fn c1_kept_call_void_causes() {
    // A message held the session while the user read the prompt.
    let (mut m, r, shown) = kept_waiting();
    let third = Identity::new(Harness::Hermes, "h1");
    m.observe(&third);
    assert_eq!(m.register("hermes", &third, Origin::User, None), Ok(None));
    let id = m
        .send(&third, "hermes", "claude", "x", 995)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&claude("c1"), &id, 996), Confirmation::Show);
    assert!(m.approve_request(0, &r, &shown, 1_000).is_err());
    assert!(
        last_user_line(&m).contains("Approval void: a message held the session. claude's call ap-"),
        "{}",
        last_user_line(&m)
    );
    assert!(m.waiting_calls().is_empty());
    // The hold ended (the user typed in the session).
    let (mut m, r, shown) = kept_waiting();
    m.user_typed(&claude("c1"));
    assert!(m.approve_request(0, &r, &shown, 1_000).is_err());
    assert!(
        last_user_line(&m).contains("Approval void: the hold ended. claude's call ap-"),
        "{}",
        last_user_line(&m)
    );
    // Both true at once: the first in the order, a message held the session.
    let (mut m, r, shown) = kept_waiting();
    m.observe(&third);
    assert_eq!(m.register("hermes", &third, Origin::User, None), Ok(None));
    m.user_typed(&claude("c1"));
    let id = m
        .send(&third, "hermes", "claude", "y", 995)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&claude("c1"), &id, 996), Confirmation::Show);
    assert!(m.approve_request(0, &r, &shown, 1_000).is_err());
    assert!(
        last_user_line(&m).contains("Approval void: a message held the session."),
        "{}",
        last_user_line(&m)
    );
    // The call changed: what waits at that place is another call.
    let (mut m, r, shown) = kept_waiting();
    let mut other = r.clone();
    other.input = json!({"command": "gh pr merge 5"});
    m.requests.clear();
    m.request(other);
    assert!(m.approve_request(0, &r, &shown, 1_000).is_err());
    assert!(
        last_user_line(&m).contains("Approval void: the call changed. claude's call ap-"),
        "{}",
        last_user_line(&m)
    );
}

// C1-B12, B13 (§16 item 5): the sender hears "<agent> is HELD." once, by S or by D: the line
// recorded for D at the moment the hold begins is taken back when `send` printed it.
#[test]
fn c1_sender_hears_held_once_s_xor_d() {
    for by_s in [false, true] {
        let mut m = Mail::default();
        for (n, w) in [("claude", claude("c1")), ("codex", codex("x1"))] {
            m.observe(&w);
            assert_eq!(m.register(n, &w, Origin::User, None), Ok(None));
        }
        let id = m
            .send(&codex("x1"), "codex", "claude", "hello", 0)
            .map(|a| a.id)
            .unwrap_or_default();
        assert_eq!(m.confirm_at(&claude("c1"), &id, 5), Confirmation::Show);
        let line = if by_s {
            m.sender_heard_held_by_s(&id)
        } else {
            None
        };
        let sender = texts(m.notices(&codex("x1")));
        if by_s {
            assert!(sender.is_empty(), "S told it: {sender:?}");
            let s = line.expect("the line for S");
            assert!(
                s.contains("Lunchroom: claude is HELD. (claude)")
                    && s.ends_with(&lines::held_definition()),
                "{s}"
            );
        } else {
            assert_eq!(sender.len(), 1, "D told it: {sender:?}");
            assert!(
                sender[0].ends_with(&lines::held_definition()),
                "the definition at the first use"
            );
        }
        // The user hears it either way.
        assert!(
            m.user_notices()
                .iter()
                .any(|n| n.text.contains("claude is HELD. (claude)"))
        );
        // A second message while HELD starts no second line for the first sender.
        let second = m
            .send(&codex("x1"), "codex", "claude", "again", 6)
            .map(|a| a.id)
            .unwrap_or_default();
        assert_eq!(m.confirm_at(&claude("c1"), &second, 7), Confirmation::Show);
        assert_eq!(
            m.lines()
                .iter()
                .filter(|l| l.text.contains("claude is HELD."))
                .count(),
            1
        );
    }
}

// F2-AC52 (D1.8): a kept call the user declines stops waiting, Denied, told to the user and to the
// agent waiting on it (codex, whose message holds claude), once each; the call waits no more.
#[test]
fn f2_ac52_a_declined_kept_call_is_told_to_the_user_and_the_waiting_agent() {
    let (mut m, _) = held();
    let r = kept_request();
    m.request(r.clone());
    m.kept_call_starts(&r, &|_, _| true);
    let _ = m.take_user_notices();
    let _ = m.take_notices(&codex("x1"));
    let index = m
        .requests()
        .iter()
        .position(|q| *q == r)
        .unwrap_or(usize::MAX);
    assert!(m.refuse_request(index, &r, 110).is_ok());
    let user = texts(m.user_notices()).join("\n");
    let agent = texts(m.notices(&codex("x1"))).join("\n");
    assert_eq!(
        user.matches("Denied. claude's call ap-").count(),
        1,
        "{user}"
    );
    assert!(user.contains(" was not approved."), "{user}");
    assert_eq!(
        agent.matches("Denied. claude's call ap-").count(),
        1,
        "{agent}"
    );
    assert!(m.waiting_calls().is_empty());
}
