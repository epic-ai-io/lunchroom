//! Lines told to hearers (channels D, N, U of §16 item 2) and the calls waiting for the user
//! (spec §10 D1.8, "Calls waiting for the user").
//!
//! D1.8: "A call waiting for the user is a call that the guard sends to the user or keeps for the
//! user ...: the call of a HELD agent or, for a call the guard could not decide (H1.5), of any
//! agent. Where the agent is not HELD, only the agents with a message QUEUED for it wait on the
//! call. Lunchroom tells the user, and each agent that waits on the agent that made the call, when
//! the call starts to wait and again when it stops. ... An agent is told once for each call and
//! each event, however many of its messages hold the session or wait."

use serde::{Deserialize, Serialize};

use super::lines::{LineRecord, Who};
use super::{Mail, Notice, Status, push_notice};
use crate::identity::Identity;

/// How long a call the guard keeps waits for the user: 10 minutes (A2.3).
const CALL_LAPSE_SECS: u64 = 600;

/// An agent that hears a line, and by which of the channels D and N it hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hearer {
    pub who: Identity,
    /// Channel D: the session's own notice, told by the hooks of its harness. Not for an UNGUARDED
    /// or STOPPED agent, which has no hook Lunchroom can trust (R3.1, K1.29).
    pub d: bool,
    /// Channel N: the line stands in the next envelope Lunchroom builds for the session.
    pub n: bool,
}

/// How a call waiting for the user ended (D1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Granted,
    Denied,
    /// Lapsed, 10 minutes after the agent asked (A2.3).
    NotApprovedInTime,
    /// Approved and not used, 10 minutes after the approval (A2.3).
    NotUsedInTime,
    /// A2.2: by the time the user confirms, a message has held the session.
    VoidMessageHeld,
    /// A2.2: a hold has ended.
    VoidHoldEnded,
    /// A2.2: the request has changed.
    VoidCallChanged,
}

impl Outcome {
    /// The three Approval void lines are for a call the guard keeps only.
    const fn is_void(self) -> bool {
        matches!(
            self,
            Self::VoidMessageHeld | Self::VoidHoldEnded | Self::VoidCallChanged
        )
    }

    /// The first sentence of the outcome's notice, as D1.8 words it.
    fn words(self, agent: &str, id: &str, action: &str) -> String {
        match self {
            Self::Granted => crate::cards::granted(agent, action),
            Self::Denied => format!("Denied. {agent}'s call {id} was not approved."),
            Self::NotApprovedInTime => {
                format!("Not approved in time. {agent}'s call {id} was dropped.")
            }
            Self::NotUsedInTime => {
                format!("Not used in time. {agent}'s approval of call {id} expired.")
            }
            Self::VoidMessageHeld => format!(
                "Approval void: a message held the session. {agent}'s call {id} was not approved."
            ),
            Self::VoidHoldEnded => {
                format!("Approval void: the hold ended. {agent}'s call {id} was not approved.")
            }
            Self::VoidCallChanged => {
                format!("Approval void: the call changed. {agent}'s call {id} was not approved.")
            }
        }
    }

    /// The first of the causes that are true, in D1.8's order: a message held the session, the
    /// hold ended, the call changed.
    #[must_use]
    pub fn first_void(message_held: bool, hold_ended: bool, call_changed: bool) -> Option<Self> {
        if message_held {
            Some(Self::VoidMessageHeld)
        } else if hold_ended {
            Some(Self::VoidHoldEnded)
        } else if call_changed {
            Some(Self::VoidCallChanged)
        } else {
            None
        }
    }
}

/// An agent that waits on a call, as it was when the call started to wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub who: Identity,
    /// Whether it hears by channel D (its harness has a guard hook Lunchroom trusts).
    pub d: bool,
}

/// A call waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingCall {
    pub id: String,
    pub session: Identity,
    pub agent: String,
    /// The verb of the call (`merge`, `run npm`, `use Write`), as [`crate::cards::action_of`] says.
    pub action: String,
    pub asked_at: u64,
    /// The guard keeps the call for `lunchroom approve` (it has an approval of Lunchroom's); a call
    /// a harness's own prompt answers is not kept.
    pub kept: bool,
    /// The agents told at the start, each once; the stop notice goes to the same.
    pub waiters: Vec<Waiter>,
    /// For a kept call, what names its request (tool, folder, hash of the input): the approval of
    /// that request has this call's id (A1.2).
    #[serde(default)]
    pub key: String,
    /// The ids of the messages holding the session when the call started (A2.2: what the voids
    /// compare with).
    #[serde(default)]
    pub holding: Vec<String>,
}

