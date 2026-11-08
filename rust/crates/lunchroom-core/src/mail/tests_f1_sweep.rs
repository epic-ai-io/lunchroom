//! Lunchroom 1.1.0, F1 acceptance criteria AC40, AC42, AC43, AC44, AC45 and AC46, in the core.
//! Each test names its criterion (docs/spec/1.1/F1/requirements.md), checks one behaviour, and
//! fails if that behaviour were removed. What needs the `lunchroom` program is not here.

use serde_json::{Value, json};

use super::*;
use crate::approvals::{APPROVAL_SECS, Request};
use crate::identity::Harness;
use crate::store::Store;

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

/// Claude (c1), Codex (x1) and a second Codex (x2, named `codex2`), each seen and named by the
/// user.
fn trio() -> Mail {
    let mut m = Mail::default();
    for (name, who) in [
        ("claude", claude("c1")),
        ("codex", codex("x1")),
        ("codex2", codex("x2")),
    ] {
        m.observe(&who);
        assert_eq!(m.register(name, &who, Origin::User, None), Ok(None));
    }
    m
}

/// Sends `text` from `from` (named `from_name`) to `to` at `at` and returns the id.
fn sent_at(
    m: &mut Mail,
    from: &Identity,
    from_name: &str,
    to: &str,
    text: &str,
    at: u64,
) -> String {
    m.send(from, from_name, to, text, at)
        .map(|a| a.id)
        .unwrap_or_default()
}

/// Offers `id` to `who` at `at` and marks it handed over, as a hand-over to Codex does.
fn handed(m: &mut Mail, who: &Identity, id: &str, at: u64) {
    assert_eq!(m.present(who, at).map(|d| d.id), Some(id.to_owned()));
    m.handed_over(id);
}

/// A message's failed attempts, whether it is marked handed over, and its `presented_at`.
fn counted(m: &Mail, id: &str) -> Option<(u32, bool, Option<u64>)> {
    m.message(id)
        .map(|d| (d.failed_attempts, d.handed_over, d.presented_at))
}

// F1-AC40: one check counts every hand-over that stayed unconfirmed for 10 minutes, whatever its
// recipient: none at 599 s, each at 600 s, and each is offered again by the next check.
#[test]
fn f1_ac40_a_check_counts_every_handover_unconfirmed_for_ten_minutes() {
    assert_eq!(HANDOVER_SECS, 600);
    let mut m = trio();
    let one = sent_at(&mut m, &claude("c1"), "claude", "codex", "one", 0);
    let two = sent_at(&mut m, &claude("c1"), "claude", "codex2", "two", 0);
    handed(&mut m, &codex("x1"), &one, 0);
    handed(&mut m, &codex("x2"), &two, 0);
    m.expire_handovers(HANDOVER_SECS - 1);
    for id in [&one, &two] {
        assert_eq!(counted(&m, id), Some((0, true, Some(0))), "{id}");
    }
    m.expire_handovers(HANDOVER_SECS);
    for id in [&one, &two] {
        assert_eq!(counted(&m, id), Some((1, false, None)), "{id}");
        assert_eq!(
            m.message(id).map(|d| d.status.state()),
            Some(MessageState::Queued)
        );
    }
    assert_eq!(
        m.present_again(&codex("x1"), HANDOVER_SECS).map(|d| d.id),
        Some(one)
    );
    assert_eq!(
        m.present_again(&codex("x2"), HANDOVER_SECS).map(|d| d.id),
        Some(two)
    );
}

// F1-AC40: the third hand-over left unconfirmed for 10 minutes makes the message FAILED, and the
// message is not offered again.
#[test]
fn f1_ac40_three_unconfirmed_handovers_fail_the_message() {
    let mut m = trio();
    let id = sent_at(&mut m, &claude("c1"), "claude", "codex", "one", 0);
    let x1 = codex("x1");
    let mut at = 0;
    for n in 1..=MAX_ATTEMPTS {
        handed(&mut m, &x1, &id, at);
        at += HANDOVER_SECS;
        m.expire_handovers(at);
        assert_eq!(m.message(&id).map(|d| d.failed_attempts), Some(n));
    }
    assert_eq!(
        m.message(&id).map(|d| d.status.state()),
        Some(MessageState::Failed)
    );
    assert_eq!(m.present_again(&x1, at + RETRY_GAP_SECS), None);
}

