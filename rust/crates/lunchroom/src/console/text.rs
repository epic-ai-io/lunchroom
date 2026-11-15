//! Text an agent chose, as the screen shows it (§17 C2 item 10): every control character and
//! direction mark is replaced by a visible mark, its `\u{...}` code, and never reaches the
//! terminal. Wrapping measures the width each character takes on the screen.

use ratatui::text::Span;

/// How a piece of a line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Dim,
    Label,
    Key,
    Amber,
    Red,
    Green,
    /// The part of a call that loosens or destroys (A1.2).
    Warn,
    /// A visible mark standing for a character the terminal must not act on.
    Mark,
}

/// A piece of one line.
pub type Piece = (Tone, String);

/// One line of pieces.
pub type Line = Vec<Piece>;

/// Whether `c` is shown as its code (§17.10): see [`lunchroom_core::shown::is_mark`].
#[must_use]
pub fn is_mark(c: char) -> bool {
    lunchroom_core::shown::is_mark(c)
}

/// The visible mark for `c`: `\u{1b}` for an escape, `\u{5c}` for a backslash.
#[must_use]
pub fn mark(c: char) -> String {
    lunchroom_core::shown::mark(c)
}

/// `text` split into lines at each `\n` when `breaks`, each line as pieces: ordinary text in
/// `base`, each mark in [`Tone::Mark`]. With `breaks` false a line feed is a mark like any other
/// control character. A backslash is shown as `\u{5c}` too (§17 item 10), so that text an agent
/// typed cannot look like a mark.
#[must_use]
pub fn lines(text: &str, base: Tone, breaks: bool) -> Vec<Line> {
    let mut out: Vec<Line> = vec![Vec::new()];
    let mut run = String::new();
    let flush = |run: &mut String, line: &mut Line| {
        if !run.is_empty() {
            line.push((base, std::mem::take(run)));
        }
    };
    for c in text.chars() {
        if breaks && c == '\n' {
            if let Some(line) = out.last_mut() {
                flush(&mut run, line);
            }
            out.push(Vec::new());
        } else if is_mark(c) || c == '\\' {
            if let Some(line) = out.last_mut() {
                flush(&mut run, line);
                line.push((Tone::Mark, mark(c)));
            }
        } else {
            run.push(c);
        }
    }
    if let Some(line) = out.last_mut() {
        flush(&mut run, line);
    }
    out
}

/// A call as the guard's records keep it (§17 item 4): already safe, with each character that is
/// not shown as itself written as its mark. It is drawn as it is, not marked again (its backslashes
/// begin its marks), each mark in [`Tone::Mark`].
#[must_use]
pub fn kept(text: &str, base: Tone) -> Line {
    let mut out: Line = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\\u{") {
        let tail = &rest[at..];
        let end = tail
            .find('}')
            .filter(|e| *e > 3 && *e <= 11 && tail[3..*e].chars().all(|c| c.is_ascii_hexdigit()));
        let Some(end) = end else {
            // Not a mark: its first character is text.
            let cut = at + 1;
            out.push((base, rest[..cut].to_owned()));
            rest = &rest[cut..];
            continue;
        };
        if at > 0 {
            out.push((base, rest[..at].to_owned()));
        }
        out.push((Tone::Mark, tail[..=end].to_owned()));
        rest = &tail[end + 1..];
    }
    if !rest.is_empty() {
        out.push((base, rest.to_owned()));
    }
    out
}

/// `text` as one line of text with every control character and direction mark replaced by its
/// code (a line feed too).
#[must_use]
pub fn visible(text: &str) -> String {
    lines(text, Tone::Normal, false)
        .into_iter()
        .flatten()
        .map(|(_, s)| s)
        .collect()
}

/// The columns `c` takes on the screen.
#[must_use]
pub fn char_width(c: char) -> usize {
    if c.is_ascii() {
        1
    } else {
        Span::raw(c.to_string()).width()
    }
}

/// The columns `text` takes on the screen.
#[must_use]
pub fn width(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        Span::raw(text).width()
    }
}

/// `text` cut to at most `max` columns, ending in `…` where it was cut.
#[must_use]
pub fn fit(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// `text` as one visible line fitted to `max` columns (for a list, which may cut: the panel never
/// does).
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let shown = visible(text);
    if width(&shown) <= max {
        return shown;
    }
    if max == 0 {
        return String::new();
    }
    // Cut between pieces: a mark (`\u{1b}`) is whole or left out, never cut in the middle.
    let mut out = String::new();
    let mut used = 0;
    for (tone, piece) in lines(text, Tone::Normal, false).into_iter().flatten() {
        if tone == Tone::Mark {
            if used + width(&piece) + 1 > max {
                break;
            }
            out.push_str(&piece);
            used += width(&piece);
        } else {
            for c in piece.chars() {
                let w = char_width(c);
                if used + w + 1 > max {
                    out.push('…');
                    return out;
                }
                out.push(c);
                used += w;
            }
        }
    }
    out.push('…');
    out
}

