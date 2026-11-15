//! The screen's state and its key handling (§17 C2 items 3, 9 and 10), apart from any terminal:
//! a key or a click goes in, the state changes, and the [`Backend`] is asked only for what the
//! user has confirmed, with the passphrase where item 3 asks for it.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use lunchroom_core::conversation::Entry;
use lunchroom_core::policy::HoldMode;

use super::Ending;
use super::model::{
    Action, AgentState, AlertKind, Backend, Cmd, Fix, Item, ItemKey, Listing, MessageKind, ReqView,
    Snapshot,
};

/// Seconds without a key after which the screen locks (item 3).
pub const IDLE_LOCK_SECS: u64 = 15 * 60;

/// A passphrase may be wrong this many times in a row.
pub const TRIES: u8 = 3;

/// What a click on a place does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// The same as pressing the key.
    Press(KeyCode),
    /// Selects and opens an item.
    Item(ItemKey),
    /// One of the six cells of the alert strip.
    Cell(usize),
    /// A listed controller request.
    Request(usize),
    /// Inside the detail panel: swallows the click.
    Inside,
    /// A line of the open conversation panel (its key): selects it and opens its whole text.
    Entry(String),
}

/// Which layer of the screen a hit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Base,
    Keys,
    Panel,
    /// The conversation panel (§17.10).
    Conv,
    /// The second panel over it: one line's whole text.
    Line,
    Modal,
}

/// The key a [`Cmd`] has.
#[cfg(test)]
#[must_use]
pub const fn key_of(cmd: Cmd) -> KeyCode {
    match cmd {
        Cmd::Up => KeyCode::Up,
        Cmd::Down => KeyCode::Down,
        Cmd::Open => KeyCode::Enter,
        Cmd::Close => KeyCode::Esc,
        Cmd::Approve => KeyCode::Char('a'),
        Cmd::Refuse => KeyCode::Char('x'),
        Cmd::Release => KeyCode::Char('r'),
        Cmd::Resend => KeyCode::Char('s'),
        Cmd::Discard => KeyCode::Char('d'),
        Cmd::Hold => KeyCode::Char('h'),
        Cmd::Unregister => KeyCode::Char('u'),
        Cmd::Mode => KeyCode::Char('m'),
        Cmd::Yolo => KeyCode::Char('y'),
        Cmd::Controller => KeyCode::Char('c'),
        Cmd::Lock => KeyCode::Char('l'),
        Cmd::Quit => KeyCode::Char('q'),
    }
}

/// The [`Cmd`] a key asks for on the main screen.
#[must_use]
pub const fn cmd_of(code: KeyCode) -> Option<Cmd> {
    Some(match code {
        KeyCode::Up => Cmd::Up,
        KeyCode::Down => Cmd::Down,
        KeyCode::Enter => Cmd::Open,
        KeyCode::Esc => Cmd::Close,
        KeyCode::Char('a') => Cmd::Approve,
        KeyCode::Char('x') => Cmd::Refuse,
        KeyCode::Char('r') => Cmd::Release,
        KeyCode::Char('s') => Cmd::Resend,
        KeyCode::Char('d') => Cmd::Discard,
        KeyCode::Char('h') => Cmd::Hold,
        KeyCode::Char('u') => Cmd::Unregister,
        KeyCode::Char('m') => Cmd::Mode,
        KeyCode::Char('y') => Cmd::Yolo,
        KeyCode::Char('c') => Cmd::Controller,
        KeyCode::Char('l') => Cmd::Lock,
        KeyCode::Char('q') => Cmd::Quit,
        _ => return None,
    })
}

/// What an action needs before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// `y` (anything else is no).
    Confirm,
    /// The passphrase.
    Passphrase,
    /// `y`, then the passphrase unless it was typed within the last 15 minutes (`unregister`).
    ConfirmThenRide,
}

/// An action waiting for what it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub action: Action,
    /// What the screen says it will do (agent text: drawn with its marks).
    pub lines: Vec<String>,
    pub need: Need,
}

/// The open detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub key: ItemKey,
    pub scroll: usize,
    /// The rows of text the panel showed last; Page keys move by it.
    pub view: usize,
}

/// The open conversation panel of a pair (§17.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvPanel {
    /// The conversation it shows.
    pub key: String,
    /// The line chosen, by its key, so that a line added meanwhile does not move the choice.
    pub sel: Option<String>,
    pub sel_idx: usize,
    /// The rows of lines the panel showed last; Page keys move by it.
    pub view: usize,
}

/// The second panel: the whole text of one line of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinePanel {
    pub entry: String,
    pub scroll: usize,
    pub view: usize,
}

/// The most characters of a passphrase the screen takes.
const SECRET_MAX: usize = 256;

