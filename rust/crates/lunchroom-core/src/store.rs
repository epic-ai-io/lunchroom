//! A crash-safe store: one state file, replaced whole and atomically, one writer at a time.
//!
//! Spec: §10 D1 (no loss, no duplicates across a crash or restart) and §8 A2.4 (approving and using
//! an approval are each one atomic step). Every change is one [`Store::update`]: take the
//! exclusive lock, read the state, change it, write it to a temporary file, flush it to disk,
//! rename it over the state file and flush the folder; a change that leaves the state as it was
//! writes nothing. A crash at any point leaves either the old
//! state or the new one, never a mix, and two processes' changes never interleave. A state file
//! that cannot be read is an error, never an empty state, so nothing fails open.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::marker::PhantomData;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const STATE: &str = "state.json";
const TEMP: &str = "state.json.tmp";
/// The state as it was before the last change: a state that was read whole, so `repair` can
/// return to it when the state file cannot be read.
const PREVIOUS: &str = "state.json.prev";
const PREVIOUS_TEMP: &str = "state.json.prev.tmp";
/// The file of the fields 1.1 adds (spec §10 D1.2): beside the state file, read and written under
/// the same lock. A 1.0.1 program takes the lock and rewrites the state file without the fields it
/// does not know; it never rewrites this one, so none of them is lost while it still runs.
const SIDE: &str = "state.side.json";
const SIDE_TEMP: &str = "state.side.json.tmp";
const SIDE_PREVIOUS: &str = "state.side.json.prev";
const SIDE_PREVIOUS_TEMP: &str = "state.side.json.prev.tmp";
const GUARD_FILE: &str = "state.flock";
/// How long a reader or writer waits for another to finish: past it, the store is reported busy,
/// so no command, the user's own included, waits on a process that never lets go.
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

static STOPPED: AtomicBool = AtomicBool::new(false);

/// H1.7: the step of waiting for another Lunchroom process to let go of the store.
const WAITING_STEP: &str = "waiting for another Lunchroom process to release Lunchroom's store";

/// From now on, no update in this process changes the state (H1.5: a guard past its deadline
/// answers as one that could not decide, so nothing it is still doing may commit a different
/// outcome). An update already under way finishes.
pub fn stop_updates() {
    STOPPED.store(true, Ordering::SeqCst);
}

/// The time a caller with a limit of its own (a hook that must answer within its time) allows
/// every wait for the store to end by; none unless [`limit_waits_until`] set one.
static WAIT_END: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// From now on, no wait for the store in this process lasts past `end`: a wait that would is
/// reported busy at `end`. Set by a program that must answer within a time of its own; the
/// longest wait ([`LOCK_WAIT`]) still applies as well.
pub fn limit_waits_until(end: std::time::Instant) {
    if let Ok(mut set) = WAIT_END.lock() {
        *set = Some(end);
    }
}

/// The longest pause between tries for a lock held by another.
const LOCK_RETRY_MAX: std::time::Duration = std::time::Duration::from_millis(20);

/// The pause after `pause`: twice as long, up to [`LOCK_RETRY_MAX`].
fn longer(pause: std::time::Duration) -> std::time::Duration {
    (pause * 2).min(LOCK_RETRY_MAX)
}

/// Takes the lock with `try_lock`, retrying until [`LOCK_WAIT`] has passed. The first retry comes
/// after 1 ms, so a lock held briefly costs little more than that wait, not a flat 20 ms.
fn wait_for(try_lock: impl Fn() -> Result<(), fs::TryLockError>) -> io::Result<()> {
    let start = std::time::Instant::now();
    let limit = WAIT_END.lock().ok().and_then(|set| *set);
    let mut pause = std::time::Duration::from_millis(1);
    loop {
        match try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::Error(e)) => return Err(e),
            Err(fs::TryLockError::WouldBlock)
                if start.elapsed() < LOCK_WAIT && limit.is_none_or(|end| Instant::now() < end) =>
            {
                std::thread::sleep(pause);
                pause = longer(pause);
            }
            Err(fs::TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "another Lunchroom process held the store for as long as this program waits \
                     for it",
                ));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "another Lunchroom process has held the store for 10 seconds",
                ));
            }
        }
    }
}

/// What identifies one version of the state file: its change time (seconds, nanoseconds), size
/// and inode. Every write replaces the file, so a new state is a new stamp.
pub type Stamp = (i64, i64, u64, u64);

/// A state that keeps some of its fields in a file of their own (spec §10 D1.2).
///
/// [`Sided::split`] takes the fields an older program does not know out of the state's JSON and
/// returns them, [`Sided::join`] puts them back. The defaults keep nothing apart, so a state with
/// no such fields is one file.
pub trait Sided {
    /// Removes from `main` the fields that belong in the second file and returns them as one JSON
    /// object, or `Null` when there are none.
    fn split(main: &mut Value) -> Value {
        let _ = main;
        Value::Null
    }

