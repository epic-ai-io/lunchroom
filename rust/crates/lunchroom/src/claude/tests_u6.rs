//! Lunchroom 1.1, unit U6: the retry of a call the user declined (§17.10) when the guard could not
//! decide the call. The guard's decided path is tested through the binary (`tests/guard.rs`).

use super::*;

fn env_in(name: &str, now: u64, fresh: bool) -> Env {
    let dir = crate::test_target()
        .join("claude-u6")
        .join(format!("{}-{name}", std::process::id()));
    if fresh {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(std::fs::create_dir_all(&dir).is_ok());
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

fn call(session: &str, command: &str) -> String {
    json!({
        "session_id": session, "cwd": "/work/a", "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_input": {"command": command}, "tool_use_id": "t1",
    })
    .to_string()
}

// §17::188: a call the user declined stays DENIED with that reason when the guard could
// not decide it on the retry; it is not kept for approval again; another call and another session's
// same call are not DENIED so; it does not lapse by time.
#[test]
fn an_undecided_retry_of_a_declined_call_is_denied_with_the_reason_that_the_user_declined_it() {
    let env = env_in("undecided", 1000, true);
    let Ok(store) = store(&env) else {
        assert!(store(&env).is_ok());
        return;
    };
    let me = Identity::new(Harness::ClaudeCode, "s1");
    let request = Request {
        session: me,
        agent: "bob".to_owned(),
        tool: "Bash".to_owned(),
        input: json!({ "command": "git push" }),
        cwd: "/work/a".to_owned(),
        rule: String::new(),
        at: 1000,
    };
    let waiting = request.clone();
    let declined = store.update(|m| {
        m.request(waiting);
        m.refuse_request(0, &request, 1001).ok()
    });
    assert!(declined.is_ok_and(|id| id.is_some()));
    let text = declined_retry_in(&call("s1", "git push"), &env);
    assert!(
        text.as_deref().is_some_and(
            |t| t.contains("the user declined this exact call") && t.contains("DENIED")
        ),
        "{text:?}"
    );
    assert_eq!(
        declined_retry_in(&call("s1", "git push --force"), &env),
        None
    );
    assert_eq!(declined_retry_in(&call("s2", "git push"), &env), None);
    assert_eq!(declined_retry_in("not a hook input", &env), None);
    // It does not lapse by time: a day later it still stands.
    let later = env_in("undecided", 1001 + 86_400, false);
    assert!(declined_retry_in(&call("s1", "git push"), &later).is_some());
}

// §17::189 (Claude Code): the retry's reason names who decided, what stays DENIED and for
// how long, and says that running it again changes nothing; it states no rule the call broke.
#[test]
fn the_declined_retry_reason_says_who_declined_what_and_for_how_long() {
    let text = refused_retry_text(1090, 1000);
    assert!(
        text.starts_with("Lunchroom: DENIED · declined by the user:"),
        "{text}"
    );
    assert!(
        text.contains("the user declined this exact call 1 min ago"),
        "{text}"
    );
    assert!(
        text.contains(
            "stays DENIED until the user approves it or the message that held this session"
        ),
        "{text}"
    );
    assert!(!text.contains(" min from then"), "{text}");
    assert!(text.contains("Running it again changes nothing."), "{text}");
    // A stamp ahead of the clock reads as no time passed, never as a wrapped number.
    assert!(refused_retry_text(10, 1000).contains("0 s ago"));
}

/// A store in a folder of its own in which the call `input` of Claude Code session `s1`, run in
/// `cwd`, was waiting and has been declined at 1001.
fn declined_in(name: &str, cwd: &str, input: &Value) -> Env {
    let env = env_in(name, 1002, true);
    let store = store(&env);
    assert!(store.is_ok());
    if let Ok(store) = store {
        let request = Request {
            session: Identity::new(Harness::ClaudeCode, "s1"),
            agent: "bob".to_owned(),
            tool: "Bash".to_owned(),
            input: input.clone(),
            cwd: cwd.to_owned(),
            rule: String::new(),
            at: 1000,
        };
        let declined = store.update(|m| {
            m.request(request.clone());
            m.refuse_request(0, &request, 1001).ok()
        });
        assert!(declined.is_ok_and(|id| id.is_some()));
    }
    env
}

fn call_in(session: &str, cwd: &str, tool_input: &Value) -> String {
    json!({
        "session_id": session, "cwd": cwd, "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_input": tool_input, "tool_use_id": "t1",
    })
    .to_string()
}

// U6-062: a call that carries no input is not the call whose input was null, and the other way
// round; neither is a call with an empty input.
#[test]
fn missing_input_is_distinct_from_null() {
    let env = declined_in("null-input", "/work/a", &Value::Null);
    assert!(declined_retry_in(&call_in("s1", "/work/a", &Value::Null), &env).is_some());
    let missing = json!({
        "session_id": "s1", "cwd": "/work/a", "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_use_id": "t1",
    })
    .to_string();
    assert_eq!(declined_retry_in(&missing, &env), None);
    assert_eq!(
        declined_retry_in(&call_in("s1", "/work/a", &json!({})), &env),
        None
    );
    let env = declined_in("empty-input", "/work/a", &json!({}));
    assert!(declined_retry_in(&call_in("s1", "/work/a", &json!({})), &env).is_some());
    assert_eq!(
        declined_retry_in(&call_in("s1", "/work/a", &Value::Null), &env),
        None
    );
    assert_eq!(declined_retry_in(&missing, &env), None);
}

// U6-065: a retry that names no session, or one that is empty, is no one's call.
#[test]
fn missing_runtime_identity_fails_closed() {
    let env = declined_in("no-session", "/work/a", &json!({"command": "ls"}));
    let nameless = json!({
        "cwd": "/work/a", "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_input": {"command": "ls"},
    })
    .to_string();
    assert_eq!(declined_retry_in(&nameless, &env), None);
    assert_eq!(
        declined_retry_in(&call_in("", "/work/a", &json!({"command": "ls"})), &env),
        None
    );
    assert!(
        declined_retry_in(&call_in("s1", "/work/a", &json!({"command": "ls"})), &env).is_some()
    );
}

// U6-066: an input that cannot be read finds no decline: the guard's own refusal of such input
// stands.
#[test]
fn unparseable_retry_cannot_bypass_refusal() {
    let env = declined_in("unparseable", "/work/a", &json!({"command": "ls"}));
    for raw in [
        "",
        "{",
        "null",
        "[]",
        "\"x\"",
        "{\"tool_input\": {\"command\": \"ls\"}}",
    ] {
        assert_eq!(declined_retry_in(raw, &env), None, "{raw:?}");
    }
}

// U6-016, U6-087: a folder named another way is the same folder: `..`, `.`, doubled and trailing
// slashes make no other call; a different folder is another call; a path of 5,000 bytes and one of
// 500 parts is read without a panic.
#[test]
fn cwd_normalization_is_consistent() {
    let env = declined_in("normal-cwd", "/work/a", &json!({"command": "ls"}));
    for cwd in [
        "/work/a",
        "/work/b/../a",
        "/work/./a/",
        "//work//a",
        "/../work/a",
    ] {
        assert!(
            declined_retry_in(&call_in("s1", cwd, &json!({"command": "ls"})), &env).is_some(),
            "{cwd}"
        );
    }
    for cwd in ["/work", "/work/a/b", "/work/a/../b", "work/a", "/Work/a"] {
        assert_eq!(
            declined_retry_in(&call_in("s1", cwd, &json!({"command": "ls"})), &env),
            None,
            "{cwd}"
        );
    }
}

#[test]
fn path_resolution_identity_is_safe() {
    let long = format!("/{}", "d/".repeat(2500));
    let env = declined_in("long-cwd", &long, &json!({"command": "ls"}));
    assert!(declined_retry_in(&call_in("s1", &long, &json!({"command": "ls"})), &env).is_some());
    assert_eq!(
        declined_retry_in(
            &call_in("s1", &format!("{long}x"), &json!({"command": "ls"})),
            &env
        ),
        None
    );
    let deep = format!("/w{}", "/p".repeat(500));
    let env = declined_in(
        "deep-cwd",
        &format!("{deep}/../../q"),
        &json!({"command": "ls"}),
    );
    let same = format!("/w{}/q", "/p".repeat(498));
    assert!(declined_retry_in(&call_in("s1", &same, &json!({"command": "ls"})), &env).is_some());
    assert_eq!(
        declined_retry_in(
            &call_in(
                "s1",
                &format!("/w{}/q", "/p".repeat(499)),
                &json!({"command": "ls"})
            ),
            &env
        ),
        None
    );
}

// U6-088: the folder is taken as the hook names it, with no link followed: a link to a folder and the
// folder are two spellings of two calls, in both directions, and a `..` after a link is taken
// against the link's name (never against where it points).
#[test]
fn symlinked_cwd_identity_is_tested() {
    let dir = crate::test_target()
        .join("claude-u6-links")
        .join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(std::fs::create_dir_all(dir.join("real/inner")).is_ok());
    assert!(std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).is_ok());
    let (real, link) = (
        dir.join("real").display().to_string(),
        dir.join("link").display().to_string(),
    );
    let input = json!({"command": "ls"});
    let env = declined_in("link-real", &real, &input);
    assert!(declined_retry_in(&call_in("s1", &real, &input), &env).is_some());
    assert_eq!(declined_retry_in(&call_in("s1", &link, &input), &env), None);
    let env = declined_in("link-link", &link, &input);
    assert!(declined_retry_in(&call_in("s1", &link, &input), &env).is_some());
    assert_eq!(declined_retry_in(&call_in("s1", &real, &input), &env), None);
    assert!(declined_retry_in(&call_in("s1", &format!("{link}/inner/.."), &input), &env).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

// U6-089: what a command's text says it will do to its folder (a `cd`, a subshell, an expansion)
// has no part in the lookup: the folder is the hook's, and the command is one string.
#[test]
fn runtime_shell_text_cannot_change_refusal_lookup() {
    for command in [
        "cd /tmp && git push",
        "(cd /x; ls)",
        "$(pwd)/run",
        "cd \"$HOME\"; make",
        "`cd ..`",
    ] {
        let input = json!({"command": command});
        let env = declined_in("shell-text", "/work/a", &input);
        assert!(
            declined_retry_in(&call_in("s1", "/work/a", &input), &env).is_some(),
            "{command}"
        );
        assert_eq!(
            declined_retry_in(&call_in("s1", "/tmp", &input), &env),
            None,
            "{command}"
        );
        assert_eq!(
            declined_retry_in(&call_in("s1", "/work", &input), &env),
            None,
            "{command}"
        );
    }
}
