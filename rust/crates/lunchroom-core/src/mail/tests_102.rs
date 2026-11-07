//! Lunchroom 1.0.2, delivery and store findings G2-3 to G2-20, in the core. Each test fails on the
//! code before the fix for the defect it names, and asserts the full text where text is the defect.

use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::approvals::{APPROVAL_SECS, Changed, Request};
use crate::identity::Harness;
use crate::store::Store;

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
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

/// Sends `text` from `from` (named `from_name`) to `to` at time 0 and returns the id.
fn sent(m: &mut Mail, from: &Identity, from_name: &str, to: &str, text: &str) -> String {
    m.send(from, from_name, to, text, 0)
        .map(|a| a.id)
        .unwrap_or_default()
}

fn event(id: &str, text: &str) -> Notice {
    Notice {
        message: id.to_owned(),
        text: text.to_owned(),
        at: 0,
        reason: false,
    }
}

// G2-4: a Claude Code message that was offered and has no listener to ask again is counted failed
// by any check once its time has passed, then offered again by the next listener of the session;
// the third makes it FAILED without any listener having run. Before the fix nothing but a
// listener's own offer ever counted it.
#[test]
fn an_offer_that_nobody_asks_again_for_is_counted_failed_by_any_check() {
    let mut m = pair();
    let id = sent(&mut m, &codex("x1"), "codex", "claude", "hi");
    let c1 = claude("c1");
    assert_eq!(m.present(&c1, 100).map(|d| d.id), Some(id.clone()));
    // Within its time nothing counts.
    m.expire_handovers(100 + CONFIRM_SECS - 1);
    assert_eq!(m.message(&id).map(|d| d.failed_attempts), Some(0));
    // The facts say what really happens: the check counts it, and only the listener offers it.
    let line = m.message(&id).map(|d| standing(d, 130)).unwrap_or_default();
    assert!(
        line.ends_with(
            "if not confirmed by 00:02 UTC, it counts as a failed attempt, and the session's \
             listener offers it again when that session runs"
        ),
        "{line}"
    );
    // A check at its time counts it, with no listener having asked.
    m.expire_handovers(100 + CONFIRM_SECS);
    assert_eq!(
        m.message(&id)
            .map(|d| (d.failed_attempts, d.presented_at, d.handed_over)),
        Some((1, None, false))
    );
    let told: Vec<String> = m
        .notices(&codex("x1"))
        .iter()
        .map(|n| n.text.clone())
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].starts_with(&format!(
            "{id} to claude: attempt 1 of 3 failed (no confirmation from the recipient's harness)."
        )),
        "{told:?}"
    );
    // The next listener of the session offers it again; two more unconfirmed offers, each counted
    // by a check alone, make it FAILED.
    let mut at = 100 + CONFIRM_SECS + 1;
    for n in 2..=MAX_ATTEMPTS {
        assert_eq!(m.present(&c1, at).map(|d| d.id), Some(id.clone()), "{n}");
        at += CONFIRM_SECS;
        m.expire_handovers(at);
        assert_eq!(m.message(&id).map(|d| d.failed_attempts), Some(n));
    }
    assert_eq!(
        m.message(&id).map(|d| d.status.state()),
        Some(MessageState::Failed)
    );
}

// G2-4: a clock that stepped back does not block delivery to the recipient: an offer stamped far
// ahead of the present time has run out, so the message is offered again at once.
#[test]
fn a_clock_that_stepped_back_does_not_block_delivery() {
    let mut m = pair();
    let id = sent(&mut m, &codex("x1"), "codex", "claude", "hi");
    let c1 = claude("c1");
    assert_eq!(m.present(&c1, 5000).map(|d| d.id), Some(id.clone()));
    assert_eq!(m.present(&c1, 100).map(|d| d.id), Some(id.clone()));
    assert_eq!(m.message(&id).map(|d| d.failed_attempts), Some(1));
}