/// An approval given for a kept call that is waiting to be used: if it is still unused 10 minutes
/// after it was given, the user and the agents told when the call started hear "Not used in time"
/// (D1.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnusedWatch {
    pub approval: String,
    pub session: Identity,
    pub agent: String,
    pub given_at: u64,
    pub waiters: Vec<Waiter>,
}

/// The key that names a request among a session's kept calls.
pub(super) fn request_key(tool: &str, cwd: &str, input: &serde_json::Value) -> String {
    format!("{tool}\u{0}{cwd}\u{0}{}", crate::jcs::input_hash(input))
}

impl Mail {
    /// Tells one line (§16 item 2): records it, tells the user's list when `user`, tells each
    /// hearer by D (its own notice, with V1.7's definition at the first use of HELD to that
    /// session) and puts it in the next envelope for each hearer by N. Returns the record.
    #[allow(clippy::too_many_arguments)] // one line's facts: when, words, agents, who, more, hearers
    pub fn tell_line(
        &mut self,
        at: u64,
        words: &str,
        who: Who<'_>,
        ids: &[Identity],
        further: &[String],
        hearers: &[Hearer],
        user: bool,
    ) -> LineRecord {
        let record = self.record_line(at, words, who, ids, further);
        if user {
            push_notice(
                &mut self.user_notices,
                Notice {
                    message: String::new(),
                    text: record.text.clone(),
                    at,
                    reason: false,
                },
                false,
            );
        }
        // A hearer named twice hears once on each channel (a line stands in one envelope only).
        let mut once: Vec<Hearer> = Vec::new();
        for h in hearers {
            match once.iter_mut().find(|o| o.who == h.who) {
                Some(o) => {
                    o.d |= h.d;
                    o.n |= h.n;
                }
                None => once.push(h.clone()),
            }
        }
        let held = super::lines::uses_held(words);
        for h in &once {
            if h.d {
                let text = self.line_with(&h.who, &record.text, held);
                let list = self.notices.entry(h.who.key()).or_default();
                push_notice(
                    list,
                    Notice {
                        message: String::new(),
                        text,
                        at,
                        reason: false,
                    },
                    false,
                );
            }
            if h.n {
                // A line identical to one already waiting for this session's envelope is not put
                // in twice (the same event, told again before the envelope was built).
                let waiting_already = self.envelope_pending.get(&h.who.key()).is_some_and(|seqs| {
                    seqs.iter().any(|s| {
                        self.lines
                            .iter()
                            .any(|l| l.seq == *s && l.text == record.text)
                    })
                });
                if !waiting_already {
                    self.envelope_pending
                        .entry(h.who.key())
                        .or_default()
                        .push(record.seq);
                }
            }
        }
        record
    }

    /// The lines for the envelope of message `id`, built at `at` (§16 item 2, channel N): first
    /// the line "<agent> is HELD." that the delivery makes true (card 1, line 2), then each line
    /// waiting for the recipient's session, in the order recorded. The first line to use HELD
    /// carries V1.7's definition if this session has not been told it. Nothing is taken: the
    /// lines are marked shown when the message is confirmed ([`Mail::confirm_at`]), so an
    /// envelope that is built again (a failed hand-over) carries them again, and a line stands in
    /// one envelope only.
    pub fn envelope_lines(&mut self, id: &str, at: u64) -> Vec<String> {
        let Some(message) = self.messages.get(id) else {
            return Vec::new();
        };
        let (to, to_name) = (message.to.clone(), message.to_name.clone());
        let key = to.key();
        let mut entries = vec![(
            super::lines::build(
                at,
                &crate::cards::held(&to_name),
                Who::Agents(&[&to_name]),
                &[],
            ),
            true,
        )];
        let mut shown = Vec::new();
        for seq in self.envelope_pending.get(&key).cloned().unwrap_or_default() {
            if let Some(l) = self.lines.iter().find(|l| l.seq == seq) {
                entries.push((l.text.clone(), super::lines::uses_held(&l.words)));
                shown.push(seq);
            }
        }
        self.envelope_shown.insert(id.to_owned(), shown);
        let mut defined = self.held_defined.contains(&key);
        entries
            .into_iter()
            .map(|(text, uses)| {
                if uses && !defined {
                    defined = true;
                    format!("{text} {}", super::lines::held_definition())
                } else {
                    text
                }
            })
            .collect()
    }

