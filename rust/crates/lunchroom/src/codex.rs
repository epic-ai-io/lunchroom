//! Delivery into a Codex session (spec §10 D1, §13 H1.1 item 4): `codex queue --thread <id>
//! --message <line>`, where the line is one line of Lunchroom's naming the message id and
//! `lunchroom read <id>`, never the envelope (D1.1). Codex shows a queued message as the user's
//! input, so Lunchroom holds the session by the message before it is queued (failing safe, I1.4
//! still lets it reply to the sender), and the message is DELIVERED when the guard's
//! `UserPromptSubmit` hook sees the line or its id appears in the thread's rollout file after the
//! length the file had at the hand-over.
//!
//! Host facts: `docs/host-facts/codex.md`.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Seek as _, SeekFrom};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lunchroom_core::approvals::{Request, TellKind};
use lunchroom_core::identity::Identity;
use lunchroom_core::mail::{AttemptOutcome, Confirmation, Evidence, Mail, RecordMark, Status};
use lunchroom_core::store::Store;

use crate::checks::clock_now;
use crate::delivery::{Handed, handover_line};
use crate::evidence::evidence;
use crate::home::Env;

/// How long `codex queue` may take before the attempt counts as failed.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long Lunchroom looks for the message in the thread's rollout file after queueing it.
const CONFIRM_WAIT: Duration = Duration::from_secs(10);
/// How often it looks.
const CONFIRM_STEP: Duration = Duration::from_millis(500);
/// The least time left, below which a hand-over is not begun: `codex queue` needs a moment, and a
/// hand-over cut off half way is the worst outcome (the message is marked handed over). The next
/// `lunchroom check` makes it instead.
const LEAST_TO_HAND_OVER: Duration = Duration::from_secs(3);

/// What one delivery attempt to a Codex session did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// Nothing was offered: no message waits, the session is not guarded (R3.2), or one is
    /// already in front of the harness.
    Nothing,
    /// Queued and seen in the thread, or confirmed by its session's guard: DELIVERED.
    Confirmed(String),
    /// Queued; not yet seen (Codex is mid-turn): it stays QUEUED until confirmed.
    Unconfirmed(String),
    /// The hand-over did not succeed, for the reason; how that stands is [`FailedAs`].
    Failed(String, String, FailedAs),
}

/// How a hand-over that did not succeed stands, so no case is taken for another (a counted
/// failure is not "the program cannot be established").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedAs {
    /// The recipient's program could not be established, so nothing was tried: the attempt is not
    /// counted, the message stays QUEUED and not handed over, and the user starts the session
    /// anew.
    NotAttempted,
    /// The attempt failed and is counted (D1.4); the message stays QUEUED.
    Counted,
    /// The message is FAILED: the attempt was its third, or it was made FAILED while the hand-over
    /// waited (its recipient's name was taken away).
    NowFailed,
}

impl FailedAs {
    /// The message is FAILED.
    #[must_use]
    pub const fn message_failed(self) -> bool {
        matches!(self, Self::NowFailed)
    }
}

/// Offers the next waiting message for the Codex session `to`, if any, through `codex queue`.
///
/// # Errors
/// When the store cannot be used.
/// `by`: the sender whose `lunchroom send` makes this hand-over. It hands over only that sender's
/// own message, never another sender's, and looks for the message in the thread's rollout file for
/// a while. `None` from the session's own `Stop` hook: that hands over the next message whoever
/// sent it and does not wait, since the session takes the message only after that hook ends, and
/// its prompt hook confirms it then.
pub fn deliver(
    env: &Env,
    store: &Store<Mail>,
    to: &Identity,
    by: Option<&Identity>,
) -> Result<Delivered, String> {
    hand_over(env, store, to, by, Again::No, None)
}

/// [`deliver`] for the recipient's own hook, which must answer within its time limit: nothing
/// waits past `deadline`, and a hand-over that would not finish by then is not begun (the next
/// `lunchroom check` makes it).
///
/// # Errors
/// When the store cannot be used.
pub fn deliver_until(
    env: &Env,
    store: &Store<Mail>,
    to: &Identity,
    deadline: Instant,
) -> Result<Delivered, String> {
    hand_over(env, store, to, None, Again::No, Some(deadline))
}

/// D1.7: offers `to` again, at once and with no wait, a message whose earlier hand-over counted as
/// failed, through the recipient's own `codex queue` as [`deliver`] does, whatever session the
/// check that calls this runs in; with `only_from`, only that sender's own message, since the check
/// then runs in that sender's process (D1.1). No other message is offered.
///
/// # Errors
/// When the store cannot be used.
pub fn deliver_again(
    env: &Env,
    store: &Store<Mail>,
    to: &Identity,
    only_from: Option<&Identity>,
) -> Result<Delivered, String> {
    hand_over(env, store, to, None, Again::Of(only_from), None)
}

/// Whether a hand-over is the first offer of a message or an offer again by a check (D1.7).
#[derive(Clone, Copy)]
pub(crate) enum Again<'a> {
    No,
    /// A check's offer again, of any sender's message or, with `Some`, only that sender's.
    Of(Option<&'a Identity>),
}

/// D1.7: each message handed to a Codex session and awaiting its confirmation is looked for again
/// in that session's record (its thread's rollout file), and DELIVERED when there. Nothing is read
/// past `deadline`.
///
/// # Errors
/// When the store cannot be used.
pub fn confirm_recorded(
    env: &Env,
    store: &Store<Mail>,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let mail = store.read().map_err(crate::cli::unread)?;
    let waiting: Vec<(Identity, String, PathBuf, Option<RecordMark>)> = mail
        .queued()
        .into_iter()
        .filter(|m| m.handed_over && m.to.harness == lunchroom_core::identity::Harness::Codex)
        .map(|m| {
            let home = crate::evidence::recorded_codex_home(&mail, &m.to, &env.home);
            (m.to.clone(), m.id.clone(), home, mail.handed_at(&m.id))
        })
        .collect();
    for (to, id, codex_home, mark) in waiting {
        if in_rollout_until(&codex_home, &to.session, &id, mark, deadline) {
            confirm(store, &to, &id, clock_now(env))?;
        }
    }
    Ok(())
}

