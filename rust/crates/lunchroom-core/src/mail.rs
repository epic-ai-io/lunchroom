//! Messages between agents: who may send to whom, delivery, and what holds a session.
//!
//! Spec: §9 I1 (identity, opt-in receiving, names, replies from a HELD session), §10 D1 (QUEUED
//! until the recipient's harness confirms, shown once, honest outcomes, FAILED after 3 attempts,
//! delivery to a HELD session), and the rules that stop a message: §2 R1 (HALTED), §11 L1.2 and
//! L1.4 (secrets). [`Mail`] is the whole state; [`crate::store::Store`] keeps it across crashes and
//! makes each change one atomic step.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approvals::{Approval, Request};
use crate::identity::{Harness, Identity};
use crate::loops::Limits;
use crate::loops::{Conversations, Outcome};
use crate::secrets::{asks_for_secret, has_secret};
use crate::vocab::{AgentState, MessageState, Reason};

/// Failed delivery attempts after which a message is FAILED (D1.4).
pub const MAX_ATTEMPTS: u32 = 3;

/// The number a store written by this program carries, so a later program can tell a store an
/// older program rewrote (it drops the number) from one this program wrote. 1.0.1 wrote none.
pub const SCHEMA: u32 = 2;

/// Seconds a failed hand-over waits before the same message is offered again: attempts that fail
/// at once (a program that exits with an error) are not used up back to back.
pub const RETRY_GAP_SECS: u64 = 10;

/// Seconds a stamp may stand ahead of a process's own clock before it is taken for a stamp from a
/// clock that stepped back. Two processes read the clock at different moments, and a check that
/// waited for the store's lock reads an older one than the hand-over it finds.
pub const FUTURE_SLACK_SECS: u64 = 5;

/// How many notices a list keeps: past it the oldest are dropped, and one line says how many.
pub const NOTICES_KEPT: usize = 50;

/// How many notices of one message a list keeps.
const NOTICES_PER_MESSAGE: usize = 8;

/// How long the notices of a session that is no longer running are kept: a day.
pub const GONE_NOTICE_SECS: u64 = 86_400;

/// The longest message text Lunchroom accepts, in UTF-8 bytes (64 KiB). Every send is stored whole
/// in `state.json`, rewritten on every guard hook. No harness takes the envelope as a command
/// argument (D1.1), so the number of lines is not limited (D1.2).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Seconds an attempt waits for the harness's confirmation before it counts as failed (D1.3, D1.4).
pub const CONFIRM_SECS: u64 = 60;

/// Seconds a message handed to a harness that confirms it only when its turn ends (Codex, Hermes)
/// waits for that confirmation before the hand-over counts as one failed attempt (D1.4). Ten
/// minutes outlasts an ordinary turn, and a longer wait only costs a repeated copy, which the guard
/// blocks as a repeat (D1.2).
pub const HANDOVER_SECS: u64 = 600;

/// Where a message stands (D1.1, D1.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Queued,
    Delivered,
    Failed { reason: String },
}

impl Status {
    /// The table word for this status.
    #[must_use]
    pub const fn state(&self) -> MessageState {
        match self {
            Self::Queued => MessageState::Queued,
            Self::Delivered => MessageState::Delivered,
            Self::Failed { .. } => MessageState::Failed,
        }
    }
}

/// One message, recorded by identity (I1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: Identity,
    /// The sender's name when it sent, for display only (I1.1).
    pub from_name: String,
    pub to: Identity,
    /// The name the sender addressed, for display only.
    pub to_name: String,
    pub text: String,
    pub sent_at: u64,
    pub status: Status,
    pub failed_attempts: u32,
    /// When the last attempt put it in front of the recipient's harness, while unconfirmed.
    #[serde(default)]
    pub presented_at: Option<u64>,
    /// The recipient's harness accepted it (Codex: `codex queue` succeeded), so it is never
    /// handed over again: it stays QUEUED, not confirmed, until the harness confirms it (D1.2,
    /// D1.3).
    #[serde(default)]
    pub handed_over: bool,
    /// The order this message was recorded in, among all messages and all lines of Lunchroom's:
    /// one counter in the store, taken under its lock (§17.10: equal times keep the order
    /// recorded). 0 for a message recorded before 1.1. A field of 1.1: kept in the second file.
    #[serde(default)]
    pub rec: u64,
    /// What the user and the sender have been told of it while it waits (D1.8), and why it cannot
    /// be handed over now (D1.9). A store written before 1.1.0 reads as nothing told.
    #[serde(default)]
    pub told: Told,
    /// Fields a later program wrote that this one does not know, carried through every read and
    /// write so an older program in a session that stays open does not erase them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The plain session record of a Codex session as it stood at a hand-over (D1.1): its length and its
/// file identity (device and inode), all 0 when there was none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMark {
    pub len: u64,
    pub dev: u64,
    pub ino: u64,
}

/// What Lunchroom has told of a QUEUED message, and why it cannot be handed over (D1.8, D1.9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // independent facts about what was told, read separately
pub struct Told {
    /// The hand-over (its `presented_at`) whose wait of [`WAITING_TOLD_SECS`] was told.
    #[serde(default)]
    pub waiting: Option<u64>,
    /// Why the message cannot be handed over now, found at a hand-over (its program cannot be
    /// established), while that holds; `None` once a hand-over goes ahead.
    #[serde(default)]
    pub blocked: Option<String>,
    /// That reason was told.
    #[serde(default)]
    pub blocked_told: bool,
    /// Why the message cannot be handed over now, from its recipient's state (UNGUARDED with its
    /// R3.4 reason, or STOPPED); `None` while the recipient is guarded.
    #[serde(default)]
    pub unreachable: Option<String>,
    /// That reason was told.
    #[serde(default)]
    pub unreachable_told: bool,
    /// The reason of its last failed attempt.
    #[serde(default)]
    pub last_failure: Option<String>,
    /// Something about it was told, so its DELIVERED is told too.
    #[serde(default)]
    pub any: bool,
    /// The sender was told why it cannot be handed over when it sent it, so that reason is not
    /// queued to the sender again (the user is still told).
    #[serde(default)]
    pub sender_told: bool,
    /// The time before which a failed hand-over is not offered again ([`RETRY_GAP_SECS`]).
    #[serde(default)]
    pub retry_after: Option<u64>,
    /// Fields a later program wrote that this one does not know.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// D1.8: how long a hand-over waits for its confirmation before the user and the sender are told.
/// A message that cannot be handed over is told as soon as that is found.
pub const WAITING_TOLD_SECS: u64 = 120;

/// A message that holds a session (D1.5, I1.4). A record from before I1 carries only the sender's
/// name: it holds the session and permits no reply (I1.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub message: String,
    #[serde(default)]
    pub from: Option<Identity>,
    pub from_name: String,
}

/// Where a hook last saw a session run (R3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The harness process, `<pid>:<start time>`.
    pub process: String,
    pub cwd: PathBuf,
    /// The script the harness process runs under its runtime, as its plugin read it from inside
    /// that process: the gateway rewrites its own command line, so the system cannot show it.
    #[serde(default)]
    pub entry: Option<PathBuf>,
}

/// What the harness adapter found about an agent's guard (R3.1, R3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The session is not running, or cannot be reached: STOPPED.
    NotRunning,
    /// It runs, but the configuration that runs its guard is not in place: UNGUARDED.
    RunningUnguarded,
    /// Its guard ran in its current session and the configuration is in place.
    Guarded,
}

/// What a sender is told about one of its messages (D1.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub message: String,
    pub text: String,
    /// When it was made (seconds since the Unix epoch); a notice from before 1.1.0 has none.
    #[serde(default)]
    pub at: u64,
    /// It states why the message cannot be handed over or waits: a newer one for the same message
    /// replaces it while unread.
    #[serde(default)]
    pub reason: bool,
}

/// Who may register a name (I1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The user, through the approval channel (A1).
    User,
    /// A command run from inside a guarded session.
    GuardedSession,
}

/// Why a registration did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRefusal {
    /// It came from inside a guarded session (I1.5): DENIED.
    FromGuardedSession,
    /// The name is taken (I1.5): reported to the user, never moved silently. The user may then
    /// register it again with `replace`.
    Taken { holder: Identity },
    /// The name is not 1 to 32 letters, digits, `.`, `_` or `-`: it must stand as one word in a
    /// command and in a message's header line.
    InvalidName,
}

