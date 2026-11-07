//! A DENIED call explained in full (spec §1 V1.4).
//!
//! V1.4 (docs/spec/lunchroom-spec.md §1 item 4): "A DENIED call is explained in full, never by its
//! reason label alone. Its message says, in plain words: what was DENIED (the path, program,
//! command or text at fault, as the guard read it); why (the rule, and for a session that is HELD,
//! the agent and message id that hold it); and every way forward that exists for that call (what
//! the agent can do instead, and what the user can do: run it in a terminal of their own, type in
//! the session to clear the hold, approve the exact call with `lunchroom approve`, or change the
//! setting at fault). The same applies to a message `lunchroom send` DENIES."
//!
//! The first line carries the short answer on its own, for a screen that shows one line of a
//! message: `DENIED · <part at fault>: <what to do next>`. For a path it names the path as the
//! call wrote it; for every other reason it names the reason's label (V1.2: `DENIED · <reason>:
//! <next step>`). The lines after it carry the specifics, which the caller passes from where the
//! denial is made.
//!
//! The message is read by the agent whose call it is DENIED, which may be HELD by another
//! agent's text. It therefore never says how to loosen the guard in exact commands (`lunchroom
//! mode`, `lunchroom yolo`): the agent's ways forward say that the user can change how strict
//! Lunchroom is ([`strictness_way`]) without the commands. A hook has no channel to the user that
//! the agent's session does not also show, so no message carries them; they are in `lunchroom help`
//! and the README.

use std::fmt::Write as _;

use crate::vocab::{AgentState, Reason, Verdict};

/// The longest command or text shown whole; a longer one is shown around the part at fault.
pub const SHOWN_CHARS: usize = 200;

/// One DENIED call or message, with everything V1.4 requires its message to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// The reason from the vocabulary table.
    pub reason: Reason,
    /// What was DENIED, as the guard read it: "the command `…`", "the Read of /x", "the message
    /// to Kimi".
    pub what: String,
    /// The path, program, word or text at fault inside it, when the guard found one. A path is
    /// always one the call itself named (never one the guard found by looking), so the message
    /// tells a HELD agent nothing about where other sessions' files are.
    pub fault: Option<String>,
    /// The rule, in plain words (by default [`Reason::rule`]).
    pub rule: String,
    /// For a HELD session: what holds it, each as `<agent> (<message id>)`.
    pub held_by: Vec<String>,
    /// Every way forward that exists for this call, each one sentence, safe for the agent to
    /// read: none holds an exact command that loosens the guard ([`strictness_way`]).
    pub ways: Vec<String>,
}

/// The way forward that tells the agent the user can change how strict Lunchroom is, without
/// naming the commands: an agent HELD by another agent's text must not be handed them (the
/// commands are in `lunchroom help` and the README).
#[must_use]
pub fn strictness_way() -> String {
    "The user: change how strict Lunchroom is for this session, in a terminal of their own."
        .to_owned()
}

impl Denial {
    /// A denial for `reason` of `what`, with the table's rule and no way forward yet.
    #[must_use]
    pub fn new(reason: Reason, what: impl Into<String>) -> Self {
        Self {
            reason,
            what: what.into(),
            fault: None,
            rule: reason.rule(),
            held_by: Vec::new(),
            ways: Vec::new(),
        }
    }

    /// The message for the agent: one line for the label or the path at fault with the next step,
    /// what, why, and one line per way forward.
    #[must_use]
    pub fn render(&self) -> String {
        let fault = self.fault.as_deref().filter(|f| !f.is_empty());
        let head = match (self.reason, fault) {
            (Reason::DeniedPath, Some(f)) => shown(f, None),
            _ => self.reason.word().to_owned(),
        };
        let mut out = format!(
            "{} · {head}: {}",
            Verdict::Denied.word(),
            self.reason.next_step()
        );
        let _ = write!(out, "\nWhat was {}: {}", Verdict::Denied.word(), self.what);
        if let Some(fault) = fault {
            let _ = write!(out, " (the part at fault: {})", shown(fault, None));
        }
        out.push('.');
        let _ = write!(out, "\nWhy: {}.", self.rule);
        if !self.held_by.is_empty() {
            let held_word = AgentState::Held.word();
            let _ = write!(
                out,
                " This session is {held_word} by {}",
                self.held_by.join(", ")
            );
            // The word is defined the first time a message uses it: the rule may have done it.
            if self.rule.contains(held_word) {
                out.push('.');
            } else {
                let _ = write!(out, ": {}.", crate::vocab::HELD_MEANING);
            }
        }
        if !self.ways.is_empty() {
            out.push_str("\nWays forward:");
            for way in &self.ways {
                let _ = write!(out, "\n- {way}");
            }
        }
        out
    }
}

