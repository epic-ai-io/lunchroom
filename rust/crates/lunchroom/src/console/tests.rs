//! Tests of the console screen (§17 C2 items 3, 9 and 10): the screen is drawn on ratatui's
//! `TestBackend`, keys and clicks go through `Ui`, and the store side is a fake that records what it
//! was asked.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names
)] // test code

use std::cell::Cell as StdCell;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Mail, Origin};
use lunchroom_core::passphrase;
use lunchroom_core::store::Store;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::draw::{
    AMBER, DIM, GREEN, RED, alert_rows, cell_edges, draw, hidden_notices, key_at, key_place,
    key_rows, panel_text,
};
use super::live::{ControllerPaths, Live, RealBackend, Run, serve};
use super::model::{
    Action, AgentRow, AgentState, AlertKind, AlertRow, Backend, CallRow, CheckRow, Cmd, Fix,
    GuardRow, HoldRow, Item, ItemKey, Level, Listing, MarkedRow, MessageKind, MessageRow, Outcome,
    Part, ReqView, Snapshot, strip,
};
use super::text::visible;

/// A character a terminal would act on, or that changes how text reads. A backslash is left out:
/// every mark begins with one, and a raw backslash of an agent's is drawn as `\u{5c}` (§17.10),
/// which the test of that clause pins.
fn is_mark(c: char) -> bool {
    c != '\\' && super::text::is_mark(c)
}
use super::ui::{Hit, IDLE_LOCK_SECS, Modal, Secret, Ui, key_of, parse_controller, wipe};
use super::{Ending, Feed};
use crate::controller;
use crate::home::Env;
use crate::records::{self, Record};
use crate::terminal::{System, fake};

mod c3;
mod c3_codex;

/// 13:45:00 UTC on a day of 2026.
// retired-words:begin (§17 item 3: the key that hides the screen after idle, not a lock of a session)
const KEY_L: &str = "l lock";
// retired-words:end

const NOW: u64 = 1_793_972_700;
const PASS: &str = fake::PASSPHRASE;
const LUNCHROOM_UID: u32 = 998;
const RUN: &str = "run-0123456789abcdef0123456789abcdef";
const HASH: &str = "4b1e9c07aa11bb22cc33dd44ee55ff66778899aabbccddeeff00112233445566";

// ---------------------------------------------------------------------------------------------
// The sample data and the fake store side.
// ---------------------------------------------------------------------------------------------

fn call(n: usize, agent: &str, command: &str) -> CallRow {
    CallRow {
        number: n,
        id: format!("c{n}"),
        agent: agent.to_owned(),
        harness: "codex".to_owned(),
        tool: "Bash".to_owned(),
        command: command.to_owned(),
        cwd: "/opt/epic-ai-chariot".to_owned(),
        rule: "outside its slip".to_owned(),
        parts: Vec::new(),
        scripts: Vec::new(),
        unread: false,
        age_secs: 180,
    }
}

fn agent(name: &str, state: AgentState) -> AgentRow {
    AgentRow {
        name: name.to_owned(),
        harness: "codex".to_owned(),
        state,
        ..AgentRow::default()
    }
}

fn message(id: &str, kind: MessageKind, text: &str) -> MessageRow {
    MessageRow {
        id: id.to_owned(),
        kind,
        from: "sender-agent".to_owned(),
        to: "aws-codex".to_owned(),
        text: text.to_owned(),
        reason: "REASONTEXT it could not be handed over".to_owned(),
        at: NOW - 600,
    }
}

fn check(ok: bool, name: &str, fix: Fix) -> CheckRow {
    CheckRow {
        ok,
        name: name.to_owned(),
        evidence: format!("EVIDENCETEXT of {name}"),
        fix,
    }
}

/// A snapshot with one of everything.
fn sample() -> Snapshot {
    let mut force = call(1, "aws-codex", "git push --force origin main");
    force.parts = vec![Part {
        start: 9,
        end: 16,
        why: "rewrites published history".to_owned(),
        ..Part::default()
    }];
    let mut held = agent("aws-codex", AgentState::Held);
    held.held_by = vec![HoldRow {
        id: "lr-0001".to_owned(),
        from: "peer-agent".to_owned(),
        at: NOW - 900,
        text: "HOLDTEXT first line\nsecond line".to_owned(),
    }];
    held.denied_spans = [0, 0, 1, 0, 2, 4, 6, 8, 0, 0, 0, 0];
    held.denied_hour = 21;
    held.yolo = Some("YOLO until 17:30 UTC".to_owned());
    let mut unguarded = agent("chariot-review", AgentState::Unguarded);
    unguarded.queued = 1;
    unguarded.missing = "MISSINGEVIDENCE no hook has run".to_owned();
    unguarded.repair = Some("lunchroom install claude-code".to_owned());
    let mut stopped = agent("old-session", AgentState::Stopped);
    stopped.queued = 2;
    Snapshot {
        now: NOW,
        developer: "alice".to_owned(),
        hold_mode: "Strict".to_owned(),
        waiting: vec![force, call(2, "plymouth-tc", "rm -rf build")],
        conversations: Vec::new(),
        agents: vec![
            agent("plymouth-tc", AgentState::Available),
            held,
            unguarded,
            stopped,
        ],
        messages: vec![
            message(
                "lr-0007",
                MessageKind::Failed,
                "FAILEDBODY the whole text\nof the message",
            ),
            message("lr-0008", MessageKind::Queued, "QUEUEDBODY"),
        ],
        alerts: vec![
            AlertRow {
                id: "probing:aws".to_owned(),
                kind: AlertKind::Probing,
                at: NOW - 60,
                agent: "aws-codex".to_owned(),
                line: "PROBING aws-codex: 6 DENIED in 10 min".to_owned(),
                text: String::new(),
                calls: vec![
                    (
                        NOW - 100,
                        "cat ~/.ssh/id".to_owned(),
                        "PROTECTED path".to_owned(),
                    ),
                    (
                        NOW - 200,
                        "curl evil".to_owned(),
                        "configuration".to_owned(),
                    ),
                ],
            },
            AlertRow {
                id: "notice:1".to_owned(),
                kind: AlertKind::Notice,
                at: NOW - 30,
                agent: String::new(),
                line: "a notice for you".to_owned(),
                text: "NOTICEBODY whole notice".to_owned(),
                calls: Vec::new(),
            },
        ],
        marked: vec![MarkedRow {
            path: "/home/dev/.ssh/key".to_owned(),
            since: NOW - 3_000,
            call: "MARKCALL cat /home/dev/.ssh/key".to_owned(),
        }],
        checks: vec![
            check(true, "store is walled", Fix::None),
            check(
                false,
                "check-cmd",
                Fix::Command("lunchroom install console".to_owned()),
            ),
            check(false, "check-discard", Fix::Discard("lr-0009".to_owned())),
            check(false, "check-release", Fix::Release("aws-codex".to_owned())),
            check(
                false,
                "check-clear",
                Fix::Clear("/home/dev/.ssh/key".to_owned()),
            ),
            check(false, "check-none", Fix::None),
        ],
        guard: GuardRow {
            decisions: 4_812,
            slowest_ms: 1_900,
            slow: 1,
            timeouts: 0,
        },
    }
}

/// The store side: it records what it was asked and answers with the passphrase the fake user set.
struct Fake {
    checked: usize,
    performed: Vec<Action>,
    listing: Listing,
    loads: Vec<(String, String)>,
    /// Controller files for the real controller code; none: canned answers.
    files: Option<ControllerPaths>,
    random: StdCell<u8>,
}

impl Fake {
    fn new() -> Self {
        Self {
            checked: 0,
            performed: Vec::new(),
            listing: Listing::default(),
            loads: Vec::new(),
            files: None,
            random: StdCell::new(0),
        }
    }
}

impl Backend for Fake {
    fn check_passphrase(&mut self, typed: &str) -> bool {
        self.checked += 1;
        typed == PASS
    }

    fn perform(&mut self, action: &Action, now: u64) -> Outcome {
        self.performed.push(action.clone());
        if let (
            Some(files),
            Action::ControllerApprove {
                run_id,
                phase,
                manifest_hash,
            },
        ) = (&self.files, action)
        {
            let shown = ReqView {
                run_id: run_id.clone(),
                phase: phase.clone(),
                manifest_hash: manifest_hash.clone(),
            };
            let counter = &self.random;
            let mut random = || {
                counter.set(counter.get() + 1);
                Ok([counter.get(); 16])
            };
            let mut ctl = controller::Ctl {
                requests: files.requests.clone(),
                approvals: files.approvals.clone(),
                key: files.key.clone(),
                operator: "alice".to_owned(),
                random: &mut random,
                owner: LUNCHROOM_UID,
                chown: |_, _| Ok(()),
            };
            return match controller::approve(&mut ctl, &shown, now) {
                Ok(path) => Outcome::done(format!("approved {}", path.display())),
                Err(why) => Outcome::refused(why),
            };
        }
        Outcome::done(format!("did {}", action.title()))
    }

    fn controller_requests(&mut self) -> Listing {
        self.files.as_ref().map_or_else(
            || self.listing.clone(),
            |f| controller::list_requests(&f.requests),
        )
    }

    fn controller_load(&mut self, run_id: &str, phase: &str) -> Result<ReqView, String> {
        self.loads.push((run_id.to_owned(), phase.to_owned()));
        match &self.files {
            Some(f) => controller::read_request(&f.requests, run_id, phase),
            None => self
                .listing
                .requests
                .iter()
                .find(|r| r.run_id == run_id && r.phase == phase)
                .cloned()
                .ok_or_else(|| "no such request".to_owned()),
        }
    }
}

/// A screen, its state and the fake store side.
struct T {
    ui: Ui,
    snap: Snapshot,
    be: Fake,
    /// The monotonic clock the screen decides by, in seconds.
    now: u64,
    /// The wall clock, which only stamps what is written.
    wall: u64,
    size: (u16, u16),
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn click_event(x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

impl T {
    /// Started and unlocked by the right passphrase.
    fn new(snap: Snapshot) -> Self {
        let mut t = Self::locked(snap);
        t.pass(PASS);
        assert!(!t.ui.locked());
        t
    }

    /// Just started: asking the passphrase.
    fn locked(snap: Snapshot) -> Self {
        Self {
            ui: Ui::new(NOW),
            snap,
            be: Fake::new(),
            now: NOW,
            wall: NOW,
            size: (120, 40),
        }
    }

    fn code(&mut self, code: KeyCode) {
        self.ui.set_wall(self.wall);
        self.ui.key(key(code), self.now, &self.snap, &mut self.be);
    }

    fn press(&mut self, c: char) {
        self.code(KeyCode::Char(c));
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.press(c);
        }
    }

    fn pass(&mut self, text: &str) {
        self.typed(text);
        self.code(KeyCode::Enter);
    }

    /// A click on the frame the user last saw: the screen is drawn first, as the loop draws it
    /// after every event.
    fn click(&mut self, x: u16, y: u16) {
        let _ = self.draw();
        self.ui.set_wall(self.wall);
        self.ui
            .mouse(click_event(x, y), self.now, &self.snap, &mut self.be);
    }

    fn advance(&mut self, secs: u64) {
        self.now += secs;
        self.wall += secs;
        self.ui.tick(self.now);
    }

    fn at(&mut self, w: u16, h: u16) -> &mut Self {
        self.size = (w, h);
        self
    }

    fn draw(&mut self) -> Buffer {
        let (w, h) = self.size;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("a test terminal");
        let (ui, snap, now) = (&mut self.ui, &self.snap, self.now);
        terminal
            .draw(|f| draw(f, ui, snap, now))
            .expect("a drawn frame");
        terminal.backend().buffer().clone()
    }

    fn screen(&mut self) -> String {
        rows(&self.draw()).join("\n")
    }

    /// Chooses the item `want` with the arrow keys, up or down.
    fn select(&mut self, want: &ItemKey) {
        for _ in 0..4_000 {
            self.ui.sync(&self.snap);
            if self.ui.selected() == Some(want) {
                return;
            }
            let items = self.snap.items();
            let target = items
                .iter()
                .position(|i| &i.key() == want)
                .expect("an item of that key");
            let here = items
                .iter()
                .position(|i| Some(&i.key()) == self.ui.selected())
                .unwrap_or(0);
            self.code(if target > here {
                KeyCode::Down
            } else {
                KeyCode::Up
            });
        }
        panic!("{want:?} cannot be chosen");
    }

    fn select_agent(&mut self, name: &str) {
        self.select(&ItemKey("agent", name.to_owned()));
    }

    fn select_message(&mut self, id: &str) {
        self.select(&ItemKey("message", id.to_owned()));
    }

    fn select_alert(&mut self, id: &str) {
        self.select(&ItemKey("alert", id.to_owned()));
    }

    fn select_check(&mut self, name: &str) {
        let key = self
            .snap
            .checks
            .iter()
            .find(|c| c.name == name)
            .map(|c| Item::Check(c).key())
            .expect("a check of that name");
        self.select(&key);
    }

    /// Where `needle` is on the drawn screen.
    fn find(&mut self, needle: &str) -> (u16, u16) {
        let buf = self.draw();
        for (y, row) in rows(&buf).iter().enumerate() {
            if let Some(x) = row.find(needle) {
                return (u16::try_from(x).unwrap_or(0), u16::try_from(y).unwrap_or(0));
            }
        }
        panic!(
            "{needle:?} is not on the screen:\n{}",
            rows(&buf).join("\n")
        );
    }
}

fn rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect()
        })
        .collect()
}

fn snap_item<'a>(t: &'a T, agent: &str) -> Item<'a> {
    Item::Agent(t.snap.agent(agent).expect("an agent of that name"))
}

fn modal_title(t: &T) -> Option<String> {
    match t.ui.modal()? {
        Modal::Confirm(p) | Modal::Passphrase { pending: p, .. } => Some(p.action.title()),
        Modal::Mode => Some("MODE".to_owned()),
        Modal::Yolo { .. } => Some("YOLO".to_owned()),
        Modal::Controller(_) => Some("CONTROLLER".to_owned()),
    }
}

fn asking_passphrase(t: &T) -> bool {
    matches!(t.ui.modal(), Some(Modal::Passphrase { .. }))
}

// ---------------------------------------------------------------------------------------------
// Item 3: the passphrase.
// ---------------------------------------------------------------------------------------------

// §17::29: starting the console shows a passphrase prompt before anything else.
#[test]
fn starting_shows_the_passphrase_prompt_and_nothing_else() {
    let mut t = T::locked(sample());
    let screen = t.screen();
    assert!(screen.contains("Lunchroom passphrase:"), "{screen}");
    for hidden in [
        "CALLS WAIT",
        "aws-codex",
        "alice",
        "AGENTS",
        "Strict",
        "STRICT",
        "approve",
    ] {
        assert!(
            !screen.contains(hidden),
            "{hidden} on the start screen:\n{screen}"
        );
    }
    assert!(t.ui.locked());
    // Keys do nothing before the passphrase.
    for c in ['a', 'q', 'c', 'm', 'l'] {
        t.press(c);
    }
    t.code(KeyCode::Enter);
    assert!(t.ui.ending().is_none() || t.ui.ending() == Some(Ending::Failed));
    assert!(t.be.performed.is_empty());
}

// §17::29, §17::34: a wrong passphrase three times ends the console; two wrong and a right one
// shows the screen.
#[test]
fn three_wrong_passphrases_end_the_console() {
    let mut t = T::locked(sample());
    t.pass("not it");
    t.pass("");
    assert!(t.ui.ending().is_none());
    let screen = t.screen();
    assert!(screen.contains("1 tries left"), "{screen}");
    t.pass("again no");
    assert_eq!(t.ui.ending(), Some(Ending::Failed));
    assert!(t.ui.locked());

    let mut t = T::locked(sample());
    t.pass("not it");
    t.pass("not it");
    t.pass(PASS);
    assert!(t.ui.ending().is_none());
    assert!(!t.ui.locked());
    assert!(t.screen().contains("CALLS WAITING FOR YOU"));
}

// The typed passphrase is drawn as dots, never as itself.
#[test]
fn the_typed_passphrase_is_never_drawn() {
    let mut t = T::locked(sample());
    t.typed("hunter2");
    let screen = t.screen();
    assert!(!screen.contains("hunter2"), "{screen}");
    assert!(screen.contains("•••••••"), "{screen}");
    let mut t = T::new(sample());
    t.press('a');
    t.typed("topsecretword");
    let screen = t.screen();
    assert!(!screen.contains("topsecretword"), "{screen}");
    assert!(screen.contains("•••••••••••••"), "{screen}");
}

// §17::30: each loosening action asks the passphrase, a second right after the first included;
// the tightening one (strict) asks only y/N.
#[test]
fn each_loosening_action_asks_the_passphrase() {
    let mut t = T::new(sample());
    for (key, mode) in [
        ('b', "MODE BALANCED"),
        ('r', "MODE RELAXED"),
        ('b', "MODE BALANCED"),
    ] {
        t.press('m');
        assert_eq!(modal_title(&t).as_deref(), Some("MODE"));
        t.press(key);
        assert!(asking_passphrase(&t), "{mode}");
        assert_eq!(modal_title(&t).as_deref(), Some(mode));
        // A wrong one changes nothing and asks again.
        t.pass("wrong");
        assert!(asking_passphrase(&t));
        t.pass(PASS);
        assert!(t.ui.modal().is_none());
    }
    assert_eq!(t.be.performed.len(), 3);
    // Yolo, twice.
    for n in 1..=2 {
        t.press('y');
        t.typed("kimi,codex");
        t.code(KeyCode::Enter);
        t.typed("17:30");
        t.code(KeyCode::Enter);
        assert!(asking_passphrase(&t), "yolo {n}");
        t.pass(PASS);
    }
    assert_eq!(t.be.performed.len(), 5);
    assert_eq!(
        t.be.performed[3],
        Action::Yolo {
            names: "kimi,codex".to_owned(),
            till: "17:30".to_owned()
        }
    );
    // Strict asks y/N and no passphrase.
    let before = t.be.checked;
    t.press('m');
    t.press('s');
    assert!(matches!(t.ui.modal(), Some(Modal::Confirm(_))));
    t.press('y');
    assert_eq!(t.be.checked, before);
    assert_eq!(t.be.performed.len(), 6);
}

// The yolo form refuses what is not a name list and an HH:MM time.
#[test]
fn the_yolo_form_checks_its_fields() {
    let mut t = T::new(sample());
    t.press('y');
    t.code(KeyCode::Enter);
    t.typed("25:00");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(screen.contains("Name the agents"), "{screen}");
    t.typed("kimi");
    t.code(KeyCode::Tab);
    for bad in ["1730", "24:00", "12:60", "ab:cd", "7:30"] {
        t.ui.modal = Some(Modal::Yolo {
            names: "kimi".to_owned(),
            till: bad.to_owned(),
            second: true,
            error: String::new(),
        });
        t.code(KeyCode::Enter);
        assert!(
            matches!(t.ui.modal(), Some(Modal::Yolo { error, .. }) if !error.is_empty()),
            "{bad}"
        );
    }
    assert!(t.be.performed.is_empty());
    t.code(KeyCode::Esc);
    assert!(t.ui.modal().is_none());
}

// §17::31: each approve, a second one a minute after the first included, asks the passphrase; an
// approve with a wrong passphrase or none approves nothing.
#[test]
fn each_approve_asks_the_passphrase() {
    let mut t = T::new(sample());
    t.press('a');
    assert!(asking_passphrase(&t));
    assert_eq!(modal_title(&t).as_deref(), Some("APPROVE CALL 1"));
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Approve {
            number: 1,
            id: "c1".to_owned()
        }]
    );
    // A minute later, the next one asks again.
    t.advance(60);
    t.press('a');
    assert!(asking_passphrase(&t));
    t.pass(PASS);
    assert_eq!(t.be.performed.len(), 2);
    // None: Enter on an empty line.
    t.advance(60);
    t.press('a');
    t.code(KeyCode::Enter);
    assert!(asking_passphrase(&t));
    // Wrong.
    t.pass("wrong");
    assert_eq!(t.be.performed.len(), 2);
    // Esc.
    t.code(KeyCode::Esc);
    assert!(t.ui.modal().is_none());
    assert_eq!(t.be.performed.len(), 2);
    // The checks were made by the backend, not skipped: start, two rights, one wrong (the empty
    // line is refused before it is asked).
    assert_eq!(t.be.checked, 4);
}

// Three wrong passphrases at an approve lock the screen and approve nothing.
#[test]
fn three_wrong_passphrases_at_an_approve_lock_the_screen() {
    let mut t = T::new(sample());
    t.press('a');
    for _ in 0..3 {
        t.pass("wrong");
    }
    assert!(t.ui.locked());
    assert!(t.ui.modal().is_none());
    assert!(t.be.performed.is_empty());
    let screen = t.screen();
    assert!(screen.contains("The details are hidden"), "{screen}");
}

// §17::32, 134: each controller approval asks y/N and then the passphrase, a second one a minute
// after the first included; N, an empty answer and a wrong passphrase write nothing.
#[test]
fn each_controller_approval_asks_the_passphrase() {
    let mut t = T::new(sample());
    t.be.listing.requests = vec![ReqView {
        run_id: RUN.to_owned(),
        phase: "deploy-prod".to_owned(),
        manifest_hash: HASH.to_owned(),
    }];
    for n in 0..2 {
        t.press('c');
        t.code(KeyCode::Enter);
        let screen = t.screen();
        assert!(screen.contains("Approve this phase? y/N"), "{screen}");
        t.press('y');
        assert!(asking_passphrase(&t), "approval {n}");
        t.pass(PASS);
        assert_eq!(t.be.performed.len(), n + 1);
        t.advance(60);
    }
    // N, an empty answer (Enter) and any other key write nothing.
    for answer in [
        KeyCode::Char('n'),
        KeyCode::Enter,
        KeyCode::Char('x'),
        KeyCode::Esc,
    ] {
        t.press('c');
        t.code(KeyCode::Enter);
        t.code(answer);
        assert!(t.ui.modal().is_none());
        assert_eq!(t.be.performed.len(), 2);
    }
    // A wrong passphrase writes nothing.
    t.press('c');
    t.code(KeyCode::Enter);
    t.press('y');
    t.pass("wrong");
    assert_eq!(t.be.performed.len(), 2);
    t.code(KeyCode::Esc);
}

