//! Lunchroom 1.1, unit C1: what the hooks tell (D1.8, §16 item 2, K1.28). Each test is derived from
//! a failure class of the unit's list (named in its comment).

// A test of a list that is empty says so by `is_empty`, as the clause does; a test that cannot go
// on without its setup says so by `expect`.
#![allow(clippy::assert_is_empty, clippy::expect_used)]

use super::*;

use lunchroom_core::mail::Origin;
use lunchroom_core::mail::calls::Hearer;
use lunchroom_core::mail::lines::Who;

fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("claude-c1")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// An environment where this test process stands for an attended, interactive Claude Code.
fn person_env(dir: &Path, now: u64) -> Env {
    Env {
        home: dir.join("home"),
        lunchroom_home: dir.join("lunchroom"),
        codex_home: dir.join("home/.codex"),
        claude_home: dir.join("home/.claude"),
        hermes_home: dir.join("home/.hermes"),
        claude_pid: Some(std::process::id()),
        attended: Some("1".to_owned()),
        entrypoint: Some("cli".to_owned()),
        claude_session: None,
        codex_thread: None,
        hermes_session: None,
        lunchroom_session: None,
        now,
    }
}

fn claude_id(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn open(env: &Env) -> Store<Mail> {
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        unreachable!("the store could not be opened");
    };
    store
}

/// Claude (c1) HELD by Codex's message (x1), both named.
fn held_pair(store: &Store<Mail>) -> Identity {
    let codex = Identity::new(Harness::Codex, "x1");
    let me = claude_id("c1");
    let _ = store.update(|m| {
        m.observe(&codex);
        m.observe(&me);
        assert!(m.register("codex", &codex, Origin::User, None).is_ok());
        assert!(m.register("claude", &me, Origin::User, None).is_ok());
        let id = m
            .send(&codex, "codex", "claude", "merge it", 0)
            .map(|a| a.id)
            .unwrap_or_default();
        let _ = m.confirm_at(&me, &id, 1);
    });
    codex
}

fn post(session: &str, tool_use_id: &str) -> HookInput {
    let Ok(input) = HookInput::parse(
        &json!({
            "session_id": session, "cwd": "/w", "tool_name": "Bash", "tool_use_id": tool_use_id,
            "tool_input": {"command": "gh pr merge 4"}, "tool_response": "ok",
        })
        .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    input
}

fn context_of(out: &HookOut) -> Option<String> {
    let (stdout, _, _) = out.render();
    let v: Value = serde_json::from_str(&stdout?).ok()?;
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .map(str::to_owned)
}

fn tell_me(store: &Store<Mail>, me: &Identity, words: &str) {
    let _ = store.update(|m| {
        m.tell_line(
            100,
            words,
            Who::Agents(&["codex"]),
            std::slice::from_ref(me),
            &[],
            &[Hearer {
                who: me.clone(),
                d: true,
                n: false,
            }],
            false,
        );
    });
}

// C1-D01, D06, D08: PostToolUse returns the session's notices as additionalContext, marks them
// told once the answer is produced, and does not tell them again.
#[test]
fn c1_cc_posttool_returns_additional_context_once() {
    let dir = scratch("posttool-context");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    tell_me(&store, &me, "Mistral is UNGUARDED.");
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    let text = context_of(&out).expect("additionalContext");
    assert!(
        text.contains("00:01 UTC Lunchroom: Mistral is UNGUARDED. (codex)"),
        "{text}"
    );
    assert!(
        !text.contains("Lunchroom: 00:01"),
        "no word in front of a line"
    );
    let (stdout, _, _) = out.render();
    let v: Value = serde_json::from_str(&stdout.unwrap_or_default()).unwrap_or_default();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert!(
        store.read().expect("read").notices(&me).is_empty(),
        "marked told"
    );
    let again = posttool(&post("c1", "t2"), &env).expect("posttool");
    assert!(context_of(&again).is_none(), "told once");
}

// C1-D04: no notice, no output.
#[test]
fn c1_cc_posttool_silent_when_none() {
    let dir = scratch("posttool-silent");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let _ = held_pair(&store);
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    assert_eq!(out.render(), (None, None, 0));
}

// C1-D02: the user's notices are not told to the agent.
#[test]
fn c1_cc_posttool_no_user_notice() {
    let dir = scratch("posttool-no-user");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let _ = held_pair(&store);
    let _ = store.update(|m| m.tell_user("a user-only notice", 100));
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    assert!(context_of(&out).is_none());
    let still = store.read().expect("read");
    assert!(
        still
            .user_notices()
            .iter()
            .any(|n| n.text == "a user-only notice"),
        "still the user's"
    );
}

// C1-M07 (D1.8 "a tool-call hook included"): a `claude -p` run is told nothing by PostToolUse and
// its notices stay.
#[test]
fn c1_cc_print_mode_posttool_told_nothing() {
    let dir = scratch("posttool-print");
    let mut env = person_env(&dir, 200);
    // A process whose command line is `sh -p -c ...`: `-p` is what `claude -p` carries.
    let mut child = std::process::Command::new("sh")
        .args(["-p", "-c", "sleep 20"])
        .spawn()
        .expect("spawn");
    env.claude_pid = Some(child.id());
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    tell_me(&store, &me, "Mistral is UNGUARDED.");
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    let _ = child.kill();
    let _ = child.wait();
    assert!(context_of(&out).is_none(), "{out:?}");
    assert_eq!(store.read().expect("read").notices(&me).len(), 1, "stays");
}

// C1-D11: the call ran: Granted to the user, the waiting agent and, in Claude Code, the asker.
#[test]
fn c1_cc_granted_on_posttool() {
    let dir = scratch("granted");
    let env = person_env(&dir, 14 * 3_600 + 60);
    let store = open(&env);
    let me = claude_id("c1");
    let codex = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(
            &me,
            "claude",
            "t1",
            "merge",
            14 * 3_600,
            false,
            true,
            &|_, _| true,
        );
    });
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    assert!(
        context_of(&out)
            .unwrap_or_default()
            .contains("14:01 UTC Lunchroom: Granted. claude can merge. (claude)"),
        "{out:?}"
    );
    let m = store.read().expect("read");
    assert!(m.waiting_calls().is_empty());
    assert!(
        m.notices(&codex)
            .iter()
            .any(|n| n.text.contains("Granted. claude can merge.")),
        "the waiting agent"
    );
    assert!(
        m.user_notices()
            .iter()
            .any(|n| n.text.contains("Granted. claude can merge."))
    );
}

