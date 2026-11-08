//! The `lunchroom` commands an agent may run, `send` and `status`, and the user's `doctor`.
//!
//! Spec: §9 I1.2 (the sender is the hook-observed session the command runs in), §10 D1.1 (a message
//! is QUEUED when accepted), the rules that stop a message (§2 R1, §11 L1, §9 I1.4, I1.5), §4 R3.4
//! (each UNGUARDED agent's reason in `status`) and §14 C1.4 (`doctor`, which the guard DENIES to
//! an agent).

/// This build: `lunchroom <version> (<commit>)`, with `, modified` when built from uncommitted
/// sources, and the version alone when built with no git history (`build.rs`).
pub const VERSION: &str = env!("LUNCHROOM_VERSION");

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;

use lunchroom_core::denial::Denial;
use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{MAX_MESSAGE_BYTES, Mail, SendRefusal};
use lunchroom_core::store::Store;
use lunchroom_core::vocab::{AgentState, MessageState, Reason, Verdict, Word};
use lunchroom_core::{cards, secrets};

use crate::evidence::{Assessment, assess, evidence};
use crate::home::Env;
use crate::process;
use crate::terminal::System;

/// What a command prints, where, and its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

/// What the user is told when Lunchroom's store cannot be read.
pub(crate) fn unread(e: impl std::fmt::Display) -> String {
    format!("Lunchroom's store cannot be read: {e}")
}

/// What the user is told when Lunchroom's store cannot be updated.
pub(crate) fn unwritten(e: impl std::fmt::Display) -> String {
    format!("Lunchroom's store cannot be updated: {e}")
}

impl Outcome {
    pub(crate) const fn ok(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            code: 0,
        }
    }

    /// An error that is no verdict: a usage mistake, or a file or store that cannot be used.
    pub(crate) fn failed(text: &str, code: u8) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("{text}\n"),
            code,
        }
    }

    pub(crate) fn denied(reason: &str, code: u8) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("{} · {reason}\n", Verdict::Denied.word()),
            code,
        }
    }
}

/// Lunchroom's store; one that cannot be used DENIES what needs it, with the reason (H1.5). `what`
/// is what was DENIED (`the message to alice`, `lunchroom status`).
fn store(env: &Env, what: &str) -> Result<Store<Mail>, Outcome> {
    Store::open(&env.store_dir()).map_err(|e| store_unusable(&e, what, "opened"))
}

/// A command that stops because Lunchroom's store cannot be used, laid out as the other DENIED
/// answers: what happened, why, and what to do. `done` is `opened`, `read` or `updated`.
pub(crate) fn store_unusable(e: &dyn std::fmt::Display, what: &str, done: &str) -> Outcome {
    Outcome {
        stdout: String::new(),
        stderr: explained(
            &format!("Lunchroom's store cannot be {done}"),
            what,
            &format!(
                "Lunchroom keeps its messages and state in a store, and it cannot be {done}: {e}"
            ),
            &[],
            &[
                "The agent: wait a moment and try once more; if it fails again, tell the user."
                    .to_owned(),
                "The user: run `lunchroom status` in a terminal of their own to see whether the \
                 store can be read, and check that the disk has space; `lunchroom repair` puts \
                 back the last good store when it cannot be read."
                    .to_owned(),
            ],
        ),
        code: 1,
    }
}

/// A command that stops for a reason of its own, laid out as the other DENIED answers.
fn stopped_with(line: &str, what: &str, rule: &str, ways: &[String], code: u8) -> Outcome {
    Outcome {
        stdout: String::new(),
        stderr: explained(line, what, rule, &[], ways),
        code,
    }
}

/// Why the sender cannot be found.
enum NoSender {
    /// No harness process a hook has seen is above this command.
    Unseen,
    /// The environment names a session other than the one the hook saw (a forged id).
    Forged,
}

/// The session this command runs in, as a guard hook saw it (I1.2): the nearest process above this
/// one that a hook recorded, found through `/proc`, never from the environment. The environment's
/// session id, when set, must name that same session.
fn sender(mail: &Mail, env: &Env) -> Result<Identity, NoSender> {
    let (process, last) = process::ancestors(std::process::id())
        .into_iter()
        .skip(1)
        .find_map(|pid| {
            process::key(pid).and_then(|k| mail.process_session(&k).cloned().map(|who| (k, who)))
        })
        .ok_or(NoSender::Unseen)?;
    // I1.2: only the variable of the harness the command runs in names a session: Claude Code's
    // `CLAUDE_CODE_SESSION_ID`, Codex's `CODEX_THREAD_ID`, Hermes's `HERMES_SESSION_ID`, and
    // `LUNCHROOM_SESSION`, which Lunchroom's OpenClaw plugin supplies. Another harness's variable,
    // inherited when one harness is started from another's session, names nothing. In a process
    // that runs many sessions (an OpenClaw or Hermes gateway) the session is the one its variable
    // names, if a hook saw it in this very process; with none named, only a process that ran one
    // session gives one. In Claude Code and Codex the variable, when set, must name the session the
    // process shows.
    let own = match last.harness {
        Harness::OpenClaw => env.lunchroom_session.as_deref().map(Identity::parse),
        Harness::Hermes => env
            .hermes_session
            .as_deref()
            .map(|id| Some(Identity::new(Harness::Hermes, id))),
        Harness::ClaudeCode => env
            .claude_session
            .as_deref()
            .map(|id| Some(Identity::new(Harness::ClaudeCode, id))),
        Harness::Codex => env
            .codex_thread
            .as_deref()
            .map(|id| Some(Identity::new(Harness::Codex, id))),
    };
    let named = match own {
        Some(Some(who)) if who.harness == last.harness => Some(who),
        Some(_) => return Err(NoSender::Forged),
        None => None,
    };
    let me = match named {
        Some(who) if matches!(who.harness, Harness::ClaudeCode | Harness::Codex) => {
            if who != last {
                return Err(NoSender::Forged);
            }
            who
        }
        Some(who) if mail.seen_in_process(&process, &who) => who,
        Some(_) => return Err(NoSender::Forged),
        None if matches!(last.harness, Harness::OpenClaw | Harness::Hermes)
            && mail.sessions_in_process(&process) > 1 =>
        {
            return Err(NoSender::Unseen);
        }
        None => last,
    };
    Ok(me)
}

/// `lunchroom send <to> [--reply-to <id>] <text…>`; a text of `-` is read from standard input.
#[must_use]
#[allow(clippy::too_many_lines)] // one send: arguments, the text and its bounds, the store, the hand-over
pub fn send(args: &[String], env: &Env) -> Outcome {
    // `--reply-to <id>` may stand before or after the name; the text is everything after both
    // (R1.2: the id plays no part, and words of the text are never taken for it).
    let mut rest: Vec<&str> = Vec::new();
    let mut k = 0;
    while k < args.len() {
        if args[k] == "--reply-to" && rest.len() < 2 {
            k += 2;
            continue;
        }
        rest.push(&args[k]);
        k += 1;
    }
    let Some((to, words)) = rest.split_first().filter(|(_, w)| !w.is_empty()) else {
        return send_usage();
    };
    let mut text = words.join(" ");
    if text == "-" {
        match read_bounded(std::io::stdin()) {
            Ok(bytes) if bytes.len() > MAX_MESSAGE_BYTES => return too_long(to),
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(read) => text = read,
                Err(_) => return unreadable_text(to),
            },
            Err(_) => return unreadable_text(to),
        }
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return too_long(to);
    }
    if text.trim().is_empty() {
        return empty_text(to);
    }
    let store = match store(env, &format!("the message to {to}")) {
        Ok(s) => s,
        Err(o) => return o,
    };
    let result = store.update(|m| {
        let me = sender(m, env).map_err(Err)?;
        let from_name = m.name_of(&me).unwrap_or(&me.session).to_owned();
        match m.send(&me, &from_name, to, &text, env.now) {
            Ok(accepted) => Ok(accepted),
            // V1.4: what holds the sender, for the refusal's message.
            Err(refusal) => {
                let held_by = crate::claude::holders(m, &me);
                Err(Ok((refusal, held_by, from_name)))
            }
        }
    });
    match result {
        // H1.5: a send the store cannot take is DENIED with the reason.
        Err(e) => store_unusable(&e, &format!("the message to {to}"), "updated"),
        Ok(Ok(accepted)) => {
            let mut out = format!("{} {}\n", MessageState::Queued.word(), accepted.id);
            if accepted.halted {
                out.push_str(&cards::loop_halted());
                out.push('\n');
            }
            // R3.2: a message to an UNGUARDED or STOPPED agent stays QUEUED, and the sender is told.
            if let Ok(mail) = store.read() {
                let told = match mail
                    .agent_state(&accepted.to, evidence(&mail, &accepted.to, &env.home))
                {
                    AgentState::Unguarded => {
                        let _ = writeln!(out, "{}", cards::unguarded(to));
                        true
                    }
                    AgentState::Stopped => {
                        let _ = writeln!(out, "{}", cards::stopped(to));
                        true
                    }
                    _ => false,
                };
                // G2-20: the reason the check below notes for this message is told to the user
                // only; the sender has just been told it.
                if told {
                    let _ = store.update(|m| m.sender_told_reason(&accepted.id));
                }
            }
            hand_over_now(env, &store, &accepted, &mut out);
            // D1.7: every `lunchroom send` checks every QUEUED message's time limits.
            // D1.1: the check runs in the sender's process, so it offers only the sender's own.
            let offers = crate::checks::Offers::From(accepted.from.clone());
            if let Err(e) = crate::checks::run(env, &store, &offers) {
                let _ = writeln!(
                    out,
                    "Lunchroom could not check the other QUEUED messages: {e}"
                );
            }
            Outcome::ok(out)
        }
        Ok(Err(Ok((SendRefusal::UnknownName, _, _)))) => unknown_name(&store, to),
        Ok(Err(Ok((refusal, held_by, from)))) => refused(
            &refusal,
            &Refused {
                to,
                from: &from,
                text: &text,
                held_by: &held_by,
            },
        ),
        Ok(Err(Err(NoSender::Unseen))) => refused(
            &SendRefusal::UnseenSender,
            &Refused {
                to,
                from: "",
                text: &text,
                held_by: &[],
            },
        ),
        Ok(Err(Err(NoSender::Forged))) => forged(to),
    }
}

/// What `lunchroom send` says when it is given no name or no text.
pub(crate) fn send_usage() -> Outcome {
    stopped_with(
        "lunchroom send needs the name of an agent and a text",
        "the message",
        "a message goes to one named agent, and has text for that agent to read",
        &[
            "The agent: write `lunchroom send <name> <text>`; `--reply-to <id>` may stand before \
             the text, and a text of `-` is read from standard input."
                .to_owned(),
            "`lunchroom status` lists every agent and its name.".to_owned(),
        ],
        2,
    )
}

/// What `lunchroom send` says when its text read from standard input is not UTF-8 or cannot be
/// read.
pub(crate) fn unreadable_text(to: &str) -> Outcome {
    stopped_with(
        "the message text cannot be read",
        &format!("the message to {to}"),
        "the text given on standard input is not valid UTF-8 text, or could not be read",
        &[format!(
            "The agent: give the text as the command's argument, or pipe UTF-8 text into \
             `lunchroom send {to} -`."
        )],
        2,
    )
}

/// What `lunchroom send` says of a message with no text.
pub(crate) fn empty_text(to: &str) -> Outcome {
    stopped_with(
        "the message has no text",
        &format!("the message to {to}"),
        &format!("a message needs text for {to} to read, and this one is empty or only spaces"),
        &[format!(
            "The agent: put the text after the name, for example `lunchroom send {to} \"the \
             build passed\"`; a text of `-` is read from standard input."
        )],
        2,
    )
}

/// What `lunchroom send` says when the environment names another session than the one it runs in
/// (I1.2).
pub(crate) fn forged(to: &str) -> Outcome {
    stopped_with(
        "the session id in this command's environment is not the session it runs in",
        &format!("the message to {to}"),
        "Lunchroom stamps a message with the session it really runs in, found from the process \
         that started it, and does not take another session's id from the environment",
        &[
            "The agent: run `lunchroom send` again without setting a session id in the command's \
             environment (CLAUDE_CODE_SESSION_ID, CODEX_THREAD_ID, HERMES_SESSION_ID or \
             LUNCHROOM_SESSION)."
                .to_owned(),
        ],
        3,
    )
}

/// D1.1 (Codex, OpenClaw): the accepted message is handed to the session now, through `codex
/// queue` or `openclaw agent`; a Claude Code or Hermes session takes it from its listener or at
/// its next turn. What happened is added to the sender's report, and kept for what is told (D1.8).
fn hand_over_now(
    env: &Env,
    store: &Store<Mail>,
    accepted: &lunchroom_core::mail::Accepted,
    out: &mut String,
) {
    let handed = match accepted.to.harness {
        Harness::Codex => crate::codex::deliver(env, store, &accepted.to, Some(&accepted.from)),
        Harness::OpenClaw => {
            crate::openclaw::deliver(env, store, &accepted.to, Some(&accepted.from))
        }
        Harness::ClaudeCode | Harness::Hermes => return,
    };
    if let Err(e) = crate::checks::after_hand_over(store, &handed) {
        let _ = writeln!(out, "{e}");
    }
    out.push_str(&hand_over_report(&accepted.id, &handed));
}