// F1-AC40: a confirmation found before the 10 minutes are up delivers the message, and the later
// check counts no failed attempt against it.
#[test]
fn f1_ac40_a_confirmed_handover_is_not_counted_failed() {
    let mut m = trio();
    let id = sent_at(&mut m, &claude("c1"), "claude", "codex", "one", 0);
    let x1 = codex("x1");
    handed(&mut m, &x1, &id, 0);
    assert_eq!(
        m.confirm_at(&x1, &id, HANDOVER_SECS - 1),
        Confirmation::Show
    );
    m.expire_handovers(HANDOVER_SECS + 60);
    assert_eq!(
        m.message(&id)
            .map(|d| (d.status.state(), d.failed_attempts)),
        Some((MessageState::Delivered, 0))
    );
}

// F1-AC40: a QUEUED message whose recipient left (`unregister`) is FAILED by the check with the
// reason and its sender is told; a message to a recipient still named stays QUEUED.
#[test]
fn f1_ac40_a_message_to_a_recipient_that_left_is_failed() {
    let mut m = trio();
    let gone = sent_at(&mut m, &claude("c1"), "claude", "codex", "for x1", 0);
    let kept = sent_at(&mut m, &codex("x1"), "codex", "claude", "for c1", 0);
    assert!(m.unregister("codex"));
    assert_eq!(m.fail_unnamed(30), vec![gone.clone()]);
    let reason = "codex no longer takes messages: it left with `leave`, or its name was taken away";
    assert_eq!(
        m.message(&gone).map(|d| d.status.clone()),
        Some(Status::Failed {
            reason: reason.to_owned()
        })
    );
    assert_eq!(
        m.message(&kept).map(|d| d.status.state()),
        Some(MessageState::Queued)
    );
    let told = m.notices(&claude("c1"));
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0]
            .text
            .starts_with(&format!("{gone} FAILED: {reason}")),
        "{told:?}"
    );
}

// F1-AC42: the recipient was UNGUARDED, so the message was not handed over; when that clears, the
// user is told it is offered now, and the next check offers it again.
#[test]
fn f1_ac42_a_message_held_by_an_unguarded_recipient_is_offered_when_it_clears() {
    let mut m = trio();
    let id = sent_at(&mut m, &claude("c1"), "claude", "codex", "hi", 0);
    let reason = "codex is UNGUARDED. Its configuration is missing; the user runs `lunchroom \
                  install`.";
    m.note_unreachable(&id, Some(reason.to_owned()));
    m.tell_waiting(4);
    assert_eq!(m.take_user_notices().len(), 1);
    let held = m.message(&id).map(|d| standing(d, 4)).unwrap_or_default();
    assert!(
        held.contains("it cannot be handed over: codex is UNGUARDED"),
        "{held}"
    );
    m.note_unreachable(&id, None);
    assert_eq!(
        m.message(&id).map(|d| cannot_hand_over(d).is_none()),
        Some(true)
    );
    let user = m.take_user_notices();
    assert_eq!(user.len(), 1, "{user:?}");
    assert_eq!(
        user[0].text,
        format!(
            "{id} to codex can be handed over again: what stopped it no longer applies, so \
             Lunchroom offers it now (sent 4 s ago by claude)."
        )
    );
    assert_eq!(m.to_offer_again(RETRY_GAP_SECS), vec![codex("x1")]);
    assert_eq!(
        m.present_again(&codex("x1"), RETRY_GAP_SECS).map(|d| d.id),
        Some(id)
    );
}

// F1-AC43: the list `lunchroom status` prints holds every QUEUED message, oldest first, with the
// facts, a message to an agent no longer named included and a DELIVERED one left out. After the
// check of F1-AC40 the unnamed agent's message is FAILED and no longer listed, which is why status
// runs the check first.
#[test]
fn f1_ac43_the_queued_list_has_every_message_with_its_facts() {
    let mut m = trio();
    let a = sent_at(&mut m, &claude("c1"), "claude", "codex", "one", 0);
    let b = sent_at(&mut m, &codex("x1"), "codex", "claude", "two", 60);
    let done = sent_at(&mut m, &claude("c1"), "claude", "codex2", "three", 30);
    assert_eq!(m.confirm_at(&codex("x2"), &done, 40), Confirmation::Show);
    assert!(m.unregister("codex"));
    let ids = |m: &Mail| m.queued().iter().map(|d| d.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&m), vec![a.clone(), b.clone()]);
    let line = m.message(&a).map(|d| standing(d, 300)).unwrap_or_default();
    let head = format!("{a} to codex (codex:x1), sent 5 min ago by claude, attempt 1 of 3: ");
    assert!(line.starts_with(&head), "{line}");
    assert!(
        line.contains(
            "Lunchroom waits for the message id in that session's record, or its next prompt"
        ),
        "{line}"
    );
    assert_eq!(m.fail_unnamed(300), vec![a]);
    assert_eq!(ids(&m), vec![b]);
}