// C1-D18: a late PostToolUse after the call was told Denied tells nothing and does not Grant.
#[test]
fn c1_cc_posttool_after_denied_is_ignored() {
    let dir = scratch("late");
    let env = person_env(&dir, 300);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(&me, "claude", "t1", "merge", 0, false, true, &|_, _| true);
        assert!(m.call_ends_in(&me, "t1", Outcome::Denied, 5, None));
    });
    let before = store.read().expect("read").user_notices().len();
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    assert!(!context_of(&out).unwrap_or_default().contains("Granted"));
    assert_eq!(store.read().expect("read").user_notices().len(), before);
}

fn prompt_input(session: &str) -> HookInput {
    let Ok(input) = HookInput::parse(
        &json!({"session_id": session, "cwd": "/w", "prompt": "run the tests again"}).to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    input
}

// C1-M08, D12, D13, D19: a call waiting on the harness's prompt with no after-call hook when the
// next prompt arrives is told as Denied; one whose after-call hook came is not told twice.
#[test]
fn c1_cc_waiting_call_without_after_hook_denied_at_next_prompt() {
    let dir = scratch("denied-prompt");
    let env = person_env(&dir, 400);
    let store = open(&env);
    let me = claude_id("c1");
    let codex = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(&me, "claude", "t1", "merge", 100, false, true, &|_, _| true);
        m.call_starts(&me, "claude", "t2", "push", 101, false, true, &|_, _| true);
    });
    // t1's after-call hook arrives: Granted. t2 has none.
    let _ = posttool(&post("c1", "t1"), &env).expect("posttool");
    assert_eq!(store.read().expect("read").waiting_calls().len(), 1);
    let _ = prompt(&prompt_input("c1"), &env);
    let m = store.read().expect("read");
    assert!(m.waiting_calls().is_empty());
    // (The prompt of a person tells the user's list and takes it, so the record of lines is read.)
    let told: Vec<String> = m.lines().iter().map(|l| l.text.clone()).collect();
    assert!(
        told.iter()
            .any(|t| t.contains("Denied. claude's call t2 was not approved.")),
        "{told:?}"
    );
    assert!(
        !told.iter().any(|t| t.contains("Denied. claude's call t1")),
        "t1 ran"
    );
    // The waiting agent is not guarded in this test, so it hears by N, in its next envelope.
    assert!(
        m.pending_envelope_lines(&codex).iter().any(|l| l
            .text
            .contains("Denied. claude's call t2 was not approved.")),
        "the waiting agent"
    );
}

