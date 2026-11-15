//! The Lunchroom console (1.1): the user's own screen, run as the system user `lunchroom`.
//!
//! Every agent runs as the developer's own Linux user, and the kernel treats every process of one
//! user alike. The console runs as another user, `lunchroom`, so no process of the developer can
//! signal it, trace it, read its memory or environment, or open its terminal; and the store it
//! reads and writes is a folder only that user's group can enter (`lunchroom_core::setgid`).
//!
//! The person starts it in a terminal of their own:
//! `sudo -u lunchroom /usr/local/lib/lunchroom/lunchroom console`. It draws one screen with
//! Ratatui (§17 C2 item 10), redrawn from a read of the store that takes no lock:
//! 1. a header line, then the alert strip of six cells;
//! 2. the calls waiting, with the selected call beside them;
//! 3. the agents, with their DENIED decisions of the last hour;
//! 4. the alerts and the guard;
//! 5. the key line.
//!
//! It asks the passphrase when it starts, for every `approve` and every controller approval, for
//! every loosening action (`m balanced`, `m relaxed`, `y`), and again after 15 minutes without a
//! key, when the screen locks and only the alert strip stays drawn; `unregister` within 15 minutes
//! of the passphrase last typed rides on it (§17 C2 item 3).
//!
//! The pieces: [`model`] is what a screen shows and what the user can ask (`Snapshot`, `Cmd`,
//! `Action`, the `Backend` trait the store side implements); [`ui`] is the screen's state and key
//! handling; [`draw`] draws it; [`text`] shows agent text with each control character and
//! direction mark as its `\u{...}` code; [`live`] reads the store and runs the terminal.
//!
//! While it runs the console rewrites `console.alive` in the store folder every 2 seconds with its
//! process id and start time; a guard prompt hook that finds none says so once a day.

mod draw;
mod live;
pub(crate) mod model;
#[cfg(test)]
mod tests;
mod text;
mod ui;

use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::ExitCode;

use lunchroom_core::mail::Mail;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;

use live::{Live, RealBackend, Run, serve};

use crate::home::Env;
use crate::records::{self, Record};
use crate::terminal::{OsSystem, System};

/// The file in the store folder that says a console runs.
pub const ALIVE_FILE: &str = "console.alive";

/// The file that keeps the day the "console is not running" notice was last told.
const MISSING_DAY_FILE: &str = "console-missing.day";

/// The liveness file is written this often, in seconds.
const ALIVE_EVERY: u64 = 2;

/// The setup checks are made again this often, in seconds.
const CHECKS_EVERY: u64 = 60;

/// The notices the console keeps on its screen.
const NOTICES_KEPT: usize = 20;

/// The words of the command that starts the console as the user `lunchroom`.
fn start_words() -> [&'static str; 5] {
    [
        "sudo",
        "-u",
        setgid::SYSTEM_USER,
        setgid::PROGRAM,
        "console",
    ]
}

/// The command that starts the console as the user `lunchroom`.
#[must_use]
pub fn start_command() -> String {
    start_words().join(" ")
}

/// What `lunchroom console` does, from two facts (F2 design, "Finding and starting the console",
/// item 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launch {
    /// This process runs as the user `lunchroom`: it is the console.
    IsConsole,
    /// A developer's process, and the launch rule is there: start the console through `sudo`.
    Run,
    /// A developer's process, and the launch rule is absent: say so, and run nothing.
    NotSetUp,
}

/// The decision of item 2: `is_console` (the process runs as user `lunchroom`) first; else the
/// launch rule `/etc/sudoers.d/lunchroom` decides between starting `sudo` and saying the console
/// is not set up.
#[must_use]
pub(crate) fn launch_decision(is_console: bool, launch_rule_there: bool) -> Launch {
    if is_console {
        Launch::IsConsole
    } else if launch_rule_there {
        Launch::Run
    } else {
        Launch::NotSetUp
    }
}

/// Whether the launch rule counts as there, from what looking at it gave (`None`: it was found).
/// Only a plain "no such file" is absent: `/etc/sudoers.d` is commonly root-only (mode 0750), so
/// a developer's own look at the file is refused, which says nothing of the file; `sudo` itself
/// then answers.
#[must_use]
pub(crate) fn launch_rule_there(look: Option<std::io::ErrorKind>) -> bool {
    look != Some(std::io::ErrorKind::NotFound)
}