// §17::33: after 15 minutes without a key the console locks; at 14:59 it does not.
#[test]
fn the_screen_locks_at_15_minutes() {
    let mut t = T::new(sample());
    t.advance(IDLE_LOCK_SECS - 1);
    assert!(!t.ui.locked(), "14:59 is not hidden yet");
    let screen = t.screen();
    assert!(screen.contains("screen locks in 00:01"), "{screen}");
    t.advance(1);
    assert!(t.ui.locked(), "15:00 hides the screen");
    // A key at 14:59 starts the 15 minutes again.
    let mut t = T::new(sample());
    t.advance(IDLE_LOCK_SECS - 1);
    t.press('x');
    t.code(KeyCode::Esc);
    t.advance(IDLE_LOCK_SECS - 1);
    assert!(!t.ui.locked());
    t.advance(1);
    assert!(t.ui.locked());
    // A key that comes at 15:00 is not an answer to anything: it is dropped.
    let mut t = T::new(sample());
    t.now += IDLE_LOCK_SECS;
    t.press('a');
    assert!(t.ui.locked());
    assert!(t.ui.modal().is_none());
    assert!(t.be.performed.is_empty());
}

// The header counts the time to the lock.
#[test]
fn the_header_shows_the_minutes_until_the_screen_locks() {
    let mut t = T::new(sample());
    t.advance(28);
    let screen = t.screen();
    assert!(screen.contains("screen locks in 14:32"), "{screen}");
}

// §17::34, §17::230: locked, nothing but the alert strip is shown or done until the passphrase.
#[test]
fn locked_shows_only_the_strip_and_does_nothing() {
    let mut t = T::new(sample());
    assert!(
        t.screen().contains("HOLD MODE STRICT"),
        "the header shows while unlocked"
    );
    t.code(KeyCode::Enter);
    assert!(t.ui.detail_key().is_some());
    t.press('l');
    assert!(t.ui.locked());
    assert!(
        t.ui.detail_key().is_none(),
        "the panel is closed when the screen hides"
    );
    let screen = t.screen();
    assert!(screen.contains("CALLS WAIT 2"), "{screen}");
    assert!(screen.contains("The details are hidden"), "{screen}");
    for hidden in [
        "aws-codex",
        "plymouth-tc",
        "AGENTS",
        "ALERTS",
        "approve",
        "git push",
        "alice",
        "STRICT",
        "CALLS WAITING FOR YOU",
        "┌ GUARD",
    ] {
        assert!(
            !screen.contains(hidden),
            "{hidden} shows while the screen is hidden:\n{screen}"
        );
    }
    // No action runs, no panel opens, no question opens.
    for c in ['a', 'x', 'r', 's', 'd', 'h', 'u', 'm', 'y', 'c'] {
        t.press(c);
    }
    t.code(KeyCode::Enter);
    t.code(KeyCode::Down);
    assert!(t.ui.modal().is_none());
    assert!(t.ui.detail_key().is_none());
    assert!(t.be.performed.is_empty());
    assert!(t.ui.locked());
    // Clicks do nothing: not on an item, not on the key line, not on a cell.
    t.click(10, 10);
    t.click(0, 39);
    t.click(5, 2);
    assert!(t.ui.detail_key().is_none() && t.ui.modal().is_none() && t.be.performed.is_empty());
    // The passphrase brings the screen back.
    t.pass(PASS);
    assert!(!t.ui.locked());
    assert!(t.screen().contains("AGENTS"));
}

// §17 item 3: a locked screen shows the alert strip and the passphrase prompt and nothing else:
// not the header (the console's name, the login, the time, the hold mode, the minutes), not a
// body, not a key line.
#[test]
fn a_locked_screen_holds_only_the_strip_and_the_prompt() {
    for (w, h) in [(120u16, 40u16), (80, 24)] {
        let mut t = T::new(sample());
        t.at(w, h).press('l');
        t.typed("abc");
        let r = rows(&t.draw());
        let live: Vec<&String> = r.iter().filter(|row| !row.trim().is_empty()).collect();
        for row in &live {
            let strip = row.contains('┌') || row.contains('└') || row.contains('│');
            let prompt =
                row.contains("Passphrase to unlock") || row.contains("The details are hidden");
            assert!(
                strip || prompt,
                "{w}x{h}: {row:?} is neither the strip nor the prompt"
            );
        }
        assert_eq!(
            live.len(),
            3 + 2,
            "{w}x{h}: the strip (three rows) and the prompt (two)\n{}",
            r.join("\n")
        );
        assert!(r[0].trim().is_empty(), "the top row is empty: {:?}", r[0]);
        for hidden in [
            "LUNCHROOM CONSOLE",
            "UTC",
            "HOLD MODE",
            "alice",
            "screen locks",
            "approve",
            "dev",
        ] {
            assert!(
                !r.join("\n").contains(hidden),
                "{hidden} shown while the screen waits for the passphrase"
            );
        }
        // The passphrase typed is dots.
        assert!(r.join("\n").contains("•••"));
    }
    // The start prompt, before the first passphrase, shows no strip and no header either.
    let mut t = T::locked(sample());
    let r = rows(&t.draw());
    assert!(!r.join("\n").contains("CALLS WAIT") && !r.join("\n").contains("UTC"));
}

// §17 item 10: text an agent typed cannot look like a mark: its backslash is shown as `\u{5c}`,
// and an odd space as its code, in a list, in a panel and in a question.
#[test]
fn a_typed_backslash_and_odd_space_are_shown_as_codes_on_the_screen() {
    let mut snap = sample();
    snap.agents[0].name = "x\\u{1b}".to_owned();
    snap.waiting[0].command = "echo a\\nb\u{a0}c\u{3000}d".to_owned();
    let mut t = T::new(snap);
    let screen = t.screen();
    assert!(screen.contains("x\\u{5c}u{1b}"), "{screen}");
    t.code(KeyCode::Enter);
    let panel = t.screen();
    assert!(
        panel.contains("echo a\\u{5c}nb\\u{a0}c\\u{3000}d"),
        "{panel}"
    );
    t.press('a');
    let question = t.screen();
    assert!(question.contains("\\u{5c}nb\\u{a0}c"), "{question}");
}

// §17::231: locked, the strip is live (it changes with the store) and a red cell shows.
#[test]
fn the_strip_stays_live_while_locked() {
    let mut t = T::new(sample());
    t.press('l');
    let before = t.draw();
    t.snap.alerts.clear();
    t.snap.waiting.clear();
    let after = t.draw();
    let row = |b: &Buffer| rows(b)[2].clone();
    assert!(row(&before).contains("PROBING 1"), "{}", row(&before));
    assert!(row(&after).contains("PROBING 0"), "{}", row(&after));
    assert!(row(&after).contains("CALLS WAIT 0"));
    // Red while locked.
    t.snap.alerts.push(AlertRow {
        kind: AlertKind::Probing,
        ..AlertRow::default()
    });
    let buf = t.draw();
    let edges = cell_edges(120);
    assert_eq!(buf[(edges[1] + 1, 2)].style().bg, Some(RED));
}

// §17::35: `unregister` within 15 minutes of the passphrase asks none; after 15 minutes it asks.
#[test]
fn unregister_rides_on_the_passphrase_for_15_minutes() {
    let mut t = T::new(sample());
    t.select_agent("plymouth-tc");
    t.advance(IDLE_LOCK_SECS - 1);
    t.press('u');
    assert!(
        matches!(t.ui.modal(), Some(Modal::Confirm(_))),
        "y/N, no passphrase"
    );
    let checked = t.be.checked;
    t.press('y');
    assert_eq!(t.be.checked, checked);
    assert_eq!(
        t.be.performed,
        [Action::Unregister {
            agent: "plymouth-tc".to_owned()
        }]
    );
    // A key now and the 15 minutes from the passphrase have passed: it asks.
    t.advance(2);
    t.press('u');
    t.press('y');
    assert!(asking_passphrase(&t), "after 15 minutes");
    t.pass(PASS);
    assert_eq!(t.be.performed.len(), 2);
    // And right after that passphrase, within 15 minutes, it rides on it.
    t.press('u');
    t.press('y');
    assert!(t.ui.modal().is_none());
    assert_eq!(t.be.performed.len(), 3);
    // `y/N`: N unregisters nothing.
    t.press('u');
    t.press('n');
    assert_eq!(t.be.performed.len(), 3);
}

// ---------------------------------------------------------------------------------------------
// Item 10: the layout and the strip.
// ---------------------------------------------------------------------------------------------

fn row_of(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|r| r.contains(needle))
}

// §17::165, 166: the header holds five items and the screen its six parts in order.
#[test]
fn the_screen_has_its_six_parts_in_order() {
    for (w, h) in [(120, 40), (80, 24)] {
        let mut t = T::new(sample());
        let buf = t.at(w, h).draw();
        let r = rows(&buf);
        assert!(r[0].contains("LUNCHROOM CONSOLE"), "{}", r[0]);
        assert!(r[0].contains("alice"));
        assert!(r[0].contains("UTC") && r[0].contains(':'));
        assert!(r[0].contains("HOLD MODE STRICT"), "{}", r[0]);
        assert!(r[0].contains("screen locks in 15:00"), "{}", r[0]);
        let strip_row = row_of(&r, "CALLS WAIT").expect("the strip");
        let calls = row_of(&r, "CALLS WAITING FOR YOU").expect("the calls");
        let beside = row_of(&r, "CALL 1").expect("the call beside them");
        let agents = row_of(&r, "AGENTS").expect("the agents");
        let alerts = row_of(&r, "ALERTS").expect("the alerts");
        let guard = row_of(&r, "┌ GUARD").expect("the guard");
        let keys = row_of(&r, "approve").expect("the key line");
        assert!(strip_row < calls, "{w}x{h}");
        assert_eq!(calls, beside, "the selected call is beside the calls");
        assert!(
            calls < agents && agents < alerts && alerts == guard && alerts < keys,
            "{w}x{h}"
        );
        assert!(keys >= usize::from(h) - usize::from(key_rows(w)));
    }
}

fn lit(snap: &mut Snapshot, which: usize) {
    match which {
        0 => snap.waiting = vec![call(1, "a", "c")],
        1 => {
            snap.alerts = vec![AlertRow {
                kind: AlertKind::Probing,
                ..AlertRow::default()
            }];
        }
        2 => snap.messages = vec![message("m", MessageKind::Failed, "t")],
        3 => {
            let mut a = agent("x", AgentState::Unguarded);
            a.queued = 1;
            snap.agents = vec![a];
        }
        4 => snap.guard.slow = 1,
        _ => snap.checks = vec![check(false, "c", Fix::None)],
    }
}

fn quiet() -> Snapshot {
    Snapshot {
        now: NOW,
        developer: "dev".to_owned(),
        hold_mode: "Balanced".to_owned(),
        checks: vec![check(true, "ok", Fix::None)],
        ..Snapshot::default()
    }
}

// §17::167-178: for each cell, the state that lights it and the state that leaves it dark, the
// colours drawn, and the word or count in each, also with the colours taken away.
#[test]
fn each_strip_cell_lights_and_darkens() {
    let words = [
        "CALLS WAIT",
        "PROBING",
        "FAILED MSG",
        "UNGUARDED",
        "GUARD SLOW",
        "CHECKS",
    ];
    let edges = cell_edges(120);
    let dark = |t: &mut T, i: usize| {
        let b = t.draw();
        let cell = &b[(edges[i] + 1, 2)];
        assert_eq!(cell.style().bg, Some(Color::Reset), "cell {i} is dark");
        assert_eq!(cell.style().fg, Some(DIM), "cell {i} is dark");
        rows(&b)[2].clone()
    };
    let lit_with = |t: &mut T, i: usize, bg: Option<Color>, fg: Option<Color>| {
        let b = t.draw();
        let cell = &b[(edges[i] + 1, 2)];
        if let Some(bg) = bg {
            assert_eq!(cell.style().bg, Some(bg), "cell {i}");
        }
        if let Some(fg) = fg {
            assert_eq!(cell.style().fg, Some(fg), "cell {i}");
        }
        rows(&b)[2].clone()
    };
    for (i, word) in words.iter().enumerate().take(5) {
        // Nothing to report: dark, with its word and its count 0.
        let mut t = T::new(quiet());
        let row = dark(&mut t, i);
        assert!(row.contains(&format!("{word} 0")), "{row}");
        // Something to report: amber (red for PROBING), with its count 1.
        let mut snap = quiet();
        lit(&mut snap, i);
        let mut t = T::new(snap);
        let level = if i == 1 { RED } else { AMBER };
        let row = lit_with(&mut t, i, Some(level), Some(Color::Black));
        assert!(row.contains(&format!("{word} 1")), "{row}");
    }
    // GUARD SLOW is red for a timeout in the last hour.
    let mut snap = quiet();
    snap.guard.timeouts = 1;
    let mut t = T::new(snap);
    let row = lit_with(&mut t, 4, Some(RED), None);
    assert!(row.contains("GUARD SLOW 1"), "{row}");
    // A slow decision and a timeout together: red, counting both.
    let mut snap = quiet();
    snap.guard = GuardRow {
        slow: 2,
        timeouts: 1,
        ..GuardRow::default()
    };
    let mut t = T::new(snap);
    let row = lit_with(&mut t, 4, Some(RED), None);
    assert!(row.contains("GUARD SLOW 3"), "{row}");
    // CHECKS: green while every check passes, red while one fails.
    let mut t = T::new(quiet());
    let row = lit_with(&mut t, 5, None, Some(GREEN));
    assert!(row.contains("CHECKS OK"), "{row}");
    let mut snap = quiet();
    lit(&mut snap, 5);
    let mut t = T::new(snap);
    let row = lit_with(&mut t, 5, Some(RED), None);
    assert!(row.contains("CHECKS 1 FAIL"), "{row}");
    // The spec names two states, green and red: with no check listed none fails, so it is green.
    let mut snap = quiet();
    snap.checks.clear();
    let mut t = T::new(snap);
    let row = lit_with(&mut t, 5, None, Some(GREEN));
    assert!(row.contains("CHECKS OK"), "{row}");
    assert!(!row.contains("CHECKS -"), "{row}");
}

// §17::172 as the approved mock-up shows it: UNGUARDED counts every agent that is UNGUARDED, and a
// STOPPED one only with messages QUEUED for it.
#[test]
fn unguarded_counts_each_unguarded_agent_and_a_stopped_one_with_queued_messages() {
    let mut snap = quiet();
    let mut a = agent("a", AgentState::Unguarded);
    a.queued = 2;
    let mut b = agent("b", AgentState::Stopped);
    b.queued = 1;
    let c = agent("c", AgentState::Unguarded);
    let mut d = agent("d", AgentState::Available);
    d.queued = 5;
    let mut e = agent("e", AgentState::Held);
    e.queued = 5;
    let f = agent("f", AgentState::Stopped);
    snap.agents = vec![a, b, c, d, e, f];
    let cells = strip(&snap);
    // a and c are UNGUARDED (c with nothing queued), b is STOPPED with one QUEUED; f is STOPPED
    // with none.
    assert_eq!(cells[3].text, "UNGUARDED 3");
    assert_eq!(cells[3].level, Level::Amber);
    snap.agents.truncate(3);
    snap.agents.remove(0);
    snap.agents.remove(0);
    assert_eq!(
        strip(&snap)[3].text,
        "UNGUARDED 1",
        "an UNGUARDED agent with nothing queued"
    );
    snap.agents.clear();
    assert_eq!(strip(&snap)[3].level, Level::Dark);
}

// §17::168, 167: the six cells are in this order and each at the same place on every screen.
#[test]
fn the_cells_never_move() {
    let words = [
        "CALLS WAIT",
        "PROBING",
        "FAILED MSG",
        "UNGUARDED",
        "GUARD SLOW",
        "CHECKS",
    ];
    let edges = cell_edges(120);
    let mut screens: Vec<Buffer> = Vec::new();
    let mut t = T::new(quiet());
    screens.push(t.draw());
    let mut t = T::new(sample());
    screens.push(t.draw());
    t.code(KeyCode::Enter);
    screens.push(t.draw());
    t.code(KeyCode::Esc);
    t.press('a');
    screens.push(t.draw());
    t.code(KeyCode::Esc);
    t.press('l');
    screens.push(t.draw());
    for buf in &screens {
        let r = rows(buf);
        let mut last = 0;
        for (i, word) in words.iter().enumerate() {
            let x = r[2].find(word).expect(word);
            assert!(
                x > usize::from(edges[i]) && x < usize::from(edges[i + 1]),
                "{word} at {x}"
            );
            assert!(x >= last);
            last = x;
        }
        // The cell rows (rules and text) are drawn alike on every screen.
        assert_eq!(r[1].chars().count(), r[0].chars().count());
        assert!(r[1].starts_with('┌') && r[3].starts_with('└'));
    }
    // The positions of the rules are the same on every screen.
    let rules: Vec<String> = screens.iter().map(|b| rows(b)[1].clone()).collect();
    assert!(rules.windows(2).all(|w| w[0] == w[1]));
}

// §17::178: with the colours removed each cell still carries its count or word.
#[test]
fn colour_is_never_the_only_sign() {
    for snap in [quiet(), sample()] {
        let mut t = T::new(snap);
        let r = rows(&t.draw());
        let strip_row = &r[2];
        for word in [
            "CALLS WAIT ",
            "PROBING ",
            "FAILED MSG ",
            "UNGUARDED ",
            "GUARD SLOW ",
            "CHECKS ",
        ] {
            assert!(strip_row.contains(word), "{strip_row}");
        }
        // Every cell has a digit or a word of state.
        for cell in strip(&t.snap) {
            assert!(
                cell.text.chars().any(|c| c.is_ascii_digit())
                    || cell.text.contains("OK")
                    || cell.text.contains('-')
            );
        }
    }
}

// §17::179: the main screen takes one line per item and holds no message text, no full command
// and no evidence.
#[test]
fn the_main_screen_is_one_line_per_item_and_holds_no_text() {
    for (w, h) in [(120, 40), (80, 24)] {
        let mut t = T::new(sample());
        let screen = t.at(w, h).screen();
        for secret in [
            "FAILEDBODY",
            "QUEUEDBODY",
            "HOLDTEXT",
            "MISSINGEVIDENCE",
            "EVIDENCETEXT",
            "NOTICEBODY",
            "MARKCALL",
            "git push",
            "rm -rf",
            "--force",
            "lunchroom install claude-code",
            "cat ~/.ssh",
            "outside its slip",
        ] {
            assert!(
                !screen.contains(secret),
                "{secret} on the main screen at {w}x{h}:\n{screen}"
            );
        }
        // Each item is on one line.
        for item in ["plymouth-tc", "chariot-review", "old-session"] {
            assert!(
                screen.lines().filter(|l| l.contains(item)).count() <= 2,
                "{item}:\n{screen}"
            );
        }
    }
}

// §17::164, 183, 184: agent text with control characters, direction marks, escape sequences, OSC
// 52, NUL, line breaks and wide characters changes no cell of the layout, and nothing reaches a
// cell that a terminal would act on.
#[test]
fn hostile_text_changes_nothing_of_the_layout_and_reaches_no_terminal() {
    const HOSTILE: &str = "\u{1b}[2J\u{1b}[H\u{1b}]52;c;SGVsbG8=\u{7}\u{202e}evil\u{200f}\0\r\n\t日本語\u{9b}31m\u{85}";
    let clean = sample();
    let mut hostile = sample();
    hostile.agents[0].name = format!("pl{HOSTILE}ym");
    hostile.agents[1].harness = HOSTILE.to_owned();
    hostile.agents[1].held_by[0].from = HOSTILE.to_owned();
    hostile.waiting[1].agent = HOSTILE.to_owned();
    hostile.waiting[1].tool = HOSTILE.to_owned();
    hostile.waiting[1].command = HOSTILE.to_owned();
    hostile.messages[0].to = HOSTILE.to_owned();
    hostile.messages[0].id = "lr-9".to_owned();
    hostile.alerts[0].line = HOSTILE.to_owned();
    hostile.alerts[1].line = HOSTILE.to_owned();
    hostile.checks[1].name = HOSTILE.to_owned();
    hostile.marked[0].path = HOSTILE.to_owned();
    hostile.developer = HOSTILE.to_owned();
    hostile.hold_mode = HOSTILE.to_owned();
    for (w, h) in [(120u16, 40u16), (80, 24)] {
        let mut a = T::new(clean.clone());
        let mut b = T::new(hostile.clone());
        let (ba, bb) = (a.at(w, h).draw(), b.at(w, h).draw());
        // The strip (rows 1-3) and the key line are the same.
        for y in 1..4 {
            assert_eq!(rows(&ba)[y], rows(&bb)[y], "strip row {y} at {w}x{h}");
        }
        let keys = usize::from(h) - usize::from(key_rows(w));
        for y in keys..usize::from(h) {
            assert_eq!(rows(&ba)[y], rows(&bb)[y], "key row {y} at {w}x{h}");
        }
        // Every row is exactly as wide as the screen.
        for y in 0..h {
            let n = (0..w).filter(|x| !bb[(*x, y)].symbol().is_empty()).count();
            assert_eq!(n, usize::from(w));
        }
        // No cell holds a character a terminal would act on.
        for y in 0..h {
            for x in 0..w {
                assert!(
                    !bb[(x, y)].symbol().chars().any(is_mark),
                    "a mark character at ({x},{y}) of {w}x{h}"
                );
            }
        }
        let screen = rows(&bb).join("\n");
        assert!(
            screen.contains("\\u{1b}"),
            "the escape is drawn as its code:\n{screen}"
        );
    }
    // The panel: the escape sequences are drawn as codes, whole.
    let mut t = T::new(hostile);
    t.select(&ItemKey("call", "c2".to_owned()));
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let screen = rows(&buf).join("\n");
    for code in [
        "\\u{1b}",
        "\\u{202e}",
        "\\u{200f}",
        "\\u{0}",
        "\\u{7}",
        "\\u{9b}",
        "\\u{85}",
        "\\u{d}",
    ] {
        assert!(screen.contains(code), "{code} missing:\n{screen}");
    }
    assert!(screen.contains("]52;c;SGVsbG8="));
    for y in 0..40 {
        for x in 0..120 {
            assert!(!buf[(x, y)].symbol().chars().any(is_mark));
        }
    }
}

