//! `lunchroom install claude-code|codex`: installs the program and registers the guard's hooks.
//! `lunchroom install openclaw|hermes`: installs the program and writes Lunchroom's plugin for that
//! harness, which the user then enables in the harness (its trust records are the user's).
//!
//! Spec: §13 H1.4 (the installer writes each harness configuration beside the old one, keeps a
//! backup, tells the user any step only they can do, and never writes a harness's trust records),
//! H1.5 (the tool-call hook of Claude Code and of Codex is `<program> guard pretool || exit 2` with a
//! line on standard error first, and Claude Code's carries `"onFailure": "block"`, so a guard that
//! fails blocks that call; the prompt hook carries neither, so a failure never blocks the user's
//! typing), A1.3 (it changes the configuration that runs the guard, so it runs only on the user's
//! confirmation from a terminal no guarded session owns: the user's passphrase, or, for the install
//! that sets the first one, a typed yes). Host facts:
//! `docs/host-facts/claude-code.md` (hook output, `onFailure`, `asyncRewake`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use lunchroom_core::mail::Mail;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;

#[cfg(feature = "console")]
pub(crate) mod console_keys;
#[cfg(feature = "console")]
pub(crate) mod console_setup;
#[cfg(feature = "console")]
pub(crate) mod console_state;
#[cfg(all(test, feature = "console"))]
mod console_tests;

use crate::cli::Outcome;
use crate::evidence::{
    CODEX_TTY, CODEX_WEB, CODEX_WEB_DISABLED, codex_tty_setting, codex_web_is_off,
};
use crate::home::Env;
use crate::terminal::{NO_VERIFIER, System, confirm, confirm_or_begin, verifier_shown};

/// Seconds Claude Code gives each guard hook before it counts as failed. The guard answers within
/// its own deadline (10 seconds) first, so this is reached only when the guard is stopped.
const GUARD_TIMEOUT: u64 = 15;
/// Seconds one `lunchroom listen` may run (it returns before, after 23 hours).
const LISTEN_TIMEOUT: u64 = 86_400;

/// How long a configuration transaction waits for another to finish; past it, it changes nothing
/// and says so. Short under test, so the test of a held lock does not wait ten seconds.
const TRANSACTION_WAIT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(300)
} else {
    std::time::Duration::from_secs(10)
};

/// The exclusive lock one configuration transaction (an install or an uninstall: reading the
/// settings, backing them up, replacing them) holds, so two of them never interleave and neither
/// loses the other's change. It is the flock on `<LUNCHROOM_HOME>/install.flock`, a file that is
/// never replaced; the lock goes when this value is dropped (or the process ends).
struct Transaction(fs::File);

impl Transaction {
    /// Takes the lock, waiting [`TRANSACTION_WAIT`] for another transaction to let go.
    fn begin(env: &Env) -> std::io::Result<Self> {
        Self::begin_within(env, TRANSACTION_WAIT)
    }

    fn begin_within(env: &Env, wait: std::time::Duration) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&env.lunchroom_home)?;
        let path = env.lunchroom_home.join("install.flock");
        match fs::symlink_metadata(&path) {
            Ok(m) if !m.is_file() => {
                return Err(std::io::Error::other(format!(
                    "{} is not a plain file; remove it and run the command again",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        lunchroom_core::setgid::settle(&path);
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a plain file; remove it and run the command again",
                path.display()
            )));
        }
        let start = std::time::Instant::now();
        let mut pause = std::time::Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(fs::TryLockError::Error(e)) => return Err(e),
                Err(fs::TryLockError::WouldBlock) if start.elapsed() < wait => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(std::time::Duration::from_millis(20));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "another Lunchroom install or uninstall is changing the configuration; \
                         nothing was changed, run the command again when it is done",
                    ));
                }
            }
        }
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// `secs` as `YYYYMMDDTHHMMSSZ`, for backup names.
fn stamp(secs: u64) -> String {
    crate::delivery::utc(secs).replace(['-', ':'], "")
}

/// Fails unless `path` is absent or a plain file: a directory, a link (one that does not resolve,
/// since the callers resolve the others first), a socket or any other kind is not Lunchroom's to
/// replace.
fn plain_file_or_absent(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Ok(_) => Err(std::io::Error::other(format!(
            "{} is not a plain file (a link that leads nowhere, a folder or something else); \
             nothing was changed",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Creates a new file for writing, named `name(n)` for the first `n` that is free: exclusively, so
/// a name that exists (a file, a link to anywhere, a file another install just made) is never
/// opened and never written through.
fn create_new_named(
    name: impl Fn(u32) -> PathBuf,
    mode: u32,
) -> std::io::Result<(PathBuf, fs::File)> {
    for n in 0..1000 {
        let path = name(n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name for a new file"))
}

/// Writes `bytes` to a new file beside `target`, flushes it, then renames it over `target`; a crash
/// leaves the old file or the new one. The new file is created exclusively under a name that holds
/// the process id (and a number when that name is taken), so no other file is written through and
/// nothing is overwritten; on any failure it is removed.
fn write_beside(target: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pid = std::process::id();
    let (beside, mut file) = create_new_named(
        |n| {
            dir.join(if n == 0 {
                format!("{name}.lunchroom-new-{pid}")
            } else {
                format!("{name}.lunchroom-new-{pid}-{n}")
            })
        },
        mode,
    )?;
    let written = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&beside, fs::Permissions::from_mode(mode))?;
        fs::rename(&beside, target)
    })();
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&beside);
        return Err(e);
    }
    fs::File::open(dir)?.sync_all()
}

/// `, lunchroom 1.0.0 (61861fb),` for the report's backup line when the program at `program`
/// carries a build stamp; builds before the stamp carry none.
fn earlier_stamp(program: &Path) -> String {
    fs::read(program)
        .ok()
        .and_then(|bytes| stamp_in(&bytes))
        .map(|stamp| format!(", {stamp},"))
        .unwrap_or_default()
}

/// The build stamp a program carries (`cli::VERSION`): `lunchroom`, a three-part version, and
/// optionally a seven-character commit with `, modified`, in parentheses.
fn stamp_in(program: &[u8]) -> Option<String> {
    const MARK: &[u8] = b"lunchroom ";
    program
        .windows(MARK.len())
        .enumerate()
        .filter(|(_, w)| *w == MARK)
        .find_map(|(at, _)| {
            let text = &program[at..];
            let digits = text[MARK.len()..]
                .iter()
                .take_while(|b| b.is_ascii_digit() || **b == b'.')
                .count();
            let mut end = MARK.len() + digits;
            let version = std::str::from_utf8(&text[MARK.len()..end]).ok()?;
            let parts: Vec<&str> = version.split('.').collect();
            if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
                return None;
            }
            if let Some(tail) = text[end..].strip_prefix(b" (") {
                let inside = tail.iter().take(18).position(|b| *b == b')');
                if let Some(close) = inside.filter(|&close| {
                    let build = &tail[..close];
                    let commit = build.strip_suffix(b", modified").unwrap_or(build);
                    commit.len() == 7 && commit.iter().all(u8::is_ascii_hexdigit)
                }) {
                    end += 2 + close + 1;
                }
            }
            String::from_utf8(text[..end].to_vec()).ok()
        })
}

/// Writes `bytes` over `target` as [`write_beside`] does, first copying the old file to a dated
/// backup; a `target` that already holds `bytes` with `mode` is left as it is. A `target` that is a
/// link stays one: the file it points to is written. A `target` that is not, once resolved, a
/// plain file or absent (a folder, a link that leads nowhere) is refused, nothing written. The
/// backup, with the older backups that could not be removed (F112-AC4: once the new file is
/// written, only the three newest backups of `target` are kept).
fn replace(target: &Path, bytes: &[u8], mode: u32, now: u64) -> std::io::Result<Option<Backup>> {
    let target = &fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    if plain_file_or_absent(target)? {
        let same = fs::read(target).is_ok_and(|old| old == bytes)
            && fs::metadata(target).is_ok_and(|m| m.permissions().mode() & 0o777 == mode);
        if same {
            return Ok(None);
        }
    }
    let backup = back_up(target, now)?;
    write_beside(target, bytes, mode)?;
    Ok(backup.map(|path| Backup {
        path,
        unremoved: prune_backups(target),
    }))
}

/// A backup [`replace`] made, and each older backup it could not remove, with the reason.
#[derive(Debug)]
struct Backup {
    path: PathBuf,
    unremoved: Vec<String>,
}

impl Backup {
    /// The backup's path, then a line for each older backup that could not be removed.
    fn display(&self) -> String {
        let mut shown = self.path.display().to_string();
        for old in &self.unremoved {
            let _ = write!(shown, "\n  an older backup could not be removed: {old}");
        }
        shown
    }
}

/// How many backups of one file an install keeps (F112-AC4).
const BACKUPS_KEPT: usize = 3;

/// F112-AC4: removes all but the [`BACKUPS_KEPT`] newest backups of `target` that an install made:
/// plain files beside it named `<name>.lunchroom-backup-<stamp>` or `…-<stamp>-<n>`, the names
/// [`back_up`] makes, newest by stamp and then by `n`. A link, a folder or any other name is never
/// removed. Each backup that cannot be removed is returned, with the reason.
fn prune_backups(target: &Path) -> Vec<String> {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.lunchroom-backup-", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut backups: Vec<((String, u64), PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_str()?.to_owned();
            let key = backup_key(file.strip_prefix(&prefix)?)?;
            // The entry's own type, not its target's: a link is never a backup removed.
            entry
                .file_type()
                .ok()?
                .is_file()
                .then(|| (key, entry.path()))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(BACKUPS_KEPT);
    backups
        .into_iter()
        .take(excess)
        .filter_map(|(_, path)| {
            fs::remove_file(&path)
                .err()
                .map(|e| format!("{} ({e})", path.display()))
        })
        .collect()
}

/// The order of a backup name's tail, `<stamp>` or `<stamp>-<n>` ([`stamp`]'s form,
/// `YYYYMMDDTHHMMSSZ`); none for any other tail.
fn backup_key(tail: &str) -> Option<(String, u64)> {
    let (stamp, n) = match tail.split_once('-') {
        Some((stamp, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (stamp, n.parse().ok()?)
        }
        Some(_) => return None,
        None => (tail, 0),
    };
    let b = stamp.as_bytes();
    let form = b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit);
    form.then(|| (stamp.to_owned(), n))
}

/// Copies `target` to a dated backup beside it, when it exists; the backup's path. The backup is
/// created exclusively, with the file's own mode: a name that is taken (a file, or a link to
/// anywhere) is skipped, never written, so a backup is never overwritten, however many installs
/// run in the same second.
fn back_up(target: &Path, now: u64) -> std::io::Result<Option<PathBuf>> {
    if !plain_file_or_absent(target)? {
        return Ok(None);
    }
    let mut source = fs::File::open(target)?;
    let meta = source.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other(format!(
            "{} is not a plain file; nothing was changed",
            target.display()
        )));
    }
    let mode = meta.permissions().mode() & 0o777;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = format!("{name}.lunchroom-backup-{}", stamp(now));
    let (backup, mut copy) = create_new_named(
        |n| {
            target.with_file_name(if n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            })
        },
        mode,
    )
    .map_err(|e| std::io::Error::new(e.kind(), format!("no backup could be made: {e}")))?;
    let copied = (|| {
        std::io::copy(&mut source, &mut copy)?;
        copy.sync_all()?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(mode))
    })();
    drop(copy);
    if let Err(e) = copied {
        let _ = fs::remove_file(&backup);
        return Err(e);
    }
    Ok(Some(backup))
}

/// The harnesses `lunchroom install` sets up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    ClaudeCode,
    Codex,
}

impl Target {
    const fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// Seconds Codex gives Lunchroom's listener, which may run `codex queue` (60 seconds at most).
const CODEX_LISTEN_TIMEOUT: u64 = 75;

/// `word` as one shell word: as it is when it holds only characters the shell leaves alone, else in
/// single quotes (a path with a single quote is refused before this is called).
fn shell_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:=,@%".contains(c))
    {
        word.to_owned()
    } else {
        format!("'{word}'")
    }
}

/// What the tool-call hook writes to standard error when the guard fails (H1.5): that Lunchroom's
/// guard failed and the call was blocked, and what the user does. It is one single-quoted shell
/// word, so it holds no single quote of its own; `file` is the harness configuration file the user
/// edits, a quote in its name escaped.
pub(crate) fn guard_failed_line(file: &Path) -> String {
    format!(
        "{FAILED_LINE_HEAD}{}{FAILED_LINE_TAIL}",
        file.display().to_string().replace('\'', "'\\''")
    )
}

/// The line of [`guard_failed_line`] before the configuration file's name.
const FAILED_LINE_HEAD: &str = "Lunchroom: the guard failed and this tool call was blocked. Run \
     lunchroom doctor, or reinstall Lunchroom, or, where the Lunchroom program is gone, remove the \
     Lunchroom hooks from ";
/// The line of [`guard_failed_line`] after the file's name.
const FAILED_LINE_TAIL: &str = " or run lunchroom uninstall.";

/// Before the line of [`guard_failed_line`] in the tool-call hook's command.
const FAILURE_HEAD: &str = "|| { [ $? -eq 2 ] || printf '%s\\n' '";
/// After it.
const FAILURE_TAIL: &str = "' >&2; exit 2; }";

/// The tool-call hook's command (H1.5): `<program> guard pretool || exit 2`, with a line on
/// standard error before the `exit 2`, since Codex blocks on an exit 2 only when the hook wrote a
/// reason to standard error. A guard that itself exits 2 has blocked the call and written its own
/// reason, so the line is written only for another failure: a program that is missing (127),
/// stopped by the system (128 and more) or ended by an error of its own.
pub(crate) fn pretool_command(program: &str, file: &Path) -> String {
    format!(
        "{program} guard pretool {FAILURE_HEAD}{}{FAILURE_TAIL}",
        guard_failed_line(file)
    )
}

/// The hook groups Lunchroom adds, by hook event. Claude Code's listener waits in the background
/// (`asyncRewake`); Codex's hooks cannot, so there the listener hands a waiting message over once,
/// at a session's start and a turn's end. Claude Code runs `PostToolUseFailure` instead of
/// `PostToolUse` when a tool fails, so the guard sees that output too. `file` is the configuration
/// file the hooks are written to; the tool-call hook's failure line names it (H1.5).
fn lunchroom_hooks(program: &Path, target: Target, file: &Path) -> Vec<(&'static str, Value)> {
    let p = shell_word(&program.display().to_string());
    let guard = |mode: &str| {
        json!({"hooks": [{
            "type": "command",
            "command": format!("{p} guard {mode}"),
            "timeout": GUARD_TIMEOUT,
        }]})
    };
    // H1.5: the tool-call hook fails closed, and in Claude Code carries `"onFailure": "block"`,
    // which also blocks a call when the hook passes its time limit. No other hook carries either.
    let pretool = {
        let mut hook = json!({
            "type": "command",
            "command": pretool_command(&p, file),
            "timeout": GUARD_TIMEOUT,
        });
        if target == Target::ClaudeCode {
            hook["onFailure"] = json!("block");
        }
        json!({"hooks": [hook]})
    };
    let listen = match target {
        Target::ClaudeCode => json!({"hooks": [{
            "type": "command",
            "command": format!("{p} listen"),
            "asyncRewake": true,
            "timeout": LISTEN_TIMEOUT,
        }]}),
        Target::Codex => json!({"hooks": [{
            "type": "command",
            "command": format!("{p} listen --once"),
            "timeout": CODEX_LISTEN_TIMEOUT,
        }]}),
    };
    let mut hooks = vec![
        ("PreToolUse", pretool),
        ("PostToolUse", guard("posttool")),
        ("UserPromptSubmit", guard("prompt")),
        ("SessionStart", guard("start")),
        ("SessionStart", listen.clone()),
        ("Stop", listen),
    ];
    if target == Target::ClaudeCode {
        hooks.push(("PostToolUseFailure", guard("posttool")));
    }
    hooks
}

/// `command`'s program word, unquoted, and the rest of it.
pub(crate) fn program_and_args(command: &str) -> (&str, &str) {
    if let Some(quoted) = command.strip_prefix('\'')
        && let Some((program, args)) = quoted.split_once('\'')
    {
        return (program, args.trim_start());
    }
    command
        .split_once(char::is_whitespace)
        .map_or((command, ""), |(program, args)| {
            (program, args.trim_start())
        })
}

/// The program an earlier install ran besides the installed one: the default install location
/// (`~/.lunchroom/bin/lunchroom`), which differs from the installed program when `LUNCHROOM_HOME`
/// names another folder. Ownership is this exact path and nothing inferred from how a path is laid
/// out: another application's `~/.<name>/bin/<name>` is not Lunchroom's.
fn default_program(home: &Path) -> PathBuf {
    home.join(".lunchroom/bin/lunchroom")
}

/// The arguments Lunchroom's hooks give its program, as one string with single spaces.
const HOOK_ARGUMENTS: [&str; 6] = [
    "guard pretool",
    "guard posttool",
    "guard prompt",
    "guard start",
    "listen",
    "listen --once",
];

/// `args`, the words after Lunchroom's program in a hook command, are the arguments one of
/// Lunchroom's hooks gives it: one of [`HOOK_ARGUMENTS`], or `guard pretool` followed by the
/// failure branch of H1.5 as [`pretool_command`] writes it (the file name any). An earlier
/// install's `guard pretool` alone (the 1.0.1 installer wrote it, commit 276b58f, `install.rs` line
/// 346) is still Lunchroom's, so an install over it replaces it. Every other `guard pretool ...`,
/// among them a bare `|| exit 2` that no release wrote, is the user's.
pub(crate) fn is_hook_arguments(args: &str) -> bool {
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let args = words(args);
    if HOOK_ARGUMENTS.contains(&args.as_str()) {
        return true;
    }
    failure_branch_matches(&args)
}

/// `args` are `guard pretool` followed by exactly the failure branch [`pretool_command`] writes
/// (H1.5): the form that blocks a call when the guard fails. The 1.0.x form (`guard pretool`
/// alone) and a bare `|| exit 2` (Codex counts an exit 2 with nothing on standard error as a failed
/// hook, which lets the call run) are not it.
///
/// The comparison is byte for byte with what the install writes, and the line's file name is the
/// file the hook was read from (`file`; the same file by its path, or once both are resolved): a
/// leading, trailing, repeated, tab, line-break, carriage-return or Unicode space, any other text,
/// or the name of another file is not it. The hook is still Lunchroom's, so an install replaces it
/// ([`is_hook_arguments`], which reads runs of whitespace as one space), but it is not the form
/// that blocks: the shell reads a line break as a second command, and a syntax error exits 2 with
/// nothing on standard error, which Codex lets through.
pub(crate) fn is_fail_closed_for(args: &str, file: &Path) -> bool {
    let Some(name) = args
        .strip_prefix("guard pretool ")
        .and_then(|rest| rest.strip_prefix(FAILURE_HEAD))
        .and_then(|rest| rest.strip_suffix(FAILURE_TAIL))
        .and_then(|line| line.strip_prefix(FAILED_LINE_HEAD))
        .and_then(|line| line.strip_suffix(FAILED_LINE_TAIL))
    else {
        return false;
    };
    // The name is one single-quoted word: no quote in it but an escaped one.
    if name.is_empty() || name.replace("'\\''", "").contains('\'') {
        return false;
    }
    // The name is, byte for byte, the path of the file the hook was read from, the string the
    // install writes: a link, a relative path, `..`, `.` or a doubled slash that reaches the same
    // file is another spelling, not it (the safe side: the reason names the reinstall). No call is
    // made on the file system, so there is no race and nothing to follow.
    // Compared as bytes: a path that is not UTF-8 has no spelling in the line (the install refuses
    // it), so it is never equal to one.
    std::os::unix::ffi::OsStrExt::as_bytes(file.as_os_str())
        == name.replace("'\\''", "'").as_bytes()
}

/// The tool-call hook's line names the configuration file in text (a JSON string), and evidence
/// takes the hook as the install's only when that name is the file's path byte for byte: a path
/// that is not text cannot be named, so the guard would never count as in place (H1.5). The
/// install refuses such a path before it changes anything.
fn refuse_unnamable(settings_path: &Path) -> Option<Outcome> {
    settings_path.to_str().is_none().then(|| {
        Outcome::failed(
            "the configuration file's path holds a byte that is not text, so the guard's hook \
             cannot name it; nothing was changed",
            2,
        )
    })
}

/// `args` are, byte for byte, `guard pretool`: the only form a released installer wrote for the
/// tool-call hook (the 1.0.1 release, commit 276b58f, `rust/crates/lunchroom/src/install.rs` line
/// 346, `"command": format!("{p} guard {mode}")`), the one evidence tells the user to reinstall
/// over. No space, tab, no-break or other Unicode space, line break or carriage return is folded,
/// and no other form is an older install: a bare `|| exit 2` was never written by a release and is
/// not Lunchroom's hook.
pub(crate) fn is_pre_1_1_pretool(args: &str) -> bool {
    args == "guard pretool"
}

/// The longest hook command that can be the install's line. A longer one is not read at all.
pub(crate) const MAX_HOOK_COMMAND: usize = 4096;

/// [`is_fail_closed_for`]'s shape with runs of whitespace read as one space, and any file name.
fn failure_branch_matches(args: &str) -> bool {
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let args = words(args);
    // The line is exactly [`guard_failed_line`]'s: its fixed words around a file name, which is one
    // single-quoted word: no quote in it but an escaped one, and not empty.
    args.strip_prefix("guard pretool ")
        .and_then(|rest| rest.strip_prefix(words(FAILURE_HEAD).as_str()))
        .and_then(|r| r.strip_suffix(words(FAILURE_TAIL).as_str()))
        .and_then(|line| line.strip_prefix(words(FAILED_LINE_HEAD).as_str()))
        .and_then(|line| line.strip_suffix(words(FAILED_LINE_TAIL).as_str()))
        .is_some_and(|file| !file.trim().is_empty() && !file.replace("'\\''", "").contains('\''))
}

/// A hook Lunchroom wrote: its command runs Lunchroom's program, by its path (quoted or not) or by
/// a link that resolves to it (an earlier install under another name) or as [`default_program`],
/// with exactly the arguments one of Lunchroom's hooks gives it. A user's hook that runs the same
/// program with any other arguments is the user's and stays.
fn is_lunchroom_hook(hook: &Value, program: &Path, home: &Path) -> bool {
    let Some(command) = hook["command"].as_str() else {
        return false;
    };
    let known = is_hook_arguments;
    let p = program.display().to_string();
    let quoted = shell_word(&p);
    if [p.as_str(), quoted.as_str()].into_iter().any(|p| {
        command
            .strip_prefix(p)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace) && known(rest))
    }) {
        return true;
    }
    // The same reading of a hook's command words as the guard's evidence uses (R3.1).
    let (first, args) = program_and_args(command);
    if !known(args) {
        return false;
    }
    let first = first
        .strip_prefix("~/")
        .or_else(|| first.strip_prefix("$HOME/"))
        .map_or_else(|| PathBuf::from(first), |rest| home.join(rest));
    first == program
        || first == default_program(home)
        || fs::canonicalize(&first)
            .is_ok_and(|f| fs::canonicalize(program).is_ok_and(|program| f == program))
}

/// Removes Lunchroom's hooks from every event of `hooks`; a group goes only when nothing but
/// Lunchroom's hooks was in it. For each event that held one, the place its group was at.
fn strip_lunchroom_hooks(
    hooks: &mut Map<String, Value>,
    program: &Path,
    home: &Path,
) -> HashMap<String, usize> {
    let mut places = HashMap::new();
    for (event, list) in hooks {
        let Some(list) = list.as_array_mut() else {
            continue;
        };
        let (mut kept, mut place) = (0, None);
        list.retain_mut(|g| {
            let Some(group) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                kept += 1;
                return true;
            };
            let before = group.len();
            group.retain(|h| !is_lunchroom_hook(h, program, home));
            let removed = group.len() != before;
            let keep = !removed || !group.is_empty();
            if removed && place.is_none() {
                place = Some(kept + usize::from(keep));
            }
            kept += usize::from(keep);
            keep
        });
        if let Some(place) = place {
            places.insert(event.clone(), place);
        }
    }
    places
}

/// `settings` with Lunchroom's hook groups in place: earlier Lunchroom hooks are replaced, every
/// other key and hook is kept as it was, in its place. Lunchroom's group takes the place of the one
/// it replaces (Codex trusts a hook by its position), and is added last where there was none.
fn with_lunchroom_hooks(
    mut settings: Map<String, Value>,
    program: &Path,
    home: &Path,
    target: Target,
    file: &Path,
) -> Result<Map<String, Value>, String> {
    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("the settings' \"hooks\" is not an object")?;
    let mut places = strip_lunchroom_hooks(hooks, program, home);
    for (event, group) in lunchroom_hooks(program, target, file) {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| format!("the settings' hooks for {event} are not a list"))?;
        let at = places
            .get(event)
            .map_or(list.len(), |at| (*at).min(list.len()));
        list.insert(at, group);
        // An event's later Lunchroom group follows its first, in the same order on every install.
        places.insert(event.to_owned(), at + 1);
    }
    Ok(settings)
}