/// What `lunchroom console` says, and exits 3, where the console is not set up.
#[must_use]
pub(crate) fn not_set_up_text() -> String {
    "The Lunchroom console is not set up on this computer. Run `lunchroom install` to set it up, \
     then `lunchroom console`.\n"
        .to_owned()
}

/// Item 2, the thin part: runs [`start_command`] as a child on this terminal, waits for it and
/// gives its exit code (1 where it ended by a signal or did not start).
fn start_as_lunchroom() -> ExitCode {
    let [_, user_flag, user, program, console] = start_words();
    let Some(sudo) = crate::terminal::system_program("sudo") else {
        return fail("sudo was not found in the system folders", 1);
    };
    let mut command = std::process::Command::new(sudo);
    command.args([user_flag, user, program, console]);
    setgid::drop_group(&mut command);
    match command.status() {
        Ok(status) => ExitCode::from(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        ),
        Err(e) => fail(&format!("sudo could not be started: {e}"), 1),
    }
}

/// What the console says when it is not run as the user `lunchroom`.
#[must_use]
pub fn wrong_user_text() -> String {
    format!(
        "The Lunchroom console runs as the user {user}, so that nothing your agents run can reach \
         it. Start it in a terminal of your own with:\n  {}\nIt asks for your sudo password, \
         then for your Lunchroom passphrase. If sudo refuses that command, the console is not \
         set up yet: run `lunchroom install console` first.\n",
        start_command(),
        user = setgid::SYSTEM_USER
    )
}

// ---------------------------------------------------------------------------------------------
// The liveness file.
// ---------------------------------------------------------------------------------------------

/// The text of the liveness file for process `pid`: `<pid> <start time>`.
#[must_use]
pub fn alive_text(pid: u32) -> Option<String> {
    let key = crate::process::key(pid)?;
    let (pid, start) = key.split_once(':')?;
    Some(format!("{pid} {start}\n"))
}

/// Writes the liveness file in `home` whole, by a rename.
fn write_alive(home: &Path, pid: u32) -> std::io::Result<()> {
    let Some(text) = alive_text(pid) else {
        return Err(std::io::Error::other(
            "the console's own process is unreadable",
        ));
    };
    write_whole(&home.join(ALIVE_FILE), &text)
}

/// Writes `text` to `path` through a temporary file beside it, group-readable in setgid mode.
fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text)?;
    setgid::settle(&temp);
    std::fs::rename(&temp, path)
}

/// The process id in a liveness file, when that process still runs under the start time the file
/// names (a pid reused by another process, or a console that ended, is none).
#[must_use]
pub fn alive_pid(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join(ALIVE_FILE)).ok()?;
    let mut parts = text.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let start = parts.next()?;
    (crate::process::key(pid)? == format!("{pid}:{start}")).then_some(pid)
}

/// What a joined session's start adds to its start line when the console is open.
pub(crate) const CONSOLE_IS_OPEN_LINE: &str = "Your console is open.";

/// Whether the console is set up for this process: as [`missing_notice`] reads it, the process runs
/// setgid.
#[must_use]
pub(crate) fn set_up_here() -> bool {
    setgid::active()
}

/// F2 design, "Finding and starting the console", item 4: the words that follow the start line of
/// a joined session. Console not set up: none (the start line stands alone). Set up and a console
/// runs in `home` ([`alive_pid`]): [`CONSOLE_IS_OPEN_LINE`]. Set up and none runs:
/// [`crate::cli::OPEN_CONSOLE_LINE`].
#[must_use]
pub(crate) fn start_line_addition(set_up: bool, home: &Path) -> Option<&'static str> {
    if !set_up {
        return None;
    }
    Some(if alive_pid(home).is_some() {
        CONSOLE_IS_OPEN_LINE
    } else {
        crate::cli::OPEN_CONSOLE_LINE
    })
}

