//! The guard's records of calls it could not decide, and of slow decisions (spec §13 H1.8).
//!
//! H1.8 (docs/spec/lunchroom-spec.md §13 item 8): "The guard records each call it cannot decide,
//! and each decision that takes longer than 2 seconds, in a file in Lunchroom's own folder: the
//! time, the session, the call as V1.4 shows it, the slowest step and how long it took. It keeps
//! the latest 200 records and sends nothing anywhere. `lunchroom doctor` shows the records of the
//! last 24 hours (C1.4)."
//!
//! The file is `<LUNCHROOM_HOME>/guard-records.jsonl`, one JSON record a line, oldest first, readable
//! by its owner alone. It is written whole to a temporary file and renamed over the old one, under
//! a lock of its own.
//!
//! A record is never dropped. A record is cleaned before it is written, and again when it is read:
//! control characters are removed (the call is text an agent chose, and doctor prints it on the
//! user's screen, where an escape sequence could redraw it), and every secret in it is replaced
//! ([`lunchroom_core::secrets::redact`]). A guard that cannot take the lock within [`LOCK_WAIT`]
//! does not wait past its time and does not leave the record out: it writes the record to a file
//! of its own in `guard-records.d/`, and the next guard that takes the lock folds those files into
//! the records file. [`read`] shows them meanwhile.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The records kept: the latest this many.
pub const KEEP: usize = 200;

/// A decision that took longer than this is recorded.
pub const SLOW: Duration = Duration::from_secs(2);

/// What doctor shows: the records of the last this many seconds.
pub const SHOWN_SECS: u64 = 24 * 3_600;

/// How long a guard waits for the lock before it writes its record to a file of its own.
pub const LOCK_WAIT: Duration = Duration::from_secs(1);

/// How often a waiting guard tries the lock again.
const POLL: Duration = Duration::from_millis(10);

/// One record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When the hook ran, in seconds since the Unix epoch.
    pub at: u64,
    /// The session, as the hook named it (its harness's session id).
    pub session: String,
    /// The hook (`pretool`, `prompt`, `posttool`, `start`).
    pub hook: String,
    /// The call, as V1.4 shows it.
    pub call: String,
    /// The slowest step, and how long it took, in milliseconds.
    pub step: String,
    pub step_ms: u64,
    /// How long the whole decision took, in milliseconds.
    pub took_ms: u64,
    /// Why the guard could not decide the call; `None` for a decision that was only slow.
    pub undecided: Option<String>,
}

/// The records file in Lunchroom's folder `lunchroom_home`.
#[must_use]
pub fn file(lunchroom_home: &Path) -> PathBuf {
    lunchroom_home.join("guard-records.jsonl")
}

/// The folder of records written while the lock was held by another guard.
fn spill_dir(lunchroom_home: &Path) -> PathBuf {
    lunchroom_home.join("guard-records.d")
}

/// Characters that change what a terminal shows without being shown themselves: the right-to-left
/// and left-to-right marks and overrides and isolates, which `char::is_control` does not name.
fn changes_direction(c: char) -> bool {
    matches!(c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `text` as safe to print: a line break or tab becomes one space, every other control character
/// (an escape, a bell, a carriage return that redraws the line) and every direction mark is
/// removed.
#[must_use]
pub fn clean(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() || changes_direction(c) => None,
            c => Some(c),
        })
        .collect()
}

/// `text` cleaned, with its secrets replaced.
fn safe(text: &str) -> String {
    lunchroom_core::secrets::redact(&clean(text))
}

/// `record` with every text in it made safe to keep and to print.
fn sanitized(record: &Record) -> Record {
    Record {
        at: record.at,
        session: safe(&record.session),
        hook: safe(&record.hook),
        call: safe(&record.call),
        step: safe(&record.step),
        step_ms: record.step_ms,
        took_ms: record.took_ms,
        undecided: record.undecided.as_deref().map(safe),
    }
}

