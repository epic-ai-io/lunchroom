//! Lunchroom 1.1, unit U5, the adapter's side: the reply shown in the approval request (`§6::10`),
//! the sessions a HELD session starts (T1.9), the first word of a Codex command line (D1.8), the
//! six commands an agent is told (I1.5), the hold mode read from a store the guard could not lock
//! (H1.5), the paths resolved on parallel threads (L1.1), the numeric ids of `lunchroom`
//! (§17.5) and the kept `sudo` answer (A1.3). Each test is derived from the node's acceptance
//! criterion and the failure classes of its clause.

// A test of a list that is empty says so by `is_empty`, as the clause does.
#![allow(clippy::assert_is_empty)]

use super::*;

use lunchroom_core::fs::Fs as _;
use lunchroom_core::mail::Origin;

fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("claude-u5")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn words(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_owned).collect()
}

fn reply_call(command: &str, call: CallClass) -> String {
    let me = Identity::new(Harness::ClaudeCode, "c1");
    let judged_input = json!({ "command": command });
    let tool_input = judged_input.clone();
    reply_shown(&Judged {
        me: &me,
        tool: "Bash",
        tool_input: &tool_input,
        judged_input: &judged_input,
        dir: "/w",
        call,
    })
}

const REPLY: CallClass = CallClass::Protected(ProtectedKind::Reply);

// §6::10 (P1.1): in Strict the user is shown the reply's text as the recipient reads it; a reply
// with `--reply-to` shows the same; text from standard input is said not to be shown; a call that
// is not a reply shows nothing.
#[test]
fn a_reply_is_shown_to_the_user_with_its_text() {
    let shown = reply_call("lunchroom send bob 'fix  the tests'", REPLY);
    assert!(
        shown.contains("The reply to bob, as it will be sent:"),
        "{shown}"
    );
    assert!(shown.contains("fix  the tests"), "{shown}");
    for command in [
        "lunchroom send --reply-to lr-1 bob hello there",
        "lunchroom send bob --reply-to lr-1 hello there",
    ] {
        let shown = reply_call(command, REPLY);
        assert!(shown.contains("bob"), "{command}: {shown}");
        assert!(shown.contains("hello there"), "{command}: {shown}");
        assert!(!shown.contains("lr-1"), "{command}: {shown}");
    }
    let stdin = reply_call("lunchroom send bob -", REPLY);
    assert!(stdin.contains("standard input"), "{stdin}");
    assert!(stdin.contains("not shown"), "{stdin}");
    // A call that is not a reply, an empty or a missing command, and a command that is not one
    // `lunchroom send` show nothing.
    assert_eq!(reply_call("lunchroom send bob hi", CallClass::InSlip), "");
    assert_eq!(reply_call("", REPLY), "");
    assert_eq!(reply_call("lunchroom send", REPLY), "");
}

// §6::10: the text is shown with no control character that could rewrite what the user reads.
#[test]
fn a_reply_shown_carries_no_terminal_control() {
    let shown = reply_call("lunchroom send bob $'a\\x1b[2Jb'", REPLY);
    assert!(!shown.contains('\u{1b}'), "{shown:?}");
}

fn held_parent(m: &mut Mail) -> (Identity, String) {
    let parent = Identity::new(Harness::ClaudeCode, "p1");
    let sender = Identity::new(Harness::Codex, "x1");
    m.observe_process("123:456", Path::new("/w"), &parent);
    for (name, who) in [("parent", &parent), ("sender", &sender)] {
        m.observe(who);
        assert_eq!(m.register(name, who, Origin::User, None), Ok(None));
    }
    let id = m
        .send(&sender, "sender", "parent", "hello", 0)
        .map(|a| a.id)
        .unwrap_or_default();
    let _ = m.confirm_at(&parent, &id, 1);
    assert!(m.is_held(&parent));
    (parent, id)
}

fn holding(m: &Mail, who: &Identity) -> Vec<String> {
    m.holding(who).iter().map(|h| h.message.clone()).collect()
}

