//! Approvals: the user's yes to one exact call, used once, for 10 minutes.
//!
//! Spec: §8 A2.1 (an approval covers the session id, the tool name, the SHA-256 of the input in
//! RFC 8785 form, the folder the call runs in, and the ids of the messages holding the session),
//! A2.2 (what the prompt shows; an approval covers only what the prompt showed), A2.3 (10 minutes, once, void on any change; the session stays HELD), A2.4 (approving and using
//! are each one atomic step: each is one [`crate::store::Store::update`]).

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::Identity;
use crate::jcs::{input_hash, shown};
use crate::mail::Mail;
use crate::vocab::Verdict;

/// How long an approval lasts after it is given, in seconds (A2.3).
pub const APPROVAL_SECS: u64 = 600;

/// The user's approval of one call (A2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub session: Identity,
    pub tool: String,
    pub input_hash: String,
    /// The folder the call runs in, as the prompt showed it (an approval from before the folder was
    /// bound has none, and covers no call).
    #[serde(default)]
    pub cwd: String,
    /// Ids of the messages holding the session when it was given, sorted.
    pub holding: Vec<String>,
    pub given_at: u64,
    pub used: bool,
}

impl Approval {
    const fn live(&self, now: u64) -> bool {
        !self.used && now >= self.given_at && now < self.given_at.saturating_add(APPROVAL_SECS)
    }
}

/// How many calls waiting for the user's approval are kept; older ones are dropped.
pub const REQUESTS_KEPT: usize = 20;

/// `dir` with its `.` and empty parts dropped and each `..` taken against the part before it, so
/// two spellings of one folder (`/work/a`, `/work/b/../a/`) are one call's folder (§17.10, A2.1). A
/// `..` at the top of an absolute path stays at the top; a relative path keeps the `..` it cannot
/// take. Symbolic links are not followed: the guard judges the folder as the hook named it.
#[must_use]
pub fn normal_dir(dir: &str) -> String {
    let absolute = dir.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// A call the guard DENIED because nobody could be asked in that session (A1.2), kept so the user
/// can approve that exact call with `lunchroom approve` before the agent tries it again (A1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub session: Identity,
    /// The asking agent's name, for display only.
    pub agent: String,
    pub tool: String,
    pub input: Value,
    /// The folder the call runs in: the hook's working folder, or the command's own for a harness
    /// that gives each command one.
    #[serde(default)]
    pub cwd: String,
    /// The rule the call is outside of, as the guard named it (A1.4).
    #[serde(default)]
    pub rule: String,
    pub at: u64,
}

/// The store's own name for a waiting call: the key under which it keeps one request, so no two
/// waiting calls have the same one (the session, the tool, the folder and the SHA-256 of the
/// input; the second the call was made in is not part of it).
#[must_use]
pub fn call_key(request: &Request) -> String {
    format!(
        "{}|{}|{}|{}",
        request.session.key(),
        request.tool,
        request.cwd,
        input_hash(&request.input)
    )
}

/// The SHA-256 of what the user is shown of a waiting call: the agent, the tool, the folder, the
/// rule and the input. A waiting call that is shown differently has another one.
#[must_use]
pub fn call_shown_hash(request: &Request) -> String {
    input_hash(&serde_json::json!([
        request.agent,
        request.tool,
        request.cwd,
        request.rule,
        request.input
    ]))
}

/// How many lines telling an agent of an approval or a refusal are kept.
pub const TELLS_KEPT: usize = 100;

/// How long such a line is kept: a day.
pub const TELL_SECS: u64 = 86_400;

/// What the user decided about a waiting call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TellKind {
    Approved,
    Refused,
}