// C1-D14, D21: SessionStart closes it too, and a second start or prompt tells nothing more.
#[test]
fn c1_cc_waiting_call_denied_at_session_start_idempotent() {
    let dir = scratch("denied-start");
    let env = person_env(&dir, 400);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(&me, "claude", "t1", "merge", 100, false, true, &|_, _| true);
    });
    for source in ["startup", "resume", "clear", "compact"] {
        let Ok(input) = HookInput::parse(
            &json!({"session_id": "c1", "cwd": "/w", "source": source}).to_string(),
        ) else {
            unreachable!("the input did not parse");
        };
        let _ = start(&input, &env);
    }
    let m = store.read().expect("read");
    let denied = m
        .user_notices()
        .iter()
        .filter(|n| {
            n.text
                .contains("Denied. claude's call t1 was not approved.")
        })
        .count();
    assert_eq!(denied, 1);
}

// C1-D20: another session's call is not closed by this session's prompt.
#[test]
fn c1_cc_prompt_closes_only_its_own_sessions_calls() {
    let dir = scratch("denied-own");
    let env = person_env(&dir, 400);
    let store = open(&env);
    let me = claude_id("c1");
    let other = claude_id("c2");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.observe(&other);
        m.call_starts(&me, "claude", "t1", "merge", 100, false, true, &|_, _| true);
        m.call_starts(&other, "c2", "t1", "merge", 100, false, true, &|_, _| true);
    });
    let _ = prompt(&prompt_input("c1"), &env);
    let m = store.read().expect("read");
    assert_eq!(m.waiting_calls().len(), 1);
    assert_eq!(m.waiting_calls()[0].session, other);
}

fn report(v: &Value) -> Result<HookOut, String> {
    let dir = scratch("callend");
    let env = person_env(&dir, 500);
    let store = open(&env);
    let me = Identity::new(Harness::OpenClaw, "o1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.observe(&me);
        m.call_starts(
            &me,
            "oc",
            "toolcall-1",
            "merge",
            100,
            false,
            false,
            &|_, _| true,
        );
    });
    let out = callend(&v.to_string(), &env);
    let waiting = store.read().expect("read").waiting_calls().len();
    // A report that is refused leaves the call waiting.
    assert_eq!(waiting, usize::from(out.is_err()), "{v}");
    out
}

// C1-M11 (the plugins' report): each outcome the plugin names is told once; anything else is an
// error and tells nothing.
#[test]
fn c1_callend_reports_outcomes_and_refuses_the_rest() {
    let base = json!({"harness": "openclaw", "session_id": "o1", "tool_use_id": "toolcall-1"});
    let with = |outcome: &str| {
        let mut v = base.clone();
        v["outcome"] = json!(outcome);
        v
    };
    assert!(report(&with("granted")).is_ok());
    assert!(report(&with("denied")).is_ok());
    assert!(report(&with("not_approved_in_time")).is_ok());
    assert!(report(&with("approved")).is_err());
    assert!(report(&with("")).is_err());
    assert!(
        report(&json!({"harness": "openclaw", "session_id": "o1", "outcome": "denied"})).is_err()
    );
    assert!(
        report(&json!({"session_id": "o1", "tool_use_id": "toolcall-1", "outcome": "denied"}))
            .is_err()
    );
    assert!(
        report(&json!({"harness": "codex", "session_id": "o1", "tool_use_id": "toolcall-1", "outcome": "denied"}))
            .is_err()
    );
}

// C1-M11: each outcome a plugin names is told as that outcome, to the user and the waiting agent.
#[test]
fn c1_callend_tells_the_outcome_it_names() {
    for (outcome, words) in [
        ("granted", "Granted. oc can merge."),
        ("denied", "Denied. oc's call toolcall-1 was not approved."),
        (
            "not_approved_in_time",
            "Not approved in time. oc's call toolcall-1 was dropped.",
        ),
    ] {
        let dir = scratch(&format!("callend-text-{outcome}"));
        let env = person_env(&dir, 500);
        let store = open(&env);
        let me = Identity::new(Harness::OpenClaw, "o1");
        let codex = held_pair(&store);
        let _ = store.update(|m| {
            m.observe(&me);
            m.call_starts(
                &me,
                "oc",
                "toolcall-1",
                "merge",
                100,
                false,
                false,
                &|_, _| true,
            );
            // codex has a message QUEUED for the asker, so it waits on the call.
            let _ = m.send(&codex, "codex", "oc", "x", 101);
        });
        let report = json!({"harness": "openclaw", "session_id": "o1", "tool_use_id": "toolcall-1", "outcome": outcome});
        assert!(callend(&report.to_string(), &env).is_ok());
        let m = store.read().expect("read");
        assert!(
            m.user_notices()
                .iter()
                .any(|n| n.text.contains(&format!("UTC Lunchroom: {words} (oc)"))),
            "{outcome}: {:?}",
            m.user_notices()
        );
        assert!(m.waiting_calls().is_empty(), "{outcome}");
    }
}

