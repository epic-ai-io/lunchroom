//! Lunchroom 1.1, unit U10: what `lunchroom status` and `lunchroom doctor` print (the read column,
//! the recorded paths, the Codex threads no guard hook has run in, the store wall, what `sudo -n
//! -l` shows and the console). Each test is derived from the acceptance criterion of the node it
//! names and from the failure classes of its clause: a part broken, a part absent, an answer that
//! changed, a line that must leave the exit code, a list too long to print.

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin};
use lunchroom_core::store::Store;
use lunchroom_core::sudoers::{Entry, Kept, PathFacts};

use super::tests::{SUDO_ASKS, Setup};
use super::{Outcome, status};
use crate::install::console_state::{Kind, State};
use crate::wall::tests::good;

/// A part of the wall broken: its name, the change, and what the FAIL says.
type Break = (&'static str, fn(&mut crate::wall::Facts), &'static str);

const WIDE: &str =
    "User dev may run the following commands on host:\n    (ALL : ALL) NOPASSWD: ALL\n";
const NARROW: &str = "User dev may run the following commands on host:\n    (root) NOPASSWD: /usr/local/sbin/send-lead-email\n";

fn fails(out: &Outcome) -> Vec<&str> {
    out.stdout
        .lines()
        .filter(|l| l.starts_with("FAIL "))
        .collect()
}

fn notes(out: &Outcome) -> Vec<&str> {
    out.stdout
        .lines()
        .filter(|l| l.starts_with("NOTE "))
        .collect()
}

/// A file root owns in a folder root owns, as the narrow rule names it.
#[allow(clippy::unnecessary_wraps)] // the shape the doctor's file facts have
fn root_owns(path: &str) -> Option<PathFacts> {
    let entry = Entry {
        owner: 0,
        mode: 0o755,
    };
    Some(PathFacts {
        file: Some(entry),
        above: vec![entry, entry],
        names: vec![path.rsplit('/').next().unwrap_or_default().to_owned()],
    })
}

fn codex(session: &str) -> Identity {
    Identity::new(Harness::Codex, session)
}

fn claude(session: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, session)
}

fn open(s: &Setup) -> Option<Store<Mail>> {
    Store::open(&s.env.store_dir()).ok()
}

const THREAD: &str = "019a1b2c-3d4e-5f60-8a9b-0c1d2e3f4a5b";