/// The line that tells an agent of the user's approval or refusal of one of its calls (A1.2,
/// §17.10): what `lunchroom read <id>` prints for its id, never put on a command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tell {
    pub id: String,
    pub session: Identity,
    pub kind: TellKind,
    /// The whole line: the decision, its id, the tool and the exact call.
    pub line: String,
    pub at: u64,
    /// Waiting for a Hermes session's next turn, which takes it through the plugin's
    /// `pre_llm_call` (A1.2); the other harnesses are handed an id-only line when it is made.
    pub pending: bool,
    /// Fields a later program wrote that this one does not know, carried through every read and
    /// write (D1.2).
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// A call the user refused (§17.10): the same call tried again is DENIED until the user approves
/// that exact call, or until the hold that carried it ends (no message of `holding` still holds the
/// session). It never lapses by time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub session: Identity,
    pub tool: String,
    pub input_hash: String,
    pub cwd: String,
    pub at: u64,
    pub id: String,
    /// The ids of the messages that held the session when the user declined, sorted (A2.1). Empty
    /// when none did: the decline then has no hold to end.
    #[serde(default)]
    pub holding: Vec<String>,
    /// Fields a later program wrote that this one does not know, carried through every read and
    /// write (D1.2).
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// The line telling an agent that the user `kind` its call `id` (A1.2, §17.10). Neither adds a hold
/// nor clears one (A2.1). The tool and the input are the agent's own text, written out as the
/// prompt showed them.
#[must_use]
pub fn tell_line(kind: TellKind, id: &str, tool: &str, input: &Value) -> String {
    let what = match kind {
        TellKind::Approved => format!(
            "the user approved one call of yours with `lunchroom approve` ({id}). It is not another \
             agent's message. You may run it again now, exactly as it was, once; you stay HELD."
        ),
        TellKind::Refused => format!(
            "the user declined one call of yours ({id}). It is not another agent's message. The call \
             stays DENIED: running it again is DENIED, and the reason says the user declined it."
        ),
    };
    format!(
        "Lunchroom: {what}\nTool: {}\nThe call's input:\n{}",
        visible(tool),
        shown(input),
    )
}

impl Mail {
    /// Keeps the line that tells `session` of the decision `id` (A1.2, §17.10); `pending` for a
    /// session that takes it at its next turn. The oldest go past [`TELLS_KEPT`].
    pub fn keep_tell(
        &mut self,
        id: &str,
        session: &Identity,
        kind: TellKind,
        line: String,
        now: u64,
        pending: bool,
    ) {
        self.tells.retain(|t| t.id != id);
        self.tells.push(Tell {
            id: id.to_owned(),
            session: session.clone(),
            kind,
            line,
            at: now,
            pending,
            extra: std::collections::BTreeMap::new(),
        });
        // Past the bound the oldest line already read goes first: a line still waiting for a Hermes
        // turn is dropped only when every line kept is waiting (A1.2: told at its next turn).
        while self.tells.len() > TELLS_KEPT {
            let at = self.tells.iter().position(|t| !t.pending).unwrap_or(0);
            self.tells.remove(at);
        }
    }

    /// What `lunchroom read <id>` prints for `id` when run by `me`: the line telling `me` of the
    /// decision. `None` for an id that does not exist and for one told to another session, which
    /// the command DENIES (D1.1).
    #[must_use]
    pub fn tell_line_for(&self, me: &Identity, id: &str) -> Option<&str> {
        self.tells
            .iter()
            .find(|t| t.id == id && t.session == *me)
            .map(|t| t.line.as_str())
    }

    /// `id` is a decision of the user told to `me`, whether or not it is still in force: the line
    /// that tells of it reaches the session's prompt hook, which adds no hold for it (A1.2).
    #[must_use]
    pub fn is_tell_of(&self, me: &Identity, id: &str) -> bool {
        self.tells.iter().any(|t| t.id == id && t.session == *me)
    }

    /// A line waits for the Hermes session `me`'s next turn.
    #[must_use]
    pub fn has_pending_tell(&self, me: &Identity) -> bool {
        self.tells.iter().any(|t| t.pending && t.session == *me)
    }