/// [`deliver`], or with [`Again::Of`] [`deliver_again`]; with a `deadline`, nothing runs or waits past it.
fn hand_over(
    env: &Env,
    store: &Store<Mail>,
    to: &Identity,
    by: Option<&Identity>,
    again: Again<'_>,
    deadline: Option<Instant>,
) -> Result<Delivered, String> {
    let wait = by.is_some();
    // One read, outside the exclusive lock: the recipient's own Codex folder, from the process its
    // guard hook ran in, for `codex queue` and its rollout files; the messages already handed over;
    // and what the evidence shows of the session (the judgment reads files and processes, so it is
    // never made while the lock is held).
    let mail = store.read().map_err(crate::cli::unread)?;
    let codex_home = crate::evidence::recorded_codex_home(&mail, to, &env.home);
    let waiting: Vec<(String, Option<RecordMark>)> = mail
        .handed_over_ids(to)
        .into_iter()
        .map(|id| {
            let mark = mail.handed_at(&id);
            (id, mark)
        })
        .collect();
    let judged = evidence(&mail, to, &env.home);
    drop(mail);
    // D1.1: a message handed over earlier and now in the thread's session record is DELIVERED.
    for (id, mark) in &waiting {
        if in_rollout_until(&codex_home, &to.session, id, *mark, deadline) {
            confirm(store, to, id, clock_now(env))?;
        }
    }
    if deadline.is_some_and(|d| d.saturating_duration_since(Instant::now()) < LEAST_TO_HAND_OVER) {
        return Ok(Delivered::Nothing);
    }
    // D1.1: the length and file identity of the plain session record at this moment, which the
    // search for the id starts from. Read before the lock, as the judgment is: the id is not in the
    // record before the hand-over, so nothing written meanwhile can hide it.
    let mark = record_mark(&codex_home, &to.session);
    // The hold and the hand-over mark are kept before `codex queue` runs, in one update: a crash
    // after it can leave the message not confirmed, never handed over twice (D1.2). The clock is
    // read here, under the lock, not at the start of the process: a process that waited for the
    // lock would otherwise count a newer hand-over as one stamped in the future.
    let next = update(store, |m| {
        let now = clock_now(env);
        if !guarded(m, now, judged) {
            return None;
        }
        let process = m.session(to).map(|r| r.process.clone())?;
        let message = match again {
            Again::Of(only_from) => m.present_again_from(to, only_from, now)?,
            Again::No => m.present_from(to, by, now)?,
        };
        m.hold_before_delivery(to, &message.id);
        m.handed_over_at(&message.id, mark);
        Some((
            message.id.clone(),
            handover_line(&message.id, Handed::Message),
            process,
        ))
    })?;
    let Some((id, text, process)) = next else {
        return Ok(Delivered::Nothing);
    };
    // D1.2: a message already in the thread's session record is confirmed, never queued again.
    let kept = update(store, |m| m.handed_at(&id))?;
    if in_rollout_until(&codex_home, &to.session, &id, kept, deadline) {
        return confirmed(store, to, id, clock_now(env));
    }
    let timeout = deadline.map_or(QUEUE_TIMEOUT, |d| {
        QUEUE_TIMEOUT.min(d.saturating_duration_since(Instant::now()))
    });
    let handed = queue(
        &process,
        &to.session,
        &text,
        &codex_home,
        &env.home,
        timeout,
    );
    // §16 item 5, D1.5: Codex took the message (or gave no answer, below): the session is HELD
    // from the hand-over, and the line "<agent> is HELD." is told now, not before the program
    // had run (a hand-over that fails releases the hold).
    if matches!(handed, Ok(()) | Err(QueueError::Unknown)) {
        update(store, |m| m.hand_over_held(&id, clock_now(env)))?;
    }
    // D1.3: `codex queue` gave no answer, so it is not known whether Codex took the message. It
    // stays handed over (never queued again, D1.2), reported not confirmed, and is not counted
    // as a failed attempt.
    if handed == Err(QueueError::Unknown) {
        return Ok(Delivered::Unconfirmed(id));
    }
    // A recipient's `codex` program that cannot be established says nothing about the recipient:
    // the message stays QUEUED, uncounted and not handed over, for a later attempt (D1.3).
    if let Err(QueueError::Missing(reason)) = handed {
        update(store, |m| m.hand_over_undone(&id))?;
        return Ok(Delivered::Failed(id, reason, FailedAs::NotAttempted));
    }
    if let Err(QueueError::Failed(reason)) = handed {
        return failed(store, id, reason, clock_now(env));
    }
    if !wait {
        return Ok(Delivered::Unconfirmed(id));
    }
    await_confirmation(
        store,
        &codex_home,
        to,
        (id, kept),
        env,
        (CONFIRM_WAIT, CONFIRM_STEP),
        deadline,
    )
}

/// Waits up to `wait` for the message `id`, handed to `to`, to be confirmed, looking every `step`
/// (`timing` is `(wait, step)`): in the thread's record, or confirmed by the session's guard
/// meanwhile. What it reports is what the store says at the end: DELIVERED is
/// [`Delivered::Confirmed`], and a message made FAILED while it waited (the recipient's name was
/// taken away, the attempts ran out) is [`Delivered::Failed`], never DELIVERED; anything else is
/// [`Delivered::Unconfirmed`].
fn await_confirmation(
    store: &Store<Mail>,
    codex_home: &Path,
    to: &Identity,
    (id, kept): (String, Option<RecordMark>),
    env: &Env,
    (wait, step): (Duration, Duration),
    deadline: Option<Instant>,
) -> Result<Delivered, String> {
    let start = Instant::now();
    while start.elapsed() < wait && deadline.is_none_or(|d| Instant::now() < d) {
        // The guard's prompt hook in that session may have confirmed it first, or it may have been
        // made FAILED.
        if let done @ (Delivered::Confirmed(_) | Delivered::Failed(..)) =
            settled(store, id.clone())?
        {
            return Ok(done);
        }
        if in_rollout_until(codex_home, &to.session, &id, kept, deadline) {
            return confirmed(store, to, id, clock_now(env));
        }
        std::thread::sleep(step);
    }
    settled(store, id)
}

