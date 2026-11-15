//! The real console: the snapshot of the store, the backend that does what the user confirmed,
//! and the terminal loop that draws the screen and reads keys and clicks.
//!
//! Every action of [`Action`] runs a command of `user.rs` (approve, refuse, release, resend,
//! discard, hold, dismiss, release-stale, clear, unregister, mode, yolo) or, for a controller
//! approval, `controller.rs`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use crossterm::event::{self, Event, MouseEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use lunchroom_core::classify::Guard;
use lunchroom_core::conversation::{self, Conversation, LineIn, MessageIn, Party};
use lunchroom_core::fs::{BUDGET, OsFs};
use lunchroom_core::identity::Identity;
// STUB (C3 branch): becomes `lunchroom_core::classify::{Mark..}` when C2 merges.
use lunchroom_core::classify::{MarkKind, MarkSource, Marked};
use lunchroom_core::mail::{Mail, Notice, clock, standing};
use lunchroom_core::passphrase::Verifier;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::model::{
    Action, AgentRow, AgentState, AlertKind, AlertRow, Backend, CallRow, CheckRow, Fix, GuardRow,
    HoldRow, Listing, MarkedRow, MessageKind, MessageRow, Outcome, Part, PartKind, ReqView,
    Snapshot,
};
use super::ui::Ui;
use super::{
    ALIVE_EVERY, ALIVE_FILE, CHECKS_EVERY, Ending, Feed, NOTICES_KEPT, probing_notice, write_alive,
};
use crate::cli::Outcome as CmdOutcome;
use crate::controller;
use crate::home::Env;
use crate::records::{self, Record};
use crate::terminal::System;

/// The screen is read again this often.
const REDRAW: Duration = Duration::from_millis(250);

/// The snapshot is made again this often, in seconds.
const SNAPSHOT_EVERY: u64 = 1;

// ---------------------------------------------------------------------------------------------
// The snapshot of the store.
// ---------------------------------------------------------------------------------------------

/// The agent a session id belongs to, by the name the user gave it.
fn agent_of(mail: &Mail, session: &str) -> String {
    mail.names()
        .find(|(_, who)| who.session == session)
        .map_or_else(
            || {
                let short: String = session.chars().take(8).collect();
                format!("a session with no agent name ({short})")
            },
            |(name, _)| name.to_owned(),
        )
}

/// The conversations the store holds (§17.10): every message, by session identity, and every line of
/// Lunchroom's. Nothing is left out and nothing is removed.
fn conversations_of(mail: &Mail) -> Vec<Conversation> {
    let party = |who: &Identity, name: &str| Party {
        harness: crate::evidence::harness_name(who.harness).to_owned(),
        session: who.session.clone(),
        name: mail
            .name_of(who)
            .map_or_else(|| name.to_owned(), str::to_owned),
    };
    let messages: Vec<MessageIn> = mail
        .message_ids()
        .iter()
        .filter_map(|id| mail.message(id))
        .map(|m| MessageIn {
            id: m.id.clone(),
            at: m.sent_at,
            // One order over messages and lines, recorded by the store (§17.10: equal times in
            // the order recorded).
            order: m.rec,
            from: party(&m.from, &m.from_name),
            to: party(&m.to, &m.to_name),
            text: m.text.clone(),
        })
        .collect();
    // Every line of Lunchroom's the store holds (§16 item 1), placed by the identities it names.
    let lines: Vec<LineIn> = mail
        .lines()
        .iter()
        .map(|l| LineIn {
            id: l.seq,
            at: l.at,
            order: l.rec,
            text: l.text.clone(),
            agents: l.who.iter().map(|w| party(w, "")).collect(),
        })
        .collect();
    conversation::conversations(&messages, &lines)
}

/// A call's marks as the screen draws them (§17.10): the call's own text and scripts as the classifier
/// read them, each mark's kinds named, and a call that cannot be read marked whole. `Script(n)` is
/// the screen's source `n + 1` (source 0 is the call).
pub(super) fn parts_of(m: &Marked) -> (Vec<Part>, bool) {
    let parts = m
        .marks
        .iter()
        .map(|k| Part {
            source: match k.source {
                MarkSource::Call => 0,
                MarkSource::Script(n) => n + 1,
            },
            start: k.start,
            end: k.end,
            kinds: k
                .kinds
                .iter()
                .map(|kind| match kind {
                    MarkKind::Loosens => PartKind::Loosens,
                    MarkKind::Destroys => PartKind::Destroys,
                    MarkKind::NotRead => PartKind::NotRead,
                    MarkKind::Reply => PartKind::Reply,
                })
                .collect(),
            why: k.words.clone(),
            reason: k.reason.map(|r| r.word().to_owned()),
        })
        .collect();
    (parts, m.whole_call_not_read)
}

/// The first command in a line of prose: the text of the first backquoted span that follows `run`.
fn command_in(line: &str) -> Option<String> {
    let after = line.split_once("run `")?.1;
    let (cmd, _) = after.split_once('`')?;
    (!cmd.is_empty()).then(|| cmd.to_owned())
}