// §17::184: the title of a question and the text the store side answers with can carry an agent's
// name: they too reach no cell as a character a terminal would act on.
#[test]
fn a_hostile_name_in_a_question_or_an_answer_reaches_no_terminal() {
    const EVIL: &str = "x\u{1b}]52;c;AAAA\u{7}\u{202e}y\0";
    let mut snap = sample();
    snap.agents[1].name = EVIL.to_owned();
    snap.waiting[0].agent = EVIL.to_owned();
    snap.messages[0].id = EVIL.to_owned();
    snap.alerts[0].agent = EVIL.to_owned();
    snap.marked[0].path = EVIL.to_owned();
    let mut t = T::new(snap);
    let check_cells = |t: &mut T, what: &str| {
        let buf = t.draw();
        for y in 0..40 {
            for x in 0..120 {
                assert!(
                    !buf[(x, y)].symbol().chars().any(is_mark),
                    "{what}: ({x},{y})"
                );
            }
        }
        rows(&buf).join("\n")
    };
    t.select_agent(EVIL);
    t.press('r');
    assert!(check_cells(&mut t, "release").contains("RELEASE x\\u{1b}]52"));
    t.code(KeyCode::Esc);
    t.press('u');
    let _ = check_cells(&mut t, "unregister");
    t.press('n');
    t.select(&ItemKey("call", "c1".to_owned()));
    t.press('a');
    let _ = check_cells(&mut t, "approve");
    t.code(KeyCode::Esc);
    t.select(&ItemKey("message", EVIL.to_owned()));
    t.press('d');
    let _ = check_cells(&mut t, "discard");
    t.press('y');
    // The answer of the store side: "did DISCARD <name>".
    assert!(check_cells(&mut t, "answer").contains("did DISCARD x\\u{1b}"));
    t.select(&ItemKey("path", EVIL.to_owned()));
    t.press('d');
    let _ = check_cells(&mut t, "clear");
}

// §17::183: the codes for the characters named in the spec.
#[test]
fn escape_and_direction_marks_are_drawn_as_codes() {
    assert_eq!(visible("\u{1b}"), "\\u{1b}");
    assert_eq!(visible("\u{202e}"), "\\u{202e}");
}

// §17::36-42: the agents, the QUEUED and FAILED messages with their reasons, the notices, the calls
// waiting with their age, the DENIED counts, the guard's timeouts and slow decisions and the
// checks of doctor are shown.
#[test]
fn the_screen_shows_what_item_3_lists() {
    let mut t = T::new(sample());
    let screen = t.screen();
    // Agents.
    for name in ["plymouth-tc", "aws-codex", "chariot-review", "old-session"] {
        assert!(screen.contains(name), "{name}\n{screen}");
    }
    assert!(screen.contains("UNGUARDED") && screen.contains("STOPPED") && screen.contains("HELD"));
    // QUEUED and FAILED messages (one line each; the reason is in the panel).
    assert!(
        screen.contains("FAILED lr-0007") && screen.contains("QUEUED lr-0008"),
        "{screen}"
    );
    // Each with its reason, on its line (§17::37); the message text stays in the panel.
    assert!(screen.contains("- REASONTEXT it could not"), "{screen}");
    assert!(!screen.contains("FAILEDBODY"), "{screen}");
    t.select_message("lr-0007");
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("REASONTEXT"));
    t.code(KeyCode::Esc);
    // Notices (decision of 2026-11-14, "Both always visible": the newest stay on a full box).
    assert!(screen.contains("a notice for you"), "{screen}");
    // Calls waiting with their age.
    assert!(screen.contains("asked 3m ago"), "{screen}");
    // The DENIED counts: the hour's count and the sparkline.
    assert!(screen.contains("21") && screen.contains("█"), "{screen}");
    // The guard's timeouts and slow decisions.
    assert!(
        screen.contains("slow >2 s 1h") && screen.contains("timeouts 1h"),
        "{screen}"
    );
    // The checks of doctor.
    assert!(
        screen.contains("1 OK") && screen.contains("5 FAIL"),
        "{screen}"
    );
    assert!(screen.contains("FAIL check-cmd"));
}

// The agents table: at 120 columns every column, at 80 the ones that fit; the sparkline reaches
// full height for the busiest span.
#[test]
fn the_agents_table_fits_its_columns() {
    let mut t = T::new(sample());
    let wide = t.at(120, 40).screen();
    for head in [
        "NAME",
        "HARNESS",
        "STATE",
        "QUEUED",
        "HELD BY",
        "DENIED 60 min",
        "YOLO",
    ] {
        assert!(wide.contains(head), "{head}\n{wide}");
    }
    assert!(
        wide.contains("peer-agent") && wide.contains("YOLO until 17:30 UTC"),
        "{wide}"
    );
    let narrow = t.at(80, 24).screen();
    for head in ["NAME", "STATE", "DENIED 60 min"] {
        assert!(narrow.contains(head), "{head}\n{narrow}");
    }
    for line in narrow.lines() {
        assert!(line.chars().count() <= 80);
    }
}

// ---------------------------------------------------------------------------------------------
// Item 10: sizes and lists.
// ---------------------------------------------------------------------------------------------

// A screen smaller than the layout, even none at all, draws without a panic, with the strip where
// it fits.
#[test]
fn a_screen_smaller_than_the_layout_does_not_panic() {
    for (w, h) in [
        (0u16, 0u16),
        (1, 1),
        (5, 2),
        (10, 3),
        (20, 5),
        (30, 8),
        (40, 12),
        (60, 14),
        (79, 23),
    ] {
        for step in 0..6 {
            let mut t = T::new(sample());
            t.at(w, h);
            match step {
                1 => t.code(KeyCode::Enter),
                2 => t.press('a'),
                3 => t.press('c'),
                4 => t.press('m'),
                5 => t.press('l'),
                _ => {}
            }
            let _ = t.draw();
            t.code(KeyCode::Esc);
            let _ = t.draw();
        }
        let mut t = T::locked(sample());
        t.at(w, h);
        let _ = t.draw();
        t.pass("x");
        let _ = t.draw();
    }
    // At 40x12 the strip cells are still where they are.
    let mut t = T::new(sample());
    let r = rows(&t.at(80, 24).draw());
    assert!(r[2].contains("CALLS WAIT 2"));
}

// Empty lists draw.
#[test]
fn empty_lists_draw() {
    let mut t = T::new(Snapshot {
        now: NOW,
        ..Snapshot::default()
    });
    for (w, h) in [(120, 40), (80, 24)] {
        let screen = t.at(w, h).screen();
        assert!(screen.contains("none waiting"), "{screen}");
        assert!(screen.contains("no agents registered"));
        assert!(screen.contains("nothing needs you"));
    }
    // Keys on an empty screen say so and change nothing.
    for c in ['a', 'x', 'r', 's', 'd', 'h', 'u'] {
        t.press(c);
        assert!(t.ui.modal().is_none());
    }
    t.code(KeyCode::Enter);
    t.code(KeyCode::Down);
    t.code(KeyCode::Up);
    assert!(t.ui.detail_key().is_none());
    assert!(t.be.performed.is_empty());
}

fn many_agents(n: usize) -> Snapshot {
    let mut snap = quiet();
    snap.agents = (0..n)
        .map(|i| {
            let mut a = agent(&format!("agent-{i:04}"), AgentState::Available);
            a.denied_hour = i % 7;
            a
        })
        .collect();
    snap
}

// 1,000 agents draw; the arrow keys and clicks reach each, and the window follows the selection.
#[test]
fn a_thousand_agents_scroll_and_click() {
    let started = std::time::Instant::now();
    let mut t = T::new(many_agents(1_000));
    let first = t.at(120, 40).screen();
    assert!(first.contains("AGENTS (1000)"));
    assert!(first.contains("agent-0000") && !first.contains("agent-0999"));
    for _ in 0..999 {
        t.code(KeyCode::Down);
    }
    let last = t.screen();
    assert!(last.contains("agent-0999"), "{last}");
    assert!(!last.contains("agent-0000"));
    // The window keeps the selection in view: a click on the last row opens its panel.
    let (x, y) = t.find("agent-0999");
    t.click(x, y);
    assert_eq!(
        t.ui.detail_key(),
        Some(&ItemKey("agent", "agent-0999".to_owned()))
    );
    t.code(KeyCode::Esc);
    // Up again.
    for _ in 0..990 {
        t.code(KeyCode::Up);
    }
    assert!(t.screen().contains("agent-0009"));
    // At 80x24 too.
    let _ = t.at(80, 24).screen();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------------------------
// Item 10: the detail panel.
// ---------------------------------------------------------------------------------------------

// §17::180, 182, 212, 213, 214: Enter and a click each open the selected item's panel with its
// whole text; Esc and a click outside close it.
#[test]
fn enter_and_a_click_open_the_panel_and_esc_and_an_outside_click_close_it() {
    let mut t = T::new(sample());
    t.code(KeyCode::Enter);
    assert_eq!(t.ui.detail_key(), Some(&ItemKey("call", "c1".to_owned())));
    let screen = t.screen();
    assert!(
        screen.contains("THE WHOLE CALL") && screen.contains("git push --force origin main"),
        "{screen}"
    );
    t.code(KeyCode::Esc);
    assert!(t.ui.detail_key().is_none());
    // The arrow keys move the selection, and Enter opens that one.
    t.code(KeyCode::Down);
    t.code(KeyCode::Enter);
    assert_eq!(t.ui.detail_key(), Some(&ItemKey("call", "c2".to_owned())));
    t.code(KeyCode::Esc);
    // A click on an item opens it.
    let (x, y) = t.find("aws-codex   ");
    t.click(x, y);
    // The first `aws-codex` on screen is the call's line or the agent's row; both open a panel.
    assert!(t.ui.detail_key().is_some());
    t.code(KeyCode::Esc);
    let (x, y) = t.find("old-session");
    t.click(x, y);
    assert_eq!(
        t.ui.detail_key(),
        Some(&ItemKey("agent", "old-session".to_owned()))
    );
    // A click inside the panel keeps it; a click outside (the header, below it) closes it.
    t.click(60, 20);
    assert!(t.ui.detail_key().is_some());
    t.click(60, 0);
    assert!(t.ui.detail_key().is_none());
    // The key line stays live under a panel: a click on `Esc` closes it.
    t.code(KeyCode::Enter);
    assert!(t.ui.detail_key().is_some());
    let (row, x) = key_place(120, Cmd::Close).expect("Esc on the key line");
    t.click(x, 40 - key_rows(120) + row);
    assert!(t.ui.detail_key().is_none());
}

// §17::181: a text longer than the screen scrolls and its last byte is shown.
#[test]
fn a_long_text_scrolls_to_its_last_byte() {
    for (w, h) in [(120, 40), (80, 24)] {
        let mut snap = sample();
        let mut body = String::new();
        for i in 0..400 {
            let _ = writeln!(body, "line {i:03} of the message");
        }
        body.push_str("THE-LAST-BYTE");
        snap.messages[0].text = body;
        let mut t = T::new(snap);
        t.at(w, h);
        t.select_message("lr-0007");
        t.code(KeyCode::Enter);
        let top = t.screen();
        assert!(top.contains("line 000"), "{top}");
        assert!(!top.contains("THE-LAST-BYTE"));
        // The panel says where it is.
        assert!(top.contains("lines 1-"), "{top}");
        // Page down reaches further; End reaches the last byte.
        t.code(KeyCode::PageDown);
        let page = t.screen();
        assert!(!page.contains("line 000"), "{page}");
        let mut seen = 0;
        loop {
            let before = t.screen();
            t.code(KeyCode::PageDown);
            seen += 1;
            if before == t.screen() || seen > 200 {
                break;
            }
        }
        assert!(t.screen().contains("THE-LAST-BYTE"));
        t.code(KeyCode::Home);
        assert!(t.screen().contains("line 000"));
        t.code(KeyCode::End);
        let end = t.screen();
        assert!(end.contains("THE-LAST-BYTE"), "{end}");
        assert!(end.contains("of 4"), "{end}");
        // The arrows scroll a line; the mouse wheel too.
        t.code(KeyCode::Home);
        t.code(KeyCode::Down);
        t.code(KeyCode::Down);
        assert!(t.screen().contains("lines 3-"));
        t.ui.mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 5,
                row: 12,
                modifiers: KeyModifiers::NONE,
            },
            t.now,
            &t.snap,
            &mut t.be,
        );
        assert!(t.screen().contains("lines 4-"));
    }
}

// §17::181: a 1 MB message in a panel scrolls to its last byte, with every byte of it on the way.
#[test]
fn a_one_megabyte_message_scrolls_to_its_last_byte() {
    let started = std::time::Instant::now();
    let mut body = String::with_capacity(1_048_700);
    while body.len() < 1_048_576 {
        body.push_str("0123456789 abcdefghij klmnopqrst uvwxyz ");
        if body.len().is_multiple_of(7) {
            body.push('\n');
        }
    }
    body.push_str("\u{1b}[31mEND-OF-ONE-MEGABYTE");
    let mut snap = sample();
    snap.messages[0].text = body;
    let mut t = T::new(snap);
    t.select_message("lr-0007");
    t.code(KeyCode::Enter);
    let _ = t.screen();
    t.code(KeyCode::End);
    let end = t.screen();
    assert!(
        end.contains("END-OF-ONE-MEGABYTE"),
        "the last bytes:\n{end}"
    );
    assert!(end.contains("\\u{1b}[31mEND"), "{end}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    t.code(KeyCode::Esc);
    // The main screen with the 1 MB message in the store draws at once.
    let main = t.screen();
    assert!(!main.contains("0123456789"));
}

// §17::181: the panel never cuts: a line of 300 characters wraps and every character is shown, in
// order, at 80 columns.
#[test]
fn the_panel_wraps_and_never_cuts() {
    let command: String = (0..300)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap_or(0)))
        .collect();
    let mut snap = sample();
    snap.waiting[1].command = command.clone();
    let mut t = T::new(snap);
    t.at(80, 24);
    t.select(&ItemKey("call", "c2".to_owned()));
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let r = rows(&buf);
    // Reassemble the command from the rows that hold its letters.
    let mut got = String::new();
    let start = row_of(&r, "THE WHOLE CALL").expect("the heading") + 1;
    let stop = row_of(&r, "[Esc] close").expect("the panel's last row");
    for row in &r[start..stop] {
        let inner: String = row.chars().filter(char::is_ascii_lowercase).collect();
        if inner.len() >= 20 {
            got.push_str(row.trim_matches(|c: char| !c.is_ascii_lowercase()));
        }
    }
    assert_eq!(got, command);
}

// §17::185: a waiting call's panel shows the whole call, its folder, the agent and the parts that
// loosen or destroy, marked.
#[test]
fn a_waiting_calls_panel_shows_the_call_folder_agent_and_marked_parts() {
    let mut t = T::new(sample());
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let screen = rows(&buf).join("\n");
    for want in [
        "git push --force origin main",
        "/opt/epic-ai-chariot",
        "aws-codex (codex)",
        "! marked bytes 9-16: rewrites published history",
        "outside its slip",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    // The marked part is drawn apart from the rest: its cells have their own background.
    let (x, y) = t.find("--force origin");
    let marked = buf[(x, y)].style().bg;
    let plain = buf[(x + 9, y)].style().bg;
    assert_ne!(marked, plain, "{marked:?} {plain:?}");
    assert_ne!(marked, Some(Color::Reset));
    assert_eq!(plain, Some(Color::Reset));
    // A part with a range that is not on a character boundary is not marked and does not panic.
    let mut snap = sample();
    snap.waiting[0].command = "écho --force".to_owned();
    snap.waiting[0].parts = vec![
        Part {
            start: 1,
            end: 3,
            why: "bad".to_owned(),
            ..Part::default()
        },
        Part {
            start: 99,
            end: 120,
            why: "far".to_owned(),
            ..Part::default()
        },
        Part {
            start: 6,
            end: 3,
            why: "reversed".to_owned(),
            ..Part::default()
        },
    ];
    let mut t = T::new(snap);
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("écho --force"));
}

// §17::186: approve in the panel asks the passphrase and only a right one approves.
#[test]
fn approve_in_the_panel_needs_the_right_passphrase() {
    let mut t = T::new(sample());
    t.code(KeyCode::Enter);
    t.press('a');
    assert!(asking_passphrase(&t));
    let screen = t.screen();
    assert!(
        screen.contains("This approves this one exact call, once"),
        "{screen}"
    );
    assert!(screen.contains("git push --force origin main"));
    t.pass("wrong");
    assert!(t.be.performed.is_empty());
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Approve {
            number: 1,
            id: "c1".to_owned()
        }]
    );
    assert!(t.ui.detail_key().is_none());
    assert!(t.screen().contains("did APPROVE CALL 1"));
}

// §17::187-189: refuse DENIES the call: the console asks the store side to refuse that exact call.
#[test]
fn refuse_asks_the_store_side_to_deny_the_exact_call() {
    let mut t = T::new(sample());
    t.code(KeyCode::Enter);
    t.press('x');
    assert!(matches!(t.ui.modal(), Some(Modal::Confirm(_))));
    t.press('n');
    assert!(t.be.performed.is_empty());
    t.press('x');
    t.press('y');
    assert_eq!(
        t.be.performed,
        [Action::Refuse {
            number: 1,
            id: "c1".to_owned()
        }]
    );
}

// §17::190, 191: a HELD agent's panel shows each holding message, whole, with sender and time;
// release asks the passphrase.
#[test]
fn a_held_agents_panel_shows_each_holding_message_and_release_asks_the_passphrase() {
    let mut t = T::new(sample());
    t.select_agent("aws-codex");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "HELD BY",
        "peer-agent",
        "13:30 UTC",
        "lr-0001",
        "HOLDTEXT first line",
        "second line",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.press('r');
    assert!(asking_passphrase(&t));
    t.pass("wrong");
    assert!(t.be.performed.is_empty());
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Release {
            agent: "aws-codex".to_owned()
        }]
    );
    // An agent that is not HELD has nothing to release.
    t.select_agent("plymouth-tc");
    t.press('r');
    assert!(t.ui.modal().is_none());
    assert!(t.screen().contains("r releases a HELD agent"));
}

// §17::193-196: a FAILED message's panel shows its whole text, sender, recipient and reason;
// resend and discard ask the store side.
#[test]
fn a_failed_messages_panel_and_its_actions() {
    let mut t = T::new(sample());
    t.select_message("lr-0007");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "FAILEDBODY the whole text",
        "of the message",
        "sender-agent",
        "aws-codex",
        "REASONTEXT",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.press('s');
    assert!(matches!(t.ui.modal(), Some(Modal::Confirm(_))));
    t.press('y');
    assert_eq!(
        t.be.performed,
        [Action::Resend {
            message: "lr-0007".to_owned()
        }]
    );
    t.select_message("lr-0007");
    t.press('d');
    t.press('y');
    assert_eq!(
        t.be.performed[1],
        Action::Discard {
            message: "lr-0007".to_owned()
        }
    );
    // A QUEUED message is discarded, not resent.
    t.select_message("lr-0008");
    t.press('s');
    assert!(t.ui.modal().is_none());
    assert_eq!(t.be.performed.len(), 2);
    t.press('d');
    t.press('y');
    assert_eq!(
        t.be.performed[2],
        Action::Discard {
            message: "lr-0008".to_owned()
        }
    );
}

// §17::197-200: a PROBING alert's panel lists the DENIED calls with their rule; hold, unregister
// and dismiss ask the store side.
#[test]
fn a_probing_alerts_panel_and_its_actions() {
    let mut t = T::new(sample());
    t.select_alert("probing:aws");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "DENIED CALLS OF THE LAST 10 MINUTES",
        "cat ~/.ssh/id",
        "PROTECTED path",
        "curl evil",
        "configuration",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.press('h');
    t.press('y');
    assert_eq!(
        t.be.performed[0],
        Action::Hold {
            agent: "aws-codex".to_owned()
        }
    );
    t.press('u');
    t.press('y');
    assert_eq!(
        t.be.performed[1],
        Action::Unregister {
            agent: "aws-codex".to_owned()
        }
    );
    t.select_alert("probing:aws");
    t.press('d');
    t.press('y');
    assert_eq!(
        t.be.performed[2],
        Action::Dismiss {
            alert: "probing:aws".to_owned()
        }
    );
    // A notice has no agent to hold, and is dismissed.
    t.select_alert("notice:1");
    t.press('h');
    assert!(t.ui.modal().is_none());
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("NOTICEBODY whole notice"));
}