// C1-D20: a report for another session's call closes nothing and is not an error.
#[test]
fn c1_callend_for_another_session_closes_nothing() {
    let dir = scratch("callend-other");
    let env = person_env(&dir, 500);
    let store = open(&env);
    let me = Identity::new(Harness::OpenClaw, "o1");
    let _ = store.update(|m| {
        m.observe(&me);
        m.call_starts(
            &me,
            "oc",
            "toolcall-1",
            "merge",
            100,
            false,
            false,
            &|_, _| true,
        );
    });
    let other = json!({"harness": "openclaw", "session_id": "o2", "tool_use_id": "toolcall-1", "outcome": "denied"});
    assert!(callend(&other.to_string(), &env).is_ok());
    assert_eq!(store.read().expect("read").waiting_calls().len(), 1);
    let hermes = json!({"harness": "hermes", "session_id": "o1", "tool_use_id": "toolcall-1", "outcome": "denied"});
    assert!(callend(&hermes.to_string(), &env).is_ok());
    assert_eq!(
        store.read().expect("read").waiting_calls().len(),
        1,
        "another harness"
    );
}

fn notices_input(typed: bool) -> HookInput {
    let Ok(input) = HookInput::parse(
        &json!({"harness": "openclaw", "session_id": "o1", "cwd": "/w", "user_typed": typed})
            .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    input
}

// C1-F01, M12: `guard notices` returns the session's own notices as additionalContext in a turn a
// person typed, and nothing in a turn no person typed (its notices stay).
#[test]
fn c1_notices_mode_tells_own_notices_only_in_a_person_turn() {
    let dir = scratch("notices-mode");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let me = Identity::new(Harness::OpenClaw, "o1");
    let _ = store.update(|m| {
        m.observe(&me);
        m.tell_line(
            100,
            "Mistral is UNGUARDED.",
            Who::Agents(&["codex"]),
            &[],
            &[],
            &[Hearer {
                who: me.clone(),
                d: true,
                n: false,
            }],
            false,
        );
    });
    let none = notices(&notices_input(false), &env).expect("notices");
    assert!(context_of(&none).is_none());
    assert_eq!(
        store.read().expect("read").notices(&me).len(),
        1,
        "stays untold"
    );
    let told = notices(&notices_input(true), &env).expect("notices");
    assert!(
        context_of(&told)
            .unwrap_or_default()
            .contains("00:01 UTC Lunchroom: Mistral is UNGUARDED. (codex)"),
        "{told:?}"
    );
    assert!(store.read().expect("read").notices(&me).is_empty());
}

// C1-C19: the answer is bounded; the rest stays untold, not lost.
#[test]
fn c1_hook_answer_size_bounded_rest_stays_untold() {
    let dir = scratch("bounded");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    for i in 0..40 {
        let words = format!("Claude is HELD. {i} {}", "x".repeat(900));
        tell_me(&store, &me, &words);
    }
    let out = posttool(&post("c1", "t1"), &env).expect("posttool");
    let text = context_of(&out).expect("context");
    assert!(text.len() <= 16 * 1024 + 1_100, "{}", text.len());
    let left = store.read().expect("read").notices(&me).len();
    assert!(left > 0 && left < 40, "{left}");
}

// C1-C18 (§17.10): older notices get the word; lines of Lunchroom's are shown as they are.
#[test]
fn c1_as_shown_prefixes_only_older_notices() {
    assert_eq!(
        c1::as_shown("14:00 UTC Lunchroom: Claude is HELD. (Claude)"),
        "14:00 UTC Lunchroom: Claude is HELD. (Claude)"
    );
    assert_eq!(
        c1::as_shown("lr-00000001: Not confirmed."),
        "Lunchroom: lr-00000001: Not confirmed."
    );
    assert_eq!(
        c1::as_shown("1:00 UTC Lunchroom: x"),
        "Lunchroom: 1:00 UTC Lunchroom: x"
    );
    assert_eq!(c1::as_shown(""), "Lunchroom: ");
}

// C1-I22 via the guard: a call the guard sends to the user's prompt starts to wait, and the user
// and the holder are told.
#[test]
fn c1_ask_user_starts_the_waiting_call() {
    let dir = scratch("ask-user");
    let env = person_env(&dir, 14 * 3_600 + 300);
    let store = open(&env);
    let me = claude_id("c1");
    let codex = held_pair(&store);
    let input = json!({"command": "gh pr merge 4"});
    let judged = Judged {
        me: &me,
        tool: "Bash",
        tool_input: &input,
        judged_input: &input,
        dir: "/w",
        call: CallClass::InSlip,
    };
    assert!(ask_user(&store, &judged, "toolu_1", &env).is_ok());
    let m = store.read().expect("read");
    assert_eq!(m.waiting_calls().len(), 1);
    assert_eq!(m.waiting_calls()[0].id, "toolu_1");
    assert!(!m.waiting_calls()[0].kept);
    assert!(
        m.user_notices().iter().any(|n| n
            .text
            .starts_with("14:05 UTC Lunchroom: claude has a call waiting for you (call toolu_1)")),
        "{:?}",
        m.user_notices()
    );
    // Codex is not guarded in this test: it hears by N, in its next envelope (K1.29).
    assert!(
        m.pending_envelope_lines(&codex)
            .iter()
            .any(|l| l.text.contains("Your message lr-00000001 holds claude.")),
        "the holder"
    );
    // The user's note carries no lapse and no `lunchroom approve` (the harness's prompt answers).
    let user = m
        .user_notices()
        .iter()
        .map(|n| n.text.clone())
        .collect::<String>();
    assert!(!user.contains("lapses") && !user.contains("lunchroom approve"));
}

// C1-M11, M12 (the plugins' next turn): a call recorded as waiting for an OpenClaw or Hermes
// session with no report when that session's next turn starts (`guard prompt`: before_agent_run,
// pre_llm_call) is told as Denied. This holds for a session the plugin never saw in this process
// (a gateway restart): the record is the store's, keyed by session.
#[test]
fn c1_plugin_next_turn_closes_orphan_calls_even_after_a_restart() {
    for (harness, name) in [(Harness::OpenClaw, "openclaw"), (Harness::Hermes, "hermes")] {
        let dir = scratch(&format!("orphan-{name}"));
        let env = person_env(&dir, 900);
        let store = open(&env);
        let sess = Identity::new(harness, "s9");
        let _ = held_pair(&store);
        let _ = store.update(|m| {
            m.call_starts(
                &sess,
                "agent",
                "toolcall-7",
                "merge",
                100,
                false,
                false,
                &|_, _| true,
            );
        });
        let Ok(input) = HookInput::parse(
            &json!({"harness": name, "session_id": "s9", "cwd": "/w", "prompt": "hello",
                    "user_typed": false, "can_show_user": false})
            .to_string(),
        ) else {
            unreachable!("the input did not parse");
        };
        let _ = prompt(&input, &env);
        let m = store.read().expect("read");
        assert!(m.waiting_calls().is_empty(), "{name}");
        assert!(
            m.lines().iter().any(|l| l
                .text
                .contains("Denied. agent's call toolcall-7 was not approved.")),
            "{name}"
        );
    }
}

// ---- the classes of the independent review of the unit's list ---------------------------------

fn failure_post(session: &str, tool_use_id: &str) -> HookInput {
    // `PostToolUseFailure`: the tool ran and failed; Claude Code sends `error` for the result.
    let Ok(input) = HookInput::parse(
        &json!({
            "session_id": session, "cwd": "/w", "tool_name": "Bash", "tool_use_id": tool_use_id,
            "tool_input": {"command": "gh pr merge 4"}, "error": "exit 1",
        })
        .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    input
}

// C1-O29 (D1.8 "Granted in Claude Code: `PostToolUse` or `PostToolUseFailure` reports that the
// call ran"): a call that ran and failed is Granted, not Denied; it ran.
#[test]
fn c1_cc_posttool_failure_outcome() {
    let dir = scratch("o29");
    let env = person_env(&dir, 14 * 3_600 + 60);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(
            &me,
            "claude",
            "t1",
            "merge",
            14 * 3_600,
            false,
            true,
            &|_, _| true,
        );
    });
    let out = posttool(&failure_post("c1", "t1"), &env).expect("posttool");
    assert!(
        context_of(&out)
            .unwrap_or_default()
            .contains("Granted. claude can merge."),
        "{out:?}"
    );
    let m = store.read().expect("read");
    assert!(m.waiting_calls().is_empty());
    assert!(!m.lines().iter().any(|l| l.text.contains("Denied.")));
}