/// F2-AC58: each path the store holds as marked, once, with when it was marked and the call that
/// marked it.
fn marked_rows(mail: &Mail) -> Vec<MarkedRow> {
    let mut rows: Vec<MarkedRow> = Vec::new();
    for m in mail.marked() {
        for path in &m.paths {
            if !rows.iter().any(|r| r.path == *path) {
                rows.push(MarkedRow {
                    path: path.clone(),
                    since: m.since,
                    call: m.call.clone(),
                });
            }
        }
    }
    rows
}

/// F2-AC57: the checks whose fixes are Lunchroom's own, done inside the store: a QUEUED or FAILED
/// message to an agent no longer named (fix: discard it), and an agent holding a hold whose message
/// the store no longer holds (fix: release those holds).
fn store_checks(mail: &Mail) -> Vec<CheckRow> {
    let mut rows: Vec<CheckRow> = mail
        .unnamed_messages()
        .into_iter()
        .map(|m| CheckRow {
            ok: false,
            name: format!("message {} to an agent no longer named", m.id),
            evidence: format!(
                "{} from {} to {} is {}, and no agent has that name now",
                m.id,
                m.from_name,
                m.to_name,
                m.status.state().word()
            ),
            fix: Fix::Discard(m.id.clone()),
        })
        .collect();
    // A path a HELD agent's call marked: the fix is to clear it, as `lunchroom clear <path>` does.
    rows.extend(marked_rows(mail).into_iter().map(|m| CheckRow {
        ok: false,
        name: format!("{} is marked", m.path),
        evidence: format!("call {} marked {} at {}", m.call, m.path, m.since),
        fix: Fix::Clear(m.path),
    }));
    rows.extend(mail.names().filter_map(|(name, who)| {
        let stale = mail.stale_holds(who).len();
        (stale > 0).then(|| CheckRow {
            ok: false,
            name: format!("{name} is held by a message that is gone"),
            evidence: format!("{stale} hold(s) on {name} name a message the store no longer holds"),
            fix: Fix::Release(name.to_owned()),
        })
    }));
    rows
}

/// A check line of the setup (`PASS ...`, `FAIL ...` or a note) as a row.
fn check_row(line: &str) -> CheckRow {
    let (ok, text) = match line.split_once(' ') {
        Some(("PASS", rest)) => (true, rest),
        Some(("FAIL", rest)) => (false, rest),
        _ => (true, line),
    };
    let name = text
        .split([':', ';'])
        .next()
        .unwrap_or(text)
        .split(". ")
        .next()
        .unwrap_or(text)
        .trim()
        .to_owned();
    CheckRow {
        ok,
        name,
        evidence: text.to_owned(),
        fix: if ok {
            Fix::None
        } else {
            command_in(text).map_or(Fix::None, Fix::Command)
        },
    }
}

/// How long a finding that an agent's guard runs shows it AVAILABLE on the console (decision of
/// 2026-11-14, "Hooks + age limit": 2 minutes).
const AVAILABLE_FOR: u64 = 120;

/// The commands that repair a STOPPED agent, for the user to run (§17 item 10): its harness's own
/// command that resumes that session, where it has one, or, for an agent gone for good, the one
/// that takes its name away.
fn stopped_repair(name: &str, who: &Identity) -> String {
    use lunchroom_core::identity::Harness;
    let unregister = format!("lunchroom unregister {name}");
    match who.harness {
        Harness::ClaudeCode => format!(
            "claude --resume {}\n(or, if this agent is gone for good: {unregister})",
            who.session
        ),
        Harness::Codex => format!(
            "codex resume {}\n(or, if this agent is gone for good: {unregister})",
            who.session
        ),
        Harness::OpenClaw | Harness::Hermes => format!(
            "{unregister}\n(where the session is to go on, start it again in {})",
            crate::evidence::harness_name(who.harness)
        ),
    }
}

/// What reads the store and the files around it and makes the screen's [`Snapshot`].
pub struct Live<'a> {
    pub env: Env,
    pub developer: String,
    pub feed: &'a dyn Feed,
    seen: Vec<Notice>,
    alerted: BTreeSet<String>,
    checks: (u64, Vec<String>),
    /// The console runs as the owner of the developer's home folder and reads it, so it can judge
    /// each agent's evidence itself (a console as the user `lunchroom` cannot).
    judges_itself: bool,
}

impl<'a> Live<'a> {
    #[must_use]
    pub fn new(env: Env, developer: String, feed: &'a dyn Feed) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let judges_itself = std::fs::read_dir(&env.home).is_ok()
            && std::fs::metadata(&env.home)
                .is_ok_and(|m| m.uid() == rustix::process::geteuid().as_raw());
        Self {
            env,
            developer,
            feed,
            seen: Vec::new(),
            alerted: BTreeSet::new(),
            checks: (0, Vec::new()),
            judges_itself,
        }
    }

