//! No message waits without an end (spec §10 D1.7, D1.8).
//!
//! D1.7 (docs/spec/lunchroom-spec.md §10 item 7): "Lunchroom checks every QUEUED message's time
//! limits whenever any of its programs runs: every guard hook in any session, every listener, every
//! `lunchroom send` and `lunchroom status`. At each check it looks again for the recipient's
//! confirmation (D1.1), counts a hand-over past its time as a failed attempt (D1.4) and offers that
//! message again at once, through the recipient harness's own program as D1.1 says, whatever
//! session that check runs in. A message to an agent that is no longer named, or no longer takes
//! messages after `leave`, is FAILED at once with that reason."
//!
//! `lunchroom send` and `lunchroom status` run the whole check in their own process. A guard hook
//! answers within its deadline (H1.5) and a listener within its hook's time limit, and a hand-over
//! may take a minute, so a guard hook starts the check as `lunchroom check` in a process of its
//! own, which it does not wait for, and a listener runs every part but the offers again in its own
//! process and starts `lunchroom check` for those. A check is started only when a message is
//! QUEUED; one waiting to run is enough, so none is started while one waits. Checks run one at a
//! time, each waiting for the one before. Each started check leaves a marker in
//! `<store>/checks/` from its start until it ends, so anyone (a test) can wait until none runs; the
//! marker holds the check's process (its pid and start time), so the marker of a check that was
//! killed is known for what it is and does not hold up the next one.
//! `lunchroom doctor` runs no check: it changes nothing (C1.4).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lunchroom_core::cards;
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::Mail;
use lunchroom_core::store::Store;

use crate::codex::{Delivered, FailedAs};
use crate::evidence::{Assessment, assess};
use crate::home::Env;

/// The time now, read from the clock at the moment it is needed: the later of the time this process
/// started with and the clock now. A process that waited for the store's lock, or for a hand-over,
/// would otherwise act on a time from before the wait and count a newer hand-over as stamped in the
/// future.
#[must_use]
pub(crate) fn clock_now(env: &Env) -> u64 {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    env.now.max(wall)
}

/// Whose messages a check may offer again (D1.1): a check that runs in an agent's process must not
/// put another sender's text in front of that agent, since the text is a command argument that
/// `ps` shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offers {
    /// Any sender's: a check a guard hook started (`lunchroom check`), a listener's, and
    /// `lunchroom status` run by the user.
    Any,
    /// Only this sender's own: the check inside `lunchroom send`.
    From(Identity),
    /// Nobody's: `lunchroom status` run by an agent.
    Nobody,
}

impl Offers {
    /// What `lunchroom status` may offer: nothing when an agent runs it, any sender's when the
    /// user does.
    #[must_use]
    pub fn for_status(run_by_agent: bool) -> Self {
        if run_by_agent {
            Self::Nobody
        } else {
            Self::Any
        }
    }

    /// The sender a hand-over is limited to, if it is.
    fn only_from(&self) -> Option<&Identity> {
        match self {
            Self::From(sender) => Some(sender),
            Self::Any | Self::Nobody => None,
        }
    }

    /// Whether any message is offered again.
    fn offers(&self) -> bool {
        *self != Self::Nobody
    }
}

/// D1.7: one check, run in this process: confirmations looked for again, hand-overs past their
/// time counted, messages to agents no longer named FAILED, why a message cannot be handed over
/// noted, what has waited long enough told (D1.8), and each message whose hand-over counted as
/// failed offered again at once, to whom `offers` allows (D1.1).
///
/// # Errors
/// When the store cannot be used.
pub fn run(env: &Env, store: &Store<Mail>, offers: &Offers) -> Result<(), String> {
    run_with(env, store, offers, None)
}