/// A Codex session record for thread `thread`, as Codex writes it.
fn rollout(s: &Setup, day: u32, thread: &str) {
    let dir = s.env.codex_home.join(format!("sessions/2026/10/{day:02}"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join(format!("rollout-2026-10-{day:02}T00-00-00-{thread}.jsonl")),
        "{}\n",
    );
}

/// A thread id of the shape Codex names a session by, numbered `n`.
fn thread_number(n: usize) -> String {
    format!("{n:08x}-1111-2222-3333-444444444444")
}

// §14::30: each broken part of the store wall (group, setgid program, folder, verifier,
// user in the group) gives its own FAIL line, and doctor exits 1; a wall as installed gives none.
#[test]
fn each_broken_part_of_the_wall_is_a_fail_and_exit_1() {
    let s = Setup::new("u10-wall");
    let whole = s.doctor();
    assert_eq!(
        (whole.code, fails(&whole).len()),
        (0, 0),
        "{}",
        whole.stdout
    );
    let breaks: [Break; 5] = [
        ("group", |f| f.group = None, "there is no group lunchroom"),
        (
            "setgid",
            |f| {
                if let Ok(n) = &mut f.program {
                    n.mode = 0o755;
                }
            },
            "does not have the set-group-id bit",
        ),
        (
            "folder",
            |f| f.folder = Err("it is not there".to_owned()),
            "the store's folder /var/lib/lunchroom/1000: it is not there",
        ),
        (
            "verifier",
            |f| {
                if let Ok(n) = &mut f.verifier {
                    n.mode = 0o644;
                }
            },
            "has mode 0644, not 0640",
        ),
        (
            "member",
            |f| f.members.push("dev".to_owned()),
            "your account dev is in the group lunchroom",
        ),
    ];
    for (part, change, said) in breaks {
        let mut wall = good();
        change(&mut wall);
        let out = s.doctor_full(
            Ok(crate::terminal::fake::verifier()),
            Ok(SUDO_ASKS.to_owned()),
            wall,
            &|_| None,
        );
        assert_eq!(out.code, 1, "{part}: {}", out.stdout);
        let broken = fails(&out);
        assert!(
            broken.iter().any(|l| l.contains(said)),
            "{part}: {said} in {}",
            out.stdout
        );
        assert!(
            broken.iter().all(|l| l.starts_with("FAIL the store wall")),
            "{part}: {}",
            out.stdout
        );
        // The other four parts still pass: one FAIL for the part, not one for each.
        let walls = out
            .stdout
            .lines()
            .filter(|l| l.contains("the store wall"))
            .count();
        assert_eq!(
            walls,
            if part == "group" { 5 } else { 1 },
            "{part}: {}",
            out.stdout
        );
    }
}

// §14::31: no password-free rule in `sudo -n -l` is a PASS line.
#[test]
fn no_password_free_rule_is_a_pass() {
    let s = Setup::new("u10-sudo-pass");
    let out = s.doctor();
    assert!(
        out.stdout
            .contains("PASS `sudo -n -l` shows no rule that runs a command without a password\n"),
        "{}",
        out.stdout
    );
    assert_eq!(out.code, 0, "{}", out.stdout);
}

// §14::32, §7::59, §17::83-84: a password-free rule is printed on a line that is not a
// FAIL, and the exit code is the one doctor gives without that line: 0 on a setup whose checks
// pass, 1 on one where a check fails. A wide rule says plainly that the passphrase check and the
// store wall do not hold until it is removed; a narrow one says the walls stand.
#[test]
fn a_password_free_rule_is_a_line_that_leaves_the_exit_code() {
    let s = Setup::new("u10-sudo-line");
    let with = |text: &str| {
        s.doctor_full(
            Ok(crate::terminal::fake::verifier()),
            Ok(text.to_owned()),
            good(),
            &root_owns,
        )
    };
    for (what, text, said) in [
        (
            "wide",
            WIDE,
            "The passphrase check and the store wall do not hold on this account until that \
             rule is removed",
        ),
        (
            "narrow",
            NARROW,
            "every such rule is narrow (§17.8), so the store wall and the console's wall stand",
        ),
    ] {
        let out = with(text);
        assert_eq!(out.code, 0, "{what}: {}", out.stdout);
        assert!(fails(&out).is_empty(), "{what}: {}", out.stdout);
        let lines = notes(&out);
        assert!(
            lines.iter().any(|l| l
                .contains("a rule that lets your account run a command without a password")
                && l.contains(said)),
            "{what}: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("PASS `sudo -n -l`"),
            "{what}: {}",
            out.stdout
        );
    }
    // The same rules on a setup where a check fails: still exit 1, with the same FAIL lines.
    let _ = std::fs::remove_file(s.program());
    let without = s.doctor();
    assert_eq!(without.code, 1, "{}", without.stdout);
    for text in [WIDE, NARROW] {
        let out = with(text);
        assert_eq!(out.code, 1, "{}", out.stdout);
        assert_eq!(fails(&out), fails(&without), "{}", out.stdout);
    }
}

// §14::31-32: `sudo -n -l` that cannot be run, or printed what is not a listing, is not
// shown to hold either way: a line, no PASS, and the exit code stays.
#[test]
fn a_sudo_that_cannot_be_read_is_a_line_not_a_pass() {
    let s = Setup::new("u10-sudo-unread");
    for answer in [
        Err("sudo was not found in the system folders".to_owned()),
        Ok("sudo: something nobody expected\n".to_owned()),
    ] {
        let out = s.doctor_full(
            Ok(crate::terminal::fake::verifier()),
            answer,
            good(),
            &|_| None,
        );
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(!out.stdout.contains("PASS `sudo -n -l`"), "{}", out.stdout);
        assert!(
            notes(&out)
                .iter()
                .any(|l| l.contains("not shown whether a rule")),
            "{}",
            out.stdout
        );
    }
}

// §17::76-77 (fixed2): doctor uses the answer kept at the install, and prints a FAIL
// telling the user to run `lunchroom install console` again when the developer's `sudo -n -l`
// differs from the one kept; with it unchanged there is no such FAIL.
#[test]
fn a_changed_sudo_answer_is_a_fail_that_says_to_install_again() {
    let s = Setup::new("u10-sudo-kept");
    let kept = Kept {
        read_at: 5,
        own: SUDO_ASKS.to_owned(),
        others: std::collections::BTreeMap::new(),
    };
    assert!(crate::terminal::keep_answer_in(&s.env.store_dir(), &kept).is_ok());
    let again = |text: &str| {
        s.doctor_full(
            Ok(crate::terminal::fake::verifier()),
            Ok(text.to_owned()),
            good(),
            &root_owns,
        )
    };
    let unchanged = again(SUDO_ASKS);
    assert_eq!(unchanged.code, 0, "{}", unchanged.stdout);
    assert!(fails(&unchanged).is_empty(), "{}", unchanged.stdout);
    // A text that prints the same rules in other words (a refusal worded as sudo words it) is the
    // same answer.
    let reworded = again("sudo: a password is required\n\n");
    assert!(fails(&reworded).is_empty(), "{}", reworded.stdout);
    // A rule appeared since the install.
    let changed = again(NARROW);
    assert_eq!(changed.code, 1, "{}", changed.stdout);
    assert!(
        fails(&changed)
            .iter()
            .any(|l| l.contains("differ from the ones Lunchroom kept")
                && l.contains("run `lunchroom install console` again")),
        "{}",
        changed.stdout
    );
    // The kept answer is the one judged: kept is narrow, and so is what the line says.
    let narrow_kept = Kept {
        read_at: 6,
        own: NARROW.to_owned(),
        others: std::collections::BTreeMap::new(),
    };
    assert!(crate::terminal::keep_answer_in(&s.env.store_dir(), &narrow_kept).is_ok());
    let out = again(NARROW);
    assert!(fails(&out).is_empty(), "{}", out.stdout);
    assert!(
        notes(&out)
            .iter()
            .any(|l| l.contains("so the store wall and the console's wall stand")),
        "{}",
        out.stdout
    );
    // Kept answers of other users are used: a rule that runs as `builder`, whose own rules were
    // kept and hold a wide rule, is wide.
    let as_other =
        "User dev may run the following commands on host:\n    (builder) NOPASSWD: ALL\n";
    let mut others = std::collections::BTreeMap::new();
    others.insert("builder".to_owned(), WIDE.to_owned());
    let wide_kept = Kept {
        read_at: 7,
        own: as_other.to_owned(),
        others,
    };
    assert!(crate::terminal::keep_answer_in(&s.env.store_dir(), &wide_kept).is_ok());
    let out = again(as_other);
    assert!(fails(&out).is_empty(), "{}", out.stdout);
    assert!(
        notes(&out)
            .iter()
            .any(|l| l.contains("do not hold on this account until that rule is removed")),
        "{}",
        out.stdout
    );
}

// §14::33: the console installed is a PASS.
#[test]
fn an_installed_console_is_a_pass() {
    let s = Setup::new("u10-console-pass");
    let mut wall = good();
    wall.console_installed = true;
    let out = s.doctor_full(
        Ok(crate::terminal::fake::verifier()),
        Ok(SUDO_ASKS.to_owned()),
        wall,
        &|_| None,
    );
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stdout.contains("PASS the console is installed"),
        "{}",
        out.stdout
    );
    assert!(notes(&out).is_empty(), "{}", out.stdout);
}

