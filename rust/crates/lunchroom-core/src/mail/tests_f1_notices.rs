//! Lunchroom 1.1.0, F1-AC41 and F1-AC51 to F1-AC54: the user and the sender are told, in the next
//! prompt of any guarded session, of a hand-over with no confirmation, a failed attempt, a FAILED
//! message, a message that cannot be handed over, and a DELIVERED message that was told of. One
//! fixture (`world`) serves the five: sender `alpha` (any harness) sends at time 0 to `bravo`, a
//! Codex session. `mail/tests.rs` and `mail/tests_102.rs` assert parts of this text; these tests
//! assert that BOTH the user and the sender get each notice, with every fact F1-AC41 lists.

use super::*;
use crate::identity::Harness;

fn bravo() -> Identity {
    Identity::new(Harness::Codex, "x1")
}

fn senders() -> [Identity; 3] {
    [
        Identity::new(Harness::ClaudeCode, "c1"),
        Identity::new(Harness::Codex, "x9"),
        Identity::new(Harness::OpenClaw, "o1"),
    ]
}

/// `alpha` (the sender) and `bravo` (Codex session x1), both named by the user; `alpha` has sent
/// one message to `bravo` at time 0. Returns the mail, the sender and the message id.
fn world(sender: Identity) -> (Mail, Identity, String) {
    let mut m = Mail::default();
    for (name, who) in [("alpha", sender.clone()), ("bravo", bravo())] {
        m.observe(&who);
        assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
    }
    let id = m
        .send(&sender, "alpha", "bravo", "hi", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_ne!(id, "", "the message has an id");
    (m, sender, id)
}

/// Hands the message to bravo's harness at `at`, as `lunchroom send` does for Codex.
fn hand_over(m: &mut Mail, id: &str, at: u64) {
    let presented = m.present(&bravo(), at).map(|d| d.id);
    assert_eq!(presented.as_deref(), Some(id));
    m.hold_before_delivery(&bravo(), id);
    m.handed_over(id);
}

fn last_text(list: &[Notice]) -> String {
    list.last().map(|n| n.text.clone()).unwrap_or_default()
}

/// The latest notice the sender and the user each have, which must be the same text.
fn told_both(m: &Mail, sender: &Identity) -> String {
    let to_sender = last_text(m.notices(sender));
    let to_user = last_text(m.user_notices());
    assert!(!to_sender.is_empty(), "the sender was told nothing");
    assert_eq!(to_sender, to_user, "the user and the sender differ");
    to_sender
}

fn has_all(text: &str, facts: &[String]) {
    for fact in facts {
        assert!(text.contains(fact.as_str()), "{fact:?} missing from {text}");
    }
}

fn s(text: &str) -> String {
    text.to_owned()
}

// F1-AC41: a hand-over with no confirmation for 2 minutes is told to the user and the sender, of
// any harness, naming message, recipient, age, attempt, what is waited for, when it is offered
// again and what the user can do; nothing is told one second before the 2 minutes are over.
#[test]
fn no_confirmation_for_two_minutes_is_told_to_user_and_sender() {
    for sender in senders() {
        let (mut m, sender, id) = world(sender);
        hand_over(&mut m, &id, 400);
        m.tell_waiting(400 + WAITING_TOLD_SECS - 1);
        assert!(m.user_notices().is_empty() && m.notices(&sender).is_empty());
        m.tell_waiting(400 + WAITING_TOLD_SECS);
        let text = told_both(&m, &sender);
        has_all(
            &text,
            &[
                format!("{id} to bravo: no confirmation 2 min after its hand-over."),
                s("sent 8 min ago by alpha, attempt 1 of 3"),
                format!("Lunchroom waits for {}", waiting_for(Harness::Codex)),
                s("it is offered again at 00:16 UTC if not confirmed by then"),
                s("the user can run `lunchroom unregister bravo`"),
            ],
        );
    }
}

// F1-AC51: a failed attempt is told to the user and the sender with the facts of F1-AC41 that an
// attempt has: message, recipient, attempt, what was waited for, when it is offered again, what
// the user can do.
#[test]
fn a_failed_attempt_is_told_to_user_and_sender() {
    let (mut m, sender, id) = world(Identity::new(Harness::ClaudeCode, "c1"));
    assert!(m.attempt_failed(&id, "codex queue exited 1", 100).is_some());
    let text = told_both(&m, &sender);
    has_all(
        &text,
        &[
            format!("{id} to bravo: attempt 1 of 3 failed (codex queue exited 1)."),
            format!("Lunchroom was waiting for {}", waiting_for(Harness::Codex)),
            s("it is offered again at the next Lunchroom check"),
            s("not before 00:01 UTC; the next try is attempt 2 of 3"),
            s("the user can run `lunchroom unregister bravo`"),
        ],
    );
}

// F1-AC51 with F1-AC41: the notice of a failed attempt names the message's age, as every notice of
// F1-AC41 does. (The text of `attempt_text`, waiting.rs, has no age for a QUEUED message.)
#[test]
fn a_failed_attempt_notice_names_the_age_of_the_message() {
    let (mut m, sender, id) = world(Identity::new(Harness::ClaudeCode, "c1"));
    assert!(m.attempt_failed(&id, "codex queue exited 1", 100).is_some());
    let text = told_both(&m, &sender);
    let age = format!("{} ago", span(100));
    assert!(text.contains(&age), "{age:?} missing from {text}");
}

// F1-AC52: a FAILED message is told to the user and the sender with the message, recipient, age,
// attempts, what was waited for and what the sender can do.
#[test]
fn a_failed_message_is_told_to_user_and_sender() {
    let (mut m, sender, id) = world(Identity::new(Harness::ClaudeCode, "c1"));
    for at in [100, 100, 120] {
        assert!(m.attempt_failed(&id, "codex queue exited 1", at).is_some());
    }
    assert_eq!(
        m.message(&id).map(|d| d.status.state()),
        Some(MessageState::Failed)
    );
    let text = told_both(&m, &sender);
    has_all(
        &text,
        &[
            format!("{id} FAILED: codex queue exited 1 (to bravo, sent 2 min ago by alpha"),
            s("after 3 of 3 attempts)"),
            format!("Lunchroom was waiting for {}", waiting_for(Harness::Codex)),
            s("To try again, alpha sends the message again"),
        ],
    );
}

// F1-AC53: a message that cannot be handed over is told to the user and the sender, of any
// harness, with the reason, the age, the attempt, what is waited for, when it is offered again and
// what the user can do.
#[test]
fn a_message_that_cannot_be_handed_over_is_told_to_user_and_sender() {
    for sender in senders() {
        let (mut m, sender, id) = world(sender);
        let reason = s("bravo is UNGUARDED. Its configuration is missing.");
        m.note_unreachable(&id, Some(reason));
        m.tell_waiting(5);
        let text = told_both(&m, &sender);
        has_all(
            &text,
            &[
                format!("{id} to bravo cannot be handed over: bravo is UNGUARDED."),
                s("sent 5 s ago by alpha, attempt 1 of 3"),
                format!("Lunchroom waits for {}", waiting_for(Harness::Codex)),
                s("it is offered once it can be handed over"),
                s("the user can run `lunchroom unregister bravo`"),
            ],
        );
    }
}

/// The three cases of F1-AC41, F1-AC51 and F1-AC53 a message is DELIVERED after.
#[derive(Clone, Copy)]
enum Before {
    NoConfirmation,
    FailedAttempt,
    CannotHandOver,
}

/// Brings the message to `case`, then delivers it; returns the time of the confirmation and the
/// attempt it was confirmed at.
fn deliver_after(m: &mut Mail, id: &str, case: Before) -> (u64, u32) {
    let (at, attempt) = match case {
        Before::NoConfirmation => {
            hand_over(m, id, 400);
            m.tell_waiting(400 + WAITING_TOLD_SECS);
            (700, 1)
        }
        Before::FailedAttempt => {
            assert!(m.attempt_failed(id, "codex queue exited 1", 100).is_some());
            assert!(m.present(&bravo(), 200).is_some());
            (230, 2)
        }
        Before::CannotHandOver => {
            m.note_unreachable(id, Some(s("bravo is UNGUARDED.")));
            m.tell_waiting(5);
            m.note_unreachable(id, None);
            assert!(m.present(&bravo(), 10).is_some());
            (20, 1)
        }
    };
    assert_eq!(m.confirm_at(&bravo(), id, at), Confirmation::Show);
    (at, attempt)
}

// F1-AC54: a message DELIVERED after a case of F1-AC41, F1-AC51 or F1-AC53 is told to the user and
// the sender, naming message, recipient, age and the attempt it was delivered at.
#[test]
fn a_delivery_after_any_of_those_is_told_to_user_and_sender() {
    for case in [
        Before::NoConfirmation,
        Before::FailedAttempt,
        Before::CannotHandOver,
    ] {
        let (mut m, sender, id) = world(Identity::new(Harness::ClaudeCode, "c1"));
        let (at, attempt) = deliver_after(&mut m, &id, case);
        let text = told_both(&m, &sender);
        assert_eq!(
            text,
            format!(
                "{id} to bravo is DELIVERED, {} after it was sent by alpha, at attempt {attempt} \
                 of 3.",
                span(at)
            )
        );
    }
}
