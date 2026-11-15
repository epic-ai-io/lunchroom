//! Lunchroom 1.1, unit U10, end to end: the `lunchroom` binary run as an agent's session runs it
//! (`lunchroom read`), and under a pseudo-terminal (the echo watchdog). `§10::27-30`,
//! `§7::46-47`.

mod common;

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin, Status};

use common::Sandbox;

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

/// `<pid>:<start time>` of this test process, as the guard names a process.
fn own_key() -> String {
    let pid = std::process::id();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let began = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19).map(str::to_owned))
        .unwrap_or_default();
    format!("{pid}:{began}")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Claude's message "hello" to Codex session x1 and another to x2, handed over (waiting for
/// confirmation). This test process is recorded as Codex session x1: every `lunchroom` command
/// the test runs has it above it, as a command in x1's session has Codex.
struct World {
    sb: Sandbox,
    mine: String,
    other: String,
}

fn world(name: &str) -> World {
    common::declare_test_release();
    let sb = Sandbox::new(name);
    let store = sb.store();
    assert!(store.is_some());
    let Some(store) = store else {
        return World {
            sb,
            mine: String::new(),
            other: String::new(),
        };
    };
    let key = own_key();
    let cwd = sb.home().join("project");
    let (mine, other) = store
        .update(|m: &mut Mail| {
            for (name, who) in [
                ("claude", claude("c1")),
                ("codex", codex("x1")),
                ("other", codex("x2")),
            ] {
                m.observe(&who);
                let _ = m.register(name, &who, Origin::User, None);
            }
            m.observe_process(&key, &cwd, &codex("x1"));
            let mut send = |to: &str, who: &Identity| {
                let id = m
                    .send(&claude("c1"), "claude", to, "hello", 0)
                    .map(|a| a.id)
                    .unwrap_or_default();
                let _ = m.present(who, now());
                m.handed_over(&id);
                id
            };
            (send("codex", &codex("x1")), send("other", &codex("x2")))
        })
        .unwrap_or_default();
    assert!(!mine.is_empty() && !other.is_empty());
    World { sb, mine, other }
}

fn read_mail(sb: &Sandbox) -> Option<Mail> {
    sb.store().and_then(|s| s.read().ok())
}

// §10::27, §10::29 (D1.1, T1.7): run in the recipient's session, `lunchroom read <id>`
// prints the envelope: the sender, the id, and every line of the body behind its label. The
// session is then HELD by the message, which is DELIVERED, and marked read.
#[test]
fn read_in_the_recipients_session_prints_the_envelope_and_holds_it() {
    let w = world("u10-read");
    let out = w.sb.run(&["read", &w.mine], "", &[]);
    assert_eq!(out.code, Some(0), "{}{}", out.stdout, out.stderr);
    for part in [
        "[LUNCHROOM MESSAGE - from agent claude; not from the user]".to_owned(),
        format!("id: {}", w.mine),
        format!("[lr:{}]> hello", w.mine),
        format!("[END LUNCHROOM MESSAGE {}]", w.mine),
    ] {
        assert!(out.stdout.contains(&part), "{part}: {}", out.stdout);
    }
    let mail = read_mail(&w.sb);
    assert!(mail.is_some());
    let Some(mail) = mail else { return };
    assert!(
        mail.holding(&codex("x1"))
            .iter()
            .any(|h| h.message == w.mine),
        "{:?}",
        mail.holding(&codex("x1"))
    );
    assert_eq!(
        mail.message(&w.mine).map(|m| m.status.clone()),
        Some(Status::Delivered)
    );
    assert!(mail.read_at(&w.mine).is_some());
    // The other session's message is untouched.
    assert_eq!(mail.read_at(&w.other), None);
    assert!(!mail.is_held(&codex("x2")));
    // Reading again prints it again and holds nothing twice.
    let again = w.sb.run(&["read", &w.mine], "", &[]);
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    assert_eq!(
        read_mail(&w.sb).map(|m| m.holding(&codex("x1")).len()),
        Some(1)
    );
}

