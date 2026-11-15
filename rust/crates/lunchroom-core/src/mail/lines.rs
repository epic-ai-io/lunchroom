//! A line of Lunchroom's: its form, its record, and the first use of HELD to a session
//! (spec §16 item 1).
//!
//! §16 item 1: "A line of Lunchroom's is `<HH:MM> UTC Lunchroom: <words> (<agents>)`. `<HH:MM>` is
//! the time of the event in UTC, written as R2.2 writes an end time (`14:00 UTC`). `<words>` are
//! the card's words, exactly. Further sentences, where an item says so, follow `(<agents>)`.
//! `<agents>` are the names of the agents of the event, as `lunchroom status` shows them: for a
//! message, the sender, `to`, the recipient; for two agents otherwise, `A and B`; for three or more
//! agents, the agents separated by commas with `and` before the last (`A, B and C`). A line is told
//! to a session, not to a name (I1.1) ... A line that uses HELD for the first time to a session
//! that reads it adds V1.7's definition after `(<agents>)`. Lunchroom records each line, with its
//! time and its agents, in the store (§17.2), in the file of the fields 1.1 adds (D1.2). A line is
//! at most 1,024 bytes, a longer agent name being cut. Lunchroom removes no line."

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::Mail;
use crate::approvals::visible;
use crate::identity::Identity;
use crate::vocab::{HELD_MEANING, Word};

/// The most bytes a line has (§16 item 1).
pub const LINE_MAX_BYTES: usize = 1_024;

/// What a line says is the event it tells: who it is about (`Pair`: a message, the sender, `to`,
/// the recipient; `Agents`: the agents joined as item 1 says).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who<'a> {
    /// `(<sender> to <recipient>)`.
    Message { from: &'a str, to: &'a str },
    /// `(A)`, `(A and B)`, `(A, B and C)`.
    Agents(&'a [&'a str]),
}

/// A line as recorded: its time, its agents and its words, never removed (§16 item 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRecord {
    /// The order the lines were recorded in, from 1: the clock may step back, this does not.
    pub seq: u64,
    /// The order this line was recorded in among all messages ([`super::Message::rec`]) and all
    /// lines: one counter in the store. 0 for a line recorded before this field.
    #[serde(default)]
    pub rec: u64,
    /// The time of the event, seconds since the Unix epoch.
    pub at: u64,
    /// The card's words and the further sentences, without the first-use definition (which depends
    /// on the reader).
    pub words: String,
    /// The agents of the event, as the line names them (a sender, `to`, the recipient for a
    /// message).
    pub agents: String,
    /// The identities of the agents of the event, in the order the line names them: a console
    /// keeps conversations by session (I1.1). A line recorded before this field has none.
    #[serde(default)]
    pub who: Vec<Identity>,
    /// The line as built for no reader in particular.
    pub text: String,
}

/// `<HH:MM> UTC` for `at`, as R2.2 writes an end time: truncated, zero padded.
#[must_use]
pub fn clock(at: u64) -> String {
    let day = at % 86_400;
    format!("{:02}:{:02} UTC", day / 3_600, day % 3_600 / 60)
}

/// The agents of an event as item 1 writes them: `A`, `A and B`, `A, B and C`.
#[must_use]
pub fn join_agents(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, last] => format!("{first} and {last}"),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The bytes of each agent name a line keeps whatever else is long.
const NAME_FLOOR: usize = 32;

/// What a line shows for an agent that has no name.
const UNNAMED: &str = "unnamed";