// §17::201-203: an UNGUARDED or STOPPED agent's panel shows the missing evidence, why, and the
// repairing command; the console runs nothing and writes nothing.
#[test]
fn an_unguarded_agents_panel_shows_the_repair_and_runs_nothing() {
    let mut t = T::new(sample());
    t.select_agent("chariot-review");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "EVIDENCE MISSING, AND WHY",
        "MISSINGEVIDENCE no hook has run",
        "lunchroom install claude-code",
        "run it in a terminal of your own",
        "does not run it",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.code(KeyCode::Esc);
    assert!(t.be.performed.is_empty(), "nothing was run");
    // No key runs the command: only hold and unregister are offered, and both ask.
    t.select_agent("old-session");
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("EVIDENCE MISSING"));
    t.code(KeyCode::Esc);
    assert!(t.be.performed.is_empty());
}

// §17::204-208: a failed check's panel shows the check and the evidence; a fix inside the store
// is done from the console, any other is shown and not run.
#[test]
fn a_failed_checks_panel_and_its_fixes() {
    let mut t = T::new(sample());
    t.select_check("check-cmd");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "CHECK FAILED",
        "check-cmd",
        "EVIDENCETEXT of check-cmd",
        "lunchroom install console",
        "does not run it",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    // The command is not run by any key.
    for c in ['d', 'r', 's'] {
        t.press(c);
    }
    assert!(t.ui.modal().is_none());
    assert!(t.be.performed.is_empty());
    t.code(KeyCode::Esc);
    // Discard a message to an agent no longer named.
    t.select_check("check-discard");
    t.press('d');
    t.press('y');
    assert_eq!(
        t.be.performed[0],
        Action::Discard {
            message: "lr-0009".to_owned()
        }
    );
    // Release a hold whose message the store no longer holds: the passphrase.
    t.select_check("check-release");
    t.press('r');
    assert!(asking_passphrase(&t));
    t.pass(PASS);
    assert_eq!(
        t.be.performed[1],
        Action::ReleaseStale {
            agent: "aws-codex".to_owned()
        }
    );
    // Clear a marked path: the passphrase.
    t.select_check("check-clear");
    t.press('d');
    assert!(asking_passphrase(&t));
    t.pass(PASS);
    assert_eq!(
        t.be.performed[2],
        Action::Clear {
            path: "/home/dev/.ssh/key".to_owned()
        }
    );
    // A check with no fix names none.
    t.select_check("check-none");
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("names no fix"));
}

// §17::209, 210: a marked path's panel shows the path, since when and the call; clear asks the
// passphrase.
#[test]
fn a_marked_paths_panel_and_clear() {
    let mut t = T::new(sample());
    t.select(&ItemKey("path", "/home/dev/.ssh/key".to_owned()));
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [
        "MARKED PATH",
        "/home/dev/.ssh/key",
        "12:55 UTC",
        "MARKCALL cat /home/dev/.ssh/key",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.press('d');
    assert!(asking_passphrase(&t));
    t.pass("wrong");
    assert!(t.be.performed.is_empty());
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Clear {
            path: "/home/dev/.ssh/key".to_owned()
        }]
    );
}

// §17::211, 215-227: each key runs its action.
#[test]
fn each_key_runs_its_action() {
    // a, x on a call.
    let mut t = T::new(sample());
    t.press('a');
    assert_eq!(modal_title(&t).as_deref(), Some("APPROVE CALL 1"));
    t.code(KeyCode::Esc);
    t.press('x');
    assert_eq!(modal_title(&t).as_deref(), Some("REFUSE CALL 1"));
    t.press('n');
    // r on a HELD agent, h and u on an agent.
    t.select_agent("aws-codex");
    t.press('r');
    assert_eq!(modal_title(&t).as_deref(), Some("RELEASE aws-codex"));
    t.code(KeyCode::Esc);
    t.press('h');
    assert_eq!(modal_title(&t).as_deref(), Some("HOLD aws-codex"));
    t.press('n');
    t.press('u');
    assert_eq!(modal_title(&t).as_deref(), Some("UNREGISTER aws-codex"));
    t.press('n');
    // s and d on a FAILED message.
    t.select_message("lr-0007");
    t.press('s');
    assert_eq!(modal_title(&t).as_deref(), Some("RESEND lr-0007"));
    t.press('n');
    t.press('d');
    assert_eq!(modal_title(&t).as_deref(), Some("DISCARD lr-0007"));
    t.press('n');
    // m, y, c.
    t.press('m');
    assert_eq!(modal_title(&t).as_deref(), Some("MODE"));
    t.code(KeyCode::Esc);
    t.press('y');
    assert_eq!(modal_title(&t).as_deref(), Some("YOLO"));
    t.code(KeyCode::Esc);
    t.press('c');
    assert_eq!(modal_title(&t).as_deref(), Some("CONTROLLER"));
    t.code(KeyCode::Esc);
    // Arrows choose, Enter opens, Esc closes.
    let first = t.ui.selected().cloned();
    t.code(KeyCode::Down);
    assert_ne!(t.ui.selected().cloned(), first);
    t.code(KeyCode::Up);
    assert_eq!(t.ui.selected().cloned(), first);
    t.code(KeyCode::Enter);
    assert!(t.ui.detail_key().is_some());
    t.code(KeyCode::Esc);
    assert!(t.ui.detail_key().is_none());
    // l locks, q quits.
    t.press('l');
    assert!(t.ui.locked());
    let mut t = T::new(sample());
    t.press('q');
    assert_eq!(t.ui.ending(), Some(Ending::Quit));
    // A key that does not apply to the chosen item says so, and nothing opens.
    let mut t = T::new(sample());
    t.select_agent("plymouth-tc");
    t.press('s');
    assert!(t.ui.modal().is_none());
    assert!(t.screen().contains("s resends a FAILED message"));
}

fn state_of(t: &T) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}",
        t.ui.modal(),
        t.ui.detail_key(),
        t.ui.locked(),
        t.ui.ending(),
        t.ui.message(),
        t.ui.selected(),
        t.be.performed
    )
}

// §17::228, 229: a click on each key of the key line runs the same action as the key, with the
// same result, for every key.
#[test]
fn each_click_on_the_key_line_runs_the_same_action_as_its_key() {
    let cmds = [
        (Cmd::Up, "agent", "aws-codex"),
        (Cmd::Down, "call", "c1"),
        (Cmd::Open, "call", "c1"),
        (Cmd::Close, "call", "c1"),
        (Cmd::Approve, "call", "c1"),
        (Cmd::Refuse, "call", "c2"),
        (Cmd::Release, "agent", "aws-codex"),
        (Cmd::Resend, "message", "lr-0007"),
        (Cmd::Discard, "message", "lr-0007"),
        (Cmd::Hold, "agent", "aws-codex"),
        (Cmd::Unregister, "agent", "plymouth-tc"),
        (Cmd::Mode, "call", "c1"),
        (Cmd::Yolo, "call", "c1"),
        (Cmd::Controller, "call", "c1"),
        (Cmd::Lock, "call", "c1"),
        (Cmd::Quit, "call", "c1"),
    ];
    for (w, h) in [(120u16, 40u16), (80, 24)] {
        for (cmd, kind, name) in cmds {
            let mut by_key = T::new(sample());
            let mut by_click = T::new(sample());
            by_key.at(w, h);
            by_click.at(w, h);
            by_key.select(&ItemKey(kind_static(kind), name.to_owned()));
            by_click.select(&ItemKey(kind_static(kind), name.to_owned()));
            // The same state before: draw so that the clicks are recorded.
            let _ = by_click.draw();
            by_key.code(key_of(cmd));
            let (row, x) = key_place(w, cmd).expect("a place on the key line");
            let y = h - key_rows(w) + row;
            assert_eq!(key_at(w, h - key_rows(w), x, y), Some(key_of(cmd)));
            by_click.click(x, y);
            assert_eq!(state_of(&by_key), state_of(&by_click), "{cmd:?} at {w}x{h}");
        }
    }
}

fn kind_static(kind: &str) -> &'static str {
    match kind {
        "agent" => "agent",
        "message" => "message",
        _ => "call",
    }
}

// §17::229: every key of the key line is a click target that is drawn: the line names all sixteen.
#[test]
fn the_key_line_names_every_key() {
    for (w, h) in [(120, 40), (80, 24), (60, 20)] {
        let mut t = T::new(sample());
        let r = rows(&t.at(w, h).draw());
        let keys = r[usize::from(h - key_rows(w))..].join(" ");
        for word in [
            "choose",
            "Enter open",
            "Esc close",
            "a approve",
            "x refuse",
            "r release",
            "s resend",
            "d discard",
            "h hold",
            "u unregister",
            "m mode",
            "y yolo",
            "c controller",
            KEY_L,
            "q quit",
        ] {
            assert!(keys.contains(word), "{word} at {w}x{h}:\n{keys}");
        }
        assert!(keys.contains('↑') && keys.contains('↓'));
    }
}

// A click on each cell of the strip selects the first item it counts.
#[test]
fn a_click_on_a_strip_cell_selects_what_it_counts() {
    let want = [
        Some(ItemKey("call", "c1".to_owned())),
        Some(ItemKey("alert", "probing:aws".to_owned())),
        Some(ItemKey("message", "lr-0007".to_owned())),
        Some(ItemKey("agent", "chariot-review".to_owned())),
        None,
        Some(ItemKey(
            "check",
            "check-cmd|EVIDENCETEXT of check-cmd".to_owned(),
        )),
    ];
    let edges = cell_edges(120);
    for (i, want) in want.iter().enumerate() {
        let mut t = T::new(sample());
        t.select_agent("old-session");
        let before = t.ui.selected().cloned();
        let _ = t.draw();
        t.click(edges[i] + 3, 2);
        match want {
            Some(key) => assert_eq!(t.ui.selected(), Some(key), "cell {i}"),
            None => assert_eq!(t.ui.selected().cloned(), before, "cell {i}"),
        }
        assert!(t.be.performed.is_empty());
    }
}

// A click on a question's own keys, and the wheel, work; a click outside a question does nothing.
#[test]
fn clicks_in_a_question_press_its_keys() {
    let mut t = T::new(sample());
    t.press('a');
    let (x, y) = t.find("Esc cancel");
    t.click(x, y);
    assert!(t.ui.modal().is_none());
    t.press('x');
    let (x, y) = t.find("y yes");
    t.click(x, y);
    assert_eq!(
        t.be.performed,
        [Action::Refuse {
            number: 1,
            id: "c1".to_owned()
        }]
    );
    // A click outside the question keeps it.
    t.press('x');
    t.click(2, 12);
    assert!(t.ui.modal().is_some());
    t.press('n');
    // The mode question: a click on `balanced` asks the passphrase.
    t.press('m');
    let (x, y) = t.find("b balanced");
    t.click(x, y);
    assert!(asking_passphrase(&t));
}

// ---------------------------------------------------------------------------------------------
// Item 9: controller approvals.
// ---------------------------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = crate::test_target()
        .join("console-unit")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("requests"));
    dir
}

/// Writes a request as the controller would have just written it, at the time the tests act at.
fn write_request(dir: &std::path::Path, run: &str, phase: &str, hash: &str) {
    let path = dir.join("requests").join(controller::file_name(run, phase));
    std::fs::write(
        &path,
        format!(r#"{{"run_id":"{run}","phase":"{phase}","manifest_hash":"{hash}"}}"#),
    )
    .expect("a request file");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("the request")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW))
        .expect("a time");
}

fn with_files(dir: &std::path::Path) -> T {
    let key = dir.join("key");
    std::fs::write(&key, crate::install::console_keys::private_text(&[5; 32])).expect("a key");
    let mut t = T::new(sample());
    t.be.files = Some(ControllerPaths {
        requests: dir.join("requests"),
        approvals: dir.to_path_buf(),
        key,
    });
    t
}

fn approvals_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("a folder")
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("run-"))
        .collect();
    v.sort();
    v
}

// §17::122, 127, 131-133, 224, 225: `c` lists the requests with run id, phase and the plan hash in
// full; choosing one shows the three and asks y/N, then the passphrase, and writes the approval.
#[test]
fn c_lists_the_requests_and_choosing_one_approves_it() {
    let dir = scratch("c-list");
    write_request(&dir, RUN, "deploy-prod", HASH);
    write_request(&dir, RUN, "build", &"a".repeat(64));
    std::fs::write(dir.join("requests/junk"), "x").expect("a file");
    let mut t = with_files(&dir);
    t.press('c');
    let screen = t.screen();
    for want in [
        RUN,
        "build",
        "deploy-prod",
        HASH,
        &"a".repeat(64),
        "1 file(s) in requests/ are not valid requests",
    ] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    // Choose the second (`deploy-prod`): the screen shows the three and asks y/N.
    t.code(KeyCode::Down);
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for want in [RUN, "deploy-prod", HASH, "Approve this phase? y/N"] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    t.press('y');
    assert!(asking_passphrase(&t));
    t.pass("wrong");
    assert!(approvals_in(&dir).is_empty());
    t.pass(PASS);
    assert_eq!(
        approvals_in(&dir),
        [controller::file_name(RUN, "deploy-prod")]
    );
    assert!(t.screen().contains("approved"));
    let file: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join(controller::file_name(RUN, "deploy-prod"))).expect("the approval"),
    )
    .expect("json");
    assert_eq!(file["record"]["manifest_hash"], HASH);
    assert_eq!(file["record"]["source"], "console");
    // A click on a listed request chooses it too.
    t.advance(60);
    t.press('c');
    let (x, y) = t.find("build");
    t.click(x, y);
    assert!(t.screen().contains("Approve this phase? y/N"));
    t.press('n');
    assert_eq!(approvals_in(&dir).len(), 1);
}

// §17::122, 125, 126: `a-controller <run-id> <phase>` starts an approval; a malformed run id or
// phase is refused and nothing is written.
#[test]
fn a_controller_key_is_typed_and_a_malformed_one_is_refused() {
    let dir = scratch("a-controller");
    write_request(&dir, RUN, "deploy-prod", HASH);
    let mut t = with_files(&dir);
    t.press('c');
    t.typed(&format!("a-controller {RUN} deploy-prod"));
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(
        screen.contains(HASH) && screen.contains("Approve this phase? y/N"),
        "{screen}"
    );
    t.press('y');
    t.pass(PASS);
    assert_eq!(approvals_in(&dir).len(), 1);
    // Malformed.
    let run_upper = RUN.to_uppercase();
    let bad = [
        "a-controller run-xyz deploy-prod".to_owned(),
        format!("a-controller {RUN}0 deploy-prod"),
        format!("a-controller {run_upper} deploy-prod"),
        format!("a-controller {RUN} .hidden"),
        format!("a-controller {RUN} {}", "p".repeat(65)),
        format!("a-controller {RUN}"),
        format!("controller {RUN} deploy-prod"),
        format!("a-controller {RUN} deploy-prod extra"),
    ];
    let before = approvals_in(&dir);
    t.advance(60);
    t.press('c');
    for line in bad {
        t.typed(&line);
        t.code(KeyCode::Enter);
        let screen = t.screen();
        assert!(!screen.contains("Approve this phase?"), "{line}\n{screen}");
        assert!(
            matches!(t.ui.modal(), Some(Modal::Controller(s)) if !s.error.is_empty()),
            "{line}"
        );
        // Clear the input for the next one.
        if let Some(Modal::Controller(s)) = t.ui.modal.as_mut() {
            s.input.clear();
        }
        assert!(!asking_passphrase(&t));
    }
    assert_eq!(approvals_in(&dir), before);
    assert_eq!(
        parse_controller("a-controller r p"),
        Ok(("r".to_owned(), "p".to_owned()))
    );
    assert!(parse_controller("a-controller").is_err());
}

// §17::128-130: a request that is missing, holds another field, names another run or phase is
// refused and nothing is written.
#[test]
fn a_wrong_request_is_refused_and_nothing_is_written() {
    let dir = scratch("wrong-request");
    let mut t = with_files(&dir);
    // Missing.
    t.press('c');
    t.typed(&format!("a-controller {RUN} deploy-prod"));
    t.code(KeyCode::Enter);
    assert!(!t.screen().contains("Approve this phase?"));
    t.code(KeyCode::Esc);
    // Another field, another run, another phase, a bad hash.
    let name = controller::file_name(RUN, "deploy-prod");
    let other = "run-ffffffffffffffffffffffffffffffff";
    for text in [
        format!(r#"{{"run_id":"{RUN}","phase":"deploy-prod","manifest_hash":"{HASH}","x":1}}"#),
        format!(r#"{{"run_id":"{other}","phase":"deploy-prod","manifest_hash":"{HASH}"}}"#),
        format!(r#"{{"run_id":"{RUN}","phase":"other","manifest_hash":"{HASH}"}}"#),
        format!(
            r#"{{"run_id":"{RUN}","phase":"deploy-prod","manifest_hash":"{}"}}"#,
            HASH.to_uppercase()
        ),
        "not json".to_owned(),
    ] {
        std::fs::write(dir.join("requests").join(&name), text).expect("a request");
        t.advance(1);
        t.press('c');
        t.typed(&format!("a-controller {RUN} deploy-prod"));
        t.code(KeyCode::Enter);
        assert!(!t.screen().contains("Approve this phase?"));
        t.code(KeyCode::Esc);
    }
    assert!(approvals_in(&dir).is_empty());
    assert!(t.be.performed.is_empty());
}

// §17::134, 142, 160: two approvals a minute apart each ask the passphrase and carry different
// nonces; the later replaces the earlier; a release phase asks like any other.
#[test]
fn two_controller_approvals_each_ask_and_the_later_replaces_the_earlier() {
    let dir = scratch("two-approvals");
    write_request(&dir, RUN, "release", HASH);
    let mut t = with_files(&dir);
    let mut nonces = Vec::new();
    for n in 0..2 {
        t.press('c');
        t.code(KeyCode::Enter);
        t.press('y');
        assert!(asking_passphrase(&t), "approval {n} asks");
        t.pass(PASS);
        let file: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join(controller::file_name(RUN, "release"))).expect("the approval"),
        )
        .expect("json");
        nonces.push(
            file["record"]["nonce"]
                .as_str()
                .expect("a nonce")
                .to_owned(),
        );
        t.advance(60);
    }
    assert_ne!(nonces[0], nonces[1]);
    assert_eq!(approvals_in(&dir).len(), 1);
}

// A request that an agent rewrites between the screen and the signing is refused: the record holds
// the plan hash shown.
#[test]
fn a_request_rewritten_after_it_was_shown_signs_nothing() {
    let dir = scratch("rewritten");
    write_request(&dir, RUN, "deploy-prod", HASH);
    let mut t = with_files(&dir);
    t.press('c');
    t.code(KeyCode::Enter);
    t.press('y');
    write_request(&dir, RUN, "deploy-prod", &"f".repeat(64));
    t.pass(PASS);
    assert!(approvals_in(&dir).is_empty());
    assert!(t.screen().contains("changed after it was shown"));
}

// No request in requests/: the screen says so, and Enter chooses nothing.
#[test]
fn c_with_no_request_says_so() {
    let dir = scratch("none");
    let mut t = with_files(&dir);
    t.press('c');
    let screen = t.screen();
    assert!(
        screen.contains("No approval request is waiting"),
        "{screen}"
    );
    t.code(KeyCode::Enter);
    assert!(
        matches!(t.ui.modal(), Some(Modal::Controller(s)) if s.error.contains("no approval request"))
    );
    // A folder that cannot be listed says why.
    let mut t = T::new(sample());
    t.be.files = Some(ControllerPaths {
        requests: dir.join("missing"),
        approvals: dir.clone(),
        key: dir.join("key"),
    });
    t.press('c');
    assert!(t.screen().contains("cannot be listed"));
}

// ---------------------------------------------------------------------------------------------
// The real backend and the snapshot of the store.
// ---------------------------------------------------------------------------------------------

/// The system the real backend's commands run on in a test.
struct Base;

impl System for Base {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        Vec::new()
    }
    fn own_tty(&self) -> u64 {
        5
    }
    fn tty_of(&self, _key: &str) -> Option<u64> {
        None
    }
    fn ask(&mut self, _q: &str) -> Option<String> {
        None
    }
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn verifier(&self) -> Result<String, String> {
        Ok(fake::verifier())
    }
    fn ask_secret(&mut self, _p: &str) -> Option<String> {
        None
    }
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        Ok([3; passphrase::SALT_BYTES])
    }
    fn install_verifier(&mut self, _l: &str) -> Result<(), String> {
        Ok(())
    }
}

struct TestFeed {
    records: Mutex<Vec<Record>>,
    checks: Vec<String>,
}

impl Feed for TestFeed {
    fn records(&self, _env: &Env) -> Vec<Record> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }
    fn checks(&self, _env: &Env, _mail: &Mail, _base: &dyn System) -> Vec<String> {
        self.checks.clone()
    }
}

struct World {
    env: Env,
    feed: TestFeed,
}

impl World {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let mut env = Env::console_view(1000, root.join("home"));
        env.lunchroom_home = root.join("lunchroom");
        let _ = std::fs::create_dir_all(&env.home);
        Self {
            env,
            feed: TestFeed {
                records: Mutex::new(Vec::new()),
                checks: vec!["PASS test checks".to_owned()],
            },
        }
    }

    fn store(&self) -> Option<Store<Mail>> {
        Store::<Mail>::open(&self.env.store_dir()).ok()
    }

    fn name(&self, name: &str, session: &str) {
        let who = Identity::new(Harness::Codex, session);
        if let Some(store) = self.store() {
            let _ = store.update(|m| m.register(name, &who, Origin::User, None));
        }
    }

    fn mail(&self) -> Mail {
        Store::<Mail>::read_without_change(&self.env.store_dir()).unwrap_or_default()
    }
}