// G2-5: a message never handed over (its recipient could not take it when it was sent) is offered
// by a check once it has waited RETRY_GAP_SECS and the recipient can take it; a Claude Code
// recipient is offered by its listener, not by a check; and when the reason that held it clears,
// the user and the sender are told so in full, once, with the unread reason replaced.
#[test]
fn a_never_handed_over_message_is_offered_when_its_reason_clears() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    let reason = "codex is STOPPED. Its session is not running, or Lunchroom cannot reach it: start \
                  it again.";
    m.note_unreachable(&id, Some(reason.to_owned()));
    m.tell_waiting(4);
    assert_eq!(m.to_offer_again(4), Vec::<Identity>::new());
    assert_eq!(m.to_offer_again(RETRY_GAP_SECS), vec![codex("x1")]);
    // The sender's unread notice is the reason; the clearing replaces it.
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    m.note_unreachable(&id, None);
    let sender = m.notices(&claude("c1"));
    assert_eq!(sender.len(), 1, "{sender:?}");
    assert_eq!(
        sender[0].text,
        format!(
            "{id} to codex can be handed over again: what stopped it no longer applies, so \
             Lunchroom offers it now (sent 4 s ago by claude)."
        )
    );
    assert_eq!(m.user_notices().len(), 1);
    // Cleared once: the same clearing again is nothing.
    m.note_unreachable(&id, None);
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    assert_eq!(
        m.present_again(&codex("x1"), RETRY_GAP_SECS).map(|d| d.id),
        Some(id)
    );
}

// G2-6: when a name moves, the earlier holder is no longer a receiver unless another name labels
// it, so none of its QUEUED messages is offered to it; they are FAILED at the next check with the
// reason, and their senders are told.
#[test]
fn a_moved_name_fails_the_messages_queued_for_it() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "for x1");
    m.observe(&codex("x2"));
    assert!(
        m.register("codex", &codex("x2"), Origin::User, Some(&codex("x1")))
            .is_ok()
    );
    // Neither session is offered it: x1 no longer receives, and x2 is not the addressee.
    assert_eq!(m.present(&codex("x1"), 1000), None);
    assert_eq!(m.present(&codex("x2"), 1000), None);
    assert_eq!(m.fail_unnamed(1000), vec![id.clone()]);
    let reason = "codex now names another session: its name was moved to that session";
    assert_eq!(
        m.message(&id).map(|d| d.status.clone()),
        Some(Status::Failed {
            reason: reason.to_owned()
        })
    );
    let told = m.notices(&claude("c1"));
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        told[0].text,
        format!(
            "{id} FAILED: {reason} (to codex, sent 16 min ago by claude). Lunchroom was waiting for \
             codex to take it and will not now. To try again, the user names the session again \
             with `lunchroom register codex codex:x1`, and claude sends the message again."
        )
    );
    // A session another name still labels keeps receiving.
    let mut m = pair();
    assert_eq!(m.register("cx", &codex("x1"), Origin::User, None), Ok(None));
    m.observe(&codex("x2"));
    assert!(
        m.register("codex", &codex("x2"), Origin::User, Some(&codex("x1")))
            .is_ok()
    );
    let id = sent(&mut m, &claude("c1"), "claude", "cx", "for x1");
    assert_eq!(m.fail_unnamed(1000), Vec::<String>::new());
    assert_eq!(m.present(&codex("x1"), 1000).map(|d| d.id), Some(id));
}

// G2-9: a message that something was told of is told DELIVERED with its age at the time of the
// confirmation, not at the time the last check ran.
#[test]
fn the_delivered_age_is_counted_to_the_confirmation() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    assert!(m.attempt_failed(&id, "codex queue exited 1", 10).is_some());
    let _ = m.take_user_notices();
    assert_eq!(m.confirm_at(&codex("x1"), &id, 4000), Confirmation::Show);
    let told = m.take_user_notices();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        told[0].text,
        format!(
            "{id} to codex is DELIVERED, 1 h 6 min after it was sent by claude, at attempt 2 of 3."
        )
    );
}

