//! The console screen (§17 C2 item 10), drawn from a [`Snapshot`] and a [`Ui`] with ratatui: no
//! terminal is touched here, so a test draws it on `TestBackend`. Drawing also records where
//! every click lands ([`Ui`]'s hits).

use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear};

use lunchroom_core::conversation::{Conversation, Entry, EntryKind};

use super::model::{
    AgentRow, AgentState, AlertKind, AlertRow, CallRow, CheckRow, Fix, Item, Level, MarkedRow,
    MessageKind, MessageRow, Part, PartKind, Snapshot, strip,
};
use super::text::{self, Line, Tone, fit, one_line, visible, wrap};
use super::ui::{Hit, Layer, Modal, Pending, TRIES, Ui};

pub(super) const AMBER: Color = Color::Rgb(255, 176, 0);
pub(super) const RED: Color = Color::Rgb(255, 77, 77);
pub(super) const GREEN: Color = Color::Rgb(63, 208, 127);
pub(super) const CYAN: Color = Color::Rgb(95, 211, 243);
pub(super) const DIM: Color = Color::Rgb(91, 101, 112);
pub(super) const INK: Color = Color::Rgb(201, 209, 217);
pub(super) const SELECTED: Color = Color::Rgb(27, 42, 54);
pub(super) const HOT: Color = Color::Rgb(74, 16, 16);

/// The width of the six alert cells and where each starts: the same on every screen.
#[must_use]
pub fn cell_edges(width: u16) -> [u16; 7] {
    let mut edges = [0u16; 7];
    for (i, edge) in edges.iter_mut().enumerate() {
        *edge = u16::try_from(usize::from(width) * i / 6).unwrap_or(width);
    }
    edges
}

fn style(tone: Tone) -> Style {
    let base = Style::default().fg(INK);
    match tone {
        Tone::Normal => base,
        Tone::Dim | Tone::Label => Style::default().fg(DIM),
        Tone::Key => Style::default().fg(CYAN),
        Tone::Amber => Style::default().fg(AMBER),
        Tone::Red => Style::default().fg(RED),
        Tone::Green => Style::default().fg(GREEN),
        Tone::Warn => Style::default().bg(HOT).fg(Color::Rgb(255, 208, 208)),
        Tone::Mark => Style::default()
            .fg(Color::Black)
            .bg(AMBER)
            .add_modifier(Modifier::BOLD),
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, max_x: u16, text: &str, st: Style) -> u16 {
    let max_x = max_x.min(buf.area.right());
    if x >= max_x || y < buf.area.top() || y >= buf.area.bottom() {
        return x;
    }
    let (nx, _) = buf.set_stringn(x, y, text, usize::from(max_x - x), st);
    nx
}

fn put_line(buf: &mut Buffer, area: Rect, row: u16, line: &Line, base_bg: Option<Color>) {
    let mut x = area.x;
    let end = area.x.saturating_add(area.width);
    for (tone, piece) in line {
        let mut st = style(*tone);
        if let Some(bg) = base_bg
            && st.bg.is_none()
        {
            st = st.bg(bg);
        }
        x = put(buf, x, area.y + row, end, piece, st);
    }
}

fn fill_bg(buf: &mut Buffer, area: Rect, row: u16, bg: Color) {
    for x in area.x..area.x.saturating_add(area.width) {
        if let Some(cell) = buf.cell_mut((x, area.y + row)) {
            cell.set_bg(bg);
        }
    }
}

fn hhmm(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 3_600 % 24, secs / 60 % 60)
}

fn hhmmss(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3_600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A strip cell's text fitted to `w` columns: when it is too long the label is cut and the count
/// stays, so the number is never the part that is lost.
fn fit_cell(text: &str, w: usize) -> String {
    if text::width(text) <= w {
        return text.to_owned();
    }
    match text.rsplit_once(' ') {
        Some((label, count)) if count.chars().all(|c| c.is_ascii_digit()) => {
            let room = w.saturating_sub(text::width(count) + 1);
            format!("{} {count}", fit(label, room))
        }
        _ => fit(text, w),
    }
}

fn pad(text: &str, w: usize) -> String {
    let cut = fit(text, w);
    let used = text::width(&cut);
    format!("{cut}{}", " ".repeat(w.saturating_sub(used)))
}

fn block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .title(format!(" {title} "))
        .title_style(Style::default().fg(DIM))
}

// ---------------------------------------------------------------------------------------------
// The key line.
// ---------------------------------------------------------------------------------------------

/// One thing on the key line.
#[derive(Debug, Clone, Copy)]
struct KeyItem {
    label: &'static str,
    word: &'static str,
    code: KeyCode,
    /// Drawn against the item before it, with no space between.
    glued: bool,
}

const KEYS: [KeyItem; 16] = [
    KeyItem {
        label: "↑",
        word: "",
        code: KeyCode::Up,
        glued: false,
    },
    KeyItem {
        label: "↓",
        word: "choose",
        code: KeyCode::Down,
        glued: true,
    },
    KeyItem {
        label: "Enter",
        word: "open",
        code: KeyCode::Enter,
        glued: false,
    },
    KeyItem {
        label: "Esc",
        word: "close",
        code: KeyCode::Esc,
        glued: false,
    },
    KeyItem {
        label: "a",
        word: "approve",
        code: KeyCode::Char('a'),
        glued: false,
    },
    KeyItem {
        label: "x",
        word: "refuse",
        code: KeyCode::Char('x'),
        glued: false,
    },
    KeyItem {
        label: "r",
        word: "release",
        code: KeyCode::Char('r'),
        glued: false,
    },
    KeyItem {
        label: "s",
        word: "resend",
        code: KeyCode::Char('s'),
        glued: false,
    },
    KeyItem {
        label: "d",
        word: "discard",
        code: KeyCode::Char('d'),
        glued: false,
    },
    KeyItem {
        label: "h",
        word: "hold",
        code: KeyCode::Char('h'),
        glued: false,
    },
    KeyItem {
        label: "u",
        word: "unregister",
        code: KeyCode::Char('u'),
        glued: false,
    },
    KeyItem {
        label: "m",
        word: "mode",
        code: KeyCode::Char('m'),
        glued: false,
    },
    KeyItem {
        label: "y",
        word: "yolo",
        code: KeyCode::Char('y'),
        glued: false,
    },
    KeyItem {
        label: "c",
        word: "controller",
        code: KeyCode::Char('c'),
        glued: false,
    },
    // retired-words:begin (§17 item 3: the screen that hides after 15 minutes, not a lock of a session)
    KeyItem {
        label: "l",
        word: "lock",
        code: KeyCode::Char('l'),
        glued: false,
    },
    // retired-words:end
    KeyItem {
        label: "q",
        word: "quit",
        code: KeyCode::Char('q'),
        glued: false,
    },
];

/// Where each key sits: `(row, x, width, item index)`, packed in rows of `width` columns.
fn key_places(width: u16) -> Vec<(u16, u16, u16, usize)> {
    let width = usize::from(width).max(1);
    let (mut row, mut x) = (0usize, 1usize);
    let mut out = Vec::new();
    for (i, k) in KEYS.iter().enumerate() {
        let w = text::width(k.label)
            + if k.word.is_empty() {
                0
            } else {
                1 + text::width(k.word)
            };
        let gap = if k.glued || x == 1 { 0 } else { 2 };
        if x + gap + w > width && x > 1 && !k.glued {
            row += 1;
            x = 1;
        }
        let at = x + if k.glued || x == 1 { 0 } else { 2 };
        out.push((
            u16::try_from(row).unwrap_or(u16::MAX),
            u16::try_from(at).unwrap_or(u16::MAX),
            u16::try_from(w).unwrap_or(u16::MAX),
            i,
        ));
        x = at + w;
    }
    out
}

/// How many rows the key line takes at `width` columns.
#[must_use]
pub fn key_rows(width: u16) -> u16 {
    key_places(width)
        .iter()
        .map(|p| p.0)
        .max()
        .map_or(1, |r| r + 1)
}

/// The key a click at (`x`, `y`) on the key line of a screen `width` wide whose key line starts at
/// row `top` presses; for the tests of "a click on each key".
#[cfg(test)]
#[must_use]
pub fn key_at(width: u16, top: u16, x: u16, y: u16) -> Option<KeyCode> {
    key_places(width)
        .into_iter()
        .find(|(row, at, w, _)| top + row == y && x >= *at && x < at + w)
        .map(|(_, _, _, i)| KEYS[i].code)
}