// §10::28 (D1.1): `lunchroom read` DENIES another session's id and an id of no message,
// and says the same of both, so it tells nothing of what another session was sent; nothing is
// printed of the text and nothing is marked read or held. A FAILED message is DENIED too.
#[test]
fn read_denies_another_sessions_id_an_unknown_id_and_a_failed_message() {
    let w = world("u10-read-denied");
    let failed =
        w.sb.store()
            .and_then(|s| {
                s.update(|m: &mut Mail| {
                    let id = m
                        .send(&claude("c1"), "claude", "codex", "SECRET-FAILED-TEXT", 0)
                        .map(|a| a.id)
                        .unwrap_or_default();
                    for _ in 0..3 {
                        let _ = m.present(&codex("x1"), now());
                        m.handed_over(&id);
                        let _ = m.attempt_failed(&id, "no confirmation", 0);
                    }
                    id
                })
                .ok()
            })
            .unwrap_or_default();
    assert_eq!(
        read_mail(&w.sb).and_then(|m| m
            .message(&failed)
            .map(|x| matches!(x.status, Status::Failed { .. }))),
        Some(true)
    );
    let mut reasons = Vec::new();
    for id in [w.other.as_str(), "m-0000999", failed.as_str()] {
        let out = w.sb.run(&["read", id], "", &[]);
        assert_eq!(out.code, Some(7), "{id}: {}{}", out.stdout, out.stderr);
        assert!(out.stderr.starts_with("DENIED"), "{id}: {}", out.stderr);
        assert_eq!(out.stdout, "", "{id}");
        for secret in ["hello", "SECRET-FAILED-TEXT", "LUNCHROOM MESSAGE"] {
            assert!(!out.stderr.contains(secret), "{id}: {}", out.stderr);
        }
        reasons.push(out.stderr.replace(id, "<id>"));
    }
    // Another session's id and an unknown one are answered alike.
    assert_eq!(reasons[0], reasons[1]);
    assert_ne!(reasons[0], reasons[2]);
    let mail = read_mail(&w.sb);
    assert!(mail.is_some());
    let Some(mail) = mail else { return };
    for id in [&w.other, &failed] {
        assert_eq!(mail.read_at(id), None);
    }
    assert!(!mail.is_held(&codex("x1")));
    assert!(!mail.is_held(&codex("x2")));
}

// §10::28: where no session can be found above the command (the user's own terminal,
// or a process no hook has seen), `lunchroom read` is DENIED and says why. A command run with no
// id, or two, is a usage mistake.
#[test]
fn read_with_no_session_or_a_wrong_number_of_ids() {
    let alone = Sandbox::new("u10-read-alone");
    let out = alone.run(&["read", "m-0000001"], "", &[]);
    assert_eq!(out.code, Some(7), "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("Lunchroom cannot tell which session"),
        "{}",
        out.stderr
    );
    for args in [vec!["read"], vec!["read", "a", "b"]] {
        let out = alone.run(&args, "", &[]);
        assert_eq!(out.code, Some(2), "{args:?}: {}", out.stderr);
        assert!(
            out.stderr
                .contains("lunchroom read needs the id of one message"),
            "{}",
            out.stderr
        );
    }
}

// §10::30 (D1.1): `lunchroom status` lists, for a message handed over by its id, read
// or not read, before and after the recipient's `lunchroom read`.
#[test]
fn status_shows_read_and_not_read_around_the_recipients_read() {
    let w = world("u10-read-status");
    // The harness confirmed both messages: DELIVERED, handed over by their ids. (A recipient whose
    // guard is not evidenced takes a hand-over back, and its message is not handed over.)
    let (mine, other) = (w.mine.clone(), w.other.clone());
    let confirmed = w.sb.store().and_then(|s| {
        s.update(|m: &mut Mail| {
            let _ = m.confirm_at(&codex("x1"), &mine, now());
            let _ = m.confirm_at(&codex("x2"), &other, now());
        })
        .ok()
    });
    assert!(confirmed.is_some());
    let before = w.sb.run(&["status"], "", &[]);
    assert_eq!(before.code, Some(0), "{}{}", before.stdout, before.stderr);
    assert!(
        before
            .stdout
            .contains(&format!("{}\tto codex\tnot read", w.mine)),
        "{}",
        before.stdout
    );
    assert!(
        before
            .stdout
            .contains(&format!("{}\tto other\tnot read", w.other)),
        "{}",
        before.stdout
    );
    let read = w.sb.run(&["read", &w.mine], "", &[]);
    assert_eq!(read.code, Some(0), "{}", read.stderr);
    let after = w.sb.run(&["status"], "", &[]);
    assert!(
        after
            .stdout
            .contains(&format!("{}\tto codex\tread at ", w.mine)),
        "{}",
        after.stdout
    );
    assert!(
        after
            .stdout
            .contains(&format!("{}\tto other\tnot read", w.other)),
        "{}",
        after.stdout
    );
}