/// The sender's report of a hand-over `handed` of message `sent` (D1.3, D1.4): DELIVERED, a failed
/// attempt and whether it made the message FAILED, not confirmed, or a store that could not be
/// used; each a line, or nothing when nothing was handed over.
pub(crate) fn hand_over_report(
    sent: &str,
    handed: &Result<crate::codex::Delivered, String>,
) -> String {
    match handed {
        Ok(crate::codex::Delivered::Confirmed(id)) => {
            format!("{} {id}\n", MessageState::Delivered.word())
        }
        // D1.4: the third failed attempt makes the message FAILED.
        Ok(crate::codex::Delivered::Failed(id, reason, failed)) => {
            let state = if failed.message_failed() {
                MessageState::Failed
            } else {
                MessageState::Queued
            };
            format!("{id}: {reason}; {}\n", state.word())
        }
        // D1.3: handed over, not yet seen in the session.
        Ok(crate::codex::Delivered::Unconfirmed(id)) => {
            // G2-21: the text, with what it means and what to do, is the cards' (G2).
            format!("{}\n", cards::not_confirmed(id))
        }
        Ok(crate::codex::Delivered::Nothing) => String::new(),
        // The store could not be used during the hand-over: the sender is told what happened, why
        // and what to do; the message is QUEUED as reported above.
        Err(e) => format!(
            "{sent}: the hand-over could not be completed ({e}); it is still {}. Do not send it \
             again; `lunchroom status` shows where it stands. If the store still cannot be used, \
             tell the user.\n",
            MessageState::Queued.word()
        ),
    }
}

/// No agent is named `to`: the refusal names the closest registered name, or all of them.
pub(crate) fn unknown_name(store: &Store<Mail>, to: &str) -> Outcome {
    let names: Vec<String> = store
        .read()
        .map(|m| m.names().map(|(n, _)| n.to_owned()).collect())
        .unwrap_or_default();
    let user = Word::TheUser.text();
    let way = match closest(to, &names) {
        Some(n) => format!("The agent: send again to {n}, the name closest to {to}."),
        None if names.is_empty() => format!(
            "{}: type join (or join <name>) in each session that should receive messages; until \
             then no agent has a name to send to.",
            capital(user)
        ),
        None => format!(
            "The agent: send again to one of the names: {}.",
            names.join(", ")
        ),
    };
    stopped_with(
        &format!("no agent is named {to}"),
        &format!("the message to {to}"),
        &format!("only a session {user} named receives messages, and none is named {to}"),
        &[
            way,
            "`lunchroom status` lists every agent and its name.".to_owned(),
        ],
        3,
    )
}

/// The registered name `to` stands for: the same name up to case and `_` for `-`,
/// or the only one within two edits.
fn closest(to: &str, names: &[String]) -> Option<String> {
    let norm = |s: &str| s.to_ascii_lowercase().replace('_', "-");
    if let Some(n) = names.iter().find(|n| norm(n) == norm(to)) {
        return Some(n.clone());
    }
    let near: Vec<&String> = names
        .iter()
        .filter(|n| edits(&norm(n), &norm(to)) <= 2)
        .collect();
    (near.len() == 1).then(|| near[0].clone())
}

/// The Levenshtein distance between `a` and `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (here + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(ca != *cb));
            prev = here;
        }
    }
    row[b.len()]
}

/// Reads at most [`MAX_MESSAGE_BYTES`] + 1 bytes of `from`: one more than the limit is enough to
/// show the text is over it, and the rest of the stream is never read.
fn read_bounded(from: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    from.take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn too_long(to: &str) -> Outcome {
    stopped_with(
        &format!("the message is longer than {MAX_MESSAGE_BYTES} bytes"),
        &format!("the message to {to}"),
        &format!("a message may be at most {MAX_MESSAGE_BYTES} bytes long"),
        &[
            "The agent: send a shorter message, and send what remains as further messages."
                .to_owned(),
        ],
        2,
    )
}

/// A message with more lines than one command argument can carry once each line is labelled.
fn too_many_lines(to: &str, text: &str) -> Outcome {
    stopped_with(
        "the message has too many lines",
        &format!("the message to {to} ({} lines)", text.lines().count()),
        "each line of a message is labelled, and the labelled message must fit in one command \
         argument",
        &["The agent: send fewer, longer lines, or split the message into several.".to_owned()],
        2,
    )
}

/// What a refused message's explanation is built from (V1.4).
pub(crate) struct Refused<'a> {
    /// The name the sender gave.
    pub(crate) to: &'a str,
    /// The sender's name, or its session id; empty when no session was found.
    pub(crate) from: &'a str,
    pub(crate) text: &'a str,
    /// What holds the sender, each `<agent> (<message id>)`.
    pub(crate) held_by: &'a [String],
}

/// V1.4 for a refusal the vocabulary table has no reason for: the DENIED line, what was DENIED,
/// why (with what holds the sender), and each way forward.
fn explained(line: &str, what: &str, rule: &str, held_by: &[String], ways: &[String]) -> String {
    let mut out = format!(
        "{} · {line}\nWhat was {}: {what}.\nWhy: {rule}.",
        Verdict::Denied.word(),
        Verdict::Denied.word()
    );
    if !held_by.is_empty() {
        let _ = write!(
            out,
            " This session is {} by {}.",
            AgentState::Held.word(),
            held_by.join(", ")
        );
    }
    if !ways.is_empty() {
        out.push_str("\nWays forward:");
        for way in ways {
            let _ = write!(out, "\n- {way}");
        }
    }
    out.push('\n');
    out
}

pub(crate) fn refused(refusal: &SendRefusal, r: &Refused<'_>) -> Outcome {
    // The text is never shown: a refused message may carry a secret (L1.2).
    let what = format!("the message to {} ({} bytes)", r.to, r.text.len());
    match refusal {
        SendRefusal::TooLong => too_long(r.to),
        SendRefusal::TooManyLines => too_many_lines(r.to, r.text),
        SendRefusal::UnknownName => stopped_with(
            &format!("no agent is named {}", r.to),
            &what,
            &format!(
                "only a session {} named receives messages",
                Word::TheUser.text()
            ),
            &["`lunchroom status` lists every agent and its name.".to_owned()],
            3,
        ),
        SendRefusal::UnseenSender | SendRefusal::NotNamed => unreached(refusal, r, &what),
        SendRefusal::NotHolder | SendRefusal::NameOnlyHolder => not_a_holder(refusal, r, &what),
        SendRefusal::Denied(reason) => by_rule(*reason, r, what),
    }
}

/// V1.4: the sender is not one Lunchroom knows, or the recipient is not one the user named.
fn unreached(refusal: &SendRefusal, r: &Refused<'_>, what: &str) -> Outcome {
    let user = Word::TheUser.text();
    match refusal {
        SendRefusal::UnseenSender => Outcome {
            stdout: String::new(),
            stderr: explained(
                "this session has not been seen by a guard hook",
                what,
                "Lunchroom stamps a message with the session that sends it, and knows a session \
                 only once a guard hook has seen it",
                &[],
                &[
                    "The agent: run `lunchroom send` from inside a session Lunchroom guards, after \
                     a tool call or a prompt there has passed its guard."
                        .to_owned(),
                    format!(
                        "{}: install Lunchroom's hooks for this harness with `lunchroom install`, \
                         in a terminal of their own.",
                        capital(user)
                    ),
                ],
            ),
            code: 3,
        },
        _ => Outcome {
            stdout: String::new(),
            stderr: explained(
                &format!("{user} has not named {} to receive messages", r.to),
                what,
                &format!("only a session {user} named receives messages"),
                &[],
                &[format!(
                    "{}: type join (or join <name>) in that session, or run `lunchroom register \
                     <name> <harness>:<session id>` in a terminal of their own.",
                    capital(user)
                )],
            ),
            code: 3,
        },
    }
}

/// V1.4: a HELD sender may reply only to the agents whose messages hold it.
fn not_a_holder(refusal: &SendRefusal, r: &Refused<'_>, what: &str) -> Outcome {
    let user = Word::TheUser.text();
    let held = AgentState::Held.word();
    let clear_hold = format!(
        "{}: type in this session to clear the hold; it may then message any agent {user} named.",
        capital(user)
    );
    // The agents whose messages hold the sender, by name.
    let holder_names: Vec<&str> = r
        .held_by
        .iter()
        .filter_map(|h| h.rsplit_once(" (").map(|(name, _)| name))
        .collect();
    match refusal {
        SendRefusal::NotHolder => {
            let mut ways = Vec::new();
            if !holder_names.is_empty() {
                ways.push(format!(
                    "The agent: reply to {} with `lunchroom send <name> <text>`.",
                    holder_names.join(" or ")
                ));
            }
            ways.push(clear_hold);
            Outcome {
                stdout: String::new(),
                stderr: explained(
                    &format!(
                        "a {held} session may reply only to the agents whose messages hold it"
                    ),
                    what,
                    &format!(
                        "a {held} session may reply only to the agents whose messages hold it, and \
                         {} is not one of them",
                        r.to
                    ),
                    r.held_by,
                    &ways,
                ),
                code: 7,
            }
        }
        _ => Outcome {
            stdout: String::new(),
            stderr: explained(
                "the message holding this session names no agent to reply to",
                what,
                &format!(
                    "a {held} session replies only to the agent whose message holds it, and the \
                     record that holds this one carries no agent to reply to"
                ),
                r.held_by,
                &[clear_hold],
            ),
            code: 7,
        },
    }
}

/// V1.4: a message DENIED by a rule of the vocabulary table (leak, halted, loop, rate limit).
fn by_rule(reason: Reason, r: &Refused<'_>, what: String) -> Outcome {
    let user = Word::TheUser.text();
    let mut denial = Denial::new(reason, what);
    denial.held_by = r.held_by.to_vec();
    let pair = if r.from.is_empty() {
        r.to.to_owned()
    } else {
        format!("{} and {}", r.from, r.to)
    };
    denial.ways = match reason {
        Reason::Leak => vec![
            "The agent: send it again without the secret, and without asking for one.".to_owned(),
        ],
        Reason::Halted => vec![
            format!("The agent: wait; nothing passes between {pair} until {user} types."),
            format!(
                "{}: type in either session ({pair}) to clear it.",
                capital(user)
            ),
        ],
        Reason::Loop => vec!["The agent: send something new.".to_owned()],
        Reason::RateLimit => vec![
            "The agent: wait, then send again.".to_owned(),
            format!(
                "{}: `lunchroom rate <messages> <seconds>`, in a terminal of their own, sets the \
                 limit.",
                capital(user)
            ),
        ],
        // A tool call's reasons; `lunchroom send` gives none of them.
        _ => Vec::new(),
    };
    denial.rule = match reason {
        Reason::Halted | Reason::Loop | Reason::RateLimit => {
            format!("{}; the two agents are {pair}", reason.rule())
        }
        _ => reason.rule(),
    };
    // Cards 2 and 3 (L1.4, R1.4): the card's line first, as printed.
    let card = match reason {
        Reason::Leak if secrets::asks_for_secret(r.text) => {
            format!("{}\n", cards::asked_for_secret())
        }
        Reason::Halted => format!("{}\n", cards::conversation_halted()),
        _ => String::new(),
    };
    Outcome {
        stdout: String::new(),
        stderr: format!("{card}{}\n", denial.render()),
        code: match reason {
            Reason::Halted => 4,
            Reason::RateLimit => 5,
            _ => 7,
        },
    }
}