/// A passphrase being typed. It is made with room for [`SECRET_MAX`] characters and takes no more,
/// so it never moves in memory and leaves no copy behind; it is overwritten when it is cleared and
/// when it is dropped; it has no `Clone`, and its `Debug` shows nothing of it.
#[derive(Default)]
pub struct Secret(Vec<u8>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Overwrites everything `bytes` has room for: its text and what it held before.
pub(super) fn wipe(bytes: &mut Vec<u8>) {
    let room = bytes.capacity();
    bytes.resize(room, 0);
    bytes.fill(0);
    std::hint::black_box(&*bytes);
}

impl Secret {
    /// Adds a character; one past the limit is dropped.
    pub fn push(&mut self, c: char) {
        if self.0.capacity() < SECRET_MAX {
            self.0 = Vec::with_capacity(SECRET_MAX);
        }
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        if self.0.len() + bytes.len() <= SECRET_MAX {
            self.0.extend_from_slice(bytes);
        }
    }

    /// Takes the last character away.
    pub fn pop(&mut self) {
        let text = String::from_utf8_lossy(&self.0).chars().count();
        if text == 0 {
            return;
        }
        // The last character's bytes, found from the end.
        let mut cut = self.0.len() - 1;
        while cut > 0 && (self.0[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        for b in &mut self.0[cut..] {
            *b = 0;
        }
        self.0.truncate(cut);
    }

    /// Overwrites and empties it.
    pub fn clear(&mut self) {
        wipe(&mut self.0);
        self.0.clear();
    }

    /// Nothing is typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many characters are typed.
    #[must_use]
    pub fn count(&self) -> usize {
        String::from_utf8_lossy(&self.0).chars().count()
    }

    /// What is typed, for the one check against the verifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The lock screen.
#[derive(Debug)]
pub struct Lock {
    /// Before the first passphrase: nothing but the prompt is drawn.
    pub start: bool,
    pub typed: Secret,
    pub wrong: u8,
}

/// The controller approval screen (item 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtlScreen {
    pub listing: Listing,
    pub sel: usize,
    /// What the user typed: `a-controller <run-id> <phase>`.
    pub input: String,
    /// The request being confirmed (`y/N`).
    pub confirming: Option<ReqView>,
    /// Why the last choice was refused.
    pub error: String,
}

/// A question over the screen.
#[derive(Debug)]
pub enum Modal {
    Confirm(Pending),
    Passphrase {
        pending: Pending,
        typed: Secret,
        wrong: u8,
    },
    Mode,
    Yolo {
        names: String,
        till: String,
        second: bool,
        error: String,
    },
    Controller(CtlScreen),
}

/// The screen's state.
#[derive(Debug)]
pub struct Ui {
    pub(super) hits: Vec<(Layer, Rect, Hit)>,
    pub(super) sel: Option<ItemKey>,
    pub(super) sel_idx: usize,
    pub(super) detail: Option<Detail>,
    pub(super) conv: Option<ConvPanel>,
    pub(super) line: Option<LinePanel>,
    pub(super) modal: Option<Modal>,
    pub(super) lock: Option<Lock>,
    /// The wall clock (seconds since the Unix epoch), for what the store side stamps. Nothing
    /// here decides by it: the lock and the ride of a passphrase run on `now`, which the caller
    /// takes from a monotonic clock.
    wall: u64,
    last_key: u64,
    last_pass: Option<u64>,
    pub(super) message: String,
    ended: Option<Ending>,
}

fn time_ok(till: &str) -> bool {
    let b = till.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit)
        && till[..2].parse::<u8>().is_ok_and(|h| h < 24)
        && till[3..].parse::<u8>().is_ok_and(|m| m < 60)
}

/// What `a-controller <run-id> <phase>` names.
///
/// # Errors
/// What to type instead.
pub fn parse_controller(line: &str) -> Result<(String, String), String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["a-controller", run_id, phase] => Ok(((*run_id).to_owned(), (*phase).to_owned())),
        _ => Err("type: a-controller <run-id> <phase>".to_owned()),
    }
}

impl Ui {
    /// A screen that starts by asking the passphrase (item 3).
    #[must_use]
    pub fn new(now: u64) -> Self {
        Self {
            hits: Vec::new(),
            sel: None,
            sel_idx: 0,
            detail: None,
            conv: None,
            line: None,
            modal: None,
            lock: Some(Lock {
                start: true,
                typed: Secret::default(),
                wrong: 0,
            }),
            wall: 0,
            last_key: now,
            last_pass: None,
            message: String::new(),
            ended: None,
        }
    }

