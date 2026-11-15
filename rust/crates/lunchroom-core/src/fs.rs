//! The file system as the guard sees it: real paths, wildcard matches, a bounded walk, script text.
//!
//! Spec §11 L1.1: "Paths are resolved (symlinks, wildcards) before checking." The guard runs before
//! every tool call, so wildcard matching and walking stop at a time budget; the caller treats a
//! stopped search as one that may have reached anything (fail safe). A walk enters every folder:
//! the command being judged may read any of them, so none is skipped as not the project's own.

use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Time one classification may spend resolving paths, matching wildcards and walking folders:
/// 8 seconds of a decision's 10 (spec §11 L1.1, H1.5).
pub const BUDGET: Duration = Duration::from_secs(8);

/// Most threads that resolve a call's paths at one time (spec §11 L1.1, lead's decision 2026-11-05).
pub const RESOLVE_THREADS: usize = 16;

/// Largest script file the guard reads to check what it runs.
pub const SCRIPT_MAX: u64 = 1 << 20;

/// Deepest folder level a wildcard or walk descends to.
const MAX_DEPTH: usize = 64;

/// Folders of a project that hold fetched or built files, usually large: a walk enters them after
/// everything else, so a budget that runs out spends itself on them last. They are never skipped,
/// because a search such as `grep -R` reads them, and a walk that ends early is treated as having
/// reached a credential.
const LAST: [&str; 7] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "vendor",
    ".venv",
    "__pycache__",
];

/// Most symbolic links one path resolution follows (Linux's own limit is 40, `ELOOP` beyond it).
pub const MAX_LINKS: usize = 40;

/// The paths a wildcard matched.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Every match found.
    pub matches: Vec<PathBuf>,
    /// The budget ran out before every match was found.
    pub capped: bool,
}

/// How a walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkEnd {
    /// Every entry was visited.
    Done,
    /// The visitor asked to stop.
    Stopped,
    /// The budget ran out first.
    Capped,
}

/// What a single path is, looked at without following it (`lstat`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Nothing is there, or a part before it is not a folder.
    Missing,
    /// A file or folder.
    Plain,
    /// A symbolic link, with the text it holds.
    Link(PathBuf),
    /// It could not be looked at (no permission, I/O error): what it is stays unknown.
    Unreadable,
}

/// A path resolved the way the operating system opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The real path of the longest part that exists, with the rest after it as written.
    pub path: PathBuf,
    /// Every part was looked at. `false` after a link loop, a part that could not be looked at, or
    /// a `..` that follows a part that does not exist (a command may make that part a link before
    /// the path is used): the file the path opens is then unknown.
    pub complete: bool,
}

/// The parts of `p` that name something (`Normal` and `..`), last first, for use as a stack.
fn parts_reversed(p: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect();
    v.reverse();
    v
}

/// A script file's text, as far as the guard can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Script {
    /// The whole file.
    Text(String),
    /// Larger than [`SCRIPT_MAX`].
    TooLarge,
    /// Missing or unreadable (the harness cannot run it either).
    Unreadable,
}

/// File-system questions the classifier asks.
pub trait Fs {
    /// The real path: symlinks resolved, for the longest part of the path that exists. `p` must
    /// not hold `..` (a link before it changes which folder it leaves): use [`Fs::resolve`].
    fn real(&self, p: &Path) -> PathBuf;
    /// What `p` is, not following it if it is a link.
    fn node(&self, p: &Path) -> Node;
    /// The time this view allows for looking at the file system has run out (never, by default).
    /// [`Fs::resolve`] looks at no further part once it has, and says so by returning incomplete.
    fn out_of_time(&self) -> bool {
        false
    }
    /// A look at `p` (or at a part of it) was cut short because the time ran out: `p` is one
    /// Lunchroom could not check (L1.1), which is not the same as one that cannot be followed at
    /// all. True only for a path whose own look was cut, never for a path that was resolved or
    /// that cannot be followed, whatever else the time did (a view with no clock says no).
    fn cut(&self, _p: &Path) -> bool {
        false
    }
    /// Notes that the look at `p` was cut by the time: [`Fs::resolve`] calls it when it stops
    /// looking at the parts of `p` because the time ran out. A view that answers [`Fs::cut`]
    /// keeps the note.
    fn note_cut(&self, _p: &Path) {}
    /// Process `pid` is process `root` or one of its descendants (1.1: the agent's own subtree).
    /// A view that cannot tell says no.
    fn descends_from(&self, pid: u32, root: u32) -> bool {
        let _ = (pid, root);
        false
    }
    /// Resolves the absolute path `p` as the operating system does when it opens it: part by part
    /// from the root, a link replaced by its target (relative to the folder holding the link), and
    /// `..` leaving the folder reached so far, which is already real. A part that does not exist
    /// (a file about to be written) and what follows it stay as written. A loop, a part that
    /// cannot be looked at and a `..` after a part that does not exist leave the result
    /// incomplete, for the caller to treat as reaching anything.
    fn resolve(&self, p: &Path) -> Resolved {
        let mut todo = parts_reversed(p);
        let mut out = PathBuf::from("/");
        let (mut exists, mut complete, mut links) = (true, true, 0usize);
        while let Some(part) = todo.pop() {
            if part == ".." {
                complete &= exists;
                out.pop();
                continue;
            }
            let next = out.join(&part);
            if !exists {
                out = next;
                continue;
            }
            // Out of time: no more looking. The rest stays as written and the result is
            // incomplete, so the caller treats the file as possibly anywhere.
            if self.out_of_time() {
                self.note_cut(p);
                (exists, complete) = (false, false);
                out = next;
                continue;
            }
            match self.node(&next) {
                Node::Plain => out = next,
                Node::Missing => {
                    exists = false;
                    out = next;
                }
                Node::Unreadable => {
                    (exists, complete) = (false, false);
                    out = next;
                }
                Node::Link(target) => {
                    links += 1;
                    if links > MAX_LINKS {
                        (exists, complete) = (false, false);
                        out = next;
                    } else {
                        if target.is_absolute() {
                            out = PathBuf::from("/");
                        }
                        todo.extend(parts_reversed(&target));
                    }
                }
            }
        }
        Resolved {
            path: out,
            complete,
        }
    }
    /// `p` is a folder (following symlinks).
    fn is_dir(&self, p: &Path) -> bool;
    /// Something exists at `p` (following symlinks: a dangling link does not).
    fn exists(&self, p: &Path) -> bool;
    /// Every existing path an absolute wildcard pattern matches.
    fn expand(&self, pattern: &Path) -> Expansion;
    /// Visits every path below `dir`. A symlink is visited as written and as its real path, and a
    /// linked folder is walked at its real path; each folder is walked once, and every folder is
    /// entered (those named in [`LAST`] after the rest). The visitor returns `true` to stop.
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd;
    /// The text of a script file, for checking what it runs.
    fn script(&self, p: &Path) -> Script;
    /// The system runs this file as a script: its first two bytes are `#!`, or it is an executable
    /// file that is not an ELF program, which a shell runs as a script of its own.
    fn runs_as_script(&self, p: &Path) -> bool;
    /// Where `PATH` finds a program, as a real path.
    fn which(&self, prog: &str) -> Option<PathBuf>;
    /// The device and inode of the file at `p` (following links), so a hard link under another name
    /// and folder is known to be the same file (C6-X09). A view that cannot tell says none.
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        let _ = p;
        None
    }
}

/// The real file system, with one time budget shared by every question.
#[derive(Debug)]
pub struct OsFs {
    deadline: Instant,
    path_var: Option<OsString>,
    /// Paths whose resolution the deadline cut short (see [`Fs::cut`]).
    cut: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
}

