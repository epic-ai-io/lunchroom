//! Tests for the F1 acceptance criteria of `lunchroom doctor` and the undecided-call messages
//! (F1-AC22, F1-AC24, F1-AC25, F1-AC30, F1-AC57). Criteria already asserted in full elsewhere are
//! not repeated here:
//! - F1-AC12 (doctor's UNGUARDED line): `doctor_shows_each_running_agent_with_its_reason`.
//! - F1-AC29 (release from the program on PATH, plugin report time, Hermes FAIL until a session
//!   ran): `doctor_reads_openclaws_release_from_the_program_on_path`,
//!   `doctor_reads_hermes_release_from_its_latest_session`,
//!   `doctor_says_when_a_plugin_reported_its_release`.
//! - F1-AC27 (an agent's `lunchroom doctor` / `lunchroom check` is DENIED): the doctor terminal
//!   check in `doctor_checks_where_it_runs_before_it_reads_anything`, and the guard's rule in
//!   `lunchroom-core` (`classify/tests.rs`) and `tests/guard.rs`.

use super::tests::Setup;
use super::*;

use lunchroom_core::mail::Origin;
use std::os::unix::fs::PermissionsExt as _;

/// The FAIL lines of a doctor's output.
fn fail_lines(out: &Outcome) -> Vec<&str> {
    out.stdout
        .lines()
        .filter(|l| l.starts_with("FAIL "))
        .collect()
}

/// A system that is asked nothing: `lunchroom yolo` with the wrong words refuses before it asks.
struct Quiet;

impl System for Quiet {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        Vec::new()
    }
    fn own_tty(&self) -> u64 {
        0
    }
    fn tty_of(&self, _key: &str) -> Option<u64> {
        None
    }
    fn ask(&mut self, _question: &str) -> Option<String> {
        None
    }
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn verifier(&self) -> Result<String, String> {
        Err("no passphrase in this test".to_owned())
    }
    fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
        None
    }
    fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
        Err("no salt in this test".to_owned())
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Err("nothing is installed in this test".to_owned())
    }
}

/// A Hermes agent called `name` for `session`, registered by the user; when `running`, its
/// session is recorded in this very process, as a guard hook would record it.
fn hermes_agent(s: &Setup, name: &str, session: &str, running: bool) {
    let store = Store::<Mail>::open(&s.env.store_dir());
    assert!(store.is_ok(), "the store opens");
    let Ok(store) = store else { return };
    let who = Identity::new(Harness::Hermes, session);
    let process = crate::process::key(std::process::id()).unwrap_or_default();
    let named = store.update(|m| {
        if running {
            m.observe_process(&process, &s.root, &who);
        }
        m.register(name, &who, Origin::User, None).is_ok()
    });
    assert_eq!(named.ok(), Some(true), "{name} is registered");
}

/// A record of the guard's, with the call and the step a test names.
fn guard_record(
    at: u64,
    session: &str,
    call: &str,
    undecided: Option<&str>,
) -> crate::records::Record {
    crate::records::Record {
        at,
        session: session.to_owned(),
        hook: "pretool".to_owned(),
        call: call.to_owned(),
        step: "searching below the folder /home/u".to_owned(),
        step_ms: 9_812,
        took_ms: 10_004,
        undecided: undecided.map(str::to_owned),
    }
}