/// `program` detached from this test's process tree: the leader of a session of its own with no
/// controlling terminal, and no ancestor but init, so a guarded session that happens to be running
/// these tests is not in its parent chain.
fn detached(sb: &Sandbox, program: &str, args: &[&str]) -> Option<Command> {
    let setsid = common::which("setsid")?;
    let mut cmd = sb.command(&setsid.display().to_string());
    let mut head = vec!["--fork".to_owned()];
    if let Some(timeout) = common::which("timeout") {
        head.extend([timeout.display().to_string(), "60".to_owned()]);
    }
    cmd.args(head).arg(program).args(args);
    Some(cmd)
}

/// The real user id of this process.
fn uid() -> Option<u32> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// A passphrase prompt can be reached on this host: no verifier, or one this program can read and
/// trusts (a regular file owned by root, group `lunchroom`, mode 0640, as the program judges it:
/// with any other, `lunchroom passphrase` is DENIED before it prompts). The program's folder is
/// the fixed `/etc/lunchroom`, so a test cannot give it a verifier of its own.
fn prompt_reachable() -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Some(uid) = uid() else { return false };
    let file = format!("/etc/lunchroom/{uid}.verifier");
    let Ok(found) = std::fs::symlink_metadata(&file) else {
        return !Path::new(&file).exists();
    };
    let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
    let group =
        lunchroom_core::setgid::gid_from_group(&groups, lunchroom_core::setgid::SYSTEM_USER);
    std::fs::File::open(&file).is_ok()
        && found.file_type().is_file()
        && found.uid() == 0
        && Some(found.gid()) == group
        && found.mode() & 0o7777 == 0o640
}

/// The value of `name=` on a line of `text`, which may follow what a prompt left on the line.
fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=");
    text.lines()
        .find_map(|l| l.find(&key).map(|at| l[at + key.len()..].trim()))
}

