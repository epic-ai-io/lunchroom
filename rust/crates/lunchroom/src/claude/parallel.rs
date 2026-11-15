//! A call's paths resolved on parallel threads (spec §11 L1.1, `§11::*`).
//!
//! "The guard resolves a call's paths on parallel threads, each path on one thread and at most 16
//! threads at a time (lead's decision, 2026-11-05), and spends at most 8 seconds of a decision's 10
//! (H1.5) doing so; a path of a HELD session's read still unresolved when that time ends is one
//! Lunchroom could not check, PROTECTED as a credential path would be."
//!
//! The classifier asks the file system for each path as it meets it, one after another. So the call
//! is judged twice. The first pass runs against a [`Recorder`], which answers every question by the
//! path's spelling, touching nothing, and notes what was asked (the resolution of a path, its real
//! path, whether it is a folder, whether it exists, its script text); its result is dropped. What
//! it noted is looked up at once on parallel threads ([`lunchroom_core::fs::map_on`]), and the
//! second pass, the one that counts, runs against a [`Parallel`] view that answers each question
//! from that result. A question the first pass did not ask (one that only the real answer to
//! another reaches) is answered at that moment on a worker thread under the same deadline
//! ([`Deadlined`]): the deciding thread never waits in the file system past the deadline. A look
//! not finished by the deadline is answered cautiously and noted against its own path, and the
//! view says so for that path only ([`Fs::cut`]), so the classifier PROTECTS a HELD session's read
//! of that path (`Guard::unresolved`) while a path that cannot be followed at all stays DENIED,
//! whatever the order of the call's paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use lunchroom_core::fs::{
    Deadlined, Expansion, Fs, Job, Node, OsFs, RESOLVE_THREADS, Resolved, Script, Threads, WalkEnd,
    map_on, resolve_bounded,
};

/// What a look asks about a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    Resolve,
    Real,
    IsDir,
    Exists,
    Script,
    RunsAsScript,
    Which,
    FileId,
    Expand,
    Walk,
}

/// One look: a kind of question and the path it is about.
pub(super) type Query = (Kind, PathBuf);

/// The answer to one look.
#[derive(Debug, Clone)]
pub(super) enum Answer {
    Resolved(Resolved),
    Path(PathBuf),
    Flag(bool),
    Script(Script),
    Found(Option<PathBuf>),
    /// A file's device and inode.
    Id(Option<(u64, u64)>),
    Expanded(Expansion),
    /// The paths a walk visited, and how it ended.
    Walked(Vec<PathBuf>, WalkEnd),
}

/// What the looks of one decision share: the real file system, the same bounded on worker
/// threads, and the one deadline (8 seconds of the decision's 10, counted from where the
/// decision began).
pub(super) struct Look<F: Fs + Send + Sync + 'static = OsFs> {
    os: Arc<F>,
    bounded: Arc<Deadlined<F>>,
    /// The time the parallel looks end (the decision's own deadline).
    pool_until: Instant,
    threads: Arc<Threads>,
}

impl Look {
    /// The looks of a decision whose time ends at `deadline`, on the process's own threads.
    pub(super) fn new(deadline: Instant) -> Self {
        Self::over(
            Arc::new(OsFs::until(deadline)),
            deadline,
            Threads::global().clone(),
        )
    }
}

impl<F: Fs + Send + Sync + 'static> Look<F> {
    /// The looks of a decision over the file system `os`, on the given threads.
    pub(super) fn over(os: Arc<F>, deadline: Instant, threads: Arc<Threads>) -> Self {
        Self {
            bounded: Arc::new(Deadlined::with_threads(
                Arc::clone(&os),
                deadline,
                Arc::clone(&threads),
            )),
            os,
            pool_until: deadline,
            threads,
        }
    }

    /// A test's view in which the parallel looks end sooner than the looks made one at a time, so
    /// that what the cut of one kind of look does can be told from what the time does after it.
    #[cfg(test)]
    pub(super) fn pool_until(mut self, until: Instant) -> Self {
        self.pool_until = until;
        self
    }
}