// §14::34, §17::98 (fixed2 104), §17::104 (fixed2 110): a console not set up because
// the user answered anything but `yes` (no, an empty answer, `y`), or because the rule could not
// be put in force, is a FAIL that gives the reason the install recorded and says that running
// `lunchroom install console` again asks once more; doctor exits 1.
#[test]
fn a_console_not_set_up_is_a_fail_with_the_recorded_reason() {
    let s = Setup::new("u10-console-fail");
    for (kind, reason) in [
        (
            Kind::Declined,
            "you answered no to the rule that makes sudo ask for your password",
        ),
        (
            Kind::Declined,
            "you gave an empty answer to the rule that makes sudo ask for your password",
        ),
        (
            Kind::Declined,
            "you answered y, which is not yes, to the rule that makes sudo ask for your password",
        ),
        (
            Kind::NotInForce,
            "a later rule, `dev ALL=(ALL) NOPASSWD: ALL` in /etc/sudoers, wins",
        ),
    ] {
        let mut wall = good();
        wall.console_state = Some(Ok(State {
            kind,
            reason: reason.to_owned(),
            at: 10,
        }));
        let out = s.doctor_full(
            Ok(crate::terminal::fake::verifier()),
            Ok(SUDO_ASKS.to_owned()),
            wall,
            &|_| None,
        );
        assert_eq!(out.code, 1, "{reason}: {}", out.stdout);
        assert!(
            fails(&out).iter().any(|l| l.contains(reason)
                && l.contains("Running `lunchroom install console` again asks once more")),
            "{reason}: {}",
            out.stdout
        );
    }
    // A record that cannot be used is a FAIL too, not a pass.
    let mut wall = good();
    wall.console_state = Some(Err("it cannot be read".to_owned()));
    let out = s.doctor_full(
        Ok(crate::terminal::fake::verifier()),
        Ok(SUDO_ASKS.to_owned()),
        wall,
        &|_| None,
    );
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        fails(&out)
            .iter()
            .any(|l| l.contains("again asks once more")),
        "{}",
        out.stdout
    );
}

