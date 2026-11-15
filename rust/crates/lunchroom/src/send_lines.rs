//! What `lunchroom send` prints about the recipient: channel S of §16 item 2.
//!
//! §16 item 2: "S, the output of `lunchroom send`: printed at once, before `send` returns, the
//! same in Claude Code, Codex, OpenClaw and Hermes ... It carries the line of an event that has
//! happened by then: a message DENIED, a pair HALTED, a recipient UNGUARDED or STOPPED, a
//! recipient already HELD. A recipient is not HELD yet when `send` returns if it is a Claude Code
//! or Hermes session ... or a Codex or OpenClaw session whose hand-over completes later ... For a
//! message to one of them `send` prints "<agent> will be HELD when it reads this." at once, and
//! the line "<agent> is HELD." reaches the sender by D at the moment the hold begins."

use lunchroom_core::cards;
use lunchroom_core::mail::Accepted;
use lunchroom_core::mail::Mail;
use lunchroom_core::mail::calls::Hearer;
use lunchroom_core::mail::lines::{self, Who};
use lunchroom_core::store::Store;
use lunchroom_core::vocab::AgentState;

use crate::evidence::{evidence, unguarded_reason};
use crate::home::Env;

/// The lines for a message to an UNGUARDED or STOPPED recipient `to` (R3.2, §16 item 20): the line
/// and, for UNGUARDED, the reason in the form an agent may read, as `send` prints them; the line
/// is also told to the user (U) and put in the recipient's next envelope (N, K1.29), once each
/// time the agent becomes UNGUARDED. `None` for a guarded recipient.
pub(crate) fn unguarded(
    env: &Env,
    store: &Store<Mail>,
    accepted: &Accepted,
    to: &str,
) -> Option<String> {
    let mail = store.read().ok()?;
    let state = mail.agent_state(&accepted.to, evidence(&mail, &accepted.to, &env.home));
    let (words, agent_reason, user_reason) = match state {
        AgentState::Unguarded => {
            let why = unguarded_reason(&mail, &accepted.to, &env.home);
            (
                cards::unguarded(to),
                why.as_ref().map(crate::evidence::Why::text_for_agent),
                why.as_ref().map(crate::evidence::Why::text),
            )
        }
        AgentState::Stopped => (cards::stopped(to), None, None),
        _ => return None,
    };
    drop(mail);
    let at = env.now;
    let line = lines::build(
        at,
        &words,
        Who::Agents(&[to]),
        &agent_reason.into_iter().collect::<Vec<_>>(),
    );
    let _ = store.update(|m| {
        let fresh = state != AgentState::Unguarded || m.first_unguarded_line(&accepted.to);
        if fresh {
            m.tell_line(
                at,
                &words,
                Who::Agents(&[to]),
                std::slice::from_ref(&accepted.to),
                &user_reason.into_iter().collect::<Vec<_>>(),
                // The agent has no hook Lunchroom can trust: it hears the line by N (K1.29).
                &[Hearer {
                    who: accepted.to.clone(),
                    d: false,
                    n: true,
                }],
                true,
            );
        }
    });
    Some(format!("{line}\n"))
}

/// A `send` that was DENIED (V1.4, §16 items 8 and 12): the card's line, where the refusal has
/// one, is printed in the form of item 1; and the user is told, as a notice of the user's list,
/// what was DENIED, why and the ways forward (the console shows the user's list), with the text at
/// fault as the message wrote it with its secrets replaced. The text of the refusal the agent
/// reads is otherwise as it was.
pub(crate) fn denied(
    env: &Env,
    store: &Store<Mail>,
    refusal: &lunchroom_core::mail::SendRefusal,
    from: &str,
    to: &str,
    text: &str,
    mut out: crate::cli::Outcome,
) -> crate::cli::Outcome {
    use lunchroom_core::mail::SendRefusal;
    use lunchroom_core::vocab::Reason;
    let (words, message_event) = match refusal {
        SendRefusal::Denied(Reason::Leak) if lunchroom_core::secrets::asks_for_secret(text) => {
            (Some(cards::asked_for_secret()), true)
        }
        SendRefusal::Denied(Reason::Halted) => (Some(cards::conversation_halted()), false),
        _ => (None, false),
    };
    let mut body = out.stderr.clone();
    if let Some(words) = words {
        let names = [from, to];
        let who = match (from.is_empty(), message_event) {
            (true, _) => Who::Agents(&names[1..]),
            (false, true) => Who::Message { from, to },
            (false, false) => Who::Agents(&names),
        };
        let line = lines::build(env.now, &words, who, &[]);
        if let Some(rest) = body.strip_prefix(&format!("{words}\n")) {
            body = format!("{line}\n{rest}");
        }
    }
    // The user's notice: the explanation, with the text at fault, secrets replaced and every
    // control character written visibly, cut to what a notice can hold.
    let shown = lunchroom_core::approvals::visible(&lunchroom_core::secrets::redact(text));
    let shown: String = shown.chars().take(512).collect();
    let notice = format!(
        "{}\nThe text of the message, as sent (secrets replaced): {shown}",
        body.trim_end()
    );
    let _ = store.update(|m| m.tell_user(&notice, env.now));
    out.stderr = body;
    out
}

/// The line for a message to a guarded recipient, after the hand-over `send` made: "<agent> is
/// HELD." when the recipient is HELD now (the sender hears it by S, and not again by D), else
/// "<agent> will be HELD when it reads this." (the sender hears "is HELD." by D when the hold
/// begins).
pub(crate) fn held(env: &Env, store: &Store<Mail>, accepted: &Accepted, to: &str) -> String {
    let at = env.now;
    let told = store
        .update(|m| {
            // §16 item 17 (channel S): the sender hears the YOLO line in the output of each
            // `send` to the covered agent while the YOLO lasts.
            let yolo = m
                .yolo_until(&accepted.to, at)
                .map(|end| lines::build(at, &cards::yolo_until(end), Who::Agents(&[to]), &[]));
            let held = if !m.is_held(&accepted.to) {
                let words = format!(
                    "{to} will be {} when it reads this.",
                    lunchroom_core::vocab::Word::Held.text()
                );
                let line = lines::build(at, &words, Who::Agents(&[to]), &[]);
                m.line_for(&accepted.from, &line)
            } else if let Some(line) = m.sender_heard_held_by_s(&accepted.id) {
                // The hold began with this message (the hand-over went ahead inside `send`): the
                // line was recorded for D, and `send` prints it instead.
                line
            } else {
                // The recipient was HELD already: the line of the event as it stands now.
                let line = lines::build(at, &cards::held(to), Who::Agents(&[to]), &[]);
                m.line_for(&accepted.from, &line)
            };
            match yolo {
                Some(y) => format!("{held}\n{y}"),
                None => held,
            }
        })
        .unwrap_or_default();
    format!("{told}\n")
}
