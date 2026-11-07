//! The `lunchroom` command and the Claude Code guard hook.
//!
//! `lunchroom guard pretool|prompt|posttool` is the guard and `lunchroom listen [--once]` the
//! delivery hook (both read Claude Code's hook input on standard input); `lunchroom send` and
//! `lunchroom status` are the commands an agent may run. What the guard cannot decide (its own
//! error, a store it cannot use, a panic, or no answer within `GUARD_DEADLINE`) never locks the
//! user out (H1.5): a call goes to the user, or is DENIED alone where nobody can answer.

// The guard reads harness processes and sessions from `/proc`.
#[cfg(not(target_os = "linux"))]
compile_error!("Lunchroom runs on Linux: the guard reads harness processes from /proc.");

mod checks;
mod claude;
mod cli;
mod codex;
#[cfg(feature = "console")]
mod console;
#[cfg(test)]
mod corpus;
mod delivery;
mod evidence;
mod home;
mod install;
mod openclaw;
mod process;
mod records;
mod terminal;
mod user;

use std::io::{Read as _, Write as _};
use std::process::ExitCode;
use std::time::Duration;

/// The build folder this test runs from (`<target>/release/deps/<test>`), for scratch files.
#[cfg(test)]
fn test_target() -> std::path::PathBuf {
    let test = std::env::current_exe().unwrap_or_default();
    test.ancestors()
        .nth(3)
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf()
}

/// How long one `lunchroom listen` waits for a message: under the hook's timeout of a day.
const LISTEN_MAX: Duration = Duration::from_hours(23);

fn write(out: Option<&str>, err: Option<&str>) {
    if let Some(text) = out {
        let _ = writeln!(std::io::stdout(), "{text}");
    }
    if let Some(text) = err {
        let _ = writeln!(std::io::stderr(), "{text}");
    }
}

/// H1.5: the guard answers within this time, under the hook's own timeout (15 seconds).
const GUARD_DEADLINE: Duration = Duration::from_secs(10);
/// How long a guard past its deadline waits for an answer already settled in the store.
const SETTLE: Duration = Duration::from_secs(1);

fn run_hook(
    mode: &str,
    text: &str,
    env: Result<home::Env, String>,
) -> Result<claude::HookOut, String> {
    let env = env?;
    // D1.7: every guard hook starts a check of the QUEUED messages' time limits, which it does not
    // wait for; a listener runs the check itself (`delivery::listen`). The guard starts it after
    // its answer is written (`guard`), never before the decision (G3-12).
    let input = claude::HookInput::parse(text)?;
    match mode {
        "pretool" => claude::pretool(&input, &env),
        "prompt" => claude::prompt(&input, &env),
        "posttool" => claude::posttool(&input, &env),
        "start" => Ok(claude::start(&input, &env)),
        "listen" => delivery::listen(&input, &env, LISTEN_MAX),
        "listen-once" => delivery::listen(&input, &env, Duration::ZERO),
        _ => Err("usage: lunchroom guard pretool|prompt|posttool|start".to_owned()),
    }
}

/// What the hook's worker thread reports: the input it read, then the answer.
enum Step {
    Input(String),
    Answer(Result<claude::HookOut, String>),
}

/// The hook input. Input that is not UTF-8 is the guard's own error, but its text, decoded with
/// replacement characters, still comes back so a prompt carrying another agent's text is held
/// (H1.5).
fn read_input() -> Result<String, (String, String)> {
    // H1.7: the step named if the guard's time runs out here.
    let _step = lunchroom_core::steps::begin("reading what the harness sent");
    let mut bytes = Vec::new();
    if std::io::stdin().read_to_end(&mut bytes).is_err() {
        return Err((String::new(), "the hook input cannot be read".to_owned()));
    }
    String::from_utf8(bytes).map_err(|e| {
        (
            String::from_utf8_lossy(e.as_bytes()).into_owned(),
            "the hook input is not UTF-8 text".to_owned(),
        )
    })
}

/// The hook `mode` is a listener, which waits for messages by design and decides no call.
fn owned_is_listener(mode: Option<&str>) -> bool {
    mode.is_some_and(|m| m.starts_with("listen"))
}

/// A guard hook (not a listener) starts the check of the QUEUED messages' time limits (D1.7).
fn starts_check(mode: Option<&str>) -> bool {
    matches!(mode, Some("pretool" | "prompt" | "posttool" | "start"))
}

/// How long the guard spends judging a call again after an error (G3-13): a second pass over the
/// call must not take as long as the failure it follows.
const FAILED_CALL_DEADLINE: Duration = Duration::from_secs(2);