/// `text` as shown in a message: whole when it has at most [`SHOWN_CHARS`] characters, else a
/// window of that many around `fault` where `fault` is in it (else its start), with `…` where text
/// was left out. Line breaks are shown as `⏎` so the message stays readable on one line.
#[must_use]
pub fn shown(text: &str, fault: Option<&str>) -> String {
    let flat = text.replace('\n', " ⏎ ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= SHOWN_CHARS {
        return flat;
    }
    let at = fault.map(|f| f.replace('\n', " ⏎ ")).and_then(|f| {
        flat.find(&f)
            .map(|byte| (flat[..byte].chars().count(), f.chars().count()))
    });
    let start = match at {
        // The part at fault in the window, with some text before it.
        Some((at, len)) => {
            let before = SHOWN_CHARS.saturating_sub(len.min(SHOWN_CHARS)) / 3;
            at.saturating_sub(before).min(chars.len() - SHOWN_CHARS)
        }
        None => 0,
    };
    let end = start + SHOWN_CHARS;
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// What holds a session, as a denial names it: a message's sender and id (`alice
/// (lr-00000045)`), or a hold no message caused by its source (`tool output (toolu_1)`).
#[must_use]
pub fn holder(from_name: &str, message: &str, from_a_message: bool) -> String {
    if from_a_message {
        format!("{from_name} ({message})")
    } else {
        format!("{} ({message})", from_name.replace('-', " "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELD_RULE: &str = "a HELD session may not read Lunchroom's records or another \
                             session's transcript, nor search a folder that holds one; HELD \
                             means a message from another agent is in this session, so \
                             Lunchroom restricts what it may do";

    // §1 V1.4: the whole message of a denied path, word for word: the first line names
    // the path and the next step, then what was DENIED and the part at fault, why with the holder
    // and message id (HELD defined once, by the rule), and each way forward.
    #[test]
    fn a_denied_path_message_in_full() {
        let mut d = Denial::new(
            Reason::DeniedPath,
            "the command `cat ~/.claude/projects/x.jsonl`",
        );
        d.fault = Some("/home/u/.claude/projects/x.jsonl".to_owned());
        d.held_by = vec![holder("alice", "lr-00000045", true)];
        d.ways = vec!["The agent: leave that path out.".to_owned()];
        let why = format!("Why: {HELD_RULE}. This session is HELD by alice (lr-00000045).");
        let expected = [
            "DENIED · /home/u/.claude/projects/x.jsonl: this path is, or leads to, Lunchroom's \
             records or another session's transcripts; leave it out of the call, and the user \
             can read it in a terminal of their own.",
            "What was DENIED: the command `cat ~/.claude/projects/x.jsonl` (the part at fault: \
             /home/u/.claude/projects/x.jsonl).",
            why.as_str(),
            "Ways forward:",
            "- The agent: leave that path out.",
        ]
        .join("\n");
        assert_eq!(d.render(), expected);
        assert_eq!(
            holder("tool-output", "toolu_1", false),
            "tool output (toolu_1)"
        );
    }

    // G1-8: a path denial with no path found keeps the label; every other reason keeps its label
    // too, with HELD defined at its first use where the rule does not use the word.
    #[test]
    fn other_denials_keep_the_label_and_define_held_once() {
        let mut d = Denial::new(Reason::Fence, "the command `codex queue bob hi`");
        d.held_by = vec![holder("alice", "lr-00000046", true)];
        let expected = [
            "DENIED · fence: this would start work in another agent's session; to message an \
             agent, use `lunchroom send <name> <text>`.",
            "What was DENIED: the command `codex queue bob hi`.",
            "Why: a call that starts work in another agent's session by any way but `lunchroom \
             send`, calls a local session chat API, or changes the variables Lunchroom reads an \
             agent's identity from is DENIED in every session. This session is HELD by alice \
             (lr-00000046): a message from another agent is in this session, so Lunchroom \
             restricts what it may do.",
        ]
        .join("\n");
        assert_eq!(d.render(), expected);
        let mut path = Denial::new(Reason::DeniedPath, "the Read of x");
        path.fault = Some(String::new());
        assert!(
            path.render()
                .starts_with("DENIED · denied path: this path is, or leads to,"),
            "{}",
            path.render()
        );
    }

    // G1-7, DV-11: the agent's ways forward say the user can change how strict Lunchroom is, and
    // name no command: a hook has no channel to the user that the agent's session does not also
    // show, so the message the agent reads is the only one there is.
    #[test]
    fn the_agent_is_not_given_the_commands_that_loosen_the_guard() {
        let mut d = Denial::new(Reason::NoApprovalChannel, "the command `git push`");
        d.ways = vec![strictness_way()];
        let agent = d.render();
        assert!(
            !agent.contains("lunchroom mode") && !agent.contains("lunchroom yolo"),
            "{agent}"
        );
        assert!(
            agent.ends_with(
                "Ways forward:\n- The user: change how strict Lunchroom is for this session, in \
                 a terminal of their own."
            ),
            "{agent}"
        );
    }

    // V1.4: a long command is cut to a window that keeps the part at fault; a short one is whole;
    // line breaks show as one line.
    #[test]
    fn a_long_command_is_cut_around_the_part_at_fault() {
        assert_eq!(shown("ls\npwd", None), "ls ⏎ pwd");
        let long = format!(
            "{} cat /home/u/.lunchroom/store/state.json {}",
            "a".repeat(300),
            "b".repeat(300)
        );
        let cut = shown(&long, Some("/home/u/.lunchroom/store/state.json"));
        assert!(cut.contains("/home/u/.lunchroom/store/state.json"), "{cut}");
        assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), SHOWN_CHARS + 2);
        let head = shown(&long, Some("not in it"));
        assert!(head.starts_with("aaa") && head.ends_with('…'), "{head}");
    }

    // G1-10: the part at fault at the very end, longer than the window, or made of characters
    // wider than one byte. The window is always 200 characters (plus the marks for what was left
    // out), never cuts inside a character, and never panics.
    #[test]
    fn shown_handles_the_fault_at_the_end_a_long_fault_and_wide_characters() {
        // At the end: the window ends with the fault and has a mark only at its start.
        let tail = "/home/u/.claude/projects/p/t.jsonl";
        let at_end = format!("{} {tail}", "a".repeat(400));
        let cut = shown(&at_end, Some(tail));
        assert!(cut.starts_with('…') && !cut.ends_with('…'), "{cut}");
        assert!(cut.ends_with(tail), "{cut}");
        assert_eq!(cut.chars().count(), SHOWN_CHARS + 1);
        // Longer than the window: the window starts at the fault, and ends with a mark.
        let fault = "f".repeat(300);
        let text = format!("{} {fault} {}", "a".repeat(50), "b".repeat(50));
        let cut = shown(&text, Some(&fault));
        assert!(cut.starts_with('…') || cut.starts_with("aaa"), "{cut}");
        assert_eq!(
            cut.chars().filter(|c| *c == 'f').count(),
            SHOWN_CHARS - cut.chars().filter(|c| *c == 'a').count()
        );
        assert!(cut.ends_with('…'), "{cut}");
        // Wide characters: positions are in characters, not bytes.
        let wide = format!("{}é/чей/路径{}", "é".repeat(150), "é".repeat(150));
        let cut = shown(&wide, Some("é/чей/路径"));
        assert!(cut.contains("é/чей/路径"), "{cut}");
        assert_eq!(cut.chars().count(), SHOWN_CHARS + 2);
        let end_wide = format!("{}路径", "é".repeat(300));
        let cut = shown(&end_wide, Some("路径"));
        assert!(cut.ends_with("路径") && cut.starts_with('…'), "{cut}");
        // The first 200 characters whole when the text is exactly the limit; one more is cut.
        assert_eq!(shown(&"é".repeat(200), None), "é".repeat(200));
        assert_eq!(
            shown(&"é".repeat(201), None),
            format!("{}…", "é".repeat(200))
        );
        // Nothing to show.
        assert_eq!(shown("", None), "");
        assert_eq!(
            shown(&"x".repeat(300), Some("")).chars().count(),
            SHOWN_CHARS + 1
        );
    }
}
