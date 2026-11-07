//! The Lunchroom console (1.1): the user's own screen, run as the system user `lunchroom`.
//!
//! Every agent runs as the developer's own Linux user, and the kernel treats every process of one
//! user alike. The console runs as another user, `lunchroom`, so no process of the developer can
//! signal it, trace it, read its memory or environment, or open its terminal; and the store it
//! reads and writes is a folder only that user's group can enter (`lunchroom_core::setgid`).
//!
//! The person starts it in a terminal of their own:
//! `sudo -u lunchroom /usr/local/lib/lunchroom/lunchroom console`. It asks for the passphrase once
//! (echo off), checks it against the developer's verifier (`/etc/lunchroom/<uid>.verifier`, where
//! the uid is `SUDO_UID`), and then redraws one screen every second from a read of the store that
//! takes no lock:
//! 1. a header line;
//! 2. the agents (name, harness, state, QUEUED count, HELD by, YOLO);
//! 3. the messages that are QUEUED or FAILED, with why;
//! 4. notices for the user, which the console marks told when it shows them;
//! 5. the calls waiting for approval, numbered, with their age;
//! 6. the DENIED calls of the last 10 minutes and 24 hours for each agent, and probing alerts;
//! 7. the guard's timeouts and slow decisions;
//! 8. the checks of the setup.
//!
//! The keys are `a <n>`, `u <name>`, `m strict|balanced|relaxed`, `y <names> till HH:MM` and `q`.
//! Each action is confirmed with y/N; the ones that loosen Lunchroom ask for the passphrase again.
//! The actions are the commands of `user.rs` (approve, unregister, mode, yolo), run on a system
//! whose confirmation is the console's own, so the console decides nothing the commands do not.
//! After 15 minutes without a key the screen locks and asks for the passphrase.
//!
//! While it runs the console rewrites `console.alive` in the store folder every 2 seconds with its
//! process id and start time; a guard prompt hook that finds none says so once a day.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use lunchroom_core::mail::{Mail, Notice, clock, span, standing};
use lunchroom_core::passphrase::{self, Verifier};
use lunchroom_core::policy::HoldMode;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;
use lunchroom_core::vocab::{AgentState, MessageState, Word};

use crate::cli::Outcome;
use crate::home::Env;
use crate::records::{self, Record};
use crate::terminal::{OsSystem, System};

/// The file in the store folder that says a console runs.
pub const ALIVE_FILE: &str = "console.alive";

/// The file that keeps the day the "console is not running" notice was last told.
const MISSING_DAY_FILE: &str = "console-missing.day";

/// Seconds without a key after which the screen locks.
pub const IDLE_LOCK_SECS: u64 = 15 * 60;

/// The screen is read again this often.
const REDRAW: Duration = Duration::from_secs(1);

/// The liveness file is written this often, in seconds.
const ALIVE_EVERY: u64 = 2;

/// The setup checks are made again this often, in seconds.
const CHECKS_EVERY: u64 = 60;

/// The passphrase may be wrong this many times in a row.
const TRIES: u32 = 3;

/// The notices the console keeps on its screen.
const NOTICES_KEPT: usize = 20;

/// The line printed to a person who runs `lunchroom console` as anyone but the user `lunchroom`.
#[must_use]
pub fn start_command() -> String {
    format!(
        "sudo -u {} {} console",
        setgid::SYSTEM_USER,
        setgid::PROGRAM
    )
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

/// [`mode_line`] for this process and `env`.
///
/// # Errors
/// The FAIL line.
pub fn mode_check(env: &Env, uid: Option<u32>) -> Result<String, String> {
    let set_up =
        Path::new(setgid::PROGRAM).is_file() && uid.is_some_and(|u| setgid::store_home(u).is_dir());
    mode_line(set_up, setgid::active(), &env.lunchroom_home)
}

// ---------------------------------------------------------------------------------------------
// The screen: a snapshot, and its text.
// ---------------------------------------------------------------------------------------------

/// One agent's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub name: String,
    pub harness: String,
    pub state: String,
    pub queued: usize,
    pub held_by: Vec<String>,
    pub yolo: Option<String>,
}

/// One call waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingRow {
    pub number: usize,
    pub agent: String,
    pub call: String,
    pub age_secs: u64,
}

/// One agent's DENIED counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedRow {
    pub agent: String,
    pub recent: usize,
    pub day: usize,
    pub probing: bool,
}

/// Everything one screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub now: u64,
    pub developer: String,
    pub hold_mode: String,
    pub agents: Vec<AgentRow>,
    pub messages: Vec<String>,
    pub notices: Vec<String>,
    pub waiting: Vec<WaitingRow>,
    pub denied: Vec<DeniedRow>,
    pub guard: Vec<String>,
    pub checks: Vec<String>,
    pub message: String,
    pub locked: bool,
}

/// `text` as safe to print on the user's terminal: control characters and direction marks are
/// removed (the text is what an agent chose).
fn safe(text: &str) -> String {
    records::clean(text)
}

fn age(secs: u64) -> String {
    if secs < 60 {
        "under 1 min".to_owned()
    } else {
        span(secs)
    }
}

fn section(out: &mut String, title: &str, lines: &[String]) {
    let _ = writeln!(out, "\n{title}");
    if lines.is_empty() {
        out.push_str("  none\n");
    }
    for line in lines {
        let _ = writeln!(out, "  {}", safe(line));
    }
}

/// The keys, as the last line of the screen.
pub const KEYS_LINE: &str = "Keys: a <n> approve call n | u <name> unregister | m \
                             strict|balanced|relaxed | y <names> till HH:MM | q quit";