    /// This view as the console that runs as the user `lunchroom` has it: it cannot judge any
    /// agent's evidence itself.
    #[cfg(test)]
    #[must_use]
    pub const fn viewed_by_the_lunchroom_user(mut self) -> Self {
        self.judges_itself = false;
        self
    }

    /// An agent's state, the evidence it lacks and why, and the command that repairs it (§17
    /// item 10's panel). F2 design, "The agents on the console": a console that runs as the
    /// developer and reads their home folder judges the evidence itself; the console as the user
    /// `lunchroom` reads neither the developer's home folder nor their processes' programs, so it
    /// tells STOPPED itself from `/proc/<pid>/stat`, which every user may read, and otherwise shows
    /// what the developer's own program last found of that same process.
    fn agent_state(
        &self,
        mail: &Mail,
        name: &str,
        who: &Identity,
        now: u64,
    ) -> (AgentState, String, Option<String>) {
        use crate::evidence::{Assessment, install_word, not_running};
        use lunchroom_core::mail::Found;
        let state_of = |evidence| match mail.agent_state(who, evidence) {
            lunchroom_core::vocab::AgentState::Available => AgentState::Available,
            lunchroom_core::vocab::AgentState::Held => AgentState::Held,
            lunchroom_core::vocab::AgentState::Unguarded => AgentState::Unguarded,
            lunchroom_core::vocab::AgentState::Stopped => AgentState::Stopped,
        };
        let install = || Some(format!("lunchroom install {}", install_word(who.harness)));
        let stopped = || {
            (
                state_of(lunchroom_core::mail::Evidence::NotRunning),
                "The session is not running, or cannot be reached: a hook of Lunchroom's last \
                 saw it in a process that has ended."
                    .to_owned(),
                Some(stopped_repair(name, who)),
            )
        };
        if self.judges_itself {
            return match crate::evidence::assess(mail, who, &self.env.home) {
                Assessment::NotRunning => stopped(),
                Assessment::Unguarded(why) => (
                    state_of(lunchroom_core::mail::Evidence::RunningUnguarded),
                    why.text(),
                    install(),
                ),
                Assessment::Guarded => (
                    state_of(lunchroom_core::mail::Evidence::Guarded),
                    String::new(),
                    None,
                ),
            };
        }
        if not_running(mail, who) {
            return stopped();
        }
        let process = mail.session(who).map(|r| r.process.clone());
        match mail.judged(who).filter(|j| j.process == process) {
            Some(j) if j.found == Found::Unguarded => (
                state_of(lunchroom_core::mail::Evidence::RunningUnguarded),
                format!(
                    "{} (found by your Lunchroom program at {} UTC)",
                    j.why,
                    clock(j.at)
                ),
                install(),
            ),
            // decision of 2026-11-14 ("Hooks + age limit"): AVAILABLE only from a finding of the last
            // AVAILABLE_FOR seconds; an older one is shown for what it is, with its time.
            Some(j) if j.found == Found::Guarded && now.saturating_sub(j.at) > AVAILABLE_FOR => (
                AgentState::Unknown,
                format!(
                    "Not known now: your Lunchroom program last found this agent's guard running at \
                     {} UTC, over {} minutes ago (each of the agent's own calls has it judged \
                     again; an agent that makes no call is judged by lunchroom status).",
                    clock(j.at),
                    AVAILABLE_FOR / 60
                ),
                Some("lunchroom status".to_owned()),
            ),
            Some(j) if j.found == Found::Guarded => (
                state_of(lunchroom_core::mail::Evidence::Guarded),
                String::new(),
                None,
            ),
            // Not judged for the process that runs now (or judged STOPPED before it started).
            _ if mail.is_held(who) => (AgentState::Held, String::new(), None),
            _ => (
                AgentState::Unknown,
                "Your Lunchroom program has not yet judged this agent's guard in the session that \
                 runs now, and the console, which runs as the user lunchroom, cannot read your \
                 home folder or your processes to judge it itself. lunchroom status judges it \
                 and the console then shows what it found."
                    .to_owned(),
                Some("lunchroom status".to_owned()),
            ),
        }
    }

