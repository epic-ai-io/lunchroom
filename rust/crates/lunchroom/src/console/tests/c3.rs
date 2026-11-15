//! C3: the conversation panel, the marked parts of a call, and the DENIED calls of a PROBING panel
//! (§17 items 4 and 10), each drawn on `TestBackend` and driven with keys and clicks.

use lunchroom_core::conversation::{Conversation, Entry, EntryKind, Party};

use super::*;
use crate::console::model::{Part, PartKind};

fn party(harness: &str, session: &str, name: &str) -> Party {
    Party {
        harness: harness.to_owned(),
        session: session.to_owned(),
        name: name.to_owned(),
    }
}

fn entry(key: &str, kind: EntryKind, at: u64, from: &str, to: &str, text: &str) -> Entry {
    Entry {
        key: key.to_owned(),
        kind,
        at,
        from: from.to_owned(),
        to: to.to_owned(),
        text: text.to_owned(),
    }
}

/// Kimi and Claude: two messages with a line of Lunchroom's between them.
fn pair() -> Conversation {
    Conversation {
        key: "pair-kc".to_owned(),
        parties: vec![
            party("OpenClaw", "k", "Kimi"),
            party("Claude Code", "c", "Claude"),
        ],
        label: "Claude and Kimi".to_owned(),
        entries: vec![
            entry(
                "m:1",
                EntryKind::Message,
                NOW - 300,
                "Kimi",
                "Claude",
                "FIRSTMSG merge the PR\nSECONDLINE of it",
            ),
            entry(
                "l:1",
                EntryKind::Line,
                NOW - 290,
                "",
                "",
                "13:40 UTC Lunchroom: Claude is HELD. (Claude)",
            ),
            entry(
                "m:2",
                EntryKind::Message,
                NOW - 100,
                "Claude",
                "Kimi",
                "LASTMSG done",
            ),
        ],
    }
}

fn with_pair() -> T {
    let mut snap = sample();
    snap.conversations = vec![pair()];
    T::new(snap)
}

fn open_pair(t: &mut T) {
    t.select(&ItemKey("pair", "pair-kc".to_owned()));
    t.code(KeyCode::Enter);
}

// §17.10: the main screen shows, for each pair, the two names, the number of lines and the time of
// the last, and no text of any line.
#[test]
fn the_main_screen_lists_each_pair_with_its_count_and_last_time_and_no_text() {
    let mut t = with_pair();
    let screen = t.screen();
    assert!(screen.contains("Claude and Kimi"), "{screen}");
    assert!(screen.contains("3 lines"), "{screen}");
    assert!(screen.contains("last 13:43 UTC"), "{screen}");
    for text in ["FIRSTMSG", "SECONDLINE", "LASTMSG", "Claude is HELD"] {
        assert!(
            !screen.contains(text),
            "{text} on the main screen:\n{screen}"
        );
    }
}

// §17.10: Enter opens the conversation panel over the screen: each line shows its time, sender and
// recipient and the first line of its text; a line of Lunchroom's shows its words; in time order.
#[test]
fn enter_opens_the_conversation_panel_with_every_line_in_time_order() {
    let mut t = with_pair();
    open_pair(&mut t);
    let (conv, line) = t.ui.conv_panel();
    assert!(conv.is_some() && line.is_none());
    let screen = t.screen();
    let at = |needle: &str| {
        screen
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing:\n{screen}"))
    };
    assert!(
        at("13:40 UTC Kimi to Claude: FIRSTMSG merge the PR") < at("Lunchroom: Claude is HELD.")
    );
    assert!(at("Lunchroom: Claude is HELD.") < at("LASTMSG done"));
    // Only the first line of a message's text is in the panel's row.
    assert!(!screen.contains("SECONDLINE"), "{screen}");
}