    /// The oldest line waiting for the Hermes session `me`, marked as told: it goes to the turn's
    /// context once.
    pub fn take_pending_tell(&mut self, me: &Identity) -> Option<String> {
        let tell = self
            .tells
            .iter_mut()
            .find(|t| t.pending && t.session == *me)?;
        tell.pending = false;
        Some(tell.line.clone())
    }

    /// Drops the lines older than [`TELL_SECS`] at `now` (a line stamped ahead is kept, and one
    /// still waiting for a Hermes turn) and the declines whose hold has ended
    /// ([`Mail::end_declines`]); a decline never lapses by time.
    pub fn forget_old_tells(&mut self, now: u64) {
        self.tells
            .retain(|t| t.pending || now < t.at.saturating_add(TELL_SECS));
        self.end_declines();
    }

    /// §17.10: ends each decline whose hold has ended: one made while messages held the session
    /// stands only while at least one of them still holds it (A2.1 binds an approval to the same
    /// ids). A decline made when no message held the session has no hold to end, and stands until
    /// the user approves that exact call. An ended decline is dropped, so a message that holds the
    /// session later never revives it.
    pub fn end_declines(&mut self) {
        let holds = &self.holds;
        self.declined.retain(|d| {
            d.holding.is_empty()
                || holds
                    .get(&d.session.key())
                    .is_some_and(|held| held.iter().any(|h| d.holding.contains(&h.message)))
        });
    }

    /// The user refuses the exact call of the waiting request at `index` (§17.10) and it is removed,
    /// only if it is still `expected`. The call is DENIED from now on, until the user approves that
    /// exact call or the hold that carried it ends ([`Mail::end_declines`]): a retry is DENIED with
    /// the reason that the user refused it ([`Mail::refused_at`]), and a line tells the agent
    /// through its harness's channel ([`Mail::tell_line_for`]).
    ///
    /// # Errors
    /// [`Changed`] when the request is gone or different.
    pub fn refuse_request(
        &mut self,
        index: usize,
        expected: &Request,
        now: u64,
    ) -> Result<String, Changed> {
        let r = self.requests.get(index).ok_or(Changed)?;
        // A request that has waited as long as an approval lasts is no longer waiting: nothing is
        // declined, as nothing could be approved.
        if r != expected || now >= r.at.saturating_add(APPROVAL_SECS) {
            return Err(Changed);
        }
        // No id is left to give when the counter has run out: nothing is recorded.
        let next = self.next_approval.checked_add(1).ok_or(Changed)?;
        let r = self.requests.remove(index);
        self.expire_requests(now);
        self.forget_old_tells(now);
        self.next_approval = next;
        let id = format!("rf-{:08}", self.next_approval);
        let hash = input_hash(&r.input);
        let holding = self.holding_ids(&r.session);
        let cwd = normal_dir(&r.cwd);
        self.declined.retain(|f| {
            !(f.session == r.session && f.tool == r.tool && f.cwd == cwd && f.input_hash == hash)
        });
        self.declined.push(Refusal {
            session: r.session.clone(),
            tool: r.tool.clone(),
            input_hash: hash,
            cwd,
            at: now,
            id: id.clone(),
            holding,
            extra: std::collections::BTreeMap::new(),
        });
        let line = tell_line(TellKind::Refused, &id, &r.tool, &r.input);
        let pending = r.session.harness == crate::identity::Harness::Hermes;
        self.keep_tell(&id, &r.session, TellKind::Refused, line, now, pending);
        // D1.8 (F2-AC52): a call the guard kept stops waiting, Denied, told to the user and the
        // agents waiting on it. The agent that asked hears it through A1.2's channel (the line
        // above; in Claude Code the retry's reason), so it is not added here.
        if let Some(call) = self.kept_call_id(&r) {
            self.call_ends_in(
                &r.session,
                &call,
                crate::mail::calls::Outcome::Denied,
                now,
                None,
            );
        }
        Ok(id)
    }