fn denied(at: u64, session: &str, n: u64) -> Record {
    Record {
        at,
        session: session.to_owned(),
        hook: records::DENIED_HOOK.to_owned(),
        call: String::new(),
        step: "configuration".to_owned(),
        step_ms: n,
        took_ms: 0,
        undecided: None,
    }
}

// The real passphrase check: the fake user's passphrase passes and nothing else does.
#[test]
fn the_real_backend_checks_the_developers_passphrase() {
    let w = World::new("backend-pass");
    let mut base = Base;
    let mut be = RealBackend {
        env: w.env.clone(),
        base: &mut base,
        uid: 1000,
        developer: "dev".to_owned(),
        lunchroom_uid: Some(LUNCHROOM_UID),
        controller: None,
    };
    assert!(be.check_passphrase(PASS));
    assert!(!be.check_passphrase("not it"));
    assert!(!be.check_passphrase(""));
    // F2-AC52: a decline with no call name declines nothing.
    let out = be.perform(
        &Action::Refuse {
            number: 1,
            id: "x".to_owned(),
        },
        NOW,
    );
    assert!(
        !out.done && out.text.contains("nothing was declined"),
        "{}",
        out.text
    );
    // The store actions (F2-AC54, AC55, AC57), given a message, agent or alert the store does not
    // have, are not done and record nothing.
    let lines = w.mail().lines().len();
    for action in [
        Action::Discard {
            message: "m".to_owned(),
        },
        Action::Hold {
            agent: "a".to_owned(),
        },
        Action::Dismiss {
            alert: "x".to_owned(),
        },
        Action::ReleaseStale {
            agent: "a".to_owned(),
        },
    ] {
        let out = be.perform(&action, NOW);
        assert!(!out.done, "{action:?}: {}", out.text);
    }
    assert_eq!(w.mail().lines().len(), lines, "nothing was recorded");
    // F2-AC58: Clear runs `lunchroom clear <path>`: a path nothing marked gets clear's own answer.
    let out = be.perform(
        &Action::Clear {
            path: "/p".to_owned(),
        },
        NOW,
    );
    assert!(
        !out.done && out.text.contains("/p is not a recorded path"),
        "{}",
        out.text
    );
    // An approve whose call is no longer there approves nothing.
    let out = be.perform(
        &Action::Approve {
            number: 1,
            id: "gone#x".to_owned(),
        },
        NOW,
    );
    assert!(
        !out.done && out.text.contains("no longer waiting"),
        "{}",
        out.text
    );
}