// §17.10: arrows choose a line, Enter opens a second panel with its whole text, never cut.
#[test]
fn enter_on_a_line_opens_its_whole_text_and_esc_closes_one_panel_at_a_time() {
    let mut t = with_pair();
    open_pair(&mut t);
    t.code(KeyCode::Up);
    t.code(KeyCode::Up);
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(screen.contains("FIRSTMSG merge the PR"), "{screen}");
    assert!(
        screen.contains("SECONDLINE of it"),
        "the whole text:\n{screen}"
    );
    assert!(
        screen.contains("merge the PR\\u{a}SECONDLINE"),
        "a line feed is drawn as its mark:\n{screen}"
    );
    let (conv, line) = t.ui.conv_panel();
    assert!(conv.is_some() && line.is_some());
    t.code(KeyCode::Esc);
    let (conv, line) = t.ui.conv_panel();
    assert!(
        conv.is_some() && line.is_none(),
        "the first Esc closes the second panel only"
    );
    assert!(!t.screen().contains("SECONDLINE"));
    t.code(KeyCode::Esc);
    let (conv, line) = t.ui.conv_panel();
    assert!(
        conv.is_none() && line.is_none(),
        "the second Esc closes the conversation panel"
    );
    assert!(t.ui.detail_key().is_none());
}

// §17.10: a click on a pair opens its panel, a click on a line opens the line.
#[test]
fn a_click_opens_the_panel_and_a_click_on_a_line_opens_its_text() {
    let mut t = with_pair();
    let (x, y) = t.find("Claude and Kimi");
    t.click(x, y);
    assert!(t.ui.conv_panel().0.is_some());
    let (x, y) = t.find("LASTMSG");
    t.click(x, y);
    assert!(t.ui.conv_panel().1.is_some());
    assert!(t.screen().contains("THE WHOLE TEXT"));
}

// §17.10: Esc, or a click outside the second panel, closes it; a click outside the conversation
// panel closes it. The second panel is inside the first, so a click beside the second panel closes
// only the second.
#[test]
fn a_click_outside_each_panel_closes_that_panel_only() {
    let mut t = with_pair();
    t.at(120, 40);
    open_pair(&mut t);
    t.code(KeyCode::Enter);
    assert!(t.ui.conv_panel().1.is_some());
    // Inside the second panel: nothing closes.
    let (x, y) = t.find("THE WHOLE TEXT");
    t.click(x, y);
    assert!(
        t.ui.conv_panel().1.is_some(),
        "a click inside the second panel closes nothing"
    );
    // The first column of the body is the conversation panel's border, outside the second panel.
    t.click(0, 5);
    let (conv, line) = t.ui.conv_panel();
    assert!(
        conv.is_some() && line.is_none(),
        "outside the second panel closes it alone"
    );
    // Outside the conversation panel: the key line is a target of its own, so use the header row.
    t.click(0, 0);
    let (conv, line) = t.ui.conv_panel();
    assert!(
        conv.is_none() && line.is_none(),
        "outside the conversation panel closes it"
    );
    assert!(t.ui.detail_key().is_none());
}

// §17.10 "Locked": the panels are hidden, and after the passphrase the main screen shows, not the
// panel that was open.
#[test]
fn locking_closes_both_panels_and_unlocking_shows_the_main_screen() {
    let mut t = with_pair();
    open_pair(&mut t);
    t.code(KeyCode::Enter);
    assert!(t.ui.conv_panel().1.is_some());
    t.press('l');
    assert!(t.ui.locked());
    let screen = t.screen();
    assert!(
        !screen.contains("FIRSTMSG") && !screen.contains("LASTMSG"),
        "{screen}"
    );
    t.pass(PASS);
    assert!(!t.ui.locked());
    let (conv, line) = t.ui.conv_panel();
    assert!(conv.is_none() && line.is_none(), "no panel is restored");
    assert!(t.screen().contains("CALLS WAITING FOR YOU"));
}

// §17.10: the idle lock does the same.
#[test]
fn the_idle_lock_closes_the_panels_too() {
    let mut t = with_pair();
    open_pair(&mut t);
    t.advance(IDLE_LOCK_SECS);
    assert!(t.ui.locked());
    let (conv, line) = t.ui.conv_panel();
    assert!(conv.is_none() && line.is_none());
}

