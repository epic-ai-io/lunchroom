//! Lunchroom 1.1, unit U6: `lunchroom approve` tells the agent (A1.2) and the user's refusal tells
//! it (§17.10), through the channel of each harness, with the call's text on no command line.
//! Each test is derived from the acceptance criterion of the node it names and from the failure
//! classes of the clause: each harness, a hand-over program that is missing, fails, or is another
//! program's, an approval twice, a refusal and then a retry.

// A test of a list that is empty says so by `is_empty`, as the clause does.
#![allow(clippy::assert_is_empty)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use lunchroom_core::approvals::Request;
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::Mail;
use lunchroom_core::passphrase;
use lunchroom_core::store::Store;

use super::*;
use crate::terminal::{System, fake};

/// The user in a terminal of their own: a yes is the right passphrase.
struct User;

impl System for User {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        vec![(Some("100:5".to_owned()), "bash".to_owned())]
    }
    fn own_tty(&self) -> u64 {
        34816
    }
    fn tty_of(&self, _key: &str) -> Option<u64> {
        None
    }
    fn ask(&mut self, _question: &str) -> Option<String> {
        Some("yes\n".to_owned())
    }
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn verifier(&self) -> Result<String, String> {
        Ok(fake::verifier())
    }
    fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
        Some(fake::typed("yes"))
    }
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        Ok([3; passphrase::SALT_BYTES])
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Ok(())
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A scratch Lunchroom folder under the build folder, with the recipient processes it started.
struct World {
    env: Env,
    root: PathBuf,
    children: Vec<Child>,
}