    /// When the user refused this exact call of `session` (tool, input and folder), if that is
    /// still in force: the guard DENIES the retry with the reason that the user refused it (§17.10).
    /// It stands until the user approves that exact call or the hold that carried it ends.
    #[must_use]
    pub fn refused_at(
        &self,
        session: &Identity,
        tool: &str,
        input: &Value,
        cwd: &str,
    ) -> Option<u64> {
        let hash = input_hash(input);
        let cwd = normal_dir(cwd);
        self.declined
            .iter()
            .find(|f| {
                f.session == *session && f.tool == tool && f.input_hash == hash && f.cwd == cwd
            })
            .map(|f| f.at)
    }

    /// Keeps a call waiting for the user's approval; the same call again replaces the older entry.
    /// A call that has waited [`APPROVAL_SECS`] or more by the time of this one is dropped.
    pub fn request(&mut self, request: Request) {
        self.expire_requests(request.at);
        let hash = input_hash(&request.input);
        self.requests.retain(|r| {
            !(r.session == request.session
                && r.tool == request.tool
                && r.cwd == request.cwd
                && input_hash(&r.input) == hash)
        });
        self.requests.push(request);
        let excess = self.requests.len().saturating_sub(REQUESTS_KEPT);
        self.requests.drain(..excess);
    }

    /// Drops the calls that have waited [`APPROVAL_SECS`] or more at `now`: the user's yes lasts as
    /// long as that after it is given, so a request older than that is no longer one anybody is
    /// asked about. A request stamped ahead of `now` (the clock stepped back) is kept.
    pub fn expire_requests(&mut self, now: u64) {
        self.requests
            .retain(|r| now < r.at.saturating_add(APPROVAL_SECS));
    }

    /// The calls waiting for the user's approval, oldest first.
    #[must_use]
    pub fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// The guard sent the call `call_id` of `session` to the user's own permission prompt (A1.1);
    /// `line` is what to say if it then runs (card 4). At most [`REQUESTS_KEPT`] are kept.
    pub fn asked(&mut self, session: &Identity, call_id: &str, line: &str) {
        self.asked
            .retain(|(s, c, _)| !(s == &session.key() && c == call_id));
        self.asked
            .push((session.key(), call_id.to_owned(), line.to_owned()));
        let excess = self.asked.len().saturating_sub(REQUESTS_KEPT);
        self.asked.drain(..excess);
    }

    /// The call `call_id` of `session` ran: when the guard had sent it to the user, the user said
    /// yes, and this is the line to say (card 4). The record is used once.
    pub fn ran(&mut self, session: &Identity, call_id: &str) -> Option<String> {
        let at = self
            .asked
            .iter()
            .position(|(s, c, _)| s == &session.key() && c == call_id)?;
        Some(self.asked.remove(at).2)
    }

    /// Approves the exact call of the waiting request at `index` (A2.1) and removes it, only if it
    /// is still `expected` and `shown` is still what [`Mail::approval_prompt`] says for it now: the
    /// user confirmed `shown`, so a message that has since held the session, or any other change to
    /// what the prompt showed, approves nothing (A2.2). Only the `lunchroom approve` command calls
    /// this, after reading the user's confirmation from a terminal no guarded session owns (A1.3);
    /// the guard DENIES an agent's `lunchroom approve` (A1.3, node `§7::11`).
    ///
    /// # Errors
    /// [`Changed`] when the request is gone or different, or the prompt would now read differently.
    pub fn approve_request(
        &mut self,
        index: usize,
        expected: &Request,
        shown: &str,
        now: u64,
    ) -> Result<String, Changed> {
        let r = self.requests.get(index).ok_or(Changed)?;
        if now >= r.at.saturating_add(APPROVAL_SECS) {
            return Err(Changed);
        }
        let now_shown = self.approval_prompt(&r.session, &r.agent, &r.tool, &r.input, &r.cwd);
        if r != expected || now_shown != shown {
            // A2.2 (D1.8): the call stops waiting, void, with the first cause that is true.
            self.kept_call_void(expected, now);
            return Err(Changed);
        }
        let r = self.requests.remove(index);
        // The approval of a call the guard kept has that call's id (A1.2), and the call stops
        // waiting: Granted, told to the user and the agents waiting (in Claude Code, which A1.2
        // gives no line, the agent that asked hears it by D too).
        let call = self.kept_call_id(&r);
        let id = self.approve_as(call.as_deref(), &r.session, &r.tool, &r.input, &r.cwd, now);
        // The line that tells the agent (A1.2); Hermes takes it at its next turn.
        let line = tell_line(TellKind::Approved, &id, &r.tool, &r.input);
        let pending = r.session.harness == crate::identity::Harness::Hermes;
        self.keep_tell(&id, &r.session, TellKind::Approved, line, now, pending);
        if let Some(call) = call {
            let asker = (r.session.harness == crate::identity::Harness::ClaudeCode).then_some(true);
            self.call_ends_in(
                &r.session,
                &call,
                crate::mail::calls::Outcome::Granted,
                now,
                asker,
            );
        }
        Ok(id)
    }
}