    /// Puts the fields of the second file back into `main`.
    fn join(main: &mut Value, side: &Value) {
        let _ = (main, side);
    }
}

/// The two files of one state as read: the state file and the second file, each as text (`None`
/// when the file was never written; a file that is there and empty is text, and does not parse).
struct Loaded<T> {
    state: T,
    main: Option<String>,
    side: Option<String>,
}

/// A state of type `T` kept in one folder.
#[derive(Debug)]
pub struct Store<T> {
    dir: PathBuf,
    kind: PhantomData<fn() -> T>,
}

/// Where a test stops an update, to stand for a crash (D1.2). Only tests stop one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Crash {
    /// Part way through writing the new state to the temporary file: half of it is there, none
    /// of it flushed.
    MidWrite,
    /// After the new state is written to the temporary file, before it replaces the state file.
    BeforeRename,
    /// After the state being replaced is kept as the previous one, before the new state replaces it.
    AfterPrevious,
    /// After the second file replaces its file, before the state file does.
    AfterSide,
    /// After the new state replaces the state file, before the folder is flushed.
    AfterRename,
    /// A repair, after the previous state is written to the temporary file, before the unreadable
    /// state file is kept or replaced.
    RepairAfterTemp,
    /// A repair, after the unreadable state file is kept beside, before it is replaced.
    RepairAfterKeep,
    /// A repair, after the previous state replaces the state file, before the folder is flushed.
    RepairAfterRename,
}