/// Oldest first by time (records of one time keep their order), each once.
fn merged(mut all: Vec<Record>) -> Vec<Record> {
    all.sort_by_key(|r| r.at);
    let mut out: Vec<Record> = Vec::with_capacity(all.len());
    for r in all {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// The records in the file, cleaned; a line that is not a record is passed over.
fn read_file(lunchroom_home: &Path) -> Vec<Record> {
    std::fs::read_to_string(file(lunchroom_home))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .map(|r| sanitized(&r))
        .collect()
}

/// The records waiting in `guard-records.d/`, oldest name first, each with the file it is in.
fn read_spilled(lunchroom_home: &Path) -> Vec<(PathBuf, Record)> {
    let Ok(entries) = std::fs::read_dir(spill_dir(lunchroom_home)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension() == Some("json".as_ref()))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let record = serde_json::from_str::<Record>(&std::fs::read_to_string(&p).ok()?).ok()?;
            Some((p, sanitized(&record)))
        })
        .collect()
}

/// Every record in `lunchroom_home`, oldest first, cleaned; none when there is no file. A line
/// that is not a record is passed over. Reads only.
#[must_use]
pub fn read(lunchroom_home: &Path) -> Vec<Record> {
    let mut all = read_file(lunchroom_home);
    all.extend(read_spilled(lunchroom_home).into_iter().map(|(_, r)| r));
    merged(all)
}