/// `lines` wrapped to `max` columns, never dropping a character: a line breaks where the next
/// character would pass the edge, and a character wider than the screen takes a line of its own.
#[must_use]
pub fn wrap(lines: &[Line], max: usize) -> Vec<Line> {
    let max = max.max(1);
    let mut out: Vec<Line> = Vec::new();
    for line in lines {
        let mut row: Line = Vec::new();
        let mut used = 0usize;
        if line.iter().all(|(_, s)| s.is_empty()) {
            out.push(Vec::new());
            continue;
        }
        for (tone, text) in line {
            if *tone == Tone::Mark && text.len() <= max {
                // A mark is one unit: it moves to the next row whole.
                let w = text.len();
                if used > 0 && used + w > max {
                    out.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push((*tone, text.clone()));
                used += w;
                continue;
            }
            let mut piece = String::new();
            for c in text.chars() {
                let w = char_width(c);
                if used > 0 && used + w > max {
                    if !piece.is_empty() {
                        row.push((*tone, std::mem::take(&mut piece)));
                    }
                    out.push(std::mem::take(&mut row));
                    used = 0;
                }
                piece.push(c);
                used += w;
            }
            if !piece.is_empty() {
                row.push((*tone, piece));
            }
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // §17::183: control characters and direction marks are drawn as their codes.
    #[test]
    fn marks_are_drawn_as_codes() {
        assert_eq!(visible("a\u{1b}[2Jb"), "a\\u{1b}[2Jb");
        assert_eq!(visible("x\u{202e}y\u{200f}z"), "x\\u{202e}y\\u{200f}z");
        assert_eq!(visible("n\0l\r\n\t"), "n\\u{0}l\\u{d}\\u{a}\\u{9}");
        assert_eq!(visible("\u{9b}31m \u{7f}"), "\\u{9b}31m \\u{7f}");
        assert_eq!(
            visible("ZW\u{200b}J\u{2066}i\u{feff}"),
            "ZW\\u{200b}J\\u{2066}i\\u{feff}"
        );
        assert_eq!(visible("plain text, ü and 日本"), "plain text, ü and 日本");
    }

    // §17 item 10: "with each backslash shown as `\u{5c}`", and each space other than the ordinary
    // space as its code, so that typed text cannot look like a mark or like an ordinary gap.
    #[test]
    fn a_backslash_and_an_odd_space_are_marks() {
        assert_eq!(visible("a\\b"), "a\\u{5c}b");
        // Typed text that spells a mark is not one: its backslash is shown.
        assert_eq!(visible("\\u{1b}"), "\\u{5c}u{1b}");
        assert_eq!(visible("C:\\dir\\n"), "C:\\u{5c}dir\\u{5c}n");
        for (odd, shown) in [
            ('\u{a0}', "\\u{a0}"),
            ('\u{2003}', "\\u{2003}"),
            ('\u{202f}', "\\u{202f}"),
            ('\u{3000}', "\\u{3000}"),
            ('\u{1680}', "\\u{1680}"),
            ('\u{205f}', "\\u{205f}"),
        ] {
            assert!(is_mark(odd));
            assert_eq!(visible(&format!("a{odd}b")), format!("a{shown}b"));
        }
        // The ordinary space is a space.
        assert!(!is_mark(' '));
        assert_eq!(visible("a b"), "a b");
        // In the panel a backslash is a mark piece, not part of the text's run.
        let got = lines("x\\y", Tone::Normal, true);
        assert_eq!(
            got[0],
            vec![
                (Tone::Normal, "x".to_owned()),
                (Tone::Mark, "\\u{5c}".to_owned()),
                (Tone::Normal, "y".to_owned())
            ]
        );
    }

    // The panel breaks a line at a line feed and marks every other control character.
    #[test]
    fn the_panel_breaks_lines_and_marks_the_rest() {
        let got = lines("one\ntwo\u{1b}x", Tone::Normal, true);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], vec![(Tone::Normal, "one".to_owned())]);
        assert_eq!(
            got[1],
            vec![
                (Tone::Normal, "two".to_owned()),
                (Tone::Mark, "\\u{1b}".to_owned()),
                (Tone::Normal, "x".to_owned())
            ]
        );
    }

    // Wrapping keeps every character, in order, for ASCII, wide characters and marks.
    #[test]
    fn wrapping_drops_nothing() {
        let text = "ab\u{1b}cdef日本語ghij\u{202e}k";
        let wrapped = wrap(&lines(text, Tone::Normal, true), 5);
        let joined: String = wrapped
            .iter()
            .flat_map(|l| l.iter().map(|(_, s)| s.as_str()))
            .collect();
        assert_eq!(joined, visible(text));
        for row in &wrapped {
            let w: usize = row.iter().map(|(_, s)| width(s)).sum();
            assert!(w <= 6, "{row:?} is {w} wide");
        }
    }

    #[test]
    fn fit_cuts_with_an_ellipsis_and_never_passes_the_width() {
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本語日本語", 5), "日本…");
        assert_eq!(fit("abc", 0), "");
    }
}
