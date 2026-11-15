//! Lunchroom 1.1, unit C1: what the tool-call and turn hooks tell (spec §16 item 2 channel D,
//! §10 D1.8, §15 K1.28).
//!
//! D1.8: "In Claude Code, the result of the session's next tool call (`PostToolUse`) carries them
//! to a working session." "A prompt or turn no person typed is told nothing by any hook of that
//! session, a tool-call hook included." "Claude Code reports no answer, so a call recorded as
//! waiting that has no after-call hook when the session's next `Stop`, `UserPromptSubmit` or
//! `SessionStart` runs ... is told as Denied, since the turn or the session ended with the call
//! not run (K1.28)."

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::Mail;
use lunchroom_core::mail::calls::Outcome;
use serde_json::Value;

use super::{HookInput, HookOut, cmdline, codex_headless, codex_process, store};
use crate::home::Env;

/// The most bytes of notices one answer carries: a hook answers within 10 seconds and the harness
/// limits what it takes. The notices past it stay untold for the next hook (they are not lost).
const ANSWER_BYTES: usize = 16 * 1024;

/// A notice as it is shown: a line of Lunchroom's already begins `<HH:MM> UTC Lunchroom: `; an
/// older notice (D1.4) is given the word `Lunchroom:` in front.
pub(super) fn as_shown(text: &str) -> String {
    let b = text.as_bytes();
    let line_form = b.len() > 5
        && b[2] == b':'
        && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
        && text[5..].starts_with(" UTC Lunchroom: ");
    if line_form {
        text.to_owned()
    } else {
        format!("Lunchroom: {text}")
    }
}

/// Whether a person, not a program, typed the turn this hook runs in, as far as the harness tells:
/// an attended interactive Claude Code; for Codex, not a `codex exec` or `codex review`; for the
/// plugins, the plugin's report of the user's own typing (D1.8).
pub(super) fn person_turn(input: &HookInput, env: &Env) -> bool {
    match input.harness {
        Harness::ClaudeCode => super::channel_of(input, env),
        Harness::Codex => !codex_process()
            .and_then(cmdline)
            .is_some_and(|a| codex_headless(&a)),
        Harness::OpenClaw | Harness::Hermes => input.user_typed == Some(true),
    }
}

/// The session's own notices as the context of the hook's answer (`additionalContext`), marked told
/// once the answer is produced (K1.25: a hook killed before that shows them again). Nothing in a
/// turn no person typed, nothing when there are none. Notice text is data to the agent: it is
/// added as Lunchroom's lines, never read for commands.
///
/// # Errors
/// When the store cannot be used.
pub(super) fn own_notices(
    input: &HookInput,
    env: &Env,
    event: &'static str,
) -> Result<HookOut, String> {
    if !person_turn(input, env) {
        return Ok(HookOut::Silent);
    }
    let me = input.me();
    let store = store(env)?;
    let all = store.read().map_err(crate::cli::unwritten)?;
    let mut taken = Vec::new();
    let mut shown = Vec::new();
    let mut total = 0;
    for n in all.notices(&me) {
        let line = as_shown(&n.text);
        if !taken.is_empty() && total + line.len() + 1 > ANSWER_BYTES {
            break;
        }
        total += line.len() + 1;
        shown.push(line);
        taken.push(n.clone());
    }
    if taken.is_empty() {
        return Ok(HookOut::Silent);
    }
    let out = HookOut::Event {
        event,
        notice: None,
        context: Some(shown.join("\n")),
        title: None,
    };
    let _ = store.update(|m| m.mark_notices_told(&me, &taken));
    Ok(out)
}

/// The calls of `me` that the harness's own prompt answers and that have no after-call hook by now
/// are told as Denied (K1.28): the turn or the session ended with the call not run.
pub(super) fn close_unanswered(m: &mut Mail, me: &Identity, at: u64) {
    let ids: Vec<String> = m
        .waiting_calls()
        .iter()
        .filter(|c| c.session == *me && !c.kept)
        .map(|c| c.id.clone())
        .collect();
    for id in ids {
        m.call_ends_in(me, &id, Outcome::Denied, at, None);
    }
}

/// `lunchroom guard callend`: a plugin reports that the call `tool_use_id` of `session_id` stopped
/// waiting: `{"harness": "openclaw"|"hermes", "session_id", "tool_use_id", "outcome":
/// "granted"|"denied"|"not_approved_in_time"}` (D1.8). An outcome it does not know is an error:
/// nothing is told on a guess.
///
/// # Errors
/// When the report is not that, or the store cannot be used.
pub fn callend(raw: &str, env: &Env) -> Result<HookOut, String> {
    let v: Value = serde_json::from_str(raw).map_err(|_| "the report is not JSON")?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let harness = match text("harness") {
        Some("openclaw") => Harness::OpenClaw,
        Some("hermes") => Harness::Hermes,
        _ => return Err("the report names no harness of a plugin".to_owned()),
    };
    let session = text("session_id").ok_or("the report has no session id")?;
    let call = text("tool_use_id").ok_or("the report has no call id")?;
    let outcome = match text("outcome") {
        Some("granted") => Outcome::Granted,
        Some("denied") => Outcome::Denied,
        Some("not_approved_in_time") => Outcome::NotApprovedInTime,
        _ => return Err("the report's outcome is not one Lunchroom knows".to_owned()),
    };
    let me = Identity::new(harness, session);
    store(env)?
        .update(|m| m.call_ends_in(&me, call, outcome, env.now, None))
        .map_err(crate::cli::unwritten)?;
    Ok(HookOut::Silent)
}