/// The call judged again by the rules that need no record, after the guard met `reason`, on a
/// thread of its own so that this takes at most [`FAILED_CALL_DEADLINE`]: `None` when it decides
/// nothing or does not finish.
fn judged_again(text: &str, reason: &str) -> Option<claude::HookOut> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (text, reason) = (text.to_owned(), reason.to_owned());
    std::thread::Builder::new()
        .spawn(move || {
            let _ = tx.send(claude::failed_call(&text, &reason));
        })
        .ok()?;
    rx.recv_timeout(FAILED_CALL_DEADLINE).ok().flatten()
}

#[allow(clippy::too_many_lines)] // one guard run: read the hook, decide against a deadline, answer
fn guard(mode: Option<&str>, env: Result<home::Env, String>) -> ExitCode {
    let owned = mode.unwrap_or_default().to_owned();
    let mut text = String::new();
    let mut timed_out = false;
    // H1.8: where the guard's records are kept, and when it began.
    let records_at = env.as_ref().ok().map(|e| (e.lunchroom_home.clone(), e.now));
    // The check of D1.7 runs with this, after the answer is written.
    let check_env = env.as_ref().ok().cloned().filter(|_| starts_check(mode));
    let began = std::time::Instant::now();
    let mut worker_id = None;
    let result = if owned.starts_with("listen") {
        // The listener waits for messages by design; it has no deadline and asks nobody.
        read_input()
            .map_err(|(_, reason)| reason)
            .and_then(|t| run_hook(&owned, &t, env))
    } else {
        // Reading the input and deciding run on their own thread, so a stalled input, a panic or
        // a stall still gets an answer by the deadline.
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new().spawn(move || {
            let answer = match read_input() {
                Ok(t) => {
                    let _ = tx.send(Step::Input(t.clone()));
                    run_hook(&owned, &t, env)
                }
                Err((t, reason)) => {
                    let _ = tx.send(Step::Input(t));
                    Err(reason)
                }
            };
            let _ = tx.send(Step::Answer(answer));
        });
        let deadline = std::time::Instant::now() + GUARD_DEADLINE;
        let mut result = Err("the guard could not start".to_owned());
        worker_id = worker.as_ref().ok().map(|w| w.thread().id());
        if worker.is_ok() {
            // Every way out of this loop sets `result` (an answer, the deadline, or the worker
            // gone), so none is set before it. The deadline's reason is built where it passes,
            // with the step still running.
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Step::Input(t)) => text = t,
                    Ok(Step::Answer(answer)) => {
                        result = answer;
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // H1.7: the step still running when the time ran out is named.
                        let step = worker_id.and_then(lunchroom_core::steps::current);
                        result = Err(match step {
                            Some(step) => format!(
                                "no answer within {} seconds, while the guard was {step}",
                                GUARD_DEADLINE.as_secs()
                            ),
                            None => {
                                format!("no answer within {} seconds", GUARD_DEADLINE.as_secs())
                            }
                        });
                        // Past the deadline nothing more is written; an answer whose last write
                        // was already made arrives at once and is the one given.
                        lunchroom_core::store::stop_updates();
                        match rx.recv_timeout(SETTLE) {
                            Ok(Step::Answer(Ok(out))) => result = Ok(out),
                            Ok(Step::Input(t)) => {
                                text = t;
                                timed_out = true;
                            }
                            _ => timed_out = true,
                        }
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        result = Err("the guard stopped with an internal error".to_owned());
                        break;
                    }
                }
            }
        }
        result
    };
    let undecided = result.as_ref().err().cloned();
    let out = result.unwrap_or_else(|reason| {
        // A call is still judged by the rules that need no record, unless the guard ran out of
        // time (judging it again could take as long); and then for at most two seconds, with the
        // reason that stopped the first pass (G3-13).
        let settled = (mode == Some("pretool") && !timed_out)
            .then(|| judged_again(&text, &reason))
            .flatten();
        settled.unwrap_or_else(|| claude::failed(mode, &reason, &text, claude::process_channel()))
    });
    // H1.8: an undecided call, or a decision past 2 seconds, is recorded; the listener, which waits
    // by design, is not.
    if !owned_is_listener(mode)
        && let Some((home, at)) = records_at
        && (undecided.is_some() || began.elapsed() > records::SLOW)
    {
        let (step, took) = worker_id
            .and_then(lunchroom_core::steps::slowest)
            .unwrap_or_else(|| ("no step was marked".to_owned(), std::time::Duration::ZERO));
        let session = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["session_id"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ = records::add(
            &home,
            &records::Record {
                at,
                session,
                hook: mode.unwrap_or_default().to_owned(),
                call: claude::hook_call(mode, &text),
                step,
                step_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
                took_ms: u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
                undecided,
            },
        );
    }
    // V1.4: a DENIED tool call in Claude Code reaches the agent and the user both; Codex and the
    // plugins read exit 2 with the reason on standard error.
    let (stdout, stderr, code) = match &out {
        claude::HookOut::Block(reason)
            if mode == Some("pretool") && claude::is_claude_code_input(&text) =>
        {
            (Some(claude::claude_code_block(reason)), None, 0)
        }
        _ => out.render(),
    };
    write(stdout.as_deref(), stderr.as_deref());
    // D1.7 (G3-12): the check of the QUEUED messages is started now, after the answer is written,
    // so it never adds to the time a decision takes. A guard past its deadline starts none.
    if let Some(env) = check_env
        && !timed_out
    {
        checks::start(&env);
    }
    ExitCode::from(code)
}