// G2-10, G2-11: the total of waiting calls is the list `lunchroom approve` shows, named or not;
// a call waits APPROVAL_SECS and no longer: expired by a later request, by the prune, and not
// approvable at its time.
#[test]
fn a_waiting_call_expires_with_the_approval_lifetime() {
    let request = |at: u64, command: &str| Request {
        session: claude("unnamed"),
        agent: "claude".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({ "command": command }),
        cwd: "/work/a".to_owned(),
        rule: "the permission slip of a HELD session".to_owned(),
        at,
    };
    let mut m = Mail::default();
    m.request(request(100, "one"));
    assert_eq!(m.requests_total(), 1);
    // Just short of its time it stays; at its time it goes.
    m.expire_requests(100 + APPROVAL_SECS - 1);
    assert_eq!(m.requests_total(), 1);
    // At its time it cannot be approved either, whatever the list still holds.
    let waiting = m
        .requests()
        .first()
        .cloned()
        .unwrap_or_else(|| request(0, ""));
    let prompt = m.approval_prompt(
        &waiting.session,
        &waiting.agent,
        &waiting.tool,
        &waiting.input,
        &waiting.cwd,
    );
    assert_eq!(
        m.approve_request(0, &waiting, &prompt, 100 + APPROVAL_SECS),
        Err(Changed)
    );
    assert_eq!(m.requests_total(), 1);
    // A later request drops it.
    m.request(request(100 + APPROVAL_SECS, "two"));
    assert_eq!(m.requests_total(), 1);
    assert_eq!(
        m.requests().first().map(|r| r.input.clone()),
        Some(json!({ "command": "two" }))
    );
    // So does the prune.
    m.prune(100 + 2 * APPROVAL_SECS, |_| true);
    assert_eq!(m.requests_total(), 0);
    // A request stamped ahead of the present time is kept.
    m.request(request(5000, "three"));
    m.expire_requests(10);
    assert_eq!(m.requests_total(), 1);
}

// G2-12: a list of notices holds at most NOTICES_KEPT, the oldest dropped with one line saying how
// many; an identical notice is not added twice; at most eight stand for one message; a newer reason
// replaces an unread one of the same message; the prune forgets the unread notices of a session no
// longer running after a day, and the user's after a week.
#[test]
fn notices_are_capped_deduplicated_and_pruned() {
    let mut list = Vec::new();
    for n in 0..NOTICES_KEPT + 20 {
        push_notice(
            &mut list,
            event(&format!("lr-{n:08}"), &format!("note {n}")),
            false,
        );
    }
    assert_eq!(list.len(), NOTICES_KEPT);
    assert_eq!(
        list[0].text,
        "21 earlier notices were dropped because the list holds at most 50."
    );
    assert_eq!(list[1].text, "note 21");
    assert_eq!(list.last().map(|n| n.text.as_str()), Some("note 69"));
    // The dropped line is counted on, not repeated.
    push_notice(&mut list, event("lr-00000100", "note 70"), false);
    assert_eq!(list.len(), NOTICES_KEPT);
    assert_eq!(
        list[0].text,
        "22 earlier notices were dropped because the list holds at most 50."
    );

    let mut list = Vec::new();
    for _ in 0..3 {
        push_notice(&mut list, event("lr-00000001", "same"), false);
    }
    assert_eq!(list.len(), 1);
    for n in 0..30 {
        push_notice(
            &mut list,
            event("lr-00000001", &format!("event {n}")),
            false,
        );
    }
    assert_eq!(list.len(), 8);
    assert_eq!(list[0].text, "event 22");
    assert_eq!(list[7].text, "event 29");

    // A newer reason replaces the unread one; an event stays.
    let mut list = Vec::new();
    let reason = |text: &str| Notice {
        reason: true,
        ..event("lr-00000002", text)
    };
    push_notice(&mut list, reason("first reason"), false);
    push_notice(&mut list, event("lr-00000002", "an attempt failed"), false);
    push_notice(&mut list, reason("second reason"), false);
    let texts: Vec<&str> = list.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(texts, ["an attempt failed", "second reason"]);

    // The prune: a session no longer running keeps its notices a day; the user's, a week.
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    m.observe_process("1:1", Path::new("/w"), &claude("c1"));
    m.expire_handovers(10);
    m.tell(&id, "something happened");
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    m.prune(10 + GONE_NOTICE_SECS - 1, |_| false);
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    m.prune(10 + GONE_NOTICE_SECS + 1, |_| true);
    assert_eq!(m.notices(&claude("c1")).len(), 1, "its session still runs");
    m.prune(10 + GONE_NOTICE_SECS + 1, |_| false);
    assert_eq!(m.notices(&claude("c1")).len(), 0);
    assert_eq!(m.user_notices().len(), 1);
    m.prune(10 + KEEP_SECS + 1, |_| true);
    assert_eq!(m.user_notices().len(), 0);
}