// C1-O30: the after-call hook of another call leaves this one waiting and Grants nothing.
#[test]
fn c1_cc_posttool_id_mismatch() {
    let dir = scratch("o30");
    let env = person_env(&dir, 400);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(&me, "claude", "t1", "merge", 100, false, true, &|_, _| true);
    });
    let out = posttool(&post("c1", "other-call"), &env).expect("posttool");
    assert!(
        !context_of(&out).unwrap_or_default().contains("Granted"),
        "{out:?}"
    );
    let m = store.read().expect("read");
    assert_eq!(m.waiting_calls().len(), 1);
    assert!(!m.lines().iter().any(|l| l.text.contains("Granted.")));
}

// C1-O31: of two unanswered calls, the one whose after-call hook came is Granted and not Denied; the
// other stays waiting until the session's next Stop, UserPromptSubmit or SessionStart.
#[test]
fn c1_cc_close_unanswered_selective() {
    let dir = scratch("o31");
    let env = person_env(&dir, 400);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let _ = store.update(|m| {
        m.call_starts(&me, "claude", "t1", "merge", 100, false, true, &|_, _| true);
        m.call_starts(&me, "claude", "t2", "push", 101, false, true, &|_, _| true);
    });
    let _ = posttool(&post("c1", "t1"), &env).expect("posttool");
    let still = store.read().expect("read");
    assert_eq!(
        still.waiting_calls().len(),
        1,
        "t2 waits until the turn ends"
    );
    assert_eq!(still.waiting_calls()[0].id, "t2");
    let _ = prompt(&prompt_input("c1"), &env);
    let m = store.read().expect("read");
    let denied: Vec<&str> = m
        .lines()
        .iter()
        .filter(|l| l.text.contains("Denied."))
        .map(|l| l.text.as_str())
        .collect();
    // One line is recorded for the event (the user and codex hear that one line).
    assert_eq!(denied.len(), 1, "{denied:?}");
    assert!(denied.iter().all(|t| t.contains("call t2")), "{denied:?}");
}