impl<T: Serialize + DeserializeOwned + Default + Sided> Store<T> {
    /// The store in `dir`, which is created, readable by its owner only, if it is missing.
    ///
    /// # Errors
    /// When the folder cannot be created.
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| with_path(dir, &e))?;
        crate::setgid::settle(dir);
        Ok(Self {
            dir: dir.to_path_buf(),
            kind: PhantomData,
        })
    }

    /// The state in `dir` as last written, read without creating or locking anything (C1.4:
    /// `lunchroom doctor` changes no file): the default state when there is no folder or no state
    /// yet. Every write replaces the state file whole by a rename, so the file read is one whole
    /// state.
    ///
    /// # Errors
    /// When the state file cannot be read or does not parse, or it is missing beside a previous
    /// state.
    pub fn read_without_change(dir: &Path) -> io::Result<T> {
        Self {
            dir: dir.to_path_buf(),
            kind: PhantomData,
        }
        .load()
        .map(|loaded| loaded.state)
    }

    /// The state as last written; the default state before the first write.
    ///
    /// # Errors
    /// When the state file cannot be read or does not parse.
    pub fn read(&self) -> io::Result<T> {
        // H1.7: the steps named if the guard's time runs out here.
        let _step = crate::steps::begin("reading Lunchroom's store");
        let guard = self.guard()?;
        {
            let _wait = crate::steps::begin(WAITING_STEP);
            wait_for(|| guard.try_lock_shared())?;
        }
        let state = self.load().map(|loaded| loaded.state);
        guard.unlock()?;
        state
    }

    /// The stamp of the state as it is now, taken without a lock; all zeros when there is none.
    /// Equal stamps mean the same state, so a caller that found nothing to do may skip reading
    /// until the stamp changes. It covers the state file and the second file (D1.2): a change
    /// to either is a new stamp.
    #[must_use]
    pub fn stamp(&self) -> Stamp {
        let of = |name: &str| {
            fs::metadata(self.dir.join(name)).map_or((0, 0, 0, 0), |m| {
                (m.mtime(), m.mtime_nsec(), m.size(), m.ino())
            })
        };
        let (main, side) = (of(STATE), of(SIDE));
        (
            main.0.wrapping_add(side.0.wrapping_mul(31)),
            main.1.wrapping_add(side.1.wrapping_mul(31)),
            main.2.wrapping_add(side.2.wrapping_mul(31)),
            main.3 ^ side.3.rotate_left(17),
        )
    }

    /// Changes the state in one atomic step and returns what `change` returns.
    ///
    /// # Errors
    /// When the state cannot be read, or the new state cannot be written; the state on disk is then
    /// the one before the call.
    pub fn update<R>(&self, change: impl FnOnce(&mut T) -> R) -> io::Result<R> {
        self.update_inner(change, None)
    }

    /// [`Store::update`] that stops at `crash`, as a crash would.
    #[cfg(test)]
    pub(crate) fn update_crashing<R>(
        &self,
        change: impl FnOnce(&mut T) -> R,
        crash: Crash,
    ) -> io::Result<R> {
        self.update_inner(change, Some(crash))
    }

    fn update_inner<R>(
        &self,
        change: impl FnOnce(&mut T) -> R,
        crash: Option<Crash>,
    ) -> io::Result<R> {
        // H1.7: the steps named if the guard's time runs out here.
        let _step = crate::steps::begin("updating Lunchroom's store");
        let guard = self.guard()?;
        {
            let _wait = crate::steps::begin(WAITING_STEP);
            wait_for(|| guard.try_lock())?;
        }
        // Checked under the lock: an update that starts after `stop_updates` changes nothing.
        let result = if STOPPED.load(Ordering::SeqCst) {
            Err(io::Error::other(
                "the guard's time ran out; nothing more is written",
            ))
        } else {
            self.change_locked(change, crash)
        };
        guard.unlock()?;
        result
    }

    fn change_locked<R>(
        &self,
        change: impl FnOnce(&mut T) -> R,
        crash: Option<Crash>,
    ) -> io::Result<R> {
        let loaded = self.load()?;
        let mut state = loaded.state;
        let result = change(&mut state);
        // The state as JSON, with the fields of the second file taken out of it (D1.2).
        let mut main_value = serde_json::to_value(&state).map_err(io::Error::other)?;
        let side_value = T::split(&mut main_value);
        let bytes = serde_json::to_vec(&main_value).map_err(io::Error::other)?;
        let (old_main, old_side) = (
            loaded.main.as_deref().unwrap_or_default(),
            loaded.side.as_deref().unwrap_or_default(),
        );
        let side_bytes = if side_value.is_null() {
            loaded.side.is_some().then(|| b"{}".to_vec())
        } else {
            Some(serde_json::to_vec(&side_value).map_err(io::Error::other)?)
        };
        let main_changed = bytes != old_main.as_bytes();
        let side_changed = side_bytes
            .as_ref()
            .is_some_and(|b| b.as_slice() != old_side.as_bytes());
        // Nothing changed: the state on disk already is the new state.
        if !main_changed && !side_changed {
            return Ok(result);
        }
        // A second file's temporary left by a crash is no longer wanted when the second file is
        // not what this update writes.
        if !side_changed {
            let _ = fs::remove_file(self.dir.join(SIDE_TEMP));
        }
        // Both new files are written whole and flushed before either replaces its file.
        if let (true, Some(side_bytes)) = (side_changed, &side_bytes) {
            let mut file = self.create_temp(SIDE_TEMP)?;
            file.write_all(side_bytes)?;
            file.sync_all()?;
        }
        let temp = self.dir.join(TEMP);
        if main_changed {
            let mut file = self.create_temp(TEMP)?;
            let half = bytes.len() / 2;
            file.write_all(&bytes[..half])?;
            if crash == Some(Crash::MidWrite) {
                return Err(io::Error::other(
                    "stopped part way through the write, as a crash",
                ));
            }
            file.write_all(&bytes[half..])?;
            file.sync_all()?;
        }
        if crash == Some(Crash::BeforeRename) {
            return Err(io::Error::other("stopped before the rename, as a crash"));
        }
        // The state replaced is kept as the previous one (it was read whole above).
        if main_changed && loaded.main.is_some() {
            self.keep_previous(STATE, PREVIOUS_TEMP, PREVIOUS)?;
        }
        if side_changed && loaded.side.is_some() {
            self.keep_previous(SIDE, SIDE_PREVIOUS_TEMP, SIDE_PREVIOUS)?;
        }
        if crash == Some(Crash::AfterPrevious) {
            return Err(io::Error::other(
                "stopped after keeping the previous state, as a crash",
            ));
        }
        // The second file first: the state file's rename is the last step, so a crash between the
        // two leaves a state file the second file's newer fields only add to.
        if side_changed {
            fs::rename(self.dir.join(SIDE_TEMP), self.dir.join(SIDE))?;
        }
        if crash == Some(Crash::AfterSide) {
            return Err(io::Error::other(
                "stopped after the second file's rename, as a crash",
            ));
        }
        if main_changed {
            fs::rename(&temp, self.dir.join(STATE))?;
        }
        if crash == Some(Crash::AfterRename) {
            return Err(io::Error::other("stopped after the rename, as a crash"));
        }
        File::open(&self.dir)?.sync_all()?;
        Ok(result)
    }

    /// A new, empty temporary file `name` in the store's folder, readable by its owner only.
    fn create_temp(&self, name: &str) -> io::Result<File> {
        let path = self.dir.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        crate::setgid::settle(&path);
        Ok(file)
    }

    /// Keeps `file` as `previous` by a link and a rename, so the previous state is whole at every
    /// moment.
    fn keep_previous(&self, file: &str, link_name: &str, previous: &str) -> io::Result<()> {
        let link = self.dir.join(link_name);
        let _ = fs::remove_file(&link);
        fs::hard_link(self.dir.join(file), &link)?;
        fs::rename(&link, self.dir.join(previous))
    }

    /// The state from before the last change.
    ///
    /// # Errors
    /// When there is none, or it does not parse.
    pub fn read_previous(&self) -> io::Result<T> {
        let main = fs::read_to_string(self.dir.join(PREVIOUS))?;
        let side = match fs::read_to_string(self.dir.join(SIDE_PREVIOUS)) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Self::parse(Some(&main), side.as_deref())
    }

    /// When the state file cannot be read, puts the previous state back and keeps the unreadable
    /// file beside it as `state.json.unreadable-<n>`. `Ok(false)` when the state file reads.
    ///
    /// The replacement is written and flushed first, the unreadable file is kept by a second link
    /// to it, and the state file is then replaced in one rename, so there is no moment without a
    /// state file: a crash at any step leaves the unreadable file in place (the repair runs
    /// again) or the previous state in its place.
    ///
    /// # Errors
    /// When there is no previous state that reads either, or the files cannot be moved.
    pub fn restore_previous(&self) -> io::Result<bool> {
        self.restore_inner(None)
    }

    /// [`Store::restore_previous`] that stops at `crash`, as a crash would.
    #[cfg(test)]
    pub(crate) fn restore_previous_crashing(&self, crash: Crash) -> io::Result<bool> {
        self.restore_inner(Some(crash))
    }

    fn restore_inner(&self, crash: Option<Crash>) -> io::Result<bool> {
        let guard = self.guard()?;
        wait_for(|| guard.try_lock())?;
        let result = self.repair_locked(crash);
        guard.unlock()?;
        result
    }

    fn repair_locked(&self, crash: Option<Crash>) -> io::Result<bool> {
        if self.load().is_ok() {
            return Ok(false);
        }
        let mut repaired = false;
        // The state file first, from the state before the last change.
        let main_reads = self
            .read_text(STATE, PREVIOUS)
            .and_then(|main| Self::parse(main.as_deref(), None))
            .is_ok();
        if !main_reads {
            self.repair_file(
                (STATE, TEMP, PREVIOUS),
                |text| serde_json::from_str::<T>(text).is_ok(),
                crash,
            )?;
            repaired = true;
        }
        // The second file, when the state still does not read with it (D1.2): from its own
        // previous state, kept the same way.
        if self.load().is_err() {
            let main = self.read_text(STATE, PREVIOUS)?;
            self.repair_file(
                (SIDE, SIDE_TEMP, SIDE_PREVIOUS),
                |text| Self::parse(main.as_deref(), Some(text)).is_ok(),
                None,
            )?;
            repaired = true;
        }
        self.load()?;
        Ok(repaired)
    }

    /// Puts the previous state of the file `names.0` (`names.1` the temporary file, `names.2` the
    /// previous state) back when `valid` accepts it, keeping the unreadable file beside it as
    /// `<name>.unreadable-<n>`.
    fn repair_file(
        &self,
        names: (&str, &str, &str),
        valid: impl Fn(&str) -> bool,
        crash: Option<Crash>,
    ) -> io::Result<()> {
        let (file, temp_name, previous_name) = names;
        let previous = fs::read_to_string(self.dir.join(previous_name))?;
        if !valid(&previous) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the previous state of {file} does not read either"),
            ));
        }
        let temp = self.dir.join(temp_name);
        let mut handle = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        crate::setgid::settle(&temp);
        handle.write_all(previous.as_bytes())?;
        handle.sync_all()?;
        drop(handle);
        if crash == Some(Crash::RepairAfterTemp) {
            return Err(io::Error::other(
                "stopped after the temporary file, as a crash",
            ));
        }
        // The unreadable file is kept by a link, not a move, so the state file is absent for no
        // moment. A state file that is missing (lost, or an earlier repair that stopped) has
        // nothing to keep.
        match fs::symlink_metadata(self.dir.join(file)) {
            Ok(_) => {
                let mut n = 1;
                loop {
                    let kept = self.dir.join(format!("{file}.unreadable-{n}"));
                    match fs::hard_link(self.dir.join(file), &kept) {
                        Ok(()) => break,
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                        Err(e) => return Err(e),
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if crash == Some(Crash::RepairAfterKeep) {
            return Err(io::Error::other(
                "stopped after keeping the file, as a crash",
            ));
        }
        fs::rename(&temp, self.dir.join(file))?;
        if crash == Some(Crash::RepairAfterRename) {
            return Err(io::Error::other("stopped after the rename, as a crash"));
        }
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// The text of `file`, or `None` when it was never written. A missing file is a store never
    /// written to only when no previous state (`previous`) exists: every write keeps one, and no
    /// step of a write or a repair removes the file, so a previous state beside a missing file is
    /// a store whose state was lost, an error to repair, never an empty store.
    fn read_text(&self, file: &str, previous: &str) -> io::Result<Option<String>> {
        let mut text = String::new();
        match File::open(self.dir.join(file)) {
            Ok(mut handle) => {
                handle.read_to_string(&mut text)?;
                Ok(Some(text))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if self.dir.join(previous).exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{file} is missing but a previous state exists"),
                    ));
                }
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// The state from the text of the state file and of the second file (`None`: never written).
    /// The default state when there is no state file. An empty or damaged file is an error.
    fn parse(main: Option<&str>, side: Option<&str>) -> io::Result<T> {
        let bad = |e: serde_json::Error| io::Error::new(io::ErrorKind::InvalidData, e);
        let Some(side) = side else {
            return main.map_or_else(
                || Ok(T::default()),
                |m| serde_json::from_str(m).map_err(bad),
            );
        };
        let mut value = match main {
            Some(m) => serde_json::from_str::<Value>(m).map_err(bad)?,
            None => serde_json::to_value(T::default()).map_err(bad)?,
        };
        let side_value: Value = serde_json::from_str(side).map_err(bad)?;
        if !side_value.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the second file of the store is not a JSON object",
            ));
        }
        T::join(&mut value, &side_value);
        serde_json::from_value(value).map_err(bad)
    }

    /// The state and the text of the files it was read from.
    fn load(&self) -> io::Result<Loaded<T>> {
        let main = self.read_text(STATE, PREVIOUS)?;
        let side = self.read_text(SIDE, SIDE_PREVIOUS)?;
        let state = Self::parse(main.as_deref(), side.as_deref())?;
        Ok(Loaded { state, main, side })
    }

    /// The file whose exclusive lock admits one writer at a time. It is never replaced, so every
    /// process locks the same file.
    fn guard(&self) -> io::Result<File> {
        let path = self.dir.join(GUARD_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| with_path(&path, &e))?;
        crate::setgid::settle(&path);
        Ok(file)
    }
}