/// Changes the store; an error says the store cannot be used.
pub(crate) fn update<R>(
    store: &Store<Mail>,
    change: impl FnOnce(&mut Mail) -> R,
) -> Result<R, String> {
    store.update(change).map_err(crate::cli::unwritten)
}

/// Whether a delivery to a recipient judged `judged` may go on, after the hand-overs nobody
/// confirmed in time count as failed attempts at `now`, so a dead session does not keep its hold
/// (D1.4). The judgment is made by the caller before it takes the store's lock.
pub(crate) fn guarded(m: &mut Mail, now: u64, judged: Evidence) -> bool {
    m.expire_handovers(now);
    judged == Evidence::Guarded
}

/// The message `id` handed to `to` is DELIVERED, at `now`; what the store says of it.
fn confirm(store: &Store<Mail>, to: &Identity, id: &str, now: u64) -> Result<Confirmation, String> {
    update(store, |m| m.confirm_at(to, id, now))
}

/// The message `id` was seen in `to`'s record: [`Delivered::Confirmed`] only when the store shows it
/// DELIVERED (it was shown now, or had been), and what the store says otherwise (a message already
/// FAILED stays FAILED and is reported so, never as delivered).
fn confirmed(
    store: &Store<Mail>,
    to: &Identity,
    id: String,
    now: u64,
) -> Result<Delivered, String> {
    match confirm(store, to, &id, now)? {
        Confirmation::Show | Confirmation::Duplicate => Ok(Delivered::Confirmed(id)),
        Confirmation::Failed | Confirmation::NotFound => settled(store, id),
    }
}

/// Where the store has the message `id`: DELIVERED, FAILED with its reason, or still QUEUED (or
/// unknown), which is not confirmed.
pub(crate) fn settled(store: &Store<Mail>, id: String) -> Result<Delivered, String> {
    let mail = store.read().map_err(crate::cli::unread)?;
    Ok(match mail.message(&id).map(|m| m.status.clone()) {
        Some(Status::Delivered) => Delivered::Confirmed(id),
        Some(Status::Failed { reason }) => Delivered::Failed(id, reason, FailedAs::NowFailed),
        Some(Status::Queued) | None => Delivered::Unconfirmed(id),
    })
}

/// The attempt to hand over the message `id` failed for `reason` at `now`; it is counted (D1.4) and
/// told (D1.8). A message that is no longer QUEUED is reported as the store has it.
pub(crate) fn failed(
    store: &Store<Mail>,
    id: String,
    reason: String,
    now: u64,
) -> Result<Delivered, String> {
    match update(store, |m| m.attempt_failed(&id, &reason, now))? {
        Some(AttemptOutcome::Failed) => Ok(Delivered::Failed(id, reason, FailedAs::NowFailed)),
        Some(AttemptOutcome::StillQueued { .. }) => {
            Ok(Delivered::Failed(id, reason, FailedAs::Counted))
        }
        None => settled(store, id),
    }
}

/// Tells the Codex session of `request` of the user's decision `id` (A1.2, §17.10): through
/// `codex queue`, one line naming only the id, which the agent reads with `lunchroom read <id>`,
/// so the call's text never stands on a command line. The line adds no hold and clears none: the
/// session's prompt hook knows the id of a decision told to it.
///
/// # Errors
/// Why the session could not be told.
pub fn tell(
    env: &Env,
    store: &Store<Mail>,
    request: &Request,
    id: &str,
    kind: TellKind,
) -> Result<(), String> {
    let text = handover_line(
        id,
        match kind {
            TellKind::Approved => Handed::Approved,
            TellKind::Refused => Handed::Refused,
        },
    );
    let mail = store.read().map_err(|e| e.to_string())?;
    let process = mail
        .session(&request.session)
        .map(|r| r.process.clone())
        .ok_or("the session is not recorded")?;
    let codex_home = crate::evidence::recorded_codex_home(&mail, &request.session, &env.home);
    queue(
        &process,
        &request.session.session,
        &text,
        &codex_home,
        &env.home,
        QUEUE_TIMEOUT,
    )
    .map_err(|e| match e {
        QueueError::Missing(reason) | QueueError::Failed(reason) => reason,
        QueueError::Unknown => "codex queue gave no answer".to_owned(),
    })
}

/// Why a `codex queue` did not plainly succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QueueError {
    /// The recipient's own `codex` program cannot be established or is gone: nothing was tried.
    Missing(String),
    /// It did not run, or it ran and exited with an error: Codex did not take the message.
    Failed(String),
    /// It gave no answer in time, or could not be waited for: the outcome is unknown.
    Unknown,
}

/// What a `codex queue` that could not start says about the delivery.
fn spawn_error(e: &std::io::Error) -> QueueError {
    if e.kind() == std::io::ErrorKind::NotFound {
        QueueError::Missing("the recipient's codex program was not found".to_owned())
    } else {
        QueueError::Failed(format!("codex queue could not start: {e}"))
    }
}

/// The program the harness process `process` (`<pid>:<start time>`, as a guard hook recorded it) is
/// running, by its absolute path, for Lunchroom to run in its place: never a name looked up on a
/// `PATH`, which a sender could point at a program of its own (it would be handed the recipient's
/// messages). The process must still be the one recorded, and the path must name the very file the
/// process runs (same device and inode as `/proc/<pid>/exe`), so a program put in its place, or one
/// the harness replaced since it started, is refused.
///
/// # Errors
/// Why the program cannot be established.
pub(crate) fn recipient_program(process: &str) -> Result<PathBuf, String> {
    let pid = process
        .split(':')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .ok_or("the recipient's process is not recorded")?;
    if crate::process::key(pid).as_deref() != Some(process) {
        return Err("the recipient's process has ended".to_owned());
    }
    let exe = PathBuf::from(format!("/proc/{pid}/exe"));
    // As the user's own group: a setgid program cannot read another process's exe link
    // (`lunchroom_core::setgid::as_own_group`).
    let (path, same) = lunchroom_core::setgid::as_own_group(|| {
        let path = std::fs::read_link(&exe);
        let same = path.as_ref().is_ok_and(|p| same_file(p, &exe));
        (path, same)
    });
    let path = path.map_err(|_| "the recipient's program cannot be read".to_owned())?;
    if same {
        Ok(path)
    } else {
        Err("the recipient's program is not the file it is running".to_owned())
    }
}