/// What a guard prompt hook tells the user when the console is set up and none runs; told once
/// per day, by the day number it keeps in `home`. `alive`: a console runs now.
#[must_use]
pub fn missing_notice_at(home: &Path, now: u64, alive: bool) -> Option<String> {
    if alive {
        return None;
    }
    let day = (now / 86_400).to_string();
    let file = home.join(MISSING_DAY_FILE);
    if std::fs::read_to_string(&file).is_ok_and(|told| told.trim() == day) {
        return None;
    }
    write_whole(&file, &day).ok()?;
    Some(format!(
        "The Lunchroom console is not running: start it with {} in a terminal of your own.",
        start_command()
    ))
}

/// [`missing_notice_at`] for this process: only where the console is set up (the process runs
/// setgid, so the store is the fixed one).
#[must_use]
pub fn missing_notice(env: &Env) -> Option<String> {
    if !setgid::active() {
        return None;
    }
    missing_notice_at(
        &env.lunchroom_home,
        env.now,
        alive_pid(&env.lunchroom_home).is_some(),
    )
}

/// What `lunchroom doctor` says of the mode Lunchroom runs in: `Ok` is a PASS line, `Err` a FAIL
/// line with what to do. `set_up`: the console's program and the user's folder exist;
/// `setgid_mode`: this process runs setgid.
///
/// # Errors
/// The FAIL line: the console is set up but the store is still user-owned.
pub fn mode_line(set_up: bool, setgid_mode: bool, store: &Path) -> Result<String, String> {
    match (set_up, setgid_mode) {
        (true, true) => Ok(format!(
            "the console is set up and the store at {} is out of reach of your agents (group {})",
            store.display(),
            setgid::SYSTEM_USER
        )),
        (false, false) => Ok(format!(
            "Lunchroom runs in user mode: the store at {} is owned by you, so a process of yours \
             can read it. `lunchroom install console` moves it out of reach",
            store.display()
        )),
        (true, false) => Err(format!(
            "the console is set up, but this Lunchroom still uses the store at {}, which you own \
             and your agents' processes can reach: run Lunchroom by {} (the link in your \
             ~/.local/bin points at it after `lunchroom install console`) and move the old store",
            store.display(),
            setgid::PROGRAM
        )),
        (false, true) => Err(format!(
            "this Lunchroom runs setgid but the console is not set up: {} is missing; run \
             `lunchroom install console` again",
            setgid::PROGRAM
        )),
    }
}

/// [`program_place_with`] on this system.
///
/// # Errors
/// The FAIL line.
pub fn program_place_check() -> Result<String, String> {
    use std::os::unix::fs::MetadataExt as _;
    program_place_with(&|p: &Path| std::fs::metadata(p).ok().map(|m| (m.uid(), m.mode())))
}

/// decision of 2026-11-14 ("Add the check": "`/usr/local/lib/lunchroom` is root's and writable by no one
/// else (and the program itself likewise)"): the folder and the setgid program are root's with no
/// group or other write bit, or whoever can write one of them can replace the program and the store
/// wall no longer holds. `meta` gives a path's owner and mode.
///
/// # Errors
/// The FAIL line: the first path at fault and what to do.
pub fn program_place_with(meta: &dyn Fn(&Path) -> Option<(u32, u32)>) -> Result<String, String> {
    for path in [Path::new(setgid::PROGRAM_DIR), Path::new(setgid::PROGRAM)] {
        match meta(path) {
            Some((0, mode)) if mode & 0o022 == 0 => {}
            Some((uid, mode)) => {
                return Err(format!(
                    "{} is owned by user id {uid} with mode {:o}; it must be root's and writable by \
                     no one else, or whoever can write it can replace Lunchroom's program: as root, \
                     run `chown root {}` and `chmod go-w {}`",
                    path.display(),
                    mode & 0o7777,
                    path.display(),
                    path.display()
                ));
            }
            None => return Err(format!("{} cannot be read", path.display())),
        }
    }
    Ok(format!(
        "{} and {} are root's and writable by no one else",
        setgid::PROGRAM_DIR,
        setgid::PROGRAM
    ))
}