/// The screen for `s`, as plain text.
#[must_use]
pub fn render(s: &Snapshot) -> String {
    let mut out = String::new();
    if s.locked {
        let _ = writeln!(
            out,
            "Lunchroom console - screen hidden after {} min without a key",
            IDLE_LOCK_SECS / 60
        );
        out.push_str("Type your Lunchroom passphrase to show it again.\n");
        return out;
    }
    let _ = writeln!(
        out,
        "Lunchroom console - {} - hold mode {} - acting for {}",
        clock(s.now),
        safe(&s.hold_mode),
        safe(&s.developer)
    );
    let agents: Vec<String> = s
        .agents
        .iter()
        .map(|a| {
            let mut line = format!(
                "{:<14} {:<12} {:<10} {} {}",
                safe(&a.name),
                safe(&a.harness),
                safe(&a.state),
                MessageState::Queued.word(),
                a.queued
            );
            if !a.held_by.is_empty() {
                let _ = write!(
                    line,
                    "  {} by {}",
                    Word::Held.text(),
                    a.held_by
                        .iter()
                        .map(|h| safe(h))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(y) = &a.yolo {
                let _ = write!(line, "  {}", safe(y));
            }
            line
        })
        .collect();
    section(&mut out, "AGENTS", &agents);
    section(&mut out, "MESSAGES QUEUED OR FAILED", &s.messages);
    section(
        &mut out,
        "NOTICES FOR YOU (marked told when shown)",
        &s.notices,
    );
    let waiting: Vec<String> = s
        .waiting
        .iter()
        .map(|w| {
            format!(
                "{}. {} {} (asked {} ago)",
                w.number,
                safe(&w.agent),
                safe(&w.call),
                age(w.age_secs)
            )
        })
        .collect();
    section(&mut out, "CALLS WAITING FOR YOUR APPROVAL", &waiting);
    let denied: Vec<String> = s
        .denied
        .iter()
        .map(|d| {
            let mut line = format!(
                "{} - {} in the last 10 min, {} in the last 24 h",
                safe(&d.agent),
                d.recent,
                d.day
            );
            if d.probing {
                let _ = write!(
                    line,
                    " - PROBING: {} or more DENIED calls in 10 minutes",
                    records::PROBE_COUNT
                );
            }
            line
        })
        .collect();
    section(&mut out, "DENIED CALLS", &denied);
    section(&mut out, "GUARD TIMEOUTS AND SLOW DECISIONS", &s.guard);
    section(&mut out, "CHECKS", &s.checks);
    if !s.message.is_empty() {
        let _ = writeln!(out, "\n{}", safe(&s.message));
    }
    let _ = writeln!(out, "\n{KEYS_LINE}");
    out
}

// ---------------------------------------------------------------------------------------------
// The snapshot of the store.
// ---------------------------------------------------------------------------------------------

/// The agent a session id belongs to, by the name the user gave it.
fn agent_of(mail: &Mail, session: &str) -> String {
    mail.names()
        .find(|(_, who)| who.session == session)
        .map_or_else(
            || {
                let short: String = session.chars().take(8).collect();
                format!("a session with no agent name ({short})")
            },
            |(name, _)| name.to_owned(),
        )
}

/// The agents' rows. `home_readable`: the console can read the developer's home folder, which the
/// evidence of an agent's guard is in; when it cannot, a state that needs it is not guessed.
fn agent_rows(mail: &Mail, env: &Env, home_readable: bool) -> Vec<AgentRow> {
    let mut rows: Vec<(bool, AgentRow)> = mail
        .names()
        .map(|(name, who)| {
            let state = if home_readable {
                let judged = crate::evidence::assess(mail, who, &env.home);
                mail.agent_state(who, judged.evidence())
            } else if mail.is_held(who) {
                AgentState::Held
            } else {
                AgentState::Available
            };
            let state_text = if home_readable || state == AgentState::Held {
                state.word().to_owned()
            } else {
                "state unknown (the console cannot read your home folder)".to_owned()
            };
            let mut held_by = Vec::new();
            for h in mail.holding(who) {
                if mail.message(&h.message).is_some() {
                    held_by.push(format!("{} ({})", h.from_name, h.message));
                } else {
                    held_by.push(h.from_name.replace('-', " "));
                }
            }
            let mut yolo = String::new();
            if let Some(end) = mail.yolo_until(who, env.now) {
                let _ = write!(yolo, "{} until {}", Word::Yolo.text(), clock(end));
            }
            if mail.harness_yolo(who) {
                if !yolo.is_empty() {
                    yolo.push_str(", ");
                }
                let _ = write!(yolo, "{} (the harness's own)", Word::Yolo.text());
            }
            (
                state == AgentState::Stopped,
                AgentRow {
                    name: name.to_owned(),
                    harness: crate::evidence::harness_name(who.harness).to_owned(),
                    state: state_text,
                    queued: mail.deliveries(who).len(),
                    held_by,
                    yolo: (!yolo.is_empty()).then_some(yolo),
                },
            )
        })
        .collect();
    rows.sort_by_key(|(stopped, _)| *stopped);
    rows.into_iter().map(|(_, row)| row).collect()
}

/// What the screen is built from.
#[derive(Debug)]
pub struct ScreenInput<'a> {
    pub env: &'a Env,
    pub mail: &'a Mail,
    pub records: &'a [Record],
    pub notices: &'a [String],
    pub guard: &'a [String],
    pub checks: &'a [String],
    pub message: &'a str,
    pub developer: &'a str,
    pub home_readable: bool,
}

/// The snapshot of `input`.
#[must_use]
pub fn snapshot(input: &ScreenInput<'_>) -> Snapshot {
    let ScreenInput {
        env,
        mail,
        records: recs,
        ..
    } = *input;
    let now = env.now;
    let mut messages = Vec::new();
    for m in mail.queued() {
        messages.push(format!(
            "{}: {}",
            MessageState::Queued.word(),
            standing(m, now)
        ));
    }
    for (m, reason) in mail.failed() {
        messages.push(format!(
            "{}: {} to {} ({}) - {}",
            MessageState::Failed.word(),
            m.id,
            m.to_name,
            m.to,
            reason
        ));
    }
    let waiting = mail
        .requests()
        .iter()
        .enumerate()
        .map(|(k, r)| WaitingRow {
            number: k + 1,
            agent: r.agent.clone(),
            call: format!(
                "{} {}",
                lunchroom_core::approvals::visible(&r.tool),
                lunchroom_core::approvals::visible(&lunchroom_core::jcs::shown(&r.input))
            ),
            age_secs: now.saturating_sub(r.at),
        })
        .collect();
    let denied = records::denied_counts(recs, now)
        .into_iter()
        .map(|c| DeniedRow {
            agent: agent_of(mail, &c.session),
            recent: c.recent,
            day: c.day,
            probing: c.probing(),
        })
        .collect();
    Snapshot {
        now,
        developer: input.developer.to_owned(),
        hold_mode: mail.hold_mode().word().to_owned(),
        agents: agent_rows(mail, env, input.home_readable),
        messages,
        notices: input.notices.to_vec(),
        waiting,
        denied,
        guard: input.guard.to_vec(),
        checks: input.checks.to_vec(),
        message: input.message.to_owned(),
        locked: false,
    }
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
// The keys.
// ---------------------------------------------------------------------------------------------

/// What a line the user typed asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Nothing,
    Quit,
    Approve(String),
    Unregister(String),
    Mode(HoldMode),
    Yolo { names: String, till: String },
}

impl Key {
    /// The action loosens Lunchroom (`m balanced|relaxed`, `y`): it asks for the passphrase again.
    #[must_use]
    pub const fn loosens(&self) -> bool {
        matches!(
            self,
            Self::Mode(HoldMode::Balanced | HoldMode::Relaxed) | Self::Yolo { .. }
        )
    }
}

/// The key a typed line is.
///
/// # Errors
/// What to type instead.
pub fn parse_key(line: &str) -> Result<Key, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(Key::Nothing),
        ["q" | "quit"] => Ok(Key::Quit),
        ["a", n] if n.bytes().all(|b| b.is_ascii_digit()) => Ok(Key::Approve((*n).to_owned())),
        ["a", ..] => Err("a needs the number of a call waiting: a 1".to_owned()),
        ["u", name] => Ok(Key::Unregister((*name).to_owned())),
        ["u", ..] => Err("u needs one agent name: u kimi".to_owned()),
        ["m", mode] => HoldMode::parse(mode)
            .map(Key::Mode)
            .ok_or_else(|| "m needs strict, balanced or relaxed: m strict".to_owned()),
        ["m", ..] => Err("m needs strict, balanced or relaxed: m strict".to_owned()),
        ["y", names, "till", till] => Ok(Key::Yolo {
            names: (*names).to_owned(),
            till: (*till).to_owned(),
        }),
        ["y", ..] => Err("y needs names and an end time: y kimi,codex till 17:30 (UTC)".to_owned()),
        [other, ..] => Err(format!("{other} is not a key. {KEYS_LINE}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Input and output.
// ---------------------------------------------------------------------------------------------

/// What reading the keyboard gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Line(String),
    Quiet,
    Closed,
}

/// The console's screen and keyboard.
pub trait Io {
    /// Clears the screen and draws `text`.
    fn draw(&mut self, text: &str);
    /// Prints `text` below the screen.
    fn say(&mut self, text: &str);
    /// The next line typed, waiting at most `wait`.
    fn line(&mut self, wait: Duration) -> Typed;
    /// The next line typed, the typed characters not shown, waiting as long as it takes.
    fn secret(&mut self, prompt: &str) -> Option<String>;
}

/// The real keyboard and screen: `/dev/tty`, read on a thread of its own so a redraw never waits
/// for a key.
struct TtyIo {
    tty: std::fs::File,
    lines: Receiver<Option<String>>,
}

impl TtyIo {
    fn open() -> Result<Self, String> {
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|e| format!("the console needs a terminal of its own: {e}"))?;
        let reader = tty
            .try_clone()
            .map_err(|e| format!("the terminal cannot be used: {e}"))?;
        let (tx, lines): (Sender<Option<String>>, _) = channel();
        std::thread::Builder::new()
            .name("keys".to_owned())
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(n) if n > 0 => {
                            if tx
                                .send(Some(line.trim_end_matches(['\n', '\r']).to_owned()))
                                .is_err()
                            {
                                return;
                            }
                        }
                        _ => {
                            let _ = tx.send(None);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("the keyboard cannot be read: {e}"))?;
        Ok(Self { tty, lines })
    }
}

impl Io for TtyIo {
    fn draw(&mut self, text: &str) {
        let _ = self.tty.write_all(b"\x1b[2J\x1b[H");
        let _ = self.tty.write_all(text.as_bytes());
        let _ = self.tty.write_all(b"> ");
        let _ = self.tty.flush();
    }

    fn say(&mut self, text: &str) {
        let _ = self.tty.write_all(text.as_bytes());
        let _ = self.tty.flush();
    }

    fn line(&mut self, wait: Duration) -> Typed {
        match self.lines.recv_timeout(wait) {
            Ok(Some(line)) => Typed::Line(line),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => Typed::Closed,
            Err(RecvTimeoutError::Timeout) => Typed::Quiet,
        }
    }

    fn secret(&mut self, prompt: &str) -> Option<String> {
        self.say(prompt);
        // Nothing is read while the typed characters would show.
        if !crate::terminal::stty(&self.tty, "-echo") {
            self.say(
                "\nthe typed characters cannot be hidden (it needs `stty`); nothing was read\n",
            );
            return None;
        }
        let typed = loop {
            match self.lines.recv_timeout(Duration::from_secs(3600)) {
                Ok(Some(line)) => break Some(line),
                Ok(None) | Err(RecvTimeoutError::Disconnected) => break None,
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        let _ = crate::terminal::stty(&self.tty, "echo");
        self.say("\n");
        typed
    }
}

// ---------------------------------------------------------------------------------------------
// The system the user's commands run on.
// ---------------------------------------------------------------------------------------------

/// The system the commands of `user.rs` run on inside the console. Its confirmation is the
/// console's own: the question is shown and answered y/N, and a loosening action asks the
/// passphrase again and checks it against the developer's verifier. The commands themselves still
/// check that no guarded session is above the console and that it has a terminal.
///
/// The commands check a passphrase against [`System::verifier`]; this system hands them a verifier
/// made from a random token that lives only here, and returns that token once the console has
/// confirmed. The developer's passphrase is checked by the console, never kept.
struct Acting<'a> {
    base: &'a mut dyn System,
    io: &'a mut dyn Io,
    session: Verifier,
    token: String,
    loosens: bool,
    /// The user answered no.
    declined: bool,
    /// The passphrase asked again was wrong.
    wrong: bool,
}

impl System for Acting<'_> {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        self.base.ancestors()
    }
    fn own_tty(&self) -> u64 {
        self.base.own_tty()
    }
    fn tty_of(&self, key: &str) -> Option<u64> {
        self.base.tty_of(key)
    }
    fn ask(&mut self, _question: &str) -> Option<String> {
        None
    }
    fn uid(&self) -> Option<u32> {
        self.base.uid()
    }
    fn verifier(&self) -> Result<String, String> {
        Ok(self.session.line())
    }
    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        let question = prompt.strip_suffix("Passphrase: ").unwrap_or(prompt);
        let shown: Vec<String> = question.lines().map(safe).collect();
        self.io.say(&format!("\n{}\n", shown.join("\n").trim_end()));
        self.io.say("Do it? [y/N] ");
        let answer = match self.io.line(Duration::from_secs(120)) {
            Typed::Line(l) => l,
            Typed::Quiet | Typed::Closed => return None,
        };
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            self.declined = true;
            return Some(String::new());
        }
        if self.loosens {
            let real = crate::terminal::trusted_verifier(self.base).ok()?;
            let typed = self
                .io
                .secret("This loosens Lunchroom. Passphrase again: ")?;
            if !real.matches(&typed) {
                self.wrong = true;
                return Some(String::new());
            }
        }
        Some(self.token.clone())
    }
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        self.base.salt()
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Err("the console does not change the passphrase".to_owned())
    }
}