impl OsFs {
    /// A view whose wildcard matching and walks stop after `budget`.
    #[must_use]
    pub fn new(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
            path_var: std::env::var_os("PATH"),
            cut: std::sync::Mutex::default(),
        }
    }

    /// As [`OsFs::new`], with an explicit `PATH`.
    #[must_use]
    pub fn with_path(budget: Duration, path_var: Option<OsString>) -> Self {
        Self {
            deadline: Instant::now() + budget,
            path_var,
            cut: std::sync::Mutex::default(),
        }
    }

    /// A view whose wildcard matching and walks stop at `deadline` (the decision's own 8 seconds,
    /// L1.1, not a time counted from when this view was made).
    #[must_use]
    pub fn until(deadline: Instant) -> Self {
        Self {
            deadline,
            path_var: std::env::var_os("PATH"),
            cut: std::sync::Mutex::default(),
        }
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

impl Fs for OsFs {
    fn real(&self, p: &Path) -> PathBuf {
        let mut base = p.to_path_buf();
        let mut rest: Vec<OsString> = Vec::new();
        loop {
            if let Ok(r) = std::fs::canonicalize(&base) {
                return rest.iter().rev().fold(r, |acc, part| acc.join(part));
            }
            match (base.file_name().map(ToOwned::to_owned), base.parent()) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    base = parent.to_path_buf();
                }
                _ => return p.to_path_buf(),
            }
        }
    }

    fn out_of_time(&self) -> bool {
        self.expired()
    }

    fn note_cut(&self, p: &Path) {
        if let Ok(mut cut) = self.cut.lock() {
            cut.insert(p.to_path_buf());
        }
    }

    fn cut(&self, p: &Path) -> bool {
        self.cut.lock().map_or(true, |c| c.contains(p))
    }

    fn descends_from(&self, pid: u32, root: u32) -> bool {
        // The parent of a process is field 4 of /proc/<pid>/stat, after the name (which may hold
        // spaces and `)`).
        let mut at = pid;
        for _ in 0..64 {
            if at == root {
                return true;
            }
            if at <= 1 {
                return false;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{at}/stat")) else {
                return false;
            };
            let Some(parent) = stat
                .rfind(')')
                .and_then(|i| stat.get(i + 2..))
                .and_then(|rest| rest.split(' ').nth(1))
                .and_then(|p| p.parse::<u32>().ok())
            else {
                return false;
            };
            at = parent;
        }
        false
    }

    fn node(&self, p: &Path) -> Node {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::read_link(p).map_or(Node::Unreadable, Node::Link)
            }
            Ok(_) => Node::Plain,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Node::Missing
            }
            Err(_) => Node::Unreadable,
        }
    }

    fn is_dir(&self, p: &Path) -> bool {
        p.is_dir()
    }

    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }

    fn expand(&self, pattern: &Path) -> Expansion {
        let parts: Vec<String> = pattern
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let mut out = Expansion::default();
        let mut stack: Vec<(PathBuf, usize, usize)> = vec![(PathBuf::from("/"), 0, 0)];
        while let Some((dir, i, depth)) = stack.pop() {
            if self.expired() || depth > MAX_DEPTH {
                out.capped = true;
                break;
            }
            let Some(part) = parts.get(i) else {
                if std::fs::symlink_metadata(&dir).is_ok() {
                    out.matches.push(dir);
                }
                continue;
            };
            if !part.contains(['*', '?', '[']) {
                stack.push((dir.join(part), i + 1, depth));
                continue;
            }
            if part == "**" {
                stack.push((dir.clone(), i + 1, depth + 1));
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for (seen, e) in entries.flatten().enumerate() {
                // A folder of many names, matched against a long pattern, must not keep the guard
                // past its time: the clock is read every 64 names.
                if seen % 64 == 63 && self.expired() {
                    out.capped = true;
                    return out;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let path = e.path();
                if part == "**" {
                    if !name.starts_with('.') && path.is_dir() {
                        stack.push((path, i, depth + 1));
                    }
                } else if fnmatch(part, &name) {
                    stack.push((path, i + 1, depth + 1));
                }
            }
        }
        out
    }

    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        let mut stack = vec![(dir.to_path_buf(), 0usize)];
        // Folders already walked, by real path, so a link loop is walked once.
        let mut seen = std::collections::HashSet::new();
        seen.insert(self.real(dir));
        while let Some((d, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            // Folders found here: those in LAST go on the stack first, so they are walked last.
            let (mut first, mut last) = (Vec::new(), Vec::new());
            for e in entries.flatten() {
                if self.expired() || depth > MAX_DEPTH {
                    return WalkEnd::Capped;
                }
                let path = e.path();
                if visit(&path) {
                    return WalkEnd::Stopped;
                }
                let Ok(ft) = e.file_type() else { continue };
                let late = LAST.iter().any(|n| e.file_name() == *n);
                let queue = if late { &mut last } else { &mut first };
                if ft.is_symlink() {
                    let real = self.real(&path);
                    if visit(&real) {
                        return WalkEnd::Stopped;
                    }
                    // A linked folder is walked at its real path: a search that follows links
                    // reads what is there.
                    if real.is_dir() && seen.insert(real.clone()) {
                        queue.push((real, depth + 1));
                    }
                } else if ft.is_dir() && seen.insert(self.real(&path)) {
                    queue.push((path, depth + 1));
                }
            }
            stack.extend(last);
            stack.extend(first);
        }
        WalkEnd::Done
    }

    fn script(&self, p: &Path) -> Script {
        let Ok(meta) = std::fs::metadata(p) else {
            return Script::Unreadable;
        };
        if !meta.is_file() {
            return Script::Unreadable;
        }
        if meta.len() > SCRIPT_MAX {
            return Script::TooLarge;
        }
        match std::fs::read(p) {
            Ok(bytes) => Script::Text(String::from_utf8_lossy(&bytes).into_owned()),
            Err(_) => Script::Unreadable,
        }
    }

    fn runs_as_script(&self, p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(meta) = std::fs::metadata(p) else {
            return false;
        };
        // Only a regular file is opened: a terminal device (`/dev/pts/7`), a pipe or a socket
        // named in a command blocks the open or the read, and runs as no script.
        if !meta.is_file() {
            return false;
        }
        let executable = meta.permissions().mode() & 0o111 != 0;
        let mut head = [0u8; 4];
        // An executable file the guard cannot open is judged as one: `script` then refuses it.
        let Ok(mut file) = std::fs::File::open(p) else {
            return executable;
        };
        let read = file.read(&mut head).unwrap_or(0);
        head.starts_with(b"#!") && read >= 2 || executable && head != *b"\x7fELF"
    }

    fn which(&self, prog: &str) -> Option<PathBuf> {
        let path_var = self.path_var.as_ref()?;
        std::env::split_paths(path_var)
            .map(|d| d.join(prog))
            .find(|c| c.is_file())
            .map(|c| self.real(&c))
    }

    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
    }
}

/// How one path of a parallel resolution ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The resolution ran to its end. `Resolved::complete` is `false` for a path that cannot be
    /// followed at all (a link loop, a part that cannot be looked at, a `..` after a part that
    /// does not exist): the caller DENIES it (spec §11 L1.1).
    Done(Resolved),
    /// The shared deadline passed before the path was resolved: one Lunchroom could not check. For
    /// a HELD session's read the caller PROTECTS it as a credential path would be.
    Unresolved,
}

/// A view of `inner` that reports out of time at `deadline`, and notes that it did.
struct Bounded<F: Fs + ?Sized> {
    inner: Arc<F>,
    deadline: Instant,
    cut: AtomicBool,
}

impl<F: Fs + ?Sized> Fs for Bounded<F> {
    fn real(&self, p: &Path) -> PathBuf {
        self.inner.real(p)
    }
    fn node(&self, p: &Path) -> Node {
        self.inner.node(p)
    }
    fn out_of_time(&self) -> bool {
        if self.inner.out_of_time() || Instant::now() >= self.deadline {
            self.cut.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }
    fn descends_from(&self, pid: u32, root: u32) -> bool {
        self.inner.descends_from(pid, root)
    }
    fn is_dir(&self, p: &Path) -> bool {
        self.inner.is_dir(p)
    }
    fn exists(&self, p: &Path) -> bool {
        self.inner.exists(p)
    }
    fn expand(&self, pattern: &Path) -> Expansion {
        self.inner.expand(pattern)
    }
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        self.inner.walk(dir, visit)
    }
    fn script(&self, p: &Path) -> Script {
        self.inner.script(p)
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.inner.runs_as_script(p)
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.inner.which(prog)
    }
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        self.inner.file_id(p)
    }
}

/// The threads that look at the file system for the guard, counted across every call of the
/// process (L1.1: at most 16 at a time). A thread stuck in a file-system call keeps its place until
/// it ends, so stuck threads count against the cap and no new thread starts in their place.
#[derive(Debug)]
pub struct Threads {
    limit: usize,
    busy: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
}

/// One place among the [`Threads`], released when dropped (when the thread holding it ends).
#[derive(Debug)]
pub struct Slot(Arc<Threads>);

impl Threads {
    /// A set of `limit` places (at least 1).
    #[must_use]
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            busy: std::sync::Mutex::new(0),
            freed: std::sync::Condvar::new(),
        })
    }

    /// The process's own set, [`RESOLVE_THREADS`] places.
    #[must_use]
    pub fn global() -> &'static Arc<Self> {
        static GLOBAL: std::sync::OnceLock<Arc<Threads>> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| Threads::new(RESOLVE_THREADS))
    }

    /// Places in use now.
    #[must_use]
    pub fn in_use(&self) -> usize {
        self.busy.lock().map_or(self.limit, |b| *b)
    }

    /// A place if one is free now.
    #[must_use]
    pub fn try_take(self: &Arc<Self>) -> Option<Slot> {
        let mut busy = self.busy.lock().ok()?;
        if *busy >= self.limit {
            return None;
        }
        *busy += 1;
        Some(Slot(Arc::clone(self)))
    }

    /// A place, waiting for one to be freed until `deadline`.
    #[must_use]
    pub fn take_until(self: &Arc<Self>, deadline: Instant) -> Option<Slot> {
        let mut busy = self.busy.lock().ok()?;
        loop {
            if *busy < self.limit {
                *busy += 1;
                return Some(Slot(Arc::clone(self)));
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            if left.is_zero() {
                return None;
            }
            busy = self.freed.wait_timeout(busy, left).ok()?.0;
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.0.busy.lock() {
            *busy = busy.saturating_sub(1);
        }
        self.0.freed.notify_one();
    }
}

/// `answer` if the deadline has not passed, else nothing: an answer that comes after the time is
/// not an answer (spec §11 L1.1: "still unresolved when that time ends").
#[must_use]
pub fn on_time<T>(answer: T, deadline: Instant) -> Option<T> {
    (Instant::now() < deadline).then_some(answer)
}

/// What the walker thread sends the deciding thread.
enum Walked {
    Path(PathBuf),
    End(WalkEnd),
}

/// A view of `inner` in which every look at the file system runs on a worker thread, one of the
/// shared [`Threads`], and the deciding thread waits for it only until `deadline` (spec §11 L1.1:
/// "spends at most 8 seconds of a decision's 10 doing so"). A look still unfinished then is
/// answered with the cautious answer and noted against its path ([`Fs::cut`]): the path is one
/// Lunchroom could not check. The deciding thread never blocks in the file system past the
/// deadline; a worker stuck in a call (a stalled mount) is left behind, and keeps its place
/// among the threads until the call ends.
pub struct Deadlined<F: Fs + Send + Sync + ?Sized + 'static> {
    inner: Arc<F>,
    deadline: Instant,
    threads: Arc<Threads>,
    cut: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
}

impl<F: Fs + Send + Sync + ?Sized + 'static> std::fmt::Debug for Deadlined<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deadlined")
            .field("deadline", &self.deadline)
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl<F: Fs + Send + Sync + ?Sized + 'static> Deadlined<F> {
    /// `inner`, bounded at `deadline`, on the process's own threads.
    #[must_use]
    pub fn new(inner: Arc<F>, deadline: Instant) -> Self {
        Self::with_threads(inner, deadline, Threads::global().clone())
    }

    /// As [`Deadlined::new`], on the given set of threads.
    #[must_use]
    pub fn with_threads(inner: Arc<F>, deadline: Instant, threads: Arc<Threads>) -> Self {
        Self {
            inner,
            deadline,
            threads,
            cut: std::sync::Mutex::new(std::collections::BTreeSet::new()),
        }
    }

    fn mark(&self, p: &Path) {
        if let Ok(mut cut) = self.cut.lock() {
            cut.insert(p.to_path_buf());
        }
    }

    /// Starts `job` on a worker thread holding a place; false if no place came free by the
    /// deadline or no thread could be made.
    fn start(&self, job: impl FnOnce(Arc<F>) + Send + 'static) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        let Some(slot) = self.threads.take_until(self.deadline) else {
            return false;
        };
        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("lunchroom-look".to_owned())
            .spawn(move || {
                // A panic in the view is an unfinished look (fail closed); the place is freed.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(inner)));
                drop(slot);
            })
            .is_ok()
    }

    /// Runs `f` on a worker and waits for it until the deadline: `None` if it did not finish.
    fn look<T: Send + 'static>(&self, f: impl FnOnce(&F) -> T + Send + 'static) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        let deadline = self.deadline;
        if !self.start(move |inner| {
            // An answer that comes after the deadline is not sent: it was not made in time.
            if let Some(answer) = on_time(f(&inner), deadline) {
                let _ = tx.send(answer);
            }
        }) {
            return None;
        }
        // A message that arrives in the instant the deadline does is checked against the clock.
        rx.recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
            .ok()
            .and_then(|answer| on_time(answer, deadline))
    }

    /// Runs `f` on the calling thread, which is a worker of the caller's own (a place among the
    /// threads already held), under this view's deadline: not started once the deadline has
    /// passed, and its answer is `None` if it came after it. The looks the guard makes in parallel
    /// run through this, so they are bounded by the same clock as the looks made one at a time.
    pub fn here<T>(&self, f: impl FnOnce(&F) -> T) -> Option<T> {
        if Instant::now() >= self.deadline {
            return None;
        }
        on_time(f(&self.inner), self.deadline)
    }
}