// §17.10: the panel has no key of its own and removes nothing: a, x, d, h, u and the rest act on no
// line, and the store's lines stay as they were.
#[test]
fn the_conversation_panel_changes_nothing() {
    let mut t = with_pair();
    open_pair(&mut t);
    let before = t.snap.clone();
    for c in ['a', 'x', 'r', 's', 'd', 'h', 'u'] {
        t.press(c);
        assert!(
            t.ui.modal().is_none(),
            "{c} opened a question over a conversation"
        );
    }
    assert!(t.be.performed.is_empty());
    assert_eq!(t.snap, before);
    assert!(t.ui.conv_panel().0.is_some());
}

// §17.10: text an agent chose is drawn with each backslash as `\u{5c}`, zero-width and non-ordinary
// spaces as their codes, in both panels and in a name; nothing a terminal would act on is in a cell.
#[test]
fn agent_text_in_both_panels_is_drawn_as_marks() {
    let mut snap = sample();
    let mut c = pair();
    c.label = "Cl\u{1b}[2Jaude\u{202e} and Kimi".to_owned();
    c.entries[0].from = "Ki\u{200b}mi".to_owned();
    c.entries[0].text = "a\\u{1b}b\u{a0}c\u{2028}d\x1b]52;c;AAAA\x07 tail".to_owned();
    snap.conversations = vec![c];
    let mut t = T::new(snap);
    let main = t.screen();
    assert!(main.contains("Cl\\u{1b}[2Jaude\\u{202e}"), "{main}");
    open_pair(&mut t);
    let screen = t.screen();
    assert!(screen.contains("Ki\\u{200b}mi"), "{screen}");
    assert!(
        screen.contains("a\\u{5c}u{1b}b\\u{a0}c\\u{2028}d\\u{1b}]52;c;AAAA\\u{7} tail"),
        "{screen}"
    );
    t.code(KeyCode::Up);
    t.code(KeyCode::Up);
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let whole = rows(&buf).join("\n");
    assert!(whole.contains("a\\u{5c}u{1b}b"), "{whole}");
    for y in 0..40 {
        for x in 0..120 {
            assert!(
                !buf[(x, y)].symbol().chars().any(is_mark),
                "a raw mark at ({x},{y})"
            );
        }
    }
}

// §17.10: where two sessions have had one name each conversation is its own and carries the
// harness in its label.
#[test]
fn two_sessions_of_one_name_are_two_lines_labelled_with_the_harness() {
    let mut snap = sample();
    let mut a = pair();
    a.key = "a".to_owned();
    a.label = "Claude (Claude Code) and Kimi".to_owned();
    let mut b = pair();
    b.key = "b".to_owned();
    b.label = "Claude (Codex) and Kimi".to_owned();
    b.entries.truncate(1);
    snap.conversations = vec![a, b];
    let mut t = T::new(snap);
    let screen = t.screen();
    assert!(screen.contains("Claude (Claude Code) and Kimi"), "{screen}");
    assert!(screen.contains("Claude (Codex) and Kimi"), "{screen}");
    t.select(&ItemKey("pair", "b".to_owned()));
    t.code(KeyCode::Enter);
    let panel = t.screen();
    assert!(
        panel.contains("CONVERSATION Claude (Codex) and Kimi"),
        "{panel}"
    );
    assert!(
        !panel.contains("LASTMSG"),
        "the other session's line is not here:\n{panel}"
    );
}

// §17.10: a text longer than the screen scrolls in the second panel and its last byte is shown.
#[test]
fn a_long_line_scrolls_and_its_last_byte_is_shown() {
    let mut snap = sample();
    let mut c = pair();
    let body: String = (0..3_000)
        .flat_map(|i: u32| ["row".to_owned(), format!("{i:04}"), " ".to_owned()])
        .collect::<String>()
        + "THE-LAST-BYTE";
    c.entries[2].text = body;
    snap.conversations = vec![c];
    let mut t = T::new(snap);
    open_pair(&mut t);
    t.code(KeyCode::Enter);
    let first = t.screen();
    assert!(
        first.contains("row0000") && !first.contains("THE-LAST-BYTE"),
        "{first}"
    );
    t.code(KeyCode::End);
    let last = t.screen();
    assert!(last.contains("THE-LAST-BYTE"), "{last}");
    assert!(!last.contains("row0000"));
}