/// A name an agent can be given: 1 to 32 ASCII letters, digits, `.`, `_` or `-`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Why a message was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRefusal {
    /// No hook has seen the sender's session (I1.2).
    UnseenSender,
    /// No session has this name.
    UnknownName,
    /// The user has not named the recipient through the approval channel (I1.5).
    NotNamed,
    /// The sender is HELD and the recipient's messages do not hold it (I1.4).
    NotHolder,
    /// The sender is HELD by a record that carries only a name: no reply (I1.6).
    NameOnlyHolder,
    /// The text is longer than [`MAX_MESSAGE_BYTES`].
    TooLong,
    /// DENIED by a rule: `leak` (L1.2, L1.4), `halted` or `rate limit` (R1).
    Denied(Reason),
}

/// A message accepted and QUEUED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub id: String,
    /// The sender: its process hands over only its own messages.
    pub from: Identity,
    pub to: Identity,
    /// This message was delivered-and-halts: the pair is now HALTED for reason `loop` (R1.3).
    pub halted: bool,
    /// The line of Lunchroom's that tells it (card 3, line 3), for the sender's `send` output.
    pub halted_line: Option<String>,
}

/// What the recipient's guard does with a message it sees arrive (D1.1, D1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// The first time: it is now DELIVERED and holds the session; show it.
    Show,
    /// It was already DELIVERED: a repeated attempt; do not show it again.
    Duplicate,
    /// It is FAILED and its sender was told so (D1.4): a late copy; do not let it in.
    Failed,
    /// No such message for this session.
    NotFound,
}

/// What a failed attempt led to (D1.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    StillQueued { failed_attempts: u32 },
    Failed,
}

/// How far a session's transcript has been read (T1.7). It holds a byte offset and message ids, and
/// no text of the transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRead {
    /// The transcript's path, so a transcript that is another file starts again.
    #[serde(default)]
    pub path: String,
    /// The byte the next read starts at.
    #[serde(default)]
    pub offset: u64,
    /// The ids of the labels found so far.
    #[serde(default)]
    pub ids: BTreeSet<String>,
    /// The read reached the end of the transcript.
    #[serde(default)]
    pub done: bool,
    /// The user typed after the read began: the unread transcript no longer holds the session, and
    /// the labels found before are not held against it.
    #[serde(default)]
    pub typed: bool,
    /// A hook began a read that has not reported its result: it was cut off by the guard's time,
    /// or its file stalls. The next hook's read of it is limited in time, so a stalled transcript
    /// does not cost every hook the guard's whole time.
    #[serde(default)]
    pub reading: bool,
}

/// What the guard judged a call to write, kept from before the call to its after-call hook (T1.8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingCall {
    /// The paths, each spelling the guard resolved a word to.
    #[serde(default)]
    pub paths: Vec<String>,
    /// When it was kept.
    #[serde(default)]
    pub at: u64,
}

/// Paths a HELD session's call wrote, with the messages that held it at that moment (T1.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marked {
    /// The paths the call wrote, as the guard judged them before the call (every spelling).
    pub paths: Vec<String>,
    /// The session that wrote them, and the name it had.
    pub writer: Identity,
    #[serde(default)]
    pub writer_name: String,
    /// The messages that held the writer when the call ran.
    pub holders: Vec<Hold>,
    /// When the call's after-call hook ran.
    #[serde(default)]
    pub since: u64,
    /// The harness's id for the call.
    #[serde(default)]
    pub call: String,
}

/// The name a hold takes while a session's transcript is not read to its end (T1.7): the source
/// "unjudged", as for a tool output the guard did not judge, with this as its mark.
pub const UNREAD_TRANSCRIPT: &str = "transcript (not read to the end)";

/// Most calls whose written paths are kept between a call's two hooks: a call that never reaches
/// its after-call hook (cancelled by the user) leaves its entry, so the oldest go.
const WRITING_KEPT: usize = 512;

/// How long the paths of a call are kept for its after-call hook: a day.
const WRITING_SECS: u64 = 86_400;

/// Every message, name, hold and approval Lunchroom keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    /// The [`SCHEMA`] of the program that last wrote the store: every write stamps the current one,
    /// and a store an older program rewrote reads as less. 0 is a store from before 1.1.0.
    #[serde(default, serialize_with = "write_schema")]
    schema: u32,
    next_message: u64,
    /// Identity keys of the sessions a hook has seen (I1.2).
    seen: BTreeSet<String>,
    /// The session a hook last saw in each harness process, keyed `<pid>:<start time>` (I1.2).
    #[serde(default)]
    processes: BTreeMap<String, Identity>,
    /// The boot, and the time on the clock of a process's start time, just after the install set
    /// `unified_exec_tty`: Codex reads it when a session starts, so a process started by then can
    /// still type past the guard (H1).
    #[serde(default)]
    codex_setting: Option<(String, u64)>,
    /// The sessions a hook had seen when the install set `unified_exec_tty`, by identity key.
    #[serde(default)]
    codex_setting_open: BTreeSet<String>,
    /// Where a hook last saw each session run, by identity key: its harness process and working
    /// folder, the evidence R3.1 reads.
    #[serde(default)]
    sessions: BTreeMap<String, SessionRecord>,
    names: BTreeMap<String, Identity>,
    /// Identity keys of the sessions the user named, which alone receive messages (I1.5).
    receivers: BTreeSet<String>,
    messages: BTreeMap<String, Message>,
    /// Message ids by recipient: per harness, keyed by session id (I1.3). The harness level keeps
    /// the same session id in two harnesses two agents (I1.1).
    inboxes: BTreeMap<Harness, BTreeMap<String, Vec<String>>>,
    /// What holds each session, by identity key (D1.5).
    pub(crate) holds: BTreeMap<String, Vec<Hold>>,
    notices: BTreeMap<String, Vec<Notice>>,
    /// What the user is told of messages that wait or end (D1.8), in the next prompt or listener
    /// of any guarded session; each once.
    #[serde(default)]
    user_notices: Vec<Notice>,
    /// The latest time a check of D1.7 ran at (seconds since the Unix epoch).
    #[serde(default)]
    seen_at: u64,
    conversations: Conversations,
    pub(crate) next_approval: u64,
    pub(crate) approvals: Vec<Approval>,
    #[serde(default)]
    pub(crate) requests: Vec<Request>,
    /// The lines that tell an agent of the user's approval or refusal of one of its calls (A1.2,
    /// §17.10), each with the text `lunchroom read <id>` prints. A field of 1.1: it is kept in the
    /// store's second file (D1.2).
    #[serde(default)]
    pub(crate) tells: Vec<Tell>,
    /// The calls the user refused (§17.10): a retry of the exact call is DENIED with the reason that
    /// the user refused it. A field of 1.1: it is kept in the store's second file.
    #[serde(default)]
    pub(crate) declined: Vec<Refusal>,
    /// The plain session record as it stood when each message was handed to a Codex session, by
    /// message id (D1.1). A field of 1.1: it is kept in the store's second file.
    #[serde(default)]
    handed_at: BTreeMap<String, RecordMark>,
    /// Calls the guard sent to the user's own permission prompt, by session key and the harness's
    /// id for the call, with the agent's name and the action, so card 4's line follows the yes.
    #[serde(default)]
    pub(crate) asked: Vec<(String, String, String)>,
    /// The end time of each `lunchroom yolo` the user set, by identity key (R2.2).
    #[serde(default)]
    yolo: BTreeMap<String, u64>,
    /// Identity keys of the sessions whose harness reported its own YOLO at their last call (R2.6).
    #[serde(default)]
    harness_yolo: BTreeSet<String>,
    /// Identity keys of the named agents the user has been told are UNGUARDED (R3.4).
    #[serde(default)]
    unguarded_told: BTreeSet<String>,
    /// F2-AC55: when the user dismissed each session's PROBING alert, by session id; the alert
    /// stays dark until a DENIED decision newer than this. A field of 1.2: it is kept in the store's
    /// second file (D1.2).
    #[serde(default)]
    probe_dismissed: BTreeMap<String, u64>,
    /// Identity keys of the sessions whose transcript a guard hook has searched (T1.7). Kept for a
    /// 1.0.1 program, which reads it; 1.1 reads [`Mail::transcripts`].
    #[serde(default)]
    transcripts_read: BTreeSet<String>,
    /// How far each session's transcript has been read, by identity key (T1.7): the byte offset
    /// and the message ids found, never the transcript's text. A field of 1.1: it is kept in the
    /// store's second file (D1.2).
    #[serde(default)]
    transcripts: BTreeMap<String, TranscriptRead>,
    /// The paths each call of a session is judged to write, by session key and the harness's id
    /// for the call, kept from the call's guard hook to its after-call hook (T1.8).
    #[serde(default)]
    writing: BTreeMap<String, WritingCall>,
    /// The paths a HELD session's calls wrote, each with the messages that held it (T1.8). Never
    /// expires: only the user clears one.
    #[serde(default)]
    marked: Vec<Marked>,
    /// The answer `sudo -n -l` gave at the last install (A1.3, §17.8: "keeps the answer"), as the
    /// JSON of [`crate::sudoers::Kept`]. A field of 1.1: it is kept in the store's second file. A
    /// value that is not an answer reads as none kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sudo_answer: Option<Value>,
    /// For each Codex session, by identity key, the fingerprint of the trusted guard that ran in
    /// it (R3.1, Codex adapter).
    #[serde(default)]
    guard_fingerprints: BTreeMap<String, String>,
    /// For each harness, the guard fingerprint recorded last in any of its sessions, kept after
    /// that session is gone: for a plugin harness, the plugin's file and the release it reported
    /// (C1.4: `lunchroom doctor` reads the release Hermes's plugin reported in its latest session).
    #[serde(default)]
    latest_guard_fingerprint: BTreeMap<Harness, String>,
    /// Every session a hook saw in each harness process, by `<pid>:<start time>`: one OpenClaw or
    /// Hermes gateway process runs many sessions (I1.2).
    #[serde(default)]
    process_members: BTreeMap<String, BTreeSet<String>>,
    /// The hold mode the user chose; Balanced until they choose (P1.6).
    #[serde(default)]
    hold_mode: Option<crate::policy::HoldMode>,
    /// The title Lunchroom last sent each session, by identity key: a session's title that equals
    /// it is Lunchroom's own, not a custom title (I1.5). It outlives the session's names.
    #[serde(default)]
    titles: BTreeMap<String, String>,
    /// Every line of Lunchroom's, with its time and its agents, never removed (§16 item 1). A
    /// field of 1.1: it is kept in the store's second file (D1.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lines: Vec<lines::LineRecord>,
    /// The last number of the one counter that orders every message and every line recorded
    /// ([`Message::rec`], [`lines::LineRecord::rec`]). A field of 1.1.
    #[serde(default, skip_serializing_if = "is_zero")]
    rec_counter: u64,
    /// Identity keys of the sessions that have been told what HELD means (V1.7, §16 item 1): a
    /// line is told to a session, so a session that starts anew is told again. A field of 1.1.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    held_defined: BTreeSet<String>,
    /// For each session, by identity key, the sequence numbers of the lines not yet shown in an
    /// envelope (channel N, §16 item 2). A line is kept until the session hears it (K1.29): the
    /// one-day rule of D1.8 does not remove it. A field of 1.1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    envelope_pending: BTreeMap<String, Vec<u64>>,
    /// The calls waiting for the user (D1.8). A field of 1.1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    calls: Vec<calls::WaitingCall>,
    /// Approvals given for kept calls and not yet used or expired (D1.8 "Not used in time"). A
    /// field of 1.1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unused: Vec<calls::UnusedWatch>,
    /// For each message that began a hold, the sequence number of the line "<agent> is HELD."
    /// that told it (§16 item 5). A field of 1.1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    held_lines: BTreeMap<String, u64>,
    /// For each message whose envelope was built, the sequence numbers of the lines it carried,
    /// until the message is confirmed (§16 item 2, channel N). A field of 1.1.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    envelope_shown: BTreeMap<String, Vec<u64>>,
    /// What the program last found of each named agent's guard (R3), by identity key, for the
    /// console: it runs as the user `lunchroom`, which reads neither the developer's home folder
    /// nor another user's `/proc/<pid>/exe`, so it shows what the developer's own program found.
    /// A field of 1.2: it is kept in the store's second file (D1.2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    judged: BTreeMap<String, Judged>,
    /// The text of each arrival of another agent's text that Lunchroom did not deliver and that
    /// holds a session (an OpenClaw or Hermes prompt the user did not type, a harness's own
    /// cross-session message), by the hold's message key, cut at [`UNRECORDED_MAX`] bytes: the
    /// console's panel of a HELD agent shows each message that holds it, whole (§17.10). Kept
    /// while it holds a session. A field of 1.2: it is kept in the store's second file (D1.2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unrecorded: BTreeMap<String, Unrecorded>,
    /// Fields a later program wrote that this one does not know, carried through every read and
    /// write.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// Serde: a counter at 0 is not written.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
const fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Mail {
    /// The next number of the one counter that orders every message and every line recorded.
    pub(crate) fn next_rec(&mut self) -> u64 {
        self.rec_counter += 1;
        self.rec_counter
    }

    /// Records what the program found of `who`'s guard, replacing what it found before. A
    /// session that has no name is not recorded: the console shows only named agents.
    pub fn set_judged(&mut self, who: &Identity, judged: Judged) {
        let key = who.key();
        if self.names.values().any(|w| w.key() == key) {
            self.judged.insert(key, judged);
        }
    }

    /// [`Mail::hold_unrecorded`], keeping the arrival's `text`, every secret replaced
    /// ([`crate::secrets::redact`]) and cut at [`UNRECORDED_MAX`] bytes at a character boundary,
    /// for the console's panel of the HELD agent.
    pub fn hold_unrecorded_text(
        &mut self,
        who: &Identity,
        source: &str,
        mark: &str,
        text: &str,
        now: u64,
    ) {
        self.hold_unrecorded(who, source, mark);
        // decision of 2026-11-14 ("Keep, secrets replaced"): kept with every secret replaced, as a
        // DENIED record is (V1.4), then cut.
        let text = crate::secrets::redact(text);
        let mut end = text.len().min(UNRECORDED_MAX);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.unrecorded.insert(
            format!("{source}:{mark}"),
            Unrecorded {
                text: text[..end].to_owned(),
                cut: end < text.len(),
                at: now,
            },
        );
    }

    /// The kept text of the arrival that the hold `message` names, if there is one.
    #[must_use]
    pub fn unrecorded(&self, message: &str) -> Option<&Unrecorded> {
        self.unrecorded.get(message)
    }

    /// What the program last found of `who`'s guard, if it has judged it.
    #[must_use]
    pub fn judged(&self, who: &Identity) -> Option<&Judged> {
        self.judged.get(&who.key())
    }
}

/// The most bytes of an arrival's text the store keeps for the console.
pub const UNRECORDED_MAX: usize = 64 * 1024;

/// The text of an arrival Lunchroom did not deliver, and when it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unrecorded {
    pub text: String,
    /// The text was longer than [`UNRECORDED_MAX`] bytes and was cut.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut: bool,
    pub at: u64,
}

/// What the developer's own program found of an agent's guard (R3), and when, for the console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judged {
    /// What the evidence showed.
    pub found: Found,
    /// For an UNGUARDED agent, the one reason R3.4 gives, as the user reads it; else empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The harness process the judgment was made of, `<pid>:<start time>`, if the session had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// When (seconds since the Unix epoch).
    pub at: u64,
}

/// The three findings of R3.1 to R3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Found {
    /// Not running, or cannot be reached.
    NotRunning,
    /// Running without the evidence R3.1 needs.
    Unguarded,
    /// The guard ran in its current session and the configuration is in place.
    Guarded,
}

/// The fields 1.1 adds are kept in the store's second file (D1.2, [`side`]).
impl crate::store::Sided for Mail {
    fn split(main: &mut Value) -> Value {
        side::split(main)
    }

    fn join(main: &mut Value, side_fields: &Value) {
        side::join(main, side_fields);
    }
}

/// Writes the current [`SCHEMA`] whatever the store was read with. (Serde hands the field by
/// reference; generic over its type, the function takes it as serde gives it.)
fn write_schema<T, S: serde::Serializer>(_read: &T, out: S) -> Result<S::Ok, S::Error> {
    out.serialize_u32(SCHEMA)
}

/// The text of the line a list of notices starts with once it has dropped some.
fn dropped_line(count: usize) -> String {
    format!(
        "{count} earlier notice{} dropped because the list holds at most {NOTICES_KEPT}.",
        if count == 1 { " was" } else { "s were" }
    )
}

/// How many notices the first line of `list` says were dropped, if it is that line.
fn dropped_count(list: &[Notice]) -> usize {
    let ending = format!(" dropped because the list holds at most {NOTICES_KEPT}.");
    list.first()
        .filter(|n| n.message.is_empty() && n.text.ends_with(&ending))
        .and_then(|n| n.text.split(' ').next()?.parse().ok())
        .unwrap_or(0)
}