impl World {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = crate::test_target()
            .join("user-u6")
            .join(format!("{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(std::fs::create_dir_all(root.join("home")).is_ok());
        Self {
            env: Env {
                home: root.join("home"),
                lunchroom_home: root.join("lunchroom"),
                codex_home: root.join("home/.codex"),
                claude_home: root.join("home/.claude"),
                hermes_home: root.join("home/.hermes"),
                claude_pid: None,
                attended: None,
                entrypoint: None,
                claude_session: None,
                codex_thread: None,
                hermes_session: None,
                lunchroom_session: None,
                now: 1000,
            },
            root,
            children: Vec::new(),
        }
    }

    fn store(&self) -> Store<Mail> {
        let store = Store::<Mail>::open(&self.env.store_dir());
        assert!(store.is_ok());
        match store {
            Ok(store) => store,
            Err(_) => unreachable!("the store opened at the assertion above"),
        }
    }

    /// Starts a recipient: a copy of `bash` named `program`, which stays alive, and a sub-program
    /// `subcommand` (`queue` for Codex, `agent` for OpenClaw) that the hand-over's arguments make it
    /// run and that records the command line it was started with in `log`, then exits `code`. The
    /// session `who` is recorded as running in that process. Returns the log.
    fn recipient(&mut self, who: &Identity, program: &str, subcommand: &str, code: i32) -> PathBuf {
        let log = self
            .root
            .join(format!("{}-cmdline.txt", who.session.replace(':', "_")));
        let body = format!(
            "tr '\\0' '|' < /proc/$$/cmdline > '{}'\nexit {code}\n",
            log.display()
        );
        self.recipient_script(who, program, subcommand, &body);
        log
    }

    /// As [`World::recipient`], with the sub-program's own `body` (shell lines). Returns the folder
    /// the sub-program is in, which is first on the recipient's `PATH`.
    fn recipient_script(
        &mut self,
        who: &Identity,
        program: &str,
        subcommand: &str,
        body: &str,
    ) -> PathBuf {
        let dir = self
            .root
            .join(format!("bin-{}", who.session.replace(':', "_")));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let log = dir.clone();
        let script = dir.join(subcommand);
        let body = format!("#!/bin/sh\n{body}");
        assert!(std::fs::write(&script, body).is_ok());
        assert!(std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).is_ok());
        let fake_program = dir.join(program);
        let bash = ["/usr/bin/bash", "/bin/bash"]
            .iter()
            .map(Path::new)
            .find(|p| p.is_file());
        assert!(bash.is_some());
        assert!(std::fs::copy(bash.unwrap_or(Path::new("/bin/bash")), &fake_program).is_ok());
        assert!(
            std::fs::set_permissions(&fake_program, std::fs::Permissions::from_mode(0o755)).is_ok()
        );
        // A binary just copied can fail to start with "Text file busy".
        let mut child = None;
        for _ in 0..50 {
            match Command::new(&fake_program)
                .args(["-c", "sleep 120; true"])
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
                .env("HOME", &self.env.home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        assert!(child.is_some());
        let Some(child) = child else { return log };
        let key = crate::process::key(child.id());
        assert!(key.is_some());
        let key = key.unwrap_or_default();
        self.children.push(child);
        let who = who.clone();
        let cwd = self.root.clone();
        assert!(
            self.store()
                .update(|m| m.observe_process(&key, &cwd, &who))
                .is_ok()
        );
        log
    }

    /// The call `command` of `who` waits for the user.
    fn wait(&self, who: &Identity, command: &str) -> Request {
        let request = Request {
            session: who.clone(),
            agent: "Bob".to_owned(),
            tool: "Bash".to_owned(),
            input: json!({ "command": command }),
            cwd: "/work/a".to_owned(),
            rule: "PROTECTED (push or publish)".to_owned(),
            at: 1000,
        };
        let kept = request.clone();
        assert!(self.store().update(|m| m.request(kept)).is_ok());
        request
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What the first waiting call's approval prints, for the user who approves it.
fn approve_first(w: &World) -> Outcome {
    approve(&["1".to_owned()], &w.env, &mut User)
}

/// The id the approval's output ends with (`Granted. <card>\n<id>\n`).
fn id_of(out: &Outcome) -> String {
    out.stdout.lines().nth(1).unwrap_or_default().to_owned()
}

const CALL: &str = "git push ZEBRA-CALL-TEXT-91";

// §7::21, §7::23, §7::25 (Codex): after `lunchroom approve` the Codex session is told
// through its own hand-over program with one line that names only the approval's id: the program's
// command line holds the id and `lunchroom read <id>`, and no byte of the call.
#[test]
fn a_codex_agent_is_told_of_an_approval_by_id_only() {
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let log = w.recipient(&me, "codex", "queue", 0);
    let _ = w.wait(&me, CALL);
    let out = approve_first(&w);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stdout.contains("not told"), "{}", out.stdout);
    let id = id_of(&out);
    assert!(id.starts_with("ap-"), "{id}");
    let cmdline = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        cmdline.contains("|queue|--thread|x1|--message|"),
        "{cmdline}"
    );
    assert!(cmdline.contains(&format!("id {id}")), "{cmdline}");
    assert!(cmdline.contains(&format!(" read {id}`")), "{cmdline}");
    for call_text in [
        "ZEBRA-CALL-TEXT-91",
        "git push",
        "Bash",
        "/work/a",
        "command",
    ] {
        assert!(!cmdline.contains(call_text), "{call_text}: {cmdline}");
    }
    // The line `lunchroom read <id>` prints: the approval and the exact call, for this session only.
    let mail = w.store().read().unwrap_or_default();
    let line = mail.tell_line_for(&me, &id).unwrap_or_default();
    assert!(line.contains("ZEBRA-CALL-TEXT-91"), "{line}");
    assert!(line.contains(&format!("({id})")), "{line}");
    assert_eq!(
        mail.tell_line_for(&Identity::new(Harness::Codex, "x2"), &id),
        None
    );
}

// §7::22, §7::23, §7::25 (OpenClaw): through the hand-over of D1.1 (`openclaw agent`),
// one line naming only the id.
#[test]
fn an_openclaw_agent_is_told_of_an_approval_by_id_only() {
    let mut w = World::new();
    let me = Identity::new(Harness::OpenClaw, "agent:main:k1");
    let log = w.recipient(&me, "openclaw", "agent", 0);
    let _ = w.wait(&me, CALL);
    let out = approve_first(&w);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!out.stdout.contains("not told"), "{}", out.stdout);
    let id = id_of(&out);
    let cmdline = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        cmdline.contains("|agent|--session-key|agent:main:k1|--message|"),
        "{cmdline}"
    );
    assert!(cmdline.contains(&format!("id {id}")), "{cmdline}");
    assert!(cmdline.contains(&format!(" read {id}`")), "{cmdline}");
    for call_text in ["ZEBRA-CALL-TEXT-91", "git push", "/work/a", "command"] {
        assert!(!cmdline.contains(call_text), "{call_text}: {cmdline}");
    }
    let mail = w.store().read().unwrap_or_default();
    assert!(
        mail.tell_line_for(&me, &id)
            .is_some_and(|l| l.contains("ZEBRA-CALL-TEXT-91"))
    );
}