// T1.9 (§5::47, §5::48): a session first seen with a HELD session's process above it, or with its
// identity variable, is HELD by the parent's messages; an unrelated one is not; a session already
// seen is not made HELD later.
#[test]
fn a_session_a_held_session_started_is_held_by_its_messages() {
    let mut m = Mail::default();
    let (parent, id) = held_parent(&mut m);
    let by_process = Identity::new(Harness::Codex, "child-1");
    inherit_from(&mut m, &by_process, &["123:456".to_owned()], &[]);
    assert_eq!(holding(&m, &by_process), vec![id.clone()]);
    let by_variable = Identity::new(Harness::Codex, "child-2");
    inherit_from(&mut m, &by_variable, &[], std::slice::from_ref(&parent));
    assert_eq!(holding(&m, &by_variable), vec![id]);
    let unrelated = Identity::new(Harness::Hermes, "other");
    inherit_from(&mut m, &unrelated, &["999:1".to_owned()], &[]);
    assert!(!m.is_held(&unrelated));
    // A parent that is not HELD gives nothing, and a session is never its own parent.
    let free = Identity::new(Harness::ClaudeCode, "free");
    m.observe_process("77:8", Path::new("/w"), &free);
    let child = Identity::new(Harness::Codex, "child-3");
    inherit_from(
        &mut m,
        &child,
        &["77:8".to_owned()],
        std::slice::from_ref(&free),
    );
    assert!(!m.is_held(&child));
    inherit_from(&mut m, &parent, &[], std::slice::from_ref(&parent));
    assert_eq!(m.holding(&parent).len(), 1);
}

// T1.9: the variables are read as the harness that sets each writes it, from a process's
// `environ`; an empty value, an unknown name and a bad `LUNCHROOM_SESSION` name no session.
#[test]
fn identity_variables_are_read_from_an_environment() {
    let raw = b"PATH=/bin\0CODEX_THREAD_ID=t1\0HERMES_SESSION_ID=\0LUNCHROOM_SESSION=codex:x9\0\
                CLAUDE_CODE_SESSION_ID=c1\0LUNCHROOM_SESSION=nonsense\0X=CODEX_THREAD_ID=no";
    let found = identities_in_environ(raw);
    assert_eq!(
        found,
        vec![
            Identity::new(Harness::Codex, "t1"),
            Identity::new(Harness::Codex, "x9"),
            Identity::new(Harness::ClaudeCode, "c1"),
        ]
    );
    assert!(identities_in_environ(b"").is_empty());
}