/// Adds `notice` to `list`, which stays small: an identical notice is not added twice; with
/// `clears` (or when the notice states a reason) the unread reasons of the same message go, since
/// the newer one replaces them; no more than [`NOTICES_PER_MESSAGE`] stand for one message; and
/// past [`NOTICES_KEPT`] the oldest go, the list starting with one line that says how many.
fn push_notice(list: &mut Vec<Notice>, notice: Notice, clears: bool) {
    if list
        .iter()
        .any(|n| n.message == notice.message && n.text == notice.text)
    {
        return;
    }
    if notice.message.is_empty() {
        list.push(notice);
    } else {
        if clears || notice.reason {
            list.retain(|n| !(n.message == notice.message && n.reason));
        }
        let id = notice.message.clone();
        list.push(notice);
        while list.iter().filter(|n| n.message == id).count() > NOTICES_PER_MESSAGE {
            if let Some(oldest) = list.iter().position(|n| n.message == id) {
                list.remove(oldest);
            }
        }
    }
    if list.len() > NOTICES_KEPT {
        let already = dropped_count(list);
        if already > 0 {
            list.remove(0);
        }
        let excess = list.len().saturating_sub(NOTICES_KEPT - 1);
        list.drain(..excess);
        list.insert(
            0,
            Notice {
                message: String::new(),
                text: dropped_line(already + excess),
                at: 0,
                reason: false,
            },
        );
    }
}

/// Removes from `list` one notice equal (message and text) to each of `told`.
fn drop_told(list: &mut Vec<Notice>, told: &[Notice]) {
    for t in told {
        if let Some(at) = list
            .iter()
            .position(|n| n.message == t.message && n.text == t.text)
        {
            list.remove(at);
        }
    }
}

/// How long the text of a DELIVERED or FAILED message that holds nobody is kept: a week.
pub const KEEP_SECS: u64 = 7 * 86_400;

impl Mail {
    /// Forgets what can no longer matter, so the store stays the size of what is live: harness
    /// processes no longer running (`alive` says which `<pid>:<start time>` still runs), and the
    /// text of DELIVERED or FAILED messages older than [`KEEP_SECS`] that hold no session. Each
    /// message's record stays, so a late copy is still known for what it is (D1.2).
    pub fn prune(&mut self, now: u64, alive: impl Fn(&str) -> bool) {
        self.expire_handovers(now);
        self.expire_requests(now);
        // Notices nobody read: those of a session no longer running, after a day, and the user's
        // after a week; the sender's of a session that still runs are kept (capped by count).
        let running: BTreeSet<String> = self
            .sessions
            .iter()
            .filter(|(_, r)| alive(&r.process))
            .map(|(k, _)| k.clone())
            .collect();
        self.notices.retain(|k, list| {
            if !running.contains(k) {
                list.retain(|n| now.saturating_sub(n.at) < GONE_NOTICE_SECS);
            }
            !list.is_empty()
        });
        self.user_notices
            .retain(|n| now.saturating_sub(n.at) < KEEP_SECS);
        self.forget_old_writing(now);
        self.forget_old_tells(now);
        let known: BTreeSet<String> = self.messages.keys().cloned().collect();
        self.handed_at.retain(|id, _| known.contains(id));
        // The text of another agent's arrival is kept while it holds a session.
        let held_by: BTreeSet<&String> =
            self.holds.values().flatten().map(|h| &h.message).collect();
        self.unrecorded.retain(|k, _| held_by.contains(k));
        self.processes.retain(|k, _| alive(k));
        self.process_members.retain(|k, _| alive(k));
        let holding: BTreeSet<String> = self
            .holds
            .values()
            .flatten()
            .map(|h| h.message.clone())
            .collect();
        for m in self.messages.values_mut() {
            if m.status != Status::Queued
                && now.saturating_sub(m.sent_at) > KEEP_SECS
                && !holding.contains(&m.id)
            {
                m.text.clear();
            }
        }
    }

    /// Records the fingerprint of the trusted guard that ran in `who`'s session, or forgets it
    /// when there is none now.
    pub fn record_guard_fingerprint(&mut self, who: &Identity, fingerprint: Option<String>) {
        match fingerprint {
            Some(f) => {
                self.latest_guard_fingerprint.insert(who.harness, f.clone());
                self.guard_fingerprints.insert(who.key(), f);
            }
            None => {
                self.guard_fingerprints.remove(&who.key());
            }
        }
    }

    /// The fingerprint of the trusted guard last seen running in `who`'s session.
    #[must_use]
    pub fn guard_fingerprint(&self, who: &Identity) -> Option<&str> {
        self.guard_fingerprints.get(&who.key()).map(String::as_str)
    }

    /// The guard fingerprint recorded last in any session of `harness` (C1.4), or `None` when no
    /// session of it has recorded one.
    #[must_use]
    pub fn latest_guard_fingerprint(&self, harness: Harness) -> Option<&str> {
        self.latest_guard_fingerprint
            .get(&harness)
            .map(String::as_str)
    }

    /// R3.4: given every named agent that is UNGUARDED now, the ones the user has not yet been
    /// told about; each is told once until it is guarded again, when it is forgotten so a later
    /// change is told again.
    pub fn newly_unguarded(&mut self, unguarded: &[Identity]) -> Vec<Identity> {
        let now: BTreeSet<String> = unguarded.iter().map(Identity::key).collect();
        self.unguarded_told.retain(|k| now.contains(k));
        unguarded
            .iter()
            .filter(|who| self.unguarded_told.insert(who.key()))
            .cloned()
            .collect()
    }

    /// A hook saw this session (I1.2).
    pub fn observe(&mut self, who: &Identity) {
        self.seen.insert(who.key());
    }

    /// Keeps the answer `sudo -n -l` gave (A1.3), replacing the one kept before.
    pub fn keep_sudo_answer(&mut self, kept: &crate::sudoers::Kept) {
        self.sudo_answer = serde_json::from_str(&kept.to_json()).ok();
    }

    /// The answer kept at the last install; `None` when none was kept or the field is not an
    /// answer (`lunchroom doctor` then has nothing to compare with).
    #[must_use]
    pub fn sudo_answer(&self) -> Option<crate::sudoers::Kept> {
        crate::sudoers::Kept::from_json(&self.sudo_answer.as_ref()?.to_string())
    }

    /// The ids of every message Lunchroom has a record of: the labels of a transcript that name
    /// none of them hold nothing (T1.7).
    #[must_use]
    pub fn message_ids(&self) -> BTreeSet<String> {
        self.messages.keys().cloned().collect()
    }

    /// A hook saw this session running in the harness process `process` (its pid and start time,
    /// read from the system, not from the environment). `lunchroom send` finds its sender by
    /// walking up from its own process to one recorded here (I1.2).
    pub fn observe_process(&mut self, process: &str, cwd: &Path, who: &Identity) {
        self.observe(who);
        self.processes.insert(process.to_owned(), who.clone());
        self.process_members
            .entry(process.to_owned())
            .or_default()
            .insert(who.key());
        self.sessions.insert(
            who.key(),
            SessionRecord {
                process: process.to_owned(),
                cwd: cwd.to_path_buf(),
                entry: None,
            },
        );
    }

    /// The harness process running `who` runs the script `entry` under its runtime.
    pub fn observe_entry(&mut self, who: &Identity, entry: &Path) {
        if let Some(record) = self.sessions.get_mut(&who.key()) {
            record.entry = Some(entry.to_path_buf());
        }
    }

    /// The install set `unified_exec_tty` in the boot `boot`, by `start` on the clock of a process's
    /// start time.
    pub fn codex_setting_changed(&mut self, boot: &str, start: u64) {
        self.codex_setting = Some((boot.to_owned(), start));
        self.codex_setting_open = self.sessions.keys().cloned().collect();
    }

    /// `who` was open when the install set `unified_exec_tty`: a hook had seen it, and its process
    /// has not started since, as the process's key and `boot`, this boot, tell. Codex can run a new
    /// session in a server older than the install, so an old process alone proves nothing. A later
    /// boot started every process after it; what cannot be read counts as before.
    #[must_use]
    pub fn opened_before_codex_setting(&self, who: &Identity, boot: Option<&str>) -> bool {
        let (Some((set_in, set_at)), Some(record)) = (&self.codex_setting, self.session(who))
        else {
            return false;
        };
        if !self.codex_setting_open.contains(&who.key()) {
            return false;
        }
        if boot.is_none() || set_in.is_empty() {
            return true;
        }
        if boot != Some(set_in.as_str()) {
            return false;
        }
        let start = record
            .process
            .split(':')
            .nth(1)
            .and_then(|s| s.parse().ok());
        start.is_none_or(|s: u64| s <= *set_at)
    }

    /// A hook saw `who` in the harness process `process` (I1.2).
    #[must_use]
    pub fn seen_in_process(&self, process: &str, who: &Identity) -> bool {
        self.process_members
            .get(process)
            .is_some_and(|m| m.contains(&who.key()))
    }

    /// How many sessions a hook saw in the harness process `process`.
    #[must_use]
    pub fn sessions_in_process(&self, process: &str) -> usize {
        self.process_members.get(process).map_or(0, BTreeSet::len)
    }

    /// Where a hook last saw `who` run (R3.1).
    #[must_use]
    pub fn session(&self, who: &Identity) -> Option<&SessionRecord> {
        self.sessions.get(&who.key())
    }

