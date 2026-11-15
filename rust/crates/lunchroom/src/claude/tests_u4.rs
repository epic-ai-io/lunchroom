//! Lunchroom 1.1, unit U4 (spec §11 L1.1): every kind of look, the path spellings, the empty and
//! missing inputs and each harness, driven through the production `Parallel`, `Guard` and
//! `decide_with`, with a file system in which one look stalls (a stalled mount) or is slow.

// A test of a list that is empty says so by `is_empty`, as the clause does.
#![allow(clippy::assert_is_empty, clippy::expect_used)]

use super::*;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lunchroom_core::fs::{Deadlined, Expansion, Fs, Node, OsFs, Script, Threads, WalkEnd};
use lunchroom_core::mail::Mail;
use lunchroom_core::policy::{CallClass, DenyReason, ProtectedKind};

/// Which look of the real file system stalls (or is slow).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    Node,
    Real,
    IsDir,
    Exists,
    Script,
    RunsAsScript,
    Which,
    Walk,
    Nothing,
}

/// The real file system, except that the look `at` of the path `stuck` waits for the test to drop
/// the sender (or, with `slow`, takes that long), and `which` searches one folder, `bin`.
struct Stall {
    inner: OsFs,
    at: At,
    stuck: PathBuf,
    slow: Option<Duration>,
    /// Every path below `stuck` stalls, not only `stuck`.
    below: bool,
    /// Looks stalled at this moment, and the most there were at one moment.
    waiting: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    bin: PathBuf,
    gate: Mutex<Receiver<()>>,
}

impl Stall {
    fn new(deadline: Instant, at: At, stuck: &Path, bin: &Path) -> (Arc<Self>, Sender<()>) {
        let (tx, rx) = channel();
        let fs = Self {
            inner: OsFs::until(deadline),
            at,
            stuck: stuck.to_path_buf(),
            slow: None,
            below: false,
            waiting: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
            bin: bin.to_path_buf(),
            gate: Mutex::new(rx),
        };
        (Arc::new(fs), tx)
    }

    /// As [`Stall::new`], with every path below `stuck` stalling.
    fn flooding(deadline: Instant, at: At, stuck: &Path, bin: &Path) -> (Arc<Self>, Sender<()>) {
        let (fs, tx) = Self::new(deadline, at, stuck, bin);
        let mut fs = fs;
        if let Some(f) = Arc::get_mut(&mut fs) {
            f.below = true;
        }
        (fs, tx)
    }

    fn wait(&self, at: At, p: &Path) {
        if self.at == at && (p == self.stuck || (self.below && p.starts_with(&self.stuck))) {
            use std::sync::atomic::Ordering::SeqCst;
            let now = self.waiting.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            match self.slow {
                Some(d) => std::thread::sleep(d),
                None => {
                    if let Ok(gate) = self.gate.lock() {
                        let _ = gate.recv();
                    }
                }
            }
            self.waiting.fetch_sub(1, SeqCst);
        }
    }
}

impl Fs for Stall {
    fn real(&self, p: &Path) -> PathBuf {
        self.wait(At::Real, p);
        self.inner.real(p)
    }
    fn node(&self, p: &Path) -> Node {
        self.wait(At::Node, p);
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
        self.wait(At::IsDir, p);
        self.inner.is_dir(p)
    }
    fn exists(&self, p: &Path) -> bool {
        self.wait(At::Exists, p);
        self.inner.exists(p)
    }
    fn expand(&self, pattern: &Path) -> Expansion {
        self.inner.expand(pattern)
    }
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        self.wait(At::Walk, dir);
        self.inner.walk(dir, visit)
    }
    fn script(&self, p: &Path) -> Script {
        self.wait(At::Script, p);
        self.inner.script(p)
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.wait(At::RunsAsScript, p);
        self.inner.runs_as_script(p)
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.wait(At::Which, Path::new(prog));
        let found = self.bin.join(prog);
        found.is_file().then_some(found)
    }
}

/// A call's class and the reason given when it is refused.
type Judged = (CallClass, Option<String>);

/// A scratch home with a project: a plain file, a folder, a script, a program in `bin`.
struct World {
    dir: PathBuf,
    project: PathBuf,
    layout: lunchroom_core::paths::Layout,
}

fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("claude-u4")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
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

