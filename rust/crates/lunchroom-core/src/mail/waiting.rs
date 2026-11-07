//! No message waits without an end, and the user and the sender are told (spec §10 D1.7–D1.9).
//!
//! D1.7 (docs/spec/lunchroom-spec.md §10 item 7): "Lunchroom checks every QUEUED message's time
//! limits whenever any of its programs runs … A message to an agent that is no longer named, or no
//! longer takes messages after `leave`, is FAILED at once with that reason." D1.8 (item 8): "Each
//! notice names the message id, the recipient, how long ago it was sent, the attempt (n of 3), what
//! Lunchroom is waiting for (for Codex: the message id in that session's record, or its next
//! prompt), when it will be offered again, and what the user can do." D1.9 (item 9): `lunchroom
//! status` "shows every QUEUED message, whatever its recipient, with the same facts".
//!
//! This part keeps the facts and the notices; the checks that need a harness (Codex's session
//! record, a hand-over through the harness's own program) run in the `lunchroom` program.

use super::{
    CONFIRM_SECS, HANDOVER_SECS, MAX_ATTEMPTS, Mail, Message, Notice, RETRY_GAP_SECS, Status,
    WAITING_TOLD_SECS, push_notice,
};
use crate::identity::{Harness, Identity};
use crate::vocab::{MessageState, Word};

/// How a notice stands to the unread ones of the same message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// An event: an attempt that failed, FAILED, DELIVERED. Every one is kept.
    Event,
    /// A reason the message waits or cannot be handed over: it replaces an older unread reason.
    Reason,
    /// The reason no longer holds: it replaces the unread reasons.
    Clears,
}

/// A span of seconds as people read it: `45 s`, `3 min`, `2 h 5 min`.
#[must_use]
pub fn span(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3_600 => format!("{} min", s / 60),
        s => format!("{} h {} min", s / 3_600, s % 3_600 / 60),
    }
}

/// A time of day from seconds since the Unix epoch: `14:05 UTC`.
#[must_use]
pub fn clock(at: u64) -> String {
    let day = at % 86_400;
    format!("{:02}:{:02} UTC", day / 3_600, day % 3_600 / 60)
}

/// What Lunchroom waits for from the recipient's harness before a message is DELIVERED (D1.1).
#[must_use]
pub const fn waiting_for(harness: Harness) -> &'static str {
    match harness {
        Harness::Codex => "the message id in that session's record, or its next prompt",
        Harness::ClaudeCode => "its delivery prompt to reach the session's guard",
        Harness::OpenClaw => "the turn's prompt to reach Lunchroom's plugin",
        Harness::Hermes => "the session's next turn to carry it to the model",
    }
}

/// When a message that is not in front of its recipient's harness is offered, by harness (D1.1,
/// D1.7). Only a program that runs says it will offer: a Claude Code or Hermes message is offered
/// by the session's listener, so it waits for that session to run.
fn offered_when(m: &Message) -> String {
    match m.to.harness {
        Harness::Codex | Harness::OpenClaw if m.failed_attempts > 0 => {
            "it is offered again at the next Lunchroom check (any guard hook, `lunchroom send` or \
             `lunchroom status`)"
                .to_owned()
        }
        Harness::Codex => format!(
            "it is offered at {}'s next `lunchroom send` to it, or when {}'s session starts or \
             ends a turn",
            m.from_name, m.to_name
        ),
        Harness::OpenClaw => format!(
            "it is offered at {}'s next `lunchroom send` to it, or when {}'s turn ends",
            m.from_name, m.to_name
        ),
        Harness::ClaudeCode => {
            "it is offered by the session's listener, which looks every second while the session \
             runs"
                .to_owned()
        }
        Harness::Hermes => "it is offered at the start of the session's next turn".to_owned(),
    }
}

/// What happens to a message that is in front of its harness and is not confirmed in time: a
/// message handed over is offered again by the next Lunchroom check; one only offered (no
/// program of Lunchroom's hands it over) is offered again by its recipient's listener when that
/// session runs, which is the only program that offers it.
fn after_wait(m: &Message, last_attempt: bool) -> String {
    if last_attempt {
        return "it is FAILED".to_owned();
    }
    if m.handed_over {
        return "it is offered again".to_owned();
    }
    match m.to.harness {
        Harness::Codex | Harness::OpenClaw => {
            "it counts as a failed attempt and is offered again at the next Lunchroom check"
                .to_owned()
        }
        Harness::ClaudeCode => {
            "it counts as a failed attempt, and the session's listener offers it \
             again when that session runs"
                .to_owned()
        }
        Harness::Hermes => {
            "it counts as a failed attempt and is offered again at the start of the session's \
             next turn"
                .to_owned()
        }
    }
}