// T1.9, concurrency: two programs seeing two children of one HELD parent at once both hold them.
#[test]
fn two_hooks_at_once_both_inherit() {
    let dir = scratch("inherit-two");
    let store = Store::<Mail>::open(&dir.join("store"));
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    assert!(store.update(|m| held_parent(m).1).is_ok());
    let threads: Vec<_> = (0..2)
        .map(|k| {
            let path = dir.join("store");
            std::thread::spawn(move || {
                let store = Store::<Mail>::open(&path)?;
                let child = Identity::new(Harness::Codex, &format!("kid-{k}"));
                store.update(|m| {
                    inherit_from(m, &child, &["123:456".to_owned()], &[]);
                    m.is_held(&child)
                })
            })
        })
        .collect();
    for t in threads {
        assert!(t.join().is_ok_and(|r| r.is_ok_and(|held| held)));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// D1.8 (§10::88, §10::89): the first word after Codex's options and their values decides.
#[test]
fn the_first_word_after_codexs_options_decides_whether_a_person_types() {
    for headless in [
        "codex -m gpt exec fix",
        "codex -c k=v e hello",
        "codex --profile p review",
        "codex --model=o3 exec x",
    ] {
        assert!(codex_headless(&words(headless)), "{headless}");
    }
    for person in [
        "codex",
        "codex -m gpt",
        "codex -c k=v",
        "codex --profile p resume abc",
    ] {
        assert!(!codex_headless(&words(person)), "{person}");
    }
    // An option not in the table is a program's prompt: the run counts as headless (P1.4).
    assert!(codex_headless(&words("codex --no-such-option exec x")));
}

// I1.5 (§9::26): the join text names the agent and all six commands.
#[test]
fn a_joined_agent_is_told_its_name_and_the_six_commands() {
    let text = member_context("builder");
    assert!(text.contains("builder"), "{text}");
    for command in [
        "lunchroom send",
        "lunchroom read",
        "lunchroom status",
        "lunchroom codex-threads",
        "lunchroom help",
        "lunchroom version",
    ] {
        assert!(text.contains(&format!("`{command}")), "{command}: {text}");
    }
    assert_eq!(MEMBER_COMMANDS.len(), 6);
}

fn env_in(dir: &Path) -> Env {
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
        now: 100,
    }
}

// H1.5 (§13::89, §13::90): with the store unusable the hold mode is read from the state as it
// stands: Strict when the user chose it, Balanced and Relaxed when chosen, and Strict (the
// stricter) when no state can be read at all. A damaged state file leaves the state before the
// last change.
#[test]
fn the_hold_mode_is_known_from_a_store_that_cannot_be_locked() {
    let dir = scratch("mode");
    let env = env_in(&dir);
    let store = Store::<Mail>::open(&env.store_dir());
    assert!(store.is_ok());
    let Ok(store) = store else { return };
    for mode in [HoldMode::Strict, HoldMode::Balanced, HoldMode::Relaxed] {
        assert!(store.update(|m| m.set_hold_mode(mode)).is_ok());
        assert_eq!(known_hold_mode(&env), mode);
    }
    // Damaged state file, with a previous state that reads (Strict was kept before the last one).
    assert!(store.update(|m| m.set_hold_mode(HoldMode::Strict)).is_ok());
    assert!(store.update(|m| m.set_hold_mode(HoldMode::Relaxed)).is_ok());
    assert!(std::fs::write(env.store_dir().join("state.json"), b"{ not json").is_ok());
    assert_eq!(known_hold_mode(&env), HoldMode::Strict);
    // Nothing readable: Balanced applies until the user chooses (P1.6), and `lunchroom send`
    // DENIES the reply that passes.
    assert!(std::fs::write(env.store_dir().join("state.json.prev"), b"{ not json").is_ok());
    assert_eq!(known_hold_mode(&env), HoldMode::Balanced);
    let _ = std::fs::remove_dir_all(&dir);
}

// H1.5: a Balanced reply passes when the guard cannot use its store, and a Strict one does not.
#[test]
fn a_reply_with_an_unusable_store_passes_in_balanced_and_not_in_strict() {
    for (mode, passes) in [
        (HoldMode::Balanced, true),
        (HoldMode::Relaxed, true),
        (HoldMode::Strict, false),
    ] {
        let dir = scratch(&format!("reply-{mode:?}"));
        let env = env_in(&dir);
        let store = Store::<Mail>::open(&env.store_dir());
        assert!(store.is_ok_and(|s| s.update(|m| m.set_hold_mode(mode)).is_ok()));
        let program = std::env::current_exe()
            .unwrap_or_default()
            .display()
            .to_string();
        let raw = json!({
            "session_id": "s1", "cwd": dir.join("home"), "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_input": { "command": format!("{program} send bob hello") },
            "tool_use_id": "t1",
        })
        .to_string();
        // The store the hook failed on is the one in this environment's own folder.
        let out = failed_call_in(&raw, "Lunchroom's store cannot be opened: x", &env);
        assert_eq!(
            matches!(out, Some(HookOut::Silent)),
            passes,
            "{mode:?}: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// L1.1: a call's paths are resolved on parallel threads and the second judging reads the result;
// a path the deadline cut off is unresolved and the view says so, for that path only.
#[test]
fn paths_are_resolved_before_the_second_judging() {
    use std::time::{Duration, Instant};
    let dir = scratch("parallel");
    let file = dir.join("a.txt");
    assert!(std::fs::write(&file, b"x").is_ok());
    let look = parallel::Look::new(Instant::now() + BUDGET);
    let mut carried = parallel::Carried::new();
    let asked = file.clone();
    let view = parallel::Parallel::after(&look, &mut carried, |fs| {
        let _ = fs.resolve(&asked);
        let _ = fs.resolve(&asked.with_file_name("b.txt"));
    });
    assert_eq!(view.counts(), (2, 0));
    assert!(!view.cut(&file));
    assert!(view.resolve(&file).complete);
    // A path asked about only by the second judging is resolved at that moment.
    assert!(view.resolve(&dir).complete);
    // The decision's 8 seconds already spent (Fable M9b: counted from where the decision began):
    // nothing is resolved, and the view says so for the path asked about, not for another.
    let spent = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let late_look = parallel::Look::new(spent);
    let late = parallel::Parallel::after(&late_look, &mut parallel::Carried::new(), |fs| {
        let _ = fs.resolve(&file);
    });
    assert_eq!(late.counts(), (0, 1));
    assert!(!late.resolve(&file).complete);
    assert!(late.cut(&file));
    assert!(!late.cut(&dir), "the cut is per path");
    // The deciding thread's own looks are bounded too: past the deadline `real` and `is_dir` are
    // answered cautiously and noted, without a call to the file system.
    assert_eq!(late.real(&dir), dir);
    assert!(late.is_dir(&file));
    assert!(late.cut(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real file system, except that a look at one path does not end (a stalled mount) until the
/// test drops the sender.
struct Stalling {
    inner: OsFs,
    stuck: PathBuf,
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl lunchroom_core::fs::Fs for Stalling {
    fn real(&self, p: &Path) -> PathBuf {
        self.inner.real(p)
    }
    fn node(&self, p: &Path) -> lunchroom_core::fs::Node {
        if p == self.stuck
            && let Ok(gate) = self.gate.lock()
        {
            let _ = gate.recv();
        }
        self.inner.node(p)
    }
    fn out_of_time(&self) -> bool {
        self.inner.out_of_time()
    }
    fn note_cut(&self, p: &Path) {
        self.inner.note_cut(p);
    }
    fn cut(&self, p: &Path) -> bool {
        self.inner.cut(p)
    }
    fn is_dir(&self, p: &Path) -> bool {
        self.inner.is_dir(p)
    }
    fn exists(&self, p: &Path) -> bool {
        self.inner.exists(p)
    }
    fn expand(&self, pattern: &Path) -> lunchroom_core::fs::Expansion {
        self.inner.expand(pattern)
    }
    fn walk(
        &self,
        dir: &Path,
        visit: &mut dyn FnMut(&Path) -> bool,
    ) -> lunchroom_core::fs::WalkEnd {
        self.inner.walk(dir, visit)
    }
    fn script(&self, p: &Path) -> lunchroom_core::fs::Script {
        self.inner.script(p)
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.inner.runs_as_script(p)
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.inner.which(prog)
    }
}

// L1.1 (Codex U4 findings 1-3): through the production file-system view and the production
// deadline, a HELD session's read of the one path whose look the deadline cut (a stalled mount) is
// PROTECTED, a read of a plain file that resolved is in the slip, and a read of a link loop, which
// resolved to "cannot be followed", is DENIED, whatever the order of the call's paths and
// although the deadline has passed by the time the second judging asks.
#[test]
fn only_the_path_whose_look_was_cut_is_protected_and_a_loop_is_still_denied() {
    use lunchroom_core::policy::{CallClass, DenyReason, ProtectedKind};
    use std::time::{Duration, Instant};
    let dir = scratch("production-cut");
    let project = dir.join("home/project");
    assert!(std::fs::create_dir_all(&project).is_ok());
    assert!(std::fs::write(project.join("a.txt"), b"x").is_ok());
    assert!(std::os::unix::fs::symlink(project.join("lb"), project.join("la")).is_ok());
    assert!(std::os::unix::fs::symlink(project.join("la"), project.join("lb")).is_ok());
    let layout = env_in(&dir).layout();
    let (plain, looped, stalled) = ("a.txt", "la", "stalled");
    for order in [
        [plain, looped, stalled],
        [stalled, looped, plain],
        [looped, stalled, plain],
    ] {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(700);
        let fs = std::sync::Arc::new(Stalling {
            inner: OsFs::until(deadline),
            stuck: project.join(stalled),
            gate: std::sync::Mutex::new(rx),
        });
        let look = parallel::Look::over(fs, deadline, lunchroom_core::fs::Threads::new(8));
        let judge = |fs: &dyn lunchroom_core::fs::Fs, name: &str| {
            let guard = Guard {
                layout: &layout,
                fs,
                held: true,
            };
            classify(
                &guard,
                "Read",
                &json!({ "file_path": project.join(name) }),
                &project,
            )
            .0
        };
        let view = parallel::Parallel::after(&look, &mut parallel::Carried::new(), |fs| {
            for name in order {
                let _ = judge(fs, name);
            }
        });
        assert!(Instant::now() >= deadline, "the deadline has passed");
        assert_eq!(judge(&view, plain), CallClass::InSlip, "{order:?}");
        assert_eq!(
            judge(&view, looped),
            CallClass::Denied(DenyReason::DeniedPath),
            "{order:?}"
        );
        assert_eq!(
            judge(&view, stalled),
            CallClass::Protected(ProtectedKind::Credential),
            "{order:?}"
        );
        drop(tx);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// Fable M9c: the T1.8 re-judging reads what the first judging found; it does not meet a deadline
// the first judging spent. Without the carried answers the same path is cut.
#[test]
fn the_second_judging_of_a_call_reuses_the_first_ones_answers() {
    use std::time::{Duration, Instant};
    let dir = scratch("carried");
    let file = dir.join("a.txt");
    assert!(std::fs::write(&file, b"x").is_ok());
    let look = parallel::Look::new(Instant::now() + Duration::from_millis(400));
    let mut carried = parallel::Carried::new();
    let first = parallel::Parallel::after(&look, &mut carried, |fs| {
        let _ = fs.resolve(&file);
    });
    assert_eq!(first.counts(), (1, 0));
    std::thread::sleep(Duration::from_millis(500)); // the deadline passes
    let again = parallel::Parallel::after(&look, &mut carried, |fs| {
        let _ = fs.resolve(&file);
    });
    assert_eq!(again.counts(), (1, 0));
    assert!(again.resolve(&file).complete);
    assert!(!again.cut(&file));
    let fresh = parallel::Parallel::after(&look, &mut parallel::Carried::new(), |fs| {
        let _ = fs.resolve(&file);
    });
    assert_eq!(fresh.counts(), (0, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Hermes `terminal` command judged for a session that is HELD or not, with the plugin having read
/// its folder or not (`workdir_unknown`).
fn hermes_call(command: &str, held: bool, unknown: bool) -> CallClass {
    // The command's folder holds none of Lunchroom's own paths, as a developer's project does not.
    let root = scratch("folder");
    let dir = root.join("home/project");
    let _ = std::fs::create_dir_all(&dir);
    let env = env_in(&root);
    let layout = env.layout();
    let fs = OsFs::new(BUDGET);
    let guard = Guard {
        layout: &layout,
        fs: &fs,
        held,
    };
    let input = if unknown {
        json!({ "command": command, "workdir_unknown": true })
    } else {
        json!({ "command": command, "workdir": dir })
    };
    let (judged_tool, judged_input) = as_claude_call(Harness::Hermes, "terminal", &input);
    let (call, _, _) = classify(&guard, &judged_tool, &judged_input, &dir);
    in_a_folder_the_plugin_could_not_read(
        call,
        held,
        Harness::Hermes,
        "terminal",
        &input,
        &judged_input,
    )
}

const FOLDER_UNREAD: CallClass = CallClass::Protected(ProtectedKind::GuardConfig);

// §11::15, §11::16: a HELD Hermes agent's command with its folder unread is PROTECTED when it names
// a relative path, or runs with no path named a program that reads the folder it runs in; not so
// for a session that is not HELD.
#[test]
fn a_held_commands_unread_folder_protects_only_what_reads_it() {
    for command in [
        "cat ./notes.txt",
        "rg secret",
        "cat notes.txt",
        "cat ../up.txt",
    ] {
        assert_eq!(hermes_call(command, true, true), FOLDER_UNREAD, "{command}");
        assert_ne!(
            hermes_call(command, false, true),
            FOLDER_UNREAD,
            "{command}"
        );
    }
    // The programs of the guard's table, with no file named.
    for command in [
        "grep -r token",
        "find",
        "ls",
        "du",
        "tree",
        "git status",
        "make",
        "cargo build",
        "npm test",
        "pnpm install",
        "yarn",
        "pip list",
        "python -m pytest",
        "pytest",
        "go test",
    ] {
        assert_eq!(hermes_call(command, true, true), FOLDER_UNREAD, "{command}");
    }
}

// §11::17: any other command is judged as it would be with a folder.
#[test]
fn a_held_commands_unread_folder_changes_nothing_for_other_commands() {
    for command in ["cat /tmp/x", "date", "echo hi", "ls /tmp"] {
        assert_eq!(
            hermes_call(command, true, true),
            hermes_call(command, true, false),
            "{command}"
        );
        assert_ne!(hermes_call(command, true, true), FOLDER_UNREAD, "{command}");
    }
}

// §5::43-44 etc. are end to end in `tests/u5.rs`; this is the last unit test.
// §17.5: the numbers of the system user and group `lunchroom`, once each, none where absent.
#[test]
fn the_numeric_ids_of_lunchroom_come_from_the_account_files() {
    let passwd =
        "root:x:0:0::/root:/bin/sh\nlunchroom:x:998:997::/var/lib/lunchroom:/usr/sbin/nologin\n";
    let group = "root:x:0:\nlunchroom:x:997:\n";
    assert_eq!(crate::home::lunchroom_ids(passwd, group), vec![998, 997]);
    assert_eq!(crate::home::lunchroom_ids(passwd, "root:x:0:\n"), vec![998]);
    // The same number for both is listed once.
    assert_eq!(
        crate::home::lunchroom_ids("lunchroom:x:5:5::/h:/s\n", "lunchroom:x:5:\n"),
        vec![5]
    );
    assert!(crate::home::lunchroom_ids("", "").is_empty());
}

// A1.3 "keeps the answer": the answer is written to the store in its folder, under its lock, and
// replaces the one before; an unwritable folder is an error, not silence.
#[test]
fn the_sudo_answer_is_kept_in_the_store() {
    use lunchroom_core::sudoers::Kept;
    let dir = scratch("keep");
    let folder = dir.join("store");
    let kept = Kept {
        read_at: 7,
        own: "(root) NOPASSWD: ALL".to_owned(),
        others: std::collections::BTreeMap::new(),
    };
    assert!(crate::terminal::keep_answer_in(&folder, &kept).is_ok());
    let read = Store::<Mail>::read_without_change(&folder).ok();
    assert_eq!(read.and_then(|m| m.sudo_answer()), Some(kept.clone()));
    let next = Kept {
        read_at: 8,
        own: String::new(),
        ..kept.clone()
    };
    assert!(crate::terminal::keep_answer_in(&folder, &next).is_ok());
    let read = Store::<Mail>::read_without_change(&folder).ok();
    assert_eq!(read.and_then(|m| m.sudo_answer()), Some(next));
    // A path that is a file cannot be a store folder.
    let file = dir.join("file");
    assert!(std::fs::write(&file, b"x").is_ok());
    assert!(crate::terminal::keep_answer_in(&file.join("store"), &kept).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
