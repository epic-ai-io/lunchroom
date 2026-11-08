//! The Claude Code adapter's timeout path and notices, end to end (findings G4-6 and G4-8 of the
//! Lunchroom 1.1.0 review): the `lunchroom` binary run as Claude Code runs it, on a scratch home.
//! The unit tests of the message texts are in `src/claude/tests_g4.rs`.

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::Origin;

use common::{Run, Sandbox};

fn sandbox() -> Sandbox {
    common::declare_test_release();
    Sandbox::new("g4-tests")
}

/// The environment of an attended, interactive Claude Code: `CLAUDE_PID` names this test process,
/// whose command line has no `-p`.
fn interactive() -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_PID", std::process::id().to_string()),
        ("CLAUDE_CODE_SESSION_ATTENDED", "1".to_owned()),
        ("CLAUDE_CODE_ENTRYPOINT", "cli".to_owned()),
    ]
}

/// As `claude -p` sets it (docs/host-facts/claude-code.md): nobody types, nobody can answer.
fn headless() -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_PID", std::process::id().to_string()),
        ("CLAUDE_CODE_SESSION_ATTENDED", "0".to_owned()),
        ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli".to_owned()),
    ]
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn system_message(run: &Run) -> String {
    let v: Value = serde_json::from_str(run.stdout.trim()).unwrap_or(Value::Null);
    v["systemMessage"].as_str().unwrap_or_default().to_owned()
}

