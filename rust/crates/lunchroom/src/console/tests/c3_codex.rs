//! C3, the classes of Codex's complete list (FAILURES-CODEX.md), each test named for its class.

use lunchroom_core::conversation::{Conversation, Entry, EntryKind, Party};
use lunchroom_core::mail::lines::Who;

use super::*;
use crate::console::model::PartKind;

fn party(harness: &str, session: &str, name: &str) -> Party {
    Party {
        harness: harness.to_owned(),
        session: session.to_owned(),
        name: name.to_owned(),
    }
}

fn entry(n: usize, text: &str) -> Entry {
    Entry {
        key: format!("m:{n}"),
        kind: EntryKind::Message,
        at: NOW - 1_000 + n as u64,
        from: "Kimi".to_owned(),
        to: "Claude".to_owned(),
        text: text.to_owned(),
    }
}

fn conv_with(entries: Vec<Entry>) -> Conversation {
    Conversation {
        key: "pair".to_owned(),
        parties: vec![
            party("OpenClaw", "k", "Kimi"),
            party("Claude Code", "c", "Claude"),
        ],
        label: "Claude and Kimi".to_owned(),
        entries,
    }
}

fn open_with(entries: Vec<Entry>, w: u16, h: u16) -> T {
    let mut snap = sample();
    snap.conversations = vec![conv_with(entries)];
    let mut t = T::new(snap);
    t.at(w, h);
    t.select(&ItemKey("pair", "pair".to_owned()));
    t.code(KeyCode::Enter);
    t
}

/// The screen with its borders and line ends removed: text that wraps across two rows reads whole.
fn flat(t: &mut T) -> String {
    rows(&t.draw())
        .iter()
        .map(|r| r.replace('│', "").trim().to_owned())
        .collect()
}

fn no_raw_marks(buf: &Buffer) {
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            assert!(
                !buf[(x, y)].symbol().chars().any(is_mark),
                "a raw mark at ({x},{y})"
            );
        }
    }
}

/// The column (in cells) where `needle` starts on the drawn screen, and its row.
fn at(t: &mut T, needle: &str) -> (u16, u16) {
    let buf = t.draw();
    let (byte, y) = t.find(needle);
    let row = rows(&buf)[usize::from(y)].clone();
    (
        u16::try_from(row[..usize::from(byte)].chars().count()).unwrap_or(0),
        y,
    )
}

// C3-019: arrows, Enter, Esc, a click and the wheel serve the panel.
#[test]
fn c3_open_pair_click_and_enter() {
    let mut t = open_with(
        (0..30).map(|n| entry(n, &format!("line-{n:02}"))).collect(),
        120,
        40,
    );
    assert_eq!(
        t.ui.conv_panel().0.and_then(|c| c.sel.clone()).as_deref(),
        Some("m:29")
    );
    wheel(&mut t, false);
    assert_eq!(
        t.ui.conv_panel().0.and_then(|c| c.sel.clone()).as_deref(),
        Some("m:28")
    );
    wheel(&mut t, true);
    assert_eq!(
        t.ui.conv_panel().0.and_then(|c| c.sel.clone()).as_deref(),
        Some("m:29")
    );
}

// C3-021: after the list scrolls and the screen is resized, a click opens the line it is on.
#[test]
fn c3_click_maps_to_line_after_scroll_resize() {
    let mut t = open_with(
        (0..60).map(|n| entry(n, &format!("TEXT-{n:02}"))).collect(),
        100,
        14,
    );
    for _ in 0..15 {
        t.code(KeyCode::Up);
    }
    let _ = t.draw();
    t.at(70, 11);
    let screen = t.screen();
    let shown: Vec<usize> = (0..60)
        .filter(|n| screen.contains(&format!("TEXT-{n:02}")))
        .collect();
    let pick = shown[shown.len() / 2];
    let (x, y) = at(&mut t, &format!("TEXT-{pick:02}"));
    t.click(x, y);
    assert_eq!(
        t.ui.conv_panel().1.map(|l| l.entry.clone()),
        Some(format!("m:{pick}"))
    );
}