    /// The message `id` reached `to`: the lines its envelope carried are shown, so they are not
    /// pending any more, and the session has been told what HELD means.
    pub(super) fn envelope_confirmed(&mut self, to: &Identity, id: &str) {
        if let Some(seqs) = self.envelope_shown.remove(id)
            && let Some(pending) = self.envelope_pending.get_mut(&to.key())
        {
            pending.retain(|s| !seqs.contains(s));
            if pending.is_empty() {
                self.envelope_pending.remove(&to.key());
            }
        }
        self.held_defined.insert(to.key());
    }

    /// The senders with a message holding `who` or QUEUED for it (they hear a `lunchroom yolo`
    /// set after their message, §16 item 17), each once, in the order recorded.
    #[must_use]
    pub fn senders_waiting_on(&self, who: &Identity) -> Vec<Identity> {
        let mut out: Vec<Identity> = Vec::new();
        let holding = self.holding(who).iter().filter_map(|h| h.from.clone());
        let mut queued: Vec<&super::Message> = self
            .messages
            .values()
            .filter(|m| m.status == Status::Queued && &m.to == who)
            .collect();
        queued.sort_by_key(|m| (m.rec, m.id.clone()));
        for from in holding.chain(queued.into_iter().map(|m| m.from.clone())) {
            if from != *who && !out.contains(&from) {
                out.push(from);
            }
        }
        out
    }

    /// The line "<agent> is UNGUARDED." is told once for each time `who` becomes UNGUARDED (R3.4:
    /// forgotten when it is guarded again, by [`Mail::newly_unguarded`]). `true` the first time.
    pub fn first_unguarded_line(&mut self, who: &Identity) -> bool {
        self.unguarded_told.insert(who.key())
    }

    /// The lines not yet shown in an envelope to `who`, in the order recorded (`lunchroom status`
    /// lists them).
    #[must_use]
    pub fn pending_envelope_lines(&self, who: &Identity) -> Vec<&LineRecord> {
        self.envelope_pending
            .get(&who.key())
            .into_iter()
            .flatten()
            .filter_map(|seq| self.lines.iter().find(|l| l.seq == *seq))
            .collect()
    }

    /// Takes the lines for the envelope built for `who` now, each as the session reads it: a line
    /// stands in one envelope only, and a session never gets a line recorded for another session.
    ///
    /// A pending number with no record behind it (a damaged store) is not delivered and not
    /// discarded: it stays pending and [`Mail::missing_envelope_lines`] reports it.
    pub fn take_envelope_lines(&mut self, who: &Identity) -> Vec<String> {
        let seqs = self.envelope_pending.remove(&who.key()).unwrap_or_default();
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for seq in seqs {
            match self
                .lines
                .iter()
                .find(|l| l.seq == seq)
                .map(|l| (l.text.clone(), super::lines::uses_held(&l.words)))
            {
                Some((text, held)) => out.push(self.line_with(who, &text, held)),
                None => missing.push(seq),
            }
        }
        if !missing.is_empty() {
            self.envelope_pending.insert(who.key(), missing);
        }
        out
    }

    /// The pending line numbers of `who` that no record backs: a damaged store, reported rather
    /// than silently dropped.
    #[must_use]
    pub fn missing_envelope_lines(&self, who: &Identity) -> Vec<u64> {
        self.envelope_pending
            .get(&who.key())
            .into_iter()
            .flatten()
            .filter(|seq| !self.lines.iter().any(|l| l.seq == **seq))
            .copied()
            .collect()
    }