/// D1.8, D1.9: the facts of the QUEUED message `m` at `now` after its recipient: its age, the
/// attempt (n of 3), its stage (waiting to be handed over, handed over and since when, or why it
/// cannot be handed over; the reason only with `with_reason`), what Lunchroom waits for, and when
/// it is offered again.
fn facts(m: &Message, now: u64, with_reason: bool) -> String {
    let attempt = (m.failed_attempts + 1).min(MAX_ATTEMPTS);
    let age = span(now.saturating_sub(m.sent_at));
    let in_front = m.presented_at.filter(|_| m.status == Status::Queued);
    let (stage, again) = match (in_front, cannot_hand_over(m)) {
        (Some(at), _) => {
            let limit = if m.handed_over {
                HANDOVER_SECS
            } else {
                CONFIRM_SECS
            };
            let next = after_wait(m, attempt >= MAX_ATTEMPTS);
            let due = clock(at.saturating_add(limit));
            (
                format!(
                    "handed over {} ago, at {}, not yet confirmed",
                    span(now.saturating_sub(at)),
                    clock(at)
                ),
                if m.handed_over {
                    format!("{next} at {due} if not confirmed by then")
                } else {
                    format!("if not confirmed by {due}, {next}")
                },
            )
        }
        (None, Some(reason)) if with_reason => (
            format!("it cannot be handed over: {reason}"),
            "it is offered once it can be handed over".to_owned(),
        ),
        (None, Some(_)) => (
            "waiting for that to clear".to_owned(),
            "it is offered once it can be handed over".to_owned(),
        ),
        (None, None) => (
            match &m.told.last_failure {
                Some(reason) if m.failed_attempts > 0 => format!(
                    "waiting to be handed over (attempt {} failed: {reason})",
                    m.failed_attempts
                ),
                _ => "waiting to be handed over".to_owned(),
            },
            offered_when(m),
        ),
    };
    format!(
        "sent {age} ago by {}, attempt {attempt} of {MAX_ATTEMPTS}: {stage}; Lunchroom waits for \
         {}; {again}",
        m.from_name,
        waiting_for(m.to.harness)
    )
}

/// D1.8, D1.9: where the QUEUED message `m` stands at `now`: its recipient, its age, the attempt (n
/// of 3), its stage (waiting to be handed over, handed over and since when, or why it cannot be
/// handed over), what Lunchroom waits for, and when it is offered again.
#[must_use]
pub fn standing(m: &Message, now: u64) -> String {
    format!(
        "{} to {} ({}), {}",
        m.id,
        m.to_name,
        m.to,
        facts(m, now, true)
    )
}

/// Why `m` cannot be handed over now, if it cannot: its recipient's state first, then what a
/// hand-over found.
#[must_use]
pub fn cannot_hand_over(m: &Message) -> Option<&str> {
    m.told.unreachable.as_deref().or(m.told.blocked.as_deref())
}

/// `reason` as one sentence: it ends with a full stop, once.
fn sentence(reason: &str) -> String {
    format!("{}.", reason.trim().trim_end_matches('.'))
}

/// What the user can do about a message that waits (D1.8): it is always the same way out. When a
/// reason says what to fix, that is the way to keep the message; this ends the wait instead.
fn user_can(m: &Message) -> String {
    let user = Word::TheUser.text();
    let lead = if cannot_hand_over(m).is_some() {
        "To end the wait without fixing that,"
    } else {
        "Nothing is needed; to end the wait,"
    };
    format!(
        " {lead} {user} can run `lunchroom unregister {}` in a terminal of their own; the message \
         is then {} and its sender is told.",
        m.to_name,
        MessageState::Failed.word()
    )
}

impl Mail {
    /// D1.8: tells the sender of `id`, and the user, `text`, in the next prompt or listener of any
    /// of their guarded sessions; the message's DELIVERED is then told too.
    pub(super) fn tell(&mut self, id: &str, text: &str) {
        self.tell_as(id, text, Kind::Event);
    }