impl<F: Fs + Send + Sync + ?Sized + 'static> Fs for Deadlined<F> {
    fn real(&self, p: &Path) -> PathBuf {
        let q = p.to_path_buf();
        self.look(move |f| f.real(&q)).unwrap_or_else(|| {
            self.mark(p);
            p.to_path_buf()
        })
    }
    fn node(&self, p: &Path) -> Node {
        let q = p.to_path_buf();
        self.look(move |f| f.node(&q)).unwrap_or_else(|| {
            self.mark(p);
            Node::Unreadable
        })
    }
    fn resolve(&self, p: &Path) -> Resolved {
        let q = p.to_path_buf();
        self.look(move |f| f.resolve(&q)).unwrap_or_else(|| {
            self.mark(p);
            Resolved {
                path: p.to_path_buf(),
                complete: false,
            }
        })
    }
    fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline || self.inner.out_of_time()
    }
    fn cut(&self, p: &Path) -> bool {
        self.cut.lock().map_or(true, |c| c.contains(p)) || self.inner.cut(p)
    }
    fn descends_from(&self, pid: u32, root: u32) -> bool {
        // A view that cannot tell says no.
        self.look(move |f| f.descends_from(pid, root))
            .unwrap_or(false)
    }
    fn is_dir(&self, p: &Path) -> bool {
        let q = p.to_path_buf();
        // Unknown: taken as a folder, so a search is made of it and its own cut is noted.
        self.look(move |f| f.is_dir(&q)).unwrap_or_else(|| {
            self.mark(p);
            true
        })
    }
    fn exists(&self, p: &Path) -> bool {
        let q = p.to_path_buf();
        self.look(move |f| f.exists(&q)).unwrap_or_else(|| {
            self.mark(p);
            true
        })
    }
    fn expand(&self, pattern: &Path) -> Expansion {
        let q = pattern.to_path_buf();
        self.look(move |f| f.expand(&q)).unwrap_or_else(|| {
            self.mark(pattern);
            Expansion {
                matches: Vec::new(),
                capped: true,
            }
        })
    }
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        // The walk runs on a worker and sends each path it visits; the visitor runs here. A full
        // channel holds the worker back; a visitor that stops, or a call that returns, drops the
        // receiver, which ends the walk at its next path.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Walked>(256);
        let d = dir.to_path_buf();
        if !self.start(move |f| {
            let end = f.walk(&d, &mut |p| tx.send(Walked::Path(p.to_path_buf())).is_err());
            let _ = tx.send(Walked::End(end));
        }) {
            self.mark(dir);
            return WalkEnd::Capped;
        }
        loop {
            // A stream that never runs dry must not hold the decision: the time is read for every
            // path, not only when the stream is empty.
            if Instant::now() >= self.deadline {
                self.mark(dir);
                return WalkEnd::Capped;
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Walked::Path(p)) => {
                    if visit(&p) {
                        return WalkEnd::Stopped;
                    }
                }
                Ok(Walked::End(end)) => return end,
                Err(_) => {
                    self.mark(dir);
                    return WalkEnd::Capped;
                }
            }
        }
    }
    fn script(&self, p: &Path) -> Script {
        let q = p.to_path_buf();
        self.look(move |f| f.script(&q)).unwrap_or_else(|| {
            self.mark(p);
            Script::Unreadable
        })
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        let q = p.to_path_buf();
        // Unknown: judged as a script, which `script` then refuses as unreadable.
        self.look(move |f| f.runs_as_script(&q)).unwrap_or_else(|| {
            self.mark(p);
            true
        })
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        let q = prog.to_owned();
        self.look(move |f| f.which(&q)).unwrap_or_else(|| {
            self.mark(Path::new(prog));
            None
        })
    }
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        let q = p.to_path_buf();
        self.look(move |f| f.file_id(&q)).unwrap_or_else(|| {
            self.mark(p);
            None
        })
    }
}

/// Resolves every path of a call on parallel threads (spec §11 L1.1): each path on one thread, at
/// most [`RESOLVE_THREADS`] threads at a time, all sharing the deadline `budget` from now. The
/// result holds one [`Resolution`] per path, in order.
#[must_use]
pub fn resolve_many<F: Fs + Send + Sync + ?Sized + 'static>(
    fs: &Arc<F>,
    paths: &[PathBuf],
    budget: Duration,
) -> Vec<Resolution> {
    resolve_many_until(fs, paths, Instant::now() + budget, RESOLVE_THREADS)
}

/// As [`resolve_many`], with an explicit `deadline` and thread limit (at least 1), on the
/// process's own threads ([`Threads::global`]).
#[must_use]
pub fn resolve_many_until<F: Fs + Send + Sync + ?Sized + 'static>(
    fs: &Arc<F>,
    paths: &[PathBuf],
    deadline: Instant,
    max_threads: usize,
) -> Vec<Resolution> {
    resolve_many_on(Threads::global(), fs, paths, deadline, max_threads)
}

/// As [`resolve_many_until`], on the given set of threads: a worker takes a place among them and
/// keeps it until it ends, so a worker stuck in a call counts against the cap of the whole
/// process, across calls (L1.1: at most 16 threads at a time).
///
/// The workers are detached threads that report over a channel, and the call returns at the
/// deadline with every path not yet reported as [`Resolution::Unresolved`]: a path whose
/// resolution was cut short by the deadline, that the deadline reached before its turn, or that a
/// worker is still stuck on is one Lunchroom could not check, never one reported as resolved.
///
/// Spec §11 L1.1: the guard is a short-lived process that exits after one decision, so it does not
/// wait for a worker stuck in a file-system call (a hung mount). That is the one case where a
/// thread outlives this call.
#[must_use]
pub fn resolve_many_on<F: Fs + Send + Sync + ?Sized + 'static>(
    threads: &Arc<Threads>,
    fs: &Arc<F>,
    paths: &[PathBuf],
    deadline: Instant,
    max_threads: usize,
) -> Vec<Resolution> {
    let fs = Arc::clone(fs);
    let job = move |p: &PathBuf| resolve_bounded(&fs, p, deadline);
    let job: Arc<Job<PathBuf, Resolved>> = Arc::new(job);
    map_on(threads, paths, deadline, max_threads, &job)
        .into_iter()
        .map(|r| r.map_or(Resolution::Unresolved, Resolution::Done))
        .collect()
}

/// Resolves `p` as [`Fs::resolve`] does, looking at no further part once `deadline` has passed:
/// `None` if the deadline cut the resolution short (the path is then one Lunchroom could not
/// check), `Some` if it ran to its end, complete or not.
#[must_use]
pub fn resolve_bounded<F: Fs + ?Sized>(
    fs: &Arc<F>,
    p: &Path,
    deadline: Instant,
) -> Option<Resolved> {
    let view = Bounded {
        inner: Arc::clone(fs),
        deadline,
        cut: AtomicBool::new(false),
    };
    let r = view.resolve(p);
    (!view.cut.load(Ordering::SeqCst)).then_some(r)
}

/// What one worker of [`map_on`] does: takes the next item, runs the job on it and reports. No item
/// is started once the deadline has passed (a worker past it ends, and its place is freed), and an
/// answer that came after it is reported as none. A panic in the job leaves the item unanswered.
fn work<I, O>(
    list: &[I],
    next: &AtomicUsize,
    deadline: Instant,
    job: &Job<I, O>,
    tx: &std::sync::mpsc::Sender<(usize, Option<O>)>,
) {
    loop {
        let i = next.fetch_add(1, Ordering::SeqCst);
        let Some(item) = list.get(i) else { break };
        if Instant::now() >= deadline {
            break;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(item)))
            .unwrap_or(None)
            .and_then(|answer| on_time(answer, deadline));
        if tx.send((i, result)).is_err() {
            break; // the call has returned
        }
    }
}

/// A job [`map_on`] runs for each item: its answer, or `None` for none.
pub type Job<I, O> = dyn Fn(&I) -> Option<O> + Send + Sync;

/// Runs `job` for every item on parallel threads, each item on one thread, the threads taken from
/// `threads` (so counted with every other look of the process) and at most `max_threads` of them,
/// all sharing `deadline`. The result holds each item's answer in order, `None` for an item whose
/// job gave none or was not finished by the deadline. Detached workers report over a channel and
/// the call returns at the deadline without joining a worker stuck in a call (L1.1).
#[must_use]
pub fn map_on<I, O>(
    threads: &Arc<Threads>,
    items: &[I],
    deadline: Instant,
    max_threads: usize,
    job: &Arc<Job<I, O>>,
) -> Vec<Option<O>>
where
    I: Clone + Send + Sync + 'static,
    O: Send + 'static,
{
    let n = items.len();
    let mut out: Vec<Option<O>> = (0..n).map(|_| None).collect();
    if n == 0 || Instant::now() >= deadline {
        return out;
    }
    let list: Arc<Vec<I>> = Arc::new(items.to_vec());
    let next = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Option<O>)>();
    let mut spawned = 0usize;
    for k in 0..n.min(max_threads.max(1)) {
        // The first worker waits for a place until the deadline; the others take what is free.
        let slot = if k == 0 {
            threads.take_until(deadline)
        } else {
            threads.try_take()
        };
        let Some(slot) = slot else { break };
        let (job, list, next, tx) = (Arc::clone(job), list.clone(), next.clone(), tx.clone());
        let worker = move || {
            let _place = slot; // held until this worker ends
            work(&list, &next, deadline, &*job, &tx);
        };
        if std::thread::Builder::new()
            .name(format!("lunchroom-resolve-{k}"))
            .spawn(worker)
            .is_err()
        {
            break;
        }
        spawned += 1;
    }
    drop(tx);
    if spawned == 0 {
        return out; // no thread could be made: nothing was checked
    }
    let mut reported = 0usize;
    while reported < n {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok((i, r)) => {
                out[i] = r;
                reported += 1;
            }
            Err(_) => break,
        }
    }
    // Whatever arrived in the same instant the deadline did.
    while let Ok((i, r)) = rx.try_recv() {
        out[i] = r;
    }
    out
}

/// Shell wildcard matching of one name: `*`, `?` and `[...]` (with `!` or `^` and ranges). A name
/// starting with `.` matches only a pattern starting with `.`.
#[must_use]
pub fn fnmatch(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    match_from(&p, &n)
}