/// The sessions named in a registry file left in Lunchroom's folder by a previous installation
/// (`<LUNCHROOM_HOME>/registry.json`,
/// `{"agents": {"<name>": {"kind": "claude"|"codex", "id": "<session or thread id>"}}}`), which
/// the install offers to name again; nothing else of that installation is read.
fn earlier_names(env: &Env) -> Vec<(String, lunchroom_core::identity::Identity)> {
    use lunchroom_core::identity::{Harness, Identity};
    let Some(registry) = fs::read_to_string(user_folder(env).join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, agent) in registry["agents"].as_object().into_iter().flatten() {
        let harness = match agent["kind"].as_str() {
            Some("claude") => Harness::ClaudeCode,
            Some("codex") => Harness::Codex,
            _ => continue,
        };
        let Some(id) = agent["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        out.push((name.clone(), Identity::new(harness, id)));
    }
    out
}

/// The marker in a plugin's source that `lunchroom install` replaces with the program's path.
const PROGRAM_MARKER: &str = "__LUNCHROOM_PROGRAM__";

/// Lunchroom's OpenClaw plugin, as `openclaw plugins install --link` reads it.
const OPENCLAW_PLUGIN: [(&str, &str); 3] = [
    (
        "index.mjs",
        include_str!("../../../plugins/openclaw/index.mjs"),
    ),
    (
        "openclaw.plugin.json",
        include_str!("../../../plugins/openclaw/openclaw.plugin.json"),
    ),
    (
        "package.json",
        include_str!("../../../plugins/openclaw/package.json"),
    ),
];

/// Lunchroom's Hermes plugin, as Hermes finds it in `<HERMES_HOME>/plugins/lunchroom/`.
const HERMES_PLUGIN: [(&str, &str); 2] = [
    (
        "plugin.yaml",
        include_str!("../../../plugins/hermes/plugin.yaml"),
    ),
    (
        "__init__.py",
        include_str!("../../../plugins/hermes/__init__.py"),
    ),
];

/// Where `lunchroom` is linked so it runs by name: `~/.local/bin/lunchroom` or `~/bin/lunchroom`,
/// the first of those folders on `path` that holds no other `lunchroom` (one already linked to
/// `program` counts as free); with neither folder on the account at all, `~/.local/bin/lunchroom`
/// (the install makes the folder). `None` when neither on `path` is free and one of them exists.
fn path_link(home: &Path, path: &std::ffi::OsStr, program: &Path) -> Option<PathBuf> {
    let wanted = [home.join(".local/bin"), home.join("bin")];
    std::env::split_paths(path)
        .filter(|d| wanted.contains(d) && d.is_dir())
        .map(|d| d.join("lunchroom"))
        .find(|l| match fs::read_link(l) {
            Ok(target) => target == program,
            Err(_) => fs::symlink_metadata(l).is_err(),
        })
        // Decision of 2026-11-14 ("Make the link"): an account with neither folder gets
        // ~/.local/bin, which the common login profiles put on PATH once it exists.
        .or_else(|| (!wanted.iter().any(|d| d.exists())).then(|| home.join(".local/bin/lunchroom")))
}

/// Links `link` to `program` (nothing to do when it already is); the report's line. A link that
/// cannot be made is a line of the report too, never an error: the program and what was written
/// before the link stay in the report of an install that went on.
fn link_program(link: &Path, program: &Path) -> String {
    let folder = link.parent().unwrap_or(link);
    // Decision of 2026-11-14 ("Make the link"): on an account with neither ~/.local/bin nor
    // ~/bin, ~/.local/bin is made for the link (path_link chose it, and the question named it).
    let made = if fs::read_link(link).is_err() {
        if folder.ends_with(".local/bin") && !folder.exists() {
            // Made with the user's own group, not the setgid program's `lunchroom`.
            lunchroom_core::setgid::as_own_group(|| fs::create_dir_all(folder))
                .and_then(|()| std::os::unix::fs::symlink(program, link))
        } else {
            std::os::unix::fs::symlink(program, link)
        }
    } else {
        Ok(())
    };
    let on_path =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == folder));
    match made {
        Ok(()) if on_path => format!(
            "linked {} to it, so `lunchroom` runs by name\n",
            link.display()
        ),
        Ok(()) => format!(
            "linked {} to it; {} is not on your PATH yet, so until it is, run it by its path, {}. \
             Open a new login shell: Ubuntu, Debian and Fedora put ~/.local/bin on PATH when it \
             exists\n",
            link.display(),
            folder.display(),
            program.display()
        ),
        Err(e) => format!(
            "{} was not linked to it ({e}); run it by its path, {}\n",
            link.display(),
            program.display()
        ),
    }
}

/// The question's lines naming the previous installation's registry sessions, empty when there
/// are none.
fn earlier_lines(earlier: &[(String, lunchroom_core::identity::Identity)]) -> String {
    let mut names = String::new();
    if !earlier.is_empty() {
        names.push_str(
            "It also names these sessions from the earlier Lunchroom's registry, so they \
             receive messages:\n",
        );
        for (name, who) in earlier {
            let _ = writeln!(names, "  {name}  {who}");
        }
    }
    names
}

/// The question's line about the link, empty when there is none.
fn link_line(link: Option<&PathBuf>) -> String {
    link.map(|l| match l.parent().filter(|f| !f.exists()) {
        Some(folder) => format!(
            "\nIt also makes {} and links {} to the program.",
            folder.display(),
            l.display()
        ),
        None => format!("\nIt also links {} to the program.", l.display()),
    })
    .unwrap_or_default()
}

/// `lunchroom install openclaw` or `lunchroom install hermes`: the program, and the plugin's files
/// with the program's path in them, each old file kept beside as a backup.
#[allow(clippy::too_many_lines)] // one transaction: plan, lock, re-plan, write, report
fn install_plugin(openclaw: bool, env: &Env, sys: &mut dyn System, program: &Path) -> Outcome {
    let program = program.to_path_buf();
    let shown = program.display().to_string();
    // The path is written inside a quoted string in JavaScript and Python.
    if shown.contains(['"', '\\', '\n']) {
        return Outcome::failed(
            "the program's path holds a quote, backslash or line break; nothing was changed",
            2,
        );
    }
    let (dir, files): (PathBuf, &[(&str, &str)]) = if openclaw {
        (openclaw_plugin_dir(env), &OPENCLAW_PLUGIN)
    } else {
        (
            env.hermes_home.join("plugins").join("lunchroom"),
            &HERMES_PLUGIN,
        )
    };
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let names: Vec<String> = files
        .iter()
        .map(|(name, _)| format!("  {}", dir.join(name).display()))
        .collect();
    let link = std::env::var_os("PATH").and_then(|p| path_link(&env.home, &p, &program));
    let question = format!(
        "Lunchroom: add Lunchroom to {}? This writes Lunchroom's program and the plugin that runs \
         the guard, which only you may change.\nInstall the program at {shown} and write these \
         files? Old files \
         are kept beside them as backups.{}\n{}\n",
        if openclaw { "OpenClaw" } else { "Hermes" },
        link_line(link.as_ref()),
        names.join("\n")
    );
    if let Err(reason) = confirm_or_begin(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let _transaction = match Transaction::begin(env) {
        Ok(t) => t,
        Err(e) => return Outcome::failed(&format!("the install did not start: {e}"), 1),
    };
    let written = (|| -> std::io::Result<String> {
        let mut report = String::new();
        let earlier = earlier_stamp(&program);
        let program_backup = write_program(&program, env.now)?;
        let _ = writeln!(report, "installed {shown}: {}", crate::cli::VERSION);
        if let Some(b) = program_backup {
            let _ = writeln!(
                report,
                "  the earlier program{earlier} is kept at {}",
                b.display()
            );
        }
        if let Some(l) = &link {
            report.push_str(&link_program(l, &program));
        }
        for (name, source) in files {
            let target = dir.join(name);
            let text = source.replace(PROGRAM_MARKER, &shown);
            let backup = replace(&target, text.as_bytes(), 0o644, env.now)?;
            let _ = writeln!(report, "wrote {}", target.display());
            if let Some(b) = backup {
                let _ = writeln!(report, "  the earlier file is kept at {}", b.display());
            }
        }
        Ok(report)
    })();
    let mut report = match written {
        Ok(r) => r,
        Err(e) => return Outcome::failed(&format!("the install stopped: {e}"), 1),
    };
    if openclaw {
        let _ = write!(
            report,
            "Steps only you can do (Lunchroom writes no trust record):\n\
             1. openclaw plugins install --link {} --force   (review and accept its capabilities)\n\
             2. openclaw plugins enable lunchroom\n\
             3. openclaw config set plugins.entries.lunchroom.hooks.allowConversationAccess true \
             --strict-json   (Lunchroom confirms deliveries and sees another agent's text in each \
             turn's prompt only with it; without it every OpenClaw session is UNGUARDED and \
             receives nothing)\n\
             4. openclaw plugins reload lunchroom, or restart the gateway.\n\
             5. In each session that should receive messages, type join (or join <name>); type \
             leave to take it out.\n",
            dir.display()
        );
    } else {
        report.push_str(
            "Steps only you can do (Lunchroom writes no trust record):\n\
             1. hermes plugins enable lunchroom\n\
             2. Start each Hermes session that Lunchroom should guard anew: Hermes loads a plugin \
             when a session starts.\n\
             3. In each session that should receive messages, type join (or join <name>); type \
             leave to take it out.\n",
        );
    }
    Outcome::ok(report)
}

/// Spec section 13 item 4: `lunchroom install` with no harness named found none: nothing was changed, and how to
/// name one. A harness's folder is made the first time it runs, so one installed and never started
/// is not found.
fn no_harness_found() -> Outcome {
    Outcome::failed(
        "no supported harness was found (Claude Code, Codex, OpenClaw, Hermes), so nothing was \
         changed. A harness's folder (~/.claude, ~/.codex, ~/.openclaw, ~/.hermes) is made the \
         first time it runs: start it once, or name it: lunchroom install \
         claude-code|codex|openclaw|hermes",
        2,
    )
}

/// The supported harnesses whose folder exists for this user (H1.4): the word `install` takes, and
/// the harness's name.
pub(crate) fn found_harnesses(env: &Env) -> Vec<(&'static str, &'static str)> {
    found_harnesses_in(env, &env.claude_home)
}

/// [`found_harnesses`] with Claude Code's user settings folder `claude` (G3-5): `lunchroom doctor`
/// passes the folder Claude Code reads, `CLAUDE_CONFIG_DIR` when it is set, so a setup whose
/// `~/.claude` does not exist is still checked.
pub(crate) fn found_harnesses_in(env: &Env, claude: &Path) -> Vec<(&'static str, &'static str)> {
    let openclaw = std::env::var_os("OPENCLAW_STATE_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| env.home.join(".openclaw"), PathBuf::from);
    [
        ("claude-code", "Claude Code", claude.to_path_buf()),
        ("codex", "Codex", env.codex_home.clone()),
        ("openclaw", "OpenClaw", openclaw),
        ("hermes", "Hermes", env.hermes_home.clone()),
    ]
    .into_iter()
    .filter(|(_, _, folder)| folder.is_dir())
    .map(|(word, name, _)| (word, name))
    .collect()
}

/// `lunchroom install` with no harness named (H1.4): each supported harness found is offered in
/// turn, and only those the user answers yes to are installed. With a passphrase set and more than
/// one harness found, the passphrase is asked once, before the first harness, and each harness is
/// then offered with a plain yes (F112-AC2); with no passphrase set (the first install) or one
/// harness found, each harness's own question is asked, as in 1.1.1.
///
/// Each harness's outcome is printed on its own line (installed, not installed, or failed with its
/// reason). The exit code is a failure's own when any harness failed, even if another was
/// installed (a user's no is no failure); 3 when none was installed and none failed; else 0. The
/// flag is whether any harness was installed, so the steps that follow an install still run.
fn install_found(env: &Env, sys: &mut dyn System) -> (Outcome, bool) {
    let found = found_harnesses(env);
    if found.is_empty() {
        return (no_harness_found(), false);
    }
    let names: Vec<&str> = found.iter().map(|(_, name)| *name).collect();
    let mut first = Some(format!("Found: {}\n", names.join(", ")));
    // F112-AC2: the one passphrase, asked and checked before the first harness. A wrong one, or
    // none read, installs nothing.
    let mut passphrase = None;
    if found.len() > 1 && sys.verifier().is_ok() {
        let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
            Ok(m) => m,
            Err(e) => return (Outcome::failed(&crate::cli::unread(e), 1), false),
        };
        let question = format!(
            "{}Lunchroom will show each harness's changes and ask yes or no for each.\n",
            first.take().unwrap_or_default()
        );
        let mut capture = Prompted {
            sys: &mut *sys,
            mode: Mode::Capture(None),
        };
        if let Err(reason) = confirm_or_begin(&mail, &mut capture, &question) {
            return (Outcome::denied(&reason, 3), false);
        }
        if let Mode::Capture(typed) = capture.mode {
            passphrase = typed;
        }
    }
    let mut out = Outcome::ok(String::new());
    let (mut installed, mut failed) = (false, Vec::new());
    for (word, name) in found {
        let mode = match &passphrase {
            Some(typed) => Mode::Offer {
                harness: name,
                passphrase: typed.clone(),
                declined: false,
            },
            None => Mode::Preface(first.take()),
        };
        let mut asker = Prompted {
            sys: &mut *sys,
            mode,
        };
        let one = install_target(&[word.to_owned()], env, &mut asker);
        let declined = matches!(asker.mode, Mode::Offer { declined: true, .. });
        if !out.stdout.is_empty() {
            out.stdout.push('\n');
        }
        let verdict = match one.code {
            0 => "installed",
            3 if declined => "not installed (you answered no)",
            3 => "not installed",
            _ => "failed",
        };
        let _ = writeln!(out.stdout, "{name}: {verdict}");
        if !declined {
            out.stdout.push_str(&one.stdout);
            out.stdout.push_str(&one.stderr);
        }
        installed |= one.code == 0;
        if !matches!(one.code, 0 | 3) {
            failed.push((name, one.code));
        }
    }
    if let Some(&(_, code)) = failed.first() {
        out.code = code;
        let names: Vec<&str> = failed.iter().map(|(name, _)| *name).collect();
        let _ = writeln!(
            out.stderr,
            "the install failed for: {}{}",
            names.join(", "),
            if installed {
                " (the others were installed; see above)"
            } else {
                ""
            }
        );
    } else if !installed {
        out.code = 3;
    }
    (out, installed)
}

/// How [`Prompted`] meets a question.
enum Mode {
    /// Puts the text before the first question, so the user reads it before answering.
    Preface(Option<String>),
    /// Asks the user, and keeps the answer (the one passphrase, F112-AC2).
    Capture(Option<String>),
    /// F112-AC2: meets the passphrase question with a plain yes from the user and, on yes, gives
    /// the passphrase already typed; any other answer gives none and marks the harness declined.
    /// Every other question is asked as it is.
    Offer {
        harness: &'static str,
        passphrase: String,
        declined: bool,
    },
}

/// The user's terminal, with each question met as [`Mode`] says.
struct Prompted<'a> {
    sys: &'a mut dyn System,
    mode: Mode,
}

impl Prompted<'_> {
    fn meet(&mut self, prompt: &str, secret: bool) -> Option<String> {
        let ask = |sys: &mut dyn System, prompt: &str| {
            if secret {
                sys.ask_secret(prompt)
            } else {
                sys.ask(prompt)
            }
        };
        match &mut self.mode {
            Mode::Preface(first) => match first.take() {
                Some(first) => ask(&mut *self.sys, &format!("{first}{prompt}")),
                None => ask(&mut *self.sys, prompt),
            },
            Mode::Capture(kept) => {
                let answer = ask(&mut *self.sys, prompt);
                kept.clone_from(&answer);
                answer
            }
            Mode::Offer {
                harness,
                passphrase,
                declined,
            } => {
                let Some(question) = prompt.strip_suffix("Passphrase: ").filter(|_| secret) else {
                    return ask(&mut *self.sys, prompt);
                };
                // No answer read (no terminal) is not a no: the install reports it as such.
                let answer = self
                    .sys
                    .ask(&format!("{question}Install for {harness}? Type yes: "))?;
                if answer.trim().eq_ignore_ascii_case("yes") {
                    Some(passphrase.clone())
                } else {
                    *declined = true;
                    None
                }
            }
        }
    }
}

impl System for Prompted<'_> {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        self.sys.ancestors()
    }
    fn own_tty(&self) -> u64 {
        self.sys.own_tty()
    }
    fn tty_of(&self, key: &str) -> Option<u64> {
        self.sys.tty_of(key)
    }
    fn ask(&mut self, question: &str) -> Option<String> {
        self.meet(question, false)
    }
    fn uid(&self) -> Option<u32> {
        self.sys.uid()
    }
    fn verifier(&self) -> Result<String, String> {
        self.sys.verifier()
    }
    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        self.meet(prompt, true)
    }
    fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
        self.sys.salt()
    }
    fn install_verifier(&mut self, line: &str) -> Result<(), String> {
        self.sys.install_verifier(line)
    }
    fn verifier_blocker(&self) -> Option<String> {
        self.sys.verifier_blocker()
    }
    fn confirm_root(&mut self, question: &str) -> Result<(), String> {
        // The preface goes before the first question, as in `meet`; other modes pass it on.
        let first = match &mut self.mode {
            Mode::Preface(first) => first.take(),
            Mode::Capture(_) | Mode::Offer { .. } => None,
        };
        match first {
            Some(first) => self.sys.confirm_root(&format!("{first}{question}")),
            None => self.sys.confirm_root(question),
        }
    }
    fn sudo_list(&mut self) -> Result<String, String> {
        self.sys.sudo_list()
    }
    fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
        self.sys.sudo_list_user(user)
    }
    #[cfg(feature = "console")]
    fn root_blocker(&self) -> Option<String> {
        self.sys.root_blocker()
    }
    #[cfg(feature = "console")]
    fn missing(&self, path: &Path) -> bool {
        self.sys.missing(path)
    }
    #[cfg(feature = "console")]
    fn run_root(&mut self, what: &str, argv: &[String], input: Option<&str>) -> Result<(), String> {
        self.sys.run_root(what, argv, input)
    }
    #[cfg(feature = "console")]
    fn run_user(&mut self, program: &Path, args: &[&str]) -> Result<i32, String> {
        self.sys.run_user(program, args)
    }
}

/// `lunchroom install [claude-code|codex|openclaw|hermes]`, or `lunchroom install console`.
#[must_use]
pub fn install(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    if args.first().map(String::as_str) == Some("console") {
        // The console comes in a later release; a build without it says so and changes nothing.
        #[cfg(not(feature = "console"))]
        return Outcome::failed(
            "lunchroom install console: the Lunchroom console comes in a later release; this \
             release does not include it, so nothing was changed.",
            2,
        );
        #[cfg(feature = "console")]
        return if args.len() == 1 {
            console_setup::install_console(env, sys)
        } else {
            Outcome::failed("usage: lunchroom install console", 2)
        };
    }
    // §17.2: where this system sets the store wall up and this is not yet the setgid program, the
    // wall comes first and the setgid program's own install writes the hooks.
    #[cfg(feature = "console")]
    if sys.sets_up_wall() && !lunchroom_core::setgid::active() {
        // §13::4: with no harness named and none found, the install changes nothing and says how
        // to name one. Asked before the wall: the wall's root steps change the system (cold
        // first-time install, 2026-11-14: the wall was set up, then the hooks could not be).
        if args.is_empty() && found_harnesses(env).is_empty() {
            return no_harness_found();
        }
        // F2 design, "Finding and starting the console", item 1: the console setup that ends the
        // install is run by the installed (setgid) program's own install, which `finish_wall`
        // starts as a child; this unpacked parent does not run it a second time.
        return console_setup::install_with_wall(args, env, sys);
    }
    // F112-AC3: an install run with the build already installed is told so before its question.
    let installed_now = installed_program(env);
    let notice = std::env::current_exe()
        .and_then(fs::read)
        .ok()
        .and_then(|running| same_build_notice(&installed_now, &running));
    let mut noticed = Prompted {
        sys,
        mode: Mode::Preface(notice),
    };
    let sys: &mut dyn System = &mut noticed;
    // With no passphrase set yet there is nothing to check one against: this install is confirmed
    // with a typed yes, and sets the first passphrase once the hooks are written.
    let first = sys.verifier().is_err_and(|reason| reason == NO_VERIFIER);
    let (mut out, installed) = if args.is_empty() {
        install_found(env, sys)
    } else {
        let one = install_target(args, env, sys);
        let installed = one.code == 0;
        (one, installed)
    };
    // The steps after an install run when any harness was installed, even if another failed: the
    // hooks written are in place, and the first passphrase must exist for them.
    if installed {
        out.stdout.push_str(&ask_hold_mode(env, sys));
        if first {
            out.stdout.push_str("\nSetting your passphrase:\n");
            let set = crate::user::set_passphrase(sys);
            out.stdout.push_str(&set.stdout);
            if set.code != 0 {
                out.stderr.push_str(&set.stderr);
                out.stderr.push_str(
                    "The hooks are in place, but no passphrase is set: every command only you \
                     may run is denied until `lunchroom passphrase` succeeds. Run \
                     `lunchroom passphrase` in a terminal of your own to set it.\n",
                );
                // The install did not finish what it set out to do.
                out.code = out.code.max(1);
            }
        }
    }
    // F2 design item 1: this install ends by running the console setup.
    #[cfg(feature = "console")]
    if installed && out.code == 0 && lunchroom_core::setgid::active() {
        out = end_with_console(out, env, &mut *noticed.sys);
    }
    out
}

/// What an install says when the console setup that ends it stops: the wall stays installed.
#[cfg(feature = "console")]
const CONSOLE_STOPPED: &str = "The store wall is installed and the hooks run the new program, but \
the console is not set up (the reason is above). To finish, run `lunchroom install console` in a \
terminal of your own.\n";

/// What the end of an install does about the console (F2 design, item 1).
#[cfg(feature = "console")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterInstall {
    /// The console is not set up: run the console setup.
    RunSetup,
    /// The console is set up: only say how to open it.
    SayOpen,
}

/// The decision of [`AfterInstall`], from the one fact it rests on.
#[cfg(feature = "console")]
fn after_install(console_set_up: bool) -> AfterInstall {
    if console_set_up {
        AfterInstall::SayOpen
    } else {
        AfterInstall::RunSetup
    }
}

/// F2 design, "Finding and starting the console", item 1: the end of `lunchroom install`, run by
/// the installed (setgid) program, so a rerun of it and the fresh install's `finish_wall` child
/// are the same path. A console already set up is only told how to open
/// ([`crate::cli::OPEN_CONSOLE_LINE`]); otherwise [`console_setup::install_console`] runs in this
/// process (with the wall active it builds no wall steps and does not run `install` again), with
/// its checks and the stop on a wide password-free rule (F2-AC31). When it stops the report says
/// how to finish and the code is not 0.
#[cfg(feature = "console")]
fn end_with_console(mut out: Outcome, env: &Env, sys: &mut dyn System) -> Outcome {
    if after_install(console_setup::console_is_set_up(env, sys)) == AfterInstall::RunSetup {
        let setup = console_setup::install_console(env, sys);
        out.stdout.push_str(&setup.stdout);
        out.stderr.push_str(&setup.stderr);
        if setup.code != 0 {
            // The hooks are written, the console is not set up: exit code 6, which the unpacked
            // program that started this one reads as "hooks done" (F2 design, "Finding and
            // starting the console", item 1, exit codes). The report says how to finish.
            out.stdout.push_str(CONSOLE_STOPPED);
            out.code = console_setup::CONSOLE_NOT_SET_UP;
            return out;
        }
    }
    out.stdout.push_str(crate::cli::OPEN_CONSOLE_LINE);
    out.stdout.push('\n');
    out
}

/// F112-AC3: when the program installed at `installed` holds the same bytes as `running`, the
/// notice that this build is already installed, naming it, and how a newer build is installed.
fn same_build_notice(installed: &Path, running: &[u8]) -> Option<String> {
    let bytes = fs::read(fs::canonicalize(installed).ok()?).ok()?;
    (bytes == running).then(|| {
        format!(
            "This is the build already installed ({}); installing it again upgrades nothing. To \
             install a newer build, unpack it and run its program by its path: ./lunchroom \
             install, in the unpacked folder.\n",
            crate::cli::VERSION
        )
    })
}

/// P1.6: after a successful install, the user is asked the hold mode once, on the same terminal
/// the install was confirmed on; an answer that names no mode, or none, leaves Balanced.
fn ask_hold_mode(env: &Env, sys: &mut dyn System) -> String {
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        return String::new();
    };
    if store.read().is_ok_and(|m| m.hold_mode_chosen()) {
        return String::new();
    }
    let answer = sys
        .ask(&format!(
            "{}Choose 1, 2 or 3 [2]: ",
            crate::user::MODE_QUESTION
        ))
        .unwrap_or_default();
    let mode = crate::user::mode_answer(&answer).unwrap_or_default();
    match store.update(|m| m.set_hold_mode(mode)) {
        Ok(()) => format!(
            "hold mode: {} (change it with: lunchroom mode relaxed|balanced|strict)\n",
            mode.word()
        ),
        Err(e) => format!("the hold mode was not saved ({e}); it is Balanced\n"),
    }
}

