//! F1 acceptance criteria that name a listener or the outcome `lunchroom send` prints: an IDE
//! session keeps its listener (F1-AC7), and `send` prints FAILED when the message failed while it
//! waited (F1-AC20). F1-AC19 (DELIVERED only for a confirmed message) is asserted in full by
//! `codex_delivery_through_codex_queue` (DELIVERED once confirmed) and
//! `codex_send_reports_unconfirmed_and_failed_attempts` (never DELIVERED while unconfirmed) in
//! `guard.rs`, so it is not repeated here.

mod common;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin};
use lunchroom_core::store::Store;

use common::{GroupChild, Sandbox};

/// An attended-or-not Claude Code run by an IDE: `CLAUDE_PID` names this test process, whose
/// command line has no `-p`, and the entrypoint is not `cli`.
fn ide_env(entrypoint: &str) -> [(&'static str, String); 3] {
    [
        ("CLAUDE_PID", std::process::id().to_string()),
        ("CLAUDE_CODE_SESSION_ATTENDED", "0".to_owned()),
        ("CLAUDE_CODE_ENTRYPOINT", entrypoint.to_owned()),
    ]
}

/// `lunchroom listen` as the hook runs it, fed `event` for `session` on standard input.
fn start_listener(
    sb: &Sandbox,
    session: &str,
    event: &str,
    entrypoint: &str,
) -> Option<GroupChild> {
    let input = json!({
        "session_id": session, "cwd": sb.home().join("project"), "hook_event_name": event,
    })
    .to_string();
    let mut cmd = sb.command(env!("CARGO_BIN_EXE_lunchroom"));
    cmd.arg("listen")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in ide_env(entrypoint) {
        cmd.env(k, v);
    }
    let mut child = GroupChild::spawn(&mut cmd).ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    Some(child)
}

// F1-AC7
#[test]
fn an_ide_session_keeps_its_listener_at_a_start_and_at_a_turns_end() {
    common::declare_test_release();
    let sb = Sandbox::new("f1-tests");
    let mut listeners = Vec::new();
    for entrypoint in ["claude-vscode", "claude-desktop"] {
        for event in ["SessionStart", "Stop"] {
            let session = format!("ide-{entrypoint}-{event}");
            let started = start_listener(&sb, &session, event, entrypoint);
            assert!(
                started.is_some(),
                "{entrypoint} {event}: listen did not start"
            );
            if let Some(child) = started {
                listeners.push((format!("{entrypoint} {event}"), child));
            }
        }
    }
    // A one-prompt run's listener returns within a second or two; these must still be waiting.
    std::thread::sleep(Duration::from_secs(3));
    for (what, child) in &mut listeners {
        let state = child.try_wait();
        assert!(
            matches!(state, Ok(None)),
            "{what}: an IDE session's listener returned: {state:?}"
        );
    }
}

/// Claude Code settings holding the guard hooks `lunchroom install` writes.
fn configure_claude_guard(sb: &Sandbox) {
    let program = env!("CARGO_BIN_EXE_lunchroom");
    let hook = |mode: &str| json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}]);
    let settings = json!({"hooks": {
        "PreToolUse": hook("pretool"), "PostToolUse": hook("posttool"),
        "PostToolUseFailure": hook("posttool"), "UserPromptSubmit": hook("prompt"),
    }});
    let _ = std::fs::create_dir_all(sb.home().join(".claude"));
    let _ = std::fs::write(
        sb.home().join(".claude/settings.json"),
        settings.to_string(),
    );
}

/// Codex's guard hooks written and trusted, and a `codex queue` stand-in that runs `body`.
fn configure_codex(sb: &Sandbox, body: &str) {
    let program = env!("CARGO_BIN_EXE_lunchroom");
    let codex = sb.home().join(".codex");
    let _ = std::fs::create_dir_all(codex.join("sessions/2026/10/27"));
    let hooks = [
        ("PreToolUse", "pretool", "pre_tool_use"),
        ("PostToolUse", "posttool", "post_tool_use"),
        ("UserPromptSubmit", "prompt", "user_prompt_submit"),
        ("SessionStart", "start", "session_start"),
    ];
    let mut events = serde_json::Map::new();
    for (event, mode, _) in hooks {
        events.insert(
            event.to_owned(),
            json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}]),
        );
    }
    let file = codex.join("hooks.json");
    let _ = std::fs::write(&file, json!({ "hooks": events }).to_string());
    let mut config = "model = \"x\"\n\n[features]\nunified_exec_tty = false\n".to_owned();
    for (_, _, state) in hooks {
        let _ = write!(
            config,
            "\n[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:abc\"\n",
            file.display()
        );
    }
    let _ = std::fs::write(codex.join("config.toml"), config);
    let bin = sb.root.join("queue-bin");
    let _ = std::fs::create_dir_all(&bin);
    let queue = bin.join("codex");
    let _ = std::fs::write(&queue, format!("#!/bin/sh\n{body}\n"));
    // The hand-over runs the recorded session process (a copy of `bash`) with `queue` as its
    // script, found on the recipient's `PATH`.
    let subcommand = bin.join("queue");
    let _ = std::fs::write(
        &subcommand,
        format!("#!/bin/sh\nexec '{}' queue \"$@\"\n", queue.display()),
    );
    let _ = Command::new("chmod")
        .arg("+x")
        .arg(&queue)
        .arg(&subcommand)
        .status();
}