/// `name` as a line shows it: a control character, a direction mark and a zero-width character
/// written visibly (§17.10), an empty name as `unnamed`.
fn marked(name: &str) -> String {
    if name.is_empty() {
        return UNNAMED.to_owned();
    }
    let mut out = String::new();
    for c in visible(name).chars() {
        if matches!(
            c,
            '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{fffd}'
        ) {
            let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The sentence that defines HELD at its first use to a reader (V1.7).
#[must_use]
pub fn held_definition() -> String {
    format!("{}: {HELD_MEANING}.", Word::Held.text())
}

/// Whether `words` use the word HELD (the whole word: `UNHELD` and `BEHELD` do not).
#[must_use]
pub fn uses_held(words: &str) -> bool {
    words
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == Word::Held.text())
}

/// The names of `who` and the text between two of them.
fn names_of(who: Who<'_>) -> Vec<&str> {
    match who {
        Who::Message { from, to } => vec![from, to],
        Who::Agents([]) => vec![""],
        Who::Agents(a) => a.to_vec(),
    }
}

/// The bytes the separators between `n` names take.
const fn separators(who: Who<'_>, n: usize) -> usize {
    match who {
        Who::Message { .. } => " to ".len(),
        Who::Agents(_) => match n {
            0 | 1 => 0,
            2 => " and ".len(),
            n => ", ".len() * (n - 2) + " and ".len(),
        },
    }
}

/// Builds `<HH:MM> UTC Lunchroom: <words> (<agents>)` and `further` sentences after it, in at most
/// [`LINE_MAX_BYTES`] bytes, V1.7's definition included for a line that uses HELD: a name too long
/// for the line is cut, each to the same share of what the fixed text leaves; when the fixed text
/// alone is too long its further sentences, then its words, are cut. Agents too many for the line
/// even at [`NAME_FLOOR`] bytes each are named as far as they fit, followed by `N more`.
#[must_use]
pub fn build(at: u64, words: &str, who: Who<'_>, further: &[String]) -> String {
    let names = names_of(who);
    let sep = separators(who, names.len());
    // A line that uses HELD may be told with V1.7's definition after it: room is kept for it.
    let reserve = if uses_held(words) {
        held_definition().len() + 1
    } else {
        0
    };
    let budget = LINE_MAX_BYTES.saturating_sub(reserve);
    let prefix = format!("{} Lunchroom: ", clock(at));
    let mut marked_names: Vec<String> = names.iter().map(|n| marked(n)).collect();
    let mut words = words.to_owned();
    let mut tail = String::new();
    for sentence in further {
        tail.push(' ');
        tail.push_str(sentence);
    }
    // Too many agents for the line even at each name's first characters: the line names as many
    // as fit, in order, and ends the list with `N more` (never cut), so the words stay whole and
    // the line stays within its bytes (design decision for §16 item 1, 2026-11-13).
    let mut more = String::new();
    if matches!(who, Who::Agents(_)) {
        let total = marked_names.len();
        let need = |k: usize, more: &str| {
            let floor: usize = marked_names[..k]
                .iter()
                .map(|m| cut(m, NAME_FLOOR).len())
                .sum();
            let listed = k + usize::from(!more.is_empty());
            prefix.len()
                + words.len()
                + 3
                + tail.len()
                + separators(who, listed)
                + floor
                + more.len()
        };
        let mut kept = total;
        while kept > 1 && need(kept, &more) > budget {
            kept -= 1;
            more = format!("{} more", total - kept);
        }
        marked_names.truncate(kept);
    }
    let listed = marked_names.len() + usize::from(!more.is_empty());
    let sep = if more.is_empty() {
        sep
    } else {
        separators(who, listed)
    };
    // The line keeps room for each name's first characters: a name is cut before the sentences
    // are, but never to nothing.
    let floor: usize = marked_names
        .iter()
        .map(|m| cut(m, NAME_FLOOR).len())
        .sum::<usize>()
        + more.len();
    // " ()" is the parentheses and the space before them.
    let fixed = |w: &str, t: &str| prefix.len() + w.len() + 3 + t.len() + sep + floor;
    if fixed(&words, &tail) > budget {
        tail = cut(&tail, budget.saturating_sub(fixed(&words, "")));
    }
    if fixed(&words, &tail) > budget {
        words = cut(&words, budget.saturating_sub(fixed("", &tail)));
    }
    // `floor` holds `more`, which is never cut: the names share what is left beside it.
    let share =
        budget.saturating_sub(fixed(&words, &tail) - floor + more.len()) / marked_names.len();
    let mut shown: Vec<String> = marked_names.iter().map(|m| cut(m, share)).collect();
    if !more.is_empty() {
        shown.push(more);
    }
    let agents = match who {
        Who::Message { .. } => format!("{} to {}", shown[0], shown[1]),
        Who::Agents(_) => join_agents(&shown),
    };
    format!("{prefix}{words} ({agents}){tail}")
}

impl Mail {
    /// Records a line: its time, its agents and its words, under the next sequence number. No line
    /// is ever removed. `ids` are the identities of the agents of the event in the order the line
    /// names them; a list that does not match the agents named is recorded as none, never as a
    /// list that names the wrong session.
    pub fn record_line(
        &mut self,
        at: u64,
        words: &str,
        who: Who<'_>,
        ids: &[Identity],
        further: &[String],
    ) -> LineRecord {
        let text = build(at, words, who, further);
        let named = match who {
            Who::Message { .. } => 2,
            Who::Agents(a) => a.len(),
        };
        let agents = match who {
            Who::Message { from, to } => format!("{} to {}", marked(from), marked(to)),
            Who::Agents([]) => UNNAMED.to_owned(),
            Who::Agents(a) => join_agents(&a.iter().map(|n| marked(n)).collect::<Vec<_>>()),
        };
        let rec = self.next_rec();
        let record = LineRecord {
            seq: self.lines.last().map_or(1, |l| l.seq + 1),
            rec,
            at,
            words: std::iter::once(words.to_owned())
                .chain(further.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            agents,
            who: if ids.len() == named {
                ids.to_vec()
            } else {
                Vec::new()
            },
            text,
        };
        self.lines.push(record.clone());
        record
    }

    /// Every line recorded, in the order recorded.
    #[must_use]
    pub fn lines(&self) -> &[LineRecord] {
        &self.lines
    }

    /// The line as `reader`'s session reads it: with V1.7's definition after it when the line uses
    /// HELD and this session has not been told what HELD means yet (the first use only).
    pub fn line_for(&mut self, reader: &Identity, line: &str) -> String {
        let uses = uses_held(line);
        self.line_with(reader, line, uses)
    }

    /// [`Mail::line_for`] where the caller knows whether the words of the line use HELD.
    pub(super) fn line_with(&mut self, reader: &Identity, line: &str, uses: bool) -> String {
        if !uses || !self.held_defined.insert(reader.key()) {
            return line.to_owned();
        }
        // `build` left room for the definition in any line that uses HELD, so the line is still
        // at most 1,024 bytes with it.
        format!("{line} {}", held_definition())
    }
}