/// For a listener, which answers within its hook's time limit: [`run`] without the offers again,
/// each of which may take a minute, and with nothing read past `deadline`; a `lunchroom check`
/// started in a process of its own makes the offers ([`start`]).
///
/// # Errors
/// When the store cannot be used.
pub fn run_in_listener(
    env: &Env,
    store: &Store<Mail>,
    deadline: Option<Instant>,
) -> Result<(), String> {
    run_with(env, store, &Offers::Nobody, deadline)?;
    start(env);
    Ok(())
}

/// [`run`], making the offers again `offers` allows, and reading nothing past `deadline`.
fn run_with(
    env: &Env,
    store: &Store<Mail>,
    offers: &Offers,
    deadline: Option<Instant>,
) -> Result<(), String> {
    crate::codex::confirm_recorded(env, store, deadline)?;
    // What each recipient of a waiting message shows of its guard is judged before the store's
    // lock is taken, once for each recipient: the judgment reads files and processes, and the lock
    // holds every other Lunchroom program up while it is held.
    let judged = judge_recipients(env, store)?;
    let again = store
        .update(|m| {
            // The clock is read under the lock: this process may have waited for it.
            let now = clock_now(env);
            m.expire_handovers(now);
            m.fail_unnamed(now);
            note_unreachable(m, &judged);
            m.tell_waiting(now);
            if offers.offers() {
                m.to_offer_again(now)
            } else {
                Vec::new()
            }
        })
        .map_err(crate::cli::unwritten)?;
    for to in again {
        let handed = match to.harness {
            Harness::Codex => crate::codex::deliver_again(env, store, &to, offers.only_from()),
            Harness::OpenClaw => {
                crate::openclaw::deliver_again(env, store, &to, offers.only_from())
            }
            Harness::ClaudeCode | Harness::Hermes => continue,
        };
        after_hand_over(store, &handed)?;
    }
    // What the hand-overs above found, told now.
    store
        .update(|m| m.tell_waiting(clock_now(env)))
        .map_err(crate::cli::unwritten)
}

/// What the evidence shows of each distinct recipient of a QUEUED message not in front of its
/// harness, read from the store without its lock.
fn judge_recipients(
    env: &Env,
    store: &Store<Mail>,
) -> Result<BTreeMap<Identity, Assessment>, String> {
    let mail = store.read().map_err(crate::cli::unread)?;
    let mut judged = BTreeMap::new();
    for d in mail
        .queued()
        .into_iter()
        .filter(|d| d.presented_at.is_none())
    {
        if !judged.contains_key(&d.to) {
            judged.insert(d.to.clone(), assess(&mail, &d.to, &env.home));
        }
    }
    Ok(judged)
}

/// D1.8: what a hand-over found, kept for what is told and shown: a program that cannot be
/// established is why the message cannot be handed over; a hand-over that went ahead, or that was
/// tried and failed (so its program was established), clears it. A counted failure is told by the
/// store as it is counted.
///
/// # Errors
/// When the store cannot be used.
pub fn after_hand_over(
    store: &Store<Mail>,
    handed: &Result<Delivered, String>,
) -> Result<(), String> {
    let note = match handed {
        Ok(Delivered::Failed(id, reason, FailedAs::NotAttempted)) => {
            Some((id.clone(), Some(program_reason(reason))))
        }
        Ok(
            Delivered::Failed(id, _, FailedAs::Counted)
            | Delivered::Unconfirmed(id)
            | Delivered::Confirmed(id),
        ) => Some((id.clone(), None)),
        Ok(Delivered::Failed(_, _, FailedAs::NowFailed) | Delivered::Nothing) | Err(_) => None,
    };
    match note {
        Some((id, reason)) => store
            .update(|m| m.note_blocked(&id, reason))
            .map_err(crate::cli::unwritten),
        None => Ok(()),
    }
}