/// A stand-in Codex session `x1`: a copy of `bash` named `codex` that runs its guard's `PreToolUse`
/// hook, then stays alive for `lifetime` seconds.
fn codex_session(sb: &Sandbox, lifetime: u32) -> Option<GroupChild> {
    let harness = sb
        .root
        .join(".codex/packages/app-server-daemon/releases/0.163.0-x86_64-unknown-linux-musl/bin");
    let _ = std::fs::create_dir_all(&harness);
    let fake = harness.join("codex");
    if !fake.exists() {
        let _ = std::fs::copy(common::which("bash")?, &fake);
    }
    let input = json!({
        "session_id": "x1", "turn_id": "turn-1", "cwd": sb.home().join("project"),
        "hook_event_name": "PreToolUse", "permission_mode": "bypassPermissions",
        "tool_name": "Bash", "tool_input": {"command": "true"}, "tool_use_id": "c1",
        "transcript_path": sb.home().join(
            ".codex/sessions/2026/10/27/rollout-2026-10-27T00-00-00-x1.jsonl"),
    });
    let input_file = sb.root.join("codex-hook-input.json");
    let _ = std::fs::write(&input_file, input.to_string());
    let script = format!("\"$LR\" guard pretool < \"$IN\"; sleep {lifetime}; true");
    // A binary just copied can fail to start with "Text file busy" while another test thread
    // forks; such a start is tried again.
    for _ in 0..50 {
        let started = GroupChild::spawn(
            Command::new(&fake)
                .args(["-c", &script])
                .env_clear()
                .env("HOME", sb.home())
                .env("LUNCHROOM_HOME", sb.lunchroom())
                .env("CODEX_HOME", sb.home().join(".codex"))
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", sb.root.join("queue-bin").display()),
                )
                .env("LR", env!("CARGO_BIN_EXE_lunchroom"))
                .env("IN", &input_file)
                .current_dir(sb.home().join("project"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        );
        match started {
            Ok(child) => return Some(child),
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    None
}

/// Names Claude Code session `c1` `Claude` and Codex thread `x1` `Codex`.
fn name_claude_and_codex(sb: &Sandbox) {
    let named = sb.store().and_then(|s| {
        s.update(|m| {
            let claude = Identity::new(Harness::ClaudeCode, "c1");
            let codex = Identity::new(Harness::Codex, "x1");
            m.register("Claude", &claude, Origin::User, None).ok();
            m.register("Codex", &codex, Origin::User, None).ok();
        })
        .ok()
    });
    assert!(named.is_some(), "the names were not registered");
}

/// A guard hook in Claude session `c1`, so that a `send` below it is bound to that session.
fn bind_sender(sb: &Sandbox) {
    let input = json!({
        "session_id": "c1", "cwd": sb.home().join("project"), "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_input": {"command": "true"}, "permission_mode": "default",
        "tool_use_id": "t1",
    });
    let env = [
        ("CLAUDE_PID", std::process::id().to_string()),
        ("CLAUDE_CODE_SESSION_ATTENDED", "1".to_owned()),
        ("CLAUDE_CODE_ENTRYPOINT", "cli".to_owned()),
    ];
    let run = sb.run(&["guard", "pretool"], &input.to_string(), &env);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
}

/// Waits up to `limit` for `file` to exist.
fn appears(file: &Path, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if file.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    file.exists()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The state word the store gives message `id`.
fn state_of(store: &Store<Mail>, id: &str) -> Option<String> {
    let mail = store.read().ok()?;
    let word = mail.message(id)?.status.state().word();
    Some(word.to_owned())
}

// F1-AC20
#[test]
fn send_prints_failed_when_the_message_fails_while_it_waits() {
    common::declare_test_release();
    let sb = Sandbox::new("f1-tests").strict();
    configure_claude_guard(&sb);
    let handed = sb.root.join("handed.marker");
    // Codex takes the message but never shows it in its thread: `send` waits for confirmation.
    configure_codex(&sb, &format!("touch '{}'", handed.display()));
    name_claude_and_codex(&sb);
    let session = codex_session(&sb, 30);
    assert!(
        session.is_some(),
        "the stand-in Codex session did not start"
    );
    let Some(_session) = session else { return };
    std::thread::sleep(Duration::from_millis(1500));
    bind_sender(&sb);
    let path = format!("{}:/usr/bin:/bin", sb.root.join("queue-bin").display());
    let mut cmd = sb.command(env!("CARGO_BIN_EXE_lunchroom"));
    cmd.args(["send", "Codex", "Are you there?"])
        .env("PATH", path)
        .env("CODEX_HOME", sb.root.join("sender-codex"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let sender = GroupChild::spawn(&mut cmd);
    assert!(sender.is_ok(), "lunchroom send did not start");
    let Ok(sender) = sender else { return };
    // Once Codex has been handed the message, `send` is waiting. The user takes Codex's name away.
    assert!(
        appears(&handed, Duration::from_secs(5)),
        "the message was never handed to `codex queue`"
    );
    let taken = sb.store().and_then(|s| {
        s.update(|m| {
            m.unregister("Codex");
            m.fail_unnamed(unix_now())
        })
        .ok()
    });
    assert_eq!(taken.map(|ids| ids.len()), Some(1), "no message was failed");
    let out = sender.into_output();
    assert!(out.is_ok(), "lunchroom send did not end");
    let Ok(out) = out else { return };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let id = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    assert!(id.starts_with("lr-"), "no message id in: {stdout}");
    let line = stdout
        .lines()
        .find(|l| l.starts_with(&format!("{id}: ")))
        .unwrap_or_default();
    assert!(
        line.ends_with("; FAILED"),
        "send did not print FAILED: {stdout}"
    );
    assert!(!stdout.contains("DELIVERED"), "{stdout}");
    let store = sb.store();
    let state = store.as_ref().and_then(|s| state_of(s, &id));
    assert_eq!(state.as_deref(), Some("FAILED"), "{stdout}");
}
