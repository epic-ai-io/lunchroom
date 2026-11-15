//! Lunchroom 1.1, unit U10: `lunchroom clear <path>` (T1.8, `§5::45`, `§5::49`). Each test
//! is derived from the acceptance criterion of the node it names and from the failure classes of
//! its clause: a path spelled another way, a path that is not recorded, a wrong passphrase, an
//! agent running the command, two entries sharing a record.

use std::sync::atomic::{AtomicUsize, Ordering};

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin};
use lunchroom_core::passphrase;
use lunchroom_core::store::Store;

use super::clear;
use crate::home::Env;
use crate::terminal::{System, fake};

/// A terminal that is the user's own (an unrecorded shell above it) or an agent's (a recorded
/// harness process above it); `answer` stands for the passphrase typed.
struct Terminal {
    ancestor: &'static str,
    answer: &'static str,
    secrets: usize,
}

impl System for Terminal {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        vec![(Some(self.ancestor.to_owned()), "bash".to_owned())]
    }
    fn own_tty(&self) -> u64 {
        34816
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
        Ok(fake::verifier())
    }
    fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
        self.secrets += 1;
        Some(fake::typed(self.answer))
    }
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        Ok([3; passphrase::SALT_BYTES])
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Ok(())
    }
}

fn user(answer: &'static str) -> Terminal {
    Terminal {
        ancestor: "100:5",
        answer,
        secrets: 0,
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A scratch Lunchroom folder with a store in it, removed afterwards.
struct Scratch {
    env: Env,
    root: std::path::PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = crate::test_target()
            .join("clear-tests")
            .join(format!("{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(root.join("real"));
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
        }
    }

    fn store(&self) -> Option<Store<Mail>> {
        Store::open(&self.env.store_dir()).ok()
    }

    /// The paths recorded now, each spelling of each entry.
    fn recorded(&self) -> Vec<String> {
        self.store()
            .and_then(|s| s.read().ok())
            .map(|m| m.marked().iter().flat_map(|e| e.paths.clone()).collect())
            .unwrap_or_default()
    }

    /// A HELD writer (c1, held by Codex's message) whose call wrote `paths`.
    fn record(&self, paths: &[&str]) {
        let store = self.store();
        assert!(store.is_some());
        let Some(store) = store else { return };
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        let ok = store
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
                m.record_written(&claude("c1"), paths, "t1", 5)
            })
            .unwrap_or(false);
        assert!(ok);
    }

    /// `reader` reads `path`: whether a recorded path held it, and whether it is HELD now.
    fn read_by(&self, reader: &str, path: &str) -> (bool, bool) {
        let who = claude(reader);
        self.store()
            .and_then(|s| {
                s.update(|m| {
                    let reached = m.hold_by_marked(&who, &[path.to_owned()], &[]);
                    (reached, m.is_held(&who))
                })
                .ok()
            })
            .unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn claude(s: &str) -> Identity {
    Identity::new(Harness::ClaudeCode, s)
}

fn codex(s: &str) -> Identity {
    Identity::new(Harness::Codex, s)
}

fn args(path: &str) -> Vec<String> {
    vec![path.to_owned()]
}

// §5::45 (T1.8): a recorded path holds a reader until the user clears it with `lunchroom
// clear <path>` on their passphrase; after that a session that reads it is not held by it, and the
// record is gone. The passphrase is asked once.
#[test]
fn the_user_clears_a_recorded_path_and_a_reader_is_no_longer_held() {
    let s = Scratch::new();
    s.record(&["/work/out.txt"]);
    assert_eq!(s.read_by("r1", "/work/out.txt"), (true, true));
    // The user types in the reader's session, which ends that hold; the path holds it again.
    let ended = s
        .store()
        .and_then(|st| st.update(|m| m.user_typed(&claude("r1"))).ok());
    assert!(ended.is_some());
    assert_eq!(s.read_by("r1", "/work/out.txt"), (true, true));
    let ended = s
        .store()
        .and_then(|st| st.update(|m| m.user_typed(&claude("r1"))).ok());
    assert!(ended.is_some());
    let mut terminal = user("yes");
    let out = clear(&args("/work/out.txt"), &s.env, &mut terminal);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.starts_with("cleared /work/out.txt"),
        "{}",
        out.stdout
    );
    assert_eq!(terminal.secrets, 1);
    assert_eq!(s.recorded(), Vec::<String>::new());
    assert_eq!(s.read_by("r1", "/work/out.txt"), (false, false));
}

// §5::45: a wrong passphrase clears nothing; the path stays recorded and holds a reader.
#[test]
fn a_wrong_passphrase_clears_nothing() {
    let s = Scratch::new();
    s.record(&["/work/out.txt"]);
    let mut terminal = user("no");
    let out = clear(&args("/work/out.txt"), &s.env, &mut terminal);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("DENIED"), "{}", out.stderr);
    assert_eq!(s.recorded(), ["/work/out.txt"]);
    assert_eq!(s.read_by("r1", "/work/out.txt"), (true, true));
}

