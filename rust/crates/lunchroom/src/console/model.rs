//! What one screen shows (the `Snapshot`), what the user can ask (`Cmd`, `Action`) and what the
//! store side does about it (`Backend`). Rendering and key handling work on these alone, so both
//! are pure and run in a test against ratatui's `TestBackend`.

use lunchroom_core::conversation::Conversation;
use lunchroom_core::policy::HoldMode;

/// A call waiting for the user's approval.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallRow {
    /// The number `lunchroom approve` gives it (its place in the list when the snapshot was made).
    pub number: usize,
    /// What names this exact call, so an action never lands on another call that took its place.
    pub id: String,
    pub agent: String,
    pub harness: String,
    pub tool: String,
    /// The whole call as the agent made it.
    pub command: String,
    pub cwd: String,
    /// The rule the call is outside of (A1.4).
    pub rule: String,
    /// The marked parts (A1.2, §17.10): byte ranges of `command` and of each script, and why.
    pub parts: Vec<Part>,
    /// The text of each script the call runs, each with its own ranges.
    pub scripts: Vec<String>,
    /// The call's text cannot be read: the whole call is marked "not read".
    pub unread: bool,
    pub age_secs: u64,
}

/// The kind a marked part has (§17.10 "Marked parts"): a part loosens, destroys, is not read, or is
/// the reply of the Strict hold mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Loosens,
    Destroys,
    NotRead,
    Reply,
}

impl PartKind {
    /// The word beside the range on the screen.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Loosens => "loosens",
            Self::Destroys => "destroys",
            Self::NotRead => "not read",
            Self::Reply => "reply",
        }
    }
}

/// A marked part of a call (§17.10): a byte range of the call's text as shown, or of one of the
/// scripts the call runs, with its kinds and the words of the rule that matched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Part {
    /// 0 for the call's own text; `n` for the `n`th script of [`CallRow::scripts`].
    pub source: usize,
    /// Byte range of that text, `start` inclusive, `end` exclusive.
    pub start: usize,
    pub end: usize,
    /// What it is; empty is shown as a part that loosens or destroys, unnamed.
    pub kinds: Vec<PartKind>,
    /// The words of the rule that matched.
    pub why: String,
    /// The reason word, where the range made the call DENIED.
    pub reason: Option<String>,
}

/// A message that holds an agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HoldRow {
    pub id: String,
    pub from: String,
    pub at: u64,
    /// The whole text; empty when the store no longer holds the message.
    pub text: String,
}

/// What an agent's row says of its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentState {
    #[default]
    Available,
    Held,
    Unguarded,
    Stopped,
    /// The console could not read what the state needs.
    Unknown,
}

impl AgentState {
    /// The word on the screen.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Available => "AVAILABLE",
            Self::Held => "HELD",
            Self::Unguarded => "UNGUARDED",
            Self::Stopped => "STOPPED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// One agent's row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRow {
    pub name: String,
    pub harness: String,
    pub state: AgentState,
    pub queued: usize,
    pub held_by: Vec<HoldRow>,
    /// DENIED decisions in each of the last twelve five-minute spans, oldest first.
    pub denied_spans: [u16; 12],
    /// DENIED decisions in the last hour.
    pub denied_hour: usize,
    pub yolo: Option<String>,
    /// The evidence of R3 that is missing, and why (UNGUARDED or STOPPED).
    pub missing: String,
    /// The command that repairs it, for the user to run in a terminal of their own.
    pub repair: Option<String>,
}

/// QUEUED or FAILED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageKind {
    #[default]
    Queued,
    Failed,
}

/// A QUEUED or FAILED message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageRow {
    pub id: String,
    pub kind: MessageKind,
    pub from: String,
    pub to: String,
    pub text: String,
    pub reason: String,
    pub at: u64,
}

/// What an alert is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlertKind {
    /// An agent had many DENIED calls in ten minutes.
    #[default]
    Probing,
    /// A notice for the user.
    Notice,
}

/// An alert.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlertRow {
    /// What names it, for dismissing it.
    pub id: String,
    pub kind: AlertKind,
    pub at: u64,
    pub agent: String,
    /// The line shown on the screen.
    pub line: String,
    /// The whole text.
    pub text: String,
    /// The agent's DENIED calls of the last ten minutes: when, the call, its rule.
    pub calls: Vec<(u64, String, String)>,
}

/// A path marked by the guard (T1.8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarkedRow {
    pub path: String,
    pub since: u64,
    pub call: String,
}

/// The fix a failed check names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))] // the store side makes the fixes it can do (second part)
pub enum Fix {
    /// Nothing is named.
    #[default]
    None,
    /// A command for the user's own terminal; the console shows it and does not run it.
    Command(String),
    /// Discard a QUEUED or FAILED message to an agent no longer named.
    Discard(String),
    /// Release a hold whose message the store no longer holds, on the named agent.
    Release(String),
    /// Clear a marked path.
    Clear(String),
}

/// A check of `lunchroom doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckRow {
    pub ok: bool,
    /// What the check is, short.
    pub name: String,
    /// What it read.
    pub evidence: String,
    pub fix: Fix,
}