impl World {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch(name);
        let project = dir.join("home/project");
        for d in ["sub", "bin"] {
            assert!(std::fs::create_dir_all(project.join(d)).is_ok());
        }
        assert!(std::fs::write(project.join("a.txt"), b"x").is_ok());
        assert!(std::fs::write(project.join("target"), b"x").is_ok());
        assert!(std::fs::write(project.join("sub/f.txt"), b"x").is_ok());
        for script in ["prog.sh", "bin/tool"] {
            let path = project.join(script);
            assert!(std::fs::write(&path, b"#!/bin/sh\necho hi\n").is_ok());
            assert!(
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).is_ok()
            );
        }
        let layout = env_in(&dir).layout();
        Self {
            dir,
            project,
            layout,
        }
    }

    fn bin(&self) -> PathBuf {
        self.project.join("bin")
    }

    /// The class of a call by a HELD session judged three ways, with `at` of `stuck` stalled and a
    /// 600 ms deadline: through the first and second judging of the guard's own two passes, and
    /// again from the answers they kept (the T1.8 re-judging). Returns the second and the third
    /// class and the time all of it took.
    fn judged(
        &self,
        at: At,
        stuck: &Path,
        tool: &str,
        input: &Value,
    ) -> (Judged, Judged, Duration) {
        // The looks made one at a time have 2 s, the parallel looks 600 ms: what the cut of the
        // one stalled look does is then told from what the time does to every look after it.
        let deadline = Instant::now() + Duration::from_secs(2);
        let (fs, tx) = Stall::new(deadline, at, stuck, &self.bin());
        let look = parallel::Look::over(fs, deadline, Threads::new(8))
            .pool_until(Instant::now() + Duration::from_millis(600));
        let class_of = |fs: &dyn Fs| {
            let guard = Guard {
                layout: &self.layout,
                fs,
                held: true,
            };
            let (class, fault, _) = classify(&guard, tool, input, &self.project);
            (class, fault)
        };
        let start = Instant::now();
        let mut carried = parallel::Carried::new();
        let view = parallel::Parallel::after(&look, &mut carried, class_of);
        let second = class_of(&view);
        let view_again = parallel::Parallel::after(&look, &mut carried, class_of);
        let third = class_of(&view_again);
        let spent = start.elapsed();
        drop(tx);
        (second, third, spent)
    }
}