/// `text` with its first letter in capitals.
fn capital(text: &str) -> String {
    let mut cs = text.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

/// D1.9: `lunchroom status`'s lines for every QUEUED message, whatever its recipient (one no
/// longer named included): its recipient, age, stage and since when, attempt (n of 3), when it is
/// offered again, and why it cannot be handed over; and why the check of D1.7 could not run, if it
/// could not.
pub(crate) fn queued_lines(lines: &mut String, mail: &Mail, now: u64, unchecked: Option<&str>) {
    let queued = mail.queued();
    if !queued.is_empty() {
        let _ = writeln!(lines, "{} messages:", MessageState::Queued.word());
        for m in queued {
            let _ = writeln!(lines, "  {}", lunchroom_core::mail::standing(m, now));
        }
    }
    if let Some(e) = unchecked {
        let _ = writeln!(
            lines,
            "Lunchroom could not check the QUEUED messages' time limits: {e}"
        );
    }
}

/// One agent's lines in `lunchroom status`.
fn agent_lines(
    lines: &mut String,
    mail: &Mail,
    name: &str,
    who: &Identity,
    state: AgentState,
    now: u64,
) {
    // R2.6: the harness's own YOLO as its guard last saw it, and a `lunchroom yolo`'s end time.
    let mut yolo = String::new();
    if let Some(end) = mail.yolo_until(who, now) {
        let day = end % 86_400;
        let _ = write!(
            yolo,
            "\t{} until {:02}:{:02} UTC",
            Word::Yolo.text(),
            day / 3600,
            day % 3600 / 60
        );
    }
    if mail.harness_yolo(who) {
        let _ = write!(yolo, "\t{} (the harness's own)", Word::Yolo.text());
    }
    // `deliveries` is the session's QUEUED messages, oldest first.
    let queued = mail.deliveries(who);
    let _ = writeln!(
        lines,
        "{name}\t{who}\t{}\t{} {}{yolo}",
        state.word(),
        MessageState::Queued.word(),
        queued.len()
    );
    // C1.1: what holds it. A hold by a message Lunchroom sent is listed with its sender and
    // message id; holds that no message caused (a tool's output, a transcript) are counted by
    // their source.
    let mut sources: BTreeMap<&str, usize> = BTreeMap::new();
    for h in mail.holding(who) {
        if mail.message(&h.message).is_some() {
            let _ = writeln!(
                lines,
                "  {} by {}\t{}",
                Word::Held.text(),
                h.from_name,
                h.message
            );
        } else {
            *sources.entry(&h.from_name).or_default() += 1;
        }
    }
    for (source, n) in sources {
        let _ = writeln!(
            lines,
            "  {} by {} ({n})",
            Word::Held.text(),
            source.replace('-', " ")
        );
    }
    // C1.1: calls waiting for the user's approval (`lunchroom approve` lists them).
    let waiting = mail.requests().iter().filter(|r| &r.session == who).count();
    if waiting > 0 {
        let _ = writeln!(
            lines,
            "  calls waiting for {} from this agent: {waiting}",
            Word::TheUser.text()
        );
    }
    // D1.3: an attempt whose confirmation has not arrived is reported as such, never DELIVERED.
    for d in queued.iter().filter(|d| d.presented_at.is_some()) {
        if let Some(said) = mail.attempt_unknown(&d.id) {
            let _ = writeln!(lines, "  {}\t{said}", d.id);
        }
    }
}

/// `lunchroom status`: each named agent, whether it is HELD, and its QUEUED messages.
#[must_use]
pub fn status(env: &Env) -> Outcome {
    // C1.1: a held store is waited for 10 seconds in all, not 10 seconds for each of the reads and
    // writes below; past that each one reports the store busy at once.
    lunchroom_core::store::limit_waits_until(
        std::time::Instant::now() + lunchroom_core::store::LOCK_WAIT,
    );
    let store = match store(env, "lunchroom status") {
        Ok(s) => s,
        Err(o) => return o,
    };
    // D1.7: `lunchroom status` checks every QUEUED message's time limits before it shows them, so
    // it writes the store (the user's command; `lunchroom doctor` changes nothing).
    // D1.1: an agent's `status` offers nobody's message (it would hand the agent another sender's
    // text at a time it chooses); the user's offers any. A store that cannot be read counts as an
    // agent's.
    let by_agent_now = store.read().ok().is_none_or(|m| run_by_agent(&m));
    let offers = crate::checks::Offers::for_status(by_agent_now);
    let checked = crate::checks::run(env, &store, &offers).err();
    let mail = match store.read() {
        Ok(m) => m,
        Err(e) => return store_unusable(&e, "lunchroom status", "read"),
    };
    // C1.1: the agents, live ones before STOPPED ones; R3.3: each state from the vocabulary table,
    // never AVAILABLE or HELD without the evidence.
    let mut agents: Vec<_> = mail
        .names()
        .map(|(name, who)| {
            let judged = assess(&mail, who, &env.home);
            let state = mail.agent_state(who, judged.evidence());
            (name, who, state, judged)
        })
        .collect();
    agents.sort_by_key(|(_, _, state, _)| *state == AgentState::Stopped);
    let mut lines = String::new();
    // C1.1: one line first, how many agents and how many in each state.
    {
        let counts: Vec<String> = [
            AgentState::Available,
            AgentState::Held,
            AgentState::Unguarded,
            AgentState::Stopped,
        ]
        .into_iter()
        .filter_map(|s| {
            let n = agents.iter().filter(|(_, _, state, _)| *state == s).count();
            (n > 0).then(|| format!("{n} {}", s.word()))
        })
        .collect();
        let _ = writeln!(
            lines,
            "{} agent{}{}{}",
            agents.len(),
            if agents.len() == 1 { "" } else { "s" },
            if counts.is_empty() { "" } else { ": " },
            counts.join(", ")
        );
    }
    // P1.6: the hold mode in force.
    let _ = writeln!(lines, "hold mode: {}", mail.hold_mode().word());
    // C1.1 (G3-11): every call waiting for the user, named agent or not, as `lunchroom approve`
    // lists them; the lines below count them for each named agent.
    let waiting = mail.requests_total();
    if waiting > 0 {
        let _ = writeln!(
            lines,
            "calls waiting for {}: {waiting} (`lunchroom approve` lists them)",
            Word::TheUser.text()
        );
    }
    // P1.1: the slip, as shipped; every agent has this one and nothing can be added to it.
    if !agents.is_empty() {
        let _ = writeln!(lines, "slip: {SLIP}");
    }
    // G1-11: an agent running `status` reads another session's reason without its paths; the user
    // reads the whole reason.
    let by_agent = run_by_agent(&mail);
    for (name, who, state, judged) in agents {
        agent_lines(&mut lines, &mail, name, who, state, env.now);
        // R3.4: each UNGUARDED agent's one reason, the first its evidence lacks, and what the user
        // does (for Codex the setting or hook at fault, as the notes on `unified_exec_tty` said).
        if let Assessment::Unguarded(why) = judged {
            let text = if by_agent {
                why.text_for_agent()
            } else {
                why.text()
            };
            let _ = writeln!(lines, "  why: {text}");
        }
    }
    queued_lines(&mut lines, &mail, env.now, checked.as_deref());
    // An agent whose session cannot be established (`sender` fails) reads no notices.
    let reader: Option<Option<Identity>> = if by_agent {
        sender(&mail, env).ok().map(Some)
    } else {
        Some(None)
    };
    if let Some(agent) = reader {
        notice_lines(&mut lines, &mail, agent.as_ref());
    }
    // C1.1: each HALTED pair and why.
    let name = |key: &str| {
        mail.names()
            .find(|(_, who)| who.key() == key)
            .map_or_else(|| key.to_owned(), |(n, _)| n.to_owned())
    };
    for (a, b, reason) in mail.halted_pairs() {
        let _ = writeln!(
            lines,
            "{}\t{} and {}\t{}",
            Word::Halted.text(),
            name(a),
            name(b),
            reason.word()
        );
    }
    Outcome::ok(lines)
}

/// G4-2, G4-3: the notices not yet told, so none is lost where a harness's prompt hook can show
/// nothing (OpenClaw shows neither the user nor the agent anything; Hermes shows the user nothing).
/// The user, running `status` in a terminal of their own, reads the notices waiting for them; an
/// agent reads those waiting for its own session as a sender (its messages), never the user's.
/// Reading marks nothing told: the next prompt or listener that can show them still tells them.
fn notice_lines(lines: &mut String, mail: &Mail, agent: Option<&Identity>) {
    let notices: &[lunchroom_core::mail::Notice] = match agent {
        Some(me) => mail.notices(me),
        None => mail.user_notices(),
    };
    if notices.is_empty() {
        return;
    }
    let _ = writeln!(lines, "notices not yet told:");
    for n in notices {
        let _ = writeln!(lines, "  {}", n.text);
    }
}

/// This command runs under a harness process a guard hook recorded: an agent runs it, not the user
/// in a terminal of their own. Any recorded process above this one counts, in a gateway that runs
/// many sessions too.
fn run_by_agent(mail: &Mail) -> bool {
    process::ancestors(std::process::id())
        .into_iter()
        .skip(1)
        .any(|pid| process::key(pid).is_some_and(|k| mail.process_session(&k).is_some()))
}

/// `lunchroom codex-threads [-n COUNT]`: the most recent Codex threads, newest first, from the
/// names of Codex's session files and its session index (thread names only; no session's
/// contents are read), each with the name it is registered under, for `lunchroom register`.
#[must_use]
pub fn codex_threads(args: &[String], env: &Env) -> Outcome {
    let limit = match args.iter().position(|a| a == "-n") {
        Some(k) => match args.get(k + 1).and_then(|n| n.parse::<usize>().ok()) {
            Some(n) => n,
            None => return Outcome::failed("usage: lunchroom codex-threads [-n COUNT]", 2),
        },
        None => 6,
    };
    // Thread id -> (start stamp from the file name, last change, seconds since the epoch).
    let mut threads: std::collections::BTreeMap<String, (String, u64)> =
        std::collections::BTreeMap::new();
    let mut dirs = vec![(env.codex_home.join("sessions"), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && depth < 4 {
                dirs.push((path, depth + 1));
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name
                .strip_prefix("rollout-")
                .and_then(|n| n.strip_suffix(".jsonl"))
            else {
                continue;
            };
            // `<YYYY-MM-DDTHH-MM-SS>-<thread id, 36 characters>[...]`, as the prototype reads it.
            let Some((started, id)) = stem
                .get(..19)
                .zip(stem.get(20..56))
                .filter(|(_, id)| id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
                .map(|(t, id)| (t.to_owned(), id.to_owned()))
            else {
                continue;
            };
            let changed = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let newer = threads.get(&id).is_none_or(|(_, at)| changed > *at);
            if newer {
                threads.insert(id, (started, changed));
            }
        }
    }
    // Thread names, from Codex's session index; a later line is a newer rename.
    let mut names = std::collections::BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(env.codex_home.join("session_index.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str()) {
                names.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    let registered = store(env, "lunchroom codex-threads")
        .ok()
        .and_then(|s| s.read().ok());
    let mut rows: Vec<(&String, &(String, u64))> = threads.iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1.1));
    let mut out = String::new();
    for (id, (started, changed)) in rows.into_iter().take(limit) {
        let title = names.get(id).map_or_else(
            || "(no name)".to_owned(),
            |n| {
                format!(
                    "\"{}\"",
                    n.chars().filter(|c| !c.is_control()).collect::<String>()
                )
            },
        );
        let name = registered
            .as_ref()
            .and_then(|m| m.name_of(&Identity::new(Harness::Codex, id)))
            .unwrap_or_default()
            .to_owned();
        let _ = writeln!(
            out,
            "{id}  started {started}  last-write {}  {title}  {name}",
            crate::delivery::utc(*changed)
        );
    }
    Outcome::ok(out)
}

/// What `lunchroom doctor` reads of the computer besides the user's own files: taken from this
/// process in the product, set to a scratch folder in tests.
pub(crate) struct Host<'a> {
    /// The user's passphrase check (A1.3).
    pub sys: &'a dyn System,
    /// The user's `PATH`.
    pub path: Option<std::ffi::OsString>,
    /// The folder doctor runs in. Doctor checks a setup, not a session: the Claude Code and Codex
    /// project settings it reads are the ones a session started in this folder would read.
    pub cwd: std::path::PathBuf,
    /// The process whose `PATH` a hook's bare program name is looked up in, where the existing
    /// checks take a session's process: doctor's own, since no session is named.
    pub pid: u32,
    /// Claude Code's user settings folder (`CLAUDE_CONFIG_DIR`, or `~/.claude`).
    pub claude_config: std::path::PathBuf,
    /// Claude Code's managed settings file.
    pub managed: std::path::PathBuf,
    /// The `openclaw.json` OpenClaw reads.
    pub openclaw_config: std::path::PathBuf,
    /// The folder the system's package records are under (`/`).
    pub root: std::path::PathBuf,
}

/// The lines of `lunchroom doctor`, and how many checks failed.
#[derive(Default)]
struct Checks {
    text: String,
    failed: usize,
    /// A HELD line has been printed, so the word has been explained.
    explained_held: bool,
}

impl Checks {
    /// The line of an agent that is HELD. It is neither a PASS nor a FAIL: the agent's guard runs,
    /// and the hold is Lunchroom doing its work. The first one says what HELD means.
    fn held(&mut self, agent: &str, by: &str) {
        let meaning = if self.explained_held {
            ""
        } else {
            self.explained_held = true;
            " HELD means that text from another agent is in a session, so Lunchroom restricts \
             what the agent may do until the user types in the session."
        };
        let _ = writeln!(
            self.text,
            "HELD agent {agent}: HELD, because {by} is in its session.{meaning}"
        );
    }

    /// One check's line: `PASS <what>`, or `FAIL <what is wrong>: <what the user does>`.
    fn line(&mut self, result: Result<String, String>) {
        match result {
            Ok(line) => {
                let _ = writeln!(self.text, "PASS {line}");
            }
            Err(line) => {
                self.failed += 1;
                let _ = writeln!(self.text, "FAIL {line}");
            }
        }
    }
}

/// `path` is an executable regular file.
fn runnable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program `name` the shell runs from `path`: the first folder of `path` holding an executable
/// file of that name (a relative folder is taken from `cwd`).
fn on_path(
    path: Option<&std::ffi::OsStr>,
    name: &str,
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| cwd.join(dir).join(name))
        .find(|p| runnable(p))
}

/// The agent a guard record is about, by the name the user gave it (the record holds the harness's
/// own session id), as "the session of <name> (<harness>)"; `a session with no agent name` when
/// `mail` has no agent of that session.
fn record_agent(mail: Option<&Mail>, session: &str) -> String {
    mail.and_then(|m| {
        m.names()
            .find(|(_, who)| who.session == session)
            .map(|(name, who)| {
                format!(
                    "the session of {name} ({})",
                    crate::evidence::harness_name(who.harness)
                )
            })
    })
    .unwrap_or_else(|| "a session with no agent name".to_owned())
}

/// C1.4 (docs/spec/lunchroom-spec.md §14 item 4): `lunchroom doctor` shows the guard's records of
/// the last 24 hours (H1.8), and FAILs for each call the guard could not decide in that time, with
/// what the user does; a record older than 24 hours, or one that was only slow, does not FAIL. Each
/// slow decision is a PASS line of its own; with no undecided call, one PASS line says so. A FAIL
/// line names the agent and says when it clears (24 hours after the call).
/// The doctor lines about the guard's records of the last 24 hours, as text, for the console
/// (1.1, feature `console`).
#[cfg(feature = "console")]
pub(crate) fn guard_record_text(env: &Env, mail: Option<&Mail>) -> String {
    let mut checks = Checks {
        text: String::new(),
        failed: 0,
        explained_held: false,
    };
    guard_record_lines(&mut checks, env, mail);
    checks.text
}

fn guard_record_lines(checks: &mut Checks, env: &Env, mail: Option<&Mail>) {
    let all = crate::records::read(&env.lunchroom_home);
    let recent = crate::records::recent(&all, env.now);
    let mut undecided = 0;
    for r in &recent {
        let at = lunchroom_core::mail::clock(r.at);
        let agent = record_agent(mail, &r.session);
        match &r.undecided {
            Some(reason) => {
                undecided += 1;
                let clears = r.at + crate::records::SHOWN_SECS;
                let day = crate::delivery::utc(clears);
                checks.line(Err(format!(
                    "Lunchroom could not decide on {} at {at} in {agent}: {}. Its slowest step \
                     was {} ({} ms). This line clears at {} on {}, 24 hours after the call. If \
                     the call is still needed, run it yourself in a terminal of your own. If the \
                     cause is Lunchroom's store, `lunchroom status` shows whether it can be read, \
                     and `lunchroom repair` puts back the last good copy.",
                    r.call,
                    crate::claude::undecided_cause(reason),
                    r.step,
                    r.step_ms,
                    lunchroom_core::mail::clock(clears),
                    day.get(..10).unwrap_or_default()
                )));
            }
            None => checks.line(Ok(format!(
                "a slow decision at {at} in {agent}: {} took {} ms; its slowest step was {} ({} \
                 ms)",
                r.call, r.took_ms, r.step, r.step_ms
            ))),
        }
    }
    if undecided == 0 {
        checks.line(Ok(
            "the guard decided every call in the last 24 hours".to_owned()
        ));
    }
}

/// The harness `lunchroom install` names by `word`.
fn harness_of(word: &str) -> Option<Harness> {
    match word {
        "claude-code" => Some(Harness::ClaudeCode),
        "codex" => Some(Harness::Codex),
        "openclaw" => Some(Harness::OpenClaw),
        "hermes" => Some(Harness::Hermes),
        _ => None,
    }
}

/// `lunchroom doctor` (C1.4, docs/spec/lunchroom-spec.md §14 C1 item 4): "`lunchroom doctor`
/// checks the user's setup and changes nothing. It prints one line for
/// each check, PASS or FAIL, and for each FAIL what the user does: Lunchroom's program is installed
/// and runs by name from the user's `PATH`; the passphrase check is in place (A1.3); for each
/// supported harness whose folder exists, Lunchroom is installed and the harness configuration runs
/// the whole guard, and the harness release is the release its adapter passed with or later
/// (H1.2): for Claude Code, Codex and OpenClaw read from the install of its program on the user's
/// `PATH`; for Hermes, whose release its plugin reads inside Hermes, the release the plugin
/// reported in its latest session, a FAIL until a Hermes session has run; and each running agent's
/// state, with its reason (R3.4) when UNGUARDED. It exits 0 when every check passes and 1
/// otherwise. It reads only local files and sends nothing anywhere."
#[must_use]
pub fn doctor(env: &Env, sys: &dyn System) -> Outcome {
    // G3-10: doctor shows where every agent's settings and programs are, so it runs only where the
    // user's commands run (A1.3): outside every guarded session, in a terminal. The guard also
    // DENIES it to an agent, but a renamed copy of the program is not caught by name.
    let mail = Store::<Mail>::read_without_change(&env.store_dir()).unwrap_or_default();
    if let Err(reason) = crate::terminal::check(&mail, sys) {
        return doctor_denied(&reason);
    }
    doctor_text(env, sys)
}

/// The checks of `lunchroom doctor` without its terminal check, for the console, which runs as the
/// `lunchroom` user in the person's own terminal behind the passphrase.
pub(crate) fn doctor_text(env: &Env, sys: &dyn System) -> Outcome {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let host = Host {
        sys,
        path: std::env::var_os("PATH"),
        cwd: std::env::current_dir().unwrap_or_else(|_| env.home.clone()),
        pid: std::process::id(),
        claude_config: var("CLAUDE_CONFIG_DIR")
            .map_or_else(|| env.home.join(".claude"), std::path::PathBuf::from),
        managed: std::path::PathBuf::from(crate::evidence::MANAGED_SETTINGS),
        openclaw_config: crate::evidence::openclaw_config_from(&var, &env.home),
        root: std::path::PathBuf::from("/"),
    };
    doctor_on(env, &host)
}

/// What `lunchroom doctor` says where it is not run by the user: why, and what to do.
pub(crate) fn doctor_denied(reason: &str) -> Outcome {
    Outcome::denied(
        &format!(
            "lunchroom doctor was not run, because {reason}. Doctor shows where every agent's \
             settings and programs are, so it runs only for the user: open a terminal of your \
             own, outside every agent session, and run lunchroom doctor there."
        ),
        3,
    )
}

/// [`doctor`] on `host`. Every check reads; none writes, locks or creates a file, and nothing is
/// sent anywhere (the one program it may run is `rpm -qf`, which reads rpm's database).
#[allow(clippy::too_many_lines)] // the checks of C1.4, in its order, one after another
pub(crate) fn doctor_on(env: &Env, host: &Host<'_>) -> Outcome {
    use crate::evidence::{self as ev, harness_name};
    let mut checks = Checks::default();
    // 1.1: where the console is set up (or this program runs setgid), which store is in force: a
    // PASS when it is out of reach of the user's own processes, a FAIL with what to do when not.
    let console_program = std::path::Path::new(lunchroom_core::setgid::PROGRAM);
    // 1.1 only (feature `console`): a 1.1.0 build prints no console line.
    #[cfg(feature = "console")]
    if console_program.is_file() || lunchroom_core::setgid::active() {
        checks.line(crate::console::mode_check(env, host.sys.uid()));
    }
    // Lunchroom's program, where `lunchroom install` puts it (in console mode, the setgid program
    // `lunchroom install console` puts in place), and the one run by name from PATH.
    let program = if lunchroom_core::setgid::active() {
        console_program.to_path_buf()
    } else {
        env.lunchroom_home.join("bin").join("lunchroom")
    };
    checks.line(if runnable(&program) {
        Ok(format!(
            "Lunchroom's program is installed at {}",
            program.display()
        ))
    } else {
        Err(format!(
            "Lunchroom's program is not installed at {}: in a terminal of your own, run `lunchroom \
             install` with the lunchroom program you have, which puts it there",
            program.display()
        ))
    });
    let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    checks.line(
        match on_path(host.path.as_deref(), "lunchroom", &host.cwd) {
            Some(found) if runnable(&program) && real(&found) == real(&program) => Ok(format!(
                "lunchroom runs by name from your PATH: {} is that program",
                found.display()
            )),
            Some(found) => Err(format!(
                "the lunchroom first on your PATH, {}, is not Lunchroom's installed program {}: \
             remove or rename it, or put a folder linking to {} before it on your PATH",
                found.display(),
                program.display(),
                program.display()
            )),
            None => Err(format!(
                "no program named lunchroom is on your PATH: put {} on your PATH and run lunchroom \
             install again in a terminal of your own, which links {} there",
                env.home.join(".local/bin").display(),
                program.display()
            )),
        },
    );
    // A1.3: the passphrase check, read as every command only the user may run reads it.
    checks.line(
        crate::terminal::trusted_verifier(host.sys)
            .map(|_| {
                format!(
                    "the passphrase check is in place ({})",
                    crate::terminal::verifier_shown(host.sys)
                )
            })
            .map_err(|why| format!("the passphrase check: {why}")),
    );
    // Lunchroom's records, read without creating or locking anything: Hermes's reported release
    // and the running agents come from them.
    let stored = Store::<Mail>::read_without_change(&env.store_dir());
    // Each supported harness whose folder exists, found as `lunchroom install` finds it (H1.4).
    for (word, _) in crate::install::found_harnesses_in(env, &host.claude_config) {
        let Some(harness) = harness_of(word) else {
            continue;
        };
        let name = harness_name(harness);
        let fault = match harness {
            Harness::ClaudeCode => {
                ev::claude_setup_fault(&host.claude_config, &host.managed, host.pid, &host.cwd)
            }
            Harness::Codex => ev::codex_guard_in(&env.codex_home, host.pid, &host.cwd)
                .err()
                .map(|fault| fault.text()),
            Harness::OpenClaw => ev::openclaw_fault(&host.openclaw_config).or_else(|| {
                ev::plugin_setup_fault(
                    harness,
                    &env.lunchroom_home.join("plugins/openclaw/index.mjs"),
                )
            }),
            Harness::Hermes => ev::hermes_fault(&env.hermes_home).or_else(|| {
                ev::plugin_setup_fault(
                    harness,
                    &env.hermes_home.join("plugins/lunchroom/__init__.py"),
                )
            }),
        };
        checks.line(fault.map_or_else(
            || {
                Ok(format!(
                    "{name}: Lunchroom is installed and its configuration runs the whole guard"
                ))
            },
            |fault| Err(format!("{name}: {fault}")),
        ));
        // H1.2: Claude Code, Codex and OpenClaw from the install of the program on PATH; Hermes,
        // the release its plugin reported in its latest session (C1.4).
        // G3-7: OpenClaw's plugin runs inside the gateway and reports the gateway's own release;
        // when it has, that is the release judged (and the line says so), not the `openclaw` that
        // happens to be first on the doctor's PATH.
        let release = if harness == Harness::Hermes {
            match &stored {
                Ok(mail) => ev::hermes_reported_release(mail, env.now),
                Err(e) => Err(format!(
                    "{}: run lunchroom repair in a terminal of your own",
                    unread(e)
                )),
            }
        } else if let Some(reported) = stored
            .as_ref()
            .ok()
            .filter(|_| harness == Harness::OpenClaw)
            .and_then(|mail| ev::reported_release(mail, harness, env.now))
        {
            reported
        } else {
            let found = on_path(host.path.as_deref(), ev::program_name(harness), &host.cwd)
                .map(|p| real(&p));
            ev::path_release(harness, found.as_deref(), &host.root)
        };
        checks.line(
            release
                .map(|release| {
                    format!(
                        "{name}: release {release}. This is the release Lunchroom was tested \
                         with, or a later one."
                    )
                })
                .map_err(|why| format!("{name}: {why}")),
        );
    }
    guard_record_lines(&mut checks, env, stored.as_ref().ok());
    // Each running agent's state, with its reason when UNGUARDED (R3.4).
    match stored {
        Err(e) => checks.line(Err(format!(
            "{}: run lunchroom repair in a terminal of your own",
            unread(e)
        ))),
        Ok(mail) => {
            if let Some(line) =
                older_writer_line(&mail, env.store_dir().join("state.json").is_file())
            {
                checks.line(Err(line));
            }
            for (name, who) in mail.names() {
                let judged = assess(&mail, who, &env.home);
                let state = mail.agent_state(who, judged.evidence());
                // The agent by the name the user gave it and its harness, never by session id.
                let agent = format!("{name} ({})", harness_name(who.harness));
                match judged {
                    Assessment::NotRunning => {}
                    // G3-9: a HELD agent is the guard at work, neither a pass nor a failure.
                    Assessment::Guarded if state == AgentState::Held => {
                        checks.held(&agent, &held_by_text(&mail, who));
                    }
                    Assessment::Guarded => checks.line(Ok(format!(
                        "agent {agent} is {}: its guard is running and no other agent's text is \
                         in its session",
                        state.word()
                    ))),
                    Assessment::Unguarded(why) => checks.line(Err(format!(
                        "agent {agent} is {} (running, but Lunchroom has no evidence that its \
                         guard runs, so it delivers nothing to it): {}",
                        state.word(),
                        why.text()
                    ))),
                }
            }
        }
    }
    Outcome {
        stdout: checks.text,
        stderr: String::new(),
        code: u8::from(checks.failed > 0),
    }
}

/// The doctor line for a store an older Lunchroom program wrote last (D1.2): a session started
/// before the upgrade still runs its old program, which drops the fields this version keeps. Only
/// when the store file exists (`stored`): a store that was never written reads as written by the
/// oldest program, and a fresh install has nothing to say.
fn older_writer_line(mail: &Mail, stored: bool) -> Option<String> {
    (stored && mail.written_by_older_program()).then(|| {
        "Lunchroom's store was last written by an older Lunchroom program. A session that was \
         started before the upgrade is still running its old program, which drops what this \
         version keeps. Start those sessions anew."
            .to_owned()
    })
}

/// What holds `who`, for the user: each message from another agent by its sender and id, and each
/// other source (a tool's output, a transcript) by name.
fn held_by_text(mail: &Mail, who: &Identity) -> String {
    let mut by: Vec<String> = Vec::new();
    for h in mail.holding(who) {
        let one = if mail.message(&h.message).is_some() {
            format!("a message from {} ({})", h.from_name, h.message)
        } else {
            format!("text from {}", h.from_name.replace('-', " "))
        };
        if !by.contains(&one) {
            by.push(one);
        }
    }
    if by.is_empty() {
        "text from another agent".to_owned()
    } else {
        by.join(" and ")
    }
}

/// The permission slip every agent has (P1.1): it ships as every read but the paths L1.1 denies or
/// protects, and the reply, and the user can add nothing that changes a file or runs a command.
pub const SLIP: &str =
    "read any file except the paths Lunchroom denies or protects; reply with lunchroom send";

#[cfg(test)]
mod tests_f1_doctor;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // An unknown name is answered with the name meant: the same up to case and `_` for `-`, or
    // the only one within two edits; with two that close, none is named.
    #[test]
    fn an_unknown_name_is_answered_with_the_closest_one() {
        let names = ["code-review", "glm", "kimi"].map(str::to_owned);
        assert_eq!(
            closest("code_review", &names).as_deref(),
            Some("code-review")
        );
        assert_eq!(
            closest("Code-Review", &names).as_deref(),
            Some("code-review")
        );
        assert_eq!(
            closest("code-reveiw", &names).as_deref(),
            Some("code-review")
        );
        assert_eq!(closest("kimmi", &names).as_deref(), Some("kimi"));
        assert_eq!(closest("someone", &names), None);
        let two = ["ab1", "ab2"].map(str::to_owned);
        assert_eq!(closest("ab", &two), None);
        assert_eq!(edits("kitten", "sitting"), 3);
    }

    // A stream is read to the limit and one byte more, no further: an endless one still ends.
    #[test]
    fn a_message_read_from_standard_input_stops_one_byte_past_the_limit() {
        let bytes = read_bounded(std::io::repeat(b'a')).unwrap_or_default();
        assert_eq!(bytes.len(), MAX_MESSAGE_BYTES + 1);
        let at = vec![b'a'; MAX_MESSAGE_BYTES];
        assert_eq!(
            read_bounded(at.as_slice()).map(|b| b.len()).ok(),
            Some(MAX_MESSAGE_BYTES)
        );
    }

    /// The passphrase check as the system reports it; nothing is asked.
    struct Verifier(Result<String, String>);

    impl System for Verifier {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            Vec::new()
        }
        fn own_tty(&self) -> u64 {
            0
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
            self.0.clone()
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            None
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            Err("no salt in this test".to_owned())
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Err("nothing is installed in this test".to_owned())
        }
    }

    /// A complete setup in a scratch folder: Lunchroom's program installed and linked by name,
    /// Claude Code and Codex configured with the whole guard (Codex's hooks trusted, its terminals
    /// off), `claude` at 2.1.309 (native install) and `codex` at 0.163.0 (npm package) on `PATH`.
    pub(crate) struct Setup {
        pub(crate) root: std::path::PathBuf,
        pub(crate) env: Env,
        pub(crate) path_dir: std::path::PathBuf,
    }

    pub(crate) const CLAUDE_EVENTS: [(&str, &str); 4] = [
        ("PreToolUse", "pretool"),
        ("PostToolUse", "posttool"),
        ("PostToolUseFailure", "posttool"),
        ("UserPromptSubmit", "prompt"),
    ];

    const CODEX_GUARD: [(&str, &str, &str); 4] = [
        ("PreToolUse", "pre_tool_use", "pretool"),
        ("PostToolUse", "post_tool_use", "posttool"),
        ("UserPromptSubmit", "user_prompt_submit", "prompt"),
        ("SessionStart", "session_start", "start"),
    ];

    impl Setup {
        pub(crate) fn new(name: &str) -> Self {
            let root = crate::test_target()
                .join("doctor-unit")
                .join(format!("{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            let env = Env {
                home: home.clone(),
                lunchroom_home: root.join("lunchroom"),
                codex_home: home.join(".codex"),
                claude_home: home.join(".claude"),
                hermes_home: home.join(".hermes"),
                claude_pid: None,
                attended: None,
                entrypoint: None,
                claude_session: None,
                codex_thread: None,
                hermes_session: None,
                lunchroom_session: None,
                now: 1000,
            };
            let s = Self {
                path_dir: root.join("path"),
                root,
                env,
            };
            for dir in [
                s.env.lunchroom_home.join("bin"),
                s.path_dir.clone(),
                home.join(".claude"),
                s.env.codex_home.clone(),
                s.root.join("project"),
                s.root.join("sysroot"),
            ] {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy("/bin/true", s.program());
            s.link("lunchroom", &s.program());
            s.claude_at("claude/versions/2.1.309");
            s.codex_at("0.163.0");
            s.claude_settings(&CLAUDE_EVENTS);
            s.codex_settings(
                true,
                &[
                    "pre_tool_use",
                    "post_tool_use",
                    "user_prompt_submit",
                    "session_start",
                ],
            );
            s
        }

        pub(crate) fn program(&self) -> std::path::PathBuf {
            self.env.lunchroom_home.join("bin/lunchroom")
        }

        /// `name` on `PATH`, a link to `target` (none: removed).
        pub(crate) fn link(&self, name: &str, target: &std::path::Path) {
            let link = self.path_dir.join(name);
            let _ = std::fs::remove_file(&link);
            let _ = std::os::unix::fs::symlink(target, &link);
        }

        /// `claude` on `PATH`, a program at `relative` under the scratch folder's installs.
        pub(crate) fn claude_at(&self, relative: &str) -> std::path::PathBuf {
            let program = self.root.join("inst").join(relative);
            if let Some(dir) = program.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy("/bin/true", &program);
            self.link("claude", &program);
            program
        }

        /// `codex` on `PATH`, the npm package `@openai/codex` at `version`.
        pub(crate) fn codex_at(&self, version: &str) {
            let package = self.root.join("inst/node_modules/@openai/codex");
            let _ = std::fs::create_dir_all(package.join("bin"));
            let _ = std::fs::write(
                package.join("package.json"),
                format!("{{\"name\": \"@openai/codex\", \"version\": \"{version}\"}}"),
            );
            let _ = std::fs::copy("/bin/true", package.join("bin/codex.js"));
            self.link("codex", &package.join("bin/codex.js"));
        }

        pub(crate) fn claude_settings(&self, events: &[(&str, &str)]) {
            let program = self.program().display().to_string();
            let mut hooks = serde_json::Map::new();
            for (event, mode) in events {
                hooks.insert(
                    (*event).to_owned(),
                    serde_json::json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}]),
                );
            }
            let _ = std::fs::write(
                self.env.home.join(".claude/settings.json"),
                serde_json::json!({ "hooks": hooks }).to_string(),
            );
        }

        /// Codex's hooks file with the whole guard set, `config.toml` with terminals off or not,
        /// and the user's trust for the hooks named by `trusted`.
        pub(crate) fn codex_settings(&self, tty_off: bool, trusted: &[&str]) {
            let program = self.program().display().to_string();
            let file = self.env.codex_home.join("hooks.json");
            let mut hooks = serde_json::Map::new();
            for (event, _, mode) in CODEX_GUARD {
                hooks.insert(
                    event.to_owned(),
                    serde_json::json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}]),
                );
            }
            let _ = std::fs::write(&file, serde_json::json!({ "hooks": hooks }).to_string());
            let mut config = format!("[features]\nunified_exec_tty = {}\n", !tty_off);
            for state in trusted {
                let _ = write!(
                    config,
                    "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"\n",
                    file.display()
                );
            }
            let _ = std::fs::write(self.env.codex_home.join("config.toml"), config);
        }

        /// `openclaw` on `PATH`: the file `file` of a package named `name` at `version` (none
        /// when empty); the program's real path.
        pub(crate) fn openclaw_at(
            &self,
            name: &str,
            file: &str,
            version: &str,
        ) -> std::path::PathBuf {
            let package = self.root.join("inst/openclaw-package");
            let _ = std::fs::remove_dir_all(&package);
            let _ = std::fs::create_dir_all(&package);
            let manifest = if version.is_empty() {
                serde_json::json!({ "name": name })
            } else {
                serde_json::json!({ "name": name, "version": version })
            };
            let _ = std::fs::write(package.join("package.json"), manifest.to_string());
            let program = package.join(file);
            let _ = std::fs::copy("/bin/true", &program);
            self.link("openclaw", &program);
            std::fs::canonicalize(&program).unwrap_or(program)
        }

        /// OpenClaw's folder, its settings enabling Lunchroom's plugin with conversation access,
        /// the plugin file where `lunchroom install openclaw` writes it, and `openclaw` 2026.10.7
        /// on `PATH`.
        pub(crate) fn add_openclaw(&self) {
            let _ = std::fs::create_dir_all(self.env.home.join(".openclaw"));
            let _ = std::fs::write(
                self.env.home.join(".openclaw/openclaw.json"),
                "{plugins: {entries: {lunchroom: {enabled: true, hooks: {allowConversationAccess: true}}}}}",
            );
            let plugins = self.env.lunchroom_home.join("plugins/openclaw");
            let _ = std::fs::create_dir_all(&plugins);
            let _ = std::fs::write(
                plugins.join("index.mjs"),
                format!("const PROGRAM = \"{}\";\n", self.program().display()),
            );
            self.openclaw_at("openclaw", "openclaw.mjs", "2026.10.7");
        }

        /// Hermes's folder, its `config.yaml` enabling Lunchroom's plugin, and the plugin file
        /// where `lunchroom install hermes` writes it.
        pub(crate) fn add_hermes(&self) {
            let plugin = self.env.hermes_home.join("plugins/lunchroom");
            let _ = std::fs::create_dir_all(&plugin);
            let _ = std::fs::write(
                self.env.hermes_home.join("config.yaml"),
                "plugins:\n  enabled:\n    - lunchroom\n",
            );
            let _ = std::fs::write(
                plugin.join("__init__.py"),
                format!("PROGRAM = \"{}\"\n", self.program().display()),
            );
        }

        /// The Hermes session `session`'s guard hook recording the plugin file and `release`,
        /// as the plugin reports them.
        pub(crate) fn hermes_report(&self, session: &str, release: &str) {
            let file = self.env.hermes_home.join("plugins/lunchroom/__init__.py");
            let who = Identity::new(Harness::Hermes, session);
            let recorded = Store::<Mail>::open(&self.env.store_dir()).and_then(|store| {
                store.update(|m| {
                    m.record_guard_fingerprint(
                        &who,
                        Some(crate::evidence::plugin_fingerprint_at(
                            &file,
                            release,
                            self.env.now.saturating_sub(3 * 3600 + 5 * 60),
                        )),
                    );
                })
            });
            assert!(recorded.is_ok(), "{recorded:?}");
        }

        pub(crate) fn doctor_with(&self, verifier: Result<String, String>) -> Outcome {
            let sys = Verifier(verifier);
            let host = Host {
                sys: &sys,
                path: Some(self.path_dir.clone().into_os_string()),
                cwd: self.root.join("project"),
                pid: std::process::id(),
                claude_config: self.env.home.join(".claude"),
                managed: self.root.join("managed-settings.json"),
                openclaw_config: self.env.home.join(".openclaw/openclaw.json"),
                root: self.root.join("sysroot"),
            };
            doctor_on(&self.env, &host)
        }

        pub(crate) fn doctor(&self) -> Outcome {
            self.doctor_with(Ok(crate::terminal::fake::verifier()))
        }

        /// Every file and folder under the scratch folder, with its kind, size and change time.
        fn tree(&self) -> Vec<(std::path::PathBuf, bool, u64, Option<std::time::SystemTime>)> {
            let mut out = Vec::new();
            let mut dirs = vec![self.root.clone()];
            while let Some(dir) = dirs.pop() {
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    let Ok(meta) = std::fs::symlink_metadata(&path) else {
                        continue;
                    };
                    if meta.is_dir() {
                        dirs.push(path.clone());
                    }
                    out.push((path, meta.is_dir(), meta.len(), meta.modified().ok()));
                }
            }
            out.sort();
            out
        }
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The FAIL lines of a doctor's output.
    fn fails(out: &Outcome) -> Vec<&str> {
        out.stdout
            .lines()
            .filter(|l| l.starts_with("FAIL "))
            .collect()
    }

    // §14::5, §14::6, §14::7 (C1.4, C1.5): on a complete setup every check of C1.4
    // passes, one line each (the program installed, it runs by name from PATH, the passphrase
    // check, and for Claude Code and Codex the configuration and the release read from the program
    // on PATH), and doctor exits 0; it changes no file: the scratch tree is the same before and
    // after, and no store is created.
    #[test]
    fn doctor_passes_a_complete_setup_and_changes_nothing() {
        let s = Setup::new("complete");
        let before = s.tree();
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert_eq!(fails(&out), Vec::<&str>::new());
        let lines: Vec<&str> = out.stdout.lines().collect();
        // C1.4 gained the guard's records: one PASS line with none undecided.
        assert_eq!(lines.len(), 8, "{}", out.stdout);
        assert!(
            lines.iter().all(|l| l.starts_with("PASS ")),
            "{}",
            out.stdout
        );
        for part in [
            "the guard decided every call in the last 24 hours",
            "Lunchroom's program is installed at",
            "lunchroom runs by name from your PATH",
            "the passphrase check is in place",
            "Claude Code: Lunchroom is installed and its configuration runs the whole guard",
            "Claude Code: release 2.1.309, read from",
            "Codex: Lunchroom is installed and its configuration runs the whole guard",
            "Codex: release 0.163.0, read from",
        ] {
            assert!(out.stdout.contains(part), "{part}: {}", out.stdout);
        }
        assert_eq!(s.tree(), before);
        assert!(!s.env.store_dir().exists());
        // With OpenClaw's and Hermes's folders too, each configured with Lunchroom's plugin,
        // `openclaw` 2026.10.7 on PATH and a Hermes session having reported 9976f48: still every
        // line PASS, exit 0, and no file changed.
        s.add_openclaw();
        s.add_hermes();
        s.hermes_report("h1", "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003");
        let before = s.tree();
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines.len(), 12, "{}", out.stdout);
        assert!(
            lines.iter().all(|l| l.starts_with("PASS ")),
            "{}",
            out.stdout
        );
        for part in [
            "OpenClaw: Lunchroom is installed and its configuration runs the whole guard",
            "OpenClaw: release 2026.10.7, read from",
            "Hermes: Lunchroom is installed and its configuration runs the whole guard",
            "Hermes: release 9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003",
        ] {
            assert!(out.stdout.contains(part), "{part}: {}", out.stdout);
        }
        assert_eq!(s.tree(), before);
    }

    // §14::5, §14::6 (C1.4, C1.5): each check fails, with its own fix, when its
    // condition is broken, and doctor then exits 1.
    #[test]
    #[allow(clippy::too_many_lines)] // one setup, each check broken on it in turn
    fn each_broken_check_fails_with_its_fix_and_exit_1() {
        let s = Setup::new("broken");
        let failing = |out: &Outcome, wanted: &[&str]| {
            assert_eq!(out.code, 1, "{}", out.stdout);
            let lines = fails(out);
            assert!(
                lines.iter().any(|l| wanted.iter().all(|w| l.contains(w))),
                "{wanted:?}: {}",
                out.stdout
            );
        };
        // Lunchroom's program missing.
        let _ = std::fs::remove_file(s.program());
        failing(
            &s.doctor(),
            &[
                "Lunchroom's program is not installed at",
                "lunchroom install",
            ],
        );
        let _ = std::fs::copy("/bin/true", s.program());
        // Not on PATH; another lunchroom first on PATH.
        let _ = std::fs::remove_file(s.path_dir.join("lunchroom"));
        failing(
            &s.doctor(),
            &[
                "no program named lunchroom is on your PATH",
                "lunchroom install",
            ],
        );
        let other = s.root.join("inst/other-lunchroom");
        let _ = std::fs::create_dir_all(s.root.join("inst"));
        let _ = std::fs::copy("/bin/true", &other);
        s.link("lunchroom", &other);
        failing(
            &s.doctor(),
            &[
                "is not Lunchroom's installed program",
                "remove or rename it",
            ],
        );
        s.link("lunchroom", &s.program());
        assert_eq!(s.doctor().code, 0);
        // The passphrase check: none set; one that cannot be trusted.
        failing(
            &s.doctor_with(Err(crate::terminal::NO_VERIFIER.to_owned())),
            &["the passphrase check:", "lunchroom passphrase"],
        );
        failing(
            &s.doctor_with(Ok("not a verifier".to_owned())),
            &[
                "the passphrase check:",
                "cannot be used",
                "lunchroom passphrase",
            ],
        );
        // Claude Code: a guard hook missing.
        s.claude_settings(&CLAUDE_EVENTS[..3]);
        failing(
            &s.doctor(),
            &[
                "Claude Code: Claude Code's settings",
                "lunchroom install claude-code",
            ],
        );
        s.claude_settings(&CLAUDE_EVENTS);
        // Claude Code's release: older; unreadable; none on PATH; read from dpkg's records.
        s.claude_at("claude/versions/2.1.308");
        failing(
            &s.doctor(),
            &[
                "Claude Code: Claude Code 2.1.308 is older than 2.1.309",
                "update Claude Code",
            ],
        );
        let plain = s.claude_at("bin/claude");
        failing(
            &s.doctor(),
            &[
                "Claude Code: Claude Code's release cannot be read from the install of its program",
                "claude-code",
            ],
        );
        let listed = std::fs::canonicalize(&plain).unwrap_or(plain);
        let dpkg = s.root.join("sysroot/var/lib/dpkg");
        let _ = std::fs::create_dir_all(dpkg.join("info"));
        let _ = std::fs::write(
            dpkg.join("info/claude-code.list"),
            format!("/usr\n{}\n", listed.display()),
        );
        let _ = std::fs::write(
            dpkg.join("status"),
            "Package: claude-code\nStatus: install ok installed\nVersion: 2.1.309-1\n",
        );
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(
            out.stdout
                .contains("PASS Claude Code: release 2.1.309-1, read from"),
            "{}",
            out.stdout
        );
        let _ = std::fs::remove_file(s.path_dir.join("claude"));
        failing(
            &s.doctor(),
            &["Claude Code: no program named claude is on your PATH"],
        );
        s.claude_at("claude/versions/2.1.309");
        // Codex: terminals on; a hook not trusted; an older release.
        s.codex_settings(
            false,
            &[
                "pre_tool_use",
                "post_tool_use",
                "user_prompt_submit",
                "session_start",
            ],
        );
        failing(
            &s.doctor(),
            &[
                "Codex: Codex sessions are UNGUARDED until unified_exec_tty is false",
                "lunchroom install codex",
            ],
        );
        s.codex_settings(
            true,
            &["pre_tool_use", "post_tool_use", "user_prompt_submit"],
        );
        failing(
            &s.doctor(),
            &[
                "Codex: Codex has not trusted Lunchroom's SessionStart hook",
                "/hooks",
            ],
        );
        s.codex_settings(
            true,
            &[
                "pre_tool_use",
                "post_tool_use",
                "user_prompt_submit",
                "session_start",
            ],
        );
        s.codex_at("0.162.0");
        failing(&s.doctor(), &["Codex: Codex 0.162.0 is older than 0.163.0"]);
        s.codex_at("0.163.0");
        assert_eq!(s.doctor().code, 0);
        // Hermes's folder exists: its plugin is not enabled, and no Hermes session has reported
        // its release yet.
        let _ = std::fs::create_dir_all(&s.env.hermes_home);
        let out = s.doctor();
        failing(
            &out,
            &[
                "Hermes: Hermes's settings",
                "hermes plugins enable lunchroom",
            ],
        );
        failing(
            &out,
            &[
                "Hermes: no Hermes session has reported its release",
                "start a Hermes session, then run lunchroom doctor again",
            ],
        );
        let _ = std::fs::remove_dir_all(&s.env.hermes_home);
        assert_eq!(s.doctor().code, 0);
    }

    // §14::5 (C1.4: "for Claude Code, Codex and OpenClaw read from the install of its
    // program on the user's `PATH`"): OpenClaw's release is the `version` of the `package.json`
    // named `openclaw` beside the `openclaw.mjs` the `openclaw` on PATH runs. The stated release
    // passes; an older one FAILs naming both; a program that is not `openclaw.mjs`, a package of
    // another name, or no version FAILs as unreadable naming the program; none on PATH FAILs.
    #[test]
    fn doctor_reads_openclaws_release_from_the_program_on_path() {
        let s = Setup::new("openclaw");
        s.add_openclaw();
        let release = |out: &Outcome| {
            out.stdout
                .lines()
                .find(|l| l.contains("OpenClaw: release") || l.contains("OpenClaw: OpenClaw"))
                .unwrap_or_default()
                .to_owned()
        };
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(
            out.stdout
                .contains("PASS OpenClaw: release 2026.10.7, read from"),
            "{}",
            out.stdout
        );
        s.openclaw_at("openclaw", "openclaw.mjs", "2026.10.6");
        let out = s.doctor();
        assert_eq!(out.code, 1);
        let line = release(&out);
        assert!(
            line.starts_with("FAIL OpenClaw: OpenClaw 2026.10.6 is older than 2026.10.7"),
            "{}",
            out.stdout
        );
        for (name, file, version) in [
            ("openclaw", "cli.mjs", "2026.10.7"),
            ("other", "openclaw.mjs", "2026.10.7"),
            ("openclaw", "openclaw.mjs", ""),
        ] {
            let program = s.openclaw_at(name, file, version);
            let out = s.doctor();
            assert_eq!(out.code, 1, "{name} {file} {version}");
            let line = release(&out);
            assert!(
                line.starts_with("FAIL OpenClaw: OpenClaw's release cannot be read")
                    && line.contains(&program.display().to_string()),
                "{name} {file} {version}: {}",
                out.stdout
            );
        }
        let _ = std::fs::remove_file(s.path_dir.join("openclaw"));
        let out = s.doctor();
        assert!(
            fails(&out)
                .iter()
                .any(|l| l.starts_with("FAIL OpenClaw: no program named openclaw is on your PATH")),
            "{}",
            out.stdout
        );
    }

    // §14::5 (C1.4: "for Hermes, whose release its plugin reads inside Hermes, the
    // release the plugin reported in its latest session, a FAIL until a Hermes session has run"):
    // no Hermes session recorded FAILs with the fix to start one; the stated release reported
    // passes; an older one FAILs naming both; the latest report is the one read; a report the
    // evidence cannot read FAILs.
    #[test]
    fn doctor_reads_hermes_release_from_its_latest_session() {
        let s = Setup::new("hermes");
        s.add_hermes();
        let out = s.doctor();
        assert_eq!(out.code, 1);
        assert!(
            fails(&out).iter().any(|l| l.starts_with("FAIL Hermes: ")
                && l.ends_with("start a Hermes session, then run lunchroom doctor again.")),
            "{}",
            out.stdout
        );
        let passed = "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003";
        let older = "0123456789abcdef0123456789abcdef01234567@1793038002";
        s.hermes_report("h1", passed);
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(
            out.stdout
                .contains(&format!("PASS Hermes: release {passed}")),
            "{}",
            out.stdout
        );
        // Another, later session reports an older release: that latest report is read.
        s.hermes_report("h2", older);
        let out = s.doctor();
        assert_eq!(out.code, 1);
        assert!(
            fails(&out).iter().any(|l| l.starts_with(&format!(
                "FAIL Hermes: Hermes {older} is older than commit 9976f4857b72439d79016354d7b6e1a3ee952e2f"
            ))),
            "{}",
            out.stdout
        );
        s.hermes_report("h1", "0123456789abcdef0123456789abcdef01234567");
        let out = s.doctor();
        assert!(
            fails(&out).iter().any(|l| l
                .starts_with("FAIL Hermes: the release Lunchroom's plugin reported")
                && l.contains("cannot be read")),
            "{}",
            out.stdout
        );
    }

    // §14::5, §14::7, §4::9 (C1.4, R3.4): each running agent's state is a line, an
    // UNGUARDED one FAILs with its reason and what the user does; a stopped agent has no line; a
    // store that cannot be read FAILs with its fix; and the store is read without being changed.
    #[test]
    fn doctor_shows_each_running_agent_with_its_reason() {
        doctor_agents_case();
    }

    // §14::5 (C1.4: the guard's records), §13 H1.8: doctor shows the records of the
    // last 24 hours; an undecided call in them FAILs with its call, cause and step and what the
    // user does; a slow decision is shown and does not FAIL; a record exactly 24 hours old is
    // shown, one a second older is not; doctor still changes no file.
    #[test]
    fn doctor_shows_the_guards_records_of_the_last_24_hours() {
        let mut s = Setup::new("records");
        let now = 1_793_000_000;
        s.env.now = now;
        let record = |at: u64, undecided: Option<&str>| crate::records::Record {
            at,
            session: "019a3c1e-5b2f".to_owned(),
            hook: "pretool".to_owned(),
            call: "the command `git push origin main`".to_owned(),
            step: "searching below the folder /home/u".to_owned(),
            step_ms: 9_812,
            took_ms: 10_004,
            undecided: undecided.map(str::to_owned),
        };
        let home = s.env.lunchroom_home.clone();
        // Only slow, and an undecided call one second past 24 hours: no FAIL.
        for r in [
            record(now - 60, None),
            record(
                now - crate::records::SHOWN_SECS - 1,
                Some(
                    "no answer within 10 seconds, while the guard was searching below the folder /home/u",
                ),
            ),
        ] {
            assert_eq!(crate::records::add(&home, &r), Ok(()));
        }
        let before = s.tree();
        let out = s.doctor();
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(
            out.stdout.contains("PASS a slow decision at ")
                && out.stdout.contains("took 10004 ms")
                && out
                    .stdout
                    .contains("PASS the guard decided every call in the last 24 hours"),
            "{}",
            out.stdout
        );
        assert_eq!(s.tree(), before);
        // An undecided call exactly 24 hours old: shown, and a FAIL with what to do.
        assert_eq!(
            crate::records::add(
                &home,
                &record(
                    now - crate::records::SHOWN_SECS,
                    Some("Lunchroom's store cannot be updated: Permission denied (os error 13)")
                )
            ),
            Ok(())
        );
        let out = s.doctor();
        assert_eq!(out.code, 1, "{}", out.stdout);
        let lines = fails(&out);
        assert_eq!(lines.len(), 1, "{}", out.stdout);
        // G3-8: the agent is named (not the session id), and the line says when it clears: 24
        // hours after the call (here 1793000000, 2026-10-26 at 07:33 UTC).
        for part in [
            "FAIL Lunchroom could not decide on the command `git push origin main` at ",
            "in a session with no agent name: Lunchroom's store, the folder where it keeps its \
             records, could not be used (Lunchroom's store cannot be updated: Permission denied \
             (os error 13)).",
            "Its slowest step was searching below the folder /home/u (9812 ms).",
            "This line clears at 07:33 UTC on 2026-10-26, 24 hours after the call.",
            "If the call is still needed, run it yourself in a terminal of your own.",
            "`lunchroom repair` puts back the last good copy.",
        ] {
            assert!(lines[0].contains(part), "{part}\n{}", out.stdout);
        }
        assert!(!lines[0].contains("019a3c1e-5b2f"), "{}", out.stdout);
        assert!(
            !out.stdout.contains("the guard decided every call"),
            "{}",
            out.stdout
        );
    }

    fn doctor_agents_case() {
        let s = Setup::new("agents");
        let store = Store::<Mail>::open(&s.env.store_dir());
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        let process = process::key(std::process::id()).unwrap_or_default();
        let claw = Identity::new(Harness::OpenClaw, "s1");
        let gone = Identity::new(Harness::ClaudeCode, "c9");
        let named = store.update(|m| {
            m.observe_process(&process, &s.root, &claw);
            // No process has this pid: the agent is STOPPED.
            m.observe_process("4294967294:1", &s.root, &gone);
            m.register("Claw", &claw, lunchroom_core::mail::Origin::User, None)
                .is_ok()
                && m.register("Gone", &gone, lunchroom_core::mail::Origin::User, None)
                    .is_ok()
        });
        assert_eq!(named.ok(), Some(true));
        let before = s.tree();
        let out = s.doctor();
        assert_eq!(out.code, 1, "{}", out.stdout);
        let line = fails(&out)
            .into_iter()
            .find(|l| {
                l.starts_with(
                    "FAIL agent Claw (OpenClaw) is UNGUARDED (running, but Lunchroom has no \
                     evidence that its guard runs, so it delivers nothing to it): ",
                )
            })
            .unwrap_or_default();
        // G3-8: the agent is told by its name and harness, never by its session id.
        assert!(!line.contains("s1)"), "{}", out.stdout);
        assert!(
            line.contains("OpenClaw's settings")
                && line.contains("openclaw plugins enable lunchroom"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("Gone"), "{}", out.stdout);
        assert_eq!(s.tree(), before);
        let _ = std::fs::write(s.env.store_dir().join("state.json"), "{ broken");
        let out = s.doctor();
        assert_eq!(out.code, 1);
        assert!(
            fails(&out)
                .iter()
                .any(|l| l.contains("Lunchroom's store cannot be read")
                    && l.contains("lunchroom repair")),
            "{}",
            out.stdout
        );
    }

    // §1 V1.4, V1.5 (the reasons `lunchroom send` DENIES a message for, and its other
    // refusals): each message keeps the table's label, names the message DENIED (never its text,
    // which may carry a secret), the rule, the agents in the pair, for a HELD sender the holding
    // agent and message id, and each way forward that exists for it; and the exit codes stay.
    #[test]
    fn each_refused_message_explains_what_why_who_holds_and_the_ways_forward() {
        let held = ["alice (lr-00000045)".to_owned()];
        let secret = format!("{}{}", "sk", "-a1b2c3d4e5f6g7h8i9j0k1l2");
        let carrying = format!("Here it is: {secret}");
        let r = |text: &'static str| Refused {
            to: "Kimi",
            from: "builder",
            text,
            held_by: &[],
        };
        // Leak: the text is not shown.
        let out = refused(
            &SendRefusal::Denied(Reason::Leak),
            &Refused {
                text: &carrying,
                ..r("")
            },
        );
        assert_eq!(out.code, 7);
        assert!(!out.stderr.contains(&secret), "{}", out.stderr);
        // The whole text, word for word (G1-8 changed a DENIED first line for a path with a
        // fault; a message refused by the send rules keeps the reason's label).
        assert_eq!(
            out.stderr,
            format!(
                "DENIED · leak: the text carries or asks for a secret; remove it and send again.\n\
                 What was DENIED: the message to Kimi ({} bytes).\n\
                 Why: a message that carries a secret, or asks for one, is DENIED.\n\
                 Ways forward:\n\
                 - The agent: send it again without the secret, and without asking for one.\n",
                carrying.len()
            )
        );
        // Halted: card 3's line first, then the pair, and the user's way out.
        let out = refused(&SendRefusal::Denied(Reason::Halted), &r("Again."));
        assert_eq!(out.code, 4);
        assert_eq!(
            out.stderr,
            "Conversation halted.\n\
             DENIED · halted: this pair is HALTED: Lunchroom stopped their conversation, and the \
             user typing in either session clears it.\n\
             What was DENIED: the message to Kimi (6 bytes).\n\
             Why: a conversation between these two agents is HALTED: no message passes between \
             them until the user types in either session; the two agents are builder and Kimi.\n\
             Ways forward:\n\
             - The agent: wait; nothing passes between builder and Kimi until the user types.\n\
             - The user: type in either session (builder and Kimi) to clear it.\n"
        );
        // Rate limit: the agent waits; the user can change the limit.
        let out = refused(&SendRefusal::Denied(Reason::RateLimit), &r("Again."));
        assert_eq!(out.code, 5);
        assert_eq!(
            out.stderr,
            "DENIED · rate limit: this pair passed the rate limit; wait, then send again.\n\
             What was DENIED: the message to Kimi (6 bytes).\n\
             Why: these two agents passed the rate limit the user set for messages between two \
             agents; the two agents are builder and Kimi.\n\
             Ways forward:\n\
             - The agent: wait, then send again.\n\
             - The user: `lunchroom rate <messages> <seconds>`, in a terminal of their own, sets \
             the limit.\n"
        );
        // Loop: something new.
        let out = refused(&SendRefusal::Denied(Reason::Loop), &r("Again."));
        assert!(
            out.stderr.contains("- The agent: send something new."),
            "{}",
            out.stderr
        );
        // Not a holder: the holders, by name and message id, and both ways.
        let out = refused(
            &SendRefusal::NotHolder,
            &Refused {
                held_by: &held,
                ..r("Hi.")
            },
        );
        assert_eq!(out.code, 7);
        for part in [
            "DENIED · a HELD session may reply only",
            "What was DENIED: the message to Kimi (3 bytes).",
            "Kimi is not one of them. This session is HELD by alice (lr-00000045).",
            "- The agent: reply to alice with `lunchroom send <name> <text>`.",
            "- The user: type in this session to clear the hold",
        ] {
            assert!(out.stderr.contains(part), "{part}: {}", out.stderr);
        }
        // Not named, and an unseen sender: the user's ways.
        let out = refused(&SendRefusal::NotNamed, &r("Hi."));
        assert!(
            out.stderr.contains("`lunchroom register <name>"),
            "{}",
            out.stderr
        );
        let out = refused(&SendRefusal::UnseenSender, &r("Hi."));
        assert!(out.stderr.contains("`lunchroom install`"), "{}", out.stderr);
        for out in [
            refused(&SendRefusal::Denied(Reason::RateLimit), &r("x")),
            refused(&SendRefusal::NameOnlyHolder, &r("x")),
        ] {
            assert_eq!(
                lunchroom_core::vocab::find_retired_word(&out.stderr),
                None,
                "{}",
                out.stderr
            );
        }
    }

    // G4-2, G4-3: `status` lists the notices not yet told (a harness whose prompt hook shows
    // nothing leaves them there): the user reads the user's, an agent reads its own as a sender,
    // and reading removes none.
    #[test]
    fn status_lists_the_notices_not_yet_told() {
        use lunchroom_core::mail::{Origin, WAITING_TOLD_SECS};
        let sender = Identity::new(Harness::OpenClaw, "s1");
        let other = Identity::new(Harness::ClaudeCode, "o1");
        let mut mail = Mail::default();
        for (name, who) in [("sender", &sender), ("other", &other)] {
            mail.observe(who);
            assert!(mail.register(name, who, Origin::User, None).is_ok());
        }
        let id = mail
            .send(&sender, "sender", "other", "hi", 0)
            .map(|a| a.id)
            .unwrap_or_default();
        mail.note_unreachable(&id, Some("other is STOPPED.".to_owned()));
        mail.tell_waiting(WAITING_TOLD_SECS);
        let expect = |notices: &[lunchroom_core::mail::Notice]| {
            let mut out = String::from("notices not yet told:\n");
            for n in notices {
                out.push_str("  ");
                out.push_str(&n.text);
                out.push('\n');
            }
            out
        };
        let mut for_user = String::new();
        notice_lines(&mut for_user, &mail, None);
        assert_ne!(mail.user_notices(), []);
        assert_eq!(for_user, expect(mail.user_notices()));
        let mut for_agent = String::new();
        notice_lines(&mut for_agent, &mail, Some(&sender));
        assert_ne!(mail.notices(&sender), []);
        assert_eq!(for_agent, expect(mail.notices(&sender)));
        // Another session's notices are not its to read; nothing waiting, nothing printed.
        let mut none = String::new();
        notice_lines(&mut none, &mail, Some(&other));
        assert_eq!(none, "");
    }

    // The refusal is a DENIED outcome that names the limit.
    #[test]
    fn the_over_limit_refusal_names_the_limit() {
        let out = too_long("alice");
        assert!(
            out.stderr.starts_with(Verdict::Denied.word()),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("65536 bytes"), "{}", out.stderr);
        assert_eq!(out.code, 2);
    }

    // G3-16: every refusal of `lunchroom send` says what was DENIED, why, and what to do, in full
    // (empty message, unreadable text, forged session id, over the limit, too many lines, unknown
    // name, a store that cannot be used), each with its exit code.
    #[test]
    fn each_send_failure_says_what_why_and_what_to_do() {
        let out = empty_text("alice");
        assert_eq!(out.code, 2);
        assert_eq!(
            out.stderr,
            "DENIED · the message has no text\n\
             What was DENIED: the message to alice.\n\
             Why: a message needs text for alice to read, and this one is empty or only spaces.\n\
             Ways forward:\n\
             - The agent: put the text after the name, for example `lunchroom send alice \"the \
             build passed\"`; a text of `-` is read from standard input.\n"
        );
        let out = unreadable_text("alice");
        assert_eq!(out.code, 2);
        assert_eq!(
            out.stderr,
            "DENIED · the message text cannot be read\n\
             What was DENIED: the message to alice.\n\
             Why: the text given on standard input is not valid UTF-8 text, or could not be read.\n\
             Ways forward:\n\
             - The agent: give the text as the command's argument, or pipe UTF-8 text into \
             `lunchroom send alice -`.\n"
        );
        let out = forged("alice");
        assert_eq!(out.code, 3);
        assert_eq!(
            out.stderr,
            "DENIED · the session id in this command's environment is not the session it runs in\n\
             What was DENIED: the message to alice.\n\
             Why: Lunchroom stamps a message with the session it really runs in, found from the \
             process that started it, and does not take another session's id from the \
             environment.\n\
             Ways forward:\n\
             - The agent: run `lunchroom send` again without setting a session id in the \
             command's environment (CLAUDE_CODE_SESSION_ID, CODEX_THREAD_ID, HERMES_SESSION_ID \
             or LUNCHROOM_SESSION).\n"
        );
        let out = too_long("alice");
        assert_eq!(out.code, 2);
        assert_eq!(
            out.stderr,
            "DENIED · the message is longer than 65536 bytes\n\
             What was DENIED: the message to alice.\n\
             Why: a message may be at most 65536 bytes long.\n\
             Ways forward:\n\
             - The agent: send a shorter message, and send what remains as further messages.\n"
        );
        let out = too_many_lines("alice", "a\nb\nc");
        assert_eq!(out.code, 2);
        assert_eq!(
            out.stderr,
            "DENIED · the message has too many lines\n\
             What was DENIED: the message to alice (3 lines).\n\
             Why: each line of a message is labelled, and the labelled message must fit in one \
             command argument.\n\
             Ways forward:\n\
             - The agent: send fewer, longer lines, or split the message into several.\n"
        );
        let out = store_unusable(&"disk full", "the message to alice", "updated");
        assert_eq!(out.code, 1);
        assert_eq!(
            out.stderr,
            "DENIED · Lunchroom's store cannot be updated\n\
             What was DENIED: the message to alice.\n\
             Why: Lunchroom keeps its messages and state in a store, and it cannot be updated: \
             disk full.\n\
             Ways forward:\n\
             - The agent: wait a moment and try once more; if it fails again, tell the user.\n\
             - The user: run `lunchroom status` in a terminal of their own to see whether the \
             store can be read, and check that the disk has space; `lunchroom repair` puts back \
             the last good store when it cannot be read.\n"
        );
    }

    // G3-16: an unknown name is answered in full: the closest name, and with no names yet, what
    // the user does.
    #[test]
    fn an_unknown_name_is_answered_with_what_why_and_what_to_do() {
        let s = Setup::new("unknown-name");
        let Ok(store) = Store::<Mail>::open(&s.env.store_dir()) else {
            unreachable!("the store opens");
        };
        let out = unknown_name(&store, "Kimmi");
        assert_eq!(out.code, 3);
        assert_eq!(
            out.stderr,
            "DENIED · no agent is named Kimmi\n\
             What was DENIED: the message to Kimmi.\n\
             Why: only a session the user named receives messages, and none is named Kimmi.\n\
             Ways forward:\n\
             - The user: type join (or join <name>) in each session that should receive \
             messages; until then no agent has a name to send to.\n\
             - `lunchroom status` lists every agent and its name.\n"
        );
        let kimi = Identity::new(Harness::Codex, "k1");
        let named = store.update(|m| {
            m.register("Kimi", &kimi, lunchroom_core::mail::Origin::User, None)
                .is_ok()
        });
        assert_eq!(named.ok(), Some(true));
        assert_eq!(
            unknown_name(&store, "Kimmi").stderr,
            "DENIED · no agent is named Kimmi\n\
             What was DENIED: the message to Kimmi.\n\
             Why: only a session the user named receives messages, and none is named Kimmi.\n\
             Ways forward:\n\
             - The agent: send again to Kimi, the name closest to Kimmi.\n\
             - `lunchroom status` lists every agent and its name.\n"
        );
    }

    // G2-21: a hand-over the session has not confirmed gives the next step: the sender's report is
    // the cards' text (G2) and one line.
    #[test]
    fn an_unconfirmed_hand_over_says_what_to_do_next() {
        let handed = Ok(crate::codex::Delivered::Unconfirmed(
            "lr-00000001".to_owned(),
        ));
        let report = hand_over_report("lr-00000001", &handed);
        assert_eq!(report, format!("{}\n", cards::not_confirmed("lr-00000001")));
        assert!(
            report.starts_with("lr-00000001: Not confirmed; still QUEUED. "),
            "{report}"
        );
        assert!(report.contains("`lunchroom status`"), "{report}");
        assert_eq!(report.lines().count(), 1, "{report}");
    }

    // G3-6, G3-7: doctor says when the Hermes plugin reported its release (here 3 h 5 min before
    // doctor runs), and a Hermes with no report says it has not reported since this Lunchroom.
    #[test]
    fn doctor_says_when_a_plugin_reported_its_release() {
        let mut s = Setup::new("reported-when");
        s.env.now = 1_793_038_003 + 3 * 3600 + 5 * 60;
        s.add_hermes();
        let out = s.doctor();
        let none_since = format!(
            "Hermes: no Hermes session has reported its release to Lunchroom since Lunchroom {} \
             was installed: start a Hermes session, then run lunchroom doctor again.",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            fails(&out).iter().any(|l| l.contains(&none_since)),
            "{}",
            out.stdout
        );
        s.hermes_report("h1", "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003");
        let out = s.doctor();
        assert!(
            out.stdout.contains(
                "PASS Hermes: release 9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003, as \
                 Lunchroom's plugin reported it in Hermes's latest session, on 2026-10-26 at \
                 18:06 UTC, 3 h 5 min ago. This is the release Lunchroom was tested with, or a \
                 later one."
            ),
            "{}",
            out.stdout
        );
    }

    const OLDER_WRITER: &str = "Lunchroom's store was last written by an older Lunchroom program. \
        A session that was started before the upgrade is still running its old program, which \
        drops what this version keeps. Start those sessions anew.";

    // NC-04 (D1.2): when the store was last written by an older Lunchroom program, doctor FAILs
    // and says a session started before the upgrade still runs its old program, and to start
    // those sessions anew. A store this program wrote, and a store that does not exist yet (a
    // fresh install, whose schema number reads 0), show nothing.
    #[test]
    fn doctor_says_when_an_older_program_last_wrote_the_store() {
        let s = Setup::new("older-writer");
        let store_file = s.env.store_dir().join("state.json");
        // A fresh install: no store file, no line.
        assert!(!store_file.exists());
        let out = s.doctor();
        assert!(!out.stdout.contains(OLDER_WRITER), "{}", out.stdout);
        // This program wrote the store: no line.
        let store = Store::<Mail>::open(&s.env.store_dir());
        assert!(store.is_ok());
        let Ok(store) = store else { return };
        assert!(
            store
                .update(|m| m.observe(&Identity::new(Harness::ClaudeCode, "c1")))
                .is_ok()
        );
        assert!(store_file.is_file());
        let out = s.doctor();
        assert!(!out.stdout.contains(OLDER_WRITER), "{}", out.stdout);
        // An older program rewrote it (its schema number is lower, or absent).
        for older in [Some(1), None] {
            let text = std::fs::read_to_string(&store_file).unwrap_or_default();
            let mut v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            match older {
                Some(n) => v["schema"] = serde_json::json!(n),
                None => {
                    if let Some(o) = v.as_object_mut() {
                        o.remove("schema");
                    }
                }
            }
            assert!(std::fs::write(&store_file, v.to_string()).is_ok());
            let before = s.tree();
            let out = s.doctor();
            assert!(
                fails(&out).contains(&format!("FAIL {OLDER_WRITER}").as_str()),
                "{older:?}\n{}",
                out.stdout
            );
            assert_eq!(out.code, 1);
            // Doctor changes nothing: the store still reads as the older program left it.
            assert_eq!(s.tree(), before);
        }
    }

    // G3-5: doctor checks Claude Code in the folder Claude Code reads (`CLAUDE_CONFIG_DIR`), not
    // only when `~/.claude` exists: with the user's settings folder elsewhere and no `~/.claude`,
    // today's code left Claude Code out of the checks altogether.
    #[test]
    fn doctor_checks_claude_code_in_the_folder_claude_code_reads() {
        let s = Setup::new("claude-config-dir");
        let elsewhere = s.root.join("claude-config");
        let _ = std::fs::remove_dir_all(s.env.home.join(".claude"));
        let _ = std::fs::create_dir_all(&elsewhere);
        let sys = Verifier(Ok(crate::terminal::fake::verifier()));
        let host = |claude_config: std::path::PathBuf| Host {
            sys: &sys,
            path: Some(s.path_dir.clone().into_os_string()),
            cwd: s.root.join("project"),
            pid: std::process::id(),
            claude_config,
            managed: s.root.join("managed-settings.json"),
            openclaw_config: s.env.home.join(".openclaw/openclaw.json"),
            root: s.root.join("sysroot"),
        };
        let skipped = doctor_on(&s.env, &host(s.env.home.join(".claude")));
        assert!(
            !skipped.stdout.contains("Claude Code: "),
            "{}",
            skipped.stdout
        );
        let checked = doctor_on(&s.env, &host(elsewhere));
        assert!(
            checked
                .stdout
                .contains("FAIL Claude Code: Claude Code's settings ("),
            "{}",
            checked.stdout
        );
        assert!(
            checked
                .stdout
                .contains("PASS Claude Code: release 2.1.309, read from"),
            "{}",
            checked.stdout
        );
    }

    // G3-10: `lunchroom doctor` says why it did not run, and what to do, in full.
    #[test]
    fn doctor_run_by_an_agent_says_why_it_did_not_run_and_what_to_do() {
        let out = doctor_denied(
            "this command has no controlling terminal to read the user's answer from",
        );
        assert_eq!(out.code, 3);
        assert_eq!(
            out.stderr,
            "DENIED · lunchroom doctor was not run, because this command has no controlling \
             terminal to read the user's answer from. Doctor shows where every agent's settings \
             and programs are, so it runs only for the user: open a terminal of your own, outside \
             every agent session, and run lunchroom doctor there.\n"
        );
    }

    // G3-10 with the real checks: a doctor started under a harness process (the terminal check
    // finds an ancestor named like a harness, or none controlling terminal at all) is denied
    // whatever the program is called, and nothing is printed to standard output.
    #[test]
    fn doctor_checks_where_it_runs_before_it_reads_anything() {
        /// A system whose parent chain and terminal are as given.
        struct Placed(Vec<(Option<String>, String)>, u64);
        impl System for Placed {
            fn ancestors(&self) -> Vec<(Option<String>, String)> {
                self.0.clone()
            }
            fn own_tty(&self) -> u64 {
                self.1
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
                Ok(crate::terminal::fake::verifier())
            }
            fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
                None
            }
            fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
                Err("no salt in this test".to_owned())
            }
            fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
                Err("nothing is installed in this test".to_owned())
            }
        }
        let s = Setup::new("doctor-gate");
        // Under a process named like a harness: denied, whatever the program is called.
        let under = Placed(vec![(None, "claude".to_owned())], 34816);
        let out = doctor(&s.env, &under);
        assert_eq!(out.code, 3, "{}", out.stderr);
        assert!(out.stdout.is_empty(), "{}", out.stdout);
        assert_eq!(
            out.stderr,
            "DENIED · lunchroom doctor was not run, because this command runs under a guarded \
             session (claude); only the user, in a terminal outside every guarded session, can \
             run it. Doctor shows where every agent's settings and programs are, so it runs only \
             for the user: open a terminal of your own, outside every agent session, and run \
             lunchroom doctor there.\n"
        );
        // With no controlling terminal: denied.
        let detached = Placed(vec![(None, "bash".to_owned())], 0);
        let out = doctor(&s.env, &detached);
        assert_eq!(out.code, 3, "{}", out.stderr);
        assert!(
            out.stderr.starts_with(
                "DENIED · lunchroom doctor was not run, because this command has no controlling \
                 terminal"
            ),
            "{}",
            out.stderr
        );
        // The user in a terminal of their own: doctor runs (its checks print, one line each).
        let user = Placed(vec![(None, "bash".to_owned())], 34816);
        let out = doctor(&s.env, &user);
        assert!(out.stderr.is_empty(), "{}", out.stderr);
        assert!(
            out.stdout.contains("Lunchroom's program is "),
            "{}",
            out.stdout
        );
    }

    // G3-9: a HELD agent is a line of its own, neither PASS nor FAIL, that names what holds it, and
    // the first one says what HELD means.
    #[test]
    fn a_held_agent_is_shown_as_held_and_explained_once() {
        let mut checks = Checks::default();
        checks.held("alice (Claude Code)", "a message from bob (lr-00000012)");
        checks.held("carol (Codex)", "a message from bob (lr-00000013)");
        assert_eq!(
            checks.text,
            "HELD agent alice (Claude Code): HELD, because a message from bob (lr-00000012) is in \
             its session. HELD means that text from another agent is in a session, so Lunchroom \
             restricts what the agent may do until the user types in the session.\n\
             HELD agent carol (Codex): HELD, because a message from bob (lr-00000013) is in its \
             session.\n"
        );
        assert_eq!(checks.failed, 0);
    }

    // G3-11: `lunchroom status` prints the total of the calls waiting for the user, the number
    // `lunchroom approve` lists, whoever asked.
    #[test]
    fn status_prints_every_call_waiting_for_the_user() {
        let s = Setup::new("status-total");
        let Ok(store) = Store::<Mail>::open(&s.env.store_dir()) else {
            unreachable!("the store opens");
        };
        let now = s.env.now;
        let stored = store.update(|m| {
            for k in 0..2 {
                m.request(lunchroom_core::approvals::Request {
                    session: Identity::new(Harness::ClaudeCode, &format!("unnamed-{k}")),
                    agent: String::new(),
                    tool: "Bash".to_owned(),
                    input: serde_json::json!({"command": format!("ls {k}")}),
                    cwd: String::new(),
                    rule: String::new(),
                    at: now,
                });
            }
        });
        assert!(stored.is_ok());
        let out = status(&s.env);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.stdout
                .contains("calls waiting for the user: 2 (`lunchroom approve` lists them)\n"),
            "{}",
            out.stdout
        );
    }
}