    /// The agents' rows; `recs` gives each its DENIED decisions of the last hour.
    fn agent_rows(&self, mail: &Mail, env: &Env, recs: &[Record], now: u64) -> Vec<AgentRow> {
        let mut rows: Vec<(bool, AgentRow)> = mail
            .names()
            .map(|(name, who)| {
                let (state, missing, repair) = self.agent_state(mail, name, who, now);
                let held_by = mail
                    .holding(who)
                    .iter()
                    .map(|h| {
                        // A message Lunchroom delivered, else the kept text of an arrival it did
                        // not deliver (§17.10: each message that holds it, whole).
                        let (at, text) = match (
                            mail.message(&h.message),
                            mail.unrecorded(&h.message),
                        ) {
                            (Some(m), _) => (m.sent_at, m.text.clone()),
                            // decision of 2026-11-14 ("Keep, secrets replaced"): the panel says so.
                            (None, Some(u)) => (
                                u.at,
                                format!(
                                    "{}\n(Lunchroom did not deliver this text; it was kept with \
                                         every secret replaced{})",
                                    u.text,
                                    if u.cut {
                                        format!(", and cut at {} bytes", u.text.len())
                                    } else {
                                        String::new()
                                    }
                                ),
                            ),
                            (None, None) => (0, String::new()),
                        };
                        HoldRow {
                            id: h.message.clone(),
                            from: h.from_name.clone(),
                            at,
                            text,
                        }
                    })
                    .collect();
                let mut yolo = String::new();
                if let Some(end) = mail.yolo_until(who, env.now) {
                    let _ = write!(yolo, "YOLO until {}", clock(end));
                }
                if mail.harness_yolo(who) {
                    if !yolo.is_empty() {
                        yolo.push_str(", ");
                    }
                    yolo.push_str("YOLO (the harness's own)");
                }
                let mut spans = [0u16; 12];
                let mut hour = 0usize;
                for r in recs.iter().filter(|r| {
                    r.hook == records::DENIED_HOOK
                        && r.session == who.session
                        && r.at <= now
                        && now - r.at < 3_600
                }) {
                    let ago = usize::try_from((now - r.at) / 300).unwrap_or(11).min(11);
                    spans[11 - ago] = spans[11 - ago].saturating_add(1);
                    hour += 1;
                }
                (
                    state == AgentState::Stopped,
                    AgentRow {
                        name: name.to_owned(),
                        harness: crate::evidence::harness_name(who.harness).to_owned(),
                        state,
                        queued: mail.deliveries(who).len(),
                        held_by,
                        denied_spans: spans,
                        denied_hour: hour,
                        yolo: (!yolo.is_empty()).then_some(yolo),
                        missing,
                        repair,
                    },
                )
            })
            .collect();
        rows.sort_by_key(|(stopped, _)| *stopped);
        rows.into_iter().map(|(_, row)| row).collect()
    }

    #[cfg(test)]
    /// The snapshot of the store at `now`, with its new notices marked told. `show`: the screen
    /// is showing the details, so the notices it holds are drawn now; a locked screen shows none
    /// and tells none.
    pub fn snapshot(&mut self, now: u64, base: &dyn System, show: bool) -> Snapshot {
        let (snap, new) = self.snapshot_unmarked(now, base, show);
        self.mark_told(&new);
        snap
    }

    /// Marks the notices told: what `lunchroom status` and the next prompt then leave out.
    pub fn mark_told(&self, notices: &[Notice]) {
        if notices.is_empty() {
            return;
        }
        let mut env = self.env.clone();
        env.now = 0;
        if let Ok(store) = Store::<Mail>::open(&env.store_dir()) {
            let _ = store.update(|m| m.mark_user_notices_told(notices));
        }
    }

