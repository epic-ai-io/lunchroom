//! Tests for the Claude Code adapter's messages and timeout path (findings G4-1, G4-6, G4-7,
//! G4-8, G4-10, G4-12, G4-13, G4-14 of the Lunchroom 1.1.0 review). Each test is derived from the
//! failure enumeration of its finding, not from the code that fixes it.

use super::*;

use lunchroom_core::mail::WAITING_TOLD_SECS;

/// A folder of this test's own under the build folder.
fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("claude-g4")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn args(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_owned).collect()
}

/// A tool call the guard is asked to judge.
fn denied(input: &Value, call: CallClass) -> DeniedCall<'_> {
    DeniedCall {
        reason: DenyReason::NoApprovalChannel,
        tool: "Bash",
        judged_tool: "Bash",
        judged_input: input,
        fault: None,
        agent: "builder".to_owned(),
        held_by: vec!["alice (lr-00000045)".to_owned()],
        call,
        mode: HoldMode::Strict,
        undecided: None,
    }
}

fn line_with<'t>(text: &'t str, start: &str) -> &'t str {
    text.lines()
        .find(|l| l.starts_with(start))
        .unwrap_or_default()
}

const HELD_FIRST: &str = "builder is HELD. HELD: a message from another agent is in this session, \
                          so Lunchroom restricts what it may do.";
const NO_PROMPT: &str = "so it needs the user's yes, and this session has no permission prompt \
                         to ask for it. This session is HELD by alice (lr-00000045).";

// G4-1 (R1 B2): a PROTECTED call was printed as "outside PROTECTED (deleting a file)". Every
// ProtectedKind says what it is; a call that is not PROTECTED says which list it is outside of.
#[test]
fn a_protected_call_is_protected_and_is_never_called_outside_protected() {
    let input = json!({ "command": "rm a.txt" });
    let reason_word = DenyReason::Unparsed.word();
    let kinds = [
        (ProtectedKind::PushOrPublish, "push or publish".to_owned()),
        (ProtectedKind::Delete, "deleting a file".to_owned()),
        (
            ProtectedKind::Network,
            "sending data out of this computer".to_owned(),
        ),
        (ProtectedKind::Credential, "credentials".to_owned()),
        (
            ProtectedKind::GuardConfig,
            "Lunchroom's program or the guard's configuration".to_owned(),
        ),
        (
            ProtectedKind::Unparsed,
            format!("a command Lunchroom cannot read · {reason_word}"),
        ),
        (
            ProtectedKind::UnknownTool,
            "a tool Lunchroom does not know".to_owned(),
        ),
        (
            ProtectedKind::OtherSession,
            "text sent to another agent's session".to_owned(),
        ),
    ];
    for (kind, what) in kinds {
        let text = explain(&denied(&input, CallClass::Protected(kind)));
        assert_eq!(
            line_with(&text, "Why: "),
            format!("Why: {HELD_FIRST} This call is PROTECTED ({what}), {NO_PROMPT}"),
            "{kind:?}: {text}"
        );
        assert!(!text.contains("outside PROTECTED"), "{kind:?}: {text}");
    }
}

#[test]
fn a_call_outside_the_permission_slip_says_what_the_slip_is() {
    let input = json!({ "command": "touch a.txt" });
    let text = explain(&denied(&input, CallClass::Ordinary));
    assert_eq!(
        line_with(&text, "Why: "),
        format!(
            "Why: {HELD_FIRST} This call is outside the permission slip of a HELD session (the \
             short list of calls a HELD agent may make without the user's yes), {NO_PROMPT}"
        ),
        "{text}"
    );
}

// G4-1 and G4-15: the text of the permission prompt starts with the whole short answer (who is
// HELD, which kind of call, what to do), HELD is explained where it is first printed, and the
// slip case is not called "outside PROTECTED".
#[test]
fn the_permission_prompt_text_leads_with_the_whole_answer() {
    let dir = scratch("ask");
    let Ok(store) = Store::<Mail>::open(&dir) else {
        unreachable!("the store could not be opened");
    };
    let me = Identity::new(Harness::ClaudeCode, "builder");
    let input = json!({ "command": "rm a.txt" });
    let held_means = "HELD: a message from another agent is in this session, so Lunchroom restricts \
                      what it may do.";
    for (call, sentence) in [
        (
            CallClass::Protected(ProtectedKind::Delete),
            "This call is PROTECTED (deleting a file), so it needs your yes.",
        ),
        (
            CallClass::Ordinary,
            "This call is outside the permission slip of a HELD session (the short list of calls \
             a HELD agent may make without the user's yes), so it needs your yes.",
        ),
    ] {
        let judged = Judged {
            me: &me,
            tool: "Bash",
            tool_input: &input,
            dir: "/work",
            call,
        };
        let Ok(HookOut::Ask(text)) = ask_user(&store, &judged, "t1") else {
            unreachable!("no permission prompt");
        };
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            format!(
                "Lunchroom: builder is HELD. {sentence} Answer this permission prompt: yes runs \
                 the call once, no stops it."
            ),
            "{text}"
        );
        assert_eq!(lines[1], held_means, "{text}");
        assert!(!text.contains("outside PROTECTED"), "{text}");
    }
}

// G4-1 (stored text): the sentence kept with a call for `lunchroom approve` stands alone.
#[test]
fn the_rule_kept_for_approve_is_a_whole_sentence() {
    assert_eq!(
        call_rule_sentence(CallClass::Protected(ProtectedKind::Delete)),
        "This call is PROTECTED (deleting a file), so it needs your yes."
    );
}