    /// Tells the screen the wall clock, which the store side stamps what it does with.
    pub fn set_wall(&mut self, wall: u64) {
        self.wall = wall;
    }

    /// Why the console ended, once it has.
    #[must_use]
    pub const fn ending(&self) -> Option<Ending> {
        self.ended
    }

    /// The screen is locked, or has not been unlocked yet.
    #[must_use]
    pub const fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Seconds until the screen locks.
    #[must_use]
    pub const fn lock_left(&self, now: u64) -> u64 {
        IDLE_LOCK_SECS.saturating_sub(now.saturating_sub(self.last_key))
    }

    /// The text under the screen (what the last action said).
    #[cfg(test)]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The question over the screen.
    #[cfg(test)]
    #[must_use]
    pub const fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    /// Whether a detail panel is open, and on what.
    #[cfg(test)]
    #[must_use]
    pub fn detail_key(&self) -> Option<&ItemKey> {
        self.detail.as_ref().map(|d| &d.key)
    }

    /// The open conversation panel, and the open line over it.
    #[cfg(test)]
    #[must_use]
    pub fn conv_panel(&self) -> (Option<&ConvPanel>, Option<&LinePanel>) {
        (self.conv.as_ref(), self.line.as_ref())
    }

    /// The selected item's key.
    #[cfg(test)]
    #[must_use]
    pub const fn selected(&self) -> Option<&ItemKey> {
        self.sel.as_ref()
    }

    /// Locks the screen: every panel and question is closed, nothing pending runs.
    pub fn lock_now(&mut self) {
        self.detail = None;
        // §17.10 "Locked": after the passphrase the main screen shows, not the panel that was open.
        self.conv = None;
        self.line = None;
        self.modal = None;
        self.hits.clear();
        self.lock = Some(Lock {
            start: false,
            typed: Secret::default(),
            wrong: 0,
        });
    }

    /// Locks the screen when 15 minutes have passed without a key; whether it did so now.
    pub fn tick(&mut self, now: u64) -> bool {
        if self.lock.is_none() && now.saturating_sub(self.last_key) >= IDLE_LOCK_SECS {
            self.lock_now();
            return true;
        }
        false
    }

    /// Keeps the selection on the item it was on, or the nearest place.
    pub fn sync(&mut self, snap: &Snapshot) {
        let items = snap.items();
        if items.is_empty() {
            self.sel = None;
            self.sel_idx = 0;
            return;
        }
        if let Some(key) = &self.sel
            && let Some(at) = items.iter().position(|i| &i.key() == key)
        {
            self.sel_idx = at;
            return;
        }
        self.sel_idx = self.sel_idx.min(items.len() - 1);
        self.sel = Some(items[self.sel_idx].key());
    }

    // -----------------------------------------------------------------------------------------
    // Keys.
    // -----------------------------------------------------------------------------------------

    /// A key. What the key changes is drawn before the next click lands: the places of the last
    /// frame are dropped.
    pub fn key(&mut self, ev: KeyEvent, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        self.key_inner(ev, now, snap, be);
        self.hits.clear();
    }

    fn key_inner(&mut self, ev: KeyEvent, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        if ev.kind == KeyEventKind::Release {
            return;
        }
        if self.tick(now) {
            // The key that came after the lock is not an answer to anything.
            return;
        }
        let ctrl_c = ev.modifiers.contains(KeyModifiers::CONTROL) && ev.code == KeyCode::Char('c');
        if self.lock.is_some() {
            if ctrl_c {
                self.ended = Some(Ending::Closed);
            } else {
                self.lock_key(ev.code, ev.modifiers, now, be);
            }
            return;
        }
        self.last_key = now;
        if ctrl_c {
            self.ended = Some(Ending::Quit);
            return;
        }
        self.sync(snap);
        if self.modal.is_some() {
            self.modal_key(ev.code, ev.modifiers, now, snap, be);
            return;
        }
        if let Some(cmd) = cmd_of(ev.code) {
            self.command(cmd, now, snap, be);
        } else if self.detail.is_some() || self.line.is_some() {
            self.scroll_key(ev.code);
        }
    }

