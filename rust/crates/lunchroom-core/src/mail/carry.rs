//! What carries a hold from one place to another: the transcript a session resumed with (T1.7), the
//! paths a HELD session wrote (T1.8), and the session a HELD session started (T1.9).
//!
//! `§5::31-48`. Nothing here reads a file or a process: the guard hands in what it read
//! (a byte offset and the ids of the labels, the paths a call was judged to write, the identities
//! of the sessions above a new one), and these methods keep and apply it.

use std::collections::BTreeSet;
use std::path::Path;

use super::WritingCall;
use super::{Hold, Mail, Marked, TranscriptRead, UNREAD_TRANSCRIPT, WRITING_KEPT, WRITING_SECS};
use crate::identity::Identity;

/// What a read of a transcript found, handed to [`Mail::transcript_read`] (T1.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    /// The transcript's path.
    pub path: String,
    /// The byte the read began at.
    pub from: u64,
    /// The byte the read reached: the next read begins here.
    pub reached: u64,
    /// The ids of the labels the read found.
    pub ids: BTreeSet<String>,
    /// The read reached the end of the transcript.
    pub at_end: bool,
}

impl Mail {
    /// T1.7: a hook sees `who`'s transcript `path`, which is `len` bytes long. Returns the byte a
    /// read is to begin at, or `None` when the transcript was read to its end. A transcript that
    /// is another file than the one kept, or shorter than the kept offset, is read again from its
    /// start. A transcript not read to its end holds the session, unless the user typed since it
    /// was first seen (the hold then stays ended).
    pub fn transcript_due(&mut self, who: &Identity, path: &str, len: u64) -> Option<u64> {
        let key = who.key();
        self.transcripts_read.insert(key.clone());
        let record = self.transcripts.entry(key).or_default();
        if record.path != path || len < record.offset {
            *record = TranscriptRead {
                path: path.to_owned(),
                ..TranscriptRead::default()
            };
        }
        if record.done {
            return None;
        }
        record.reading = true;
        let (offset, typed) = (record.offset, record.typed);
        if !typed {
            self.hold_unrecorded(who, "unjudged", UNREAD_TRANSCRIPT);
        }
        Some(offset)
    }

    /// T1.7: a read of `who`'s transcript found `read`. The offset and the ids are kept (never any
    /// text); a read that reached the end ends the hold of the unread transcript and holds the
    /// session by exactly the messages whose ids the transcript names. A result of a read that
    /// began before the record was started again (a shorter transcript) is dropped. Returns
    /// whether `who` is HELD now.
    pub fn transcript_read(&mut self, who: &Identity, read: &Read) -> bool {
        let key = who.key();
        let Some(record) = self.transcripts.get_mut(&key) else {
            return self.is_held(who);
        };
        if record.path != read.path || record.offset < read.from || record.done {
            return self.is_held(who);
        }
        record.reading = false;
        record.offset = record.offset.max(read.reached);
        record.ids.extend(read.ids.iter().cloned());
        if read.at_end {
            record.done = true;
            let named: Vec<String> = record.ids.iter().cloned().collect();
            let message = format!("unjudged:{UNREAD_TRANSCRIPT}");
            if let Some(holds) = self.holds.get_mut(&key) {
                holds.retain(|h| h.message != message);
                if holds.is_empty() {
                    self.holds.remove(&key);
                }
            }
            self.end_declines();
            // A label quoted with no such message id holds nothing (§5::31).
            for id in named {
                let hold = self.messages.get(&id).map(|m| Hold {
                    message: id.clone(),
                    from: Some(m.from.clone()),
                    from_name: m.from_name.clone(),
                });
                if let Some(hold) = hold {
                    let holds = self.holds.entry(key.clone()).or_default();
                    if !holds.iter().any(|h| h.message == id) {
                        holds.push(hold);
                    }
                }
            }
        }
        self.is_held(who)
    }

    /// The user typed in `who`'s session: an unread transcript holds it no longer, and the labels
    /// found so far are not held against it (T1.7).
    pub(super) fn transcript_typed(&mut self, who: &Identity) {
        if let Some(record) = self.transcripts.get_mut(&who.key()) {
            record.typed = true;
            record.ids.clear();
        }
    }

    /// Whether an earlier hook began a read of `who`'s transcript that never reported (it was cut
    /// off, or its file stalls): this hook's read of it is limited in time.
    #[must_use]
    pub fn transcript_unfinished(&self, who: &Identity) -> bool {
        self.transcripts.get(&who.key()).is_some_and(|r| r.reading)
    }

    /// How far `who`'s transcript has been read.
    #[must_use]
    pub fn transcript_record(&self, who: &Identity) -> Option<&TranscriptRead> {
        self.transcripts.get(&who.key())
    }

    /// The key a call's written paths are kept under.
    fn writing_key(who: &Identity, call: &str) -> String {
        format!("{}\u{1f}{call}", who.key())
    }