// G4-10: the words HELD and permission slip are explained wherever a DENIED call prints them, and
// the ways forward name no command that loosens Lunchroom for a HELD agent.
#[test]
fn held_and_the_permission_slip_are_explained_where_they_are_printed() {
    let input = json!({ "command": "touch a.txt" });
    let text = explain(&denied(&input, CallClass::Ordinary));
    assert!(text.contains(HELD_FIRST), "{text}");
    assert!(
        text.contains("(its permission slip): read files outside the paths Lunchroom DENIES"),
        "{text}"
    );
    for retired in ["the hold", "clear the hold", "slip now"] {
        assert!(!text.contains(retired), "{retired}: {text}");
    }
    assert_eq!(
        lunchroom_core::vocab::find_retired_word(&text),
        None,
        "{text}"
    );
}

fn raw_call(command: &str) -> String {
    json!({ "tool_name": "Bash", "tool_input": { "command": command } }).to_string()
}

const TIMEOUT: &str = "no answer within 10 seconds, while the guard was reading the session's \
                       transcript /t.jsonl";
const DOCTOR: &str = "- The user: run `lunchroom doctor` in a terminal of their own: it checks \
                      Lunchroom's setup and lists the calls Lunchroom could not check in the \
                      last 24 hours.";

// G4-7 (R2 M3, R9 M5): the cause of a call the guard could not decide, read from the reason.
#[test]
fn the_cause_is_read_from_the_reason() {
    assert_eq!(
        Cause::of(TIMEOUT),
        Cause::TimedOut {
            seconds: "10",
            step: Some("reading the session's transcript /t.jsonl"),
        }
    );
    assert_eq!(
        Cause::of("no answer within 10 seconds"),
        Cause::TimedOut {
            seconds: "10",
            step: None,
        }
    );
    // A step that is named but empty is no step.
    assert_eq!(
        Cause::of("no answer within 10 seconds, while the guard was "),
        Cause::TimedOut {
            seconds: "10",
            step: None,
        }
    );
    let store = "Lunchroom's store cannot be opened: permission denied";
    assert_eq!(Cause::of(store), Cause::Store(store));
    let held = "held the store for 10 seconds";
    assert_eq!(Cause::of(held), Cause::Store(held));
    let own = "the hook input has no session id";
    assert_eq!(Cause::of(own), Cause::Own(own));
    assert_eq!(
        undecided_cause(TIMEOUT),
        "Lunchroom did not finish within 10 seconds; it was still reading the session's \
         transcript /t.jsonl"
    );
    assert_eq!(
        undecided_cause("no answer within 10 seconds"),
        "Lunchroom did not finish within 10 seconds; which step it was in is not known"
    );
}

// G4-7, H1.7: a call that goes to the user. The whole text, for each cause.
#[test]
fn a_call_that_went_to_the_user_says_what_why_and_what_to_do() {
    let raw = raw_call("touch a");
    let cases = [
        (
            TIMEOUT,
            "Lunchroom did not finish within 10 seconds",
            "Lunchroom did not finish within 10 seconds; it was still reading the session's \
             transcript /t.jsonl",
        ),
        (
            "Lunchroom's store cannot be opened: permission denied",
            "Lunchroom's store could not be used",
            "Lunchroom's store, the folder where it keeps its records, could not be used \
             (Lunchroom's store cannot be opened: permission denied)",
        ),
        (
            "the hook input has no session id",
            "Lunchroom hit an error of its own",
            "Lunchroom hit an error of its own (the hook input has no session id)",
        ),
    ];
    for (reason, short, long) in cases {
        let text = undecided_details(Some("pretool"), reason, &raw, true, false, false);
        let expected = format!(
            "Lunchroom could not check the command `touch a`: {short}. It needs your yes: answer \
             this permission prompt (yes runs the call once, no stops it). Lunchroom did not read \
             any script the call runs.\n\
             What could not be checked: the command `touch a`.\n\
             Why: {long}; a call Lunchroom cannot check goes to the user, so the user is never \
             shut out, and Lunchroom did not read any script the call runs.\n\
             Ways forward:\n\
             - The user: answer this permission prompt: yes runs this call once, no stops it.\n\
             {DOCTOR}"
        );
        assert_eq!(text, expected, "{reason}");
        assert_eq!(
            lunchroom_core::vocab::find_retired_word(&text),
            None,
            "{text}"
        );
    }
}

// G4-7, G4-8 (H1.7): a call nobody can be asked about is DENIED alone; the agent is told to try
// once more and then tell the user (not to retry without end); `lunchroom approve` is offered
// exactly when the call was kept for it.
#[test]
fn a_call_nobody_can_be_asked_about_is_denied_with_the_ways_forward() {
    let raw = raw_call("touch a");
    let reason = "the hook input has no session id";
    let first = "Lunchroom could not check the command `touch a`: Lunchroom hit an error of its \
                 own, and nobody can be asked in this session, so this one call is DENIED. The \
                 agent can try it once more; the user can run it in a terminal of their own.";
    let why = "Why: Lunchroom hit an error of its own (the hook input has no session id); a call \
               Lunchroom cannot check goes to the user, and this session has no permission \
               prompt to ask in, so that one call is DENIED.";
    let approve = "- The user: `lunchroom approve` lists this exact call; approving it lets it \
                   run once when the agent tries it again.";
    let retry = "- The agent: try the call once more. If Lunchroom still cannot check it, stop and \
                 tell the user what the call was for.";
    let terminal = "- The user: run it in a terminal of their own, outside every agent session.";
    for kept in [false, true] {
        let text = undecided_details(Some("pretool"), reason, &raw, false, false, kept);
        let mut expected = format!(
            "{first}\nWhat could not be checked: the command `touch a`.\n{why}\nWays forward:"
        );
        if kept {
            let _ = write!(expected, "\n{approve}");
        }
        let _ = write!(expected, "\n{retry}\n{terminal}\n{DOCTOR}");
        assert_eq!(text, expected, "kept: {kept}");
    }
}