/// [`mode_line`] for this process and `env`.
///
/// # Errors
/// The FAIL line.
pub fn mode_check(env: &Env, uid: Option<u32>) -> Result<String, String> {
    let set_up =
        Path::new(setgid::PROGRAM).is_file() && uid.is_some_and(|u| setgid::store_home(u).is_dir());
    mode_line(set_up, setgid::active(), &env.lunchroom_home)
}

/// The probing alert for `agent`, as a notice for the user.
#[must_use]
pub fn probing_notice(agent: &str, recent: usize) -> String {
    format!(
        "{agent} had {recent} DENIED calls in the last 10 minutes. It may be probing for a way \
         round Lunchroom's guard: look at what it is doing."
    )
}

// ---------------------------------------------------------------------------------------------
// What the screen reads besides the store.
// ---------------------------------------------------------------------------------------------

/// What the screen reads besides the store, apart so tests can give it by hand.
pub trait Feed: Sync {
    /// The guard records.
    fn records(&self, env: &Env) -> Vec<Record>;
    /// The lines of the setup checks.
    fn checks(&self, env: &Env, mail: &Mail, base: &dyn System) -> Vec<String>;
}

/// The files of the store folder and the checks of the setup.
struct RealFeed;

impl Feed for RealFeed {
    fn records(&self, env: &Env) -> Vec<Record> {
        records::read(&env.lunchroom_home)
    }

    fn checks(&self, env: &Env, _mail: &Mail, base: &dyn System) -> Vec<String> {
        let mut lines = Vec::new();
        let uid = base.uid();
        // The console itself runs as the user `lunchroom` (checked at its start), not setgid: for
        // it the store in force is the walled one whenever the console is set up.
        let set_up = Path::new(setgid::PROGRAM).is_file()
            && uid.is_some_and(|u| setgid::store_home(u).is_dir());
        match mode_line(set_up, true, &env.lunchroom_home) {
            Ok(l) => lines.push(format!("PASS {l}")),
            Err(l) => lines.push(format!("FAIL {l}")),
        }
        lines.extend(console_file_checks(env, base));
        if std::fs::read_dir(&env.home).is_ok() {
            let out = crate::cli::doctor_text(env, base);
            lines.extend(doctor_rows(&out.stdout));
        } else {
            lines.push(
                "The console cannot read your home folder, so the checks of your agents' setup \
                 are in `lunchroom doctor`, run in a terminal of your own."
                    .to_owned(),
            );
        }
        lines
    }
}

/// Each check `lunchroom doctor` printed, as one record: a record starts at a `PASS ` or `FAIL `
/// line and holds the lines that follow it up to the next, so the screen counts doctor's own PASS
/// and FAIL lines, no more and no fewer.
fn doctor_rows(stdout: &str) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for line in stdout.lines() {
        if line.starts_with("PASS ") || line.starts_with("FAIL ") {
            rows.push(line.to_owned());
        } else if !line.trim().is_empty()
            && let Some(last) = rows.last_mut()
        {
            last.push('\n');
            last.push_str(line);
        }
    }
    rows
}

/// The checks of the console's own files: the store folder's mode and group, the program's
/// set-group-id bit, and the developer's passphrase check.
fn console_file_checks(env: &Env, base: &dyn System) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let group = std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|g| setgid::gid_from_group(&g, setgid::SYSTEM_USER));
    let mut lines = Vec::new();
    let mut check = |ok: Result<String, String>| match ok {
        Ok(l) => lines.push(format!("PASS {l}")),
        Err(l) => lines.push(format!("FAIL {l}")),
    };
    check(match std::fs::metadata(&env.lunchroom_home) {
        Ok(m) if m.mode() & 0o7777 == 0o2770 && Some(m.gid()) == group => Ok(format!(
            "{} is mode 2770, group {}",
            env.lunchroom_home.display(),
            setgid::SYSTEM_USER
        )),
        Ok(m) => Err(format!(
            "{} is mode {:o}, group {}; it should be 2770, group {}: run `lunchroom install \
             console` again",
            env.lunchroom_home.display(),
            m.mode() & 0o7777,
            m.gid(),
            setgid::SYSTEM_USER
        )),
        Err(e) => Err(format!(
            "{} cannot be read: {e}",
            env.lunchroom_home.display()
        )),
    });
    check(match std::fs::metadata(setgid::PROGRAM) {
        Ok(m) if m.mode() & 0o2000 != 0 && Some(m.gid()) == group && m.uid() == 0 => Ok(format!(
            "{} is root's, group {}, with the set-group-id bit",
            setgid::PROGRAM,
            setgid::SYSTEM_USER
        )),
        Ok(_) => Err(format!(
            "{} is not root's with group {} and the set-group-id bit: run `lunchroom install \
             console` again",
            setgid::PROGRAM,
            setgid::SYSTEM_USER
        )),
        Err(e) => Err(format!("{} cannot be read: {e}", setgid::PROGRAM)),
    });
    // decision of 2026-11-14 ("Add the check"): no one but root can replace the program.
    check(program_place_check());
    check(
        crate::terminal::trusted_verifier(base)
            .map(|_| "your passphrase check is in place and trusted".to_owned()),
    );
    lines
}