// The real backend runs the controller approval over real files.
#[test]
fn the_real_backend_signs_a_controller_approval() {
    let w = World::new("backend-controller");
    let dir = scratch("backend-controller-files");
    write_request(&dir, RUN, "deploy-prod", HASH);
    // The screen's load goes by the real clock: the request is as new as the controller made it.
    std::fs::OpenOptions::new()
        .write(true)
        .open(
            dir.join("requests")
                .join(controller::file_name(RUN, "deploy-prod")),
        )
        .expect("the request")
        .set_modified(std::time::SystemTime::now())
        .expect("a time");
    let key = dir.join("key");
    std::fs::write(&key, crate::install::console_keys::private_text(&[8; 32])).expect("a key");
    // The real chown: to the owner this test already is (any process may), so it is really called.
    let owner = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&dir).expect("a folder"));
    let mut base = Base;
    let mut be = RealBackend {
        env: w.env.clone(),
        base: &mut base,
        uid: 1000,
        developer: "alice".to_owned(),
        lunchroom_uid: Some(owner),
        controller: Some(ControllerPaths {
            requests: dir.join("requests"),
            approvals: dir.clone(),
            key,
        }),
    };
    assert_eq!(be.controller_requests().requests.len(), 1);
    let shown = be.controller_load(RUN, "deploy-prod").expect("the request");
    assert_eq!(shown.manifest_hash, HASH);
    assert!(be.controller_load(RUN, "../etc").is_err());
    let out = be.perform(
        &Action::ControllerApprove {
            run_id: RUN.to_owned(),
            phase: "deploy-prod".to_owned(),
            manifest_hash: HASH.to_owned(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    assert_eq!(approvals_in(&dir).len(), 1);
    // The hash shown must be the one in the file.
    let out = be.perform(
        &Action::ControllerApprove {
            run_id: RUN.to_owned(),
            phase: "deploy-prod".to_owned(),
            manifest_hash: "b".repeat(64),
        },
        NOW,
    );
    assert!(!out.done);
}

// §17.10: a HELD agent's panel shows each message that holds it, whole: an arrival Lunchroom did not
// deliver (an OpenClaw prompt the user did not type) shows its kept text and its time; one whose
// text was not kept shows none (found by the console proof).
#[test]
fn a_held_agents_panel_shows_the_text_of_an_arrival_lunchroom_did_not_deliver() {
    let w = World::new("unrecorded");
    w.name("kimi", "k1");
    let who = Identity::new(Harness::Codex, "k1");
    let store = w.store().expect("store");
    let _ = store.update(|m| {
        m.hold_unrecorded_text(&who, "not-typed", "p1", "hello from another agent", NOW - 5);
        m.hold_unrecorded(&who, "tool-output", "t1");
    });
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let row = snap.agent("kimi").expect("kimi");
    let texts: Vec<(&str, &str, u64)> = row
        .held_by
        .iter()
        .map(|h| (h.id.as_str(), h.text.as_str(), h.at))
        .collect();
    assert_eq!(
        texts,
        [
            (
                "not-typed:p1",
                "hello from another agent\n(Lunchroom did not deliver this text; it was kept with \
                 every secret replaced)",
                NOW - 5
            ),
            ("tool-output:t1", "", 0)
        ]
    );
}

// F2 design, "Screen": in the alerts box what the user acts on comes first (PROBING, the QUEUED
// and FAILED messages, a failing check, a marked path) and the notices last, so a box full of
// notices never pushes a message or a failing check off the screen (found by the console proof).
#[test]
fn the_alerts_box_puts_what_needs_the_user_before_the_notices() {
    let alert = |id: &str, kind| AlertRow {
        id: id.to_owned(),
        kind,
        ..AlertRow::default()
    };
    let message = |id: &str, kind| MessageRow {
        id: id.to_owned(),
        kind,
        ..MessageRow::default()
    };
    let snap = Snapshot {
        alerts: vec![
            alert("n1", AlertKind::Notice),
            alert("p1", AlertKind::Probing),
            alert("n2", AlertKind::Notice),
        ],
        messages: vec![
            message("q1", MessageKind::Queued),
            message("f1", MessageKind::Failed),
        ],
        checks: vec![
            CheckRow {
                ok: true,
                name: "passes".to_owned(),
                ..CheckRow::default()
            },
            CheckRow {
                ok: false,
                name: "fails".to_owned(),
                ..CheckRow::default()
            },
        ],
        marked: vec![MarkedRow {
            path: "/m".to_owned(),
            ..MarkedRow::default()
        }],
        ..Snapshot::default()
    };
    let order: Vec<String> = snap
        .attention()
        .iter()
        .map(|i| match i {
            Item::Alert(a) => a.id.clone(),
            Item::Message(m) => m.id.clone(),
            Item::Check(c) => c.name.clone(),
            Item::Marked(m) => m.path.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(order, ["p1", "q1", "f1", "fails", "/m", "n1", "n2"]);
}

// decision of 2026-11-14 ("Add the check"): /usr/local/lib/lunchroom and the setgid program in it must be
// root's and writable by no one else; the one at fault is named, with what to do. Folders above it
// are not part of the check.
#[test]
fn the_program_and_its_folder_are_roots_alone() {
    use std::path::Path;
    let all = |p: &Path| -> Option<(u32, u32)> {
        let _ = p;
        Some((0, 0o40_755))
    };
    assert!(super::program_place_with(&all).is_ok());
    let writable_dir = |p: &Path| {
        Some((
            0,
            if p == Path::new("/usr/local/lib/lunchroom") {
                0o40_775
            } else {
                0o40_755
            },
        ))
    };
    let err = super::program_place_with(&writable_dir)
        .err()
        .unwrap_or_default();
    assert!(
        err.starts_with("/usr/local/lib/lunchroom is owned by user id 0 with mode 775"),
        "{err}"
    );
    let users_program = |p: &Path| {
        Some((
            if p == Path::new("/usr/local/lib/lunchroom/lunchroom") {
                1000
            } else {
                0
            },
            0o102_755,
        ))
    };
    let err = super::program_place_with(&users_program)
        .err()
        .unwrap_or_default();
    assert!(
        err.starts_with("/usr/local/lib/lunchroom/lunchroom is owned by user id 1000"),
        "{err}"
    );
    let other_writable_usr = |p: &Path| {
        Some((
            0,
            if p == Path::new("/usr") {
                0o40_757
            } else {
                0o40_755
            },
        ))
    };
    // A folder above /usr/local/lib/lunchroom is not part of the check.
    assert!(super::program_place_with(&other_writable_usr).is_ok());
    let unreadable = |_: &Path| None;
    assert!(
        super::program_place_with(&unreadable)
            .err()
            .unwrap_or_default()
            .ends_with("cannot be read")
    );
}

// decision of 2026-11-14 ("Both always visible"): a full alerts box shows the items to act on above a
// line counting the notices not shown and the newest notices, at least two and as many as the rows
// left; the selected item to act on scrolls into its part; a notice selected below those scrolls
// the whole list; a box with room shows every item.
#[test]
fn a_full_alerts_box_keeps_the_newest_notices_and_the_items_to_act_on() {
    let snap = |actions: usize, notices: usize| Snapshot {
        messages: (0..actions)
            .map(|i| MessageRow {
                id: format!("m{i}"),
                ..MessageRow::default()
            })
            .collect(),
        alerts: (0..notices)
            .map(|i| AlertRow {
                id: format!("n{i}"),
                kind: AlertKind::Notice,
                ..AlertRow::default()
            })
            .collect(),
        ..Snapshot::default()
    };
    let rows = |s: &Snapshot, height, at| alert_rows(&s.attention(), height, at);
    let some = |r: std::ops::Range<usize>| r.map(Some).collect::<Vec<_>>();
    // Room for all: every item.
    assert_eq!(rows(&snap(3, 4), 9, None), some(0..7));
    // 7 to act on, 20 notices, 9 rows: 6 to act on, the count, the 2 newest notices.
    let full = snap(7, 20);
    let mut want = some(0..6);
    want.push(None);
    want.extend(some(7..9));
    assert_eq!(rows(&full, 9, None), want);
    // The 7th selected: its part scrolls to it; the notices stay.
    let mut want = some(1..7);
    want.push(None);
    want.extend(some(7..9));
    assert_eq!(rows(&full, 9, Some(6)), want);
    // 2 to act on: the notices take the rows they leave (6 of them).
    let few = snap(2, 20);
    let mut want = some(0..2);
    want.push(None);
    want.extend(some(2..8));
    assert_eq!(rows(&few, 9, None), want);
    // A notice selected below those shown: the whole list scrolls to it.
    assert_eq!(rows(&full, 9, Some(20)), some(12..21));
    // The line counts the notices not shown, never an item to act on scrolled out of view
    // (Codex review of 5a75a15: 5 to act on, 10 notices, 4 rows said 12, not 8).
    let s = snap(5, 10);
    let items = s.attention();
    let r = alert_rows(&items, 4, None);
    assert_eq!(r, [Some(0), None, Some(5), Some(6)]);
    assert_eq!(hidden_notices(&items, &r), 8);
}

// F2 design, "The agents on the console": the console as the user `lunchroom` reads neither the
// developer's home folder nor their processes' programs. It tells STOPPED itself, with the commands
// that repair it; otherwise it shows what the developer's own program found of the process that
// runs now, and an agent never judged for it is not known, with `lunchroom status` to judge it.
#[test]
fn the_console_as_lunchroom_shows_what_the_developers_program_found() {
    use lunchroom_core::mail::{Found, Judged};
    let w = World::new("judged");
    w.name("alpha", "A1");
    w.name("beta", "B1");
    let beta = Identity::new(Harness::Codex, "B1");
    let here = crate::process::key(std::process::id()).expect("this process's key");
    let store = w.store().expect("store");
    let _ = store.update(|m| m.observe_process(&here, std::path::Path::new("/"), &beta));
    let mut live =
        Live::new(w.env.clone(), "dev".to_owned(), &w.feed).viewed_by_the_lunchroom_user();
    let snap = live.snapshot(NOW, &Base, true);
    // alpha has no running process: STOPPED, told by the console itself, with its repair.
    let alpha = snap.agent("alpha").expect("alpha");
    assert_eq!(alpha.state, AgentState::Stopped);
    let repair = alpha.repair.clone().unwrap_or_default();
    assert!(repair.contains("codex resume A1"), "{repair}");
    assert!(repair.contains("lunchroom unregister alpha"), "{repair}");
    // beta runs, never judged: not known, and `lunchroom status` judges it.
    let row = snap.agent("beta").expect("beta");
    assert_eq!(row.state, AgentState::Unknown);
    assert_eq!(row.repair.as_deref(), Some("lunchroom status"));
    // Judged UNGUARDED for the process that runs now: UNGUARDED, with the reason and the install.
    let judged = |found, why: &str, process: &str| Judged {
        found,
        why: why.to_owned(),
        process: Some(process.to_owned()),
        at: NOW - 60,
    };
    let _ = store.update(|m| {
        m.set_judged(
            &beta,
            judged(Found::Unguarded, "no guard hook has run", &here),
        );
    });
    let snap = live.snapshot(NOW, &Base, true);
    let row = snap.agent("beta").expect("beta");
    assert_eq!(row.state, AgentState::Unguarded);
    assert!(
        row.missing.contains("no guard hook has run"),
        "{}",
        row.missing
    );
    assert_eq!(row.repair.as_deref(), Some("lunchroom install codex"));
    // A judgment of another process is not this session's: not known again.
    let _ = store.update(|m| m.set_judged(&beta, judged(Found::Guarded, "", "1:1")));
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(snap.agent("beta").expect("beta").state, AgentState::Unknown);
    // Judged guarded for this process: AVAILABLE.
    let _ = store.update(|m| m.set_judged(&beta, judged(Found::Guarded, "", &here)));
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(
        snap.agent("beta").expect("beta").state,
        AgentState::Available
    );
    // decision of 2026-11-14 ("Hooks + age limit"): over 2 minutes old, the finding shows AVAILABLE no
    // more: not known, with the time it was found and `lunchroom status` to judge it now.
    let at = |when: u64| {
        let mut j = judged(Found::Guarded, "", &here);
        j.at = when;
        j
    };
    let _ = store.update(|m| m.set_judged(&beta, at(NOW - 121)));
    let snap = live.snapshot(NOW, &Base, true);
    let row = snap.agent("beta").expect("beta");
    assert_eq!(row.state, AgentState::Unknown);
    assert!(
        row.missing
            .contains("last found this agent's guard running at"),
        "{}",
        row.missing
    );
    assert_eq!(row.repair.as_deref(), Some("lunchroom status"));
    // At exactly 2 minutes it still shows AVAILABLE.
    let _ = store.update(|m| m.set_judged(&beta, at(NOW - 120)));
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(
        snap.agent("beta").expect("beta").state,
        AgentState::Available
    );
}

// Five DENIED calls in ten minutes from one agent: the snapshot shows PROBING, and the user is told
// once, as a notice that is then marked told; a locked screen tells none.
#[test]
fn a_probing_agent_raises_one_notice() {
    let w = World::new("probe");
    w.name("kimi", "k1");
    {
        let mut records = w.feed.records.lock().unwrap();
        for n in 0..5 {
            records.push(denied(NOW - 60 * n, "k1", n));
        }
        records.push(denied(NOW - 3_600, "k1", 99));
    }
    let base = Base;
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    // Locked: the strip counts the alert, no notice is marked told.
    let snap = live.snapshot(NOW, &base, false);
    assert_eq!(strip(&snap)[1].text, "PROBING 1");
    assert_eq!(strip(&snap)[1].level, Level::Red);
    assert_eq!(
        w.mail().user_notices().len(),
        1,
        "the notice is kept for when it can be shown"
    );
    // Shown: the notice is on the screen and told.
    let snap = live.snapshot(NOW + 1, &base, true);
    let probing = snap
        .alerts
        .iter()
        .find(|a| a.kind == AlertKind::Probing)
        .expect("the alert");
    assert_eq!(probing.agent, "kimi");
    assert!(
        probing.line.contains("5 DENIED in 10 min"),
        "{}",
        probing.line
    );
    assert_eq!(probing.calls.len(), 5);
    let notice = snap
        .alerts
        .iter()
        .find(|a| a.kind == AlertKind::Notice)
        .expect("the notice");
    assert!(
        notice
            .text
            .contains("kimi had 5 DENIED calls in the last 10 minutes"),
        "{}",
        notice.text
    );
    assert_eq!(w.mail().user_notices(), []);
    // The agent's row carries its hour of DENIED decisions.
    let row = snap.agent("kimi").expect("kimi");
    assert_eq!(row.denied_hour, 5);
    assert_eq!(
        row.denied_spans
            .iter()
            .map(|n| usize::from(*n))
            .sum::<usize>(),
        5
    );
    // Four calls make no alert.
    let w = World::new("probe-4");
    {
        let mut records = w.feed.records.lock().unwrap();
        for n in 0..4 {
            records.push(denied(NOW - n, "k1", n));
        }
    }
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(strip(&snap)[1].level, Level::Dark);
}

// The guard's slow decisions and timeouts of the last hour light GUARD SLOW amber and red.
#[test]
fn the_snapshot_counts_slow_decisions_and_timeouts_of_the_last_hour() {
    let w = World::new("guard-slow");
    let rec = |at: u64, took: u64, undecided: Option<&str>| Record {
        at,
        session: "s".to_owned(),
        hook: "pretool".to_owned(),
        call: "c".to_owned(),
        step: "s".to_owned(),
        step_ms: 1,
        took_ms: took,
        undecided: undecided.map(str::to_owned),
    };
    *w.feed.records.lock().unwrap() = vec![
        rec(NOW - 10, 2_500, None),
        rec(NOW - 20, 1_900, None),
        rec(NOW - 7_200, 9_000, None),
    ];
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!((snap.guard.slow, snap.guard.timeouts), (1, 0));
    assert_eq!(strip(&snap)[4].level, Level::Amber);
    w.feed
        .records
        .lock()
        .unwrap()
        .push(rec(NOW - 30, 10_000, Some("no answer within 10 seconds")));
    w.feed
        .records
        .lock()
        .unwrap()
        .push(rec(NOW - 40, 10, Some("a store error")));
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(snap.guard.timeouts, 1);
    assert_eq!(strip(&snap)[4].level, Level::Red);
}

// The checks of the setup become rows: PASS ok, FAIL failing with the command it names.
#[test]
fn the_snapshot_turns_check_lines_into_rows() {
    let mut w = World::new("checks");
    w.feed.checks = vec![
        "PASS the store is walled".to_owned(),
        "FAIL the program is not set up: run `lunchroom install console` again".to_owned(),
        "FAIL nothing to run here".to_owned(),
        "a note about the home folder".to_owned(),
    ];
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(snap.checks.len(), 4);
    assert!(snap.checks[0].ok && snap.checks[0].name == "the store is walled");
    assert!(!snap.checks[1].ok);
    assert_eq!(snap.checks[1].name, "the program is not set up");
    assert_eq!(
        snap.checks[1].fix,
        Fix::Command("lunchroom install console".to_owned())
    );
    assert_eq!(snap.checks[2].fix, Fix::None);
    assert!(snap.checks[3].ok);
    assert_eq!(strip(&snap)[5].text, "CHECKS 2 FAIL");
}

// The agents' rows come from the store: names, harness and queued count.
#[test]
fn the_snapshot_lists_the_agents_of_the_store() {
    let w = World::new("agents");
    w.name("kimi", "k1");
    w.name("codex", "c1");
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let names: Vec<&str> = snap.agents.iter().map(|a| a.name.as_str()).collect();
    assert!(
        names.contains(&"kimi") && names.contains(&"codex"),
        "{names:?}"
    );
    assert!(snap.agents.iter().all(|a| a.harness == "Codex"));
    assert_eq!(snap.hold_mode, "Balanced");
    assert_eq!(snap.developer, "dev");
    assert!(snap.waiting.is_empty());
    // The screen draws it.
    let mut t = T::new(snap);
    assert!(t.screen().contains("kimi"));
}

// The console's view of the developer: the fixed store folder of their uid, the home folder given,
// and no variable of the environment in it.
#[test]
fn the_consoles_view_is_the_fixed_folder_of_the_developer() {
    let env = Env::console_view(1000, PathBuf::from("/home/dev"));
    assert_eq!(env.lunchroom_home, PathBuf::from("/var/lib/lunchroom/1000"));
    assert_eq!(
        env.store_dir(),
        PathBuf::from("/var/lib/lunchroom/1000/store")
    );
    assert_eq!(env.home, PathBuf::from("/home/dev"));
    assert_eq!(env.codex_home, PathBuf::from("/home/dev/.codex"));
    assert_eq!(env.hermes_home, PathBuf::from("/home/dev/.hermes"));
    assert_eq!(
        (env.claude_pid, env.claude_session, env.codex_thread),
        (None, None, None)
    );
    assert_eq!(env.lunchroom_session, None);
}

// The DENIED counts are read from the records: a record of another kind is not counted, and
// doctor's list of the last 24 hours does not show a DENIED one.
#[test]
fn denied_records_are_counted_apart() {
    let now = 1_000_000;
    let mut all = vec![
        denied(now - 5, "a", 1),
        denied(now - 700, "a", 2),
        denied(now - 90_000, "a", 3),
    ];
    all.push(Record {
        hook: "pretool".to_owned(),
        ..denied(now - 5, "a", 4)
    });
    let counts = records::denied_counts(&all, now);
    assert_eq!(counts.len(), 1);
    assert_eq!((counts[0].recent, counts[0].day), (1, 2));
    assert_eq!(records::recent(&all, now).len(), 1);
}

// The model's Hit type carries what a click does (compile-time use of the public surface).
#[test]
fn hits_compare() {
    assert_eq!(Hit::Press(KeyCode::Esc), Hit::Press(KeyCode::Esc));
    assert_ne!(Hit::Inside, Hit::Cell(0));
}

// ---------------------------------------------------------------------------------------------
// The liveness file and the notice.
// ---------------------------------------------------------------------------------------------

fn scratch_plain(name: &str) -> PathBuf {
    scratch(name)
}

// The liveness file names the console's pid and start time; it counts only while that process
// runs under that start time.
#[test]
fn the_liveness_file_names_a_live_process() {
    let home = scratch_plain("alive");
    assert_eq!(super::alive_pid(&home), None);
    assert!(super::write_alive(&home, std::process::id()).is_ok());
    assert_eq!(super::alive_pid(&home), Some(std::process::id()));
    // The same pid with another start time is another process.
    let text = std::fs::read_to_string(home.join(super::ALIVE_FILE)).unwrap_or_default();
    let pid = std::process::id();
    assert!(std::fs::write(home.join(super::ALIVE_FILE), format!("{pid} 1\n")).is_ok());
    assert_eq!(super::alive_pid(&home), None);
    assert!(text.starts_with(&format!("{pid} ")), "{text}");
    let _ = std::fs::remove_dir_all(&home);
}

// F2 design, "Finding and starting the console" item 2. Failure classes: `lunchroom console` run
// by the developer that fails with the wrong-user text instead of starting `sudo` (rule there and
// not the console: Run); a missing rule that runs `sudo` anyway (NotSetUp, which says to run
// `lunchroom install`); run as `lunchroom`, it is the console as before (IsConsole, whatever the
// rule). Fails before the change: `Launch` and `launch_decision` do not exist, and `run` has only
// the wrong-user answer.
#[test]
fn the_developers_console_command_runs_sudo_only_where_the_launch_rule_is_there() {
    use super::{Launch, launch_decision};
    assert_eq!(launch_decision(false, true), Launch::Run);
    assert_eq!(launch_decision(false, false), Launch::NotSetUp);
    assert_eq!(launch_decision(true, true), Launch::IsConsole);
    assert_eq!(launch_decision(true, false), Launch::IsConsole);
    // The command it runs is the one the spec names, as one argument list.
    assert_eq!(
        super::start_command(),
        "sudo -u lunchroom /usr/local/lib/lunchroom/lunchroom console"
    );
    assert_eq!(
        crate::install::console_setup::LAUNCH_RULE,
        "/etc/sudoers.d/lunchroom"
    );
    let said = super::not_set_up_text();
    assert!(
        said.contains("not set up") && said.contains("`lunchroom install`"),
        "{said}"
    );
    assert!(!said.contains("sudo"), "{said}");
}

// The rule file is absent only when looking for it found no such file: `/etc/sudoers.d` is
// commonly root-only, and a refused look says nothing of the file.
#[test]
fn only_a_missing_launch_rule_file_is_absent() {
    use std::io::ErrorKind;
    assert!(super::launch_rule_there(None));
    assert!(super::launch_rule_there(Some(ErrorKind::PermissionDenied)));
    assert!(!super::launch_rule_there(Some(ErrorKind::NotFound)));
}

// No live console: one notice a day, and none when it runs.
#[test]
fn the_missing_console_is_told_once_a_day() {
    let home = scratch_plain("missing");
    let want = "The Lunchroom console is not running: start it with sudo -u lunchroom \
                /usr/local/lib/lunchroom/lunchroom console in a terminal of your own.";
    assert_eq!(super::missing_notice_at(&home, 86_400 * 10 + 5, true), None);
    assert_eq!(
        super::missing_notice_at(&home, 86_400 * 10 + 5, false).as_deref(),
        Some(want)
    );
    assert_eq!(
        super::missing_notice_at(&home, 86_400 * 10 + 900, false),
        None
    );
    assert_eq!(
        super::missing_notice_at(&home, 86_400 * 11 + 1, false).as_deref(),
        Some(want)
    );
    assert_eq!(super::missing_notice_at(&home, 86_400 * 11 + 2, true), None);
    let _ = std::fs::remove_dir_all(&home);
}

// The mode line: user mode and console mode pass; a console set up with the store still
// user-owned fails, and so does a setgid program with no console set up.
#[test]
fn the_mode_line_fails_a_console_with_a_user_owned_store() {
    use super::mode_line;
    let store = std::path::Path::new("/home/u/.lunchroom");
    assert!(
        mode_line(false, false, store).is_ok_and(|l| l.starts_with("Lunchroom runs in user mode"))
    );
    assert_eq!(
        mode_line(true, true, std::path::Path::new("/var/lib/lunchroom/1000")),
        Ok(
            "the console is set up and the store at /var/lib/lunchroom/1000 is out of reach of \
            your agents (group lunchroom)"
                .to_owned()
        )
    );
    let fail = mode_line(true, false, store);
    assert!(
        fail.as_ref().is_err_and(|l| l.starts_with(
            "the console is set up, but this Lunchroom still uses the store at \
             /home/u/.lunchroom, which you own"
        )),
        "{fail:?}"
    );
    assert!(mode_line(false, true, store).is_err());
}

// The panel text of each kind of item holds every part of the item (compile and content).
#[test]
fn each_panel_holds_the_whole_item() {
    let snap = sample();
    let text = |item: Item<'_>| panel_text(&item).join("\n");
    assert!(text(Item::Call(&snap.waiting[0])).contains("git push --force origin main"));
    assert!(text(Item::Agent(&snap.agents[1])).contains("HOLDTEXT first line\nsecond line"));
    assert!(
        text(Item::Message(&snap.messages[0]))
            .contains("FAILEDBODY the whole text\nof the message")
    );
    assert!(text(Item::Alert(&snap.alerts[0])).contains("curl evil"));
    assert!(text(Item::Check(&snap.checks[1])).contains("EVIDENCETEXT of check-cmd"));
    assert!(text(Item::Marked(&snap.marked[0])).contains("MARKCALL"));
}

// §17::127, 151, 149: the real paths for user id 1000.
#[test]
fn the_real_paths_have_the_forms_the_spec_gives() {
    let paths = ControllerPaths::for_uid(1000);
    assert_eq!(
        paths.requests,
        PathBuf::from("/var/lib/lunchroom-approvals/1000/requests")
    );
    assert_eq!(
        paths.approvals,
        PathBuf::from("/var/lib/lunchroom-approvals/1000")
    );
    assert_eq!(paths.key, PathBuf::from("/etc/lunchroom/1000.console.key"));
    assert_eq!(
        paths
            .requests
            .join(controller::file_name(RUN, "deploy-prod")),
        PathBuf::from(format!(
            "/var/lib/lunchroom-approvals/1000/requests/{RUN}.deploy-prod.json"
        ))
    );
}

// §17::157: with no user `lunchroom` named, the real backend writes no approval.
#[test]
fn the_real_backend_writes_nothing_without_the_lunchroom_user() {
    let w = World::new("backend-no-owner");
    let dir = scratch("backend-no-owner-files");
    write_request(&dir, RUN, "deploy-prod", HASH);
    let key = dir.join("key");
    std::fs::write(&key, crate::install::console_keys::private_text(&[8; 32])).expect("a key");
    let mut base = Base;
    let mut be = RealBackend {
        env: w.env.clone(),
        base: &mut base,
        uid: 1000,
        developer: "alice".to_owned(),
        lunchroom_uid: None,
        controller: Some(ControllerPaths {
            requests: dir.join("requests"),
            approvals: dir.clone(),
            key,
        }),
    };
    let out = be.perform(
        &Action::ControllerApprove {
            run_id: RUN.to_owned(),
            phase: "deploy-prod".to_owned(),
            manifest_hash: HASH.to_owned(),
        },
        NOW,
    );
    assert!(!out.done, "{}", out.text);
    assert!(approvals_in(&dir).is_empty());
}

// §17::201, 202: the panel of an UNGUARDED agent shows the missing evidence and why, and the
// repairing command, as the real assessment gives them: a Claude Code session whose process runs
// and whose settings are not in place, assessed by the real `evidence` code.
#[test]
fn an_unguarded_agents_panel_holds_the_real_assessment() {
    let w = World::new("unguarded-real");
    let who = Identity::new(Harness::ClaudeCode, "cc-1");
    let process = crate::process::key(std::process::id()).expect("this process's own key");
    if let Some(store) = w.store() {
        let _ = store.update(|m| {
            let _ = m.register("kimi", &who, Origin::User, None);
            m.observe_process(&process, &w.env.home, &who);
        });
    }
    let want = crate::evidence::unguarded_reason(&w.mail(), &who, &w.env.home)
        .expect("the real assessment finds the session UNGUARDED")
        .text();
    assert!(!want.is_empty());
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let row = snap.agent("kimi").expect("kimi");
    assert_eq!(row.state, AgentState::Unguarded);
    assert_eq!(row.missing, want);
    assert_eq!(row.repair.as_deref(), Some("lunchroom install claude-code"));
    // The panel shows it, whole (its words, wrapped), and the command; nothing is run.
    let mut t = T::new(snap);
    t.select_agent("kimi");
    t.code(KeyCode::Enter);
    // The whole text is the panel's (the wrap breaks it at the edge, so compare without spaces).
    let squeeze = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect()
    };
    let item = snap_item(&t, "kimi");
    assert!(squeeze(&panel_text(&item).join("\n")).contains(&squeeze(&want)));
    let top = squeeze(&t.screen());
    let head: String = squeeze(&want).chars().take(40).collect();
    assert!(top.contains(&head), "the start of the reason:\n{top}");
    t.code(KeyCode::End);
    let screen = t.screen();
    let tail: String = squeeze(&want)
        .chars()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(
        squeeze(&screen).contains(&tail),
        "the end of the reason:\n{screen}"
    );
    assert!(screen.contains("lunchroom install claude-code"));
    assert!(t.be.performed.is_empty());
    // A session whose process is gone is STOPPED, with its reason.
    if let Some(store) = w.store() {
        let _ = store.update(|m| m.observe_process("999999999:1", &w.env.home, &who));
    }
    let snap = live.snapshot(NOW + 1, &Base, true);
    let row = snap.agent("kimi").expect("kimi");
    assert_eq!(row.state, AgentState::Stopped);
    assert!(row.missing.contains("not running"), "{}", row.missing);
}

// §17::38: the node asks that the console show each notice for the user, "a notice from a Claude
// Code or Codex session and a notice OpenClaw or Hermes could not show included"; it does not ask
// that a notice name its harness. The notices of all four sources are in the store, and each is
// shown.
#[test]
fn the_notices_of_every_harness_are_shown() {
    let w = World::new("notices-four");
    let texts = [
        "NOTICE-CLAUDE-CODE a message to your Claude Code session is waiting",
        "NOTICE-CODEX a message to your Codex session is waiting",
        "NOTICE-OPENCLAW a call by your OpenClaw session could not be shown to you",
        "NOTICE-HERMES a call by your Hermes session could not be shown to you",
    ];
    if let Some(store) = w.store() {
        let _ = store.update(|m| {
            for (n, text) in texts.iter().enumerate() {
                m.tell_user(text, NOW - 10 + u64::try_from(n).unwrap_or(0));
            }
        });
    }
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let shown: Vec<&AlertRow> = snap
        .alerts
        .iter()
        .filter(|a| a.kind == AlertKind::Notice)
        .collect();
    assert_eq!(shown.len(), 4);
    for text in texts {
        assert!(shown.iter().any(|a| a.text == text), "{text}");
    }
    let mut t = T::new(snap);
    t.at(140, 40);
    let screen = t.screen();
    for tag in [
        "NOTICE-CLAUDE-CODE",
        "NOTICE-CODEX",
        "NOTICE-OPENCLAW",
        "NOTICE-HERMES",
    ] {
        assert!(screen.contains(tag), "{tag}\n{screen}");
    }
    // A hidden screen shows none and tells none: they are still in the store.
    let w2 = World::new("notices-hidden");
    if let Some(store) = w2.store() {
        let _ = store.update(|m| m.tell_user("NOTICE-WAITING", NOW - 5));
    }
    let mut live = Live::new(w2.env.clone(), "dev".to_owned(), &w2.feed);
    let snap = live.snapshot(NOW, &Base, false);
    assert!(snap.alerts.iter().all(|a| a.kind != AlertKind::Notice));
    assert_eq!(w2.mail().user_notices().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// The terminal, under a real pseudo-terminal (§17::184 and the glue: raw mode, the alternate
// screen, mouse capture, and their restore on a signal, an end, and a panic).
//
// The console loop runs in a child process: this test program, run again on the one test below
// (`pty_child_session`) with the pseudo-terminal as its standard input and output. The parent
// plays the user: it types, sends signals, and reads what reaches the terminal.
// ---------------------------------------------------------------------------------------------

const CHILD_ENV: &str = "LUNCHROOM_TEST_PTY_CHILD";
const ROOT_ENV: &str = "LUNCHROOM_TEST_PTY_ROOT";

/// What a store holds when a hostile agent has had its say.
const PTY_HOSTILE: &str = "\u{1b}]52;c;SGVsbG8=\u{7}\u{202e}evil\u{200f}\0\u{9b}31m";

/// The store side of the child: the fake user's passphrase, or a panic at the first one typed.
struct PtyBackend {
    panics: bool,
}

impl Backend for PtyBackend {
    fn check_passphrase(&mut self, typed: &str) -> bool {
        assert!(!self.panics, "a panic inside the console loop");
        typed == PASS
    }
    fn perform(&mut self, _action: &Action, _now: u64) -> Outcome {
        Outcome::done("done")
    }
    fn controller_requests(&mut self) -> Listing {
        Listing::default()
    }
    fn controller_load(&mut self, _run_id: &str, _phase: &str) -> Result<ReqView, String> {
        Err("none".to_owned())
    }
}

// The child half. Without the variable it does nothing, so the suite passes it by.
#[test]
fn pty_child_session() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("the folder of the store"));
    let mut env = Env::console_view(1000, root.join("home"));
    env.lunchroom_home = root.join("lunchroom");
    // In the slow mode five DENIED calls make a probing alert, whose notice the store is asked to
    // keep: a write the parent's held lock makes wait.
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let records = if mode == "slow" {
        (0..5).map(|n| denied(at - n, "k1", n)).collect()
    } else {
        Vec::new()
    };
    let feed = TestFeed {
        records: Mutex::new(records),
        checks: vec![format!("FAIL {PTY_HOSTILE}"), "PASS ok".to_owned()],
    };
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/stdin")
        .expect("the terminal is the standard input");
    let mut clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    };
    let mut run = Run {
        live: Live::new(env, "dev".to_owned(), &feed),
        alive: None,
        clock: &mut clock,
    };
    let mut backend = PtyBackend {
        panics: mode == "panic",
    };
    let ending = serve(&tty, &Base, &mut backend, &mut run);
    assert!(ending.is_ok(), "{ending:?}");
}

/// A pseudo-terminal: its controlling side and the terminal side the child gets.
fn open_pty() -> (std::fs::File, std::fs::File) {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::os::unix::ffi::OsStrExt as _;
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("a pty");
    grantpt(&master).expect("grantpt");
    unlockpt(&master).expect("unlockpt");
    let name = ptsname(&master, Vec::new()).expect("ptsname");
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::ffi::OsStr::from_bytes(name.as_bytes()))
        .expect("the terminal side");
    (std::fs::File::from(master), slave)
}

/// The parts of a terminal's modes the console changes.
fn modes_of(tty: &std::fs::File) -> (String, String, String, String) {
    let m = rustix::termios::tcgetattr(tty).expect("the terminal's modes");
    (
        format!("{:?}", m.input_modes),
        format!("{:?}", m.output_modes),
        format!("{:?}", m.control_modes),
        format!("{:?}", m.local_modes),
    )
}

fn raw_now(tty: &std::fs::File) -> bool {
    let m = rustix::termios::tcgetattr(tty).expect("the terminal's modes");
    !m.local_modes.contains(rustix::termios::LocalModes::ECHO)
        && !m.local_modes.contains(rustix::termios::LocalModes::ICANON)
}

struct PtyRun {
    child: std::process::Child,
    master: std::fs::File,
    slave: std::fs::File,
    out: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    cooked: (String, String, String, String),
}

impl Drop for PtyRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PtyRun {
    /// Starts the child on a pseudo-terminal of 140 by 30 with a store that holds a hostile notice.
    fn start(mode: &str, name: &str) -> Self {
        Self::start_with(mode, name, false)
    }

    /// As [`PtyRun::start`]; with `hold` the parent keeps the store's lock for 25 seconds before
    /// the child starts, so any write the child makes to the store waits.
    fn start_with(mode: &str, name: &str, hold: bool) -> Self {
        use std::io::Read as _;
        let world = World::new(name);
        if let Some(store) = world.store() {
            let _ = store.update(|m| m.tell_user(PTY_HOSTILE, NOW));
        }
        if hold && let Some(store) = world.store() {
            let (held, holding) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = store.update(|_| {
                    let _ = held.send(());
                    std::thread::sleep(std::time::Duration::from_secs(25));
                });
            });
            holding.recv().expect("the store is held");
        }
        let root = world
            .env
            .lunchroom_home
            .parent()
            .expect("a folder above")
            .to_path_buf();
        let (master, slave) = open_pty();
        rustix::termios::tcsetwinsize(
            &slave,
            rustix::termios::Winsize {
                ws_row: 30,
                ws_col: 140,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .expect("a window size");
        let cooked = modes_of(&slave);
        let child = std::process::Command::new(std::env::current_exe().expect("this program"))
            .args([
                "--exact",
                "console::tests::pty_child_session",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, mode)
            .env(ROOT_ENV, &root)
            .stdin(slave.try_clone().expect("a clone"))
            .stdout(slave.try_clone().expect("a clone"))
            .stderr(slave.try_clone().expect("a clone"))
            .spawn()
            .expect("the child");
        let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (sink, mut reader) = (
            std::sync::Arc::clone(&out),
            master.try_clone().expect("a clone"),
        );
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut all) = sink.lock() {
                    all.extend_from_slice(&buf[..n]);
                }
            }
        });
        Self {
            child,
            master,
            slave,
            out,
            cooked,
        }
    }

    fn output(&self) -> Vec<u8> {
        self.out.lock().map(|o| o.clone()).unwrap_or_default()
    }

    fn has(&self, needle: &[u8]) -> bool {
        self.output().windows(needle.len()).any(|w| w == needle)
    }

    /// Waits up to 30 seconds for `needle` to reach the terminal.
    fn wait_for(&self, needle: &[u8]) -> bool {
        self.wait_for_within(needle, 30)
    }

    /// Waits up to `secs` seconds for `needle` to reach the terminal.
    fn wait_for_within(&self, needle: &[u8], secs: u64) -> bool {
        for _ in 0..(secs * 50) {
            if self.has(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    fn type_text(&self, text: &[u8]) {
        use std::io::Write as _;
        let mut master = self.master.try_clone().expect("a clone");
        master.write_all(text).expect("typing");
    }

    /// Types the passphrase and waits for the screen.
    fn log_in(&self) {
        assert!(
            self.wait_for(b"\x1b[?1049h"),
            "the alternate screen was never entered"
        );
        assert!(self.wait_for(b"Lunchroom passphrase"));
        self.type_text(format!("{PASS}\r").as_bytes());
        assert!(self.wait_for(b"AGENTS"), "the screen never came");
        assert!(
            self.wait_for(b"\\u{1b}"),
            "the hostile notice was never drawn"
        );
    }

    /// Waits for the child to end, up to 30 seconds.
    fn end(&mut self) -> std::process::ExitStatus {
        for _ in 0..1_500 {
            if let Ok(Some(status)) = self.child.try_wait() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the child did not end");
    }

    /// The terminal is as it was before the child started, and the bytes that undo the screen
    /// arrived after the screen was set up.
    fn assert_restored(&self, what: &str) {
        let out = self.output();
        let entered = out
            .windows(8)
            .position(|w| w == b"\x1b[?1049h")
            .expect("the alternate screen was entered");
        let after = &out[entered..];
        for undo in [&b"\x1b[?1049l"[..], b"\x1b[?1000l", b"\x1b[?25h"] {
            assert!(
                after.windows(undo.len()).any(|w| w == undo),
                "{what}: {:?} never arrived",
                String::from_utf8_lossy(undo)
            );
        }
        assert_eq!(
            modes_of(&self.slave),
            self.cooked,
            "{what}: the modes were not restored"
        );
        assert!(!raw_now(&self.slave), "{what}");
    }
}

fn signal_case(signal: rustix::process::Signal, number: i32, name: &str) {
    use std::os::unix::process::ExitStatusExt as _;
    let mut run = PtyRun::start("session", &format!("pty-{name}"));
    run.log_in();
    // While it runs: raw mode, the alternate screen, mouse capture.
    assert!(raw_now(&run.slave), "raw mode while it runs");
    assert!(run.has(b"\x1b[?1000h"), "mouse capture");
    let pid = rustix::process::Pid::from_raw(i32::try_from(run.child.id()).expect("a pid"));
    rustix::process::kill_process(pid.expect("a pid"), signal).expect("the signal");
    let status = run.end();
    assert_eq!(
        status.signal(),
        Some(number),
        "{name}: ended by the signal itself, got {status:?}"
    );
    run.assert_restored(name);
}

// The terminal is put back, with the bytes that undo the screen, when the console is ended by
// SIGINT, SIGTERM, SIGHUP or SIGQUIT.
#[test]
fn a_signal_restores_the_terminal_under_a_real_pty_sigint() {
    signal_case(rustix::process::Signal::INT, 2, "int");
}

#[test]
fn a_signal_restores_the_terminal_under_a_real_pty_sigterm() {
    signal_case(rustix::process::Signal::TERM, 15, "term");
}

#[test]
fn a_signal_restores_the_terminal_under_a_real_pty_sighup() {
    signal_case(rustix::process::Signal::HUP, 1, "hup");
}

#[test]
fn a_signal_restores_the_terminal_under_a_real_pty_sigquit() {
    signal_case(rustix::process::Signal::QUIT, 3, "quit");
}

// An ordinary end (`q`) restores it too, and exits 0.
#[test]
fn quitting_restores_the_terminal_under_a_real_pty() {
    let mut run = PtyRun::start("session", "pty-quit");
    run.log_in();
    run.type_text(b"q");
    let status = run.end();
    assert_eq!(status.code(), Some(0), "{status:?}");
    run.assert_restored("quit");
}

// A panic inside the console loop restores it: the passphrase check panics.
#[test]
fn a_panic_restores_the_terminal_under_a_real_pty() {
    let mut run = PtyRun::start("panic", "pty-panic");
    assert!(run.wait_for(b"\x1b[?1049h"));
    assert!(run.wait_for(b"Lunchroom passphrase"));
    assert!(raw_now(&run.slave));
    run.type_text(b"anything\r");
    let status = run.end();
    assert_eq!(status.code(), Some(101), "a failed test: {status:?}");
    assert!(run.has(b"a panic inside the console loop"));
    run.assert_restored("panic");
    // The terminal is put back before the message is printed, so the message is read on the
    // user's own screen and not lost with the alternate one.
    let out = run.output();
    let entered = out
        .windows(8)
        .position(|w| w == b"\x1b[?1049h")
        .expect("the screen");
    let find = |needle: &[u8], from: usize| {
        out[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| i + from)
    };
    let left = find(b"\x1b[?1049l", entered).expect("the screen was left");
    let message = find(b"a panic inside the console loop", entered).expect("the message");
    assert!(
        left < message,
        "the message came before the terminal was put back"
    );
}

// §17::184: store text with escape sequences, OSC 52, a bell, NUL, direction marks and a C1
// control reaches the terminal only as visible codes: every escape in the output is the start of
// a control sequence the screen itself wrote, none is an OSC, and none of the hostile bytes is
// there.
#[test]
fn hostile_store_text_reaches_the_pty_only_as_visible_codes() {
    let mut run = PtyRun::start("session", "pty-hostile");
    run.log_in();
    assert!(run.wait_for(b"\\u{7}"));
    assert!(run.wait_for(b"\\u{202e}"));
    run.type_text(b"q");
    let _ = run.end();
    let out = run.output();
    // From the start of the screen: the test harness's own lines come before it.
    let from = out
        .windows(8)
        .position(|w| w == b"\x1b[?1049h")
        .expect("the screen");
    let screen = &out[from..];
    for bad in [
        &b"\x1b]"[..],
        b"\x07",
        b"\0",
        "\u{202e}".as_bytes(),
        "\u{200f}".as_bytes(),
        "\u{9b}".as_bytes(),
    ] {
        assert!(
            !screen.windows(bad.len()).any(|w| w == bad),
            "{bad:?} reached the terminal"
        );
    }
    // Every escape the screen wrote starts a control sequence (`ESC [`).
    for (i, b) in screen.iter().enumerate() {
        if *b == 0x1b {
            assert_eq!(
                screen.get(i + 1),
                Some(&b'['),
                "an escape that is not a control sequence at {i}"
            );
        }
    }
    // The hostile text was shown, as codes.
    for code in [
        "\\u{1b}",
        "\\u{7}",
        "\\u{202e}",
        "\\u{200f}",
        "\\u{0}",
        "\\u{9b}",
    ] {
        assert!(
            screen.windows(code.len()).any(|w| w == code.as_bytes()),
            "{code} was never drawn"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The Fable gate on part 1: H1, H2, M1, M3, M7 and the LOWs.
// ---------------------------------------------------------------------------------------------

/// A system that has a chain of parent processes and a terminal number, as a test gives them.
struct Under {
    chain: Vec<(Option<String>, String)>,
    tty: u64,
}

impl System for Under {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        self.chain.clone()
    }
    fn own_tty(&self) -> u64 {
        self.tty
    }
    fn tty_of(&self, _key: &str) -> Option<u64> {
        None
    }
    fn ask(&mut self, _q: &str) -> Option<String> {
        None
    }
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn verifier(&self) -> Result<String, String> {
        Ok(fake::verifier())
    }
    fn ask_secret(&mut self, _p: &str) -> Option<String> {
        None
    }
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        Ok([3; passphrase::SALT_BYTES])
    }
    fn install_verifier(&mut self, _l: &str) -> Result<(), String> {
        Ok(())
    }
}

fn under_a_harness() -> Under {
    Under {
        chain: vec![(None, "bash".to_owned()), (None, "claude".to_owned())],
        tty: 5,
    }
}

fn backend_in<'a>(w: &World, base: &'a mut dyn System) -> RealBackend<'a> {
    RealBackend {
        env: w.env.clone(),
        base,
        uid: 1000,
        developer: "dev".to_owned(),
        lunchroom_uid: Some(LUNCHROOM_UID),
        controller: None,
    }
}

fn request(
    session: &str,
    command: &str,
    rule: &str,
    at: u64,
) -> lunchroom_core::approvals::Request {
    lunchroom_core::approvals::Request {
        session: Identity::new(Harness::ClaudeCode, session),
        agent: "kimi".to_owned(),
        tool: "Bash".to_owned(),
        input: serde_json::json!({ "command": command }),
        cwd: "/work".to_owned(),
        rule: rule.to_owned(),
        at,
    }
}

// H1, A1.3 and §17 item 3: the console starts only with no guarded session in its parent chain, a
// terminal, and a terminal that user `lunchroom` owns.
#[test]
fn the_console_does_not_start_in_a_guarded_chain_or_on_the_developers_terminal() {
    let none = Mail::default();
    // A guarded harness above it.
    let why = super::start_check(&none, &under_a_harness(), LUNCHROOM_UID, LUNCHROOM_UID)
        .expect_err("a harness above the console");
    assert!(why.contains("guarded session"), "{why}");
    // A process the store records as a guarded session's, whatever its name.
    let key = crate::process::key(std::process::id()).expect("this process's key");
    let mut mail = Mail::default();
    mail.observe_process(
        &key,
        std::path::Path::new("/work"),
        &Identity::new(Harness::Codex, "c1"),
    );
    let chain = Under {
        chain: vec![(Some(key), "node".to_owned())],
        tty: 5,
    };
    assert!(super::start_check(&mail, &chain, LUNCHROOM_UID, LUNCHROOM_UID).is_err());
    // No controlling terminal.
    let no_tty = Under {
        chain: Vec::new(),
        tty: 0,
    };
    let why = super::start_check(&none, &no_tty, LUNCHROOM_UID, LUNCHROOM_UID).expect_err("no tty");
    assert!(why.contains("no controlling terminal"), "{why}");
    // A terminal the developer owns: a process of theirs could type into it.
    let why = super::start_check(&none, &Base, 1000, LUNCHROOM_UID).expect_err("their terminal");
    assert!(
        why.contains("owned by user id 1000") && why.contains("use_pty"),
        "{why}"
    );
    // And the one case that passes.
    assert_eq!(
        super::start_check(&none, &Base, LUNCHROOM_UID, LUNCHROOM_UID),
        Ok(())
    );
}

// H1: every action, the controller's approval first among them, is refused under a guarded
// session or with no terminal, before anything is signed, written or sent.
#[test]
fn no_action_is_done_under_a_guarded_session() {
    let w = World::new("a13-actions");
    let dir = scratch("a13-actions-files");
    write_request(&dir, RUN, "deploy-prod", HASH);
    let key = dir.join("key");
    std::fs::write(&key, crate::install::console_keys::private_text(&[8; 32])).expect("a key");
    let owner = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&dir).expect("a folder"));
    if let Some(store) = w.store() {
        let _ = store.update(|m| {
            let _ = m.register(
                "kimi",
                &Identity::new(Harness::ClaudeCode, "s1"),
                Origin::User,
                None,
            );
            m.request(request("s1", "echo A", "r", NOW));
        });
    }
    let call = {
        let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
        live.snapshot(NOW, &Base, true).waiting[0].clone()
    };
    let actions = [
        Action::ControllerApprove {
            run_id: RUN.to_owned(),
            phase: "deploy-prod".to_owned(),
            manifest_hash: HASH.to_owned(),
        },
        Action::Approve {
            number: 1,
            id: call.id.clone(),
        },
        Action::Unregister {
            agent: "kimi".to_owned(),
        },
        Action::Mode(lunchroom_core::policy::HoldMode::Strict),
        Action::Yolo {
            names: "kimi".to_owned(),
            till: "23:59".to_owned(),
        },
    ];
    for mut base in [
        under_a_harness(),
        Under {
            chain: Vec::new(),
            tty: 0,
        },
    ] {
        let mut be = backend_in(&w, &mut base);
        be.controller = Some(ControllerPaths {
            requests: dir.join("requests"),
            approvals: dir.clone(),
            key: key.clone(),
        });
        be.lunchroom_uid = Some(owner);
        for action in &actions {
            let out = be.perform(action, NOW);
            assert!(
                !out.done && out.text.contains("Nothing was done"),
                "{action:?}: {}",
                out.text
            );
        }
    }
    assert!(approvals_in(&dir).is_empty(), "nothing was signed");
    let mail = w.mail();
    assert_eq!(mail.requests().len(), 1, "the call is still waiting");
    assert!(mail.named("kimi").is_some(), "the agent is still named");
}