// §7::20 (Hermes), §7::25: no process is started for a Hermes session: the line waits in
// the store for the plugin's `pre_llm_call` (the listener returns it at the start of its next turn).
// §7::26, §7::27: for each harness the line changes no hold. Claude Code has no channel in
// A1.2, so nothing is started for it either.
#[test]
fn hermes_and_claude_code_start_no_process_and_no_harness_is_given_a_hold() {
    for harness in [Harness::Hermes, Harness::ClaudeCode] {
        let w = World::new();
        let me = Identity::new(harness, "s1");
        let _ = w.wait(&me, CALL);
        let held = |w: &World| w.store().read().map_or(99, |m| m.holding(&me).len());
        let before = held(&w);
        let out = approve_first(&w);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(!out.stdout.contains("not told"), "{}", out.stdout);
        let mail = w.store().read().unwrap_or_default();
        assert_eq!(mail.has_pending_tell(&me), harness == Harness::Hermes);
        assert_eq!(held(&w), before);
    }
}

// §7::21 (failure classes): the hand-over program fails, is missing or is not the
// recipient's: the approval stands (the call runs if the agent tries it again) and the user is told
// that the session was not told, with the reason.
#[test]
fn a_session_that_cannot_be_told_is_reported_and_the_approval_stands() {
    // The program exits with an error.
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let _ = w.recipient(&me, "codex", "queue", 1);
    let _ = w.wait(&me, CALL);
    let out = approve_first(&w);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout
            .contains("The session was not told: codex queue exited 1."),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("The call runs if the agent tries it again"),
        "{}",
        out.stdout
    );
    let used = w
        .store()
        .update(|m| m.use_approval(&me, "Bash", &json!({ "command": CALL }), "/work/a", 1001));
    assert_eq!(used.ok(), Some(lunchroom_core::vocab::Verdict::Granted));
    // No process is recorded for the session: the program cannot be established.
    let w = World::new();
    let lost = Identity::new(Harness::Codex, "x9");
    let _ = w.wait(&lost, CALL);
    let out = approve_first(&w);
    assert!(
        out.stdout
            .contains("The session was not told: the session is not recorded."),
        "{}",
        out.stdout
    );
    // A process that is gone: the recipient's process has ended.
    let mut w = World::new();
    let gone = Identity::new(Harness::OpenClaw, "agent:main:k2");
    let _ = w.recipient(&gone, "openclaw", "agent", 0);
    for child in &mut w.children {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = w.wait(&gone, CALL);
    let out = approve_first(&w);
    assert!(
        out.stdout
            .contains("The session was not told: the recipient's process has ended."),
        "{}",
        out.stdout
    );
}

// §7::21 (an approval twice): the second `lunchroom approve` of the same waiting call
// finds nothing waiting, approves nothing and tells nothing; the first approval's line still reads.
#[test]
fn an_approval_twice_tells_once() {
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let log = w.recipient(&me, "codex", "queue", 0);
    let _ = w.wait(&me, CALL);
    let first = approve_first(&w);
    let id = id_of(&first);
    assert!(std::fs::remove_file(&log).is_ok());
    let second = approve_first(&w);
    assert_eq!(second.code, 2, "{}", second.stderr);
    assert!(
        second
            .stderr
            .contains("no call with that number is waiting"),
        "{}",
        second.stderr
    );
    assert!(!log.exists(), "the second approval told the session again");
    let mail = w.store().read().unwrap_or_default();
    assert!(mail.tell_line_for(&me, &id).is_some());
    assert_eq!(
        w.store()
            .update(|m| m.use_approval(&me, "Bash", &json!({ "command": CALL }), "/work/a", 1001))
            .ok(),
        Some(lunchroom_core::vocab::Verdict::Granted)
    );
}