// ---------------------------------------------------------------------------------------------
// The entry.
// ---------------------------------------------------------------------------------------------

/// Why the console ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The user typed `q`.
    Quit,
    /// The keyboard closed.
    Closed,
    /// The passphrase was wrong too many times.
    Failed,
}

/// The developer's login name for `uid`: the operator of a controller approval (§17 item 9) is
/// that name, never the number.
///
/// # Errors
/// That the name cannot be read.
pub(crate) fn developer_name(passwd: &str, uid: u32) -> Result<String, String> {
    setgid::name_from_passwd(passwd, uid).ok_or_else(|| {
        format!("the login name of user id {uid} is not in /etc/passwd, so an approval would carry no name")
    })
}

/// The checks the console makes before it draws anything: A1.3 (no guarded session in the parent
/// chain, a controlling terminal, not the terminal of a guarded session) and §17 item 3 (the
/// terminal is owned by user `lunchroom`, not by the developer).
///
/// # Errors
/// Why the console must not start.
pub(crate) fn start_check(
    mail: &Mail,
    sys: &dyn System,
    tty_uid: u32,
    lunchroom_uid: u32,
) -> Result<(), String> {
    crate::terminal::check(mail, sys)?;
    if tty_uid != lunchroom_uid {
        return Err(format!(
            "this terminal is owned by user id {tty_uid}, not by {}: a process of yours could \
             type into it. Run the console with sudo configured with `use_pty`",
            setgid::SYSTEM_USER
        ));
    }
    Ok(())
}

/// Item 2 of the design: run by the developer, `lunchroom console` starts the console as the user
/// `lunchroom` (or says it is not set up) and its exit code is the result (`Err`); run as that
/// user, this is the console, and the result is the user's id from `passwd`.
fn launch(passwd: &str) -> Result<Option<u32>, ExitCode> {
    let lunchroom_uid = setgid::uid_from_passwd(passwd, setgid::SYSTEM_USER);
    let ids = setgid::current();
    let is_console =
        matches!((ids, lunchroom_uid), (Some(i), Some(u)) if i.uid == u && i.euid == u);
    let rule_look = std::fs::symlink_metadata(crate::install::console_setup::LAUNCH_RULE)
        .err()
        .map(|e| e.kind());
    match launch_decision(is_console, launch_rule_there(rule_look)) {
        Launch::IsConsole => Ok(lunchroom_uid),
        Launch::Run => Err(start_as_lunchroom()),
        Launch::NotSetUp => Err(fail(&not_set_up_text(), 3)),
    }
}

/// The owner of this process's controlling terminal, read from the device that the standard
/// streams name (`terminal_owner`); `None` when none of them is that terminal.
fn own_terminal_owner() -> Option<u32> {
    let ctty = crate::process::tty(std::process::id()).unwrap_or(0);
    let names: Vec<std::path::PathBuf> = [
        rustix::stdio::stdin(),
        rustix::stdio::stdout(),
        rustix::stdio::stderr(),
    ]
    .into_iter()
    .filter_map(|fd| rustix::termios::ttyname(fd, Vec::new()).ok())
    .map(|name| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name.as_bytes())))
    .collect();
    terminal_owner(ctty, &names)
}