// F1-AC25: the FAIL for a call the guard could not decide names the agent by the name the user
// gave it and its harness (not the session id), and the time the line clears, 24 hours after the
// call. A record from a session no name labels says so instead. Remove the lookup of the agent
// or the clearing time and this fails.
#[test]
fn doctor_names_the_agent_and_when_the_failure_clears() {
    let mut s = Setup::new("f1-clears");
    let now = 1_793_000_000;
    s.env.now = now;
    hermes_agent(&s, "Hera", "h-session", false);
    let cause =
        "no answer within 10 seconds, while the guard was searching below the folder /home/u";
    let home = s.env.lunchroom_home.clone();
    // Five hours old: it clears 19 hours from now, at 02:33 UTC on the next day.
    let named = guard_record(
        now - 5 * 3600,
        "h-session",
        "the command `git status`",
        Some(cause),
    );
    let other = guard_record(now - 600, "x-session", "the command `ls`", Some(cause));
    assert_eq!(crate::records::add(&home, &named), Ok(()));
    assert_eq!(crate::records::add(&home, &other), Ok(()));
    let out = s.doctor();
    assert_eq!(out.code, 1, "{}", out.stdout);
    let lines = fail_lines(&out);
    assert_eq!(lines.len(), 2, "{}", out.stdout);
    let hera = lines
        .iter()
        .find(|l| l.contains("the command `git status`"))
        .copied()
        .unwrap_or_default();
    assert!(
        hera.contains(" in the session of Hera (Hermes): "),
        "{}",
        out.stdout
    );
    assert!(
        hera.contains("This line clears at 02:33 UTC on 2026-10-27, 24 hours after the call."),
        "{}",
        out.stdout
    );
    assert!(!hera.contains("h-session"), "{}", out.stdout);
    let nameless = lines
        .iter()
        .find(|l| l.contains("the command `ls`"))
        .copied()
        .unwrap_or_default();
    assert!(
        nameless.contains(" in a session with no agent name: "),
        "{}",
        out.stdout
    );
    // The ten minute old call clears 23 hours 50 minutes from now: 07:23 UTC on 2026-10-27.
    assert!(
        nameless.contains("This line clears at 07:23 UTC on 2026-10-27, 24 hours after the call."),
        "{}",
        out.stdout
    );
}

// F1-AC24: what an agent chose for a call, and what a command held in it, reaches the user's
// screen through doctor only with control characters removed and secrets hidden, and the file the
// records are kept in is the owner's alone. Doctor's FAIL stays one line however many line breaks
// the call held.
#[test]
fn doctor_prints_records_without_control_characters_or_secrets() {
    let mut s = Setup::new("f1-records-clean");
    let now = 1_793_000_000;
    s.env.now = now;
    let home = s.env.lunchroom_home.clone();
    let token = format!("ghp_{}", "b".repeat(36));
    let call = format!("the command `curl\u{1b}[2J -H {token}\nwipe\r\u{202e}`");
    let record = guard_record(now - 60, "s1", &call, Some("no answer within 10 seconds"));
    assert_eq!(crate::records::add(&home, &record), Ok(()));
    let out = s.doctor();
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        !out.stdout
            .chars()
            .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}'),
        "{:?}",
        out.stdout
    );
    assert!(!out.stdout.contains("ghp_"), "{}", out.stdout);
    let lines = fail_lines(&out);
    assert_eq!(lines.len(), 1, "{}", out.stdout);
    assert!(
        lines[0].contains("the command `curl[2J -H [secret removed] wipe `"),
        "{}",
        out.stdout
    );
    let mode =
        std::fs::metadata(crate::records::file(&home)).map(|m| m.permissions().mode() & 0o777);
    assert_eq!(mode.ok(), Some(0o600));
}

// F1-AC22: a call a Codex agent makes that the guard cannot decide, where nobody can be asked, is
// DENIED with the cause in plain words, the call, the step still running when the time ran out,
// and the ways forward: Codex is told to try the call once more and then to tell the user.
#[test]
fn a_codex_call_that_could_not_be_decided_says_cause_call_step_and_ways_forward() {
    let raw = serde_json::json!({
        "session_id": "t1", "cwd": "/w", "turn_id": "turn-1", "tool_name": "Bash",
        "tool_input": { "command": "touch a" },
    })
    .to_string();
    let reason = "no answer within 10 seconds, while the guard was searching below the folder /w";
    let crate::claude::HookOut::Block(text) =
        crate::claude::failed(Some("pretool"), reason, &raw, false, None)
    else {
        unreachable!("a Codex call nobody can be asked about is DENIED");
    };
    for part in [
        // The cause, in plain words.
        "Lunchroom could not check the command `touch a`: Lunchroom did not finish within 10 seconds",
        // The call.
        "What could not be checked: the command `touch a`.",
        // The step still running when the time ran out.
        "Lunchroom did not finish within 10 seconds; it was still searching below the folder /w",
        // The ways forward, the agent's first: try once more, then tell the user.
        "Ways forward:\n",
        "- The agent: try the call once more. If Lunchroom still cannot check it, stop and tell the \
         user what the call was for.",
        "- The user: run it in a terminal of their own, outside every agent session.",
    ] {
        assert!(text.contains(part), "{part}\n{text}");
    }
    // Once more, and then the user: not an invitation to retry without end.
    assert!(!text.contains("keep trying"), "{text}");
}

