//! Lunchroom 1.0.2, delivery and store findings (G2), through the `lunchroom` binary run in a
//! scratch home with an empty environment. Each test fails on the code before the fix for the
//! defect it names.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use lunchroom_core::approvals::Request;
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin};

use common::Sandbox;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// claude (c1) and codex (x1), each seen and named.
fn two_agents(m: &mut Mail) {
    for (name, who) in [
        ("claude", Identity::new(Harness::ClaudeCode, "c1")),
        ("codex", Identity::new(Harness::Codex, "x1")),
    ] {
        m.observe(&who);
        let _ = m.register(name, &who, Origin::User, None);
    }
}

// G2-11: `lunchroom approve` lists each waiting call with how long it has waited, and a call that
// has waited as long as an approval lasts (10 minutes) is no longer listed, and is gone from the
// store. Before the fix a request waited for ever and the list showed no age.
#[test]
fn approve_lists_the_age_of_each_call_and_drops_a_lapsed_one() {
    let sb = Sandbox::new("g2-tests");
    let at = now();
    let request = |ago: u64, command: &str| Request {
        session: Identity::new(Harness::ClaudeCode, "s1"),
        agent: "Claude".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({ "command": command }),
        cwd: "/work/a".to_owned(),
        rule: "PROTECTED (push or publish)".to_owned(),
        at: at.saturating_sub(ago),
    };
    let store = sb.store();
    assert!(store.is_some());
    let Some(store) = store else { return };
    // The newer call first: a request that comes after drops the ones that lapsed by its time.
    let kept = store.update(|m| {
        m.request(request(70, "git push new"));
        m.request(request(700, "git push old"));
        m.requests_total()
    });
    assert_eq!(kept.ok(), Some(2));
    let listed = sb.run(&["approve"], "", &[]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    assert!(
        listed.stdout.starts_with(
            "1. Claude (claude-code:s1) Bash {\"command\":\"git push new\"} (asked 1 min ago)"
        ),
        "{}",
        listed.stdout
    );
    assert_eq!(listed.stdout.lines().count(), 1, "{}", listed.stdout);
    assert_eq!(store.read().map(|m| m.requests_total()).ok(), Some(1));
}

// G2-13: a store another program wrote, with fields this one does not know and no schema number,
// is read by `lunchroom status` and written back whole: every unknown field at the store and
// message level is still there, and the schema number is now written.
#[test]
fn status_keeps_the_fields_it_does_not_know() {
    let sb = Sandbox::new("g2-tests");
    let store = sb.store();
    assert!(store.is_some());
    let Some(store) = store else { return };
    let sent = store.update(|m| {
        two_agents(m);
        m.send(
            &Identity::new(Harness::ClaudeCode, "c1"),
            "claude",
            "codex",
            "hi",
            now(),
        )
        .map(|a| a.id)
        .unwrap_or_default()
    });
    let id = sent.unwrap_or_default();
    assert_ne!(id, "");
    let file = sb.lunchroom().join("store/state.json");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let Some(object) = v.as_object_mut() {
        object.remove("schema");
        object.insert("future_top".to_owned(), json!({ "a": 1 }));
    }
    v["messages"][id.as_str()]["future_message"] = json!("kept");
    assert!(std::fs::write(&file, v.to_string()).is_ok());
    let run = sb.run(&["status"], "", &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap_or_default())
        .unwrap_or(Value::Null);
    assert_eq!(raw["future_top"], json!({ "a": 1 }));
    assert_eq!(raw["messages"][id.as_str()]["future_message"], "kept");
    assert_eq!(raw["schema"], json!(2));
}

// G2-4: a message to a Claude Code session that was offered and never confirmed, with no listener
// asking again, is counted as a failed attempt by the next check, which `lunchroom status` runs.
// Before the fix only a listener's own offer ever counted it, so it waited for ever.
#[test]
fn status_counts_an_offer_nobody_asked_again_for() {
    let sb = Sandbox::new("g2-tests");
    let store = sb.store();
    assert!(store.is_some());
    let Some(store) = store else { return };
    let at = now();
    let made = store.update(|m| {
        two_agents(m);
        let id = m
            .send(
                &Identity::new(Harness::Codex, "x1"),
                "codex",
                "claude",
                "hi",
                at - 300,
            )
            .map(|a| a.id)
            .unwrap_or_default();
        m.present(&Identity::new(Harness::ClaudeCode, "c1"), at - 120);
        id
    });
    let id = made.unwrap_or_default();
    assert_ne!(id, "");
    let run = sb.run(&["status"], "", &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let after = store
        .read()
        .ok()
        .and_then(|m| m.message(&id).map(|d| (d.failed_attempts, d.presented_at)));
    assert_eq!(after, Some((1, None)));
}