// §14::35: no console and no recorded answer is a line that says how to install it, not
// a FAIL, and leaves the exit code: 0 where every check passes, 1 where one fails.
#[test]
fn no_console_and_no_answer_is_a_line_that_leaves_the_exit_code() {
    let s = Setup::new("u10-console-none");
    let out = s.doctor();
    assert_eq!(out.code, 0, "{}", out.stdout);
    let lines = notes(&out);
    assert_eq!(lines.len(), 1, "{}", out.stdout);
    assert!(
        lines[0].contains("the console is not installed: `lunchroom install console` sets it up"),
        "{}",
        out.stdout
    );
    assert!(fails(&out).is_empty(), "{}", out.stdout);
    let _ = std::fs::remove_file(s.program());
    let broken = s.doctor();
    assert_eq!(broken.code, 1, "{}", broken.stdout);
    assert!(
        notes(&broken).iter().any(|l| l.contains("sets it up")),
        "{}",
        broken.stdout
    );
}

// §13::78, §4::42: a Codex thread with a session record and no guard hook record is
// listed by doctor as UNGUARDED with the reason that no guard hook has run in it, as a FAIL; a
// thread a hook recorded is not listed.
#[test]
fn doctor_lists_a_codex_thread_no_guard_hook_has_run_in() {
    let s = Setup::new("u10-doctor-thread");
    rollout(&s, 27, THREAD);
    let out = s.doctor();
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        fails(&out).iter().any(
            |l| l.contains(&format!("Codex thread {THREAD} is UNGUARDED"))
                && l.contains("no guard hook has run in it")
        ),
        "{}",
        out.stdout
    );
    let store = open(&s);
    assert!(store.is_some());
    let Some(store) = store else { return };
    assert!(
        store
            .update(|m| m.observe_process("9:9", &s.root, &codex(THREAD)))
            .is_ok()
    );
    let recorded = s.doctor();
    assert_eq!(recorded.code, 0, "{}", recorded.stdout);
    assert!(!recorded.stdout.contains(THREAD), "{}", recorded.stdout);
}