// L1.1 (Codex U4 judge, class 1): each kind of look the guard makes (the resolution of a path, its
// real path, folder, existence, script text, the search for a program, a walk) is routed through
// `Parallel` and `Guard`, in the first and second judging and in the re-judging from the kept
// answers. With the one look stalled, the cut answer reaches the decision (the call is not
// ordinary, and a read or search is PROTECTED) and the decision is made by the deadline. With
// nothing stalled the same call is judged as it would be.
#[test]
#[allow(clippy::too_many_lines)] // one table of cases, one loop
fn a_stalled_look_of_each_kind_reaches_the_decision_through_the_guard() {
    let w = World::new("kinds");
    let p = &w.project;
    assert!(std::fs::create_dir_all(p.join("cred")).is_ok());
    assert!(std::fs::write(p.join("cred/.env"), b"K=1").is_ok());
    let read = json!({ "file_path": p.join("a.txt") });
    let plain = json!({ "pattern": "x", "path": p.join("sub") });
    let creds = json!({ "pattern": "x", "path": p.join("cred") });
    let unparsed = CallClass::Denied(DenyReason::Unparsed);
    let credential = CallClass::Protected(ProtectedKind::Credential);
    // (the look that stalls, the path it stalls for, the call, the class with nothing stalled, the
    // class when the look is cut)
    let cases: Vec<(&str, At, PathBuf, &str, Value, CallClass, CallClass)> = vec![
        (
            "resolution",
            At::Node,
            p.join("a.txt"),
            "Read",
            read.clone(),
            CallClass::InSlip,
            credential,
        ),
        (
            "real path",
            At::Real,
            p.join("a.txt"),
            "Read",
            read,
            CallClass::InSlip,
            credential,
        ),
        // A path cut as a folder is searched as one: the credential inside it is found.
        (
            "is_dir",
            At::IsDir,
            p.join("cred"),
            "Grep",
            creds,
            credential,
            credential,
        ),
        (
            "walk",
            At::Walk,
            p.join("sub"),
            "Grep",
            plain,
            CallClass::InSlip,
            credential,
        ),
        (
            "exists",
            At::Exists,
            p.join("missing.sh"),
            "Bash",
            json!({ "command": "./missing.sh" }),
            unparsed,
            CallClass::Protected(ProtectedKind::GuardConfig),
        ),
        (
            "script",
            At::Script,
            p.join("prog.sh"),
            "Bash",
            json!({ "command": "sh prog.sh" }),
            CallClass::Ordinary,
            unparsed,
        ),
        (
            "runs_as_script",
            At::RunsAsScript,
            p.join("bin/tool"),
            "Bash",
            json!({ "command": "./bin/tool" }),
            CallClass::Ordinary,
            CallClass::Protected(ProtectedKind::GuardConfig),
        ),
        (
            "which",
            At::Which,
            PathBuf::from("tool"),
            "Bash",
            json!({ "command": "tool" }),
            CallClass::Ordinary,
            CallClass::Protected(ProtectedKind::GuardConfig),
        ),
    ];
    for (name, at, stuck, tool, input, control, cut) in cases {
        // Nothing stalled: the call is judged as the control says.
        let (base, base_again, took) = w.judged(At::Nothing, &stuck, tool, &input);
        assert_eq!(base.0, control, "{name}: control");
        assert_eq!(base_again.0, control, "{name}: control, kept answers");
        assert!(
            took < Duration::from_millis(500),
            "{name}: control took {took:?}"
        );
        // The look stalls: decided by the deadline, as the cut look is answered.
        let (second, third, took) = w.judged(at, &stuck, tool, &input);
        assert_eq!(
            second.0, cut,
            "{name}: the cut reached the second judging: {second:?}"
        );
        assert_eq!(
            third.0, cut,
            "{name}: the cut reached the re-judging: {third:?}"
        );
        assert!(
            took < Duration::from_millis(if name == "walk" { 3500 } else { 1500 }),
            "{name}: the deciding thread waited {took:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (class 3): the cut is known by the path the look was made for, in every spelling the
// classifier is given. A stalled file read by an absolute path, `./`, `../`, a repeated
// separator, a relative path or a symbolic link to its folder is PROTECTED each time (no spelling
// turns a cut path into an uncut one), while the same spellings of a file that answers are in the
// slip. A credential reached through a link is PROTECTED whether or not its look was cut.
#[test]
fn no_spelling_of_a_path_turns_a_cut_path_into_an_uncut_one() {
    let w = World::new("aliases");
    let p = &w.project;
    assert!(std::os::unix::fs::symlink(p, p.join("alias")).is_ok());
    assert!(std::fs::create_dir_all(w.dir.join("home/.ssh")).is_ok());
    let key = w.dir.join("home/.ssh/id_rsa");
    assert!(std::fs::write(&key, b"k").is_ok());
    assert!(std::os::unix::fs::symlink(&key, p.join("key")).is_ok());
    let spell = |name: &str| -> Vec<String> {
        let a = p.display().to_string();
        vec![
            format!("{a}/{name}"),
            format!("{a}/./{name}"),
            format!("{a}/sub/../{name}"),
            format!("{a}//{name}"),
            format!("{a}/alias/{name}"),
            format!("{a}/alias/./sub/../{name}"),
            name.to_owned(),
            format!("./{name}"),
            format!("../project/{name}"),
        ]
    };
    for spelling in spell("target") {
        let (second, third, _) = w.judged(
            At::Node,
            &p.join("target"),
            "Read",
            &json!({ "file_path": spelling }),
        );
        let cut = CallClass::Protected(ProtectedKind::Credential);
        assert_eq!((&second.0, &third.0), (&cut, &cut), "{spelling}");
    }
    for spelling in spell("a.txt") {
        let (second, third, _) = w.judged(
            At::Node,
            &p.join("target"),
            "Read",
            &json!({ "file_path": spelling }),
        );
        assert_eq!(
            (&second.0, &third.0),
            (&CallClass::InSlip, &CallClass::InSlip),
            "{spelling}"
        );
    }
    // A credential through a link: PROTECTED with nothing stalled and with its own look stalled.
    for at in [At::Nothing, At::Node] {
        for spelling in [
            p.join("key").display().to_string(),
            "key".to_owned(),
            "./alias/key".to_owned(),
        ] {
            let (second, third, _) = w.judged(at, &key, "Read", &json!({ "file_path": spelling }));
            let cut = CallClass::Protected(ProtectedKind::Credential);
            assert_eq!((&second.0, &third.0), (&cut, &cut), "{at:?} {spelling}");
        }
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (class 3, C8): a part the guard has no permission to look at is DENIED to a read, and the
// time that ran out for another path of the same call does not change it; the stalled path is
// PROTECTED. (A link loop is the same class and is in `tests_u5`.)
#[test]
fn a_part_that_cannot_be_looked_at_stays_denied_beside_a_stalled_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let w = World::new("blind");
    let blind = w.project.join("blind");
    assert!(std::fs::create_dir_all(&blind).is_ok());
    assert!(std::fs::write(blind.join("x"), b"x").is_ok());
    assert!(std::fs::set_permissions(&blind, std::fs::Permissions::from_mode(0o000)).is_ok());
    // Running as root, a folder without permission is still searched: nothing to prove here.
    let looked = std::fs::symlink_metadata(blind.join("x")).is_ok();
    if !looked {
        let blind_x = json!({ "file_path": blind.join("x") });
        let (second, third, _) = w.judged(At::Node, &w.project.join("a.txt"), "Read", &blind_x);
        let denied = CallClass::Denied(DenyReason::DeniedPath);
        assert_eq!((&second.0, &third.0), (&denied, &denied));
    }
    let _ = std::fs::set_permissions(&blind, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (class 4): an empty path, an empty or missing command, a missing tool input, an absent
// working folder and the optional fields a harness leaves out are judged through the looks as they
// are judged one look after another: the same class, and no panic.
#[test]
fn empty_and_missing_inputs_are_judged_as_they_are_without_the_threads() {
    let w = World::new("empties");
    let cases: Vec<(Harness, &str, Value)> = vec![
        (Harness::ClaudeCode, "Read", json!({ "file_path": "" })),
        (Harness::ClaudeCode, "Read", json!({})),
        (Harness::ClaudeCode, "Bash", json!({ "command": "" })),
        (Harness::ClaudeCode, "Bash", json!({ "command": "   " })),
        (Harness::ClaudeCode, "Bash", json!({ "command": "''" })),
        (
            Harness::ClaudeCode,
            "Bash",
            json!({ "command": "\"\" \"\"" }),
        ),
        (Harness::ClaudeCode, "Bash", json!({})),
        (
            Harness::ClaudeCode,
            "Grep",
            json!({ "pattern": "x", "path": "" }),
        ),
        (Harness::ClaudeCode, "Grep", json!({})),
        (Harness::ClaudeCode, "Glob", json!({ "pattern": "" })),
        (Harness::ClaudeCode, "LS", json!({})),
        (Harness::ClaudeCode, "Write", json!({ "file_path": "" })),
        (Harness::Codex, "apply_patch", json!({ "command": "" })),
        (Harness::Codex, "Bash", json!({ "command": "which" })),
        (
            Harness::OpenClaw,
            "exec",
            json!({ "command": "", "workdir": "" }),
        ),
        (Harness::OpenClaw, "exec", json!({ "command": "ls" })),
        (Harness::OpenClaw, "read", json!({ "path": "" })),
        (Harness::OpenClaw, "read", json!({})),
        (Harness::OpenClaw, "apply_patch", json!({})),
        (
            Harness::Hermes,
            "terminal",
            json!({ "command": "ls", "workdir": "" }),
        ),
        (
            Harness::Hermes,
            "terminal",
            json!({ "command": "ls", "workdir_unknown": true }),
        ),
        (Harness::Hermes, "terminal", json!({})),
        (Harness::Hermes, "read_file", json!({})),
        (Harness::Hermes, "search_files", json!({ "path": "" })),
    ];
    for held in [true, false] {
        for (harness, tool, input) in &cases {
            let (judged_tool, judged_input) = as_claude_call(*harness, tool, input);
            let dir = execution_dir(*harness, tool, input, &w.project);
            let one_by_one = {
                let fs = OsFs::new(BUDGET);
                let guard = Guard {
                    layout: &w.layout,
                    fs: &fs,
                    held,
                };
                classify(&guard, &judged_tool, &judged_input, &dir).0
            };
            let look = parallel::Look::new(Instant::now() + BUDGET);
            let class_of = |fs: &dyn Fs| {
                let guard = Guard {
                    layout: &w.layout,
                    fs,
                    held,
                };
                classify(&guard, &judged_tool, &judged_input, &dir).0
            };
            let mut carried = parallel::Carried::new();
            let view = parallel::Parallel::after(&look, &mut carried, class_of);
            assert_eq!(
                class_of(&view),
                one_by_one,
                "{harness:?} {tool} {input} held={held}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// A hook input of each harness for a read of `file` (the shapes the plugins and hooks send).
fn read_input(harness: Harness, session: &str, project: &Path, file: &Path) -> HookInput {
    let mut v = match harness {
        Harness::ClaudeCode => json!({
            "tool_name": "Read", "tool_input": { "file_path": file },
        }),
        Harness::Codex => json!({
            "tool_name": "Bash", "tool_input": { "command": format!("cat {}", file.display()) },
            "turn_id": "turn-1",
            "transcript_path": project.join(format!("rollout-2026-11-06T00-00-00-{session}.jsonl")),
        }),
        Harness::OpenClaw => json!({
            "harness": "openclaw", "tool_name": "read", "tool_input": { "path": file },
        }),
        Harness::Hermes => json!({
            "harness": "hermes", "tool_name": "read_file", "tool_input": { "path": file },
        }),
    };
    if let Some(o) = v.as_object_mut() {
        o.insert("session_id".to_owned(), json!(session));
        o.insert("cwd".to_owned(), json!(project));
        o.insert("hook_event_name".to_owned(), json!("PreToolUse"));
        o.insert("tool_use_id".to_owned(), json!("t1"));
    }
    HookInput::parse(&v.to_string()).expect("a hook input")
}

fn runs(out: &HookOut) -> bool {
    matches!(out, HookOut::Silent | HookOut::Allow(_))
}

// L1.1 (class 5): through each harness's entry (Claude Code, Codex, OpenClaw, Hermes) the 8 seconds
// are counted from where the decision began, and a HELD session's read of a plain file that finds
// them spent goes to the user, where the same read with the time in hand runs.
#[test]
fn each_harness_finds_the_eight_seconds_counted_from_where_the_decision_began() {
    for harness in [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::OpenClaw,
        Harness::Hermes,
    ] {
        let w = World::new(&format!("spent-{harness:?}"));
        let env = env_in(&w.dir);
        let me = Identity::new(harness, "s1");
        let held = store(&env).and_then(|s| {
            s.update(|m| m.hold_unrecorded(&me, "test", "m1"))
                .map_err(|e| e.to_string())
        });
        assert!(held.is_ok(), "{harness:?}: {held:?}");
        let input = read_input(harness, "s1", &w.project, &w.project.join("a.txt"));
        let fresh = pretool_decide_from(&input, &env, Instant::now());
        assert!(
            fresh.as_ref().is_ok_and(runs),
            "{harness:?} with the time in hand: {fresh:?}"
        );
        let spent = Instant::now()
            .checked_sub(BUDGET + Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let late = pretool_decide_from(&input, &env, spent);
        assert!(
            late.as_ref().is_ok_and(|o| !runs(o)),
            "{harness:?} with it spent: {late:?}"
        );
        let _ = std::fs::remove_dir_all(&w.dir);
    }
}

// L1.1 + T1.8 (class 5): through each harness's entry, the second judging of a call (the session
// is HELD once it reads a path a HELD session wrote) reads the answers of the first, not a deadline
// the first one spent. The look at the file is slow enough that a second look would not finish.
#[test]
fn each_harness_re_judges_from_the_answers_the_first_judging_kept() {
    for harness in [
        Harness::ClaudeCode,
        Harness::Codex,
        Harness::OpenClaw,
        Harness::Hermes,
    ] {
        let w = World::new(&format!("carried-{harness:?}"));
        let env = env_in(&w.dir);
        let file = w.project.join("a.txt");
        let writer = Identity::new(Harness::ClaudeCode, "writer");
        let marked = store(&env).and_then(|s| {
            s.update(|m| {
                m.hold_unrecorded(&writer, "test", "m1");
                m.record_written(&writer, vec![file.display().to_string()], "c1", 100)
            })
            .map_err(|e| e.to_string())
        });
        assert_eq!(marked, Ok(true), "{harness:?}");
        let input = read_input(harness, "reader", &w.project, &file);
        let slow = |delay: Duration, ms: u64| {
            let deadline = Instant::now() + Duration::from_millis(ms);
            let (fs, tx) = Stall::new(deadline, At::Node, &file, &w.bin());
            let mut fs = fs;
            if let Some(f) = Arc::get_mut(&mut fs) {
                f.slow = Some(delay);
            }
            let look = parallel::Look::over(fs, deadline, Threads::new(8));
            drop(tx);
            look
        };
        // The look takes 300 ms and the time is 450 ms: one look fits, two do not.
        let out = decide_with(&input, &env, || slow(Duration::from_millis(300), 450));
        // The session read a marked path, so it was HELD and judged again: the second judging
        // meets the answers of the first and the read runs (it is in the slip).
        assert!(out.as_ref().is_ok_and(runs), "{harness:?}: {out:?}");
        let held = store(&env).and_then(|s| s.read().map_err(|e| e.to_string()));
        assert!(
            held.is_ok_and(|m| m.is_held(&Identity::new(harness, "reader"))),
            "{harness:?}: the re-judging happened"
        );
        let _ = std::fs::remove_dir_all(&w.dir);
    }
}

// Keeps `Mail` in use for the store helpers above.
const _: fn() = || {
    let _ = std::mem::size_of::<Mail>();
};

/// One kind of look, asked of the file system through the view, and the answer it gives.
type Ask = Box<dyn Fn(&dyn Fs) -> String>;

// L1.1 (class 1, at the level of the view): each kind of look is answered from the parallel looks
// as the file system answers it, and when its look is cut it is answered cautiously and its path is
// noted as one Lunchroom could not check. The kinds: resolution, real path, folder, existence,
// script text, whether a file runs as a script, and the search for a program.
#[test]
#[allow(clippy::too_many_lines)] // one table of cases, one loop
fn each_kind_of_look_is_answered_as_the_file_system_does_and_cautiously_when_cut() {
    let w = World::new("views");
    let p = &w.project;
    assert!(std::os::unix::fs::symlink(p.join("a.txt"), p.join("link")).is_ok());
    let cases: Vec<(&str, At, PathBuf, Ask, &str, &str)> = vec![
        (
            "resolution",
            At::Node,
            p.join("link"),
            Box::new({
                let p = p.clone();
                move |fs| {
                    let r = fs.resolve(&p.join("link"));
                    format!("{} {}", r.path == fs.real(&p.join("a.txt")), r.complete)
                }
            }),
            "true true",
            "false false",
        ),
        (
            "real path",
            At::Real,
            p.join("link"),
            Box::new({
                let p = p.clone();
                move |fs| (fs.real(&p.join("link")) == p.join("a.txt")).to_string()
            }),
            "true",
            "false",
        ),
        (
            "is_dir",
            At::IsDir,
            p.join("a.txt"),
            Box::new({
                let p = p.clone();
                move |fs| fs.is_dir(&p.join("a.txt")).to_string()
            }),
            "false",
            "true",
        ),
        (
            "exists",
            At::Exists,
            p.join("missing"),
            Box::new({
                let p = p.clone();
                move |fs| fs.exists(&p.join("missing")).to_string()
            }),
            "false",
            "true",
        ),
        (
            "script",
            At::Script,
            p.join("prog.sh"),
            Box::new({
                let p = p.clone();
                move |fs| format!("{:?}", fs.script(&p.join("prog.sh")))
            }),
            "Text(\"#!/bin/sh\\necho hi\\n\")",
            "Unreadable",
        ),
        (
            "runs_as_script",
            At::RunsAsScript,
            p.join("a.txt"),
            Box::new({
                let p = p.clone();
                move |fs| fs.runs_as_script(&p.join("a.txt")).to_string()
            }),
            "false",
            "true",
        ),
        (
            "which",
            At::Which,
            PathBuf::from("tool"),
            Box::new({
                let p = p.clone();
                move |fs| (fs.which("tool") == Some(p.join("bin/tool"))).to_string()
            }),
            "true",
            "false",
        ),
    ];
    for (name, at, stuck, ask, ok, cautious) in cases {
        for stalled in [false, true] {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (fs, tx) = Stall::new(
                deadline,
                if stalled { at } else { At::Nothing },
                &stuck,
                &w.bin(),
            );
            let look = parallel::Look::over(fs, deadline, Threads::new(8))
                .pool_until(Instant::now() + Duration::from_millis(500));
            let began = Instant::now();
            let view = parallel::Parallel::after(&look, &mut parallel::Carried::new(), |fs| {
                let _ = ask(fs);
            });
            let answer = ask(&view);
            assert!(
                began.elapsed() < Duration::from_millis(1500),
                "{name}: waited"
            );
            assert_eq!(
                answer,
                if stalled { cautious } else { ok },
                "{name} stalled={stalled}"
            );
            assert_eq!(view.cut(&stuck), stalled, "{name} stalled={stalled}: cut");
            drop(tx);
        }
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (class 1): a question the first pass did not ask is looked up when the second pass asks it,
// and answered as the file system answers it (a link loop cannot be followed, a link leads to its
// file), not as a path that is simply there.
#[test]
fn a_question_the_first_pass_did_not_ask_is_looked_up_as_it_is() {
    let w = World::new("unasked");
    let p = &w.project;
    assert!(std::os::unix::fs::symlink(p.join("lb"), p.join("la")).is_ok());
    assert!(std::os::unix::fs::symlink(p.join("la"), p.join("lb")).is_ok());
    assert!(std::os::unix::fs::symlink(p.join("a.txt"), p.join("link")).is_ok());
    let deadline = Instant::now() + Duration::from_secs(5);
    let (fs, tx) = Stall::new(deadline, At::Nothing, &p.join("none"), &w.bin());
    let look = parallel::Look::over(fs, deadline, Threads::new(8));
    let view = parallel::Parallel::after(&look, &mut parallel::Carried::new(), |_| {});
    assert!(
        !view.resolve(&p.join("la")).complete,
        "a link loop cannot be followed"
    );
    assert!(view.resolve(&p.join("a.txt")).complete);
    assert_eq!(
        view.resolve(&p.join("link")).path,
        view.real(&p.join("a.txt"))
    );
    assert!(!view.cut(&p.join("la")));
    drop(tx);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// The real file system, counting every look it is asked, each taking `slow`.
struct Counted {
    inner: OsFs,
    bin: PathBuf,
    slow: Duration,
    counts: Mutex<std::collections::BTreeMap<&'static str, usize>>,
}

impl Counted {
    fn new(deadline: Instant, bin: &Path, slow: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: OsFs::until(deadline),
            bin: bin.to_path_buf(),
            slow,
            counts: Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    fn hit(&self, what: &'static str) {
        if let Ok(mut counts) = self.counts.lock() {
            *counts.entry(what).or_insert(0) += 1;
        }
        if !self.slow.is_zero() {
            std::thread::sleep(self.slow);
        }
    }

    fn total(&self) -> usize {
        self.counts.lock().map_or(0, |c| c.values().sum())
    }
}

impl Fs for Counted {
    fn real(&self, p: &Path) -> PathBuf {
        self.hit("real");
        self.inner.real(p)
    }
    fn node(&self, p: &Path) -> Node {
        self.hit("node");
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
        self.hit("is_dir");
        self.inner.is_dir(p)
    }
    fn exists(&self, p: &Path) -> bool {
        self.hit("exists");
        self.inner.exists(p)
    }
    fn expand(&self, pattern: &Path) -> Expansion {
        self.hit("expand");
        self.inner.expand(pattern)
    }
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        self.hit("walk");
        self.inner.walk(dir, visit)
    }
    fn script(&self, p: &Path) -> Script {
        self.hit("script");
        self.inner.script(p)
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.hit("runs_as_script");
        self.inner.runs_as_script(p)
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.hit("which");
        let found = self.bin.join(prog);
        found.is_file().then_some(found)
    }
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        self.hit("file_id");
        self.inner.file_id(p)
    }
}

// L1.1 + T1.8 (judge, class 3): a look only the second judging asked (the first pass listed none of
// them) is kept for the re-judging, so the re-judging makes no look again: resolution, real path,
// folder, existence, script text, runs-as-script, the search for a program, a wildcard match and a
// walk, with a non-ASCII name and a name that is not text among them.
#[test]
fn answers_only_the_second_judging_found_are_kept_for_the_rejudging() {
    use std::os::unix::ffi::OsStrExt as _;
    let w = World::new("kept");
    let p = &w.project;
    let non_ascii = p.join("ünï-文件.txt");
    let not_text = p.join(std::ffi::OsStr::from_bytes(b"bad-\xff.txt"));
    for f in [&non_ascii, &not_text] {
        assert!(std::fs::write(f, b"x").is_ok());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let fs = Counted::new(deadline, &w.bin(), Duration::ZERO);
    let look = parallel::Look::over(fs.clone(), deadline, Threads::new(8));
    let ask_all = |view: &dyn Fs| -> String {
        let mut walked = Vec::new();
        let end = view.walk(&p.join("sub"), &mut |f| {
            walked.push(f.to_path_buf());
            false
        });
        format!(
            "{:?} {:?} {} {} {:?} {} {:?} {:?} {:?} {:?} {:?} {} {}",
            view.resolve(&non_ascii),
            view.real(&not_text),
            view.is_dir(&p.join("sub")),
            view.exists(&non_ascii),
            view.script(&p.join("prog.sh")),
            view.runs_as_script(&p.join("prog.sh")),
            view.which("tool"),
            view.expand(&p.join("sub/*.txt")),
            walked,
            end,
            view.resolve(&not_text),
            view.exists(&not_text),
            view.is_dir(&non_ascii),
        )
    };
    let mut carried = parallel::Carried::new();
    let first = parallel::Parallel::after(&look, &mut carried, |_| {});
    let before = ask_all(&first);
    first.keep(&mut carried);
    let looked = fs.total();
    assert!(looked > 0, "the second judging looked");
    let again = parallel::Parallel::after(&look, &mut carried, |_| {});
    let after = ask_all(&again);
    assert_eq!(before, after, "the same answers");
    assert_eq!(fs.total(), looked, "the re-judging looked again");
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (judge, class 4): the looks are keyed by the path as spelled, so spellings of one path are
// separate looks. That cannot exceed the time or the 16 threads: forty paths, every one of them
// stalled, held by one call's words, keep no more than 16 looks stalled at once, and the decision
// is made by the deadline and goes to the user.
#[test]
fn forty_stalled_paths_in_one_call_stay_inside_the_cap_and_the_time() {
    let w = World::new("flood");
    let p = &w.project;
    assert!(std::fs::create_dir_all(p.join("flood")).is_ok());
    let words: Vec<String> = (0..40)
        .map(|n| format!("{}/./flood/{}f{n}", p.display(), "./".repeat(n % 3)))
        .collect();
    let command = format!("cat {}", words.join(" "));
    let deadline = Instant::now() + Duration::from_secs(2);
    let (fs, tx) = Stall::flooding(deadline, At::Node, &p.join("flood"), &w.bin());
    let threads = Threads::new(16);
    let look = parallel::Look::over(fs.clone(), deadline, threads.clone())
        .pool_until(Instant::now() + Duration::from_millis(600));
    let class_of = |view: &dyn Fs| {
        let guard = Guard {
            layout: &w.layout,
            fs: view,
            held: true,
        };
        classify(&guard, "Bash", &json!({ "command": command }), p).0
    };
    let began = Instant::now();
    let view = parallel::Parallel::after(&look, &mut parallel::Carried::new(), class_of);
    let class = class_of(&view);
    assert!(
        began.elapsed() < Duration::from_millis(1500),
        "{:?}",
        began.elapsed()
    );
    assert!(matches!(class, CallClass::Protected(_)), "{class:?}");
    assert!(fs.peak.load(std::sync::atomic::Ordering::SeqCst) <= 16);
    assert!(threads.in_use() <= 16);
    drop(tx);
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (judge, class 5): a look of the first pass's list is made through the bounded view. It is
// not started when the deadline has passed (the file system is not asked), and an answer that comes
// after the deadline is none, for every kind of look.
#[test]
fn a_look_of_the_list_is_not_started_late_and_a_late_answer_is_none() {
    let kinds = [
        parallel::Kind::Real,
        parallel::Kind::IsDir,
        parallel::Kind::Exists,
        parallel::Kind::Script,
        parallel::Kind::RunsAsScript,
        parallel::Kind::Which,
        parallel::Kind::FileId,
    ];
    let w = World::new("late");
    let target = w.project.join("prog.sh");
    let named = |kind| {
        if kind == parallel::Kind::Which {
            PathBuf::from("tool")
        } else {
            target.clone()
        }
    };
    for kind in kinds {
        // The deadline has passed: nothing is asked.
        let fs = Counted::new(
            Instant::now() + Duration::from_secs(5),
            &w.bin(),
            Duration::ZERO,
        );
        let past = Instant::now();
        let bounded = Deadlined::with_threads(fs.clone(), past, Threads::new(4));
        assert!(
            parallel::run_look(&fs, &bounded, kind, &named(kind), past).is_none(),
            "{kind:?}"
        );
        assert_eq!(
            fs.total(),
            0,
            "{kind:?}: a look was started after the deadline"
        );
        // A look that starts in time and ends after it: no answer.
        let fs = Counted::new(
            Instant::now() + Duration::from_secs(5),
            &w.bin(),
            Duration::from_millis(250),
        );
        let soon = Instant::now() + Duration::from_millis(100);
        let bounded = Deadlined::with_threads(fs.clone(), soon, Threads::new(4));
        assert!(
            parallel::run_look(&fs, &bounded, kind, &named(kind), soon).is_none(),
            "{kind:?}: late"
        );
        assert_eq!(fs.total(), 1, "{kind:?}: the look was made");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

// L1.1 (judge, class 6): the search for a program is made for the name as it is. A name that is not
// text (it cannot have come from a command) is not looked for under the name its lossy form would
// give: the look is cut. A name that is text is found.
#[test]
fn a_program_name_is_not_made_lossy() {
    use std::os::unix::ffi::OsStrExt as _;
    let w = World::new("names");
    // A program whose name is what a lossy conversion of the bytes below would make.
    let lossy = w.bin().join("tool\u{FFFD}");
    assert!(std::fs::write(&lossy, b"#!/bin/sh\n").is_ok());
    let deadline = Instant::now() + Duration::from_secs(5);
    let fs = Counted::new(deadline, &w.bin(), Duration::ZERO);
    let bounded = Deadlined::with_threads(fs.clone(), deadline, Threads::new(4));
    let not_text = PathBuf::from(std::ffi::OsStr::from_bytes(b"tool\xff"));
    assert!(
        parallel::run_look(&fs, &bounded, parallel::Kind::Which, &not_text, deadline).is_none()
    );
    let found = parallel::run_look(
        &fs,
        &bounded,
        parallel::Kind::Which,
        Path::new("tool"),
        deadline,
    );
    assert!(
        matches!(found, Some(parallel::Answer::Found(Some(_)))),
        "{found:?}"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}