// A panel at every size of the terminal matrix draws and answers without a panic.
#[test]
fn the_panels_draw_at_every_size_without_a_panic() {
    for (w, h) in [
        (0u16, 0u16),
        (1, 1),
        (20, 5),
        (40, 10),
        (80, 24),
        (200, 60),
        (10_000, 3),
    ] {
        let mut t = with_pair();
        t.at(w, h);
        let _ = t.draw();
        open_pair(&mut t);
        let _ = t.draw();
        t.code(KeyCode::Enter);
        let _ = t.draw();
        t.code(KeyCode::Down);
        t.code(KeyCode::PageDown);
        let _ = t.draw();
        t.click(0, 0);
        t.click(w / 2, h / 2);
        let _ = t.draw();
    }
}

// A line the store gains while the panel is open does not move the chosen line.
#[test]
fn a_new_line_does_not_move_the_chosen_line() {
    let mut t = with_pair();
    open_pair(&mut t);
    t.code(KeyCode::Up);
    t.code(KeyCode::Up);
    let chosen = t.ui.conv_panel().0.and_then(|c| c.sel.clone());
    assert_eq!(chosen.as_deref(), Some("m:1"));
    t.snap.conversations[0].entries.insert(
        0,
        entry(
            "m:0",
            EntryKind::Message,
            NOW - 900,
            "Kimi",
            "Claude",
            "EARLIER",
        ),
    );
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("FIRSTMSG"));
}

// ---------------------------------------------------------------------------------------------
// Marked parts (§17.10).
// ---------------------------------------------------------------------------------------------

fn marked_call(command: &str, parts: Vec<Part>) -> T {
    let mut snap = sample();
    snap.waiting[0].command = command.to_owned();
    snap.waiting[0].parts = parts;
    T::new(snap)
}

fn part(source: usize, start: usize, end: usize, kinds: &[PartKind], why: &str) -> Part {
    Part {
        source,
        start,
        end,
        kinds: kinds.to_vec(),
        why: why.to_owned(),
        reason: None,
    }
}

// §17.10: each kind names itself beside its range; the range is drawn in the mark colour.
#[test]
fn each_kind_of_marked_part_names_its_kind_and_its_bytes() {
    let cmd = "git push origin main && rm -rf build";
    for (kind, word) in [
        (PartKind::Loosens, "loosens"),
        (PartKind::Destroys, "destroys"),
        (PartKind::NotRead, "not read"),
        (PartKind::Reply, "reply"),
    ] {
        let mut t = marked_call(cmd, vec![part(0, 0, 8, &[kind], "RULEWORDS push")]);
        t.select(&ItemKey("call", "c1".to_owned()));
        t.code(KeyCode::Enter);
        let screen = t.screen();
        assert!(
            screen.contains(&format!("! {word} bytes 0-8: RULEWORDS push")),
            "{word}:\n{screen}"
        );
    }
}

