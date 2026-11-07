//! The one table of words Lunchroom prints (spec §1 V1; `docs/spec/standard-vocabulary.md`).
//!
//! [`TABLE`] is the only place a state, verdict, party or reason word is written. Every type below
//! takes its word from it; messages, log events and audit fields take words from these types and
//! never type one out themselves (V1.2).

use std::fmt;

/// Every word in the table, in table order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Word {
    Available,
    Held,
    Yolo,
    Unguarded,
    Stopped,
    Halted,
    Queued,
    Delivered,
    Failed,
    Granted,
    Denied,
    Protected,
    TheUser,
    ReasonFence,
    ReasonUnparsed,
    ReasonDeniedPath,
    ReasonConfiguration,
    ReasonNoApprovalChannel,
    ReasonLeak,
    ReasonHalted,
    ReasonLoop,
    ReasonRateLimit,
}

/// The one table of states, verdicts, the party, and reason codes (V1.2). Row `i` holds the word
/// for the `Word` whose discriminant is `i`; a test checks the order.
pub const TABLE: [(Word, &str); 22] = [
    (Word::Available, "AVAILABLE"),
    (Word::Held, "HELD"),
    (Word::Yolo, "YOLO"),
    (Word::Unguarded, "UNGUARDED"),
    (Word::Stopped, "STOPPED"),
    (Word::Halted, "HALTED"),
    (Word::Queued, "QUEUED"),
    (Word::Delivered, "DELIVERED"),
    (Word::Failed, "FAILED"),
    (Word::Granted, "GRANTED"),
    (Word::Denied, "DENIED"),
    (Word::Protected, "PROTECTED"),
    (Word::TheUser, "the user"),
    (Word::ReasonFence, "fence"),
    (Word::ReasonUnparsed, "unparsed"),
    (Word::ReasonDeniedPath, "denied path"),
    (Word::ReasonConfiguration, "configuration"),
    (Word::ReasonNoApprovalChannel, "no approval channel"),
    (Word::ReasonLeak, "leak"),
    (Word::ReasonHalted, "halted"),
    (Word::ReasonLoop, "loop"),
    (Word::ReasonRateLimit, "rate limit"),
];

impl Word {
    /// The text of this word, read from [`TABLE`].
    #[must_use]
    pub const fn text(self) -> &'static str {
        TABLE[self as usize].1
    }
}

/// The state of one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Available,
    Held,
    Unguarded,
    Stopped,
}

/// The state of one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageState {
    Queued,
    Delivered,
    Failed,
}

/// The verdict on a call or a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    Granted,
    Denied,
}

/// The party at the terminal, from the table.
pub const THE_USER: &str = Word::TheUser.text();

/// What HELD means, for a message to say the first time it uses the word. It follows the word,
/// as in "HELD means a message from another agent is in this session, so Lunchroom restricts what
/// it may do."
pub const HELD_MEANING: &str =
    "a message from another agent is in this session, so Lunchroom restricts what it may do";

/// The reason printed beside a verdict or HALTED, as `DENIED · <reason>` (vocabulary file: "A reason
/// is a small label beside the word, never a different word").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// A fence: the call would put a turn into another agent's session (spec §12 F1.1).
    Fence,
    /// A command the guard could not parse (spec §12 F1.2).
    Unparsed,
    /// A read of a path L1.1 denies: Lunchroom's stores, transcripts, `/proc` (spec §11 L1.1).
    DeniedPath,
    /// An agent's change to Lunchroom's stores, a slip, a registration or the rate limit (§7 A1.3).
    Configuration,
    /// The call had to go to the user and this harness has no approval channel (spec §7 A1.2).
    NoApprovalChannel,
    /// A secret in a reply, or a message asking for one (spec §11 L1.2, L1.4).
    Leak,
    /// A message between a HALTED pair (spec §2 R1.4).
    Halted,
    /// The pair repeated itself (spec §2 R1.3).
    Loop,
    /// The pair passed the rate limit (spec §2 R1.6).
    RateLimit,
}

impl AgentState {
    /// The table word for this state.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Available => Word::Available,
            Self::Held => Word::Held,
            Self::Unguarded => Word::Unguarded,
            Self::Stopped => Word::Stopped,
        }
        .text()
    }
}

impl MessageState {
    /// The table word for this state.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Queued => Word::Queued,
            Self::Delivered => Word::Delivered,
            Self::Failed => Word::Failed,
        }
        .text()
    }
}