// G2-14: notices are marked told after they are shown, so a notice that arrives between the
// reading and the marking stays, and one never marked is told again.
#[test]
fn notices_leave_the_store_only_when_marked_told() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    assert!(m.attempt_failed(&id, "codex queue exited 1", 10).is_some());
    let user = m.user_notices().to_vec();
    let sender = m.notices(&claude("c1")).to_vec();
    assert_eq!((user.len(), sender.len()), (1, 1));
    // Not marked (the hook was killed before it wrote its output): told again at the next reading.
    assert_eq!(m.user_notices().len(), 1);
    // Another arrives meanwhile.
    m.expire_handovers(10);
    assert!(
        m.attempt_failed(&id, "codex queue exited 1 again", 40)
            .is_some()
    );
    m.mark_user_notices_told(&user);
    m.mark_notices_told(&claude("c1"), &sender);
    assert_eq!(m.user_notices().len(), 1);
    assert!(
        m.user_notices()[0]
            .text
            .contains("codex queue exited 1 again")
    );
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    let rest = m.notices(&claude("c1")).to_vec();
    m.mark_notices_told(&claude("c1"), &rest);
    assert_eq!(m.notices(&claude("c1")).len(), 0);
}

// G2-13: a store written by a later program, or by 1.0.2 with fields this program does not know,
// reads and writes back with every unknown field at the store, message and told levels; a store
// from before 1.0.2 reads as schema 0 and is written as the current schema.
#[test]
fn unknown_fields_survive_a_read_and_a_write() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
    assert_eq!(v["schema"], json!(SCHEMA));
    v["future_top"] = json!({ "a": 1 });
    v["messages"][id.as_str()]["future_message"] = json!("kept");
    v["messages"][id.as_str()]["told"]["future_told"] = json!([1, 2]);
    let back: Result<Mail, _> = serde_json::from_value(v);
    assert!(back.is_ok(), "{back:?}");
    let back = back.unwrap_or_default();
    let again = serde_json::to_value(&back).unwrap_or(Value::Null);
    assert_eq!(again["future_top"], json!({ "a": 1 }));
    assert_eq!(again["messages"][id.as_str()]["future_message"], "kept");
    assert_eq!(
        again["messages"][id.as_str()]["told"]["future_told"],
        json!([1, 2])
    );

    // The schema: absent reads as 0 (an older program wrote it), later reads as later; every write
    // stamps the current one.
    let mut legacy = serde_json::to_value(&m).unwrap_or(Value::Null);
    if let Some(object) = legacy.as_object_mut() {
        object.remove("schema");
    }
    let legacy: Mail = serde_json::from_value(legacy).unwrap_or_default();
    assert_eq!(legacy.schema(), 0);
    assert!(legacy.written_by_older_program());
    assert_eq!(
        serde_json::to_value(&legacy).unwrap_or(Value::Null)["schema"],
        json!(SCHEMA)
    );
    let mut later = serde_json::to_value(&m).unwrap_or(Value::Null);
    later["schema"] = json!(SCHEMA + 1);
    let later: Mail = serde_json::from_value(later).unwrap_or_default();
    assert!(later.written_by_newer_program() && !later.written_by_older_program());
}