/// The guard's timeouts and slow decisions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardRow {
    pub decisions: usize,
    pub slowest_ms: u64,
    /// Decisions over two seconds in the last hour.
    pub slow: usize,
    /// Timeouts in the last hour.
    pub timeouts: usize,
}

/// Everything one screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub now: u64,
    pub developer: String,
    pub hold_mode: String,
    pub waiting: Vec<CallRow>,
    pub agents: Vec<AgentRow>,
    /// One for each pair of agents that has exchanged a message or a line of Lunchroom's (§17.10).
    pub conversations: Vec<Conversation>,
    pub messages: Vec<MessageRow>,
    pub alerts: Vec<AlertRow>,
    pub marked: Vec<MarkedRow>,
    pub checks: Vec<CheckRow>,
    pub guard: GuardRow,
}

/// One thing the user can choose on the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    Call(&'a CallRow),
    Agent(&'a AgentRow),
    Conversation(&'a Conversation),
    Message(&'a MessageRow),
    Alert(&'a AlertRow),
    Check(&'a CheckRow),
    Marked(&'a MarkedRow),
}

/// What names an item across snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemKey(pub &'static str, pub String);

impl Item<'_> {
    #[must_use]
    pub fn key(&self) -> ItemKey {
        match self {
            Item::Call(c) => ItemKey("call", c.id.clone()),
            Item::Agent(a) => ItemKey("agent", a.name.clone()),
            Item::Conversation(c) => ItemKey("pair", c.key.clone()),
            Item::Message(m) => ItemKey("message", m.id.clone()),
            Item::Alert(a) => ItemKey("alert", a.id.clone()),
            Item::Check(c) => ItemKey("check", format!("{}|{}", c.name, c.evidence)),
            Item::Marked(m) => ItemKey("path", m.path.clone()),
        }
    }
}

impl Snapshot {
    /// The items, in the order the arrow keys walk them: the calls, the agents, then what needs
    /// attention (alerts, messages, failed checks, marked paths).
    #[must_use]
    pub fn items(&self) -> Vec<Item<'_>> {
        let mut out: Vec<Item<'_>> = Vec::new();
        out.extend(self.waiting.iter().map(Item::Call));
        out.extend(self.agents.iter().map(Item::Agent));
        out.extend(self.conversations.iter().map(Item::Conversation));
        out.extend(self.attention());
        out
    }

    /// The items of the alerts panel, one line each.
    #[must_use]
    pub fn attention(&self) -> Vec<Item<'_>> {
        // F2 design, "Screen": what the user acts on comes first, so it stays on the screen when
        // notices fill the box: PROBING alerts, the QUEUED and FAILED messages, failing checks,
        // marked paths; then the notices, which only tell.
        let mut out: Vec<Item<'_>> = Vec::new();
        out.extend(
            self.alerts
                .iter()
                .filter(|a| a.kind == AlertKind::Probing)
                .map(Item::Alert),
        );
        out.extend(self.messages.iter().map(Item::Message));
        out.extend(self.checks.iter().filter(|c| !c.ok).map(Item::Check));
        out.extend(self.marked.iter().map(Item::Marked));
        out.extend(
            self.alerts
                .iter()
                .filter(|a| a.kind != AlertKind::Probing)
                .map(Item::Alert),
        );
        out
    }

    /// The item `key` names.
    #[must_use]
    pub fn find(&self, key: &ItemKey) -> Option<Item<'_>> {
        self.items().into_iter().find(|i| &i.key() == key)
    }

    /// An agent by name.
    #[cfg(test)]
    #[must_use]
    pub fn agent(&self, name: &str) -> Option<&AgentRow> {
        self.agents.iter().find(|a| a.name == name)
    }
}

/// How lit a cell of the alert strip is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Dark,
    Amber,
    Red,
    Green,
}

/// One cell of the alert strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub level: Level,
}

