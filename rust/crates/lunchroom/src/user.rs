//! The commands only the user may run: `lunchroom approve`, `register`, `unregister`, `rate` and
//! `passphrase`.
//!
//! Spec: §7 A1.1 (the user approves through `lunchroom approve` in a terminal outside every guarded
//! session), A1.3 (each runs only on the user's passphrase from such a terminal; the guard also
//! DENIES an agent's command that runs one), §8 A2.1–A2.2 (the approval covers one exact call; the
//! prompt shows it in full), §9 I1.5 (naming a session; a taken name is reported, never moved
//! silently), §2 R1.7 (the user sets the rate limit).

use std::fmt::Write as _;

use lunchroom_core::approvals::{
    APPROVAL_SECS, Request, TellKind, call_key, call_shown_hash, visible,
};
use lunchroom_core::cards;
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::jcs::shown;
use lunchroom_core::loops::Limits;
use lunchroom_core::mail::{Mail, Origin, RegisterRefusal, clock, span, valid_name};
use lunchroom_core::passphrase::{self, Verifier};
use lunchroom_core::policy::HoldMode;
use lunchroom_core::store::Store;
use lunchroom_core::vocab::Word;

use crate::cli::Outcome;
use crate::home::Env;
use crate::terminal::{
    ITERATIONS, NO_SECRET_READ, NO_VERIFIER, System, check, confirm, trusted_verifier,
    verifier_shown,
};

fn store(env: &Env) -> Result<Store<Mail>, Outcome> {
    Store::open(&env.store_dir())
        .map_err(|e| Outcome::failed(&format!("Lunchroom's store cannot be opened: {e}"), 1))
}

fn read(store: &Store<Mail>) -> Result<Mail, Outcome> {
    store
        .read()
        .map_err(|e| Outcome::failed(&crate::cli::unread(e), 1))
}

/// `lunchroom approve` lists the calls waiting for the user; `lunchroom approve <n>` approves the
/// n-th one's exact call, on the user's confirmation.
#[must_use]
pub fn approve(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    // A call that has waited as long as an approval lasts is no longer asked about; the list shows
    // only calls that can still be approved. A store that cannot be written lists what it reads.
    let _ = store.update(|m| m.expire_requests(env.now));
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let Some(arg) = args.first() else {
        let mut out = String::new();
        let now = env.now;
        for (n, r) in mail.requests().iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. {} ({}) {} {} (asked {} ago)",
                n + 1,
                visible(&r.agent),
                visible(&r.session.to_string()),
                visible(&r.tool),
                shown(&r.input),
                span(now.saturating_sub(r.at))
            );
        }
        return Outcome::ok(out);
    };
    let Some(index) = arg.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) else {
        return Outcome::failed("usage: lunchroom approve [<number from the list>]", 2);
    };
    let Some(request) = mail.requests().get(index).cloned() else {
        return Outcome::denied("no call with that number is waiting", 2);
    };
    approve_at(index, &request, &mail, &store, env, sys)
}

/// The console's approve: approves the exact call the user was shown, found by the store's own
/// name for it ([`call_key`]) and only if it is still shown as it was (`shown`, from
/// [`call_shown_hash`]). A call that is gone, or is now shown differently, approves nothing: the
/// position it had in the list when it was shown is never used.
#[must_use]
pub fn approve_call(key: &str, shown: &str, env: &Env, sys: &mut dyn System) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let _ = store.update(|m| m.expire_requests(env.now));
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let Some((index, request)) = mail
        .requests()
        .iter()
        .enumerate()
        .find(|(_, r)| call_key(r) == key)
        .map(|(i, r)| (i, r.clone()))
    else {
        return Outcome::denied(
            "the call is no longer waiting, so nothing was approved: look at the list again",
            2,
        );
    };
    if call_shown_hash(&request) != shown {
        return Outcome::denied(
            "the call is shown differently now, so nothing was approved: look at it again",
            3,
        );
    }
    approve_at(index, &request, &mail, &store, env, sys)
}

