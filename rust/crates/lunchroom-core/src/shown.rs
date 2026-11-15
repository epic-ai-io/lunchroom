//! Text an agent chose, as the console and the guard's records show it (spec §17 item 10).
//!
//! §17.10: "Text an agent chose is shown with each control character, direction mark, zero-width or
//! other format character, line or paragraph separator, and each space other than the ordinary
//! space, replaced by a visible mark (its `\u{...}` code), and with each backslash shown as
//! `\u{5c}`, so that text an agent typed cannot look like a mark; it is never acted on by the
//! terminal."
//!
//! §17 item 4: a DENIED decision's call is kept "with each control character and direction mark
//! replaced by its visible mark (§17.10) and its secrets replaced, as H1.8 does for a record, cut
//! at 64 KiB with the mark `cut`".

use crate::secrets;

/// The most bytes of a DENIED decision's call that are kept (§17 item 4).
pub const CALL_KEPT_BYTES: usize = 64 * 1_024;

/// The mark that stands where a call was cut at [`CALL_KEPT_BYTES`] (§17 item 4).
pub const CUT_MARK: &str = "cut";

/// Whether `c` is shown as its code: a control character, a direction mark, a zero-width or other
/// format character, a line or paragraph separator, a space other than the ordinary space, or a
/// backslash.
#[must_use]
pub fn is_mark(c: char) -> bool {
    c == '\\'
        || c.is_control()
        || matches!(c as u32,
            0x00A0
            | 0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x1680
            | 0x180E
            | 0x2000..=0x200F
            | 0x2028..=0x202F
            | 0x205F..=0x206F
            | 0x3000
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F)
}

/// The visible mark for `c`: `\u{1b}` for an escape, `\u{5c}` for a backslash.
#[must_use]
pub fn mark(c: char) -> String {
    format!("\\u{{{:x}}}", c as u32)
}

/// `text` with each character [`is_mark`] names replaced by its mark.
#[must_use]
pub fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_mark(c) {
            out.push_str(&mark(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// A DENIED decision's call as it is kept: its secrets replaced first (so a secret that crosses the
/// cut is not left half in), each marked character shown as its mark, and the whole cut at
/// [`CALL_KEPT_BYTES`] bytes, never inside a mark or a character, with [`CUT_MARK`] where it was
/// cut. A call of exactly [`CALL_KEPT_BYTES`] bytes is not cut.
#[must_use]
pub fn kept_call(text: &str) -> String {
    let redacted = secrets::redact(text);
    let mut out = String::with_capacity(redacted.len().min(CALL_KEPT_BYTES + 8));
    for c in redacted.chars() {
        let piece = if is_mark(c) { mark(c) } else { c.to_string() };
        if out.len() + piece.len() > CALL_KEPT_BYTES {
            out.push_str(CUT_MARK);
            return out;
        }
        out.push_str(&piece);
    }
    out
}

/// A kept call read back from a file (§17 item 4, H1.8 "again when it is read"): its secrets
/// replaced again and each marked character that is not a backslash shown as its mark. A backslash
/// is left, since a kept call has none of an agent's: each was written as `\u{5c}`, and every other
/// one begins a mark. A text past what a kept call can be (its limit and the cut mark) is cut again.
#[must_use]
pub fn reread_call(text: &str) -> String {
    let redacted = secrets::redact(text);
    let mut out = String::with_capacity(redacted.len());
    for c in redacted.chars() {
        let piece = if c != '\\' && is_mark(c) {
            mark(c)
        } else {
            c.to_string()
        };
        if out.len() + piece.len() > CALL_KEPT_BYTES + CUT_MARK.len() {
            out.push_str(CUT_MARK);
            return out;
        }
        out.push_str(&piece);
    }
    out
}

/// The text of a call as the console shows it (§17.10 "Marked parts"): for a shell command its
/// command line, and for any other tool call its input as the text A2.1 hashes (its canonical
/// JSON). A shell call is the `Bash` tool the harness adapters make of it.
#[must_use]
pub fn call_text(tool: &str, input: &serde_json::Value) -> String {
    if tool == "Bash"
        && let Some(command) = input.get("command").and_then(serde_json::Value::as_str)
    {
        return command.to_owned();
    }
    crate::jcs::canonical(input)
}

#[cfg(test)]
mod tests;