// C1-O32: the permission prompt asked twice with one mark is one waiting call and one start line.
#[test]
fn c1_cc_prompt_duplicate_mark_idempotent() {
    let dir = scratch("o32");
    let env = person_env(&dir, 14 * 3_600 + 300);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    let input = json!({"command": "gh pr merge 4"});
    let judged = Judged {
        me: &me,
        tool: "Bash",
        tool_input: &input,
        judged_input: &input,
        dir: "/w",
        call: CallClass::InSlip,
    };
    assert!(ask_user(&store, &judged, "toolu_1", &env).is_ok());
    assert!(ask_user(&store, &judged, "toolu_1", &env).is_ok());
    let m = store.read().expect("read");
    assert_eq!(m.waiting_calls().len(), 1);
    let starts = m
        .lines()
        .iter()
        .filter(|l| l.text.contains("has a call waiting for you (call toolu_1)"))
        .count();
    assert_eq!(starts, 1);
}

// C1-O48: no hook of a turn nobody typed tells or marks a notice, on any harness; the notice stays
// for `lunchroom status`.
#[test]
fn c1_nonperson_turn_matrix_all_harnesses() {
    let dir = scratch("o48");
    let store_env = person_env(&dir, 200);
    let store = open(&store_env);
    let cases: Vec<(&str, Harness, Value)> = vec![
        (
            "openclaw cron",
            Harness::OpenClaw,
            json!({"harness": "openclaw", "user_typed": false}),
        ),
        (
            "openclaw sessions_send",
            Harness::OpenClaw,
            json!({"harness": "openclaw"}),
        ),
        (
            "hermes cron",
            Harness::Hermes,
            json!({"harness": "hermes", "user_typed": false}),
        ),
        (
            "hermes subagent",
            Harness::Hermes,
            json!({"harness": "hermes"}),
        ),
    ];
    for (what, harness, extra) in cases {
        let me = Identity::new(harness, "s48");
        let _ = store.update(|m| {
            m.observe(&me);
            m.tell_line(
                100,
                "Mistral is UNGUARDED.",
                Who::Agents(&["codex"]),
                &[],
                &[],
                &[Hearer {
                    who: me.clone(),
                    d: true,
                    n: false,
                }],
                false,
            );
        });
        let mut v = json!({"session_id": "s48", "cwd": "/w"});
        for (k, val) in extra.as_object().cloned().unwrap_or_default() {
            v[k] = val;
        }
        let Ok(input) = HookInput::parse(&v.to_string()) else {
            unreachable!("the input did not parse");
        };
        let out = notices(&input, &store_env).expect("notices");
        assert!(context_of(&out).is_none(), "{what}");
        assert_eq!(
            store.read().expect("read").notices(&me).len(),
            1,
            "{what}: still untold"
        );
        // `lunchroom status` lists it.
        let mut text = String::new();
        crate::cli::notice_lines(&mut text, &store.read().expect("read"), Some(&me));
        assert!(text.contains("UNGUARDED"), "{what}: {text}");
    }
    // Claude Code `-p` is covered by c1_cc_print_mode_posttool_told_nothing.
}