impl Verdict {
    /// The table word for this verdict.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Granted => Word::Granted,
            Self::Denied => Word::Denied,
        }
        .text()
    }
}

impl Reason {
    /// The table label for this reason.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fence => Word::ReasonFence,
            Self::Unparsed => Word::ReasonUnparsed,
            Self::DeniedPath => Word::ReasonDeniedPath,
            Self::Configuration => Word::ReasonConfiguration,
            Self::NoApprovalChannel => Word::ReasonNoApprovalChannel,
            Self::Leak => Word::ReasonLeak,
            Self::Halted => Word::ReasonHalted,
            Self::Loop => Word::ReasonLoop,
            Self::RateLimit => Word::ReasonRateLimit,
        }
        .text()
    }

    /// What a DENIED call means and what to do next, shown after the reason (the guard's DENIED
    /// line); its state and party words come from the table.
    #[must_use]
    pub fn next_step(self) -> String {
        let user = Word::TheUser.text();
        match self {
            // F1.1.
            Self::Fence => "this would start work in another agent's session; to message an \
                            agent, use `lunchroom send <name> <text>`."
                .into(),
            // F1.2: the agent splits the command; only the user runs it as it is.
            Self::Unparsed => format!(
                "Lunchroom could not read this command in full; split it into simple commands and \
                 run them one at a time. Only {user} can run it as it is, in a terminal of their \
                 own."
            ),
            // L1.1: names no path; the part at fault is the caller's (`Denial::fault`).
            Self::DeniedPath => format!(
                "this path is, or leads to, Lunchroom's records or another session's \
                 transcripts; leave it out of the call, and {user} can read it in a terminal of \
                 their own."
            ),
            Self::Configuration => format!(
                "changes to Lunchroom and its settings are for {user} alone, in a terminal of \
                 their own."
            ),
            Self::NoApprovalChannel => format!(
                "this session has no permission prompt to ask {user}; {user} can approve this \
                 exact call with `lunchroom approve`, and then it can run."
            ),
            Self::Leak => "the text carries or asks for a secret; remove it and send again.".into(),
            Self::Halted => format!(
                "this pair is {}: Lunchroom stopped their conversation, and {user} typing in \
                 either session clears it.",
                Word::Halted.text()
            ),
            Self::Loop => "this pair repeated itself; send something new.".into(),
            Self::RateLimit => "this pair passed the rate limit; wait, then send again.".into(),
        }
    }
}

impl Reason {
    /// The rule behind this reason, in plain words (V1.4: "why (the rule …)"); a DENIED call's
    /// message gives it after what was DENIED ([`crate::denial::Denial`]).
    #[must_use]
    pub fn rule(self) -> String {
        let user = Word::TheUser.text();
        let held = Word::Held.text();
        let halted = Word::Halted.text();
        // The clause behind each rule is in the comment above its text, never in the text a person
        // reads.
        match self {
            // F1.1, I1.2.
            Self::Fence => "a call that starts work in another agent's session by any way but \
                            `lunchroom send`, calls a local session chat API, or changes the \
                            variables Lunchroom reads an agent's identity from is DENIED in \
                            every session"
                .into(),
            // F1.2.
            Self::Unparsed => "Lunchroom could not read this call in full, and what it could read \
                               names a way into another session or an identity variable, or runs \
                               a program or script that Lunchroom cannot find or read in full; \
                               such a call is DENIED in every session"
                .into(),
            // L1.1. HELD is defined here, where a message first uses it (V1.4).
            Self::DeniedPath => format!(
                "a {held} session may not read Lunchroom's records or another session's \
                 transcript, nor search a folder that holds one; {held} means {HELD_MEANING}"
            ),
            // A1.3.
            Self::Configuration => format!(
                "Lunchroom's records, its settings and the commands only {user} may run are \
                 changed by {user} alone, in a terminal of their own"
            ),
            // A1.2.
            Self::NoApprovalChannel => format!(
                "this call needs {user}'s yes, and this session has no permission prompt to ask \
                 it"
            ),
            // L1.2, L1.4.
            Self::Leak => "a message that carries a secret, or asks for one, is DENIED".into(),
            // R1.4.
            Self::Halted => format!(
                "a conversation between these two agents is {halted}: no message passes between \
                 them until {user} types in either session"
            ),
            // R1.3.
            Self::Loop => {
                "these two agents repeated themselves; a repeat stops their conversation".into()
            }
            // R1.6.
            Self::RateLimit => format!(
                "these two agents passed the rate limit {user} set for messages between two \
                 agents"
            ),
        }
    }
}

