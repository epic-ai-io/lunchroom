//! What the integration tests share: a scratch home and Lunchroom folder, and the `lunchroom`
//! binary run in it with an empty environment. Each test file wraps `Sandbox` for its own setup.

// Each test file uses only part of this module.
#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use lunchroom_core::mail::Mail;
use lunchroom_core::policy::HoldMode;
use lunchroom_core::store::Store;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The path of the test process's commands unless a test sets its own.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

pub struct Run {
    pub stdout: String,
    pub stderr: String,
    /// The exit code, with Claude Code's block form read as the block it is: V1.4 (2026-11-03)
    /// gives a DENIED tool call to Claude Code as a `permissionDecision` of `deny` (exit 0), whose
    /// reason the agent reads, and as a `systemMessage`, which the user reads; such a run counts as
    /// exit 2 with that reason on standard error, so a test of a decision reads one form. The
    /// process's own exit code is `raw_code`, and the tests of the form itself read it.
    pub code: Option<i32>,
    pub raw_code: Option<i32>,
}

impl Run {
    /// A command that could not be run or waited for.
    pub fn failed(why: &str) -> Self {
        Self {
            stdout: String::new(),
            stderr: why.to_owned(),
            code: None,
            raw_code: None,
        }
    }
}

impl From<Output> for Run {
    fn from(out: Output) -> Self {
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let raw_code = out.status.code();
        let mut code = raw_code;
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_default();
        let specific = &v["hookSpecificOutput"];
        if raw_code == Some(0) && specific["permissionDecision"] == "deny" {
            code = Some(2);
            let reason = specific["permissionDecisionReason"]
                .as_str()
                .unwrap_or_default();
            stderr = format!("{reason}{stderr}");
        }
        Self {
            stdout,
            stderr,
            code,
            raw_code,
        }
    }
}

/// Runs `cmd` with `stdin` written to it and its output captured.
pub fn finish(cmd: Command, stdin: &str) -> Run {
    finish_bytes(cmd, stdin.as_bytes())
}

/// As [`finish`], with input that need not be text.
pub fn finish_bytes(mut cmd: Command, stdin: &[u8]) -> Run {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        return Run::failed("spawn failed");
    };
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(stdin);
    }
    child
        .wait_with_output()
        .map_or_else(|_| Run::failed("wait failed"), Run::from)
}

/// A child shell started in a process group of its own, which is killed with all that the shell
/// started (a `sleep`, a `flock`'s command, a Lunchroom process) when the guard is dropped, however
/// the test ends: a failed assertion unwinds through the drop. The workspace forbids `unsafe`, so
/// the group is killed by the system's `kill -KILL -- -<pgid>` (the group's id is the child's).
/// A child already waited for is killed again harmlessly: the signal finds no such group.
pub struct GroupChild {
    child: Child,
}

impl GroupChild {
    /// Starts `cmd` as the leader of a new process group.
    pub fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        cmd.process_group(0);
        cmd.spawn().map(|child| Self { child })
    }

    /// Closes the child's input, reads its output to the end and waits for it, as
    /// [`Child::wait_with_output`] does; the group is killed afterwards, by the drop.
    pub fn into_output(mut self) -> std::io::Result<Output> {
        drop(self.child.stdin.take());
        let err = self.child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut err) = err {
                let _ = err.read_to_end(&mut bytes);
            }
            bytes
        });
        let mut stdout = Vec::new();
        if let Some(mut out) = self.child.stdout.take() {
            out.read_to_end(&mut stdout)?;
        }
        let status = self.child.wait()?;
        Ok(Output {
            status,
            stdout,
            stderr: reader.join().unwrap_or_default(),
        })
    }
}

impl std::ops::Deref for GroupChild {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for GroupChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", "--"])
            .arg(format!("-{}", self.child.id()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
    }
}

/// The processes whose command line holds `root` followed by a slash: what a test left running in
/// its sandbox (the id and the command line of each).
pub fn processes_holding(root: &Path) -> Vec<(u32, String)> {
    // The sandbox's path as the test spells it (it has `..` in it) and as a shell reports its
    // working directory.
    let mut needles = vec![format!("{}/", root.display())];
    if let Ok(real) = root.canonicalize() {
        needles.push(format!("{}/", real.display()));
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let line = std::fs::read(entry.path().join("cmdline")).ok()?;
            let line = String::from_utf8_lossy(&line).replace('\0', " ");
            needles
                .iter()
                .any(|needle| line.contains(needle))
                .then_some((pid, line))
        })
        .collect()
}

/// The full path of `program` as the test process's own `PATH` finds it: tests name a system
/// program (`flock`, `setsid`, `bash`) by what it is, not by where one distribution keeps it.
/// `None` when the system has no such program.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|file| file.is_file())
}

/// `program` as a command. The CI runner and a development machine have every program the tests
/// name; a system without one is a broken environment, so the test fails rather than passes.
pub fn require(program: &str) -> Command {
    let found = which(program);
    assert!(
        found.is_some(),
        "this test needs `{program}` on PATH, and the system has none"
    );
    Command::new(found.unwrap_or_default())
}

/// The release this test process declares when it stands in for Claude Code (`CLAUDE_PID` names
/// it): a `package.json` above the test binary (`target/release/deps/…`), as an npm install of
/// Claude Code has one above its program (H1.2). A file that holds anything else is replaced, by a
/// rename, so a test process running at the same time never reads half a file.
pub fn declare_test_release() {
    const RELEASE: &str = r#"{"name": "@anthropic-ai/claude-code", "version": "99.0.0"}"#;
    // Above this test binary, wherever the build folder is (`CARGO_TARGET_DIR`).
    let Some(file) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("package.json")))
    else {
        return;
    };
    if std::fs::read_to_string(&file).is_ok_and(|text| text == RELEASE) {
        return;
    }
    let temp = file.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&temp, RELEASE).is_ok() {
        let _ = std::fs::rename(&temp, &file);
    }
}

/// A scratch home and Lunchroom folder under `target/<dir>`, removed afterwards.
pub struct Sandbox {
    pub root: PathBuf,
    /// The `PATH` the commands run with.
    pub path: String,
}

impl Sandbox {
    pub fn new(dir: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(dir)
            .join(format!("{}-{n}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("home/project"));
        Self {
            root,
            path: SYSTEM_PATH.to_owned(),
        }
    }

    /// P1.6: sets the Strict hold mode, which the user chose here, for tests written for it.
    pub fn strict(self) -> Self {
        if let Some(store) = self.store() {
            let _ = store.update(|m| m.set_hold_mode(HoldMode::Strict));
        }
        self
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn lunchroom(&self) -> PathBuf {
        self.root.join("lunchroom")
    }

    pub fn store(&self) -> Option<Store<Mail>> {
        Store::open(&self.lunchroom().join("store")).ok()
    }

    /// `program` with an empty environment but this sandbox's home, folder and `PATH`.
    pub fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", self.home())
            .env("LUNCHROOM_HOME", self.lunchroom())
            .env("PATH", &self.path);
        cmd
    }

    pub fn run(&self, args: &[&str], stdin: &str, env: &[(&str, String)]) -> Run {
        self.run_bytes(args, stdin.as_bytes(), env)
    }

    /// As [`Sandbox::run`], with standard input that need not be text.
    pub fn run_bytes(&self, args: &[&str], stdin: &[u8], env: &[(&str, String)]) -> Run {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_lunchroom"));
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        finish_bytes(cmd, stdin)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
