//! The install, evidence, doctor and entry-point fixes of Lunchroom 1.0.2, through the real
//! `lunchroom` program: what it prints for a refused `send`, for `help`, and for a `doctor` that
//! is not run by the user at a terminal. The rules behind them are unit-tested in the program's
//! own modules (`evidence.rs`, `cli.rs`, `main.rs`).

mod common;

use std::path::Path;
use std::process::Command;

use common::Run;

/// `program` detached from this test's process tree (`setsid --fork`: a session of its own, no
/// controlling terminal, no ancestor but init), so that a guarded session that happens to run
/// these tests is not in its parent chain. See `tests/terminal.rs`.
fn detached(sb: &common::Sandbox, program: &str, args: &[&str]) -> Command {
    let setsid = common::which("setsid");
    assert!(setsid.is_some(), "this test needs `setsid` on PATH");
    let mut cmd = sb.command(&setsid.unwrap_or_default().display().to_string());
    let mut head = vec!["--fork".to_owned()];
    if let Some(timeout) = common::which("timeout") {
        head.extend([timeout.display().to_string(), "60".to_owned()]);
    }
    cmd.args(head).arg(program).args(args);
    cmd
}

fn source(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
        .unwrap_or_default()
}

// G3-10: `lunchroom doctor` started where the user's commands do not run (here with no
// controlling terminal) is DENIED before it reads anything, with the reason and what to do, in
// full. Today's code ran doctor for anyone.
#[test]
fn doctor_without_a_controlling_terminal_is_denied_in_full() {
    let sb = common::Sandbox::new("g3-doctor");
    let run = common::finish(
        detached(&sb, env!("CARGO_BIN_EXE_lunchroom"), &["doctor"]),
        "",
    );
    assert_eq!(
        run.stderr,
        "DENIED · lunchroom doctor was not run, because this command has no controlling terminal \
         to read the user's answer from. Doctor shows where every agent's settings and programs \
         are, so it runs only for the user: open a terminal of your own, outside every agent \
         session, and run lunchroom doctor there.\n"
    );
    assert!(run.stdout.is_empty(), "{}", run.stdout);
}

// G3-16: a message with no text is DENIED with what, why and what to do, in full, and exit code 2.
#[test]
fn an_empty_message_is_denied_in_full() {
    let sb = common::Sandbox::new("g3-empty");
    let run = sb.run(&["send", "alice", ""], "", &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "DENIED · the message has no text\n\
         What was DENIED: the message to alice.\n\
         Why: a message needs text for alice to read, and this one is empty or only spaces.\n\
         Ways forward:\n\
         - The agent: put the text after the name, for example `lunchroom send alice \"the build \
         passed\"`; a text of `-` is read from standard input.\n"
    );
}

// G3-16: a text over the limit, read from standard input, is DENIED in full without reading the
// rest of the stream.
#[test]
fn a_message_over_the_limit_is_denied_in_full() {
    let sb = common::Sandbox::new("g3-long");
    let run = sb.run(&["send", "alice", "-"], &"a".repeat(65_537), &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "DENIED · the message is longer than 65536 bytes\n\
         What was DENIED: the message to alice.\n\
         Why: a message may be at most 65536 bytes long.\n\
         Ways forward:\n\
         - The agent: send a shorter message, and send what remains as further messages.\n"
    );
}

// G3-16: text on standard input that is not UTF-8 is DENIED in full.
#[test]
fn text_that_is_not_utf8_is_denied_in_full() {
    let sb = common::Sandbox::new("g3-utf8");
    let run = sb.run_bytes(&["send", "alice", "-"], &[0xff, 0xfe, 0xfd], &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "DENIED · the message text cannot be read\n\
         What was DENIED: the message to alice.\n\
         Why: the text given on standard input is not valid UTF-8 text, or could not be read.\n\
         Ways forward:\n\
         - The agent: give the text as the command's argument, or pipe UTF-8 text into \
         `lunchroom send alice -`.\n"
    );
}