    /// `who`'s state from the vocabulary table, given the evidence for its guard (R3.1–R3.3): never
    /// AVAILABLE or HELD without the evidence that its guard runs in its current session.
    #[must_use]
    pub fn agent_state(&self, who: &Identity, evidence: Evidence) -> AgentState {
        match evidence {
            Evidence::NotRunning => AgentState::Stopped,
            Evidence::RunningUnguarded => AgentState::Unguarded,
            Evidence::Guarded if self.is_held(who) => AgentState::Held,
            Evidence::Guarded => AgentState::Available,
        }
    }

    /// The session a hook last saw running in the harness process `process`.
    #[must_use]
    pub fn process_session(&self, process: &str) -> Option<&Identity> {
        self.processes.get(process)
    }

    /// Every harness process a hook has recorded, as `<pid>:<start time>`.
    pub fn harness_processes(&self) -> impl Iterator<Item = &str> {
        self.processes.keys().map(String::as_str)
    }

    /// Sets a `lunchroom yolo` for `who` until `end` (R2.2). Only the `lunchroom yolo` command calls
    /// this, after reading the user's confirmation from a terminal no guarded session owns (A1.3);
    /// the guard DENIES an agent's `lunchroom yolo` (A1.3, node `§7::11`).
    pub fn set_yolo(&mut self, who: &Identity, end: u64) {
        self.yolo.insert(who.key(), end);
    }

    /// The end time of the `lunchroom yolo` covering `who` at `now`, if one does: it covers until
    /// The hold mode in force (P1.6): the user's choice, else Balanced.
    #[must_use]
    pub fn hold_mode(&self) -> crate::policy::HoldMode {
        self.hold_mode.unwrap_or_default()
    }

    /// Whether the user has chosen a hold mode yet.
    #[must_use]
    pub const fn hold_mode_chosen(&self) -> bool {
        self.hold_mode.is_some()
    }

    /// The user chooses the hold mode (P1.6). Only the user's own commands call this (A1.3).
    pub fn set_hold_mode(&mut self, mode: crate::policy::HoldMode) {
        self.hold_mode = Some(mode);
    }

    /// its end time and not after (R2.2).
    #[must_use]
    pub fn yolo_until(&self, who: &Identity, now: u64) -> Option<u64> {
        self.yolo.get(&who.key()).copied().filter(|end| now < *end)
    }

    /// What `who`'s harness reported about its own YOLO at its latest call (R2.6).
    pub fn report_harness_yolo(&mut self, who: &Identity, on: bool) {
        if on {
            self.harness_yolo.insert(who.key());
        } else {
            self.harness_yolo.remove(&who.key());
        }
    }

    /// `who`'s harness reported its own YOLO at its latest call (R2.6).
    #[must_use]
    pub fn harness_yolo(&self, who: &Identity) -> bool {
        self.harness_yolo.contains(&who.key())
    }