// C1-O60 (the environment half; the command line carrying the id and no text is the hand-over
// unit's, D1.1): the hand-over program is given only the recipient's own variables from the
// allow-list; a hostile `LD_PRELOAD`, `NODE_OPTIONS` or `PATH` trick in the recipient's process
// that is not on the list, and any variable of the sender's, never reach it.
#[test]
fn c1_handover_environment_is_recipient_scoped() {
    // A sender-only variable in this (the sender's) process.
    // SAFETY: this test process is multi-threaded; the variable is only read by name below.
    let mut child = std::process::Command::new("env")
        .args([
            "-i",
            "PATH=/usr/bin:/bin",
            "LC_ALL=C",
            "LD_PRELOAD=/tmp/evil.so",
            "NODE_OPTIONS=--require=/tmp/evil.js",
            "SENDER_ONLY=1",
            "LANG=x\u{1}y",
            "sleep",
            "30",
        ])
        .spawn()
        .expect("spawn");
    let process = format!("{}:0", child.id());
    // `env` has exec'd `sleep` once its environment is the one given.
    let mut got = Vec::new();
    for _ in 0..50 {
        got = crate::codex::recipient_env(&process, &crate::codex::CHILD_ENV).unwrap_or_default();
        if got.iter().any(|(k, _)| k == "LC_ALL") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let names: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        names.contains(&"PATH") && names.contains(&"LC_ALL"),
        "{names:?}"
    );
    for bad in ["LD_PRELOAD", "NODE_OPTIONS", "SENDER_ONLY"] {
        assert!(!names.contains(&bad), "{bad}");
    }
    // A process that does not exist gives none: the hand-over does not start (D1.1).
    assert_eq!(
        crate::codex::recipient_env("4294967295:0", &crate::codex::CHILD_ENV),
        None
    );
    assert_eq!(
        crate::codex::recipient_env("not-a-pid", &crate::codex::CHILD_ENV),
        None
    );
}

// C1-O49: the first word of a Codex command line after its options and their values.
#[test]
fn c1_codex_command_word_parser_boundaries() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
    for headless in [
        v(&["codex", "-m", "m", "exec"]),
        v(&["codex", "-c", "k=v", "e"]),
        v(&["codex", "--profile", "p", "review"]),
        v(&["codex", "exec"]),
        v(&["codex", "review", "--uncommitted"]),
        // An option the table does not hold counts as a program's prompt.
        v(&["codex", "--newopt", "exec"]),
        v(&["codex", "--newopt"]),
    ] {
        assert!(codex_headless(&headless), "{headless:?}");
    }
    for person in [
        v(&["codex"]),
        v(&["codex", "resume"]),
        v(&["codex", "-m", "m"]),
        v(&["codex", "-m", "exec"]),
        v(&["codex", "--profile", "review"]),
        v(&[]),
    ] {
        assert!(!codex_headless(&person), "{person:?}");
    }
}

// C1-O51: a store that cannot be used gives an error, tells nothing and marks nothing.
#[test]
fn c1_hook_timeout_atomicity() {
    let dir = scratch("o51");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let me = claude_id("c1");
    let _ = held_pair(&store);
    tell_me(&store, &me, "Mistral is UNGUARDED.");
    // The store's folder is now a file: nothing can be read or written.
    let mut broken = env.clone();
    broken.lunchroom_home = dir.join("not-a-folder");
    let _ = std::fs::write(&broken.lunchroom_home, b"x");
    assert!(posttool(&post("c1", "t1"), &broken).is_err());
    assert!(notices(&notices_input(true), &broken).is_err());
    // What was pending is exactly as it was.
    assert_eq!(store.read().expect("read").notices(&me).len(), 1);
}

// C1-O54: the console reads every committed line while lines are recorded, and `lunchroom doctor`
// prints a line that is not a FAIL, with the exit code as it was, when the file of the lines passes
// 64 MiB.
#[test]
fn c1_console_and_doctor_race_at_64mib() {
    let dir = scratch("o54");
    let env = person_env(&dir, 200);
    let store = open(&env);
    let writer = {
        let d = env.store_dir();
        std::thread::spawn(move || {
            let s = Store::<Mail>::open(&d).expect("open");
            for i in 0..40u64 {
                let _ = s.update(|m| {
                    m.record_line(i, "Claude is HELD.", Who::Agents(&["A"]), &[], &[]);
                });
            }
        })
    };
    let mut last = 0;
    for _ in 0..40 {
        let n = store.read().map_or(0, |m| m.lines().len());
        assert!(n >= last, "a committed line never disappears: {n} < {last}");
        last = n;
    }
    writer.join().expect("writer");
    assert_eq!(store.read().expect("read").lines().len(), 40);
    // The file of the lines at 64 MiB exactly: no line; one byte past: a NOTE, never a FAIL.
    let side = env.store_dir().join("state.side.json");
    let at = |len: u64| {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&side)
            .expect("side file");
        f.set_len(len).expect("set_len");
    };
    at(64 * 1024 * 1024);
    assert_eq!(crate::cli::lines_file_note(&env.store_dir()), None);
    at(64 * 1024 * 1024 + 1);
    let note = crate::cli::lines_file_note(&env.store_dir()).expect("a note");
    assert!(note.starts_with("NOTE "), "{note}");
    assert!(!note.contains("FAIL"));
}