/// Runs the key `key` with the commands of `user.rs`; what they said, plainly.
fn act(key: &Key, env: &Env, base: &mut dyn System, io: &mut dyn Io) -> String {
    let (Ok(salt), token) = (base.salt(), random_token(base)) else {
        return "The console could not get random bytes, so it did nothing.".to_owned();
    };
    let mut acting = Acting {
        session: Verifier::new(&token, salt, crate::terminal::ITERATIONS),
        token,
        loosens: key.loosens(),
        declined: false,
        wrong: false,
        base,
        io,
    };
    let outcome: Outcome = match key {
        Key::Approve(n) => crate::user::approve(std::slice::from_ref(n), env, &mut acting),
        Key::Unregister(name) => {
            crate::user::unregister(std::slice::from_ref(name), env, &mut acting)
        }
        Key::Mode(mode) => crate::user::mode(&[mode.word().to_ascii_lowercase()], env, &mut acting),
        Key::Yolo { names, till } => crate::user::yolo(
            &[names.clone(), "till".to_owned(), till.clone()],
            env,
            &mut acting,
        ),
        Key::Nothing | Key::Quit => return String::new(),
    };
    if acting.declined {
        return "Not confirmed. Nothing was changed.".to_owned();
    }
    if acting.wrong {
        return "The passphrase was wrong. Nothing was changed.".to_owned();
    }
    let text = format!("{}{}", outcome.stdout, outcome.stderr);
    text.trim().to_owned()
}