    /// The lines of the open conversation panel's conversation.
    fn conv_entries<'s>(&self, snap: &'s Snapshot) -> &'s [Entry] {
        self.conv
            .as_ref()
            .and_then(|c| snap.conversations.iter().find(|x| x.key == c.key))
            .map_or(&[], |c| c.entries.as_slice())
    }

    /// Opens the conversation panel of the pair `key`, with the newest line chosen.
    fn open_conv(&mut self, key: &str, snap: &Snapshot) {
        let entries = snap
            .conversations
            .iter()
            .find(|c| c.key == key)
            .map_or(&[][..], |c| c.entries.as_slice());
        let last = entries.len().saturating_sub(1);
        self.conv = Some(ConvPanel {
            key: key.to_owned(),
            sel: entries.get(last).map(|e| e.key.clone()),
            sel_idx: last,
            view: 10,
        });
        self.line = None;
    }

    /// Keeps the chosen line of the conversation panel on the line it was on, or the nearest place.
    pub(super) fn sync_conv(&mut self, snap: &Snapshot) {
        let entries = self.conv_entries(snap);
        let Some(panel) = self.conv.as_mut() else {
            return;
        };
        if entries.is_empty() {
            panel.sel = None;
            panel.sel_idx = 0;
            return;
        }
        if let Some(key) = &panel.sel
            && let Some(at) = entries.iter().position(|e| &e.key == key)
        {
            panel.sel_idx = at;
            return;
        }
        panel.sel_idx = panel.sel_idx.min(entries.len() - 1);
        panel.sel = Some(entries[panel.sel_idx].key.clone());
    }

    /// Opens the second panel with the whole text of the chosen line.
    fn open_line(&mut self, key: Option<String>) {
        if let Some(entry) = key {
            self.line = Some(LinePanel {
                entry,
                scroll: 0,
                view: 10,
            });
        }
    }

    fn lock_key(&mut self, code: KeyCode, mods: KeyModifiers, now: u64, be: &mut dyn Backend) {
        let Some(lock) = self.lock.as_mut() else {
            return;
        };
        match code {
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => lock.typed.push(c),
            KeyCode::Backspace => {
                lock.typed.pop();
            }
            KeyCode::Esc => lock.typed.clear(),
            KeyCode::Enter => {
                let right = !lock.typed.is_empty() && be.check_passphrase(lock.typed.as_str());
                lock.typed.clear();
                if right {
                    self.lock = None;
                    self.last_key = now;
                    self.last_pass = Some(now);
                    self.message.clear();
                } else {
                    lock.wrong += 1;
                    if lock.wrong >= TRIES {
                        self.ended = Some(Ending::Failed);
                    }
                }
            }
            _ => {}
        }
    }

    fn scroll_key(&mut self, code: KeyCode) {
        let (scroll, view) = if let Some(l) = self.line.as_mut() {
            (&mut l.scroll, l.view)
        } else if let Some(d) = self.detail.as_mut() {
            (&mut d.scroll, d.view)
        } else {
            return;
        };
        let page = view.saturating_sub(1).max(1);
        match code {
            KeyCode::PageUp => *scroll = scroll.saturating_sub(page),
            KeyCode::PageDown => *scroll = scroll.saturating_add(page),
            KeyCode::Home => *scroll = 0,
            KeyCode::End => *scroll = usize::MAX,
            _ => {}
        }
    }

    /// The item an action keyed to the screen acts on: the panel's item, else the selected one.
    fn target<'s>(&self, snap: &'s Snapshot) -> Option<Item<'s>> {
        let key = self.detail.as_ref().map(|d| &d.key).or(self.sel.as_ref())?;
        snap.find(key)
    }

    fn say(&mut self, text: &str) {
        text.clone_into(&mut self.message);
    }

    /// Runs the action a key or a click on the key line asks for.
    #[allow(clippy::too_many_lines)] // one match: every action of the key line
    pub fn command(&mut self, cmd: Cmd, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        self.sync(snap);
        match cmd {
            Cmd::Up | Cmd::Down => {
                if let Some(l) = self.line.as_mut() {
                    l.scroll = if cmd == Cmd::Up {
                        l.scroll.saturating_sub(1)
                    } else {
                        l.scroll.saturating_add(1)
                    };
                    return;
                }
                if self.conv.is_some() {
                    self.sync_conv(snap);
                    let entries = self.conv_entries(snap);
                    if let Some(panel) = self.conv.as_mut()
                        && !entries.is_empty()
                    {
                        let to = if cmd == Cmd::Up {
                            panel.sel_idx.saturating_sub(1)
                        } else {
                            (panel.sel_idx + 1).min(entries.len() - 1)
                        };
                        panel.sel_idx = to;
                        panel.sel = Some(entries[to].key.clone());
                    }
                    return;
                }
                if let Some(d) = self.detail.as_mut() {
                    d.scroll = if cmd == Cmd::Up {
                        d.scroll.saturating_sub(1)
                    } else {
                        d.scroll.saturating_add(1)
                    };
                    return;
                }
                let n = snap.items().len();
                if n == 0 {
                    return;
                }
                let to = if cmd == Cmd::Up {
                    self.sel_idx.saturating_sub(1)
                } else {
                    (self.sel_idx + 1).min(n - 1)
                };
                self.sel_idx = to;
                self.sel = Some(snap.items()[to].key());
            }
            Cmd::Open => {
                if self.conv.is_some() {
                    if self.line.is_none() {
                        self.sync_conv(snap);
                        let sel = self.conv.as_ref().and_then(|c| c.sel.clone());
                        self.open_line(sel);
                    }
                } else if self.detail.is_none()
                    && let Some(key) = self.sel.clone()
                {
                    if key.0 == "pair" {
                        self.open_conv(&key.1, snap);
                    } else {
                        self.detail = Some(Detail {
                            key,
                            scroll: 0,
                            view: 10,
                        });
                    }
                }
            }
            Cmd::Close => {
                // Esc closes the second panel; Esc again, the conversation panel.
                if self.line.is_some() {
                    self.line = None;
                } else if self.conv.is_some() {
                    self.conv = None;
                } else {
                    self.detail = None;
                }
            }
            Cmd::Lock => self.lock_now(),
            Cmd::Quit => self.ended = Some(Ending::Quit),
            Cmd::Mode => self.modal = Some(Modal::Mode),
            Cmd::Yolo => {
                self.modal = Some(Modal::Yolo {
                    names: String::new(),
                    till: String::new(),
                    second: false,
                    error: String::new(),
                });
            }
            Cmd::Controller => {
                let listing = be.controller_requests();
                self.modal = Some(Modal::Controller(CtlScreen {
                    listing,
                    sel: 0,
                    input: String::new(),
                    confirming: None,
                    error: String::new(),
                }));
            }
            Cmd::Approve
            | Cmd::Refuse
            | Cmd::Release
            | Cmd::Resend
            | Cmd::Discard
            | Cmd::Hold
            | Cmd::Unregister => {
                let pending = self.target(snap).and_then(|item| pending_for(cmd, item));
                match pending {
                    Some(p) => self.begin(p),
                    None => self.say(not_for(cmd)),
                }
            }
        }
        let _ = (now, be);
    }

    fn begin(&mut self, pending: Pending) {
        self.modal = Some(if pending.need == Need::Passphrase {
            Modal::Passphrase {
                pending,
                typed: Secret::default(),
                wrong: 0,
            }
        } else {
            Modal::Confirm(pending)
        });
    }

    /// The user said `y`.
    fn confirmed(&mut self, pending: Pending, now: u64, be: &mut dyn Backend) {
        let rides = self
            .last_pass
            .is_some_and(|at| now.saturating_sub(at) < IDLE_LOCK_SECS);
        match pending.need {
            Need::Confirm => self.run(&pending.action, now, be),
            Need::ConfirmThenRide if rides => self.run(&pending.action, now, be),
            Need::Passphrase | Need::ConfirmThenRide => {
                self.modal = Some(Modal::Passphrase {
                    pending,
                    typed: Secret::default(),
                    wrong: 0,
                });
            }
        }
    }

    fn run(&mut self, action: &Action, _now: u64, be: &mut dyn Backend) {
        let out = be.perform(action, self.wall);
        self.message = out.text;
        self.modal = None;
        self.detail = None;
    }

    #[allow(clippy::too_many_lines)] // one match per question the screen can ask
    fn modal_key(
        &mut self,
        code: KeyCode,
        mods: KeyModifiers,
        now: u64,
        snap: &Snapshot,
        be: &mut dyn Backend,
    ) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        let plain = !mods.contains(KeyModifiers::CONTROL);
        match modal {
            Modal::Confirm(pending) => {
                if matches!(code, KeyCode::Char('y' | 'Y')) {
                    self.confirmed(pending, now, be);
                } else {
                    self.say("Not confirmed. Nothing was changed.");
                }
            }
            Modal::Passphrase {
                pending,
                mut typed,
                mut wrong,
            } => match code {
                KeyCode::Char(c) if plain => {
                    typed.push(c);
                    self.modal = Some(Modal::Passphrase {
                        pending,
                        typed,
                        wrong,
                    });
                }
                KeyCode::Backspace => {
                    typed.pop();
                    self.modal = Some(Modal::Passphrase {
                        pending,
                        typed,
                        wrong,
                    });
                }
                KeyCode::Esc => self.say("Cancelled. Nothing was changed."),
                KeyCode::Enter => {
                    let ok = !typed.is_empty() && be.check_passphrase(typed.as_str());
                    typed.clear();
                    if ok {
                        self.last_pass = Some(now);
                        self.run(&pending.action, now, be);
                    } else {
                        wrong += 1;
                        if wrong >= TRIES {
                            self.lock_now();
                            self.say("The passphrase was wrong 3 times. Nothing was changed.");
                        } else {
                            self.say("The passphrase was wrong. Nothing was changed.");
                            self.modal = Some(Modal::Passphrase {
                                pending,
                                typed,
                                wrong,
                            });
                        }
                    }
                }
                _ => {
                    self.modal = Some(Modal::Passphrase {
                        pending,
                        typed,
                        wrong,
                    });
                }
            },
            Modal::Mode => {
                let (chosen, need) = match code {
                    KeyCode::Char('s' | 'S') => (HoldMode::Strict, Need::Confirm),
                    KeyCode::Char('b' | 'B') => (HoldMode::Balanced, Need::Passphrase),
                    KeyCode::Char('r' | 'R') => (HoldMode::Relaxed, Need::Passphrase),
                    KeyCode::Esc => return,
                    _ => {
                        self.modal = Some(Modal::Mode);
                        return;
                    }
                };
                self.begin(Pending {
                    action: Action::Mode(chosen),
                    lines: vec![format!("Set the hold mode to {}.", chosen.word())],
                    need,
                });
            }
            Modal::Yolo {
                mut names,
                mut till,
                mut second,
                mut error,
            } => {
                match code {
                    KeyCode::Esc => return,
                    KeyCode::Tab | KeyCode::BackTab => second = !second,
                    KeyCode::Backspace => {
                        if second {
                            till.pop();
                        } else {
                            names.pop();
                        }
                    }
                    KeyCode::Char(c) if plain => {
                        if second {
                            till.push(c);
                        } else {
                            names.push(c);
                        }
                    }
                    KeyCode::Enter if !second => second = true,
                    KeyCode::Enter => {
                        if names.trim().is_empty() {
                            "Name the agents, separated by commas.".clone_into(&mut error);
                            second = false;
                        } else if !time_ok(till.trim()) {
                            "The end time is HH:MM in UTC, for example 17:30."
                                .clone_into(&mut error);
                        } else {
                            let (names, till) = (names.trim().to_owned(), till.trim().to_owned());
                            self.begin(Pending {
                                lines: vec![format!("YOLO for {names} until {till} UTC.")],
                                action: Action::Yolo { names, till },
                                need: Need::Passphrase,
                            });
                            return;
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Yolo {
                    names,
                    till,
                    second,
                    error,
                });
            }
            Modal::Controller(screen) => self.controller_key(screen, code, mods, now, be),
        }
        let _ = snap;
    }

    fn controller_key(
        &mut self,
        mut screen: CtlScreen,
        code: KeyCode,
        mods: KeyModifiers,
        now: u64,
        be: &mut dyn Backend,
    ) {
        let _ = now;
        if let Some(req) = screen.confirming.take() {
            // `y/N`: anything but `y` writes nothing.
            if matches!(code, KeyCode::Char('y' | 'Y')) {
                self.begin(Pending {
                    lines: vec![
                        format!("run    {}", req.run_id),
                        format!("phase  {}", req.phase),
                        format!("plan   {}", req.manifest_hash),
                    ],
                    action: Action::ControllerApprove {
                        run_id: req.run_id,
                        phase: req.phase,
                        manifest_hash: req.manifest_hash,
                    },
                    need: Need::Passphrase,
                });
            } else {
                self.say("Not approved. Nothing was written.");
            }
            return;
        }
        let plain = !mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => return,
            KeyCode::Up => screen.sel = screen.sel.saturating_sub(1),
            KeyCode::Down => {
                screen.sel = (screen.sel + 1).min(screen.listing.requests.len().saturating_sub(1));
            }
            KeyCode::Backspace => {
                screen.input.pop();
            }
            KeyCode::Char(c) if plain => screen.input.push(c),
            KeyCode::Enter => {
                let chosen = if screen.input.trim().is_empty() {
                    screen
                        .listing
                        .requests
                        .get(screen.sel)
                        .map(|r| (r.run_id.clone(), r.phase.clone()))
                        .ok_or_else(|| "There is no approval request to choose.".to_owned())
                } else {
                    parse_controller(&screen.input)
                };
                match chosen.and_then(|(run, phase)| be.controller_load(&run, &phase)) {
                    Ok(req) => {
                        screen.error.clear();
                        screen.confirming = Some(req);
                    }
                    Err(why) => why.clone_into(&mut screen.error),
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::Controller(screen));
    }

    // -----------------------------------------------------------------------------------------
    // The mouse.
    // -----------------------------------------------------------------------------------------

    /// A mouse event: a left click on what the last frame drew, or the wheel.
    pub fn mouse(&mut self, ev: MouseEvent, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        self.mouse_inner(ev, now, snap, be);
        self.hits.clear();
    }

    fn mouse_inner(&mut self, ev: MouseEvent, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        let click = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
        let wheel = matches!(
            ev.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        if !click && !wheel {
            return;
        }
        if self.tick(now) || self.lock.is_some() {
            return;
        }
        self.last_key = now;
        self.sync(snap);
        if click && self.hits.is_empty() {
            // The screen has not been drawn since the last change: a click lands on nothing the
            // user saw.
            return;
        }
        if wheel {
            // A question that is answered by any key but `y` is not answered by the wheel.
            let asking = match &self.modal {
                Some(Modal::Confirm(_)) => true,
                Some(Modal::Controller(screen)) => screen.confirming.is_some(),
                _ => false,
            };
            if asking {
                return;
            }
            let up = ev.kind == MouseEventKind::ScrollUp;
            let code = if up { KeyCode::Up } else { KeyCode::Down };
            if self.modal.is_some() {
                self.modal_key(code, KeyModifiers::NONE, now, snap, be);
            } else {
                self.command(if up { Cmd::Up } else { Cmd::Down }, now, snap, be);
            }
            return;
        }
        let allowed = |layer: Layer| {
            if self.modal.is_some() {
                layer == Layer::Modal
            } else if self.line.is_some() {
                matches!(layer, Layer::Line | Layer::Keys)
            } else if self.conv.is_some() {
                matches!(layer, Layer::Conv | Layer::Keys)
            } else if self.detail.is_some() {
                matches!(layer, Layer::Panel | Layer::Keys)
            } else {
                matches!(layer, Layer::Base | Layer::Keys)
            }
        };
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(layer, rect, _)| {
                allowed(*layer)
                    && ev.column >= rect.x
                    && ev.column < rect.x + rect.width
                    && ev.row >= rect.y
                    && ev.row < rect.y + rect.height
            })
            .map(|(_, _, hit)| hit.clone());
        match hit {
            Some(Hit::Press(code)) => {
                let key = KeyEvent::new(code, KeyModifiers::NONE);
                self.key(key, now, snap, be);
            }
            Some(Hit::Item(key)) => {
                self.sel = Some(key.clone());
                self.sync(snap);
                if key.0 == "pair" {
                    self.open_conv(&key.1, snap);
                } else {
                    self.detail = Some(Detail {
                        key,
                        scroll: 0,
                        view: 10,
                    });
                }
            }
            Some(Hit::Entry(key)) => {
                if let Some(panel) = self.conv.as_mut() {
                    panel.sel = Some(key.clone());
                }
                self.sync_conv(snap);
                self.open_line(Some(key));
            }
            Some(Hit::Cell(i)) => self.select_cell(i, snap),
            Some(Hit::Request(i)) => self.click_request(i, now, snap, be),
            Some(Hit::Inside) => {}
            None => {
                // Outside the panel closes it; outside a question does nothing. Outside the
                // second panel closes it alone; outside the conversation panel closes that.
                if self.modal.is_none() {
                    if self.line.is_some() {
                        self.line = None;
                    } else if self.conv.is_some() {
                        self.conv = None;
                    } else {
                        self.detail = None;
                    }
                }
            }
        }
    }

    fn click_request(&mut self, i: usize, now: u64, snap: &Snapshot, be: &mut dyn Backend) {
        if let Some(Modal::Controller(screen)) = self.modal.as_mut()
            && screen.confirming.is_none()
            && i < screen.listing.requests.len()
        {
            screen.sel = i;
            screen.input.clear();
            self.modal_key(KeyCode::Enter, KeyModifiers::NONE, now, snap, be);
        }
    }

    /// A click on a cell of the alert strip selects the first item it counts.
    fn select_cell(&mut self, i: usize, snap: &Snapshot) {
        let key = match i {
            0 => snap.waiting.first().map(|c| Item::Call(c).key()),
            1 => snap
                .alerts
                .iter()
                .find(|a| a.kind == AlertKind::Probing)
                .map(|a| Item::Alert(a).key()),
            2 => snap
                .messages
                .iter()
                .find(|m| m.kind == MessageKind::Failed)
                .map(|m| Item::Message(m).key()),
            3 => snap
                .agents
                .iter()
                .find(|a| {
                    a.state == AgentState::Unguarded
                        || (a.state == AgentState::Stopped && a.queued > 0)
                })
                .map(|a| Item::Agent(a).key()),
            5 => snap
                .checks
                .iter()
                .find(|c| !c.ok)
                .map(|c| Item::Check(c).key()),
            _ => None,
        };
        if let Some(key) = key {
            self.sel = Some(key);
            self.sync(snap);
        }
    }
}

/// What a key says when it does not apply to the chosen item.
const fn not_for(cmd: Cmd) -> &'static str {
    match cmd {
        Cmd::Approve | Cmd::Refuse => "Choose a call waiting first: a approves and x refuses it.",
        Cmd::Release => "r releases a HELD agent: choose one first.",
        Cmd::Resend => "s resends a FAILED message: choose one first.",
        Cmd::Discard => {
            "d discards a message, dismisses an alert or clears a marked path: choose one first."
        }
        Cmd::Hold | Cmd::Unregister => {
            "h holds and u unregisters an agent: choose an agent or a PROBING alert first."
        }
        _ => "That key does not apply here.",
    }
}