/// The settings at `path`, empty when there is no file; anything but a JSON object changes nothing.
fn read_settings(path: &Path) -> Result<Map<String, Value>, Outcome> {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(Outcome::failed(
                &format!(
                    "{} is not a JSON object; nothing was changed",
                    path.display()
                ),
                2,
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(Outcome::failed(
            &format!("{} cannot be read: {e}", path.display()),
            1,
        )),
    }
}

/// The settings at `path` without Lunchroom's hooks (an event left with none goes), and whether
/// that changes them.
fn plan_removal(
    path: &Path,
    program: &Path,
    home: &Path,
) -> Result<(Map<String, Value>, bool), Outcome> {
    let old = read_settings(path)?;
    let mut new = old.clone();
    if let Some(hooks) = new.get_mut("hooks").and_then(Value::as_object_mut) {
        for event in strip_lunchroom_hooks(hooks, program, home).keys() {
            if hooks[event].as_array().is_some_and(Vec::is_empty) {
                hooks.remove(event);
            }
        }
    }
    let changed = new != old;
    Ok((new, changed))
}

/// What still uses Lunchroom's shared program and store, by harness: Claude Code's settings and
/// Codex's `hooks.json` that still hold a hook of Lunchroom's, and the OpenClaw and Hermes plugin
/// files that are still written. A configuration that cannot be read is listed too, as one that
/// could not be checked: it is not known to be free of Lunchroom.
fn remaining_users(env: &Env, program: &Path) -> Vec<String> {
    let mut users = Vec::new();
    for (harness, path) in [
        ("Claude Code", env.claude_home.join("settings.json")),
        ("Codex", env.codex_home.join("hooks.json")),
    ] {
        match plan_removal(&path, program, &env.home) {
            Ok((_, true)) => users.push(format!(
                "{harness} (Lunchroom's hooks are in {}; `lunchroom uninstall {}`)",
                path.display(),
                if harness == "Codex" {
                    "codex"
                } else {
                    "claude-code"
                }
            )),
            Ok((_, false)) => {}
            Err(_) => users.push(format!(
                "{harness} ({} could not be checked)",
                path.display()
            )),
        }
    }
    for (harness, plugin, disable) in [
        (
            "OpenClaw",
            openclaw_plugin_dir(env).join("index.mjs"),
            "openclaw plugins disable lunchroom",
        ),
        (
            "Hermes",
            env.hermes_home.join("plugins/lunchroom/__init__.py"),
            "hermes plugins disable lunchroom",
        ),
    ] {
        if fs::symlink_metadata(&plugin).is_ok() {
            users.push(format!(
                "{harness} (Lunchroom's plugin is written at {}; `{disable}`)",
                plugin.display()
            ));
        }
    }
    users
}

/// `lunchroom uninstall claude-code|codex|openclaw|hermes`: on the user's yes, removes Lunchroom's
/// hooks from that harness's configuration and keeps a backup; every other hook stays. Lunchroom's
/// program and store are left for the user to delete.
#[must_use]
#[allow(clippy::too_many_lines)] // one transaction: plan, lock, re-plan, write, report
pub fn uninstall(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    #[cfg(feature = "console")]
    if args.first().map(String::as_str) == Some("console") {
        return if args.len() == 1 {
            console_setup::uninstall_console(env, sys)
        } else {
            Outcome::failed("usage: lunchroom uninstall console", 2)
        };
    }
    let (harness, settings_path) = match args.first().map(String::as_str) {
        Some("claude-code") => ("Claude Code", env.claude_home.join("settings.json")),
        Some("codex") => ("Codex", env.codex_home.join("hooks.json")),
        Some("openclaw") => {
            return Outcome::ok(
                "Lunchroom's OpenClaw plugin is the harness's to remove: openclaw plugins disable \
                 lunchroom\n"
                    .to_owned(),
            );
        }
        Some("hermes") => {
            return Outcome::ok(
                "Lunchroom's Hermes plugin is the harness's to remove: hermes plugins disable \
                 lunchroom\n"
                    .to_owned(),
            );
        }
        _ => {
            return Outcome::failed(
                "usage: lunchroom uninstall claude-code|codex|openclaw|hermes",
                2,
            );
        }
    };
    let program = installed_program(env);
    match plan_removal(&settings_path, &program, &env.home) {
        Err(out) => return out,
        Ok((_, false)) => {
            return Outcome::ok(format!(
                "{} holds no Lunchroom hooks; nothing was changed\n",
                settings_path.display()
            ));
        }
        Ok(_) => {}
    }
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let question = format!(
        "Lunchroom: this changes the configuration that runs the guard, which only you may \
         change.\nRemove Lunchroom's hooks from {}? Every other hook stays, and the old file is \
         kept beside it as a backup.\n",
        settings_path.display()
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    // As on install: one transaction at a time, and what is written is computed from the settings
    // as they are after the yes.
    let _transaction = match Transaction::begin(env) {
        Ok(t) => t,
        Err(e) => return Outcome::failed(&format!("the uninstall did not start: {e}"), 1),
    };
    let (new, _) = match plan_removal(&settings_path, &program, &env.home) {
        Ok(planned) => planned,
        Err(out) => return out,
    };
    let written = serde_json::to_string_pretty(&Value::Object(new))
        .map_err(std::io::Error::other)
        .and_then(|mut text| {
            text.push('\n');
            let mode =
                fs::metadata(&settings_path).map_or(0o600, |m| m.permissions().mode() & 0o777);
            replace(&settings_path, text.as_bytes(), mode, env.now)
        });
    match written {
        Ok(backup) => {
            let mut report = format!(
                "removed Lunchroom's hooks from {}\n",
                settings_path.display()
            );
            if let Some(b) = backup {
                let _ = writeln!(report, "  the earlier settings are kept at {}", b.display());
            }
            let _ = writeln!(
                report,
                "Start each {harness} session anew, so it stops running the guard."
            );
            let users = remaining_users(env, &program);
            report.push_str(&what_stays(&env.lunchroom_home, &users));
            #[cfg(feature = "console")]
            if let Some(note) = console_setup::rule_file_note(sys) {
                report.push_str(&note);
            }
            // The passphrase check is root's: Lunchroom does not remove it.
            let _ = writeln!(
                report,
                "Your passphrase check stays: `sudo rm {}` removes it.",
                verifier_shown(sys)
            );
            Outcome::ok(report)
        }
        Err(e) => Outcome::failed(&format!("the uninstall stopped: {e}"), 1),
    }
}

/// What `lunchroom uninstall <harness>` says stays after the hooks are removed, for Lunchroom's
/// folder `home` and the harnesses still using it. With the store wall (1.1, §17 item 2) the
/// program is the shared `/usr/local/lib/lunchroom/lunchroom` and the messages are in
/// `/var/lib/lunchroom/<uid>`, both root's: the developer cannot delete them, so the line names
/// them as root's instead of telling the developer to delete a folder.
fn what_stays(home: &Path, users: &[String]) -> String {
    let walled = home.starts_with(setgid::STORE_ROOT);
    match (walled, users.is_empty()) {
        (false, true) => format!(
            "Lunchroom's program and messages stay in {}: delete that folder, and the `lunchroom` \
             link in ~/.local/bin or ~/bin, to remove them. No other harness was found using \
             them.\n",
            home.display()
        ),
        (false, false) => format!(
            "Lunchroom's program and messages stay in {}, and are still used by: {}. Do not delete \
             that folder or the `lunchroom` link until those are uninstalled too.\n",
            home.display(),
            users.join("; ")
        ),
        (true, true) => format!(
            "Lunchroom's program stays at {} and your messages in {}, both root's. No other \
             harness was found using them. To remove them, run `lunchroom uninstall console` \
             first, then, as root, remove {}, and delete the `lunchroom` link in ~/.local/bin or \
             ~/bin. The program is shared by every user of this computer who installed \
             Lunchroom: remove {} only when none of them uses it.\n",
            setgid::PROGRAM,
            home.display(),
            home.display(),
            setgid::PROGRAM_DIR
        ),
        (true, false) => format!(
            "Lunchroom's program stays at {} and your messages in {}, both root's, and they are \
             still used by: {}. Do not remove them or the `lunchroom` link until those are \
             uninstalled too.\n",
            setgid::PROGRAM,
            home.display(),
            users.join("; ")
        ),
    }
}

/// The settings at `path` with Lunchroom's hooks in place, and whether that changes them. Sessions
/// read their hooks when they start; with the same hooks, open sessions already run the program at
/// its path and take the new one at their next call.
fn plan(
    path: &Path,
    program: &Path,
    home: &Path,
    target: Target,
) -> Result<(Map<String, Value>, bool), Outcome> {
    let old = read_settings(path)?;
    match with_lunchroom_hooks(old.clone(), program, home, target, path) {
        Ok(new) => {
            let changed = new != old;
            Ok((new, changed))
        }
        Err(reason) => Err(Outcome::denied(
            &format!("{reason}; nothing was changed"),
            2,
        )),
    }
}

fn install_target(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    install_target_with(args, env, sys, &installed_program(env))
}

/// Where the program lives once installed for this process: the setgid program (1.1) when this
/// process runs as it, else `<LUNCHROOM_HOME>/bin/lunchroom` as in 1.0.x.
pub(crate) fn installed_program(env: &Env) -> PathBuf {
    if lunchroom_core::setgid::active() {
        PathBuf::from(lunchroom_core::setgid::PROGRAM)
    } else {
        env.lunchroom_home.join("bin").join("lunchroom")
    }
}

/// The folder `lunchroom install openclaw` writes Lunchroom's OpenClaw plugin to, and the one
/// `doctor` checks: the user's own folder (2026-11-14 real-world kit: doctor looked in the store,
/// which in setgid mode is `/var/lib/lunchroom/<uid>`, and reported a plugin it could not find).
pub(crate) fn openclaw_plugin_dir(env: &Env) -> PathBuf {
    user_folder(env).join("plugins").join("openclaw")
}

/// The user's own Lunchroom folder, which the user's processes can read (the OpenClaw plugin, the
/// old registry): `~/.lunchroom`. In setgid mode (1.1) Lunchroom's folder is the store, which they
/// cannot, so this is the home's folder; otherwise it is Lunchroom's folder, as in 1.0.x.
pub(crate) fn user_folder(env: &Env) -> PathBuf {
    if lunchroom_core::setgid::active() {
        env.home.join(".lunchroom")
    } else {
        env.lunchroom_home.clone()
    }
}

/// Writes the running program to `program`, keeping an earlier one beside it. The setgid program
/// at [`lunchroom_core::setgid::PROGRAM`] is root's and is put in place by the console set-up, so
/// it is never written here.
fn write_program(program: &Path, now: u64) -> std::io::Result<Option<Backup>> {
    if program.starts_with(lunchroom_core::setgid::PROGRAM_DIR) {
        return Ok(None);
    }
    let exe = fs::read(std::env::current_exe()?)?;
    replace(program, &exe, 0o755, now)
}

/// [`install_target`] with the program the hooks run named by the caller.
pub(crate) fn install_target_with(
    args: &[String],
    env: &Env,
    sys: &mut dyn System,
    program: &Path,
) -> Outcome {
    let (target, settings_path) = match args.first().map(String::as_str) {
        Some("claude-code") => (Target::ClaudeCode, env.claude_home.join("settings.json")),
        Some("codex") => (Target::Codex, env.codex_home.join("hooks.json")),
        Some("openclaw") => return install_plugin(true, env, sys, program),
        Some("hermes") => return install_plugin(false, env, sys, program),
        _ => {
            return Outcome::failed(
                "usage: lunchroom install claude-code|codex|openclaw|hermes",
                2,
            );
        }
    };
    let program = program.to_path_buf();
    // The path is written into shell commands, quoted with single quotes.
    if program
        .to_str()
        .is_none_or(|p| p.contains(['\'', '\n', '\0']))
    {
        return Outcome::failed(
            "the program's path holds a quote, a line break or a byte that is not text; nothing \
             was changed",
            2,
        );
    }
    if let Some(out) = refuse_unnamable(&settings_path) {
        return out;
    }
    if let Err(out) = plan(&settings_path, &program, &env.home, target) {
        return out;
    }
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    // Shown as written: an event's groups in order, so two hooks on one event both show.
    let mut added = Map::new();
    for (event, group) in lunchroom_hooks(&program, target, &settings_path) {
        if let Value::Array(groups) = added.entry(event).or_insert_with(|| json!([])) {
            groups.push(group);
        }
    }
    // I1.5: sessions the earlier Lunchroom had named are named again only on this same yes.
    let earlier: Vec<_> = earlier_names(env)
        .into_iter()
        .filter(|(name, _)| mail.named(name).is_none())
        .collect();
    let names = earlier_lines(&earlier);
    let link = std::env::var_os("PATH").and_then(|p| path_link(&env.home, &p, &program));
    let harness = target.name();
    let config_path = settings_path.with_file_name("config.toml");
    let terminals = match terminals_line(target, &config_path) {
        Ok(text) => text,
        Err(out) => return out,
    };
    let question = format!(
        "Lunchroom: add Lunchroom to {harness}? This writes Lunchroom's program and the \
         configuration that runs the guard, which only you may change.\nInstall the program at {} \
         and add these hooks to {}? \
         The old files are kept beside them as backups.{}{terminals}\nThe hooks it adds:\n{}\n{names}",
        program.display(),
        settings_path.display(),
        link_line(link.as_ref()),
        serde_json::to_string_pretty(&Value::Object(added)).unwrap_or_default()
    );
    if let Err(reason) = confirm_or_begin(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    // One transaction at a time reads, backs up and replaces the configuration; the lock is held
    // to the end of this function, after the user's yes (never while a question waits).
    let _transaction = match Transaction::begin(env) {
        Ok(t) => t,
        Err(e) => return Outcome::failed(&format!("the install did not start: {e}"), 1),
    };
    // The settings may have changed while the user read the question: what is written is computed
    // from them as they are now.
    let (new, hooks_changed) = match plan(&settings_path, &program, &env.home, target) {
        Ok(planned) => planned,
        Err(out) => return out,
    };
    if let Err(out) = setting_as_asked(target, &config_path, &terminals) {
        return out;
    }
    let config = fs::read_to_string(&config_path).unwrap_or_default();
    let setting_new = matches!(target, Target::Codex) && !codex_settings_are_off(&config);
    match install_files(
        &program,
        link.as_deref(),
        &settings_path,
        &new,
        env.now,
        target,
        hooks_changed,
    ) {
        Ok(mut report) => {
            if let Ok(store) = Store::<Mail>::open(&env.store_dir()) {
                if setting_new {
                    // Taken after the write: a session started during the question read no setting.
                    let boot = crate::process::boot_id().unwrap_or_default();
                    let now = crate::process::ticks_now().unwrap_or(u64::MAX);
                    let _ = store.update(|m| m.codex_setting_changed(&boot, now));
                }
                name_earlier(&store, &earlier, &mut report);
            }
            Outcome::ok(report)
        }
        Err(e) => Outcome::failed(&format!("the install stopped: {e}"), 1),
    }
}

/// Names again the sessions the earlier Lunchroom had named (I1.5).
fn name_earlier(
    store: &Store<Mail>,
    earlier: &[(String, lunchroom_core::identity::Identity)],
    report: &mut String,
) {
    for (name, who) in earlier {
        let registered = store
            .update(|m| {
                m.register(name, who, lunchroom_core::mail::Origin::User, None)
                    .is_ok()
            })
            .unwrap_or(false);
        if registered {
            let _ = writeln!(report, "named {name} {who}");
        }
    }
}

/// `config` with the two paths Codex runs no hook for turned off, everything else as it was: its
/// terminals (`unified_exec_tty = false` under `[features]`) and its hosted web search
/// (`web_search = "disabled"` at the top level, H1.4); `None` when it is not TOML or `features` is
/// not a table, which Codex would refuse too.
fn codex_tty_off(config: &str) -> Option<String> {
    let mut document = config.parse::<toml_edit::DocumentMut>().ok()?;
    document
        .entry("features")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()?
        .insert(CODEX_TTY, toml_edit::value(false));
    document.insert(CODEX_WEB, toml_edit::value(CODEX_WEB_DISABLED));
    Some(document.to_string())
}

/// The two settings of [`codex_tty_off`] are both in place in `config`.
fn codex_settings_are_off(config: &str) -> bool {
    codex_tty_setting(config) == Some(false) && codex_web_is_off(config)
}

/// What the install question says of the setting (H1), or why the file cannot take it.
fn terminals_line(target: Target, config_path: &Path) -> Result<String, Outcome> {
    if matches!(target, Target::ClaudeCode) {
        return Ok(String::new());
    }
    let config = fs::read_to_string(config_path).unwrap_or_default();
    if codex_settings_are_off(&config) {
        return Ok(String::new());
    }
    if codex_tty_off(&config).is_none() {
        return Err(Outcome::failed(
            &format!(
                "{} names features in a form Lunchroom cannot edit; set {CODEX_TTY} = false under \
                 [features] and {CODEX_WEB} = \"{CODEX_WEB_DISABLED}\" at the top there. Nothing was \
                 changed.",
                config_path.display()
            ),
            2,
        ));
    }
    let mut said = String::new();
    if codex_tty_setting(&config) != Some(false) {
        let _ = write!(
            said,
            "\nIt also sets {CODEX_TTY} = false under [features] in {}: Codex runs no hook for \
             what an agent types into an open terminal, so with terminals on a HELD agent could \
             get past the guard. It applies to every Codex session of this user, and those agents \
             then cannot keep a terminal program open. Start open Codex sessions anew afterwards.",
            config_path.display()
        );
    }
    if !codex_web_is_off(&config) {
        let _ = write!(
            said,
            "\nIt also sets {CODEX_WEB} = \"{CODEX_WEB_DISABLED}\" at the top of {}: Codex's hosted web \
             search reaches no hook, so with it on an agent could fetch what the guard never \
             sees. It applies to every Codex session of this user, and those agents then cannot \
             search the web. The earlier file is kept as a backup. Start open Codex sessions anew \
             afterwards.",
            config_path.display()
        );
    }
    Ok(said)
}

/// The yes covers the setting as the question stated it: a file that changed since is asked again.
fn setting_as_asked(target: Target, config_path: &Path, asked: &str) -> Result<(), Outcome> {
    if terminals_line(target, config_path)? == asked {
        return Ok(());
    }
    Err(Outcome::failed(
        "Codex's config.toml changed while you confirmed; nothing was changed. Run the install \
         again.",
        1,
    ))
}

/// Turns Codex's terminals and hosted web search off in `config_path`, keeping the old file as a
/// backup (H1.1, H1.4).
fn write_codex_tty_off(config_path: &Path, now: u64) -> std::io::Result<String> {
    let old = match fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if codex_settings_are_off(&old) {
        return Ok(String::new());
    }
    let new = codex_tty_off(&old).ok_or_else(|| {
        std::io::Error::other("Codex's config.toml names features in another form")
    })?;
    let mode = fs::metadata(config_path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    let backup = replace(config_path, new.as_bytes(), mode, now)?;
    let mut report = format!(
        "set {CODEX_TTY} = false and {CODEX_WEB} = \"{CODEX_WEB_DISABLED}\" in {}\n",
        config_path.display()
    );
    if let Some(b) = backup {
        let _ = writeln!(report, "  the earlier settings are kept at {}", b.display());
    }
    Ok(report)
}

fn install_files(
    program: &Path,
    link: Option<&Path>,
    settings_path: &Path,
    new: &Map<String, Value>,
    now: u64,
    target: Target,
    hooks_changed: bool,
) -> std::io::Result<String> {
    let earlier = earlier_stamp(program);
    let program_backup = write_program(program, now)?;
    let mut text =
        serde_json::to_string_pretty(&Value::Object(new.clone())).map_err(std::io::Error::other)?;
    text.push('\n');
    // A settings file that exists keeps its mode.
    let mode = fs::metadata(settings_path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    let settings_backup = replace(settings_path, text.as_bytes(), mode, now)?;
    let mut report = format!("installed {}: {}\n", program.display(), crate::cli::VERSION);
    if let Some(b) = program_backup {
        let _ = writeln!(
            report,
            "  the earlier program{earlier} is kept at {}",
            b.display()
        );
    }
    if let Some(l) = link {
        report.push_str(&link_program(l, program));
    }
    let _ = writeln!(
        report,
        "wrote the guard's hooks to {}",
        settings_path.display()
    );
    if let Some(b) = settings_backup {
        let _ = writeln!(report, "  the earlier settings are kept at {}", b.display());
    }
    let terminals = match target {
        Target::Codex => write_codex_tty_off(&settings_path.with_file_name("config.toml"), now)?,
        Target::ClaudeCode => String::new(),
    };
    report.push_str(&terminals);
    // The restart step's place in the list: before naming sessions, after Codex's trust step.
    let (harness, at, mut steps) = match target {
        Target::ClaudeCode => (
            "Claude Code",
            0,
            vec![
                "Each new Claude Code session asks whether to include it: type join (or join \
                 <name>) in it, and the name shows at the top of the window; type leave to take \
                 it out.",
                "Lunchroom writes no trust record: if Claude Code asks whether to trust a folder, \
                 that answer is yours.",
            ],
        ),
        Target::Codex => (
            "Codex",
            1,
            vec![
                "Open Codex and approve Lunchroom's hooks in /hooks (Trust). Codex runs no hook you \
                 have not trusted, and Lunchroom writes no trust record; until you approve them, \
                 every Codex session is UNGUARDED and receives nothing.",
                "Each new Codex session asks whether to include it: type join (or join <name>) \
                 in it; type leave to take it out.",
            ],
        ),
    };
    let restart = format!(
        "Start each {harness} session that Lunchroom should guard anew, so it reads these hooks \
         when it starts."
    );
    if hooks_changed || !terminals.is_empty() {
        steps.insert(at, &restart);
    } else {
        let _ = writeln!(
            report,
            "The hooks were already in place: open {harness} sessions run the new program from \
             their next call{}.",
            match target {
                Target::Codex => "; start anew any that `lunchroom status` shows UNGUARDED",
                Target::ClaudeCode => "",
            }
        );
    }
    report.push_str("Steps only you can do:\n");
    for (n, step) in steps.iter().enumerate() {
        let _ = writeln!(report, "{}. {step}", n + 1);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::terminal::fake;

    // Decision of 2026-11-14 ("Make the link"; cold first-time install: a fresh account had no
    // ~/.local/bin, no link was made and `lunchroom` was not found): with neither ~/.local/bin nor
    // ~/bin, the install names ~/.local/bin in its question, makes it, links the program there,
    // and says the folder is not on PATH yet and how to run it until it is; with either folder
    // there and none free on PATH, nothing is made.
    #[test]
    fn a_fresh_account_gets_a_local_bin_link() {
        let home = crate::test_target()
            .join("install-fresh-link")
            .join(std::process::id().to_string());
        let _ = fs::remove_dir_all(&home);
        assert!(fs::create_dir_all(&home).is_ok());
        let program = PathBuf::from("/usr/local/lib/lunchroom/lunchroom");
        let path = std::env::join_paths([Path::new("/usr/bin")]).unwrap_or_default();
        let link = home.join(".local/bin/lunchroom");
        assert_eq!(path_link(&home, &path, &program), Some(link.clone()));
        assert!(
            link_line(Some(&link)).contains(&format!(
                "It also makes {}",
                home.join(".local/bin").display()
            )),
            "{}",
            link_line(Some(&link))
        );
        let said = link_program(&link, &program);
        assert_eq!(fs::read_link(&link).ok(), Some(program.clone()));
        assert!(said.contains("is not on your PATH yet"), "{said}");
        assert!(
            said.contains("/usr/local/lib/lunchroom/lunchroom"),
            "{said}"
        );
        // ~/bin there, not on PATH: nothing is made.
        let _ = fs::remove_dir_all(home.join(".local"));
        assert!(fs::create_dir_all(home.join("bin")).is_ok());
        assert_eq!(path_link(&home, &path, &program), None);
        let _ = fs::remove_dir_all(&home);
    }

    // The program is linked by name from ~/.local/bin or ~/bin when one is on PATH, never over
    // another program of that name; one already linked to it counts as free.
    #[test]
    fn the_program_is_linked_from_a_home_folder_on_path() {
        let home = crate::test_target()
            .join("install-link")
            .join(std::process::id().to_string());
        let local = home.join(".local/bin");
        assert!(fs::create_dir_all(&local).is_ok());
        let program = home.join(".lunchroom/bin/lunchroom");
        let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap_or_default();
        let link = local.join("lunchroom");
        assert_eq!(
            path_link(&home, &path(&[Path::new("/usr/bin")]), &program),
            None
        );
        let on = path(&[Path::new("/usr/bin"), &local]);
        assert_eq!(path_link(&home, &on, &program), Some(link.clone()));
        assert!(
            link_program(&link, &program).starts_with("linked "),
            "{link:?}"
        );
        // A link that cannot be made is reported with the program's path, not raised.
        let lost = link_program(&local.join("no-such-folder/lunchroom"), &program);
        assert!(
            lost.contains("was not linked to it") && lost.contains(&program.display().to_string()),
            "{lost}"
        );
        assert_eq!(fs::read_link(&link).ok(), Some(program.clone()));
        assert_eq!(path_link(&home, &on, &program), Some(link.clone()));
        let _ = fs::remove_file(&link);
        assert!(fs::write(&link, "another").is_ok());
        assert_eq!(path_link(&home, &on, &program), None);
        let _ = fs::remove_dir_all(&home);
    }

    /// The user, in a terminal of their own, who has set a passphrase, giving `answer`: a yes
    /// stands for typing the passphrase, anything else for typing a wrong one.
    struct UserAt(&'static str);

    impl System for UserAt {
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
            Some(self.0.to_owned())
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            Some(fake::typed(self.0))
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            Ok([6; lunchroom_core::passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// A user who has set a passphrase, gives each answer in turn (a yes stands for typing the
    /// passphrase, anything else for a wrong one) and whose questions and prompts are kept.
    struct Answers {
        answers: Vec<&'static str>,
        asked: Vec<String>,
    }

    impl System for Answers {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            UserAt("").ancestors()
        }
        fn own_tty(&self) -> u64 {
            UserAt("").own_tty()
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            UserAt("").tty_of(key)
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            (!self.answers.is_empty()).then(|| self.answers.remove(0).to_owned())
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.ask(prompt).map(|answer| fake::typed(&answer))
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            Ok([6; lunchroom_core::passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    // The program is stamped with its build; the stamp is read back from the program's own bytes,
    // and from an earlier program's, while text that only starts like one is not taken for it.
    #[test]
    fn a_program_carries_its_build_stamp() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            crate::cli::VERSION == format!("lunchroom {version}")
                || crate::cli::VERSION.starts_with(&format!("lunchroom {version} (")),
            "{}",
            crate::cli::VERSION
        );
        let own = std::env::current_exe()
            .and_then(fs::read)
            .unwrap_or_default();
        assert!(
            own.windows(crate::cli::VERSION.len())
                .any(|w| w == crate::cli::VERSION.as_bytes())
        );
        for (bytes, stamp) in [
            (
                &b"xlunchroom 1.0.0 (61861fb)usage"[..],
                Some("lunchroom 1.0.0 (61861fb)"),
            ),
            (
                b"lunchroom 1.0.0 (61861fb, modified)",
                Some("lunchroom 1.0.0 (61861fb, modified)"),
            ),
            (b"lunchroom sendlunchroom 1.2.3", Some("lunchroom 1.2.3")),
            (b"lunchroom 1.0.0 (not a commit)", Some("lunchroom 1.0.0")),
            (b"lunchroom 1.0 (61861fb)", None),
            (b"lunchroom send lunchroom status", None),
            (b"", None),
        ] {
            assert_eq!(stamp_in(bytes).as_deref(), stamp, "{bytes:?}");
        }
    }

    // The install report names the build it installed and the one it kept as a backup.
    #[test]
    fn the_install_names_the_build_installed_and_the_one_replaced() {
        let s = Scratch::new();
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let _ = fs::create_dir_all(s.env.lunchroom_home.join("bin"));
        let _ = fs::write(&program, b"\x7fELF lunchroom 1.0.0 (61861fb)\0");
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.stdout.contains(&format!(
                "installed {}: {}\n",
                program.display(),
                crate::cli::VERSION
            )),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout
                .contains("the earlier program, lunchroom 1.0.0 (61861fb), is kept at"),
            "{}",
            out.stdout
        );
        // A program from before the stamp is named only as the earlier program.
        let _ = fs::write(&program, b"lunchroom send");
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert!(
            again.stdout.contains("the earlier program is kept at"),
            "{}",
            again.stdout
        );
    }

    // §6::16: the install asks the hold mode, offering Strict, Balanced and Relaxed and
    // recommending Balanced; the answer is the mode.
    #[test]
    fn the_install_asks_the_hold_mode() {
        let s = Scratch::new();
        let mut user = Answers {
            answers: vec!["yes\n", "1\n"],
            asked: Vec::new(),
        };
        let out = install(&["claude-code".to_owned()], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let question = user.asked.last().cloned().unwrap_or_default();
        for option in ["1. Relaxed", "2. Balanced (recommended)", "3. Strict"] {
            assert!(question.contains(option), "{question}");
        }
        assert!(out.stdout.contains("hold mode: Relaxed"), "{}", out.stdout);
        let mode = Store::<Mail>::open(&s.env.store_dir()).and_then(|st| st.read());
        assert_eq!(
            mode.ok().map(|m| m.hold_mode()),
            Some(lunchroom_core::policy::HoldMode::Relaxed)
        );
    }

    /// The names in `dir`, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // §13::4 (H1.4, "keeps a backup"): a backup is never overwritten and never written
    // through a link. A name taken by a file, or by a link to another file, is skipped for the next
    // free one; the other files are as they were; the backup holds the old bytes with the old
    // file's mode.
    #[test]
    fn a_backup_never_overwrites_or_follows_a_taken_name() {
        let s = Scratch::new();
        let dir = s.env.home.join("backups");
        assert!(fs::create_dir_all(&dir).is_ok());
        let target = dir.join("settings.json");
        assert!(fs::write(&target, "old state").is_ok());
        assert!(fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).is_ok());
        let base = format!("settings.json.lunchroom-backup-{}", stamp(s.env.now));
        assert!(fs::write(dir.join(&base), "kept").is_ok());
        let victim = dir.join("victim");
        assert!(fs::write(&victim, "victim").is_ok());
        assert!(std::os::unix::fs::symlink(&victim, dir.join(format!("{base}-1"))).is_ok());
        let backup = back_up(&target, s.env.now);
        assert_eq!(
            backup.as_ref().ok().cloned().flatten(),
            Some(dir.join(format!("{base}-2"))),
            "{backup:?}"
        );
        assert_eq!(
            fs::read_to_string(dir.join(&base)).ok().as_deref(),
            Some("kept")
        );
        assert_eq!(fs::read_to_string(&victim).ok().as_deref(), Some("victim"));
        let made = dir.join(format!("{base}-2"));
        assert_eq!(fs::read_to_string(&made).ok().as_deref(), Some("old state"));
        assert_eq!(
            fs::metadata(&made)
                .map(|m| m.permissions().mode() & 0o777)
                .ok(),
            Some(0o600)
        );
    }

    // §13::4: two installs in the same second take two backup names; neither loses its
    // backup to the other (names are taken by exclusive creation, not by a look beforehand).
    #[test]
    fn concurrent_backups_take_different_names() {
        let s = Scratch::new();
        let dir = s.env.home.join("concurrent");
        assert!(fs::create_dir_all(&dir).is_ok());
        let target = dir.join("settings.json");
        assert!(fs::write(&target, "state").is_ok());
        let made: Vec<Option<PathBuf>> = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| back_up(&target, s.env.now).ok().flatten()))
                .collect();
            runs.into_iter()
                .map(|run| run.join().ok().flatten())
                .collect()
        });
        let mut paths: Vec<PathBuf> = made.into_iter().flatten().collect();
        assert_eq!(paths.len(), 8);
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), 8);
        for path in &paths {
            assert_eq!(fs::read_to_string(path).ok().as_deref(), Some("state"));
        }
    }

    // §13::4: the temporary file is created exclusively. A link planted at its
    // predictable name is not written through (its target keeps its bytes) and is left alone; the
    // new file takes the next free name and replaces the target; no other temporary file is left.
    #[test]
    fn a_planted_temporary_name_is_not_written_through() {
        let s = Scratch::new();
        let dir = s.env.home.join("temporary");
        assert!(fs::create_dir_all(&dir).is_ok());
        let target = dir.join("settings.json");
        let victim = dir.join("victim");
        assert!(fs::write(&victim, "victim").is_ok());
        let planted = dir.join(format!(
            "settings.json.lunchroom-new-{}",
            std::process::id()
        ));
        assert!(std::os::unix::fs::symlink(&victim, &planted).is_ok());
        assert!(write_beside(&target, b"new state", 0o600).is_ok());
        assert_eq!(fs::read_to_string(&victim).ok().as_deref(), Some("victim"));
        assert_eq!(
            fs::read_to_string(&target).ok().as_deref(),
            Some("new state")
        );
        assert!(fs::symlink_metadata(&planted).is_ok_and(|m| m.is_symlink()));
        let left: Vec<String> = names_in(&dir)
            .into_iter()
            .filter(|n| n.contains(".lunchroom-new"))
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    // §13::4: a target that is not a plain file once resolved (a folder, a link that
    // leads nowhere) is refused with nothing written, no backup made and the target as it was.
    #[test]
    fn a_target_of_an_unexpected_type_is_refused() {
        let s = Scratch::new();
        let dir = s.env.home.join("types");
        assert!(fs::create_dir_all(&dir).is_ok());
        let folder = dir.join("folder");
        assert!(fs::create_dir_all(&folder).is_ok());
        assert!(replace(&folder, b"x", 0o600, s.env.now).is_err());
        assert!(folder.is_dir());
        let missing = dir.join("missing");
        let dangling = dir.join("dangling");
        assert!(std::os::unix::fs::symlink(&missing, &dangling).is_ok());
        assert!(replace(&dangling, b"x", 0o600, s.env.now).is_err());
        assert!(fs::symlink_metadata(&dangling).is_ok_and(|m| m.is_symlink()));
        assert!(!missing.exists());
        assert_eq!(names_in(&dir), ["dangling", "folder"]);
        // A plain file is replaced, and a plain absent one created, as before.
        let plain = dir.join("plain");
        assert!(matches!(replace(&plain, b"x", 0o600, s.env.now), Ok(None)));
        assert!(matches!(
            replace(&plain, b"y", 0o600, s.env.now),
            Ok(Some(_))
        ));
    }

    // §13::4: configuration transactions are serialized. While one holds the lock, a
    // second waits and then reports that it changed nothing (a timeout, not a wait for ever); once
    // the first lets go, the second proceeds.
    #[test]
    fn a_second_transaction_waits_then_reports_it_changed_nothing() {
        let s = Scratch::new();
        let first = Transaction::begin(&s.env);
        assert!(first.is_ok());
        let second = Transaction::begin_within(&s.env, std::time::Duration::from_millis(50));
        assert_eq!(
            second.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::TimedOut)
        );
        drop(first);
        assert!(Transaction::begin_within(&s.env, std::time::Duration::from_millis(50)).is_ok());
    }

    // §13::4, A1.3: an install, an uninstall and a plugin install that cannot take the
    // transaction lock change nothing (no program, no settings, no backup) and fail visibly.
    #[test]
    fn a_command_that_cannot_take_the_transaction_changes_nothing() {
        let s = Scratch::new();
        let installed = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(installed.code, 0, "{}", installed.stderr);
        let settings = s.env.home.join(".claude/settings.json");
        let state = || {
            (
                fs::read_to_string(&settings).ok(),
                fs::read(s.env.lunchroom_home.join("bin/lunchroom")).ok(),
                names_in(&s.env.home.join(".claude")),
                names_in(&s.env.lunchroom_home.join("bin")),
                names_in(&s.env.hermes_home),
                names_in(&s.env.lunchroom_home.join("plugins")),
            )
        };
        let before = state();
        let hold = Transaction::begin(&s.env);
        assert!(hold.is_ok());
        for harness in ["claude-code", "hermes", "openclaw"] {
            let out = install(&[harness.to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 1, "{harness}: {}", out.stderr);
            assert!(
                out.stderr
                    .contains("another Lunchroom install or uninstall"),
                "{}",
                out.stderr
            );
        }
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 1, "{}", out.stderr);
        assert!(
            out.stderr
                .contains("another Lunchroom install or uninstall"),
            "{}",
            out.stderr
        );
        assert_eq!(
            state(),
            before,
            "no program, settings, backup or plugin changed"
        );
        drop(hold);
    }

    // §13::4 (H1.4, "Every other hook stays"): a hook is Lunchroom's only by an exact
    // identity: the installed program's path (quoted or not, or a link that resolves to it) or the
    // default install location, each with one of the arguments Lunchroom's hooks give. Another
    // application laid out as `~/.<name>/bin/<name>`, a program that is merely named `lunchroom`,
    // another home's default location, Lunchroom's program with arguments Lunchroom's hooks never
    // give it, or another program in its folder stay.
    #[test]
    fn only_exact_known_identities_are_lunchroom_hooks() {
        let program = Path::new("/srv/l/bin/lunchroom");
        let home = Path::new("/home/u");
        let hook = |command: &str| json!({"type": "command", "command": command});
        for owned in [
            "/srv/l/bin/lunchroom guard pretool",
            "'/srv/l/bin/lunchroom' listen",
            "~/.lunchroom/bin/lunchroom guard prompt",
            "$HOME/.lunchroom/bin/lunchroom listen --once",
            "'/home/u/.lunchroom/bin/lunchroom' listen",
            "/home/u/.lunchroom/bin/lunchroom guard start",
            "~/.lunchroom/bin/lunchroom   guard   posttool",
        ] {
            assert!(is_lunchroom_hook(&hook(owned), program, home), "{owned}");
        }
        for other in [
            "/home/u/.other/bin/other listen",
            "/home/u/.other/bin/other guard pretool",
            "~/.other/bin/other listen --once",
            "$HOME/.other/bin/other listen",
            "/opt/lunchroom guard pretool",
            "/gone/lunchroom listen",
            "'/gone dir/lunchroom' listen",
            "/home/u/.lunchroom/bin/other listen",
            "/home/u/.lunchroom/lunchroom listen",
            "/home/you/.lunchroom/bin/lunchroom listen",
            "/home/u/.lunchroom/bin/lunchroom serve",
            // The program itself, run with arguments Lunchroom's hooks never give it.
            "/srv/l/bin/lunchroom",
            "/srv/l/bin/lunchroom status",
            "'/srv/l/bin/lunchroom' send me hi",
            "/srv/l/bin/lunchroom guard pretool --extra",
            "/srv/l/bin/lunchroom listen && other",
            "~/.lunchroom/bin/lunchroom approve 1",
            "/srv/l/bin/lunchroomX listen",
        ] {
            assert!(!is_lunchroom_hook(&hook(other), program, home), "{other}");
        }
    }

    // §13::4: with no harness named, a failure for one harness is reported as one and
    // the exit status is the failure's, even though another harness was installed; each harness's
    // outcome has its own line; the steps after an install (the hold mode) still run for the one
    // that was installed. Only a user's no is not a failure: with it and an install, the status is 0.
    #[test]
    fn a_partly_failed_install_reports_each_outcome_and_fails() {
        let s = Scratch::new();
        // Codex's folder holds a folder where hooks.json should be: its settings cannot be read.
        assert!(fs::create_dir_all(s.env.codex_home.join("hooks.json")).is_ok());
        // F112-AC2: the passphrase once, then the yes for Claude Code (Codex fails before its
        // question), then the hold mode.
        let mut user = Answers {
            answers: vec!["yes\n", "yes\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&[], &s.env, &mut user);
        assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
        assert!(
            out.stdout.contains("Claude Code: installed"),
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("Codex: failed"), "{}", out.stdout);
        assert!(
            out.stderr.contains("the install failed for: Codex"),
            "{}",
            out.stderr
        );
        assert!(
            lunchroom_commands(&s.settings(), "PreToolUse").len() == 1,
            "{}",
            s.settings()
        );
        assert!(
            user.asked
                .last()
                .is_some_and(|q| q.contains("Choose 1, 2 or 3")),
            "{:?}",
            user.asked
        );
        assert!(out.stdout.contains("hold mode: "), "{}", out.stdout);
        // A user's no beside an install is not a failure (F112-AC2: the passphrase once, then a
        // no for Claude Code and a yes for Codex).
        let t = Scratch::new();
        assert!(fs::create_dir_all(&t.env.codex_home).is_ok());
        let mut user = Answers {
            answers: vec!["yes\n", "no\n", "yes\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&[], &t.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            out.stdout.contains("Claude Code: not installed"),
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("Codex: installed"), "{}", out.stdout);
    }

    /// A scratch home where all four harnesses are found.
    fn four_found() -> Scratch {
        let s = Scratch::new();
        for dir in [
            s.env.codex_home.clone(),
            s.env.home.join(".openclaw"),
            s.env.hermes_home.clone(),
        ] {
            assert!(fs::create_dir_all(dir).is_ok());
        }
        assert_eq!(found_harnesses(&s.env).len(), 4);
        s
    }

    const FOUR: [&str; 4] = ["Claude Code", "Codex", "OpenClaw", "Hermes"];

    // F112-AC2: with a passphrase set and all four harnesses found, the passphrase is asked once,
    // then each harness is offered with a plain yes, asking no passphrase again; only the yes are
    // installed (the 1.1.1 dogfood upgrade asked the passphrase four times).
    #[test]
    fn f112_ac2_four_harnesses_are_offered_after_one_passphrase() {
        let s = four_found();
        let mut user = Answers {
            answers: vec!["yes\n", "yes\n", "no\n", "yes\n", "no\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&[], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let passphrase: Vec<&String> = user
            .asked
            .iter()
            .filter(|q| q.ends_with("Passphrase: "))
            .collect();
        assert_eq!(passphrase.len(), 1, "{:?}", user.asked);
        assert!(
            user.asked[0].starts_with("Found: Claude Code, Codex, OpenClaw, Hermes\n")
                && user.asked[0].ends_with("Passphrase: "),
            "{}",
            user.asked[0]
        );
        for (i, name) in FOUR.iter().enumerate() {
            let offer = &user.asked[i + 1];
            assert!(
                offer.contains(&format!("add Lunchroom to {name}?"))
                    && offer.ends_with(&format!("Install for {name}? Type yes: ")),
                "{offer}"
            );
        }
        for name in ["Claude Code", "OpenClaw"] {
            assert!(
                out.stdout.contains(&format!("{name}: installed")),
                "{}",
                out.stdout
            );
        }
        for name in ["Codex", "Hermes"] {
            assert!(
                out.stdout
                    .contains(&format!("{name}: not installed (you answered no)")),
                "{}",
                out.stdout
            );
        }
        assert!(!s.env.codex_home.join("hooks.json").exists());
        assert_eq!(lunchroom_commands(&s.settings(), "PreToolUse").len(), 1);
    }

    // F112-AC2: a wrong passphrase, asked once, installs none of the harnesses found and offers
    // none of them.
    #[test]
    fn f112_ac2_a_wrong_passphrase_installs_none() {
        let s = four_found();
        let mut user = Answers {
            answers: vec!["no\n", "yes\n", "yes\n", "yes\n", "yes\n"],
            asked: Vec::new(),
        };
        let out = install(&[], &s.env, &mut user);
        assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
        assert_eq!(user.asked.len(), 1, "{:?}", user.asked);
        assert!(
            out.stderr.contains("passphrase was wrong"),
            "{}",
            out.stderr
        );
        assert!(!s.env.codex_home.join("hooks.json").exists());
        assert_eq!(
            lunchroom_commands(&s.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
    }

    // F112-AC3: an install run with the build already installed (the program typed by name, or an
    // identical copy run by its path) says so before its question; a different build installed, or
    // none, gets no notice.
    #[test]
    fn f112_ac3_an_install_of_the_build_already_installed_says_so() {
        let notice = "This is the build already installed (";
        let running = std::env::current_exe()
            .and_then(fs::read)
            .unwrap_or_default();
        assert_ne!(running.len(), 0);
        let program = |s: &Scratch| s.env.lunchroom_home.join("bin").join("lunchroom");
        // None installed.
        let none = Scratch::new();
        assert!(!program(&none).exists());
        let mut user = Answers {
            answers: vec!["yes\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&["claude-code".to_owned()], &none.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(!user.asked[0].contains(notice), "{}", user.asked[0]);
        // The same build installed: the first question opens with the notice, naming the build.
        let mut user = Answers {
            answers: vec!["yes\n"],
            asked: Vec::new(),
        };
        let out = install(&["claude-code".to_owned()], &none.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            user.asked[0].starts_with(&format!("{notice}{})", crate::cli::VERSION)),
            "{}",
            user.asked[0]
        );
        assert!(
            user.asked[0].contains("./lunchroom install"),
            "{}",
            user.asked[0]
        );
        assert_eq!(
            user.asked.iter().filter(|q| q.contains(notice)).count(),
            1,
            "{:?}",
            user.asked
        );
        // A different build installed: no notice.
        let other = Scratch::new();
        assert!(fs::create_dir_all(other.env.lunchroom_home.join("bin")).is_ok());
        assert!(fs::write(program(&other), b"another build").is_ok());
        assert!(fs::set_permissions(program(&other), fs::Permissions::from_mode(0o755)).is_ok());
        assert_eq!(same_build_notice(&program(&other), &running), None);
        let mut user = Answers {
            answers: vec!["yes\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&["claude-code".to_owned()], &other.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(!user.asked[0].contains(notice), "{}", user.asked[0]);
        assert!(same_build_notice(&program(&none), &running).is_some());
    }

    // F112-AC4: after each backup an install makes, only the three newest backups of that file are
    // kept; a link or a file of another target's backup form is never removed, nor is a name that
    // is not of the backup form.
    #[test]
    fn f112_ac4_only_the_three_newest_backups_are_kept() {
        let s = Scratch::new();
        let dir = s.env.home.join("pruned");
        assert!(fs::create_dir_all(&dir).is_ok());
        let target = dir.join("lunchroom");
        let victim = dir.join("victim");
        assert!(fs::write(&victim, "victim").is_ok());
        let link = dir.join(format!("lunchroom.lunchroom-backup-{}", stamp(1)));
        assert!(std::os::unix::fs::symlink(&victim, &link).is_ok());
        let other = dir.join(format!("settings.json.lunchroom-backup-{}", stamp(2)));
        assert!(fs::write(&other, "other target").is_ok());
        let odd = dir.join("lunchroom.lunchroom-backup-notastamp");
        assert!(fs::write(&odd, "odd").is_ok());
        assert!(fs::write(&target, "v0").is_ok());
        let backups = |dir: &Path| -> Vec<String> {
            names_in(dir)
                .into_iter()
                .filter(|n| {
                    n.starts_with("lunchroom.lunchroom-backup-2")
                        && fs::symlink_metadata(dir.join(n)).is_ok_and(|m| m.is_file())
                })
                .collect()
        };
        // No earlier backup, one, two, three: none removed.
        for i in 1..=3_u64 {
            let made = replace(&target, format!("v{i}").as_bytes(), 0o755, s.env.now + i);
            assert!(made.is_ok_and(|b| b.is_some_and(|b| b.unremoved.is_empty())));
            assert_eq!(backups(&dir).len(), usize::try_from(i).unwrap_or(0));
        }
        // A fourth removes the oldest; ten in all keep the newest three.
        for i in 4..=10_u64 {
            assert!(replace(&target, format!("v{i}").as_bytes(), 0o755, s.env.now + i).is_ok());
            assert_eq!(backups(&dir).len(), 3, "{:?}", names_in(&dir));
        }
        let kept: Vec<String> = backups(&dir)
            .iter()
            .filter_map(|n| fs::read_to_string(dir.join(n)).ok())
            .collect();
        assert_eq!(kept, ["v7", "v8", "v9"]);
        // Two backups in the same second: `-<n>` orders after the plain stamp.
        assert!(replace(&target, b"v11", 0o755, s.env.now + 20).is_ok());
        assert!(replace(&target, b"v12", 0o755, s.env.now + 20).is_ok());
        let kept: Vec<String> = backups(&dir)
            .iter()
            .filter_map(|n| fs::read_to_string(dir.join(n)).ok())
            .collect();
        assert_eq!(kept, ["v9", "v10", "v11"]);
        assert!(fs::symlink_metadata(&link).is_ok_and(|m| m.is_symlink()));
        assert_eq!(fs::read_to_string(&victim).ok().as_deref(), Some("victim"));
        assert_eq!(
            fs::read_to_string(&other).ok().as_deref(),
            Some("other target")
        );
        assert_eq!(fs::read_to_string(&odd).ok().as_deref(), Some("odd"));
    }

    // F112-AC4: a backup that cannot be removed is reported with the backup made, and the replace
    // still succeeds.
    #[test]
    fn f112_ac4_a_backup_that_cannot_be_removed_is_reported() {
        let s = Scratch::new();
        let dir = s.env.home.join("stuck");
        assert!(fs::create_dir_all(&dir).is_ok());
        let target = dir.join("lunchroom");
        assert!(fs::write(&target, "v0").is_ok());
        for i in 1..=3_u64 {
            assert!(replace(&target, format!("v{i}").as_bytes(), 0o755, s.env.now + i).is_ok());
        }
        // A fourth, oldest backup, in a folder no one may write: it cannot be removed.
        let oldest = dir.join(format!("lunchroom.lunchroom-backup-{}", stamp(1)));
        assert!(fs::write(&oldest, "old").is_ok());
        assert!(fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).is_ok());
        let writable = fs::write(dir.join("probe"), "x").is_ok();
        let unremoved = prune_backups(&target);
        assert!(fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).is_ok());
        if writable {
            // Run as root, where the folder's mode does not stop a removal: nothing to show.
            return;
        }
        assert!(oldest.exists());
        assert_eq!(unremoved.len(), 1, "{unremoved:?}");
        assert!(
            unremoved[0].contains("lunchroom.lunchroom-backup-"),
            "{unremoved:?}"
        );
        let shown = Backup {
            path: dir.join("made"),
            unremoved,
        }
        .display();
        assert!(
            shown.contains("\n  an older backup could not be removed: "),
            "{shown}"
        );
    }

    // F112-AC2 as F2 settles it (2026-11-09: the console's rule replaces the 1.1.2
    // first-install yes; spec section 17, "a typed `yes` confirms nothing"): with no passphrase set
    // (the first install), each harness found is confirmed on its own through the root program's
    // prompt, no typed yes is asked, no passphrase is asked before them, and a refused root prompt
    // installs nothing.
    #[test]
    fn f112_ac2_the_first_install_confirms_each_harness_through_the_root_prompt() {
        let s = four_found();
        let mut user = NewUser::new(&["2\n"], &[FIRST, FIRST]);
        let out = install(&[], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            user.asked[0].starts_with("Found: Claude Code, Codex, OpenClaw, Hermes\n"),
            "{}",
            user.asked[0]
        );
        for (i, name) in FOUR.iter().enumerate() {
            let question = &user.asked[i];
            assert!(
                question.contains(&format!("add Lunchroom to {name}?")),
                "{question}"
            );
            assert!(!question.contains("Type yes"), "{question}");
            assert!(
                out.stdout.contains(&format!("{name}: installed")),
                "{}",
                out.stdout
            );
        }
        assert!(user.prompts.iter().all(|p| !p.ends_with("Passphrase: ")));
        // The root prompt refused: nothing is installed.
        let t = four_found();
        let mut refused = NewUser::new(&["2\n"], &[FIRST, FIRST]);
        refused.root = Err("the root program's prompt was refused".to_owned());
        let out = install(&[], &t.env, &mut refused);
        assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
        assert_eq!(
            lunchroom_commands(&t.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
        assert!(!t.env.codex_home.join("hooks.json").exists());
    }

    // §13::4: uninstalling one harness does not tell the user to delete the shared
    // program and store while another harness still uses them: it names each one that does (hooks
    // still in Claude Code's or Codex's configuration, plugin files still written); the advice to
    // delete comes only when none is left.
    #[test]
    fn uninstall_names_the_harnesses_still_using_the_shared_files() {
        let s = Scratch::new();
        for harness in ["claude-code", "codex"] {
            let out = install(&[harness.to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 0, "{harness}: {}", out.stderr);
        }
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.stdout.contains("still used by: Codex"),
            "{}",
            out.stdout
        );
        assert!(
            !out.stdout.contains(": delete that folder"),
            "{}",
            out.stdout
        );
        let plugin = s.env.hermes_home.join("plugins/lunchroom/__init__.py");
        assert!(fs::create_dir_all(plugin.parent().unwrap_or(&s.env.home)).is_ok());
        assert!(fs::write(&plugin, "# Lunchroom's plugin").is_ok());
        let out = uninstall(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.stdout.contains("still used by: Hermes"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("Codex ("), "{}", out.stdout);
        assert!(
            !out.stdout.contains(": delete that folder"),
            "{}",
            out.stdout
        );
        assert!(fs::remove_file(&plugin).is_ok());
        // Nothing is left that uses them: the uninstall of a configuration with no hooks says so
        // without advice, and the next uninstall that removes the last hooks gives the advice.
        let again = install(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(again.code, 0, "{}", again.stderr);
        let last = uninstall(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(last.code, 0, "{}", last.stderr);
        assert!(
            last.stdout.contains(": delete that folder"),
            "{}",
            last.stdout
        );
        assert!(!last.stdout.contains("still used by"), "{}", last.stdout);
    }

    // With the store wall the program and the messages are root's: the uninstall names them as
    // root's and never tells the developer to delete a folder they cannot delete.
    #[test]
    fn uninstall_with_the_store_wall_names_root_s_program_and_store() {
        let home = Path::new("/var/lib/lunchroom/1001");
        let alone = what_stays(home, &[]);
        assert!(!alone.contains("delete that folder"), "{alone}");
        assert!(
            alone.contains("/usr/local/lib/lunchroom/lunchroom"),
            "{alone}"
        );
        assert!(alone.contains("/var/lib/lunchroom/1001"), "{alone}");
        assert!(alone.contains("root's"), "{alone}");
        assert!(alone.contains("lunchroom uninstall console"), "{alone}");
        let shared = what_stays(home, &["Codex (/home/u/.codex/hooks.json)".to_owned()]);
        assert!(shared.contains("still used by: Codex"), "{shared}");
        assert!(!shared.contains("delete that folder"), "{shared}");
        assert!(
            shared.contains("/usr/local/lib/lunchroom/lunchroom"),
            "{shared}"
        );
        // Without the wall the folder is the developer's own, and the advice stays as it was.
        let own = what_stays(Path::new("/home/u/.lunchroom"), &[]);
        assert!(
            own.contains("stay in /home/u/.lunchroom: delete that folder"),
            "{own}"
        );
        assert!(!own.contains("root's"), "{own}");
    }

    // §13::4: `lunchroom install` with no harness named offers each supported harness
    // whose folder exists, in turn, and installs only those the user answers yes to; with none
    // found it changes nothing and says how to name one.
    #[test]
    fn install_offers_each_harness_found_and_installs_only_the_yes() {
        let mut s = Scratch::new();
        // Codex's folder as `CODEX_HOME` names it, with no `~/.codex`.
        s.env.codex_home = s.root.join("codex-elsewhere");
        let _ = fs::create_dir_all(&s.env.codex_home);
        assert!(!s.env.home.join(".codex").exists());
        // A passphrase is set (F112-AC2): it is asked once, then each harness is offered in turn.
        let mut user = Answers {
            answers: vec!["yes\n", "no\n", "yes\n", "2\n"],
            asked: Vec::new(),
        };
        let out = install(&[], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        // What was found is read before the first question, and only there.
        assert!(
            user.asked[0].starts_with("Found: Claude Code, Codex\n"),
            "{}",
            user.asked[0]
        );
        assert!(user.asked[0].ends_with("Passphrase: "), "{}", user.asked[0]);
        assert!(!user.asked[1].contains("Found:"), "{}", user.asked[1]);
        assert!(!user.asked[2].contains("Found:"), "{}", user.asked[2]);
        assert!(!out.stdout.contains("Found:"), "{}", out.stdout);
        assert!(
            user.asked[1].contains("add Lunchroom to Claude Code?")
                && user.asked[1].ends_with("Install for Claude Code? Type yes: "),
            "{}",
            user.asked[1]
        );
        assert!(
            user.asked[2].contains("add Lunchroom to Codex?")
                && user.asked[2].ends_with("Install for Codex? Type yes: "),
            "{}",
            user.asked[2]
        );
        assert!(
            out.stdout
                .contains("Claude Code: not installed (you answered no)"),
            "{}",
            out.stdout
        );
        assert_eq!(
            lunchroom_commands(&s.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
        assert!(s.env.codex_home.join("hooks.json").exists());

        let none = Scratch::new();
        let _ = fs::remove_dir_all(none.env.home.join(".claude"));
        let out = install(&[], &none.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 2);
        assert!(
            out.stderr.contains("lunchroom install claude-code"),
            "{}",
            out.stderr
        );
        assert!(!none.env.lunchroom_home.join("bin/lunchroom").exists());
    }

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A scratch home with an existing Claude Code settings file, removed afterwards.
    struct Scratch {
        root: PathBuf,
        env: Env,
    }

    const OLD_SETTINGS: &str = r#"{
  "model": "opus",
  "hooks": {
    "PreToolUse": [{"hooks": [{"type": "command", "command": "/usr/local/bin/my-hook"}]}]
  },
  "permissions": {"deny": ["Read(./.env)"]}
}
"#;

    impl Scratch {
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let root = crate::test_target()
                .join("install-tests")
                .join(format!("{}-{n}", std::process::id()));
            let home = root.join("home");
            let _ = fs::create_dir_all(home.join(".claude"));
            let _ = fs::write(home.join(".claude/settings.json"), OLD_SETTINGS);
            let env = Env {
                lunchroom_home: home.join(".lunchroom"),
                claude_home: home.join(".claude"),
                codex_home: home.join(".codex"),
                hermes_home: home.join(".hermes"),
                home,
                claude_pid: None,
                attended: None,
                entrypoint: None,
                claude_session: None,
                codex_thread: None,
                hermes_session: None,
                lunchroom_session: None,
                now: 1_793_023_200,
            };
            Self { root, env }
        }

        fn settings(&self) -> Value {
            fs::read_to_string(self.env.home.join(".claude/settings.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// The tool-call hook's command as an install of `program` writes it into `file` (H1.5).
    fn pretool_for(program: &Path, file: &Path) -> String {
        pretool_command(&shell_word(&program.display().to_string()), file)
    }

    fn lunchroom_commands(settings: &Value, event: &str) -> Vec<Value> {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter(|h| {
                h["command"]
                    .as_str()
                    .is_some_and(|c| c.contains("/.lunchroom/bin/lunchroom "))
            })
            .collect()
    }

    // §13::4, §13::5: on the user's yes the installer writes the program and the
    // settings beside the old ones, keeps backups, keeps every other key and hook, adds the guard's
    // hooks and the listener, tells the user the steps only they can do, and writes
    // no trust record; run again, it replaces its own hooks instead of adding more.
    // §13::4, §13::5 (Codex): `lunchroom install codex` writes the guard hooks and the
    // listener to `<CODEX_HOME>/hooks.json` beside the old file, keeping a backup and every other
    // hook, writes no trust record, and tells the user to approve the hooks in Codex's `/hooks`.
    #[test]
    fn codex_terminals_are_turned_off_and_every_other_line_stays() {
        let on = "model = \"x\" # keep\n\n[features]\nunified_exec_tty = true\nhooks = true\n\n[a]\nb = 1\n";
        let new = codex_tty_off(on).unwrap_or_default();
        for kept in ["model = \"x\" # keep\n", "hooks = true\n", "[a]\nb = 1\n"] {
            assert!(new.contains(kept), "{new}");
        }
        assert!(!new.contains("= true\nhooks"), "{new}");
        // Every way TOML writes it, read back as TOML and as the guard's evidence requires.
        for config in [
            "",
            "model = \"x\"\n",
            "[features]\n",
            "[ features ]\nunified_exec_tty = true\n",
            "[features] # note\nunified_exec_tty = true\n",
            "[\"features\"]\nunified_exec_tty = true\n",
            "[features]\n\"unified_exec_tty\" = true\n",
            "features.unified_exec_tty = true\n",
            "features.hooks = true\n",
            "features = { unified_exec_tty = true }\n",
            "features\t= { hooks = true }\n",
            "notes = \"\"\"\n[features]\n\"\"\"\n",
        ] {
            let new = codex_tty_off(config).unwrap_or_default();
            assert!(new.parse::<toml_edit::DocumentMut>().is_ok(), "{new}");
            let seen = crate::evidence::codex_tty_setting(&new);
            assert_eq!(seen, Some(false), "{config} -> {new}");
        }
        // Text that is not TOML, or `features` that is no table, which Codex would refuse too.
        for config in ["features = 1\n", "[features\n"] {
            assert_eq!(codex_tty_off(config), None, "{config}");
        }
    }

    #[test]
    fn codex_installer_writes_hooks_json_and_asks_the_user_to_trust_them() {
        let s = Scratch::new();
        let codex = s.env.codex_home.clone();
        let _ = fs::create_dir_all(&codex);
        let old = r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "/usr/local/bin/my-hook"}]}]}}"#;
        let _ = fs::write(codex.join("hooks.json"), old);
        let _ = fs::write(codex.join("config.toml"), "model = \"x\"\n");
        let out = install(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("/hooks"), "{}", out.stdout);
        assert!(out.stdout.contains("type join"), "{}", out.stdout);
        let hooks: Value = fs::read_to_string(codex.join("hooks.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let pre = hooks["hooks"]["PreToolUse"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(pre.len(), 2, "{hooks}");
        assert_eq!(pre[0]["hooks"][0]["command"], "/usr/local/bin/my-hook");
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        // H1.5: Codex's tool-call hook fails closed by its command; Codex has no `onFailure`.
        assert_eq!(
            pre[1]["hooks"][0]["command"],
            pretool_for(&program, &codex.join("hooks.json"))
        );
        assert!(pre[1]["hooks"][0].get("onFailure").is_none());
        for (event, mode) in [
            ("PostToolUse", "guard posttool"),
            ("UserPromptSubmit", "guard prompt"),
        ] {
            assert_eq!(
                hooks["hooks"][event][0]["hooks"][0]["command"],
                format!("{} {mode}", program.display()),
                "{event}"
            );
        }
        assert_eq!(
            hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            format!("{} guard start", program.display())
        );
        for (event, group) in [("SessionStart", 1), ("Stop", 0)] {
            let h = &hooks["hooks"][event][group]["hooks"][0];
            assert_eq!(
                h["command"],
                format!("{} listen --once", program.display()),
                "{event}"
            );
            assert!(h.get("asyncRewake").is_none(), "{event}");
        }
        assert_eq!(
            fs::read_to_string(codex.join("config.toml"))
                .ok()
                .as_deref(),
            Some(
                "model = \"x\"\nweb_search = \"disabled\"\n\n[features]\nunified_exec_tty = false\n"
            )
        );
        let backups = fs::read_dir(&codex)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("hooks.json.lunchroom-backup-")
            })
            .count();
        assert_eq!(backups, 1);
    }

    // §13 H1: the question names the setting only when the install will write it, and a
    // file that changes under the question changes nothing and is asked about again.
    #[test]
    fn codex_question_states_the_setting_only_when_the_install_writes_it() {
        let s = Scratch::new();
        let _ = fs::create_dir_all(&s.env.codex_home);
        let config = s.env.codex_home.join("config.toml");
        let said = |target| terminals_line(target, &config).unwrap_or_default();
        // §13::63: the question names the tty setting and `web_search`, each only when it is
        // written, before the yes.
        assert!(said(Target::Codex).contains(CODEX_TTY));
        assert!(said(Target::Codex).contains("web_search = \"disabled\""));
        assert_eq!(said(Target::ClaudeCode), "");
        let _ = fs::write(&config, "[features]\nunified_exec_tty = false\n");
        assert!(!said(Target::Codex).contains(CODEX_TTY));
        assert!(said(Target::Codex).contains("web_search = \"disabled\""));
        let _ = fs::write(&config, "web_search = \"disabled\"\n");
        assert!(said(Target::Codex).contains(CODEX_TTY));
        assert!(!said(Target::Codex).contains("web_search"));
        let _ = fs::write(
            &config,
            "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n",
        );
        assert_eq!(said(Target::Codex), "");
        let turned_on = "[features]\nunified_exec_tty = true\n";
        let mut user = EditsWhileAsking {
            path: config.clone(),
            text: Some(turned_on),
        };
        let out = install(&["codex".to_owned()], &s.env, &mut user);
        assert_ne!(out.code, 0, "{}", out.stdout);
        assert!(out.stderr.contains("changed while you confirmed"));
        assert!(!s.env.codex_home.join("hooks.json").exists());
        let kept = fs::read_to_string(&config).ok();
        assert_eq!(kept.as_deref(), Some(turned_on));
    }

    // §13 H1: a session seen in a process started before the install wrote the setting
    // counts as before it; one started after, or first seen after, does not; and an install that
    // finds the setting written moves nothing.
    #[test]
    fn codex_install_marks_when_it_wrote_the_setting() {
        use lunchroom_core::identity::{Harness, Identity};
        let s = Scratch::new();
        let _ = fs::create_dir_all(&s.env.codex_home);
        let store = Store::<Mail>::open(&s.env.store_dir());
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        let boot = crate::process::boot_id();
        let (older, newer) = (
            Identity::new(Harness::Codex, "older"),
            Identity::new(Harness::Codex, "newer"),
        );
        let before = |who: &Identity| {
            store
                .read()
                .is_ok_and(|m| m.opened_before_codex_setting(who, boot.as_deref()))
        };
        let seen = |process: &str, who: &Identity| {
            store
                .update(|m| m.observe_process(process, Path::new("/"), who))
                .is_ok()
        };
        let process = crate::process::key(std::process::id()).unwrap_or_default();
        assert!(seen(&process, &older));
        let codex = ["codex".to_owned()];
        assert_eq!(install(&codex, &s.env, &mut UserAt("yes\n")).code, 0);
        assert!(before(&older));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let child = std::process::Command::new("sleep").arg("1").spawn();
        assert!(child.is_ok());
        let Ok(mut child) = child else { return };
        let process = crate::process::key(child.id()).unwrap_or_default();
        assert!(seen(&process, &newer));
        assert!(!before(&newer));
        // Codex's server can run a new session in a process older than the install.
        let server = crate::process::key(std::process::id()).unwrap_or_default();
        let in_server = Identity::new(Harness::Codex, "in-server");
        assert!(seen(&server, &in_server));
        assert!(!before(&in_server));
        assert_eq!(install(&codex, &s.env, &mut UserAt("yes\n")).code, 0);
        assert!(!before(&newer));
        assert!(!before(&in_server));
        let _ = child.kill();
        let _ = child.wait();
    }

    // §13::4, §13::5 (OpenClaw, Hermes): the installer writes the program and the
    // plugin's files with the program's path in place of the marker, keeps an old file as a backup,
    // lists the steps only the user can do (enable the plugin, OpenClaw's conversation access) and
    // writes no harness configuration or trust record itself; without a yes it changes nothing.
    #[test]
    fn plugin_installers_write_the_plugin_and_list_the_users_steps() {
        let s = Scratch::new();
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let shown = program.display().to_string();
        let out = install(&["openclaw".to_owned()], &s.env, &mut UserAt("no\n"));
        assert_ne!(out.code, 0);
        assert!(!program.exists());
        let dir = s.env.lunchroom_home.join("plugins/openclaw");
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("index.mjs"), "old");
        let out = install(&["openclaw".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let index = fs::read_to_string(dir.join("index.mjs")).unwrap_or_default();
        assert!(
            index.contains(&format!("const PROGRAM = \"{shown}\";")),
            "{index}"
        );
        assert!(!index.contains(PROGRAM_MARKER));
        assert_eq!(
            crate::evidence::plugin_program(&index),
            Some(shown.as_str())
        );
        for name in ["openclaw.plugin.json", "package.json"] {
            assert!(dir.join(name).is_file(), "{name}");
        }
        let backups = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("index.mjs.lunchroom-backup-")
            })
            .count();
        assert_eq!(backups, 1);
        for step in [
            "openclaw plugins install --link",
            "openclaw plugins enable lunchroom",
            "plugins.entries.lunchroom.hooks.allowConversationAccess true",
            "type join (or join <name>)",
        ] {
            assert!(out.stdout.contains(step), "{step}: {}", out.stdout);
        }
        assert!(!s.env.home.join(".openclaw").exists());
        let hermes = s.env.hermes_home.join("plugins/lunchroom");
        let _ = fs::create_dir_all(&hermes);
        let _ = fs::write(hermes.join("__init__.py"), "old");
        let out = install(&["hermes".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let backups = fs::read_dir(&hermes)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("__init__.py.lunchroom-backup-")
            })
            .count();
        assert_eq!(backups, 1);
        assert!(
            out.stdout.contains("the earlier file is kept at"),
            "{}",
            out.stdout
        );
        let init = fs::read_to_string(hermes.join("__init__.py")).unwrap_or_default();
        assert!(init.contains(&format!("PROGRAM = \"{shown}\"")), "{init}");
        assert_eq!(crate::evidence::plugin_program(&init), Some(shown.as_str()));
        assert!(hermes.join("plugin.yaml").is_file());
        assert!(
            out.stdout.contains("hermes plugins enable lunchroom"),
            "{}",
            out.stdout
        );
        assert!(!hermes.join("../../config.yaml").exists());
        assert!(program.is_file());
    }

    // I1.5 with the earlier Lunchroom: the sessions named in its registry are listed in the install
    // question and named again on that yes; without it, none is.
    #[test]
    fn installer_names_the_earlier_registrys_sessions_on_the_same_yes() {
        let registry = r#"{"agents": {
            "Planner": {"kind": "claude", "id": "aaaa-1", "registered": "x", "auto": true},
            "Builder": {"kind": "codex", "id": "bbbb-2"},
            "Other": {"kind": "opencode", "id": "cccc-3"}
        }}"#;
        let refused = Scratch::new();
        let _ = fs::create_dir_all(&refused.env.lunchroom_home);
        let _ = fs::write(refused.env.lunchroom_home.join("registry.json"), registry);
        let out = install(
            &["claude-code".to_owned()],
            &refused.env,
            &mut UserAt("no\n"),
        );
        assert_eq!(out.code, 3);
        let none = Store::<Mail>::open(&refused.env.store_dir())
            .and_then(|s| s.read())
            .map(|m| m.names().count());
        assert_eq!(none.ok(), Some(0));
        let s = Scratch::new();
        let _ = fs::create_dir_all(&s.env.lunchroom_home);
        let _ = fs::write(s.env.lunchroom_home.join("registry.json"), registry);
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        // P1.6: the install asks the hold mode; an answer naming none leaves Balanced, recorded as
        // chosen so it is not asked again.
        assert!(out.stdout.contains("hold mode: Balanced"), "{}", out.stdout);
        let mail = Store::<Mail>::open(&s.env.store_dir()).and_then(|st| st.read());
        assert!(mail.as_ref().is_ok_and(Mail::hold_mode_chosen));
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert!(!again.stdout.contains("hold mode:"), "{}", again.stdout);
        assert!(
            out.stdout.contains("named Planner claude-code:aaaa-1"),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout.contains("named Builder codex:bbbb-2"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("Other"), "{}", out.stdout);
        let named = Store::<Mail>::open(&s.env.store_dir())
            .and_then(|st| st.read())
            .map(|m| {
                (
                    m.named("Planner").map(ToString::to_string),
                    m.named("Builder").map(ToString::to_string),
                )
            });
        assert_eq!(
            named.ok(),
            Some((
                Some("claude-code:aaaa-1".to_owned()),
                Some("codex:bbbb-2".to_owned())
            ))
        );
    }

    #[test]
    fn installer_writes_beside_old_config_with_backup_and_lists_user_steps() {
        let s = Scratch::new();
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let backup = s
            .env
            .home
            .join(".claude/settings.json.lunchroom-backup-20261026T140000Z");
        assert_eq!(
            fs::read_to_string(&backup).ok().as_deref(),
            Some(OLD_SETTINGS)
        );
        assert!(
            out.stdout.contains("the earlier settings are kept at"),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout.contains("Steps only you can do:"),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout.contains("type join (or join <name>)"),
            "{}",
            out.stdout
        );
        let settings = s.settings();
        let keys: Vec<&String> = settings
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        assert_eq!(keys, ["model", "hooks", "permissions"]);
        assert_eq!(settings["permissions"]["deny"][0], "Read(./.env)");
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/my-hook"
        );
        for (event, mode) in [
            ("PreToolUse", "pretool"),
            ("PostToolUse", "posttool"),
            ("UserPromptSubmit", "prompt"),
        ] {
            let hooks = lunchroom_commands(&settings, event);
            assert_eq!(hooks.len(), 1, "{event}");
            let program = s.env.lunchroom_home.join("bin/lunchroom");
            if event == "PreToolUse" {
                // §13::100, §13::103: `<program> guard pretool || exit 2` with its line on
                // standard error, and `"onFailure": "block"`.
                assert_eq!(
                    hooks[0]["command"],
                    pretool_for(&program, &s.env.claude_home.join("settings.json"))
                );
                assert_eq!(hooks[0]["onFailure"], "block");
            } else {
                // §13::104-106: no other hook has `|| exit 2` or `onFailure`.
                assert!(
                    hooks[0]["command"].as_str().is_some_and(|c| c
                        .ends_with(&format!(" guard {mode}"))
                        && !c.contains("||"))
                );
                assert!(hooks[0].get("onFailure").is_none(), "{event}");
            }
        }
        // The session-start line answers at once; the listener waits in the background.
        let start = lunchroom_commands(&settings, "SessionStart");
        assert_eq!(start.len(), 2);
        assert!(
            start[0]["command"]
                .as_str()
                .is_some_and(|c| c.ends_with(" guard start"))
        );
        assert!(start[0].get("asyncRewake").is_none());
        for hooks in [&start[1..], &lunchroom_commands(&settings, "Stop")[..]] {
            assert_eq!(hooks.len(), 1);
            assert_eq!(hooks[0]["asyncRewake"], true);
        }
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let mode = fs::metadata(&program).map_or(0, |m| m.permissions().mode() & 0o777);
        assert_eq!(mode, 0o755);
        // No trust record (Claude Code keeps them in ~/.claude.json).
        assert!(!s.env.home.join(".claude.json").exists());
        assert!(
            out.stdout
                .contains("1. Start each Claude Code session that Lunchroom should guard anew"),
            "{}",
            out.stdout
        );
    }

    // §13::4 run again, after the user changed the file by hand: the same hooks, not twice; the
    // user's hook still first; open sessions need no restart; a backup only of what changed.
    #[test]
    fn installer_run_again_replaces_its_hooks_and_backs_up_only_changes() {
        let s = Scratch::new();
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let settings = s.settings();
        let backup = s
            .env
            .home
            .join(".claude/settings.json.lunchroom-backup-20261026T140000Z");
        let edited = OLD_SETTINGS.replace("\"opus\"", "\"sonnet\"");
        let mut by_hand: Value = serde_json::from_str(&edited).unwrap_or(Value::Null);
        by_hand["hooks"] = settings["hooks"].clone();
        let _ = fs::write(
            s.env.home.join(".claude/settings.json"),
            by_hand.to_string(),
        );
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(again.code, 0, "{}", again.stderr);
        let settings = s.settings();
        assert_eq!(settings["model"], "sonnet");
        assert_eq!(lunchroom_commands(&settings, "PreToolUse").len(), 1);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/my-hook"
        );
        assert!(!again.stdout.contains("anew"), "{}", again.stdout);
        assert!(
            again.stdout.contains("The hooks were already in place"),
            "{}",
            again.stdout
        );
        // Both installs ran in the same second: the first backup still holds the original
        // settings, and the second took the next free name.
        assert_eq!(
            fs::read_to_string(&backup).ok().as_deref(),
            Some(OLD_SETTINGS)
        );
        let second = s
            .env
            .home
            .join(".claude/settings.json.lunchroom-backup-20261026T140000Z-1");
        assert!(second.exists());
        // The program is the same one: nothing to keep.
        assert!(
            !again.stdout.contains("the earlier program is kept at"),
            "{}",
            again.stdout
        );
        let programs = fs::read_dir(s.env.lunchroom_home.join("bin"))
            .into_iter()
            .flatten()
            .count();
        assert_eq!(programs, 1);
        // A third time with nothing changed: nothing is written, so no backup is needed.
        let path = s.env.home.join(".claude/settings.json");
        let written = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
        let before = (
            written(&path),
            written(&s.env.lunchroom_home.join("bin/lunchroom")),
        );
        let third = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(third.code, 0, "{}", third.stderr);
        assert!(!third.stdout.contains("kept at"), "{}", third.stdout);
        assert_eq!(
            (
                written(&path),
                written(&s.env.lunchroom_home.join("bin/lunchroom"))
            ),
            before
        );
    }

    // §13::4: earlier Lunchroom groups go from every event; the user's hooks stay, among them one
    // that runs the program with no arguments Lunchroom writes.
    #[test]
    fn installer_replaces_earlier_lunchroom_hooks_in_every_event() {
        let program = Path::new("/home/u/.lunchroom/bin/lunchroom");
        let p = program.display().to_string();
        let mut earlier = Map::new();
        earlier.insert(
            "hooks".to_owned(),
            json!({
                "Notification": [
                    {"hooks": [{"type": "command", "command": p}]},
                    {"hooks": [{"type": "command", "command": "notify-send hi"}]}
                ],
                "PreToolUse": [{"hooks": [{"type": "command", "command": format!("{p} guard pretool")}]}]
            }),
        );
        let reconciled = with_lunchroom_hooks(
            earlier,
            program,
            Path::new("/home/u"),
            Target::ClaudeCode,
            Path::new("/home/u/.claude/settings.json"),
        )
        .map_or(Value::Null, Value::Object);
        // A bare program path is a hook Lunchroom never writes: the user's, and it stays.
        assert_eq!(
            reconciled["hooks"]["Notification"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(
            reconciled["hooks"]["Notification"][0]["hooks"][0]["command"],
            p
        );
        assert_eq!(
            reconciled["hooks"]["Notification"][1]["hooks"][0]["command"],
            "notify-send hi"
        );
        let pre: Vec<&Value> = reconciled["hooks"]["PreToolUse"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["onFailure"], "block");
        assert_eq!(
            pre[0]["hooks"][0]["command"],
            pretool_for(program, Path::new("/home/u/.claude/settings.json"))
        );
    }

    // §13::4: in a group shared with the user's hooks only Lunchroom's hook goes; a hook that runs
    // the program through a link (an earlier install under another name) is Lunchroom's.
    #[test]
    fn installer_keeps_the_users_hooks_in_a_shared_group_and_replaces_a_linked_name() {
        let s = Scratch::new();
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let _ = fs::create_dir_all(program.parent().unwrap_or(&s.env.home));
        let _ = fs::write(&program, "old program");
        let old_name = s.env.home.join(".old-name/bin/old-name");
        let _ = fs::create_dir_all(old_name.parent().unwrap_or(&s.env.home));
        let _ = std::os::unix::fs::symlink(&program, &old_name);
        let p = program.display();
        let o = old_name.display();
        let mut earlier = Map::new();
        earlier.insert(
            "hooks".to_owned(),
            json!({
                "UserPromptSubmit": [{"hooks": [
                    {"type": "command", "command": "/usr/local/bin/facts.sh"},
                    {"type": "command", "command": "/usr/local/bin/beacon.sh"},
                    {"type": "command", "command": format!("{p} guard prompt")}
                ]}],
                "PreToolUse": [{"hooks": [{"type": "command", "command": format!("{o} guard pretool")}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "/usr/local/bin/my-stop"}]}]
            }),
        );
        let reconciled = with_lunchroom_hooks(
            earlier,
            &program,
            &s.env.home,
            Target::Codex,
            &s.env.codex_home.join("hooks.json"),
        )
        .map_or(Value::Null, Value::Object);
        let commands = |event: &str| -> Vec<Vec<String>> {
            reconciled["hooks"][event]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| {
                    g["hooks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|h| h["command"].as_str().map(str::to_owned))
                        .collect()
                })
                .collect()
        };
        assert_eq!(
            commands("UserPromptSubmit"),
            [
                vec!["/usr/local/bin/facts.sh", "/usr/local/bin/beacon.sh"],
                vec![format!("{p} guard prompt").as_str()]
            ]
        );
        assert_eq!(
            commands("PreToolUse"),
            [[pretool_for(&program, &s.env.codex_home.join("hooks.json"))]]
        );
        assert_eq!(
            commands("Stop"),
            [
                vec!["/usr/local/bin/my-stop".to_owned()],
                vec![format!("{p} listen --once")]
            ]
        );
        // An empty group of the user's own stays as it was.
        let mut empty = Map::new();
        empty.insert("hooks".to_owned(), json!({"Notification": [{"hooks": []}]}));
        let kept = with_lunchroom_hooks(
            empty,
            &program,
            &s.env.home,
            Target::Codex,
            &s.env.codex_home.join("hooks.json"),
        )
        .map_or(Value::Null, Value::Object);
        assert_eq!(kept["hooks"]["Notification"], json!([{"hooks": []}]));
    }

    // §13::4 with A1.3: without the user's yes nothing is written; settings that are not a JSON
    // object are left as they are.
    #[test]
    fn installer_changes_nothing_without_yes_or_on_unreadable_settings() {
        // The question says what is changed and shows the exact hooks it adds.
        struct Asked(Vec<String>);
        impl System for Asked {
            fn ancestors(&self) -> Vec<(Option<String>, String)> {
                Vec::new()
            }
            fn own_tty(&self) -> u64 {
                34816
            }
            fn tty_of(&self, _key: &str) -> Option<u64> {
                None
            }
            fn ask(&mut self, question: &str) -> Option<String> {
                self.0.push(question.to_owned());
                Some("no\n".to_owned())
            }
            fn ask_secret(&mut self, prompt: &str) -> Option<String> {
                self.0.push(prompt.to_owned());
                Some(fake::typed("no"))
            }
            fn uid(&self) -> Option<u32> {
                Some(1000)
            }
            fn verifier(&self) -> Result<String, String> {
                Ok(fake::verifier())
            }
            fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
                Ok([6; lunchroom_core::passphrase::SALT_BYTES])
            }
            fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let shown = Scratch::new();
        let mut asked = Asked(Vec::new());
        assert_eq!(
            install(&["claude-code".to_owned()], &shown.env, &mut asked).code,
            3
        );
        let question = asked.0.first().cloned().unwrap_or_default();
        assert!(
            question.starts_with("Lunchroom: add Lunchroom to Claude Code? This writes"),
            "{question}"
        );
        assert!(question.contains("The hooks it adds:\n{"), "{question}");
        // §13::103, §13::104: `onFailure` is on the tool-call hook only.
        assert_eq!(question.matches("onFailure").count(), 1, "{question}");
        // Every hook written is shown under its event, both of SessionStart's among them.
        let shown_hooks = question
            .split_once("The hooks it adds:\n")
            .and_then(|(_, rest)| rest.rsplit_once("\nPassphrase: "))
            .and_then(|(hooks, _)| serde_json::from_str::<Value>(hooks).ok())
            .unwrap_or_default();
        let written = lunchroom_hooks(
            &shown.env.lunchroom_home.join("bin/lunchroom"),
            Target::ClaudeCode,
            &shown.env.claude_home.join("settings.json"),
        );
        for (event, group) in &written {
            let groups = shown_hooks[*event].as_array().cloned().unwrap_or_default();
            assert!(groups.contains(group), "{event}: {question}");
        }
        let count: usize = shown_hooks
            .as_object()
            .map(|events| {
                events
                    .values()
                    .filter_map(Value::as_array)
                    .map(Vec::len)
                    .sum()
            })
            .unwrap_or_default();
        assert_eq!(count, written.len(), "{question}");
        assert_eq!(
            shown_hooks["SessionStart"].as_array().map(Vec::len),
            Some(2)
        );
        let s = Scratch::new();
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("no\n"));
        assert_eq!(out.code, 3);
        assert_eq!(
            fs::read_to_string(s.env.home.join(".claude/settings.json"))
                .ok()
                .as_deref(),
            Some(OLD_SETTINGS)
        );
        assert!(!s.env.lunchroom_home.join("bin/lunchroom").exists());
        let _ = fs::write(s.env.home.join(".claude/settings.json"), "[1, 2]");
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 2);
        assert_eq!(
            fs::read_to_string(s.env.home.join(".claude/settings.json"))
                .ok()
                .as_deref(),
            Some("[1, 2]")
        );
        assert_eq!(
            install(&["opencode".to_owned()], &s.env, &mut UserAt("yes\n")).code,
            2
        );
    }

    fn settings_of(hooks: &Value) -> Map<String, Value> {
        let mut settings = Map::new();
        settings.insert("hooks".to_owned(), hooks.clone());
        settings
    }

    /// A settings file as Claude Code users have them: matchers, several hooks to a group,
    /// timeouts, and the other top-level keys.
    const REAL_SETTINGS: &str = r#"{
  "env": {"FOO": "1"},
  "permissions": {"allow": ["Bash(ls:*)"], "deny": ["Read(./.env)"]},
  "enabledPlugins": {"a@b": true},
  "hooks": {
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [
        {"type": "command", "command": "/u/a.sh", "timeout": 5},
        {"type": "command", "command": "/u/b.sh"}
      ]},
      {"matcher": "Write|Edit|MultiEdit|NotebookEdit", "hooks": [
        {"type": "command", "command": "/u/c.sh", "timeout": 10}
      ]}
    ],
    "SessionStart": [
      {"matcher": "startup|compact|resume", "hooks": [
        {"type": "command", "command": "/u/s.sh"},
        {"type": "command", "command": "/u/t.sh", "timeout": 3}
      ]}
    ]
  },
  "model": "opus",
  "statusLine": {"type": "command", "command": "/u/status.sh"}
}
"#;

    // §13::4: a real settings file keeps every key, group, matcher, timeout and hook as it was;
    // Lunchroom's groups are added after them, and `PostToolUseFailure` (Claude Code's event for a
    // failed tool) runs the guard for Claude Code only.
    #[test]
    fn installer_keeps_a_real_settings_shape_and_guards_failed_tools_for_claude_code_only() {
        let s = Scratch::new();
        let path = s.env.home.join(".claude/settings.json");
        let _ = fs::write(&path, REAL_SETTINGS);
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let before: Value = serde_json::from_str(REAL_SETTINGS).unwrap_or(Value::Null);
        let after = s.settings();
        let keys: Vec<&String> = after
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "env",
                "permissions",
                "enabledPlugins",
                "hooks",
                "model",
                "statusLine"
            ]
        );
        for key in [
            "env",
            "permissions",
            "enabledPlugins",
            "model",
            "statusLine",
        ] {
            assert_eq!(after[key], before[key], "{key}");
        }
        for (event, groups, ours) in [("PreToolUse", 2, 1), ("SessionStart", 1, 2)] {
            for g in 0..groups {
                assert_eq!(
                    after["hooks"][event][g], before["hooks"][event][g],
                    "{event} {g}"
                );
            }
            assert_eq!(
                after["hooks"][event].as_array().map(Vec::len),
                Some(groups + ours)
            );
        }
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        assert_eq!(
            after["hooks"]["PostToolUseFailure"][0]["hooks"][0]["command"],
            format!("{} guard posttool", program.display())
        );
        let codex = Scratch::new();
        let _ = fs::create_dir_all(&codex.env.codex_home);
        let out = install(&["codex".to_owned()], &codex.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let hooks = fs::read_to_string(codex.env.codex_home.join("hooks.json")).unwrap_or_default();
        assert!(!hooks.contains("PostToolUseFailure"), "{hooks}");
    }

    // §13::4: a settings file that is a link (a dotfiles repository) stays a link and the file it
    // points to gets the hooks, with its mode; the backup is beside that file; no temporary file
    // is left.
    #[test]
    fn installer_writes_through_a_linked_settings_file_and_keeps_its_mode() {
        let s = Scratch::new();
        let real_dir = s.env.home.join("dotfiles");
        let _ = fs::create_dir_all(&real_dir);
        let real = real_dir.join("settings.json");
        let _ = fs::write(&real, REAL_SETTINGS);
        let _ = fs::set_permissions(&real, fs::Permissions::from_mode(0o640));
        let link = s.env.home.join(".claude/settings.json");
        let _ = fs::remove_file(&link);
        let _ = std::os::unix::fs::symlink(&real, &link);
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(fs::read_link(&link).ok(), Some(real.clone()));
        let text = fs::read_to_string(&real).unwrap_or_default();
        assert!(text.contains("guard pretool"), "{text}");
        assert_eq!(
            fs::metadata(&real)
                .map(|m| m.permissions().mode() & 0o777)
                .ok(),
            Some(0o640)
        );
        let names: Vec<String> = fs::read_dir(&real_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"settings.json.lunchroom-backup-20261026T140000Z".to_owned()),
            "{names:?}"
        );
        assert!(
            names.iter().all(|n| !n.contains(".lunchroom-new")),
            "{names:?}"
        );
        // Run again: the same hooks, the same mode, nothing more to write.
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(again.code, 0, "{}", again.stderr);
        assert!(!again.stdout.contains("kept at"), "{}", again.stdout);
    }

    /// `s` moved to a home whose name is `name`, holding the settings Claude Code reads.
    fn move_home(s: &mut Scratch, name: &str) {
        let home = s.root.join(name);
        let _ = fs::create_dir_all(home.join(".claude"));
        let _ = fs::write(home.join(".claude/settings.json"), OLD_SETTINGS);
        s.env.lunchroom_home = home.join(".lunchroom");
        s.env.claude_home = home.join(".claude");
        s.env.codex_home = home.join(".codex");
        s.env.hermes_home = home.join(".hermes");
        s.env.home = home;
    }

    // §13::4: a program path with a space is quoted in the hook commands, and run again the quoted
    // hooks are recognised as Lunchroom's (one each); a path with a quote or a line break is
    // refused and nothing is changed.
    #[test]
    fn installer_quotes_the_program_path_and_refuses_one_that_cannot_be_quoted() {
        let mut s = Scratch::new();
        move_home(&mut s, "my home");
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let quoted = format!("'{}'", program.display());
        let settings = s.settings();
        assert_eq!(
            settings["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            pretool_command(&quoted, &s.env.claude_home.join("settings.json"))
        );
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            format!("{quoted} listen")
        );
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(again.code, 0, "{}", again.stderr);
        assert_eq!(s.settings(), settings);
        for bad in ["it's", "two\nlines"] {
            let mut s = Scratch::new();
            move_home(&mut s, bad);
            let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 2, "{bad}");
            assert!(!s.env.lunchroom_home.join("bin/lunchroom").exists());
            assert_eq!(
                fs::read_to_string(s.env.home.join(".claude/settings.json"))
                    .ok()
                    .as_deref(),
                Some(OLD_SETTINGS)
            );
        }
    }

    /// Every name a folder or a file can have that a shell would treat as more than text.
    const SHELL_NAMES: [&str; 14] = [
        "a b",
        "a$(touch S)b",
        "a`touch S`b",
        "a;touch S;b",
        "a\\b",
        "a\tb",
        "a\rb",
        "a\"b",
        "a$HOMEb",
        "a%sb",
        "a&b|c>S",
        "*",
        "-n",
        "u\u{fc}\u{20ac}\u{202e}x",
    ];

    //H1.5 (shell injection): the hook commands the install writes, run through a real
    // `sh -c`, execute no byte of a folder name or a configuration file's name. For every name above
    // as the guard program's folder and as the configuration file's folder, and for a file name that
    // holds a quote or a line break as well: the stand-in guard program runs (so the program word
    // was one word), no command named inside a path ran (the sentinel file `S` does not exist), and
    // the tool-call hook's failure line, when the guard fails, is the fixed words around the path
    // printed as data, byte for byte.
    #[test]
    fn a_hook_command_runs_no_byte_of_a_path_and_prints_the_path_as_data() {
        let s = Scratch::new();
        let work = s.root.join("work");
        assert!(fs::create_dir_all(&work).is_ok());
        let run_in_work = |command: &str| {
            let out = std::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(&work)
                .stdin(std::process::Stdio::null())
                .output();
            out.map_or((-1, String::new()), |o| {
                (
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                )
            })
        };
        let mut cases: Vec<(String, String)> = SHELL_NAMES
            .iter()
            .map(|n| ((*n).to_owned(), "settings.json".to_owned()))
            .collect();
        for file_name in [
            "it's.json",
            "a\nb.json",
            "'; touch S; '.json",
            "\"; touch S; \".json",
            "a\r\tb.json",
        ] {
            cases.push(("plain".to_owned(), file_name.to_owned()));
        }
        for target in [Target::ClaudeCode, Target::Codex] {
            for (n, (program_dir, file_name)) in cases.iter().enumerate() {
                let sub = format!("{n}-{target:?}");
                let stand = stand_in(
                    &s.root.join(&sub).join(program_dir),
                    "echo ran >> \"$0.ran\"\nexit 1",
                );
                let file = s.root.join(&sub).join("conf dir").join(file_name);
                let hooks = lunchroom_hooks(&stand, target, &file);
                assert!(hooks.len() >= 6, "{target:?}");
                for (event, group) in hooks {
                    let command = group["hooks"][0]["command"].as_str().unwrap_or_default();
                    let ran = PathBuf::from(format!("{}.ran", stand.display()));
                    let _ = fs::remove_file(&ran);
                    let (code, stderr) = run_in_work(command);
                    assert!(
                        ran.exists(),
                        "{event}, {program_dir:?}: the guard program did not run as one word"
                    );
                    assert!(
                        !work.join("S").exists(),
                        "{event}, {program_dir:?}, {file_name:?}: a command in a name ran"
                    );
                    if event == "PreToolUse" {
                        assert_eq!(code, 2, "{program_dir:?}, {file_name:?}");
                        assert_eq!(
                            stderr,
                            format!("{FAILED_LINE_HEAD}{}{FAILED_LINE_TAIL}\n", file.display()),
                            "{program_dir:?}, {file_name:?}"
                        );
                    } else {
                        assert_eq!(code, 1, "{event}, {program_dir:?}");
                        assert!(stderr.is_empty(), "{event}: {stderr}");
                    }
                }
            }
        }
    }

    //H1.5: a program path that cannot be quoted into one shell word (a single quote,
    // a line break, bytes that are not text) is refused for Claude Code and for Codex with the
    // reason stated, and nothing is written or changed; a path that only holds shell characters
    // (the names above) is installed, and evidence reads that install as the guard in place.
    #[test]
    fn an_unquotable_program_path_is_refused_with_its_reason_for_both_targets() {
        use std::os::unix::ffi::OsStrExt as _;
        for target in ["claude-code", "codex"] {
            let bad: Vec<std::ffi::OsString> = vec![
                "it's".into(),
                "two\nlines".into(),
                "'; touch S; '".into(),
                std::ffi::OsStr::from_bytes(b"bytes\xff").to_owned(),
            ];
            for name in bad {
                let mut s = Scratch::new();
                move_home(&mut s, "plain");
                let home = s.root.join(&name);
                let _ = fs::create_dir_all(home.join(".claude"));
                let _ = fs::write(home.join(".claude/settings.json"), OLD_SETTINGS);
                s.env.lunchroom_home = home.join(".lunchroom");
                let out = install(&[target.to_owned()], &s.env, &mut UserAt("yes\n"));
                assert_eq!(out.code, 2, "{target}, {name:?}");
                assert!(
                    out.stderr
                        .contains("holds a quote, a line break or a byte that is not text"),
                    "{target}, {name:?}: {}",
                    out.stderr
                );
                assert!(out.stderr.contains("nothing was changed"), "{}", out.stderr);
                assert!(!s.env.lunchroom_home.join("bin/lunchroom").exists());
                assert!(!s.env.codex_home.join("hooks.json").exists());
                assert_eq!(
                    fs::read_to_string(home.join(".claude/settings.json"))
                        .ok()
                        .as_deref(),
                    Some(OLD_SETTINGS)
                );
            }
            // A name full of shell characters is quoted and installed: one guard in place.
            for (i, name) in SHELL_NAMES.iter().enumerate() {
                let mut s = Scratch::new();
                move_home(&mut s, &format!("h{i}{name}"));
                let out = install(&[target.to_owned()], &s.env, &mut UserAt("yes\n"));
                assert_eq!(out.code, 0, "{target}, {name:?}: {}", out.stderr);
                let file = if target == "codex" {
                    s.env.codex_home.join("hooks.json")
                } else {
                    s.env.claude_home.join("settings.json")
                };
                let text = fs::read_to_string(&file).unwrap_or_default();
                let settings: Value = serde_json::from_str(&text).unwrap_or_default();
                let hook = settings["hooks"]["PreToolUse"].as_array().and_then(|g| {
                    g.iter()
                        .find_map(|g| {
                            g["hooks"][0]["command"]
                                .as_str()
                                .filter(|c| c.contains("guard pretool"))
                        })
                        .map(|_| ())
                });
                assert!(hook.is_some(), "{target}, {name:?}");
            }
        }
    }

    // §13::4: an earlier Lunchroom hook whose program is gone, by the default install location
    // (`~/` and `$HOME/` spelled out, quoted or not), is Lunchroom's and goes; a command with
    // another program or other arguments stays.
    #[test]
    fn installer_replaces_earlier_hooks_whose_program_no_longer_exists() {
        let program = Path::new("/srv/l/bin/lunchroom");
        let earlier = settings_of(&json!({
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "~/.lunchroom/bin/lunchroom guard pretool", "timeout": 15},
                {"type": "command", "command": "/usr/bin/other guard pretool"},
                {"type": "command", "command": "/gone/lunchroom guard other"}
            ]}],
            "Stop": [{"hooks": [{"type": "command", "command": "$HOME/.lunchroom/bin/lunchroom listen --once"}]}],
            "SessionStart": [{"hooks": [{"type": "command", "command": "'/home/u/.lunchroom/bin/lunchroom' listen"}]}]
        }));
        let reconciled = with_lunchroom_hooks(
            earlier,
            program,
            Path::new("/home/u"),
            Target::ClaudeCode,
            Path::new("/home/u/.claude/settings.json"),
        )
        .map_or(Value::Null, Value::Object);
        let pre = &reconciled["hooks"]["PreToolUse"];
        assert_eq!(pre[0]["matcher"], "Bash");
        assert_eq!(pre[0]["hooks"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            pre[0]["hooks"][0]["command"],
            "/usr/bin/other guard pretool"
        );
        assert_eq!(pre[0]["hooks"][1]["command"], "/gone/lunchroom guard other");
        assert_eq!(
            pre[1]["hooks"][0]["command"],
            pretool_for(program, Path::new("/home/u/.claude/settings.json"))
        );
        for (event, commands) in [
            ("Stop", vec!["listen"]),
            ("SessionStart", vec!["guard start", "listen"]),
        ] {
            let list = reconciled["hooks"][event]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(list.len(), commands.len(), "{event}");
            for (group, command) in list.iter().zip(commands) {
                assert_eq!(
                    group["hooks"][0]["command"],
                    format!("{} {command}", program.display()),
                    "{event}"
                );
            }
        }
    }

    // §17.2 (17::25): hooks written for the installed program all run the program at its
    // new path, and none runs the earlier one.
    #[cfg(feature = "console")]
    #[test]
    fn hooks_for_the_installed_program_run_it_at_its_new_path() {
        let program = Path::new(lunchroom_core::setgid::PROGRAM);
        let earlier = settings_of(&json!({
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "/home/u/.lunchroom/bin/lunchroom guard pretool", "timeout": 15}
            ]}],
            "Stop": [{"hooks": [{"type": "command", "command": "/home/u/.lunchroom/bin/lunchroom listen --once"}]}]
        }));
        let file = Path::new("/home/u/.claude/settings.json");
        let new = with_lunchroom_hooks(
            earlier,
            program,
            Path::new("/home/u"),
            Target::ClaudeCode,
            file,
        )
        .map_or(Value::Null, Value::Object);
        let mut commands = Vec::new();
        for (_, groups) in new["hooks"].as_object().into_iter().flatten() {
            for group in groups.as_array().into_iter().flatten() {
                for hook in group["hooks"].as_array().into_iter().flatten() {
                    commands.push(hook["command"].as_str().unwrap_or_default().to_owned());
                }
            }
        }
        assert!(commands.len() >= 4, "{commands:?}");
        for c in &commands {
            assert!(c.contains(lunchroom_core::setgid::PROGRAM), "{c}");
            assert!(!c.contains("/home/u/.lunchroom/bin/lunchroom"), "{c}");
        }
    }

    // §13::4: a reinstall puts Lunchroom's group where its earlier group was (Codex trusts a hook
    // by its position), after the user's hooks that followed it; with none, it is added last.
    #[test]
    fn installer_puts_a_reinstalled_group_in_its_earlier_place() {
        let program = Path::new("/home/u/.lunchroom/bin/lunchroom");
        let p = program.display();
        let earlier = settings_of(&json!({
            "Stop": [
                {"hooks": [{"type": "command", "command": "/u/first.sh"}]},
                {"hooks": [{"type": "command", "command": format!("{p} listen --once")}]},
                {"hooks": [{"type": "command", "command": "/u/last.sh"}]}
            ],
            "PreToolUse": [
                {"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "/u/a.sh"},
                    {"type": "command", "command": format!("{p} guard pretool")}
                ]},
                {"matcher": "Write", "hooks": [{"type": "command", "command": "/u/b.sh"}]}
            ]
        }));
        let reconciled = with_lunchroom_hooks(
            earlier,
            program,
            Path::new("/home/u"),
            Target::Codex,
            Path::new("/home/u/.codex/hooks.json"),
        )
        .map_or(Value::Null, Value::Object);
        let commands = |event: &str| -> Vec<String> {
            reconciled["hooks"][event]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| {
                    g["hooks"][0]["command"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        };
        assert_eq!(
            commands("Stop"),
            [
                "/u/first.sh".to_owned(),
                format!("{p} listen --once"),
                "/u/last.sh".to_owned()
            ]
        );
        // Lunchroom's hook left the shared group; its new group follows that group.
        assert_eq!(
            commands("PreToolUse"),
            [
                "/u/a.sh".to_owned(),
                pretool_command(&p.to_string(), Path::new("/home/u/.codex/hooks.json")),
                "/u/b.sh".to_owned()
            ]
        );
        assert_eq!(commands("UserPromptSubmit"), [format!("{p} guard prompt")]);
    }

    /// A user who edits `path` to `text` while the question is shown, then types the passphrase.
    struct EditsWhileAsking {
        path: PathBuf,
        text: Option<&'static str>,
    }

    impl System for EditsWhileAsking {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            UserAt("").ancestors()
        }
        fn own_tty(&self) -> u64 {
            UserAt("").own_tty()
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            UserAt("").tty_of(key)
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            Some("yes\n".to_owned())
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            if let Some(text) = self.text.take() {
                let _ = fs::write(&self.path, text);
            }
            Some(fake::PASSPHRASE.to_owned())
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            Ok([6; lunchroom_core::passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    // §13::62, §13::63, §13::64, §4::43, §4::44 (H1.4): `lunchroom install codex` sets
    // `web_search = "disabled"` at the top level of `config.toml` (whatever the user had there: no
    // file, no setting, `live`, `cached`, a value after a table or in a profile), names the setting
    // in the question before the yes, and keeps the earlier file as a backup; the user's other
    // lines and profile values stay.
    #[test]
    fn codex_install_sets_web_search_disabled_at_the_top_and_keeps_a_backup() {
        for (name, old) in [
            ("no file", None),
            ("unset", Some("model = \"x\"\n[features]\nhooks = true\n")),
            (
                "live",
                Some("web_search = \"live\" # mine\nmodel = \"x\"\n"),
            ),
            (
                "cached",
                Some("web_search = \"cached\"\n[features]\nunified_exec_tty = true\n"),
            ),
            (
                "a table first",
                Some("[features]\nhooks = true\n[profiles.p]\nweb_search = \"live\"\n"),
            ),
            ("dotted", Some("features.hooks = true\n")),
            ("quoted key", Some("\"web_search\" = \"indexed\"\n")),
            ("other type", Some("web_search = true\n")),
        ] {
            let s = Scratch::new();
            let _ = fs::create_dir_all(&s.env.codex_home);
            let config = s.env.codex_home.join("config.toml");
            if let Some(old) = old {
                assert!(fs::write(&config, old).is_ok(), "{name}");
            }
            let mut user = Answers {
                answers: vec!["yes\n"],
                asked: Vec::new(),
            };
            let out = install(&["codex".to_owned()], &s.env, &mut user);
            assert_eq!(out.code, 0, "{name}: {}{}", out.stdout, out.stderr);
            // §13::63: the question names the change.
            let question = user.asked.first().cloned().unwrap_or_default();
            assert!(
                question.contains("sets web_search = \"disabled\" at the top of"),
                "{name}: {question}"
            );
            // §13::62: top level, before any table, as a string.
            let new = fs::read_to_string(&config).unwrap_or_default();
            let document = new.parse::<toml_edit::DocumentMut>();
            assert!(document.is_ok(), "{name}: {new}");
            let document = document.unwrap_or_default();
            assert_eq!(
                document["web_search"].as_str(),
                Some("disabled"),
                "{name}: {new}"
            );
            let first_table = new.find("\n[").unwrap_or(new.len());
            assert!(
                new.find("web_search = \"disabled\"")
                    .is_some_and(|at| at <= first_table),
                "{name}: {new}"
            );
            assert!(crate::evidence::codex_web_is_off(&new), "{name}");
            assert_eq!(codex_tty_setting(&new), Some(false), "{name}");
            // §13::64: the earlier file is kept.
            let backups: Vec<_> = fs::read_dir(&s.env.codex_home)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("config.toml.lunchroom-backup-")
                })
                .collect();
            match old {
                Some(old) => {
                    assert_eq!(backups.len(), 1, "{name}");
                    let kept = fs::read_to_string(backups[0].path()).unwrap_or_default();
                    assert_eq!(kept, old, "{name}");
                }
                None => assert!(backups.is_empty(), "{name}"),
            }
            // The user's other lines stay.
            for line in ["model = \"x\"", "hooks = true", "[profiles.p]"] {
                if old.is_some_and(|o| o.contains(line)) {
                    assert!(new.contains(line), "{name}: {new}");
                }
            }
        }
    }

    // §13::64, §4::44, §13::4 (an install over an older install): a second install
    // changes neither setting again and keeps the first backup as it was; an older install's hooks
    // (`guard pretool` alone) are replaced by the new tool-call hook, not added to it; and the
    // question of the second install names neither setting.
    #[test]
    fn an_install_over_an_older_install_keeps_the_backup_and_replaces_the_old_hook() {
        let s = Scratch::new();
        let codex = s.env.codex_home.clone();
        let _ = fs::create_dir_all(&codex);
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let p = program.display();
        let older = json!({"hooks": {
            "PreToolUse": [{"hooks": [{"type": "command", "command": format!("{p} guard pretool"), "timeout": 15}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("{p} guard prompt"), "timeout": 15}]}]
        }})
        .to_string();
        let _ = fs::write(codex.join("hooks.json"), &older);
        let _ = fs::write(codex.join("config.toml"), "model = \"x\"\n");
        let out = install(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let backups = |prefix: &str| -> Vec<(PathBuf, String)> {
            fs::read_dir(&codex)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .map(|e| (e.path(), fs::read_to_string(e.path()).unwrap_or_default()))
                .collect()
        };
        let config_backups = backups("config.toml.lunchroom-backup-");
        assert_eq!(config_backups.len(), 1);
        assert_eq!(config_backups[0].1, "model = \"x\"\n");
        let hook_backups = backups("hooks.json.lunchroom-backup-");
        assert_eq!(hook_backups.len(), 1);
        assert_eq!(hook_backups[0].1, older);
        let hooks: Value = fs::read_to_string(codex.join("hooks.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        // The older install's tool-call hook is replaced, in its place, not added to.
        assert_eq!(
            hooks["hooks"]["PreToolUse"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            pretool_for(&program, &codex.join("hooks.json"))
        );
        // Spelled out, so an install that writes the old form cannot pass by agreeing with itself.
        let written = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            written.starts_with(&format!("{p} guard pretool || {{ [ $? -eq 2 ] || printf "))
                && written.contains("lunchroom doctor")
                && written.ends_with("' >&2; exit 2; }"),
            "{written}"
        );
        assert!(
            hooks["hooks"]["PreToolUse"][0]["hooks"][0]
                .get("onFailure")
                .is_none()
        );
        // A second install, on the same files.
        let after_first = fs::read_to_string(codex.join("config.toml")).unwrap_or_default();
        let mut user = Answers {
            answers: vec!["yes\n"],
            asked: Vec::new(),
        };
        let out = install(&["codex".to_owned()], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let question = user.asked.first().cloned().unwrap_or_default();
        assert!(!question.contains("web_search"), "{question}");
        assert!(!question.contains(CODEX_TTY), "{question}");
        assert_eq!(
            fs::read_to_string(codex.join("config.toml")).unwrap_or_default(),
            after_first
        );
        assert_eq!(backups("config.toml.lunchroom-backup-"), config_backups);
        let again: Value = fs::read_to_string(codex.join("hooks.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        assert_eq!(again, hooks);
        // `lunchroom uninstall` removes the new form of the hook, and only Lunchroom's hooks.
        let out = uninstall(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let left = fs::read_to_string(codex.join("hooks.json")).unwrap_or_default();
        assert!(!left.contains("guard pretool"), "{left}");
        assert!(!left.contains("guard prompt"), "{left}");
    }

    // §13::100, §13::103 (H1.5), Fable M11: an install over a 1.0.1 install (`<program>
    // guard pretool`, commit 276b58f install.rs line 346), replaces the tool-call hook with the fail-closed form (and, in Claude Code,
    // `onFailure: block`) in the place the old one stood, keeps the user's hooks and a backup, and
    // leaves one Lunchroom tool-call hook; the old forms are Lunchroom's, so uninstall removes them.
    // This test fails when the install leaves a 1.0.x `guard pretool` in place, drops `onFailure: block`, writes the old form, or skips the backup: each of those
    // mutations was made and caught. The code
    // before the M11 fix already replaced the old forms; the fix for what evidence accepts is
    // proved in evidence.rs by `claude_hooks_of_the_old_form_are_not_the_guard_and_name_the_reinstall`
    // and `a_codex_tool_call_hook_of_the_old_form_is_unguarded_and_names_the_reinstall`.
    #[test]
    fn an_install_over_old_form_tool_call_hooks_writes_the_fail_closed_form() {
        // The old form alone, with Windows line endings, and two Lunchroom tool-call hooks.
        for old in [
            &["guard pretool"][..],
            &["guard pretool\r\n"],
            &["guard pretool", "guard pretool\r\n"],
        ] {
            let s = Scratch::new();
            let program = s.env.lunchroom_home.join("bin/lunchroom");
            let p = program.display();
            let settings = s.env.claude_home.join("settings.json");
            let mut groups = vec![
                json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "/u/a.sh"}]}),
            ];
            for suffix in old {
                groups.push(json!({"hooks": [{"type": "command", "command": format!("{p} {suffix}"), "timeout": 15}]}));
            }
            let before = json!({"model": "opus", "hooks": {
                "PreToolUse": groups,
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("{p} guard prompt")}]}]
            }})
            .to_string();
            assert!(fs::write(&settings, &before).is_ok());
            let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 0, "{old:?}:{}", out.stderr);
            let after = s.settings();
            let pre = after["hooks"]["PreToolUse"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(pre.len(), 2, "{old:?}:{after}");
            assert_eq!(pre[0]["hooks"][0]["command"], "/u/a.sh");
            assert_eq!(
                pre[1]["hooks"][0]["command"],
                pretool_for(&program, &settings),
                "{old:?}"
            );
            // The command is spelled out, not only compared with what `pretool_command` returns,
            // so a install that writes the old form cannot pass by agreeing with itself.
            let written = pre[1]["hooks"][0]["command"].as_str().unwrap_or_default();
            assert!(
                written.starts_with(&format!("{p} guard pretool || {{ [ $? -eq 2 ] || printf "))
                    && written.contains("lunchroom doctor")
                    && written.ends_with("' >&2; exit 2; }"),
                "{old:?}: {written}"
            );
            assert_eq!(pre[1]["hooks"][0]["onFailure"], "block", "{old:?}");
            assert_eq!(lunchroom_commands(&after, "PreToolUse").len(), 1, "{old:?}");
            let backup = fs::read_dir(&s.env.claude_home)
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("settings.json.lunchroom-backup-")
                })
                .and_then(|e| fs::read_to_string(e.path()).ok());
            assert_eq!(backup.as_deref(), Some(before.as_str()), "{old:?}");
        }
        // The old forms are Lunchroom's: uninstall removes them, and keeps the user's hook.
        let s = Scratch::new();
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        let older = json!({"hooks": {"PreToolUse": [
            {"hooks": [{"type": "command", "command": "/u/a.sh"}]},
            {"hooks": [{"type": "command", "command": format!("{} guard pretool", program.display())}]}
        ]}})
        .to_string();
        assert!(fs::write(s.env.claude_home.join("settings.json"), older).is_ok());
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let left = s.settings();
        assert_eq!(
            left["hooks"]["PreToolUse"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            left["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/u/a.sh"
        );
    }

    /// Runs a hook's `command` as Claude Code and Codex run it, through `sh -c`, with `input` on
    /// its standard input: its exit code, standard output and standard error.
    fn run_hook(command: &str, input: &str) -> (i32, String, String) {
        use std::io::Write as _;
        let child = std::process::Command::new("sh")
            .args(["-c", command])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            return (-1, String::new(), "sh did not start".to_owned());
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
        match child.wait_with_output() {
            Ok(out) => (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ),
            Err(e) => (-1, String::new(), e.to_string()),
        }
    }

    /// Whether `target`'s harness blocks the tool call a hook of exit code `code` and standard
    /// error `stderr` answered. Claude Code: exit 2 blocks (`docs/host-facts/claude-code.md`).
    /// Codex 0.163.0: exit 2 blocks only with a reason on standard error (`pre_tool_use.rs` lines
    /// 261-276); any other failure lets the call run.
    fn harness_blocks(target: Target, code: i32, stderr: &str) -> bool {
        match target {
            Target::ClaudeCode => code == 2,
            Target::Codex => code == 2 && !stderr.trim().is_empty(),
        }
    }

    /// A stand-in guard program in `dir` named `lunchroom` with `body` as its shell script.
    fn stand_in(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::create_dir_all(dir);
        let path = dir.join("lunchroom");
        let _ = fs::remove_file(&path);
        assert!(fs::write(&path, format!("#!/bin/sh\n{body}\n")).is_ok());
        assert!(fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).is_ok());
        path
    }

    // §13::99, §13::100, §13::101, §13::102, §4::37 (H1.5): the tool-call hook of
    // Claude Code and of Codex, run as the harness runs it, blocks the call when the guard program
    // is missing, is killed by the system (SIGKILL, and SIGTERM) during the call, exits with an
    // error of its own, or is not executable; the line on standard error says that the guard failed
    // and the call was blocked, and names `lunchroom doctor`, reinstalling, the configuration file
    // and `lunchroom uninstall`. A guard that answers is not touched: its own exit 2 keeps its own
    // reason (no failure line), and its answer on standard output passes with exit 0.
    #[test]
    fn the_tool_call_hook_blocks_the_call_when_the_guard_fails() {
        let s = Scratch::new();
        for (target, file) in [
            (Target::ClaudeCode, s.env.claude_home.join("settings.json")),
            (Target::Codex, s.env.codex_home.join("hooks.json")),
        ] {
            let program = s.env.lunchroom_home.join("bin/lunchroom");
            let template = lunchroom_hooks(&program, target, &file)
                .into_iter()
                .find(|(event, _)| *event == "PreToolUse")
                .and_then(|(_, hook)| hook["hooks"][0]["command"].as_str().map(str::to_owned))
                .unwrap_or_default();
            // The same command with the stand-in in folder `dir` as the guard program.
            let at = |dir: &str| {
                let stand = s.root.join(dir).join("lunchroom");
                template.replace(&program_word(&program), &program_word(&stand))
            };
            assert!(template.starts_with(&program_word(&program)));
            for (what, body) in [
                ("killed with SIGKILL", Some("kill -9 $$")),
                ("stopped with SIGTERM", Some("kill -15 $$")),
                ("an error of its own", Some("echo oops >&2; exit 1")),
                ("a fault exit 101", Some("exit 101")),
                ("missing", None),
            ] {
                let dir = format!("guard-{}", what.replace(' ', "-"));
                if let Some(body) = body {
                    stand_in(&s.root.join(&dir), body);
                }
                let (code, stdout, stderr) = run_hook(&at(&dir), "{}");
                assert_eq!(code, 2, "{target:?} {what}: {stderr}");
                assert!(stdout.is_empty(), "{target:?} {what}: {stdout}");
                assert!(
                    harness_blocks(target, code, &stderr),
                    "{target:?} {what}: {stderr}"
                );
                for named in [
                    "the guard failed",
                    "tool call was blocked",
                    "lunchroom doctor",
                    "reinstall Lunchroom",
                    "lunchroom uninstall",
                    &file.display().to_string(),
                ] {
                    assert!(
                        stderr.contains(named),
                        "{target:?} {what}: {named}: {stderr}"
                    );
                }
                // The line is the last of standard error, before the exit.
                assert!(
                    stderr.trim_end().ends_with("lunchroom uninstall."),
                    "{target:?} {what}: {stderr}"
                );
            }
            // Not executable.
            let dir = s.root.join("guard-not-executable");
            let path = stand_in(&dir, "exit 0");
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
            }
            let (code, _, stderr) = run_hook(&at("guard-not-executable"), "{}");
            assert_eq!(
                (code, harness_blocks(target, code, &stderr)),
                (2, true),
                "{stderr}"
            );
            // A guard that answers: its own block keeps its reason, its allow passes.
            stand_in(
                &s.root.join("guard-blocks"),
                "echo 'DENIED: no' >&2; exit 2",
            );
            let (code, stdout, stderr) = run_hook(&at("guard-blocks"), "{}");
            assert_eq!(
                (code, stdout.as_str(), stderr.as_str()),
                (2, "", "DENIED: no\n")
            );
            assert!(harness_blocks(target, code, &stderr));
            stand_in(
                &s.root.join("guard-allows"),
                "cat >/dev/null; echo '{\"ok\":1}'",
            );
            let (code, stdout, stderr) = run_hook(&at("guard-allows"), "{}");
            assert_eq!(
                (code, stdout.as_str(), stderr.as_str()),
                (0, "{\"ok\":1}\n", "")
            );
        }
    }

    /// `program` as the program word of a hook command written by this install.
    fn program_word(program: &Path) -> String {
        shell_word(&program.display().to_string())
    }

    // §13::101, §13::102: a configuration file whose name holds a single quote, a space
    // or a `%` still gives one line, naming the file, and the command is one valid shell command.
    #[test]
    fn the_failure_line_names_a_file_with_a_quote_in_its_name() {
        let dir = Path::new("/tmp/it's a %s dir");
        let file = dir.join("settings.json");
        let command = pretool_command("/nonexistent/lunchroom", &file);
        let (code, stdout, stderr) = run_hook(&command, "");
        assert_eq!(code, 2, "{stderr}");
        assert_eq!(stdout, "");
        assert!(stderr.contains(&file.display().to_string()), "{stderr}");
        // One line of ours, after what the shell said of the missing program.
        assert_eq!(
            stderr
                .lines()
                .filter(|l| l.starts_with("Lunchroom: "))
                .count(),
            1,
            "{stderr}"
        );
        // And it is read back as Lunchroom's: the hook the install replaces and `uninstall` removes.
        assert!(is_hook_arguments(program_and_args(&command).1));
    }

    // §13::83, §13::97, §13::98, §13::105, §13::106, §13::104: the prompt hook has no
    // `|| exit 2` and no `onFailure`, so a guard that fails there (program missing, killed) answers
    // with an exit code Claude Code and Codex do not read as a block (not 2): the user's typing
    // reaches the agent; a guard that itself blocks a prompt carrying another agent's text (T1)
    // exits 2 with its reason, which both harnesses honour. Claude Code's `onFailure` is on the
    // tool-call hook only, and Codex's hooks carry none.
    #[test]
    fn the_prompt_hook_never_blocks_the_users_typing() {
        let s = Scratch::new();
        for (target, file) in [
            (Target::ClaudeCode, s.env.claude_home.join("settings.json")),
            (Target::Codex, s.env.codex_home.join("hooks.json")),
        ] {
            let hooks = lunchroom_hooks(&s.env.lunchroom_home.join("bin/lunchroom"), target, &file);
            let with_on_failure: Vec<&str> = hooks
                .iter()
                .filter(|(_, group)| group["hooks"][0].get("onFailure").is_some())
                .map(|(event, _)| *event)
                .collect();
            let with_exit: Vec<&str> = hooks
                .iter()
                .filter(|(_, group)| {
                    group["hooks"][0]["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("exit 2"))
                })
                .map(|(event, _)| *event)
                .collect();
            assert_eq!(with_exit, ["PreToolUse"], "{target:?}");
            match target {
                Target::ClaudeCode => assert_eq!(with_on_failure, ["PreToolUse"]),
                Target::Codex => assert_eq!(with_on_failure, [] as [&str; 0]),
            }
            let prompt = hooks
                .iter()
                .find(|(event, _)| *event == "UserPromptSubmit")
                .map(|(_, group)| {
                    group["hooks"][0]["command"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            let program = s.env.lunchroom_home.join("bin/lunchroom");
            for (what, body) in [
                ("missing", None),
                ("killed", Some("kill -9 $$")),
                ("an error", Some("exit 1")),
            ] {
                let dir = format!("prompt-{what}");
                if let Some(body) = body {
                    stand_in(&s.root.join(&dir), body);
                }
                let command = prompt.replace(
                    &program_word(&program),
                    &shell_word(&s.root.join(&dir).join("lunchroom").display().to_string()),
                );
                let (code, _, stderr) = run_hook(&command, "{}");
                assert_ne!(code, 0, "{target:?} {what}");
                assert_ne!(code, 2, "{target:?} {what}");
                assert!(!harness_blocks(target, code, &stderr), "{target:?} {what}");
            }
            // The guard's own block of a prompt carrying another agent's text passes through.
            stand_in(
                &s.root.join("prompt-blocks"),
                "echo 'another agent wrote this' >&2; exit 2",
            );
            let command = prompt.replace(
                &program_word(&program),
                &shell_word(&s.root.join("prompt-blocks/lunchroom").display().to_string()),
            );
            let (code, _, stderr) = run_hook(&command, "{}");
            assert!(harness_blocks(target, code, &stderr), "{target:?}");
            assert_eq!(stderr, "another agent wrote this\n");
        }
    }

    // §13::4 (R3.1): which commands are Lunchroom's hooks. The install writes, reads
    // back, replaces and removes the failure branch of H1.5 exactly as it writes it; an earlier
    // `guard pretool` is still Lunchroom's (the 1.0.1 installer wrote it); a bare `|| exit 2`, a
    // user's own `guard pretool || <something else>`, a hook that runs the guard through another program, and a line with a
    // quote that ends the word early are not.
    #[test]
    fn only_the_exact_failure_branch_is_lunchroom_s_tool_call_hook() {
        let program = Path::new("/home/u/.lunchroom/bin/lunchroom");
        let home = Path::new("/home/u");
        let file = Path::new("/home/u/.claude/settings.json");
        let written = pretool_command(&program.display().to_string(), file);
        let hook = |c: &str| json!({"type": "command", "command": c});
        for ours in [
            written.clone(),
            format!("{} guard pretool", program.display()),
            pretool_command(
                &program.display().to_string(),
                Path::new("/a b/it's/settings.json"),
            ),
        ] {
            assert!(is_lunchroom_hook(&hook(&ours), program, home), "{ours}");
        }
        for theirs in [
            format!("{} guard pretool || exit 2", program.display()),
            format!("'{}' guard pretool || exit 2", program.display()),
            format!("{} guard pretool || true", program.display()),
            format!("{} guard pretool || exit 0", program.display()),
            format!("{} guard pretool || exit 2; rm -rf /", program.display()),
            format!(
                "{} guard pretool || {{ [ $? -eq 2 ] || printf '%s\\n' 'x'; rm y; echo 'z' >&2; exit 2; }}",
                program.display()
            ),
            format!("env FOO=1 {} guard pretool || exit 2", program.display()),
            format!("timeout 5 {written}"),
            "/usr/bin/other guard pretool || exit 2".to_owned(),
            written.replace("exit 2; }", "exit 0; }"),
            format!("{written} && other"),
            // Fable's LOW: between the fixed head and tail, only the line the install writes.
            // An empty line, no file name, a line that says something else, an extra command
            // (inside the branch, before the exit, and after the line), a changed test or exit.
            format!(
                "{} guard pretool {FAILURE_HEAD}{FAILURE_TAIL}",
                program.display()
            ),
            format!(
                "{} guard pretool {FAILURE_HEAD}{FAILED_LINE_HEAD}{FAILED_LINE_TAIL}{FAILURE_TAIL}",
                program.display()
            ),
            format!(
                "{} guard pretool {FAILURE_HEAD}the guard is fine{FAILURE_TAIL}",
                program.display()
            ),
            written.replace("' >&2; exit 2; }", "' >&2; rm -rf x; exit 2; }"),
            written.replace("printf '%s\\n' '", "true; printf '%s\\n' '"),
            written.replace("[ $? -eq 2 ]", "[ $? -eq 3 ]"),
            written.replace("exit 2; }", "exit 3; }"),
            written.replace(" or run lunchroom uninstall.", " or do nothing."),
            written.replace("Lunchroom: the guard failed", "Lunchroom: the guard worked"),
        ] {
            assert!(
                !is_lunchroom_hook(&hook(&theirs), program, home),
                "{theirs}"
            );
        }
        // Whitespace the install did not write (Windows line endings, a tab, a line break) is still
        // Lunchroom's hook, so an install replaces it and uninstall removes it, but it is not the
        // form that blocks: the shell reads a line break as a second command.
        for odd in [
            format!("{written}\r"),
            format!("{written}\r\n"),
            written.replace("; exit 2", ";\nexit 2"),
            written.replace(" || { ", " ||\t{ "),
            written.replace(" >&2; ", " >&2;\r\n "),
        ] {
            assert!(is_lunchroom_hook(&hook(&odd), program, home), "{odd:?}");
            assert!(
                !is_fail_closed_for(program_and_args(&odd).1, file),
                "{odd:?}"
            );
        }
        assert!(is_fail_closed_for(program_and_args(&written).1, file));
        // The evidence reads the same forms.
        let guard = |command: &str| {
            crate::evidence::guard_program(
                &hook(command),
                "pretool",
                crate::evidence::PreTool::Older,
                Some(file),
            )
            .is_some()
        };
        assert!(guard(&written));
        assert!(guard(&format!("{} guard pretool", program.display())));
        assert!(!guard(&format!(
            "{} guard pretool || true",
            program.display()
        )));
        assert!(!guard(&written.replace("exit 2; }", "exit 0; }")));
    }

    //H1.5 (a configuration path that is not UTF-8): the hook's line names the file in
    // text and evidence takes the hook only when the name is the path byte for byte, so the install
    // refuses such a path, for Claude Code and for Codex, before it writes the program or the
    // settings or asks for a confirmation.
    #[test]
    fn the_install_refuses_a_configuration_path_that_is_not_text() {
        use std::os::unix::ffi::OsStringExt as _;
        for target in ["claude-code", "codex"] {
            let mut s = Scratch::new();
            let dir = s
                .root
                .join(std::ffi::OsString::from_vec(b"not-text-\xff".to_vec()));
            assert!(fs::create_dir_all(&dir).is_ok());
            if target == "codex" {
                s.env.codex_home = dir.clone();
            } else {
                s.env.claude_home = dir.clone();
            }
            let mut user = Answers {
                answers: vec!["yes\n"],
                asked: Vec::new(),
            };
            let out = install(&[target.to_owned()], &s.env, &mut user);
            assert_eq!(out.code, 2, "{target}: {}{}", out.stdout, out.stderr);
            assert!(out.stderr.contains("not text"), "{target}: {}", out.stderr);
            assert!(user.asked.is_empty(), "{target}: no question is asked");
            assert_eq!(
                fs::read_dir(&dir).map_or(1, Iterator::count),
                0,
                "{target}: nothing is written"
            );
            assert!(!s.env.lunchroom_home.join("bin/lunchroom").exists());
        }
    }

    //H1.5, §13::4: every character of Unicode's White_Space (U+0009-000D, 0020, 0085,
    // 00A0, 1680, 2000-200A, 2028, 2029, 202F, 205F, 3000), alone and mixed with a carriage return and
    // a line feed, in any of the places a hook's words are separated, leaves a Lunchroom hook
    // Lunchroom's: the install replaces it and uninstall removes it, the user's hook staying. A
    // character that only looks like a space (zero-width space, byte order mark, Mongolian vowel
    // separator) does not: the word it is in is another word.
    #[test]
    fn every_unicode_white_space_leaves_a_hook_lunchroom_s_to_replace_and_remove() {
        let program = Path::new("/home/u/.lunchroom/bin/lunchroom");
        let home = Path::new("/home/u");
        let file = Path::new("/home/u/.claude/settings.json");
        let p = program.display().to_string();
        let written = pretool_command(&p, file);
        let args = written
            .strip_prefix(&format!("{p} "))
            .unwrap_or_default()
            .to_owned();
        let table: Vec<char> = ('\u{9}'..='\u{d}')
            .chain(['\u{20}', '\u{85}', '\u{a0}', '\u{1680}'])
            .chain('\u{2000}'..='\u{200a}')
            .chain(['\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}'])
            .collect();
        assert_eq!(table.len(), 25);
        let hook = |command: &str| json!({"type": "command", "command": command});
        let mut found = Vec::new();
        for c in &table {
            assert!(c.is_whitespace(), "{c:?}");
            for pad in [
                c.to_string(),
                format!("{c}\r\n"),
                format!("\r{c}\n"),
                format!("\n{c}\r"),
            ] {
                for variant in [
                    format!("{p}{pad}{args}"),
                    format!(
                        "{p} {}",
                        args.replacen("guard pretool", &format!("guard{pad}pretool"), 1)
                    ),
                    format!("{p} {}", args.replacen("||", &format!("{pad}||{pad}"), 1)),
                    format!("{written}{pad}"),
                    format!("{p}{pad}guard{pad}pretool"),
                    format!("{p} guard{pad}pretool{pad}"),
                ] {
                    assert!(
                        is_lunchroom_hook(&hook(&variant), program, home),
                        "{c:?}: {variant:?}"
                    );
                    found.push(variant);
                }
            }
        }
        // Install and uninstall: every variant goes, the user's hook stays.
        let mut groups = vec![json!({"hooks": [hook("/u/mine.sh")]})];
        groups.extend(found.iter().map(|v| json!({"hooks": [hook(v)]})));
        let mut hooks = Map::new();
        hooks.insert("PreToolUse".to_owned(), Value::Array(groups));
        let places = strip_lunchroom_hooks(&mut hooks, program, home);
        assert_eq!(places.get("PreToolUse"), Some(&1));
        assert_eq!(
            hooks["PreToolUse"],
            json!([{"hooks": [{"type": "command", "command": "/u/mine.sh"}]}])
        );
        // Look-alikes of a space are not one.
        for like in ['\u{200b}', '\u{feff}', '\u{180e}', '\u{2060}'] {
            assert!(!like.is_whitespace(), "{like:?}");
            for variant in [
                format!("{p} guard{like}pretool"),
                format!("{p}{like}guard pretool"),
                written.replacen("guard pretool", &format!("guard{like}pretool"), 1),
            ] {
                assert!(
                    !is_lunchroom_hook(&hook(&variant), program, home),
                    "{like:?}: {variant:?}"
                );
            }
        }
    }

    // A1.3 with §13::4: what the user changed in the settings while the question was shown is kept;
    // if it is no longer a JSON object, nothing is changed.
    #[test]
    fn installer_writes_from_the_settings_as_they_are_after_the_yes() {
        let s = Scratch::new();
        let path = s.env.home.join(".claude/settings.json");
        let mut user = EditsWhileAsking {
            path: path.clone(),
            text: Some(
                r#"{"model": "haiku", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/u/new.sh"}]}]}}"#,
            ),
        };
        let out = install(&["claude-code".to_owned()], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let settings = s.settings();
        assert_eq!(settings["model"], "haiku");
        assert!(settings["permissions"].is_null());
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/u/new.sh"
        );
        assert_eq!(settings["hooks"]["Stop"].as_array().map(Vec::len), Some(2));
        let broken = Scratch::new();
        let path = broken.env.home.join(".claude/settings.json");
        let mut user = EditsWhileAsking {
            path: path.clone(),
            text: Some("[1, 2]"),
        };
        let out = install(&["claude-code".to_owned()], &broken.env, &mut user);
        assert_eq!(out.code, 2);
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some("[1, 2]"));
        assert!(!broken.env.lunchroom_home.join("bin/lunchroom").exists());
    }

    /// The commands of `settings`' hooks for `event` that are not Lunchroom's, as (matcher,
    /// commands) per group; a group with nothing else is left out.
    fn others(settings: &Value, event: &str, program: &str) -> Vec<(Option<String>, Vec<String>)> {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| {
                let commands: Vec<String> = g["hooks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|h| h["command"].as_str())
                    .filter(|c| !c.starts_with(program))
                    .map(str::to_owned)
                    .collect();
                (!commands.is_empty()).then(|| (g["matcher"].as_str().map(str::to_owned), commands))
            })
            .collect()
    }

    /// Lunchroom's own commands in `settings`' hooks for `event`.
    fn ours(settings: &Value, event: &str, program: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_owned))
            .filter(|c| c.starts_with(program))
            .collect()
    }

    // §13::4: on a full settings file (an earlier Lunchroom install, one shared
    // UserPromptSubmit group) every other hook stays in its group, exactly one Lunchroom hook
    // stands per event, every other key is unchanged, and a second install changes nothing.
    #[test]
    fn a_full_settings_file_keeps_every_other_hook() {
        let s = Scratch::new();
        let program = s
            .env
            .lunchroom_home
            .join("bin/lunchroom")
            .display()
            .to_string();
        let text = include_str!("../tests/fixtures/settings-claude.json")
            .replace("__LUNCHROOM__", &program);
        let path = s.env.home.join(".claude/settings.json");
        assert!(fs::write(&path, &text).is_ok());
        let before: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let after = s.settings();
        for event in [
            "SessionStart",
            "SessionEnd",
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "Stop",
        ] {
            assert_eq!(
                others(&after, event, &program),
                others(&before, event, &program),
                "{event}"
            );
        }
        let one = |event: &str, rest: &str| {
            assert_eq!(
                ours(&after, event, &program),
                [format!("{program} {rest}")],
                "{event}"
            );
        };
        assert_eq!(
            ours(&after, "PreToolUse", &program),
            [pretool_command(
                &program,
                &s.env.claude_home.join("settings.json")
            )]
        );
        one("PostToolUse", "guard posttool");
        one("UserPromptSubmit", "guard prompt");
        assert_eq!(
            ours(&after, "SessionStart", &program),
            [
                format!("{program} guard start"),
                format!("{program} listen")
            ]
        );
        one("Stop", "listen");
        assert_eq!(ours(&after, "SessionEnd", &program), Vec::<String>::new());
        for (key, value) in before.as_object().into_iter().flatten() {
            if key != "hooks" {
                assert_eq!(&after[key.as_str()], value, "{key}");
            }
        }
        let files = || fs::read_dir(s.env.home.join(".claude")).map_or(0, |d| d.flatten().count());
        let (bytes, n) = (fs::read(&path).ok(), files());
        let again = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(again.code, 0, "{}", again.stderr);
        assert_eq!(fs::read(&path).ok(), bytes);
        assert_eq!(files(), n);
    }

    // §13::4: an earlier install under another name, a link to the program, leaves no
    // command by that name in Codex's hooks.json; a program by that name that is not a link to
    // Lunchroom's is another application's, and its hooks stay.
    #[test]
    fn an_install_under_the_old_name_leaves_no_old_command() {
        for as_link in [true, false] {
            let s = Scratch::new();
            let program = s.env.lunchroom_home.join("bin/lunchroom");
            let old_dir = s.env.home.join(".oldname/bin");
            for dir in [
                program.parent(),
                Some(old_dir.as_path()),
                Some(s.env.codex_home.as_path()),
            ]
            .into_iter()
            .flatten()
            {
                assert!(fs::create_dir_all(dir).is_ok());
            }
            assert!(fs::write(&program, "x").is_ok());
            if as_link {
                assert!(std::os::unix::fs::symlink(&program, old_dir.join("oldname")).is_ok());
            } else {
                assert!(fs::write(old_dir.join("oldname"), "#!/usr/bin/env python3\n").is_ok());
            }
            let text = include_str!("../tests/fixtures/codex-hooks-old-name.json")
                .replace("__HOME__", &s.env.home.display().to_string());
            let path = s.env.codex_home.join("hooks.json");
            assert!(fs::write(&path, text).is_ok());
            let out = install(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 0, "{}", out.stderr);
            let after = fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                after.contains("oldname"),
                !as_link,
                "link={as_link}: {after}"
            );
            let hooks: Value = serde_json::from_str(&after).unwrap_or(Value::Null);
            let p = program.display().to_string();
            assert_eq!(
                ours(&hooks, "PreToolUse", &p),
                [pretool_command(&p, &path)],
                "PreToolUse"
            );
            for (event, rest) in [
                ("PostToolUse", "guard posttool"),
                ("UserPromptSubmit", "guard prompt"),
            ] {
                assert_eq!(ours(&hooks, event, &p), [format!("{p} {rest}")], "{event}");
            }
        }
    }

    // H1.4, A1.3: on a full settings file, uninstall removes only Lunchroom's hooks on
    // the user's yes, keeps a backup, and changes nothing without the yes or when none is left.
    #[test]
    fn uninstall_removes_only_lunchroom_hooks_on_the_users_yes() {
        let s = Scratch::new();
        let program = s
            .env
            .lunchroom_home
            .join("bin/lunchroom")
            .display()
            .to_string();
        let text = include_str!("../tests/fixtures/settings-claude.json")
            .replace("__LUNCHROOM__", &program);
        let path = s.env.home.join(".claude/settings.json");
        assert!(fs::write(&path, &text).is_ok());
        let before: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let no = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("no\n"));
        assert_eq!(no.code, 3);
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str())
        );
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.stdout.contains("the earlier settings are kept at"),
            "{}",
            out.stdout
        );
        let after = s.settings();
        for event in [
            "SessionStart",
            "SessionEnd",
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "Stop",
        ] {
            assert_eq!(
                ours(&after, event, &program),
                Vec::<String>::new(),
                "{event}"
            );
            assert_eq!(
                others(&after, event, &program),
                others(&before, event, &program),
                "{event}"
            );
        }
        for (key, value) in before.as_object().into_iter().flatten() {
            if key != "hooks" {
                assert_eq!(&after[key.as_str()], value, "{key}");
            }
        }
        let again = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert!(
            again.stdout.contains("holds no Lunchroom hooks"),
            "{}",
            again.stdout
        );
        // OpenClaw and Hermes: the harness's own command that disables the plugin is named.
        for (harness, command) in [
            ("openclaw", "openclaw plugins disable lunchroom"),
            ("hermes", "hermes plugins disable lunchroom"),
        ] {
            let out = uninstall(&[harness.to_owned()], &s.env, &mut UserAt("yes\n"));
            assert_eq!(out.code, 0, "{harness}");
            assert!(out.stdout.contains(command), "{}", out.stdout);
        }
        assert_eq!(
            uninstall(&["nothing".to_owned()], &s.env, &mut UserAt("yes\n")).code,
            2
        );
    }

    // H1.4, A1.3: uninstall of a partly installed state (some of Lunchroom's hooks gone, its
    // program never there or deleted, an event shared with another hook, a file without hooks or
    // none at all) removes what is there on the user's yes and keeps every other hook, every other
    // key and each other hook's place; a file holding none of Lunchroom's hooks, or no file, is left
    // as it is, and without the yes nothing changes. The same for Codex's `hooks.json`.
    #[test]
    #[allow(clippy::too_many_lines)] // one case per partial state, each checked in full
    fn uninstall_of_a_partly_installed_state_removes_what_is_there() {
        let s = Scratch::new();
        // Lunchroom's program is not installed: nothing of it exists beside the hooks.
        let program = s.env.lunchroom_home.join("bin/lunchroom");
        assert!(!program.exists());
        let program = program.display().to_string();
        let path = s.env.home.join(".claude/settings.json");
        let text = format!(
            r#"{{
  "model": "opus",
  "hooks": {{
    "PreToolUse": [
      {{"hooks": [{{"type": "command", "command": "{program} guard pretool", "timeout": 15}}]}},
      {{"matcher": "Bash", "hooks": [{{"type": "command", "command": "/usr/local/bin/check-a"}}]}}
    ],
    "UserPromptSubmit": [
      {{"hooks": [
        {{"type": "command", "command": "/usr/local/bin/prompt-a"}},
        {{"type": "command", "command": "{program} guard prompt"}},
        {{"type": "command", "command": "/usr/local/bin/prompt-b"}}
      ]}}
    ],
    "Stop": [
      {{"hooks": [{{"type": "command", "command": "{program} listen", "asyncRewake": true}}]}}
    ],
    "SessionEnd": [
      {{"hooks": [{{"type": "command", "command": "/usr/local/bin/session-end"}}]}}
    ]
  }},
  "permissions": {{"deny": ["Read(./.env)"]}}
}}
"#
        );
        assert!(fs::write(&path, &text).is_ok());
        let before: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let no = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("no\n"));
        assert_eq!(no.code, 3);
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str())
        );
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let after = s.settings();
        // The events that held Lunchroom's hooks lose them and keep the rest in place; an event
        // with only Lunchroom's hook goes; events Lunchroom never had are not added.
        assert_eq!(
            after["hooks"]["PreToolUse"],
            json!([before["hooks"]["PreToolUse"][1].clone()])
        );
        assert_eq!(
            others(&after, "UserPromptSubmit", &program),
            vec![(
                None,
                vec![
                    "/usr/local/bin/prompt-a".to_owned(),
                    "/usr/local/bin/prompt-b".to_owned()
                ]
            )]
        );
        assert_eq!(after["hooks"]["SessionEnd"], before["hooks"]["SessionEnd"]);
        let mut events: Vec<&str> = after["hooks"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(event, _)| event.as_str())
            .collect();
        events.sort_unstable();
        assert_eq!(events, ["PreToolUse", "SessionEnd", "UserPromptSubmit"]);
        for event in ["PreToolUse", "UserPromptSubmit", "Stop"] {
            assert_eq!(
                ours(&after, event, &program),
                Vec::<String>::new(),
                "{event}"
            );
        }
        assert_eq!(after["model"], before["model"]);
        assert_eq!(after["permissions"], before["permissions"]);
        // Nothing of Lunchroom's is left, so a second uninstall changes nothing.
        let bytes = fs::read(&path).ok();
        let again = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert!(
            again.stdout.contains("holds no Lunchroom hooks"),
            "{}",
            again.stdout
        );
        assert_eq!(fs::read(&path).ok(), bytes);
        // Only Lunchroom's hooks were there: none is left, and the file stays a settings file.
        let only = format!(
            r#"{{"model": "opus", "hooks": {{"PreToolUse": [{{"hooks": [{{"type": "command", "command": "{program} guard pretool"}}]}}]}}}}"#
        );
        assert!(fs::write(&path, only).is_ok());
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let after = s.settings();
        assert_eq!(after["model"], "opus");
        assert!(after["hooks"].as_object().is_some_and(Map::is_empty));
        // A file without hooks is left as it is.
        let plain = r#"{"model": "opus"}"#;
        assert!(fs::write(&path, plain).is_ok());
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert!(
            out.stdout.contains("holds no Lunchroom hooks"),
            "{}",
            out.stdout
        );
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some(plain));
        // No file: nothing is made.
        let files = || fs::read_dir(s.env.home.join(".claude")).map_or(0, |d| d.flatten().count());
        assert!(fs::remove_file(&path).is_ok());
        let n = files();
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            out.stdout.contains("holds no Lunchroom hooks"),
            "{}",
            out.stdout
        );
        assert!(!path.exists());
        assert_eq!(files(), n);
        // Codex: one of Lunchroom's hooks beside another program's.
        assert!(fs::create_dir_all(&s.env.codex_home).is_ok());
        let codex = s.env.codex_home.join("hooks.json");
        let text = format!(
            r#"{{"hooks": {{"PostToolUse": [{{"hooks": [
  {{"type": "command", "command": "/usr/local/bin/post"}},
  {{"type": "command", "command": "{program} guard posttool"}}]}}]}}}}"#
        );
        assert!(fs::write(&codex, text).is_ok());
        let out = uninstall(&["codex".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        let after: Value = fs::read_to_string(&codex)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        assert_eq!(
            after["hooks"]["PostToolUse"][0]["hooks"],
            json!([{"type": "command", "command": "/usr/local/bin/post"}])
        );
    }

    /// A user who has set no passphrase yet: the root program's prompt confirms the install, the
    /// user answers the hold mode, types the secrets in turn, and has the system put a verifier in
    /// place as root's.
    struct NewUser {
        answers: Vec<&'static str>,
        secrets: Vec<&'static str>,
        asked: Vec<String>,
        prompts: Vec<String>,
        verifier: Result<String, String>,
        installed: Option<String>,
        install: Result<(), String>,
        blocker: Option<String>,
        /// What the root program's own prompt answers.
        root: Result<(), String>,
    }

    impl NewUser {
        fn new(answers: &[&'static str], secrets: &[&'static str]) -> Self {
            Self {
                answers: answers.to_vec(),
                secrets: secrets.to_vec(),
                asked: Vec::new(),
                prompts: Vec::new(),
                verifier: Err(NO_VERIFIER.to_owned()),
                installed: None,
                install: Ok(()),
                blocker: None,
                root: Ok(()),
            }
        }
    }

    impl System for NewUser {
        fn verifier_blocker(&self) -> Option<String> {
            self.blocker.clone()
        }
        fn confirm_root(&mut self, question: &str) -> Result<(), String> {
            self.asked.push(question.to_owned());
            self.root.clone()
        }
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            UserAt("").ancestors()
        }
        fn own_tty(&self) -> u64 {
            UserAt("").own_tty()
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            UserAt("").tty_of(key)
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            (!self.answers.is_empty()).then(|| self.answers.remove(0).to_owned())
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            self.verifier.clone()
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.prompts.push(prompt.to_owned());
            (!self.secrets.is_empty()).then(|| self.secrets.remove(0).to_owned())
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            Ok([8; lunchroom_core::passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, line: &str) -> Result<(), String> {
            self.install.clone()?;
            self.installed = Some(line.to_owned());
            self.verifier = Ok(line.to_owned());
            Ok(())
        }
    }

    const FIRST: &str = "my first passphrase";

    // A1.3: the install that finds no passphrase is confirmed by the root program's own prompt
    // (never by a typed yes), because there is nothing to check a passphrase against; once the
    // hooks are written it sets the first passphrase (asked twice, put in place as root's, read
    // back) and says so.
    #[test]
    fn the_first_install_is_confirmed_by_the_root_program_and_sets_the_passphrase() {
        let s = Scratch::new();
        let mut user = NewUser::new(&["2\n"], &[FIRST, FIRST]);
        let out = install(&["claude-code".to_owned()], &s.env, &mut user);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            user.asked[0].contains("Lunchroom: add Lunchroom to")
                && !user.asked[0].contains("Type yes"),
            "{}",
            user.asked[0]
        );
        assert_ne!(
            lunchroom_commands(&s.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
        assert!(
            out.stdout.contains("Setting your passphrase:\n"),
            "{}",
            out.stdout
        );
        assert!(
            out.stdout.contains("the passphrase is set;"),
            "{}",
            out.stdout
        );
        assert!(out.stderr.is_empty(), "{}", out.stderr);
        assert_eq!(user.prompts.len(), 2);
        let line = user.installed.clone().unwrap_or_default();
        assert_eq!(
            lunchroom_core::passphrase::Verifier::parse(&line, crate::terminal::ITERATIONS)
                .map(|v| v.matches(FIRST)),
            Ok(true)
        );
    }

    // A1.3: an install the root program's prompt refuses changes nothing and sets no passphrase; an install that
    // cannot put the verifier in place is done, and says that the passphrase is not set and that
    // the commands only the user may run are denied until it is.
    #[test]
    fn a_declined_first_install_sets_nothing_and_a_failed_passphrase_step_is_reported() {
        let s = Scratch::new();
        let mut no = NewUser::new(&[], &[FIRST, FIRST]);
        no.root = Err("sudo -v did not succeed; nothing was written".to_owned());
        assert_eq!(
            install(&["claude-code".to_owned()], &s.env, &mut no).code,
            3
        );
        assert_eq!(no.prompts, Vec::<String>::new());
        assert_eq!(no.installed, None);
        assert!(!s.env.lunchroom_home.join("bin/lunchroom").exists());
        let mut failing = NewUser::new(&["2\n"], &[FIRST, FIRST]);
        failing.install = Err("making /etc/lunchroom did not succeed (exit status: 1)".to_owned());
        let out = install(&["claude-code".to_owned()], &s.env, &mut failing);
        // The hooks are written, and the install fails: no passphrase is set.
        assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
        assert_ne!(
            lunchroom_commands(&s.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
        assert_eq!(failing.installed, None);
        assert!(
            out.stderr
                .contains("the passphrase was not set: making /etc/lunchroom"),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("The hooks are in place, but no passphrase is set"),
            "{}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("denied until `lunchroom passphrase` succeeds"),
            "{}",
            out.stderr
        );
    }

    // A1.3: a system that cannot put the verifier in place (no sudo) is found before the user types
    // a passphrase: none is asked for, the hooks stay, the install exits non-zero and says what to do.
    #[test]
    fn a_system_that_cannot_hold_the_verifier_fails_the_install_before_any_passphrase_is_typed() {
        let s = Scratch::new();
        let mut user = NewUser::new(&["2\n"], &[FIRST, FIRST]);
        user.blocker = Some("sudo was not found; install sudo".to_owned());
        let out = install(&["claude-code".to_owned()], &s.env, &mut user);
        assert_ne!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert_eq!(user.prompts, Vec::<String>::new());
        assert_eq!(user.installed, None);
        assert_ne!(
            lunchroom_commands(&s.settings(), "PreToolUse"),
            Vec::<Value>::new()
        );
        assert!(out.stderr.contains("sudo was not found"), "{}", out.stderr);
        assert!(
            out.stderr
                .contains("The hooks are in place, but no passphrase is set")
                && out.stderr.contains("`lunchroom passphrase`"),
            "{}",
            out.stderr
        );
    }

    // A1.3: with a passphrase set, the install asks for it and sets no new one; with none set, a
    // command other than the install is denied with the command that sets one, and uninstall tells
    // the user that `sudo rm` of the check removes it, which Lunchroom never does itself.
    #[test]
    fn later_installs_and_uninstall_ask_for_the_passphrase() {
        let s = Scratch::new();
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            !out.stdout.contains("Setting your passphrase"),
            "{}",
            out.stdout
        );
        let before = fs::read(s.env.home.join(".claude/settings.json")).ok();
        let mut none = NewUser::new(&["yes\n"], &[FIRST]);
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut none);
        assert_eq!(out.code, 3, "{}", out.stdout);
        assert!(
            out.stderr.contains("`lunchroom passphrase`"),
            "{}",
            out.stderr
        );
        assert!(none.asked.is_empty() && none.prompts.is_empty());
        assert_eq!(
            fs::read(s.env.home.join(".claude/settings.json")).ok(),
            before
        );
        let wrong = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("no\n"));
        assert_eq!(wrong.code, 3);
        assert!(
            wrong.stderr.contains("the passphrase was wrong"),
            "{}",
            wrong.stderr
        );
        assert_eq!(
            fs::read(s.env.home.join(".claude/settings.json")).ok(),
            before
        );
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            out.stdout.contains(
                "Your passphrase check stays: `sudo rm /etc/lunchroom/1000.verifier` removes it."
            ),
            "{}",
            out.stdout
        );
    }

    // G3-5: Claude Code is found where Claude Code reads its settings: a folder other than
    // `~/.claude` (`CLAUDE_CONFIG_DIR`) counts, and `~/.claude` alone no longer decides. Today's
    // code looked only at `~/.claude`, so a setup with the settings folder elsewhere was skipped.
    #[test]
    fn claude_code_is_found_in_the_folder_it_reads() {
        let s = Scratch::new();
        let names = |found: Vec<(&str, &str)>| -> Vec<String> {
            found.into_iter().map(|(word, _)| word.to_owned()).collect()
        };
        assert!(names(found_harnesses(&s.env)).contains(&"claude-code".to_owned()));
        let _ = fs::remove_dir_all(s.env.home.join(".claude"));
        assert!(!names(found_harnesses(&s.env)).contains(&"claude-code".to_owned()));
        let elsewhere = s.root.join("claude-config");
        let _ = fs::create_dir_all(&elsewhere);
        assert!(names(found_harnesses_in(&s.env, &elsewhere)).contains(&"claude-code".to_owned()));
        assert!(
            !names(found_harnesses_in(&s.env, &s.root.join("missing")))
                .contains(&"claude-code".to_owned())
        );
    }

    // DV-16 (H1.4, C1.4): install finds and writes Claude Code's folder as `CLAUDE_CONFIG_DIR`
    // names it, else `~/.claude`; uninstall removes from the same one. Before the fix `install`
    // wrote `~/.claude` whatever the variable said, a folder Claude Code did not read, so the fix
    // doctor named never closed the loop.
    #[test]
    fn install_writes_the_folder_claude_code_reads() {
        let mut s = Scratch::new();
        let elsewhere = s.root.join("claude-config");
        let _ = fs::create_dir_all(&elsewhere);
        let _ = fs::remove_dir_all(s.env.home.join(".claude"));
        // `CLAUDE_CONFIG_DIR` set: that folder is found and written; `~/.claude` is not made.
        s.env.claude_home = elsewhere.clone();
        assert!(
            found_harnesses(&s.env)
                .iter()
                .any(|(word, _)| *word == "claude-code")
        );
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(elsewhere.join("settings.json").is_file(), "{}", out.stdout);
        assert!(!s.env.home.join(".claude/settings.json").exists());
        let out = uninstall(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        let left = fs::read_to_string(elsewhere.join("settings.json")).unwrap_or_default();
        assert!(!left.contains("guard"), "{left}");
        // Not set: `~/.claude`.
        s.env.claude_home = s.env.home.join(".claude");
        let _ = fs::create_dir_all(s.env.home.join(".claude"));
        let out = install(&["claude-code".to_owned()], &s.env, &mut UserAt("yes\n"));
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(s.env.home.join(".claude/settings.json").is_file());
    }

    // DV-16: the variable names the folder only when it is set and not empty.
    #[test]
    fn the_claude_folder_is_the_variable_when_set_and_not_empty() {
        let home = Path::new("/home/u");
        assert_eq!(
            crate::home::claude_home(Some("/srv/claude".to_owned()), home),
            PathBuf::from("/srv/claude")
        );
        for unset in [None, Some(String::new())] {
            assert_eq!(
                crate::home::claude_home(unset, home),
                PathBuf::from("/home/u/.claude")
            );
        }
    }
}