/// Writes `text` to `path`, readable by its owner alone, even where a file of that name already
/// existed with wider access.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    // In setgid mode (1.1) the group `lunchroom` reads and writes it too: the console does.
    lunchroom_core::setgid::settle(path);
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Tries the lock until `wait` has passed (once at least).
fn take(lock: &std::fs::File, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    loop {
        if lock.try_lock().is_ok() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Writes `record` to a file of its own in `guard-records.d/`, whole or not at all.
fn spill(lunchroom_home: &Path, record: &Record) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = spill_dir(lunchroom_home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    lunchroom_core::setgid::settle(&dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let name = format!("{:020}-{}-{nanos:09}", record.at, std::process::id());
    let temp = dir.join(format!("{name}.tmp"));
    write_private(
        &temp,
        &serde_json::to_string(record).map_err(|e| e.to_string())?,
    )?;
    std::fs::rename(&temp, dir.join(format!("{name}.json"))).map_err(|e| e.to_string())
}

/// Adds `record` in `lunchroom_home`, keeping the latest [`KEEP`]. Sends nothing anywhere. The
/// record is never dropped for a busy lock (see the module's text).
///
/// # Errors
/// Why the record could not be written at all: the folder cannot be written to.
pub fn add(lunchroom_home: &Path, record: &Record) -> Result<(), String> {
    add_waiting(lunchroom_home, record, LOCK_WAIT)
}

/// [`add`], waiting `wait` for the lock.
fn add_waiting(lunchroom_home: &Path, record: &Record, wait: Duration) -> Result<(), String> {
    let record = sanitized(record);
    std::fs::create_dir_all(lunchroom_home).map_err(|e| e.to_string())?;
    let lock_path = lunchroom_home.join("guard-records.flock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| e.to_string())?;
    lunchroom_core::setgid::settle(&lock_path);
    if !take(&lock, wait) {
        return spill(lunchroom_home, &record);
    }
    let spilled = read_spilled(lunchroom_home);
    let mut all = read_file(lunchroom_home);
    all.extend(spilled.iter().map(|(_, r)| r.clone()));
    all.push(record);
    // H1.8: the latest KEEP of the guard's undecided and slow records. With the console (1.1), and,
    // apart, the latest KEEP_DENIED DENIED ones: a burst of DENIED calls never pushes out an
    // undecided call.
    #[cfg(feature = "console")]
    let mut all: Vec<Record> = {
        let (denied, other): (Vec<Record>, Vec<Record>) =
            merged(all).into_iter().partition(|r| r.hook == DENIED_HOOK);
        other
            .into_iter()
            .rev()
            .take(KEEP)
            .chain(denied.into_iter().rev().take(KEEP_DENIED))
            .collect()
    };
    #[cfg(not(feature = "console"))]
    let mut all: Vec<Record> = merged(all).into_iter().rev().take(KEEP).collect();
    all.sort_by_key(|r| r.at);
    let mut text = String::new();
    for r in &all {
        text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let temp = lunchroom_home.join("guard-records.jsonl.tmp");
    write_private(&temp, &text)?;
    std::fs::rename(&temp, file(lunchroom_home)).map_err(|e| e.to_string())?;
    // The records are in the file: the files they came from are no longer needed.
    for (path, _) in spilled {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// The records of the [`SHOWN_SECS`] up to `now`: a record at exactly that age is one of them, an
/// older one is not.
#[must_use]
pub fn recent(records: &[Record], now: u64) -> Vec<&Record> {
    let shown = records
        .iter()
        .filter(|r| r.at <= now && now - r.at <= SHOWN_SECS);
    // The records of DENIED decisions (1.1) are not guard records and are not shown here.
    #[cfg(feature = "console")]
    let shown = shown.filter(|r| r.hook != DENIED_HOOK);
    shown.collect()
}

/// The `hook` of a record of a DENIED decision (1.1). Such a record holds the time, the session and
/// the reason word (`step`) and no command text; `step_ms` and `took_ms` only keep two DENIED
/// records of one second apart.
#[cfg(feature = "console")]
pub const DENIED_HOOK: &str = "denied";

/// The DENIED records kept, apart from the [`KEEP`] others.
#[cfg(feature = "console")]
pub const KEEP_DENIED: usize = 1_000;

/// The window of a probing alert, in seconds.
#[cfg(feature = "console")]
pub const PROBE_SECS: u64 = 600;

/// DENIED decisions from one agent in [`PROBE_SECS`] that make a probing alert.
#[cfg(feature = "console")]
pub const PROBE_COUNT: usize = 5;

/// Records one DENIED decision of `session` for `reason` (the reason's word) at `at`. Sends
/// nothing anywhere.
///
/// # Errors
/// Why the record could not be written at all.
#[cfg(feature = "console")]
pub fn add_denied(
    lunchroom_home: &Path,
    at: u64,
    session: &str,
    reason: &str,
) -> Result<(), String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    add(
        lunchroom_home,
        &Record {
            at,
            session: session.to_owned(),
            hook: DENIED_HOOK.to_owned(),
            call: String::new(),
            step: reason.to_owned(),
            step_ms: nanos,
            took_ms: u64::from(std::process::id()),
            undecided: None,
        },
    )
}

/// How many DENIED decisions one agent had.
#[cfg(feature = "console")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedCount {
    pub session: String,
    /// In the last [`PROBE_SECS`].
    pub recent: usize,
    /// In the last [`SHOWN_SECS`].
    pub day: usize,
}

#[cfg(feature = "console")]
impl DeniedCount {
    /// The agent is probing: [`PROBE_COUNT`] or more DENIED decisions in [`PROBE_SECS`].
    #[must_use]
    pub const fn probing(&self) -> bool {
        self.recent >= PROBE_COUNT
    }
}

/// The DENIED decisions up to `now`, counted for each session, the sessions with the most recent
/// ones first (then by name).
#[must_use]
#[cfg(feature = "console")]
pub fn denied_counts(records: &[Record], now: u64) -> Vec<DeniedCount> {
    let mut counts: std::collections::BTreeMap<&str, (usize, usize)> =
        std::collections::BTreeMap::new();
    for r in records
        .iter()
        .filter(|r| r.hook == DENIED_HOOK && r.at <= now && now - r.at <= SHOWN_SECS)
    {
        let entry = counts.entry(r.session.as_str()).or_default();
        entry.1 += 1;
        if now - r.at <= PROBE_SECS {
            entry.0 += 1;
        }
    }
    let mut out: Vec<DeniedCount> = counts
        .into_iter()
        .map(|(session, (recent, day))| DeniedCount {
            session: session.to_owned(),
            recent,
            day,
        })
        .collect();
    out.sort_by(|a, b| b.recent.cmp(&a.recent).then(a.session.cmp(&b.session)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(at: u64, undecided: Option<&str>) -> Record {
        Record {
            at,
            session: "019a3c1e".to_owned(),
            hook: "pretool".to_owned(),
            call: "the command `git push origin main`".to_owned(),
            step: "searching the folder /home/u".to_owned(),
            step_ms: 9_800,
            took_ms: 10_000,
            undecided: undecided.map(str::to_owned),
        }
    }

    /// A scratch Lunchroom folder of its own, removed first.
    fn scratch(name: &str) -> PathBuf {
        let home = crate::test_target()
            .join("records-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        home
    }

    // §13 H1.8, H1.9: a record keeps the time, session, call, slowest step and its time;
    // the file keeps only the latest 200, oldest dropped first.
    #[test]
    fn the_records_keep_only_the_latest_200() {
        let home = scratch("latest");
        assert_eq!(read(&home), Vec::new());
        for at in 0..(KEEP as u64 + 5) {
            assert_eq!(add(&home, &record(at, None)), Ok(()));
        }
        let kept = read(&home);
        assert_eq!(kept.len(), KEEP);
        assert_eq!(kept.first().map(|r| r.at), Some(5));
        assert_eq!(kept.last().map(|r| r.at), Some(KEEP as u64 + 4));
        assert_eq!(kept.last(), Some(&record(KEEP as u64 + 4, None)));
        let _ = std::fs::remove_dir_all(&home);
    }

    // DV-10 (H1.8): a 1.1.0 build keeps 200 records and no others: a record of a DENIED decision
    // (1.1) is not kept apart there, so the file never holds more than 200 lines, and `recent`
    // does not hide it. With the console the same file keeps up to 1,000 of them besides.
    #[test]
    fn denied_records_are_kept_apart_only_with_the_console() {
        let home = scratch("denied");
        let denied = |at: u64| Record {
            hook: "denied".to_owned(),
            call: String::new(),
            step: "protected".to_owned(),
            step_ms: at,
            ..record(at, None)
        };
        for at in 0..(KEEP as u64 + 50) {
            assert_eq!(add(&home, &denied(at)), Ok(()));
        }
        assert_eq!(add(&home, &record(10_000, None)), Ok(()));
        let kept = read(&home);
        if cfg!(feature = "console") {
            // The 250 DENIED ones are kept apart from the guard's one.
            assert_eq!(kept.len(), KEEP + 50 + 1);
            assert_eq!(recent(&kept, 10_000).len(), 1);
        } else {
            let lines = std::fs::read_to_string(file(&home)).unwrap_or_default();
            assert_eq!(lines.lines().count(), KEEP, "{lines}");
            assert_eq!(kept.len(), KEEP);
            assert_eq!(kept.last().map(|r| r.at), Some(10_000));
            assert_eq!(recent(&kept, 10_000).len(), KEEP);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // §14 C1.4: the records shown are those of the last 24 hours: one exactly 24 hours
    // old is shown, one a second older is not, nor one from the future.
    #[test]
    fn the_last_24_hours_are_shown_both_edges() {
        let now = 1_793_000_000;
        let all = [
            record(now - SHOWN_SECS - 1, Some("x")),
            record(now - SHOWN_SECS, Some("y")),
            record(now, None),
            record(now + 1, None),
        ];
        let shown: Vec<u64> = recent(&all, now).iter().map(|r| r.at).collect();
        assert_eq!(shown, vec![now - SHOWN_SECS, now]);
    }

    // G1-4: the call is text an agent chose, and doctor prints it. No control character survives
    // the write or the read (an escape sequence would redraw the user's screen), a line break
    // becomes a space, and a direction override is removed too.
    #[test]
    fn control_characters_are_removed_when_written_and_when_read() {
        let home = scratch("controls");
        let mut r = record(10, Some("the store\u{1b}[31m was busy"));
        r.call = "ls\u{1b}[2J\u{7}\nwipe\r\u{202e}x".to_owned();
        r.step = "step\u{0}".to_owned();
        r.session = "s\u{1b}1".to_owned();
        assert_eq!(add(&home, &r), Ok(()));
        let text = std::fs::read_to_string(file(&home)).unwrap_or_default();
        assert!(
            !text.chars().any(|c| c == '\u{1b}' || c == '\u{7}'),
            "{text:?}"
        );
        let kept = read(&home);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].call, "ls[2J wipe x");
        assert_eq!(kept[0].step, "step");
        assert_eq!(kept[0].session, "s1");
        assert_eq!(kept[0].undecided.as_deref(), Some("the store[31m was busy"));
        // A record an older program wrote with an escape in it is cleaned on the way out.
        let old = serde_json::to_string(&record(11, None))
            .unwrap_or_default()
            .replace("git push", "git\\u001b[2J push");
        let mut file_text = text;
        file_text.push_str(&old);
        file_text.push('\n');
        assert!(std::fs::write(file(&home), file_text).is_ok());
        let after = read(&home);
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].call, "the command `git[2J push origin main`");
        assert!(after.iter().all(|r| {
            ![&r.session, &r.hook, &r.call, &r.step]
                .iter()
                .any(|t| t.chars().any(char::is_control))
        }));
        let _ = std::fs::remove_dir_all(&home);
    }

    // G1-4: a secret in the call (a command given a token) is not kept: neither in the file nor in
    // what is read. The sample is assembled here so no credential-shaped text sits in the source.
    #[test]
    fn a_secret_in_a_record_is_replaced_before_it_is_kept() {
        let home = scratch("secrets");
        let token = format!("ghp_{}", "a".repeat(36));
        let mut r = record(20, Some("no answer"));
        r.call = format!("the command `curl -H {token} https://example.com`");
        r.step = format!("looking up the path /x/{token}");
        assert_eq!(add(&home, &r), Ok(()));
        let text = std::fs::read_to_string(file(&home)).unwrap_or_default();
        assert!(!text.contains("ghp_"), "{text}");
        let kept = read(&home);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].call,
            "the command `curl -H [secret removed] https://example.com`"
        );
        assert_eq!(kept[0].step, "looking up the path /x/[secret removed]");
        let _ = std::fs::remove_dir_all(&home);
    }

    // G1-4: the records file is readable by its owner alone, also over a temporary file left by
    // an older program with wider access.
    #[test]
    fn the_records_file_is_private() {
        let home = scratch("mode");
        assert!(std::fs::create_dir_all(&home).is_ok());
        let temp = home.join("guard-records.jsonl.tmp");
        assert!(std::fs::write(&temp, "").is_ok());
        assert!(std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).is_ok());
        assert_eq!(add(&home, &record(30, None)), Ok(()));
        let mode = std::fs::metadata(file(&home)).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode.ok(), Some(0o600));
        let _ = std::fs::remove_dir_all(&home);
    }

    // G1-4: a guard that cannot take the lock does not drop its record: it is written to a file
    // of its own, shown by `read` at once, and folded into the records file by the next guard that
    // takes the lock.
    #[test]
    fn a_record_is_not_dropped_when_the_lock_is_busy() {
        let home = scratch("busy");
        assert!(std::fs::create_dir_all(&home).is_ok());
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("guard-records.flock"));
        assert!(holder.as_ref().is_ok_and(|f| f.try_lock().is_ok()));
        assert_eq!(
            add_waiting(&home, &record(40, Some("first")), Duration::from_millis(50)),
            Ok(())
        );
        assert_eq!(
            read(&home).iter().map(|r| r.at).collect::<Vec<_>>(),
            vec![40]
        );
        assert!(!file(&home).exists());
        let spilled = std::fs::read_dir(spill_dir(&home)).map(Iterator::count);
        assert_eq!(spilled.ok(), Some(1));
        let mode = std::fs::read_dir(spill_dir(&home))
            .ok()
            .and_then(|mut d| d.next())
            .and_then(Result::ok)
            .and_then(|e| e.metadata().ok())
            .map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode, Some(0o600));
        drop(holder);
        assert_eq!(add(&home, &record(41, None)), Ok(()));
        assert_eq!(
            read(&home).iter().map(|r| r.at).collect::<Vec<_>>(),
            vec![40, 41]
        );
        let in_file = std::fs::read_to_string(file(&home)).unwrap_or_default();
        assert_eq!(in_file.lines().count(), 2, "{in_file}");
        let left = std::fs::read_dir(spill_dir(&home)).map(Iterator::count);
        assert_eq!(left.ok(), Some(0));
        let _ = std::fs::remove_dir_all(&home);
    }

    // G1-4: guards writing at once lose none: eight threads, each its own lock handle as eight
    // guard processes would have, write ten records each.
    #[test]
    fn records_written_at_once_are_all_kept() {
        let home = scratch("many");
        let writers: Vec<_> = (0..8u64)
            .map(|t| {
                let home = home.clone();
                std::thread::spawn(move || {
                    for k in 0..10u64 {
                        let mut r = record(1_000 + t * 10 + k, None);
                        r.session = format!("s{t}-{k}");
                        assert_eq!(add(&home, &r), Ok(()));
                    }
                })
            })
            .collect();
        for w in writers {
            assert!(w.join().is_ok());
        }
        let kept = read(&home);
        assert_eq!(kept.len(), 80);
        let mut sessions: Vec<&str> = kept.iter().map(|r| r.session.as_str()).collect();
        sessions.sort_unstable();
        sessions.dedup();
        assert_eq!(sessions.len(), 80);
        let _ = std::fs::remove_dir_all(&home);
    }
}