    /// A call of `session` starts to wait for the user (D1.8). `guarded` says whether an agent's
    /// harness has a guard hook that can tell it. `holds_session` is whether the asking agent is
    /// HELD. The user is told; so is each agent whose message holds the asker, or is QUEUED for it
    /// (an agent once per call, naming the first of its messages that holds, else the first that
    /// is QUEUED). The asker itself is not told.
    #[allow(clippy::too_many_arguments)] // one call's facts
    pub fn call_starts(
        &mut self,
        session: &Identity,
        agent: &str,
        call_id: &str,
        action: &str,
        asked_at: u64,
        kept: bool,
        holds_session: bool,
        guarded: &dyn Fn(&Mail, &Identity) -> bool,
    ) {
        // A call has an id (D1.8): none, no record. The same call again (the harness asks twice)
        // is one call waiting and one start notice.
        if call_id.is_empty()
            || self
                .calls
                .iter()
                .any(|c| c.id == call_id && c.session == *session)
        {
            return;
        }
        // One entry per sender, the message that is named for it.
        let mut named: Vec<(Identity, String, bool)> = Vec::new();
        if holds_session {
            for h in self.holding(session) {
                if let Some(from) = &h.from
                    && from != session
                    && !named.iter().any(|(w, _, _)| w == from)
                {
                    named.push((from.clone(), h.message.clone(), true));
                }
            }
        }
        // The first of a sender's messages is the first recorded (`rec`), not the first by id.
        let mut waiting: Vec<&super::Message> = self
            .messages
            .values()
            .filter(|m| m.status == Status::Queued && &m.to == session)
            .collect();
        waiting.sort_by_key(|m| (m.rec, m.id.clone()));
        let queued: Vec<(Identity, String)> = waiting
            .into_iter()
            .map(|m| (m.from.clone(), m.id.clone()))
            .collect();
        for (from, id) in queued {
            if from != *session && !named.iter().any(|(w, _, _)| *w == from) {
                named.push((from, id, false));
            }
        }
        // The user.
        let mut user_further = Vec::new();
        if kept {
            user_further.push(format!(
                "It lapses at {}.",
                super::lines::clock(asked_at.saturating_add(CALL_LAPSE_SECS))
            ));
            user_further.push(
                "Approve it or deny it on the console, or approve it with `lunchroom approve`."
                    .to_owned(),
            );
        }
        let _ = self.tell_line(
            asked_at,
            &format!(
                "{agent} has a call waiting for you (call {call_id}), asked at {}.",
                super::lines::clock(asked_at)
            ),
            Who::Agents(&[agent]),
            std::slice::from_ref(session),
            &user_further,
            &[],
            true,
        );
        // Each waiting agent.
        let mut waiters = Vec::new();
        for (from, message, holds) in named {
            let d = guarded(self, &from);
            let further = [if holds {
                format!("Your message {message} holds {agent}.")
            } else {
                format!("Your message {message} waits for {agent}.")
            }];
            let _ = self.tell_line(
                asked_at,
                &format!("{agent} has a call waiting for the user (call {call_id})."),
                Who::Agents(&[agent]),
                std::slice::from_ref(session),
                &further,
                &[Hearer {
                    who: from.clone(),
                    d,
                    n: true,
                }],
                false,
            );
            waiters.push(Waiter { who: from, d });
        }
        let holding = self.holding_ids_sorted(session);
        self.calls.push(WaitingCall {
            id: call_id.to_owned(),
            session: session.clone(),
            agent: agent.to_owned(),
            action: action.to_owned(),
            asked_at,
            kept,
            waiters,
            key: String::new(),
            holding,
        });
    }

    fn holding_ids_sorted(&self, session: &Identity) -> Vec<String> {
        let mut ids: Vec<String> = self
            .holding(session)
            .iter()
            .map(|h| h.message.clone())
            .collect();
        ids.sort();
        ids
    }

    /// The guard keeps `request` for `lunchroom approve` (A1.2): the call starts to wait with the
    /// id its approval will have. The same call asked again is the same waiting call, its lapse
    /// counted from the latest ask; no new id and no second start notice.
    pub fn kept_call_starts(
        &mut self,
        request: &crate::approvals::Request,
        guarded: &dyn Fn(&Mail, &Identity) -> bool,
    ) {
        let key = request_key(&request.tool, &request.cwd, &request.input);
        if let Some(call) = self
            .calls
            .iter_mut()
            .find(|c| c.kept && c.session == request.session && c.key == key)
        {
            call.asked_at = request.at;
            return;
        }
        self.next_approval += 1;
        let id = format!("ap-{:08}", self.next_approval);
        let held = self.is_held(&request.session);
        self.call_starts(
            &request.session,
            &request.agent,
            &id,
            &crate::cards::action_of(&request.tool, &request.input),
            request.at,
            true,
            held,
            guarded,
        );
        if let Some(call) = self.calls.last_mut().filter(|c| c.id == id) {
            call.key = key;
        }
    }

    /// The id of the waiting call kept for `request`, if one waits.
    #[must_use]
    pub fn kept_call_id(&self, request: &crate::approvals::Request) -> Option<String> {
        let key = request_key(&request.tool, &request.cwd, &request.input);
        self.calls
            .iter()
            .find(|c| c.kept && c.session == request.session && c.key == key)
            .map(|c| c.id.clone())
    }