    /// [`Live::snapshot`] that leaves the notices it newly holds for the caller to mark told once
    /// they have been drawn.
    pub fn snapshot_unmarked(
        &mut self,
        now: u64,
        base: &dyn System,
        show: bool,
    ) -> (Snapshot, Vec<Notice>) {
        let mut env = self.env.clone();
        env.now = now;
        let store_dir = env.store_dir();
        let mut mail = Store::<Mail>::read_without_change(&store_dir).unwrap_or_default();
        let recs = self.feed.records(&env);
        if self.checks.1.is_empty() || now >= self.checks.0 + CHECKS_EVERY {
            self.checks = (now, self.feed.checks(&env, &mail, base));
        }
        // Probing: a notice for the user, once while it lasts, written to the store and read back.
        let counts = records::denied_counts(&recs, now);
        let mut fresh: Vec<String> = Vec::new();
        for c in &counts {
            if c.probing() {
                if self.alerted.insert(c.session.clone()) {
                    fresh.push(probing_notice(&agent_of(&mail, &c.session), c.recent));
                }
            } else {
                self.alerted.remove(&c.session);
            }
        }
        if !fresh.is_empty()
            && let Ok(store) = Store::<Mail>::open(&store_dir)
        {
            let _ = store.update(|m| {
                for text in &fresh {
                    m.tell_user(text, now);
                }
            });
            mail = Store::<Mail>::read_without_change(&store_dir).unwrap_or_default();
        }
        let new: Vec<Notice> = if show {
            mail.user_notices()
                .iter()
                .filter(|n| !self.seen.contains(n))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        self.seen.extend(new.iter().cloned());
        if self.seen.len() > NOTICES_KEPT {
            self.seen.drain(..self.seen.len() - NOTICES_KEPT);
        }
        let snap = self.build(&mail, &env, &recs, &counts, now);
        (snap, new)
    }

    #[allow(clippy::too_many_lines)] // one snapshot: each list of the screen from the store
    fn build(
        &self,
        mail: &Mail,
        env: &Env,
        recs: &[Record],
        counts: &[records::DeniedCount],
        now: u64,
    ) -> Snapshot {
        // The guard reads each waiting call again as the user is shown it (§17.10): the call's text
        // as shown, and the byte ranges that made it go to the user.
        let layout = env.layout();
        let fs = OsFs::new(BUDGET);
        let guard = Guard {
            layout: &layout,
            fs: &fs,
            held: true,
        };
        let marked: Vec<Marked> = mail
            .requests()
            .iter()
            .map(|r| {
                let (tool, input) =
                    crate::claude::as_claude_call(r.session.harness, &r.tool, &r.input);
                guard.marked(&tool, &input, std::path::Path::new(&r.cwd))
            })
            .collect();
        let waiting = mail
            .requests()
            .iter()
            .enumerate()
            .map(|(k, r)| CallRow {
                number: k + 1,
                // The store's own name for the call, and what the user is shown of it: an
                // approval binds to both, never to a place in the list.
                id: format!(
                    "{}#{}",
                    lunchroom_core::approvals::call_key(r),
                    lunchroom_core::approvals::call_shown_hash(r)
                ),
                agent: r.agent.clone(),
                harness: crate::evidence::harness_name(r.session.harness).to_owned(),
                tool: r.tool.clone(),
                // §17.10: a shell command is shown as its command line, any other tool call as
                // the text A2.1 hashes; the approval binds to that text, shown whole.
                command: marked[k].text.clone(),
                cwd: r.cwd.clone(),
                rule: r.rule.clone(),
                parts: parts_of(&marked[k]).0,
                scripts: marked[k].scripts.clone(),
                unread: parts_of(&marked[k]).1,
                age_secs: now.saturating_sub(r.at),
            })
            .collect();
        let mut messages: Vec<MessageRow> = mail
            .queued()
            .iter()
            .map(|m| MessageRow {
                id: m.id.clone(),
                kind: MessageKind::Queued,
                from: m.from_name.clone(),
                to: m.to_name.clone(),
                text: m.text.clone(),
                reason: standing(m, now),
                at: m.sent_at,
            })
            .collect();
        messages.extend(mail.failed().into_iter().map(|(m, reason)| MessageRow {
            id: m.id.clone(),
            kind: MessageKind::Failed,
            from: m.from_name.clone(),
            to: m.to_name.clone(),
            text: m.text.clone(),
            reason: reason.to_owned(),
            at: m.sent_at,
        }));
        let mut alerts: Vec<AlertRow> = counts
            .iter()
            .filter(|c| c.probing())
            // F2-AC55: a dismissed alert stays dark until a DENIED decision newer than the dismiss.
            .filter(|c| {
                mail.probe_dismissed_at(&c.session).is_none_or(|dismissed| {
                    recs.iter().any(|r| {
                        r.hook == records::DENIED_HOOK && r.session == c.session && r.at > dismissed
                    })
                })
            })
            .map(|c| {
                let agent = agent_of(mail, &c.session);
                AlertRow {
                    id: format!("probing:{}", c.session),
                    kind: AlertKind::Probing,
                    at: now,
                    line: format!("PROBING {agent}: {} DENIED in 10 min", c.recent),
                    text: probing_notice(&agent, c.recent),
                    // Newest first: the records are oldest first, and of one second the later
                    // recorded is newer.
                    calls: recs
                        .iter()
                        .filter(|r| {
                            r.hook == records::DENIED_HOOK
                                && r.session == c.session
                                && r.at <= now
                                && now - r.at <= records::PROBE_SECS
                        })
                        .rev()
                        .map(|r| (r.at, r.call.clone(), r.step.clone()))
                        .collect(),
                    agent,
                }
            })
            .collect();
        alerts.extend(self.seen.iter().map(|n| AlertRow {
            id: format!("notice:{}:{}:{}", n.message, n.at, n.text.len()),
            kind: AlertKind::Notice,
            at: n.at,
            agent: String::new(),
            line: n.text.clone(),
            text: n.text.clone(),
            calls: Vec::new(),
        }));
        let hour: Vec<&Record> = recs
            .iter()
            .filter(|r| r.hook != records::DENIED_HOOK && r.at <= now && now - r.at < 3_600)
            .collect();
        let guard = GuardRow {
            decisions: recs
                .iter()
                .filter(|r| r.hook != records::DENIED_HOOK)
                .count(),
            slowest_ms: recs.iter().map(|r| r.took_ms).max().unwrap_or(0),
            slow: hour
                .iter()
                .filter(|r| r.undecided.is_none() && r.took_ms > 2_000)
                .count(),
            timeouts: hour
                .iter()
                .filter(|r| {
                    r.undecided
                        .as_deref()
                        .is_some_and(|u| u.starts_with("no answer within "))
                })
                .count(),
        };
        Snapshot {
            now,
            developer: self.developer.clone(),
            hold_mode: mail.hold_mode().word().to_owned(),
            waiting,
            conversations: conversations_of(mail),
            agents: self.agent_rows(mail, env, recs, now),
            messages,
            alerts,
            marked: marked_rows(mail),
            checks: self
                .checks
                .1
                .iter()
                .map(|l| check_row(l))
                .chain(store_checks(mail))
                .collect(),
            guard,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What the user's commands run on, and the backend.
// ---------------------------------------------------------------------------------------------

/// The system the commands of `user.rs` run on inside the console. The console has already asked
/// what each needs (y/N, the passphrase) on its own screen, so this hands the commands a verifier
/// made from a random token that lives only here, and returns the token as the typed passphrase;
/// the developer's passphrase is checked by the console, never kept. The commands still check that
/// no guarded session is above the console and that it has a terminal.
struct Acting<'a> {
    base: &'a mut dyn System,
    session: Verifier,
    token: String,
}

impl System for Acting<'_> {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        self.base.ancestors()
    }
    fn own_tty(&self) -> u64 {
        self.base.own_tty()
    }
    fn tty_of(&self, key: &str) -> Option<u64> {
        self.base.tty_of(key)
    }
    fn ask(&mut self, _question: &str) -> Option<String> {
        None
    }
    fn uid(&self) -> Option<u32> {
        self.base.uid()
    }
    fn verifier(&self) -> Result<String, String> {
        Ok(self.session.line())
    }
    fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
        Some(self.token.clone())
    }
    fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
        self.base.salt()
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Err("the console does not change the passphrase".to_owned())
    }
}