// §17::189 (Codex, OpenClaw): the user's refusal tells the agent through the channel A1.2
// names for an approval of its harness, one line naming only the id; the call's text is on no
// command line; the line read by that session names the refusal and the exact call.
#[test]
fn a_refusal_tells_codex_and_openclaw_by_id_only() {
    for (who, program, subcommand) in [
        (Identity::new(Harness::Codex, "x1"), "codex", "queue"),
        (
            Identity::new(Harness::OpenClaw, "agent:main:k1"),
            "openclaw",
            "agent",
        ),
    ] {
        let mut w = World::new();
        let log = w.recipient(&who, program, subcommand, 0);
        let request = w.wait(&who, CALL);
        let store = w.store();
        let id = refuse(&w.env, &store, &request);
        assert!(id.is_ok(), "{id:?}");
        let id = id.unwrap_or_default();
        assert!(id.starts_with("rf-"), "{id}");
        let cmdline = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(cmdline.contains(&format!("id {id}")), "{cmdline}");
        assert!(cmdline.contains(&format!(" read {id}`")), "{cmdline}");
        assert!(cmdline.contains("declined"), "{cmdline}");
        for call_text in ["ZEBRA-CALL-TEXT-91", "git push", "/work/a", "command"] {
            assert!(!cmdline.contains(call_text), "{call_text}: {cmdline}");
        }
        let mail = store.read().unwrap_or_default();
        let line = mail.tell_line_for(&who, &id).unwrap_or_default();
        assert!(
            line.contains("the user declined one call of yours"),
            "{line}"
        );
        assert!(line.contains("ZEBRA-CALL-TEXT-91"), "{line}");
        // The call is DENIED from now on: the guard reads the refusal (the guard's own test).
        assert!(
            mail.refused_at(&who, "Bash", &json!({ "command": CALL }), "/work/a")
                .is_some()
        );
        assert!(mail.requests().is_empty());
        assert!(!mail.is_held(&who));
    }
}

// §17::189 (Hermes, Claude Code): a Hermes agent is told at its next turn (the line
// waits for the plugin); Claude Code has no channel in A1.2, so nothing is sent and the retry's
// reason tells it (the guard's test).
#[test]
fn a_refusal_waits_for_hermes_and_sends_nothing_to_claude_code() {
    for harness in [Harness::Hermes, Harness::ClaudeCode] {
        let w = World::new();
        let me = Identity::new(harness, "s1");
        let request = w.wait(&me, CALL);
        let store = w.store();
        let id = refuse(&w.env, &store, &request);
        assert!(id.is_ok(), "{id:?}");
        let mail = store.read().unwrap_or_default();
        assert_eq!(mail.has_pending_tell(&me), harness == Harness::Hermes);
    }
}

// §17::187 (refuse twice, a call that is gone): a refusal of a call that is no longer
// waiting refuses nothing, tells nothing and says so.
#[test]
fn a_refusal_of_a_call_that_is_not_waiting_refuses_nothing_and_tells_nothing() {
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let log = w.recipient(&me, "codex", "queue", 0);
    let request = w.wait(&me, CALL);
    let store = w.store();
    assert!(refuse(&w.env, &store, &request).is_ok());
    assert!(std::fs::remove_file(&log).is_ok());
    let again = refuse(&w.env, &store, &request);
    assert_eq!(again, Err("that call is no longer waiting".to_owned()));
    assert!(!log.exists());
    // A call that never waited.
    let never = Request {
        input: json!({ "command": "ls" }),
        ..request
    };
    assert_eq!(
        refuse(&w.env, &store, &never),
        Err("that call is no longer waiting".to_owned())
    );
}

// §17::189 (the program fails): the refusal stands when the session could not be told,
// and the user is told why.
#[test]
fn a_refusal_stands_when_the_session_cannot_be_told() {
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let _ = w.recipient(&me, "codex", "queue", 1);
    let request = w.wait(&me, CALL);
    let store = w.store();
    let told = refuse(&w.env, &store, &request);
    assert!(
        told.as_ref()
            .err()
            .is_some_and(|e| e.contains("declined; the session was not told: codex queue exited 1")),
        "{told:?}"
    );
    let mail = store.read().unwrap_or_default();
    assert!(
        mail.refused_at(&me, "Bash", &json!({ "command": CALL }), "/work/a")
            .is_some()
    );
}