    /// The rate limit in force (R1.7).
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.conversations.limits()
    }

    /// Every HALTED pair, by its two identity keys, and why (C1.1).
    #[must_use]
    pub fn halted_pairs(&self) -> Vec<(&str, &str, Reason)> {
        self.conversations.halted_pairs()
    }

    /// Sets the rate limit (R1.7). Only the `lunchroom rate` command calls this, after reading the
    /// user's confirmation from a terminal no guarded session owns (A1.3); an agent reaches
    /// Lunchroom only through commands, and the guard DENIES its `lunchroom rate` (A1.3, node `§7::11`).
    pub const fn set_limits(&mut self, limits: Limits) {
        self.conversations.set_limits(limits);
    }

    /// Gives `name` to `who`, who from then on receives messages (I1.5). A taken name is never
    /// moved silently: the first attempt is refused with [`RegisterRefusal::Taken`], which names
    /// the holder for the user to see, and only a second attempt that passes back that same holder
    /// as `replacing` moves the name. The messages QUEUED for the moved name never reach the new
    /// holder (I1.5); they become FAILED at the next check, and their senders are told.
    ///
    /// # Errors
    /// [`RegisterRefusal`] when it comes from a guarded session, or the name is taken and
    /// `replacing` does not name its present holder.
    pub fn register(
        &mut self,
        name: &str,
        who: &Identity,
        origin: Origin,
        replacing: Option<&Identity>,
    ) -> Result<Option<Identity>, RegisterRefusal> {
        if origin == Origin::GuardedSession {
            return Err(RegisterRefusal::FromGuardedSession);
        }
        if !valid_name(name) {
            return Err(RegisterRefusal::InvalidName);
        }
        let earlier = self.names.get(name).filter(|h| *h != who).cloned();
        if let Some(holder) = &earlier
            && replacing != Some(holder)
        {
            return Err(RegisterRefusal::Taken {
                holder: holder.clone(),
            });
        }
        self.names.insert(name.to_owned(), who.clone());
        // The name moved: the earlier holder receives no more unless another name still labels it,
        // and the messages QUEUED for the moved name are FAILED at the next check
        // ([`Mail::fail_unnamed`]); none of them reaches the earlier holder meanwhile (D1.7). A
        // HELD session's reply to that holder is then refused as to an agent no longer named; it
        // never goes to the new holder of the name (§9 item 5 keeps that part).
        if let Some(old) = &earlier
            && !self.names.values().any(|h| h == old)
        {
            self.receivers.remove(&old.key());
        }
        self.receivers.insert(who.key());
        Ok(earlier)
    }

    /// Takes `name` away, so its session receives no more messages unless another name still labels
    /// it. The messages QUEUED for it become FAILED, and their senders are told, at the next check
    /// ([`Mail::fail_unnamed`]); a caller that wants that at once calls it in the same update.
    /// False when no session has that name. Called by the `lunchroom unregister` command after the
    /// user's confirmation (A1.3), and by `join` and `leave`, which the user typed in the session
    /// (I1.5).
    pub fn unregister(&mut self, name: &str) -> bool {
        let Some(who) = self.names.remove(name) else {
            return false;
        };
        if !self.names.values().any(|h| *h == who) {
            self.receivers.remove(&who.key());
        }
        true
    }

    /// Records the title Lunchroom gave `who` (I1.5).
    pub fn record_title(&mut self, who: &Identity, title: &str) {
        self.titles.insert(who.key(), title.to_owned());
    }

    /// The title Lunchroom last gave `who` (I1.5).
    #[must_use]
    pub fn title_given(&self, who: &Identity) -> Option<&str> {
        self.titles.get(&who.key()).map(String::as_str)
    }

    /// Forgets the title Lunchroom gave `who`: a custom title has taken its place (I1.5).
    pub fn forget_title(&mut self, who: &Identity) {
        self.titles.remove(&who.key());
    }

    /// The identity a name labels now.
    #[must_use]
    pub fn named(&self, name: &str) -> Option<&Identity> {
        self.names.get(name)
    }

    /// A name that labels `who` now, the first in name order (a display label only, I1.1).
    #[must_use]
    pub fn name_of(&self, who: &Identity) -> Option<&str> {
        self.names
            .iter()
            .find(|(_, h)| *h == who)
            .map(|(n, _)| n.as_str())
    }

    /// Every name and the identity it labels, in name order.
    pub fn names(&self) -> impl Iterator<Item = (&str, &Identity)> {
        self.names.iter().map(|(n, h)| (n.as_str(), h))
    }

    /// Accepts a message from `from` to the session named `to_name`, QUEUED, or says why not.
    ///
    /// # Errors
    /// [`SendRefusal`], for the first rule that stops it.
    #[allow(clippy::too_many_lines)] // one send: the rules in order, the record, the lines it tells
    pub fn send(
        &mut self,
        from: &Identity,
        from_name: &str,
        to_name: &str,
        text: &str,
        now: u64,
    ) -> Result<Accepted, SendRefusal> {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(SendRefusal::TooLong);
        }
        self.expire_handovers(now);
        if !self.seen.contains(&from.key()) {
            return Err(SendRefusal::UnseenSender);
        }
        let to = if self.is_held(from) {
            self.holder_named(from, to_name)?
        } else {
            self.names
                .get(to_name)
                .cloned()
                .ok_or(SendRefusal::UnknownName)?
        };
        if !self.receivers.contains(&to.key()) {
            return Err(SendRefusal::NotNamed);
        }
        if asks_for_secret(text) {
            // L1.4: DENIED, and both agents are told; the recipient learns who asked, never the
            // message itself (card 2), once until it has read the notice. §16 item 8: the line
            // names the two agents ("<sender> to <recipient>") and carries no text of the message;
            // the recipient hears it by D and N, the user by U; the sender hears it by S.
            let _ = self.tell_line(
                now,
                &crate::cards::asked_for_secret(),
                lines::Who::Message {
                    from: from_name,
                    to: to_name,
                },
                &[from.clone(), to.clone()],
                &[],
                &[calls::Hearer {
                    who: to.clone(),
                    d: true,
                    n: true,
                }],
                true,
            );
            return Err(SendRefusal::Denied(Reason::Leak));
        }
        if self.is_held(from) && has_secret(text) {
            return Err(SendRefusal::Denied(Reason::Leak));
        }
        let halted = match self
            .conversations
            .on_message(&from.key(), &to.key(), text, now)
        {
            Outcome::Deny(reason) => return Err(SendRefusal::Denied(reason)),
            Outcome::DeliverAndHalt => true,
            Outcome::Deliver => false,
        };
        self.next_message += 1;
        let id = format!("lr-{:08}", self.next_message);
        let rec = self.next_rec();
        self.messages.insert(
            id.clone(),
            Message {
                id: id.clone(),
                from: from.clone(),
                from_name: from_name.to_owned(),
                to: to.clone(),
                to_name: to_name.to_owned(),
                text: text.to_owned(),
                sent_at: now,
                status: Status::Queued,
                failed_attempts: 0,
                presented_at: None,
                handed_over: false,
                rec,
                told: Told::default(),
                extra: BTreeMap::new(),
            },
        );
        self.inboxes
            .entry(to.harness)
            .or_default()
            .entry(to.session.clone())
            .or_default()
            .push(id.clone());
        // §16 item 12: the eighth send halts the pair: the sender hears the line by S (printed
        // from this text), the other agent by D and N, the user by U. A message the pair
        // exchanged is "A and B": the agents of the event are the two of the pair.
        let halted_line = halted.then(|| {
            self.tell_line(
                now,
                &crate::cards::loop_halted(),
                lines::Who::Agents(&[from_name, to_name]),
                &[from.clone(), to.clone()],
                &[],
                &[calls::Hearer {
                    who: to.clone(),
                    d: true,
                    n: true,
                }],
                true,
            )
            .text
        });
        Ok(Accepted {
            id,
            from: from.clone(),
            to,
            halted,
            halted_line,
        })
    }

    /// The identity a HELD session's reply to `to_name` goes to: the sender, as recorded, of the
    /// latest message holding it that came under that name. The name table as it is now plays no
    /// part (I1.4).
    fn holder_named(&self, held: &Identity, to_name: &str) -> Result<Identity, SendRefusal> {
        let record = self
            .holding(held)
            .iter()
            .rev()
            .find(|h| h.from_name == to_name)
            .ok_or(SendRefusal::NotHolder)?;
        record.from.clone().ok_or(SendRefusal::NameOnlyHolder)
    }

    /// The messages QUEUED for `to`, oldest first (D1.1).
    #[must_use]
    pub fn deliveries(&self, to: &Identity) -> Vec<&Message> {
        self.inboxes
            .get(&to.harness)
            .and_then(|by_session| by_session.get(&to.session))
            .into_iter()
            .flatten()
            .filter_map(|id| self.messages.get(id))
            .filter(|m| m.status == Status::Queued)
            .collect()
    }

    /// The recipient's harness confirms the message reached `to` (Claude Code: the delivery prompt
    /// reached the guard). The first confirmation makes it DELIVERED and adds it to what holds the
    /// session (D1.5); any later one is a repeated attempt, not shown again (D1.2). A message
    /// already FAILED stays FAILED, as its sender was told (D1.4): a late copy is not let in.
    ///
    /// What this core guarantees for D1.2: called inside one [`crate::store::Store::update`],
    /// exactly one arrival of a message ever gets [`Confirmation::Show`] from a completed update,
    /// across crashes and restarts; an update that crashes leaves the message QUEUED and gives
    /// nobody `Show`. What the harness adapter adds (the Claude Code guard, a later increment): it
    /// lets a copy into the session only on `Show` from a completed update and blocks it on any
    /// error, and it establishes what its harness does with a copy when the guard process itself is
    /// killed. Displaying is the harness's; this core never displays.
    pub fn confirm(&mut self, to: &Identity, id: &str) -> Confirmation {
        self.confirm_at(to, id, self.seen_at)
    }

    /// [`Mail::confirm`] at the time `now`, which the DELIVERED notice counts the message's age
    /// to. A caller with a clock passes it: [`Mail::confirm`] has only the latest time any check
    /// ran at, which can be long before the confirmation.
    pub fn confirm_at(&mut self, to: &Identity, id: &str, now: u64) -> Confirmation {
        self.saw(now);
        let now = self.seen_at;
        let Some(message) = self.messages.get_mut(id).filter(|m| m.to == *to) else {
            return Confirmation::NotFound;
        };
        match message.status {
            Status::Delivered => return Confirmation::Duplicate,
            Status::Failed { .. } => return Confirmation::Failed,
            Status::Queued => {}
        }
        message.status = Status::Delivered;
        message.presented_at = None;
        message.told.blocked = None;
        message.told.unreachable = None;
        let hold = Hold {
            message: id.to_owned(),
            from: Some(message.from.clone()),
            from_name: message.from_name.clone(),
        };
        // D1.8: a message the user and the sender were told of while it waited is told DELIVERED.
        let told = message.told.any.then(|| Self::delivered_text(message, now));
        let holds = self.holds.entry(to.key()).or_default();
        // §16 item 5: the hold begins with the first message that holds the session.
        let begins = holds.is_empty() || (holds.len() == 1 && holds[0].message == id);
        // (Held from the hand-over already, D1.5: the line was told then, if it was.)
        if !holds.iter().any(|h| h.message == id) {
            holds.push(hold);
        }
        if let Some(text) = told {
            self.tell(id, &text);
        }
        if begins && !self.held_lines.contains_key(id) {
            self.hold_begins(id, now);
        }
        self.envelope_confirmed(to, id);
        Confirmation::Show
    }

    /// [`Mail::handed_over`] for a Codex session, keeping `mark`, the plain session record as it
    /// stood at the hand-over (D1.1): the search for the message's id starts there.
    pub fn handed_over_at(&mut self, id: &str, mark: RecordMark) {
        if self
            .messages
            .get(id)
            .is_some_and(|m| m.status == Status::Queued)
        {
            // The earliest mark stands: an earlier hand-over nobody confirmed may have reached the
            // record, and a search from a later mark would miss it (D1.2).
            self.handed_at.entry(id.to_owned()).or_insert(mark);
        }
        self.handed_over(id);
    }

    /// The record mark kept when message `id` was handed over; `None` for a hand-over that kept
    /// none (a message handed over by 1.0.1, or to a harness with no record).
    #[must_use]
    pub fn handed_at(&self, id: &str) -> Option<RecordMark> {
        self.handed_at.get(id).copied()
    }

    /// Ids of `to`'s QUEUED messages already handed to its harness, awaiting confirmation.
    #[must_use]
    pub fn handed_over_ids(&self, to: &Identity) -> Vec<String> {
        self.deliveries(to)
            .iter()
            .filter(|m| m.handed_over)
            .map(|m| m.id.clone())
            .collect()
    }

    /// The QUEUED message `id` is being handed to the recipient's harness (Codex: `codex queue`):
    /// it is marked before the hand-over, so a crash after it can never lead to a second
    /// hand-over; it stays QUEUED until confirmed (D1.2), and a failed attempt clears the mark.
    pub fn handed_over(&mut self, id: &str) {
        if let Some(m) = self
            .messages
            .get_mut(id)
            .filter(|m| m.status == Status::Queued)
        {
            m.handed_over = true;
        }
    }

    /// Holds `to` by the QUEUED message `id` before it is handed to a harness that cannot hold
    /// the session when the message arrives (Codex's `codex queue`): the session is HELD from that
    /// moment, failing safe, and may reply to the sender (I1.4). Nothing for another recipient's
    /// message or one that is not QUEUED.
    pub fn hold_before_delivery(&mut self, to: &Identity, id: &str) {
        let Some(message) = self
            .messages
            .get(id)
            .filter(|m| m.to == *to && m.status == Status::Queued)
        else {
            return;
        };
        let hold = Hold {
            message: id.to_owned(),
            from: Some(message.from.clone()),
            from_name: message.from_name.clone(),
        };
        let holds = self.holds.entry(to.key()).or_default();
        if !holds.iter().any(|h| h.message == id) {
            holds.push(hold);
        }
        // The line "<agent> is HELD." is told once the hand-over has gone ahead
        // ([`Mail::hand_over_held`]): a hand-over that fails releases this hold, and a line told
        // before that would have been false (D1.5).
    }

    /// The hand-over of `id` went ahead (the harness's program took it, or gave no answer, D1.3):
    /// the recipient is HELD from this moment, and when this message began the hold the line
    /// "<agent> is HELD." is told (§16 item 5, D1.5). Nothing for a message that is not holding
    /// its recipient, one whose recipient was HELD already, or one already told.
    pub fn hand_over_held(&mut self, id: &str, now: u64) {
        let Some(message) = self.messages.get(id) else {
            return;
        };
        let began = self
            .holds
            .get(&message.to.key())
            .is_some_and(|h| h.len() == 1 && h[0].message == id);
        if began && !self.held_lines.contains_key(id) {
            self.hold_begins(id, now);
        }
    }

    /// The outcome of an attempt to deliver `id` cannot be known (its confirmation was lost): the
    /// message stays QUEUED, the attempt is not counted as failed, and Lunchroom says
    /// "Not confirmed; still QUEUED" (D1.3). `None` when the message is not QUEUED.
    #[must_use]
    pub fn attempt_unknown(&self, id: &str) -> Option<String> {
        self.messages
            .get(id)
            .filter(|m| m.status == Status::Queued)
            .map(|m| format!("Not confirmed; still {}", m.status.state().word()))
    }

    /// Every QUEUED message put in front of its recipient's harness and still unconfirmed counts
    /// one failed attempt (D1.4): a message handed over (Codex, OpenClaw, Hermes), [`HANDOVER_SECS`]
    /// later; one only offered (Claude Code), [`CONFIRM_SECS`] later, even when no listener ever
    /// asks for it again. The mark goes and the message is offered again, or is FAILED on its
    /// third. The hold taken before a hand-over stays, since the harness may have shown the message
    /// without confirming it; the user typing clears it. A hand-over not stamped at all, or stamped
    /// more than [`FUTURE_SLACK_SECS`] ahead of `now` (the clock stepped back; two processes read
    /// the clock at different moments, so a smaller difference is not that), has run out. This needs
    /// no live recipient. A late confirmation of the old hand-over still shows the message once,
    /// and a repeat of it is a duplicate (D1.2).
    pub fn expire_handovers(&mut self, now: u64) {
        self.saw(now);
        let expired: Vec<String> = self
            .messages
            .values()
            .filter(|m| m.status == Status::Queued)
            .filter(|m| match (m.presented_at, m.handed_over) {
                (None, handed) => handed,
                (Some(at), handed) => {
                    let limit = if handed { HANDOVER_SECS } else { CONFIRM_SECS };
                    at > now.saturating_add(FUTURE_SLACK_SECS) || now.saturating_sub(at) >= limit
                }
            })
            .map(|m| m.id.clone())
            .collect();
        for id in expired {
            self.count_failed(
                &id,
                "no confirmation from the recipient's harness",
                false,
                now,
                false,
            );
        }
    }

    /// The message to put in front of `to`'s harness now, if any: one attempt (D1.1), oldest first,
    /// one at a time. A message already in front of the harness waits up to [`CONFIRM_SECS`] for
    /// its confirmation; past that, that attempt counts as failed (D1.4) and the message is offered
    /// again, unless that was its third failed attempt, which makes it FAILED.
    ///
    /// Offering is not showing (D1.2). A harness adapter shows a message only through its guard
    /// confirming it ([`Mail::confirm`]); in Claude Code an offer wakes the session with a prompt
    /// that passes through the guard's `UserPromptSubmit` hook before the model sees it
    /// (`docs/host-facts/claude-code.md` section 3). So an earlier offer either never
    /// reached the guard, and nothing was shown; or reached it and was confirmed, and the message is
    /// no longer QUEUED, so it is not offered again and any later copy is blocked as a repeat; or
    /// reached it while the guard failed, and the prompt was blocked.
    pub fn present(&mut self, to: &Identity, now: u64) -> Option<Message> {
        self.present_from(to, None, now)
    }

    /// [`Mail::present`] for a hand-over that runs in one sender's process (`lunchroom send`): with
    /// `only_from`, the message offered is that sender's own. Another sender's waiting message is
    /// passed over and left as it was, so a sender's process is never given another sender's text;
    /// that message goes by its own sender's send or the listener (D1.1).
    pub fn present_from(
        &mut self,
        to: &Identity,
        only_from: Option<&Identity>,
        now: u64,
    ) -> Option<Message> {
        self.present_matching(to, only_from, false, now)
    }

    /// D1.7: [`Mail::present`] for a check, which offers again at once, whatever session it runs
    /// in, a message whose hand-over counted as failed (not sooner than [`RETRY_GAP_SECS`] after a
    /// hand-over that failed at once), and a message that has waited [`RETRY_GAP_SECS`] and was
    /// never handed over because its recipient could not take it then.
    pub fn present_again(&mut self, to: &Identity, now: u64) -> Option<Message> {
        self.present_again_from(to, None, now)
    }

    /// [`Mail::present_again`] for a check that runs in one sender's process (the check inside
    /// `lunchroom send`): with `only_from`, the message offered is that sender's own, and another
    /// sender's waiting message is passed over and left as it was (D1.1).
    pub fn present_again_from(
        &mut self,
        to: &Identity,
        only_from: Option<&Identity>,
        now: u64,
    ) -> Option<Message> {
        self.present_matching(to, only_from, true, now)
    }

    /// [`Mail::present_from`], with `again`: only messages with a failed attempt, or never handed
    /// over and waiting [`RETRY_GAP_SECS`] or more.
    fn present_matching(
        &mut self,
        to: &Identity,
        only_from: Option<&Identity>,
        again: bool,
        now: u64,
    ) -> Option<Message> {
        // I1.5: only a session the user named receives anything.
        if !self.receivers.contains(&to.key()) {
            return None;
        }
        self.expire_handovers(now);
        let ids: Vec<String> = self
            .deliveries(to)
            .iter()
            .filter(|m| {
                !m.handed_over
                    && (!again
                        || m.failed_attempts > 0
                        || now.saturating_sub(m.sent_at) >= RETRY_GAP_SECS)
            })
            .map(|m| m.id.clone())
            .collect();
        for id in ids {
            let (presented, retry_after) = self
                .messages
                .get(&id)
                .map_or((None, None), |m| (m.presented_at, m.told.retry_after));
            // One attempt at a time, whoever's message it is. What was in front of the harness
            // longer than its time has been counted failed above, so what is left is within it.
            if presented.is_some() {
                return None;
            }
            // D1.1: another sender's message waits for its own sender or the recipient's listener,
            // and this sender's send counts nothing against it.
            if only_from.is_some_and(|f| self.messages.get(&id).is_some_and(|m| m.from != *f)) {
                continue;
            }
            // A failed attempt is not tried again within [`RETRY_GAP_SECS`]. A gap that stands
            // further ahead than that was stamped by a clock that stepped back and is void.
            if retry_after.is_some_and(|t| now < t && t <= now.saturating_add(RETRY_GAP_SECS)) {
                return None;
            }
            let message = self.messages.get_mut(&id)?;
            message.presented_at = Some(now);
            return Some(message.clone());
        }
        None
    }

    /// The attempt on QUEUED message `id` is over without having reached the session: the message
    /// may be offered again at once, and the hold taken before handing it over goes (D1.1). It is
    /// not counted as failed here. `false` when the message is not QUEUED.
    pub fn hand_over_undone(&mut self, id: &str) -> bool {
        self.undo_hand_over(id, true)
    }

    /// [`Mail::hand_over_undone`], keeping the hold unless `release`.
    fn undo_hand_over(&mut self, id: &str, release: bool) -> bool {
        let Some(message) = self
            .messages
            .get_mut(id)
            .filter(|m| m.status == Status::Queued)
        else {
            return false;
        };
        message.presented_at = None;
        message.handed_over = false;
        if release {
            // Nothing reached the session: the record is looked at afresh at the next hand-over.
            self.handed_at.remove(id);
            if let Some(holds) = self.holds.get_mut(&message.to.key()) {
                holds.retain(|h| h.message != id);
            }
            self.end_declines();
        }
        true
    }

    /// One delivery attempt failed for `reason` at `now`. The third makes the message FAILED (D1.4);
    /// each tells the user and the sender (D1.8). The message is not offered again for
    /// [`RETRY_GAP_SECS`]. `None` when the message is not QUEUED.
    pub fn attempt_failed(&mut self, id: &str, reason: &str, now: u64) -> Option<AttemptOutcome> {
        self.count_failed(id, reason, true, now, true)
    }

    /// [`Mail::attempt_failed`], keeping the hold taken for the attempt unless `release`, and with
    /// `gap` the pause before the next offer.
    fn count_failed(
        &mut self,
        id: &str,
        reason: &str,
        release: bool,
        now: u64,
        gap: bool,
    ) -> Option<AttemptOutcome> {
        if !self.undo_hand_over(id, release) {
            return None;
        }
        let message = self.messages.get_mut(id)?;
        message.failed_attempts += 1;
        message.told.last_failure = Some(reason.to_owned());
        message.told.retry_after = gap.then(|| now.saturating_add(RETRY_GAP_SECS));
        let outcome = if message.failed_attempts < MAX_ATTEMPTS {
            AttemptOutcome::StillQueued {
                failed_attempts: message.failed_attempts,
            }
        } else {
            message.status = Status::Failed {
                reason: reason.to_owned(),
            };
            AttemptOutcome::Failed
        };
        let text = Self::attempt_text(message, reason, now);
        self.tell(id, &text);
        Some(outcome)
    }

    /// One message by id.
    #[must_use]
    pub fn message(&self, id: &str) -> Option<&Message> {
        self.messages.get(id)
    }

    /// What `who` has been told about its messages (D1.4).
    #[must_use]
    pub fn notices(&self, who: &Identity) -> &[Notice] {
        self.notices.get(&who.key()).map_or(&[], Vec::as_slice)
    }

    /// Takes the notices waiting for `who`, to tell it now (D1.4, L1.4); each is told once.
    pub fn take_notices(&mut self, who: &Identity) -> Vec<Notice> {
        self.notices.remove(&who.key()).unwrap_or_default()
    }

    /// `told` were shown to `who`: they are not told again. Used after the output that carries
    /// them is written, so a hook killed before that loses none (a notice leaves the store only
    /// once it has been shown). A notice that arrived meanwhile stays.
    pub fn mark_notices_told(&mut self, who: &Identity, told: &[Notice]) {
        let key = who.key();
        if let Some(list) = self.notices.get_mut(&key) {
            drop_told(list, told);
            if list.is_empty() {
                self.notices.remove(&key);
            }
        }
    }

    /// `told` were shown to the user: they are not told again (see [`Mail::mark_notices_told`]).
    pub fn mark_user_notices_told(&mut self, told: &[Notice]) {
        drop_told(&mut self.user_notices, told);
    }

    /// How many calls wait for the user's approval, whoever asked (named or not): what `lunchroom
    /// approve` lists.
    #[must_use]
    pub fn requests_total(&self) -> usize {
        self.requests.len()
    }

    /// The [`SCHEMA`] of the program that last wrote this store; 0 for a store from before 1.1.0.
    #[must_use]
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// An older program wrote this store last: its fields may have been dropped, so what this
    /// program finds missing is not proof of anything.
    #[must_use]
    pub const fn written_by_older_program(&self) -> bool {
        self.schema < SCHEMA
    }

    /// A newer program wrote this store: this program keeps what it does not know ([`Mail`] carries
    /// unknown fields through), but a caller that must not guess may refuse to write.
    #[must_use]
    pub const fn written_by_newer_program(&self) -> bool {
        self.schema > SCHEMA
    }

    /// The messages that hold `who` (D1.5).
    #[must_use]
    pub fn holding(&self, who: &Identity) -> &[Hold] {
        self.holds.get(&who.key()).map_or(&[], Vec::as_slice)
    }

    /// `who` is HELD: some message holds it.
    #[must_use]
    pub fn is_held(&self, who: &Identity) -> bool {
        !self.holding(who).is_empty()
    }

    /// The user typed in `who`'s session: nothing holds it any more, and its pairs are cleared of
    /// HALTED (R1.5). The caller passes only a prompt the guard accepts as the user's.
    pub fn user_typed(&mut self, who: &Identity) {
        self.holds.remove(&who.key());
        // §17.10: a decline stands only while its hold does.
        self.end_declines();
        self.conversations.user_typed(&who.key());
        self.transcript_typed(who);
    }

    /// Another agent's text reached `who` by a way Lunchroom did not deliver (a harness's own
    /// cross-session message, a Lunchroom label in a tool's output): it holds the session like any
    /// delivered message (T1, D1.5). No sender identity is recorded, so it permits no reply (I1.4).
    /// `source` names where it came from and `mark` tells one arrival from another.
    pub fn hold_unrecorded(&mut self, who: &Identity, source: &str, mark: &str) {
        let message = format!("{source}:{mark}");
        let holds = self.holds.entry(who.key()).or_default();
        if !holds.iter().any(|h| h.message == message) {
            holds.push(Hold {
                message,
                from: None,
                from_name: source.to_owned(),
            });
        }
    }

    /// F2-AC54, F2-AC57: discards the QUEUED or FAILED message `id`: it leaves `messages` and its
    /// recipient's inbox, every hold that names it ends, and its sender is told (D1.4).
    ///
    /// # Errors
    ///
    /// [`Refused::Delivered`] for a DELIVERED message, [`Refused::Unknown`] for an id the store
    /// does not hold; nothing is changed.
    pub fn discard_message(&mut self, id: &str, words: &str, now: u64) -> Result<Message, Refused> {
        let Some(found) = self.messages.get(id) else {
            return Err(Refused::Unknown);
        };
        if found.status == Status::Delivered {
            return Err(Refused::Delivered);
        }
        let Some(gone) = self.messages.remove(id) else {
            return Err(Refused::Unknown);
        };
        if let Some(list) = self
            .inboxes
            .get_mut(&gone.to.harness)
            .and_then(|by_session| by_session.get_mut(&gone.to.session))
        {
            list.retain(|m| m != id);
        }
        for holds in self.holds.values_mut() {
            holds.retain(|h| h.message != id);
        }
        self.holds.retain(|_, holds| !holds.is_empty());
        push_notice(
            self.notices.entry(gone.from.key()).or_default(),
            Notice {
                message: id.to_owned(),
                text: words.to_owned(),
                at: now,
                reason: false,
            },
            true,
        );
        Ok(gone)
    }

    /// F2-AC55: holds `who` as HELD by the user. The hold names no sender, so it permits no reply
    /// (I1.6), and it ends as every hold does: the user typing in that session or releasing it.
    pub fn hold_by_user(&mut self, who: &Identity, now: u64) {
        let message = format!("user:{now}");
        let holds = self.holds.entry(who.key()).or_default();
        if !holds.iter().any(|h| h.message == message) {
            holds.push(Hold {
                message,
                from: None,
                from_name: "the user".to_owned(),
            });
        }
    }

    /// F2-AC55: the user dismissed the PROBING alert of session `session` (the session id its
    /// DENIED records carry) at `now`.
    pub fn dismiss_probe(&mut self, session: &str, now: u64) {
        self.probe_dismissed.insert(session.to_owned(), now);
    }

    /// When the user last dismissed the PROBING alert of session `session`.
    #[must_use]
    pub fn probe_dismissed_at(&self, session: &str) -> Option<u64> {
        self.probe_dismissed.get(session).copied()
    }

    /// F2-AC57: the holds of `who` whose message the store no longer holds. A hold the user made
    /// (`user:<n>`) or one that came another way (`<source>:<mark>`) names no message and is never
    /// stale.
    #[must_use]
    pub fn stale_holds(&self, who: &Identity) -> Vec<&Hold> {
        self.holding(who)
            .iter()
            .filter(|h| !h.message.contains(':') && !self.messages.contains_key(&h.message))
            .collect()
    }

    /// F2-AC57: ends the stale holds of `who`; returns how many ended.
    pub fn release_stale(&mut self, who: &Identity) -> usize {
        let stale: Vec<String> = self
            .stale_holds(who)
            .iter()
            .map(|h| h.message.clone())
            .collect();
        if let Some(holds) = self.holds.get_mut(&who.key()) {
            holds.retain(|h| !stale.contains(&h.message));
            if holds.is_empty() {
                self.holds.remove(&who.key());
            }
        }
        stale.len()
    }

    /// F2-AC57: the QUEUED or FAILED messages whose recipient has no name in the store.
    #[must_use]
    pub fn unnamed_messages(&self) -> Vec<&Message> {
        self.messages
            .values()
            .filter(|m| m.status != Status::Delivered)
            .filter(|m| !self.names.values().any(|who| *who == m.to))
            .collect()
    }
}

/// Why the store would not discard a message (F2-AC54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The store holds no message of that id.
    Unknown,
    /// The message was DELIVERED: it reached its session and cannot be taken back.
    Delivered,
}

pub mod calls;
mod carry;
pub mod lines;
mod reading;
mod side;
mod waiting;
pub use crate::approvals::{Refusal, Tell, TellKind};
pub use carry::Read as TranscriptSearch;
pub use waiting::{cannot_hand_over, clock, span, standing, waiting_for};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_102;

#[cfg(test)]
mod tests_f1_sweep;

#[cfg(test)]
mod tests_f1_notices;

#[cfg(test)]
mod tests_u5;

#[cfg(test)]
mod tests_u6;

#[cfg(test)]
mod tests_enum;

#[cfg(test)]
mod tests_c1;

#[cfg(test)]
mod tests_c1_codex;