/// What the pattern at one place does with one character of the name.
enum Step {
    /// A `*`: matches any run of characters, tried shortest first.
    Star,
    /// Matches this character; the pattern goes on at this place.
    One(usize),
    /// Does not match.
    No,
}

fn step(p: &[char], at: usize, c: char) -> Step {
    match p.get(at) {
        Some('*') => Step::Star,
        Some('?') => Step::One(at + 1),
        Some('[') => match class(&p[at..]) {
            Some((len, set)) if set(c) => Step::One(at + len),
            None if c == '[' => Step::One(at + 1),
            Some(_) | None => Step::No,
        },
        Some(lit) if *lit == c => Step::One(at + 1),
        Some(_) | None => Step::No,
    }
}

/// Matches the whole of `n` against `p`, remembering only the last `*` to go back to, so the work
/// is at most the product of the two lengths. (A match that tried every split at every `*` takes
/// time exponential in the number of stars: `a*a*a*a*a*b` against a long run of `a`, or a patch
/// text read as a wildcard, would keep the guard past its time.)
fn match_from(p: &[char], n: &[char]) -> bool {
    let (mut pi, mut ni) = (0usize, 0usize);
    // After the last `*` seen: the place in the pattern after it, and the place in the name it has
    // matched up to.
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        match step(p, pi, n[ni]) {
            Step::Star => {
                star = Some((pi + 1, ni));
                pi += 1;
                continue;
            }
            Step::One(next) => {
                pi = next;
                ni += 1;
                continue;
            }
            Step::No => {}
        }
        match star {
            Some((after, matched)) => {
                star = Some((after, matched + 1));
                pi = after;
                ni = matched + 1;
            }
            None => return false,
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

/// A `[...]` class at the start of `p`: its length and its test, or `None` if unterminated.
fn class(p: &[char]) -> Option<(usize, impl Fn(char) -> bool)> {
    let mut i = 1;
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let start = i;
    if p.get(i) == Some(&']') {
        i += 1;
    }
    while p.get(i).is_some_and(|c| *c != ']') {
        i += 1;
    }
    p.get(i)?;
    let body: Vec<char> = p[start..i].to_vec();
    let test = move |c: char| {
        let mut hit = false;
        let mut k = 0;
        while k < body.len() {
            if body.get(k + 1) == Some(&'-') && k + 2 < body.len() {
                hit |= body[k] <= c && c <= body[k + 2];
                k += 3;
            } else {
                hit |= body[k] == c;
                k += 1;
            }
        }
        hit != negate
    };
    Some((i + 1, test))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A file system held in a map, for resolution cases that need no real folders: a path not in
    /// the map is missing.
    #[derive(Default)]
    struct MapFs(HashMap<PathBuf, Node>);

    impl MapFs {
        fn with(plain: &[&str], links: &[(&str, &str)]) -> Self {
            let mut m = HashMap::new();
            for p in plain {
                m.insert(PathBuf::from(p), Node::Plain);
            }
            for (l, t) in links {
                m.insert(PathBuf::from(l), Node::Link(PathBuf::from(t)));
            }
            Self(m)
        }
    }

    impl Fs for MapFs {
        fn real(&self, p: &Path) -> PathBuf {
            p.to_path_buf()
        }
        fn node(&self, p: &Path) -> Node {
            self.0.get(p).cloned().unwrap_or(Node::Missing)
        }
        fn is_dir(&self, _: &Path) -> bool {
            false
        }
        fn exists(&self, _: &Path) -> bool {
            false
        }
        fn expand(&self, _: &Path) -> Expansion {
            Expansion::default()
        }
        fn walk(&self, _: &Path, _: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
            WalkEnd::Done
        }
        fn script(&self, _: &Path) -> Script {
            Script::Unreadable
        }
        fn runs_as_script(&self, _: &Path) -> bool {
            false
        }
        fn which(&self, _: &str) -> Option<PathBuf> {
            None
        }
    }

    fn resolved(fs: &MapFs, p: &str) -> (String, bool) {
        let r = fs.resolve(Path::new(p));
        (r.path.to_string_lossy().into_owned(), r.complete)
    }

    // §11 L1.1 "Paths are resolved (symlinks, wildcards) before checking": `..` leaves the folder
    // a link led to, not the folder holding the link (review finding 2).
    #[test]
    fn resolve_follows_a_link_before_a_parent_part() {
        let fs = MapFs::with(
            &[
                "/project",
                "/home",
                "/home/u",
                "/home/u/.claude",
                "/home/u/.claude/projects",
            ],
            &[("/project/link", "/home/u/.claude/projects")],
        );
        assert_eq!(
            resolved(&fs, "/project/link/../history.jsonl"),
            ("/home/u/.claude/history.jsonl".to_owned(), true)
        );
        // Without a link in front, `..` is the folder above.
        assert_eq!(
            resolved(&fs, "/project/../home/u"),
            ("/home/u".to_owned(), true)
        );
        // Beyond the root stays at the root, as `/..` does.
        assert_eq!(resolved(&fs, "/../x"), ("/x".to_owned(), true));
    }

    // L1.1: relative targets are read from the folder holding the link; a chain of links is
    // followed to its end; an absolute target restarts at the root.
    #[test]
    fn resolve_follows_relative_and_chained_links() {
        let fs = MapFs::with(
            &["/a", "/a/b", "/a/b/c", "/z", "/t", "/t/dir"],
            &[
                ("/a/rel", "b/c"),
                ("/a/up", "../z"),
                ("/a/l1", "l2"),
                ("/a/l2", "/t/dir"),
                ("/a/b/back", "../../z"),
            ],
        );
        assert_eq!(resolved(&fs, "/a/rel/x"), ("/a/b/c/x".to_owned(), true));
        assert_eq!(resolved(&fs, "/a/up/f"), ("/z/f".to_owned(), true));
        assert_eq!(resolved(&fs, "/a/l1/f"), ("/t/dir/f".to_owned(), true));
        // A link's target is read from the folder the link sits in.
        assert_eq!(resolved(&fs, "/a/b/back/f"), ("/z/f".to_owned(), true));
        // `..` after a relative link leaves the target's folder.
        assert_eq!(resolved(&fs, "/a/rel/../g"), ("/a/b/g".to_owned(), true));
    }

    // L1.1: a destination that does not exist yet keeps its name; a dangling link is followed to
    // the file a write would create.
    #[test]
    fn resolve_keeps_a_destination_that_does_not_exist() {
        let fs = MapFs::with(
            &["/w", "/home", "/home/u", "/home/u/.claude"],
            &[("/w/new", "/home/u/.claude/settings.json")],
        );
        assert_eq!(
            resolved(&fs, "/w/file.txt"),
            ("/w/file.txt".to_owned(), true)
        );
        assert_eq!(
            resolved(&fs, "/w/nodir/deeper/file.txt"),
            ("/w/nodir/deeper/file.txt".to_owned(), true)
        );
        assert_eq!(
            resolved(&fs, "/w/new"),
            ("/home/u/.claude/settings.json".to_owned(), true)
        );
        // `.` parts are dropped.
        assert_eq!(
            resolved(&fs, "/w/./file.txt"),
            ("/w/file.txt".to_owned(), true)
        );
    }

    // L1.1, fail closed: what cannot be resolved is reported incomplete, never guessed complete.
    #[test]
    fn resolve_reports_what_it_could_not_resolve() {
        let mut fs = MapFs::with(
            &["/a"],
            &[
                ("/a/loop", "/a/loop"),
                ("/a/ping", "pong"),
                ("/a/pong", "ping"),
            ],
        );
        assert!(!fs.resolve(Path::new("/a/loop")).complete);
        assert!(!fs.resolve(Path::new("/a/ping/x")).complete);
        // A `..` after a part that does not exist: a command may make it a link first.
        assert!(!fs.resolve(Path::new("/a/missing/../x")).complete);
        // A part that cannot be looked at.
        fs.0.insert(PathBuf::from("/a/locked"), Node::Unreadable);
        assert!(!fs.resolve(Path::new("/a/locked/x")).complete);
        // The limit is 40 links: a chain of 40 resolves, 41 does not.
        let links: Vec<(String, String)> = (0..41)
            .map(|i| (format!("/c/l{i}"), format!("l{}", i + 1)))
            .collect();
        let refs: Vec<(&str, &str)> = links
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let chain = MapFs::with(&["/c", "/c/l41"], &refs);
        assert!(chain.resolve(Path::new("/c/l1")).complete);
        assert!(!chain.resolve(Path::new("/c/l0")).complete);
    }

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A throwaway folder under the build's `target` folder, removed on drop.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let base = crate::test_target().join("fs-tests");
            std::fs::create_dir_all(&base).unwrap_or_default();
            let base = std::fs::canonicalize(&base).unwrap_or(base);
            let root = base.join(format!("{}-{n}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap_or_default();
            Self(root)
        }
        fn put(&self, rel: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap_or(&self.0)).unwrap_or_default();
            std::fs::write(p, "x").unwrap_or_default();
        }
        fn link(&self, name: &str, target: &str) {
            let p = self.0.join(name);
            std::fs::create_dir_all(p.parent().unwrap_or(&self.0)).unwrap_or_default();
            std::os::unix::fs::symlink(target, p).unwrap_or_default();
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap_or_default();
        }
    }

    // L1.1: links the system follows one hop at a time can reach a file by a path too long to
    // spell. The system opens it; what `resolve` cannot spell is incomplete, never a path it names.
    #[test]
    fn a_path_too_long_to_spell_is_incomplete() {
        let t = Tmp::new();
        t.put("store/secret");
        let chain = vec!["e".repeat(200); 20].join("/");
        let script = "mkdir -p \"$1\" && ln -s \"$1\" H && mkdir -p \"H/$1\" && ln -s \"$1\" H/H2 && \
                      ln -s \"$2\" H/H2/top && cat H/H2/top/store/secret";
        let opened = std::process::Command::new("/bin/sh")
            .args(["-c", script, "sh", &chain, &t.0.to_string_lossy()])
            .current_dir(&t.0)
            .output();
        assert!(opened.is_ok_and(|o| o.stdout == b"x"));
        let got = OsFs::new(BUDGET).resolve(&t.0.join("H/H2/top/store/secret"));
        assert!(!got.complete, "{}", got.path.display());
    }

    // L1.1: the real file system is the oracle: for every path that exists, `resolve` reaches the
    // file the operating system opens (`canonicalize`), `..` after a link included.
    #[test]
    fn resolve_agrees_with_the_operating_system() {
        let t = Tmp::new();
        t.put("store/projects/t.jsonl");
        t.put("store/history.jsonl");
        t.put("proj/a.txt");
        t.link("proj/link", "../store/projects");
        t.link("proj/abs", &t.0.join("store").to_string_lossy());
        t.link("proj/chain", "link");
        t.link("proj/up", "..");
        let fs = OsFs::new(BUDGET);
        for rel in [
            "proj/link/../history.jsonl",
            "proj/link/t.jsonl",
            "proj/abs/../store/history.jsonl",
            "proj/chain/../history.jsonl",
            "proj/up/store/history.jsonl",
            "proj/up/proj/a.txt",
            "proj/./a.txt",
            "proj/link/../../proj/a.txt",
        ] {
            let p = t.0.join(rel);
            let want = std::fs::canonicalize(&p);
            assert!(want.is_ok(), "{rel}: {want:?}");
            let Ok(want) = want else { return };
            let got = fs.resolve(&p);
            assert_eq!(got.path, want, "{rel}");
            assert!(got.complete, "{rel}");
        }
        // The lexical form is another file, which is the defect resolve closes.
        assert_ne!(
            t.0.join("proj/history.jsonl"),
            fs.resolve(&t.0.join("proj/link/../history.jsonl")).path
        );
        // A destination that does not exist: the existing folder resolves, the name stays.
        assert_eq!(
            fs.resolve(&t.0.join("proj/link/../new.txt")).path,
            t.0.join("store/new.txt")
        );
        // A loop on the real file system.
        t.link("proj/self", "self");
        assert!(!fs.resolve(&t.0.join("proj/self/x")).complete);
        assert_eq!(fs.node(&t.0.join("proj/missing")), Node::Missing);
        assert_eq!(fs.node(&t.0.join("proj/a.txt/x")), Node::Missing);
        assert_eq!(fs.node(&t.0.join("proj/a.txt")), Node::Plain);
        assert_eq!(
            fs.node(&t.0.join("proj/link")),
            Node::Link(PathBuf::from("../store/projects"))
        );
    }

    // §11 L1.1 (review finding 7): a walk enters every folder, the ones that usually hold fetched
    // or built files included, because a recursive search reads them; those are walked last.
    #[test]
    fn walk_enters_every_folder_and_the_usual_bulk_ones_last() {
        let t = Tmp::new();
        t.put("src/main.rs");
        for dir in LAST {
            t.put(&format!("{dir}/deep/.env"));
        }
        t.link("pnpm/node_modules", "../store");
        t.put("store/pkg/id_rsa");
        let fs = OsFs::new(BUDGET);
        let mut seen: Vec<PathBuf> = Vec::new();
        let end = fs.walk(&t.0, &mut |p| {
            seen.push(p.to_path_buf());
            false
        });
        assert_eq!(end, WalkEnd::Done);
        for dir in LAST {
            assert!(seen.contains(&t.0.join(dir).join("deep/.env")), "{dir}");
        }
        assert!(seen.contains(&t.0.join("store/pkg/id_rsa")));
        let at = |p: &Path| seen.iter().position(|s| s == p);
        let main = at(&t.0.join("src/main.rs"));
        let bulk = at(&t.0.join("node_modules/deep/.env"));
        assert!(main.is_some() && bulk.is_some() && main < bulk);
    }

    // L1.1, fail closed: a walk whose budget ran out says so, so the caller does not read the
    // part it saw as the whole.
    #[test]
    fn walk_out_of_budget_is_capped() {
        let t = Tmp::new();
        t.put("a/b/c.txt");
        let fs = OsFs::new(Duration::ZERO);
        assert_eq!(fs.walk(&t.0, &mut |_| false), WalkEnd::Capped);
    }

    #[test]
    fn fnmatch_classes_and_dots() {
        assert!(fnmatch("*.pem", "a.pem"));
        assert!(fnmatch("cod?x", "codex"));
        assert!(fnmatch("c[a-z]dex", "codex"));
        assert!(!fnmatch("c[!o]dex", "codex"));
        assert!(fnmatch("c[^x]dex", "codex"));
        assert!(!fnmatch("*", ".env"));
        assert!(fnmatch(".e*", ".env"));
        assert!(fnmatch("a[b", "a[b"));
        assert!(fnmatch("[]]x", "]x"));
    }

    // G1-1: a pattern with many stars against a name that nearly matches took time exponential in
    // the stars when every split was tried; it is decided at once, with the right answer, and
    // the cases where backing up to the last `*` matters still match.
    #[test]
    fn fnmatch_with_many_stars_is_decided_at_once() {
        let started = std::time::Instant::now();
        let pattern = format!("{}b", "a*".repeat(40));
        let name = "a".repeat(200);
        assert!(!fnmatch(&pattern, &name));
        assert!(fnmatch(&pattern, &format!("{name}b")));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(fnmatch("*a*b*c", "xxabxxbxcxxc"));
        assert!(fnmatch("*.rs", "main.rs"));
        assert!(!fnmatch("*.rs", "main.rsx"));
        assert!(fnmatch("a**b", "ab"));
        assert!(fnmatch("**", "anything"));
        assert!(fnmatch("*", ""));
        assert!(!fnmatch("?", ""));
    }

    // G1-1: a view whose time has run out looks at no more parts of a path; the result is
    // incomplete (the file may be anywhere) and keeps the rest as written.
    #[test]
    fn resolve_out_of_time_is_incomplete() {
        let t = Tmp::new();
        t.put("a/b/c.txt");
        let fs = OsFs::new(Duration::ZERO);
        let r = fs.resolve(&t.0.join("a/b/c.txt"));
        assert!(!r.complete);
        assert_eq!(r.path, t.0.join("a/b/c.txt"));
        let fresh = OsFs::new(BUDGET);
        assert!(fresh.resolve(&t.0.join("a/b/c.txt")).complete);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // test code
mod parallel_tests {
    use super::*;
    use std::sync::Mutex;

    /// A stand-in whose every `node` question takes `delay`, counting the questions and the most
    /// asked at one moment. A name starting `loop` is a link to itself; any other path is plain.
    struct SlowFs {
        delay: Duration,
        now: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        threads: Mutex<Vec<std::thread::ThreadId>>,
        /// A path whose look blocks until the sender of this channel is dropped.
        stuck: Option<(PathBuf, Mutex<std::sync::mpsc::Receiver<()>>)>,
    }

    impl SlowFs {
        fn new(delay: Duration) -> Arc<Self> {
            Arc::new(Self::raw(delay))
        }

        fn raw(delay: Duration) -> Self {
            Self {
                stuck: None,
                delay,
                now: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                threads: Mutex::new(Vec::new()),
            }
        }
    }

    impl Fs for SlowFs {
        fn real(&self, p: &Path) -> PathBuf {
            p.to_path_buf()
        }
        fn node(&self, p: &Path) -> Node {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            if let Some((blocked, rx)) = &self.stuck
                && p == blocked
            {
                // Blocks "forever": until the test drops the sender.
                let _ = rx.lock().unwrap().recv();
            }
            std::thread::sleep(self.delay);
            self.now.fetch_sub(1, Ordering::SeqCst);
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with("loop") {
                Node::Link(p.to_path_buf())
            } else {
                Node::Plain
            }
        }
        fn is_dir(&self, _: &Path) -> bool {
            false
        }
        fn exists(&self, _: &Path) -> bool {
            false
        }
        fn expand(&self, _: &Path) -> Expansion {
            Expansion::default()
        }
        fn walk(&self, _: &Path, _: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
            WalkEnd::Done
        }
        fn script(&self, _: &Path) -> Script {
            Script::Unreadable
        }
        fn runs_as_script(&self, _: &Path) -> bool {
            false
        }
        fn which(&self, _: &str) -> Option<PathBuf> {
            None
        }
    }

    fn paths(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("/p{i}"))).collect()
    }

    fn all_done(r: &[Resolution]) -> bool {
        r.iter()
            .all(|x| matches!(x, Resolution::Done(d) if d.complete))
    }

    #[test]
    fn budget_is_eight_seconds_and_threads_sixteen() {
        assert_eq!(BUDGET, Duration::from_secs(8));
        assert_eq!(RESOLVE_THREADS, 16);
    }

    /// The one test that runs at the spec's real scale: 50 paths at 1 s each, the 8 s budget.
    /// Four rounds of 16 take about 4 s; it needs the 16 threads (one thread would take 50 s).
    #[test]
    fn fifty_paths_at_one_second_resolve_within_budget_on_at_most_sixteen_threads() {
        let fs = SlowFs::new(Duration::from_secs(1));
        let list = paths(50);
        let start = Instant::now();
        let got = resolve_many_on(&Threads::new(16), &fs, &list, Instant::now() + BUDGET, 16);
        let took = start.elapsed();
        assert_eq!(got.len(), 50);
        assert!(all_done(&got), "every path resolved");
        assert!(took < BUDGET, "took {took:?}");
        assert_eq!(fs.calls.load(Ordering::SeqCst), 50);
        assert_eq!(
            fs.peak.load(Ordering::SeqCst),
            16,
            "16 ran at once, never more"
        );
        assert!(
            fs.threads
                .lock()
                .unwrap()
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                <= 16
        );
        assert_eq!(
            got[7],
            Resolution::Done(Resolved {
                path: PathBuf::from("/p7"),
                complete: true
            })
        );
    }

    #[test]
    fn a_path_unresolved_at_the_deadline_is_unresolved_not_resolved() {
        // The first path has 3 parts: the look at its third part would come after the deadline.
        let fs = SlowFs::new(Duration::from_millis(100));
        let list = vec![PathBuf::from("/a/b/d"), PathBuf::from("/c")];
        let got = resolve_many_on(
            &Threads::new(16),
            &fs,
            &list,
            Instant::now() + Duration::from_millis(150),
            16,
        );
        assert_eq!(got[0], Resolution::Unresolved);
        assert!(matches!(got[1], Resolution::Done(_)));
    }

    #[test]
    fn paths_the_deadline_reaches_before_their_turn_are_unresolved() {
        let fs = SlowFs::new(Duration::from_millis(200));
        let list = paths(6);
        let got = resolve_many_on(
            &Threads::new(2),
            &fs,
            &list,
            Instant::now() + Duration::from_millis(300),
            2,
        );
        // Two threads: paths 0 and 1 end at 200 ms, 2 and 3 start before the 300 ms deadline and
        // end at 400 ms (a one-part path has no later part to cut), 4 and 5 start after it.
        assert!(matches!(got[0], Resolution::Done(_)));
        assert!(matches!(got[1], Resolution::Done(_)));
        assert_eq!(got[4], Resolution::Unresolved);
        assert_eq!(got[5], Resolution::Unresolved);
        // Time for a worker, had it been let go on to path 4 or 5, to have asked.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(fs.calls.load(Ordering::SeqCst), 4);
    }

    // A resolution the deadline cut short is `None`; one that ran to its end is `Some`, complete
    // or not (a link loop is a resolution that ended, and cannot be followed).
    #[test]
    fn resolve_bounded_tells_a_cut_from_a_resolution_that_cannot_be_followed() {
        let fs = SlowFs::new(Duration::from_millis(100));
        let until = Instant::now() + Duration::from_millis(150);
        // Three parts at 100 ms each: the third is not looked at.
        assert_eq!(resolve_bounded(&fs, Path::new("/a/b/c"), until), None);
        let fs = SlowFs::new(Duration::ZERO);
        let until = Instant::now() + Duration::from_secs(5);
        let plain = resolve_bounded(&fs, Path::new("/a"), until);
        assert!(plain.is_some_and(|r| r.complete));
        let looped = resolve_bounded(&fs, Path::new("/loop1"), until);
        assert!(looped.is_some_and(|r| !r.complete));
    }

    #[test]
    fn a_path_that_cannot_be_followed_stays_incomplete_not_unresolved() {
        let fs = SlowFs::new(Duration::ZERO);
        let list = vec![PathBuf::from("/loop1"), PathBuf::from("/ok")];
        let got = resolve_many_on(&Threads::new(16), &fs, &list, Instant::now() + BUDGET, 16);
        match &got[0] {
            Resolution::Done(r) => assert!(!r.complete),
            Resolution::Unresolved => panic!("a followed-to-the-end loop is not unresolved"),
        }
        assert!(matches!(&got[1], Resolution::Done(r) if r.complete));
    }

    /// L1.1: the call returns at the deadline although one worker is stuck in a file-system call;
    /// that path is unresolved and the others resolve. The stuck worker counts against the cap.
    #[test]
    fn a_stuck_call_does_not_hold_the_decision() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut raw = SlowFs::raw(Duration::from_millis(10));
        raw.stuck = Some((PathBuf::from("/p3"), Mutex::new(rx)));
        let fs = Arc::new(raw);
        let list = paths(40);
        let start = Instant::now();
        let got = resolve_many_on(
            &Threads::new(4),
            &fs,
            &list,
            Instant::now() + Duration::from_millis(600),
            4,
        );
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(500) && took < Duration::from_secs(3),
            "{took:?}"
        );
        assert_eq!(got[3], Resolution::Unresolved);
        for (i, r) in got.iter().enumerate() {
            if i != 3 {
                assert!(
                    matches!(r, Resolution::Done(d) if d.complete),
                    "path {i}: {r:?}"
                );
            }
        }
        assert!(
            fs.peak.load(Ordering::SeqCst) <= 4,
            "the stuck thread is one of the 4"
        );
        drop(tx); // let the stuck worker end
    }

    #[test]
    fn zero_one_sixteen_and_seventeen_paths() {
        for (n, peak) in [(0usize, 0usize), (1, 1), (16, 16), (17, 16)] {
            let fs = SlowFs::new(Duration::from_millis(150));
            let got = resolve_many_on(
                &Threads::new(16),
                &fs,
                &paths(n),
                Instant::now() + BUDGET,
                16,
            );
            assert_eq!(got.len(), n);
            assert!(all_done(&got), "{n} paths");
            assert_eq!(fs.calls.load(Ordering::SeqCst), n);
            assert_eq!(fs.peak.load(Ordering::SeqCst), peak, "{n} paths");
        }
    }

    #[test]
    fn a_deadline_already_passed_looks_at_nothing() {
        let fs = SlowFs::new(Duration::ZERO);
        let got = resolve_many_on(&Threads::new(16), &fs, &paths(20), Instant::now(), 16);
        assert_eq!(got, vec![Resolution::Unresolved; 20]);
        // Time for a thread, had one been started, to have asked.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fs.calls.load(Ordering::SeqCst), 0);
    }

    /// Counts, when a thread that touched it ends, that thread's end.
    struct Ended(std::sync::Arc<AtomicUsize>);
    impl Drop for Ended {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn no_thread_outlives_the_call() {
        thread_local! {
            static MARK: std::cell::RefCell<Option<Ended>> = const { std::cell::RefCell::new(None) };
        }
        struct Marking(SlowFs, Arc<AtomicUsize>);
        impl Fs for Marking {
            fn real(&self, p: &Path) -> PathBuf {
                self.0.real(p)
            }
            fn node(&self, p: &Path) -> Node {
                MARK.with(|m| {
                    m.borrow_mut().get_or_insert_with(|| Ended(self.1.clone()));
                });
                self.0.node(p)
            }
            fn is_dir(&self, p: &Path) -> bool {
                self.0.is_dir(p)
            }
            fn exists(&self, p: &Path) -> bool {
                self.0.exists(p)
            }
            fn expand(&self, p: &Path) -> Expansion {
                self.0.expand(p)
            }
            fn walk(&self, d: &Path, v: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
                self.0.walk(d, v)
            }
            fn script(&self, p: &Path) -> Script {
                self.0.script(p)
            }
            fn runs_as_script(&self, p: &Path) -> bool {
                self.0.runs_as_script(p)
            }
            fn which(&self, p: &str) -> Option<PathBuf> {
                self.0.which(p)
            }
        }
        let ended = std::sync::Arc::new(AtomicUsize::new(0));
        let fs = Arc::new(Marking(
            SlowFs::raw(Duration::from_millis(50)),
            ended.clone(),
        ));
        let _ = resolve_many_on(
            &Threads::new(16),
            &fs,
            &paths(40),
            Instant::now() + BUDGET,
            16,
        );
        let used =
            fs.0.threads
                .lock()
                .unwrap()
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len();
        assert!(used > 1 && used <= 16, "{used} threads");
        // Each thread's local value is dropped as the thread ends; all had ended by the return.
        // The workers are detached: each reports its last result, then ends. Wait for the ends.
        let until = Instant::now() + Duration::from_secs(5);
        while ended.load(Ordering::SeqCst) < used && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ended.load(Ordering::SeqCst), used);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // test code
mod deadlined_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, Sender, channel};

    /// A stand-in whose every look at the path `stuck` blocks until the test drops the sender (a
    /// stalled mount), and answers at once for any other path.
    struct StallFs {
        stuck: PathBuf,
        gate: Mutex<Receiver<()>>,
        calls: AtomicUsize,
    }

    fn stall(path: &str) -> (Arc<StallFs>, Sender<()>) {
        let (tx, rx) = channel();
        let fs = StallFs {
            stuck: PathBuf::from(path),
            gate: Mutex::new(rx),
            calls: AtomicUsize::new(0),
        };
        (Arc::new(fs), tx)
    }

    impl StallFs {
        fn look(&self, p: &Path) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if p == self.stuck {
                let _ = self.gate.lock().unwrap().recv();
            }
        }
    }

    impl Fs for StallFs {
        fn real(&self, p: &Path) -> PathBuf {
            self.look(p);
            PathBuf::from("/real").join(p.strip_prefix("/").unwrap_or(p))
        }
        fn node(&self, p: &Path) -> Node {
            self.look(p);
            Node::Plain
        }
        fn is_dir(&self, p: &Path) -> bool {
            self.look(p);
            false
        }
        fn exists(&self, p: &Path) -> bool {
            self.look(p);
            false
        }
        fn expand(&self, pattern: &Path) -> Expansion {
            self.look(pattern);
            Expansion {
                matches: vec![PathBuf::from("/m")],
                capped: false,
            }
        }
        fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
            self.look(dir);
            for n in ["/w1", "/w2", "/w3"] {
                if visit(Path::new(n)) {
                    return WalkEnd::Stopped;
                }
            }
            WalkEnd::Done
        }
        fn script(&self, p: &Path) -> Script {
            self.look(p);
            Script::Text("ok".to_owned())
        }
        fn runs_as_script(&self, p: &Path) -> bool {
            self.look(p);
            false
        }
        fn which(&self, prog: &str) -> Option<PathBuf> {
            self.look(Path::new(prog));
            Some(PathBuf::from("/bin").join(prog))
        }
    }

    type Case = Box<dyn Fn(&Deadlined<StallFs>) -> bool>;

    const STUCK: &str = "/stalled";

    fn within(start: Instant) {
        let took = start.elapsed();
        assert!(
            took < Duration::from_secs(3),
            "the deciding thread waited {took:?}"
        );
    }

    fn view(fs: &Arc<StallFs>, ms: u64) -> Deadlined<StallFs> {
        Deadlined::with_threads(
            fs.clone(),
            Instant::now() + Duration::from_millis(ms),
            Threads::new(8),
        )
    }

    // Fable M9: `real` on a stalled path no longer blocks the deciding thread: it answers at the
    // deadline with the path as written, and the path is noted as cut (one Lunchroom could not
    // check), while a path that answers is not.
    #[test]
    fn real_on_a_stalled_path_returns_at_the_deadline_and_is_cut() {
        let (fs, tx) = stall(STUCK);
        let v = view(&fs, 300);
        assert_eq!(v.real(Path::new("/fine")), PathBuf::from("/real/fine"));
        assert!(!v.cut(Path::new("/fine")));
        let start = Instant::now();
        assert_eq!(v.real(Path::new(STUCK)), PathBuf::from(STUCK));
        within(start);
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "waited for the deadline"
        );
        assert!(v.cut(Path::new(STUCK)));
        assert!(!v.cut(Path::new("/fine")), "the cut is per path");
        drop(tx);
    }

    // Fable M9: every other look (`is_dir`, `exists`, `script`, `resolve`, `node`, `expand`,
    // `walk`, `runs_as_script`, `which`) is bounded the same way, and answered cautiously.
    #[test]
    fn every_look_is_bounded_and_answered_cautiously() {
        let stuck = Path::new(STUCK);
        let cases: Vec<(&str, Case)> = vec![
            ("is_dir", Box::new(|v| v.is_dir(Path::new(STUCK)))),
            ("exists", Box::new(|v| v.exists(Path::new(STUCK)))),
            (
                "script",
                Box::new(|v| v.script(Path::new(STUCK)) == Script::Unreadable),
            ),
            (
                "resolve",
                Box::new(|v| {
                    let r = v.resolve(Path::new(STUCK));
                    !r.complete && r.path == Path::new(STUCK)
                }),
            ),
            (
                "node",
                Box::new(|v| v.node(Path::new(STUCK)) == Node::Unreadable),
            ),
            (
                "expand",
                Box::new(|v| {
                    let e = v.expand(Path::new(STUCK));
                    e.capped && e.matches.is_empty()
                }),
            ),
            (
                "walk",
                Box::new(|v| v.walk(Path::new(STUCK), &mut |_| false) == WalkEnd::Capped),
            ),
            (
                "runs_as_script",
                Box::new(|v| v.runs_as_script(Path::new(STUCK))),
            ),
            ("which", Box::new(|v| v.which(STUCK).is_none())),
        ];
        for (name, case) in cases {
            let (fs, tx) = stall(STUCK);
            let v = view(&fs, 250);
            let start = Instant::now();
            assert!(case(&v), "{name}: the cautious answer");
            within(start);
            assert!(v.cut(stuck), "{name}: the path is noted as cut");
            drop(tx);
        }
    }

    // The looks that answer give the real answers, and a walk's visitor still runs on the deciding
    // thread and can stop it.
    #[test]
    fn looks_that_finish_answer_as_the_view_does() {
        let (fs, tx) = stall(STUCK);
        let v = view(&fs, 2000);
        assert!(!v.is_dir(Path::new("/x")) && !v.exists(Path::new("/x")));
        assert_eq!(v.script(Path::new("/x")), Script::Text("ok".to_owned()));
        assert_eq!(v.expand(Path::new("/x")).matches, vec![PathBuf::from("/m")]);
        assert_eq!(v.which("ls"), Some(PathBuf::from("/bin/ls")));
        let here = std::thread::current().id();
        let mut seen = Vec::new();
        let end = v.walk(Path::new("/x"), &mut |p| {
            assert_eq!(std::thread::current().id(), here, "the visitor runs here");
            seen.push(p.to_path_buf());
            seen.len() == 2
        });
        assert_eq!(end, WalkEnd::Stopped);
        assert_eq!(seen, vec![PathBuf::from("/w1"), PathBuf::from("/w2")]);
        assert_eq!(v.walk(Path::new("/x"), &mut |_| false), WalkEnd::Done);
        assert!(!v.cut(Path::new("/x")));
        drop(tx);
    }

    // Fable M9 and the cap: a thread stuck in a call keeps its place among the 16, across calls
    // and views. With every place held by a stuck thread, a new look starts no thread (the stand-in
    // is not asked) and is answered cautiously when its deadline comes.
    #[test]
    fn stuck_threads_count_against_the_cap_across_calls() {
        let threads = Threads::new(2);
        let (fs, tx) = stall(STUCK);
        for _ in 0..2 {
            let v = Deadlined::with_threads(
                fs.clone(),
                Instant::now() + Duration::from_millis(150),
                threads.clone(),
            );
            let _ = v.real(Path::new(STUCK));
        }
        assert_eq!(threads.in_use(), 2);
        assert_eq!(fs.calls.load(Ordering::SeqCst), 2);
        let late = Deadlined::with_threads(
            fs.clone(),
            Instant::now() + Duration::from_millis(200),
            threads.clone(),
        );
        let start = Instant::now();
        assert_eq!(late.real(Path::new("/fine")), PathBuf::from("/fine"));
        within(start);
        assert!(late.cut(Path::new("/fine")));
        assert_eq!(
            fs.calls.load(Ordering::SeqCst),
            2,
            "no third thread started"
        );
        assert_eq!(threads.in_use(), 2);
        // The stuck calls end: the places are free again and a look is answered.
        drop(tx);
        let until = Instant::now() + Duration::from_secs(5);
        while threads.in_use() > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(threads.in_use(), 0);
        let fresh =
            Deadlined::with_threads(fs.clone(), Instant::now() + Duration::from_secs(2), threads);
        assert_eq!(fresh.real(Path::new("/fine")), PathBuf::from("/real/fine"));
    }

    // The map of a call's paths also takes its places from the shared set: two calls at once never
    // run more than the cap between them.
    #[test]
    fn map_on_takes_its_places_from_the_shared_set() {
        let threads = Threads::new(3);
        let (fs, tx) = stall(STUCK);
        let a = Deadlined::with_threads(
            fs.clone(),
            Instant::now() + Duration::from_millis(150),
            threads.clone(),
        );
        let _ = a.real(Path::new(STUCK)); // one place held by a stuck thread
        let seen = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (s, p) = (seen.clone(), peak.clone());
        let job = move |_: &u32| {
            let now = s.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            s.fetch_sub(1, Ordering::SeqCst);
            Some(())
        };
        let items: Vec<u32> = (0..12).collect();
        let job: Arc<Job<u32, ()>> = Arc::new(job);
        let got = map_on(
            &threads,
            &items,
            Instant::now() + Duration::from_secs(5),
            16,
            &job,
        );
        assert!(got.iter().all(Option::is_some));
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "3 places, one stuck: 2 workers at most"
        );
        drop(tx);
    }

    // The 8 seconds are the decision's, not a view's: a deadline in the past answers every look at
    // once without a thread.
    #[test]
    fn a_deadline_in_the_past_asks_nothing() {
        let (fs, tx) = stall(STUCK);
        let v = Deadlined::with_threads(fs.clone(), Instant::now(), Threads::new(4));
        assert!(v.is_dir(Path::new("/fine")));
        assert_eq!(v.script(Path::new("/fine")), Script::Unreadable);
        assert!(v.cut(Path::new("/fine")));
        // Time for a thread, had one been started, to have asked.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fs.calls.load(Ordering::SeqCst), 0);
        drop(tx);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // test code
mod osfs_cut_tests {
    use super::*;

    // Codex U4 finding 2: the production view says a path was cut only if its own look was cut by
    // the deadline. A path that resolved, or that cannot be followed at all, is not cut, before
    // or after the deadline; a path asked about only after the deadline is.
    #[test]
    fn osfs_cut_is_per_path() {
        let dir = std::env::temp_dir().join(format!("osfs-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plain"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("b"), dir.join("a")).unwrap();
        std::os::unix::fs::symlink(dir.join("a"), dir.join("b")).unwrap();
        let fs = OsFs::until(Instant::now() + Duration::from_millis(300));
        let looped = fs.resolve(&dir.join("a"));
        assert!(!looped.complete, "a link loop cannot be followed");
        assert!(fs.resolve(&dir.join("plain")).complete);
        std::thread::sleep(Duration::from_millis(400));
        assert!(fs.out_of_time());
        assert!(
            !fs.cut(&dir.join("a")),
            "a loop is not a cut look, even after the deadline"
        );
        assert!(!fs.cut(&dir.join("plain")), "a resolved path is not cut");
        let late = fs.resolve(&dir.join("plain"));
        assert!(!late.complete, "no look is made once the time is out");
        assert!(
            fs.cut(&dir.join("plain")),
            "a path asked only after the deadline is cut"
        );
        assert!(
            !fs.cut(&dir.join("b")),
            "a path never asked about is not cut"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The bounded view reports a cut its inner view noted (the inner clock ran out before the
    // view's own deadline): the path is cut, a path never asked about is not.
    #[test]
    fn deadlined_reports_a_cut_the_inner_view_noted() {
        let dir = std::env::temp_dir().join(format!("osfs-cut-d-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plain"), b"x").unwrap();
        let osfs = Arc::new(OsFs::until(Instant::now() + Duration::from_millis(50)));
        std::thread::sleep(Duration::from_millis(100));
        let view = Deadlined::with_threads(
            osfs,
            Instant::now() + Duration::from_secs(5),
            Threads::new(4),
        );
        assert!(!view.resolve(&dir.join("plain")).complete);
        assert!(view.cut(&dir.join("plain")));
        assert!(!view.cut(&dir.join("other")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // test code
mod routing_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, channel};

    /// A view with nothing but a walk: `many` paths, or paths until `give_up` has passed, and a
    /// count of the paths it handed out and of whether it was told to stop.
    struct Walker {
        many: Option<usize>,
        give_up: Duration,
        handed: AtomicUsize,
        told_to_stop: AtomicBool,
        ended: AtomicBool,
    }

    impl Walker {
        fn new(many: Option<usize>, give_up: Duration) -> Arc<Self> {
            Arc::new(Self {
                many,
                give_up,
                handed: AtomicUsize::new(0),
                told_to_stop: AtomicBool::new(false),
                ended: AtomicBool::new(false),
            })
        }
    }

    impl Fs for Walker {
        fn real(&self, p: &Path) -> PathBuf {
            p.to_path_buf()
        }
        fn node(&self, _: &Path) -> Node {
            Node::Plain
        }
        fn is_dir(&self, _: &Path) -> bool {
            true
        }
        fn exists(&self, _: &Path) -> bool {
            true
        }
        fn expand(&self, _: &Path) -> Expansion {
            Expansion::default()
        }
        fn walk(&self, _: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
            let began = Instant::now();
            let end = loop {
                let n = self.handed.fetch_add(1, Ordering::SeqCst);
                if self.many.is_some_and(|m| n >= m) || began.elapsed() > self.give_up {
                    break WalkEnd::Done;
                }
                if visit(Path::new(&format!("/w{n}"))) {
                    self.told_to_stop.store(true, Ordering::SeqCst);
                    break WalkEnd::Stopped;
                }
            };
            self.ended.store(true, Ordering::SeqCst);
            end
        }
        fn script(&self, _: &Path) -> Script {
            Script::Unreadable
        }
        fn runs_as_script(&self, _: &Path) -> bool {
            false
        }
        fn which(&self, _: &str) -> Option<PathBuf> {
            None
        }
    }

    fn wait_for(what: impl Fn() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while !what() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        what()
    }

    // Class 6: a walk that never runs dry (a stream the visitor never stops) must not hold the
    // deciding thread past the deadline. The channel is always ready, so the time is read for every
    // path. The walk ends Capped, the folder is noted as cut, the walker is told to stop when the
    // receiver goes, and its place among the threads is freed.
    #[test]
    fn a_walk_that_never_runs_dry_returns_at_the_deadline() {
        let walker = Walker::new(None, Duration::from_secs(4));
        let threads = Threads::new(4);
        let view = Deadlined::with_threads(
            walker.clone(),
            Instant::now() + Duration::from_millis(300),
            threads.clone(),
        );
        let start = Instant::now();
        let mut seen = 0usize;
        let end = view.walk(Path::new("/dir"), &mut |_| {
            seen += 1;
            // A visitor slower than the stream: the channel is always full, never empty.
            std::thread::sleep(Duration::from_micros(300));
            false
        });
        let took = start.elapsed();
        assert_eq!(end, WalkEnd::Capped);
        assert!(took < Duration::from_millis(1500), "waited {took:?}");
        assert!(
            seen > 256,
            "the stream went past the channel's capacity: {seen}"
        );
        assert!(view.cut(Path::new("/dir")));
        assert!(
            wait_for(|| walker.told_to_stop.load(Ordering::SeqCst)),
            "the walker was told to stop"
        );
        assert!(wait_for(|| threads.in_use() == 0), "its place was freed");
    }

    // Class 6: a long finite stream (more than the channel holds, many times over) is seen whole,
    // in order of arrival, and ends Done: no path is dropped and none is taken for the end.
    #[test]
    fn a_long_walk_is_seen_whole() {
        let walker = Walker::new(Some(100_000), Duration::from_secs(30));
        let view = Deadlined::with_threads(
            walker,
            Instant::now() + Duration::from_secs(20),
            Threads::new(4),
        );
        let mut seen = 0usize;
        let end = view.walk(Path::new("/dir"), &mut |_| {
            seen += 1;
            false
        });
        assert_eq!(end, WalkEnd::Done);
        assert_eq!(seen, 100_000);
        assert!(!view.cut(Path::new("/dir")));
    }

    // Class 2: calls racing for the one set of places. Eight callers map 40 jobs each while four
    // more callers each leave one job stuck; the jobs that run at one moment (stuck ones included)
    // never exceed the cap, nothing deadlocks, every unstuck call finishes whole, and each call
    // with a stuck job returns at its own deadline.
    #[test]
    fn simultaneous_calls_share_the_cap_and_each_returns_at_its_own_deadline() {
        let threads = Threads::new(16);
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (release, gate) = channel::<()>();
        let gate: Arc<Mutex<Receiver<()>>> = Arc::new(Mutex::new(gate));
        let job_of = |stick: Option<u32>| {
            let (now, peak, gate) = (now.clone(), peak.clone(), gate.clone());
            let job = move |n: &u32| {
                let at = now.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(at, Ordering::SeqCst);
                if Some(*n) == stick {
                    let _ = gate.lock().unwrap().recv();
                } else {
                    std::thread::sleep(Duration::from_millis(15));
                }
                now.fetch_sub(1, Ordering::SeqCst);
                Some(*n)
            };
            let job: Arc<Job<u32, u32>> = Arc::new(job);
            job
        };
        let items: Vec<u32> = (0..40).collect();
        let started = Instant::now();
        let (whole, stuck_calls) = std::thread::scope(|s| {
            let mut whole = Vec::new();
            let mut stuck_calls = Vec::new();
            for _ in 0..8 {
                let (threads, items, job) = (threads.clone(), items.clone(), job_of(None));
                whole.push(s.spawn(move || {
                    map_on(
                        &threads,
                        &items,
                        Instant::now() + Duration::from_secs(20),
                        16,
                        &job,
                    )
                }));
            }
            for _ in 0..4 {
                let (threads, items, job) = (threads.clone(), items.clone(), job_of(Some(7)));
                stuck_calls.push(s.spawn(move || {
                    let began = Instant::now();
                    let got = map_on(
                        &threads,
                        &items,
                        began + Duration::from_millis(900),
                        16,
                        &job,
                    );
                    (got, began.elapsed())
                }));
            }
            (
                whole
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect::<Vec<_>>(),
                stuck_calls
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect::<Vec<_>>(),
            )
        });
        assert!(started.elapsed() < Duration::from_secs(15), "no deadlock");
        assert!(
            peak.load(Ordering::SeqCst) <= 16,
            "peak {}",
            peak.load(Ordering::SeqCst)
        );
        for got in &whole {
            assert!(
                got.iter().all(Option::is_some),
                "an unstuck call finished whole"
            );
        }
        for (got, took) in &stuck_calls {
            assert!(
                *took < Duration::from_millis(2500),
                "returned at its own deadline: {took:?}"
            );
            assert_eq!(got[7], None, "the stuck job is unanswered");
        }
        drop(release);
        assert!(
            wait_for(|| threads.in_use() == 0),
            "the stuck jobs ended when released"
        );
    }

    // Judge round 4, item 1: a worker takes no item once the deadline has passed in the middle of
    // its queue. The channel is kept open, so only the check before each item stops it.
    #[test]
    fn no_item_is_started_after_the_deadline_passes_mid_queue() {
        let started = Arc::new(AtomicUsize::new(0));
        let job = {
            let started = started.clone();
            let job = move |n: &u32| {
                started.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(60));
                Some(*n)
            };
            let job: Arc<Job<u32, u32>> = Arc::new(job);
            job
        };
        let items: Vec<u32> = (0..20).collect();
        let (tx, rx) = channel();
        let next = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_millis(200);
        work(&items, &next, deadline, &*job, &tx);
        let began = started.load(Ordering::SeqCst);
        assert!(
            (3..=5).contains(&began),
            "{began} items started in 200 ms of 60 ms jobs"
        );
        assert_eq!(rx.try_iter().count(), began);
    }

    // Judge round 4, item 2: an answer that came after the deadline is reported as none, and a
    // value that arrives in the instant of the deadline is checked against the clock.
    #[test]
    fn an_answer_that_comes_after_the_deadline_is_not_an_answer() {
        let job = |n: &u32| {
            std::thread::sleep(Duration::from_millis(150));
            Some(*n)
        };
        let job: Arc<Job<u32, u32>> = Arc::new(job);
        let (tx, rx) = channel();
        let next = AtomicUsize::new(0);
        work(
            &[1],
            &next,
            Instant::now() + Duration::from_millis(60),
            &*job,
            &tx,
        );
        assert_eq!(rx.try_recv().ok(), Some((0, None)));
        let soon = Instant::now() + Duration::from_millis(50);
        assert_eq!(on_time(7, soon), Some(7));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(on_time(7, soon), None);
    }

    // Judge round 4, items 2 and 5: a look run through the bounded view on the caller's own thread
    // is not started once the deadline has passed and its late answer is none.
    #[test]
    fn here_is_not_started_late_and_a_late_answer_is_none() {
        let view = Deadlined::with_threads(
            Walker::new(Some(1), Duration::from_secs(1)),
            Instant::now() + Duration::from_millis(100),
            Threads::new(2),
        );
        assert_eq!(view.here(|_| 1), Some(1));
        assert_eq!(
            view.here(|_| {
                std::thread::sleep(Duration::from_millis(150));
                2
            }),
            None
        );
        let asked = std::cell::Cell::new(false);
        assert_eq!(
            view.here(|_| {
                asked.set(true);
                3
            }),
            None
        );
        assert!(!asked.get(), "not started after the deadline");
    }

    // Class A9: no job is started for a call whose deadline has already passed (a job that is not
    // a file-system look has no clock of its own to refuse with).
    #[test]
    fn no_job_is_started_once_the_deadline_has_passed() {
        let asked = Arc::new(AtomicUsize::new(0));
        let job = {
            let asked = asked.clone();
            let job = move |n: &u32| {
                asked.fetch_add(1, Ordering::SeqCst);
                Some(*n)
            };
            let job: Arc<Job<u32, u32>> = Arc::new(job);
            job
        };
        let items: Vec<u32> = (0..20).collect();
        let got = map_on(&Threads::new(8), &items, Instant::now(), 8, &job);
        assert!(got.iter().all(Option::is_none));
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    // Class 7 (and the stalled job of class A5): a job that is not a file-system look and blocks
    // past the deadline. Its worker stays counted among the places; with every place held no
    // replacement starts, so the job is run by no more than the cap; a later call finds no place
    // and returns at its deadline; when the jobs end the places are free.
    #[test]
    fn a_blocked_job_keeps_its_place_and_no_replacement_starts() {
        let threads = Threads::new(4);
        let (release, gate) = channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let asked = Arc::new(AtomicUsize::new(0));
        let job = {
            let (gate, asked) = (gate, asked.clone());
            let job = move |_: &u32| {
                asked.fetch_add(1, Ordering::SeqCst);
                let _ = gate.lock().unwrap().recv();
                Some(())
            };
            let job: Arc<Job<u32, ()>> = Arc::new(job);
            job
        };
        let items: Vec<u32> = (0..20).collect();
        let began = Instant::now();
        let got = map_on(
            &threads,
            &items,
            Instant::now() + Duration::from_millis(400),
            16,
            &job,
        );
        assert!(began.elapsed() < Duration::from_millis(1500));
        assert!(got.iter().all(Option::is_none), "no job was answered");
        assert_eq!(threads.in_use(), 4, "the blocked workers keep their places");
        assert!(wait_for(|| asked.load(Ordering::SeqCst) == 4));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(asked.load(Ordering::SeqCst), 4, "no replacement started");
        let again = Instant::now();
        let none = map_on(
            &threads,
            &items,
            Instant::now() + Duration::from_millis(300),
            16,
            &job,
        );
        assert!(again.elapsed() < Duration::from_millis(1500));
        assert!(none.iter().all(Option::is_none));
        assert_eq!(
            asked.load(Ordering::SeqCst),
            4,
            "a later call finds no free place"
        );
        drop(release);
        assert!(
            wait_for(|| threads.in_use() == 0),
            "the places are free when the jobs end"
        );
    }

    // Class A8: a real panic in a job. In `map_on` the item is unanswered, the worker goes on to
    // its next item (one worker here, so a worker lost to the panic would leave the third item
    // unanswered until the deadline), and the place is freed.
    #[test]
    fn a_panicking_job_is_unanswered_and_the_worker_goes_on() {
        let threads = Threads::new(4);
        let job = |n: &u32| {
            assert!(*n != 1, "an injected panic");
            Some(*n)
        };
        let job: Arc<Job<u32, u32>> = Arc::new(job);
        let began = Instant::now();
        let got = map_on(
            &threads,
            &[0, 1, 2],
            Instant::now() + Duration::from_secs(5),
            1,
            &job,
        );
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(got, vec![Some(0), None, Some(2)]);
        assert!(wait_for(|| threads.in_use() == 0));
    }

    /// A view whose `real` panics for one path.
    struct Panics;

    impl Fs for Panics {
        fn real(&self, p: &Path) -> PathBuf {
            assert!(p != Path::new("/boom"), "an injected panic");
            p.to_path_buf()
        }
        fn node(&self, _: &Path) -> Node {
            Node::Plain
        }
        fn is_dir(&self, _: &Path) -> bool {
            false
        }
        fn exists(&self, _: &Path) -> bool {
            false
        }
        fn expand(&self, _: &Path) -> Expansion {
            Expansion::default()
        }
        fn walk(&self, _: &Path, _: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
            WalkEnd::Done
        }
        fn script(&self, _: &Path) -> Script {
            Script::Unreadable
        }
        fn runs_as_script(&self, _: &Path) -> bool {
            false
        }
        fn which(&self, _: &str) -> Option<PathBuf> {
            None
        }
    }

    // Class A8: a real panic in a look of the bounded view is an unfinished look at once (not a
    // wait for the deadline): the path is noted as cut, the place is freed, and the next look is
    // answered.
    #[test]
    fn a_panicking_look_is_a_cut_at_once_and_frees_its_place() {
        let threads = Threads::new(2);
        let view = Deadlined::with_threads(
            Arc::new(Panics),
            Instant::now() + Duration::from_secs(5),
            threads.clone(),
        );
        let began = Instant::now();
        assert_eq!(view.real(Path::new("/boom")), PathBuf::from("/boom"));
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
        assert!(view.cut(Path::new("/boom")));
        assert!(wait_for(|| threads.in_use() == 0));
        assert_eq!(view.real(Path::new("/fine")), PathBuf::from("/fine"));
        assert!(!view.cut(Path::new("/fine")));
    }
}