// G4-7: a prompt the user types goes on; a prompt with another agent's text is blocked; a result
// goes on; a session's start goes on. The whole text of each.
#[test]
fn prompts_results_and_starts_that_could_not_be_checked_say_what_happened() {
    let timeout = "no answer within 10 seconds";
    let prompt_raw = json!({ "session_id": "s1", "cwd": "/w", "prompt": "hi" }).to_string();
    let typed = undecided_details(Some("prompt"), timeout, &prompt_raw, false, false, false);
    assert_eq!(
        typed,
        format!(
            "Lunchroom could not check the prompt sent to this session: Lunchroom did not finish \
             within 10 seconds. It went to the agent unchecked; Lunchroom never blocks what the \
             user types.\n\
             What could not be checked: the prompt sent to this session.\n\
             Why: Lunchroom did not finish within 10 seconds; which step it was in is not known; \
             Lunchroom never blocks a prompt the user types: this one went on to the agent \
             unchecked.\n\
             Ways forward:\n{DOCTOR}"
        )
    );
    let blocked = undecided_details(Some("prompt"), timeout, &prompt_raw, false, true, false);
    assert_eq!(
        blocked,
        format!(
            "Lunchroom blocked a prompt carrying another agent's text: Lunchroom did not finish \
             within 10 seconds, so it could not check the text. The user can send the prompt \
             again once `lunchroom doctor` shows Lunchroom works.\n\
             What could not be checked: a prompt carrying another agent's text.\n\
             Why: Lunchroom did not finish within 10 seconds; which step it was in is not known; \
             a prompt carrying another agent's text is blocked when Lunchroom cannot check it, \
             because Lunchroom could not then mark the session HELD (HELD: {HELD_MEANS}).\n\
             Ways forward:\n{DOCTOR}"
        )
    );
    let post_raw = json!({ "session_id": "s1", "cwd": "/w", "tool_name": "Bash" }).to_string();
    let result = undecided_details(Some("posttool"), timeout, &post_raw, false, false, false);
    assert_eq!(
        result,
        format!(
            "Lunchroom could not check the result of the Bash call: Lunchroom did not finish \
             within 10 seconds. The result went to the agent unchecked.\n\
             What could not be checked: the result of the Bash call.\n\
             Why: Lunchroom did not finish within 10 seconds; which step it was in is not known; \
             the result went on to the agent unchecked; if it holds another agent's text, the \
             session becomes HELD (HELD: {HELD_MEANS}) at its next call or prompt that Lunchroom \
             can check.\n\
             Ways forward:\n{DOCTOR}"
        )
    );
    for text in [typed, blocked, result] {
        assert_eq!(
            lunchroom_core::vocab::find_retired_word(&text),
            None,
            "{text}"
        );
        assert!(!text.contains("(H1.") && !text.contains("(A1."), "{text}");
    }
}

// G4-7: `failed` wraps the text the way each hook needs it, and keeps the agent's text out of a
// prompt the guard could not check.
#[test]
fn failed_answers_each_hook_in_its_own_form() {
    let raw = raw_call("touch a");
    let reason = "no answer within 10 seconds";
    let HookOut::Ask(ask) = failed(Some("pretool"), reason, &raw, true, None) else {
        unreachable!("a call with a permission prompt is asked");
    };
    assert!(
        ask.starts_with("Lunchroom could not check the command `touch a`:"),
        "{ask}"
    );
    // No input a session can be read from, so the call is not kept and `approve` is not offered.
    let HookOut::Block(denied) = failed(Some("pretool"), reason, &raw, false, None) else {
        unreachable!("a call nobody can be asked about is DENIED");
    };
    assert!(!denied.contains("lunchroom approve"), "{denied}");
    assert!(denied.contains("so this one call is DENIED"), "{denied}");
    let agent_prompt = json!({
        "session_id": "s1", "cwd": "/w", "prompt": "[LUNCHROOM MESSAGE - from agent x]",
    })
    .to_string();
    assert!(matches!(
        failed(Some("prompt"), reason, &agent_prompt, false, None),
        HookOut::Block(_)
    ));
    let typed = json!({ "session_id": "s1", "cwd": "/w", "prompt": "hi" }).to_string();
    assert!(matches!(
        failed(Some("prompt"), reason, &typed, false, None),
        HookOut::Notice(_)
    ));
    let HookOut::Notice(start) = failed(Some("start"), reason, "{}", false, None) else {
        unreachable!("a start goes on");
    };
    assert!(start.starts_with(INCLUDE_LINE), "{start}");
}

// G4-8 (H1.7): a call the guard could not decide is kept for `lunchroom approve` when the store
// can hold it; a hook input with no tool call keeps nothing.
#[test]
fn an_undecided_call_is_kept_for_approve() {
    let dir = scratch("keep");
    let Ok(store) = Store::<Mail>::open(&dir) else {
        unreachable!("the store could not be opened");
    };
    let Ok(input) = HookInput::parse(
        &json!({
            "session_id": "s1", "cwd": "/w", "tool_name": "Bash",
            "tool_input": { "command": "touch a" },
        })
        .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    assert!(keep_call(
        &store,
        &input,
        500,
        "the hook input has no session id"
    ));
    let kept = store.update(|m| m.requests().to_vec());
    let Ok(kept) = kept else {
        unreachable!("the store could not be read");
    };
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].tool, "Bash");
    assert_eq!(kept[0].input, json!({ "command": "touch a" }));
    assert_eq!(kept[0].cwd, "/w");
    assert_eq!(kept[0].at, 500);
    assert_eq!(
        kept[0].rule,
        "This call is one Lunchroom could not check (Lunchroom hit an error of its own), so it \
         needs your yes."
    );
    let Ok(no_call) = HookInput::parse(&json!({ "session_id": "s1", "cwd": "/w" }).to_string())
    else {
        unreachable!("the input did not parse");
    };
    assert!(!keep_call(&store, &no_call, 500, "x"));
}