// G2-13, through the store file: a state another program wrote, with a field this one does not
// know and no schema, keeps the field through an update and gains the schema.
#[test]
fn the_store_file_keeps_unknown_fields_and_stamps_the_schema() {
    let base = crate::test_target().join("store-tests-102");
    let dir = base.join(format!("{}-fields", std::process::id()));
    let store = Store::<Mail>::open(&dir);
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    assert!(store.update(|m| *m = pair()).is_ok());
    let file = dir.join("state.json");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let Some(object) = v.as_object_mut() {
        object.remove("schema");
        object.insert("future".to_owned(), json!(7));
    }
    assert!(std::fs::write(&file, v.to_string()).is_ok());
    assert!(store.update(|m| m.observe(&claude("c9"))).is_ok());
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap_or_default())
        .unwrap_or(Value::Null);
    assert_eq!(raw["future"], json!(7));
    assert_eq!(raw["schema"], json!(SCHEMA));
    let _ = std::fs::remove_dir_all(&dir);
}

// G2-15: a reason a message cannot be handed over is told once, with the way out, in full: the
// recipient being UNGUARDED no longer hides the way out, and the facts after the reason do not say
// it again.
#[test]
fn a_reason_is_told_once_with_the_way_out() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    let reason = "codex is UNGUARDED. Its configuration is missing; the user runs `lunchroom \
                  install`.";
    m.note_unreachable(&id, Some(reason.to_owned()));
    m.tell_waiting(5);
    let told = m.take_user_notices();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        told[0].text,
        format!(
            "{id} to codex cannot be handed over: {reason} It was sent 5 s ago by claude, attempt \
             1 of 3: waiting for that to clear; Lunchroom waits for the message id in that \
             session's record, or its next prompt; it is offered once it can be handed over. To \
             end the wait without fixing that, the user can run `lunchroom unregister codex` in a \
             terminal of their own; the message is then FAILED and its sender is told."
        )
    );
    assert_eq!(told[0].text.matches("UNGUARDED").count(), 1);
}

// G2-16: a failed attempt and a FAILED message say how many attempts, what Lunchroom was waiting
// for, what happens next, and what the user and the sender can do; in full.
#[test]
fn failed_notices_say_what_was_waited_for_and_what_to_do() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    assert!(m.attempt_failed(&id, "codex queue exited 1", 100).is_some());
    let first = m.notices(&claude("c1")).first().map(|n| n.text.clone());
    assert_eq!(
        first.as_deref(),
        Some(
            format!(
                "{id} to codex: attempt 1 of 3 failed (codex queue exited 1). Lunchroom was \
                 waiting for the message id in that session's record, or its next prompt, and \
                 did not get it. The message is still QUEUED: it is offered again at the next \
                 Lunchroom check (any guard hook, `lunchroom send` or `lunchroom status`), not \
                 before 00:01 UTC; the next try is attempt 2 of 3. Nothing is needed; to end the \
                 wait, the user can run `lunchroom unregister codex` in a terminal of their own; \
                 the message is then FAILED and its sender is told."
            )
            .as_str()
        )
    );
    assert!(m.attempt_failed(&id, "codex queue exited 1", 100).is_some());
    assert_eq!(
        m.attempt_failed(&id, "codex queue exited 1", 120),
        Some(AttemptOutcome::Failed)
    );
    let last = m.notices(&claude("c1")).last().map(|n| n.text.clone());
    assert_eq!(
        last.as_deref(),
        Some(
            format!(
                "{id} FAILED: codex queue exited 1 (to codex, sent 2 min ago by claude, after 3 \
                 of 3 attempts). Lunchroom was waiting for the message id in that session's \
                 record, or its next prompt, and did not get it in any of them, so it gave up \
                 and nothing was delivered. To try again, claude sends the message again; \
                 `lunchroom status` shows whether codex is available first."
            )
            .as_str()
        )
    );
}