    /// The user's approval of the request has failed because what the prompt showed has changed
    /// (A2.2): the call stops waiting, told with the first cause that is true, in this order: a
    /// message held the session, the hold ended, the call changed. Nothing for a request with no
    /// waiting call.
    pub fn kept_call_void(&mut self, request: &crate::approvals::Request, now: u64) {
        let Some(call) = self
            .calls
            .iter()
            .find(|c| {
                c.kept
                    && c.session == request.session
                    && c.key == request_key(&request.tool, &request.cwd, &request.input)
            })
            .cloned()
        else {
            return;
        };
        let current = self.holding_ids_sorted(&call.session);
        let message_held = current.iter().any(|id| !call.holding.contains(id));
        let hold_ended = call.holding.iter().any(|id| !current.contains(id));
        let outcome =
            Outcome::first_void(message_held, hold_ended, true).unwrap_or(Outcome::VoidCallChanged);
        self.call_ends_in(&call.session, &call.id, outcome, now, None);
    }

    /// Tells "Not used in time" for each approval of a kept call still unused 10 minutes after it
    /// was given (A2.3): at exactly `given_at + 600`, stamped with that time; an approval used by
    /// then (or by that instant, one atomic step with the use) is told nothing. Returns how many.
    pub fn settle_unused(&mut self, now: u64) -> usize {
        let due: Vec<UnusedWatch> = self
            .unused
            .iter()
            .filter(|w| now >= w.given_at.saturating_add(CALL_LAPSE_SECS))
            .cloned()
            .collect();
        self.unused
            .retain(|w| now < w.given_at.saturating_add(CALL_LAPSE_SECS));
        let mut told = 0;
        for w in due {
            let used = self
                .approvals
                .iter()
                .find(|a| a.id == w.approval)
                .is_none_or(|a| a.used);
            if used {
                continue;
            }
            let at = w.given_at.saturating_add(CALL_LAPSE_SECS);
            let words = Outcome::NotUsedInTime.words(&w.agent, &w.approval, "");
            let hearers: Vec<Hearer> = w
                .waiters
                .iter()
                .map(|x| Hearer {
                    who: x.who.clone(),
                    d: x.d,
                    n: true,
                })
                .collect();
            let _ = self.tell_line(
                at,
                &words,
                Who::Agents(&[&w.agent]),
                std::slice::from_ref(&w.session),
                &[],
                &hearers,
                true,
            );
            told += 1;
        }
        told
    }

    /// A message `id` has just begun to hold its recipient, which was not HELD: the line "<agent>
    /// is HELD." (card 1, line 2) goes to the user and, by D, to the sender (unless `send` printed
    /// it, [`Mail::sender_heard_held_by_s`]).
    pub(super) fn hold_begins(&mut self, id: &str, at: u64) {
        let Some(m) = self.messages.get(id) else {
            return;
        };
        let (from, to, to_name) = (m.from.clone(), m.to.clone(), m.to_name.clone());
        let words = crate::cards::held(&to_name);
        let record = self.tell_line(
            at,
            &words,
            Who::Agents(&[&to_name]),
            std::slice::from_ref(&to),
            &[],
            &[Hearer {
                who: from,
                d: true,
                n: false,
            }],
            true,
        );
        self.held_lines.insert(id.to_owned(), record.seq);
    }

    /// `send` printed "<agent> is HELD." to the sender of message `id`, so the sender is not told
    /// it again by D (S xor D, §16 item 5). The line, as the sender reads it, or `None` when no
    /// hold began for that message.
    pub fn sender_heard_held_by_s(&mut self, id: &str) -> Option<String> {
        let seq = *self.held_lines.get(id)?;
        let record = self.lines.iter().find(|l| l.seq == seq)?.clone();
        let sender = self.messages.get(id)?.from.clone();
        let definition = super::lines::held_definition();
        if let Some(list) = self.notices.get_mut(&sender.key()) {
            let before = list.len();
            let mut had_definition = false;
            list.retain(|n| {
                let ours =
                    n.text == record.text || n.text == format!("{} {definition}", record.text);
                had_definition |= ours && n.text.ends_with(&definition);
                !ours
            });
            if list.len() != before && had_definition {
                self.held_defined.remove(&sender.key());
            }
            if list.is_empty() {
                self.notices.remove(&sender.key());
            }
        }
        Some(self.line_with(&sender, &record.text, true))
    }