// H2 (A2.1): two calls of one session and one tool in one second have ids that differ, and an
// approval lands on the call the user was shown, not on whatever is then at its place in the list.
#[test]
fn an_approval_lands_on_the_call_that_was_shown() {
    let w = World::new("exact-call");
    let store = w.store().expect("a store");
    store
        .update(|m| {
            m.request(request("s1", "echo OLD", "r", NOW - 590));
            m.request(request("s1", "echo A", "r", NOW));
            m.request(request("s1", "echo B", "r", NOW));
        })
        .expect("requests kept");
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(snap.waiting.len(), 3);
    let ids: std::collections::BTreeSet<&str> =
        snap.waiting.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "every waiting call has an id of its own");
    let shown = snap
        .waiting
        .iter()
        .find(|c| c.command.contains("echo A"))
        .expect("call A")
        .clone();
    assert_eq!(shown.number, 2);
    // While the passphrase is typed the oldest call lapses: A is now first, B second.
    store
        .update(|m| m.expire_requests(NOW + 20))
        .expect("expired");
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Approve {
            number: shown.number,
            id: shown.id.clone(),
        },
        NOW + 20,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    let left: Vec<&str> = mail
        .requests()
        .iter()
        .map(|r| r.input["command"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(left, ["echo B"], "A was approved, B was not");
    let approval = out
        .text
        .lines()
        .find(|l| l.starts_with("ap-"))
        .expect("the approval's id");
    assert!(mail.approval_live(
        &Identity::new(Harness::ClaudeCode, "s1"),
        approval,
        NOW + 20
    ));
}

// F2-AC54: the real backend resends a FAILED message against the real store: a new message from the
// same sender to the same recipient with the same text is QUEUED under D1, the FAILED one stays
// FAILED, and a message that is not FAILED resends nothing.
#[test]
fn a_resend_queues_the_failed_message_again() {
    let w = World::new("resend-failed");
    let store = w.store().expect("a store");
    let (from, to) = (
        Identity::new(Harness::ClaudeCode, "f1"),
        Identity::new(Harness::ClaudeCode, "t1"),
    );
    let failed = store
        .update(|m| {
            for (name, who) in [("alpha", &from), ("bravo", &to)] {
                m.observe(who);
                let _ = m.register(name, who, lunchroom_core::mail::Origin::User, None);
            }
            let id = m
                .send(&from, "alpha", "bravo", "hello bravo", NOW)
                .map(|a| a.id)
                .unwrap_or_default();
            let _ = m.unregister("bravo");
            let _ = m.fail_unnamed(NOW + 1);
            let _ = m.register("bravo", &to, lunchroom_core::mail::Origin::User, None);
            id
        })
        .expect("a failed message");
    assert!(matches!(
        w.mail().message(&failed).map(|m| m.status.state()),
        Some(lunchroom_core::vocab::MessageState::Failed)
    ));
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Resend {
            message: failed.clone(),
        },
        NOW + 2,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    let queued: Vec<_> = mail
        .queued()
        .into_iter()
        .filter(|d| d.id != failed)
        .collect();
    assert_eq!(queued.len(), 1, "one new message is QUEUED");
    assert_eq!(queued[0].to, to);
    assert!(
        out.text
            .contains(&format!("{failed} was resent as {}", queued[0].id)),
        "{}",
        out.text
    );
    assert!(matches!(
        mail.message(&failed).map(|m| m.status.state()),
        Some(lunchroom_core::vocab::MessageState::Failed)
    ));
    // The new message is QUEUED, not FAILED: resending it resends nothing.
    let again = be.perform(
        &Action::Resend {
            message: queued[0].id.clone(),
        },
        NOW + 3,
    );
    assert!(
        !again.done && again.text.contains("is not a FAILED message"),
        "{}",
        again.text
    );
}

// F2-AC53: the real backend releases a HELD agent against the real store: its hold ends as when
// the user types in its session, a line of Lunchroom's records it, and an agent that is not HELD,
// or a name that is not registered, has nothing released.
#[test]
fn a_release_ends_the_hold_and_records_a_line() {
    let w = World::new("release-hold");
    let store = w.store().expect("a store");
    let who = Identity::new(Harness::ClaudeCode, "s9");
    store
        .update(|m| {
            m.observe(&who);
            let _ = m.register("kimi", &who, lunchroom_core::mail::Origin::User, None);
            m.hold_unrecorded(&who, "tool-output", "t1");
        })
        .expect("held");
    assert_eq!(w.mail().holding(&who).len(), 1);
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Release {
            agent: "kimi".to_owned(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    assert!(mail.holding(&who).is_empty(), "the hold ended");
    assert!(
        mail.lines()
            .iter()
            .any(|l| l.words == crate::user::RELEASE_WORDS && l.who == [who.clone()]),
        "{:?}",
        mail.lines()
    );
    // Not HELD now: nothing is released, and no second line is recorded.
    let lines = mail.lines().len();
    let again = be.perform(
        &Action::Release {
            agent: "kimi".to_owned(),
        },
        NOW + 1,
    );
    assert!(
        !again.done && again.text.contains("not HELD"),
        "{}",
        again.text
    );
    assert_eq!(w.mail().lines().len(), lines);
    let nobody = be.perform(
        &Action::Release {
            agent: "nobody".to_owned(),
        },
        NOW + 2,
    );
    assert!(
        !nobody.done && nobody.text.contains("not a registered name"),
        "{}",
        nobody.text
    );
}

/// Two named agents, alpha (f1) and bravo (t1), in `w`'s store.
fn two_agents(w: &World) -> (Identity, Identity) {
    let (from, to) = (
        Identity::new(Harness::ClaudeCode, "f1"),
        Identity::new(Harness::ClaudeCode, "t1"),
    );
    w.store()
        .expect("a store")
        .update(|m| {
            for (name, who) in [("alpha", &from), ("bravo", &to)] {
                m.observe(who);
                let _ = m.register(name, who, lunchroom_core::mail::Origin::User, None);
            }
        })
        .expect("named");
    (from, to)
}

// F2-AC54, F2-AC57 (design "Store actions of Story 8"): a discard removes a QUEUED message from the
// store and its recipient's inbox, ends the hold that names it, tells its sender and records a line;
// a DELIVERED message and an unknown id are refused and change nothing.
#[test]
fn a_discard_removes_the_message_its_holds_and_tells_the_sender() {
    let w = World::new("discard");
    let (from, to) = two_agents(&w);
    let (queued, delivered) = w
        .store()
        .expect("a store")
        .update(|m| {
            let q = m
                .send(&from, "alpha", "bravo", "one", NOW)
                .map(|a| a.id)
                .unwrap_or_default();
            m.hold_before_delivery(&to, &q);
            let d = m
                .send(&from, "alpha", "bravo", "two", NOW)
                .map(|a| a.id)
                .unwrap_or_default();
            let _ = m.confirm_at(&to, &d, NOW);
            (q, d)
        })
        .expect("seeded");
    // The queued message holds bravo before delivery; the delivered one holds it too (D1.5).
    assert!(
        w.mail().holding(&to).iter().any(|h| h.message == queued),
        "the queued message holds bravo"
    );
    assert!(w.mail().holding(&to).iter().any(|h| h.message == delivered));
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Discard {
            message: queued.clone(),
        },
        NOW + 1,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    assert!(mail.message(&queued).is_none(), "the message is gone");
    assert!(
        mail.deliveries(&to).iter().all(|m| m.id != queued),
        "and out of the inbox"
    );
    assert!(
        mail.holding(&to).iter().all(|h| h.message != queued),
        "its hold ended"
    );
    assert!(
        mail.holding(&to).iter().any(|h| h.message == delivered),
        "another message's hold stays"
    );
    assert!(
        mail.notices(&from)
            .iter()
            .any(|n| n.text == crate::user::discard_notice(&queued, "bravo")),
        "{:?}",
        mail.notices(&from)
    );
    assert!(
        mail.lines()
            .iter()
            .any(|l| l.words == crate::user::discard_words(&queued)),
        "{:?}",
        mail.lines()
    );
    let lines = mail.lines().len();
    let refused = be.perform(
        &Action::Discard {
            message: delivered.clone(),
        },
        NOW + 2,
    );
    assert!(
        !refused.done && refused.text.contains("DELIVERED"),
        "{}",
        refused.text
    );
    assert!(
        w.mail().message(&delivered).is_some(),
        "a delivered message stays"
    );
    let unknown = be.perform(
        &Action::Discard {
            message: "lr-99999999".to_owned(),
        },
        NOW + 3,
    );
    assert!(
        !unknown.done && unknown.text.contains("no message"),
        "{}",
        unknown.text
    );
    assert_eq!(
        w.mail().lines().len(),
        lines,
        "a discard that is not done records nothing"
    );
}

// F2-AC55: a hold by the user names no sender (so it permits no reply), is recorded, and ends when
// the user types in that session; an unregistered name holds nothing.
#[test]
fn a_hold_by_the_user_permits_no_reply_and_ends_when_the_user_types() {
    let w = World::new("hold-user");
    let (_, to) = two_agents(&w);
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Hold {
            agent: "bravo".to_owned(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    let holds = mail.holding(&to);
    assert_eq!(holds.len(), 1, "{holds:?}");
    assert!(
        holds[0].from.is_none(),
        "a user hold names no sender, so it permits no reply"
    );
    assert_eq!(holds[0].from_name, "the user");
    assert!(
        mail.lines()
            .iter()
            .any(|l| l.words == crate::user::HOLD_WORDS && l.who == [to.clone()])
    );
    w.store()
        .expect("a store")
        .update(|m| m.user_typed(&to))
        .expect("typed");
    assert!(w.mail().holding(&to).is_empty(), "the user typing ends it");
    let nobody = be.perform(
        &Action::Hold {
            agent: "nobody".to_owned(),
        },
        NOW + 1,
    );
    assert!(
        !nobody.done && nobody.text.contains("not a registered name"),
        "{}",
        nobody.text
    );
}

// F2-AC55: a dismissed PROBING alert stays dark with no new DENIED decision, and returns after one.
#[test]
fn a_dismissed_probing_alert_returns_only_after_a_newer_denied_call() {
    let w = World::new("dismiss");
    w.name("kimi", "k1");
    {
        let mut records = w.feed.records.lock().unwrap();
        for n in 0..5 {
            records.push(denied(NOW - 60 * n, "k1", n));
        }
    }
    let base = Base;
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let probing = |snap: &Snapshot| snap.alerts.iter().any(|a| a.kind == AlertKind::Probing);
    assert!(
        probing(&live.snapshot(NOW, &base, false)),
        "the alert is raised"
    );
    let mut base2 = Base;
    let mut be = backend_in(&w, &mut base2);
    let out = be.perform(
        &Action::Dismiss {
            alert: "probing:k1".to_owned(),
        },
        NOW + 1,
    );
    assert!(out.done, "{}", out.text);
    assert!(
        w.mail()
            .lines()
            .iter()
            .any(|l| l.words == crate::user::DISMISS_WORDS),
        "{:?}",
        w.mail().lines()
    );
    assert!(
        !probing(&live.snapshot(NOW + 2, &base, false)),
        "dark with no new DENIED call"
    );
    w.feed
        .records
        .lock()
        .unwrap()
        .push(denied(NOW + 3, "k1", 9));
    assert!(
        probing(&live.snapshot(NOW + 4, &base, false)),
        "back after a newer DENIED call"
    );
    let other = be.perform(
        &Action::Dismiss {
            alert: "failed:lr-1".to_owned(),
        },
        NOW + 5,
    );
    assert!(!other.done, "{}", other.text);
}

// F2-AC57: releasing stale holds ends only holds whose message the store no longer holds; a hold
// by a live message and a hold by the user stay, and an agent with no stale hold has nothing ended.
/// Writes into `w`'s store file a hold on `who` naming message `id`, which the store does not hold:
/// the form an older store keeps after its message is gone (no store method makes one).
fn plant_stale_hold(w: &World, who: &Identity, id: &str) {
    let file = w.env.store_dir().join("state.json");
    let text = std::fs::read_to_string(&file).expect("the store file");
    let mut v: serde_json::Value = serde_json::from_str(&text).expect("json");
    let holds = v["holds"][who.key()]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut holds = holds;
    holds.push(serde_json::json!({"message": id, "from": null, "from_name": "alpha"}));
    v["holds"][who.key()] = serde_json::Value::Array(holds);
    std::fs::write(&file, v.to_string()).expect("written");
}

#[test]
fn a_stale_hold_release_keeps_live_holds() {
    let w = World::new("stale");
    let (from, to) = two_agents(&w);
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    // No hold yet: nothing is released.
    let none = be.perform(
        &Action::ReleaseStale {
            agent: "bravo".to_owned(),
        },
        NOW,
    );
    assert!(
        !none.done && none.text.contains("no hold whose message is gone"),
        "{}",
        none.text
    );
    let live_id = w
        .store()
        .expect("a store")
        .update(|m| {
            let live = m
                .send(&from, "alpha", "bravo", "live", NOW)
                .map(|a| a.id)
                .unwrap_or_default();
            m.hold_before_delivery(&to, &live);
            m.hold_by_user(&to, NOW);
            live
        })
        .expect("seeded");
    plant_stale_hold(&w, &to, "lr-99999999");
    assert_eq!(
        w.mail().holding(&to).len(),
        3,
        "a live hold, a user hold and a stale hold"
    );
    let out = be.perform(
        &Action::ReleaseStale {
            agent: "bravo".to_owned(),
        },
        NOW + 1,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    let left: Vec<&str> = mail
        .holding(&to)
        .iter()
        .map(|h| h.message.as_str())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.contains(&live_id.as_str()),
        "the live message's hold stays: {left:?}"
    );
    assert!(
        left.iter().any(|m| m.starts_with("user:")),
        "the user's hold stays: {left:?}"
    );
    assert!(
        mail.lines()
            .iter()
            .any(|l| l.words == crate::user::STALE_WORDS && l.who == [to.clone()])
    );
}

// F2-AC57: the two store checks fire on a seeded store, each with its fix: a QUEUED message to an
// agent no longer named (Discard), and nothing for a store with neither.
#[test]
fn the_store_checks_fire_with_their_fixes() {
    let w = World::new("store-checks");
    let (from, _) = two_agents(&w);
    let base = Base;
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let fixes = |snap: &Snapshot| {
        snap.checks
            .iter()
            .map(|c| c.fix.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        !fixes(&live.snapshot(NOW, &base, false))
            .iter()
            .any(|f| matches!(f, Fix::Discard(_) | Fix::Release(_))),
        "a clean store has no store check"
    );
    let id = w
        .store()
        .expect("a store")
        .update(|m| {
            let id = m
                .send(&from, "alpha", "bravo", "hi", NOW)
                .map(|a| a.id)
                .unwrap_or_default();
            let _ = m.unregister("bravo");
            id
        })
        .expect("seeded");
    let snap = live.snapshot(NOW + 1, &base, false);
    assert!(
        fixes(&snap).contains(&Fix::Discard(id.clone())),
        "{:?}",
        snap.checks
    );
    // A stale hold on a named agent: the check offers to release it.
    let who = Identity::new(Harness::ClaudeCode, "f1");
    plant_stale_hold(&w, &who, "lr-99999999");
    let snap = live.snapshot(NOW + 2, &base, false);
    assert!(
        fixes(&snap).contains(&Fix::Release("alpha".to_owned())),
        "{:?}",
        snap.checks
    );
}

// F2-AC58, F2-AC57: a path a HELD agent's call wrote is a marked row in the live snapshot (path, since
// when, the call), once however many spellings it has, and a failed store check whose fix is to clear
// it; a store with no marked path has neither.
#[test]
fn a_marked_path_is_listed_and_offered_for_clearing() {
    let w = World::new("marked-live");
    let (from, _) = two_agents(&w);
    let base = Base;
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &base, false);
    assert!(snap.marked.is_empty(), "{:?}", snap.marked);
    assert!(!snap.checks.iter().any(|c| matches!(c.fix, Fix::Clear(_))));
    w.store()
        .expect("a store")
        .update(|m| {
            m.hold_by_user(&from, NOW);
            m.record_written(
                &from,
                vec!["/p/out.txt".to_owned(), "/p/out.txt".to_owned()],
                "call-7",
                NOW + 1,
            )
        })
        .expect("marked");
    let snap = live.snapshot(NOW + 2, &base, false);
    assert_eq!(
        snap.marked,
        vec![MarkedRow {
            path: "/p/out.txt".to_owned(),
            since: NOW + 1,
            call: "call-7".to_owned(),
        }]
    );
    assert!(
        snap.checks
            .iter()
            .any(|c| !c.ok && c.fix == Fix::Clear("/p/out.txt".to_owned())),
        "{:?}",
        snap.checks
    );
}

// F2-AC52: the real backend declines the call that was shown, against the real store: it stops
// waiting, its exact retry is refused (the guard DENIES it with the user's reason), the other call
// still waits, and the user is told "Denied. <agent>'s call <id> was not approved."
#[test]
fn a_decline_lands_on_the_call_that_was_shown() {
    let w = World::new("decline-call");
    let store = w.store().expect("a store");
    store
        .update(|m| {
            m.request(request("s1", "echo A", "r", NOW));
            m.request(request("s1", "echo B", "r", NOW));
        })
        .expect("requests kept");
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let shown = snap
        .waiting
        .iter()
        .find(|c| c.command.contains("echo A"))
        .expect("call A")
        .clone();
    let declined = w
        .mail()
        .requests()
        .iter()
        .find(|r| r.input["command"] == "echo A")
        .cloned()
        .expect("request A");
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Refuse {
            number: shown.number,
            id: shown.id.clone(),
        },
        NOW + 5,
    );
    assert!(out.done, "{}", out.text);
    assert!(
        out.text.starts_with("Denied. kimi's call ") && out.text.contains(" was not approved."),
        "{}",
        out.text
    );
    let mail = w.mail();
    let left: Vec<&str> = mail
        .requests()
        .iter()
        .map(|r| r.input["command"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(left, ["echo B"], "A was declined, B still waits");
    assert!(
        mail.refused_at(
            &declined.session,
            &declined.tool,
            &declined.input,
            &declined.cwd
        )
        .is_some(),
        "the exact retry of A stays declined"
    );
    // The same decline again finds nothing waiting.
    let again = be.perform(
        &Action::Refuse {
            number: shown.number,
            id: shown.id,
        },
        NOW + 6,
    );
    assert!(
        !again.done && again.text.contains("no longer waiting"),
        "{}",
        again.text
    );
}

// H2: a call that is gone approves nothing; a call shown differently now (another rule) approves
// nothing; the call that is still there is left waiting.
#[test]
fn a_call_that_is_gone_or_shown_differently_approves_nothing() {
    let w = World::new("exact-call-changed");
    let store = w.store().expect("a store");
    store
        .update(|m| m.request(request("s1", "echo A", "rule one", NOW)))
        .expect("kept");
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let shown = live.snapshot(NOW, &Base, true).waiting[0].clone();
    // The same call asked again under another rule replaces the entry: it is shown differently.
    store
        .update(|m| m.request(request("s1", "echo A", "rule two", NOW + 5)))
        .expect("kept");
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    let out = be.perform(
        &Action::Approve {
            number: 1,
            id: shown.id.clone(),
        },
        NOW + 6,
    );
    assert!(
        !out.done && out.text.contains("shown differently"),
        "{}",
        out.text
    );
    assert_eq!(w.mail().requests().len(), 1);
    // A call that is not there at all.
    let gone = Action::Approve {
        number: 1,
        id: format!("{}#x", "nothing|here"),
    };
    let out = be.perform(&gone, NOW + 6);
    assert!(
        !out.done && out.text.contains("no longer waiting"),
        "{}",
        out.text
    );
    // An id that names no call has no `#` at all.
    let out = be.perform(
        &Action::Approve {
            number: 1,
            id: "no-hash-mark".to_owned(),
        },
        NOW + 6,
    );
    assert!(!out.done);
    assert_eq!(w.mail().requests().len(), 1, "nothing was approved");
}

// H2 on the screen: the approval carries the call chosen when `a` was pressed, whatever the list
// is while the passphrase is typed: a shifted list, or a list without that call.
#[test]
fn the_approval_carries_the_call_chosen_when_the_key_was_pressed() {
    let mut t = T::new(sample());
    t.select(&ItemKey("call", "c2".to_owned()));
    t.press('a');
    assert_eq!(modal_title(&t).as_deref(), Some("APPROVE CALL 2"));
    // The list shifts under the open question: the first call lapses, and the numbers move.
    t.snap.waiting.remove(0);
    t.snap.waiting[0].number = 1;
    t.draw();
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Approve {
            number: 2,
            id: "c2".to_owned()
        }]
    );
    // Or the call is gone altogether: the id is still the one that was shown.
    let mut t = T::new(sample());
    t.press('a');
    t.snap.waiting.clear();
    t.pass(PASS);
    assert_eq!(
        t.be.performed,
        [Action::Approve {
            number: 1,
            id: "c1".to_owned()
        }]
    );
}

// M1: the lock and the ride of a passphrase go by the monotonic clock: a wall clock stepped back an
// hour extends neither.
#[test]
fn a_wall_clock_step_back_does_not_extend_the_lock_or_the_ride() {
    let mut t = T::new(sample());
    t.select_agent("plymouth-tc");
    t.wall -= 3_600;
    t.now += IDLE_LOCK_SECS - 1;
    t.ui.tick(t.now);
    assert!(!t.ui.locked(), "14:59 of real time");
    t.now += 1;
    t.ui.tick(t.now);
    assert!(
        t.ui.locked(),
        "15:00 of real time, though the wall clock shows 14:59 less an hour"
    );
    // The ride: the passphrase typed to unlock, then the wall clock steps back, then 15 minutes.
    t.pass(PASS);
    t.wall -= 3_600;
    t.now += IDLE_LOCK_SECS - 5;
    t.press('u');
    t.press('y');
    assert!(t.ui.modal().is_none(), "within 15 minutes: it rides");
    assert_eq!(t.be.performed.len(), 1);
    t.now += 10;
    t.press('u');
    t.press('y');
    assert!(
        asking_passphrase(&t),
        "past 15 minutes of real time it asks"
    );
}

// M3: the doctor count is doctor's own: each PASS and each FAIL line it prints is a row, and the
// feed carries them all, with no summary in their place. Run against the real doctor.
#[test]
fn the_doctor_rows_are_doctors_own_lines() {
    let w = World::new("doctor-rows");
    let out = crate::cli::doctor_text(&w.env, &Base);
    let count = |text: &str, word: &str| text.lines().filter(|l| l.starts_with(word)).count();
    let (pass, fail) = (count(&out.stdout, "PASS "), count(&out.stdout, "FAIL "));
    assert!(pass + fail > 3, "{}", out.stdout);
    let rows = super::doctor_rows(&out.stdout);
    assert_eq!(rows.iter().filter(|r| r.starts_with("PASS ")).count(), pass);
    assert_eq!(rows.iter().filter(|r| r.starts_with("FAIL ")).count(), fail);
    let lines = super::RealFeed.checks(&w.env, &w.mail(), &Base);
    for row in &rows {
        assert!(lines.contains(row), "{row}");
    }
    assert!(
        !lines.iter().any(|l| l.contains("all pass")),
        "no summary stands for the rows"
    );
    // A check that runs over several lines is one row.
    let multi = "PASS one\nFAIL two\n  more of two\n\nPASS three\n";
    let rows = super::doctor_rows(multi);
    assert_eq!(rows, ["PASS one", "FAIL two\n  more of two", "PASS three"]);
}

// M7: the real backend runs approve, yolo, mode and unregister through the token verifier path to
// success: the commands of `user.rs` ask the passphrase of a verifier the console made, and the
// console's answer is that token.
#[test]
fn the_real_backend_runs_each_command_to_success_on_the_token_verifier() {
    let w = World::new("real-commands");
    let who = Identity::new(Harness::ClaudeCode, "s9");
    let store = w.store().expect("a store");
    store
        .update(|m| {
            let _ = m.register("kimi", &who, Origin::User, None);
            m.request(request("s9", "echo ok", "rule", NOW));
        })
        .expect("kept");
    let shown = Live::new(w.env.clone(), "dev".to_owned(), &w.feed)
        .snapshot(NOW, &Base, true)
        .waiting[0]
        .clone();
    let mut base = Base;
    let mut be = backend_in(&w, &mut base);
    // Approve.
    let out = be.perform(
        &Action::Approve {
            number: 1,
            id: shown.id.clone(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    let mail = w.mail();
    assert!(mail.requests().is_empty());
    let approval = out
        .text
        .lines()
        .find(|l| l.starts_with("ap-"))
        .expect("an approval id");
    assert!(mail.approval_live(&who, approval, NOW));
    // Yolo.
    let out = be.perform(
        &Action::Yolo {
            names: "kimi".to_owned(),
            till: "23:59".to_owned(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    assert!(w.mail().yolo_until(&who, NOW).is_some());
    // Mode.
    let out = be.perform(&Action::Mode(lunchroom_core::policy::HoldMode::Strict), NOW);
    assert!(out.done, "{}", out.text);
    assert_eq!(
        w.mail().hold_mode(),
        lunchroom_core::policy::HoldMode::Strict
    );
    // Unregister.
    let out = be.perform(
        &Action::Unregister {
            agent: "kimi".to_owned(),
        },
        NOW,
    );
    assert!(out.done, "{}", out.text);
    assert!(w.mail().named("kimi").is_none());
}

// L1: the operator is the developer's login name; with none in /etc/passwd there is none, never the
// number.
#[test]
fn the_developers_login_name_is_read_or_the_console_does_not_start() {
    let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/dev:/bin/sh\n";
    assert_eq!(super::developer_name(passwd, 1000), Ok("alice".to_owned()));
    let why = super::developer_name(passwd, 1001).expect_err("no such user");
    assert!(why.contains("1001") && why.contains("/etc/passwd"), "{why}");
}

// L2: what is typed is never in a Debug dump, takes no more room than it was made with, is taken
// away a character at a time, and the wipe overwrites what the buffer held before.
#[test]
fn a_typed_passphrase_is_kept_out_of_debug_and_wiped() {
    let mut t = T::locked(sample());
    t.typed("hunter2-secret");
    let dump = format!("{:?}", t.ui);
    assert!(!dump.contains("hunter2"), "{dump}");
    assert!(dump.contains("Secret(..)"), "{dump}");
    let mut t = T::new(sample());
    t.press('a');
    t.typed("topsecretword");
    let dump = format!("{:?}", t.ui);
    assert!(!dump.contains("topsecret"), "{dump}");
    let mut secret = Secret::default();
    for _ in 0..400 {
        secret.push('é');
    }
    assert_eq!(secret.as_str().len(), 256, "no more than it was made for");
    assert_eq!(secret.count(), 128);
    secret.pop();
    assert_eq!(secret.count(), 127);
    assert!(secret.as_str().chars().all(|c| c == 'é'));
    secret.clear();
    assert!(secret.is_empty());
    // The wipe: bytes inside the text and bytes left in the buffer past it, all overwritten.
    let mut bytes = Vec::with_capacity(8);
    bytes.extend_from_slice(b"secretXY");
    bytes.truncate(6);
    wipe(&mut bytes);
    assert_eq!(bytes.len(), bytes.capacity());
    assert!(bytes.iter().all(|b| *b == 0), "{bytes:?}");
}

fn wheel(t: &mut T, down: bool) {
    let _ = t.draw();
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    let event = MouseEvent {
        kind,
        column: 5,
        row: 12,
        modifiers: KeyModifiers::NONE,
    };
    t.ui.mouse(event, t.now, &t.snap, &mut t.be);
}

// L6: the wheel does not answer a y/N question, with a no or with anything else.
#[test]
fn the_wheel_does_not_answer_a_yes_no_question() {
    let mut t = T::new(sample());
    t.press('x');
    assert!(matches!(t.ui.modal(), Some(Modal::Confirm(_))));
    wheel(&mut t, true);
    wheel(&mut t, false);
    assert!(
        matches!(t.ui.modal(), Some(Modal::Confirm(_))),
        "still asking"
    );
    t.press('y');
    assert_eq!(t.be.performed.len(), 1);
    // The controller's y/N too.
    let mut t = T::new(sample());
    t.be.listing.requests = vec![ReqView {
        run_id: RUN.to_owned(),
        phase: "deploy-prod".to_owned(),
        manifest_hash: HASH.to_owned(),
    }];
    t.press('c');
    t.code(KeyCode::Enter);
    wheel(&mut t, true);
    assert!(matches!(t.ui.modal(), Some(Modal::Controller(s)) if s.confirming.is_some()));
    t.press('y');
    assert!(asking_passphrase(&t));
}

// L10 and §17::197: the PROBING panel of the live snapshot lists each DENIED decision of the last
// 10 minutes by its time and its rule, with no invented call text.
#[test]
fn the_probing_panel_lists_time_and_rule_and_no_made_up_call() {
    let w = World::new("probing-panel");
    w.name("kimi", "k1");
    {
        let mut records = w.feed.records.lock().unwrap();
        for n in 0..5 {
            records.push(denied(NOW - 60 * n, "k1", n));
        }
    }
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let mut t = T::new(snap);
    t.select_alert("probing:k1");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(!screen.contains("a DENIED call"), "{screen}");
    assert_eq!(
        screen.matches("rule      configuration").count(),
        5,
        "{screen}"
    );
    // A decision whose record keeps no call text (a record written before §17 item 4 kept it) says
    // DENIED; one that keeps it shows it (C3's tests).
    assert!(screen.contains("13:45 UTC  DENIED"), "{screen}");
}

// L9: a store that is busy stops nothing on the screen: with the store's lock held by another
// process and a probing alert for the console to write, the keys still answer.
#[test]
fn a_busy_store_does_not_stop_the_screen_answering_keys() {
    let mut run = PtyRun::start_with("slow", "pty-slow", true);
    // The lock is held for 25 seconds, and the first snapshot waits for it from the start: the
    // prompt, the login and a key must each be answered inside five seconds all the same.
    assert!(run.wait_for_within(b"\x1b[?1049h", 5));
    assert!(
        run.wait_for_within(b"Lunchroom passphrase", 5),
        "no prompt while the store is busy"
    );
    run.type_text(format!("{PASS}\r").as_bytes());
    assert!(run.wait_for_within(b"AGENTS", 5));
    run.type_text(b"l");
    assert!(
        run.wait_for_within(b"The details are hidden", 5),
        "the screen did not answer a key"
    );
    run.type_text(format!("{PASS}\r").as_bytes());
    assert!(run.wait_for_within(b"CALLS WAITING FOR YOU", 5));
    run.type_text(b"q");
    let status = run.end();
    assert_eq!(status.code(), Some(0), "{status:?}");
}

// L3: a stop signal does not leave the console suspended in raw mode on the alternate screen.
#[test]
fn a_stop_signal_does_not_suspend_the_console() {
    let mut run = PtyRun::start("session", "pty-tstp");
    run.log_in();
    let pid = rustix::process::Pid::from_raw(i32::try_from(run.child.id()).expect("a pid"));
    rustix::process::kill_process(pid.expect("a pid"), rustix::process::Signal::TSTP)
        .expect("signal");
    std::thread::sleep(std::time::Duration::from_millis(600));
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", run.child.id())).expect("stat");
    let state = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.chars().next());
    assert_ne!(state, Some('T'), "the console was stopped");
    assert!(raw_now(&run.slave));
    run.type_text(b"q");
    let status = run.end();
    assert_eq!(status.code(), Some(0), "{status:?}");
    run.assert_restored("stop signal then quit");
}

// §17 item 3 (the console's terminal is owned by user `lunchroom`): the owner is read from the
// terminal device, not from the alias `/dev/tty`. 2026-11-14 defect: the console read `fstat` of an
// opened `/dev/tty`, which is the alias's own entry (root's, device 5,0) under every terminal, so
// the console never started. A real pseudo-terminal's device number finds the device and its owner
// (this process's user); the alias, given the same number, is never taken for it.
#[test]
fn the_terminal_owner_is_read_from_the_device_and_never_from_the_alias() {
    use rustix::pty::{OpenptFlags, openpt, ptsname, unlockpt};
    use std::os::unix::fs::MetadataExt;
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("a pseudo-terminal");
    unlockpt(&master).expect("unlock it");
    let name = ptsname(&master, Vec::new()).expect("its name");
    let device = PathBuf::from(name.to_str().expect("a name in UTF-8"));
    let rdev = std::fs::metadata(&device).expect("the device").rdev();
    let me = std::fs::metadata(&device).expect("the device").uid();
    assert_eq!(
        super::terminal_owner(rdev, std::slice::from_ref(&device)),
        Some(me)
    );
    // The alias first in the list is passed over; the device behind it is found.
    let alias = PathBuf::from("/dev/tty");
    assert_eq!(
        super::terminal_owner(rdev, &[alias.clone(), device]),
        Some(me)
    );
    // The alias alone is never the terminal, whatever its own owner.
    assert_eq!(super::terminal_owner(rdev, &[alias]), None);
    // No controlling terminal (number 0): no owner.
    assert_eq!(
        super::terminal_owner(0, &[PathBuf::from("/dev/null")]),
        None
    );
}