/// `e` with the path it is about at its front, keeping its kind: a user told only "Permission
/// denied" has nothing to act on (cold first-time install, 2026-11-14).
fn with_path(path: &Path, e: &io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    impl Sided for BTreeMap<String, u32> {}

    // An update that changes nothing leaves the state file as it is: no rewrite and no flush,
    // which every guard hook and every listener tick would otherwise pay.
    #[test]
    fn an_update_that_changes_nothing_writes_nothing() {
        let dir = crate::test_target()
            .join("store-unit")
            .join(std::process::id().to_string());
        let store = Store::<BTreeMap<String, u32>>::open(&dir);
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        let inode = || fs::metadata(dir.join(STATE)).ok().map(|m| m.ino());
        assert!(store.update(|m| m.insert("a".to_owned(), 1)).is_ok());
        let before = inode();
        assert!(before.is_some());
        assert_eq!(store.update(|m| m.get("a").copied()).ok(), Some(Some(1)));
        assert_eq!(inode(), before);
        assert!(store.update(|m| m.insert("a".to_owned(), 2)).is_ok());
        assert_ne!(inode(), before);
        assert_eq!(store.read().ok().and_then(|m| m.get("a").copied()), Some(2));
        let _ = fs::remove_dir_all(&dir);
    }

    // The stamp moves with every write and stays put for a read and for an update that changes
    // nothing: what lets a listener tick skip the store while nothing was written.
    #[test]
    fn the_stamp_moves_only_when_the_state_is_written() {
        let dir = crate::test_target()
            .join("store-unit-stamp")
            .join(std::process::id().to_string());
        let store = Store::<BTreeMap<String, u32>>::open(&dir);
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        assert_eq!(store.stamp(), (0, 0, 0, 0));
        assert!(store.update(|m| m.insert("a".to_owned(), 1)).is_ok());
        let written = store.stamp();
        assert_ne!(written, (0, 0, 0, 0));
        assert!(store.read().is_ok());
        assert!(store.update(|m| m.get("a").copied()).is_ok());
        assert_eq!(store.stamp(), written);
        assert!(store.update(|m| m.insert("a".to_owned(), 2)).is_ok());
        assert_ne!(store.stamp(), written);
        let _ = fs::remove_dir_all(&dir);
    }

    // The wait between tries starts at 1 ms, doubles, and stops at the cap.
    #[test]
    fn the_lock_wait_backs_off_to_its_cap() {
        let mut pause = std::time::Duration::from_millis(1);
        let mut seen = Vec::new();
        for _ in 0..7 {
            seen.push(pause.as_millis());
            pause = longer(pause);
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 20, 20]);
        assert!(LOCK_RETRY_MAX < LOCK_WAIT);
    }

    // An unreadable state file is repaired from the state before the last change; the bad file is
    // kept beside it; a store that reads is left alone.
    #[test]
    fn an_unreadable_store_is_repaired_from_the_previous_state() {
        let dir = crate::test_target()
            .join("store-unit-repair")
            .join(std::process::id().to_string());
        let store = Store::<BTreeMap<String, u32>>::open(&dir);
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        assert!(store.update(|m| m.insert("a".to_owned(), 1)).is_ok());
        assert!(store.update(|m| m.insert("a".to_owned(), 2)).is_ok());
        assert_eq!(store.restore_previous().ok(), Some(false));
        assert!(fs::write(dir.join(STATE), "{ broken").is_ok());
        assert!(store.read().is_err());
        assert_eq!(store.restore_previous().ok(), Some(true));
        assert_eq!(store.read().ok().and_then(|m| m.get("a").copied()), Some(1));
        assert!(dir.join(format!("{STATE}.unreadable-1")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A store in `case` that has held `a = 1` and then `a = 2`, its state file then broken.
    fn broken(case: &str) -> Option<(PathBuf, Store<BTreeMap<String, u32>>)> {
        let dir = crate::test_target()
            .join("store-unit-repair-crash")
            .join(std::process::id().to_string())
            .join(case);
        let store = Store::<BTreeMap<String, u32>>::open(&dir).ok()?;
        store.update(|m| m.insert("a".to_owned(), 1)).ok()?;
        store.update(|m| m.insert("a".to_owned(), 2)).ok()?;
        fs::write(dir.join(STATE), "{ broken").ok()?;
        Some((dir, store))
    }

    // §10::2 (D1.2): a repair stopped at any step never turns the store into an empty
    // one. The state file exists at every step (the unreadable file is kept by a link, the
    // replacement arrives in one rename); until the rename it is still the unreadable file, so the
    // store reads as an error, never as a new store; after it, it is the previous state. The
    // repair run again, as a restarted process runs it, ends with the previous state in place and
    // every unreadable copy kept.
    #[test]
    fn a_repair_stopped_at_any_step_never_leaves_an_empty_store() {
        let previous: BTreeMap<String, u32> = [("a".to_owned(), 1)].into();
        for (case, crash, readable) in [
            ("after-temp", Crash::RepairAfterTemp, false),
            ("after-keep", Crash::RepairAfterKeep, false),
            ("after-rename", Crash::RepairAfterRename, true),
        ] {
            let broken = broken(case);
            assert!(broken.is_some(), "{case}");
            let Some((dir, store)) = broken else { return };
            assert!(store.restore_previous_crashing(crash).is_err(), "{case}");
            assert!(dir.join(STATE).exists(), "{case}: state file present");
            assert_eq!(store.read().is_ok(), readable, "{case}: reads");
            if !readable {
                assert!(store.read().is_err(), "{case}: an error, not a new store");
            }
            assert!(store.restore_previous().is_ok(), "{case}: run again");
            assert_eq!(store.read().ok().as_ref(), Some(&previous), "{case}");
            assert!(dir.join(format!("{STATE}.unreadable-1")).exists(), "{case}");
            assert_eq!(
                fs::read_to_string(dir.join(format!("{STATE}.unreadable-1"))).ok(),
                Some("{ broken".to_owned()),
                "{case}: the unreadable file is kept as it was"
            );
            let _ = fs::remove_dir_all(&dir);
        }
    }

    // §10::2 (D1.2): a state file that is missing while the previous state exists is a
    // lost state, not a new store: it reads as an error (so nothing fails open on an empty state),
    // an update refuses to write over it, and the repair puts the previous state back with nothing
    // unreadable to keep. A store never written to still reads as the default.
    #[test]
    fn a_missing_state_file_beside_a_previous_state_is_repaired_not_read_as_new() {
        let fresh = crate::test_target()
            .join("store-unit-missing")
            .join(std::process::id().to_string());
        let store = Store::<BTreeMap<String, u32>>::open(&fresh);
        assert!(
            store
                .as_ref()
                .is_ok_and(|s| s.read().is_ok_and(|m| m.is_empty()))
        );
        let _ = fs::remove_dir_all(&fresh);
        let missing = broken("missing");
        assert!(missing.is_some());
        let Some((dir, store)) = missing else { return };
        assert!(fs::remove_file(dir.join(STATE)).is_ok());
        assert!(store.read().is_err());
        assert!(store.update(|m| m.insert("b".to_owned(), 3)).is_err());
        assert!(!dir.join(STATE).exists());
        assert_eq!(store.restore_previous().ok(), Some(true));
        assert_eq!(store.read().ok().and_then(|m| m.get("a").copied()), Some(1));
        assert!(!dir.join(format!("{STATE}.unreadable-1")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A store in `case` that has held `a = 1` and then `a = 2`, stopped by `crash` while it
    /// added `b = 3`, and opened again as a restarted process opens it.
    fn after_crash(case: &str, crash: Crash) -> Option<(PathBuf, Store<BTreeMap<String, u32>>)> {
        let dir = crate::test_target()
            .join("store-unit-crash")
            .join(std::process::id().to_string())
            .join(case);
        let store = Store::<BTreeMap<String, u32>>::open(&dir).ok()?;
        store.update(|m| m.insert("a".to_owned(), 1)).ok()?;
        store.update(|m| m.insert("a".to_owned(), 2)).ok()?;
        let crashed = store.update_crashing(|m| m.insert("b".to_owned(), 3), crash);
        assert!(crashed.is_err(), "{case}");
        drop(store);
        Some((dir.clone(), Store::open(&dir).ok()?))
    }

    // §10::2 (D1.2): a crash at any point of an update leaves the state as it was or as
    // it would have been, never a mix. A crash before the new state replaces the state file (part
    // way through writing it, written, or with the previous state kept) leaves the old state, with
    // the half-written or whole temporary file ignored and replaced by the next update; a crash
    // after the replacement leaves the new state, and the old one as the previous state. The store
    // is never held by the crashed update, and the next update is whole.
    #[test]
    fn a_crash_at_any_point_of_an_update_leaves_the_old_state_or_the_new() {
        let map = |pairs: &[(&str, u32)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), *v))
                .collect::<BTreeMap<_, _>>()
        };
        let (older, old, new) = (
            map(&[("a", 1)]),
            map(&[("a", 2)]),
            map(&[("a", 2), ("b", 3)]),
        );
        for (case, crash, state, previous, temp_left) in [
            ("mid-write", Crash::MidWrite, &old, &older, true),
            ("before-rename", Crash::BeforeRename, &old, &older, true),
            ("after-previous", Crash::AfterPrevious, &old, &old, true),
            ("after-rename", Crash::AfterRename, &new, &old, false),
        ] {
            let crashed = after_crash(case, crash);
            assert!(crashed.is_some(), "{case}");
            let Some((dir, store)) = crashed else { return };
            assert_eq!(store.read().ok().as_ref(), Some(state), "{case}: state");
            assert_eq!(
                store.read_previous().ok().as_ref(),
                Some(previous),
                "{case}: previous"
            );
            assert_eq!(dir.join(TEMP).exists(), temp_left, "{case}: temporary file");
            // The restarted process updates at once (the lock was let go) and the result is whole:
            // the state it read, with its own change, and no trace of the leftover file.
            let next = store.update(|m| m.insert("c".to_owned(), 9));
            assert!(next.is_ok(), "{case}: {next:?}");
            let mut expected = state.clone();
            expected.insert("c".to_owned(), 9);
            assert_eq!(store.read().ok(), Some(expected), "{case}: next state");
            assert_eq!(
                store.read_previous().ok().as_ref(),
                Some(state),
                "{case}: next previous"
            );
            assert!(!dir.join(TEMP).exists(), "{case}: temporary file replaced");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    /// A state with one field a 1.0.1 program does not know (`b`), kept in the second file.
    #[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Two {
        a: u32,
        #[serde(default)]
        b: u32,
    }

    impl Sided for Two {
        fn split(main: &mut Value) -> Value {
            let moved = main.as_object_mut().and_then(|o| o.remove("b"));
            moved.map_or(Value::Null, |b| serde_json::json!({ "b": b }))
        }

        fn join(main: &mut Value, side: &Value) {
            if let (Some(o), Some(b)) = (main.as_object_mut(), side.get("b")) {
                o.insert("b".to_owned(), b.clone());
            }
        }
    }

    fn two(case: &str) -> Option<(PathBuf, Store<Two>)> {
        let dir = crate::test_target()
            .join("store-unit-two")
            .join(std::process::id().to_string())
            .join(case);
        let _ = fs::remove_dir_all(&dir);
        let store = Store::<Two>::open(&dir).ok()?;
        Some((dir, store))
    }

    // §10::42, §10::43 (D1.2): the fields of the second file are written to a file of
    // their own, beside the state file, and an update that changes only them writes only it.
    #[test]
    fn the_second_file_holds_its_fields_and_only_it_changes_with_them() {
        let made = two("fields");
        assert!(made.is_some());
        let Some((dir, store)) = made else { return };
        assert!(store.update(|s| *s = Two { a: 1, b: 10 }).is_ok());
        let main = fs::read_to_string(dir.join(STATE)).unwrap_or_default();
        let side = fs::read_to_string(dir.join(SIDE)).unwrap_or_default();
        assert_eq!(main, "{\"a\":1}");
        assert_eq!(side, "{\"b\":10}");
        assert_eq!(store.read().ok(), Some(Two { a: 1, b: 10 }));
        // Only the second file's field changes: the state file is not rewritten, and the stamp
        // (which a listener reads to skip the store) moves.
        let (inode, stamp) = (
            fs::metadata(dir.join(STATE)).ok().map(|m| m.ino()),
            store.stamp(),
        );
        assert!(store.update(|s| s.b = 11).is_ok());
        assert_eq!(fs::metadata(dir.join(STATE)).ok().map(|m| m.ino()), inode);
        assert_ne!(store.stamp(), stamp);
        assert_eq!(
            fs::read_to_string(dir.join(SIDE)).unwrap_or_default(),
            "{\"b\":11}"
        );
        // Nothing changed: neither file is written, and the stamp stays.
        let stamp = store.stamp();
        let side_inode = fs::metadata(dir.join(SIDE)).ok().map(|m| m.ino());
        assert!(store.update(|s| s.b).is_ok());
        assert_eq!(store.stamp(), stamp);
        assert_eq!(
            fs::metadata(dir.join(SIDE)).ok().map(|m| m.ino()),
            side_inode
        );
        // The field goes back to nothing: the second file says so rather than keeping the old value.
        assert!(store.update(|s| s.b = 0).is_ok());
        assert_eq!(store.read().ok(), Some(Two { a: 1, b: 0 }));
        let _ = fs::remove_dir_all(&dir);
    }

    // §10::43 (D1.2): reads and writes of the second file hold `state.flock`: while
    // another program holds it, an update that changes only the second file does not go through,
    // and it does as soon as the lock is let go.
    #[test]
    fn the_second_file_is_written_under_the_same_lock() {
        let made = two("lock");
        assert!(made.is_some());
        let Some((dir, store)) = made else { return };
        assert!(store.update(|s| s.b = 1).is_ok());
        let holder = store.guard();
        assert!(holder.as_ref().is_ok_and(|h| h.try_lock().is_ok()));
        let worker = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let other = Store::<Two>::open(&dir);
                other.and_then(|s| s.update(|s| s.b = 2))
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!worker.is_finished(), "the update went through a held lock");
        assert_eq!(
            fs::read_to_string(dir.join(SIDE)).unwrap_or_default(),
            "{\"b\":1}",
            "the second file is not written while the lock is held"
        );
        drop(holder);
        assert!(worker.join().is_ok_and(|r| r.is_ok()));
        assert_eq!(store.read().ok().map(|s| s.b), Some(2));
        let _ = fs::remove_dir_all(&dir);
    }

    // §10::2, §10::42: a crash between the replacement of the second file and the state
    // file leaves a store that reads, with both files whole; the next update is whole.
    #[test]
    fn a_crash_between_the_two_files_leaves_a_store_that_reads() {
        for (case, crash) in [
            ("mid-write", Crash::MidWrite),
            ("before-rename", Crash::BeforeRename),
            ("after-previous", Crash::AfterPrevious),
            ("after-side", Crash::AfterSide),
            ("after-rename", Crash::AfterRename),
        ] {
            let made = two(case);
            assert!(made.is_some(), "{case}");
            let Some((dir, store)) = made else { return };
            assert!(store.update(|s| *s = Two { a: 1, b: 1 }).is_ok());
            assert!(store.update(|s| *s = Two { a: 2, b: 2 }).is_ok());
            assert!(
                store
                    .update_crashing(|s| *s = Two { a: 3, b: 3 }, crash)
                    .is_err(),
                "{case}"
            );
            let back = store.read().ok();
            assert!(back.is_some(), "{case}: reads");
            let back = back.unwrap_or_default();
            // Old, new, or (after the second file only) the new second file beside the old state.
            let allowed = [Two { a: 2, b: 2 }, Two { a: 3, b: 3 }, Two { a: 2, b: 3 }];
            assert!(allowed.contains(&back), "{case}: {back:?}");
            assert!(store.update(|s| s.a += 10).is_ok(), "{case}");
            assert!(store.read().is_ok_and(|s| s.a >= 12), "{case}");
            assert!(
                !dir.join(TEMP).exists() && !dir.join(SIDE_TEMP).exists(),
                "{case}"
            );
            let _ = fs::remove_dir_all(&dir);
        }
    }

    // A store another process holds is reported busy after LOCK_WAIT, never waited on forever.
    #[test]
    fn a_held_store_is_reported_busy() {
        let dir = crate::test_target()
            .join("store-unit-busy")
            .join(std::process::id().to_string());
        let store = Store::<BTreeMap<String, u32>>::open(&dir);
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        let holder = store.guard();
        assert!(holder.as_ref().is_ok_and(|h| h.try_lock().is_ok()));
        let start = std::time::Instant::now();
        let busy = store.update(|m| m.insert("a".to_owned(), 1));
        assert_eq!(busy.err().map(|e| e.kind()), Some(io::ErrorKind::TimedOut));
        assert!(start.elapsed() >= LOCK_WAIT);
        drop(holder);
        assert!(store.update(|m| m.insert("a".to_owned(), 1)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    // Cold first-time install, 2026-11-14: a store that cannot be opened or locked is named in
    // the error, not only "Permission denied".
    #[test]
    fn a_store_that_cannot_be_reached_is_named() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::test_target()
            .join("store-unit-named")
            .join(std::process::id().to_string());
        let _ = fs::remove_dir_all(&root);
        assert!(fs::create_dir_all(&root).is_ok());
        assert!(fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).is_ok());
        let dir = root.join("store");
        let err = Store::<BTreeMap<String, u32>>::open(&dir)
            .err()
            .map(|e| e.to_string());
        assert!(
            err.as_deref()
                .is_some_and(|e| e.starts_with(&format!("{}: ", dir.display()))),
            "{err:?}"
        );
        // A folder that exists and cannot be written: the lock file is named.
        assert!(fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).is_ok());
        assert!(fs::create_dir_all(&dir).is_ok());
        assert!(fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).is_ok());
        let store = Store::<BTreeMap<String, u32>>::open(&dir);
        assert!(store.is_ok());
        if let Ok(store) = store {
            let err = store.read().err().map(|e| e.to_string());
            assert!(
                err.as_deref().is_some_and(
                    |e| e.starts_with(&format!("{}: ", dir.join(GUARD_FILE).display()))
                ),
                "{err:?}"
            );
        }
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&root);
    }
}