/// A token of random bytes as text, for the verifier of one action.
fn random_token(
    base: &mut dyn System,
) -> Option<(String, [u8; lunchroom_core::passphrase::SALT_BYTES])> {
    let salt = base.salt().ok()?;
    let bytes = base.salt().ok()?;
    let token = bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    Some((token, salt))
}

/// The backend of the real console.
pub struct RealBackend<'a> {
    pub env: Env,
    pub base: &'a mut dyn System,
    pub uid: u32,
    pub developer: String,
    /// The user id of the system user `lunchroom`, which owns each approval file; none when
    /// `/etc/passwd` does not name it, and then no approval is written.
    pub lunchroom_uid: Option<u32>,
    /// Where the controller's approvals go; `None` in a test that gives its own paths.
    pub controller: Option<ControllerPaths>,
}

/// Where item 9's files are.
#[derive(Debug, Clone)]
pub struct ControllerPaths {
    pub requests: PathBuf,
    pub approvals: PathBuf,
    pub key: PathBuf,
}

impl ControllerPaths {
    /// The paths for developer `uid` (§17.9).
    #[must_use]
    pub fn for_uid(uid: u32) -> Self {
        let approvals = setgid::approvals_home(uid);
        Self {
            requests: approvals.join("requests"),
            approvals,
            key: setgid::console_key(uid),
        }
    }
}

impl RealBackend<'_> {
    fn paths(&self) -> ControllerPaths {
        self.controller
            .clone()
            .unwrap_or_else(|| ControllerPaths::for_uid(self.uid))
    }

    fn command(
        &mut self,
        now: u64,
        run: impl FnOnce(&Env, &mut dyn System) -> CmdOutcome,
    ) -> Outcome {
        let Some((token, salt)) = random_token(self.base) else {
            return Outcome::refused("The console could not get random bytes, so it did nothing.");
        };
        let mut env = self.env.clone();
        env.now = now;
        let mut acting = Acting {
            session: Verifier::new(&token, salt, crate::terminal::ITERATIONS),
            token,
            base: &mut *self.base,
        };
        let out = run(&env, &mut acting);
        let text = format!("{}{}", out.stdout, out.stderr).trim().to_owned();
        Outcome {
            done: out.code == 0,
            text,
        }
    }
}