fn one(text: &str) -> String {
    text.to_owned()
}

/// The action `cmd` is for `item`, with what it needs; `None` where the key does not apply.
fn pending_for(cmd: Cmd, item: Item<'_>) -> Option<Pending> {
    match (cmd, item) {
        (Cmd::Approve, Item::Call(c)) => Some(Pending {
            action: Action::Approve {
                number: c.number,
                id: c.id.clone(),
            },
            lines: vec![
                format!("{} - {}", c.agent, c.tool),
                one(&c.command),
                "This approves this one exact call, once. Any change to it asks again.".to_owned(),
            ],
            need: Need::Passphrase,
        }),
        (Cmd::Refuse, Item::Call(c)) => Some(Pending {
            action: Action::Refuse {
                number: c.number,
                id: c.id.clone(),
            },
            lines: vec![
                format!("{} - {}", c.agent, c.tool),
                one(&c.command),
                "The call is DENIED, and the agent is told that you said no.".to_owned(),
            ],
            need: Need::Confirm,
        }),
        (Cmd::Release, Item::Agent(a)) if a.state == AgentState::Held || !a.held_by.is_empty() => {
            Some(Pending {
                action: Action::Release {
                    agent: a.name.clone(),
                },
                lines: vec![format!(
                    "Release {}: its hold ends as when you type in its session, and it is logged.",
                    a.name
                )],
                need: Need::Passphrase,
            })
        }
        (Cmd::Release, Item::Check(c)) => match &c.fix {
            Fix::Release(agent) => Some(Pending {
                action: Action::ReleaseStale {
                    agent: agent.clone(),
                },
                lines: vec![format!(
                    "Release the hold on {agent}: the store no longer holds its message."
                )],
                need: Need::Passphrase,
            }),
            _ => None,
        },
        (Cmd::Resend, Item::Message(m)) if m.kind == MessageKind::Failed => Some(Pending {
            action: Action::Resend {
                message: m.id.clone(),
            },
            lines: vec![format!(
                "Resend {} to {} as a new message, holding its recipient.",
                m.id, m.to
            )],
            need: Need::Confirm,
        }),
        (Cmd::Discard, Item::Message(m)) => Some(discard(&m.id)),
        (Cmd::Discard, Item::Check(c)) => match &c.fix {
            Fix::Discard(id) => Some(discard(id)),
            Fix::Clear(path) => Some(clear(path)),
            _ => None,
        },
        (Cmd::Discard, Item::Alert(a)) => Some(Pending {
            action: Action::Dismiss {
                alert: a.id.clone(),
            },
            lines: vec!["Dismiss this alert.".to_owned()],
            need: Need::Confirm,
        }),
        (Cmd::Discard, Item::Marked(m)) => Some(clear(&m.path)),
        (Cmd::Hold, Item::Agent(a)) => Some(hold(&a.name)),
        (Cmd::Hold, Item::Alert(a)) if !a.agent.is_empty() => Some(hold(&a.agent)),
        (Cmd::Unregister, Item::Agent(a)) => Some(unregister(&a.name)),
        (Cmd::Unregister, Item::Alert(a)) if !a.agent.is_empty() => Some(unregister(&a.agent)),
        _ => None,
    }
}

fn discard(id: &str) -> Pending {
    Pending {
        action: Action::Discard {
            message: id.to_owned(),
        },
        lines: vec![format!("Discard {id}: it is recorded in the log.")],
        need: Need::Confirm,
    }
}

fn clear(path: &str) -> Pending {
    Pending {
        action: Action::Clear {
            path: path.to_owned(),
        },
        lines: vec![format!(
            "Clear the marked path {path}, as `lunchroom clear` does."
        )],
        need: Need::Passphrase,
    }
}

fn hold(agent: &str) -> Pending {
    Pending {
        action: Action::Hold {
            agent: agent.to_owned(),
        },
        lines: vec![format!("Hold {agent}: HELD by you, released only by you.")],
        need: Need::Confirm,
    }
}

fn unregister(agent: &str) -> Pending {
    Pending {
        action: Action::Unregister {
            agent: agent.to_owned(),
        },
        lines: vec![format!("Unregister {agent}.")],
        need: Need::ConfirmThenRide,
    }
}