// F1-AC22, F1-AC23 (defect D1, task T29): the guard's failure path keeps an undecided call in the
// store of the environment it is given, never in the store of the process's own home. Before the
// fix `failed` took its home from the process, so this caller's store stayed empty.
#[test]
fn an_undecided_call_is_kept_under_the_callers_home() {
    let home = scratch("d1-home");
    let env = Env {
        home: home.clone(),
        lunchroom_home: home.join(".lunchroom"),
        codex_home: home.join(".codex"),
        claude_home: home.join(".claude"),
        hermes_home: home.join(".hermes"),
        claude_pid: None,
        attended: None,
        entrypoint: None,
        claude_session: None,
        codex_thread: None,
        hermes_session: None,
        lunchroom_session: None,
        now: 500,
    };
    let raw = json!({
        "session_id": "s1", "cwd": "/w", "tool_name": "Bash",
        "tool_input": { "command": "touch a" },
    })
    .to_string();
    let _ = failed(
        Some("pretool"),
        "the hook input has no session id",
        &raw,
        false,
        Some(&env),
    );
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        unreachable!("the caller's store could not be opened");
    };
    let Ok(kept) = store.update(|m| m.requests().to_vec()) else {
        unreachable!("the caller's store could not be read");
    };
    assert_eq!(kept.len(), 1, "the call is kept in the caller's store");
    assert_eq!(kept[0].input, json!({ "command": "touch a" }));
}

// G4-7, H1.5: a call the rules that need no record DENY is DENIED with the true cause named, not
// "Lunchroom's store could not be used" for every error.
#[test]
fn a_call_denied_without_a_record_names_the_true_cause() {
    if Env::from_process().is_err() {
        return;
    }
    let raw = json!({
        "session_id": "s1", "cwd": "/tmp", "tool_name": "Bash",
        "tool_input": { "command": "codex queue t1 hello" },
    })
    .to_string();
    let Some(HookOut::Block(text)) = failed_call(&raw, "the hook input has no session id") else {
        unreachable!("a fence is DENIED by the rules that need no record");
    };
    assert!(
        text.contains(
            "Lunchroom hit an error of its own (the hook input has no session id), so Lunchroom \
             judged this call as it does for a session that is HELD"
        ),
        "{text}"
    );
    assert!(!text.contains("store could not be used"), "{text}");
    let Some(HookOut::Block(text)) = failed_call(&raw, "Lunchroom's store cannot be opened: x")
    else {
        unreachable!("a fence is DENIED by the rules that need no record");
    };
    assert!(text.contains("could not be used"), "{text}");
}

// G4-6 (R3 M8): which prompts a person typed. Codex's `exec` (alias `e`) and `review` run without
// one; an interactive Codex, and any command line that cannot be told, count as a person.
#[test]
fn a_codex_run_without_a_person_is_told_from_its_command_line() {
    for headless in [
        "codex exec fix the tests",
        "/usr/bin/codex e hello",
        "codex review",
        "codex --search exec hello",
    ] {
        assert!(codex_headless(&args(headless)), "{headless}");
    }
    for person in [
        "codex",
        "codex resume abc",
        "codex --model o3",
        "codex hello exec",
    ] {
        assert!(!codex_headless(&args(person)), "{person}");
    }
    assert!(!codex_headless(&[]));
}

fn env_in(dir: &Path, now: u64) -> Env {
    Env {
        home: dir.join("home"),
        lunchroom_home: dir.join("lunchroom"),
        codex_home: dir.join("home/.codex"),
        claude_home: dir.join("home/.claude"),
        hermes_home: dir.join("home/.hermes"),
        claude_pid: None,
        attended: None,
        entrypoint: None,
        claude_session: None,
        codex_thread: None,
        hermes_session: None,
        lunchroom_session: None,
        now,
    }
}

/// A store in `env` where `sender` (a session of `harness`) sent a message to `other` that cannot
/// be handed over, and the sender and the user have a notice about it.
fn store_with_notices(env: &Env, harness: Harness) -> Option<Store<Mail>> {
    let store = Store::<Mail>::open(&env.store_dir()).ok()?;
    let sender = Identity::new(harness, "s1");
    let other = Identity::new(Harness::ClaudeCode, "o1");
    store
        .update(|m| {
            for (name, who) in [("sender", &sender), ("other", &other)] {
                m.observe(who);
                let _ = m.register(name, who, Origin::User, None);
            }
            let id = m
                .send(&sender, "sender", "other", "hi", 0)
                .map(|a| a.id)
                .unwrap_or_default();
            m.note_unreachable(&id, Some("other is STOPPED.".to_owned()));
            m.tell_waiting(WAITING_TOLD_SECS);
        })
        .ok()?;
    Some(store)
}

fn counts(store: &Store<Mail>, who: &Identity) -> (usize, usize) {
    match store.update(|m| (m.user_notices().len(), m.notices(who).len())) {
        Ok(n) => n,
        Err(e) => {
            unreachable!("the store could not be read: {e}")
        }
    }
}