// §13::77, §4::42: `lunchroom status` lists such a thread UNGUARDED with the reason, in
// the form the user reads (naming the session record), and in the form an agent reads (naming no
// path); it counts the older ones past 20 in one line instead of printing hundreds.
#[test]
fn status_lists_a_codex_thread_no_guard_hook_has_run_in() {
    let s = Setup::new("u10-status-thread");
    rollout(&s, 27, THREAD);
    let out = status(&s.env);
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stdout.contains(&format!(
            "Codex thread {THREAD}\tUNGUARDED\n  why: no guard hook has run in it (session record "
        )),
        "{}",
        out.stdout
    );
    // Past 20 the older ones are one line.
    for n in 0..24 {
        rollout(
            &s,
            1 + u32::try_from(n % 20).unwrap_or(0),
            &thread_number(n),
        );
    }
    let out = status(&s.env);
    let listed = out
        .stdout
        .lines()
        .filter(|l| l.starts_with("Codex thread "))
        .count();
    assert_eq!(listed, 20, "{}", out.stdout);
    assert!(
        out.stdout.contains("5 older Codex threads\tUNGUARDED"),
        "{}",
        out.stdout
    );
    // The agent's form names no path.
    let unguarded = super::unguarded_threads(&Mail::default(), &s.env, false);
    assert!(
        unguarded
            .iter()
            .all(|(_, why)| !why.contains("(session record ")),
        "{unguarded:?}"
    );
}

// §10::30: `lunchroom status` lists read or not read for each message handed over by its
// id, before and after the recipient's `lunchroom read`.
#[test]
fn status_shows_whether_a_message_handed_by_its_id_was_read() {
    let s = Setup::new("u10-status-read");
    let store = open(&s);
    assert!(store.is_some());
    let Some(store) = store else { return };
    let id = store
        .update(|m: &mut Mail| {
            for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
                m.observe(&who);
                let _ = m.register(name, &who, Origin::User, None);
            }
            let id = m
                .send(&claude("c1"), "claude", "codex", "hello", 0)
                .map(|a| a.id)
                .unwrap_or_default();
            // The harness confirmed it: DELIVERED, handed over by its id, not yet read.
            let _ = m.confirm_at(&codex("x1"), &id, 1000);
            id
        })
        .unwrap_or_default();
    assert_ne!(id, "");
    let before = status(&s.env);
    assert!(
        before.stdout.contains(&format!("{id}\tto codex\tnot read")),
        "{}",
        before.stdout
    );
    assert!(store.update(|m| m.mark_read(&id, 1000)).is_ok());
    let after = status(&s.env);
    assert!(
        after.stdout.contains(&format!("{id}\tto codex\tread at ")),
        "{}",
        after.stdout
    );
    assert!(!after.stdout.contains("not read"), "{}", after.stdout);
}

// §5::46: with one recorded path, `lunchroom status` lists it with the agent that wrote
// it and the holders.
#[test]
fn status_lists_a_recorded_path_with_its_writer_and_holders() {
    let s = Setup::new("u10-status-marked");
    let store = open(&s);
    assert!(store.is_some());
    let Some(store) = store else { return };
    let id = store
        .update(|m: &mut Mail| {
            for (name, who) in [("claude", claude("c1")), ("codex", codex("x1"))] {
                m.observe(&who);
                let _ = m.register(name, &who, Origin::User, None);
            }
            let id = m
                .send(&codex("x1"), "codex", "claude", "hi", 0)
                .map(|a| a.id)
                .unwrap_or_default();
            let _ = m.confirm_at(&claude("c1"), &id, 1);
            assert!(m.record_written(&claude("c1"), vec!["/work/out.txt".to_owned()], "t1", 5));
            id
        })
        .unwrap_or_default();
    let out = status(&s.env);
    assert!(
        out.stdout.contains(&format!(
            "  /work/out.txt\twritten by claude\tholders: codex ({id})\n"
        )),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("recorded paths"), "{}", out.stdout);
}