/// The looks already made, kept from one judging of a call to the next (the T1.8 re-judging uses
/// the first judging's answers, not a deadline already spent). `None` is a look the deadline cut.
pub(super) type Carried = BTreeMap<Query, Option<Answer>>;

/// The first pass's view: every question is answered by the path's own spelling and noted.
struct Recorder<F: Fs + Send + Sync + 'static> {
    inner: Arc<Deadlined<F>>,
    asked: Mutex<BTreeSet<Query>>,
}

impl<F: Fs + Send + Sync + 'static> Recorder<F> {
    fn note(&self, kind: Kind, p: &Path) {
        if let Ok(mut asked) = self.asked.lock() {
            asked.insert((kind, p.to_path_buf()));
        }
    }
}

impl<F: Fs + Send + Sync + 'static> Fs for Recorder<F> {
    fn real(&self, p: &Path) -> PathBuf {
        self.note(Kind::Real, p);
        p.to_path_buf()
    }
    fn node(&self, p: &Path) -> Node {
        self.inner.node(p)
    }
    fn out_of_time(&self) -> bool {
        self.inner.out_of_time()
    }
    fn cut(&self, _p: &Path) -> bool {
        false
    }
    fn descends_from(&self, pid: u32, root: u32) -> bool {
        self.inner.descends_from(pid, root)
    }
    fn resolve(&self, p: &Path) -> Resolved {
        self.note(Kind::Resolve, p);
        Resolved {
            path: p.to_path_buf(),
            complete: true,
        }
    }
    fn is_dir(&self, p: &Path) -> bool {
        self.note(Kind::IsDir, p);
        false
    }
    fn exists(&self, p: &Path) -> bool {
        self.note(Kind::Exists, p);
        true
    }
    // The first pass's result is dropped: it needs neither the matches of a wildcard nor a walk,
    // which the second pass makes.
    fn expand(&self, _pattern: &Path) -> Expansion {
        Expansion::default()
    }
    fn walk(&self, _dir: &Path, _visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        WalkEnd::Done
    }
    fn script(&self, p: &Path) -> Script {
        self.note(Kind::Script, p);
        Script::Unreadable
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.note(Kind::RunsAsScript, p);
        true
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.note(Kind::Which, Path::new(prog));
        None
    }
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        self.note(Kind::FileId, p);
        None
    }
}

/// The second pass's view: each question answered from the parallel looks.
pub(super) struct Parallel<F: Fs + Send + Sync + 'static = OsFs> {
    inner: Arc<Deadlined<F>>,
    looked: Carried,
    /// Paths a look was cut for, as the classifier asked about them in this view.
    cut: Mutex<BTreeSet<PathBuf>>,
    /// Answers to questions only this view asked (see [`Parallel::keep`]).
    fresh: Mutex<Carried>,
}