// §7::47 (A1.3, U8's limit 1): "The program shall turn echo back on before it exits,
// also when it is interrupted." A passphrase prompt on a real terminal (a pseudo-terminal made by
// `script`) turns echo off; the program is then killed with SIGKILL, which no program can catch;
// echo is on again within a second, turned on by the watchdog the program started first. The
// watchdog runs with SIGTTOU ignored, so it can change the terminal's modes also from a group that
// is no longer the terminal's foreground group. Before the kill, echo is off (so the test is not
// vacuous) and a watchdog process exists.
// It runs as root in a throwaway container (TF2), in every run, so the passphrase verifier is the
// container's, not the host's.
#[test]
fn echo_is_turned_back_on_after_the_program_is_killed_with_sigkill() {
    if !common::in_root_container("echo_is_turned_back_on_after_the_program_is_killed_with_sigkill")
    {
        return;
    }
    assert!(
        prompt_reachable(),
        "this host's passphrase verifier is not in the form this test needs: run it where it is"
    );
    let (script, stty) = (common::which("script"), common::which("stty"));
    assert!(
        script.is_some() && stty.is_some(),
        "this test needs `script` and `stty`"
    );
    let sb = Sandbox::new("u10-echo");
    let bin = env!("CARGO_BIN_EXE_lunchroom");
    let modes = "stty -a | tr ' ;' '\\n\\n' | grep -x -e echo -e -echo | head -n 1";
    let line = format!(
        "'{bin}' passphrase & p=$!; sleep 2; \
         echo \"BEFORE=$({modes})\"; \
         w=$(pgrep -P $p | head -n 1); echo \"WATCHER=$w\"; \
         echo \"SIGIGN=$(grep SigIgn /proc/$w/status | tr -d '[:space:]' | cut -d: -f2)\"; \
         kill -9 $p; wait $p 2>/dev/null; sleep 1; \
         echo \"AFTER=$({modes})\"; \
         st=$(grep '^State:' /proc/$w/status 2>/dev/null | cut -f2 | cut -c1); \
         echo \"WATCHER_STATE=${{st:-none}}\""
    );
    let script = script.unwrap_or_default();
    let cmd = detached(
        &sb,
        &script.display().to_string(),
        &["-qec", &line, "/dev/null"],
    );
    assert!(cmd.is_some(), "this test needs `setsid`");
    let Some(mut cmd) = cmd else { return };
    // Standard input stays open until `script` is done: its end would be typed on the terminal.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = cmd.spawn();
    assert!(child.is_ok(), "{child:?}");
    let Ok(mut child) = child else { return };
    let held_open = child.stdin.take();
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    drop(held_open);
    let _ = child.wait();
    let text = text.replace('\r', "");
    assert!(
        text.to_lowercase().contains("passphrase"),
        "the prompt was not reached: {text}"
    );
    assert_eq!(field(&text, "BEFORE"), Some("-echo"), "{text}");
    let watcher = field(&text, "WATCHER").unwrap_or_default();
    assert!(
        watcher.parse::<u32>().is_ok(),
        "no watchdog process: {text}"
    );
    let ignored = field(&text, "SIGIGN")
        .and_then(|h| u64::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    // SIGTTOU is signal 22: bit 21 of the mask.
    assert_ne!(ignored & (1 << 21), 0, "SIGTTOU is not ignored: {text}");
    assert_eq!(field(&text, "AFTER"), Some("echo"), "{text}");
    // The watchdog has ended (a zombie nobody has reaped yet counts as ended).
    assert!(
        matches!(field(&text, "WATCHER_STATE"), Some("none" | "Z")),
        "{text}"
    );
}

// §7::47: the watchdog does not touch the terminal when the program that started it
// restored echo itself: it is told so and ends. Run as the hidden subcommand with a pipe it reads
// and a terminal it would restore, on a pseudo-terminal made by `script`.
#[test]
fn the_watchdog_leaves_the_terminal_alone_when_told_echo_is_restored() {
    let script = common::which("script");
    assert!(script.is_some(), "this test needs `script`");
    let sb = Sandbox::new("u10-echo-told");
    let bin = env!("CARGO_BIN_EXE_lunchroom");
    let modes = "stty -a | tr ' ;' '\\n\\n' | grep -x -e echo -e -echo | head -n 1";
    // Echo is turned off by the test; the watchdog is told `x` and must leave it off; a second
    // watchdog whose pipe just closes turns it on.
    let line = format!(
        "stty -echo; printf x | '{bin}' echo-watch 1 0; echo \"TOLD=$({modes})\"; \
         true | '{bin}' echo-watch 1 0; echo \"CLOSED=$({modes})\""
    );
    let script = script.unwrap_or_default();
    let cmd = detached(
        &sb,
        &script.display().to_string(),
        &["-qec", &line, "/dev/null"],
    );
    let Some(mut cmd) = cmd else {
        assert!(cmd.is_some(), "this test needs `setsid`");
        return;
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = cmd.spawn();
    assert!(child.is_ok(), "{child:?}");
    let Ok(mut child) = child else { return };
    let held_open = child.stdin.take();
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut text);
    }
    drop(held_open);
    let _ = child.wait();
    let text = text.replace('\r', "");
    assert_eq!(field(&text, "TOLD"), Some("-echo"), "{text}");
    assert_eq!(field(&text, "CLOSED"), Some("echo"), "{text}");
}