    /// The call `call_id` stops waiting with `outcome` at `at`: the user and the agents told at
    /// the start are told, whether or not their messages still hold the session. One outcome per
    /// call: a call that is not waiting (told already, or never recorded) gives `false` and tells
    /// nothing. The three Approval void lines are for a call the guard keeps; for another call
    /// `false` and nothing told.
    pub fn call_ends(&mut self, call_id: &str, outcome: Outcome, at: u64) -> bool {
        self.end_call(None, call_id, outcome, at, None)
    }

    /// As [`Mail::call_ends`] for the call `call_id` of `session` (a harness's id for a call is
    /// only one per session). With `asker_d`, the agent that made the call hears the outcome
    /// too, by D when it is `Some(true)` and only in its next envelope when `Some(false)`: in
    /// Claude Code, which A1.2 gives no line, "Granted. <agent> can <action>." reaches the agent
    /// that merges by D (§16 item 15).
    pub fn call_ends_in(
        &mut self,
        session: &Identity,
        call_id: &str,
        outcome: Outcome,
        at: u64,
        asker_d: Option<bool>,
    ) -> bool {
        self.end_call(Some(session), call_id, outcome, at, asker_d)
    }

    fn end_call(
        &mut self,
        session: Option<&Identity>,
        call_id: &str,
        outcome: Outcome,
        at: u64,
        asker_d: Option<bool>,
    ) -> bool {
        let matching =
            |c: &&WaitingCall| c.id == call_id && session.is_none_or(|s| *s == c.session);
        // Without the session, an id two sessions share names no one call: nothing is closed.
        if session.is_none() && self.calls.iter().filter(matching).count() > 1 {
            return false;
        }
        let Some(i) = self.calls.iter().position(|c| matching(&c)) else {
            return false;
        };
        if outcome.is_void() && !self.calls[i].kept {
            return false;
        }
        let call = self.calls.remove(i);
        // An approval given for a kept call is watched: unused 10 minutes later, it is told.
        if call.kept && outcome == Outcome::Granted {
            self.unused.push(UnusedWatch {
                approval: call.id.clone(),
                session: call.session.clone(),
                agent: call.agent.clone(),
                given_at: at,
                waiters: call.waiters.clone(),
            });
        }
        let words = outcome.words(&call.agent, &call.id, &call.action);
        let mut hearers: Vec<Hearer> = call
            .waiters
            .iter()
            .map(|w| Hearer {
                who: w.who.clone(),
                d: w.d,
                n: true,
            })
            .collect();
        if let Some(d) = asker_d {
            hearers.push(Hearer {
                who: call.session.clone(),
                d,
                n: !d,
            });
        }
        let _ = self.tell_line(
            at,
            &words,
            Who::Agents(&[&call.agent]),
            std::slice::from_ref(&call.session),
            &[],
            &hearers,
            true,
        );
        true
    }

    /// The check of D1.7 over the calls waiting (any Lunchroom program runs it): a kept call that
    /// has waited 10 minutes since the agent asked is dropped, told at the lapse time as "Not
    /// approved in time" (A2.3); a call whose asking session `stopped` says is STOPPED (R3) is
    /// closed: a kept call as "Not approved in time", a call the harness's own prompt answers as
    /// Denied (the turn or the session ended with the call not run, K1.28). Returns how many
    /// calls it closed.
    pub fn sweep_calls(&mut self, now: u64, stopped: &dyn Fn(&Mail, &Identity) -> bool) -> usize {
        let unused = self.settle_unused(now);
        let due: Vec<(Identity, String, Outcome, u64)> = self
            .calls
            .iter()
            .filter_map(|c| {
                let lapse_at = c.asked_at.saturating_add(CALL_LAPSE_SECS);
                if stopped(self, &c.session) {
                    let outcome = if c.kept {
                        Outcome::NotApprovedInTime
                    } else {
                        Outcome::Denied
                    };
                    Some((c.session.clone(), c.id.clone(), outcome, now))
                } else if c.kept && now >= lapse_at {
                    Some((
                        c.session.clone(),
                        c.id.clone(),
                        Outcome::NotApprovedInTime,
                        lapse_at,
                    ))
                } else {
                    None
                }
            })
            .collect();
        unused
            + due
                .into_iter()
                .filter(|(s, id, outcome, at)| self.call_ends_in(s, id, *outcome, *at, None))
                .count()
    }

    /// The calls waiting for the user, in the order they started.
    #[must_use]
    pub fn waiting_calls(&self) -> &[WaitingCall] {
        &self.calls
    }
}