impl<F: Fs + Send + Sync + 'static> Parallel<F> {
    /// The view for a call judged twice: `first` judges the call against the recording view it is
    /// given, what it asked about and `carried` does not hold is looked up at once, and `carried`
    /// keeps every answer for the next judging.
    pub(super) fn after<R>(
        look: &Look<F>,
        carried: &mut Carried,
        first: impl FnOnce(&dyn Fs) -> R,
    ) -> Self {
        let recorder = Recorder {
            inner: Arc::clone(&look.bounded),
            asked: Mutex::new(BTreeSet::new()),
        };
        let _ = first(&recorder);
        let asked: Vec<Query> = recorder
            .asked
            .lock()
            .map(|set| {
                set.iter()
                    .filter(|q| !carried.contains_key(*q))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let (os, bounded) = (Arc::clone(&look.os), Arc::clone(&look.bounded));
        let deadline = look.pool_until;
        let job = move |q: &Query| run_look(&os, &bounded, q.0, &q.1, deadline);
        let job: Arc<Job<Query, Answer>> = Arc::new(job);
        let answers = map_on(&look.threads, &asked, deadline, RESOLVE_THREADS, &job);
        carried.extend(asked.into_iter().zip(answers));
        Self {
            inner: Arc::clone(&look.bounded),
            looked: carried.clone(),
            cut: Mutex::new(BTreeSet::new()),
            fresh: Mutex::new(Carried::new()),
        }
    }

    /// Keeps the answers this view found for questions the first pass did not ask, with those it
    /// was given, so the T1.8 re-judging reads them instead of looking again.
    pub(super) fn keep(&self, carried: &mut Carried) {
        if let Ok(fresh) = self.fresh.lock() {
            for (query, answer) in fresh.iter() {
                carried.insert(query.clone(), answer.clone());
            }
        }
    }

    /// How many resolutions the threads finished before the deadline, and how many they did not.
    #[cfg(test)]
    pub(super) fn counts(&self) -> (usize, usize) {
        let (mut done, mut total) = (0, 0);
        for (q, a) in &self.looked {
            if q.0 == Kind::Resolve {
                total += 1;
                done += usize::from(a.is_some());
            }
        }
        (done, total - done)
    }

    /// The answer to a look made before, or `None` if it was not made (`Err`: it was cut, and the
    /// path is noted as one Lunchroom could not check).
    fn answer(&self, kind: Kind, p: &Path) -> Result<Option<Answer>, ()> {
        let key = (kind, p.to_path_buf());
        let found = self.looked.get(&key).cloned().or_else(|| {
            self.fresh
                .lock()
                .ok()
                .and_then(|fresh| fresh.get(&key).cloned())
        });
        match found {
            Some(Some(a)) => Ok(Some(a)),
            Some(None) => {
                if let Ok(mut cut) = self.cut.lock() {
                    cut.insert(p.to_path_buf());
                }
                Err(())
            }
            None => Ok(None),
        }
    }

    /// A question of `kind` about `p`: the answer already found (`take` reads it), the cautious
    /// answer if its look was cut, else the look is made now through the bounded view (`ask`) and
    /// the answer is kept (`keep` writes it) for the re-judging.
    fn through<T>(
        &self,
        kind: Kind,
        p: &Path,
        take: impl FnOnce(&Answer) -> Option<T>,
        cautious: impl FnOnce() -> T,
        ask: impl FnOnce() -> T,
        keep: impl FnOnce(&T) -> Answer,
    ) -> T {
        match self.answer(kind, p) {
            Ok(Some(found)) => match take(&found) {
                Some(value) => value,
                None => ask(),
            },
            Ok(None) => {
                let value = ask();
                let answer = (!self.inner.cut(p)).then(|| keep(&value));
                if let Ok(mut fresh) = self.fresh.lock() {
                    fresh.insert((kind, p.to_path_buf()), answer);
                }
                value
            }
            Err(()) => cautious(),
        }
    }
}

impl<F: Fs + Send + Sync + 'static> Fs for Parallel<F> {
    fn real(&self, p: &Path) -> PathBuf {
        self.through(
            Kind::Real,
            p,
            |a| match a {
                Answer::Path(r) => Some(r.clone()),
                _ => None,
            },
            || p.to_path_buf(),
            || self.inner.real(p),
            |r| Answer::Path(r.clone()),
        )
    }
    fn node(&self, p: &Path) -> Node {
        self.inner.node(p)
    }
    fn out_of_time(&self) -> bool {
        self.inner.out_of_time()
    }
    fn cut(&self, p: &Path) -> bool {
        self.cut.lock().map_or(true, |c| c.contains(p)) || self.inner.cut(p)
    }
    fn descends_from(&self, pid: u32, root: u32) -> bool {
        self.inner.descends_from(pid, root)
    }
    fn resolve(&self, p: &Path) -> Resolved {
        self.through(
            Kind::Resolve,
            p,
            |a| match a {
                Answer::Resolved(r) => Some(r.clone()),
                _ => None,
            },
            || Resolved {
                path: p.to_path_buf(),
                complete: false,
            },
            || self.inner.resolve(p),
            |r| Answer::Resolved(r.clone()),
        )
    }
    fn is_dir(&self, p: &Path) -> bool {
        self.through(
            Kind::IsDir,
            p,
            |a| match a {
                Answer::Flag(f) => Some(*f),
                _ => None,
            },
            || true,
            || self.inner.is_dir(p),
            |f| Answer::Flag(*f),
        )
    }
    fn exists(&self, p: &Path) -> bool {
        self.through(
            Kind::Exists,
            p,
            |a| match a {
                Answer::Flag(f) => Some(*f),
                _ => None,
            },
            || true,
            || self.inner.exists(p),
            |f| Answer::Flag(*f),
        )
    }
    fn expand(&self, pattern: &Path) -> Expansion {
        self.through(
            Kind::Expand,
            pattern,
            |a| match a {
                Answer::Expanded(e) => Some(e.clone()),
                _ => None,
            },
            || Expansion {
                matches: Vec::new(),
                capped: true,
            },
            || self.inner.expand(pattern),
            |e| Answer::Expanded(e.clone()),
        )
    }
    fn walk(&self, dir: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> WalkEnd {
        match self.answer(Kind::Walk, dir) {
            // A walk made before is replayed to this visitor, which may stop it as it did.
            Ok(Some(Answer::Walked(paths, end))) => {
                for p in &paths {
                    if visit(p) {
                        return WalkEnd::Stopped;
                    }
                }
                end
            }
            Ok(_) => {
                let mut seen = Vec::new();
                let end = self.inner.walk(dir, &mut |p| {
                    seen.push(p.to_path_buf());
                    visit(p)
                });
                // A walk the visitor stopped is not the whole walk: it is made again for another.
                if end != WalkEnd::Stopped
                    && let Ok(mut fresh) = self.fresh.lock()
                {
                    fresh.insert(
                        (Kind::Walk, dir.to_path_buf()),
                        Some(Answer::Walked(seen, end)),
                    );
                }
                end
            }
            Err(()) => WalkEnd::Capped,
        }
    }
    fn script(&self, p: &Path) -> Script {
        self.through(
            Kind::Script,
            p,
            |a| match a {
                Answer::Script(t) => Some(t.clone()),
                _ => None,
            },
            || Script::Unreadable,
            || self.inner.script(p),
            |t| Answer::Script(t.clone()),
        )
    }
    fn runs_as_script(&self, p: &Path) -> bool {
        self.through(
            Kind::RunsAsScript,
            p,
            |a| match a {
                Answer::Flag(f) => Some(*f),
                _ => None,
            },
            || true,
            || self.inner.runs_as_script(p),
            |f| Answer::Flag(*f),
        )
    }
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.through(
            Kind::Which,
            Path::new(prog),
            |a| match a {
                Answer::Found(found) => Some(found.clone()),
                _ => None,
            },
            || None,
            || self.inner.which(prog),
            |found| Answer::Found(found.clone()),
        )
    }
    fn file_id(&self, p: &Path) -> Option<(u64, u64)> {
        self.through(
            Kind::FileId,
            p,
            |a| match a {
                Answer::Id(id) => Some(*id),
                _ => None,
            },
            || None,
            || self.inner.file_id(p),
            |id| Answer::Id(*id),
        )
    }
}