// ---------------------------------------------------------------------------------------------
// Codex's failure classes (units/U6/FAILURES-CODEX.md, ids U6-nnn) for telling an agent of a decision
// ---------------------------------------------------------------------------------------------

/// What an agent may put in a call: every one of these is on no command line.
const HOSTILE: [&str; 14] = [
    "git push 'a b' \"c\" ; rm -rf / | tee x > y < z",
    "echo `id` $(id) ${HOME} *.rs ?? [a-z]",
    "line one\nline two\r\nline three\u{2028}four\u{2029}five",
    "nul\u{0}-like \\u0000 \\x00 %00",
    "classified-value-0123456789abcdef follows-here ABC=xyz",
    "/home/u/.ssh/id_rsa /etc/shadow ../../x ~/.aws/credentials",
    "bidi \u{202e}evil\u{202c} iso \u{2066}x\u{2069} mark \u{200e}\u{200f}",
    "zero\u{200b}width\u{200c}joiner\u{200d}word\u{feff}bom\u{2060}",
    "control \u{1b}[2K\u{1b}]0;title\u{7} \u{85} \u{9f}",
    "--message --thread -- -h",
    "ZEBRA-CALL-TEXT-91",
    "$(cat /proc/self/environ)",
    "a;b&&c||d&e",
    "\u{ff1b}\u{ff06}fullwidth ｜ pipe",
];

fn only_the_line(cmdline: &str, who_program: &str, thread: &str, id: &str) {
    // `<program>|<sub>|--thread|<thread>|--message|<one line>|`: seven fields at most, the line one.
    let fields: Vec<&str> = cmdline.trim_end().split('|').collect();
    assert!(cmdline.contains(&format!("id {id}")), "{cmdline}");
    assert!(cmdline.contains(&format!(" read {id}`")), "{cmdline}");
    assert!(cmdline.trim_end().ends_with('|'), "{cmdline}");
    assert!(fields.len() <= 9, "{who_program} {thread}: {fields:?}");
    for payload in HOSTILE {
        for piece in payload.split(['\n', '\r', '\u{2028}', '\u{2029}']) {
            if piece.chars().count() > 3 {
                assert!(
                    !cmdline.contains(piece),
                    "{piece:?} is on the command line: {cmdline}"
                );
            }
        }
    }
    assert!(
        !cmdline.contains("Bash") && !cmdline.contains("command"),
        "{cmdline}"
    );
}

// U6-054, U6-055: whatever the call holds (quotes, operators, newlines, expansions, a secret, a path,
// marks that reorder text) the command line holds one line of Lunchroom's naming the id.
#[test]
fn hostile_call_text_never_reaches_handover_argv() {
    for (who, program, sub) in [
        (Identity::new(Harness::Codex, "x1"), "codex", "queue"),
        (
            Identity::new(Harness::OpenClaw, "agent:main:k1"),
            "openclaw",
            "agent",
        ),
    ] {
        for (n, payload) in HOSTILE.iter().enumerate() {
            let mut w = World::new();
            let log = w.recipient(&who, program, sub, 0);
            let request = Request {
                session: who.clone(),
                agent: "Bob".to_owned(),
                tool: "Bash".to_owned(),
                input: json!({ "command": payload, payload.to_string(): payload }),
                cwd: "/work/a".to_owned(),
                rule: String::new(),
                at: 1000,
            };
            let store = w.store();
            let kept = request.clone();
            assert!(store.update(|m| m.request(kept)).is_ok());
            let id = refuse(&w.env, &store, &request);
            assert!(id.is_ok(), "{n}: {id:?}");
            let id = id.unwrap_or_default();
            let cmdline = std::fs::read_to_string(&log).unwrap_or_default();
            only_the_line(&cmdline, program, &who.session, &id);
            // And the line read back is the call, shown with nothing raw in it.
            let mail = store.read().unwrap_or_default();
            let line = mail.tell_line_for(&who, &id).unwrap_or_default();
            assert!(
                !line.chars().any(|c| matches!(
                    c,
                    '\u{202e}' | '\u{2066}' | '\u{200b}' | '\u{feff}' | '\u{0}' | '\u{1b}'
                )),
                "{line:?}"
            );
        }
    }
}