// G2-19: a hand-over that failed at once is not tried again within RETRY_GAP_SECS, so three
// failures are not used up within seconds; the pause counts from the failure; a pause stamped
// further ahead than itself, by a clock that stepped back, is void; a hand-over that ran out of
// time (counted at its time) has no pause.
#[test]
fn a_failed_attempt_is_not_tried_again_within_the_gap() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    let x1 = codex("x1");
    assert!(
        m.attempt_failed(&id, "codex queue exited 1", 1000)
            .is_some()
    );
    for early in [1000, 1005, 1000 + RETRY_GAP_SECS - 1] {
        assert_eq!(m.present(&x1, early), None, "{early}");
        assert_eq!(m.present_again(&x1, early), None, "{early}");
    }
    assert_eq!(
        m.present_again(&x1, 1000 + RETRY_GAP_SECS).map(|d| d.id),
        Some(id.clone())
    );
    // Void: the failure was stamped by a clock far ahead of this one.
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    assert!(
        m.attempt_failed(&id, "codex queue exited 1", 99_999)
            .is_some()
    );
    assert_eq!(m.present(&x1, 1000).map(|d| d.id), Some(id.clone()));
    // A hand-over that ran out of time is offered again at once.
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    assert_eq!(m.present(&x1, 0).map(|d| d.id), Some(id.clone()));
    m.handed_over(&id);
    assert_eq!(
        m.present(&x1, HANDOVER_SECS).map(|d| d.id),
        Some(id),
        "offered at the moment it ran out"
    );
}

// G2-20: a reason the sender was given in the report of its own send is not queued to the sender
// again, only to the user; a different reason later is told to the sender too.
#[test]
fn a_reason_the_sender_was_given_at_send_is_told_to_the_user_only() {
    let mut m = pair();
    let id = sent(&mut m, &claude("c1"), "claude", "codex", "hi");
    m.sender_told_reason(&id);
    m.note_unreachable(&id, Some("codex is STOPPED.".to_owned()));
    m.tell_waiting(1);
    assert_eq!(m.notices(&claude("c1")).len(), 0);
    assert_eq!(m.user_notices().len(), 1);
    // A different reason is news to the sender.
    m.note_unreachable(&id, Some("codex is UNGUARDED.".to_owned()));
    m.tell_waiting(2);
    assert_eq!(m.notices(&claude("c1")).len(), 1);
    // The user's unread reason was replaced by the newer one.
    let user: Vec<&str> = m.user_notices().iter().map(|n| n.text.as_str()).collect();
    assert_eq!(user.len(), 1, "{user:?}");
    assert!(user[0].contains("codex is UNGUARDED."), "{user:?}");
}

// DV-03 (D1.1): a check that runs in one sender's process offers again only that sender's own
// message; another sender's due message is passed over and left as it was (still QUEUED, nothing
// counted, not in front of the harness), and is offered when the check offers any sender's.
#[test]
fn a_check_in_one_senders_process_offers_again_only_that_senders_message() {
    let mut m = pair();
    let other = claude("c2");
    m.observe(&other);
    assert_eq!(m.register("claude2", &other, Origin::User, None), Ok(None));
    let first = sent(&mut m, &claude("c1"), "claude", "codex", "from c1");
    let x1 = codex("x1");
    // The first message's hand-over counted as failed; it is due again once the gap has passed.
    assert_eq!(m.present(&x1, 0).map(|d| d.id), Some(first.clone()));
    assert!(m.attempt_failed(&first, "no confirmation", 0).is_some());
    let second = sent(&mut m, &other, "claude2", "codex", "from c2");
    let due = RETRY_GAP_SECS + 1;
    // c2's process is offered c2's message, never c1's, though c1's is older.
    assert_eq!(
        m.present_again_from(&x1, Some(&other), due).map(|d| d.id),
        Some(second.clone())
    );
    assert!(m.hand_over_undone(&second));
    // c1's own process is offered c1's message and not c2's.
    assert_eq!(
        m.present_again_from(&x1, Some(&claude("c1")), due)
            .map(|d| d.id),
        Some(first.clone())
    );
    assert!(m.hand_over_undone(&first));
    // A process of no sender (a hook-started check, a listener) is offered the oldest.
    assert_eq!(m.present_again(&x1, due).map(|d| d.id), Some(first.clone()));
    // c1's message is now in front of the harness: c2's process is offered nothing (one at a time).
    assert_eq!(m.present_again_from(&x1, Some(&other), due), None);
    assert_eq!(m.message(&first).map(|d| d.failed_attempts), Some(1));
    assert_eq!(m.message(&second).map(|d| d.failed_attempts), Some(0));
}