/// The six cells, in their fixed order (§17 C2 item 10).
#[must_use]
pub fn strip(s: &Snapshot) -> [Cell; 6] {
    let count = |label: &str, n: usize, level: Level| Cell {
        text: format!("{label} {n}"),
        level: if n > 0 { level } else { Level::Dark },
    };
    let probing = s
        .alerts
        .iter()
        .filter(|a| a.kind == AlertKind::Probing)
        .count();
    let failed = s
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::Failed)
        .count();
    let unguarded = s
        .agents
        .iter()
        .filter(|a| {
            a.state == AgentState::Unguarded || (a.state == AgentState::Stopped && a.queued > 0)
        })
        .count();
    let slow = s.guard.slow + s.guard.timeouts;
    let failing = s.checks.iter().filter(|c| !c.ok).count();
    [
        count("CALLS WAIT", s.waiting.len(), Level::Amber),
        count("PROBING", probing, Level::Red),
        count("FAILED MSG", failed, Level::Amber),
        count("UNGUARDED", unguarded, Level::Amber),
        Cell {
            text: format!("GUARD SLOW {slow}"),
            level: if s.guard.timeouts > 0 {
                Level::Red
            } else if s.guard.slow > 0 {
                Level::Amber
            } else {
                Level::Dark
            },
        },
        if failing > 0 {
            Cell {
                text: format!("CHECKS {failing} FAIL"),
                level: Level::Red,
            }
        } else {
            Cell {
                text: "CHECKS OK".to_owned(),
                level: Level::Green,
            }
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// What the user asks, and what the store side does.
// ---------------------------------------------------------------------------------------------

/// What a key or a click on the key line asks (§17 C2 item 10: every action has a key, and every
/// key a click).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Up,
    Down,
    Open,
    Close,
    Approve,
    Refuse,
    Release,
    Resend,
    Discard,
    Hold,
    Unregister,
    Mode,
    Yolo,
    Controller,
    Lock,
    Quit,
}

/// An operation on the store or the system, run only once the user has confirmed it (and typed
/// the passphrase where item 3 asks for it). The second part of U11 implements these in
/// [`Backend::perform`]; every one names what it acts on by something that does not change
/// between snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Approve the waiting call `id` (shown as `number`) once.
    Approve { number: usize, id: String },
    /// Refuse it: the call is DENIED, a retry of that exact call is DENIED with the reason that
    /// the user refused it, and the agent is told through A1.2's channel.
    Refuse { number: usize, id: String },
    /// End the hold on `agent` as when the user types in that session, and log it.
    Release { agent: String },
    /// Resend the FAILED message `message` as a new message, holding its recipient.
    Resend { message: String },
    /// Discard the QUEUED or FAILED message `message` and log it.
    Discard { message: String },
    /// Hold `agent` as HELD by the user, released only by the user.
    Hold { agent: String },
    /// Unregister `agent`.
    Unregister { agent: String },
    /// Dismiss the alert `alert`.
    Dismiss { alert: String },
    /// Release a hold whose message the store no longer holds (a failed doctor check's fix).
    ReleaseStale { agent: String },
    /// Clear the marked `path`, as `lunchroom clear <path>`.
    Clear { path: String },
    /// Set the hold mode.
    Mode(HoldMode),
    /// Yolo for the named agents until `till` (`HH:MM`, UTC).
    Yolo { names: String, till: String },
    /// Approve a phase of a controller run: sign and write the record (item 9). `manifest_hash` is
    /// the plan hash the user was shown.
    ControllerApprove {
        run_id: String,
        phase: String,
        manifest_hash: String,
    },
}

impl Action {
    /// What the action does, in a few words, for the confirmation.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Approve { number, .. } => format!("APPROVE CALL {number}"),
            Self::Refuse { number, .. } => format!("REFUSE CALL {number}"),
            Self::Release { agent } | Self::ReleaseStale { agent } => {
                format!("RELEASE {agent}")
            }
            Self::Resend { message } => format!("RESEND {message}"),
            Self::Discard { message } => format!("DISCARD {message}"),
            Self::Hold { agent } => format!("HOLD {agent}"),
            Self::Unregister { agent } => format!("UNREGISTER {agent}"),
            Self::Dismiss { .. } => "DISMISS ALERT".to_owned(),
            Self::Clear { path } => format!("CLEAR {path}"),
            Self::Mode(mode) => format!("MODE {}", mode.word().to_ascii_uppercase()),
            Self::Yolo { names, till } => format!("YOLO {names} TILL {till}"),
            Self::ControllerApprove { run_id, phase, .. } => {
                format!("CONTROLLER APPROVAL {run_id} {phase}")
            }
        }
    }
}

/// What the store side did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The action was done.
    pub done: bool,
    /// What happened, plainly.
    pub text: String,
}

impl Outcome {
    #[must_use]
    pub fn done(text: impl Into<String>) -> Self {
        Self {
            done: true,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn refused(text: impl Into<String>) -> Self {
        Self {
            done: false,
            text: text.into(),
        }
    }
}

/// A controller approval request as the console shows it (item 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReqView {
    pub run_id: String,
    pub phase: String,
    pub manifest_hash: String,
}

/// The requests in `requests/`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub requests: Vec<ReqView>,
    /// Files in the folder that are not a valid request.
    pub ignored: usize,
    /// Why the folder could not be listed.
    pub error: Option<String>,
}

/// What the screen asks of the system around it. The real one is [`super::live::RealBackend`]; a
/// test gives its own.
pub trait Backend {
    /// The typed passphrase is the developer's.
    fn check_passphrase(&mut self, typed: &str) -> bool;
    /// Runs `action`, which the user has confirmed.
    fn perform(&mut self, action: &Action, now: u64) -> Outcome;
    /// The requests in `requests/`, each with its run id, phase and plan hash.
    fn controller_requests(&mut self) -> Listing;
    /// The request for `run_id` and `phase`, read and checked as item 9 says.
    ///
    /// # Errors
    /// Why the approval is refused.
    fn controller_load(&mut self, run_id: &str, phase: &str) -> Result<ReqView, String>;
}