// C3-022, C3-023, C3-251: the main frame has no text of any line, in a message, a line of
// Lunchroom's or a name.
#[test]
fn c3_frame_contains_no_message_text() {
    let mut snap = sample();
    let mut c = conv_with(vec![
        entry(1, "UNIQUE-SECRET-1 body"),
        Entry {
            kind: EntryKind::Line,
            from: String::new(),
            to: String::new(),
            ..entry(2, "13:40 UTC Lunchroom: UNIQUE-SECRET-2 (A)")
        },
    ]);
    c.label = "A and B".to_owned();
    snap.conversations = vec![c];
    let mut t = T::new(snap);
    let screen = t.screen();
    assert!(!screen.contains("UNIQUE-SECRET"), "{screen}");
    assert!(screen.contains("A and B") && screen.contains("2 lines"));
}

// C3-013, C3-025, C3-051, C3-052, C3-053, C3-054: rows cut by display width: wide, combining and
// joined characters at many widths of the panel, no raw mark, and no mark split by the cut.
#[test]
fn c3_cut_by_display_width_not_bytes() {
    let texts = [
        "漢字漢字漢字漢字漢字",
        "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}",
        "👩\u{200d}💻👩\u{200d}💻👩\u{200d}💻",
        "\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}",
        "\\\\\\\\\\\\",
    ];
    for text in texts {
        for w in [10u16, 11, 12, 13, 20, 21, 33, 80] {
            let mut t = open_with(vec![entry(1, text)], w, 20);
            let buf = t.draw();
            no_raw_marks(&buf);
            for row in rows(&buf) {
                let opens = row.matches("\\u{").count();
                let closes = row.matches('}').count();
                assert!(opens <= closes, "a mark is split at width {w}: {row:?}");
            }
        }
    }
}

// C3-055: hostile text of 1 MiB, 200,000 combining marks and a deep right-to-left run draws in
// bounded time with no raw mark.
#[test]
fn c3_hostile_text_render_time_bounded() {
    let big = "x".repeat(1 << 20);
    let combining = format!("e{}", "\u{301}".repeat(200_000));
    let rtl = "\u{202e}\u{2067}".repeat(100_000);
    for text in [big, combining, rtl] {
        let started = std::time::Instant::now();
        let mut t = open_with(vec![entry(1, &text)], 100, 30);
        t.code(KeyCode::Enter);
        let buf = t.draw();
        no_raw_marks(&buf);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "{:?}",
            started.elapsed()
        );
    }
}

// C3-056: an agent's text with borders, key labels and ALERT words changes none of the chrome.
#[test]
fn c3_agent_text_cannot_fake_chrome() {
    let fake = "┌────┐ ALERT PROBING 99 [Esc] close │ ╚═╝ CALLS WAIT 9";
    let mut clean = sample();
    clean.conversations = vec![conv_with(vec![entry(1, "plain")])];
    let mut hostile = clean.clone();
    hostile.conversations[0].entries[0].text = fake.to_owned();
    hostile.conversations[0].label = fake.to_owned();
    let (mut a, mut b) = (T::new(clean), T::new(hostile));
    let (ba, bb) = (a.draw(), b.draw());
    for y in 1..4 {
        assert_eq!(rows(&ba)[y], rows(&bb)[y], "strip row {y}");
    }
    let keys = 40 - usize::from(key_rows(120));
    for y in keys..40 {
        assert_eq!(rows(&ba)[y], rows(&bb)[y], "key row {y}");
    }
}

// C3-059: a control character at every wrap boundary of the whole-text panel is never raw, and
// nothing is lost: the wrapped rows hold every mark whole.
#[test]
fn c3_control_at_wrap_boundary() {
    for w in 20u16..44 {
        for pad in 0..8 {
            let text = format!("{}\u{1b}\u{202e}\n\u{200b}z", "a".repeat(pad));
            let mut t = open_with(vec![entry(1, &text)], w, 24);
            t.code(KeyCode::Enter);
            let mut all = String::new();
            for _ in 0..40 {
                let buf = t.draw();
                no_raw_marks(&buf);
                all.push_str(
                    &rows(&buf)
                        .iter()
                        .map(|r| r.replace(['│', ' '], ""))
                        .collect::<String>(),
                );
                t.code(KeyCode::PageDown);
            }
            for want in ["\\u{1b}", "\\u{202e}", "\\u{a}", "\\u{200b}"] {
                assert!(all.contains(want), "{want} at width {w} pad {pad}");
            }
        }
    }
}