/// `path` is absolute, an executable file, and the same file (device and inode) as `running`.
fn same_file(path: &Path, running: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(path), std::fs::metadata(running)) else {
        return false;
    };
    path.is_absolute()
        && a.is_file()
        && a.permissions().mode() & 0o111 != 0
        && (a.dev(), a.ino()) == (b.dev(), b.ino())
}

/// The variables among `keys` in the starting environment of the recipient's process `process`:
/// the child of a hand-over gets its environment from the recipient, never from the sender whose
/// process spawns it (a sender's `NODE_OPTIONS` or `LD_PRELOAD` must not reach it). `None` when it
/// cannot be read; a recipient started with an empty environment has none to give, and that is not
/// a failure.
pub(crate) fn recipient_env(process: &str, keys: &[&str]) -> Option<Vec<(String, String)>> {
    let raw = process
        .split(':')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .and_then(|p| {
            lunchroom_core::setgid::as_own_group(|| std::fs::read(format!("/proc/{p}/environ")))
                .ok()
        })?;
    let variables = raw
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let kv = String::from_utf8_lossy(kv);
            let (k, v) = kv.split_once('=')?;
            keys.contains(&k).then(|| (k.to_owned(), v.to_owned()))
        })
        .collect();
    Some(variables)
}

/// Why a hand-over does not start when the recipient's environment cannot be read (D1.1).
pub(crate) const UNREADABLE_ENV: &str = "the recipient's environment cannot be read";

/// What a hand-over's program keeps of the recipient's environment besides what the harness's own
/// variables name.
pub(crate) const CHILD_ENV: [&str; 8] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "XDG_CONFIG_HOME",
];

/// Runs the recipient's `codex queue` for one thread, giving it up to `timeout`; an error names why
/// it failed.
fn queue(
    process: &str,
    thread: &str,
    text: &str,
    codex_home: &Path,
    home: &Path,
    timeout: Duration,
) -> Result<(), QueueError> {
    let program = recipient_program(process).map_err(QueueError::Missing)?;
    let variables = recipient_env(process, &CHILD_ENV)
        .ok_or_else(|| QueueError::Missing(UNREADABLE_ENV.to_owned()))?;
    let mut command = Command::new(&program);
    command
        .args(["queue", "--thread", thread, "--message", text])
        .env_clear()
        .env("HOME", home)
        .envs(variables)
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // 1.1: a child never inherits the group that opens the store.
    lunchroom_core::setgid::drop_group(&mut command);
    let child = command.spawn();
    let mut child = child.map_err(|e| spawn_error(&e))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(QueueError::Failed(format!(
                    "codex queue exited {}",
                    status.code().unwrap_or(-1)
                )));
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(QueueError::Unknown);
            }
            Err(_) => return Err(QueueError::Unknown),
        }
    }
}

/// The thread's rollout files: `<codex home>/sessions/**/rollout-*<thread>*.jsonl`.
fn rollouts(codex_home: &Path, thread: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![(codex_home.join("sessions"), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() && depth < 4 {
                dirs.push((path, depth + 1));
            } else if name.starts_with("rollout-")
                && Path::new(&name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
                && name.contains(thread)
            {
                out.push(path);
            }
        }
    }
    out
}

/// The end line of a hand-over, which follows the line that names the id.
const END_LINE: &str = "[END LUNCHROOM MESSAGE ";

/// The plain session record of the thread as it stands now (D1.1): the length and the file
/// identity (device and inode) of the newest `rollout-*<thread>*.jsonl`, or all 0 when there is
/// none.
pub(crate) fn record_mark(codex_home: &Path, thread: &str) -> RecordMark {
    rollouts(codex_home, thread)
        .iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            Some((
                meta.modified().ok(),
                RecordMark {
                    len: meta.len(),
                    dev: meta.dev(),
                    ino: meta.ino(),
                },
            ))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, mark)| mark)
        .unwrap_or_default()
}

/// [`in_rollout_until`] with no mark kept and no deadline.
#[cfg(test)]
fn in_rollout(codex_home: &Path, thread: &str, id: &str) -> bool {
    in_rollout_until(codex_home, thread, id, None, None)
}

/// The message's hand-over line is in the thread's rollout file: some text there holds a line of
/// Lunchroom's that names the id followed by the end line `[END LUNCHROOM MESSAGE <id>]`. A body
/// line of a message always starts with its `[lr:` label, so no message's text can stand in for
/// another's hand-over (D1.2).
///
/// D1.1: a file is searched from the length kept at the hand-over (`kept`) to its end when it is
/// the very file kept (device and inode) and no shorter than the kept length; from its start when
/// it is shorter, is another file (a compressed record written out plain again, a migrated one) or
/// no length was kept. A file is read as a stream, one line at a time, so a long session's record
/// is never held in memory whole. Nothing is read past `deadline`; a search that was cut off finds
/// nothing, and the next check looks again.
fn in_rollout_until(
    codex_home: &Path,
    thread: &str,
    id: &str,
    kept: Option<RecordMark>,
    deadline: Option<Instant>,
) -> bool {
    // H1.7: the step named if the guard's time runs out here.
    let _step =
        lunchroom_core::steps::begin(format!("searching the Codex session record of {thread}"));
    rollouts(codex_home, thread)
        .iter()
        .any(|p| file_holds(p, id, kept, deadline))
}

/// Where in `file` the search starts for a hand-over made when the record stood as `kept`.
fn search_from(file: &File, kept: Option<RecordMark>) -> u64 {
    let Some(kept) = kept else { return 0 };
    let Ok(meta) = file.metadata() else { return 0 };
    if meta.len() >= kept.len && (meta.dev(), meta.ino()) == (kept.dev, kept.ino) {
        kept.len
    } else {
        0
    }
}