// F1-AC44: each waiting call has its own age: at 10 minutes the old call stops waiting and the
// newer one stays.
#[test]
fn f1_ac44_only_the_call_that_waited_ten_minutes_stops_waiting() {
    let request = |at: u64, command: &str| Request {
        session: claude("unnamed"),
        agent: "claude".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({ "command": command }),
        cwd: "/work/a".to_owned(),
        rule: "the permission slip of a HELD session".to_owned(),
        at,
    };
    assert_eq!(APPROVAL_SECS, 600);
    let mut m = Mail::default();
    m.request(request(0, "old"));
    m.request(request(400, "new"));
    assert_eq!(m.requests_total(), 2);
    m.expire_requests(APPROVAL_SECS - 1);
    assert_eq!(m.requests_total(), 2);
    m.expire_requests(APPROVAL_SECS);
    let left: Vec<u64> = m.requests().iter().map(|r| r.at).collect();
    assert_eq!(left, vec![400]);
    m.expire_requests(400 + APPROVAL_SECS);
    assert_eq!(m.requests_total(), 0);
}

// F1-AC45: the notices for the user and for a sender are each kept to 50 through the path that
// tells them: 50 fit without a word, the 51st drops two (49 and the line stay) and says so, and
// the count goes on rising in the one first line.
#[test]
fn f1_ac45_both_notice_lists_keep_fifty_and_say_how_many_were_dropped() {
    let mut m = trio();
    let id = sent_at(&mut m, &claude("c1"), "claude", "codex", "hi", 0);
    let base = m.message(&id).cloned();
    assert!(base.is_some());
    let Some(base) = base else { return };
    let tell_one = |m: &mut Mail, n: usize| {
        let copy = format!("lr-9{n:07}");
        let message = Message {
            id: copy.clone(),
            ..base.clone()
        };
        m.messages.insert(copy.clone(), message);
        m.tell(&copy, &format!("note {n}"));
    };
    let lists = |m: &Mail| [m.user_notices().to_vec(), m.notices(&claude("c1")).to_vec()];
    for n in 0..NOTICES_KEPT {
        tell_one(&mut m, n);
    }
    for list in lists(&m) {
        assert_eq!(list.len(), NOTICES_KEPT);
        assert_eq!(list[0].text, "note 0");
    }
    tell_one(&mut m, NOTICES_KEPT);
    for list in lists(&m) {
        assert_eq!(list.len(), NOTICES_KEPT);
        assert_eq!(
            list[0].text,
            "2 earlier notices were dropped because the list holds at most 50."
        );
        assert_eq!(list[1].text, "note 2");
    }
    for n in NOTICES_KEPT + 1..60 {
        tell_one(&mut m, n);
    }
    for list in lists(&m) {
        assert_eq!(list.len(), NOTICES_KEPT);
        assert_eq!(
            list[0].text,
            "11 earlier notices were dropped because the list holds at most 50."
        );
        assert_eq!(list.last().map(|n| n.text.as_str()), Some("note 59"));
    }
}

// F1-AC46: a record of a later program, with fields this program does not know at the message and
// told levels, keeps them through a rewrite by this program, and the rewrite stamps the schema
// number even over a later one. (G2-13 covers the in-memory round trip; this goes through the
// store file.)
#[test]
fn f1_ac46_a_rewrite_through_the_store_keeps_later_records_of_a_message() {
    let dir = crate::test_target()
        .join("store-tests-f1-sweep")
        .join(format!("{}-message-fields", std::process::id()));
    let store = Store::<Mail>::open(&dir);
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    let mut built = trio();
    let id = sent_at(&mut built, &claude("c1"), "claude", "codex", "hi", 0);
    assert!(store.update(|m| *m = built.clone()).is_ok());
    let file = dir.join("state.json");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    v["schema"] = json!(SCHEMA + 1);
    v["messages"][id.as_str()]["future_message"] = json!("kept");
    v["messages"][id.as_str()]["told"]["future_told"] = json!([1, 2]);
    assert!(std::fs::write(&file, v.to_string()).is_ok());
    assert!(store.update(|m| m.observe(&claude("c9"))).is_ok());
    let raw = std::fs::read_to_string(&file).unwrap_or_default();
    let raw: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let message = &raw["messages"][id.as_str()];
    assert_eq!(message["future_message"], json!("kept"));
    assert_eq!(message["told"]["future_told"], json!([1, 2]));
    assert_eq!(raw["schema"], json!(SCHEMA));
    let _ = std::fs::remove_dir_all(&dir);
}