    /// [`Mail::tell`] with how the notice stands to the unread ones of the same message. A reason
    /// the sender was already given when it sent the message (`told.sender_told`) goes to the user
    /// only.
    pub(super) fn tell_as(&mut self, id: &str, text: &str, kind: Kind) {
        let Some(message) = self.messages.get_mut(id) else {
            return;
        };
        message.told.any = true;
        let skip_sender = kind == Kind::Reason && message.told.sender_told;
        let notice = Notice {
            message: id.to_owned(),
            text: text.to_owned(),
            at: self.seen_at,
            reason: kind == Kind::Reason,
        };
        let clears = kind == Kind::Clears;
        if !skip_sender {
            push_notice(
                self.notices.entry(message.from.key()).or_default(),
                notice.clone(),
                clears,
            );
        }
        push_notice(&mut self.user_notices, notice, clears);
    }

    /// The notices waiting for the user (D1.8).
    #[must_use]
    pub fn user_notices(&self) -> &[Notice] {
        &self.user_notices
    }

    /// Takes the notices waiting for the user, to tell them now (D1.8); each is told once. A caller
    /// that writes its output after this, and could be killed before, uses [`Mail::user_notices`]
    /// and [`Mail::mark_user_notices_told`] instead.
    pub fn take_user_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.user_notices)
    }

    /// Every QUEUED message, whatever its recipient, oldest first (D1.9).
    #[must_use]
    pub fn queued(&self) -> Vec<&Message> {
        let mut all: Vec<&Message> = self
            .messages
            .values()
            .filter(|m| m.status == Status::Queued)
            .collect();
        all.sort_by(|a, b| (a.sent_at, &a.id).cmp(&(b.sent_at, &b.id)));
        all
    }

    /// Every FAILED message, whatever its recipient, oldest first, each with why it failed.
    #[must_use]
    pub fn failed(&self) -> Vec<(&Message, &str)> {
        let mut all: Vec<(&Message, &str)> = self
            .messages
            .values()
            .filter_map(|m| match &m.status {
                Status::Failed { reason } => Some((m, reason.as_str())),
                _ => None,
            })
            .collect();
        all.sort_by(|a, b| (a.0.sent_at, &a.0.id).cmp(&(b.0.sent_at, &b.0.id)));
        all
    }

    /// Tells the user `text` (1.1: the console's probing alert), in the next place that can show
    /// it. It belongs to no message.
    pub fn tell_user(&mut self, text: &str, at: u64) {
        let notice = Notice {
            message: String::new(),
            text: text.to_owned(),
            at,
            reason: false,
        };
        push_notice(&mut self.user_notices, notice, false);
    }

    /// The latest time a check of D1.7 ran at, for what is told without a clock of its own (a
    /// confirmation's notice).
    pub(super) fn saw(&mut self, now: u64) {
        self.seen_at = self.seen_at.max(now);
    }

    /// The sender of `id` was told, in the report of its own `lunchroom send`, why the message
    /// cannot be handed over: that reason is not queued to the sender again, only to the user.
    pub fn sender_told_reason(&mut self, id: &str) {
        if let Some(m) = self
            .messages
            .get_mut(id)
            .filter(|m| m.status == Status::Queued)
        {
            m.told.sender_told = true;
        }
    }

    /// D1.7: every QUEUED message to an agent that is no longer named, or no longer takes messages
    /// after `leave`, or whose name was moved to another session, is FAILED at once with that
    /// reason, and the user and the sender are told. The ids FAILED. A session another name still
    /// labels keeps receiving what is addressed to that name.
    pub fn fail_unnamed(&mut self, now: u64) -> Vec<String> {
        self.saw(now);
        let gone: Vec<String> = self
            .messages
            .values()
            .filter(|m| m.status == Status::Queued)
            .filter(|m| {
                !self.receivers.contains(&m.to.key()) || !self.names.values().any(|h| *h == m.to)
            })
            .map(|m| m.id.clone())
            .collect();
        for id in &gone {
            let Some(m) = self.messages.get(id) else {
                continue;
            };
            let moved = self.names.get(&m.to_name).is_some_and(|h| *h != m.to);
            let reason = if moved {
                format!(
                    "{} now names another session: its name was moved to that session",
                    m.to_name
                )
            } else {
                format!(
                    "{} no longer takes messages: it left with `leave`, or its name was taken away",
                    m.to_name
                )
            };
            let Some(m) = self.messages.get_mut(id) else {
                continue;
            };
            m.presented_at = None;
            m.handed_over = false;
            m.status = Status::Failed {
                reason: reason.clone(),
            };
            let text = format!(
                "{id} {}: {reason} (to {}, sent {} ago by {}). Lunchroom was waiting for {} to \
                 take it and will not now. To try again, the user names the session again with \
                 `lunchroom register {} {}`, and {} sends the message again.",
                MessageState::Failed.word(),
                m.to_name,
                span(now.saturating_sub(m.sent_at)),
                m.from_name,
                m.to_name,
                m.to_name,
                m.to,
                m.from_name,
            );
            self.tell(id, &text);
        }
        gone
    }

    /// D1.8: why the QUEUED message `id` cannot be handed over, as a hand-over found it (its
    /// program cannot be established), or `None` once a hand-over goes ahead; a new reason is told
    /// at the next [`Mail::tell_waiting`], as soon as it is found. When a reason the user was told
    /// no longer holds, they are told that.
    pub fn note_blocked(&mut self, id: &str, reason: Option<String>) {
        let Some(m) = self
            .messages
            .get_mut(id)
            .filter(|m| m.status == Status::Queued)
        else {
            return;
        };
        if m.told.blocked == reason {
            return;
        }
        let cleared = reason.is_none() && m.told.blocked_told && m.told.unreachable.is_none();
        if m.told.blocked.is_some() && reason.is_some() {
            m.told.sender_told = false;
        }
        m.told.blocked = reason;
        m.told.blocked_told = false;
        if cleared {
            self.tell_cleared(id);
        }
    }

    /// D1.8: why the QUEUED message `id` cannot be handed over from its recipient's state
    /// (UNGUARDED, with its R3.4 reason, or STOPPED), or `None` while the recipient is guarded; told
    /// as [`Mail::note_blocked`] is.
    pub fn note_unreachable(&mut self, id: &str, reason: Option<String>) {
        let Some(m) = self
            .messages
            .get_mut(id)
            .filter(|m| m.status == Status::Queued)
        else {
            return;
        };
        if m.told.unreachable == reason {
            return;
        }
        let cleared = reason.is_none() && m.told.unreachable_told && m.told.blocked.is_none();
        if m.told.unreachable.is_some() && reason.is_some() {
            m.told.sender_told = false;
        }
        m.told.unreachable = reason;
        m.told.unreachable_told = false;
        if cleared {
            self.tell_cleared(id);
        }
    }

    /// Tells that the reason the user and the sender were given for `id` no longer holds, and that
    /// Lunchroom hands the message over now.
    fn tell_cleared(&mut self, id: &str) {
        let Some(m) = self.messages.get(id) else {
            return;
        };
        let text = format!(
            "{id} to {} can be handed over again: what stopped it no longer applies, so Lunchroom \
             offers it now (sent {} ago by {}).",
            m.to_name,
            span(self.seen_at.saturating_sub(m.sent_at)),
            m.from_name
        );
        self.tell_as(id, &text, Kind::Clears);
    }

    /// D1.8: the user and the sender are told of each hand-over with no confirmation for
    /// [`WAITING_TOLD_SECS`], and of each message that cannot be handed over, as soon as that is
    /// found (owner's decision, 2026-11-02: always told, with no wait); each once, and a reason
    /// again only when it changes. A reason is told once, with the way out.
    pub fn tell_waiting(&mut self, now: u64) {
        self.saw(now);
        let mut told: Vec<(String, String)> = Vec::new();
        for m in self.messages.values_mut() {
            if m.status != Status::Queued {
                continue;
            }
            if let Some(at) = m.presented_at
                && now >= at.saturating_add(WAITING_TOLD_SECS)
                && m.told.waiting != Some(at)
            {
                m.told.waiting = Some(at);
                let text = format!(
                    "{} to {}: no confirmation {} after its hand-over. It was {}.{}",
                    m.id,
                    m.to_name,
                    span(now - at),
                    facts(m, now, true),
                    user_can(m)
                );
                told.push((m.id.clone(), text));
            } else if m.presented_at.is_none() {
                // The recipient's state first, then what a hand-over found; each reason once.
                let reason = if let Some(r) = m.told.unreachable.clone()
                    && !m.told.unreachable_told
                {
                    m.told.unreachable_told = true;
                    Some(r)
                } else if m.told.unreachable.is_none()
                    && let Some(r) = m.told.blocked.clone()
                    && !m.told.blocked_told
                {
                    m.told.blocked_told = true;
                    Some(r)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    // The reason once; the facts after it do not repeat it.
                    let text = format!(
                        "{} to {} cannot be handed over: {} It was {}.{}",
                        m.id,
                        m.to_name,
                        sentence(&reason),
                        facts(m, now, false),
                        user_can(m)
                    );
                    told.push((m.id.clone(), text));
                }
            }
        }
        for (id, text) in told {
            self.tell_as(&id, &text, Kind::Reason);
        }
    }

    /// The recipients a check offers a message to again (D1.7): those a hand-over goes to through
    /// the harness's own program (Codex, OpenClaw), with a QUEUED message that is not in front of
    /// the harness now and is not handed over: one whose earlier hand-over counted as failed, or
    /// that has waited [`RETRY_GAP_SECS`] and was never handed over (its recipient could not take it
    /// then, and can now).
    #[must_use]
    pub fn to_offer_again(&self, now: u64) -> Vec<Identity> {
        let mut out: Vec<Identity> = Vec::new();
        for m in self.messages.values() {
            if m.status == Status::Queued
                && matches!(m.to.harness, Harness::Codex | Harness::OpenClaw)
                && (m.failed_attempts > 0 || now.saturating_sub(m.sent_at) >= RETRY_GAP_SECS)
                && !m.handed_over
                && m.presented_at.is_none()
                && self.receivers.contains(&m.to.key())
                && !out.contains(&m.to)
            {
                out.push(m.to.clone());
            }
        }
        out
    }

    /// The text told when a message that something was told of is DELIVERED (D1.8).
    pub(super) fn delivered_text(m: &Message, now: u64) -> String {
        format!(
            "{} to {} is {}, {} after it was sent by {}, at attempt {} of {MAX_ATTEMPTS}.",
            m.id,
            m.to_name,
            MessageState::Delivered.word(),
            span(now.saturating_sub(m.sent_at)),
            m.from_name,
            (m.failed_attempts + 1).min(MAX_ATTEMPTS)
        )
    }

    /// The text told when an attempt on `m` counts as failed for `reason` (D1.8): the attempt, what
    /// Lunchroom needed and did not get, where the message stands now, and what can be done.
    pub(super) fn attempt_text(m: &Message, reason: &str, now: u64) -> String {
        let waited = waiting_for(m.to.harness);
        if m.status == Status::Queued {
            let when = m
                .told
                .retry_after
                .filter(|t| *t > now)
                .map_or_else(String::new, |t| format!(", not before {}", clock(t)));
            format!(
                "{} to {}: attempt {} of {MAX_ATTEMPTS} failed ({reason}). Lunchroom was waiting \
                 for {waited}, and did not get it. The message is still {}: {}{when}; the next try \
                 is attempt {} of {MAX_ATTEMPTS}.{}",
                m.id,
                m.to_name,
                m.failed_attempts,
                MessageState::Queued.word(),
                offered_when(m),
                m.failed_attempts + 1,
                user_can(m)
            )
        } else {
            format!(
                "{} {}: {reason} (to {}, sent {} ago by {}, after {MAX_ATTEMPTS} of {MAX_ATTEMPTS} \
                 attempts). Lunchroom was waiting for {waited}, and did not get it in any of them, \
                 so it gave up and nothing was delivered. To try again, {} sends the message again; \
                 `lunchroom status` shows whether {} is available first.",
                m.id,
                MessageState::Failed.word(),
                m.to_name,
                span(now.saturating_sub(m.sent_at)),
                m.from_name,
                m.from_name,
                m.to_name
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // D1.8: spans and times as people read them.
    #[test]
    fn spans_and_clock_times_read_plainly() {
        assert_eq!(span(45), "45 s");
        assert_eq!(span(180), "3 min");
        assert_eq!(span(7_500), "2 h 5 min");
        assert_eq!(clock(1_792_591_500), "14:05 UTC");
    }

    // A reason is one sentence that ends once, whether or not it came with a full stop.
    #[test]
    fn a_reason_is_one_sentence() {
        assert_eq!(sentence("codex is STOPPED."), "codex is STOPPED.");
        assert_eq!(sentence("codex is STOPPED"), "codex is STOPPED.");
        assert_eq!(sentence(" codex is STOPPED.. "), "codex is STOPPED.");
    }
}