/// One look of the first pass's list, made on a worker of the pool through the bounded view: not
/// started once the deadline has passed, and an answer that came after it is none. The search for
/// a program takes its name as it is (a name that is not text cannot have come from a command and
/// is not looked for under another name: the look is cut).
pub(super) fn run_look<F: Fs + Send + Sync + 'static>(
    os: &Arc<F>,
    bounded: &Deadlined<F>,
    kind: Kind,
    p: &Path,
    deadline: Instant,
) -> Option<Answer> {
    match kind {
        Kind::Resolve => resolve_bounded(os, p, deadline).map(Answer::Resolved),
        Kind::Real => bounded.here(|f| f.real(p)).map(Answer::Path),
        Kind::IsDir => bounded.here(|f| f.is_dir(p)).map(Answer::Flag),
        Kind::Exists => bounded.here(|f| f.exists(p)).map(Answer::Flag),
        Kind::Script => bounded.here(|f| f.script(p)).map(Answer::Script),
        Kind::RunsAsScript => bounded.here(|f| f.runs_as_script(p)).map(Answer::Flag),
        Kind::Which => p
            .to_str()
            .and_then(|name| bounded.here(|f| f.which(name)))
            .map(Answer::Found),
        Kind::FileId => bounded.here(|f| f.file_id(p)).map(Answer::Id),
        // Asked only by the second pass, never listed by the first.
        Kind::Expand | Kind::Walk => None,
    }
}
