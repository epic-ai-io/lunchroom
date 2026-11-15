//! N4 (`docs/spec/next/N4-local-only`): Lunchroom's program, and every process it starts, makes no
//! network system call while it runs a command or a hook. Each run is traced with
//! `strace -f -e trace=%network`; a control that does connect proves the trace sees such calls.

mod common;

use std::path::Path;
use std::process::Stdio;

use common::Sandbox;

/// A Claude Code hook input for session `s1` in the sandbox's project folder.
fn hook_input(sb: &Sandbox, event: &str) -> String {
    let cwd = sb.root.join("home/project");
    match event {
        "PreToolUse" | "PostToolUse" => serde_json::json!({
            "session_id": "s1", "cwd": cwd, "hook_event_name": event, "tool_name": "Bash",
            "tool_input": {"command": "ls"}, "tool_use_id": "t1", "permission_mode": "default",
            "tool_response": {"stdout": "", "stderr": ""},
        }),
        "UserPromptSubmit" => serde_json::json!({
            "session_id": "s1", "cwd": cwd, "hook_event_name": event, "prompt": "hello",
        }),
        _ => serde_json::json!({
            "session_id": "s1", "cwd": cwd, "hook_event_name": event, "source": "startup",
        }),
    }
    .to_string()
}

/// Runs `program args` under the trace with `stdin` and no terminal; returns the trace's lines,
/// or why the trace could not be made.
fn traced(
    sb: &Sandbox,
    strace: &Path,
    name: &str,
    program: &str,
    args: &[&str],
    stdin: &str,
) -> Result<Vec<String>, String> {
    let out = sb.root.join(format!("net-{name}.txt"));
    let mut cmd = sb.command(&strace.display().to_string());
    // `signal=none`: strace also prints each signal (`timeout` reaping the program); only system
    // calls are counted.
    cmd.args([
        "-f",
        "-qq",
        "-e",
        "trace=%network",
        "-e",
        "signal=none",
        "-o",
    ])
    .arg(&out)
    .args(["timeout", "20", program])
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("strace could not be run: {e}"))?;
    let mut child = child;
    if let Some(mut input) = child.stdin.take() {
        use std::io::Write as _;
        let _ = input.write_all(stdin.as_bytes());
    }
    let done = child
        .wait_with_output()
        .map_err(|e| format!("strace did not finish: {e}"))?;
    let err = String::from_utf8_lossy(&done.stderr);
    // strace exits with the traced program's status, which Lunchroom's refusals make non-zero by
    // design, so the status cannot tell a failed trace. strace's own errors (not permitted, a bad
    // option, a program it cannot start) are each a line that begins with its own name, as it was
    // run (`/usr/bin/strace: invalid signal 'x'`): any one fails.
    if let Some(line) = err.lines().find(|l| {
        l.split_once(": ")
            .is_some_and(|(who, _)| who == "strace" || who.ends_with("/strace"))
    }) {
        return Err(format!("the trace of {name} could not be made: {line}"));
    }
    let text = std::fs::read_to_string(&out)
        .map_err(|e| format!("no trace was written for {name}: {e}; strace said: {err}"))?;
    Ok(text.lines().map(str::to_owned).collect())
}

#[test]
fn no_command_or_hook_makes_a_network_call() {
    let strace = common::which("strace");
    assert!(strace.is_some(), "this test needs strace on PATH (N4-AC2)");
    let strace = strace.unwrap_or_default();
    let bash = common::which("bash");
    assert!(
        bash.is_some(),
        "this test needs bash on PATH for its control"
    );
    let bash = bash.unwrap_or_default().display().to_string();
    let sb = Sandbox::new("network-tests");

    // The control: a run that connects must be seen, or the trace sees nothing.
    let control = traced(
        &sb,
        &strace,
        "control",
        &bash,
        &["-c", "exec 3<>/dev/tcp/127.0.0.1/9"],
        "",
    );
    assert!(control.is_ok(), "{control:?}");
    let control = control.unwrap_or_default();
    // The control's own connection, not any call: in this empty environment `bash` also looks the
    // user up through nscd (an AF_UNIX connect), which must not stand in for it.
    assert!(
        control
            .iter()
            .any(|l| l.contains("connect(") && l.contains("htons(9)") && l.contains("127.0.0.1")),
        "the trace did not record the control's connection to 127.0.0.1 port 9: it sees nothing\n{}",
        control.join("\n")
    );

    let program = env!("CARGO_BIN_EXE_lunchroom");
    let runs: &[(&str, &[&str], Option<&str>)] = &[
        ("version", &["version"], None),
        ("help", &["help"], None),
        ("status", &["status"], None),
        ("slip", &["slip"], None),
        ("codex-threads", &["codex-threads"], None),
        ("doctor", &["doctor"], None),
        ("send", &["send", "other", "hello"], None),
        ("read", &["read", "lr-00000001"], None),
        ("approve", &["approve"], None),
        ("approve-1", &["approve", "1"], None),
        ("register", &["register", "other", "claude-code:s9"], None),
        ("unregister", &["unregister", "other"], None),
        ("mode", &["mode", "strict"], None),
        ("rate", &["rate", "10", "60"], None),
        ("yolo", &["yolo", "all", "till", "23:59"], None),
        ("passphrase", &["passphrase"], None),
        ("repair", &["repair"], None),
        ("clear", &["clear", "/tmp/x"], None),
        ("install", &["install", "claude-code"], None),
        ("uninstall", &["uninstall", "claude-code"], None),
        ("console", &["console"], None),
        ("listen-once", &["listen", "--once"], None),
        ("guard-start", &["guard", "start"], Some("SessionStart")),
        (
            "guard-prompt",
            &["guard", "prompt"],
            Some("UserPromptSubmit"),
        ),
        ("guard-pretool", &["guard", "pretool"], Some("PreToolUse")),
        (
            "guard-posttool",
            &["guard", "posttool"],
            Some("PostToolUse"),
        ),
    ];
    let mut found = Vec::new();
    for (name, args, event) in runs {
        let stdin = event.map(|e| hook_input(&sb, e)).unwrap_or_default();
        let lines = traced(&sb, &strace, name, program, args, &stdin);
        assert!(lines.is_ok(), "{name}: {lines:?}");
        let lines = lines.unwrap_or_default();
        if !lines.is_empty() {
            found.push(format!("{name}:\n  {}", lines.join("\n  ")));
        }
    }
    assert!(
        found.is_empty(),
        "network system calls were made:\n{}",
        found.join("\n")
    );
}