/// `text` as the user is shown it (A2.2), for text an agent supplies that is not JSON: a character a
/// terminal would act on or that reorders text (a control character, a line break among them, DEL,
/// the C1 controls, bidirectional overrides and isolates, line and paragraph separators) is written
/// as `\n`, `\r`, `\t` or `\u{XXXX}`, and a backslash as `\\`, so no line the prompt shows can be
/// rewritten and what is read is what the agent sent.
#[must_use]
pub fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control()
                || matches!(
                    c,
                    '\u{ad}'
                        | '\u{61c}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}'
                        | '\u{2060}'..='\u{2069}'
                        | '\u{feff}'
                ) =>
            {
                let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// What the user confirmed is no longer what is waiting: nothing was approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Changed;

impl Mail {
    fn holding_ids(&self, session: &Identity) -> Vec<String> {
        let mut ids: Vec<String> = self
            .holding(session)
            .iter()
            .map(|h| h.message.clone())
            .collect();
        ids.sort();
        ids
    }

    /// What the user is shown before approving (A2.2): the call in full, the agent asking, and the
    /// folder it runs in, and the messages holding the session with their senders' names and ids.
    /// An approval covers exactly these, nothing more (A2.2).
    #[must_use]
    pub fn approval_prompt(
        &self,
        session: &Identity,
        agent: &str,
        tool: &str,
        input: &Value,
        cwd: &str,
    ) -> String {
        let holding: Vec<String> = self
            .holding(session)
            .iter()
            .map(|h| format!("{} ({})", visible(&h.from_name), visible(&h.message)))
            .collect();
        let holding = if holding.is_empty() {
            "no messages".to_owned()
        } else {
            holding.join(", ")
        };
        // The agent, tool and folder are the agent's own text: a line break or an escape sequence
        // in one of them must not rewrite the lines around it.
        format!(
            "{} asks to run one call.\nTool: {}\nThe call's input:\n{}\nRuns in: {}\nHeld by: {holding}",
            visible(agent),
            visible(tool),
            shown(input),
            visible(cwd)
        )
    }

    /// The user approves this one call now (A2.1). Spent and expired approvals are dropped.
    pub fn approve(
        &mut self,
        session: &Identity,
        tool: &str,
        input: &Value,
        cwd: &str,
        now: u64,
    ) -> String {
        self.approve_as(None, session, tool, input, cwd, now)
    }

    /// [`Mail::approve`] with the id of the kept call it approves (A1.2), when there is one.
    pub(crate) fn approve_as(
        &mut self,
        call: Option<&str>,
        session: &Identity,
        tool: &str,
        input: &Value,
        cwd: &str,
        now: u64,
    ) -> String {
        // An approval unused 10 minutes after it was given is told before it is forgotten.
        self.settle_unused(now);
        self.approvals.retain(|a| a.live(now));
        // A yes given after a refusal of the same call is the user's later word.
        let hash = input_hash(input);
        let folder = normal_dir(cwd);
        self.declined.retain(|f| {
            !(f.session == *session && f.tool == tool && f.cwd == folder && f.input_hash == hash)
        });
        let id = if let Some(call) = call {
            call.to_owned()
        } else {
            self.next_approval = self.next_approval.saturating_add(1);
            format!("ap-{:08}", self.next_approval)
        };
        let approval = Approval {
            id: id.clone(),
            session: session.clone(),
            tool: tool.to_owned(),
            input_hash: input_hash(input),
            cwd: cwd.to_owned(),
            holding: self.holding_ids(session),
            given_at: now,
            used: false,
        };
        self.approvals.push(approval);
        id
    }

    /// `id` is an approval of a call of `session`, not used and not expired (A1.2: the line that
    /// tells a Codex session of it).
    #[must_use]
    pub fn approval_live(&self, session: &Identity, id: &str, now: u64) -> bool {
        self.approvals
            .iter()
            .any(|a| a.id == id && a.session == *session && a.live(now))
    }

    /// The call `session` makes now: GRANTED, using up its approval, only when an approval live for
    /// this exact session, tool, input, folder and set of holding messages exists (A2.1, A2.3); otherwise
    /// DENIED. The session stays HELD either way (A2.3).
    pub fn use_approval(
        &mut self,
        session: &Identity,
        tool: &str,
        input: &Value,
        cwd: &str,
        now: u64,
    ) -> Verdict {
        let hash = input_hash(input);
        let holding = self.holding_ids(session);
        let found = self.approvals.iter_mut().find(|a| {
            a.live(now)
                && a.session == *session
                && a.tool == tool
                && a.input_hash == hash
                && a.cwd == cwd
                && a.holding == holding
        });
        match found {
            Some(approval) => {
                approval.used = true;
                Verdict::Granted
            }
            None => Verdict::Denied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Harness;
    use serde_json::json;

    // §8::2 — text an agent supplies is shown with every character a terminal would act
    // on written out, so it cannot rewrite a line; a plain name or path is unchanged.
    #[test]
    fn agent_text_is_shown_with_control_characters_written_out() {
        for (text, shown) in [
            ("/work/a", "/work/a"),
            ("Claude Code", "Claude Code"),
            ("a\nHeld by: no messages", "a\\nHeld by: no messages"),
            ("a\rb\tc", "a\\rb\\tc"),
            ("\u{1b}[2K\u{1b}[1A", "\\u{001b}[2K\\u{001b}[1A"),
            ("x\u{7f}y\u{85}z", "x\\u{007f}y\\u{0085}z"),
            (
                "a\u{202e}b\u{2066}c\u{2028}d",
                "a\\u{202e}b\\u{2066}c\\u{2028}d",
            ),
            ("C:\\n", "C:\\\\n"),
            ("", ""),
        ] {
            assert_eq!(visible(text), shown, "{text:?}");
        }
    }

    // §8::2 — a line break or escape in the agent, tool or folder cannot add or replace
    // a line of the prompt: it has exactly the five lines the clause lists.
    #[test]
    fn the_approval_prompt_cannot_be_rewritten_by_the_agent_s_text() {
        let mail = Mail::default();
        let session = Identity::new(Harness::ClaudeCode, "s1");
        let prompt = mail.approval_prompt(
            &session,
            "Claude\nTool: Read",
            "Bash\u{1b}[1A",
            &json!({"command": "ls"}),
            "/work\nHeld by: nobody\u{1b}[2K",
        );
        assert_eq!(prompt.lines().count(), 6, "{prompt:?}");
        assert!(prompt.starts_with("Claude\\nTool: Read asks to run one call.\n"));
        assert!(prompt.contains("\nTool: Bash\\u{001b}[1A\n"), "{prompt:?}");
        assert!(
            prompt.contains("\nRuns in: /work\\nHeld by: nobody\\u{001b}[2K\n"),
            "{prompt:?}"
        );
        assert!(prompt.ends_with("\nHeld by: no messages"), "{prompt:?}");
        assert!(!prompt.contains('\u{1b}'));
    }
}