// U6-045: nothing is started for a Claude Code session: its retry's reason tells it; even with no
// store to read, nothing is sent and nothing fails.
#[test]
fn claude_refusal_uses_retry_reason_only() {
    let w = World::new();
    let me = Identity::new(Harness::ClaudeCode, "s1");
    let request = w.wait(&me, CALL);
    let elsewhere = World::new();
    let store = w.store();
    assert_eq!(
        tell_decision(
            &elsewhere.env,
            &store,
            &request,
            "rf-00000001",
            TellKind::Refused
        ),
        Ok(())
    );
    assert_eq!(
        tell_decision(
            &elsewhere.env,
            &store,
            &request,
            "ap-00000001",
            TellKind::Approved
        ),
        Ok(())
    );
    assert!(w.children.is_empty());
    let id = refuse(&w.env, &store, &request);
    assert!(id.is_ok());
    assert!(!store.read().unwrap_or_default().has_pending_tell(&me));
}

// U6-051, U6-052: sending the line to Codex or OpenClaw changes no hold of the session: the same
// holds before and after, for a refusal and for an approval.
#[test]
fn decision_tell_sends_change_no_hold() {
    for (who, program, sub) in [
        (Identity::new(Harness::Codex, "x1"), "codex", "queue"),
        (
            Identity::new(Harness::OpenClaw, "agent:main:k1"),
            "openclaw",
            "agent",
        ),
    ] {
        let mut w = World::new();
        let _ = w.recipient(&who, program, sub, 0);
        let held = w
            .store()
            .update(|m| m.hold_unrecorded(&who, "cross-session", "m1"));
        assert!(held.is_ok());
        let holds = |w: &World| {
            w.store()
                .read()
                .map(|m| {
                    m.holding(&who)
                        .iter()
                        .map(|h| h.message.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let before = holds(&w);
        assert_eq!(before.len(), 1);
        let first = w.wait(&who, "one");
        assert!(refuse(&w.env, &w.store(), &first).is_ok());
        assert_eq!(holds(&w), before);
        let _ = w.wait(&who, "two");
        let out = approve_first(&w);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(!out.stdout.contains("not told"), "{}", out.stdout);
        assert_eq!(holds(&w), before);
    }
}

// U6-078: the hand-over program is the recipient's own, by the path of its process, started with
// the recipient's environment and none of the sender's: no variable of this test's process (cargo
// sets many) reaches it, and its folder and home are the recipient's.
#[test]
fn decision_handover_uses_recipient_program_and_environment() {
    for (who, program, sub) in [
        (Identity::new(Harness::Codex, "x1"), "codex", "queue"),
        (
            Identity::new(Harness::OpenClaw, "agent:main:k1"),
            "openclaw",
            "agent",
        ),
    ] {
        let mut w = World::new();
        let log = w.root.join("environment.txt");
        let dir = w.recipient_script(
            &who,
            program,
            sub,
            &format!("env > '{}'\nexit 0", log.display()),
        );
        // A program of the same name, planted first on the sender's side, is never run.
        let marker = w.root.join("planted-ran.txt");
        let planted = w.root.join("planted");
        assert!(std::fs::create_dir_all(&planted).is_ok());
        assert!(
            std::fs::write(
                planted.join(program),
                format!("#!/bin/sh\ntouch '{}'\n", marker.display())
            )
            .is_ok()
        );
        assert!(
            std::fs::set_permissions(
                planted.join(program),
                std::fs::Permissions::from_mode(0o755)
            )
            .is_ok()
        );
        let request = w.wait(&who, CALL);
        let id = refuse(&w.env, &w.store(), &request);
        assert!(id.is_ok(), "{id:?}");
        assert!(!marker.exists());
        let seen = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!seen.is_empty(), "the program did not run");
        assert!(
            !seen.lines().any(|l| l.starts_with("CARGO")
                || l.starts_with("RUST")
                || l.starts_with("LUNCHROOM")),
            "{seen}"
        );
        assert!(
            seen.lines()
                .any(|l| l.starts_with(&format!("PATH={}:", dir.display()))),
            "{seen}"
        );
        assert!(
            seen.lines()
                .any(|l| l == format!("HOME={}", w.env.home.display())),
            "{seen}"
        );
        if who.harness == Harness::Codex {
            assert!(seen.lines().any(|l| l.starts_with("CODEX_HOME=")), "{seen}");
        }
    }
}

// U6-079: the recipient's program cannot be established: its process has ended, its file was
// replaced or is not an executable one, its process is not recorded; nothing is started, the decision
// stands, and the user is told that the session was not told, and why.
#[test]
fn decision_tell_program_establishment_failures_are_safe() {
    for (who, program, sub) in [
        (Identity::new(Harness::Codex, "x1"), "codex", "queue"),
        (
            Identity::new(Harness::OpenClaw, "agent:main:k1"),
            "openclaw",
            "agent",
        ),
    ] {
        for case in ["replaced", "not-executable"] {
            let mut w = World::new();
            let log = w.recipient(&who, program, sub, 0);
            let fake = w
                .root
                .join(format!("bin-{}", who.session.replace(':', "_")))
                .join(program);
            match case {
                "replaced" => {
                    let other = fake.with_extension("other");
                    assert!(std::fs::write(&other, "#!/bin/sh\nexit 0\n").is_ok());
                    assert!(
                        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755))
                            .is_ok()
                    );
                    assert!(std::fs::rename(&other, &fake).is_ok());
                }
                _ => {
                    assert!(
                        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o644))
                            .is_ok()
                    );
                }
            }
            let request = w.wait(&who, CALL);
            let told = refuse(&w.env, &w.store(), &request);
            assert!(
                told.as_ref().err().is_some_and(|e| e.contains(
                    "declined; the session was not told: the recipient's program is not the file it is running"
                )),
                "{case}: {told:?}"
            );
            assert!(!log.exists(), "{case}: a program was run");
            let mail = w.store().read().unwrap_or_default();
            assert!(
                mail.refused_at(&who, "Bash", &json!({ "command": CALL }), "/work/a")
                    .is_some(),
                "{case}"
            );
            assert!(mail.tell_line_for(&who, "rf-00000001").is_some(), "{case}");
        }
    }
}