// F2 design, "Finding and starting the console" item 4: the SessionStart line of a joined session.
// Failure classes: a joined session start without the open line while no console runs, or with it
// while one runs; a session not joined that gets the line (its start line stays the include
// line); and (the design's silence on a console not set up) the start line alone there.

#[cfg(feature = "console")]
const JOINED_START: &str =
    "Lunchroom: this session is in Lunchroom as alice; type leave to take it out.";

#[cfg(feature = "console")]
fn session_start(env: &Env, joined: bool, set_up: bool) -> Option<String> {
    let me = claude_id("c1");
    let store = open(env);
    if joined {
        let _ = store.update(|m| {
            m.observe(&me);
            if m.name_of(&me).is_none() {
                assert!(m.register("alice", &me, Origin::User, None).is_ok());
            }
        });
    }
    let input = HookInput::parse(
        &json!({"session_id": "c1", "cwd": "/w", "source": "startup"}).to_string(),
    )
    .expect("hook input");
    match start_with(&input, env, set_up) {
        HookOut::Event { notice, .. } => notice,
        other => unreachable!("a session start answers with an event: {other:?}"),
    }
}

// Joined, set up, no console running: the start line, a space, the open line. Fails before the
// change: the notice is the start line alone, so the equality fails (and `start_with` is absent).
#[cfg(feature = "console")]
#[test]
fn a_joined_start_with_no_console_running_ends_with_the_open_line() {
    let dir = scratch("start-none");
    let env = person_env(&dir, 100);
    assert_eq!(
        session_start(&env, true, true),
        Some(format!("{JOINED_START} {}", crate::cli::OPEN_CONSOLE_LINE))
    );
    // A liveness file of a process that is gone is no console running.
    assert!(std::fs::create_dir_all(&env.lunchroom_home).is_ok());
    let alive = env.lunchroom_home.join(crate::console::ALIVE_FILE);
    assert!(std::fs::write(alive, "999999999 1\n").is_ok());
    assert_eq!(
        session_start(&env, true, true),
        Some(format!("{JOINED_START} {}", crate::cli::OPEN_CONSOLE_LINE))
    );
}

// Joined, set up, a console running (a live liveness file): the start line and `Your console is
// open.`, and no open line. Fails before the change: the start line alone.
#[cfg(feature = "console")]
#[test]
fn a_joined_start_with_a_console_running_says_it_is_open() {
    let dir = scratch("start-running");
    let env = person_env(&dir, 100);
    assert!(std::fs::create_dir_all(&env.lunchroom_home).is_ok());
    let text = crate::console::alive_text(std::process::id()).expect("this process's key");
    let alive = env.lunchroom_home.join(crate::console::ALIVE_FILE);
    assert!(std::fs::write(alive, text).is_ok());
    assert_eq!(
        session_start(&env, true, true),
        Some(format!("{JOINED_START} Your console is open."))
    );
}

// A session not joined keeps its include line, whether the console is set up or not, running or
// not. Passes before the change too (it guards the arm the change must not touch).
#[cfg(feature = "console")]
#[test]
fn a_session_not_joined_keeps_its_include_line() {
    let dir = scratch("start-not-joined");
    let env = person_env(&dir, 100);
    for set_up in [true, false] {
        assert_eq!(
            session_start(&env, false, set_up).as_deref(),
            Some(INCLUDE_LINE),
            "set up: {set_up}"
        );
    }
}

// The console not set up: the start line alone (the design adds the line "when the console is set
// up"; for the other case it says nothing, so nothing is added). Passes before the change too.
#[cfg(feature = "console")]
#[test]
fn a_joined_start_with_no_console_set_up_is_the_start_line_alone() {
    let dir = scratch("start-not-set-up");
    let env = person_env(&dir, 100);
    assert_eq!(
        session_start(&env, true, false).as_deref(),
        Some(JOINED_START)
    );
}