// §17.10: the marked bytes are the ones drawn in the warning tone, no more and no less.
#[test]
fn the_marked_range_is_exactly_the_bytes_drawn_in_the_mark_tone() {
    let cmd = "echo ok; git push --force";
    let start = cmd.find("git").expect("git");
    let mut t = marked_call(
        cmd,
        vec![part(0, start, cmd.len(), &[PartKind::Destroys], "push")],
    );
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let col = |t: &mut T, needle: &str| {
        let (byte, y) = t.find(needle);
        let row = rows(&buf)[usize::from(y)].clone();
        (
            u16::try_from(row[..usize::from(byte)].chars().count()).unwrap_or(0),
            y,
        )
    };
    let (x, y) = col(&mut t, "git push --force");
    let (x0, y0) = col(&mut t, "echo ok;");
    assert_eq!(y, y0);
    let inside = buf[(x, y)].style().fg;
    let outside = buf[(x0, y0)].style().fg;
    assert_ne!(inside, outside, "the marked range is drawn like the rest");
    for i in 0..u16::try_from("git push --force".len()).unwrap_or(0) {
        assert_eq!(
            buf[(x + i, y)].style().fg,
            inside,
            "byte {i} of the range is not marked"
        );
    }
    for i in 0..u16::try_from("echo ok;".len()).unwrap_or(0) {
        assert_eq!(
            buf[(x0 + i, y0)].style().fg,
            outside,
            "byte {i} before the range is marked"
        );
    }
}

// §17.10: an unreadable call is marked whole as "not read".
#[test]
fn an_unreadable_call_is_marked_whole_as_not_read() {
    let mut snap = sample();
    snap.waiting[0].command = "WHOLECALLTEXT".to_owned();
    snap.waiting[0].unread = true;
    let mut t = T::new(snap);
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let buf = t.draw();
    let screen = rows(&buf).join("\n");
    assert!(screen.contains("! not read the whole call"), "{screen}");
    let (x, y) = t.find("WHOLECALLTEXT");
    let (px, py) = t.find("THE WHOLE CALL");
    assert_ne!(buf[(x, y)].style().fg, buf[(px, py)].style().fg);
}

// §17.10: a script the call runs has its own text and its own ranges, apart from the call's.
#[test]
fn a_script_has_its_own_ranges() {
    let mut snap = sample();
    snap.waiting[0].command = "bash deploy.sh".to_owned();
    snap.waiting[0].scripts = vec!["set -e\nkubectl apply -f x\n".to_owned()];
    snap.waiting[0].parts = vec![
        part(0, 0, 4, &[PartKind::NotRead], "CALLWORDS"),
        part(1, 7, 20, &[PartKind::Destroys], "SCRIPTWORDS"),
    ];
    let mut t = T::new(snap);
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(screen.contains("SCRIPT 1 THE CALL RUNS"), "{screen}");
    assert!(
        screen.contains("! not read bytes 0-4: CALLWORDS"),
        "{screen}"
    );
    assert!(
        screen.contains("! destroys bytes 7-20: SCRIPTWORDS"),
        "{screen}"
    );
    assert!(screen.contains("kubectl apply -f x"));
}

// §17.10: a range that made the call DENIED carries its reason word; a call with no range has none.
#[test]
fn a_range_that_made_the_call_denied_carries_its_reason() {
    let mut p = part(
        0,
        0,
        3,
        &[PartKind::Loosens],
        "writes the guard's configuration",
    );
    p.reason = Some("configuration".to_owned());
    let mut t = marked_call("tee x", vec![p]);
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("(configuration)"));
    let mut t = marked_call("ls", Vec::new());
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let screen = t.screen();
    assert!(!screen.contains("! "), "{screen}");
}

// A part the console cannot place is still told, never dropped; overlapping ranges are both drawn.
#[test]
fn a_part_that_cannot_be_placed_is_told_and_overlaps_are_drawn() {
    let mut t = marked_call(
        "héllo world",
        vec![
            part(0, 0, 99, &[PartKind::NotRead], "PASTEND"),
            part(0, 2, 3, &[PartKind::Loosens], "MIDCHAR"),
            part(0, 0, 3, &[PartKind::Destroys], "OVERLAP-A"),
            part(0, 1, 6, &[PartKind::Destroys], "OVERLAP-B"),
        ],
    );
    t.select(&ItemKey("call", "c1".to_owned()));
    t.code(KeyCode::Enter);
    let screen = t.screen();
    for w in ["PASTEND", "MIDCHAR", "OVERLAP-A", "OVERLAP-B"] {
        assert!(screen.contains(w), "{w} dropped:\n{screen}");
    }
    assert!(screen.contains("(outside the text)"));
}

