//! Lunchroom 1.1, unit U6: the record mark kept at a hand-over (D1.1), the lines that tell an agent
//! of an approval (A1.2) or a refusal (§17.10), the refusal's retry, and no limit on the lines of a
//! message (D1.2). Each test is derived from the acceptance criterion of the node it names and from
//! the failure classes of the clause: an id that does not exist, a read by the wrong session, an
//! approval twice, a refusal and then a retry, each harness.

// A test of a list that is empty says so by `is_empty`, as the clause does.
#![allow(clippy::assert_is_empty)]

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::approvals::{Changed, TellKind, tell_line};
use crate::identity::Harness;
use crate::store::Store;
use crate::vocab::Verdict;

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("store-tests-u6")
        .join(format!("{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Claude (session c1) and Codex (session x1), each seen by a hook and named by the user.
fn pair() -> Mail {
    let mut m = Mail::default();
    for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
        m.observe(&who);
        assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
    }
    m
}

fn waiting(session: &Identity, command: &str, at: u64) -> Request {
    Request {
        session: session.clone(),
        agent: "bob".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({ "command": command }),
        cwd: "/work".to_owned(),
        rule: String::new(),
        at,
    }
}

fn shown_for(m: &Mail, r: &Request) -> String {
    m.approval_prompt(&r.session, &r.agent, &r.tool, &r.input, &r.cwd)
}

fn held_by(m: &Mail, who: &Identity) -> Vec<String> {
    let mut ids: Vec<String> = m.holding(who).iter().map(|h| h.message.clone()).collect();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------------------------
// D1.2 (§10::37): no limit on the number of lines of a message.
// ---------------------------------------------------------------------------------------------

// §10::37 — a message of 64 KiB whose lines are one character each is accepted and
// delivered; one byte more is refused as too long and stores nothing. Failure classes: the most
// lines per byte (all one-character lines, all bare newlines), and the byte boundary.
#[test]
fn a_message_of_64_kib_of_one_character_lines_is_accepted_and_delivered() {
    for text in [
        "a\n".repeat(MAX_MESSAGE_BYTES / 2),
        "\n".repeat(MAX_MESSAGE_BYTES),
    ] {
        assert_eq!(text.len(), MAX_MESSAGE_BYTES);
        let mut m = pair();
        let sent = m.send(&claude("c1"), "claude", "codex", &text, 0);
        assert!(sent.is_ok(), "{:?}", sent.err());
        let id = sent.map(|a| a.id).unwrap_or_default();
        assert_eq!(m.confirm_at(&codex("x1"), &id, 1), Confirmation::Show);
        assert_eq!(m.message(&id).map(|d| d.text.len()), Some(text.len()));
    }
    let mut m = pair();
    let before = m.clone();
    let too_long = "a\n".repeat(MAX_MESSAGE_BYTES / 2) + "a";
    assert_eq!(
        m.send(&claude("c1"), "claude", "codex", &too_long, 0).err(),
        Some(SendRefusal::TooLong)
    );
    assert_eq!(m, before);
}

// ---------------------------------------------------------------------------------------------
// D1.1 (§10::31-33): the record mark kept at the hand-over.
// ---------------------------------------------------------------------------------------------

// §10::31 — a hand-over keeps the length and file identity it was given, or the zeros
// that stand for no record; a message not handed over has none; the mark is kept in the second
// file; an earlier mark stands when the hand-over is made again (an unconfirmed one may have
// reached the record); it goes when the hand-over program failed (nothing reached the record).
#[test]
fn a_hand_over_keeps_the_records_length_and_identity() {
    let mut m = pair();
    let first = m
        .send(&claude("c1"), "claude", "codex", "one", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.handed_at(&first), None);
    let with_record = RecordMark {
        len: 4096,
        dev: 7,
        ino: 99,
    };
    m.handed_over_at(&first, with_record);
    assert_eq!(m.handed_at(&first), Some(with_record));
    assert!(m.message(&first).is_some_and(|d| d.handed_over));
    // No record at the hand-over: all zeros.
    let second = m
        .send(&claude("c1"), "claude", "codex", "two", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    m.handed_over_at(&second, RecordMark::default());
    assert_eq!(m.handed_at(&second), Some(RecordMark::default()));
    // A hand-over nobody confirmed runs out and is made again with a later mark: the earlier stands.
    m.expire_handovers(HANDOVER_SECS + 1);
    assert!(m.message(&first).is_some_and(|d| !d.handed_over));
    assert_eq!(m.handed_at(&first), Some(with_record));
    m.handed_over_at(
        &first,
        RecordMark {
            len: 9000,
            dev: 7,
            ino: 99,
        },
    );
    assert_eq!(m.handed_at(&first), Some(with_record));
    // A hand-over whose program failed reached nothing: the mark goes, and the next is the new one.
    assert!(
        m.attempt_failed(&first, "codex queue exited 1", HANDOVER_SECS + 2)
            .is_some()
    );
    assert_eq!(m.handed_at(&first), None);
    // A message that is not QUEUED keeps none.
    assert_eq!(m.confirm_at(&codex("x1"), &second, 5), Confirmation::Show);
    m.handed_over_at(&second, with_record);
    m.prune(10, |_| true);
    assert!(m.handed_at(&second).is_some());
}

// §10::31, D1.2 — the mark, the lines telling of a decision and the refusals are fields
// of 1.1: they are in the second file and not in the state file, and come back whole.
#[test]
fn the_mark_the_tells_and_the_refusals_are_in_the_second_file() {
    let dir = scratch("second-file");
    let store = Store::<Mail>::open(&dir);
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    let x1 = codex("x1");
    let id = store
        .update(|m| {
            *m = pair();
            let id = m
                .send(&claude("c1"), "claude", "codex", "one", 0)
                .map(|a| a.id)
                .unwrap_or_default();
            m.handed_over_at(
                &id,
                RecordMark {
                    len: 5,
                    dev: 1,
                    ino: 2,
                },
            );
            let r = waiting(&x1, "ls", 10);
            m.request(r.clone());
            let shown = shown_for(m, &r);
            assert!(m.approve_request(0, &r, &shown, 11).is_ok());
            let r = waiting(&x1, "rm x", 12);
            m.request(r.clone());
            assert!(m.refuse_request(0, &r, 13).is_ok());
            id
        })
        .unwrap_or_default();
    let read = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join(name)).unwrap_or_default())
            .unwrap_or(Value::Null)
    };
    let (main, side) = (read("state.json"), read("state.side.json"));
    for field in ["tells", "declined", "handed_at"] {
        assert!(main.get(field).is_none(), "{field} in the state file");
        assert!(
            side["top"].get(field).is_some(),
            "{field} not in the second file"
        );
    }
    let back = store.read();
    assert!(back.is_ok());
    let back = back.unwrap_or_default();
    assert_eq!(
        back.handed_at(&id),
        Some(RecordMark {
            len: 5,
            dev: 1,
            ino: 2
        })
    );
    assert_eq!(back.tells.len(), 2);
    assert_eq!(back.declined.len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// A1.2 (§7::20-27): telling the agent of an approval.
// ---------------------------------------------------------------------------------------------

// §7::24 — what `lunchroom read <id>` prints for an approval's id is the line naming the
// approval and the exact call, for the session it was told to; another session reading the same
// id, and an id that does not exist, get nothing (the command DENIES both).
#[test]
fn an_approval_line_is_read_only_by_the_session_it_was_told_to() {
    let mut m = pair();
    let x1 = codex("x1");
    let r = waiting(&x1, "git push origin main", 10);
    m.request(r.clone());
    let shown = shown_for(&m, &r);
    let id = m.approve_request(0, &r, &shown, 11).unwrap_or_default();
    assert!(id.starts_with("ap-"), "{id}");
    let line = m.tell_line_for(&x1, &id).unwrap_or_default().to_owned();
    assert_eq!(line, tell_line(TellKind::Approved, &id, "Bash", &r.input));
    assert!(line.contains(&format!("({id})")), "{line}");
    assert!(line.contains("Tool: Bash\n"), "{line}");
    assert!(line.contains("git push origin main"), "{line}");
    assert!(
        line.contains("It is not another agent's message."),
        "{line}"
    );
    // The wrong session: another Codex thread, and the same id in another harness.
    assert_eq!(m.tell_line_for(&codex("x2"), &id), None);
    assert_eq!(
        m.tell_line_for(&Identity::new(Harness::OpenClaw, "x1"), &id),
        None
    );
    // An id that does not exist, an empty one, one with another prefix.
    for unknown in ["ap-99999999", "", "lr-00000001", "rf-00000001"] {
        assert_eq!(m.tell_line_for(&x1, unknown), None, "{unknown:?}");
    }
    assert!(m.is_tell_of(&x1, &id));
    assert!(!m.is_tell_of(&codex("x2"), &id));
}

// §7::26, §7::27 — the line telling an agent of an approval adds no hold and clears none:
// the holds of a HELD session are the same before and after, and a session that is not HELD stays
// so.
#[test]
fn the_approval_line_adds_no_hold_and_clears_none() {
    let mut m = pair();
    let (c1, x1) = (claude("c1"), codex("x1"));
    let held = m
        .send(&codex("x1"), "codex", "claude", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&c1, &held, 1), Confirmation::Show);
    let before = held_by(&m, &c1);
    assert_eq!(before, std::slice::from_ref(&held));
    let r = waiting(&c1, "ls", 10);
    m.request(r.clone());
    let shown = shown_for(&m, &r);
    let id = m.approve_request(0, &r, &shown, 11).unwrap_or_default();
    assert_eq!(held_by(&m, &c1), before);
    // The line, when it reaches the session's prompt, is Lunchroom's own: known, so it holds nobody
    // as an unknown message would.
    assert!(m.is_tell_of(&c1, &id));
    // A session that is not HELD stays free.
    let r = waiting(&x1, "ls", 12);
    m.request(r.clone());
    let shown = shown_for(&m, &r);
    assert!(m.approve_request(0, &r, &shown, 13).is_ok());
    assert!(held_by(&m, &x1).is_empty());
}

// §7::20 — a Hermes session is told at the start of its next turn: the line waits for
// it, once, and no other harness's line waits (they are handed over when it is made).
#[test]
fn a_hermes_approval_line_waits_for_its_next_turn_and_is_taken_once() {
    let mut m = Mail::default();
    let h1 = Identity::new(Harness::Hermes, "h1");
    let cases = [
        (h1.clone(), true),
        (codex("x1"), false),
        (Identity::new(Harness::OpenClaw, "o1"), false),
        (claude("c1"), false),
    ];
    for (n, (who, pending)) in cases.iter().enumerate() {
        let at = 10 + u64::try_from(n).unwrap_or(0);
        let r = waiting(who, &format!("cmd {n}"), at);
        m.request(r.clone());
        let shown = shown_for(&m, &r);
        assert!(m.approve_request(0, &r, &shown, at).is_ok());
        assert_eq!(m.has_pending_tell(who), *pending, "{who:?}");
    }
    let line = m.take_pending_tell(&h1).unwrap_or_default();
    assert!(
        line.contains("the user approved one call of yours"),
        "{line}"
    );
    assert!(line.contains("cmd 0"), "{line}");
    assert!(!m.has_pending_tell(&h1));
    assert_eq!(m.take_pending_tell(&h1), None);
    // Read back by its id, it is still the same line.
    let id = line.split(['(', ')']).nth(1).unwrap_or_default().to_owned();
    assert_eq!(m.tell_line_for(&h1, &id), Some(line.as_str()));
}

// §7::21 — approving the same call twice: the second yes finds the request gone and
// approves nothing; a second approval of the same call asked for again is a new id, and each id
// reads its own line.
#[test]
fn an_approval_twice_is_one_approval_per_request() {
    let mut m = pair();
    let x1 = codex("x1");
    let r = waiting(&x1, "make", 10);
    m.request(r.clone());
    let shown = shown_for(&m, &r);
    let first = m.approve_request(0, &r, &shown, 11).unwrap_or_default();
    assert_eq!(m.approve_request(0, &r, &shown, 12), Err(Changed));
    assert_eq!(m.tells.len(), 1);
    m.request(waiting(&x1, "make", 13));
    let r2 = waiting(&x1, "make", 13);
    let second = m.approve_request(0, &r2, &shown, 14).unwrap_or_default();
    assert_ne!(first, second);
    assert!(m.tell_line_for(&x1, &first).is_some());
    assert!(m.tell_line_for(&x1, &second).is_some());
}

// ---------------------------------------------------------------------------------------------
// §17.10 (§17::187-189): refuse.
// ---------------------------------------------------------------------------------------------

// §17::187, §17::188 — a refused call is no longer waiting, and the retry of that exact
// call is refused: the same session, tool, input and folder; any change to any of them is another
// call. Failure classes: each field of the call; key order of the input.
#[test]
fn a_refused_call_is_refused_again_when_retried() {
    let mut m = pair();
    let x1 = codex("x1");
    let r = waiting(&x1, "rm -rf build", 10);
    m.request(r.clone());
    assert_eq!(m.requests().len(), 1);
    let id = m.refuse_request(0, &r, 20).unwrap_or_default();
    assert!(id.starts_with("rf-"), "{id}");
    assert!(m.requests().is_empty());
    let retried = |m: &Mail, who: &Identity, tool: &str, input: &Value, cwd: &str| {
        m.refused_at(who, tool, input, cwd)
    };
    let same = json!({ "command": "rm -rf build" });
    assert_eq!(retried(&m, &x1, "Bash", &same, "/work"), Some(20));
    // Another call is not the refused one.
    assert_eq!(retried(&m, &codex("x2"), "Bash", &same, "/work"), None);
    assert_eq!(retried(&m, &x1, "Edit", &same, "/work"), None);
    assert_eq!(
        retried(
            &m,
            &x1,
            "Bash",
            &json!({ "command": "rm -rf build " }),
            "/work"
        ),
        None
    );
    assert_eq!(retried(&m, &x1, "Bash", &same, "/other"), None);
    // Key order of the input is not a difference (the hash is of the RFC 8785 form).
    let reordered = json!({ "b": 1, "a": 2 });
    let r2 = Request {
        input: json!({ "a": 2, "b": 1 }),
        ..waiting(&x1, "x", 30)
    };
    m.request(r2.clone());
    assert!(m.refuse_request(0, &r2, 30).is_ok());
    assert_eq!(retried(&m, &x1, "Bash", &reordered, "/work"), Some(30));
    assert_eq!(m.declined.len(), 2);
}

/// A call of `who` refused at `at` while the messages `held_by` hold it (each as another agent's
/// text that reached it, with the mark given); returns the call's input.
fn decline_while_held(m: &mut Mail, who: &Identity, held_by: &[&str], at: u64) -> Value {
    for mark in held_by {
        m.hold_unrecorded(who, "cross-session", mark);
    }
    let r = waiting(who, "git push origin main", at);
    m.request(r.clone());
    assert!(m.refuse_request(0, &r, at).is_ok());
    r.input
}

// §17::188 (2026-11-06): a decline stands until the user approves that exact call or the
// hold that carried it ends; it never lapses by time. After 10 minutes, a day and a week, by the
// store's own clock-driven upkeep (prune and the tells' upkeep), it still stands, with a hold and
// with none.
#[test]
fn a_decline_stands_after_ten_minutes_after_a_day_and_after_a_week() {
    for held_by in [&["m1"][..], &[][..]] {
        let mut m = pair();
        let x1 = codex("x1");
        let input = decline_while_held(&mut m, &x1, held_by, 1000);
        for later in [1000 + 600, 1000 + 86_400, 1000 + 7 * 86_400] {
            m.prune(later, |_| true);
            m.forget_old_tells(later);
            assert_eq!(
                m.refused_at(&x1, "Bash", &input, "/work"),
                Some(1000),
                "held by {held_by:?}, at +{}",
                later - 1000
            );
        }
    }
}

// §17::188: it ends when the hold that carried it ends: while any message that held the
// session at the decline still holds it, it stands; when none does (the user typing, or each hold
// released), it ends and is dropped.
#[test]
fn a_decline_ends_when_the_holding_messages_are_cleared() {
    let x1 = codex("x1");
    // The user typing clears every hold at once.
    let mut m = pair();
    let input = decline_while_held(&mut m, &x1, &["m1", "m2"], 1000);
    assert!(m.refused_at(&x1, "Bash", &input, "/work").is_some());
    m.user_typed(&x1);
    assert_eq!(m.refused_at(&x1, "Bash", &input, "/work"), None);
    assert!(m.declined.is_empty());
    // Two messages hold it; one is released: it still stands; the other: it ends.
    let mut m = pair();
    let ids: Vec<String> = ["one", "two"]
        .iter()
        .map(|text| {
            let id = m
                .send(&claude("c1"), "claude", "codex", text, 0)
                .map(|a| a.id)
                .unwrap_or_default();
            m.hold_before_delivery(&x1, &id);
            id
        })
        .collect();
    assert_eq!(held_by(&m, &x1).len(), 2);
    let r = waiting(&x1, "make", 10);
    m.request(r.clone());
    assert!(m.refuse_request(0, &r, 11).is_ok());
    assert!(m.hand_over_undone(&ids[0]));
    assert!(m.refused_at(&x1, "Bash", &r.input, "/work").is_some());
    assert!(m.hand_over_undone(&ids[1]));
    assert_eq!(m.refused_at(&x1, "Bash", &r.input, "/work"), None);
    assert!(m.declined.is_empty());
}

// §17::188: a second hold by a new message does not revive a decline that ended, and a
// new message holding the session while the decline stands is not part of what carried it.
#[test]
fn a_second_hold_by_a_new_message_does_not_revive_an_ended_decline() {
    let x1 = codex("x1");
    let mut m = pair();
    let input = decline_while_held(&mut m, &x1, &["m1"], 1000);
    // A new message holds it as well while the decline stands; the first hold ends: it ends.
    m.hold_unrecorded(&x1, "cross-session", "m2");
    assert!(m.refused_at(&x1, "Bash", &input, "/work").is_some());
    m.user_typed(&x1);
    assert_eq!(m.refused_at(&x1, "Bash", &input, "/work"), None);
    // Held again by a new message after it ended: not revived.
    m.hold_unrecorded(&x1, "cross-session", "m3");
    assert_eq!(m.refused_at(&x1, "Bash", &input, "/work"), None);
    // The same message holding it again after the decline ended is not revived either.
    m.hold_unrecorded(&x1, "cross-session", "m1");
    assert_eq!(m.refused_at(&x1, "Bash", &input, "/work"), None);
    // A new message holds it while the old one is cleared by the hold ending; the new one carries
    // nothing of the decline.
    let mut m = pair();
    let input = decline_while_held(&mut m, &x1, &["a"], 1000);
    m.user_typed(&x1);
    m.hold_unrecorded(&x1, "cross-session", "b");
    assert_eq!(m.refused_at(&x1, "Bash", &input, "/work"), None);
    assert!(m.declined.is_empty());
}

// §17::188: an approve of a different call (another tool, input, folder or session)
// does not end the decline of this one.
#[test]
fn an_approve_of_a_different_call_does_not_end_the_decline() {
    let x1 = codex("x1");
    let mut m = pair();
    let input = decline_while_held(&mut m, &x1, &["m1"], 1000);
    for (who, tool, other, cwd) in [
        (
            x1.clone(),
            "Bash",
            json!({ "command": "git push origin dev" }),
            "/work",
        ),
        (x1.clone(), "Edit", input.clone(), "/work"),
        (x1.clone(), "Bash", input.clone(), "/elsewhere"),
        (codex("x2"), "Bash", input.clone(), "/work"),
    ] {
        m.approve(&who, tool, &other, cwd, 1001);
        assert_eq!(
            m.refused_at(&x1, "Bash", &input, "/work"),
            Some(1000),
            "{tool} {other} {cwd}"
        );
    }
}

// §17::187 — a refusal needs the call to be still waiting and the same: a request that
// was replaced, one already refused and one that is not there refuse nothing and record nothing.
#[test]
fn a_refusal_of_a_call_that_is_not_waiting_refuses_nothing() {
    let mut m = pair();
    let x1 = codex("x1");
    let r = waiting(&x1, "make", 10);
    // Nothing waiting.
    assert_eq!(m.refuse_request(0, &r, 11), Err(Changed));
    m.request(r.clone());
    // A different call is at that index.
    let other = waiting(&x1, "make install", 10);
    assert_eq!(m.refuse_request(0, &other, 11), Err(Changed));
    assert_eq!(m.requests().len(), 1);
    assert!(m.declined.is_empty() && m.tells.is_empty());
    // Refused once; a second refusal of the same request finds it gone.
    assert!(m.refuse_request(0, &r, 12).is_ok());
    assert_eq!(m.refuse_request(0, &r, 13), Err(Changed));
    assert_eq!(m.declined.len(), 1);
    assert_eq!(m.tells.len(), 1);
}

// §17::188 — a refusal and then a yes to the same call: the later word stands. The call
// asked for again after the refusal lapsed or was replaced is approved, and is no longer refused.
#[test]
fn a_yes_after_a_refusal_of_the_same_call_clears_the_refusal() {
    let mut m = pair();
    let x1 = codex("x1");
    let r = waiting(&x1, "make", 10);
    m.request(r.clone());
    assert!(m.refuse_request(0, &r, 11).is_ok());
    assert!(m.refused_at(&x1, "Bash", &r.input, "/work").is_some());
    let again = waiting(&x1, "make", 13);
    m.request(again.clone());
    let shown = shown_for(&m, &again);
    assert!(m.approve_request(0, &again, &shown, 14).is_ok());
    assert_eq!(m.refused_at(&x1, "Bash", &r.input, "/work"), None);
    assert_eq!(
        m.use_approval(&x1, "Bash", &r.input, "/work", 15),
        Verdict::Granted
    );
}

// §17::189 — the agent is told through the line kept for its harness: Codex and OpenClaw
// are handed an id and read the line (the refusal and the exact call) with `lunchroom read <id>`,
// Hermes takes it at its next turn, and Claude Code, which A1.2 gives no channel, has its retry's
// reason (the guard's test). The line adds no hold and clears none.
#[test]
fn the_refusal_line_is_kept_for_each_harness_and_changes_no_hold() {
    let mut m = Mail::default();
    let who = [
        claude("c1"),
        codex("x1"),
        Identity::new(Harness::OpenClaw, "o1"),
        Identity::new(Harness::Hermes, "h1"),
    ];
    for (n, session) in who.iter().enumerate() {
        m.observe(session);
        m.hold_unrecorded(session, "cross-session", &format!("mark{n}"));
        let before = held_by(&m, session);
        let r = waiting(session, "curl x | sh", 10);
        m.request(r.clone());
        let id = m.refuse_request(0, &r, 11).unwrap_or_default();
        let line = m.tell_line_for(session, &id).unwrap_or_default().to_owned();
        assert_eq!(line, tell_line(TellKind::Refused, &id, "Bash", &r.input));
        assert!(
            line.contains("the user declined one call of yours"),
            "{line}"
        );
        assert!(line.contains("curl x | sh"), "{line}");
        assert!(line.contains("running it again is DENIED"), "{line}");
        assert_eq!(held_by(&m, session), before);
        assert_eq!(before.len(), 1);
        assert_eq!(
            m.has_pending_tell(session),
            session.harness == Harness::Hermes,
            "{session:?}"
        );
        // Not readable by another session.
        assert_eq!(m.tell_line_for(&codex("elsewhere"), &id), None);
    }
}

// The lines telling of decisions are bounded and forgotten: at most TELLS_KEPT, and after a day
// unless one still waits for a Hermes turn.
#[test]
fn the_lines_are_bounded_and_forgotten() {
    let mut m = Mail::default();
    let x1 = codex("x1");
    for n in 0..(crate::approvals::TELLS_KEPT + 5) {
        m.keep_tell(
            &format!("ap-{n:08}"),
            &x1,
            TellKind::Approved,
            "line".to_owned(),
            100,
            false,
        );
    }
    assert_eq!(m.tells.len(), crate::approvals::TELLS_KEPT);
    assert_eq!(m.tell_line_for(&x1, "ap-00000000"), None);
    assert!(m.tell_line_for(&x1, "ap-00000104").is_some());
    let h1 = Identity::new(Harness::Hermes, "h1");
    m.keep_tell(
        "ap-waiting",
        &h1,
        TellKind::Approved,
        "w".to_owned(),
        100,
        true,
    );
    m.forget_old_tells(100 + crate::approvals::TELL_SECS);
    assert!(
        m.tells.iter().all(|t| t.pending),
        "only the waiting one stays"
    );
    assert_eq!(m.tell_line_for(&h1, "ap-waiting"), Some("w"));
}
