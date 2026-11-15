//! The marked parts of a call (spec §17.10, "Marked parts"): each byte range of the call's text, as
//! the guard read it, that made the call go to the user or be DENIED, with the kind beside it.
//!
//! The call's text is what the console shows: a shell command's command line; a tool call's input as
//! the text that A2.1 hashes (its canonical JSON); and, for a script a call runs, the script's own
//! text. A range is judged by the same rules as the call: the text of each command of the call is
//! judged on its own, so a part is marked for exactly what the classifier finds in it.

use std::path::Path;

use serde_json::{Value, json};

use super::gated::{Reading, read, scan_reading};
use super::{Guard, Judged, SHELLS, basename, program_positions};
use crate::fs::Script;
use crate::policy::{CallClass, DenyReason, ProtectedKind};

/// What a marked part is judged as (§17.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    /// Widens what an agent or Lunchroom's own files allow (writing the guard, a slip, credentials,
    /// text to another session, starting a session).
    Loosens,
    /// Deletes, pushes, publishes, merges, deploys, migrates, or sends data off the computer.
    Destroys,
    /// The guard could not read it to the end.
    NotRead,
    /// The reply, one `lunchroom send` (P1.1), as a whole.
    Reply,
}

/// The text a mark's range is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkSource {
    /// The call's own text.
    Call,
    /// The text of the script the call runs, by its place in [`Marked::scripts`].
    Script(usize),
}

/// One marked part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    /// The text the range is in.
    pub source: MarkSource,
    /// The first byte of the range (a character boundary).
    pub start: usize,
    /// The byte after the range (a character boundary).
    pub end: usize,
    /// What the part is judged as; one part can be several.
    pub kinds: Vec<MarkKind>,
    /// The part's words.
    pub words: String,
    /// The reason word, for a range that made the call DENIED.
    pub reason: Option<DenyReason>,
}

/// A call's text and what the guard marked in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marked {
    /// The command line, or the canonical JSON that A2.1 hashes.
    pub text: String,
    /// The texts of the scripts the call runs.
    pub scripts: Vec<String>,
    /// The marked ranges.
    pub marks: Vec<Mark>,
    /// The call's text cannot be read: the whole call is "not read".
    pub whole_call_not_read: bool,
}

/// The most commands of a call, and of the scripts it runs, that are marked one by one; a call with
/// more cannot be read to the end.
const MAX_PARTS: usize = 256;

/// The most scripts of a call that are read.
const MAX_SCRIPTS: usize = 8;

/// How the kinds a part raised show as mark kinds, and the reason word of a DENIED class.
fn kinds_of(j: &Judged, text: &str) -> (Vec<MarkKind>, Option<DenyReason>) {
    let mut out: Vec<MarkKind> = Vec::new();
    let mut add = |k: MarkKind| {
        if !out.contains(&k) {
            out.push(k);
        }
    };
    let mut reason = None;
    if let CallClass::Denied(r) = j.call.class {
        reason = Some(r);
        match r {
            DenyReason::Unparsed | DenyReason::TooManyPaths => add(MarkKind::NotRead),
            _ => add(MarkKind::Loosens),
        }
    }
    let kinds = j.call.kinds;
    for k in [
        ProtectedKind::GuardConfig,
        ProtectedKind::Credential,
        ProtectedKind::OtherSession,
    ] {
        if kinds.contains(k) {
            add(MarkKind::Loosens);
        }
    }
    for k in [
        ProtectedKind::Delete,
        ProtectedKind::PushOrPublish,
        ProtectedKind::Network,
    ] {
        if kinds.contains(k) {
            add(MarkKind::Destroys);
        }
    }
    if kinds.contains(ProtectedKind::UnknownTool) {
        add(MarkKind::Destroys);
        add(MarkKind::NotRead);
    }
    if kinds.contains(ProtectedKind::Unparsed) {
        add(MarkKind::NotRead);
    }
    if kinds.contains(ProtectedKind::Gated) {
        let g = scan_reading(&read(text), 0);
        if g.words() {
            add(MarkKind::Destroys);
        }
        if g.unread || !g.words() {
            add(MarkKind::NotRead);
        }
    }
    (out, reason)
}