// G3-16: a store that cannot be opened is DENIED with what, why (the system's own reason) and what
// to do, for `send` and for `status`; exit code 1.
#[test]
fn a_store_that_cannot_be_opened_is_denied_in_full() {
    let sb = common::Sandbox::new("g3-store");
    let file = sb.root.join("not-a-folder");
    assert!(std::fs::write(&file, "x").is_ok());
    let home = file.join("lunchroom").display().to_string();
    let env = [("LUNCHROOM_HOME", home)];
    for (args, what) in [
        (vec!["send", "alice", "hello"], "the message to alice"),
        (vec!["status"], "lunchroom status"),
    ] {
        let run: Run = sb.run(&args, "", &env);
        assert_eq!(run.code, Some(1), "{args:?}: {}", run.stderr);
        // The system's reason is on the third line, after "cannot be opened: ".
        let reason = run
            .stderr
            .lines()
            .nth(2)
            .and_then(|l| {
                l.strip_prefix(
                    "Why: Lunchroom keeps its messages and state in a store, and it cannot be \
                     opened: ",
                )
            })
            .and_then(|l| l.strip_suffix('.'))
            .unwrap_or_default();
        assert!(!reason.is_empty(), "{args:?}: {}", run.stderr);
        assert_eq!(
            run.stderr,
            format!(
                "DENIED · Lunchroom's store cannot be opened\n\
                 What was DENIED: {what}.\n\
                 Why: Lunchroom keeps its messages and state in a store, and it cannot be \
                 opened: {reason}.\n\
                 Ways forward:\n\
                 - The agent: wait a moment and try once more; if it fails again, tell the \
                 user.\n\
                 - The user: run `lunchroom status` in a terminal of their own to see whether \
                 the store can be read, and check that the disk has space; `lunchroom repair` \
                 puts back the last good store when it cannot be read.\n"
            ),
            "{args:?}"
        );
    }
}

// G3-14, G3-18, G3-9: the help text writes `yolo` one way (commas stated, who it works for),
// lists the commands an agent may run, and says what HELD and UNGUARDED mean.
#[test]
fn the_help_text_is_plain_and_complete() {
    let sb = common::Sandbox::new("g3-help");
    let run = sb.run(&["help"], "", &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    for part in [
        "Agents may run:\n  send <name> [--reply-to <id>] <text>",
        "  codex-threads [-n COUNT]                recent Codex threads, to name one with `register`\n  help, version\n",
        "  yolo <name,name,...|all> till <HH:MM>   let those agents act without asking until then (UTC);\n",
        "separate names with commas; it works for Claude Code\n",
        "HELD: a message from another agent is in a session, so Lunchroom restricts what that agent may do.\n",
        // R3.1 (2026-11-03): UNGUARDED is a lack of evidence, not a guard that stopped.
        "UNGUARDED: an agent is running, but Lunchroom has no evidence that its guard runs, so it delivers nothing to it.\n",
    ] {
        assert!(run.stdout.contains(part), "{part}\n{}", run.stdout);
    }
    assert!(
        !run.stdout.contains("name those that can"),
        "{}",
        run.stdout
    );
}

// G3-12: the guard starts the check of the QUEUED messages after its answer is written, never
// before the decision. The source: `run_hook` (parse, decide) does not start it; `guard` does,
// after writing the answer, and only when the guard was not past its deadline. Today's code
// started it first in `run_hook`, on every hook, with a second lock and a full parse.
#[test]
fn the_check_of_queued_messages_starts_after_the_answer_is_written() {
    let main = source("main.rs");
    let run_hook = main
        .split("fn run_hook(")
        .nth(1)
        .and_then(|rest| {
            rest.split("\n/// What the hook's worker thread reports")
                .next()
        })
        .unwrap_or_default();
    assert!(!run_hook.is_empty(), "run_hook not found");
    assert!(!run_hook.contains("checks::start"), "{run_hook}");
    let guard = main
        .split("fn guard(")
        .nth(1)
        .and_then(|rest| rest.split("\nconst USAGE").next())
        .unwrap_or_default();
    let written = guard.find("write(stdout.as_deref(), stderr.as_deref());");
    let started = guard.find("checks::start(&env);");
    assert!(written.is_some() && started.is_some(), "{guard}");
    assert!(
        written < started,
        "the check starts before the answer is written"
    );
}

// G3-13: after the guard met an error, the call is judged again for at most two seconds, on a
// thread of its own, with the reason that stopped the first pass. The source: the call goes
// through `judged_again`, which times the second pass and passes the reason on.
#[test]
fn a_call_judged_again_after_an_error_has_a_deadline_and_the_reason() {
    let main = source("main.rs");
    assert!(
        main.contains("const FAILED_CALL_DEADLINE: Duration = Duration::from_secs(2);"),
        "no two-second deadline"
    );
    assert!(
        main.contains("rx.recv_timeout(FAILED_CALL_DEADLINE)"),
        "the second pass is not timed"
    );
    assert!(
        main.contains("claude::failed_call(&text, &reason)"),
        "the reason is not passed on"
    );
    assert!(
        !main.contains(".then(|| claude::failed_call(&text))"),
        "the call is still judged again with no deadline and no reason"
    );
}