// F1-AC57: a Hermes agent whose guard runs is a PASS line; the moment another agent's text is in
// its session, doctor shows it as HELD, neither PASS nor FAIL, says what holds it, and exits 0
// when nothing else fails. Without the HELD branch the agent would print as a PASS line.
#[test]
fn doctor_shows_a_held_agent_as_held() {
    let mut s = Setup::new("f1-held");
    s.env.now = 1_793_038_003 + 3 * 3600 + 5 * 60;
    s.add_hermes();
    hermes_agent(&s, "Hera", "h1", true);
    s.hermes_report("h1", "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003");
    let before = s.doctor();
    assert_eq!(before.code, 0, "{}", before.stdout);
    assert!(
        before.stdout.contains("PASS agent Hera (Hermes) is "),
        "the agent is running and guarded before it is held:\n{}",
        before.stdout
    );
    let held = Store::<Mail>::open(&s.env.store_dir()).and_then(|store| {
        store.update(|m| {
            m.hold_unrecorded(&Identity::new(Harness::Hermes, "h1"), "tool-output", "1");
        })
    });
    assert!(held.is_ok(), "{held:?}");
    let out = s.doctor();
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stdout.contains(
            "HELD agent Hera (Hermes): HELD, because text from tool output is in its session. \
             HELD means that text from another agent is in a session, so Lunchroom restricts \
             what the agent may do until the user types in the session."
        ),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("PASS agent Hera"), "{}", out.stdout);
    assert!(!out.stdout.contains("FAIL agent Hera"), "{}", out.stdout);
}

// F1-AC30: the README gives `lunchroom yolo` the one usage that the help text uses, word for word
// (the help text itself is asserted in main.rs by `yolo_is_written_one_way_in_the_help`).
#[test]
fn the_readme_writes_yolo_the_way_the_help_does() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    let readme = std::fs::read_to_string(&path);
    assert!(readme.is_ok(), "{}: {readme:?}", path.display());
    let readme = readme.unwrap_or_default();
    assert!(
        readme.contains(&format!("`{}`", crate::YOLO_USAGE)),
        "README.md does not give the usage `{}`; its yolo lines: {:?}",
        crate::YOLO_USAGE,
        readme
            .lines()
            .filter(|l| l.contains("lunchroom yolo"))
            .collect::<Vec<_>>()
    );
}

// F1-AC30: the message for a wrong use of `lunchroom yolo` (no end time, a word other than `till`,
// nothing at all) gives the same usage as the help text, and exits 2 without asking the user.
#[test]
fn a_wrong_use_of_yolo_gives_the_usage_the_help_gives() {
    let s = Setup::new("f1-yolo");
    let usage = crate::YOLO_USAGE.trim_start_matches("lunchroom ");
    assert!(
        crate::USAGE.contains(usage),
        "the help text lacks the usage"
    );
    for args in [
        Vec::<&str>::new(),
        vec!["all"],
        vec!["all", "until", "14:00"],
        vec!["Hera", "till"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let out = crate::user::yolo(&args, &s.env, &mut Quiet);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(out.stdout.is_empty(), "{args:?}: {}", out.stdout);
        assert!(
            out.stderr
                .starts_with(&format!("usage: {}; ", crate::YOLO_USAGE)),
            "{args:?}: {}",
            out.stderr
        );
    }
}

// F1-AC47: a send whose hand-over could not use Lunchroom's store tells the sender what happened
// (the hand-over was not completed and the message is still QUEUED), why (the store's error), and
// what to do, in that order.
#[test]
fn f1_ac47_a_store_lost_during_the_hand_over_says_what_why_and_what_to_do() {
    let why = unread("the disk is full");
    let report = hand_over_report("lr-00000007", &Err(why.clone()));
    let what = "lr-00000007: the hand-over could not be completed";
    let next = "Do not send it again; `lunchroom status` shows where it stands. If the store still \
                cannot be used, tell the user.";
    let at = |part: &str| report.find(part);
    assert!(at(what).is_some(), "{report}");
    assert!(at(&why).is_some(), "{report}");
    assert!(at("it is still QUEUED").is_some(), "{report}");
    assert!(at(next).is_some(), "{report}");
    assert!(at(what) < at(&why) && at(&why) < at(next), "{report}");
    assert!(
        report.ends_with('\n') && report.lines().count() == 1,
        "{report}"
    );
}