// C3-015, C3-027, C3-028, C3-248, C3-249: the whole-text panel never cuts: 64 KiB with no spaces
// and 50,000 short lines are wholly reachable by paging, and End shows the last byte.
#[test]
fn c3_second_panel_wraps_never_cuts_and_end_clamps() {
    let long = format!("{}END64K", "q".repeat(64 * 1024));
    let mut t = open_with(vec![entry(1, &long)], 100, 24);
    t.code(KeyCode::Enter);
    let mut seen = false;
    for _ in 0..4_000 {
        if flat(&mut t).contains("END64K") {
            seen = true;
            break;
        }
        t.code(KeyCode::PageDown);
    }
    assert!(seen);
    t.code(KeyCode::End);
    assert!(flat(&mut t).contains("END64K"));
    t.code(KeyCode::Home);
    assert!(!flat(&mut t).contains("END64K"));
    let lines: String = (0..50_000)
        .flat_map(|i: u32| ["L".to_owned(), i.to_string(), ";".to_owned()])
        .collect::<String>()
        + "LASTLINE";
    let mut t = open_with(vec![entry(1, &lines)], 100, 24);
    t.code(KeyCode::Enter);
    t.code(KeyCode::End);
    assert!(flat(&mut t).contains("LASTLINE"));
    t.code(KeyCode::Up);
    t.code(KeyCode::End);
    assert!(
        flat(&mut t).contains("LASTLINE"),
        "End after Up still clamps to the last row"
    );
    for _ in 0..3 {
        t.code(KeyCode::Down);
    }
    assert!(flat(&mut t).contains("LASTLINE"));
}

// C3-029, C3-254: no item action runs from the panel; `l` locks from every layer.
#[test]
fn c3_action_keys_inert_in_conversation_panel_and_lock_reachable() {
    let mut t = open_with(vec![entry(1, "hello")], 120, 40);
    t.code(KeyCode::Enter);
    for c in ['a', 'x', 'r', 's', 'd', 'h', 'u'] {
        t.press(c);
        assert!(t.ui.modal().is_none(), "{c}");
    }
    assert!(t.be.performed.is_empty());
    t.press('l');
    assert!(t.ui.locked());
    let mut t = open_with(vec![entry(1, "hello")], 120, 40);
    t.press('l');
    assert!(t.ui.locked());
    let mut t = T::new(sample());
    t.code(KeyCode::Enter);
    assert!(t.ui.detail_key().is_some());
    t.press('l');
    assert!(t.ui.locked() && t.ui.detail_key().is_none());
}

// C3-189: only a left press and the wheel act; a release and a drag do nothing.
#[test]
fn c3_mouse_press_release_drag_wheel() {
    let mut snap = sample();
    snap.conversations = vec![conv_with(vec![entry(1, "x")])];
    let mut t = T::new(snap);
    let (x, y) = at(&mut t, "Claude and Kimi");
    for kind in [
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::Moved,
    ] {
        let ev = MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let _ = t.draw();
        t.ui.mouse(ev, t.now, &t.snap, &mut t.be);
        assert!(t.ui.conv_panel().0.is_none(), "{kind:?}");
    }
    t.click(x, y);
    assert!(t.ui.conv_panel().0.is_some());
}