macro_rules! display_word {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.word())
            }
        }
    )*};
}
display_word!(AgentState, MessageState, Verdict, Reason);

/// Words retired by the vocabulary file. A user-facing string must not contain any of them as a
/// whole word (spec §1 V1.3). Matching ignores case.
// retired-words:begin (the source scan in the tests skips this list)
pub const RETIRED_WORDS: &[&str] = &[
    "agentbus", "bus", "locked", "lock", "tainted", "refused", "refusing", "nope", "allowed",
    "allow", "pending", "human", "auto", "looping", "down", "off", "inop",
];
// retired-words:end

/// Returns the first retired word found in `text` as a whole word, ignoring case.
#[must_use]
pub fn find_retired_word(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .find_map(|token| RETIRED_WORDS.iter().copied().find(|w| *w == token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The string literals in one Rust source text, outside `#[cfg(test)]`, the retired list, and
    /// regions marked `protocol-words`, which hold only values a harness's own protocol requires
    /// (such as Claude Code's `permissionDecision` values) and never reach a person.
    fn string_literals(source: &str) -> Vec<String> {
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut text = String::new();
        let mut skipping = false;
        for line in code.lines() {
            if line.contains("retired-words:begin") || line.contains("protocol-words:begin") {
                skipping = true;
            } else if line.contains("retired-words:end") || line.contains("protocol-words:end") {
                skipping = false;
            } else if !skipping && !line.trim_start().starts_with("//") {
                text.push_str(line);
                text.push('\n');
            }
        }
        let c: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < c.len() {
            let ident_before = i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_');
            if c[i] == '\'' {
                // A char literal such as '"' or '\"' must not open a string.
                if c.get(i + 1) == Some(&'"') && c.get(i + 2) == Some(&'\'') {
                    i += 3;
                    continue;
                }
                if c.get(i + 1) == Some(&'\\') && c.get(i + 3) == Some(&'\'') {
                    i += 4;
                    continue;
                }
                i += 1;
            } else if c[i] == 'r' && !ident_before && matches!(c.get(i + 1), Some('#' | '"')) {
                // Raw string: r"...", r#"..."#, r##"..."##.
                let mut j = i + 1;
                let mut hashes = 0;
                while c.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                if c.get(j) != Some(&'"') {
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let mut k = start;
                let lit_end = loop {
                    if k >= c.len() {
                        break c.len();
                    }
                    if c[k] == '"' && (0..hashes).all(|h| c.get(k + 1 + h) == Some(&'#')) {
                        break k;
                    }
                    k += 1;
                };
                out.push(c[start..lit_end].iter().collect());
                i = lit_end + 1 + hashes;
            } else if c[i] == '"' {
                let mut lit = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    } else {
                        lit.push(c[i]);
                    }
                    i += 1;
                }
                out.push(lit);
                i += 1;
            } else {
                i += 1;
            }
        }
        out
    }

    fn workspace_sources() -> Vec<PathBuf> {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        let mut dirs = vec![crates];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // `target` holds build output; `tests` holds integration tests, which are test code
                // as `#[cfg(test)]` modules are.
                let skipped = ["target", "tests"].map(std::ffi::OsStr::new);
                if path.is_dir() && !path.file_name().is_some_and(|n| skipped.contains(&n)) {
                    dirs.push(path);
                } else if path.extension() == Some("rs".as_ref()) {
                    files.push(path);
                }
            }
        }
        files
    }

    // §1::1, §1::3 — the product's documentation (`rust/README.md`, published with the
    // program) holds no retired word, and names every state, verdict and party word it uses from
    // the table.
    #[test]
    fn v1_product_documentation_uses_only_vocabulary_words() {
        let readme = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
        let text = std::fs::read_to_string(&readme).unwrap_or_default();
        assert!(text.contains("# Lunchroom"), "{}", readme.display());
        for (n, line) in text.lines().enumerate() {
            assert_eq!(find_retired_word(line), None, "README.md:{} {line}", n + 1);
        }
        for (_, word) in TABLE.iter().take(13) {
            assert!(text.contains(word), "README.md does not use {word}");
        }
    }

    // §1::3 — the suite fails if a user-facing string in the code holds a retired word.
    #[test]
    fn v1_no_retired_word_in_any_source_string() {
        let files = workspace_sources();
        assert!(!files.is_empty(), "no Rust sources found");
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            for lit in string_literals(&source) {
                assert_eq!(find_retired_word(&lit), None, "{}: {lit:?}", file.display());
            }
        }
    }

    // Every reason a call is DENIED for tells what to do next, in one sentence.
    #[test]
    fn every_denied_reason_has_a_next_step() {
        for reason in [
            Reason::Fence,
            Reason::Unparsed,
            Reason::DeniedPath,
            Reason::Configuration,
            Reason::NoApprovalChannel,
            Reason::Leak,
            Reason::Halted,
            Reason::Loop,
            Reason::RateLimit,
        ] {
            let step = reason.next_step();
            assert!(step.ends_with('.') && step.len() > 20, "{}", reason.word());
            assert_eq!(find_retired_word(&step), None, "{}", reason.word());
        }
    }

    // §1::3 — the scan catches a retired word when one is introduced, and passes
    // once it is removed.
    #[test]
    fn v1_scan_catches_an_introduced_retired_word() {
        let bad = "fn f() { eprintln!(\"Waiting for the human.\"); }";
        let good = "fn f() { eprintln!(\"Waiting for the user.\"); }";
        // Raw strings are scanned too.
        let raw = "fn f() { let s = r#\"Session locked.\"#; let t = r\"lock\"; }";
        let lits = string_literals(raw);
        assert_eq!(lits, vec!["Session locked.".to_owned(), "lock".to_owned()]);
        assert!(lits.iter().all(|l| find_retired_word(l).is_some()));
        // An escaped quote inside a string does not end it.
        let escaped = r#"fn f() { eprintln!("say \"hi\" to the human"); }"#;
        assert_eq!(
            string_literals(escaped),
            vec!["say hi to the human".to_owned()]
        );
        assert!(
            string_literals(escaped)
                .iter()
                .any(|l| find_retired_word(l).is_some())
        );
        // A char literal holding a quote does not open a string.
        let chars = "fn f() { let q = '\"'; eprintln!(\"Claude is HELD.\"); }";
        assert_eq!(string_literals(chars), vec!["Claude is HELD.".to_owned()]);
        assert!(
            string_literals(bad)
                .iter()
                .any(|l| find_retired_word(l).is_some())
        );
        assert!(
            string_literals(good)
                .iter()
                .all(|l| find_retired_word(l).is_none())
        );
    }

    // §1::2 — the table is the single source: row i belongs to the Word with
    // discriminant i, so every word() reads its own row.
    #[test]
    fn v1_table_rows_match_word_order() {
        for (i, (word, text)) in TABLE.iter().enumerate() {
            assert_eq!(*word as usize, i, "row {i} ({text}) is out of order");
            assert_eq!(word.text(), *text);
        }
    }

    // §1::2 — reason codes live in this one table with the states and verdicts.
    #[test]
    fn v1_reasons_are_in_the_table_and_not_retired() {
        for r in [
            Reason::Fence,
            Reason::Unparsed,
            Reason::DeniedPath,
            Reason::Configuration,
            Reason::NoApprovalChannel,
            Reason::Leak,
            Reason::Halted,
            Reason::Loop,
            Reason::RateLimit,
        ] {
            assert_eq!(find_retired_word(r.word()), None, "{r:?}");
        }
    }

    // §1::1 — every state, verdict and party word comes from the vocabulary file.
    #[test]
    fn v1_words_match_the_vocabulary_file() {
        let words = [
            AgentState::Available.word(),
            AgentState::Held.word(),
            Word::Yolo.text(),
            AgentState::Unguarded.word(),
            AgentState::Stopped.word(),
            Word::Halted.text(),
            MessageState::Queued.word(),
            MessageState::Delivered.word(),
            MessageState::Failed.word(),
            Verdict::Granted.word(),
            Verdict::Denied.word(),
            Word::Protected.text(),
            THE_USER,
        ];
        let vocab = include_str!("../../../../docs/spec/standard-vocabulary.md");
        for w in words {
            let row = if w == THE_USER {
                "| the user |".to_owned()
            } else {
                format!("| {w} |")
            };
            assert!(
                vocab.contains(&row),
                "{w} is not a word in standard-vocabulary.md"
            );
        }
    }

    // §1::3 — the suite fails on a retired word and passes once it is removed.
    #[test]
    fn v1_retired_word_is_caught() {
        assert_eq!(find_retired_word("Waiting for the human."), Some("human"));
        assert_eq!(find_retired_word("Session locked."), Some("locked"));
        assert_eq!(find_retired_word("Claude is HELD."), None);
    }

    // G1-5, G1-6, G1-7: the whole text of every reason's next step and rule, as a person reads it:
    // plain words, no clause code, HELD defined where a rule first uses it, an unreadable command
    // answered with "split it" (the agent never runs it as it is), and no command that loosens the
    // guard.
    #[test]
    fn every_reason_reads_as_plain_words_in_full() {
        let table = [
            (
                Reason::Fence,
                "this would start work in another agent's session; to message an agent, use \
                 `lunchroom send <name> <text>`.",
                "a call that starts work in another agent's session by any way but `lunchroom \
                 send`, calls a local session chat API, or changes the variables Lunchroom reads \
                 an agent's identity from is DENIED in every session",
            ),
            (
                Reason::Unparsed,
                "Lunchroom could not read this command in full; split it into simple commands \
                 and run them one at a time. Only the user can run it as it is, in a terminal of \
                 their own.",
                "Lunchroom could not read this call in full, and what it could read names a way \
                 into another session or an identity variable, or runs a program or script that \
                 Lunchroom cannot find or read in full; such a call is DENIED in every session",
            ),
            (
                Reason::DeniedPath,
                "this path is, or leads to, Lunchroom's records or another session's \
                 transcripts; leave it out of the call, and the user can read it in a terminal \
                 of their own.",
                "a HELD session may not read Lunchroom's records or another session's \
                 transcript, nor search a folder that holds one; HELD means a message from \
                 another agent is in this session, so Lunchroom restricts what it may do",
            ),
            (
                Reason::Configuration,
                "changes to Lunchroom and its settings are for the user alone, in a terminal of \
                 their own.",
                "Lunchroom's records, its settings and the commands only the user may run are \
                 changed by the user alone, in a terminal of their own",
            ),
            (
                Reason::NoApprovalChannel,
                "this session has no permission prompt to ask the user; the user can approve \
                 this exact call with `lunchroom approve`, and then it can run.",
                "this call needs the user's yes, and this session has no permission prompt to \
                 ask it",
            ),
            (
                Reason::Leak,
                "the text carries or asks for a secret; remove it and send again.",
                "a message that carries a secret, or asks for one, is DENIED",
            ),
            (
                Reason::Halted,
                "this pair is HALTED: Lunchroom stopped their conversation, and the user typing \
                 in either session clears it.",
                "a conversation between these two agents is HALTED: no message passes between \
                 them until the user types in either session",
            ),
            (
                Reason::Loop,
                "this pair repeated itself; send something new.",
                "these two agents repeated themselves; a repeat stops their conversation",
            ),
            (
                Reason::RateLimit,
                "this pair passed the rate limit; wait, then send again.",
                "these two agents passed the rate limit the user set for messages between two \
                 agents",
            ),
        ];
        for (reason, step, rule) in table {
            assert_eq!(reason.next_step(), step, "{reason:?}");
            assert_eq!(reason.rule(), rule, "{reason:?}");
            for text in [reason.next_step(), reason.rule()] {
                // A clause code such as (F1.1) or (L1.2, L1.4) is for the code's comments.
                let coded = text.split('(').skip(1).any(|after| {
                    after.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                        && after
                            .split(')')
                            .next()
                            .is_some_and(|inside| inside.chars().any(|c| c.is_ascii_digit()))
                });
                assert!(!coded, "{text}");
                assert!(!text.contains("lunchroom mode") && !text.contains("lunchroom yolo"));
                assert_eq!(find_retired_word(&text), None, "{text}");
            }
        }
    }

    // §1::3 — none of the table's own words is retired.
    #[test]
    fn v1_table_words_are_not_retired() {
        for w in [
            "AVAILABLE",
            "HELD",
            "YOLO",
            "UNGUARDED",
            "STOPPED",
            "HALTED",
            "QUEUED",
            "DELIVERED",
            "FAILED",
            "GRANTED",
            "DENIED",
            "PROTECTED",
            THE_USER,
        ] {
            assert_eq!(find_retired_word(w), None, "{w}");
        }
    }
}