/// Asks the user about the waiting call `request` (at `index` in `mail`) and approves it.
fn approve_at(
    index: usize,
    request: &Request,
    mail: &Mail,
    store: &Store<Mail>,
    env: &Env,
    sys: &mut dyn System,
) -> Outcome {
    // A2.2: what the user confirms is this text; the approval covers nothing else.
    let shown = mail.approval_prompt(
        &request.session,
        &request.agent,
        &request.tool,
        &request.input,
        &request.cwd,
    );
    let question = approval_question(
        &request.rule,
        &shown,
        &span(env.now.saturating_sub(request.at)),
        &clock(request.at.saturating_add(APPROVAL_SECS)),
    );
    if let Err(reason) = confirm(mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    // The waiting list or the messages holding the session may have changed while the user read
    // the prompt: the approval is given only if the same call would still be shown the same way.
    let approved = store.update(|m| m.approve_request(index, request, &shown, env.now));
    match approved {
        // Card 4: "Granted. Codex can merge.", with the agent and the call of the real event.
        Ok(Ok(id)) => {
            let granted = cards::granted(
                &request.agent,
                &cards::action_of(&request.tool, &request.input),
            );
            let told = tell_decision(env, store, request, &id, TellKind::Approved)
                .err()
                .map_or_else(String::new, |reason| {
                    format!(
                        "The session was not told: {reason}. The call runs if the agent tries it \
                         again within 10 minutes.\n"
                    )
                });
            Outcome::ok(format!("{granted}\n{id}\n{told}"))
        }
        Ok(Err(_)) => Outcome::denied(
            "what you were shown changed while you confirmed (a message may have arrived, or the \
             list of waiting calls changed); nothing was approved. Run `lunchroom approve` again.",
            3,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// Tells the agent of `request` of the user's decision `id`, through the channel A1.2 names for an
/// approval of its harness: Codex, through `codex queue`, and OpenClaw, through the hand-over of
/// D1.1 (`openclaw agent`), each one line naming only the id, which the agent reads with
/// `lunchroom read <id>`; Hermes, at the start of its next turn, through the context the plugin's
/// `pre_llm_call` returns (the line waits in the store, kept when the decision was made). Claude
/// Code has no channel in A1.2: its retry's reason tells it (§17.10), so there is nothing to send.
/// The line adds no hold and clears none (A2.1).
///
/// # Errors
/// Why the session could not be told; the decision stands whether or not the session was told.
pub fn tell_decision(
    env: &Env,
    store: &Store<Mail>,
    request: &Request,
    id: &str,
    kind: TellKind,
) -> Result<(), String> {
    match request.session.harness {
        Harness::Codex => crate::codex::tell(env, store, request, id, kind),
        Harness::OpenClaw => crate::openclaw::tell(env, store, request, id, kind),
        Harness::Hermes | Harness::ClaudeCode => Ok(()),
    }
}

/// §17.10: the user refuses the waiting call `request`. The call is DENIED, a retry of that exact
/// call is DENIED with the reason that the user refused it, and the agent is told through
/// [`tell_decision`]. Returns the refusal's id, or why nothing was refused. The console's refuse
/// key calls it; nothing here asks for a passphrase, since a refusal only denies.
///
/// # Errors
/// The call is no longer waiting, the store cannot be used, or (the refusal then stands) the
/// session could not be told.
pub fn refuse(env: &Env, store: &Store<Mail>, request: &Request) -> Result<String, String> {
    let id = store
        .update(|m| {
            let at = m.requests().iter().position(|r| r == request)?;
            m.refuse_request(at, request, env.now).ok()
        })
        .map_err(crate::cli::unwritten)?
        .ok_or("that call is no longer waiting")?;
    tell_decision(env, store, request, &id, TellKind::Refused)
        .map_err(|reason| format!("{id} declined; the session was not told: {reason}"))?;
    Ok(id)
}

/// F2-AC52 (§17.10): the console declines the waiting call it showed as `key` and `shown`, found
/// as [`approve_call`] finds it. The call is refused through [`refuse`] (DENIED, its exact retry
/// DENIED with the user's reason, the agent told), and the user is told the outcome as D1.8 words
/// it: "Denied. <agent>'s call <id> was not approved."
pub fn refuse_call(key: &str, shown: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let _ = store.update(|m| m.expire_requests(env.now));
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let Some(request) = mail.requests().iter().find(|r| call_key(r) == key).cloned() else {
        return Outcome::denied(
            "the call is no longer waiting, so nothing was declined: look at the list again",
            2,
        );
    };
    if call_shown_hash(&request) != shown {
        return Outcome::denied(
            "the call is shown differently now, so nothing was declined: look at it again",
            3,
        );
    }
    match refuse(env, &store, &request) {
        Ok(id) => Outcome::ok(format!(
            "Denied. {}'s call {id} was not approved.\n",
            request.agent
        )),
        Err(reason) => Outcome::denied(&reason, 3),
    }
}

/// The words of the line of Lunchroom's that records a hold the user ended from the console
/// (F2-AC53; F2 design "Lists").
pub const RELEASE_WORDS: &str = "The user ended this agent's hold from the console.";

/// F2-AC53 (§17.10): the console releases the hold on the agent named `name`. The hold ends as when
/// the user types in that session ([`Mail::user_typed`]) and is recorded as a line of Lunchroom's
/// (§16 item 1). The console asked the passphrase before this.
pub fn release_hold(name: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let released = store.update(|m| {
        let who = m.named(name)?.clone();
        if m.holding(&who).is_empty() {
            return Some(false);
        }
        m.user_typed(&who);
        m.record_line(
            env.now,
            RELEASE_WORDS,
            lunchroom_core::mail::lines::Who::Agents(&[name]),
            std::slice::from_ref(&who),
            &[],
        );
        Some(true)
    });
    match released {
        Ok(Some(true)) => Outcome::ok(format!(
            "{name}'s hold ended, as when you type in its session.\n"
        )),
        Ok(Some(false)) => {
            Outcome::denied(&format!("{name} is not HELD, so nothing was released"), 3)
        }
        Ok(None) => Outcome::denied(
            &format!("{name} is not a registered name, so nothing was released"),
            2,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// F2-AC54 (§17.10): the console resends the FAILED message `id` as a new message, from the same
/// sender to the same recipient with the same text, under every rule of D1 ([`Mail::send`]); it
/// holds its recipient when it is delivered, as any message does. The FAILED message stays as it
/// was. A message that is not FAILED, or a send that D1 refuses, resends nothing.
pub fn resend_failed(id: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let sent = store.update(|m| {
        let old = m
            .message(id)
            .filter(|old| old.status.state() == lunchroom_core::vocab::MessageState::Failed)
            .cloned()?;
        let result = m
            .send(&old.from, &old.from_name, &old.to_name, &old.text, env.now)
            .map(|accepted| accepted.id);
        Some((old, result))
    });
    match sent {
        Ok(Some((_, Ok(new)))) => Outcome::ok(format!("{id} was resent as {new}, QUEUED.\n")),
        // D1 refused the new message: said as `lunchroom send` says it (the text never shown).
        Ok(Some((old, Err(refusal)))) => crate::cli::refused(
            &refusal,
            &crate::cli::Refused {
                to: &old.to_name,
                from: &old.from_name,
                text: &old.text,
                held_by: &[],
            },
        ),
        Ok(None) => Outcome::denied(
            &format!("{id} is not a FAILED message, so nothing was resent"),
            3,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// F2-AC54: the line of Lunchroom's that records a discard, and what its sender is told.
pub fn discard_words(id: &str) -> String {
    format!("The user discarded message {id} from the console.")
}
/// F2-AC54: what the sender of a discarded message is told.
pub fn discard_notice(id: &str, to: &str) -> String {
    format!("Your message {id} to {to} was discarded by the user.")
}
/// F2-AC55: the line that records a hold by the user.
pub const HOLD_WORDS: &str = "The user held this agent from the console.";
/// F2-AC55: the line that records a dismissed PROBING alert.
pub const DISMISS_WORDS: &str = "The user dismissed the PROBING alert for this agent.";
/// F2-AC57: the line that records a stale hold released.
pub const STALE_WORDS: &str = "The user released a hold whose message is gone.";

/// F2-AC54, F2-AC57 (§17.10): the console discards the QUEUED or FAILED message `id`
/// ([`Mail::discard_message`]) and records it as a line of Lunchroom's. A DELIVERED message, or an
/// id the store does not hold, discards nothing.
pub fn discard_message(id: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let done = store.update(|m| {
        let to = m
            .message(id)
            .map(|msg| msg.to_name.clone())
            .unwrap_or_default();
        let result = m.discard_message(id, &discard_notice(id, &to), env.now);
        if let Ok(gone) = &result {
            m.record_line(
                env.now,
                &discard_words(id),
                lunchroom_core::mail::lines::Who::Agents(&[gone.to_name.as_str()]),
                std::slice::from_ref(&gone.to),
                &[],
            );
        }
        result
    });
    match done {
        Ok(Ok(_)) => Outcome::ok(format!("{id} was discarded and recorded in the log.\n")),
        Ok(Err(lunchroom_core::mail::Refused::Delivered)) => {
            Outcome::denied(&format!("{id} was DELIVERED, so it cannot be discarded"), 3)
        }
        Ok(Err(lunchroom_core::mail::Refused::Unknown)) => Outcome::denied(
            &format!("there is no message {id}, so nothing was discarded"),
            2,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// F2-AC55 (§17.10): the console holds the agent named `name` as HELD by the user
/// ([`Mail::hold_by_user`]) and records it as a line of Lunchroom's.
pub fn hold_agent(name: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let held = store.update(|m| {
        let who = m.named(name)?.clone();
        m.hold_by_user(&who, env.now);
        m.record_line(
            env.now,
            HOLD_WORDS,
            lunchroom_core::mail::lines::Who::Agents(&[name]),
            std::slice::from_ref(&who),
            &[],
        );
        Some(())
    });
    match held {
        Ok(Some(())) => Outcome::ok(format!(
            "{name} is HELD by you; typing in its session, or releasing it here, ends the hold.\n"
        )),
        Ok(None) => Outcome::denied(
            &format!("{name} is not a registered name, so nothing was held"),
            2,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// F2-AC55 (§17.10): the console dismisses the PROBING alert `alert` (`probing:<session id>`)
/// ([`Mail::dismiss_probe`]); it stays dark until that agent's next DENIED decision. Any other
/// alert dismisses nothing.
pub fn dismiss_probe(alert: &str, env: &Env) -> Outcome {
    let Some(session) = alert.strip_prefix("probing:") else {
        return Outcome::denied("only a PROBING alert can be dismissed here", 2);
    };
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let done = store.update(|m| {
        m.dismiss_probe(session, env.now);
        let agents: Vec<(String, lunchroom_core::identity::Identity)> = m
            .names()
            .filter(|(_, who)| who.session == session)
            .map(|(n, who)| (n.to_owned(), who.clone()))
            .collect();
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        let ids: Vec<lunchroom_core::identity::Identity> =
            agents.iter().map(|(_, w)| w.clone()).collect();
        m.record_line(
            env.now,
            DISMISS_WORDS,
            lunchroom_core::mail::lines::Who::Agents(&names),
            &ids,
            &[],
        );
    });
    match done {
        Ok(()) => {
            Outcome::ok("The alert was dismissed until that agent's next DENIED call.\n".to_owned())
        }
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// F2-AC57 (§17.10): the console ends the holds on the agent named `name` whose message the store
/// no longer holds ([`Mail::release_stale`]), and records it as a line of Lunchroom's. A live hold
/// stays.
pub fn release_stale(name: &str, env: &Env) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let released = store.update(|m| {
        let who = m.named(name)?.clone();
        let n = m.release_stale(&who);
        if n > 0 {
            m.record_line(
                env.now,
                STALE_WORDS,
                lunchroom_core::mail::lines::Who::Agents(&[name]),
                std::slice::from_ref(&who),
                &[],
            );
        }
        Some(n)
    });
    match released {
        Ok(Some(0)) => Outcome::denied(
            &format!("{name} has no hold whose message is gone, so nothing was released"),
            3,
        ),
        Ok(Some(n)) => Outcome::ok(format!(
            "{n} hold(s) on {name} whose message is gone ended.\n"
        )),
        Ok(None) => Outcome::denied(
            &format!("{name} is not a registered name, so nothing was released"),
            2,
        ),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// What the user is asked to confirm for one waiting call (A2.2): `rule` is the reason the call
/// waits, `shown` the call as the approval covers it, `ago` how long ago the agent asked and
/// `lapses` when the request lapses. A rule kept now is a whole sentence ending in `.` and is
/// printed as it is (G4-1); a request kept by an older Lunchroom holds a noun phrase, which keeps
/// the old form.
fn approval_question(rule: &str, shown: &str, ago: &str, lapses: &str) -> String {
    let why = if rule.ends_with('.') {
        format!("Lunchroom: {rule}")
    } else {
        format!("Lunchroom: this call is outside {rule}.")
    };
    format!(
        "{why} Approve this one call, for 10 minutes, once?\n{shown}\n\
         The agent asked {ago} ago; the request lapses at {lapses} if it is not approved by \
         then.\n"
    )
}

/// The question `install` asks, and `lunchroom mode` without a mode shows (P1.6).
pub const MODE_QUESTION: &str = "\
How much should a message from one agent limit what the agent receiving it may do?
  1. Relaxed: agents act on each other's messages freely; Lunchroom still stops injection into
     other sessions, impersonating you, and leaking secrets, and changes to Lunchroom come to you.
  2. Balanced (recommended): agents act freely, but pushes, publishes, deletes, sending data out
     of this computer, credentials and changes to Lunchroom come to you.
  3. Strict: everything an agent would change after another agent's message waits for you.
Every tool-call guard has this limit: once a script runs, the guard does not see what it does.
Only Strict, without `lunchroom yolo`, stops a HELD agent's scripts.
Codex cannot show an approval prompt: what would come to you there is blocked instead and waits
for `lunchroom approve`.
";

/// The mode named by an answer to [`MODE_QUESTION`]: its number or its name; Balanced for none.
#[must_use]
pub fn mode_answer(answer: &str) -> Option<HoldMode> {
    match answer.trim() {
        "" | "2" => Some(HoldMode::Balanced),
        "1" => Some(HoldMode::Relaxed),
        "3" => Some(HoldMode::Strict),
        other => HoldMode::parse(other),
    }
}

/// `lunchroom mode [strict|balanced|relaxed]`: shows the hold mode, or the user sets it (P1.6).
#[must_use]
pub fn mode(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let Some(word) = args.first() else {
        return Outcome::ok(format!(
            "hold mode: {}\n\n{MODE_QUESTION}Set it with: lunchroom mode relaxed|balanced|strict\n",
            mail.hold_mode().word()
        ));
    };
    let Some(chosen) = HoldMode::parse(word) else {
        return Outcome::failed("usage: lunchroom mode [relaxed|balanced|strict]", 2);
    };
    let question = format!("Lunchroom: set the hold mode to {}?\n", chosen.word());
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    match store.update(|m| m.set_hold_mode(chosen)) {
        Ok(()) => Outcome::ok(format!("hold mode: {}\n", chosen.word())),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// `lunchroom repair`: when Lunchroom's store cannot be read, the user puts back the state from
/// before its last change; the unreadable file is kept beside it.
#[must_use]
pub fn repair(env: &Env, sys: &mut dyn System) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    if store.read().is_ok() {
        return Outcome::ok("Lunchroom's store reads; nothing to repair\n".to_owned());
    }
    // A1.3: the terminal is checked against the sessions and processes the previous state
    // records, the state this puts back; with none to put back, nothing is done.
    let previous = match store.read_previous() {
        Ok(m) => m,
        Err(e) => {
            return Outcome::failed(
                &format!("the store cannot be read and has no earlier state to put back: {e}"),
                1,
            );
        }
    };
    let question = "Lunchroom: the store cannot be read. Put back its state from before the last \
                    change? The unreadable file is kept beside it.\n";
    if let Err(reason) = confirm(&previous, sys, question) {
        return Outcome::denied(&reason, 3);
    }
    match store.restore_previous() {
        Ok(_) => Outcome::ok("the store's previous state is back\n".to_owned()),
        Err(e) => Outcome::failed(&format!("the store cannot be repaired: {e}"), 1),
    }
}

/// `lunchroom register <name> <harness>:<session id>`: the user names a session, which from then on
/// receives messages. A taken name is reported, and moved only on a second confirmation.
#[must_use]
pub fn register(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    // `<harness>:<session id>`, or the earlier forms `--codex-thread <id>` and
    // `--claude-session <id>`.
    let who = match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("--codex-thread"), Some(id)) if !id.is_empty() => {
            Some(Identity::new(Harness::Codex, id))
        }
        (Some("--claude-session"), Some(id)) if !id.is_empty() => {
            Some(Identity::new(Harness::ClaudeCode, id))
        }
        (Some(k), _) => Identity::parse(k),
        _ => None,
    };
    let (Some(name), Some(who)) = (args.first(), who) else {
        return Outcome::failed(
            "usage: lunchroom register <name> <harness>:<session id> \
             (or --codex-thread <id> or --claude-session <id>)",
            2,
        );
    };
    if !valid_name(name) {
        return Outcome::denied(
            "a name is 1 to 32 letters, digits, '.', '_' or '-' (no spaces)",
            2,
        );
    }
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let question = format!("Lunchroom: name {who} \"{name}\", so it receives messages?\n");
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let first = store.update(|m| m.register(name, &who, Origin::User, None));
    let holder = match first {
        Ok(Ok(_)) => return Outcome::ok(format!("{name} is {who}\n")),
        Ok(Err(RegisterRefusal::Taken { holder })) => holder,
        Ok(Err(RegisterRefusal::FromGuardedSession)) => {
            return Outcome::denied("a guarded session cannot register", 3);
        }
        Ok(Err(RegisterRefusal::InvalidName)) => {
            return Outcome::denied("the name is not valid", 2);
        }
        Err(e) => return Outcome::failed(&crate::cli::unwritten(e), 1),
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    // What moving the name does to the messages QUEUED for it, found by doing it to a copy.
    let fate = {
        let mut copy = mail.clone();
        let _ = copy.register(name, &who, Origin::User, Some(&holder));
        queued_fate(copy.fail_unnamed(env.now).len(), &holder)
    };
    let question = format!(
        "Lunchroom: the name \"{name}\" is held by {holder}. {fate} Move the name to {who}?\n"
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&format!("the name stays with {holder}: {reason}"), 3);
    }
    let moved = store.update(|m| {
        m.register(name, &who, Origin::User, Some(&holder))
            .map(|_| m.fail_unnamed(env.now))
    });
    match moved {
        Ok(Ok(failed)) => Outcome::ok(format!(
            "{name} is {who} (moved from {holder})\n{}",
            failed_line(&failed)
        )),
        Ok(Err(_)) => Outcome::denied("the name changed holder meanwhile; nothing was moved", 3),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// What taking a name from `holder` does to the `count` messages QUEUED for it: they become FAILED,
/// and their senders are told (D1.7), or there are none.
fn queued_fate(count: usize, holder: &Identity) -> String {
    match count {
        0 => format!("No message is QUEUED for {holder}."),
        1 => format!("1 message QUEUED for {holder} becomes FAILED, and its sender is told."),
        n => format!("{n} messages QUEUED for {holder} become FAILED, and their senders are told."),
    }
}

/// The line that says which messages a command made FAILED, or nothing when it made none.
fn failed_line(failed: &[String]) -> String {
    match failed {
        [] => String::new(),
        [one] => format!("{one} is now FAILED, and its sender is told.\n"),
        many => format!(
            "{} are now FAILED, and their senders are told.\n",
            many.join(", ")
        ),
    }
}

/// `lunchroom unregister <name>`: the user takes a name away, so that session receives no more
/// messages. The messages QUEUED for it become FAILED, and their senders are told, in the same step.
#[must_use]
pub fn unregister(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let [name] = args else {
        return Outcome::failed("usage: lunchroom unregister <name>", 2);
    };
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let Some(who) = mail.named(name) else {
        return Outcome::denied(&format!("no agent is named \"{name}\""), 2);
    };
    // What taking the name away does to the messages QUEUED for it, found by doing it to a copy.
    let fate = {
        let mut copy = mail.clone();
        copy.unregister(name);
        queued_fate(copy.fail_unnamed(env.now).len(), who)
    };
    let question = format!(
        "Lunchroom: take the name \"{name}\" from {who}, so it receives no more messages? {fate}\n"
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let taken = store.update(|m| m.unregister(name).then(|| m.fail_unnamed(env.now)));
    match taken {
        Ok(Some(failed)) => Outcome::ok(format!(
            "{name} is no longer named\n{}",
            failed_line(&failed)
        )),
        Ok(None) => Outcome::denied("the name was removed meanwhile; nothing was changed", 3),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// `lunchroom rate <messages> <seconds>`: the user sets the rate limit (R1.7).
#[must_use]
pub fn rate(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let number = |k: usize| args.get(k).and_then(|a| a.parse::<u64>().ok());
    let (Some(ceiling), Some(window)) = (number(0), number(1)) else {
        return Outcome::failed("usage: lunchroom rate <messages> <seconds>", 2);
    };
    let Ok(limits) = u32::try_from(ceiling)
        .map_err(|_| ())
        .and_then(|c| Limits::new(c, window).map_err(|_| ()))
    else {
        return Outcome::denied(
            "a rate limit must be at least 1 message in at least 1 second",
            2,
        );
    };
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let question =
        format!("Lunchroom: set the rate limit to {ceiling} messages in {window} seconds?\n");
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    match store.update(|m| m.set_limits(limits)) {
        Ok(()) => Outcome::ok(format!(
            "rate limit: {ceiling} messages in {window} seconds\n"
        )),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// `path` made absolute against `cwd` and lexically normal: `.` and `..` are resolved by name, no
/// link is followed.
fn lexical(path: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let joined = cwd.join(path);
    let mut out = std::path::PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The recorded spellings (T1.8: each spelling the guard resolved a word to) that are the path the
/// user typed: the same name once made absolute and normal, or the same file once links are
/// followed.
fn recorded_spellings(mail: &Mail, typed: &str, cwd: &std::path::Path) -> Vec<String> {
    let named = lexical(typed, cwd);
    let real = |p: &std::path::Path| std::fs::canonicalize(p).ok();
    let typed_real = real(&named);
    let mut found: Vec<String> = Vec::new();
    for marked in mail.marked() {
        for p in &marked.paths {
            let path = std::path::Path::new(p);
            let same = path == named
                || typed_real
                    .as_ref()
                    .is_some_and(|t| path == t || real(path).as_ref() == Some(t));
            if same && !found.contains(p) {
                found.push(p.clone());
            }
        }
    }
    found
}

/// `lunchroom clear <path>` (T1.8, `§5::45`, `§5::49`): a recorded path stays recorded
/// until the user clears it, here, on their passphrase in a terminal outside every guarded session
/// (A1.3). An agent's `clear` is DENIED, before anything is read, and the path stays recorded. A
/// path that is not recorded is said not to be, and nothing is asked or changed. Clearing takes the
/// path (every spelling of it) out of the record, so reading it no longer holds a session; a
/// session already HELD stays HELD until the user types in it.
#[must_use]
pub fn clear(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let [typed] = args else {
        return Outcome::failed("usage: lunchroom clear <path>", 2);
    };
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    // A1.3: an agent's `clear` stops here, whatever the path is.
    if let Err(reason) = check(&mail, sys) {
        return Outcome::denied(&reason, 3);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| env.home.clone());
    if recorded_spellings(&mail, typed, &cwd).is_empty() {
        return Outcome::failed(
            &format!(
                "{typed} is not a recorded path, so nothing was cleared: `lunchroom status` lists \
                 the recorded paths, and `lunchroom clear <path>` takes one of those"
            ),
            1,
        );
    }
    let question = format!(
        "Lunchroom: clear the recorded path {typed}? A session that reads it is then not HELD by \
         what held the agent that wrote it.\n"
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    // The spellings are found again inside the update, so a record that changed since is judged
    // as it is now.
    match store.update(|m| {
        let spellings = recorded_spellings(m, typed, &cwd);
        (m.clear_marked(&spellings), spellings.len())
    }) {
        Ok((0, _)) => Outcome::failed(
            &format!("{typed} is no longer a recorded path, so nothing was cleared"),
            1,
        ),
        Ok((_, spellings)) => Outcome::ok(format!(
            "cleared {typed} ({spellings} spelling{} of it): reading it no longer holds a \
             session. A session that is HELD stays HELD until you type in it.\n",
            if spellings == 1 { "" } else { "s" }
        )),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// `lunchroom passphrase`: the user sets the passphrase every command of this file asks for (A1.3).
/// With one already set, the current one is asked for first.
#[must_use]
pub fn passphrase(env: &Env, sys: &mut dyn System) -> Outcome {
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    if let Err(reason) = check(&mail, sys) {
        return Outcome::denied(&reason, 3);
    }
    set_passphrase(sys)
}

/// Sets the user's passphrase, on a terminal already checked: the current passphrase when there is
/// one, then the new one twice; the verifier is made, put in place as root's by the system, and
/// read back to show that it is root's and holds the passphrase just typed. The report names each
/// step done, whether the setting succeeds or stops.
#[must_use]
pub fn set_passphrase(sys: &mut dyn System) -> Outcome {
    let shown = verifier_shown(sys);
    // Nobody types a passphrase twice for a system that cannot put the check in place.
    if let Some(why) = sys.verifier_blocker() {
        return Outcome::failed(&format!("the passphrase was not set: {why}"), 1);
    }
    match trusted_verifier(sys) {
        Ok(current) => {
            let Some(typed) = sys.ask_secret("Current passphrase: ") else {
                return Outcome::denied(NO_SECRET_READ, 3);
            };
            if !current.matches(&typed) {
                return Outcome::denied("the current passphrase was wrong; nothing was changed", 3);
            }
        }
        Err(reason) if reason == NO_VERIFIER => {}
        // A check that cannot be trusted has no passphrase to ask for.
        Err(reason) => return Outcome::denied(&reason, 3),
    }
    let Some(first) = sys.ask_secret(&format!(
        "New passphrase (at least {} characters): ",
        passphrase::MIN_LENGTH
    )) else {
        return Outcome::denied(NO_SECRET_READ, 3);
    };
    if first.chars().count() < passphrase::MIN_LENGTH {
        return Outcome::denied(
            &format!(
                "a passphrase has at least {} characters; nothing was changed",
                passphrase::MIN_LENGTH
            ),
            2,
        );
    }
    let Some(second) = sys.ask_secret("New passphrase again: ") else {
        return Outcome::denied(NO_SECRET_READ, 3);
    };
    if first != second {
        return Outcome::denied("the two entries differ; nothing was changed", 2);
    }
    let salt = match sys.salt() {
        Ok(salt) => salt,
        Err(e) => return Outcome::failed(&format!("the passphrase was not set: {e}"), 1),
    };
    let line = Verifier::new(&first, salt, ITERATIONS).line();
    let mut report = format!("made the passphrase check ({ITERATIONS} rounds of PBKDF2)\n");
    if let Err(e) = sys.install_verifier(&line) {
        return Outcome {
            stdout: report,
            stderr: format!("the passphrase was not set: {e}\n"),
            code: 1,
        };
    }
    let _ = writeln!(
        report,
        "put it at {shown}, owned by root, group {group}, mode 0640: only Lunchroom's program, \
         which runs with group {group}, reads it",
        group = lunchroom_core::setgid::SYSTEM_USER
    );
    match trusted_verifier(sys) {
        Ok(back) if back.matches(&first) => {
            let _ = writeln!(
                report,
                "read it back: it is owned by root and holds the passphrase just typed"
            );
            let _ = writeln!(
                report,
                "the passphrase is set; `sudo rm {shown}` removes the check"
            );
            Outcome::ok(report)
        }
        read_back => {
            let why = match read_back {
                Ok(_) => "it does not hold the passphrase just typed".to_owned(),
                Err(reason) => reason,
            };
            Outcome {
                stdout: report,
                stderr: format!(
                    "the passphrase is not set: the check at {shown} was read back and {why}; \
                     `sudo rm {shown}` removes it\n"
                ),
                code: 1,
            }
        }
    }
}

/// Claude Code tools that only read; the slip as shipped holds every one of them (P1.1).
const READ_TOOLS: [&str; 5] = ["Read", "NotebookRead", "LS", "Grep", "Glob"];

/// `lunchroom slip` shows the permission slip every agent has (P1.1); `lunchroom slip add <agent>
/// <what>` is how the user would widen one, and Lunchroom DENIES every addition: the slip as shipped
/// already holds every read and the reply, and a call that changes a file, runs a command or is
/// PROTECTED is never inside a slip (P1.1, P1.4). Nothing is written.
#[must_use]
pub fn slip(args: &[String]) -> Outcome {
    match args.first().map(String::as_str) {
        None => Outcome::ok(format!("slip: {}\n", crate::cli::SLIP)),
        Some("add") => match args.get(2..).unwrap_or_default() {
            [tool] if READ_TOOLS.contains(&tool.as_str()) => Outcome::ok(format!(
                "slip: {tool} is in every slip already; nothing changed\n"
            )),
            [] => Outcome::failed("usage: lunchroom slip add <agent> <tool>", 2),
            _ => Outcome::denied(
                "a slip holds only calls that read and the reply; a call that changes a file, \
                 runs a command or is PROTECTED is never added",
                3,
            ),
        },
        Some(_) => Outcome::failed("usage: lunchroom slip [add <agent> <tool>]", 2),
    }
}

/// Harnesses whose adapter has passed its YOLO test (R2.2, H1.1): that a call the guard frees under
/// a `lunchroom yolo` runs in that harness, and that the guard asks again after the end time. All
/// four pass in 1.1, each by its test in `tests/yolo.rs` (`yolo_tested_names_every_harness` keeps
/// this list and the `Harness` enum together).
const YOLO_TESTED: [Harness; 4] = [
    Harness::ClaudeCode,
    Harness::Codex,
    Harness::OpenClaw,
    Harness::Hermes,
];

/// The names among `chosen` whose harness is not in `tested` (R2.2: `lunchroom yolo` refuses them).
fn untested<'a>(chosen: &'a [(String, Identity)], tested: &[Harness]) -> Vec<&'a str> {
    chosen
        .iter()
        .filter(|(_, w)| !tested.contains(&w.harness))
        .map(|(n, _)| n.as_str())
        .collect()
}

/// Days since the Unix epoch of a civil date (proleptic Gregorian), after H. Hinnant's
/// `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The end time `till` names, after `now`: `HH:MM` (UTC, the next time that comes) or
/// `YYYY-MM-DDTHH:MM[:SS]Z`.
fn parse_till(till: &str, now: u64) -> Option<u64> {
    // Digits only: `parse` alone takes a leading `+`, which is no part of a time.
    let num = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<i64>().ok())
            .flatten()
    };
    let clock = |s: &str| -> Option<i64> {
        let mut parts = s.split(':');
        let (h, m) = (num(parts.next()?)?, num(parts.next()?)?);
        let sec = parts.next().map_or(Some(0), num)?;
        ((0..24).contains(&h)
            && (0..60).contains(&m)
            && (0..60).contains(&sec)
            && parts.next().is_none())
        .then_some(h * 3600 + m * 60 + sec)
    };
    let end = if let Some((date, time)) = till.strip_suffix('Z').and_then(|t| t.split_once('T')) {
        let mut d = date.split('-');
        let (y, mo, da) = (num(d.next()?)?, num(d.next()?)?, num(d.next()?)?);
        // Years outside 1970 to 9999 are no end time, and keep the arithmetic below from
        // overflowing on a year typed as a huge number.
        if d.next().is_some() || !(1..=12).contains(&mo) || !(1970..=9999).contains(&y) {
            return None;
        }
        // A day the month has (`2026-02-31` is no date).
        let (ny, nm) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
        if !(1..=days_from_civil(ny, nm, 1) - days_from_civil(y, mo, 1)).contains(&da) {
            return None;
        }
        days_from_civil(y, mo, da) * 86_400 + clock(time)?
    } else {
        let at = clock(till)?;
        let today = i64::try_from(now / 86_400).ok()? * 86_400;
        let now = i64::try_from(now).ok()?;
        if today + at > now {
            today + at
        } else {
            today + 86_400 + at
        }
    };
    u64::try_from(end).ok().filter(|e| *e > now)
}

/// `lunchroom yolo <agents|all> till <time>`: the user turns on each named agent's harness YOLO
/// until that time (R2.2). It always has an end time, and refuses an agent whose harness adapter
/// has not passed its YOLO test.
#[must_use]
#[allow(clippy::too_many_lines)] // one command: arguments, the agents, the yes, the end, the lines
pub fn yolo(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let (Some(targets), Some(till)) = (
        args.first(),
        (args.get(1).map(String::as_str) == Some("till"))
            .then(|| args.get(2))
            .flatten(),
    ) else {
        return Outcome::failed(
            &format!(
                "usage: {}; separate the names with commas; it always has an end time",
                crate::YOLO_USAGE
            ),
            2,
        );
    };
    // `till now` ends a YOLO at once (R2.2 "set, extend or end"): the end time is this instant,
    // and a YOLO covers only while `now < end` (`Mail::yolo_until`).
    let ending = till == "now";
    let end = if ending {
        env.now
    } else if let Some(end) = parse_till(till, env.now) {
        end
    } else {
        return Outcome::denied(
            "the end time must be HH:MM (UTC) or YYYY-MM-DDTHH:MMZ, after now, or `now` to end it",
            2,
        );
    };
    let store = match store(env) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let mail = match read(&store) {
        Ok(m) => m,
        Err(o) => return o,
    };
    let chosen: Vec<(String, Identity)> = if targets == "all" {
        mail.names()
            .map(|(n, w)| (n.to_owned(), w.clone()))
            .collect()
    } else {
        let mut chosen = Vec::new();
        for name in targets.split(',').filter(|n| !n.is_empty()) {
            let Some(who) = mail.named(name) else {
                return Outcome::denied(&format!("no agent is named {name}"), 2);
            };
            chosen.push((name.to_owned(), who.clone()));
        }
        chosen
    };
    if chosen.is_empty() {
        return Outcome::denied("no agent is named", 2);
    }
    if let Some(refused) = refuse_untested(&chosen, targets == "all", &YOLO_TESTED) {
        return refused;
    }
    let names: Vec<&str> = chosen.iter().map(|(n, _)| n.as_str()).collect();
    let question = if ending {
        format!(
            "Lunchroom: end {} for {} now?\n",
            Word::Yolo.text(),
            names.join(", ")
        )
    } else {
        format!(
            "Lunchroom: turn {} on for {} until {} UTC ({})?\n",
            Word::Yolo.text(),
            names.join(", "),
            hhmm(end),
            crate::delivery::utc(end)
        )
    };
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    // §16 item 17: whether each agent's harness has a guard hook that can tell it (channel D) is
    // judged from the read above, before the store's lock is taken.
    let guarded: Vec<bool> = chosen
        .iter()
        .map(|(_, who)| {
            crate::evidence::evidence(&mail, who, &env.home)
                == lunchroom_core::mail::Evidence::Guarded
        })
        .collect();
    let at = env.now;
    match store.update(|m| {
        for (_, who) in &chosen {
            m.set_yolo(who, end);
        }
        // §16 item 17: the line is told to the user (the terminal where it was typed, and U),
        // to each agent by D when its harness has a hook and by N in its next envelope, and to
        // each sender with a message holding or QUEUED for a covered agent, by D, when the YOLO
        // is set after its message (the sender hears it by S in each later `send`).
        let names: Vec<&str> = chosen.iter().map(|(n, _)| n.as_str()).collect();
        let ids: Vec<Identity> = chosen.iter().map(|(_, w)| w.clone()).collect();
        let mut hearers: Vec<lunchroom_core::mail::calls::Hearer> = chosen
            .iter()
            .zip(&guarded)
            .map(|((_, who), g)| lunchroom_core::mail::calls::Hearer {
                who: who.clone(),
                d: *g,
                n: true,
            })
            .collect();
        for (_, who) in &chosen {
            for sender in m.senders_waiting_on(who) {
                if !ids.contains(&sender) {
                    hearers.push(lunchroom_core::mail::calls::Hearer {
                        who: sender,
                        d: true,
                        n: false,
                    });
                }
            }
        }
        m.tell_line(
            at,
            &cards::yolo_until(end),
            lunchroom_core::mail::lines::Who::Agents(&names),
            &ids,
            &[],
            &hearers,
            true,
        )
        .text
    }) {
        Ok(_) if ending => Outcome::ok(format!("{} ended.\n", Word::Yolo.text())),
        // R2.2: the line, at once, in the terminal where the user typed it.
        Ok(line) => Outcome::ok(format!("{line}\n")),
        Err(e) => Outcome::failed(&crate::cli::unwritten(e), 1),
    }
}

/// The refusal (R2.2, R2.7) when any agent in `chosen` has a harness not in `tested`: `yolo all`
/// and a list of names alike turn it on for every one named or for none, so the refusal says
/// which agents can be named. `None` when every harness has passed.
fn refuse_untested(
    chosen: &[(String, Identity)],
    all: bool,
    tested: &[Harness],
) -> Option<Outcome> {
    let untested = untested(chosen, tested);
    if untested.is_empty() {
        return None;
    }
    let passed: Vec<&str> = chosen
        .iter()
        .filter(|(_, w)| tested.contains(&w.harness))
        .map(|(n, _)| n.as_str())
        .collect();
    let can = if passed.is_empty() {
        "no agent can have it turned on yet".to_owned()
    } else {
        format!(
            "to turn it on for the others, name just them: lunchroom yolo {} till <HH:MM>",
            passed.join(",")
        )
    };
    Some(Outcome::denied(
        &format!(
            "{} cannot have {} turned on yet: Lunchroom has not passed its test of starting \
             and ending {} in that agent's program. {} turns it on for every agent named or \
             for none; {can}",
            untested.join(", "),
            Word::Yolo.text(),
            Word::Yolo.text(),
            if all { "`yolo all`" } else { "`yolo`" },
        ),
        3,
    ))
}

/// `HH:MM` of a time, in UTC.
fn hhmm(secs: u64) -> String {
    let day = secs % 86_400;
    format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
}

#[cfg(test)]
mod tests_u10;

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use lunchroom_core::approvals::Request;
    use lunchroom_core::vocab::Verdict;

    use super::*;
    use crate::terminal::fake;

    /// The user, in a terminal of their own, whose `answer` stands for the passphrase typed: a yes
    /// is the right one, anything else a wrong one; records each question and prompt shown.
    struct UserAt {
        answer: &'static str,
        asked: Vec<String>,
    }

    impl System for UserAt {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            vec![(Some("100:5".to_owned()), "bash".to_owned())]
        }
        fn own_tty(&self) -> u64 {
            34816
        }
        fn tty_of(&self, _key: &str) -> Option<u64> {
            None
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            Some(self.answer.to_owned())
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        // The user's answer in these tests stands for the passphrase: a yes is the right one, and
        // anything else a wrong one.
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.asked.push(prompt.to_owned());
            Some(fake::typed(self.answer))
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([3; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A scratch Lunchroom folder under the build folder, removed afterwards.
    struct Scratch(Env);

    impl Scratch {
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let root = crate::test_target()
                .join("user-tests")
                .join(format!("{}-{n}", std::process::id()));
            Self(Env {
                home: root.join("home"),
                lunchroom_home: root.join("lunchroom"),
                codex_home: root.join("home/.codex"),
                claude_home: root.join("home/.claude"),
                hermes_home: root.join("home/.hermes"),
                claude_pid: None,
                attended: None,
                entrypoint: None,
                claude_session: None,
                codex_thread: None,
                hermes_session: None,
                lunchroom_session: None,
                now: 1000,
            })
        }

        fn store(&self) -> Option<Store<Mail>> {
            Store::open(&self.0.store_dir()).ok()
        }
    }

    // P1.6: the user sets the hold mode in their own terminal with the passphrase; with no mode named
    // the command shows the mode and the question; an answer to the install question names a mode
    // by number or name, and none leaves Balanced.
    #[test]
    fn the_user_sets_the_hold_mode() {
        let s = Scratch::new();
        let store = s.store();
        assert!(store.is_some());
        let Some(store) = store else { return };
        let mut user = UserAt {
            answer: "yes",
            asked: Vec::new(),
        };
        let shown = mode(&[], &s.0, &mut user);
        assert!(
            shown.stdout.starts_with("hold mode: Balanced\n"),
            "{}",
            shown.stdout
        );
        assert!(shown.stdout.contains("2. Balanced (recommended)"));
        assert!(
            shown
                .stdout
                .contains("Every tool-call guard has this limit")
        );
        let set = mode(&["relaxed".to_owned()], &s.0, &mut user);
        assert_eq!(set.code, 0, "{}", set.stderr);
        assert_eq!(
            store.read().ok().map(|m| m.hold_mode()),
            Some(HoldMode::Relaxed)
        );
        let mut no = UserAt {
            answer: "no",
            asked: Vec::new(),
        };
        assert_eq!(mode(&["strict".to_owned()], &s.0, &mut no).code, 3);
        assert_eq!(
            store.read().ok().map(|m| m.hold_mode()),
            Some(HoldMode::Relaxed)
        );
        assert_eq!(mode(&["loose".to_owned()], &s.0, &mut user).code, 2);
        assert_eq!(mode_answer(""), Some(HoldMode::Balanced));
        assert_eq!(mode_answer("1"), Some(HoldMode::Relaxed));
        assert_eq!(mode_answer(" 3 "), Some(HoldMode::Strict));
        assert_eq!(mode_answer("strict"), Some(HoldMode::Strict));
        assert_eq!(mode_answer("yes"), None);
    }

    // `lunchroom repair` puts back the previous state only for the user in a terminal outside every
    // session the previous state records, and only when the store cannot be read.
    #[test]
    fn repair_puts_back_the_previous_state_for_the_user_only() {
        let s = Scratch::new();
        let store = s.store();
        assert!(store.is_some());
        let Some(store) = store else { return };
        let who = Identity::new(Harness::ClaudeCode, "c1");
        let mut user = UserAt {
            answer: "yes",
            asked: Vec::new(),
        };
        assert!(store.update(|m| m.observe(&who)).is_ok());
        assert!(
            store
                .update(|m| m.observe_process("100:5", Path::new("/w"), &who))
                .is_ok()
        );
        assert!(
            store
                .update(|m| m.observe(&Identity::new(Harness::Codex, "x1")))
                .is_ok()
        );
        assert_eq!(repair(&s.0, &mut user).code, 0);
        assert_eq!(user.asked, Vec::<String>::new());
        let state = s.0.store_dir().join("state.json");
        assert!(std::fs::write(&state, "{ broken").is_ok());
        // The terminal runs under a process the previous state records as a guarded session.
        let out = repair(&s.0, &mut user);
        assert_eq!(out.code, 3, "{}", out.stderr);
        assert!(store.read().is_err());
        // Without that record, the user's yes puts the previous state back.
        let fresh = Scratch::new();
        let other = fresh.store();
        assert!(other.is_some());
        let Some(other) = other else { return };
        assert!(other.update(|m| m.observe(&who)).is_ok());
        assert!(
            other
                .update(|m| m.observe(&Identity::new(Harness::Codex, "x1")))
                .is_ok()
        );
        let state = fresh.0.store_dir().join("state.json");
        assert!(std::fs::write(&state, "{ broken").is_ok());
        let out = repair(&fresh.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(other.read().is_ok());
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Some(root) = self.0.lunchroom_home.parent() {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    fn waiting(s: &Scratch) -> Identity {
        let me = Identity::new(Harness::ClaudeCode, "s1");
        let request = Request {
            session: me.clone(),
            agent: "Claude".to_owned(),
            tool: "Bash".to_owned(),
            input: json!({"command": "git push"}),
            cwd: "/work/a".to_owned(),
            rule: "PROTECTED (push or publish)".to_owned(),
            at: 1000,
        };
        assert!(
            s.store()
                .and_then(|st| st.update(|m| m.request(request)).ok())
                .is_some()
        );
        me
    }

    // §8::2: the list `lunchroom approve` prints shows the agent, session and tool with
    // control characters written out, never raw, so one waiting call stays one line.
    #[test]
    fn approve_list_shows_agent_session_and_tool_escaped() {
        let s = Scratch::new();
        let request = Request {
            session: Identity::new(Harness::ClaudeCode, "s\u{1b}[2K1"),
            agent: "Cl\naude\u{1b}[1A".to_owned(),
            tool: "Ba\rsh\u{85}".to_owned(),
            input: json!({"command": "git push"}),
            cwd: "/work/a".to_owned(),
            rule: "PROTECTED (push or publish)".to_owned(),
            at: 1000,
        };
        assert!(
            s.store()
                .and_then(|st| st.update(|m| m.request(request)).ok())
                .is_some()
        );
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = approve(&[], &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.stdout.lines().count(), 1, "{:?}", out.stdout);
        assert!(out.stdout.ends_with('\n'));
        let body = out.stdout.trim_end_matches('\n');
        assert!(
            !body.chars().any(char::is_control),
            "a raw control character: {body:?}"
        );
        for part in ["Cl\\naude\\u{001b}[1A", "Ba\\rsh\\u{0085}", "\\u{001b}[2K"] {
            assert!(body.contains(part), "{part}: {body:?}");
        }
        assert_eq!(user.asked, Vec::<String>::new());
    }

    // §7::7, §7::12, §7::13: in a terminal of the user's own, `lunchroom approve 1` asks a
    // question that starts "Lunchroom:", names the rule and shows the call; on yes the exact call is
    // approved once.
    #[test]
    fn approve_in_users_terminal_approves_that_call() {
        let s = Scratch::new();
        let me = waiting(&s);
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = approve(&["1".to_owned()], &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        // Card 4's line, with the agent and call of this event.
        assert!(
            out.stdout.starts_with("Granted. Claude can push.\n"),
            "{}",
            out.stdout
        );
        let question = user.asked.first().cloned().unwrap_or_default();
        assert!(
            question.starts_with("Lunchroom: this call is outside PROTECTED (push or publish)."),
            "{question}"
        );
        assert!(
            question.contains("The call's input:\n{\"command\":\"git push\"}"),
            "{question}"
        );
        let verdicts: Vec<Verdict> = (0..2)
            .filter_map(|_| {
                s.store().and_then(|st| {
                    st.update(|m| {
                        m.use_approval(
                            &me,
                            "Bash",
                            &json!({"command": "git push"}),
                            "/work/a",
                            1001,
                        )
                    })
                    .ok()
                })
            })
            .collect();
        assert_eq!(verdicts, [Verdict::Granted, Verdict::Denied]);
        // A2.1/A2.2: the folder is shown, and the approval covers only that folder.
        assert!(question.contains("Runs in: /work/a\n"), "{question}");
        let elsewhere = s.store().and_then(|st| {
            st.update(|m| {
                m.approve(
                    &me,
                    "Bash",
                    &json!({"command": "git push"}),
                    "/work/a",
                    1002,
                );
                m.use_approval(
                    &me,
                    "Bash",
                    &json!({"command": "git push"}),
                    "/work/b",
                    1003,
                )
            })
            .ok()
        });
        assert_eq!(elsewhere, Some(Verdict::Denied));
    }

    /// The user of [`UserAt`], while whose passphrase is being typed another agent's message
    /// arrives and holds the session.
    struct Interrupted {
        user: UserAt,
        env: Env,
        who: Identity,
    }

    impl System for Interrupted {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            self.user.ancestors()
        }
        fn own_tty(&self) -> u64 {
            self.user.own_tty()
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            self.user.tty_of(key)
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.user.ask(question)
        }
        fn uid(&self) -> Option<u32> {
            self.user.uid()
        }
        fn verifier(&self) -> Result<String, String> {
            self.user.verifier()
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            if let Ok(store) = Store::<Mail>::open(&self.env.store_dir()) {
                let _ = store.update(|m| m.hold_unrecorded(&self.who, "codex", "late-1"));
            }
            self.user.ask_secret(prompt)
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            self.user.salt()
        }
        fn install_verifier(&mut self, line: &str) -> Result<(), String> {
            self.user.install_verifier(line)
        }
    }

    // §8::1, §8::2 — the user read a prompt that showed no holding message; a message
    // that holds the session before the answer is applied was never shown, so nothing is approved
    // (not even for the same call), the request stays waiting, and the user is told to run
    // approve again, which then shows the new message and approves it.
    #[test]
    fn message_arriving_while_user_confirms_is_not_covered() {
        let s = Scratch::new();
        let me = waiting(&s);
        let mut racing = Interrupted {
            user: UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            },
            env: s.0.clone(),
            who: me.clone(),
        };
        let out = approve(&["1".to_owned()], &s.0, &mut racing);
        assert_eq!(out.code, 3, "{}", out.stdout);
        assert!(
            out.stderr.contains("nothing was approved"),
            "{}",
            out.stderr
        );
        let call = json!({"command": "git push"});
        let state = s.store().and_then(|st| {
            st.update(|m| {
                (
                    m.requests().len(),
                    // Neither the held set at confirmation time (none) nor the current one is covered.
                    m.use_approval(&me, "Bash", &call, "/work/a", 1001),
                )
            })
            .ok()
        });
        assert_eq!(state, Some((1, Verdict::Denied)));
        // Run again: the prompt now names the message, and the approval covers it.
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        assert_eq!(approve(&["1".to_owned()], &s.0, &mut user).code, 0);
        let question = user.asked.first().cloned().unwrap_or_default();
        assert!(question.contains("codex (codex:late-1)"), "{question}");
        let granted = s.store().and_then(|st| {
            st.update(|m| m.use_approval(&me, "Bash", &call, "/work/a", 1001))
                .ok()
        });
        assert_eq!(granted, Some(Verdict::Granted));
    }

    // §7::12: any answer but yes approves nothing.
    #[test]
    fn approve_without_yes_approves_nothing() {
        let s = Scratch::new();
        let me = waiting(&s);
        let mut user = UserAt {
            answer: "y\n",
            asked: Vec::new(),
        };
        assert_eq!(approve(&["1".to_owned()], &s.0, &mut user).code, 3);
        let verdict = s.store().and_then(|st| {
            st.update(|m| {
                m.use_approval(
                    &me,
                    "Bash",
                    &json!({"command": "git push"}),
                    "/work/a",
                    1001,
                )
            })
            .ok()
        });
        assert_eq!(verdict, Some(Verdict::Denied));
    }

    // §9::8 and §2::11: a taken name moves only on the second yes; the rate limit is set on yes.
    #[test]
    fn register_takes_the_earlier_forms_too() {
        let s = Scratch::new();
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        assert_eq!(
            register(
                &args(&["Builder", "--codex-thread", "t-9"]),
                &s.0,
                &mut user
            )
            .code,
            0
        );
        assert_eq!(
            register(
                &args(&["Planner", "--claude-session", "s-9"]),
                &s.0,
                &mut user
            )
            .code,
            0
        );
        assert_eq!(
            register(&args(&["Nobody", "--codex-thread"]), &s.0, &mut user).code,
            2
        );
        let named = s
            .store()
            .and_then(|st| st.read().ok())
            .map(|m| (m.named("Builder").cloned(), m.named("Planner").cloned()));
        assert_eq!(
            named,
            Some((
                Identity::parse("codex:t-9"),
                Identity::parse("claude-code:s-9")
            ))
        );
    }

    #[test]
    fn register_and_rate_on_the_users_yes() {
        let s = Scratch::new();
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        assert_eq!(
            register(&args(&["codex", "codex:x1"]), &s.0, &mut user).code,
            0
        );
        assert_eq!(
            register(&args(&["codex", "codex:x2"]), &s.0, &mut user).code,
            0
        );
        assert_eq!(user.asked.len(), 3);
        assert!(
            user.asked[2].contains("is held by codex:x1"),
            "{}",
            user.asked[2]
        );
        let named = s
            .store()
            .and_then(|st| st.read().ok())
            .and_then(|m| m.named("codex").cloned());
        assert_eq!(named, Identity::parse("codex:x2"));
        assert_eq!(rate(&args(&["0", "60"]), &s.0, &mut user).code, 2);
        assert_eq!(rate(&args(&["5", "60"]), &s.0, &mut user).code, 0);
        let limits = s.store().and_then(|st| st.read().ok()).map(|m| m.limits());
        assert_eq!(limits, Limits::new(5, 60).ok());
    }

    /// A terminal under a harness process: an agent's command, which `confirm` refuses.
    struct FromAgent;

    impl System for FromAgent {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            vec![(None, "claude".to_owned())]
        }
        fn own_tty(&self) -> u64 {
            34816
        }
        fn tty_of(&self, _key: &str) -> Option<u64> {
            None
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            Some("yes\n".to_owned())
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            Some(fake::PASSPHRASE.to_owned())
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([3; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    // `lunchroom unregister` takes the name away on the user's yes; it changes nothing without the
    // yes, from an agent, for an unknown name or with the wrong arguments.
    #[test]
    fn unregister_on_the_users_yes_only() {
        let s = Scratch::new();
        named(&s, "codex", "codex:x1");
        let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        let holds_name = || {
            s.store()
                .and_then(|st| st.read().ok())
                .is_some_and(|m| m.named("codex").is_some())
        };
        let mut no = UserAt {
            answer: "y\n",
            asked: Vec::new(),
        };
        assert_eq!(unregister(&args(&["codex"]), &s.0, &mut no).code, 3);
        assert_eq!(unregister(&args(&["codex"]), &s.0, &mut FromAgent).code, 3);
        assert_eq!(unregister(&args(&["nobody"]), &s.0, &mut no).code, 2);
        assert_eq!(unregister(&args(&[]), &s.0, &mut no).code, 2);
        assert!(holds_name());
        let mut yes = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = unregister(&args(&["codex"]), &s.0, &mut yes);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(!holds_name());
        assert_eq!(unregister(&args(&["codex"]), &s.0, &mut yes).code, 2);
    }

    // R2.2 end times: HH:MM is the next such time in UTC; a dated time must be after now.
    #[test]
    fn till_times() {
        let noon = 12 * 3600;
        assert_eq!(parse_till("14:00", noon), Some(14 * 3600));
        assert_eq!(parse_till("11:00", noon), Some(86_400 + 11 * 3600));
        assert_eq!(parse_till("12:00", noon), Some(86_400 + noon));
        assert_eq!(parse_till("2026-10-26T14:00Z", noon), Some(1_793_023_200));
        assert_eq!(
            parse_till("2026-10-26T14:00:30Z", noon),
            Some(1_793_023_230)
        );
        assert_eq!(parse_till("1970-01-01T11:00Z", noon), None);
        assert!(parse_till("2028-02-29T00:00Z", noon).is_some());
        assert!(parse_till("2026-12-31T00:00Z", noon).is_some());
        assert!(parse_till("9999-12-31T23:59Z", noon).is_some());
        for bad in [
            "",
            "14",
            "24:00",
            "14:60",
            "2026-13-01T00:00Z",
            "2026-02-29T00:00Z",
            "2026-04-31T00:00Z",
            "2026-10-00T00:00Z",
            "2026-10-26T14:00",
            "soon",
            // Years that would overflow the day count, or that no clock reaches.
            "9223372036854775807-12-01T00:00Z",
            "-9223372036854775808-01-01T00:00Z",
            "99999999999999999-01-01T00:00Z",
            "10000-01-01T00:00Z",
            "0000-01-01T00:00Z",
            "1969-12-31T23:59Z",
        ] {
            assert_eq!(parse_till(bad, noon), None, "{bad}");
        }
    }

    fn named(s: &Scratch, name: &str, who: &str) {
        let who = Identity::parse(who).unwrap_or_else(|| Identity::new(Harness::ClaudeCode, "x"));
        assert!(
            s.store()
                .and_then(|st| st
                    .update(|m| m.register(name, &who, Origin::User, None))
                    .ok())
                .is_some()
        );
    }

    // §3::1, §3::4 (command part): on the user's yes, `lunchroom yolo` covers each named
    // agent until the end time and prints card 5's line; it covers until then and not after.
    #[test]
    fn yolo_on_the_users_yes_until_its_end() {
        let s = Scratch::new();
        named(&s, "deepseek", "claude-code:d1");
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        let out = yolo(&args(&["deepseek", "till", "14:00"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        // §16 item 1 and item 17: the line in the form `<HH:MM> UTC Lunchroom: <words>
        // (<agents>)`, its time the time it was typed, in the terminal where it was typed.
        assert!(
            out.stdout
                .ends_with(" UTC Lunchroom: YOLO authorized until 14:00 UTC. (deepseek)\n"),
            "{}",
            out.stdout
        );
        assert_eq!(out.stdout.as_bytes()[2], b':', "{}", out.stdout);
        let d1 = Identity::new(Harness::ClaudeCode, "d1");
        let mail = s.store().and_then(|st| st.read().ok());
        assert_eq!(
            mail.as_ref().and_then(|m| m.yolo_until(&d1, 1000)),
            Some(14 * 3600)
        );
        assert_eq!(
            mail.as_ref().and_then(|m| m.yolo_until(&d1, 14 * 3600)),
            None
        );
    }

    // §3::2, §3::3: no end time is refused; an agent whose harness adapter has not passed
    // its YOLO test is refused; a name that does not exist is refused; nothing is asked or set.
    #[test]
    fn yolo_refusals() {
        let s = Scratch::new();
        named(&s, "deepseek", "claude-code:d1");
        named(&s, "codex", "codex:x1");
        named(&s, "claw", "openclaw:agent:main:a");
        named(&s, "hermes", "hermes:h1");
        let args = |a: &[&str]| a.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        for (a, code) in [
            (args(&["deepseek"]), 2),
            (args(&["deepseek", "till"]), 2),
            (args(&["deepseek", "for", "14:00"]), 2),
            (args(&["codex", "till"]), 2),
            (args(&["claw", "for", "14:00"]), 2),
            (args(&["hermes"]), 2),
            (args(&["nobody", "till", "14:00"]), 2),
            (args(&["codex,nobody", "till", "14:00"]), 2),
        ] {
            let mut user = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            assert_eq!(yolo(&a, &s.0, &mut user).code, code, "{a:?}");
            assert!(user.asked.is_empty(), "{a:?}");
        }
        let d1 = Identity::new(Harness::ClaudeCode, "d1");
        let set = s
            .store()
            .and_then(|st| st.read().ok())
            .and_then(|m| m.yolo_until(&d1, 1000));
        assert_eq!(set, None);
    }

    // §3::2: `yolo all` is all or nothing; its refusal says so, names the agents whose
    // harness has passed, and the command that turns it on for them.
    #[test]
    fn yolo_all_refusal_says_which_agents_can_be_named() {
        // All four harnesses have passed, so no real agent reaches the refusal; it is proven with
        // a list of tested harnesses that leaves Codex out (R2.7, C6-A07).
        let chosen = |agents: &[(&str, &str)]| -> Vec<(String, Identity)> {
            agents
                .iter()
                .filter_map(|(n, w)| Identity::parse(w).map(|i| ((*n).to_owned(), i)))
                .collect()
        };
        let both = chosen(&[("deepseek", "claude-code:d1"), ("codex", "codex:x1")]);
        let out = refuse_untested(&both, true, &[Harness::ClaudeCode]);
        assert!(
            out.is_some(),
            "a harness outside the tested list is refused"
        );
        let Some(out) = out else { return };
        assert_eq!(out.code, 3, "{}", out.stderr);
        // G2/G3-14: plain words, the one way of writing the command, and what to do.
        for part in [
            "codex cannot have YOLO turned on yet: Lunchroom has not passed its test of starting \
             and ending YOLO in that agent's program.",
            "`yolo all` turns it on for every agent named or for none; to turn it on for the \
             others, name just them: lunchroom yolo deepseek till <HH:MM>",
        ] {
            assert!(out.stderr.contains(part), "{part}: {}", out.stderr);
        }
        assert!(
            !out.stderr.contains("Claude Code agents only"),
            "{}",
            out.stderr
        );
        // A list of names says `yolo`, and none tested says so.
        let out = refuse_untested(&both[1..], false, &[Harness::ClaudeCode]);
        assert!(out.is_some(), "refused");
        let Some(out) = out else { return };
        assert!(out.stderr.contains("`yolo` turns it on for every agent named or for none; no agent can have it turned on yet"), "{}", out.stderr);
        // Every harness in the list: nothing is refused, whatever is named.
        assert!(refuse_untested(&both, true, &YOLO_TESTED).is_none());
        assert!(refuse_untested(&[], true, &[]).is_none());
        for harness in [
            Harness::ClaudeCode,
            Harness::Codex,
            Harness::OpenClaw,
            Harness::Hermes,
        ] {
            let one = [("a".to_owned(), Identity::new(harness, "i"))];
            assert!(
                refuse_untested(&one, false, &[harness]).is_none(),
                "{harness:?}"
            );
            assert!(refuse_untested(&one, false, &[]).is_some(), "{harness:?}");
        }
    }

    // §3::2: `YOLO_TESTED` and the `Harness` enum stay together: a harness added to
    // the enum fails to compile here until it is placed in the tested list or deliberately left out.
    #[test]
    fn yolo_tested_names_every_harness() {
        for harness in [
            Harness::ClaudeCode,
            Harness::Codex,
            Harness::OpenClaw,
            Harness::Hermes,
        ] {
            // Exhaustive: a new variant is a compile error in this match.
            match harness {
                Harness::ClaudeCode | Harness::Codex | Harness::OpenClaw | Harness::Hermes => {}
            }
            assert!(YOLO_TESTED.contains(&harness), "{harness:?}");
        }
        assert_eq!(YOLO_TESTED.len(), 4);
    }

    /// Four agents, one of each harness, and the key forms their harnesses give.
    fn four_agents(s: &Scratch) {
        named(s, "claude", "claude-code:c1");
        named(s, "codex", "codex:x1");
        named(s, "claw", "openclaw:agent:main:a");
        named(s, "hermes", "hermes:h1");
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| (*x).to_owned()).collect()
    }

    fn end_of(s: &Scratch, who: &Identity, at: u64) -> Option<u64> {
        s.store()
            .and_then(|st| st.read().ok())
            .and_then(|m| m.yolo_until(who, at))
    }

    fn ids() -> [(&'static str, Identity); 4] {
        [
            ("claude", Identity::new(Harness::ClaudeCode, "c1")),
            ("codex", Identity::new(Harness::Codex, "x1")),
            ("claw", Identity::new(Harness::OpenClaw, "agent:main:a")),
            ("hermes", Identity::new(Harness::Hermes, "h1")),
        ]
    }

    // §3::2 (C6-A01..A04, A11): `lunchroom yolo` is accepted for an agent of each
    // harness, prints card 5's line, sets exactly that agent's end time, and writes nothing
    // without the user's yes.
    #[test]
    fn yolo_is_set_for_each_harness() {
        for (name, who) in ids() {
            let s = Scratch::new();
            four_agents(&s);
            let mut no = UserAt {
                answer: "no\n",
                asked: Vec::new(),
            };
            let out = yolo(&argv(&[name, "till", "14:00"]), &s.0, &mut no);
            assert_eq!(out.code, 3, "{name}: {}", out.stderr);
            assert_eq!(
                end_of(&s, &who, 1000),
                None,
                "{name}: nothing without a yes"
            );
            let mut user = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            let out = yolo(&argv(&[name, "till", "14:00"]), &s.0, &mut user);
            assert_eq!(out.code, 0, "{name}: {}", out.stderr);
            // §16 item 1 and item 17: the line in the form `<HH:MM> UTC Lunchroom: <words> (<agents>)`.
            let line = format!(" UTC Lunchroom: YOLO authorized until 14:00 UTC. ({name})\n");
            assert!(out.stdout.ends_with(&line), "{name}: {}", out.stdout);
            assert_eq!(user.asked.len(), 1, "{name}");
            for (other, id) in ids() {
                let want = (other == name).then_some(14 * 3600);
                assert_eq!(end_of(&s, &id, 1000), want, "{name} set, {other} read");
            }
        }
    }

    // §3::2 (C6-A05, A06, A14): `yolo all` covers all four; a list covers exactly the
    // named agents, an empty element and a repeated name are harmless, and an empty list is refused.
    #[test]
    fn yolo_all_and_lists_cover_exactly_the_named() {
        let s = Scratch::new();
        four_agents(&s);
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = yolo(&argv(&["all", "till", "14:00"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        for (n, id) in ids() {
            assert_eq!(end_of(&s, &id, 1000), Some(14 * 3600), "{n}");
        }
        let s = Scratch::new();
        four_agents(&s);
        named(&s, "codex2", "codex:x2");
        let out = yolo(
            &argv(&["codex,,hermes,codex", "till", "15:00"]),
            &s.0,
            &mut user,
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
        let x2 = Identity::new(Harness::Codex, "x2");
        assert_eq!(
            end_of(&s, &x2, 1000),
            None,
            "an unnamed agent of the same harness"
        );
        assert_eq!(end_of(&s, &ids()[1].1, 1000), Some(15 * 3600));
        assert_eq!(end_of(&s, &ids()[3].1, 1000), Some(15 * 3600));
        assert_eq!(end_of(&s, &ids()[0].1, 1000), None);
        assert_eq!(end_of(&s, &ids()[2].1, 1000), None);
        for list in [",", ",,", ""] {
            let out = yolo(&argv(&[list, "till", "15:00"]), &s.0, &mut user);
            assert_eq!(out.code, 2, "{list:?}: {}", out.stderr);
        }
    }

    /// A user (or something posing as one) at a terminal, typing `secret` for the passphrase; with
    /// `tty` 0 there is no terminal, and `secret` `None` is an end of input.
    struct Typist {
        tty: u64,
        secret: Option<String>,
        asked: usize,
    }

    impl System for Typist {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            vec![(Some("100:5".to_owned()), "bash".to_owned())]
        }
        fn own_tty(&self) -> u64 {
            self.tty
        }
        fn tty_of(&self, _key: &str) -> Option<u64> {
            None
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            self.asked += 1;
            self.secret.clone()
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            self.asked += 1;
            self.secret.clone()
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([3; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    // §3::2, §7 A1.3 (C6-A11, A15, A16, S10): nothing is set, and no line of
    // authorization is printed, when there is no terminal, when input ends, and when what is typed
    // is not the passphrase: a refusal word, an empty line, a JSON-shaped value, a localized yes.
    #[test]
    fn yolo_writes_nothing_without_the_users_passphrase() {
        for (name, who) in ids() {
            let s = Scratch::new();
            four_agents(&s);
            let none: Vec<(&str, u64, Option<&str>)> = vec![
                ("no terminal", 0, Some(fake::PASSPHRASE)),
                ("end of input", 34816, None),
                ("no", 34816, Some("no")),
                ("n", 34816, Some("n")),
                ("empty", 34816, Some("")),
                ("null", 34816, Some("null")),
                ("true", 34816, Some("true")),
                ("zero", 34816, Some("0")),
                ("object", 34816, Some("{}")),
                ("array", 34816, Some("[]")),
                ("turkish yes", 34816, Some("evet")),
                ("japanese yes", 34816, Some("はい")),
                ("arabic yes", 34816, Some("نعم")),
                ("french yes", 34816, Some("oui")),
                ("yes, not the passphrase", 34816, Some("yes")),
                (
                    "passphrase with a trailing letter",
                    34816,
                    Some("correct horse batterys"),
                ),
            ];
            for (what, tty, secret) in none {
                let mut user = Typist {
                    tty,
                    secret: secret.map(str::to_owned),
                    asked: 0,
                };
                let out = yolo(&argv(&[name, "till", "14:00"]), &s.0, &mut user);
                assert_ne!(out.code, 0, "{name} {what}: {}", out.stdout);
                assert!(
                    !out.stdout.contains("authorized"),
                    "{name} {what}: {}",
                    out.stdout
                );
                assert_eq!(end_of(&s, &who, 1000), None, "{name} {what}");
            }
            // The passphrase itself: set, then ended with the same proof.
            let mut user = Typist {
                tty: 34816,
                secret: Some(fake::PASSPHRASE.to_owned()),
                asked: 0,
            };
            let out = yolo(&argv(&[name, "till", "14:00"]), &s.0, &mut user);
            assert_eq!(out.code, 0, "{name}: {}", out.stderr);
            assert_eq!(end_of(&s, &who, 1000), Some(14 * 3600));
        }
    }

    // §3::2 (C6-S03, A17): a store that cannot be written (the folder is read-only)
    // makes the command fail with code 1 and no line of authorization; nothing covers the agent.
    #[test]
    fn yolo_that_cannot_be_written_is_not_announced() {
        use std::os::unix::fs::PermissionsExt;
        for (name, who) in ids() {
            let s = Scratch::new();
            four_agents(&s);
            let dir = s.0.store_dir();
            assert!(std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).is_ok());
            let mut user = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            let out = yolo(&argv(&[name, "till", "14:00"]), &s.0, &mut user);
            let restored = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            assert!(restored.is_ok());
            assert_ne!(out.code, 0, "{name}: {}", out.stdout);
            assert!(!out.stdout.contains("authorized"), "{name}: {}", out.stdout);
            assert_eq!(end_of(&s, &who, 1000), None, "{name}");
        }
    }

    // §3::2, §3::7 (C6-A12, A13): the `yolo` command line with names of more than one
    // kind in the same list: an unknown name among known ones refuses the whole command.
    #[test]
    fn yolo_refuses_the_whole_list_for_one_unknown_name() {
        let s = Scratch::new();
        four_agents(&s);
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = yolo(
            &argv(&["codex,hermes,ghost", "till", "14:00"]),
            &s.0,
            &mut user,
        );
        assert_eq!(out.code, 2, "{}", out.stderr);
        for (n, id) in ids() {
            assert_eq!(end_of(&s, &id, 1000), None, "{n}");
        }
    }

    // §3::2 (C6-D06, D07, D09, I01..I03, I05): the end time is extended or shortened by a
    // later command, per agent; `till now` ends it at once with the user's yes, for one agent or
    // all, and only exactly `now` ends.
    #[test]
    fn yolo_extend_shorten_and_end() {
        for (name, who) in ids() {
            let s = Scratch::new();
            four_agents(&s);
            let mut user = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            // The agent after this one in the list: another agent, and its name.
            let next = ids()
                .into_iter()
                .cycle()
                .skip_while(|(n, _)| *n != name)
                .nth(1);
            assert!(next.is_some());
            let Some((other_name, other)) = next else {
                return;
            };
            for (n, till) in [
                (name, "14:00"),
                (other_name, "20:00"),
                (name, "16:00"),
                (name, "13:00"),
            ] {
                let out = yolo(&argv(&[n, "till", till]), &s.0, &mut user);
                assert_eq!(out.code, 0, "{name} {n} {till}: {}", out.stderr);
            }
            // Last writer's value: shortened to 13:00; the other agent's 20:00 is untouched.
            assert_eq!(end_of(&s, &who, 1000), Some(13 * 3600), "{name}");
            assert_eq!(end_of(&s, &other, 1000), Some(20 * 3600), "{name}");
            // Ends at once: the very next read is off, the other agent's is not.
            let mut asked = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            let out = yolo(&argv(&[name, "till", "now"]), &s.0, &mut asked);
            assert_eq!(
                (out.code, out.stdout.as_str()),
                (0, "YOLO ended.\n"),
                "{name}: {}",
                out.stderr
            );
            assert_eq!(asked.asked.len(), 1, "{name}: the user is asked first");
            assert_eq!(end_of(&s, &who, 1000), None, "{name}");
            assert_eq!(end_of(&s, &other, 1000), Some(20 * 3600), "{name}");
            // Not confirmed: nothing changes.
            let mut no = UserAt {
                answer: "no\n",
                asked: Vec::new(),
            };
            let out = yolo(&argv(&[other_name, "till", "now"]), &s.0, &mut no);
            assert_eq!(out.code, 3, "{}", out.stderr);
            assert_eq!(end_of(&s, &other, 1000), Some(20 * 3600), "{name}");
        }
        // For all, and with none set; other spellings are not `now`.
        let s = Scratch::new();
        four_agents(&s);
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = yolo(&argv(&["all", "till", "now"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        for (n, id) in ids() {
            assert_eq!(end_of(&s, &id, 1000), None, "{n}");
        }
        for spelling in ["NOW", "now ", " now", "\"now\"", "nowish", "n"] {
            let out = yolo(&argv(&["codex", "till", spelling]), &s.0, &mut user);
            assert_eq!(out.code, 2, "{spelling:?}: {}", out.stderr);
        }
    }

    // §3::2 (C6-B01..B05, K04): the end time must be after now, whatever its spelling
    // and whichever harness; nothing is set, nothing is asked, and nothing panics.
    #[test]
    fn yolo_hostile_times_and_sizes() {
        let mut s = Scratch::new();
        four_agents(&s);
        s.0.now = 14 * 3600; // 14:00:00 UTC on 1970-01-01
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let huge = "9".repeat(4096);
        let long_name = "n".repeat(4096);
        let many = vec!["codex"; 10_000].join(",");
        for (till, code) in [
            ("1970-01-01T14:00:00Z", 2),
            ("1970-01-01T13:59:59Z", 2),
            ("1970-01-01T14:00Z", 2),
            ("1969-12-31T23:59Z", 2),
            ("24:00", 2),
            ("23:60", 2),
            ("-1:00", 2),
            ("1:2:3:4", 2),
            ("14:00:60", 2),
            ("99:99", 2),
            ("", 2),
            (" ", 2),
            ("+14:00", 2),
            ("١٤:٠٠", 2),
            ("14:00Z", 2),
            ("2026-02-31T10:00Z", 2),
            ("2026-13-01T10:00Z", 2),
            ("0000-01-01T10:00Z", 2),
            ("10000-01-01T10:00Z", 2),
            ("99999999999999999999-01-01T10:00Z", 2),
            ("2026-1-1T10:00z", 2),
            ("T10:00Z", 2),
            ("2026-01-01T10:00+02:00", 2),
            (huge.as_str(), 2),
        ] {
            for who in ["claude", "codex", "claw", "hermes"] {
                let out = yolo(&argv(&[who, "till", till]), &s.0, &mut user);
                assert_eq!(out.code, code, "{who} {till:.40?}: {}", out.stderr);
            }
        }
        assert!(user.asked.is_empty(), "nothing is asked for a refused time");
        for (n, id) in ids() {
            assert_eq!(end_of(&s, &id, s.0.now), None, "{n}");
        }
        // The current minute rolls to tomorrow, never a zero-length YOLO (C6-B02).
        let out = yolo(&argv(&["codex", "till", "14:00"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(end_of(&s, &ids()[1].1, s.0.now), Some(14 * 3600 + 86_400));
        s.0.now = 14 * 3600 + 30;
        let out = yolo(&argv(&["claw", "till", "14:00"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(end_of(&s, &ids()[2].1, s.0.now), Some(14 * 3600 + 86_400));
        // Year 9999 is typed time, not an overflow, and shows (C6-B05).
        let out = yolo(
            &argv(&["hermes", "till", "9999-12-31T23:59:59Z"]),
            &s.0,
            &mut user,
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
        // §16 item 1: the line in the form `<HH:MM> UTC Lunchroom: <words> (<agents>)`.
        assert!(
            out.stdout
                .ends_with(" UTC Lunchroom: YOLO authorized until 23:59 UTC. (hermes)\n"),
            "{}",
            out.stdout
        );
        assert_eq!(end_of(&s, &ids()[3].1, s.0.now), Some(253_402_300_799));
        // Hostile sizes (C6-K04): a long name, a 10,000-name list, a long list of unknown names.
        let before = user.asked.len();
        let out = yolo(
            &argv(&[long_name.as_str(), "till", "15:00"]),
            &s.0,
            &mut user,
        );
        assert_eq!(out.code, 2, "{}", &out.stderr[..out.stderr.len().min(200)]);
        let out = yolo(&argv(&[many.as_str(), "till", "15:00"]), &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", &out.stderr[..out.stderr.len().min(200)]);
        assert_eq!(user.asked.len(), before + 1);
        let unknown = vec!["nobody"; 10_000].join(",");
        let out = yolo(&argv(&[unknown.as_str(), "till", "15:00"]), &s.0, &mut user);
        assert_eq!(out.code, 2);
    }

    // §3::2 (C6-K01): extend, shorten and end for one agent, run at once from several
    // threads through the real store, leave one of the written values and a readable store.
    #[test]
    fn concurrent_yolo_commands_for_one_agent() {
        let s = Scratch::new();
        four_agents(&s);
        let written: [u64; 4] = [14 * 3600, 15 * 3600, 16 * 3600, 1000];
        let who = ids()[1].1.clone();
        std::thread::scope(|scope| {
            for round in 0..5 {
                for (i, till) in ["14:00", "15:00", "16:00", "now"].into_iter().enumerate() {
                    let env = &s.0;
                    scope.spawn(move || {
                        let mut user = UserAt {
                            answer: "yes\n",
                            asked: Vec::new(),
                        };
                        let out = yolo(&argv(&["codex", "till", till]), env, &mut user);
                        // On a loaded machine a writer can wait past the store's 10-second limit
                        // (store.rs, LOCK_WAIT) and is then refused as busy, which is the product's
                        // answer, not a fault (2026-11-14: it failed this test under a parallel
                        // build). Any other failure still fails it.
                        assert!(
                            out.code == 0 || out.stderr.contains("held the store"),
                            "{round} {i}: {}",
                            out.stderr
                        );
                    });
                }
            }
        });
        // Whatever finished last, the stored end is exactly one of the four written times: it
        // covers the second before it and not that second.
        let mail = s.store().and_then(|st| st.read().ok());
        assert!(mail.is_some(), "the store stays readable");
        let ends: Vec<u64> = written
            .into_iter()
            .filter(|e| {
                mail.as_ref().is_some_and(|m| {
                    m.yolo_until(&who, e - 1).is_some() && m.yolo_until(&who, *e).is_none()
                })
            })
            .collect();
        assert_eq!(ends.len(), 1, "{ends:?}");
    }

    /// A user setting a passphrase: types each secret in turn, and the system puts a verifier in
    /// place as root's (`after` is what reading it back then gives, when that is not the verifier
    /// just put in place).
    struct Setting {
        ancestors: Vec<(Option<String>, String)>,
        verifier: Result<String, String>,
        typed: Vec<&'static str>,
        prompts: Vec<String>,
        installed: Option<String>,
        install: Result<(), String>,
        after: Option<Result<String, String>>,
    }

    impl Setting {
        fn new(verifier: Result<String, String>, typed: &[&'static str]) -> Self {
            Self {
                ancestors: vec![(Some("100:5".to_owned()), "bash".to_owned())],
                verifier,
                typed: typed.to_vec(),
                prompts: Vec::new(),
                installed: None,
                install: Ok(()),
                after: None,
            }
        }

        /// A user with no passphrase yet.
        fn first(typed: &[&'static str]) -> Self {
            Self::new(Err(NO_VERIFIER.to_owned()), typed)
        }

        /// A user whose passphrase is [`fake::PASSPHRASE`].
        fn later(typed: &[&'static str]) -> Self {
            Self::new(Ok(fake::verifier()), typed)
        }
    }

    impl System for Setting {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            self.ancestors.clone()
        }
        fn own_tty(&self) -> u64 {
            34816
        }
        fn tty_of(&self, _key: &str) -> Option<u64> {
            None
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            None
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            self.verifier.clone()
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.prompts.push(prompt.to_owned());
            (!self.typed.is_empty()).then(|| self.typed.remove(0).to_owned())
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([4; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, line: &str) -> Result<(), String> {
            self.install.clone()?;
            self.installed = Some(line.to_owned());
            self.verifier = self.after.take().unwrap_or_else(|| Ok(line.to_owned()));
            Ok(())
        }
    }

    const NEW: &str = "a new passphrase 7";

    // A1.3: with no passphrase set, `lunchroom passphrase` asks for the new one twice (and no
    // current one), makes the verifier from it, has the system put it in place as root's, reads it
    // back, and tells each step; the passphrase then confirms.
    #[test]
    fn the_first_passphrase_is_asked_twice_and_set() {
        let s = Scratch::new();
        let mut sys = Setting::first(&[NEW, NEW]);
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert_eq!(
            sys.prompts,
            [
                "New passphrase (at least 12 characters): ",
                "New passphrase again: "
            ]
        );
        let line = sys.installed.clone().unwrap_or_default();
        let made = Verifier::parse(&line, ITERATIONS);
        assert_eq!(made.as_ref().map(|v| v.matches(NEW)), Ok(true), "{line}");
        assert_eq!(made.map(|v| v.matches(fake::PASSPHRASE)), Ok(false));
        for step in [
            "made the passphrase check",
            "put it at /etc/lunchroom/1000.verifier, owned by root, group lunchroom, mode 0640: \
             only Lunchroom's program, which runs with group lunchroom, reads it",
            "read it back: it is owned by root and holds the passphrase just typed",
            "`sudo rm /etc/lunchroom/1000.verifier` removes the check",
        ] {
            assert!(out.stdout.contains(step), "{step}: {}", out.stdout);
        }
    }

    // A1.3: a passphrase of fewer than 12 characters, counted as characters and not bytes, is
    // denied before it is asked again and before anything is put in place; 12 is enough.
    #[test]
    fn a_passphrase_under_twelve_characters_is_refused() {
        let s = Scratch::new();
        for short in ["", "a", "elevenchars", "ééééééééééé"] {
            let mut sys = Setting::first(&[short, short]);
            let out = passphrase(&s.0, &mut sys);
            assert_eq!(out.code, 2, "{short:?}: {}", out.stderr);
            assert!(
                out.stderr.contains("at least 12 characters"),
                "{}",
                out.stderr
            );
            assert_eq!(sys.installed, None, "{short:?}");
            assert_eq!(sys.prompts.len(), 1, "{short:?}");
        }
        let mut sys = Setting::first(&["twelve chars", "twelve chars"]);
        assert_eq!(passphrase(&s.0, &mut sys).code, 0);
        assert!(sys.installed.is_some());
    }

    // A1.3: two entries that differ set nothing.
    #[test]
    fn two_entries_that_differ_are_refused() {
        let s = Scratch::new();
        for second in [
            "a new passphrase 8",
            "A new passphrase 7",
            "a new passphrase 7 ",
            "",
        ] {
            let mut sys = Setting::first(&[NEW, second]);
            let out = passphrase(&s.0, &mut sys);
            assert_eq!(out.code, 2, "{second:?}: {}", out.stderr);
            assert!(
                out.stderr.contains("the two entries differ"),
                "{}",
                out.stderr
            );
            assert_eq!(sys.installed, None, "{second:?}");
        }
    }

    // A1.3: with a passphrase already set, the current one is asked for first; a wrong one asks for
    // nothing more and changes nothing, the right one lets the new one replace it.
    #[test]
    fn the_current_passphrase_is_required_when_one_is_set() {
        let s = Scratch::new();
        for wrong in ["", "yes", "wrong passphrase", "Correct horse battery"] {
            let mut sys = Setting::later(&[wrong, NEW, NEW]);
            let out = passphrase(&s.0, &mut sys);
            assert_eq!(out.code, 3, "{wrong:?}: {}", out.stderr);
            assert!(
                out.stderr.contains("the current passphrase was wrong"),
                "{}",
                out.stderr
            );
            assert_eq!(sys.prompts, ["Current passphrase: "], "{wrong:?}");
            assert_eq!(sys.installed, None, "{wrong:?}");
        }
        let mut none = Setting::later(&[]);
        assert_eq!(passphrase(&s.0, &mut none).code, 3);
        assert_eq!(none.installed, None);
        let mut sys = Setting::later(&[fake::PASSPHRASE, NEW, NEW]);
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert_eq!(sys.prompts.len(), 3);
        assert_eq!(sys.prompts[0], "Current passphrase: ");
        let line = sys.installed.clone().unwrap_or_default();
        assert_eq!(
            Verifier::parse(&line, ITERATIONS).map(|v| v.matches(NEW)),
            Ok(true)
        );
    }

    // A1.3: a check the system cannot trust is not replaced by asking for a passphrase: the user is
    // told to remove it.
    #[test]
    fn a_check_that_cannot_be_trusted_is_not_replaced_unasked() {
        let s = Scratch::new();
        let mut sys = Setting::new(
            Err(
                "the passphrase check at /etc/lunchroom/1000.verifier cannot be trusted (x); \
                 `sudo rm /etc/lunchroom/1000.verifier` removes it"
                    .to_owned(),
            ),
            &[NEW, NEW],
        );
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 3, "{}", out.stderr);
        assert!(out.stderr.contains("sudo rm"), "{}", out.stderr);
        assert_eq!(sys.prompts, Vec::<String>::new());
        assert_eq!(sys.installed, None);
    }

    // A1.3: the same checks as every other command only the user may run come first: a guarded
    // session in the parent chain gets no question and sets nothing.
    #[test]
    fn an_agent_cannot_set_the_passphrase() {
        let s = Scratch::new();
        let mut sys = Setting::first(&[NEW, NEW]);
        sys.ancestors = vec![(None, "claude".to_owned())];
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 3, "{}", out.stderr);
        assert!(out.stderr.starts_with("DENIED"), "{}", out.stderr);
        assert_eq!(sys.prompts, Vec::<String>::new());
        assert_eq!(sys.installed, None);
    }

    // A1.3: a system that cannot put the verifier in place (no sudo, a wrong sudo password) sets
    // nothing and says so, with the steps done; a verifier that reads back as not root's, or as
    // another passphrase, is reported as not set.
    #[test]
    fn a_verifier_that_is_not_put_in_place_is_reported_not_set() {
        let s = Scratch::new();
        let mut sys = Setting::first(&[NEW, NEW]);
        sys.install = Err(
            "installing /etc/lunchroom/1000.verifier did not succeed (exit status: 1)".to_owned(),
        );
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 1, "{}", out.stdout);
        assert!(
            out.stderr.contains("the passphrase was not set"),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("did not succeed"), "{}", out.stderr);
        assert!(
            out.stdout.contains("made the passphrase check"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("put it at"), "{}", out.stdout);
        // Read back as not root's alone.
        let mut sys = Setting::first(&[NEW, NEW]);
        sys.after = Some(Err(
            "the passphrase check at /etc/lunchroom/1000.verifier cannot be trusted (it is not a \
             regular file owned by root and writable by no one else)"
                .to_owned(),
        ));
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 1);
        assert!(
            out.stderr.contains("the passphrase is not set"),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("cannot be trusted"), "{}", out.stderr);
        // Read back as a verifier for another passphrase (a file swapped while the user typed the
        // password for sudo).
        let mut sys = Setting::first(&[NEW, NEW]);
        sys.after = Some(Ok(fake::verifier()));
        let out = passphrase(&s.0, &mut sys);
        assert_eq!(out.code, 1);
        assert!(
            out.stderr
                .contains("does not hold the passphrase just typed"),
            "{}",
            out.stderr
        );
    }

    // ---- Lunchroom 1.1.0, delivery findings G2-7, G2-11 and G3-14 (the yolo usage) ----

    /// A scratch store where claude (c1) sent codex (x1) `queued` messages and codex is named.
    fn with_queued(s: &Scratch, queued: usize) -> Vec<String> {
        let (claude, codex) = (
            Identity::new(Harness::ClaudeCode, "c1"),
            Identity::new(Harness::Codex, "x1"),
        );
        let ids = s.store().and_then(|st| {
            st.update(|m| {
                for (name, who) in [("claude", &claude), ("codex", &codex)] {
                    m.observe(who);
                    let _ = m.register(name, who, Origin::User, None);
                }
                (0..queued)
                    .map(|n| {
                        m.send(&claude, "claude", "codex", &format!("note {n}"), 900)
                            .map(|a| a.id)
                            .unwrap_or_default()
                    })
                    .collect::<Vec<String>>()
            })
            .ok()
        });
        assert!(ids.is_some());
        ids.unwrap_or_default()
    }

    // G2-7: taking a name away says in its question what happens to the messages QUEUED for it, in
    // full (none, one, several), and on the user's yes they are FAILED in the same step and their
    // senders told; before the fix the question promised they "stay QUEUED" and the code left them
    // to the next check.
    #[test]
    fn unregister_says_and_does_what_happens_to_the_queued_messages() {
        for (count, said) in [
            (0, "No message is QUEUED for codex:x1."),
            (
                1,
                "1 message QUEUED for codex:x1 becomes FAILED, and its sender is told.",
            ),
            (
                3,
                "3 messages QUEUED for codex:x1 become FAILED, and their senders are told.",
            ),
        ] {
            let s = Scratch::new();
            let ids = with_queued(&s, count);
            let mut yes = UserAt {
                answer: "yes\n",
                asked: Vec::new(),
            };
            let out = unregister(&["codex".to_owned()], &s.0, &mut yes);
            assert_eq!(out.code, 0, "{}", out.stderr);
            assert_eq!(
                yes.asked.first().map(String::as_str),
                Some(
                    format!(
                        "Lunchroom: take the name \"codex\" from codex:x1, so it receives no more \
                         messages? {said}\nPassphrase: "
                    )
                    .as_str()
                ),
                "{count}"
            );
            let line = match count {
                0 => String::new(),
                1 => format!("{} is now FAILED, and its sender is told.\n", ids[0]),
                _ => format!(
                    "{} are now FAILED, and their senders are told.\n",
                    ids.join(", ")
                ),
            };
            assert_eq!(out.stdout, format!("codex is no longer named\n{line}"));
            let mail = s.store().and_then(|st| st.read().ok());
            for id in &ids {
                assert_eq!(
                    mail.as_ref()
                        .and_then(|m| m.message(id))
                        .map(|d| d.status.state()),
                    Some(lunchroom_core::vocab::MessageState::Failed),
                    "{id}"
                );
            }
            // The sender is told, once, in full.
            let told = mail
                .as_ref()
                .map(|m| m.notices(&Identity::new(Harness::ClaudeCode, "c1")).len());
            assert_eq!(told, Some(count));
        }
    }

    // G2-7: moving a name says the same of the earlier holder's messages and does it.
    #[test]
    fn moving_a_name_says_and_does_what_happens_to_the_queued_messages() {
        let s = Scratch::new();
        let ids = with_queued(&s, 2);
        let other = Identity::new(Harness::Codex, "x2");
        assert!(
            s.store()
                .and_then(|st| st.update(|m| m.observe(&other)).ok())
                .is_some()
        );
        let mut yes = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let args = ["codex", "codex:x2"].map(str::to_owned);
        let out = register(&args, &s.0, &mut yes);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(
            yes.asked.get(1).map(String::as_str),
            Some(
                "Lunchroom: the name \"codex\" is held by codex:x1. 2 messages QUEUED for codex:x1 \
                 become FAILED, and their senders are told. Move the name to codex:x2?\n\
                 Passphrase: "
            )
        );
        assert_eq!(
            out.stdout,
            format!(
                "codex is codex:x2 (moved from codex:x1)\n{} are now FAILED, and their senders \
                 are told.\n",
                ids.join(", ")
            )
        );
    }

    // G2-11: `lunchroom approve` lists each waiting call with how long it has waited, the question
    // says it too and when the request lapses, and a call that has waited as long as an approval
    // lasts is dropped from the list and cannot be approved.
    #[test]
    fn approve_shows_the_age_of_a_call_and_drops_one_that_lapsed() {
        let s = Scratch::new();
        let make = |at: u64| Request {
            session: Identity::new(Harness::ClaudeCode, "s1"),
            agent: "Claude".to_owned(),
            tool: "Bash".to_owned(),
            input: json!({"command": format!("git push {at}")}),
            cwd: "/work/a".to_owned(),
            rule: "PROTECTED (push or publish)".to_owned(),
            at,
        };
        // Env.now is 1000: one call asked at 700 (5 min ago), one at 400 (10 min ago, lapsed).
        assert!(
            s.store()
                .and_then(|st| st
                    .update(|m| {
                        m.request(make(400));
                        m.request(make(700));
                    })
                    .ok())
                .is_some()
        );
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let listed = approve(&[], &s.0, &mut user);
        assert_eq!(
            listed.stdout,
            "1. Claude (claude-code:s1) Bash {\"command\":\"git push 700\"} (asked 5 min ago)\n"
        );
        let out = approve(&["1".to_owned()], &s.0, &mut user);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let question = user.asked.first().cloned().unwrap_or_default();
        assert!(
            question.ends_with(
                "\nThe agent asked 5 min ago; the request lapses at 00:21 UTC if it is not \
                 approved by then.\nPassphrase: "
            ),
            "{question}"
        );
        assert_eq!(
            approve(&["2".to_owned()], &s.0, &mut user).code,
            2,
            "the lapsed call is gone"
        );
    }

    // G3-14 (the error text, in user.rs): the usage line is the one the help text shows.
    #[test]
    fn the_yolo_usage_error_is_the_one_usage_line() {
        let s = Scratch::new();
        let mut user = UserAt {
            answer: "yes\n",
            asked: Vec::new(),
        };
        let out = yolo(&["all".to_owned()], &s.0, &mut user);
        assert_eq!(out.code, 2);
        assert_eq!(
            out.stderr.trim_end(),
            "usage: lunchroom yolo <name,name,...|all> till <HH:MM>; separate the names with \
             commas; it always has an end time"
        );
    }

    // G4-1: the approval question in full, for a rule kept as a whole sentence and for one kept by
    // an older Lunchroom as a noun phrase.
    #[test]
    fn the_approval_question_reads_whole_for_both_kinds_of_rule() {
        assert_eq!(
            approval_question(
                "This call is PROTECTED (deleting a file), so it needs your yes.",
                "The call's input:\n{\"command\":\"rm x\"}",
                "2 minutes",
                "12:10",
            ),
            "Lunchroom: This call is PROTECTED (deleting a file), so it needs your yes. Approve \
             this one call, for 10 minutes, once?\n\
             The call's input:\n{\"command\":\"rm x\"}\n\
             The agent asked 2 minutes ago; the request lapses at 12:10 if it is not approved by \
             then.\n"
        );
        assert_eq!(
            approval_question(
                "PROTECTED (push or publish)",
                "The call's input:\n{\"command\":\"git push\"}",
                "2 minutes",
                "12:10",
            ),
            "Lunchroom: this call is outside PROTECTED (push or publish). Approve this one call, \
             for 10 minutes, once?\n\
             The call's input:\n{\"command\":\"git push\"}\n\
             The agent asked 2 minutes ago; the request lapses at 12:10 if it is not approved by \
             then.\n"
        );
    }
}

#[cfg(test)]
mod tests_u6;