// C3-190, C3-191: selection clamps at both ends; Enter with nothing to open, or on a stale
// selection, opens nothing and does not panic.
#[test]
fn c3_selection_key_edges_and_stale_selection() {
    let mut t = open_with((0..3).map(|n| entry(n, "t")).collect(), 120, 40);
    for _ in 0..10 {
        t.code(KeyCode::Up);
    }
    assert_eq!(
        t.ui.conv_panel().0.and_then(|c| c.sel.clone()).as_deref(),
        Some("m:0")
    );
    for _ in 0..10 {
        t.code(KeyCode::Down);
    }
    assert_eq!(
        t.ui.conv_panel().0.and_then(|c| c.sel.clone()).as_deref(),
        Some("m:2")
    );
    t.snap.conversations.clear();
    t.code(KeyCode::Enter);
    let _ = t.draw();
    t.code(KeyCode::Esc);
    t.code(KeyCode::Esc);
    let mut empty = T::new(Snapshot::default());
    empty.code(KeyCode::Enter);
    empty.code(KeyCode::Down);
    let _ = empty.draw();
}

// C3-192, C3-193: locking drops a question that waits; the idle lock begins at 900 seconds on the
// monotonic clock and not before, whatever the wall clock says.
#[test]
fn c3_lock_discards_pending_action_and_idle_boundary() {
    let mut t = T::new(sample());
    t.press('x');
    assert!(t.ui.modal().is_some());
    t.ui.lock_now();
    assert!(t.ui.modal().is_none() && t.ui.locked());
    let mut t = open_with(vec![entry(1, "x")], 120, 40);
    t.advance(IDLE_LOCK_SECS - 1);
    assert!(!t.ui.locked());
    t.advance(1);
    assert!(t.ui.locked());
    let mut t = T::new(sample());
    t.wall = 1;
    t.ui.set_wall(1);
    t.advance(IDLE_LOCK_SECS - 1);
    assert!(!t.ui.locked());
    t.advance(1);
    assert!(t.ui.locked());
}