/// A token of random bytes as text, for the verifier of one action.
fn random_token(base: &mut dyn System) -> String {
    match base.salt() {
        Ok(bytes) => bytes.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        }),
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// What the screen reads besides the store.
// ---------------------------------------------------------------------------------------------

/// What the screen reads besides the store, apart so tests can give it by hand.
pub trait Feed {
    /// The guard records.
    fn records(&self, env: &Env) -> Vec<Record>;
    /// The lines of the guard's timeouts and slow decisions.
    fn guard_lines(&self, env: &Env, mail: &Mail) -> Vec<String>;
    /// The lines of the setup checks.
    fn checks(&self, env: &Env, mail: &Mail, base: &dyn System) -> Vec<String>;
}

/// The files of the store folder and the checks of the setup.
struct RealFeed;

/// The lines of `text` that carry a check or a record.
fn lines_of(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

impl Feed for RealFeed {
    fn records(&self, env: &Env) -> Vec<Record> {
        records::read(&env.lunchroom_home)
    }

    fn guard_lines(&self, env: &Env, mail: &Mail) -> Vec<String> {
        lines_of(&crate::cli::guard_record_text(env, Some(mail)))
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
            lines.extend(
                lines_of(&out.stdout)
                    .into_iter()
                    .filter(|l| !l.starts_with("PASS")),
            );
            if out.code == 0 {
                lines.push("PASS the checks of `lunchroom doctor` all pass".to_owned());
            }
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
    check(
        crate::terminal::trusted_verifier(base)
            .map(|_| "your passphrase check is in place and trusted".to_owned()),
    );
    lines
}

// ---------------------------------------------------------------------------------------------
// The loop.
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

/// Asks for the passphrase up to [`TRIES`] times.
fn authenticate(io: &mut dyn Io, base: &mut dyn System, prompt: &str) -> Result<(), Ending> {
    for _ in 0..TRIES {
        let Ok(verifier) = crate::terminal::trusted_verifier(base) else {
            io.say("The passphrase check cannot be used; run `lunchroom doctor`.\n");
            return Err(Ending::Failed);
        };
        let Some(typed) = io.secret(prompt) else {
            return Err(Ending::Closed);
        };
        if verifier.matches(&typed) {
            return Ok(());
        }
        io.say("The passphrase was wrong.\n");
    }
    io.say("Too many wrong passphrases; the console ended.\n");
    Err(Ending::Failed)
}

/// What [`serve`] needs besides the screen, the keyboard and the system.
pub struct Run<'a> {
    pub env: Env,
    pub developer: String,
    /// Where the liveness file is written; none in a test that does not want one.
    pub alive: Option<PathBuf>,
    pub feed: &'a dyn Feed,
    /// The clock, in seconds since the Unix epoch.
    pub clock: &'a mut dyn FnMut() -> u64,
}

/// The loop: logs in, then draws, reads keys and acts until the user quits.
#[allow(clippy::too_many_lines)] // one loop: read, count, draw, mark told, read a key, act
pub fn serve(io: &mut dyn Io, base: &mut dyn System, run: &mut Run<'_>) -> Ending {
    if let Err(ending) = authenticate(io, base, "Lunchroom passphrase: ") {
        return ending;
    }
    let pid = std::process::id();
    let mut last_key = (run.clock)();
    let mut last_alive = 0;
    let mut checks: (u64, Vec<String>) = (0, Vec::new());
    let mut seen: Vec<Notice> = Vec::new();
    let mut alerted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut message = String::new();
    let mut drawn = String::new();
    let home_readable = std::fs::read_dir(&run.env.home).is_ok();
    loop {
        let now = (run.clock)();
        let mut env = run.env.clone();
        env.now = now;
        if let Some(home) = &run.alive
            && (last_alive == 0 || now >= last_alive + ALIVE_EVERY)
        {
            let _ = write_alive(home, pid);
            last_alive = now;
        }
        if now.saturating_sub(last_key) >= IDLE_LOCK_SECS {
            io.draw(&render(&Snapshot {
                locked: true,
                ..Snapshot::default()
            }));
            if let Err(ending) = authenticate(io, base, "Passphrase: ") {
                return ending;
            }
            last_key = (run.clock)();
            drawn.clear();
            continue;
        }
        let mail = Store::<Mail>::read_without_change(&env.store_dir()).unwrap_or_default();
        let recs = run.feed.records(&env);
        if checks.1.is_empty() || now >= checks.0 + CHECKS_EVERY {
            checks = (now, run.feed.checks(&env, &mail, &*base));
        }
        // Probing: a notice for the user, once while it lasts.
        let counts = records::denied_counts(&recs, now);
        let mut fresh: Vec<String> = Vec::new();
        for c in &counts {
            if c.probing() {
                if alerted.insert(c.session.clone()) {
                    fresh.push(probing_notice(&agent_of(&mail, &c.session), c.recent));
                }
            } else {
                alerted.remove(&c.session);
            }
        }
        // A probing alert is a notice for the user like any other: it is written to the store and
        // read back below, so it is drawn, marked told and seen by `lunchroom status` once.
        if !fresh.is_empty() {
            if let Ok(store) = Store::<Mail>::open(&env.store_dir()) {
                let _ = store.update(|m| {
                    for text in &fresh {
                        m.tell_user(text, now);
                    }
                });
            }
            continue;
        }
        // Notices: each is kept on the screen and marked told once it has been drawn.
        let new: Vec<Notice> = mail
            .user_notices()
            .iter()
            .filter(|n| !seen.contains(n))
            .cloned()
            .collect();
        seen.extend(new.iter().cloned());
        if seen.len() > NOTICES_KEPT {
            seen.drain(..seen.len() - NOTICES_KEPT);
        }
        let notice_lines: Vec<String> = seen
            .iter()
            .map(|n| format!("{} {}", clock(n.at), n.text))
            .collect();
        let guard = run.feed.guard_lines(&env, &mail);
        let screen = render(&snapshot(&ScreenInput {
            env: &env,
            mail: &mail,
            records: &recs,
            notices: &notice_lines,
            guard: &guard,
            checks: &checks.1,
            message: &message,
            developer: &run.developer,
            home_readable,
        }));
        if screen != drawn {
            io.draw(&screen);
            drawn = screen;
            if !new.is_empty()
                && let Ok(store) = Store::<Mail>::open(&env.store_dir())
            {
                // Told: the next prompt or `lunchroom status` does not tell them again.
                let _ = store.update(|m| m.mark_user_notices_told(&new));
            }
        }
        match io.line(REDRAW) {
            Typed::Quiet => {}
            Typed::Closed => return Ending::Closed,
            Typed::Line(line) => {
                last_key = (run.clock)();
                match parse_key(&line) {
                    Ok(Key::Quit) => return Ending::Quit,
                    Ok(Key::Nothing) => {}
                    Ok(key) => {
                        message = act(&key, &env, base, io);
                        drawn.clear();
                    }
                    Err(why) => {
                        message = why;
                        drawn.clear();
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The entry.
// ---------------------------------------------------------------------------------------------

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
    let lunchroom_uid = setgid::uid_from_passwd(&passwd, setgid::SYSTEM_USER);
    let ids = setgid::current();
    let is_console =
        matches!((ids, lunchroom_uid), (Some(i), Some(u)) if i.uid == u && i.euid == u);
    if !is_console {
        return fail(&wrong_user_text(), 3);
    }
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
    let mut base = OsSystem;
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
    let developer =
        setgid::name_from_passwd(&passwd, dev_uid).unwrap_or_else(|| dev_uid.to_string());
    let mut io = match TtyIo::open() {
        Ok(io) => io,
        Err(why) => return fail(&why, 3),
    };
    let alive = env.lunchroom_home.clone();
    let mut clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    };
    let mut run = Run {
        env,
        developer,
        alive: Some(alive.clone()),
        feed: &RealFeed,
        clock: &mut clock,
    };
    let ending = serve(&mut io, &mut base, &mut run);
    let _ = std::fs::remove_file(alive.join(ALIVE_FILE));
    io.say("\nThe Lunchroom console ended.\n");
    match ending {
        Ending::Quit | Ending::Closed => ExitCode::SUCCESS,
        Ending::Failed => ExitCode::from(3),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use lunchroom_core::identity::{Harness, Identity};
    use lunchroom_core::mail::Origin;

    use super::*;
    use crate::terminal::fake;

    // ---- The screen: full text for each section ------------------------------------------------

    const HEADER: &str = "Lunchroom console - 00:00 UTC - hold mode Balanced - acting for dev\n";
    const KEYS: &str = "\nKeys: a <n> approve call n | u <name> unregister | m \
                        strict|balanced|relaxed | y <names> till HH:MM | q quit\n";

    fn base() -> Snapshot {
        Snapshot {
            now: 0,
            developer: "dev".to_owned(),
            hold_mode: "Balanced".to_owned(),
            ..Snapshot::default()
        }
    }

    /// The whole screen, with `body` for the sections in order (each `None` is "none").
    fn screen(bodies: [&str; 7], message: &str) -> String {
        const TITLES: [&str; 7] = [
            "AGENTS",
            "MESSAGES QUEUED OR FAILED",
            "NOTICES FOR YOU (marked told when shown)",
            "CALLS WAITING FOR YOUR APPROVAL",
            "DENIED CALLS",
            "GUARD TIMEOUTS AND SLOW DECISIONS",
            "CHECKS",
        ];
        let mut out = HEADER.to_owned();
        for (title, body) in TITLES.iter().zip(bodies) {
            let _ = write!(
                out,
                "\n{title}\n{}",
                if body.is_empty() { "  none\n" } else { body }
            );
        }
        if !message.is_empty() {
            let _ = writeln!(out, "\n{message}");
        }
        out.push_str(KEYS);
        out
    }

    const NONE: [&str; 7] = [""; 7];

    fn with(slot: usize, body: &'static str) -> [&'static str; 7] {
        let mut bodies = NONE;
        bodies[slot] = body;
        bodies
    }

    // Section 1 and the keys line, and every other section empty.
    #[test]
    fn the_empty_screen() {
        let text = render(&base());
        assert_eq!(text, screen(NONE, ""));
        assert!(text.starts_with(HEADER));
        assert!(text.ends_with(KEYS));
    }

    // Section 2: each agent's name, harness, state, QUEUED count, HELD by and YOLO.
    #[test]
    fn the_agents_section() {
        let mut s = base();
        s.agents = vec![
            AgentRow {
                name: "kimi".to_owned(),
                harness: "Claude Code".to_owned(),
                state: "HELD".to_owned(),
                queued: 2,
                held_by: vec!["codex (lr-00000007)".to_owned(), "a tool output".to_owned()],
                yolo: Some("YOLO until 17:30 UTC".to_owned()),
            },
            AgentRow {
                name: "codex".to_owned(),
                harness: "Codex".to_owned(),
                state: "AVAILABLE".to_owned(),
                queued: 0,
                held_by: vec![],
                yolo: None,
            },
        ];
        assert_eq!(
            render(&s),
            screen(
                with(
                    0,
                    "  kimi           Claude Code  HELD       QUEUED 2  HELD by codex \
                     (lr-00000007), a tool output  YOLO until 17:30 UTC\n  \
                     codex          Codex        AVAILABLE  QUEUED 0\n"
                ),
                ""
            )
        );
    }

    // Section 3: QUEUED and FAILED messages with their reasons.
    #[test]
    fn the_messages_section() {
        let mut s = base();
        s.messages = vec![
            "QUEUED: lr-00000003 to codex (codex:abc), 4 min old".to_owned(),
            "FAILED: lr-00000002 to kimi (claude-code:xyz) - the name was taken away".to_owned(),
        ];
        assert_eq!(
            render(&s),
            screen(
                with(
                    1,
                    "  QUEUED: lr-00000003 to codex (codex:abc), 4 min old\n  FAILED: \
                     lr-00000002 to kimi (claude-code:xyz) - the name was taken away\n"
                ),
                ""
            )
        );
    }

    // Section 4: notices, with control characters an agent put in them removed.
    #[test]
    fn the_notices_section_removes_what_a_terminal_would_act_on() {
        let mut s = base();
        s.notices = vec!["09:01 UTC kimi \u{1b}[2Jwas told\u{7} to wait\u{202e}".to_owned()];
        assert_eq!(
            render(&s),
            screen(with(2, "  09:01 UTC kimi [2Jwas told to wait\n"), "")
        );
        assert!(!render(&s).contains('\u{1b}'));
    }

    // Section 5: waiting calls, numbered, with their age in whole minutes.
    #[test]
    fn the_waiting_calls_section() {
        let mut s = base();
        s.waiting = vec![
            WaitingRow {
                number: 1,
                agent: "kimi".to_owned(),
                call: "Bash {\"command\":\"git push\"}".to_owned(),
                age_secs: 30,
            },
            WaitingRow {
                number: 2,
                agent: "codex".to_owned(),
                call: "Write {\"file_path\":\"/p/x\"}".to_owned(),
                age_secs: 185,
            },
        ];
        assert_eq!(
            render(&s),
            screen(
                with(
                    3,
                    "  1. kimi Bash {\"command\":\"git push\"} (asked under 1 min ago)\n  \
                     2. codex Write {\"file_path\":\"/p/x\"} (asked 3 min ago)\n"
                ),
                ""
            )
        );
    }

    // Section 6: DENIED counts, and the probing alert on the agent that has it.
    #[test]
    fn the_denied_section_marks_probing() {
        let mut s = base();
        s.denied = vec![
            DeniedRow {
                agent: "kimi".to_owned(),
                recent: 6,
                day: 9,
                probing: true,
            },
            DeniedRow {
                agent: "codex".to_owned(),
                recent: 1,
                day: 1,
                probing: false,
            },
        ];
        assert_eq!(
            render(&s),
            screen(
                with(
                    4,
                    "  kimi - 6 in the last 10 min, 9 in the last 24 h - PROBING: 5 or more DENIED \
                     calls in 10 minutes\n  codex - 1 in the last 10 min, 1 in the last 24 h\n"
                ),
                ""
            )
        );
    }

    // Sections 7 and 8, and a message under them.
    #[test]
    fn the_guard_and_checks_sections_and_the_last_message() {
        let mut s = base();
        s.guard = vec!["PASS a slow decision at 08:00 UTC took 2300 ms".to_owned()];
        s.checks = vec!["PASS the passphrase check is in place".to_owned()];
        s.message = "kimi is no longer named".to_owned();
        let mut bodies = NONE;
        bodies[5] = "  PASS a slow decision at 08:00 UTC took 2300 ms\n";
        bodies[6] = "  PASS the passphrase check is in place\n";
        assert_eq!(render(&s), screen(bodies, "kimi is no longer named"));
    }

    // The locked screen shows nothing of the store.
    #[test]
    fn the_locked_screen_shows_no_agent() {
        let mut s = base();
        s.locked = true;
        s.agents = vec![AgentRow {
            name: "kimi".to_owned(),
            harness: "Codex".to_owned(),
            state: "HELD".to_owned(),
            queued: 0,
            held_by: vec![],
            yolo: None,
        }];
        assert_eq!(
            render(&s),
            "Lunchroom console - screen hidden after 15 min without a key\nType your Lunchroom \
             passphrase to show it again.\n"
        );
    }

    // ---- The keys ------------------------------------------------------------------------------

    #[test]
    fn the_keys_parse_and_the_loosening_ones_are_marked() {
        assert_eq!(parse_key(""), Ok(Key::Nothing));
        assert_eq!(parse_key("  q "), Ok(Key::Quit));
        assert_eq!(parse_key("a 3"), Ok(Key::Approve("3".to_owned())));
        assert_eq!(parse_key("u kimi"), Ok(Key::Unregister("kimi".to_owned())));
        assert_eq!(parse_key("m strict"), Ok(Key::Mode(HoldMode::Strict)));
        assert_eq!(
            parse_key("y kimi,codex till 17:30"),
            Ok(Key::Yolo {
                names: "kimi,codex".to_owned(),
                till: "17:30".to_owned()
            })
        );
        for (line, loosens) in [
            ("a 1", false),
            ("u kimi", false),
            ("m strict", false),
            ("m balanced", true),
            ("m relaxed", true),
            ("y all till 17:30", true),
        ] {
            assert_eq!(parse_key(line).map(|k| k.loosens()), Ok(loosens), "{line}");
        }
        for bad in [
            "a",
            "a x",
            "a 1 2",
            "u",
            "u a b",
            "m",
            "m loose",
            "y kimi",
            "y kimi 17:30",
            "z",
        ] {
            assert!(parse_key(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_key("z"), Err(format!("z is not a key. {KEYS_LINE}")));
    }

    // ---- The liveness file and the notice ------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::test_target()
            .join("console-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    // The liveness file names the console's pid and start time; it counts only while that process
    // runs under that start time.
    #[test]
    fn the_liveness_file_names_a_live_process() {
        let home = scratch("alive");
        assert_eq!(alive_pid(&home), None);
        assert!(write_alive(&home, std::process::id()).is_ok());
        assert_eq!(alive_pid(&home), Some(std::process::id()));
        // The same pid with another start time is another process.
        let text = std::fs::read_to_string(home.join(ALIVE_FILE)).unwrap_or_default();
        let pid = std::process::id();
        assert!(std::fs::write(home.join(ALIVE_FILE), format!("{pid} 1\n")).is_ok());
        assert_eq!(alive_pid(&home), None);
        assert!(text.starts_with(&format!("{pid} ")), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    // No live console: one notice a day, and none when it runs.
    #[test]
    fn the_missing_console_is_told_once_a_day() {
        let home = scratch("missing");
        let want = "The Lunchroom console is not running: start it with sudo -u lunchroom \
                    /usr/local/lib/lunchroom/lunchroom console in a terminal of your own.";
        assert_eq!(missing_notice_at(&home, 86_400 * 10 + 5, true), None);
        assert_eq!(
            missing_notice_at(&home, 86_400 * 10 + 5, false).as_deref(),
            Some(want)
        );
        assert_eq!(missing_notice_at(&home, 86_400 * 10 + 900, false), None);
        assert_eq!(
            missing_notice_at(&home, 86_400 * 11 + 1, false).as_deref(),
            Some(want)
        );
        assert_eq!(missing_notice_at(&home, 86_400 * 11 + 2, true), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    // The mode line: user mode and console mode pass; a console set up with the store still
    // user-owned fails, and so does a setgid program with no console set up.
    #[test]
    fn the_mode_line_fails_a_console_with_a_user_owned_store() {
        let store = Path::new("/home/u/.lunchroom");
        assert!(
            mode_line(false, false, store)
                .is_ok_and(|l| l.starts_with("Lunchroom runs in user mode"))
        );
        assert_eq!(
            mode_line(true, true, Path::new("/var/lib/lunchroom/1000")),
            Ok("the console is set up and the store at /var/lib/lunchroom/1000 is out of reach of \
                your agents (group lunchroom)"
                .to_owned())
        );
        let fail = mode_line(true, false, store);
        assert!(
            fail.as_ref().is_err_and(|l| l.starts_with(
                "the console is set up, but this Lunchroom still uses the store at \
                 /home/u/.lunchroom, which you own"
            )),
            "{fail:?}"
        );
        assert!(mode_line(false, true, store).is_err());
    }

    // ---- The loop ------------------------------------------------------------------------------

    enum Event {
        Line(&'static str),
        Secret(Option<&'static str>),
        Advance(u64),
    }

    struct Script {
        events: VecDeque<Event>,
        drawn: Vec<String>,
        said: String,
        clock: Rc<Cell<u64>>,
    }

    impl Io for Script {
        fn draw(&mut self, text: &str) {
            self.drawn.push(text.to_owned());
        }
        fn say(&mut self, text: &str) {
            self.said.push_str(text);
        }
        fn line(&mut self, _wait: Duration) -> Typed {
            match self.events.pop_front() {
                Some(Event::Line(l)) => Typed::Line(l.to_owned()),
                Some(Event::Advance(s)) => {
                    self.clock.set(self.clock.get() + s);
                    Typed::Quiet
                }
                Some(Event::Secret(_)) | None => Typed::Closed,
            }
        }
        fn secret(&mut self, prompt: &str) -> Option<String> {
            self.said.push_str(prompt);
            match self.events.pop_front() {
                Some(Event::Secret(s)) => s.map(str::to_owned),
                _ => None,
            }
        }
    }

    /// The system the console's commands run on in a test: a terminal that is nobody's, and the
    /// fake user's passphrase check.
    struct Base;

    impl System for Base {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            Vec::new()
        }
        fn own_tty(&self) -> u64 {
            5
        }
        fn tty_of(&self, _key: &str) -> Option<u64> {
            None
        }
        fn ask(&mut self, _q: &str) -> Option<String> {
            None
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn ask_secret(&mut self, _p: &str) -> Option<String> {
            None
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([3; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _l: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct TestFeed {
        records: RefCell<Vec<Record>>,
    }

    impl Feed for TestFeed {
        fn records(&self, _env: &Env) -> Vec<Record> {
            self.records.borrow().clone()
        }
        fn guard_lines(&self, _env: &Env, _mail: &Mail) -> Vec<String> {
            Vec::new()
        }
        fn checks(&self, _env: &Env, _mail: &Mail, _base: &dyn System) -> Vec<String> {
            vec!["PASS test checks".to_owned()]
        }
    }

    struct World {
        env: Env,
        clock: Rc<Cell<u64>>,
        feed: TestFeed,
    }

    impl World {
        fn new(name: &str) -> Self {
            let root = scratch(name);
            let mut env = Env::console_view(1000, root.join("home"));
            env.lunchroom_home = root.join("lunchroom");
            let _ = std::fs::create_dir_all(&env.home);
            Self {
                env,
                clock: Rc::new(Cell::new(1_000_000)),
                feed: TestFeed {
                    records: RefCell::new(Vec::new()),
                },
            }
        }

        fn store(&self) -> Option<Store<Mail>> {
            Store::<Mail>::open(&self.env.store_dir()).ok()
        }

        fn name(&self, name: &str, session: &str) {
            let who = Identity::new(Harness::Codex, session);
            if let Some(store) = self.store() {
                let _ = store.update(|m| m.register(name, &who, Origin::User, None));
            }
        }

        fn mail(&self) -> Mail {
            Store::<Mail>::read_without_change(&self.env.store_dir()).unwrap_or_default()
        }

        /// Runs the console on `events`; the screens drawn, what was said and how it ended.
        fn run(&self, events: Vec<Event>) -> (Vec<String>, String, Ending) {
            let mut io = Script {
                events: events.into(),
                drawn: Vec::new(),
                said: String::new(),
                clock: Rc::clone(&self.clock),
            };
            let clock = Rc::clone(&self.clock);
            let mut tick = move || clock.get();
            let mut run = Run {
                env: self.env.clone(),
                developer: "dev".to_owned(),
                alive: Some(self.env.lunchroom_home.clone()),
                feed: &self.feed,
                clock: &mut tick,
            };
            let ending = serve(&mut io, &mut Base, &mut run);
            (io.drawn, io.said, ending)
        }
    }

    const PASS: &str = "correct horse battery";

    // Three wrong passphrases end the console; nothing is drawn.
    #[test]
    fn three_wrong_passphrases_end_it() {
        let w = World::new("login-fail");
        let (drawn, said, ending) = w.run(vec![
            Event::Secret(Some("one")),
            Event::Secret(Some("two")),
            Event::Secret(Some("three")),
        ]);
        assert_eq!(ending, Ending::Failed);
        assert_eq!(drawn, [] as [std::string::String; 0]);
        assert_eq!(said.matches("The passphrase was wrong.").count(), 3);
        assert!(
            said.ends_with("Too many wrong passphrases; the console ended.\n"),
            "{said}"
        );
    }

    // The right passphrase shows the screen; `q` ends it and the liveness file was written.
    #[test]
    fn the_right_passphrase_shows_the_screen_and_q_quits() {
        let w = World::new("login-ok");
        w.name("kimi", "k1");
        let (drawn, _, ending) = w.run(vec![Event::Secret(Some(PASS)), Event::Line("q")]);
        assert_eq!(ending, Ending::Quit);
        assert_eq!(drawn.len(), 1);
        assert!(drawn[0].contains("\nAGENTS\n  kimi "), "{}", drawn[0]);
        assert!(drawn[0].contains("PASS test checks"));
        assert!(w.env.lunchroom_home.join(ALIVE_FILE).is_file());
    }

    // `u kimi` shows the command's question and acts only on y.
    #[test]
    fn an_action_is_confirmed_and_a_no_changes_nothing() {
        let w = World::new("confirm");
        w.name("kimi", "k1");
        let (drawn, said, _) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Line("u kimi"),
            Event::Line("n"),
            Event::Line("q"),
        ]);
        assert!(
            said.contains("take the name \"kimi\" from codex:k1"),
            "{said}"
        );
        assert!(said.contains("Do it? [y/N] "));
        assert!(
            drawn
                .last()
                .is_some_and(|s| s.contains("Not confirmed. Nothing was changed."))
        );
        assert!(w.mail().named("kimi").is_some());
        let (drawn, _, _) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Line("u kimi"),
            Event::Line("y"),
            Event::Line("q"),
        ]);
        assert!(
            drawn
                .last()
                .is_some_and(|s| s.contains("kimi is no longer named")),
            "{drawn:?}"
        );
        assert!(w.mail().named("kimi").is_none());
    }

    // A tightening action (`m strict`) asks nothing but y/N; a loosening one (`m relaxed`) asks the
    // passphrase again, and a wrong one changes nothing.
    #[test]
    fn a_loosening_action_asks_the_passphrase_again() {
        let w = World::new("loosen");
        let (drawn, said, _) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Line("m strict"),
            Event::Line("y"),
            Event::Line("m relaxed"),
            Event::Line("y"),
            Event::Secret(Some("nope")),
            Event::Line("q"),
        ]);
        assert!(
            drawn.iter().any(|s| s.contains("hold mode Strict")),
            "{drawn:?}"
        );
        assert_eq!(
            said.matches("This loosens Lunchroom. Passphrase again: ")
                .count(),
            1,
            "{said}"
        );
        assert!(
            drawn
                .last()
                .is_some_and(|s| s.contains("The passphrase was wrong. Nothing was changed."))
        );
        assert_eq!(w.mail().hold_mode(), HoldMode::Strict);
        let (_, _, _) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Line("m relaxed"),
            Event::Line("y"),
            Event::Secret(Some(PASS)),
            Event::Line("q"),
        ]);
        assert_eq!(w.mail().hold_mode(), HoldMode::Relaxed);
    }

    // After 15 minutes without a key the screen locks, shows no agent, and a wrong passphrase
    // three times ends the console; the right one brings the screen back.
    #[test]
    fn it_locks_after_15_minutes() {
        let w = World::new("lock");
        w.name("kimi", "k1");
        let (drawn, said, ending) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Advance(IDLE_LOCK_SECS),
            Event::Secret(Some("a")),
            Event::Secret(Some("b")),
            Event::Secret(Some("c")),
        ]);
        assert_eq!(ending, Ending::Failed);
        assert!(
            drawn
                .iter()
                .any(|s| s.starts_with("Lunchroom console - screen hidden")),
            "{drawn:?}"
        );
        assert!(!drawn.last().unwrap_or(&String::new()).contains("kimi"));
        assert!(said.contains("Passphrase: "));
        let w = World::new("lock-ok");
        let (drawn, _, ending) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Advance(IDLE_LOCK_SECS),
            Event::Secret(Some(PASS)),
            Event::Line("q"),
        ]);
        assert_eq!(ending, Ending::Quit);
        assert!(
            drawn.last().is_some_and(|s| s.contains("hold mode")),
            "{drawn:?}"
        );
    }

    fn denied(at: u64, session: &str, n: u64) -> Record {
        Record {
            at,
            session: session.to_owned(),
            hook: records::DENIED_HOOK.to_owned(),
            call: String::new(),
            step: "configuration".to_owned(),
            step_ms: n,
            took_ms: 0,
            undecided: None,
        }
    }

    // Five DENIED calls in ten minutes from one agent: the screen shows PROBING, and the user is
    // told once, as a notice that is then marked told.
    #[test]
    fn a_probing_agent_raises_one_notice() {
        let w = World::new("probe");
        w.name("kimi", "k1");
        let now = w.clock.get();
        {
            let mut records = w.feed.records.borrow_mut();
            for n in 0..5 {
                records.push(denied(now - 60 * n, "k1", n));
            }
            records.push(denied(now - 3_600, "k1", 99));
        }
        let (drawn, _, _) = w.run(vec![
            Event::Secret(Some(PASS)),
            Event::Advance(1),
            Event::Line("q"),
        ]);
        let last = drawn.last().cloned().unwrap_or_default();
        assert!(
            last.contains(
                "  kimi - 5 in the last 10 min, 6 in the last 24 h - PROBING: 5 or more DENIED \
                 calls in 10 minutes\n"
            ),
            "{last}"
        );
        assert!(
            last.contains(
                "kimi had 5 DENIED calls in the last 10 minutes. It may be probing for a way round \
                 Lunchroom's guard: look at what it is doing."
            ),
            "{last}"
        );
        // Shown, so told: the store holds no user notice any more.
        assert_eq!(w.mail().user_notices(), []);
        // Four calls make no alert.
        let w = World::new("probe-4");
        {
            let mut records = w.feed.records.borrow_mut();
            for n in 0..4 {
                records.push(denied(w.clock.get() - n, "k1", n));
            }
        }
        let (drawn, _, _) = w.run(vec![Event::Secret(Some(PASS)), Event::Line("q")]);
        assert!(!drawn.concat().contains("PROBING"));
    }

    // The console's view of the developer: the fixed store folder of their uid, the home folder
    // given, and no variable of the environment in it.
    #[test]
    fn the_consoles_view_is_the_fixed_folder_of_the_developer() {
        let env = Env::console_view(1000, PathBuf::from("/home/dev"));
        assert_eq!(env.lunchroom_home, PathBuf::from("/var/lib/lunchroom/1000"));
        assert_eq!(
            env.store_dir(),
            PathBuf::from("/var/lib/lunchroom/1000/store")
        );
        assert_eq!(env.home, PathBuf::from("/home/dev"));
        assert_eq!(env.codex_home, PathBuf::from("/home/dev/.codex"));
        assert_eq!(env.hermes_home, PathBuf::from("/home/dev/.hermes"));
        assert_eq!(
            (env.claude_pid, env.claude_session, env.codex_thread),
            (None, None, None)
        );
        assert_eq!(env.lunchroom_session, None);
    }

    // The DENIED counts are read from the records: a record of another kind is not counted, and
    // doctor's list of the last 24 hours does not show a DENIED one.
    #[test]
    fn denied_records_are_counted_apart() {
        let now = 1_000_000;
        let mut all = vec![
            denied(now - 5, "a", 1),
            denied(now - 700, "a", 2),
            denied(now - 90_000, "a", 3),
        ];
        all.push(Record {
            hook: "pretool".to_owned(),
            ..denied(now - 5, "a", 4)
        });
        let counts = records::denied_counts(&all, now);
        assert_eq!(counts.len(), 1);
        assert_eq!((counts[0].recent, counts[0].day), (1, 2));
        assert_eq!(records::recent(&all, now).len(), 1);
    }
}