/// The commands an agent may run, in the order `USAGE` lists them under "Agents may run" (A1.3).
/// One list: the help text, and the Configuration denial that names them
/// ([`agent_commands_text`]), take their names from here.
pub(crate) const AGENT_COMMANDS: [&str; 5] = ["send", "status", "codex-threads", "help", "version"];

/// [`AGENT_COMMANDS`] as a sentence part: `lunchroom send`, `lunchroom status`, …, `lunchroom help`
/// and `lunchroom version`.
#[must_use]
pub(crate) fn agent_commands_text() -> String {
    let named: Vec<String> = AGENT_COMMANDS
        .iter()
        .map(|c| format!("`lunchroom {c}`"))
        .collect();
    match named.split_last() {
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The one way `lunchroom yolo` is written, in the help text, the README and the error for a
/// mistaken command: names are separated by commas (G3-14).
pub(crate) const YOLO_USAGE: &str = "lunchroom yolo <name,name,...|all> till <HH:MM>";

const USAGE: &str = "\
usage: lunchroom <command>

Agents may run:
  send <name> [--reply-to <id>] <text>    message another agent (text `-` reads standard input)
  status                                  agents, their states and queued messages
  codex-threads [-n COUNT]                recent Codex threads, to name one with `register`
  help, version

Only you, typed in a session as the whole prompt:
  join [<name>]                           include this session: it receives messages as <name>
  leave                                   take this session out

Only you, in a terminal outside every agent session; each one that changes something asks for your
passphrase, which it checks against /etc/lunchroom/<uid>.verifier, a file only root can write:
  passphrase                              set your passphrase, or change it (asks for the current one)
  install [claude-code|codex|openclaw|hermes]   no harness named: offers each one found
  uninstall claude-code|codex|openclaw|hermes   remove Lunchroom's hooks; every other hook stays
  register <name> <harness>:<session id>  let a session receive messages as <name>
  unregister <name>                       stop a session receiving messages as <name>
  approve [<n>]                           list the calls waiting for you, or approve one
  yolo <name,name,...|all> till <HH:MM>   let those agents act without asking until then (UTC);
                                          separate names with commas; it works for Claude Code
                                          agents only, and `all` or a list that names any other
                                          agent changes none: name just the Claude Code agents
  mode [relaxed|balanced|strict]          how much another agent's message limits an agent
  rate <messages> <seconds>               set the message rate limit
  slip                                    show what a HELD agent may still do
  repair                                  put back the last good store when it cannot be read
  doctor                                  check your setup, one PASS or FAIL line each, with the
                                          fix for each FAIL (a HELD agent is shown as HELD, which
                                          is neither); changes nothing; exit 0 when none FAILs

HELD: a message from another agent is in a session, so Lunchroom restricts what that agent may do.
UNGUARDED: an agent is running, but Lunchroom has no evidence that its guard runs, so it delivers nothing to it.

Exit codes: 0 done, 1 store unusable, 2 usage, 3 not permitted, 4 HALTED, 5 rate limit, 7 DENIED.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("guard") => return guard(args.get(1).map(String::as_str), home::Env::from_process()),
        // The console reads no environment but `SUDO_UID`; it runs before `Env::from_process`.
        #[cfg(feature = "console")]
        Some("console") => return console::run(&args[1..]),
        Some("listen") => {
            let once = args.get(1).map(String::as_str) == Some("--once");
            let mode = if once { "listen-once" } else { "listen" };
            return guard(Some(mode), home::Env::from_process());
        }
        _ => {}
    }
    let env = match home::Env::from_process() {
        Ok(env) => env,
        Err(reason) => {
            write(None, Some(&format!("Lunchroom: {reason}")));
            return ExitCode::from(2);
        }
    };
    let outcome = match args.first().map(String::as_str) {
        Some("send") => cli::send(&args[1..], &env),
        Some("status") => cli::status(&env),
        Some("codex-threads") => cli::codex_threads(&args[1..], &env),
        Some("approve") => user::approve(&args[1..], &env, &mut terminal::OsSystem),
        Some("register") => user::register(&args[1..], &env, &mut terminal::OsSystem),
        Some("unregister") => user::unregister(&args[1..], &env, &mut terminal::OsSystem),
        Some("rate") => user::rate(&args[1..], &env, &mut terminal::OsSystem),
        Some("slip") => user::slip(&args[1..]),
        Some("yolo") => user::yolo(&args[1..], &env, &mut terminal::OsSystem),
        Some("install") => install::install(&args[1..], &env, &mut terminal::OsSystem),
        Some("uninstall") => install::uninstall(&args[1..], &env, &mut terminal::OsSystem),
        Some("repair") => user::repair(&env, &mut terminal::OsSystem),
        Some("passphrase") => user::passphrase(&env, &mut terminal::OsSystem),
        Some("mode") => user::mode(&args[1..], &env, &mut terminal::OsSystem),
        Some("doctor") => cli::doctor(&env, &terminal::OsSystem),
        // D1.7: the check a guard hook starts; not listed in `help`.
        Some("check") => checks::check(&env, &args[1..]),
        Some("help" | "--help" | "-h") => cli::Outcome::ok(USAGE.to_owned()),
        Some("version" | "--version") => cli::Outcome::ok(format!("{}\n", cli::VERSION)),
        _ => {
            write(None, Some(USAGE.trim_end()));
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    ExitCode::from(outcome.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of `USAGE` under "Agents may run:", up to the blank line.
    fn agent_section() -> Vec<&'static str> {
        USAGE
            .lines()
            .skip_while(|l| *l != "Agents may run:")
            .skip(1)
            .take_while(|l| !l.is_empty())
            .collect()
    }

    // G3-18: one list of the commands an agent may run. The help text's "Agents may run" block
    // names exactly them, in order, and the sentence the Configuration denial uses is built from
    // the same list (full text), with help and version in it.
    #[test]
    fn the_commands_an_agent_may_run_are_one_list() {
        let section = agent_section().join("\n");
        let mut at = 0;
        for name in AGENT_COMMANDS {
            let found = section[at..].find(name);
            assert!(found.is_some(), "{name} in {section}");
            at += found.unwrap_or_default() + name.len();
        }
        assert_eq!(agent_section().len(), 4, "{section}");
        assert_eq!(
            agent_commands_text(),
            "`lunchroom send`, `lunchroom status`, `lunchroom codex-threads`, `lunchroom help` \
             and `lunchroom version`"
        );
    }

    // G3-14: `lunchroom yolo` is written one way in the help text: names separated by commas,
    // `all`, and an end time, with what the user must know to avoid a refusal.
    #[test]
    fn yolo_is_written_one_way_in_the_help() {
        assert_eq!(
            YOLO_USAGE,
            "lunchroom yolo <name,name,...|all> till <HH:MM>"
        );
        let usage = YOLO_USAGE.trim_start_matches("lunchroom ");
        assert!(
            USAGE.contains(&format!("  {usage}   let those agents act without asking")),
            "{USAGE}"
        );
        assert!(USAGE.contains("it works for Claude Code\n"), "{USAGE}");
        assert!(!USAGE.contains("name those that can"), "{USAGE}");
    }

    // G3-9: HELD and UNGUARDED are explained in the help text, where a first-time user meets them.
    #[test]
    fn the_help_text_says_what_held_and_unguarded_mean() {
        assert!(
            USAGE.contains(
                "HELD: a message from another agent is in a session, so Lunchroom restricts what \
                 that agent may do.\nUNGUARDED: an agent is running, but Lunchroom has no \
                 evidence that its guard runs, so it delivers nothing to it.\n"
            ),
            "{USAGE}"
        );
    }

    // G3-12: only a guard hook starts the check of the QUEUED messages; a listener does not (it
    // runs the check itself).
    #[test]
    fn only_guard_hooks_start_the_check() {
        for mode in ["pretool", "prompt", "posttool", "start"] {
            assert!(starts_check(Some(mode)), "{mode}");
        }
        for mode in [Some("listen"), Some("listen-once"), Some("other"), None] {
            assert!(!starts_check(mode), "{mode:?}");
        }
    }
}