/// D1.8: for each QUEUED message not in front of its recipient's harness, why it cannot be handed
/// over from the recipient's state, as `judged` has it: UNGUARDED, with its R3.4 reason, or
/// STOPPED; nothing while the recipient is guarded. A recipient `judged` does not have (a message
/// that came after the judgment) is left for the next check.
fn note_unreachable(m: &mut Mail, judged: &BTreeMap<Identity, Assessment>) {
    let waiting: Vec<(String, Identity, String)> = m
        .queued()
        .into_iter()
        .filter(|d| d.presented_at.is_none())
        .map(|d| (d.id.clone(), d.to.clone(), d.to_name.clone()))
        .collect();
    for (id, to, name) in waiting {
        let Some(assessment) = judged.get(&to) else {
            continue;
        };
        let reason = match assessment {
            Assessment::Guarded => None,
            Assessment::Unguarded(why) => Some(unguarded_reason(&name, why)),
            Assessment::NotRunning => Some(stopped_reason(&name)),
        };
        m.note_unreachable(&id, reason);
    }
}

/// D1.8: why a message cannot be handed over when its recipient `name` is UNGUARDED: the card and
/// its R3.4 reason, which says what the user does. The text is told to the sender, an agent, so it
/// is the reason without another session's configuration paths (G1-11); `lunchroom doctor` has the
/// whole reason for the user.
pub(crate) fn unguarded_reason(name: &str, why: &crate::evidence::Why) -> String {
    format!("{} {}", cards::unguarded(name), why.text_for_agent())
}

/// D1.8: why a message cannot be handed over when its recipient `name` is STOPPED, and what fixes
/// it. The way to end the wait without fixing it is told with every reason, once, by the store.
pub(crate) fn stopped_reason(name: &str) -> String {
    format!(
        "{} Its session is not running, or Lunchroom cannot reach it: start it again.",
        cards::stopped(name)
    )
}

/// D1.8: why a message cannot be handed over when its recipient's program cannot be established
/// (D1.1), for `reason`, and what the user does.
pub(crate) fn program_reason(reason: &str) -> String {
    format!(
        "Lunchroom cannot find the program that runs that session ({reason}). The user starts \
         that session again, so Lunchroom can find the program it runs."
    )
}

/// Where started checks leave their markers: `<store>/checks/<name>.waiting` until the check runs,
/// `<name>.running` while it runs; none once it has ended.
fn markers(env: &Env) -> PathBuf {
    env.store_dir().join("checks")
}

/// A marker older than this is left from a check that did not end (killed, or a crash), and no
/// longer counts, whatever it holds.
const STALE: Duration = Duration::from_mins(15);

/// A marker that holds nothing yet is a check that was started and has not begun to run: it counts
/// for this long, and past it the check never began.
const STARTING: Duration = Duration::from_secs(30);

/// `lunchroom check <name>`: one check of D1.7, started by [`start`] with the name of its marker.
/// It writes its own process into its marker, waits for the check before it, if one runs, then
/// runs; its marker goes when it ends. It prints nothing.
#[must_use]
pub fn check(env: &Env, args: &[String]) -> crate::cli::Outcome {
    let ok = crate::cli::Outcome::ok(String::new());
    // Only a name `start` gives: no folder parts.
    let name = args
        .first()
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    let waiting = name.map(|n| markers(env).join(format!("{n}.waiting")));
    let running = name.map(|n| markers(env).join(format!("{n}.running")));
    let done = || {
        for marker in [&waiting, &running].into_iter().flatten() {
            let _ = std::fs::remove_file(marker);
        }
    };
    // Its marker names this process, so a check that is killed does not hold up the next ones.
    if let (Some(w), Some(me)) = (&waiting, crate::process::key(std::process::id())) {
        write_marker(w, &me);
    }
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        done();
        return ok;
    };
    let lock_path = env.store_dir().join("check.flock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path);
    lunchroom_core::setgid::settle(&lock_path);
    // One at a time: this one waits for the one before, which may be making an offer.
    if let Ok(lock) = lock
        && lock.lock().is_ok()
    {
        if let (Some(w), Some(r)) = (&waiting, &running) {
            let _ = std::fs::rename(w, r);
        }
        // Started by a guard hook, in a process of its own: any sender's message may be offered.
        let _ = run(env, &store, &Offers::Any);
    }
    done();
    ok
}