/// Whether the rollout file `path` holds the hand-over line of `id` after the kept length: the
/// line naming the id followed by the end line, as lines of the file or inside one of its JSON
/// lines' strings.
fn file_holds(path: &Path, id: &str, kept: Option<RecordMark>, deadline: Option<Instant>) -> bool {
    let end_line = format!("{END_LINE}{id}]");
    let names_id = |line: &str| line.starts_with("Lunchroom: ") && line.contains(id);
    let holds = |text: &str| {
        let lines: Vec<&str> = text.lines().collect();
        lines.windows(2).any(|w| names_id(w[0]) && w[1] == end_line)
    };
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let from = search_from(&file, kept);
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return false;
    }
    let mut reader = BufReader::new(file);
    let mut after_naming_line = false;
    let mut raw = Vec::new();
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return false;
        }
        raw.clear();
        if !matches!(reader.read_until(b'\n', &mut raw), Ok(n) if n > 0) {
            return false;
        }
        let text = String::from_utf8_lossy(&raw);
        let line = text.trim_end_matches(['\n', '\r']);
        // The hand-over as two lines of the file.
        if after_naming_line && line == end_line {
            return true;
        }
        after_naming_line = names_id(line);
        // Or as a string in one of its JSON lines; only a line that names the id can hold it.
        if line.contains(id)
            && serde_json::from_str::<serde_json::Value>(line).is_ok_and(|v| any_string(&v, &holds))
        {
            return true;
        }
    }
}