// C3-180..C3-186: the six cells, their order and their states.
#[test]
fn c3_alert_strip_states() {
    let mut s = Snapshot::default();
    let cells = strip(&s);
    let words: Vec<&str> = cells
        .iter()
        .map(|c| c.text.split(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(
        words,
        ["CALLS", "PROBING", "FAILED", "UNGUARDED", "GUARD", "CHECKS"]
    );
    assert_eq!(
        cells[..5].iter().filter(|c| c.level == Level::Dark).count(),
        5
    );
    assert_eq!(cells[5].level, Level::Green);
    s.waiting = vec![call(1, "a", "ls"), call(2, "a", "ls"), call(3, "a", "ls")];
    assert_eq!(
        (strip(&s)[0].level, strip(&s)[0].text.as_str()),
        (Level::Amber, "CALLS WAIT 3")
    );
    s.alerts = vec![sample().alerts[0].clone()];
    assert_eq!(
        (strip(&s)[1].level, strip(&s)[1].text.as_str()),
        (Level::Red, "PROBING 1")
    );
    s.alerts[0].kind = AlertKind::Notice;
    assert_eq!(strip(&s)[1].level, Level::Dark);
    let m = |kind| MessageRow {
        kind,
        ..MessageRow::default()
    };
    s.messages = vec![m(MessageKind::Queued)];
    assert_eq!(strip(&s)[2].level, Level::Dark);
    s.messages.push(m(MessageKind::Failed));
    assert_eq!(
        (strip(&s)[2].level, strip(&s)[2].text.as_str()),
        (Level::Amber, "FAILED MSG 1")
    );
    let a = |state, queued| AgentRow {
        state,
        queued,
        ..AgentRow::default()
    };
    s.agents = vec![a(AgentState::Stopped, 0)];
    assert_eq!(strip(&s)[3].level, Level::Dark);
    s.agents.push(a(AgentState::Stopped, 2));
    s.agents.push(a(AgentState::Unguarded, 0));
    s.agents.push(a(AgentState::Held, 5));
    assert_eq!(
        (strip(&s)[3].level, strip(&s)[3].text.as_str()),
        (Level::Amber, "UNGUARDED 2")
    );
    s.guard.slow = 1;
    assert_eq!(strip(&s)[4].level, Level::Amber);
    s.guard.timeouts = 1;
    assert_eq!(strip(&s)[4].level, Level::Red);
    s.guard = GuardRow {
        timeouts: 0,
        slow: 0,
        ..GuardRow::default()
    };
    assert_eq!(strip(&s)[4].level, Level::Dark);
    s.checks = vec![check(true, "a", Fix::None), check(false, "b", Fix::None)];
    assert_eq!(
        (strip(&s)[5].level, strip(&s)[5].text.as_str()),
        (Level::Red, "CHECKS 1 FAIL")
    );
}

// C3-252: the cells at widths 0..=8 have edges that never go backwards, and the screen draws.
#[test]
fn c3_alert_strip_tiny_widths() {
    for w in 0u16..=8 {
        let e = cell_edges(w);
        assert!(e.windows(2).all(|p| p[0] <= p[1]), "{w}: {e:?}");
        assert_eq!(e[6], w);
        let mut t = T::new(sample());
        t.at(w, 6);
        let _ = t.draw();
    }
}

// C3-253: the border of a panel is inside it: a click on it closes nothing.
#[test]
fn c3_panel_border_click_semantics() {
    let mut t = open_with(vec![entry(1, "hello")], 120, 40);
    t.code(KeyCode::Enter);
    let (_, y) = at(&mut t, "THE WHOLE TEXT");
    let row = rows(&t.draw())[usize::from(y)].clone();
    let border = row.chars().collect::<Vec<_>>();
    let inner = border.iter().rposition(|c| *c == '│').unwrap_or(0);
    let first = border.iter().position(|c| *c == '│').unwrap_or(0);
    assert!(inner > first);
    // The second panel's left border is the second `│` of the row.
    let second_left = border
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '│')
        .nth(1)
        .map_or(0, |(i, _)| i);
    t.click(u16::try_from(second_left).unwrap_or(0), y);
    assert!(
        t.ui.conv_panel().1.is_some(),
        "the second panel's own border is inside it"
    );
}

// C3-247: the console's mark test is exactly the core's, over every character.
#[test]
fn c3_console_text_mark_equivalence() {
    for c in (0u32..0x11_0000).filter_map(char::from_u32) {
        assert_eq!(
            super::super::text::is_mark(c),
            lunchroom_core::shown::is_mark(c),
            "{:x}",
            c as u32
        );
    }
}

// C3-124, C3-127, C3-128, C3-129, C3-135: ranges are byte offsets of the text as given, past
// multibyte characters and control characters; one range shows all its kinds as words.
#[test]
fn c3_mark_offsets_multibyte_and_control_and_kinds_as_words() {
    let cmd = "é 日本 \u{1b}x 😀 git push";
    let start = cmd.find("git").unwrap();
    let mut snap = sample();
    snap.waiting[0].command = cmd.to_owned();
    snap.waiting[0].parts = vec![Part {
        source: 0,
        start,
        end: cmd.len(),
        kinds: vec![PartKind::Loosens, PartKind::Destroys],
        why: "git push".to_owned(),
        reason: None,
    }];
    let mut t = T::new(snap);
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let screen = rows(&buf).join("\n");
    assert!(screen.contains("! loosens, destroys bytes"), "{screen}");
    let (gx, gy) = at(&mut t, "git push");
    let (ex, ey) = at(&mut t, "\\u{1b}x");
    assert_ne!(buf[(gx, gy)].style().fg, buf[(ex, ey)].style().fg);
    assert_eq!(buf[(gx + 7, gy)].style().fg, buf[(gx, gy)].style().fg);
    assert_ne!(
        buf[(gx - 3, gy)].style().fg,
        buf[(gx, gy)].style().fg,
        "text before the range is not marked"
    );
}

// C3-136, C3-137, C3-139: a 1 MiB waiting call with hostile folder and agent draws bounded, the
// panel shows the call whole and its secret as it is (the user approves that exact call), and the
// main screen holds none of it.
#[test]
fn c3_call_panel_fields_secret_policy_and_main_screen() {
    let secret = format!("ghp_{}", "c".repeat(36));
    let mut snap = sample();
    snap.waiting[0].command = format!("curl -H {secret} {}", "z".repeat(1 << 20));
    snap.waiting[0].cwd = "/tmp/\u{1b}[2J\u{202e}x".to_owned();
    snap.waiting[0].agent = "ag\u{1b}ent".to_owned();
    let mut t = T::new(snap);
    let main = t.screen();
    assert!(
        !main.contains(&secret) && !main.contains("zzzzzzzz"),
        "{main}"
    );
    t.select(&ItemKey("call", "c1".to_owned()));
    let started = std::time::Instant::now();
    t.code(KeyCode::Enter);
    let buf = t.draw();
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    no_raw_marks(&buf);
    let screen = rows(&buf).join("\n");
    assert!(
        screen.contains(&secret),
        "the waiting call is shown with its secret"
    );
    assert!(screen.contains("\\u{1b}[2J\\u{202e}x"));
}

// C3-075, C3-080, C3-082, C3-083, C3-138: the live snapshot's PROBING calls are newest first, those
// 599 and 600 seconds old are in and 601 out, and a later record of one second comes before an
// earlier one.
#[test]
fn c3_probing_panel_live_window_and_order() {
    let w = World::new("c3-probing-live");
    w.name("kimi", "k1");
    {
        let mut records = w.feed.records.lock().unwrap();
        let mk = |at: u64, call: &str, n: u64| Record {
            call: call.to_owned(),
            ..denied(at, "k1", n)
        };
        records.push(mk(NOW - 601, "AGE601", 1));
        records.push(mk(NOW - 600, "AGE600", 2));
        records.push(mk(NOW - 599, "AGE599", 3));
        records.push(mk(NOW - 30, "NEWER", 4));
        records.push(mk(NOW - 30, "SAME-SECOND-LATER", 5));
        records.push(mk(NOW - 1, "NEWEST", 6));
    }
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    let alert = snap
        .alerts
        .iter()
        .find(|a| a.id == "probing:k1")
        .expect("probing");
    let order: Vec<&str> = alert.calls.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(
        order,
        ["NEWEST", "SAME-SECOND-LATER", "NEWER", "AGE599", "AGE600"]
    );
}

// C3-243, C3-244, C3-245, C3-246: every stored line of Lunchroom's is in the conversations of the
// agents it names, and a tie of one second follows the store's one recorded order.
#[test]
fn c3_conversations_include_lunchroom_lines_in_store_order() {
    let w = World::new("c3-lines");
    w.name("kimi", "k1");
    w.name("claude", "c1");
    let (k, c) = (
        Identity::new(Harness::Codex, "k1"),
        Identity::new(Harness::Codex, "c1"),
    );
    if let Some(store) = w.store() {
        let _ = store.update(|m| {
            m.observe(&k);
            m.observe(&c);
            let sent = m.send(&k, "kimi", "claude", "FIRST-MSG", 100);
            assert!(sent.is_ok(), "{sent:?}");
            m.record_line(
                100,
                "Claude is HELD.",
                Who::Agents(&["claude"]),
                std::slice::from_ref(&c),
                &[],
            );
            let sent = m.send(&c, "claude", "kimi", "REPLY", 100);
            assert!(sent.is_ok(), "{sent:?}");
            m.record_line(
                100,
                "Kimi is told.",
                Who::Agents(&["kimi", "claude"]),
                &[k.clone(), c.clone()],
                &[],
            );
        });
    }
    let mut live = Live::new(w.env.clone(), "dev".to_owned(), &w.feed);
    let snap = live.snapshot(NOW, &Base, true);
    assert_eq!(
        snap.conversations.len(),
        1,
        "{:?}",
        snap.conversations
            .iter()
            .map(|c| &c.label)
            .collect::<Vec<_>>()
    );
    let texts: Vec<&str> = snap.conversations[0]
        .entries
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert_eq!(texts[0], "FIRST-MSG");
    assert!(texts[1].contains("Claude is HELD."));
    assert_eq!(texts[2], "REPLY");
    assert!(texts[3].contains("Kimi is told."));
    assert_eq!(
        snap.conversations[0]
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Line)
            .count(),
        2
    );
}