// §5::45: `clear` of a path that is not recorded says it is not, asks no passphrase and
// changes nothing; of a path that only resembles a recorded one, the same.
#[test]
fn clear_of_a_path_that_is_not_recorded_changes_nothing() {
    let s = Scratch::new();
    s.record(&["/work/out.txt"]);
    for typed in ["/work/other.txt", "/work", "/work/out.txt.bak", "out.txt"] {
        let mut terminal = user("yes");
        let out = clear(&args(typed), &s.env, &mut terminal);
        assert_eq!(out.code, 1, "{typed}: {}{}", out.stdout, out.stderr);
        assert!(
            out.stderr
                .contains(&format!("{typed} is not a recorded path")),
            "{typed}: {}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("`lunchroom status` lists the recorded paths")
        );
        assert_eq!(terminal.secrets, 0, "{typed}: nothing is asked");
        assert_eq!(s.recorded(), ["/work/out.txt"], "{typed}");
    }
    // No store entry at all.
    let empty = Scratch::new();
    let mut terminal = user("yes");
    let out = clear(&args("/work/out.txt"), &empty.env, &mut terminal);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert_eq!(terminal.secrets, 0);
}

// §5::49 (T1.8): an agent's `clear` is DENIED before it reads anything, the path stays
// recorded, and no passphrase is asked of it; the same for a path that is not recorded.
#[test]
fn an_agents_clear_is_denied_and_the_path_stays_recorded() {
    let s = Scratch::new();
    s.record(&["/work/out.txt"]);
    // A hook recorded the process above this command as a Claude Code session.
    let seen = s.store().and_then(|st| {
        st.update(|m| m.observe_process("200:7", &s.root, &claude("c9")))
            .ok()
    });
    assert!(seen.is_some());
    for typed in ["/work/out.txt", "/work/none.txt"] {
        let mut terminal = Terminal {
            ancestor: "200:7",
            answer: "yes",
            secrets: 0,
        };
        let out = clear(&args(typed), &s.env, &mut terminal);
        assert_eq!(out.code, 3, "{typed}: {}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("DENIED"), "{typed}: {}", out.stderr);
        assert!(
            out.stderr.contains("guarded session"),
            "{typed}: {}",
            out.stderr
        );
        assert_eq!(terminal.secrets, 0, "{typed}");
        assert_eq!(s.recorded(), ["/work/out.txt"], "{typed}");
    }
}

// §5::45: the path is cleared whatever way it is typed that names the same file: a
// `..` in it, or a link to it. Every spelling the guard recorded for that file goes, and another
// file of the same call stays recorded.
#[test]
fn every_spelling_of_the_file_goes_and_another_file_stays() {
    let s = Scratch::new();
    let real = s.root.join("real");
    let link = s.root.join("link");
    assert!(std::fs::write(real.join("a.txt"), b"a").is_ok());
    assert!(std::fs::write(real.join("b.txt"), b"b").is_ok());
    assert!(std::os::unix::fs::symlink(&real, &link).is_ok());
    let spell = |dir: &std::path::Path, name: &str| dir.join(name).display().to_string();
    let (a_real, a_link, b_real) = (
        spell(&real, "a.txt"),
        spell(&link, "a.txt"),
        spell(&real, "b.txt"),
    );
    s.record(&[&a_real, &a_link, &b_real]);
    // Typed with a `..` in it: the same name once normalised.
    let dotted = format!("{}/sub/../a.txt", real.display());
    let mut terminal = user("yes");
    let out = clear(&args(&dotted), &s.env, &mut terminal);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("2 spellings"), "{}", out.stdout);
    assert_eq!(s.recorded(), std::slice::from_ref(&b_real));
    // Typed through the link: the file is the one `real/b.txt` is, and it goes alone.
    let through = spell(&link, "b.txt");
    let mut terminal = user("yes");
    let out = clear(&args(&through), &s.env, &mut terminal);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(s.recorded(), Vec::<String>::new());
}

// §5::45: `clear` takes exactly one path.
#[test]
fn clear_takes_one_path() {
    let s = Scratch::new();
    s.record(&["/work/out.txt"]);
    for given in [vec![], vec!["/a".to_owned(), "/b".to_owned()]] {
        let mut terminal = user("yes");
        let out = clear(&given, &s.env, &mut terminal);
        assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("usage: lunchroom clear <path>"));
        assert_eq!(terminal.secrets, 0);
    }
    assert_eq!(s.recorded(), ["/work/out.txt"]);
}