/// Puts `key` (a process, `<pid>:<start time>`) in the marker `path`, which exists: a marker that
/// has been removed meanwhile (the check was judged never to have begun) is not made again.
fn write_marker(path: &Path, key: &str) {
    if let Ok(mut file) = OpenOptions::new().write(true).truncate(true).open(path) {
        let _ = file.write_all(key.as_bytes());
    }
}

/// Whether the marker `path` stands for a check that can no longer run: older than [`STALE`]; one
/// that names a process that is gone (or one that is not the process it names, its pid reused); or
/// one that names none and is older than [`STARTING`].
fn marker_stale(path: &Path) -> bool {
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());
    if age.is_some_and(|age| age > STALE) {
        return true;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let key = text.trim();
    if key.is_empty() {
        return age.is_some_and(|age| age > STARTING);
    }
    let pid = key.split(':').next().and_then(|p| p.parse::<u32>().ok());
    pid.is_none_or(|pid| crate::process::key(pid).as_deref() != Some(key))
}

/// Removes the markers in `dir` that stand for a check that can no longer run, and says whether a
/// check that has not yet begun still waits there.
fn clear_stale_markers(dir: &Path) -> bool {
    let mut one_waits = false;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if marker_stale(&entry.path()) {
            let _ = std::fs::remove_file(entry.path());
        } else if entry.file_name().to_string_lossy().ends_with(".waiting") {
            one_waits = true;
        }
    }
    one_waits
}

/// The file that holds the stamp of the state file as it was when it was last found to hold no
/// QUEUED message, so a hook that finds the state file unchanged reads and parses nothing.
const NONE_QUEUED: &str = "none-queued.stamp";

/// Whether a message is QUEUED in `store`. The state file is read and parsed only when it is not
/// the one last found to hold none, whose stamp (`idle`) the file `idle` keeps: every guard hook
/// asks, and the state file is the size of everything Lunchroom keeps.
fn queued_message_waits(store: &Store<Mail>, idle: &Path) -> bool {
    // The stamp is taken before the state is read: a state written between the two leaves a stamp
    // that is not the state's, so the next hook reads again.
    let stamp = format!("{:?}", store.stamp());
    if std::fs::read_to_string(idle).is_ok_and(|kept| kept == stamp) {
        return false;
    }
    let waits = store.read().is_ok_and(|m| !m.queued().is_empty());
    if !waits {
        let _ = std::fs::write(idle, &stamp);
        lunchroom_core::setgid::settle(idle);
    }
    waits
}