/// The owner of the terminal device whose number is `ctty` (the controlling terminal, as
/// `/proc/<pid>/stat` gives it): the first of `names` whose device number is `ctty`. An alias
/// such as `/dev/tty` has a device number of its own (5,0), so it is never the terminal.
#[must_use]
pub(crate) fn terminal_owner(ctty: u64, names: &[std::path::PathBuf]) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    if ctty == 0 {
        return None;
    }
    names
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .find(|m| m.rdev() == ctty)
        .map(|m| m.uid())
}

fn fail(text: &str, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{}", text.trim_end());
    ExitCode::from(code)
}

/// `lunchroom console`. It runs before `Env::from_process`: the console reads no variable of its
/// environment but `SUDO_UID`, which `sudo` sets.
#[must_use]
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return fail("usage: lunchroom console (no arguments)", 2);
    }
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let lunchroom_uid = match launch(&passwd) {
        Ok(uid) => uid,
        Err(code) => return code,
    };
    let Some(dev_uid) = std::env::var("SUDO_UID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|u| *u != 0 && Some(*u) != lunchroom_uid)
    else {
        return fail(&wrong_user_text(), 3);
    };
    crate::terminal::act_for(dev_uid);
    // What the console writes stays readable and writable by the developer's program (its group).
    setgid::share_with_group();
    let base = OsSystem;
    if let Err(why) = crate::terminal::trusted_verifier(&base) {
        return fail(
            &format!("The console cannot start: {why}\nRun `lunchroom passphrase` first."),
            3,
        );
    }
    let env = match Env::for_console(dev_uid) {
        Ok(env) => env,
        Err(why) => return fail(&format!("The console cannot start: {why}"), 1),
    };
    let developer = match developer_name(&passwd, dev_uid) {
        Ok(name) => name,
        Err(why) => return fail(&format!("The console cannot start: {why}"), 1),
    };
    let tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(tty) => tty,
        Err(e) => return fail(&format!("the console needs a terminal of its own: {e}"), 3),
    };
    // A1.3 and item 3, before anything is drawn or asked: no guarded session above the console,
    // and a terminal that is owned by user `lunchroom`.
    // The owner is read from the terminal device itself, found by name through the standard
    // streams: `/dev/tty` is only an alias, whose own entry is root's (2026-11-14: `fstat` of an
    // opened `/dev/tty` gave uid 0 and rdev 0x500 under every terminal, so no console started).
    let Some(tty_uid) = own_terminal_owner() else {
        return fail(
            "the terminal cannot be examined: none of the console's standard streams is its \
             controlling terminal; start it in a terminal of your own with `lunchroom console`",
            3,
        );
    };
    let mail = match Store::<Mail>::read_without_change(&env.store_dir()) {
        Ok(mail) => mail,
        Err(_) if !env.store_dir().exists() => Mail::default(),
        Err(e) => {
            return fail(
                &format!("The console cannot start: the store cannot be read ({e})"),
                1,
            );
        }
    };
    if let Err(why) = start_check(&mail, &base, tty_uid, lunchroom_uid.unwrap_or(u32::MAX)) {
        return fail(&format!("The console cannot start: {why}"), 3);
    }
    let alive = env.lunchroom_home.clone();
    let mut clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    };
    let feed = RealFeed;
    let mut run = Run {
        live: Live::new(env.clone(), developer.clone(), &feed),
        alive: Some(alive.clone()),
        clock: &mut clock,
    };
    // The backend and the loop both use the system; the loop only reads its answers for the
    // snapshot, so the backend borrows a system of its own.
    let mut acting = OsSystem;
    let mut backend = RealBackend {
        env,
        base: &mut acting,
        uid: dev_uid,
        developer,
        lunchroom_uid,
        controller: None,
    };
    let ending = serve(&tty, &base, &mut backend, &mut run);
    live::remove_alive(&alive);
    let _ = writeln!(std::io::stderr(), "The Lunchroom console ended.");
    match ending {
        Ok(Ending::Quit | Ending::Closed) => ExitCode::SUCCESS,
        Ok(Ending::Failed) => {
            let _ = writeln!(
                std::io::stderr(),
                "Too many wrong passphrases; the console ended."
            );
            ExitCode::from(3)
        }
        Err(why) => fail(&why, 3),
    }
}