// U6-080: the hand-over program exits with an error, is killed, or gives no answer: the decision
// stands, the error is shown, nothing is told twice. A Codex program that gives no answer in its
// minute is an error with that reason; an OpenClaw turn that goes on is not one.
#[test]
fn decision_tell_process_outcomes_are_safe() {
    let codex = Identity::new(Harness::Codex, "x1");
    let claw = Identity::new(Harness::OpenClaw, "agent:main:k1");
    for (who, program, sub) in [(&codex, "codex", "queue"), (&claw, "openclaw", "agent")] {
        let mut w = World::new();
        let _ = w.recipient_script(who, program, sub, "kill -9 $$");
        let request = w.wait(who, CALL);
        let told = refuse(&w.env, &w.store(), &request);
        assert!(
            told.as_ref().err().is_some_and(|e| e.contains("exited -1")),
            "{program}: {told:?}"
        );
        assert!(
            w.store()
                .read()
                .unwrap_or_default()
                .refused_at(who, "Bash", &json!({ "command": CALL }), "/work/a")
                .is_some()
        );
    }
    // No answer.
    let mut w = World::new();
    let _ = w.recipient_script(&claw, "openclaw", "agent", "sleep 300");
    let request = w.wait(&claw, CALL);
    let started = std::time::Instant::now();
    assert!(refuse(&w.env, &w.store(), &request).is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    let mut w = World::new();
    let _ = w.recipient_script(&codex, "codex", "queue", "sleep 300");
    let request = w.wait(&codex, CALL);
    let told = refuse(&w.env, &w.store(), &request);
    assert!(
        told.as_ref()
            .err()
            .is_some_and(|e| e.contains("gave no answer")),
        "{told:?}"
    );
    let mail = w.store().read().unwrap_or_default();
    assert!(
        mail.refused_at(&codex, "Bash", &json!({ "command": CALL }), "/work/a")
            .is_some()
    );
    assert!(mail.tell_line_for(&codex, "rf-00000001").is_some());
}

// U6-098: an OpenClaw gateway under a runtime is reached by the package that holds the script the
// plugin reported, never by a name found on a path: with no script reported, a script outside an
// `openclaw` package, or a package without its bin, nothing is run, and a planted `openclaw` is not.
#[test]
fn openclaw_decision_tell_resolves_gateway_safely() {
    let claw = Identity::new(Harness::OpenClaw, "agent:main:k1");
    let marker_name = "planted-ran.txt";
    for case in ["no-entry", "outside-package", "package-without-bin"] {
        let mut w = World::new();
        let dir = w.recipient_script(&claw, "node", "agent", "exit 0");
        let marker = w.root.join(marker_name);
        assert!(
            std::fs::write(
                dir.join("openclaw"),
                format!("#!/bin/sh\ntouch '{}'\n", marker.display())
            )
            .is_ok()
        );
        assert!(
            std::fs::set_permissions(dir.join("openclaw"), std::fs::Permissions::from_mode(0o755))
                .is_ok()
        );
        let entry = match case {
            "outside-package" => {
                let script = w.root.join("elsewhere/entry.js");
                assert!(std::fs::create_dir_all(w.root.join("elsewhere")).is_ok());
                assert!(std::fs::write(&script, "").is_ok());
                Some(script)
            }
            "package-without-bin" => {
                let package = w.root.join("pkg");
                assert!(std::fs::create_dir_all(package.join("dist")).is_ok());
                assert!(
                    std::fs::write(package.join("package.json"), "{\"name\": \"openclaw\"}")
                        .is_ok()
                );
                assert!(std::fs::write(package.join("dist/entry.js"), "").is_ok());
                Some(package.join("dist/entry.js"))
            }
            _ => None,
        };
        if let Some(entry) = entry {
            let who = claw.clone();
            assert!(w.store().update(|m| m.observe_entry(&who, &entry)).is_ok());
        }
        let request = w.wait(&claw, CALL);
        let told = refuse(&w.env, &w.store(), &request);
        assert!(
            told.as_ref()
                .err()
                .is_some_and(|e| e.contains("declined; the session was not told: ")),
            "{case}: {told:?}"
        );
        assert!(!marker.exists(), "{case}: the planted program ran");
    }
}

// U6-039: a store that cannot be written: the user is told the decision was not kept, and nothing is
// claimed; once it can be written again the call is still waiting.
#[test]
fn refusal_store_write_failure_is_reported() {
    let w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let request = w.wait(&me, CALL);
    let dir = w.env.store_dir();
    assert!(std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).is_ok());
    let told = refuse(&w.env, &w.store(), &request);
    assert!(std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok());
    assert!(told.is_err(), "{told:?}");
    assert!(
        !told.as_ref().err().is_some_and(|e| e.contains("declined;")),
        "no success is claimed: {told:?}"
    );
    let mail = w.store().read().unwrap_or_default();
    assert_eq!(mail.requests().len(), 1);
    assert!(
        mail.refused_at(&me, "Bash", &json!({ "command": CALL }), "/work/a")
            .is_none()
    );
}

// U6-081: the decision kept and the program that tells failing: the line is readable and the retry
// is refused; asking to refuse again finds nothing waiting and changes nothing.
#[test]
fn crash_between_refusal_and_tell_is_recoverable() {
    let mut w = World::new();
    let me = Identity::new(Harness::Codex, "x1");
    let _ = w.recipient_script(&me, "codex", "queue", "exit 1");
    let request = w.wait(&me, CALL);
    let store = w.store();
    assert!(refuse(&w.env, &store, &request).is_err());
    let mail = store.read().unwrap_or_default();
    assert!(
        mail.refused_at(&me, "Bash", &json!({ "command": CALL }), "/work/a")
            .is_some()
    );
    assert!(mail.tell_line_for(&me, "rf-00000001").is_some());
    assert_eq!(
        refuse(&w.env, &store, &request),
        Err("that call is no longer waiting".to_owned())
    );
    let again = store.read().unwrap_or_default();
    assert!(again.tell_line_for(&me, "rf-00000001").is_some());
    assert!(again.tell_line_for(&me, "rf-00000002").is_none());
}
