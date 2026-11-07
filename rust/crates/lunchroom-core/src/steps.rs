//! The step the guard is in, and the slowest step it took (spec §13 H1.7, H1.8).
//!
//! H1.7 (docs/spec/lunchroom-spec.md §13 item 7): a call the guard cannot decide is explained with
//! "the cause, which is … no answer within 10 seconds (naming the step that was still running when
//! the time ran out)". H1.8 (item 8): the guard records "the slowest step and how long it took".
//! Each step of the guard's work marks itself with [`begin`] for as long as it runs; steps nest, and
//! the innermost one running is the one named. Marks are kept per thread: the guard's deciding
//! thread marks its steps, and the thread watching its deadline reads that thread's marks by its
//! id.

use std::sync::Mutex;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// One thread's steps running, outermost first, each with when it began and the time its own
/// steps took; and the slowest step that ended, by its own time (without the steps inside it, so a
/// step that only waits on another is not named in its place).
#[derive(Default)]
struct Marks {
    running: Vec<(String, Instant, Duration)>,
    slowest: Option<(String, Duration)>,
}

static MARKS: Mutex<Vec<(ThreadId, Marks)>> = Mutex::new(Vec::new());

/// Runs `f` on the marks of thread `id`; a poisoned lock (a step's thread panicked) is still used.
fn with<R>(id: ThreadId, f: impl FnOnce(&mut Marks) -> R) -> R {
    let mut all = MARKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = all.iter().position(|(t, _)| *t == id).unwrap_or_else(|| {
        all.push((id, Marks::default()));
        all.len() - 1
    });
    f(&mut all[at].1)
}

/// One step of the guard's work, running until this value is dropped.
#[derive(Debug)]
#[must_use = "a step ends when this value is dropped"]
pub struct Step(ThreadId);

/// Marks the start of the step `name` (`"reading Lunchroom's store"`, `"searching below /x"`) in
/// this thread: it runs until the value returned is dropped.
pub fn begin(name: impl Into<String>) -> Step {
    let id = std::thread::current().id();
    let name = name.into();
    with(id, |m| {
        m.running.push((name, Instant::now(), Duration::ZERO));
    });
    Step(id)
}

impl Drop for Step {
    fn drop(&mut self) {
        with(self.0, |m| {
            if let Some((name, at, inner)) = m.running.pop() {
                let took = at.elapsed();
                if let Some(parent) = m.running.last_mut() {
                    parent.2 += took;
                }
                let own = took.saturating_sub(inner);
                if m.slowest.as_ref().is_none_or(|(_, longest)| own > *longest) {
                    m.slowest = Some((name, own));
                }
            }
        });
    }
}

/// The innermost step running now in thread `id`, if any.
#[must_use]
pub fn current(id: ThreadId) -> Option<String> {
    with(id, |m| m.running.last().map(|(name, _, _)| name.clone()))
}

/// The slowest step so far in thread `id`, by its own time: one that ended, or the innermost one
/// still running, whose own time so far is its whole time less the steps inside it that ended.
#[must_use]
pub fn slowest(id: ThreadId) -> Option<(String, Duration)> {
    with(id, |m| {
        let running = m
            .running
            .last()
            .map(|(name, at, inner)| (name.clone(), at.elapsed().saturating_sub(*inner)));
        match (m.slowest.clone(), running) {
            (Some(a), Some(b)) => Some(if b.1 > a.1 { b } else { a }),
            (a, b) => a.or(b),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // H1.7, H1.8: the innermost running step of a thread is named, by that thread's id from
    // another; the slowest is kept when it ends.
    #[test]
    fn the_innermost_step_is_named_and_the_slowest_kept() {
        let worker = std::thread::spawn(|| {
            let id = std::thread::current().id();
            let outer = begin("updating Lunchroom's store");
            let named = {
                let _inner = begin("searching the session's transcript /t");
                std::thread::sleep(Duration::from_millis(30));
                current(id)
            };
            drop(outer);
            named
        });
        let named = worker.join().ok().flatten();
        assert_eq!(
            named.as_deref(),
            Some("searching the session's transcript /t")
        );
        let id = std::thread::current().id();
        assert_eq!(current(id), None);
        let step = begin("reading Lunchroom's store");
        assert_eq!(current(id).as_deref(), Some("reading Lunchroom's store"));
        drop(step);
        assert!(slowest(id).is_some_and(|(name, _)| name == "reading Lunchroom's store"));
    }
}