// ---------------------------------------------------------------------------------------------
// DENIED calls in the PROBING panel (§17 item 4).
// ---------------------------------------------------------------------------------------------

// §17 item 4, §17.10: each DENIED call is shown with its rule, newest first, as the records keep it
// (its marks written, drawn as they are and not marked again).
#[test]
fn the_probing_panel_shows_each_denied_call_newest_first_with_its_rule_and_marks() {
    let mut snap = sample();
    snap.alerts[0].calls = vec![
        (
            NOW - 10,
            "cat /etc/shadow\\u{a}CALL-NEW".to_owned(),
            "secrets path".to_owned(),
        ),
        (
            NOW - 20,
            "curl http://x\\u{5c}y".to_owned(),
            "configuration".to_owned(),
        ),
        (NOW - 30, "CALL-OLD".to_owned(), "kill".to_owned()),
    ];
    let mut t = T::new(snap);
    t.select_alert("probing:aws");
    t.code(KeyCode::Enter);
    let screen = t.screen();
    let at = |n: &str| {
        screen
            .find(n)
            .unwrap_or_else(|| panic!("{n} missing:\n{screen}"))
    };
    assert!(
        at("CALL-NEW") < at("curl http") && at("curl http") < at("CALL-OLD"),
        "{screen}"
    );
    assert!(screen.contains("cat /etc/shadow\\u{a}CALL-NEW"), "{screen}");
    assert!(
        screen.contains("curl http://x\\u{5c}y"),
        "a kept mark is not marked again:\n{screen}"
    );
    assert!(!screen.contains("\\u{5c}u{"), "{screen}");
    for rule in ["secrets path", "configuration", "kill"] {
        assert!(screen.contains(rule));
    }
}

// §17 item 4: a decision with no call text kept is told as DENIED, not as an empty line.
#[test]
fn a_denied_decision_without_call_text_says_so() {
    let mut snap = sample();
    snap.alerts[0].calls = vec![(NOW - 10, String::new(), "protected".to_owned())];
    let mut t = T::new(snap);
    t.select_alert("probing:aws");
    t.code(KeyCode::Enter);
    assert!(t.screen().contains("DENIED"));
}

// §17.10: the classifier's marks reach the screen's parts: each kind keeps its word, a script's
// ranges are the screen's source n + 1, the reason word comes with a DENIED range, a call that
// cannot be read is marked whole. (Built against the marks contract C2 implements.)
#[test]
fn the_classifiers_marks_become_the_screens_parts() {
    use lunchroom_core::classify::{Mark, MarkKind, MarkSource, Marked};
    use lunchroom_core::policy::DenyReason;
    let marked = Marked {
        text: "bash x.sh".to_owned(),
        scripts: vec!["rm -rf /".to_owned()],
        marks: vec![
            Mark {
                source: MarkSource::Call,
                start: 0,
                end: 4,
                kinds: vec![MarkKind::NotRead, MarkKind::Destroys],
                words: "unknown tool".to_owned(),
                reason: None,
            },
            Mark {
                source: MarkSource::Script(0),
                start: 0,
                end: 8,
                kinds: vec![MarkKind::Loosens, MarkKind::Reply],
                words: "deletes".to_owned(),
                reason: Some(DenyReason::Unparsed),
            },
        ],
        whole_call_not_read: true,
    };
    let (parts, unread) = crate::console::live::parts_of(&marked);
    assert!(unread);
    assert_eq!(parts.len(), 2);
    assert_eq!((parts[0].source, parts[0].start, parts[0].end), (0, 0, 4));
    assert_eq!(parts[0].kinds, vec![PartKind::NotRead, PartKind::Destroys]);
    assert_eq!(parts[0].reason, None);
    assert_eq!(parts[1].source, 1);
    assert_eq!(parts[1].kinds, vec![PartKind::Loosens, PartKind::Reply]);
    assert_eq!(
        parts[1].reason.as_deref(),
        Some(DenyReason::Unparsed.word())
    );
    assert_eq!(parts[1].why, "deletes");
}