/// Whether any string inside `v` satisfies `f`.
fn any_string(v: &serde_json::Value, f: &impl Fn(&str) -> bool) -> bool {
    match v {
        serde_json::Value::String(s) => f(s),
        serde_json::Value::Array(a) => a.iter().any(|x| any_string(x, f)),
        serde_json::Value::Object(o) => o.values().any(|x| any_string(x, f)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use super::*;

    // §10::3 (Codex): a missing `codex` binary is not a failed delivery, so it is not
    // counted; any other start error is.
    #[test]
    fn a_missing_codex_binary_is_not_a_failed_attempt() {
        assert!(matches!(
            spawn_error(&Error::from(ErrorKind::NotFound)),
            QueueError::Missing(_)
        ));
        assert!(matches!(
            spawn_error(&Error::from(ErrorKind::PermissionDenied)),
            QueueError::Failed(_)
        ));
    }

    fn me() -> String {
        let key = crate::process::key(std::process::id());
        assert!(key.is_some());
        key.unwrap_or_default()
    }

    // §10::1 (Codex): the program a hand-over runs is the file the recipient's process
    // runs, by its absolute path, never a name found on a PATH.
    #[test]
    fn the_program_is_the_file_the_recipients_process_runs() {
        let program = recipient_program(&me());
        assert!(program.is_ok(), "{program:?}");
        let program = program.unwrap_or_default();
        assert!(program.is_absolute());
        assert_eq!(
            std::fs::read_link("/proc/self/exe").ok().as_deref(),
            Some(program.as_path())
        );
    }

    // §10::1 (Codex): a process that is not the recorded one (ended, its pid reused,
    // never recorded) gives no program.
    #[test]
    fn no_program_for_a_process_that_is_not_the_recorded_one() {
        let pid = std::process::id();
        for process in [
            format!("{pid}:1"),
            format!("{pid}"),
            format!("{}:1", u32::MAX),
            "x:1".to_owned(),
            String::new(),
        ] {
            assert!(recipient_program(&process).is_err(), "{process}");
        }
    }

    // §10::1 (Codex): a program a sender planted (a script of its own, a copy, a path
    // with a relative spelling, a file the process no longer runs) is not the recipient's program.
    #[test]
    fn a_planted_program_is_not_the_one_the_process_runs() {
        let dir = crate::test_target()
            .join("codex-unit")
            .join(format!("plant-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let running = Path::new("/proc/self/exe");
        let planted = dir.join("codex");
        let _ = std::fs::write(&planted, "#!/bin/sh\ncat\n");
        let _ = std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755));
        assert!(planted.is_file());
        let own = std::fs::read_link(running).unwrap_or_default();
        assert!(same_file(&own, running));
        assert!(!same_file(&planted, running));
        assert!(!same_file(Path::new("codex"), running));
        assert!(!same_file(&dir.join("missing"), running));
        let mut deleted = own.clone().into_os_string();
        deleted.push(" (deleted)");
        assert!(!same_file(Path::new(&deleted), running));
        // Not executable: not a program.
        let _ = std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o644));
        assert!(!same_file(&planted, &planted));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §10::1 (Codex): the program's environment comes from the recipient's process and
    // holds only the listed variables; a sender's `NODE_OPTIONS`, `LD_PRELOAD` or `PATH` is not in
    // it, and a process that is not running gives none.
    #[test]
    fn the_environment_is_the_recipients_and_only_what_is_listed() {
        let env = recipient_env(&me(), &CHILD_ENV).unwrap_or_default();
        assert_ne!(env, Vec::new());
        assert!(env.iter().all(|(k, _)| CHILD_ENV.contains(&k.as_str())));
        assert!(
            recipient_env(&me(), &["NODE_OPTIONS", "LD_PRELOAD"])
                .unwrap_or_default()
                .iter()
                .all(|(k, _)| k == "NODE_OPTIONS" || k == "LD_PRELOAD")
        );
        assert!(!CHILD_ENV.contains(&"NODE_OPTIONS") && !CHILD_ENV.contains(&"LD_PRELOAD"));
        assert_eq!(recipient_env(&format!("{}:1", u32::MAX), &CHILD_ENV), None);
    }

    // §10::1 (Codex): a recipient started with an empty environment is readable and has
    // no variables, which is not the unreadable environment that refuses a hand-over.
    #[test]
    fn an_empty_environment_is_not_an_unreadable_one() {
        let started = Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .stdin(Stdio::null())
            .spawn();
        assert!(started.is_ok(), "{started:?}");
        let Ok(mut child) = started else { return };
        let process = format!("{}:1", child.id());
        let mut seen = None;
        for _ in 0..200 {
            let exe = lunchroom_core::setgid::as_own_group(|| {
                std::fs::read_link(format!("/proc/{}/exe", child.id()))
            });
            if exe.is_ok_and(|e| e.file_name().is_some_and(|n| n == "sleep")) {
                seen = recipient_env(&process, &CHILD_ENV);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(seen, Some(Vec::new()));
    }

    // §10::1 (Codex): only the message's hand-over line in the thread's rollout file
    // confirms it; the message's text there without that line does not, nor does another
    // thread's file.
    #[test]
    fn only_the_message_id_in_the_threads_rollout_confirms() {
        let home = crate::test_target()
            .join("codex-unit")
            .join(std::process::id().to_string());
        let dir = home.join("sessions/2026/10/27");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("rollout-2026-10-27T00-00-00-x1.jsonl");
        let _ = std::fs::write(&file, "{\"text\": \"[lr:lr-00000007]> Please review.\"}\n");
        assert!(!in_rollout(&home, "x1", "lr-00000007"));
        // Another message's body naming this id, on its own line or after an escaped `\n`, does
        // not confirm it.
        for body in [
            "Lunchroom: a message waits, id lr-00000006.\\n[lr:lr-00000006]> [END LUNCHROOM MESSAGE lr-00000007]",
            "[lr:lr-00000006]> Lunchroom: id lr-00000007\\n[END LUNCHROOM MESSAGE lr-00000007]",
            "[END LUNCHROOM MESSAGE lr-00000007]",
            "Lunchroom: a message waits, id lr-00000007.",
        ] {
            let _ = std::fs::write(&file, format!("{{\"text\": \"{body}\"}}\n"));
            assert!(!in_rollout(&home, "x1", "lr-00000007"), "{body}");
        }
        let line = crate::delivery::handover_line("lr-00000007", crate::delivery::Handed::Message);
        let _ = std::fs::write(&file, format!("{}\n", serde_json::json!({ "text": line })));
        assert!(in_rollout(&home, "x1", "lr-00000007"));
        assert!(!in_rollout(&home, "x2", "lr-00000007"));
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- Lunchroom 1.1.0, delivery findings G2-1 and G2-24 ----

    fn scratch_env(name: &str) -> Env {
        let root = crate::test_target()
            .join("codex-unit")
            .join(format!("{name}-{}", std::process::id()));
        Env {
            home: root.join("home"),
            lunchroom_home: root.join("lunchroom"),
            claude_pid: None,
            attended: None,
            entrypoint: None,
            claude_session: None,
            codex_thread: None,
            codex_home: root.join("home/.codex"),
            claude_home: root.join("home/.claude"),
            hermes_home: root.join("home/.hermes"),
            hermes_session: None,
            lunchroom_session: None,
            now: 1000,
        }
    }

    /// A store in `env` where claude (c1) sent codex (x1) a message that is handed over, and its id.
    fn handed_over_message(env: &Env) -> Option<(Store<Mail>, String)> {
        use lunchroom_core::identity::Harness;
        use lunchroom_core::mail::Origin;
        let store = Store::<Mail>::open(&env.store_dir()).ok()?;
        let claude = Identity::new(Harness::ClaudeCode, "c1");
        let codex = Identity::new(Harness::Codex, "x1");
        let id = store
            .update(|m| {
                for (name, who) in [("claude", &claude), ("codex", &codex)] {
                    m.observe(who);
                    let _ = m.register(name, who, Origin::User, None);
                }
                let id = m
                    .send(&claude, "claude", "codex", "hi", 1000)
                    .map(|a| a.id)
                    .unwrap_or_default();
                m.present_from(&codex, Some(&claude), 1000);
                m.hold_before_delivery(&codex, &id);
                m.handed_over(&id);
                id
            })
            .ok()?;
        Some((store, id))
    }

    fn codex_x1() -> Identity {
        Identity::new(lunchroom_core::identity::Harness::Codex, "x1")
    }

    // G2-1: a message that is made FAILED while `send` waits for its confirmation (the user takes
    // the recipient's name away) is reported FAILED, with its reason, never as DELIVERED. Before the
    // fix the wait took "no longer presented" for "confirmed" and printed DELIVERED.
    #[test]
    fn a_message_made_failed_while_send_waits_is_reported_failed() {
        let env = scratch_env("failed-wait");
        let made = handed_over_message(&env);
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        let other = Store::<Mail>::open(&env.store_dir());
        assert!(other.is_ok());
        let Ok(other) = other else { return };
        let outcome = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                let _ = other.update(|m| {
                    m.unregister("codex");
                    m.fail_unnamed(2000)
                });
            });
            await_confirmation(
                &store,
                &env.codex_home,
                &codex_x1(),
                (id.clone(), None),
                &env,
                (Duration::from_secs(10), Duration::from_millis(20)),
                None,
            )
        });
        assert_eq!(
            outcome,
            Ok(Delivered::Failed(
                id.clone(),
                "codex no longer takes messages: it left with `leave`, or its name was taken away"
                    .to_owned(),
                FailedAs::NowFailed
            ))
        );
        let report = crate::cli::hand_over_report(&id, &outcome);
        assert!(
            report.contains("FAILED") && !report.contains("DELIVERED"),
            "{report}"
        );
        let _ = std::fs::remove_dir_all(env.lunchroom_home.clone());
    }

    // G2-1: confirmed by the session's guard while `send` waits is DELIVERED; nothing by the end of
    // the wait is not confirmed; a message seen in the thread's record is DELIVERED in the store.
    #[test]
    fn a_wait_reports_what_the_store_says_at_its_end() {
        let env = scratch_env("wait-ends");
        let made = handed_over_message(&env);
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        let to = codex_x1();
        let timing = (Duration::from_millis(80), Duration::from_millis(20));
        assert_eq!(
            await_confirmation(
                &store,
                &env.codex_home,
                &to,
                (id.clone(), None),
                &env,
                timing,
                None
            ),
            Ok(Delivered::Unconfirmed(id.clone()))
        );
        // The guard's prompt hook confirms it meanwhile.
        let other = Store::<Mail>::open(&env.store_dir());
        assert!(other.is_ok());
        let Ok(other) = other else { return };
        let outcome = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(60));
                let _ = other.update(|m| m.confirm_at(&codex_x1(), &id, 1100));
            });
            await_confirmation(
                &store,
                &env.codex_home,
                &to,
                (id.clone(), None),
                &env,
                (Duration::from_secs(10), Duration::from_millis(20)),
                None,
            )
        });
        assert_eq!(outcome, Ok(Delivered::Confirmed(id)));
        // Its envelope in the thread's record.
        let env = scratch_env("wait-record");
        let made = handed_over_message(&env);
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        let sessions = env.codex_home.join("sessions/2026/11/02");
        assert!(std::fs::create_dir_all(&sessions).is_ok());
        let line = serde_json::json!({
            "text": crate::delivery::handover_line(&id, crate::delivery::Handed::Message)
        });
        assert!(
            std::fs::write(
                sessions.join("rollout-2026-11-02T00-00-00-x1.jsonl"),
                format!("{line}\n")
            )
            .is_ok()
        );
        let outcome = await_confirmation(
            &store,
            &env.codex_home,
            &to,
            (id.clone(), None),
            &env,
            timing,
            None,
        );
        assert_eq!(outcome, Ok(Delivered::Confirmed(id.clone())));
        assert_eq!(
            store
                .read()
                .ok()
                .and_then(|m| m.message(&id).map(|d| d.status.clone())),
            Some(Status::Delivered)
        );
    }

    // G2-2: a failed attempt says how it stands: counted and still QUEUED, then counted and FAILED
    // on the third, and a message already FAILED is reported FAILED (not counted again).
    #[test]
    fn a_failed_hand_over_says_how_it_stands() {
        let env = scratch_env("failed-as");
        let made = handed_over_message(&env);
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        let why = "codex queue exited 1".to_owned();
        for _ in 0..2 {
            assert_eq!(
                failed(&store, id.clone(), why.clone(), 1000),
                Ok(Delivered::Failed(
                    id.clone(),
                    why.clone(),
                    FailedAs::Counted
                ))
            );
        }
        let third = failed(&store, id.clone(), why.clone(), 1000);
        assert_eq!(
            third,
            Ok(Delivered::Failed(
                id.clone(),
                why.clone(),
                FailedAs::NowFailed
            ))
        );
        assert_eq!(
            failed(&store, id.clone(), why.clone(), 1000),
            third,
            "a message already FAILED is reported FAILED"
        );
        assert!(FailedAs::NowFailed.message_failed());
        assert!(!FailedAs::Counted.message_failed());
        assert!(!FailedAs::NotAttempted.message_failed());
        let _ = std::fs::remove_dir_all(env.lunchroom_home.clone());
    }

    /// The hand-over line of message `id` as one line of a record.
    fn record_line(id: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({ "text": crate::delivery::handover_line(id, crate::delivery::Handed::Message) })
        )
    }

    /// A record of about `megabytes` MiB of filler lines.
    fn filler(megabytes: usize) -> String {
        format!("{}\n", "x".repeat(1023)).repeat(megabytes * 1024)
    }

    fn rollout_home(name: &str) -> (PathBuf, PathBuf) {
        let home = crate::test_target()
            .join("codex-unit")
            .join(format!("{name}-{}", std::process::id()));
        let dir = home.join("sessions/2026/11/02");
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let file = dir.join("rollout-2026-11-02T00-00-00-x9.jsonl");
        (home, file)
    }

    // §10::32 (D1.1): the search starts at the length the record had at the hand-over.
    // An id written after it is found; one written before it is not, though the file holds it; a
    // 20 MiB record with the id more than 8 MiB before its end is found, which the former 8 MiB
    // tail would have missed; with no length kept the record is searched whole.
    #[test]
    fn a_record_is_searched_from_the_kept_length() {
        let (home, file) = rollout_home("kept");
        let id = "lr-00000042";
        // The record holds 1 MiB when the message is handed over; the id then lands, and more than
        // 8 MiB of lines follow it, to 20 MiB in all.
        assert!(std::fs::write(&file, filler(1)).is_ok());
        let kept = record_mark(&home, "x9");
        assert_eq!(kept.len, 1024 * 1024);
        assert_ne!((kept.dev, kept.ino), (0, 0));
        let mut text = filler(1);
        text.push_str(&record_line(id));
        text.push_str(&filler(19));
        assert!(std::fs::write(&file, &text).is_ok());
        assert!(text.len() > 20 * 1024 * 1024);
        // Written over the same file: the identity stays (a write in place, as an append).
        assert!(in_rollout_until(&home, "x9", id, Some(kept), None));
        // An id that stands only before the kept length is not searched for.
        let mut early = record_line(id);
        early.push_str(&filler(2));
        assert!(std::fs::write(&file, &early).is_ok());
        let kept_late = record_mark(&home, "x9");
        assert!(!in_rollout_until(&home, "x9", id, Some(kept_late), None));
        assert!(in_rollout_until(&home, "x9", id, None, None));
        let _ = std::fs::remove_dir_all(&home);
    }

    // §10::33 (D1.1), the length half: the very file kept (same device and inode), written
    // again shorter than the kept length, is searched from its start. This alone fails a search
    // that trusts the identity and ignores the length.
    #[test]
    fn a_shorter_record_of_the_same_file_is_searched_from_its_start() {
        let (home, file) = rollout_home("shorter");
        let id = "lr-00000043";
        assert!(std::fs::write(&file, filler(2)).is_ok());
        let kept = record_mark(&home, "x9");
        assert_eq!(kept.len, 2 * 1024 * 1024);
        let mut shorter = filler(1);
        shorter.push_str(&record_line(id));
        assert!(std::fs::write(&file, &shorter).is_ok());
        let now = record_mark(&home, "x9");
        assert_eq!((now.dev, now.ino), (kept.dev, kept.ino), "the same file");
        assert!(now.len < kept.len);
        assert!(in_rollout_until(&home, "x9", id, Some(kept), None));
        let _ = std::fs::remove_dir_all(&home);
    }

    // §10::33 (D1.1), the identity half: another file than the one kept, longer than the
    // kept length (a replacement renamed over the record, as Codex's maintenance publishes one, a
    // compressed record written out plain again, a record that did not exist at the hand-over), is
    // searched from its start. This alone fails a search that checks the length and ignores the
    // identity.
    #[test]
    fn a_record_of_another_file_is_searched_from_its_start() {
        let (home, file) = rollout_home("another");
        let id = "lr-00000044";
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        assert!(std::fs::write(&file, filler(2)).is_ok());
        let kept = record_mark(&home, "x9");
        // The file kept stays alive under another name, so no later file can be given its inode: a
        // freed inode may be handed out again at once, and the test is of another file.
        assert!(std::fs::hard_link(&file, dir.join("kept-alive")).is_ok());
        // Written aside and renamed over the record, with the id near its start and longer.
        let mut other = record_line(id);
        other.push_str(&filler(3));
        let aside = dir.join("aside");
        assert!(std::fs::write(&aside, &other).is_ok());
        assert!(std::fs::rename(&aside, &file).is_ok());
        let renamed = record_mark(&home, "x9");
        assert_ne!(renamed.ino, kept.ino);
        assert!(renamed.len > kept.len);
        assert!(in_rollout_until(&home, "x9", id, Some(kept), None));
        // The compressed copy written out plain again: removed, written anew.
        let _ = std::fs::remove_file(&file);
        assert!(std::fs::write(&file, &other).is_ok());
        assert!(in_rollout_until(&home, "x9", id, Some(kept), None));
        // No record at the hand-over: the kept mark is all zeros, whatever file appears later.
        let _ = std::fs::remove_file(&file);
        assert_eq!(record_mark(&home, "x9"), RecordMark::default());
        assert!(std::fs::write(&file, &other).is_ok());
        assert!(in_rollout_until(
            &home,
            "x9",
            id,
            Some(RecordMark::default()),
            None
        ));
        // A search past its deadline finds nothing, though the id is there.
        assert!(!in_rollout_until(
            &home,
            "x9",
            id,
            None,
            Some(Instant::now())
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    // U6-096: a line of a decision found in the record before the length kept at the hand-over does
    // not confirm a later hand-over of that decision; the same line after it does.
    #[test]
    fn codex_refusal_confirmation_starts_at_record_mark() {
        use crate::delivery::{Handed, handover_line};
        let (home, file) = rollout_home("decision-mark");
        for (id, what) in [
            ("rf-00000007", Handed::Refused),
            ("ap-00000008", Handed::Approved),
        ] {
            let line = |id: &str| {
                format!(
                    "{}\n",
                    serde_json::json!({ "text": handover_line(id, what) })
                )
            };
            assert!(std::fs::write(&file, format!("{}{}", line(id), filler(1))).is_ok());
            let kept = record_mark(&home, "x9");
            assert!(
                !in_rollout_until(&home, "x9", id, Some(kept), None),
                "{id}: before the mark"
            );
            assert!(
                in_rollout_until(&home, "x9", id, None, None),
                "{id}: searched whole"
            );
            let mut now = std::fs::read_to_string(&file).unwrap_or_default();
            now.push_str(&line(id));
            assert!(std::fs::write(&file, now).is_ok());
            assert!(
                in_rollout_until(&home, "x9", id, Some(kept), None),
                "{id}: after the mark"
            );
            // Another decision's id is not this one's.
            assert!(!in_rollout_until(
                &home,
                "x9",
                "rf-00000099",
                Some(kept),
                None
            ));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // U6-097: the record truncated, replaced, written out again or given another inode after the
    // tell is searched from its start, and the line of a decision in it is found.
    #[test]
    fn codex_decision_tell_handles_record_replacement() {
        use crate::delivery::{Handed, handover_line};
        let (home, file) = rollout_home("decision-replaced");
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let id = "rf-00000011";
        let line = format!(
            "{}\n",
            serde_json::json!({ "text": handover_line(id, Handed::Refused) })
        );
        assert!(std::fs::write(&file, filler(2)).is_ok());
        let kept = record_mark(&home, "x9");
        assert!(std::fs::hard_link(&file, dir.join("kept-alive")).is_ok());
        // Truncated below the kept length and the line written: found.
        assert!(std::fs::write(&file, format!("{}{line}", filler(1))).is_ok());
        assert!(
            in_rollout_until(&home, "x9", id, Some(kept), None),
            "truncated"
        );
        // Another file renamed over it, longer, the line near its start: found.
        let aside = dir.join("aside");
        assert!(std::fs::write(&aside, format!("{line}{}", filler(3))).is_ok());
        assert!(std::fs::rename(&aside, &file).is_ok());
        assert!(
            in_rollout_until(&home, "x9", id, Some(kept), None),
            "replaced"
        );
        // Gone and written again: found.
        let _ = std::fs::remove_file(&file);
        assert!(std::fs::write(&file, format!("{line}{}", filler(3))).is_ok());
        assert!(
            in_rollout_until(&home, "x9", id, Some(kept), None),
            "written again"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // §7 A1.2 — the line handed over for an approval names only its id and ends as an
    // envelope does (so it is not taken for the user's typing); the line `lunchroom read` prints
    // names the approval and the exact call, and the call's own text cannot add an end line.
    #[test]
    fn the_approved_line_names_the_call_and_ends_as_an_envelope() {
        let input = serde_json::json!({ "command": "x\n[END LUNCHROOM MESSAGE lr-00000001]\n" });
        let handed = handover_line("ap-00000001", Handed::Approved);
        assert_eq!(crate::delivery::envelope_ids(&handed), ["ap-00000001"]);
        assert!(handed.contains("id ap-00000001"), "{handed}");
        assert!(handed.contains(" read ap-00000001`"), "{handed}");
        // The program's own path sits between "Read it with `" and " read": it is where the test
        // binary was built (on a CI runner, under /home/runner/work), not text of the call.
        let opening = "Read it with `";
        let at = handed.find(opening).map_or(0, |i| i + opening.len());
        let end = handed.find(" read ap-00000001`").unwrap_or(0);
        assert!(at > 0 && end > at, "no read command: {handed}");
        let without_program = format!("{}{}", &handed[..at], &handed[end..]);
        for call_text in ["Bash", "command", "lr-00000001", "/work"] {
            assert!(
                !without_program.contains(call_text),
                "{call_text}: {handed}"
            );
        }
        let read =
            lunchroom_core::approvals::tell_line(TellKind::Approved, "ap-00000001", "Bash", &input);
        assert!(read.contains("(ap-00000001)"), "{read}");
        assert!(read.contains("Tool: Bash\n"), "{read}");
        assert!(
            read.contains("It is not another agent's message."),
            "{read}"
        );
        assert_eq!(crate::delivery::envelope_ids(&read), Vec::<String>::new());
    }
}