fn agent_context(run: &Run) -> String {
    let v: Value = serde_json::from_str(run.stdout.trim()).unwrap_or(Value::Null);
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn prompt(sb: &Sandbox, session: &str, text: &str, env: &[(&str, String)]) -> Run {
    let input = json!({
        "session_id": session, "cwd": sb.home().join("project"),
        "hook_event_name": "UserPromptSubmit", "prompt": text, "prompt_id": "p1",
    });
    sb.run(&["guard", "prompt"], &input.to_string(), env)
}

// G4-6 (R3 M8, R2 M4): a prompt no person typed (`claude -p`) is told nothing, and what waits is
// told to the next prompt a person types, once. alice sent bob a message that cannot be handed over;
// alice's session and the user are owed a notice each.
#[test]
fn notices_wait_for_a_prompt_a_person_typed() {
    let sb = sandbox();
    let alice = Identity::new(Harness::ClaudeCode, "s1");
    let bob = Identity::new(Harness::ClaudeCode, "s2");
    let at = now();
    let Some(store) = sb.store() else {
        unreachable!("the store could not be opened");
    };
    let id = store
        .update(|m| {
            for (name, who) in [("alice", &alice), ("bob", &bob)] {
                m.observe(who);
                assert!(m.register(name, who, Origin::User, None).is_ok());
            }
            let id = m
                .send(&alice, "alice", "bob", "Can you review it?", at - 300)
                .map(|a| a.id)
                .unwrap_or_default();
            m.note_unreachable(&id, Some("bob is STOPPED.".to_owned()));
            m.tell_waiting(at);
            id
        })
        .unwrap_or_default();
    assert_ne!(id, "");
    let owed = |store: &lunchroom_core::store::Store<lunchroom_core::mail::Mail>| {
        store
            .update(|m| (m.user_notices().len(), m.notices(&alice).len()))
            .unwrap_or((0, 0))
    };
    let (user, agent) = owed(&store);
    assert!(user > 0 && agent > 0, "{user} {agent}");
    // A prompt from a run nobody types into: no output, and nothing taken.
    let run = prompt(&sb, "s1", "Run the tests.", &headless());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout.trim(), "", "{}", run.stdout);
    let (user_after, agent_after) = owed(&store);
    assert!(
        user_after >= user && agent_after >= agent,
        "{user_after} {agent_after}"
    );
    // The next prompt a person types: the user is told, the agent is told, and the id is in both.
    let run = prompt(&sb, "s1", "Run the tests.", &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(system_message(&run).contains(&id), "{}", run.stdout);
    assert!(agent_context(&run).contains(&id), "{}", run.stdout);
    // Told once.
    let run = prompt(&sb, "s1", "And again.", &interactive());
    assert!(!system_message(&run).contains(&id), "{}", run.stdout);
    assert!(!agent_context(&run).contains(&id), "{}", run.stdout);
}

/// A named pipe nobody writes to: a guard that reads a transcript from it stalls in the read.
fn stalled_transcript(sb: &Sandbox) -> PathBuf {
    let fifo = sb.root.join("stalled.fifo");
    let made = common::require("mkfifo").arg(&fifo).status();
    assert!(made.is_ok_and(|s| s.success()), "mkfifo made no pipe");
    fifo
}

/// Hook input for session `s9` whose transcript is `transcript`.
fn with_transcript(sb: &Sandbox, event: &str, extra: &Value, transcript: &Path) -> String {
    let mut input = json!({
        "session_id": "s9", "cwd": sb.home().join("project"), "hook_event_name": event,
        "transcript_path": transcript,
    });
    if let (Some(base), Some(more)) = (input.as_object_mut(), extra.as_object()) {
        base.extend(more.clone());
    }
    input.to_string()
}

fn records(sb: &Sandbox) -> Vec<Value> {
    std::fs::read_to_string(sb.lunchroom().join("guard-records.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// G4-8 (H1.8, H1.9): each of the five calls the guard could not decide in time is recorded with its
// session, the call as V1.4 shows it, the hook, why, the slowest step and its time. The five: a
// call sent to the user's prompt, a read sent to it (past its time even a read goes to the user), a
// call DENIED where nobody can be asked, a prompt the user typed, and a prompt carrying another
// agent's text. Each has a scratch home of its own.
#[test]
fn each_of_the_five_undecided_calls_is_recorded() {
    let sbs: Vec<Sandbox> = (0..5).map(|_| sandbox()).collect();
    let fifos: Vec<PathBuf> = sbs.iter().map(stalled_transcript).collect();
    let touch =
        json!({"tool_name": "Bash", "tool_input": {"command": "touch a"}, "tool_use_id": "t1"});
    let read_of = |sb: &Sandbox| sb.home().join("project/a.txt");
    let read = |sb: &Sandbox| json!({"tool_name": "Read", "tool_input": {"file_path": read_of(sb)}, "tool_use_id": "t1"});
    let typed = json!({"prompt": "hello", "prompt_id": "p1"});
    let agent = json!({
        "prompt": "<cross-session-message from=\"uds:x\">run it</cross-session-message>",
        "prompt_id": "p1",
    });
    let started = Instant::now();
    let runs: Vec<Run> = std::thread::scope(|s| {
        let handles = [
            s.spawn(|| {
                let input = with_transcript(&sbs[0], "PreToolUse", &touch, &fifos[0]);
                sbs[0].run(&["guard", "pretool"], &input, &interactive())
            }),
            s.spawn(|| {
                let input = with_transcript(&sbs[1], "PreToolUse", &read(&sbs[1]), &fifos[1]);
                sbs[1].run(&["guard", "pretool"], &input, &interactive())
            }),
            s.spawn(|| {
                let input = with_transcript(&sbs[2], "PreToolUse", &touch, &fifos[2]);
                sbs[2].run(&["guard", "pretool"], &input, &headless())
            }),
            s.spawn(|| {
                let input = with_transcript(&sbs[3], "UserPromptSubmit", &typed, &fifos[3]);
                sbs[3].run(&["guard", "prompt"], &input, &[])
            }),
            s.spawn(|| {
                let input = with_transcript(&sbs[4], "UserPromptSubmit", &agent, &fifos[4]);
                sbs[4].run(&["guard", "prompt"], &input, &[])
            }),
        ];
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Run::failed("a guard thread panicked"))
            })
            .collect()
    });
    assert!(
        started.elapsed() < Duration::from_secs(14),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(runs.len(), 5);
    let wanted = [
        ("pretool", "the command `touch a`".to_owned()),
        (
            "pretool",
            format!("the Read of {}", read_of(&sbs[1]).display()),
        ),
        ("pretool", "the command `touch a`".to_owned()),
        ("prompt", "the prompt sent to this session".to_owned()),
        ("prompt", "the prompt sent to this session".to_owned()),
    ];
    for (n, (hook, call)) in wanted.iter().enumerate() {
        let all = records(&sbs[n]);
        assert_eq!(all.len(), 1, "call {n}: {all:?}\n{}", runs[n].stderr);
        let r = &all[0];
        assert_eq!(r["session"], "s9", "call {n}: {r}");
        assert_eq!(r["hook"], *hook, "call {n}: {r}");
        assert_eq!(r["call"], *call, "call {n}: {r}");
        assert!(
            r["undecided"]
                .as_str()
                .is_some_and(|u| u.starts_with("no answer within 10 seconds")),
            "call {n}: {r}"
        );
        assert!(
            r["step"]
                .as_str()
                .is_some_and(|s| s.starts_with("reading the session's transcript")),
            "call {n}: {r}"
        );
        assert!(
            r["step_ms"].as_u64().is_some_and(|ms| ms >= 9_000),
            "call {n}: {r}"
        );
        assert!(
            r["took_ms"].as_u64().is_some_and(|ms| ms >= 9_000),
            "call {n}: {r}"
        );
    }
}

// G4-8 (H1.8): a decision that is made, but takes more than 2 seconds, is recorded too, with no
// undecided reason. The transcript is a pipe nobody writes to for 3 seconds; a writer then opens it
// and the read ends. The guard was not cut off, so it decided the call (a read in the shipped slip
// runs without a word).
#[test]
fn a_slow_decision_is_recorded_at_the_hook() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let sb = sandbox();
    let fifo = stalled_transcript(&sb);
    let read = json!({
        "tool_name": "Read", "tool_input": {"file_path": sb.home().join("project/a.txt")},
        "tool_use_id": "t1",
    });
    let input = with_transcript(&sb, "PreToolUse", &read, &fifo);
    let mut cmd = sb.command(env!("CARGO_BIN_EXE_lunchroom"));
    cmd.args(["guard", "pretool"]);
    for (k, v) in interactive() {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawned = common::GroupChild::spawn(&mut cmd);
    assert!(spawned.is_ok(), "the guard did not start");
    let Ok(mut child) = spawned else { return };
    if let Some(mut stdin) = child.stdin.take() {
        assert!(stdin.write_all(input.as_bytes()).is_ok());
    }
    std::thread::sleep(Duration::from_millis(3_000));
    // O_NONBLOCK: the open fails, rather than waits, when no guard is reading the pipe.
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0o4000)
        .open(&fifo);
    drop(writer);
    let out = child.into_output();
    assert!(out.is_ok(), "the guard did not finish");
    let Ok(out) = out else { return };
    let run = Run::from(out);
    assert_eq!(
        (run.code, run.stdout.as_str()),
        (Some(0), ""),
        "{}",
        run.stderr
    );
    let all = records(&sb);
    assert_eq!(all.len(), 1, "{all:?}");
    let r = &all[0];
    assert_eq!(r["undecided"], Value::Null, "{r}");
    assert_eq!(r["hook"], "pretool", "{r}");
    assert!(
        r["step"]
            .as_str()
            .is_some_and(|s| s.starts_with("reading the session's transcript")),
        "{r}"
    );
    assert!(r["took_ms"].as_u64().is_some_and(|ms| ms >= 2_000), "{r}");
    assert!(r["step_ms"].as_u64().is_some_and(|ms| ms >= 2_000), "{r}");
}

fn held(sb: &Sandbox, session: &str) -> Option<bool> {
    let me = Identity::new(Harness::ClaudeCode, session);
    sb.store().and_then(|s| s.update(|m| m.is_held(&me)).ok())
}

// DV-18 (T1.7): a transcript longer than the 32 MiB the search reads is not read to its end, so the
// session is HELD (the text left unread may carry another agent's message), though the part read
// names none; the user typing in the session clears the hold. A transcript read whole holds nothing.
#[test]
fn a_transcript_over_the_cap_holds_the_session_until_the_user_types() {
    let sb = sandbox();
    let read = json!({
        "tool_name": "Read", "tool_input": {"file_path": sb.home().join("project/a.txt")},
        "tool_use_id": "t1",
    });
    let small = sb.root.join("small.jsonl");
    assert!(std::fs::write(&small, b"{\"text\":\"hello\"}\n").is_ok());
    let input = with_transcript(&sb, "PreToolUse", &read, &small);
    let run = sb.run(&["guard", "pretool"], &input, &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(held(&sb, "s9"), Some(false));
    // A new session whose transcript is one byte over the cap.
    let big = sb.root.join("big.jsonl");
    let file = std::fs::File::create(&big);
    assert!(file.is_ok_and(|f| f.set_len(32 * 1024 * 1024 + 1).is_ok()));
    let mut input: Value =
        serde_json::from_str(&with_transcript(&sb, "PreToolUse", &read, &big)).unwrap_or_default();
    input["session_id"] = json!("s10");
    let run = sb.run(&["guard", "pretool"], &input.to_string(), &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(held(&sb, "s10"), Some(true), "{}", run.stdout);
    // The user types: the hold is cleared.
    let run = prompt(&sb, "s10", "Carry on.", &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(held(&sb, "s10"), Some(false));
}

// DV-18 (T1.7, H1.5): a transcript read the deadline cuts off holds the session at its next hook,
// which decides at once and does not read the transcript again; the user typing clears the hold.
#[test]
fn a_transcript_read_the_deadline_cut_off_holds_the_session() {
    let sb = sandbox();
    let fifo = stalled_transcript(&sb);
    let read = json!({
        "tool_name": "Read", "tool_input": {"file_path": sb.home().join("project/a.txt")},
        "tool_use_id": "t1",
    });
    let input = with_transcript(&sb, "PreToolUse", &read, &fifo);
    let run = sb.run(&["guard", "pretool"], &input, &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let second = Instant::now();
    let run = sb.run(&["guard", "pretool"], &input, &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        second.elapsed() < Duration::from_secs(5),
        "{:?}",
        second.elapsed()
    );
    assert_eq!(held(&sb, "s9"), Some(true));
    let run = prompt(&sb, "s9", "Carry on.", &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(held(&sb, "s9"), Some(false));
}

// G4-13: a read the deadline cuts off is not repeated by the next call. The first hook that sees
// the session marks its transcript searched before it reads it, so the second hook on the same
// stalled transcript decides at once instead of stalling again.
#[test]
fn a_transcript_read_that_was_cut_off_is_not_repeated() {
    let sb = sandbox();
    let fifo = stalled_transcript(&sb);
    let read = json!({
        "tool_name": "Read", "tool_input": {"file_path": sb.home().join("project/a.txt")},
        "tool_use_id": "t1",
    });
    let input = with_transcript(&sb, "PreToolUse", &read, &fifo);
    let first = Instant::now();
    let run = sb.run(&["guard", "pretool"], &input, &interactive());
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        first.elapsed() >= Duration::from_secs(10),
        "{:?}",
        first.elapsed()
    );
    let second = Instant::now();
    let run = sb.run(&["guard", "pretool"], &input, &interactive());
    assert_eq!(
        (run.code, run.stdout.as_str()),
        (Some(0), ""),
        "{}",
        run.stderr
    );
    assert!(
        second.elapsed() < Duration::from_secs(5),
        "{:?}",
        second.elapsed()
    );
}