    /// T1.8: the call `call` of `who` was judged, before it ran, to write `paths`. Kept for its
    /// after-call hook. Calls that never reach one are dropped by age, and the oldest by count.
    pub fn note_writing(&mut self, who: &Identity, call: &str, paths: Vec<String>, now: u64) {
        if paths.is_empty() {
            return;
        }
        let entry = self
            .writing
            .entry(Self::writing_key(who, call))
            .or_insert_with(|| WritingCall {
                paths: Vec::new(),
                at: now,
            });
        for path in paths {
            if !entry.paths.contains(&path) {
                entry.paths.push(path);
            }
        }
        while self.writing.len() > WRITING_KEPT {
            let oldest = self
                .writing
                .iter()
                .min_by_key(|(_, w)| w.at)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.writing.remove(&k),
                None => break,
            };
        }
    }

    /// T1.8: the paths the call `call` of `who` was judged to write, taken out of the record.
    pub fn take_writing(&mut self, who: &Identity, call: &str) -> Option<Vec<String>> {
        self.writing
            .remove(&Self::writing_key(who, call))
            .map(|w| w.paths)
    }

    /// Forgets the written paths of calls that never reached their after-call hook.
    pub(super) fn forget_old_writing(&mut self, now: u64) {
        self.writing
            .retain(|_, w| now.saturating_sub(w.at) < WRITING_SECS);
    }

    /// T1.8: the call `call` of `who` has run and wrote `paths`. When `who` is HELD now, each path
    /// is recorded with the messages that hold it; returns whether it was recorded.
    pub fn record_written(
        &mut self,
        who: &Identity,
        paths: Vec<String>,
        call: &str,
        now: u64,
    ) -> bool {
        let holders = self.holding(who).to_vec();
        if holders.is_empty() || paths.is_empty() {
            return false;
        }
        let writer_name = self.name_of(who).unwrap_or(&who.session).to_owned();
        let record = Marked {
            paths,
            writer: who.clone(),
            writer_name,
            holders,
            since: now,
            call: call.to_owned(),
        };
        // The same call run again (a repeat of the hook) records nothing twice.
        if !self.marked.iter().any(|m| {
            m.writer == record.writer && m.paths == record.paths && m.holders == record.holders
        }) {
            self.marked.push(record);
        }
        true
    }

    /// T1.8: the paths recorded as written by a HELD session, with who wrote each and its holders.
    #[must_use]
    pub fn marked(&self) -> &[Marked] {
        &self.marked
    }

    /// T1.8: `who` reads the paths `reads` or searches the folders `searches`. A recorded path that
    /// is one of the first, or lies in one of the second, holds `who` by the messages recorded for
    /// it, as if they had been delivered to it. Returns whether any recorded path was reached.
    pub fn hold_by_marked(
        &mut self,
        who: &Identity,
        reads: &[String],
        searches: &[String],
    ) -> bool {
        let mut reached = false;
        let mut add: Vec<Hold> = Vec::new();
        for marked in &self.marked {
            let hit = marked.paths.iter().any(|p| {
                reads.iter().any(|r| r == p)
                    || searches
                        .iter()
                        .any(|s| Path::new(p).starts_with(Path::new(s)))
            });
            if hit {
                reached = true;
                add.extend(marked.holders.iter().cloned());
            }
        }
        if !add.is_empty() {
            let holds = self.holds.entry(who.key()).or_default();
            for hold in add {
                if !holds.iter().any(|h| h.message == hold.message) {
                    holds.push(hold);
                }
            }
        }
        reached
    }

    /// T1.8: the user cleared `paths` with `lunchroom clear <path>`: each is no longer recorded.
    /// Returns how many recorded entries lost a path.
    pub fn clear_marked(&mut self, paths: &[String]) -> usize {
        let mut cleared = 0;
        for marked in &mut self.marked {
            let before = marked.paths.len();
            marked.paths.retain(|p| !paths.contains(p));
            if marked.paths.len() != before {
                cleared += 1;
            }
        }
        self.marked.retain(|m| !m.paths.is_empty());
        cleared
    }

    /// A hook has seen `who` before.
    #[must_use]
    pub fn has_seen(&self, who: &Identity) -> bool {
        self.seen.contains(&who.key())
    }

    /// The sessions a hook saw in the harness process `process` (`<pid>:<start time>`).
    #[must_use]
    pub fn sessions_seen_in(&self, process: &str) -> Vec<Identity> {
        self.process_members
            .get(process)
            .map(|keys| keys.iter().filter_map(|k| Identity::parse(k)).collect())
            .unwrap_or_default()
    }

    /// T1.9: `who`, a session a hook sees for the first time, is held by every message that holds
    /// `parent` now. Returns whether `who` took any hold.
    pub fn inherit_holds(&mut self, who: &Identity, parent: &Identity) -> bool {
        if who == parent {
            return false;
        }
        let from = self.holding(parent).to_vec();
        let mut added = false;
        if !from.is_empty() {
            let holds = self.holds.entry(who.key()).or_default();
            for hold in from {
                if !holds.iter().any(|h| h.message == hold.message) {
                    holds.push(hold);
                    added = true;
                }
            }
        }
        added
    }
}