/// D1.7 for a guard hook or a listener: starts `lunchroom check` in a process of its own, not waited
/// for, when a message is QUEUED and no check started before waits to run (that one reads the
/// records as they are when it runs). Its marker is made before it starts, so a check started by a
/// hook that has ended is always seen. Nothing is started when the store cannot be read or holds no
/// QUEUED message.
pub fn start(env: &Env) {
    // H1.7: the step named if the guard's time runs out here.
    let _step = lunchroom_core::steps::begin("starting a check of the QUEUED messages");
    let Ok(store) = Store::<Mail>::open(&env.store_dir()) else {
        return;
    };
    if !queued_message_waits(&store, &env.store_dir().join(NONE_QUEUED)) {
        return;
    }
    let dir = markers(env);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    lunchroom_core::setgid::settle(&dir);
    if clear_stale_markers(&dir) {
        return;
    }
    let Ok(program) = std::env::current_exe() else {
        return;
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!("{stamp}-{}", std::process::id());
    let marker = dir.join(format!("{name}.waiting"));
    if std::fs::write(&marker, b"").is_err() {
        return;
    }
    lunchroom_core::setgid::settle(&marker);
    // Its own process group: the harness ending the hook's group does not end the check.
    let started = Command::new(program)
        .args(["check", name.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if started.is_err() {
        let _ = std::fs::remove_file(&marker);
    }
}

#[cfg(test)]
mod tests {
    use lunchroom_core::mail::Origin;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::test_target()
            .join("checks-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        dir
    }

    fn claude() -> Identity {
        Identity::new(Harness::ClaudeCode, "c1")
    }

    fn codex() -> Identity {
        Identity::new(Harness::Codex, "x1")
    }

    /// A store in `dir` with a message from claude to codex, QUEUED, and its id.
    fn queued(dir: &Path) -> Option<(Store<Mail>, String)> {
        let store = Store::<Mail>::open(dir).ok()?;
        let id = store
            .update(|m| {
                for (name, who) in [("claude", claude()), ("codex", codex())] {
                    m.observe(&who);
                    let _ = m.register(name, &who, Origin::User, None);
                }
                m.send(&claude(), "claude", "codex", "hi", 1000)
                    .map(|a| a.id)
                    .unwrap_or_default()
            })
            .ok()?;
        Some((store, id))
    }

    fn blocked(store: &Store<Mail>, id: &str) -> Option<String> {
        store
            .read()
            .ok()
            .and_then(|m| m.message(id).and_then(|d| d.told.blocked.clone()))
    }

    // G2-2: each way a hand-over can come out is kept for what it is. A program that could not be
    // established is why the message cannot be handed over; a failure that was counted (so the
    // program ran) is not that, and clears it; a hand-over that went ahead clears it; a message
    // that is FAILED, nothing handed over, and a store error leave it as it was. Before the fix
    // every failure that was not the message's last was taken for "the program cannot be
    // established", and the user was told to start the session anew.
    #[test]
    fn each_way_a_hand_over_comes_out_is_kept_for_what_it_is() {
        let dir = scratch("after");
        let made = queued(&dir.join("store"));
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        let missing = Ok(Delivered::Failed(
            id.clone(),
            "the recipient's codex program was not found".to_owned(),
            FailedAs::NotAttempted,
        ));
        assert!(after_hand_over(&store, &missing).is_ok());
        assert_eq!(
            blocked(&store, &id).as_deref(),
            Some(
                "Lunchroom cannot find the program that runs that session (the recipient's codex \
                 program was not found). The user starts that session again, so Lunchroom can \
                 find the program it runs."
            )
        );
        // Unchanged by what says nothing about the program.
        let counted_final = Ok(Delivered::Failed(
            id.clone(),
            "x".to_owned(),
            FailedAs::NowFailed,
        ));
        for same in [
            counted_final,
            Ok(Delivered::Nothing),
            Err("Lunchroom's store cannot be updated".to_owned()),
        ] {
            assert!(after_hand_over(&store, &same).is_ok());
            assert!(blocked(&store, &id).is_some(), "{same:?}");
        }
        // Cleared by a failure that was counted, and by every hand-over that went ahead.
        let clearing = [
            Ok(Delivered::Failed(
                id.clone(),
                "codex queue exited 1".to_owned(),
                FailedAs::Counted,
            )),
            Ok(Delivered::Unconfirmed(id.clone())),
            Ok(Delivered::Confirmed(id.clone())),
        ];
        for how in clearing {
            assert!(after_hand_over(&store, &missing).is_ok());
            assert!(blocked(&store, &id).is_some());
            assert!(after_hand_over(&store, &how).is_ok());
            assert_eq!(blocked(&store, &id), None, "{how:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // G2-3: the clock is read when it is needed, not when the process started: a later reading of
    // the wall clock wins, and never an earlier one than the process started with.
    #[test]
    fn the_clock_is_read_when_it_is_needed() {
        let mut env = Env {
            home: PathBuf::from("/home/u"),
            lunchroom_home: PathBuf::from("/home/u/.lunchroom"),
            claude_pid: None,
            attended: None,
            entrypoint: None,
            claude_session: None,
            codex_thread: None,
            codex_home: PathBuf::from("/home/u/.codex"),
            claude_home: PathBuf::from("/home/u/.claude"),
            hermes_home: PathBuf::from("/home/u/.hermes"),
            hermes_session: None,
            lunchroom_session: None,
            now: 5,
        };
        // A process that started long ago and waited: it reads the clock now.
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        assert!(clock_now(&env) >= wall);
        // A clock that stepped back never takes the process's own time backwards.
        env.now = wall + 10_000;
        assert_eq!(clock_now(&env), wall + 10_000);
    }

    // G2-22: the evidence of a recipient is judged once for each distinct recipient, from a plain
    // read of the store (no lock held), and a recipient it did not judge keeps its reason.
    #[test]
    fn a_recipient_is_judged_once_outside_the_lock() {
        let dir = scratch("judged");
        let made = queued(&dir.join("store"));
        assert!(made.is_some());
        let Some((store, id)) = made else { return };
        // A second message to the same recipient, and one to another.
        let sent = store.update(|m| {
            let third = Identity::new(Harness::Codex, "x2");
            m.observe(&third);
            let _ = m.register("other", &third, Origin::User, None);
            (
                m.send(&claude(), "claude", "codex", "again", 1000).is_ok(),
                m.send(&claude(), "claude", "other", "hello", 1000).is_ok(),
            )
        });
        assert_eq!(sent.ok(), Some((true, true)));
        let env = Env {
            home: dir.join("home"),
            lunchroom_home: dir.clone(),
            claude_pid: None,
            attended: None,
            entrypoint: None,
            claude_session: None,
            codex_thread: None,
            codex_home: dir.join("home/.codex"),
            claude_home: dir.join("home/.claude"),
            hermes_home: dir.join("home/.hermes"),
            hermes_session: None,
            lunchroom_session: None,
            now: 1000,
        };
        let judged = judge_recipients(&env, &store);
        assert!(judged.is_ok());
        let judged = judged.unwrap_or_default();
        assert_eq!(
            judged.keys().cloned().collect::<Vec<_>>(),
            vec![codex(), Identity::new(Harness::Codex, "x2")]
        );
        // The reasons are noted from the judgment, with the lock held only for noting them; a
        // recipient the judgment lacks is left alone.
        let mut some = judged.clone();
        some.remove(&Identity::new(Harness::Codex, "x2"));
        let reasons = store.update(|m| {
            note_unreachable(m, &some);
            m.queued()
                .into_iter()
                .map(|d| (d.to_name.clone(), d.told.unreachable.is_some()))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            reasons.ok(),
            Some(vec![
                ("codex".to_owned(), true),
                ("codex".to_owned(), true),
                ("other".to_owned(), false),
            ])
        );
        let _ = id;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sets the modified time of `path` to `age` ago.
    fn age_by(path: &Path, age: Duration) {
        let file = OpenOptions::new().write(true).open(path);
        assert!(file.is_ok());
        if let Ok(file) = file {
            assert!(file.set_modified(SystemTime::now() - age).is_ok());
        }
    }

    // G2-17: the marker of a check that was killed (it names a process that is gone), one that
    // began and never named itself, and one too old are cleared and hold nothing up; the marker of
    // a check that is running (it names a live process, here this one), and one that has only just
    // been made, still count. Before the fix a killed check's marker blocked every check for 15
    // minutes.
    #[test]
    fn the_marker_of_a_check_that_was_killed_holds_nothing_up() {
        let dir = scratch("markers");
        let me = crate::process::key(std::process::id()).unwrap_or_default();
        assert_ne!(me, "");
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            assert!(std::fs::write(&path, text).is_ok());
            path
        };
        // A process that is gone: a pid no system gives out.
        let dead = write("dead.waiting", "4294967294:1");
        let reused = write("reused.waiting", &format!("{}:1", std::process::id()));
        let unnamed_old = write("unnamed.waiting", "");
        age_by(&unnamed_old, STARTING + Duration::from_secs(5));
        let too_old = write("old.waiting", &me);
        age_by(&too_old, STALE + Duration::from_secs(5));
        let dead_running = write("dead.running", "4294967294:1");
        assert!(!clear_stale_markers(&dir));
        for gone in [&dead, &reused, &unnamed_old, &too_old, &dead_running] {
            assert!(!gone.exists(), "{gone:?}");
        }
        // Those that count.
        let alive = write("alive.waiting", &me);
        assert!(clear_stale_markers(&dir));
        assert!(alive.exists());
        let _ = std::fs::remove_file(&alive);
        let starting = write("starting.waiting", "");
        assert!(clear_stale_markers(&dir));
        assert!(starting.exists());
        // A running check does not make another wait.
        let _ = std::fs::remove_file(&starting);
        let running = write("alive.running", &me);
        assert!(!clear_stale_markers(&dir));
        assert!(running.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // G3-12 (the marker G3 asked for): a hook asks whether a message is QUEUED; a state file found
    // to hold none is remembered by its stamp, and while the stamp is the same nothing is read or
    // parsed. The kept stamp is trusted (shown here by a state that does hold one under its stamp);
    // a changed state is read again, and one that holds a message is never remembered as idle.
    #[test]
    fn a_state_found_to_hold_no_queued_message_is_not_parsed_again() {
        let dir = scratch("idle");
        let store = Store::<Mail>::open(&dir.join("store"));
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        let idle = dir.join("none-queued.stamp");
        assert!(store.update(|m| m.observe(&claude())).is_ok());
        assert!(!queued_message_waits(&store, &idle));
        assert!(idle.exists());
        // The state changes: a message is QUEUED. Read again, it is found.
        let queued = store.update(|m| {
            for (name, who) in [("claude", claude()), ("codex", codex())] {
                m.observe(&who);
                let _ = m.register(name, &who, Origin::User, None);
            }
            m.send(&claude(), "claude", "codex", "hi", 1000).is_ok()
        });
        assert_eq!(queued.ok(), Some(true));
        assert!(queued_message_waits(&store, &idle));
        let kept = std::fs::read_to_string(&idle).unwrap_or_default();
        // Not remembered as idle: the stamp kept is still the older one.
        assert_ne!(kept, format!("{:?}", store.stamp()));
        // A stamp kept for this very state is trusted without reading it.
        assert!(std::fs::write(&idle, format!("{:?}", store.stamp())).is_ok());
        assert!(!queued_message_waits(&store, &idle));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // DV-03 (D1.1): who a check may offer: inside `send`, only the sender's own; `status` run by an
    // agent, nobody; `status` run by the user, a hook-started check and the listeners, any.
    #[test]
    fn a_check_offers_only_whom_its_process_may_hand_over_to() {
        let sender = claude();
        let inside_send = Offers::From(sender.clone());
        assert_eq!(inside_send.only_from(), Some(&sender));
        assert!(inside_send.offers());
        let by_agent = Offers::for_status(true);
        assert_eq!(by_agent, Offers::Nobody);
        assert!(!by_agent.offers());
        let by_user = Offers::for_status(false);
        assert_eq!(by_user, Offers::Any);
        assert!(by_user.offers());
        assert_eq!(by_user.only_from(), None);
        assert_eq!(by_agent.only_from(), None);
    }

    // G2-17: a check names its own process in its marker, and does not make a marker that was
    // removed meanwhile.
    #[test]
    fn a_check_names_its_process_in_its_marker() {
        let dir = scratch("named");
        let marker = dir.join("m.waiting");
        assert!(std::fs::write(&marker, b"").is_ok());
        let me = crate::process::key(std::process::id()).unwrap_or_default();
        write_marker(&marker, &me);
        assert_eq!(
            std::fs::read_to_string(&marker).ok().as_deref(),
            Some(me.as_str())
        );
        assert!(!marker_stale(&marker));
        let gone = dir.join("gone.waiting");
        write_marker(&gone, &me);
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