/// The place of the key for `cmd` on the key line: `(row, x)`.
#[cfg(test)]
#[must_use]
pub fn key_place(width: u16, cmd: super::model::Cmd) -> Option<(u16, u16)> {
    let code = super::ui::key_of(cmd);
    key_places(width)
        .into_iter()
        .find(|(_, _, _, i)| KEYS[*i].code == code)
        .map(|(row, x, _, _)| (row, x))
}

fn draw_keys(buf: &mut Buffer, ui: &mut Ui, area: Rect) {
    for (row, x, w, i) in key_places(area.width) {
        let k = KEYS[i];
        let y = area.y + row;
        if y >= area.y + area.height {
            continue;
        }
        let end = area.x + area.width;
        let nx = put(buf, area.x + x, y, end, k.label, style(Tone::Key));
        if !k.word.is_empty() {
            put(
                buf,
                nx,
                y,
                end,
                &format!(" {}", k.word),
                style(Tone::Normal),
            );
        }
        ui.hits.push((
            Layer::Keys,
            Rect::new(area.x + x, y, w.min(area.width.saturating_sub(x)), 1),
            Hit::Press(k.code),
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// The screen.
// ---------------------------------------------------------------------------------------------

/// Draws the screen for `snap` at time `now`.
pub fn draw(frame: &mut Frame<'_>, ui: &mut Ui, snap: &Snapshot, now: u64) {
    let area = frame.area();
    ui.hits.clear();
    ui.sync(snap);
    let buf = frame.buffer_mut();
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(lock) = &ui.lock
        && lock.start
    {
        draw_start(buf, area, lock.typed.count(), lock.wrong);
        return;
    }
    // A locked screen shows the alert strip and the prompt, and nothing else (§17 item 3).
    if ui.lock.is_none() {
        draw_header(buf, ui, snap, area, now);
    }
    draw_strip(buf, ui, snap, area);
    let top = 4u16.min(area.height);
    if let Some(lock) = &ui.lock {
        let typed = lock.typed.count();
        draw_locked(buf, area, top, typed, lock.wrong);
        return;
    }
    let keys = key_rows(area.width).min(area.height.saturating_sub(top).max(1));
    let status_y = area.y + area.height.saturating_sub(keys + 1);
    let body_h = status_y.saturating_sub(area.y + top);
    let body = Rect::new(area.x, area.y + top, area.width, body_h);
    if body.height > 0 {
        draw_body(buf, ui, snap, body, now);
    }
    if area.height > top + keys {
        let msg = one_line(&ui.message, usize::from(area.width).saturating_sub(2));
        put(
            buf,
            area.x + 1,
            status_y,
            area.x + area.width,
            &msg,
            style(Tone::Amber),
        );
    }
    let key_area = Rect::new(area.x, area.y + area.height - keys, area.width, keys);
    draw_keys(buf, ui, key_area);
    if ui.detail.is_some() && body.height > 0 {
        draw_detail(buf, ui, snap, body, now);
    }
    if ui.conv.is_some() && body.height > 0 {
        draw_conv(buf, ui, snap, body);
        if ui.line.is_some() {
            draw_line(buf, ui, snap, body);
        }
    }
    if ui.modal.is_some() {
        draw_modal(buf, ui, area);
    }
}

fn draw_start(buf: &mut Buffer, area: Rect, typed: usize, wrong: u8) {
    let y = area.y + area.height / 2;
    let prompt = format!("Lunchroom passphrase: {}▌", "•".repeat(typed.min(60)));
    put(
        buf,
        area.x + 2,
        y.saturating_sub(1),
        area.x + area.width,
        "LUNCHROOM CONSOLE",
        style(Tone::Normal).add_modifier(Modifier::BOLD),
    );
    put(
        buf,
        area.x + 2,
        y,
        area.x + area.width,
        &prompt,
        style(Tone::Key),
    );
    if wrong > 0 {
        let left = TRIES.saturating_sub(wrong);
        put(
            buf,
            area.x + 2,
            y + 1,
            area.x + area.width,
            &format!("The passphrase was wrong. {left} tries left."),
            style(Tone::Red),
        );
    }
}

fn draw_header(buf: &mut Buffer, ui: &Ui, snap: &Snapshot, area: Rect, now: u64) {
    let end = area.x + area.width;
    let mut x = area.x;
    x = put(
        buf,
        x,
        area.y,
        end,
        "LUNCHROOM CONSOLE",
        style(Tone::Normal).add_modifier(Modifier::BOLD),
    );
    // The five items of the header always fit: a long login name gives way (to its first
    // characters and a mark) before anything else does.
    let mode = one_line(&snap.hold_mode, 12).to_uppercase();
    let fixed = 17 + 2 + 14 + 12 + text::width(&mode) + 18 + 5;
    let login = usize::from(area.width).saturating_sub(fixed).clamp(3, 24);
    x = put(
        buf,
        x,
        area.y,
        end,
        &format!("  {}", one_line(&snap.developer, login)),
        style(Tone::Dim),
    );
    x = put(
        buf,
        x,
        area.y,
        end,
        &format!("  {} UTC", hhmmss(snap.now)),
        style(Tone::Normal),
    );
    x = put(buf, x, area.y, end, "  HOLD MODE ", style(Tone::Normal));
    x = put(
        buf,
        x,
        area.y,
        end,
        &mode,
        style(Tone::Normal).add_modifier(Modifier::BOLD),
    );
    x = put(buf, x, area.y, end, "  screen locks in ", style(Tone::Dim));
    let left = ui.lock_left(now);
    let tone = if left < 120 {
        Tone::Amber
    } else {
        Tone::Normal
    };
    put(
        buf,
        x,
        area.y,
        end,
        &format!("{:02}:{:02}", left / 60, left % 60),
        style(tone),
    );
}

fn draw_strip(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, area: Rect) {
    if area.height < 2 {
        return;
    }
    let edges = cell_edges(area.width);
    let cells = strip(snap);
    let dim = style(Tone::Dim);
    let end = area.x + area.width;
    // The three rows: the top rule, the cells, the bottom rule.
    for (row, (l, m, r)) in [(1u16, ('┌', '┬', '┐')), (3, ('└', '┴', '┘'))] {
        let y = area.y + row;
        if y >= area.y + area.height {
            continue;
        }
        for x in 0..area.width {
            let ch = if x == 0 {
                l
            } else if x + 1 == area.width {
                r
            } else if edges[1..6].contains(&x) {
                m
            } else {
                '─'
            };
            put(buf, area.x + x, y, end, &ch.to_string(), dim);
        }
    }
    let y = area.y + 2;
    if y >= area.y + area.height {
        return;
    }
    for x in std::iter::once(0)
        .chain(edges[1..6].iter().copied())
        .chain(std::iter::once(area.width.saturating_sub(1)))
    {
        put(buf, area.x + x, y, end, "│", dim);
    }
    for (i, cell) in cells.iter().enumerate() {
        let x0 = edges[i] + 1;
        let x1 = edges[i + 1].min(area.width.saturating_sub(1)).max(x0);
        let w = usize::from(x1.saturating_sub(x0));
        let shown = fit_cell(&cell.text, w);
        let used = text::width(&shown);
        let left = w.saturating_sub(used) / 2;
        let line = format!(
            "{}{}{}",
            " ".repeat(left),
            shown,
            " ".repeat(w.saturating_sub(used + left))
        );
        let st = match cell.level {
            Level::Dark => dim,
            Level::Amber => Style::default()
                .bg(AMBER)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            Level::Red => Style::default()
                .bg(RED)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            Level::Green => Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        };
        put(buf, area.x + x0, y, end, &line, st);
        ui.hits.push((
            Layer::Base,
            Rect::new(
                area.x + edges[i],
                area.y + 1,
                edges[i + 1] - edges[i],
                area.height.min(3),
            ),
            Hit::Cell(i),
        ));
    }
}

fn draw_locked(buf: &mut Buffer, area: Rect, top: u16, typed: usize, wrong: u8) {
    let y = area.y + top + 1;
    if y >= area.y + area.height {
        return;
    }
    let end = area.x + area.width;
    // retired-words:begin (§17 item 3: the screen that hides after 15 minutes, not a lock of a session)
    let x = put(
        buf,
        area.x + 3,
        y,
        end,
        "LOCKED",
        style(Tone::Normal).add_modifier(Modifier::BOLD),
    );
    // retired-words:end
    put(
        buf,
        x,
        y,
        end,
        "  The details are hidden. The alert strip above stays live.",
        style(Tone::Normal),
    );
    put(
        buf,
        area.x + 3,
        y + 1,
        end,
        &format!("Passphrase to unlock: {}▌", "•".repeat(typed.min(60))),
        style(Tone::Key),
    );
    if wrong > 0 {
        put(
            buf,
            area.x + 3,
            y + 2,
            end,
            &format!(
                "The passphrase was wrong. {} tries left.",
                TRIES.saturating_sub(wrong)
            ),
            style(Tone::Red),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The body: calls, agents, alerts and the guard.
// ---------------------------------------------------------------------------------------------

/// The first row of a window of `height` rows that keeps row `at` in view.
const fn window(at: Option<usize>, height: usize) -> usize {
    match at {
        Some(i) if height > 0 && i >= height => i + 1 - height,
        _ => 0,
    }
}

fn draw_body(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, body: Rect, now: u64) {
    let rows = Layout::vertical([
        Constraint::Fill(3),
        Constraint::Fill(3),
        Constraint::Fill(3),
        Constraint::Fill(3),
    ])
    .split(body);
    let (calls_row, agents_row, conv_row, alerts_row) = (rows[0], rows[1], rows[2], rows[3]);
    let cols = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(calls_row);
    draw_calls(buf, ui, snap, cols[0]);
    draw_call_beside(buf, ui, snap, cols[1]);
    draw_agents(buf, ui, snap, agents_row);
    draw_conversations(buf, ui, snap, conv_row);
    let cols = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(alerts_row);
    draw_alerts(buf, ui, snap, cols[0], now);
    draw_guard(buf, snap, cols[1]);
}

fn framed(buf: &mut Buffer, title: &str, area: Rect) -> Rect {
    let b = block(title);
    let inner = b.inner(area);
    ratatui::widgets::Widget::render(b, area, buf);
    inner
}

fn selected(ui: &Ui, item: &Item<'_>) -> bool {
    ui.sel.as_ref() == Some(&item.key())
}

fn draw_calls(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, area: Rect) {
    let inner = framed(buf, "CALLS WAITING FOR YOU", area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    if snap.waiting.is_empty() {
        put(
            buf,
            inner.x + 1,
            inner.y,
            inner.x + inner.width,
            "none waiting",
            style(Tone::Dim),
        );
        return;
    }
    let at = snap
        .waiting
        .iter()
        .position(|c| ui.sel == Some(Item::Call(c).key()));
    let start = window(at, usize::from(inner.height));
    for (row, c) in snap
        .waiting
        .iter()
        .skip(start)
        .take(usize::from(inner.height))
        .enumerate()
    {
        let row = u16::try_from(row).unwrap_or(0);
        let item = Item::Call(c);
        let is_sel = selected(ui, &item);
        let w = usize::from(inner.width);
        let agent_w = (w / 3).clamp(6, 16);
        let tool_w = (w / 4).clamp(4, 10);
        let text = format!(
            "{} {:<2} {} {} asked {} ago",
            if is_sel { "▶" } else { " " },
            c.number,
            pad(&one_line(&c.agent, agent_w), agent_w),
            pad(&one_line(&c.tool, tool_w), tool_w),
            age(c.age_secs)
        );
        let line: Line = vec![(Tone::Normal, fit(&text, w))];
        if is_sel {
            fill_bg(buf, inner, row, SELECTED);
        }
        put_line(buf, inner, row, &line, is_sel.then_some(SELECTED));
        ui.hits.push((
            Layer::Base,
            Rect::new(inner.x, inner.y + row, inner.width, 1),
            Hit::Item(item.key()),
        ));
    }
}

fn draw_call_beside(buf: &mut Buffer, ui: &Ui, snap: &Snapshot, area: Rect) {
    let chosen: Option<&CallRow> = match ui.sel.as_ref().and_then(|k| snap.find(k)) {
        Some(Item::Call(c)) => Some(c),
        _ => snap.waiting.first(),
    };
    let title = chosen.map_or_else(|| "CALL".to_owned(), |c| format!("CALL {}", c.number));
    let inner = framed(buf, &title, area);
    let Some(c) = chosen else {
        return;
    };
    let w = usize::from(inner.width).saturating_sub(10).max(1);
    let rows: [(&str, String); 3] = [
        (
            "agent",
            format!("{} ({})", one_line(&c.agent, w), one_line(&c.harness, 12)),
        ),
        ("tool", one_line(&c.tool, w)),
        ("asked", format!("{} ago", age(c.age_secs))),
    ];
    for (i, (label, value)) in rows.iter().enumerate() {
        let y = u16::try_from(i).unwrap_or(0);
        if y >= inner.height {
            return;
        }
        let end = inner.x + inner.width;
        let x = put(
            buf,
            inner.x + 1,
            inner.y + y,
            end,
            &format!("{label:<9}"),
            style(Tone::Label),
        );
        put(
            buf,
            x,
            inner.y + y,
            end,
            &fit(value, w),
            style(Tone::Normal),
        );
    }
    if inner.height > 4 {
        put(
            buf,
            inner.x + 1,
            inner.y + 4,
            inner.x + inner.width,
            "Enter shows the whole call",
            style(Tone::Dim),
        );
    }
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn sparkline(spans: &[u16; 12]) -> Vec<(Tone, String)> {
    let max = spans.iter().copied().max().unwrap_or(0).max(1);
    spans
        .iter()
        .map(|&n| {
            if n == 0 {
                (Tone::Dim, SPARK[0].to_string())
            } else {
                let level = 1 + usize::from(n) * 6 / usize::from(max);
                (Tone::Red, SPARK[level.min(7)].to_string())
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Harness,
    State,
    Queued,
    Held,
    Denied,
    Yolo,
}

/// The columns that fit in `w`, taking them in the order of importance.
fn columns(w: usize) -> Vec<(Col, usize)> {
    let all = [
        (Col::Name, 14usize),
        (Col::State, 9),
        (Col::Denied, 18),
        (Col::Queued, 6),
        (Col::Held, 14),
        (Col::Harness, 11),
        (Col::Yolo, 12),
    ];
    let mut used = 0usize;
    let mut keep: Vec<(Col, usize)> = Vec::new();
    for (col, cw) in all {
        if col == Col::Yolo {
            // What is left, for the whole of what a YOLO says.
            let left = w.saturating_sub(used + 1);
            if left >= cw {
                keep.push((col, left.min(48)));
            }
        } else if used + cw + usize::from(!keep.is_empty()) <= w {
            used += cw + usize::from(!keep.is_empty());
            keep.push((col, cw));
        }
    }
    if keep.is_empty() {
        keep.push((Col::Name, w.max(1)));
    }
    let order = [
        Col::Name,
        Col::Harness,
        Col::State,
        Col::Queued,
        Col::Held,
        Col::Denied,
        Col::Yolo,
    ];
    order
        .iter()
        .filter_map(|o| keep.iter().find(|(c, _)| c == o).copied())
        .collect()
}

fn state_tone(s: AgentState) -> Tone {
    match s {
        AgentState::Available => Tone::Green,
        AgentState::Held | AgentState::Unguarded | AgentState::Unknown => Tone::Amber,
        AgentState::Stopped => Tone::Dim,
    }
}

fn agent_cells(a: &AgentRow, col: Col, w: usize) -> Line {
    match col {
        Col::Name => vec![(Tone::Normal, pad(&one_line(&a.name, w), w))],
        Col::Harness => vec![(Tone::Normal, pad(&one_line(&a.harness, w), w))],
        Col::State => vec![(state_tone(a.state), pad(a.state.word(), w))],
        Col::Queued => vec![(Tone::Normal, pad(&a.queued.to_string(), w))],
        Col::Held => {
            let names: Vec<String> = a.held_by.iter().map(|h| visible(&h.from)).collect();
            vec![(Tone::Amber, pad(&one_line(&names.join(","), w), w))]
        }
        Col::Denied => {
            let mut cells = sparkline(&a.denied_spans);
            let tone = if a.denied_hour > 0 {
                Tone::Red
            } else {
                Tone::Dim
            };
            cells.push((
                tone,
                pad(&format!(" {}", a.denied_hour), w.saturating_sub(12)),
            ));
            cells
        }
        Col::Yolo => {
            let text = a
                .yolo
                .as_deref()
                .map_or_else(String::new, |y| one_line(y, w));
            vec![(Tone::Amber, pad(&text, w))]
        }
    }
}

fn draw_agents(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, area: Rect) {
    let inner = framed(buf, &format!("AGENTS ({})", snap.agents.len()), area);
    if inner.height == 0 || inner.width < 2 {
        return;
    }
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 1, inner.height);
    let cols = columns(usize::from(inner.width));
    let head: Line = cols
        .iter()
        .flat_map(|(c, w)| {
            let name = match c {
                Col::Name => "NAME",
                Col::Harness => "HARNESS",
                Col::State => "STATE",
                Col::Queued => "QUEUED",
                Col::Held => "HELD BY",
                Col::Denied => "DENIED 60 min",
                Col::Yolo => "YOLO",
            };
            [(Tone::Label, pad(name, *w)), (Tone::Label, " ".to_owned())]
        })
        .collect();
    put_line(buf, inner, 0, &head, None);
    if snap.agents.is_empty() {
        if inner.height > 1 {
            put(
                buf,
                inner.x,
                inner.y + 1,
                inner.x + inner.width,
                "no agents registered",
                style(Tone::Dim),
            );
        }
        return;
    }
    let rows = usize::from(inner.height).saturating_sub(1);
    let at = snap
        .agents
        .iter()
        .position(|a| ui.sel == Some(Item::Agent(a).key()));
    let start = window(at, rows);
    for (row, a) in snap.agents.iter().skip(start).take(rows).enumerate() {
        let y = u16::try_from(row + 1).unwrap_or(0);
        let item = Item::Agent(a);
        let is_sel = selected(ui, &item);
        let mut line: Line = Vec::new();
        for (c, w) in &cols {
            line.extend(agent_cells(a, *c, *w));
            line.push((Tone::Normal, " ".to_owned()));
        }
        if a.state == AgentState::Stopped {
            for piece in &mut line {
                if piece.0 == Tone::Normal {
                    piece.0 = Tone::Dim;
                }
            }
        }
        if is_sel {
            fill_bg(buf, inner, y, SELECTED);
        }
        put_line(buf, inner, y, &line, is_sel.then_some(SELECTED));
        ui.hits.push((
            Layer::Base,
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Hit::Item(item.key()),
        ));
    }
}

/// The main screen's line for one pair: the names, the number of lines and the time of the last,
/// and no text of any line (§17.10).
fn conversation_line(c: &Conversation, w: usize) -> Line {
    let tail = format!("  {} lines  last {} UTC", c.entries.len(), hhmm(c.last()));
    let room = w.saturating_sub(text::width(&tail));
    let mut line = vec![(Tone::Normal, pad(&one_line(&c.label, room), room))];
    line.push((Tone::Dim, tail));
    line
}

fn draw_conversations(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, area: Rect) {
    let inner = framed(
        buf,
        &format!("CONVERSATIONS ({})", snap.conversations.len()),
        area,
    );
    if inner.height == 0 || inner.width < 2 {
        return;
    }
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 1, inner.height);
    if snap.conversations.is_empty() {
        put(
            buf,
            inner.x,
            inner.y,
            inner.x + inner.width,
            "no agent has exchanged a message",
            style(Tone::Dim),
        );
        return;
    }
    let at = snap
        .conversations
        .iter()
        .position(|c| ui.sel == Some(Item::Conversation(c).key()));
    let rows = usize::from(inner.height);
    let start = window(at, rows);
    for (row, c) in snap.conversations.iter().skip(start).take(rows).enumerate() {
        let y = u16::try_from(row).unwrap_or(0);
        let item = Item::Conversation(c);
        let is_sel = selected(ui, &item);
        if is_sel {
            fill_bg(buf, inner, y, SELECTED);
        }
        put_line(
            buf,
            inner,
            y,
            &conversation_line(c, usize::from(inner.width)),
            is_sel.then_some(SELECTED),
        );
        ui.hits.push((
            Layer::Base,
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Hit::Item(item.key()),
        ));
    }
}

/// What a conversation shows when it is opened as an ordinary item: never used, since opening it
/// makes the conversation panel; kept so every item has a text.
fn conversation_detail(c: &Conversation) -> (String, Vec<Line>, Actions) {
    let lines = c.entries.iter().map(|e| entry_row(e, 80)).collect();
    (
        format!("CONVERSATION {}", visible(&c.label)),
        lines,
        Vec::new(),
    )
}

/// One line of the conversation panel: its time, its sender and recipient (Lunchroom for a line of
/// Lunchroom's), and the first line of its text cut to the width of the panel, or its words.
fn entry_row(e: &Entry, w: usize) -> Line {
    if e.kind == EntryKind::Line {
        // A line of Lunchroom's already begins with its time and `Lunchroom:` (§16 item 1).
        return vec![(Tone::Amber, one_line(&e.text, w))];
    }
    let head = fit(
        &format!(
            "{} UTC {} to {}: ",
            hhmm(e.at),
            visible(&e.from),
            visible(&e.to)
        ),
        w.saturating_sub(1).max(1),
    );
    let room = w.saturating_sub(text::width(&head));
    let first = e.text.split('\n').next().unwrap_or("");
    vec![(Tone::Dim, head), (Tone::Normal, one_line(first, room))]
}

fn attention_line(item: &Item<'_>, w: usize) -> Line {
    let (tone, text) = match item {
        Item::Alert(a) => (
            if a.kind == AlertKind::Probing {
                Tone::Red
            } else {
                Tone::Amber
            },
            format!("{} {}", hhmm(a.at), one_line(&a.line, w)),
        ),
        Item::Message(m) => (
            if m.kind == MessageKind::Failed {
                Tone::Amber
            } else {
                Tone::Dim
            },
            format!(
                "{} {} {} to {} - {}",
                hhmm(m.at),
                if m.kind == MessageKind::Failed {
                    "FAILED"
                } else {
                    "QUEUED"
                },
                one_line(&m.id, 20),
                one_line(&m.to, 20),
                one_line(&m.reason, w)
            ),
        ),
        Item::Check(c) => (Tone::Red, format!("FAIL {}", one_line(&c.name, w))),
        Item::Marked(m) => (Tone::Amber, format!("MARKED {}", one_line(&m.path, w))),
        Item::Call(_) | Item::Agent(_) | Item::Conversation(_) => (Tone::Normal, String::new()),
    };
    vec![(tone, fit(&text, w))]
}

fn draw_alerts(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, area: Rect, _now: u64) {
    let items = snap.attention();
    let inner = framed(buf, &format!("ALERTS ({})", items.len()), area);
    if inner.height == 0 || inner.width < 2 {
        return;
    }
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 1, inner.height);
    if items.is_empty() {
        put(
            buf,
            inner.x,
            inner.y,
            inner.x + inner.width,
            "nothing needs you",
            style(Tone::Dim),
        );
        return;
    }
    let height = usize::from(inner.height);
    let rows = alert_rows(&items, height, items.iter().position(|i| selected(ui, i)));
    for (row, line) in rows.iter().enumerate() {
        let y = u16::try_from(row).unwrap_or(0);
        let Some(i) = line else {
            let hidden = hidden_notices(&items, &rows);
            put(
                buf,
                inner.x,
                inner.y + y,
                inner.x + inner.width,
                &format!("+{hidden} more notices (the arrow keys reach them)"),
                style(Tone::Dim),
            );
            continue;
        };
        let item = &items[*i];
        let is_sel = selected(ui, item);
        if is_sel {
            fill_bg(buf, inner, y, SELECTED);
        }
        put_line(
            buf,
            inner,
            y,
            &attention_line(item, usize::from(inner.width)),
            is_sel.then_some(SELECTED),
        );
        ui.hits.push((
            Layer::Base,
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Hit::Item(item.key()),
        ));
    }
}

/// The notices of `items` that `rows` does not show: the count the line above them gives. Items to
/// act on that are scrolled out of view are not notices and are not counted.
pub(super) fn hidden_notices(items: &[Item<'_>], rows: &[Option<usize>]) -> usize {
    let notice = |i: &Item<'_>| matches!(i, Item::Alert(a) if a.kind == AlertKind::Notice);
    let shown = rows
        .iter()
        .flatten()
        .filter(|&&i| notice(&items[i]))
        .count();
    items.iter().filter(|i| notice(i)).count() - shown
}

/// How many rows of a full alerts box are kept for the newest notices (decision of 2026-11-14, "Both
/// always visible").
const NOTICE_ROWS: usize = 2;

/// The rows of the alerts box, top to bottom: each the index of an item of `items` (which lists
/// the items to act on first, the notices, newest first, last), or `None` for the line that counts
/// the notices not shown. decision of 2026-11-14 ("Both always visible"): when everything fits, every
/// item; when not, the items to act on (scrolled to the selected one) above the line that counts
/// the hidden notices and the newest notices, at least [`NOTICE_ROWS`] of them and as many as the
/// rows left allow; a selected notice below those is
/// reached by scrolling the whole list.
pub(super) fn alert_rows(
    items: &[Item<'_>],
    height: usize,
    at: Option<usize>,
) -> Vec<Option<usize>> {
    let notice = |i: &Item<'_>| matches!(i, Item::Alert(a) if a.kind == AlertKind::Notice);
    let first_notice = items.iter().position(notice).unwrap_or(items.len());
    let notices = items.len() - first_notice;
    // At least NOTICE_ROWS notices, and as many more as the rows the items to act on leave.
    let kept = notices.min(NOTICE_ROWS.max(height.saturating_sub(first_notice + 1)));
    let hidden = notices - kept;
    let reserved = kept + usize::from(hidden > 0);
    let deep = at.is_some_and(|a| a >= first_notice + kept);
    if items.len() <= height || deep || height <= reserved {
        let start = window(at, height);
        return (start..items.len().min(start + height)).map(Some).collect();
    }
    let room = height - reserved;
    let start = window(at.filter(|a| *a < first_notice), room);
    let mut rows: Vec<Option<usize>> = (start..first_notice.min(start + room)).map(Some).collect();
    if hidden > 0 {
        rows.push(None);
    }
    rows.extend((first_notice..first_notice + kept).map(Some));
    rows
}

fn draw_guard(buf: &mut Buffer, snap: &Snapshot, area: Rect) {
    let inner = framed(buf, "GUARD", area);
    if inner.height == 0 || inner.width < 2 {
        return;
    }
    let inner = Rect::new(inner.x + 1, inner.y, inner.width - 1, inner.height);
    let failing = snap.checks.iter().filter(|c| !c.ok).count();
    let passing = snap.checks.len() - failing;
    let queued = snap
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::Queued)
        .count();
    let failed = snap.messages.len() - queued;
    let g = &snap.guard;
    #[allow(clippy::cast_precision_loss)] // a decision time in seconds, shown to one place
    let slowest = g.slowest_ms as f64 / 1000.0;
    let lines: Vec<Line> = vec![
        vec![
            (Tone::Label, "recorded 24h   ".to_owned()),
            (
                Tone::Normal,
                format!("{}   slowest {slowest:.1} s", g.decisions),
            ),
        ],
        vec![
            (Tone::Label, "slow >2 s 1h   ".to_owned()),
            (
                if g.slow > 0 {
                    Tone::Amber
                } else {
                    Tone::Normal
                },
                g.slow.to_string(),
            ),
        ],
        vec![
            (Tone::Label, "timeouts 1h    ".to_owned()),
            (
                if g.timeouts > 0 {
                    Tone::Red
                } else {
                    Tone::Normal
                },
                g.timeouts.to_string(),
            ),
        ],
        vec![
            (Tone::Label, "doctor         ".to_owned()),
            (Tone::Green, format!("{passing} OK")),
            (Tone::Normal, "  ".to_owned()),
            (
                if failing > 0 { Tone::Red } else { Tone::Normal },
                format!("{failing} FAIL"),
            ),
        ],
        vec![
            (Tone::Label, "messages       ".to_owned()),
            (Tone::Normal, format!("{queued} QUEUED  {failed} FAILED")),
        ],
    ];
    for (i, line) in lines.iter().enumerate() {
        let y = u16::try_from(i).unwrap_or(0);
        if y >= inner.height {
            break;
        }
        put_line(buf, inner, y, line, None);
    }
}

// ---------------------------------------------------------------------------------------------
// The detail panel.
// ---------------------------------------------------------------------------------------------

fn kv(label: &str, value: &str) -> Vec<Line> {
    let mut out = text::lines(value, Tone::Normal, true);
    let pad_to = 10usize;
    for (i, line) in out.iter_mut().enumerate() {
        let head = if i == 0 {
            format!("{label:<pad_to$}")
        } else {
            " ".repeat(pad_to)
        };
        line.insert(0, (Tone::Label, head));
    }
    out
}

fn heading(text: &str) -> Line {
    vec![(Tone::Label, text.to_owned())]
}

fn append(lines: &mut Vec<Line>, more: Vec<Line>) {
    let mut more = more.into_iter();
    if let Some(first) = more.next() {
        match lines.last_mut() {
            Some(last) => last.extend(first),
            None => lines.push(first),
        }
    }
    lines.extend(more);
}

/// `text` with the byte ranges `ranges` drawn in [`Tone::Warn`]: ranges may overlap, and a range
/// that is empty, past the text or inside a character is not drawn (its note says so).
fn marked_text(text: &str, ranges: &[(usize, usize)], all: bool) -> Vec<Line> {
    let valid: Vec<(usize, usize)> = ranges
        .iter()
        .copied()
        .filter(|(s, e)| {
            s < e && *e <= text.len() && text.is_char_boundary(*s) && text.is_char_boundary(*e)
        })
        .collect();
    let mut edges: Vec<usize> = vec![0, text.len()];
    for (s, e) in &valid {
        edges.push(*s);
        edges.push(*e);
    }
    edges.sort_unstable();
    edges.dedup();
    let mut out: Vec<Line> = vec![Vec::new()];
    for w in edges.windows(2) {
        let (from, to) = (w[0], w[1]);
        let hot = all || valid.iter().any(|(s, e)| *s <= from && to <= *e);
        append(
            &mut out,
            text::lines(
                &text[from..to],
                if hot { Tone::Warn } else { Tone::Normal },
                true,
            ),
        );
    }
    out
}

/// How a part reads under the call: its kinds, the range, the words of its rule and its reason.
fn part_note(p: &Part, len: usize) -> Line {
    let kinds = if p.kinds.is_empty() {
        "marked".to_owned()
    } else {
        p.kinds
            .iter()
            .map(|k| k.word())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let place = if p.start < p.end && p.end <= len {
        format!("bytes {}-{}", p.start, p.end)
    } else {
        format!("bytes {}-{} (outside the text)", p.start, p.end)
    };
    let mut l = vec![(Tone::Red, format!("! {kinds} "))];
    l.push((Tone::Dim, format!("{place}: ")));
    l.extend(text::lines(&p.why, Tone::Red, false).into_iter().flatten());
    if let Some(reason) = &p.reason {
        l.push((Tone::Dim, " (".to_owned()));
        l.extend(
            text::lines(reason, Tone::Amber, false)
                .into_iter()
                .flatten(),
        );
        l.push((Tone::Dim, ")".to_owned()));
    }
    l
}

/// The whole call with its marked parts (§17.10): each range of the call's text and of each script
/// drawn marked, and under each text a note for every part naming its kind. A call that cannot be
/// read is marked whole as "not read".
fn call_text(c: &CallRow) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let ranges = |source: usize| -> Vec<(usize, usize)> {
        c.parts
            .iter()
            .filter(|p| p.source == source)
            .map(|p| (p.start, p.end))
            .collect()
    };
    append(&mut out, marked_text(&c.command, &ranges(0), c.unread));
    let notes = |out: &mut Vec<Line>, source: usize, len: usize| {
        for p in c.parts.iter().filter(|p| p.source == source) {
            out.push(part_note(p, len));
        }
    };
    if c.unread {
        out.push(vec![(
            Tone::Red,
            format!("! {} the whole call", PartKind::NotRead.word()),
        )]);
    }
    notes(&mut out, 0, c.command.len());
    for (i, script) in c.scripts.iter().enumerate() {
        out.push(Vec::new());
        out.push(heading(&format!("SCRIPT {} THE CALL RUNS", i + 1)));
        out.extend(marked_text(script, &ranges(i + 1), false));
        notes(&mut out, i + 1, script.len());
    }
    // A part of a script the call does not have is still told.
    for p in c.parts.iter().filter(|p| p.source > c.scripts.len()) {
        out.push(part_note(p, 0));
    }
    out
}

type Actions = Vec<(KeyCode, &'static str)>;

fn call_detail(c: &CallRow) -> (String, Vec<Line>, Actions) {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("agent", &format!("{} ({})", c.agent, c.harness)));
    lines.extend(kv("tool", &c.tool));
    lines.extend(kv("asked", &format!("{} ago", age(c.age_secs))));
    lines.extend(kv("folder", &c.cwd));
    if !c.rule.is_empty() {
        lines.extend(kv("rule", &c.rule));
    }
    lines.push(Vec::new());
    lines.push(heading("THE WHOLE CALL"));
    lines.extend(call_text(c));
    (
        format!("CALL {}", c.number),
        lines,
        vec![
            (KeyCode::Char('a'), "approve this exact call"),
            (KeyCode::Char('x'), "refuse"),
        ],
    )
}

fn agent_detail(a: &AgentRow) -> (String, Vec<Line>, Actions) {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("agent", &a.name));
    lines.extend(kv("harness", &a.harness));
    lines.extend(kv("state", a.state.word()));
    lines.extend(kv("queued", &a.queued.to_string()));
    if let Some(y) = &a.yolo {
        lines.extend(kv("yolo", y));
    }
    let mut actions: Actions = Vec::new();
    if !a.held_by.is_empty() || a.state == AgentState::Held {
        lines.push(Vec::new());
        lines.push(heading("HELD BY"));
        for h in &a.held_by {
            lines.push(Vec::new());
            lines.extend(kv("from", &h.from));
            lines.extend(kv("at", &format!("{} UTC", hhmm(h.at))));
            lines.extend(kv("message", &h.id));
            if h.text.is_empty() {
                lines.push(vec![(
                    Tone::Dim,
                    "the store no longer holds this message".to_owned(),
                )]);
            } else {
                lines.extend(text::lines(&h.text, Tone::Normal, true));
            }
        }
        actions.push((KeyCode::Char('r'), "release"));
    }
    if matches!(a.state, AgentState::Unguarded | AgentState::Stopped) {
        lines.push(Vec::new());
        lines.push(heading("EVIDENCE MISSING, AND WHY"));
        lines.extend(text::lines(&a.missing, Tone::Normal, true));
        if let Some(cmd) = &a.repair {
            lines.push(Vec::new());
            lines.push(heading(
                "THE COMMAND THAT REPAIRS IT: run it in a terminal of your own",
            ));
            lines.push(heading("(the console shows it and does not run it)"));
            lines.extend(text::lines(cmd, Tone::Key, true));
        }
    }
    actions.push((KeyCode::Char('h'), "hold"));
    actions.push((KeyCode::Char('u'), "unregister"));
    (format!("AGENT {}", visible(&a.name)), lines, actions)
}

fn message_detail(m: &MessageRow) -> (String, Vec<Line>, Actions) {
    let failed = m.kind == MessageKind::Failed;
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("state", if failed { "FAILED" } else { "QUEUED" }));
    lines.extend(kv("from", &m.from));
    lines.extend(kv("to", &m.to));
    lines.extend(kv("at", &format!("{} UTC", hhmm(m.at))));
    lines.extend(kv("reason", &m.reason));
    lines.push(Vec::new());
    lines.push(heading("THE WHOLE TEXT"));
    lines.extend(text::lines(&m.text, Tone::Normal, true));
    let mut actions: Actions = Vec::new();
    if failed {
        actions.push((KeyCode::Char('s'), "resend"));
    }
    actions.push((KeyCode::Char('d'), "discard"));
    (
        format!(
            "{} MESSAGE {}",
            if failed { "FAILED" } else { "QUEUED" },
            visible(&m.id)
        ),
        lines,
        actions,
    )
}

fn alert_detail(a: &AlertRow) -> (String, Vec<Line>, Actions) {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("at", &format!("{} UTC", hhmm(a.at))));
    let mut actions: Actions = Vec::new();
    if a.kind == AlertKind::Probing {
        lines.extend(kv("agent", &a.agent));
        lines.push(Vec::new());
        lines.push(heading(
            "DENIED CALLS OF THE LAST 10 MINUTES, EACH WITH ITS RULE",
        ));
        // Newest first, each with its rule (§17.10); the call is as the records keep it (item 4):
        // its secrets replaced and its marks written, so it is drawn as it is.
        for (at, call, rule) in &a.calls {
            lines.push(Vec::new());
            let mut head: Line = vec![(Tone::Label, format!("{} UTC  ", hhmm(*at)))];
            if call.is_empty() {
                head.push((Tone::Dim, "DENIED".to_owned()));
            } else {
                head.extend(text::kept(call, Tone::Normal));
            }
            lines.push(head);
            lines.extend(kv("rule", rule));
        }
        actions.push((KeyCode::Char('h'), "hold the agent"));
        actions.push((KeyCode::Char('u'), "unregister"));
    } else {
        lines.push(Vec::new());
        lines.extend(text::lines(&a.text, Tone::Normal, true));
    }
    actions.push((KeyCode::Char('d'), "dismiss"));
    (
        if a.kind == AlertKind::Probing {
            "PROBING ALERT".to_owned()
        } else {
            "NOTICE".to_owned()
        },
        lines,
        actions,
    )
}

fn check_detail(c: &CheckRow) -> (String, Vec<Line>, Actions) {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("check", &c.name));
    lines.push(Vec::new());
    lines.push(heading("WHAT IT READ"));
    lines.extend(text::lines(&c.evidence, Tone::Normal, true));
    lines.push(Vec::new());
    let mut actions: Actions = Vec::new();
    match &c.fix {
        Fix::None => lines.push(heading("This check names no fix the console can do.")),
        Fix::Command(cmd) => {
            lines.push(heading("THE FIX: run it in a terminal of your own"));
            lines.push(heading("(the console shows it and does not run it)"));
            lines.extend(text::lines(cmd, Tone::Key, true));
        }
        Fix::Discard(id) => {
            lines.extend(kv(
                "the fix",
                &format!("discard the message {id}, sent to an agent no longer named"),
            ));
            actions.push((KeyCode::Char('d'), "discard it"));
        }
        Fix::Release(agent) => {
            lines.extend(kv(
                "the fix",
                &format!("release the hold on {agent}: the store no longer holds its message"),
            ));
            actions.push((KeyCode::Char('r'), "release it"));
        }
        Fix::Clear(path) => {
            lines.extend(kv("the fix", &format!("clear the marked path {path}")));
            actions.push((KeyCode::Char('d'), "clear it"));
        }
    }
    ("CHECK FAILED".to_owned(), lines, actions)
}

fn marked_detail(m: &MarkedRow) -> (String, Vec<Line>, Actions) {
    let mut lines: Vec<Line> = Vec::new();
    lines.extend(kv("path", &m.path));
    lines.extend(kv("since", &format!("{} UTC", hhmm(m.since))));
    lines.push(Vec::new());
    lines.push(heading("THE CALL THAT MARKED IT"));
    lines.extend(text::lines(&m.call, Tone::Normal, true));
    (
        "MARKED PATH".to_owned(),
        lines,
        vec![(KeyCode::Char('d'), "clear it")],
    )
}

/// The title, the whole text and the actions of the panel for `item`.
fn detail_of(item: &Item<'_>) -> (String, Vec<Line>, Actions) {
    match item {
        Item::Call(c) => call_detail(c),
        Item::Agent(a) => agent_detail(a),
        Item::Conversation(c) => conversation_detail(c),
        Item::Message(m) => message_detail(m),
        Item::Alert(a) => alert_detail(a),
        Item::Check(c) => check_detail(c),
        Item::Marked(m) => marked_detail(m),
    }
}

/// The whole text of the panel for `item` as the screen draws it (for the tests).
#[cfg(test)]
#[must_use]
pub fn panel_text(item: &Item<'_>) -> Vec<String> {
    detail_of(item)
        .1
        .iter()
        .map(|l| l.iter().map(|(_, s)| s.as_str()).collect())
        .collect()
}

/// Draws a panel over `panel` with `lines` wrapped and scrolled from `scroll`, and the actions as
/// clicks on its last row. Returns the scroll it used (kept inside the text) and the rows it shows.
#[allow(clippy::too_many_arguments)] // one panel: where, what, how it is scrolled and which layer
fn draw_text_panel(
    buf: &mut Buffer,
    hits: &mut Vec<(Layer, Rect, Hit)>,
    layer: Layer,
    panel: Rect,
    title: &str,
    lines: &[Line],
    actions: &Actions,
    scroll: usize,
) -> (usize, usize) {
    ratatui::widgets::Widget::render(Clear, panel, buf);
    let b = block(title).border_style(Style::default().fg(CYAN));
    let inner = b.inner(panel);
    ratatui::widgets::Widget::render(b, panel, buf);
    hits.push((layer, panel, Hit::Inside));
    if inner.height == 0 || inner.width < 2 {
        return (0, 0);
    }
    let content = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width - 1,
        inner.height.saturating_sub(1).max(1),
    );
    let wrapped = wrap(lines, usize::from(content.width));
    let view = usize::from(content.height);
    let max = wrapped.len().saturating_sub(view);
    let scroll = scroll.min(max);
    for (row, line) in wrapped.iter().skip(scroll).take(view).enumerate() {
        put_line(buf, content, u16::try_from(row).unwrap_or(0), line, None);
    }
    // The last row: the actions, each a click, and where the text is.
    if inner.height >= 2 {
        let y = inner.y + inner.height - 1;
        let end = inner.x + inner.width;
        let mut x = inner.x + 1;
        for (code, word) in actions {
            let label = match code {
                KeyCode::Char(c) => format!("[{c}]"),
                _ => String::new(),
            };
            let start = x;
            x = put(buf, x, y, end, &label, style(Tone::Key));
            x = put(buf, x, y, end, &format!(" {word}   "), style(Tone::Normal));
            hits.push((
                layer,
                Rect::new(
                    start,
                    y,
                    x.saturating_sub(start).min(end.saturating_sub(start)),
                    1,
                ),
                Hit::Press(*code),
            ));
        }
        let start = x;
        x = put(buf, x, y, end, "[Esc]", style(Tone::Key));
        x = put(buf, x, y, end, " close", style(Tone::Normal));
        hits.push((
            layer,
            Rect::new(
                start,
                y,
                x.saturating_sub(start).min(end.saturating_sub(start)),
                1,
            ),
            Hit::Press(KeyCode::Esc),
        ));
        let pos = format!(
            "lines {}-{} of {}",
            scroll + 1,
            (scroll + view).min(wrapped.len()),
            wrapped.len()
        );
        let px = end.saturating_sub(u16::try_from(pos.len()).unwrap_or(0) + 1);
        if px > x {
            put(buf, px, y, end, &pos, style(Tone::Dim));
        }
    }
    (scroll, view)
}

fn draw_detail(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, body: Rect, _now: u64) {
    let panel = body;
    if panel.width < 4 || panel.height < 3 {
        return;
    }
    let Some(key) = ui.detail.as_ref().map(|d| d.key.clone()) else {
        return;
    };
    let found = snap.find(&key).map(|i| detail_of(&i));
    let (title, lines, actions) = found.unwrap_or_else(|| {
        (
            "GONE".to_owned(),
            vec![vec![(
                Tone::Dim,
                "This item is no longer there.".to_owned(),
            )]],
            Vec::new(),
        )
    });
    let at = ui.detail.as_ref().map_or(0, |d| d.scroll);
    let (scroll, view) = draw_text_panel(
        buf,
        &mut ui.hits,
        Layer::Panel,
        panel,
        &title,
        &lines,
        &actions,
        at,
    );
    if let Some(d) = ui.detail.as_mut() {
        d.scroll = scroll;
        d.view = view;
    }
}

/// The conversation panel of a pair, over the screen (§17.10): one row for each line.
fn draw_conv(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, body: Rect) {
    if body.width < 4 || body.height < 3 {
        return;
    }
    let Some(key) = ui.conv.as_ref().map(|c| c.key.clone()) else {
        return;
    };
    let conv = snap.conversations.iter().find(|c| c.key == key);
    ui.sync_conv(snap);
    ratatui::widgets::Widget::render(Clear, body, buf);
    let title = conv.map_or_else(
        || "CONVERSATION".to_owned(),
        |c| format!("CONVERSATION {}", visible(&c.label)),
    );
    let b = block(&title).border_style(Style::default().fg(CYAN));
    let inner = b.inner(body);
    ratatui::widgets::Widget::render(b, body, buf);
    ui.hits.push((Layer::Conv, body, Hit::Inside));
    if inner.height == 0 || inner.width < 2 {
        return;
    }
    let list = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width - 1,
        inner.height.saturating_sub(1).max(1),
    );
    let entries: &[Entry] = conv.map_or(&[], |c| c.entries.as_slice());
    let sel_idx = ui.conv.as_ref().map_or(0, |c| c.sel_idx);
    let rows = usize::from(list.height);
    if entries.is_empty() {
        put(
            buf,
            list.x,
            list.y,
            list.x + list.width,
            "the store holds no line of this conversation",
            style(Tone::Dim),
        );
    }
    let start = window(Some(sel_idx), rows);
    for (row, e) in entries.iter().skip(start).take(rows).enumerate() {
        let y = u16::try_from(row).unwrap_or(0);
        let is_sel = ui.conv.as_ref().and_then(|c| c.sel.as_ref()) == Some(&e.key);
        if is_sel {
            fill_bg(buf, list, y, SELECTED);
        }
        put_line(
            buf,
            list,
            y,
            &entry_row(e, usize::from(list.width)),
            is_sel.then_some(SELECTED),
        );
        ui.hits.push((
            Layer::Conv,
            Rect::new(list.x, list.y + y, list.width, 1),
            Hit::Entry(e.key.clone()),
        ));
    }
    if let Some(c) = ui.conv.as_mut() {
        c.view = rows;
    }
    if inner.height >= 2 {
        let y = inner.y + inner.height - 1;
        let end = inner.x + inner.width;
        let mut x = inner.x + 1;
        for (code, label, word) in [
            (KeyCode::Enter, "[Enter]", " open the line   "),
            (KeyCode::Esc, "[Esc]", " close"),
        ] {
            let start = x;
            x = put(buf, x, y, end, label, style(Tone::Key));
            x = put(buf, x, y, end, word, style(Tone::Normal));
            ui.hits.push((
                Layer::Conv,
                Rect::new(
                    start,
                    y,
                    x.saturating_sub(start).min(end.saturating_sub(start)),
                    1,
                ),
                Hit::Press(code),
            ));
        }
        let pos = format!("{} lines", entries.len());
        let px = end.saturating_sub(u16::try_from(pos.len()).unwrap_or(0) + 1);
        if px > x {
            put(buf, px, y, end, &pos, style(Tone::Dim));
        }
    }
}

/// The second panel: the whole text of one line, never cut, scrollable (§17.10).
fn draw_line(buf: &mut Buffer, ui: &mut Ui, snap: &Snapshot, body: Rect) {
    let Some(panel_state) = ui.line.clone() else {
        return;
    };
    // Inside the conversation panel, so a click beside it is a click outside the second panel.
    let panel = if body.width >= 12 && body.height >= 7 {
        Rect::new(body.x + 2, body.y + 1, body.width - 4, body.height - 2)
    } else {
        body
    };
    if panel.width < 4 || panel.height < 3 {
        return;
    }
    let entry = ui
        .conv
        .as_ref()
        .and_then(|c| snap.conversations.iter().find(|x| x.key == c.key))
        .and_then(|c| c.entries.iter().find(|e| e.key == panel_state.entry));
    let (title, lines) = entry.map_or_else(
        || {
            (
                "GONE".to_owned(),
                vec![vec![(
                    Tone::Dim,
                    "The store no longer holds this line.".to_owned(),
                )]],
            )
        },
        |e| {
            let mut lines: Vec<Line> = Vec::new();
            lines.extend(kv("at", &format!("{} UTC", hhmm(e.at))));
            if e.kind == EntryKind::Message {
                lines.extend(kv("from", &e.from));
                lines.extend(kv("to", &e.to));
            }
            lines.push(Vec::new());
            lines.push(heading(if e.kind == EntryKind::Message {
                "THE WHOLE TEXT"
            } else {
                "LUNCHROOM'S LINE"
            }));
            // The rule for control characters holds in both panels (§17.10): a line feed is a
            // control character, so it is drawn as its mark and the text wraps at the panel.
            lines.extend(text::lines(&e.text, Tone::Normal, false));
            (
                (if e.kind == EntryKind::Message {
                    "MESSAGE"
                } else {
                    "LUNCHROOM"
                })
                .to_owned(),
                lines,
            )
        },
    );
    let (scroll, view) = draw_text_panel(
        buf,
        &mut ui.hits,
        Layer::Line,
        panel,
        &title,
        &lines,
        &Vec::new(),
        panel_state.scroll,
    );
    if let Some(l) = ui.line.as_mut() {
        l.scroll = scroll;
        l.view = view;
    }
}

// ---------------------------------------------------------------------------------------------
// The questions over the screen.
// ---------------------------------------------------------------------------------------------

fn modal_rect(area: Rect, lines: usize) -> Rect {
    let w = area.width.saturating_sub(4).clamp(1, 78);
    let h = u16::try_from(lines + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2).max(1));
    Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

/// A line of keys the modal offers, each a click.
struct Button {
    code: KeyCode,
    label: &'static str,
    word: &'static str,
}

fn pending_lines(p: &Pending) -> Vec<Line> {
    p.lines
        .iter()
        .flat_map(|l| text::lines(l, Tone::Normal, true))
        .collect()
}

#[allow(clippy::too_many_lines)] // one match: each question the screen can ask
fn draw_modal(buf: &mut Buffer, ui: &mut Ui, area: Rect) {
    // Borrowed, never copied: what is typed stays where it is.
    let Some(modal) = ui.modal.as_ref() else {
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut buttons: Vec<Button> = Vec::new();
    let mut request_rows: Vec<(usize, usize)> = Vec::new();
    let title = match modal {
        Modal::Confirm(p) => {
            lines.extend(pending_lines(p));
            lines.push(Vec::new());
            lines.push(vec![
                (Tone::Normal, "Do it? ".to_owned()),
                (Tone::Key, "y".to_owned()),
                (Tone::Normal, "/".to_owned()),
                (Tone::Key, "N".to_owned()),
            ]);
            buttons.extend([
                Button {
                    code: KeyCode::Char('y'),
                    label: "y",
                    word: "yes",
                },
                Button {
                    code: KeyCode::Char('n'),
                    label: "N",
                    word: "no",
                },
            ]);
            p.action.title()
        }
        Modal::Passphrase {
            pending,
            typed,
            wrong,
        } => {
            lines.extend(pending_lines(pending));
            lines.push(Vec::new());
            lines.push(vec![
                (Tone::Normal, "Passphrase: ".to_owned()),
                (Tone::Key, format!("{}▌", "•".repeat(typed.count().min(60)))),
            ]);
            if *wrong > 0 {
                lines.push(vec![(
                    Tone::Red,
                    format!(
                        "The passphrase was wrong. {} tries left.",
                        TRIES.saturating_sub(*wrong)
                    ),
                )]);
            }
            lines.push(Vec::new());
            // retired-words:begin (§17 item 3: the screen that hides after 15 minutes, not a lock of a session)
            lines.push(vec![(Tone::Dim, format!("{TRIES} tries, then locked"))]);
            // retired-words:end
            buttons.extend([
                Button {
                    code: KeyCode::Enter,
                    label: "Enter",
                    word: "confirm",
                },
                Button {
                    code: KeyCode::Esc,
                    label: "Esc",
                    word: "cancel",
                },
            ]);
            pending.action.title()
        }
        Modal::Mode => {
            lines.push(vec![(Tone::Normal, "Choose the hold mode.".to_owned())]);
            lines.push(vec![(
                Tone::Dim,
                "strict asks y/N; balanced and relaxed loosen, so they ask the passphrase."
                    .to_owned(),
            )]);
            buttons.extend([
                Button {
                    code: KeyCode::Char('s'),
                    label: "s",
                    word: "strict",
                },
                Button {
                    code: KeyCode::Char('b'),
                    label: "b",
                    word: "balanced",
                },
                Button {
                    code: KeyCode::Char('r'),
                    label: "r",
                    word: "relaxed",
                },
                Button {
                    code: KeyCode::Esc,
                    label: "Esc",
                    word: "cancel",
                },
            ]);
            "HOLD MODE".to_owned()
        }
        Modal::Yolo {
            names,
            till,
            second,
            error,
        } => {
            let cursor = |on: bool| if on { "▌" } else { "" };
            lines.push(vec![
                (Tone::Label, "agents (comma separated)  ".to_owned()),
                (Tone::Key, format!("{}{}", visible(names), cursor(!second))),
            ]);
            lines.push(vec![
                (Tone::Label, "until, HH:MM in UTC       ".to_owned()),
                (Tone::Key, format!("{}{}", visible(till), cursor(*second))),
            ]);
            if !error.is_empty() {
                lines.push(vec![(Tone::Red, error.clone())]);
            }
            lines.push(vec![(
                Tone::Dim,
                "yolo loosens Lunchroom, so it asks the passphrase.".to_owned(),
            )]);
            buttons.extend([
                Button {
                    code: KeyCode::Tab,
                    label: "Tab",
                    word: "next field",
                },
                Button {
                    code: KeyCode::Enter,
                    label: "Enter",
                    word: "go",
                },
                Button {
                    code: KeyCode::Esc,
                    label: "Esc",
                    word: "cancel",
                },
            ]);
            "YOLO".to_owned()
        }
        Modal::Controller(screen) => {
            if let Some(req) = &screen.confirming {
                lines.extend(kv("run", &req.run_id));
                lines.extend(kv("phase", &req.phase));
                lines.extend(kv("plan", &req.manifest_hash));
                lines.push(Vec::new());
                lines.push(vec![
                    (Tone::Normal, "Approve this phase? ".to_owned()),
                    (Tone::Key, "y".to_owned()),
                    (Tone::Normal, "/".to_owned()),
                    (Tone::Key, "N".to_owned()),
                    (
                        Tone::Normal,
                        "   then the passphrase, for every phase".to_owned(),
                    ),
                ]);
                lines.push(vec![(Tone::Dim, "Compare the plan with `bin/controller status <run-id>` in a terminal of your own.".to_owned())]);
                buttons.extend([
                    Button {
                        code: KeyCode::Char('y'),
                        label: "y",
                        word: "yes",
                    },
                    Button {
                        code: KeyCode::Char('n'),
                        label: "N",
                        word: "no",
                    },
                ]);
            } else {
                if let Some(e) = &screen.listing.error {
                    lines.push(vec![(Tone::Red, one_line(e, 200))]);
                } else if screen.listing.requests.is_empty() {
                    lines.push(vec![(
                        Tone::Dim,
                        "No approval request is waiting in requests/.".to_owned(),
                    )]);
                }
                for (i, r) in screen.listing.requests.iter().enumerate() {
                    let first = lines.len();
                    let mark = if i == screen.sel { "▶ " } else { "  " };
                    lines.push(vec![(
                        Tone::Normal,
                        format!("{mark}{}  {}", r.run_id, r.phase),
                    )]);
                    lines.push(vec![(Tone::Dim, format!("    plan {}", r.manifest_hash))]);
                    request_rows.push((first, i));
                }
                if screen.listing.ignored > 0 {
                    lines.push(vec![(
                        Tone::Dim,
                        format!(
                            "{} file(s) in requests/ are not valid requests and are not listed.",
                            screen.listing.ignored
                        ),
                    )]);
                }
                lines.push(Vec::new());
                lines.push(vec![
                    (Tone::Label, "a-controller <run-id> <phase>: ".to_owned()),
                    (Tone::Key, format!("{}▌", visible(&screen.input))),
                ]);
                if !screen.error.is_empty() {
                    lines.push(vec![(Tone::Red, one_line(&screen.error, 300))]);
                }
                buttons.extend([
                    Button {
                        code: KeyCode::Enter,
                        label: "Enter",
                        word: "choose",
                    },
                    Button {
                        code: KeyCode::Esc,
                        label: "Esc",
                        word: "cancel",
                    },
                ]);
            }
            "CONTROLLER APPROVAL".to_owned()
        }
    };
    // The title can carry an agent's name: it is drawn with its marks like any agent text.
    let title = visible(&title);
    let rect = modal_rect(area, 0);
    let inner_w = usize::from(rect.width.saturating_sub(4)).max(1);
    // Wrap, remember where each request starts, then size the box to the text.
    let mut wrapped: Vec<Line> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for l in &lines {
        starts.push(wrapped.len());
        wrapped.extend(wrap(std::slice::from_ref(l), inner_w));
    }
    starts.push(wrapped.len());
    let rect = modal_rect(area, wrapped.len() + usize::from(!buttons.is_empty()) + 1);
    ratatui::widgets::Widget::render(Clear, rect, buf);
    let b = block(&title).border_style(Style::default().fg(CYAN));
    let inner = b.inner(rect);
    ratatui::widgets::Widget::render(b, rect, buf);
    ui.hits.push((Layer::Modal, rect, Hit::Inside));
    let content = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let rows = usize::from(content.height).saturating_sub(usize::from(!buttons.is_empty()) + 1);
    // Keep the last text rows (the input) in view when the box is short.
    let skip = wrapped.len().saturating_sub(rows);
    for (row, line) in wrapped.iter().skip(skip).take(rows.max(1)).enumerate() {
        put_line(buf, content, u16::try_from(row).unwrap_or(0), line, None);
    }
    for (first, i) in request_rows {
        let (a, z) = (starts[first], starts[first + 2]);
        for r in a..z {
            if r >= skip && r - skip < rows {
                let y = content.y + u16::try_from(r - skip).unwrap_or(0);
                ui.hits.push((
                    Layer::Modal,
                    Rect::new(content.x, y, content.width, 1),
                    Hit::Request(i),
                ));
            }
        }
    }
    if !buttons.is_empty() && inner.height >= 1 {
        let y = inner.y + inner.height - 1;
        let end = inner.x + inner.width;
        let mut x = inner.x + 1;
        for bt in &buttons {
            let start = x;
            x = put(buf, x, y, end, bt.label, style(Tone::Key));
            x = put(
                buf,
                x,
                y,
                end,
                &format!(" {}   ", bt.word),
                style(Tone::Normal),
            );
            ui.hits.push((
                Layer::Modal,
                Rect::new(
                    start,
                    y,
                    x.saturating_sub(start).min(end.saturating_sub(start)),
                    1,
                ),
                Hit::Press(bt.code),
            ));
        }
    }
}