impl Backend for RealBackend<'_> {
    fn check_passphrase(&mut self, typed: &str) -> bool {
        crate::terminal::trusted_verifier(self.base).is_ok_and(|v| v.matches(typed))
    }

    fn perform(&mut self, action: &Action, now: u64) -> Outcome {
        // A1.3 for every action, the controller's approval included: no guarded session above
        // the console, a terminal of its own, not the terminal of a guarded session.
        let dir = self.env.store_dir();
        let mail = match Store::<Mail>::read_without_change(&dir) {
            Ok(mail) => mail,
            // A store that is not there yet has no guarded session recorded in it; one that is
            // there and cannot be read leaves the check without what it needs: nothing is done.
            Err(_) if !dir.exists() => Mail::default(),
            Err(e) => {
                return Outcome::refused(format!(
                    "Nothing was done: the store cannot be read to check the terminal ({e})."
                ));
            }
        };
        if let Err(why) = crate::terminal::check(&mail, &*self.base) {
            return Outcome::refused(format!("Nothing was done: {why}."));
        }
        match action {
            Action::Approve { id, .. } => {
                let Some((key, shown)) = id.rsplit_once('#') else {
                    return Outcome::refused("The call has no name, so nothing was approved.");
                };
                self.command(now, |env, sys| {
                    crate::user::approve_call(key, shown, env, sys)
                })
            }
            Action::Unregister { agent } => self.command(now, |env, sys| {
                crate::user::unregister(std::slice::from_ref(agent), env, sys)
            }),
            Action::Mode(mode) => {
                let word = mode.word().to_ascii_lowercase();
                self.command(now, |env, sys| {
                    crate::user::mode(std::slice::from_ref(&word), env, sys)
                })
            }
            Action::Yolo { names, till } => {
                let args = [names.clone(), "till".to_owned(), till.clone()];
                self.command(now, |env, sys| crate::user::yolo(&args, env, sys))
            }
            Action::ControllerApprove {
                run_id,
                phase,
                manifest_hash,
            } => {
                let paths = self.paths();
                let Some(owner) = self.lunchroom_uid else {
                    return Outcome::refused(
                        "Nothing was written: the user lunchroom, which owns an approval, is not \
                         in /etc/passwd.",
                    );
                };
                let shown = ReqView {
                    run_id: run_id.clone(),
                    phase: phase.clone(),
                    manifest_hash: manifest_hash.clone(),
                };
                let mut random = controller::system_random;
                let mut ctl = controller::Ctl {
                    requests: paths.requests,
                    approvals: paths.approvals,
                    key: paths.key,
                    operator: self.developer.clone(),
                    random: &mut random,
                    owner,
                    chown: controller::system_chown,
                };
                match controller::approve(&mut ctl, &shown, now) {
                    Ok(path) => Outcome::done(format!(
                        "Approved {phase} of {run_id}: {} written.",
                        path.display()
                    )),
                    Err(why) => Outcome::refused(format!("Nothing was written: {why}")),
                }
            }
            // F2-AC52: a waiting call is declined as the approval finds it.
            Action::Refuse { id, .. } => {
                let Some((key, shown)) = id.rsplit_once('#') else {
                    return Outcome::refused("The call has no name, so nothing was declined.");
                };
                self.command(now, |env, _| crate::user::refuse_call(key, shown, env))
            }
            // F2-AC53: a HELD agent's hold ends as when the user types in its session.
            Action::Release { agent } => {
                self.command(now, |env, _| crate::user::release_hold(agent, env))
            }
            // F2-AC54: a FAILED message is sent again as a new message under D1.
            Action::Resend { message } => {
                self.command(now, |env, _| crate::user::resend_failed(message, env))
            }
            // F2-AC58: a marked path is cleared as `lunchroom clear <path>` clears it.
            Action::Clear { path } => self.command(now, |env, sys| {
                crate::user::clear(std::slice::from_ref(path), env, sys)
            }),
            // F2-AC54, F2-AC57: a QUEUED or FAILED message is discarded and logged.
            Action::Discard { message } => {
                self.command(now, |env, _| crate::user::discard_message(message, env))
            }
            // F2-AC55: the agent is HELD by the user, released only by the user.
            Action::Hold { agent } => {
                self.command(now, |env, _| crate::user::hold_agent(agent, env))
            }
            // F2-AC55: the PROBING alert stays dark until the agent's next DENIED decision.
            Action::Dismiss { alert } => {
                self.command(now, |env, _| crate::user::dismiss_probe(alert, env))
            }
            // F2-AC57: the holds whose message the store no longer holds end.
            Action::ReleaseStale { agent } => {
                self.command(now, |env, _| crate::user::release_stale(agent, env))
            }
        }
    }

    fn controller_requests(&mut self) -> Listing {
        controller::list_requests(&self.paths().requests)
    }

    fn controller_load(&mut self, run_id: &str, phase: &str) -> Result<ReqView, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        controller::read_fresh(&self.paths().requests, run_id, phase, now)
    }
}

// ---------------------------------------------------------------------------------------------
// The terminal.
// ---------------------------------------------------------------------------------------------

/// The bytes that undo what the console does to the terminal (mouse capture, the alternate screen,
/// the hidden cursor), for a signal to write before the modes are restored.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";

/// Puts the terminal back as it was, whatever ends the console.
struct Restore {
    tty: std::fs::File,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.tty,
            crossterm::event::DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// What [`serve`] needs besides the system.
pub struct Run<'a> {
    pub live: Live<'a>,
    /// Where the liveness file is written; none in a test that does not want one.
    pub alive: Option<PathBuf>,
    /// The clock, in seconds since the Unix epoch.
    pub clock: &'a mut dyn FnMut() -> u64,
}

/// What the screen asks the worker that reads the store.
enum Job {
    /// A snapshot at the wall time, for a screen that shows (or hides) its details.
    Snapshot { wall: u64, show: bool },
    /// These notices have been drawn: mark them told.
    Told(Vec<Notice>),
}