// G4-6 (R3 M8, R2 M4): a prompt no person typed (`claude -p`: no approval channel) is told
// nothing, and the notices wait for the next prompt a person types.
#[test]
fn a_prompt_no_person_typed_is_told_nothing() {
    let dir = scratch("headless");
    let env = env_in(&dir, WAITING_TOLD_SECS + 1);
    let Some(store) = store_with_notices(&env, Harness::ClaudeCode) else {
        unreachable!("the store could not be set up");
    };
    let sender = Identity::new(Harness::ClaudeCode, "s1");
    let before = counts(&store, &sender);
    assert!(before.0 > 0 && before.1 > 0, "{before:?}");
    let Ok(input) = HookInput::parse(
        &json!({ "session_id": "s1", "cwd": "/tmp", "prompt": "hello", "prompt_id": "p1" })
            .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    assert!(!channel_of(&input, &env));
    assert_eq!(prompt(&input, &env), Ok(HookOut::Silent));
    assert_eq!(counts(&store, &sender), before);
}

// G4-2, G4-3: where a plugin can show the user no text on a prompt that goes on (OpenClaw,
// Hermes), the user's notices are not taken, and the agent's still are where the plugin can add
// them to the agent's context (Hermes).
#[test]
fn notices_are_taken_only_where_they_can_be_shown() {
    let dir = scratch("hermes");
    let env = env_in(&dir, WAITING_TOLD_SECS + 1);
    let Some(store) = store_with_notices(&env, Harness::Hermes) else {
        unreachable!("the store could not be set up");
    };
    let sender = Identity::new(Harness::Hermes, "s1");
    let (user_before, agent_before) = counts(&store, &sender);
    assert!(user_before > 0 && agent_before > 0);
    let input = |extra: Value| {
        let mut v = json!({
            "harness": "hermes", "session_id": "s1", "cwd": "/tmp", "prompt": "hello",
            "prompt_id": "p1", "user_typed": true,
        });
        if let (Some(base), Some(more)) = (v.as_object_mut(), extra.as_object()) {
            base.extend(more.clone());
        }
        HookInput::parse(&v.to_string())
    };
    // Neither can be shown (OpenClaw): nothing is taken.
    let Ok(none) = input(json!({ "can_show_user": false, "can_tell_agent": false })) else {
        unreachable!("the input did not parse");
    };
    assert_eq!(prompt(&none, &env), Ok(HookOut::Silent));
    assert_eq!(counts(&store, &sender), (user_before, agent_before));
    // Only the agent can be told (Hermes): its notices are given, the user's stay.
    let Ok(agent_only) = input(json!({ "can_show_user": false })) else {
        unreachable!("the input did not parse");
    };
    match prompt(&agent_only, &env) {
        Ok(HookOut::Event {
            notice: None,
            context: Some(context),
            ..
        }) => assert!(context.starts_with("Lunchroom: "), "{context}"),
        other => assert_eq!(format!("{other:?}"), "an Event with context only"),
    }
    assert_eq!(counts(&store, &sender), (user_before, 0));
}

// G2-14: through the whole prompt hook, a notice is shown once: the first prompt a person types
// carries the sender's and the user's notices, and marks them told; the second carries none.
#[test]
fn a_notice_is_shown_once_by_the_prompt_hook() {
    let dir = scratch("notice-once");
    let env = env_in(&dir, WAITING_TOLD_SECS + 1);
    let Some(store) = store_with_notices(&env, Harness::Hermes) else {
        unreachable!("the store could not be set up");
    };
    let sender = Identity::new(Harness::Hermes, "s1");
    let (user_before, agent_before) = counts(&store, &sender);
    assert!(user_before > 0 && agent_before > 0);
    let Ok(typed) = HookInput::parse(
        &json!({
            "harness": "hermes", "session_id": "s1", "cwd": "/tmp", "prompt": "hello",
            "prompt_id": "p1", "user_typed": true,
        })
        .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    match prompt(&typed, &env) {
        Ok(HookOut::Event {
            notice: Some(shown),
            context: Some(context),
            ..
        }) => {
            assert!(shown.starts_with("Lunchroom: "), "{shown}");
            assert!(context.starts_with("Lunchroom: "), "{context}");
        }
        other => assert_eq!(format!("{other:?}"), "an Event with both texts"),
    }
    assert_eq!(counts(&store, &sender), (0, 0));
    assert_eq!(prompt(&typed, &env), Ok(HookOut::Silent));
}

// G4-6, G4-2: the three forms of who can be shown what.
#[test]
fn waiting_notices_are_taken_per_channel() {
    for (shows, user_taken, agent_taken) in [
        (Shows::BOTH, true, true),
        (
            Shows {
                user: false,
                agent: true,
            },
            false,
            true,
        ),
        (
            Shows {
                user: true,
                agent: false,
            },
            true,
            false,
        ),
        (
            Shows {
                user: false,
                agent: false,
            },
            false,
            false,
        ),
    ] {
        let sender = Identity::new(Harness::OpenClaw, "s1");
        let other = Identity::new(Harness::ClaudeCode, "o1");
        let mut m = Mail::default();
        for (name, who) in [("sender", &sender), ("other", &other)] {
            m.observe(who);
            assert!(m.register(name, who, Origin::User, None).is_ok());
        }
        let id = m
            .send(&sender, "sender", "other", "hi", 0)
            .map(|a| a.id)
            .unwrap_or_default();
        m.note_unreachable(&id, Some("other is STOPPED.".to_owned()));
        m.tell_waiting(WAITING_TOLD_SECS);
        let (user_before, agent_before) = (m.user_notices().len(), m.notices(&sender).len());
        let (_, carried) = waiting_notices(&m, &sender, &[], shows);
        carried.mark_told(&mut m, &sender);
        assert_eq!(m.user_notices().is_empty(), user_taken, "{shows:?}");
        assert_eq!(m.notices(&sender).is_empty(), agent_taken, "{shows:?}");
        assert!(user_before > 0 && agent_before > 0);
    }
}

/// The ids a read of the transcript at `path` found, reading at most `cap` bytes.
fn transcript_ids(path: &Path, cap: u64) -> Vec<String> {
    read_transcript(path, cap, TRANSCRIPT_IDS).ids
}

fn write_transcript(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    let _ = std::fs::write(&path, bytes);
    path
}

// G4-13 (R5 F2): the transcript is read in blocks, up to a size cap; a label cut by the end of a
// block is still found, a label past the cap is not, a repeated id is given once, and a file that
// is not there gives nothing.
#[test]
fn a_transcript_is_searched_in_blocks_up_to_a_cap() {
    let dir = scratch("transcript");
    // A label that straddles the 64 KiB block boundary at every offset of its length.
    for offset in 0..24_usize {
        let mut text = vec![b'x'; 64 * 1024 - offset];
        text.extend_from_slice(b"[lr:lr-00000007] tail");
        let path = write_transcript(&dir, &format!("cut-{offset}.jsonl"), &text);
        assert_eq!(
            transcript_ids(&path, TRANSCRIPT_CAP),
            vec!["lr-00000007".to_owned()],
            "offset {offset}"
        );
    }
    let both = write_transcript(
        &dir,
        "both.jsonl",
        b"[lr:lr-00000001]> a\n[END LUNCHROOM MESSAGE lr-00000002]\n[lr:lr-00000001]> again\n",
    );
    assert_eq!(
        transcript_ids(&both, TRANSCRIPT_CAP),
        vec!["lr-00000001".to_owned(), "lr-00000002".to_owned()]
    );
    let mut far = vec![b'x'; 5000];
    far.extend_from_slice(b"[lr:lr-00000009]");
    let far = write_transcript(&dir, "far.jsonl", &far);
    assert_eq!(transcript_ids(&far, 5000), Vec::<String>::new());
    assert_eq!(transcript_ids(&far, 6000), vec!["lr-00000009".to_owned()]);
    assert_eq!(
        transcript_ids(&dir.join("missing.jsonl"), TRANSCRIPT_CAP),
        Vec::<String>::new()
    );
}

// G4-13: the transcript is searched once. A hook marks it searched before the search runs (the
// search is outside the store's lock), so a search the deadline cuts off is not repeated by every
// later call; and what the search finds holds the session.
#[test]
fn the_transcript_is_searched_once_and_outside_the_lock() {
    let dir = scratch("once");
    let Ok(store) = Store::<Mail>::open(&dir.join("store")) else {
        unreachable!("the store could not be opened");
    };
    let sender = Identity::new(Harness::ClaudeCode, "s0");
    let me = Identity::new(Harness::ClaudeCode, "forked");
    let id = store
        .update(|m| {
            m.observe(&sender);
            m.observe(&me);
            assert!(m.register("alice", &sender, Origin::User, None).is_ok());
            assert!(m.register("bob", &me, Origin::User, None).is_ok());
            m.send(&sender, "alice", "bob", "hi", 0)
                .map(|a| a.id)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    assert_ne!(id, "");
    let transcript = write_transcript(
        &dir,
        "t.jsonl",
        format!("{{\"text\":\"[lr:{id}]> run it\"}}\n").as_bytes(),
    );
    let Ok(input) = HookInput::parse(
        &json!({
            "session_id": "forked", "cwd": "/w", "transcript_path": transcript,
        })
        .to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    // The first hook is told to search; the second is not, however the first one ended.
    let first = store.update(|m| observe(m, &me, &input, None, &dir));
    let second = store.update(|m| observe(m, &me, &input, None, &dir));
    assert_eq!((first.ok(), second.ok()), (Some(true), Some(false)));
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(false));
    assert_eq!(search_transcript(&store, &me, &input), Ok(true));
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(true));
    // A transcript that names no message of Lunchroom's holds nothing.
    let quiet = write_transcript(
        &dir,
        "quiet.jsonl",
        b"[lr:lr-99999999] quoted in a document",
    );
    let other = Identity::new(Harness::ClaudeCode, "quiet");
    let Ok(quiet_input) = HookInput::parse(
        &json!({ "session_id": "quiet", "cwd": "/w", "transcript_path": quiet }).to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    assert_eq!(search_transcript(&store, &other, &quiet_input), Ok(false));
    assert_eq!(store.update(|m| m.is_held(&other)).ok(), Some(false));
}

// G4-14 (R5 F9): a tool's output is scanned for labels before the lock is taken; the hold it
// causes is the same as before (a label naming a message Lunchroom sent holds the session, a
// quoted label naming none holds nothing).
#[test]
fn a_tool_output_naming_a_message_still_holds_the_session() {
    let dir = scratch("posttool");
    let env = env_in(&dir, 10);
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        unreachable!("the store could not be opened");
    };
    let sender = Identity::new(Harness::ClaudeCode, "s0");
    let me = Identity::new(Harness::ClaudeCode, "s1");
    let id = store
        .update(|m| {
            m.observe(&sender);
            m.observe(&me);
            assert!(m.register("alice", &sender, Origin::User, None).is_ok());
            assert!(m.register("bob", &me, Origin::User, None).is_ok());
            m.send(&sender, "alice", "bob", "hi", 0)
                .map(|a| a.id)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let post = |response: &str| {
        HookInput::parse(
            &json!({
                "session_id": "s1", "cwd": "/w", "tool_name": "Bash", "tool_use_id": "t1",
                "tool_response": response,
            })
            .to_string(),
        )
    };
    let Ok(quoted) = post("docs: [lr:lr-99999999]> text") else {
        unreachable!("the input did not parse");
    };
    assert!(posttool(&quoted, &env).is_ok());
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(false));
    let Ok(real) = post(&format!("[END LUNCHROOM MESSAGE {id}]")) else {
        unreachable!("the input did not parse");
    };
    assert!(posttool(&real, &env).is_ok());
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(true));
}

// G4-12 (G2-18): a late copy of a message is blocked with the reason that is true of it: a
// DELIVERED message reached this session; a FAILED one did not.
#[test]
fn a_repeated_message_is_blocked_with_the_reason_that_is_true_of_it() {
    assert_eq!(
        repeat_block_text("lr-00000001", &Status::Delivered),
        "Lunchroom: message lr-00000001 was DELIVERED to this session before, so it is not \
         shown again."
    );
    assert_eq!(
        repeat_block_text(
            "lr-00000002",
            &Status::Failed {
                reason: "x".to_owned()
            }
        ),
        "Lunchroom: message lr-00000002 is FAILED: Lunchroom could not hand it to this session \
         in time, and its sender was told. This late copy is not shown. The sender can send it \
         again with `lunchroom send`."
    );
    // Through the hook's own path, for a message in each state.
    let sender = Identity::new(Harness::ClaudeCode, "s0");
    let me = Identity::new(Harness::ClaudeCode, "s1");
    let mut m = Mail::default();
    for (name, who) in [("alice", &sender), ("bob", &me)] {
        m.observe(who);
        assert!(m.register(name, who, Origin::User, None).is_ok());
    }
    let delivered = m
        .send(&sender, "alice", "bob", "one", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    let failed_id = m
        .send(&sender, "alice", "bob", "two", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    assert_eq!(m.confirm_at(&me, &delivered, 5), Confirmation::Show);
    for at in [10, 100, 1000] {
        let _ = m.attempt_failed(&failed_id, "no listener", at);
    }
    assert!(matches!(
        m.message(&failed_id).map(|d| &d.status),
        Some(Status::Failed { .. })
    ));
    assert_eq!(
        confirm_delivery(&mut m, &me, std::slice::from_ref(&delivered), 2000),
        HookOut::Block(repeat_block_text(&delivered, &Status::Delivered))
    );
    let HookOut::Block(text) =
        confirm_delivery(&mut m, &me, std::slice::from_ref(&failed_id), 2000)
    else {
        unreachable!("a late copy of a FAILED message is blocked");
    };
    assert!(
        text.contains(&format!("message {failed_id} is FAILED")),
        "{text}"
    );
    assert!(!text.contains("reached this session before"), "{text}");
}

// G4-4 (cross-check of the OpenClaw side): the hook input names what a plugin can show.
#[test]
fn the_hook_input_carries_what_a_plugin_can_show() {
    let parse = |extra: &str| {
        HookInput::parse(&format!(
            r#"{{"harness":"openclaw","session_id":"s","cwd":"/w"{extra}}}"#
        ))
    };
    let Ok(plain) = parse("") else {
        unreachable!("the input did not parse");
    };
    assert_eq!((plain.can_show_user, plain.can_tell_agent), (None, None));
    let Ok(none) = parse(r#","can_show_user":false,"can_tell_agent":false"#) else {
        unreachable!("the input did not parse");
    };
    assert_eq!(
        (none.can_show_user, none.can_tell_agent),
        (Some(false), Some(false))
    );
}

// G1-1 (message side): a Codex `apply_patch` call is described by the files it writes, never by
// the whole patch.
#[test]
fn an_apply_patch_call_is_described_by_the_files_it_writes() {
    let patch = "*** Begin Patch\n*** Add File: a.rs\n+x\n*** Update File: b/c.rs\n@@\n-y\n+z\n*** End Patch";
    let input = json!({ "command": patch });
    assert_eq!(
        describe_call("apply_patch", "apply_patch", &input, None),
        "the apply_patch of a.rs, b/c.rs"
    );
    let none = json!({ "command": "nothing here" });
    assert_eq!(
        describe_call("apply_patch", "apply_patch", &none, None),
        "the apply_patch call"
    );
}

// G1-7, DV-11: the way that says the user can change how strict Lunchroom is appears only where a
// looser mode would let the call run, and no message names a command that does it.
#[test]
fn the_strictness_way_is_offered_only_where_a_looser_mode_would_help() {
    let input = json!({ "command": "touch a" });
    let with = |call, mode| {
        let mut d = denied(&input, call);
        d.mode = mode;
        a_looser_mode_would_let_it_run(&d)
    };
    assert!(with(CallClass::Ordinary, HoldMode::Strict));
    assert!(!with(CallClass::Ordinary, HoldMode::Balanced));
    assert!(with(
        CallClass::Protected(ProtectedKind::PushOrPublish),
        HoldMode::Balanced
    ));
    assert!(!with(
        CallClass::Protected(ProtectedKind::PushOrPublish),
        HoldMode::Relaxed
    ));
    assert!(!with(
        CallClass::Protected(ProtectedKind::GuardConfig),
        HoldMode::Balanced
    ));
    let mut other = denied(&input, CallClass::Ordinary);
    other.reason = DenyReason::Fence;
    assert!(!a_looser_mode_would_let_it_run(&other));
    // The agent's text names none of them.
    let text = explain(&denied(&input, CallClass::Ordinary));
    for command in ["lunchroom mode", "lunchroom yolo"] {
        assert!(!text.contains(command), "{command}: {text}");
    }
    assert!(
        text.contains("- The user: change how strict Lunchroom is for this session, in a terminal of their own."),
        "{text}"
    );
}

// G1-2/G1-8 (message side): a DENIED path way names the path the call wrote, or "that folder" for a
// search; a path of nobody's saying is "that path".
#[test]
fn a_denied_path_way_names_the_path_the_call_wrote() {
    let input = json!({ "command": "grep -r x /home/u" });
    for (fault, target) in [
        (
            Some("/home/u/.lunchroom/store/state.json"),
            "/home/u/.lunchroom/store/state.json",
        ),
        (Some("a search of /home/u"), "that folder"),
        (Some("relative.txt"), "that path"),
        (None, "that path"),
    ] {
        let mut d = denied(&input, CallClass::Denied(DenyReason::DeniedPath));
        d.reason = DenyReason::DeniedPath;
        d.fault = fault.map(str::to_owned);
        let ways = ways_forward(&d);
        assert_eq!(
            ways[0],
            format!(
                "The agent: leave {target} out of the call; read or search only folders that hold \
                 neither Lunchroom's records nor another session's transcripts."
            ),
            "{fault:?}"
        );
    }
}

// DV-18 (T1.7): a read of the transcript says whether it reached the file's end. It did not when
// the size cap stopped it before the end (a file exactly the cap long was read whole), when the id
// limit stopped it, or when the file could not be read to the end. A missing or empty file has no
// text that was left unread.
#[test]
fn a_transcript_read_says_whether_it_reached_the_end() {
    let dir = scratch("whole");
    let cap = 5000_u64;
    let at_cap = write_transcript(&dir, "at-cap.jsonl", &vec![b'x'; 5000]);
    assert!(read_transcript(&at_cap, cap, TRANSCRIPT_IDS).whole);
    let past_cap = write_transcript(&dir, "past-cap.jsonl", &vec![b'x'; 5001]);
    assert!(!read_transcript(&past_cap, cap, TRANSCRIPT_IDS).whole);
    assert!(read_transcript(&past_cap, 6000, TRANSCRIPT_IDS).whole);
    let empty = write_transcript(&dir, "empty.jsonl", b"");
    assert!(read_transcript(&empty, cap, TRANSCRIPT_IDS).whole);
    let missing = read_transcript(&dir.join("missing.jsonl"), cap, TRANSCRIPT_IDS);
    assert!(missing.whole && missing.ids.is_empty());
    // The id limit: two ids with a limit of two stop the read (the rest, if any, is unread).
    let three = write_transcript(
        &dir,
        "three.jsonl",
        b"[lr:lr-00000001] [lr:lr-00000002] [lr:lr-00000003]",
    );
    let limited = read_transcript(&three, cap, 2);
    assert_eq!(limited.ids.len(), 2);
    assert!(!limited.whole);
    let all = read_transcript(&three, cap, 3);
    assert_eq!(all.ids.len(), 3);
    assert!(
        !all.whole,
        "a read stopped by the limit is not known to be whole"
    );
    let fewer = read_transcript(&three, cap, 4);
    assert!(fewer.whole && fewer.ids.len() == 3);
}

// DV-18 (T1.7): a transcript the search could not read to the end holds the session as a tool
// output the guard did not judge does ("unjudged", source "transcript (not read to the end)"), even
// when it names no message; the user typing clears the hold; a transcript read whole that names
// none holds nothing; and the search is not repeated (it is marked done by `observe`).
#[test]
fn a_transcript_not_read_to_the_end_holds_the_session_until_the_user_types() {
    let dir = scratch("unfinished");
    let Ok(store) = Store::<Mail>::open(&dir.join("store")) else {
        unreachable!("the store could not be opened");
    };
    let me = Identity::new(Harness::ClaudeCode, "big");
    assert!(store.update(|m| m.observe(&me)).is_ok());
    let transcript = write_transcript(&dir, "big.jsonl", &vec![b'x'; 6000]);
    let Ok(input) = HookInput::parse(
        &json!({ "session_id": "big", "cwd": "/w", "transcript_path": transcript }).to_string(),
    ) else {
        unreachable!("the input did not parse");
    };
    // Read whole: nothing named, nothing held.
    assert_eq!(
        search_transcript_to(&store, &me, &input, 6000, TRANSCRIPT_IDS),
        Ok(false)
    );
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(false));
    // Cut off by the cap: HELD, by the source the spec names.
    assert_eq!(
        search_transcript_to(&store, &me, &input, 5999, TRANSCRIPT_IDS),
        Ok(true)
    );
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(true));
    let marks = store.read().map(|m| {
        m.holding(&me)
            .iter()
            .map(|h| (h.from_name.clone(), h.message.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        marks.ok(),
        Some(vec![(
            "unjudged".to_owned(),
            "unjudged:transcript (not read to the end)".to_owned()
        )])
    );
    // The user typing clears it.
    assert!(store.update(|m| m.user_typed(&me)).is_ok());
    assert_eq!(store.update(|m| m.is_held(&me)).ok(), Some(false));
}

// DV-18 (T1.7, H1.5): the guard's deadline passed while it was reading the transcript. The hook
// that answered leaves the same note a tool output it did not judge leaves, naming the transcript,
// and the next hook that can use the store holds the session by it.
#[test]
fn a_deadline_that_cut_the_transcript_read_off_holds_the_session_at_the_next_hook() {
    assert!(cut_off_in_transcript(
        "no answer within 10 seconds, while the guard was reading the session's transcript /t.jsonl"
    ));
    assert!(!cut_off_in_transcript(
        "no answer within 10 seconds, while the guard was searching below the folder /home/u"
    ));
    assert!(!cut_off_in_transcript("no answer within 10 seconds"));
    let dir = scratch("marker");
    let marker = dir.join("unjudged-x");
    assert_eq!(unjudged_source(&marker), None);
    assert!(std::fs::write(&marker, b"").is_ok());
    assert_eq!(unjudged_source(&marker).as_deref(), Some("tool output"));
    assert!(std::fs::write(&marker, UNREAD_TRANSCRIPT.as_bytes()).is_ok());
    assert_eq!(
        unjudged_source(&marker).as_deref(),
        Some("transcript (not read to the end)")
    );
}