impl Guard<'_> {
    /// The text of the call and every range of it, and of the scripts it runs, that made it go to
    /// the user or be DENIED (§17.10).
    #[must_use]
    pub fn marked(&self, tool: &str, input: &Value, cwd: &Path) -> Marked {
        let whole = self.judge(tool, input, cwd);
        let mut m = Marked {
            text: String::new(),
            scripts: Vec::new(),
            marks: Vec::new(),
            whole_call_not_read: false,
        };
        let command = (tool == "Bash")
            .then(|| input.get("command").and_then(Value::as_str))
            .flatten();
        let Some(command) = command else {
            m.text = crate::jcs::canonical(input);
            Self::mark_tool(&whole, input, &mut m);
            return m;
        };
        command.clone_into(&mut m.text);
        if whole.call.kinds.contains(ProtectedKind::Reply) {
            m.marks.push(Mark {
                source: MarkSource::Call,
                start: 0,
                end: command.len(),
                kinds: vec![MarkKind::Reply],
                words: command.to_owned(),
                reason: None,
            });
        }
        // Text the guard cannot parse, or reads only as far as its words: the whole call is not read.
        let unparsed = whole.call.kinds.contains(ProtectedKind::Unparsed)
            || matches!(whole.call.class, CallClass::Denied(DenyReason::Unparsed));
        if unparsed {
            m.whole_call_not_read = true;
            m.marks.push(Mark {
                source: MarkSource::Call,
                start: 0,
                end: command.len(),
                kinds: vec![MarkKind::NotRead],
                words: command.to_owned(),
                reason: match whole.call.class {
                    CallClass::Denied(r) => Some(r),
                    _ => None,
                },
            });
        }
        let mut budget = MAX_PARTS;
        self.mark_text(command, MarkSource::Call, cwd, &mut budget, &mut m, 0);
        // A DENIED class that no part of the text shows (a rule on the whole call) marks the whole.
        if let CallClass::Denied(r) = whole.call.class
            && !m.marks.iter().any(|k| k.reason == Some(r))
        {
            let (kinds, reason) = kinds_of(&whole, command);
            m.marks.push(Mark {
                source: MarkSource::Call,
                start: 0,
                end: command.len(),
                kinds,
                words: command.to_owned(),
                reason,
            });
        }
        m
    }

    /// A tool call that is no shell command: its text is its canonical JSON, and the part is the
    /// path it names where the text holds it, else the whole.
    fn mark_tool(whole: &Judged, input: &Value, m: &mut Marked) {
        let (kinds, reason) = kinds_of(whole, "");
        if kinds.is_empty() && reason.is_none() {
            return;
        }
        if kinds.contains(&MarkKind::NotRead)
            && matches!(reason, Some(DenyReason::Unparsed) | None)
            && whole.call.kinds.contains(ProtectedKind::Unparsed)
        {
            m.whole_call_not_read = true;
        }
        let mut range = (0, m.text.len());
        for key in ["file_path", "path", "notebook_path"] {
            if let Some(p) = input.get(key).and_then(Value::as_str)
                && !p.is_empty()
                && let Some(at) = m.text.find(&crate::jcs::canonical(&json!(p)))
            {
                range = (at, at + crate::jcs::canonical(&json!(p)).len());
                break;
            }
        }
        m.marks.push(Mark {
            source: MarkSource::Call,
            start: range.0,
            end: range.1,
            kinds,
            words: m.text[range.0..range.1].to_owned(),
            reason,
        });
    }

    /// Marks each command of `text` that the classifier finds something in, and follows the scripts
    /// it runs.
    fn mark_text(
        &self,
        text: &str,
        source: MarkSource,
        cwd: &Path,
        budget: &mut usize,
        m: &mut Marked,
        level: usize,
    ) {
        let rd: Reading = read(text);
        if rd.broken {
            m.whole_call_not_read = true;
        }
        for (c, &(start, end)) in rd.cmds.iter().zip(&rd.spans) {
            if *budget == 0 {
                m.whole_call_not_read = true;
                return;
            }
            let Some(part) = text.get(start..end) else {
                continue;
            };
            *budget -= 1;
            let j = self.judge("Bash", &json!({ "command": part }), cwd);
            let (kinds, reason) = kinds_of(&j, part);
            // The reply is marked once, as a whole.
            if !kinds.is_empty() || reason.is_some() {
                m.marks.push(Mark {
                    source,
                    start,
                    end,
                    kinds,
                    words: part.to_owned(),
                    reason,
                });
            }
            if level < 2 && m.scripts.len() < MAX_SCRIPTS {
                let t: Vec<&str> = c.iter().map(|w| w.t.as_str()).collect();
                if let Some(body) = self.script_text(&t, cwd) {
                    let idx = m.scripts.len();
                    m.scripts.push(body.clone());
                    self.mark_text(&body, MarkSource::Script(idx), cwd, budget, m, level + 1);
                }
            }
        }
    }

    /// The text of the script a command runs: a shell given a file, or a program run by a path that
    /// is a script.
    fn script_text(&self, t: &[&str], cwd: &Path) -> Option<String> {
        for p in program_positions(t) {
            let name = basename(t[p]);
            let file = if SHELLS.contains(&name) {
                let args = &t[p + 1..];
                if args
                    .iter()
                    .any(|a| a.starts_with('-') && a.contains('c') && !a.starts_with("--"))
                {
                    continue;
                }
                args.iter().find(|a| !a.starts_with('-')).copied()
            } else if t[p].contains('/') {
                Some(t[p])
            } else {
                None
            };
            let Some(file) = file else { continue };
            let path = self.physical(file, cwd);
            if !self.fs.exists(&path) || (!SHELLS.contains(&name) && !self.fs.runs_as_script(&path))
            {
                continue;
            }
            if let Script::Text(body) = self.fs.script(&path) {
                return Some(body);
            }
        }
        None
    }
}