/// The loop: asks the passphrase, then draws the screen, reads keys and clicks and acts until the
/// user quits. Echo is turned off by [`crate::terminal::EchoOff`] for as long as the screen runs.
///
/// The store, the records and `lunchroom doctor` are read by a worker thread of their own, so a
/// store that is busy never stops the screen (the alert strip included) from answering keys. The
/// lock and the ride of a passphrase run on a monotonic clock; the wall clock stamps only what is
/// written and shown as the time.
///
/// # Errors
/// Why the terminal could not be used.
pub fn serve(
    tty: &std::fs::File,
    base: &(dyn System + Sync),
    backend: &mut dyn Backend,
    run: &mut Run<'_>,
) -> Result<Ending, String> {
    let _echo = crate::terminal::EchoOff::begin_with(tty, LEAVE)
        .ok_or("the typed characters cannot be hidden on this terminal; nothing was read")?;
    crossterm::terminal::enable_raw_mode().map_err(|e| format!("raw mode failed: {e}"))?;
    let _restore = Restore {
        tty: tty
            .try_clone()
            .map_err(|e| format!("the terminal cannot be used: {e}"))?,
    };
    // A panic puts the terminal back before its message is printed, so the message is read.
    let leave_tty = tty
        .try_clone()
        .map_err(|e| format!("the terminal cannot be used: {e}"))?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = (&leave_tty).write_all(LEAVE);
        let _ = crossterm::terminal::disable_raw_mode();
        previous(info);
    }));
    // The console cannot be suspended: stopped, it would sit in raw mode on the alternate screen.
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTSTP, stopped);
    let mut out = tty
        .try_clone()
        .map_err(|e| format!("the terminal cannot be used: {e}"))?;
    crossterm::execute!(
        out,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .map_err(|e| format!("the screen cannot be set up: {e}"))?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))
        .map_err(|e| format!("the screen cannot be set up: {e}"))?;
    let (jobs, work) = channel::<Job>();
    let slot: Mutex<Option<(Snapshot, Vec<Notice>)>> = Mutex::new(None);
    let live = &mut run.live;
    let slot_ref = &slot;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            while let Ok(job) = work.recv() {
                match job {
                    Job::Snapshot { wall, show } => {
                        let made = live.snapshot_unmarked(wall, base, show);
                        if let Ok(mut held) = slot_ref.lock() {
                            *held = Some(made);
                        }
                    }
                    Job::Told(notices) => live.mark_told(&notices),
                }
            }
        });
        let ended = drive(
            &mut terminal,
            &jobs,
            slot_ref,
            backend,
            run.alive.as_deref(),
            run.clock,
        );
        drop(jobs);
        ended
    })
}

/// The loop proper: draws, reads, acts, and asks the worker for the next snapshot.
fn drive(
    terminal: &mut Terminal<CrosstermBackend<std::fs::File>>,
    jobs: &Sender<Job>,
    slot: &Mutex<Option<(Snapshot, Vec<Notice>)>>,
    backend: &mut dyn Backend,
    alive: Option<&Path>,
    clock: &mut dyn FnMut() -> u64,
) -> Result<Ending, String> {
    let pid = std::process::id();
    let started = std::time::Instant::now();
    let mono = || started.elapsed().as_secs();
    let mut ui = Ui::new(mono());
    let mut snap = Snapshot::default();
    let mut undrawn: Vec<Notice> = Vec::new();
    let mut asked = false;
    let mut last_ask: Option<u64> = None;
    let mut last_alive: Option<u64> = None;
    loop {
        let (now, wall) = (mono(), clock());
        ui.set_wall(wall);
        if let Some(home) = alive
            && last_alive.is_none_or(|at| now >= at + ALIVE_EVERY)
        {
            let _ = write_alive(home, pid);
            last_alive = Some(now);
        }
        ui.tick(now);
        if let Ok(mut held) = slot.lock()
            && let Some((fresh, new)) = held.take()
        {
            snap = fresh;
            undrawn.extend(new);
            asked = false;
        }
        if !asked && last_ask.is_none_or(|at| now >= at + SNAPSHOT_EVERY) {
            let _ = jobs.send(Job::Snapshot {
                wall,
                show: !ui.locked(),
            });
            asked = true;
            last_ask = Some(now);
        }
        terminal
            .draw(|frame| super::draw::draw(frame, &mut ui, &snap, now))
            .map_err(|e| format!("the screen cannot be drawn: {e}"))?;
        if !ui.locked() && !undrawn.is_empty() {
            // Drawn, so told: the next prompt or `lunchroom status` does not tell them again.
            let _ = jobs.send(Job::Told(std::mem::take(&mut undrawn)));
        }
        if let Some(ending) = ui.ending() {
            return Ok(ending);
        }
        if event::poll(REDRAW).map_err(|e| format!("the keyboard cannot be read: {e}"))? {
            let (at, wall) = (mono(), clock());
            ui.set_wall(wall);
            match event::read().map_err(|e| format!("the keyboard cannot be read: {e}"))? {
                Event::Key(key) => {
                    ui.key(key, at, &snap, backend);
                    last_ask = None;
                }
                // Moving the mouse is not an action: only a click or the wheel is.
                Event::Mouse(mouse)
                    if matches!(
                        mouse.kind,
                        MouseEventKind::Down(_)
                            | MouseEventKind::ScrollUp
                            | MouseEventKind::ScrollDown
                    ) =>
                {
                    ui.mouse(mouse, at, &snap, backend);
                    last_ask = None;
                }
                _ => {}
            }
        }
    }
}

/// The file in the store folder that says a console runs, removed when the console ends.
pub fn remove_alive(home: &Path) {
    let _ = std::fs::remove_file(home.join(ALIVE_FILE));
}
