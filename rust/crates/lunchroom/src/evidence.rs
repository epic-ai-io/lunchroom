//! The Claude Code adapter's guard evidence (R3.1, H1.1 item 2).
//!
//! A Claude Code session is guarded when a guard hook recorded it in its current Claude Code process
//! (the process still runs under the same `<pid>:<start time>`) and a settings file that Claude Code
//! loads for it still runs Lunchroom's complete guard set, `PreToolUse`, `PostToolUse`,
//! `PostToolUseFailure` and `UserPromptSubmit`, each covering every call (`configured_with`): the
//! managed `/etc/claude-code/managed-settings.json`, the user's `settings.json` (in
//! `CLAUDE_CONFIG_DIR`, or `~/.claude`), the project's `.claude/settings.json` and
//! `.claude/settings.local.json` in the folder it ran in, as `--setting-sources` leaves them, or a
//! file or text given to that process with `--settings`, unless `--bare` or `disableAllHooks`
//! switches the hooks off. A session no hook recorded, or whose process has ended, is STOPPED; one
//! that runs without that configuration is UNGUARDED.
//!
//! OpenClaw and Hermes load Lunchroom's plugin into the harness process, and the plugin starts the
//! guard; the guard records the plugin's file. Such a session is guarded while that file still
//! runs a Lunchroom program that is there to run, and, for OpenClaw, which applies plugin settings
//! to a running gateway, while `openclaw.json` still enables the plugin with conversation access
//! (`before_agent_run`, which confirms deliveries and sees another agent's text, runs only then).
//! Host facts: `docs/host-facts/openclaw-and-hermes.md`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::DocumentMut;

use lunchroom_core::identity::{Harness, Identity};
use lunchroom_core::mail::{Evidence, Mail};

use crate::process;

/// The evidence for `who`'s guard. Every harness first needs its recorded process still running
/// under the same `<pid>:<start time>` (else STOPPED); then its adapter's configuration and a
/// release its adapter passes (H1.2).
#[must_use]
pub fn evidence(mail: &Mail, who: &Identity, home: &Path) -> Evidence {
    assess(mail, who, home).evidence()
}

/// R3.4: why `who` is UNGUARDED, or `None` when it is not (guarded, or STOPPED). The same checks,
/// in the same one sequence, as [`evidence`].
#[must_use]
pub fn unguarded_reason(mail: &Mail, who: &Identity, home: &Path) -> Option<Why> {
    match assess(mail, who, home) {
        Assessment::Unguarded(why) => Some(why),
        Assessment::NotRunning | Assessment::Guarded => None,
    }
}

/// What the evidence shows of a session, and for an UNGUARDED one why (R3.1, R3.4). [`evidence`]
/// and [`unguarded_reason`] both read this one judgment, so the state shown and the reason given
/// come from the same checks.
#[must_use]
pub fn assess(mail: &Mail, who: &Identity, home: &Path) -> Assessment {
    assess_with(&PASSED_WITH, mail, who, home)
}

/// What the evidence shows of a session: STOPPED, UNGUARDED and why, or guarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    /// Not running, or cannot be reached: STOPPED.
    NotRunning,
    /// Running without the evidence R3.1 needs, for the first reason R3.4 lists.
    Unguarded(Why),
    /// The guard ran in its current session and the configuration is in place.
    Guarded,
}

impl Assessment {
    /// The evidence this judgment gives the vocabulary's states (R3.3).
    #[must_use]
    pub const fn evidence(&self) -> Evidence {
        match self {
            Self::NotRunning => Evidence::NotRunning,
            Self::Unguarded(_) => Evidence::RunningUnguarded,
            Self::Guarded => Evidence::Guarded,
        }
    }
}

/// R3.4 (docs/spec/lunchroom-spec.md §4 R3 item 4): "That notice, `lunchroom status` and
/// `lunchroom doctor` (C1.4) give each UNGUARDED agent one reason, the first of these its evidence
/// lacks, and what the user does about it: its harness configuration does not run the whole guard
/// (naming the harness, and for Codex the setting or hook at fault); no guard hook has run in its
/// current session; its harness release cannot be read from its install (naming the program's
/// path); or its release is older than the one its adapter passed with (naming both releases)."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Item 1: the whole sentence, naming the harness (and for Codex the setting or hook) and what
    /// the user does.
    Configuration(String),
    /// Item 2: no guard hook has run in the session's current process with the configuration now
    /// in place.
    NoGuardRan(Harness),
    /// Item 3: the release cannot be read; `program` names the program's path or paths read.
    ReleaseUnread { harness: Harness, program: String },
    /// Item 4: the release read, and the one the adapter passed with.
    ReleaseOlder {
        harness: Harness,
        have: String,
        passed: String,
    },
    /// H1.2 item 2: the harness has no passing adapter in this build, so no release of it passes.
    NoAdapter(Harness),
    /// The program the session runs was replaced while the session ran (the system shows its path
    /// with `(deleted)` after it), so the release on disk is not the release the session runs.
    ProgramReplaced { harness: Harness, program: String },
    /// Claude Code's release is not in the program's path or npm package, and the system has an
    /// rpm database but no `rpm` program to read it with.
    RpmNotInstalled { program: String },
}

/// The harness's name as the user knows it.
#[must_use]
pub const fn harness_name(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Claude Code",
        Harness::Codex => "Codex",
        Harness::OpenClaw => "OpenClaw",
        Harness::Hermes => "Hermes",
    }
}

/// The word `lunchroom install` takes for `harness`.
const fn install_word(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::OpenClaw => "openclaw",
        Harness::Hermes => "hermes",
    }
}

/// The installs Lunchroom reads `harness`'s release from (H1.2 item 2; [`version_from_path`]).
const fn installs_read(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => {
            "the native installer's …/claude/versions/<release>, Homebrew's \
             …/Caskroom/claude-code/<release>, the npm package @anthropic-ai/claude-code (or its \
             platform package) above the program, or the system package named claude-code that \
             installed the program (dpkg, apk or rpm records)"
        }
        Harness::Codex => {
            "a Codex release folder …/.codex/…/releases/<release>-<target>, or the npm package \
             @openai/codex (or one of its platform builds) above the program"
        }
        Harness::OpenClaw | Harness::Hermes => {
            "the release Lunchroom's plugin reports with each guard hook it runs"
        }
    }
}

impl Why {
    /// The reason as the user reads it, ending with what the user does about it.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Configuration(text) => text.clone(),
            Self::NoGuardRan(harness) => {
                let name = harness_name(*harness);
                let then = match harness {
                    Harness::ClaudeCode | Harness::Codex => String::new(),
                    Harness::OpenClaw => "; if it stays UNGUARDED, run openclaw plugins reload \
                                          lunchroom, or restart the gateway"
                        .to_owned(),
                    Harness::Hermes => "; if it stays UNGUARDED, start the Hermes session anew \
                                        (Hermes loads a plugin when a session starts)"
                        .to_owned(),
                };
                format!(
                    "No Lunchroom guard hook has run in this {name} session with its configuration \
                     as it is now: type a prompt in the session, so its guard hook runs{then}."
                )
            }
            Self::ReleaseUnread { harness, program } => {
                let name = harness_name(*harness);
                let fix = match harness {
                    Harness::ClaudeCode | Harness::Codex => {
                        format!("install {name} one of those ways, then start the session anew")
                    }
                    Harness::OpenClaw => format!(
                        "run lunchroom install {} in a terminal of your own, so the plugin \
                         reports it, then start the session anew",
                        install_word(*harness)
                    ),
                    // R3.4 (2026-11-03): git refuses a checkout owned by another user.
                    Harness::Hermes => format!(
                        "run lunchroom install {} in a terminal of your own, so the plugin \
                         reports it, then start the session anew; if Hermes runs from a git \
                         checkout that belongs to a user other than root and the user who runs \
                         Hermes, git refuses to read it: give the checkout to the user who runs \
                         Hermes or to root, then start Hermes anew",
                        install_word(*harness)
                    ),
                };
                format!(
                    "{name}'s release cannot be read from the install of its program ({program}): \
                     Lunchroom reads it from {}. To fix it, {fix}.",
                    installs_read(*harness)
                )
            }
            Self::ReleaseOlder {
                harness,
                have,
                passed,
            } => {
                let name = harness_name(*harness);
                format!(
                    "{name} {have} is older than {passed}, the oldest release Lunchroom was \
                     tested with: update {name} to {passed} or later, then start the session anew."
                )
            }
            Self::NoAdapter(harness) => {
                let name = harness_name(*harness);
                format!(
                    "This Lunchroom has not been tested with any release of {name}, so none is \
                     guarded: install a Lunchroom release that has been tested with {name}."
                )
            }
            Self::ProgramReplaced { harness, program } => {
                let name = harness_name(*harness);
                format!(
                    "{name}'s program ({program}) was replaced while this session was running, \
                     most likely by an upgrade, so Lunchroom cannot tell which release the session \
                     still runs: start the {name} session anew, so it runs the program now installed."
                )
            }
            Self::RpmNotInstalled { program } => format!(
                "Claude Code's release cannot be read from the install of its program ({program}): \
                 rpm is not installed, so Lunchroom cannot read the rpm database that may hold it. \
                 To fix it, install the rpm package, then start the session anew."
            ),
        }
    }

    /// The reason as an agent may read it (G1-11): the harness and the fix, with no path of
    /// another session's configuration. The full reason, with its paths, is for the user.
    #[must_use]
    pub fn text_for_agent(&self) -> String {
        match self {
            Self::Configuration(_) => "Its harness configuration does not run Lunchroom's whole \
                 guard: the user can see which setting in a terminal of their own, with \
                 `lunchroom doctor`."
                .to_owned(),
            Self::NoGuardRan(harness) => {
                let name = harness_name(*harness);
                format!(
                    "No Lunchroom guard hook has run in this {name} session with its configuration \
                     as it is now: a prompt typed in the session makes its guard hook run."
                )
            }
            Self::ReleaseUnread { harness, .. } => {
                let name = harness_name(*harness);
                format!(
                    "{name}'s release cannot be read from the install of its program: the user can \
                     see where Lunchroom looked in a terminal of their own, with `lunchroom doctor`."
                )
            }
            Self::ReleaseOlder {
                harness,
                have,
                passed,
            } => {
                let name = harness_name(*harness);
                format!(
                    "{name} {have} is older than {passed}, the oldest release Lunchroom was tested \
                     with: {name} must be updated to {passed} or later."
                )
            }
            Self::NoAdapter(harness) => {
                let name = harness_name(*harness);
                format!("This Lunchroom has not been tested with any release of {name}.")
            }
            Self::ProgramReplaced { harness, .. } => {
                let name = harness_name(*harness);
                format!(
                    "{name}'s program was replaced while this session was running: the {name} \
                     session must be started anew."
                )
            }
            Self::RpmNotInstalled { .. } => "Claude Code's release cannot be read because rpm is \
                 not installed: the user can install the rpm package."
                .to_owned(),
        }
    }
}

/// `assess` against the list `passed` of passing adapters and their releases. Each harness's
/// checks run in R3.4's order, configuration first, so the first one that fails is the reason;
/// a session passing every check is guarded.
fn assess_with(passed: &[(Harness, &str)], mail: &Mail, who: &Identity, home: &Path) -> Assessment {
    // H1.7: the step named if the guard's time runs out here.
    let _step = lunchroom_core::steps::begin(format!("working out whether {who} is guarded"));
    let Some(record) = mail.session(who) else {
        return Assessment::NotRunning;
    };
    let Some(pid) = record
        .process
        .split(':')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return Assessment::NotRunning;
    };
    if process::key(pid).as_deref() != Some(record.process.as_str()) {
        return Assessment::NotRunning;
    }
    let why = match who.harness {
        // Item 2 for Claude Code: the session's record names this very process only because a
        // Lunchroom hook ran in it, and a record of another process is STOPPED above; so a
        // running Claude Code session never lacks item 2 here.
        Harness::ClaudeCode => {
            if configured(pid, &record.cwd, home) {
                release_fault(passed, who.harness, pid)
            } else {
                Some(Why::Configuration(CLAUDE_NOT_CONFIGURED.to_owned()))
            }
        }
        Harness::Codex => {
            let current = codex_guard_in(&codex_home_of(pid, home), pid, &record.cwd);
            let opened_before =
                mail.opened_before_codex_setting(who, process::boot_id().as_deref());
            match current {
                // The setting first, as `lunchroom status` always named it.
                Err(fault @ (CodexFault::TtyNotOff | CodexFault::TtyTurnedOn)) => {
                    Some(Why::Configuration(fault.text()))
                }
                _ if opened_before => Some(Why::Configuration(CODEX_OPENED_BEFORE.to_owned())),
                Err(fault) => Some(Why::Configuration(fault.text())),
                // R3.1 (Codex): the trusted guard now in place is the one a guard hook recorded
                // when it ran in this session; else none has run with it.
                Ok(current) if mail.guard_fingerprint(who) != Some(current.as_str()) => {
                    Some(Why::NoGuardRan(who.harness))
                }
                Ok(_) => release_fault(passed, who.harness, pid),
            }
        }
        // The harness still enabling the plugin and the plugin file a guard hook recorded still
        // running the guard (R3.1), then the release the plugin reported with it (H1.2).
        Harness::OpenClaw | Harness::Hermes => {
            let recorded = mail.guard_fingerprint(who);
            let fault = if who.harness == Harness::OpenClaw {
                openclaw_fault(&openclaw_config(pid, home))
            } else {
                hermes_fault(&hermes_home_of(pid, home))
            };
            if let Some(fault) = fault {
                Some(Why::Configuration(fault))
            } else if let Some(file) = recorded_plugin_file(recorded)
                && !plugin_file_runs_guard(file)
            {
                Some(Why::Configuration(plugin_fault(who.harness, file)))
            } else if recorded.is_none() {
                Some(Why::NoGuardRan(who.harness))
            } else {
                let program = std::fs::read_link(format!("/proc/{pid}/exe"))
                    .map_or_else(|_| format!("/proc/{pid}/exe"), |p| p.display().to_string());
                version_fault(passed, who.harness, plugin_release(recorded), program)
            }
        }
    };
    why.map_or(Assessment::Guarded, Assessment::Unguarded)
}

/// What the user is told when Claude Code's configuration does not run the whole guard (R3.4
/// item 1).
pub(crate) const CLAUDE_NOT_CONFIGURED: &str = "Claude Code's settings for this session do not run \
     Lunchroom's whole guard (PreToolUse, PostToolUse, PostToolUseFailure and UserPromptSubmit, \
     each on every call, its program there to run), or the session was started with --bare or \
     with hooks left out (disableAllHooks, --setting-sources): run lunchroom install claude-code \
     in a terminal of your own, and start the session without --bare.";

/// H1.2 for a harness whose release is read from the install of process `pid`'s program (Claude
/// Code, Codex): unread, older, or none (it passes).
fn release_fault(passed: &[(Harness, &str)], harness: Harness, pid: u32) -> Option<Why> {
    let found = process_programs(pid);
    let programs = &found.paths;
    let shown = if programs.is_empty() {
        format!("/proc/{pid}/exe")
    } else {
        programs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ")
    };
    // G3-1: a program replaced under the running session (its path read `(deleted)`): what the
    // package manager or the npm package says now describes the new program, not the one running,
    // so only a release written in the path itself (`…/versions/<release>`) is still true.
    if found.replaced {
        let version = programs
            .iter()
            .find_map(|p| path_release_of(harness, p))
            .unwrap_or_default();
        if release(&version).is_none() {
            return Some(Why::ProgramReplaced {
                harness,
                program: shown,
            });
        }
        return version_fault(passed, harness, &version, shown);
    }
    let version = programs
        .iter()
        .find_map(|p| version_from_path(harness, p))
        .unwrap_or_default();
    // G3-4: nothing read the release, and the system keeps an rpm database but has no rpm program.
    if harness == Harness::ClaudeCode
        && release(&version).is_none()
        && rpm_database_without_rpm(Path::new("/"))
    {
        return Some(Why::RpmNotInstalled { program: shown });
    }
    version_fault(passed, harness, &version, shown)
}

/// H1.2: `version` of `harness`, read from the install of `program`: no passing adapter, unread,
/// older, or none (it passes).
fn version_fault(
    passed: &[(Harness, &str)],
    harness: Harness,
    version: &str,
    program: String,
) -> Option<Why> {
    if passes_with(passed, harness, version) {
        return None;
    }
    let Some(stated) = passed.iter().find(|(h, _)| *h == harness).map(|(_, v)| *v) else {
        return Some(Why::NoAdapter(harness));
    };
    if !readable(harness, version) {
        return Some(Why::ReleaseUnread { harness, program });
    }
    let passed = if harness == Harness::Hermes {
        format!("commit {stated}")
    } else {
        stated.to_owned()
    };
    Some(Why::ReleaseOlder {
        harness,
        have: version.to_owned(),
        passed,
    })
}

/// H1.2: `version` is a release of `harness` Lunchroom can order against its adapter's: a release
/// number, or for Hermes a commit with its date (a commit alone is ordered only as the stated
/// commit itself, which passes).
fn readable(harness: Harness, version: &str) -> bool {
    if harness == Harness::Hermes {
        return version
            .split_once('@')
            .is_some_and(|(commit, date)| is_commit(commit) && date.parse::<u64>().is_ok());
    }
    release(version).is_some()
}

/// Codex's folder for the session in process `pid`: its `CODEX_HOME`, or `~/.codex`.
#[must_use]
pub fn codex_home_of(pid: u32, home: &Path) -> PathBuf {
    process_env(pid, "CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from)
}

/// Codex's folder for `who`'s session, from the process a guard hook recorded it in.
#[must_use]
pub fn recorded_codex_home(mail: &Mail, who: &Identity, home: &Path) -> PathBuf {
    mail.session(who)
        .and_then(|r| {
            r.process
                .split(':')
                .next()
                .and_then(|p| p.parse::<u32>().ok())
        })
        .map_or_else(|| home.join(".codex"), |pid| codex_home_of(pid, home))
}

/// The hook events Lunchroom installs for Codex, as `hooks.json` names them and as the keys of
/// `config.toml`'s `[hooks.state."…"]` tables do.
const CODEX_EVENTS: [(&str, &str); 5] = [
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("SessionStart", "session_start"),
    ("Stop", "stop"),
];

/// The guard hooks Codex needs from Lunchroom, as `lunchroom install` writes them for Codex: the
/// event and the guard's mode, the same standard as [`CLAUDE_GUARD_HOOKS`] (Codex has no
/// `PostToolUseFailure`). With one missing, a message would reach a session whose protection is
/// partial.
const CODEX_GUARD_HOOKS: [(&str, &str); 4] = [
    ("PreToolUse", "pretool"),
    ("PostToolUse", "posttool"),
    ("UserPromptSubmit", "prompt"),
    ("SessionStart", "start"),
];

/// Codex (measured, codex-cli 0.163.0): the fingerprint of Lunchroom's trusted hooks for the
/// session in process `pid`: a `hooks.json` Codex reads for it (`<CODEX_HOME>/hooks.json`, or the
/// project's `.codex/hooks.json`) holds every one of [`CODEX_GUARD_HOOKS`] (each in a group whose
/// matcher covers every tool, synchronous, its program there to run), and `config.toml` holds the user's trust for
/// every Lunchroom hook in that file (`[hooks.state."<hooks.json>:<event>:<group>:<hook>"]` with a
/// `trusted_hash`, not disabled): a hook the user declined runs nothing, so one declined hook
/// leaves the session unguarded. The fingerprint is, for each hook, the file, its position and
/// definition, and that `trusted_hash`. Codex computes the hash itself and runs no hook whose
/// current definition the user has not trusted, so a guard hook that ran proves the trust then;
/// Lunchroom records this fingerprint when its guard runs, and the session is guarded only while
/// the fingerprint is unchanged.
#[must_use]
pub fn codex_guard_fingerprint(pid: u32, cwd: &Path, home: &Path) -> Option<String> {
    codex_fingerprint_in(&codex_home_of(pid, home), pid, cwd)
}

fn codex_fingerprint_in(codex_home: &Path, pid: u32, cwd: &Path) -> Option<String> {
    codex_guard_in(codex_home, pid, cwd).ok()
}

/// What in a Codex configuration keeps a session from being guarded (R3.4 item 1: "for Codex the
/// setting or hook at fault").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexFault {
    /// `config.toml` does not set [`CODEX_TTY`] to `false` under `[features]` (or is not TOML).
    TtyNotOff,
    /// A profile, or a project's `.codex/config.toml`, turns [`CODEX_TTY`] on, or is not TOML.
    TtyTurnedOn,
    /// No hooks file Codex reads holds every one of [`CODEX_GUARD_HOOKS`]; the user's hooks file.
    NoGuardSet(PathBuf),
    /// The guard set is there, but Lunchroom's hook of `event` in `file` is not trusted, or is
    /// disabled.
    Untrusted { file: PathBuf, event: &'static str },
}

/// What the user is told when [`CODEX_TTY`] is not set to `false`.
const CODEX_TTY_NOT_OFF: &str = "Codex sessions are UNGUARDED until unified_exec_tty is false: run \
     lunchroom install codex in a terminal of your own, then start them anew.";
/// What the user is told when a profile or a project turns [`CODEX_TTY`] on.
const CODEX_TTY_TURNED_ON: &str = "A Codex config.toml turns unified_exec_tty on, in a project or \
     a profile, or is not TOML: the Codex sessions it reaches are UNGUARDED until you change that \
     file.";
/// What the user is told of a Codex session open when the install set [`CODEX_TTY`].
pub(crate) const CODEX_OPENED_BEFORE: &str = "A Codex session started before unified_exec_tty was set is \
     UNGUARDED: start a new session; resuming this one under the same Codex server keeps it \
     UNGUARDED.";

impl CodexFault {
    /// The fault as the user reads it, naming the setting or the hook, and what to do.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::TtyNotOff => CODEX_TTY_NOT_OFF.to_owned(),
            Self::TtyTurnedOn => CODEX_TTY_TURNED_ON.to_owned(),
            Self::NoGuardSet(file) => format!(
                "Codex's hooks file {} does not hold Lunchroom's whole guard (PreToolUse, \
                 PostToolUse, UserPromptSubmit and SessionStart, each on every tool, its program \
                 there to run): run lunchroom install codex in a terminal of your own, then trust \
                 Lunchroom's hooks in Codex's /hooks.",
                file.display()
            ),
            Self::Untrusted { file, event } => format!(
                "Codex has not trusted Lunchroom's {event} hook in {}, or it is disabled, and Codex \
                 runs no hook you have not trusted: open Codex and trust Lunchroom's hooks in \
                 /hooks.",
                file.display()
            ),
        }
    }
}

/// The fingerprint of Lunchroom's trusted guard for the Codex session in process `pid` (see
/// [`codex_guard_fingerprint`]), or the first fault in the configuration: the setting, then the
/// hooks.
pub(crate) fn codex_guard_in(
    codex_home: &Path,
    pid: u32,
    cwd: &Path,
) -> Result<String, CodexFault> {
    let config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap_or_default();
    // Codex runs no hook for `write_stdin`: with terminals on, a HELD agent could type into a shell
    // it opened before the hold, so a session is guarded only with them off (H1.1).
    if codex_tty_setting(&config) != Some(false) {
        return Err(CodexFault::TtyNotOff);
    }
    if turns_tty_on(&config) || codex_project_tty(cwd) {
        return Err(CodexFault::TtyTurnedOn);
    }
    let mut fault = CodexFault::NoGuardSet(codex_home.join("hooks.json"));
    for file in [codex_home.join("hooks.json"), cwd.join(".codex/hooks.json")] {
        let Some(hooks) = read_json(&file) else {
            continue;
        };
        if !CODEX_GUARD_HOOKS
            .iter()
            .all(|(event, mode)| has_guard_hook(&hooks, event, mode, pid, cwd))
        {
            continue;
        }
        let mut trusted = Vec::new();
        let mut declined = None;
        for (state, g, h, hook) in lunchroom_hooks_in(&hooks) {
            let key = format!("{}:{state}:{g}:{h}", file.display());
            if let Some(hash) = trusted_hash(&config, &key) {
                trusted.push(format!("{key}|{hook}|{hash}"));
            } else {
                declined = Some(
                    CODEX_EVENTS
                        .iter()
                        .find(|(_, s)| *s == state)
                        .map_or(state, |(event, _)| *event),
                );
                break;
            }
        }
        match declined {
            None => return Ok(trusted.join("\n")),
            Some(event) => {
                // The first file holding the whole set names the hook at fault.
                if matches!(fault, CodexFault::NoGuardSet(_)) {
                    fault = CodexFault::Untrusted {
                        file: file.clone(),
                        event,
                    };
                }
            }
        }
    }
    Err(fault)
}

/// The feature that lets a Codex agent keep a terminal open and type into it with `write_stdin`.
pub(crate) const CODEX_TTY: &str = "unified_exec_tty";

/// The value of [`CODEX_TTY`] in a Codex `config.toml`, in any form TOML writes it.
pub(crate) fn codex_tty_setting(config: &str) -> Option<bool> {
    tty_in(&config.parse().ok()?)
}

fn tty_in(config: &DocumentMut) -> Option<bool> {
    config.get("features")?.get(CODEX_TTY)?.as_bool()
}

/// `text` turns [`CODEX_TTY`] on, in its `[features]` or in any profile, or is not TOML.
fn turns_tty_on(text: &str) -> bool {
    text.parse::<DocumentMut>().map_or(true, |config| {
        let profile = |item: &toml_edit::Item| {
            let features = item.get("features")?;
            features.get(CODEX_TTY)?.as_bool()
        };
        tty_in(&config) == Some(true)
            || config
                .get("profiles")
                .and_then(toml_edit::Item::as_table_like)
                .is_some_and(|all| all.iter().any(|(_, p)| profile(p) == Some(true)))
    })
}

/// A `.codex/config.toml` in `cwd` or a folder above it turns [`CODEX_TTY`] on. Codex reads each of
/// these in a trusted project; trust is not asked here.
fn codex_project_tty(cwd: &Path) -> bool {
    cwd.ancestors().any(|folder| {
        std::fs::read_to_string(folder.join(".codex/config.toml"))
            .is_ok_and(|text| turns_tty_on(&text))
    })
}

/// Every Lunchroom hook in a Codex `hooks.json`, as its event's `config.toml` name, its group and
/// position, and its definition.
fn lunchroom_hooks_in(hooks: &Value) -> Vec<(&'static str, usize, usize, &Value)> {
    let mut out = Vec::new();
    for (event, state) in CODEX_EVENTS {
        for (g, group) in hooks["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            for (h, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                if is_lunchroom_hook(hook) {
                    out.push((state, g, h, hook));
                }
            }
        }
    }
    out
}

/// A hook whose command runs Lunchroom's program, its path quoted or not.
fn is_lunchroom_hook(hook: &Value) -> bool {
    lunchroom_command(hook).is_some()
}

/// The program and the arguments of a hook command that runs a Lunchroom program, its path quoted
/// or not. `lunchroom install` quotes a path that needs it and reads installed commands back with
/// the same parser (`install::program_and_args`), so every path `install` writes reads the same
/// here (R3.1): a path with a space is the program, not a prefix of it.
fn lunchroom_command(hook: &Value) -> Option<(&str, &str)> {
    let command = hook["command"].as_str()?;
    let (program, args) = crate::install::program_and_args(command);
    // A quoted word ends at its closing quote: `'/p/lunchroom'guard` is one word to the shell.
    let word_ends = command
        .strip_prefix('\'')
        .and_then(|quoted| quoted.get(program.len() + 1..))
        .is_none_or(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
    (word_ends && program.rsplit('/').next() == Some("lunchroom")).then_some((program, args))
}

/// The program of a hook whose command is exactly Lunchroom's `guard <mode>`.
fn guard_program<'a>(hook: &'a Value, mode: &str) -> Option<&'a str> {
    let (program, args) = lunchroom_command(hook)?;
    (args == format!("guard {mode}")).then_some(program)
}

/// The program a Lunchroom plugin file runs: its `PROGRAM = "<path>"` line (`const PROGRAM` in
/// OpenClaw's plugin), as `lunchroom install` writes it.
#[must_use]
pub fn plugin_program(text: &str) -> Option<&str> {
    text.lines().find_map(|l| {
        let rest = l.trim_start();
        let rest = rest.strip_prefix("const ").unwrap_or(rest);
        rest.strip_prefix("PROGRAM = \"")?
            .strip_suffix("\";")
            .or_else(|| rest.strip_prefix("PROGRAM = \"")?.strip_suffix('"'))
    })
}

/// H1.2: the version each adapter passed with; that version and later ones are guarded, an older
/// or unreadable one is UNGUARDED. Claude Code, Codex and OpenClaw by
/// their release numbers; Hermes, a source install of which reports a commit and its date, by the
/// commit the adapter passed with or a later commit date (host facts:
/// `docs/host-facts/openclaw-and-hermes.md`). Claude Code: the adapter table's "Claude Code 2.1.309
/// (the release Anthropic's stable apt, dnf and apk channels served on 2026-11-02)" (§13 H1
/// item 3).
const PASSED_WITH: [(Harness, &str); 4] = [
    (Harness::ClaudeCode, "2.1.309"),
    (Harness::Codex, "0.163.0"),
    (Harness::OpenClaw, "2026.10.7"),
    (Harness::Hermes, HERMES_PASSED_COMMIT),
];
const HERMES_PASSED_COMMIT: &str = "9976f4857b72439d79016354d7b6e1a3ee952e2f";
/// The commit date of `HERMES_PASSED_COMMIT` (seconds since the Unix epoch, `git log -1 --format=%ct`).
const HERMES_PASSED_DATE: u64 = 1_793_038_003;

/// The release number at the start of `text` (`2.1.310`, `0.164.0-x86_64-…`, `2026.10.7`).
fn release(text: &str) -> Option<Vec<u64>> {
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let parts: Option<Vec<u64>> = digits
        .trim_end_matches('.')
        .split('.')
        .map(|p| p.parse().ok())
        .collect();
    parts.filter(|p| p.len() >= 2)
}

/// Codex's npm packages: `@openai/codex` and its platform builds.
const CODEX_PACKAGES: [&str; 7] = [
    "@openai/codex",
    "@openai/codex-linux-x64",
    "@openai/codex-linux-arm64",
    "@openai/codex-darwin-x64",
    "@openai/codex-darwin-arm64",
    "@openai/codex-win32-x64",
    "@openai/codex-win32-arm64",
];

/// The release of `harness`'s program at `path`, as that harness's own install shows it: Claude
/// Code's native install keeps each release at `…/claude/versions/<release>`, and an npm install
/// has the `@anthropic-ai/claude-code` package's `package.json` above its program; Codex keeps a
/// daemon release at `…/.codex/…/releases/<release>-<target>/…`, and its npm packages are named
/// `@openai/codex` or one of its platform builds (`@openai/codex-linux-x64` and the like). A path
/// or package of anything else gives none.
///
/// H1.2 (docs/spec/lunchroom-spec.md §13 H1 item 2): "Lunchroom reads Claude Code's release from
/// the install of the program its session runs: the native installer's
/// `…/claude/versions/<release>`, the npm package `@anthropic-ai/claude-code` above the program, or
/// the system package named `claude-code` that installed the program, from dpkg's records
/// (`/var/lib/dpkg`), apk's (`/lib/apk/db/installed`) or rpm's (`rpm -qf`)." So, for Claude Code
/// after the native and npm readings: the package `claude-code` that owns `path`, from dpkg's
/// records, then apk's, then rpm's (host facts: `docs/host-facts/claude-code.md`; Anthropic's apt,
/// dnf and apk packages each install `/usr/bin/claude`).
#[must_use]
pub fn version_from_path(harness: Harness, path: &Path) -> Option<String> {
    version_from_path_in(harness, path, Path::new("/"))
}

/// [`version_from_path`] with the system's package records read below `root` (`/` in the product;
/// a scratch folder in tests). `rpm` is run only for the real root: it reads its own database.
fn version_from_path_in(harness: Harness, path: &Path, root: &Path) -> Option<String> {
    install_version(harness, path).or_else(|| {
        (harness == Harness::ClaudeCode)
            .then(|| package_version(path, root))
            .flatten()
    })
}

/// The system package whose program is Claude Code's (H1.2).
const CLAUDE_PACKAGE: &str = "claude-code";

/// The version of the installed package named exactly [`CLAUDE_PACKAGE`] that owns `path`, from
/// dpkg's records, then apk's, then (real root only) rpm's; another package owning `path` gives
/// none.
fn package_version(path: &Path, root: &Path) -> Option<String> {
    let path = path.to_str()?;
    dpkg_version(root, path)
        .or_else(|| {
            let installed = std::fs::read_to_string(root.join("lib/apk/db/installed")).ok()?;
            apk_owner_version(&installed, CLAUDE_PACKAGE, path)
        })
        .or_else(|| {
            (root == Path::new("/"))
                .then(|| rpm_version(path))
                .flatten()
        })
        .filter(|v| release(v).is_some())
}

/// dpkg's records below `root`: `var/lib/dpkg/info/<package>.list` or `<package>:<arch>.list` holds
/// one installed path per line, and `var/lib/dpkg/status` the package's stanza (read on this
/// Ubuntu host's own `/var/lib/dpkg`, 2026-11-02). The status file is read only when a list of
/// [`CLAUDE_PACKAGE`] names `path`.
fn dpkg_version(root: &Path, path: &str) -> Option<String> {
    let info = root.join("var/lib/dpkg/info");
    let lists = std::fs::read_dir(&info).ok()?.flatten().filter(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name == format!("{CLAUDE_PACKAGE}.list")
            || name
                .strip_prefix(&format!("{CLAUDE_PACKAGE}:"))
                .and_then(|rest| rest.strip_suffix(".list"))
                .is_some_and(|arch| !arch.is_empty())
    });
    let mut owned = false;
    for entry in lists {
        if std::fs::read_to_string(entry.path()).is_ok_and(|text| dpkg_list_has(&text, path)) {
            owned = true;
            break;
        }
    }
    if !owned {
        return None;
    }
    let status = std::fs::read_to_string(root.join("var/lib/dpkg/status")).ok()?;
    dpkg_installed_version(&status, CLAUDE_PACKAGE)
}

/// A dpkg `.list` file names `path` on a line of its own.
fn dpkg_list_has(list: &str, path: &str) -> bool {
    list.lines().any(|line| line == path)
}

/// The version of `package` installed according to dpkg's `status` text: stanzas separated by a
/// blank line, fields `Package:`, `Status:` and `Version:` at the start of a line (a line starting
/// with a space continues the field above). Installed is the `Status:` words `<selection> ok
/// installed`, the selection `install` or `hold` (dpkg(1), "Package selection states": a package
/// on hold is kept at its version, and is installed); any other state (`config-files`,
/// `half-installed`, `unpacked`, …) or flag (`reinstreq`) is not. A version's epoch (`N:`) is left
/// out.
fn dpkg_installed_version(status: &str, package: &str) -> Option<String> {
    let mut stanza: Vec<&str> = Vec::new();
    let mut lines = status.lines().chain(std::iter::once(""));
    loop {
        let line = lines.next()?;
        if !line.trim().is_empty() {
            stanza.push(line);
            continue;
        }
        let field = |name: &str| {
            stanza.iter().copied().find_map(|l| {
                l.strip_prefix(name)
                    .and_then(|v| v.strip_prefix(':'))
                    .map(str::trim)
            })
        };
        let installed = field("Status").is_some_and(|s| {
            let words: Vec<&str> = s.split_whitespace().collect();
            matches!(words.as_slice(), ["install" | "hold", "ok", "installed"])
        });
        if field("Package") == Some(package)
            && installed
            && let Some(version) = field("Version").filter(|v| !v.is_empty())
        {
            return Some(without_epoch(version).to_owned());
        }
        stanza.clear();
    }
}

/// A Debian version without its epoch: `1:2.1.309-1` is `2.1.309-1`.
fn without_epoch(version: &str) -> &str {
    match version.split_once(':') {
        Some((epoch, rest)) if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) => {
            rest
        }
        _ => version,
    }
}

/// The version of `package` in apk's installed database text, when that package's record names
/// `path`. The format, from Alpine's "Apk spec" (<https://wiki.alpinelinux.org/wiki/Apk_spec>,
/// "Installed Database V2", read 2026-11-02): "This file is located at /lib/apk/db/installed. The
/// installed file is a plaintext file of the same format as APKINDEX", whose lines are "prefixed
/// with a letter, colon, and is followed by the value of the field" with "one blank line between
/// records for a package"; "F: - directory name that was created by package, repeated" and "R: -
/// file name, relative to preceding directory name". The name and version fields are `P` and `V`
/// (apk-tools' apk-package(5): "name (pkgname, P)", "version (pkgver, V)"). Directory names carry
/// no leading `/` (the package lists `usr/bin/claude`).
fn apk_owner_version(installed: &str, package: &str, path: &str) -> Option<String> {
    let wanted = path.strip_prefix('/')?;
    let (mut name, mut version, mut dir, mut owns) = (None, None, None::<&str>, false);
    for line in installed.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if owns
                && name == Some(package)
                && let Some(v) = version.filter(|v: &&str| !v.is_empty())
            {
                return Some(v.to_owned());
            }
            (name, version, dir, owns) = (None, None, None, false);
            continue;
        }
        let Some((key, value)) = line.split_once(':').filter(|(k, _)| k.len() == 1) else {
            continue;
        };
        match key {
            "P" => name = Some(value),
            "V" => version = Some(value),
            "F" => dir = Some(value),
            "R" => {
                if let Some(d) = dir
                    && !d.is_empty()
                    && format!("{d}/{value}") == wanted
                {
                    owns = true;
                }
            }
            _ => {}
        }
    }
    None
}

/// The program `rpm`, as the system has it.
const RPM_PROGRAMS: [&str; 2] = ["/usr/bin/rpm", "/bin/rpm"];

/// How long `rpm -qf` may take before it is stopped.
const RPM_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// rpm's own default database folder. rpm-common(8) (<https://man7.org/linux/man-pages/man8/rpm-common.8.html>,
/// read 2026-11-02): "`--dbpath` DIRECTORY: Use the database in DIRECTORY rather than the default
/// path /var/lib/rpm."
const RPM_DBPATH: &str = "/var/lib/rpm";

/// What rpm answered about the owner of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RpmAnswer {
    /// The package `claude-code` owns it, at this version.
    Claude(String),
    /// rpm finished and says no package named `claude-code` owns it (another package, or none).
    NotClaude,
    /// No answer: rpm is not installed, did not finish in time, or failed.
    Unanswered,
}

/// The arguments of `rpm` for the owner of `path`: `-qf` ("Query package owning installed FILE",
/// rpm(8), <https://man7.org/linux/man-pages/man8/rpm.8.html>, read 2026-11-02) with `--queryformat`
/// ("Output format of each queried package, as described by rpm-queryformat(7)"), and, when the
/// folder exists, `--dbpath` so a macro file cannot move the database away from rpm's default. No
/// `~/.rpmmacros` is read either: `rpm` runs with no `HOME` (`run_rpm`).
fn rpm_args(path: &str, dbpath: Option<&str>) -> Vec<String> {
    let mut args = vec!["-qf".to_owned()];
    if let Some(dbpath) = dbpath {
        args.push("--dbpath".to_owned());
        args.push(dbpath.to_owned());
    }
    args.extend(
        ["--queryformat", "%{NAME}\t%{VERSION}\n", "--", path]
            .iter()
            .map(|a| (*a).to_owned()),
    );
    args
}

/// Runs `rpm` directly (never through a shell) with `args` and no environment but `LC_ALL=C`
/// (so no `HOME`, no user macro file), killed after `limit`; its standard output and error, or
/// `None` when it cannot start or runs past `limit`.
fn run_rpm(rpm: &Path, args: &[String], limit: std::time::Duration) -> Option<String> {
    use std::io::Read as _;
    let mut command = std::process::Command::new(rpm);
    command
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // 1.1: a child never inherits the group that opens the store.
    lunchroom_core::setgid::drop_group(&mut command);
    let mut child = command.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < limit => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(65_536)
        .read_to_string(&mut out)
        .ok()?;
    if let Some(err) = child.stderr.take() {
        let mut said = String::new();
        let _ = err.take(65_536).read_to_string(&mut said);
        out.push_str(&said);
    }
    Some(out)
}

/// `output` is rpm's answer for a file no package owns: "file %s is not owned by any package"
/// (rpm's `lib/query.cc`, `initQueryIterator`, read 2026-11-02).
fn rpm_says_unowned(output: &str) -> bool {
    output.contains("is not owned by any package")
}

/// rpm's answer about `path`, from `rpm` at the first of `programs` that is a file.
fn ask_rpm(
    programs: &[&str],
    path: &str,
    dbpath: Option<&str>,
    limit: std::time::Duration,
) -> RpmAnswer {
    let Some(rpm) = programs.iter().map(Path::new).find(|p| p.is_file()) else {
        return RpmAnswer::Unanswered;
    };
    let Some(out) = run_rpm(rpm, &rpm_args(path, dbpath), limit) else {
        return RpmAnswer::Unanswered;
    };
    if let Some(version) = rpm_owner_version(&out, CLAUDE_PACKAGE) {
        RpmAnswer::Claude(version)
    } else if rpm_says_unowned(&out) || out.lines().any(|l| l.contains('\t')) {
        RpmAnswer::NotClaude
    } else {
        RpmAnswer::Unanswered
    }
}

/// How a file is told apart from a changed one: `<path>\t<inode>\t<mtime seconds>.<nanoseconds>`.
fn file_identity(path: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    Some(format!(
        "{path}\t{}\t{}.{}",
        meta.ino(),
        meta.mtime(),
        meta.mtime_nsec()
    ))
}

/// How many answers the cache file keeps.
const RPM_CACHE_KEEP: usize = 32;

/// An answer of the package `claude-code`: the version that owns the file, or `None` when the
/// package does not own it. (A cache lookup adds a third case: no answer at all.)
type Owner = Option<String>;

/// The answer cached for `identity` in the cache file's `text`: one `<identity>\t<version>` line per
/// answer, the version `-` for a file the package `claude-code` does not own. `None` when `text`
/// holds no answer for it.
fn cache_lookup(text: &str, identity: &str) -> Option<Owner> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(identity)?.strip_prefix('\t')?;
        Some((value != "-").then(|| value.to_owned()))
    })
}

/// `text` with the answer for `identity` set (any earlier answer for the same path replaced), the
/// newest [`RPM_CACHE_KEEP`] kept.
fn cache_store(text: &str, identity: &str, version: Option<&str>) -> String {
    let path = identity.split('\t').next().unwrap_or_default();
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|l| l.split('\t').next() != Some(path))
        .collect();
    let new = format!("{identity}\t{}", version.unwrap_or("-"));
    lines.push(&new);
    let from = lines.len().saturating_sub(RPM_CACHE_KEEP);
    let mut out = lines[from..].join("\n");
    out.push('\n');
    out
}

/// Answers already found in this process.
static RPM_MEMORY: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The file that keeps rpm's answers between runs of the program: `rpm-owner.cache` in Lunchroom's
/// folder (`LUNCHROOM_HOME`, else `~/.lunchroom`).
fn rpm_cache_file() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    // A setgid process (1.1) takes the folder from the fixed place, never from its environment.
    let folder = if lunchroom_core::setgid::active() {
        let ids = lunchroom_core::setgid::current()?;
        lunchroom_core::setgid::store_home(ids.uid)
    } else {
        var("LUNCHROOM_HOME").or_else(|| var("HOME").map(|h| h.join(".lunchroom")))?
    };
    Some(folder.join("rpm-owner.cache"))
}

/// Writes `text` to `file` through a temporary file only the user can read, then renames it.
fn write_cache(file: &Path, text: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let temp = file.with_extension(format!("cache.{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if written.is_ok() {
        lunchroom_core::setgid::settle(&temp);
        let _ = std::fs::rename(&temp, file);
    } else {
        let _ = std::fs::remove_file(&temp);
    }
}

/// The version of [`CLAUDE_PACKAGE`] when rpm's database says it owns `path`: `rpm -qf
/// --queryformat '%{NAME}\t%{VERSION}\n' -- <path>` ([`rpm_args`]), stopped after [`RPM_LIMIT`].
/// An answer is kept by the file's identity (path, inode, change time), in this process and in
/// `rpm-owner.cache`, so the guard does not run `rpm` on every call (G3-4).
fn rpm_version(path: &str) -> Option<String> {
    // H1.7: the step named if the guard's time runs out here.
    let _step = lunchroom_core::steps::begin(format!("asking rpm which package owns {path}"));
    let identity = file_identity(path)?;
    if let Ok(memory) = RPM_MEMORY.lock()
        && let Some(answer) = memory.get(&identity)
    {
        return answer.clone();
    }
    let file = rpm_cache_file();
    let kept = file
        .as_deref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    let answer = if let Some(answer) = cache_lookup(&kept, &identity) {
        answer
    } else {
        let dbpath = Path::new(RPM_DBPATH).is_dir().then_some(RPM_DBPATH);
        match ask_rpm(&RPM_PROGRAMS, path, dbpath, RPM_LIMIT) {
            RpmAnswer::Unanswered => return None,
            RpmAnswer::Claude(version) => {
                if let Some(file) = &file {
                    write_cache(file, &cache_store(&kept, &identity, Some(&version)));
                }
                Some(version)
            }
            RpmAnswer::NotClaude => {
                if let Some(file) = &file {
                    write_cache(file, &cache_store(&kept, &identity, None));
                }
                None
            }
        }
    };
    if let Ok(mut memory) = RPM_MEMORY.lock() {
        memory.insert(identity, answer.clone());
    }
    answer
}

/// rpm's database is on this system but its `rpm` program is not (G3-4): the folder `var/lib/rpm`
/// below `root` exists and none of [`RPM_PROGRAMS`] is a file.
fn rpm_database_without_rpm(root: &Path) -> bool {
    root.join("var/lib/rpm").is_dir()
        && !RPM_PROGRAMS
            .iter()
            .any(|p| root.join(p.trim_start_matches('/')).is_file())
}

/// The version of `package` in the output of `rpm -qf --queryformat '%{NAME}\t%{VERSION}\n'`: one
/// `<name>\t<version>` line per package owning the file; a line of another package, a line that is
/// not that form, or rpm's "is not owned by any package" gives none.
fn rpm_owner_version(output: &str, package: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (name, version) = line.split_once('\t')?;
        (name == package && !version.is_empty() && !version.contains('\t'))
            .then(|| version.to_owned())
    })
}

/// Claude Code's npm packages: `@anthropic-ai/claude-code` and its per-platform packages. Read from
/// the npm registry's metadata for `@anthropic-ai/claude-code@2.1.316`
/// (<https://registry.npmjs.org/@anthropic-ai/claude-code/latest>, read 2026-11-02):
/// `"optionalDependencies"` names `@anthropic-ai/claude-code-` plus `linux-x64`, `linux-arm64`,
/// `linux-x64-musl`, `linux-arm64-musl`, `darwin-x64`, `darwin-arm64`, `win32-x64` and
/// `win32-arm64`, and `"bin":{"claude":"bin/claude.exe"}`; Anthropic's setup page
/// (<https://code.claude.com/docs/en/setup.md>, "Install with npm"): "npm pulls the binary in
/// through a per-platform optional dependency such as `@anthropic-ai/claude-code-darwin-arm64`, and
/// a postinstall step links it into place." A running program is the file in the main package's
/// `bin/` or one in a platform package; each package's `package.json` names itself and carries the
/// release, so either is read the way the main package is.
const CLAUDE_PACKAGES: [&str; 9] = [
    "@anthropic-ai/claude-code",
    "@anthropic-ai/claude-code-linux-x64",
    "@anthropic-ai/claude-code-linux-arm64",
    "@anthropic-ai/claude-code-linux-x64-musl",
    "@anthropic-ai/claude-code-linux-arm64-musl",
    "@anthropic-ai/claude-code-darwin-x64",
    "@anthropic-ai/claude-code-darwin-arm64",
    "@anthropic-ai/claude-code-win32-x64",
    "@anthropic-ai/claude-code-win32-arm64",
];

/// The release written in the path of `harness`'s program (never read from a file): Claude Code's
/// native install keeps each release at `…/claude/versions/<release>`; Homebrew's cask installs the
/// program at `<Caskroom>/claude-code/<release>/claude` (or `claude-code@latest`). Homebrew's
/// `Casks/c/claude-code.rb` (read 2026-11-02): `url
/// "https://downloads.claude.ai/claude-code-releases/#{version}/#{os}-#{arch}/claude"` and `binary
/// "claude"`; the Cask Cookbook (<https://docs.brew.sh/Cask-Cookbook#stanza-binary>): the staged
/// files are under `$(brew --caskroom)/<token>/<version>`, and the `binary` link in
/// `$(brew --prefix)/bin` points back into it. Codex keeps a daemon release at
/// `…/.codex/…/releases/<release>-<target>/…`.
fn path_release_of(harness: Harness, path: &Path) -> Option<String> {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match harness {
        Harness::ClaudeCode => parts
            .windows(3)
            .find(|w| w[0] == "claude" && w[1] == "versions" && release(&w[2]).is_some())
            .map(|w| w[2].clone())
            .or_else(|| {
                let caskroom = parts.iter().position(|p| p == "Caskroom")?;
                parts[caskroom + 1..]
                    .windows(3)
                    .find(|w| {
                        matches!(w[0].as_str(), "claude-code" | "claude-code@latest")
                            && release(&w[1]).is_some()
                            && w[2] == "claude"
                    })
                    .map(|w| w[1].clone())
            }),
        Harness::Codex => parts.iter().position(|p| p == ".codex").and_then(|at| {
            parts[at..]
                .windows(2)
                .find(|w| w[0] == "releases" && release(&w[1]).is_some())
                .map(|w| w[1].clone())
        }),
        Harness::OpenClaw | Harness::Hermes => None,
    }
}

/// The release of `harness`'s program at `path` from the path itself or the npm package above it.
fn install_version(harness: Harness, path: &Path) -> Option<String> {
    let packages: &[&str] = match harness {
        Harness::ClaudeCode => &CLAUDE_PACKAGES,
        Harness::Codex => &CODEX_PACKAGES,
        Harness::OpenClaw | Harness::Hermes => return None,
    };
    if let from_path @ Some(_) = path_release_of(harness, path) {
        return from_path;
    }
    let mut dir = path.parent();
    for _ in 0..8 {
        let Some(d) = dir else { break };
        if let Some(v) = read_json(&d.join("package.json"))
            .filter(|p| p["name"].as_str().is_some_and(|n| packages.contains(&n)))
            .and_then(|p| p["version"].as_str().map(str::to_owned))
            .filter(|v| release(v).is_some())
        {
            return Some(v);
        }
        dir = d.parent();
    }
    None
}

/// What the system shows after the path of a program that was deleted or replaced while it ran:
/// `readlink /proc/<pid>/exe` gives `/usr/bin/claude (deleted)` (`proc_pid_exe(5)`: "if the pathname
/// has been unlinked, the symbolic link will contain the string ' (deleted)' appended to the
/// original pathname"). An upgrade by apt, dnf or apk replaces `/usr/bin/claude` this way.
const DELETED_SUFFIX: &str = " (deleted)";

/// The programs process `pid` runs.
#[derive(Debug, Default, PartialEq, Eq)]
struct Programs {
    /// The binary, then the script a runtime runs (its first argument), each with its links
    /// followed and the binary without a `(deleted)` suffix.
    paths: Vec<PathBuf>,
    /// The binary was replaced while the process ran (its path read `(deleted)`).
    replaced: bool,
}

/// [`Programs`] from what `/proc/<pid>` shows: the target of `exe`, the raw `cmdline`, the process's
/// working folder `cwd` (a relative script is taken from it), and `canonical`, which follows the
/// links of a path that exists (an npm or nvm install links `bin/claude` into the package). A
/// script path that cannot be followed is kept as it is. G3-2: the script is judged by where its
/// links lead, so an nvm or Volta install is read from the package it links to.
fn programs_of(
    exe: Option<PathBuf>,
    cmdline: &[u8],
    cwd: Option<&Path>,
    canonical: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Programs {
    let mut replaced = false;
    let exe = exe.map(
        |p| match p.to_str().and_then(|t| t.strip_suffix(DELETED_SUFFIX)) {
            Some(original) => {
                replaced = true;
                PathBuf::from(original)
            }
            None => p,
        },
    );
    let script = cmdline
        .split(|b| *b == 0)
        .nth(1)
        .filter(|a| !a.is_empty() && !a.starts_with(b"-"))
        .map(|a| PathBuf::from(String::from_utf8_lossy(a).into_owned()))
        .and_then(|p| {
            if p.is_absolute() {
                Some(canonical(&p).unwrap_or(p))
            } else {
                // A relative first argument is a script only if it is a file there (it may be a
                // prompt or a subcommand).
                canonical(&cwd?.join(p))
            }
        });
    Programs {
        paths: exe.into_iter().chain(script).collect(),
        replaced,
    }
}

/// The paths of the program process `pid` runs, as its release is read from them: its binary, then
/// the script a runtime runs (its first argument, absolute or taken from the process's folder, its
/// links followed).
fn process_programs(pid: u32) -> Programs {
    programs_of(
        std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        &std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default(),
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .as_deref(),
        &|p| std::fs::canonicalize(p).ok(),
    )
}

/// The record a plugin harness's guard hook leaves: the plugin's file and the harness's version.
#[must_use]
pub fn plugin_fingerprint(file: &Path, version: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    plugin_fingerprint_at(file, version, now)
}

/// [`plugin_fingerprint`] for the time `at` (seconds since the Unix epoch): `<file>\n<release>\n<at>`.
/// The time is when the plugin reported, which `lunchroom doctor` shows (G3-6); a record from before
/// Lunchroom kept it has two lines and no time.
#[must_use]
pub fn plugin_fingerprint_at(file: &Path, version: &str, at: u64) -> String {
    format!("{}\n{version}\n{at}", file.display())
}

/// When the plugin reported, from a record made by [`plugin_fingerprint_at`].
fn plugin_reported_at(recorded: Option<&str>) -> Option<u64> {
    recorded?.splitn(3, '\n').nth(2)?.trim().parse().ok()
}

/// A 40-character hexadecimal git commit.
fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// H1.2: `version` is the release the adapter for `harness` states in the list `passed` of passing
/// adapters, or a later one; a harness missing from the list has no passing adapter, so no release
/// of it passes. Hermes's release, as its plugin reports it,
/// is a 40-character commit, alone (passing only as the stated commit) or with its commit date
/// after `@` (passing at or after the stated date); anything else cannot be read.
fn passes_with(passed: &[(Harness, &str)], harness: Harness, version: &str) -> bool {
    if harness == Harness::Hermes {
        if !passed.iter().any(|(h, _)| *h == Harness::Hermes) {
            return false;
        }
        return match version.split_once('@') {
            None => is_commit(version) && version == HERMES_PASSED_COMMIT,
            Some((commit, date)) => {
                is_commit(commit)
                    && date.parse::<u64>().is_ok_and(|d| {
                        d >= HERMES_PASSED_DATE
                            && (d > HERMES_PASSED_DATE || commit == HERMES_PASSED_COMMIT)
                    })
            }
        };
    }
    let Some(have) = release(version) else {
        return false;
    };
    passed
        .iter()
        .find(|(h, _)| *h == harness)
        .and_then(|(_, v)| release(v))
        .is_some_and(|stated| have >= stated)
}

/// The harness release a plugin harness's guard hook recorded with its plugin file (H1.2); empty
/// when none was recorded.
fn plugin_release(recorded: Option<&str>) -> &str {
    recorded
        .and_then(|r| r.split_once('\n'))
        .map_or("", |(_, rest)| rest.split('\n').next().unwrap_or_default())
}

/// The plugin file a guard hook recorded with the harness's release.
fn recorded_plugin_file(recorded: Option<&str>) -> Option<&Path> {
    recorded
        .and_then(|r| r.split_once('\n'))
        .map(|(file, _)| Path::new(file))
}

/// R3.4 item 1 for a plugin harness: the plugin file `file` does not run the guard.
pub(crate) fn plugin_fault(harness: Harness, file: &Path) -> String {
    let name = harness_name(harness);
    let reload = match harness {
        Harness::OpenClaw => "openclaw plugins reload lunchroom, or restart the gateway",
        Harness::Hermes | Harness::ClaudeCode | Harness::Codex => {
            "start the Hermes session anew (Hermes loads a plugin when a session starts)"
        }
    };
    format!(
        "The Lunchroom plugin file {name} loaded, {}, no longer runs a Lunchroom program that is \
         there to run: run lunchroom install {} in a terminal of your own, then {reload}.",
        file.display(),
        install_word(harness)
    )
}

/// R3.1: the plugin file `file` runs a Lunchroom program, at an absolute path, that is there to
/// run.
fn plugin_file_runs_guard(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    plugin_program(&text).is_some_and(|program| {
        let path = Path::new(program);
        path.is_absolute()
            && path.file_name().is_some_and(|n| n == "lunchroom")
            && std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// `~/…` with the user's home.
fn expand(path: &str, home: &Path) -> PathBuf {
    path.strip_prefix("~/")
        .map_or_else(|| PathBuf::from(path), |rest| home.join(rest))
}

/// The value of `key` in a `.env` file.
fn dotenv(file: &Path, key: &str) -> Option<String> {
    std::fs::read_to_string(file)
        .ok()?
        .lines()
        .find_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("export ").unwrap_or(l);
            l.strip_prefix(key)?
                .strip_prefix('=')
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_owned())
        })
        .filter(|v| !v.is_empty())
}

/// The `openclaw.json` the gateway in process `pid` reads (OpenClaw `docs/help/environment.md`:
/// these variables may be set in the trusted `.env` of the state folder, `~/.openclaw/.env` by
/// default): `OPENCLAW_CONFIG_PATH` from the gateway's environment, the state folder's `.env` or
/// `~/.openclaw/.env`, else `openclaw.json` in the state folder (`OPENCLAW_STATE_DIR` from the
/// gateway's environment or `~/.openclaw/.env`, default `~/.openclaw`).
fn openclaw_config(pid: u32, home: &Path) -> PathBuf {
    openclaw_config_from(&|k| process_env(pid, k), home)
}

/// [`openclaw_config`] with the environment read through `env`.
pub(crate) fn openclaw_config_from(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> PathBuf {
    let default_dotenv = home.join(".openclaw/.env");
    let state = env("OPENCLAW_STATE_DIR")
        .or_else(|| dotenv(&default_dotenv, "OPENCLAW_STATE_DIR"))
        .map_or_else(|| home.join(".openclaw"), |d| expand(&d, home));
    env("OPENCLAW_CONFIG_PATH")
        .or_else(|| dotenv(&state.join(".env"), "OPENCLAW_CONFIG_PATH"))
        .or_else(|| dotenv(&default_dotenv, "OPENCLAW_CONFIG_PATH"))
        .map_or_else(|| state.join("openclaw.json"), |p| expand(&p, home))
}

// protocol-words:begin (OpenClaw's own `openclaw.json` key; never shown to a person)
const OPENCLAW_ALLOW_LIST: &str = "allow";
// protocol-words:end

/// R3.4 item 1 for OpenClaw: `None` when the `openclaw.json` at `file` (the one the gateway reads,
/// [`openclaw_config`]) has Lunchroom's plugin enabled with conversation access: plugins are on,
/// `lunchroom` is not denied and is allowed where an allow list is set, its entry is enabled, and
/// `hooks.allowConversationAccess` is `true`; else what the user is told. A file that cannot be
/// read counts as not enabled. The steps named are the ones `lunchroom install openclaw` gives.
pub(crate) fn openclaw_fault(file: &Path) -> Option<String> {
    let enabled = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| json5(&t))
        .is_some_and(|config| openclaw_config_enables(&config));
    (!enabled).then(|| {
        format!(
            "OpenClaw's settings ({}) do not enable Lunchroom's plugin with conversation access: \
             run openclaw plugins enable lunchroom and openclaw config set \
             plugins.entries.lunchroom.hooks.allowConversationAccess true --strict-json, then \
             openclaw plugins reload lunchroom, or restart the gateway.",
            file.display()
        )
    })
}

/// `openclaw.json`'s settings enable Lunchroom's plugin with conversation access.
#[must_use]
pub fn openclaw_config_enables(config: &Value) -> bool {
    let plugins = &config["plugins"];
    let listed = |key: &str| {
        plugins[key]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "lunchroom"))
    };
    let allow_set = plugins[OPENCLAW_ALLOW_LIST]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    let entry = &plugins["entries"]["lunchroom"];
    plugins["enabled"] != Value::Bool(false)
        && !listed("deny")
        && (!allow_set || listed(OPENCLAW_ALLOW_LIST))
        && entry["enabled"] == Value::Bool(true)
        && entry["hooks"]["allowConversationAccess"] == Value::Bool(true)
}

/// Hermes's folder for process `pid`: `HERMES_HOME` from the process's environment, default
/// `~/.hermes`.
fn hermes_home_of(pid: u32, home: &Path) -> PathBuf {
    process_env(pid, "HERMES_HOME").map_or_else(|| home.join(".hermes"), |d| expand(&d, home))
}

/// R3.4 item 1 for Hermes: `None` when Lunchroom's plugin is enabled in `hermes_home`:
/// `lunchroom` is in `plugins.enabled` and not in `plugins.disabled` (a disable wins) in
/// `<HERMES_HOME>/config.yaml` (Hermes `hermes_cli/plugins_discovery.py` 112-131, 289-299); else
/// what the user is told. A file that cannot be read counts as not enabled. The steps named are
/// the ones `lunchroom install hermes` gives.
pub(crate) fn hermes_fault(hermes_home: &Path) -> Option<String> {
    let file = hermes_home.join("config.yaml");
    let enabled = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| hermes_plugin_lists(&t))
        .is_some_and(|(enabled, disabled)| {
            enabled.iter().any(|n| n == "lunchroom") && !disabled.iter().any(|n| n == "lunchroom")
        });
    (!enabled).then(|| {
        format!(
            "Hermes's settings ({}) do not enable Lunchroom's plugin: run hermes plugins enable \
             lunchroom, then start the Hermes session anew (Hermes loads a plugin when a session \
             starts).",
            file.display()
        )
    })
}

/// One YAML scalar: without a trailing comment and its quotes.
fn yaml_scalar(text: &str) -> String {
    let text = text.split(" #").next().unwrap_or_default().trim();
    text.trim_matches('"').trim_matches('\'').to_owned()
}

/// The `plugins.enabled` and `plugins.disabled` lists of a Hermes `config.yaml`, in block style
/// (`- name` lines) or flow style (`[a, b]`); `None` when the `plugins` section is written in a
/// form this reader does not read (so the session is not counted as guarded).
#[must_use]
pub fn hermes_plugin_lists(text: &str) -> Option<(Vec<String>, Vec<String>)> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let content = |l: &str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let lines: Vec<&str> = text.lines().filter(|l| content(l)).collect();
    let Some(start) = lines
        .iter()
        .position(|l| indent(l) == 0 && l.trim_end().starts_with("plugins:"))
    else {
        return Some((Vec::new(), Vec::new()));
    };
    if !yaml_scalar(&lines[start]["plugins:".len()..]).is_empty() {
        return None;
    }
    let body: Vec<&str> = lines[start + 1..]
        .iter()
        .take_while(|l| indent(l) > 0)
        .copied()
        .collect();
    let mut lists = (Vec::new(), Vec::new());
    let child = body.first().map_or(0, |l| indent(l));
    let mut k = 0;
    while k < body.len() {
        let line = body[k];
        k += 1;
        if indent(line) != child {
            continue;
        }
        let (key, rest) = line.trim().split_once(':')?;
        let target = match key.trim() {
            "enabled" => &mut lists.0,
            "disabled" => &mut lists.1,
            _ => continue,
        };
        let rest = rest.split(" #").next().unwrap_or_default().trim();
        if let Some(flow) = rest.strip_prefix('[') {
            let flow = flow.strip_suffix(']')?;
            target.extend(flow.split(',').map(yaml_scalar).filter(|s| !s.is_empty()));
        } else if rest.is_empty() {
            while k < body.len() && indent(body[k]) > child {
                let item = body[k].trim().strip_prefix('-')?;
                target.push(yaml_scalar(item));
                k += 1;
            }
        } else {
            return None;
        }
    }
    Some(lists)
}

/// A JSON5 text (comments, trailing commas, single-quoted strings, unquoted keys) as JSON.
#[must_use]
pub fn json5(text: &str) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut k = 0;
    // Skips whitespace and comments from `k`; the index of the next other character.
    let skip = |mut k: usize| -> usize {
        loop {
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if chars.get(k) == Some(&'/') && chars.get(k + 1) == Some(&'/') {
                while k < chars.len() && chars[k] != '\n' {
                    k += 1;
                }
            } else if chars.get(k) == Some(&'/') && chars.get(k + 1) == Some(&'*') {
                k += 2;
                while k + 1 < chars.len() && !(chars[k] == '*' && chars[k + 1] == '/') {
                    k += 1;
                }
                k += 2;
            } else {
                return k;
            }
        }
    };
    while k < chars.len() {
        let c = chars[k];
        if c == '"' || c == '\'' {
            out.push('"');
            k += 1;
            while k < chars.len() && chars[k] != c {
                if chars[k] == '\\' && k + 1 < chars.len() {
                    match chars[k + 1] {
                        '\'' => out.push('\''),
                        '\n' => {}
                        e => {
                            out.push('\\');
                            out.push(e);
                        }
                    }
                    k += 2;
                    continue;
                }
                if chars[k] == '"' {
                    out.push('\\');
                }
                out.push(chars[k]);
                k += 1;
            }
            out.push('"');
            k += 1;
        } else if c == '/' && matches!(chars.get(k + 1), Some('/' | '*')) {
            k = skip(k);
        } else if c == ',' && matches!(chars.get(skip(k + 1)), Some('}' | ']')) {
            k += 1;
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = k;
            while k < chars.len()
                && (chars[k].is_alphanumeric() || chars[k] == '_' || chars[k] == '$')
            {
                k += 1;
            }
            let word: String = chars[start..k].iter().collect();
            if chars.get(skip(k)) == Some(&':') {
                out.push('"');
                out.push_str(&word);
                out.push('"');
            } else {
                out.push_str(&word);
            }
        } else {
            out.push(c);
            k += 1;
        }
    }
    serde_json::from_str(&out).ok()
}

/// A variable in process `pid`'s starting environment.
fn process_env(pid: u32, name: &str) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    raw.split(|b| *b == 0).find_map(|kv| {
        let kv = String::from_utf8_lossy(kv);
        kv.strip_prefix(&format!("{name}="))
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}

/// The `trusted_hash` under `[hooks.state."<key>"]` in `config.toml`, unless that hook is
/// disabled (`enabled = false`).
fn trusted_hash(config: &str, key: &str) -> Option<String> {
    let header = format!("[hooks.state.\"{key}\"]");
    let start = config.find(&header)?;
    let body = &config[start + header.len()..];
    let section = body.split("\n[").next().unwrap_or_default();
    let value = |k: &str| {
        section.lines().find_map(|l| {
            let (name, v) = l.split_once('=')?;
            (name.trim() == k).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    if value("enabled").as_deref() == Some("false") {
        return None;
    }
    value("trusted_hash").filter(|h| !h.is_empty())
}

/// R3.1: the program a guard hook runs is there to run: an executable file at its absolute path,
/// at a relative path from the session's working folder (where the harness's shell runs it), or,
/// for a bare name, in a folder of the harness process's `PATH`. A guard whose program is missing
/// runs nothing, so its session is not guarded.
fn program_present(hook: &Value, pid: u32, cwd: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Some((program, _)) = lunchroom_command(hook) else {
        return false;
    };
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.starts_with('/') {
        return runnable(Path::new(program));
    }
    if program.contains('/') {
        return runnable(&cwd.join(program));
    }
    process_env(pid, "PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| runnable(&dir.join(program))))
}

/// Claude Code's managed settings file.
pub(crate) const MANAGED_SETTINGS: &str = "/etc/claude-code/managed-settings.json";

/// The hooks Claude Code needs from Lunchroom's guard, as `lunchroom install` writes them: the
/// event and the guard's mode. `PreToolUse` decides each call; `PostToolUse` and
/// `PostToolUseFailure` (Claude Code's event for a tool that failed) see each result; and
/// `UserPromptSubmit` sees the prompt that holds the session and confirms a delivery (D1.1). With
/// one missing, a message would reach a session whose protection is partial.
const CLAUDE_GUARD_HOOKS: [(&str, &str); 4] = [
    ("PreToolUse", "pretool"),
    ("PostToolUse", "posttool"),
    ("PostToolUseFailure", "posttool"),
    ("UserPromptSubmit", "prompt"),
];

/// The session in process `pid` is guarded: the hooks Claude Code actually runs for it include
/// every one of [`CLAUDE_GUARD_HOOKS`].
fn configured(pid: u32, cwd: &Path, home: &Path) -> bool {
    let config_dir =
        process_env(pid, "CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    configured_with(
        &cmdline_args(pid),
        &config_dir,
        Path::new(MANAGED_SETTINGS),
        pid,
        cwd,
    )
}

/// `configured` for a Claude Code process started with `args`, whose user settings are in
/// `config_dir` and whose managed settings are `managed`.
///
/// Claude Code merges hook entries across settings levels, so the hooks that run are the union of
/// the levels it loads (<https://code.claude.com/docs/en/hooks.md>, "Hook entries merge across
/// settings levels"): the user, project and local levels named by `--setting-sources` (all three
/// without it), the `--settings` files, and the managed level. Nothing runs under `--bare`.
/// `disableAllHooks` is read as the value left after precedence (local over project over user,
/// `--settings` over those); when true it leaves only the managed hooks, which only a
/// `disableAllHooks` in managed settings stops. The managed `allowManagedHooksOnly` leaves only
/// the managed hooks too.
fn configured_with(
    args: &[String],
    config_dir: &Path,
    managed: &Path,
    pid: u32,
    cwd: &Path,
) -> bool {
    if args.iter().any(|a| a == "--bare") {
        return false;
    }
    let managed = read_json(managed);
    let managed_flag = |key: &str| managed.as_ref().and_then(|m| m[key].as_bool());
    if managed_flag("disableAllHooks") == Some(true) {
        return false;
    }
    let loaded = setting_sources(args);
    let mut levels: Vec<Value> = [
        ("user", config_dir.join("settings.json")),
        ("project", cwd.join(".claude/settings.json")),
        ("local", cwd.join(".claude/settings.local.json")),
    ]
    .into_iter()
    .filter(|(name, _)| {
        loaded
            .as_ref()
            .is_none_or(|l| l.iter().any(|s| s.as_str() == *name))
    })
    .filter_map(|(_, path)| read_json(&path))
    .collect();
    levels.extend(settings_flag(args, cwd));
    let others_off = levels
        .iter()
        .rev()
        .find_map(|v| v["disableAllHooks"].as_bool())
        == Some(true);
    let mut running: Vec<Value> = managed.clone().into_iter().collect();
    if !others_off && managed_flag("allowManagedHooksOnly") != Some(true) {
        running.extend(levels);
    }
    CLAUDE_GUARD_HOOKS.iter().all(|(event, mode)| {
        running
            .iter()
            .any(|v| has_guard_hook(v, event, mode, pid, cwd))
    })
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The arguments process `pid` was started with.
fn cmdline_args(pid: u32) -> Vec<String> {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    let mut args: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    // The last argument ends in a NUL, which leaves one empty piece after it.
    if args.last().is_some_and(String::is_empty) {
        args.pop();
    }
    args
}

/// The values of option `name` in `args`, given as `name value` or `name=value`.
fn option_values<'a>(args: &'a [String], name: &'a str) -> impl Iterator<Item = &'a str> {
    args.iter().enumerate().filter_map(move |(k, a)| {
        if a == name {
            args.get(k + 1).map(String::as_str)
        } else {
            a.strip_prefix(name)?.strip_prefix('=')
        }
    })
}

/// The settings levels named by `--setting-sources` (`user`, `project`, `local`), or `None` when
/// the process was given none and Claude Code loads all three.
fn setting_sources(args: &[String]) -> Option<Vec<String>> {
    option_values(args, "--setting-sources").last().map(|v| {
        v.split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// The settings given to a process started with `args` as `--settings <file or JSON text>`.
fn settings_flag(args: &[String], cwd: &Path) -> Vec<Value> {
    option_values(args, "--settings")
        .filter_map(|given| {
            if given.trim_start().starts_with('{') {
                serde_json::from_str(given).ok()
            } else {
                let path = PathBuf::from(given);
                read_json(&if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                })
            }
        })
        .collect()
}

/// `settings` holds a hook of `event` that runs Lunchroom's `guard <mode>` on every occurrence of
/// the event: its group's matcher is absent, empty or `*` (any other matcher is a tool name or a
/// pattern, which leaves the calls it does not match unchecked; `UserPromptSubmit` has no
/// matcher), and the hook is a synchronous command with no `if` filter whose program is there to
/// run.
fn has_guard_hook(settings: &Value, event: &str, mode: &str, pid: u32, cwd: &Path) -> bool {
    settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| {
            event == "UserPromptSubmit"
                || match &group["matcher"] {
                    Value::Null => true,
                    Value::String(m) => m.is_empty() || m == "*",
                    _ => false,
                }
        })
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .any(|h| {
            guard_program(h, mode).is_some()
                && h["type"] == "command"
                && h["if"].is_null()
                && h["async"].as_bool() != Some(true)
                && h["asyncRewake"].as_bool() != Some(true)
                && program_present(h, pid, cwd)
        })
}

/// C1.4 (`lunchroom doctor`): `None` when the Claude Code settings a new session started with no
/// options in folder `cwd` loads (the user's in `config_dir`, the project's and local ones in
/// `cwd`, the managed ones in `managed`) run Lunchroom's whole guard, each hook's program there to
/// run (a bare name looked up in process `pid`'s `PATH`); else what the user is told. The same
/// reading as a session's evidence ([`configured_with`]).
pub(crate) fn claude_setup_fault(
    config_dir: &Path,
    managed: &Path,
    pid: u32,
    cwd: &Path,
) -> Option<String> {
    (!configured_with(&[], config_dir, managed, pid, cwd)).then(|| {
        format!(
            "Claude Code's settings ({}, and those of the project in {}) do not run Lunchroom's \
             whole guard (PreToolUse, PostToolUse, PostToolUseFailure and UserPromptSubmit, each \
             on every call, its program there to run), or stop every hook (disableAllHooks): run \
             lunchroom install claude-code in a terminal of your own.",
            config_dir.join("settings.json").display(),
            cwd.display()
        )
    })
}

/// C1.4 (`lunchroom doctor`): `None` when Lunchroom's plugin file `file`, where `lunchroom
/// install` writes it, runs a Lunchroom program that is there to run; else what the user is told.
pub(crate) fn plugin_setup_fault(harness: Harness, file: &Path) -> Option<String> {
    (!plugin_file_runs_guard(file)).then(|| {
        format!(
            "Lunchroom's {} plugin file {} is missing, or runs no Lunchroom program that is there \
             to run: run lunchroom install {} in a terminal of your own.",
            harness_name(harness),
            file.display(),
            install_word(harness)
        )
    })
}

/// The program each harness is run by from the user's `PATH`.
#[must_use]
pub const fn program_name(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude",
        Harness::Codex => "codex",
        Harness::OpenClaw => "openclaw",
        Harness::Hermes => "hermes",
    }
}

/// The package OpenClaw is installed as.
const OPENCLAW_PACKAGE: &str = "openclaw";

/// C1.4: the release of an OpenClaw install whose program, its links followed, is `program`.
/// Host facts (`docs/host-facts/openclaw-and-hermes.md`, line 26): "OpenClaw's version is its
/// package's `version` (`package.json`: `"version": "2026.10.7"`); its bin is `openclaw.mjs` at the
/// package root." So the program must be a file named `openclaw.mjs`, and the `package.json`
/// beside it must be named `openclaw`; its `version` is the release. Anything else gives none.
fn openclaw_install_version(program: &Path) -> Option<String> {
    if program.file_name()? != "openclaw.mjs" {
        return None;
    }
    read_json(&program.parent()?.join("package.json"))
        .filter(|p| p["name"].as_str() == Some(OPENCLAW_PACKAGE))
        .and_then(|p| p["version"].as_str().map(str::to_owned))
}

/// C1.4 (`lunchroom doctor`) and H1.2 (docs/spec/lunchroom-spec.md §14 C1 item 4: "the harness
/// release is the release its adapter passed with or later (H1.2): for Claude Code, Codex and
/// OpenClaw read from the install of its program on the user's `PATH`"): the release of
/// `harness` read from the install of its program `found` on the user's `PATH` (its links
/// followed), with the system's package records below `root`: the release, or why it does not
/// pass with what the user does. Hermes's release is not read here ([`hermes_reported_release`]).
///
/// # Errors
/// What the user is told: no program on `PATH`; a release that cannot be read from that install;
/// a release older than the adapter's.
pub(crate) fn path_release(
    harness: Harness,
    found: Option<&Path>,
    root: &Path,
) -> Result<String, String> {
    let name = harness_name(harness);
    let program = program_name(harness);
    let Some(found) = found else {
        return Err(format!(
            "no program named {program} is on your PATH, so {name}'s release cannot be read from \
             its install: put the {program} you run on your PATH."
        ));
    };
    let read = |p: &Path| match harness {
        Harness::OpenClaw => openclaw_install_version(p),
        _ => version_from_path_in(harness, p, root),
    };
    // G3-3: a wrapper script that ends in `exec <program>` is followed to that program, up to
    // three times.
    let mut at = found.to_path_buf();
    let mut version = read(&at);
    for _ in 0..3 {
        if version.as_deref().is_some_and(|v| release(v).is_some()) {
            break;
        }
        let Some(next) = wrapper_target(&at) else {
            break;
        };
        at = next;
        version = read(&at);
    }
    let version = version.unwrap_or_default();
    if release(&version).is_none() {
        if is_volta_shim(found) {
            return Err(volta_shim_text(name, program, found));
        }
        if is_script(&at) {
            return Err(wrapper_text(name, program, found));
        }
        if harness == Harness::ClaudeCode && rpm_database_without_rpm(root) {
            return Err(Why::RpmNotInstalled {
                program: found.display().to_string(),
            }
            .text());
        }
        if harness == Harness::OpenClaw {
            return Err(format!(
                "OpenClaw's release cannot be read from the install of its program ({}): Lunchroom \
                 reads the version in the package.json, named openclaw, beside its openclaw.mjs. \
                 Put the openclaw program of an install of the openclaw package on your PATH.",
                found.display()
            ));
        }
    }
    match version_fault(&PASSED_WITH, harness, &version, at.display().to_string()) {
        None => Ok(format!("{version}, read from {}", at.display())),
        Some(why) => Err(why.text()),
    }
}

/// `path` is a script (it starts with `#!`) rather than a program.
fn is_script(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0_u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"#!"
}

/// The program the wrapper script at `script` runs, from the first `exec` line that names one: the
/// last word of the line that holds a `/` (and is no option or redirection), with `$basedir` and
/// `${basedir}` taken as the script's own folder, its links followed. `None` when `script` is not a
/// script, no `exec` line names a path, or the path is not a file there.
///
/// pnpm writes its global programs as such scripts, `cmd-shim`'s `sh` template
/// (<https://github.com/pnpm/cmd-shim>, `src/index.ts`, read 2026-11-02): `#!/bin/sh`, a `basedir`
/// computed from the script's own path, and `exec ${shProg} ${execArgs} ${shTarget}
/// ${progArgs}"$@"`, where the program is `node` and the target `"$basedir/<path>"`.
fn wrapper_target(script: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(script)
        .ok()?
        .take(65_536)
        .read_to_string(&mut text)
        .ok()?;
    if !text.starts_with("#!") {
        return None;
    }
    let dir = script.parent()?.to_str()?;
    text.lines().find_map(|line| wrapper_exec_target(line, dir))
}

/// The program one `exec` line of a wrapper script runs ([`wrapper_target`]), `dir` being the
/// script's folder.
fn wrapper_exec_target(line: &str, dir: &str) -> Option<PathBuf> {
    let words = line.trim().strip_prefix("exec ")?;
    let target = words
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .rfind(|w| w.contains('/') && !w.starts_with('-') && !w.contains(['<', '>']))?;
    let target = target.replace("${basedir}", dir).replace("$basedir", dir);
    let target = PathBuf::from(target);
    if !target.is_absolute() {
        return None;
    }
    std::fs::canonicalize(target).ok().filter(|p| p.is_file())
}

/// `path` is one of Volta's shims: Volta puts a shim for each tool in its `bin` folder, where its
/// layout (<https://github.com/volta-cli/volta>, `crates/volta-layout/src/v4.rs`, read 2026-11-02:
/// `"bin": shim_dir`) names the folder `bin`, and the shim program is `volta-shim`.
fn is_volta_shim(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.last().is_some_and(|n| n == "volta-shim")
        || parts.windows(2).any(|w| w[0] == ".volta" && w[1] == "bin")
}

/// What the user is told when the program on their `PATH` is a Volta shim. A shim does not hold the
/// release: Volta runs the tool as a child process
/// (<https://github.com/volta-cli/volta>, `crates/volta-core/src/run/executor.rs`,
/// `ToolCommand::execute`: `self.command.status()`), so a running session's own program is the
/// real install, which Lunchroom reads; and `volta which` "will unwrap Volta's shims and locate the
/// actual binary that will be launched by Volta" (<https://docs.volta.sh/reference/which>).
fn volta_shim_text(name: &str, program: &str, found: &Path) -> String {
    format!(
        "the {program} on your PATH, {}, is Volta's shim, which runs whichever {name} install \
         Volta picks for the folder, so no release can be read from it: run volta which \
         {program} to see the install it runs, and put that install's folder first on your PATH. \
         A {name} session that is running is read from the program it runs, whatever your PATH \
         holds.",
        found.display()
    )
}

/// What the user is told when the program on their `PATH` is a script Lunchroom cannot follow.
fn wrapper_text(name: &str, program: &str, found: &Path) -> String {
    format!(
        "the {program} on your PATH, {}, is a script, not {name}'s program, and Lunchroom could \
         not follow it to the program it runs (it follows a line that starts with exec and names \
         the program's path): put {name}'s own {program} program on your PATH, or end the script \
         with exec and the path of that program.",
        found.display()
    )
}

/// C1.4 (docs/spec/lunchroom-spec.md §14 C1 item 4: "for Hermes, whose release its plugin reads
/// inside Hermes, the release the plugin reported in its latest session, a FAIL until a Hermes
/// session has run"): the release Lunchroom's Hermes plugin reported with the guard fingerprint
/// recorded last in any Hermes session ([`Mail::latest_guard_fingerprint`]), the same value a
/// session's evidence reads ([`plugin_release`]), judged by [`passes_with`] as the evidence is.
///
/// # Errors
/// What the user is told: no Hermes session has run; the release reported cannot be read; it is
/// older than the adapter's.
pub(crate) fn hermes_reported_release(mail: &Mail, now: u64) -> Result<String, String> {
    reported_release(mail, Harness::Hermes, now).unwrap_or_else(|| {
        Err(format!(
            "no Hermes session has reported its release to Lunchroom since Lunchroom {} was \
             installed: start a Hermes session, then run lunchroom doctor again.",
            env!("CARGO_PKG_VERSION")
        ))
    })
}

/// When a plugin reported its release, for the user (G3-6): `2026-11-02 14:05 UTC, 3 h 5 min ago`,
/// or that Lunchroom recorded no time for it (a report stored before Lunchroom kept the time).
fn reported_when(at: Option<u64>, now: u64) -> String {
    let Some(at) = at else {
        return "at a time Lunchroom did not record".to_owned();
    };
    let utc = crate::delivery::utc(at);
    let date = utc.get(..10).unwrap_or_default();
    let clock = lunchroom_core::mail::clock(at);
    if at <= now {
        format!(
            "on {date} at {clock}, {} ago",
            lunchroom_core::mail::span(now - at)
        )
    } else {
        format!("on {date} at {clock}")
    }
}

/// The release the plugin of `harness` (OpenClaw or Hermes) reported with the guard fingerprint
/// recorded last in any session of it, judged as the evidence judges it, and when it was reported;
/// `None` when no session of `harness` has reported one.
///
/// # Errors
/// Inside the `Some`: what the user is told when the reported release cannot be read, or is older
/// than the one Lunchroom was tested with.
pub(crate) fn reported_release(
    mail: &Mail,
    harness: Harness,
    now: u64,
) -> Option<Result<String, String>> {
    let recorded = mail.latest_guard_fingerprint(harness)?;
    let name = harness_name(harness);
    let when = reported_when(plugin_reported_at(Some(recorded)), now);
    let version = plugin_release(Some(recorded));
    Some(
        match version_fault(&PASSED_WITH, harness, version, String::new()) {
            None => Ok(format!(
                "{version}, as Lunchroom's plugin reported it in {name}'s latest session, {when}"
            )),
            Some(Why::ReleaseUnread { .. }) => Err(format!(
                "the release Lunchroom's plugin reported in {name}'s latest session, {when}, \
                 cannot be read ({}): run lunchroom install {} in a terminal of your own, so the \
                 plugin reports it, then start a {name} session and run lunchroom doctor again.",
                if version.is_empty() { "none" } else { version },
                install_word(harness)
            )),
            Some(why) => Err(format!(
                "{} The plugin reported this release {when}.",
                why.text()
            )),
        },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// R3.1: the plugin file a guard hook recorded still runs a Lunchroom program, at an absolute
    /// path, that is there to run.
    fn plugin_runs_guard(recorded: Option<&str>) -> bool {
        recorded_plugin_file(recorded).is_some_and(plugin_file_runs_guard)
    }

    // R3.1 (OpenClaw): `openclaw.json` is JSON5; comments, trailing commas, single-quoted strings
    // and unquoted keys read as JSON, a `//` inside a string is text, and a file that is not JSON5
    // reads as nothing (the session is then not guarded).
    #[test]
    fn json5_reads_openclaw_settings() {
        let text = r#"// OpenClaw settings
{
  plugins: {
    /* the guard */
    entries: { 'lunchroom': { enabled: true, hooks: { allowConversationAccess: true, }, }, },
    load: { paths: ["https://example.com//x", 'it\'s'], },
  },
}
"#;
        let v = json5(text);
        assert_eq!(
            v,
            Some(json!({"plugins": {
                "entries": {"lunchroom": {"enabled": true, "hooks": {"allowConversationAccess": true}}},
                "load": {"paths": ["https://example.com//x", "it's"]},
            }}))
        );
        assert_eq!(json5("{ plugins: "), None);
        assert_eq!(json5("{\"a\": 1,}"), Some(json!({"a": 1})));
    }

    // R3.1 (OpenClaw): guarded only while the plugin is on, not denied, allowed where an allow list
    // is set, enabled, and granted conversation access.
    #[test]
    fn openclaw_settings_enable_the_guard_only_with_conversation_access() {
        let on = json!({"plugins": {"entries": {"lunchroom": {
            "enabled": true, "hooks": {"allowConversationAccess": true}}}}});
        assert!(openclaw_config_enables(&on));
        let mut cases = Vec::new();
        for (path, value) in [
            ("/plugins/enabled", json!(false)),
            ("/plugins/deny", json!(["lunchroom"])),
            ("/plugins/allow", json!(["other"])),
            ("/plugins/entries/lunchroom/enabled", json!(false)),
            (
                "/plugins/entries/lunchroom/hooks/allowConversationAccess",
                json!(false),
            ),
        ] {
            let mut c = on.clone();
            if let Some(parent) = path.rsplit_once('/').and_then(|(p, _)| c.pointer_mut(p)) {
                let key = path.rsplit('/').next().unwrap_or_default();
                parent[key] = value;
            }
            cases.push((path, c));
        }
        let mut no_access = on.clone();
        if let Some(h) = no_access.pointer_mut("/plugins/entries/lunchroom/hooks") {
            *h = json!({});
        }
        cases.push(("no grant", no_access));
        cases.push(("no entry", json!({"plugins": {}})));
        for (what, config) in &cases {
            assert!(!openclaw_config_enables(config), "{what}");
        }
        let mut allowed = on;
        allowed["plugins"]["allow"] = json!(["lunchroom", "other"]);
        assert!(openclaw_config_enables(&allowed));
    }

    // H1.2: the version each adapter passed with and later ones pass;
    // an older version, an unreadable one and none do not. §13::2: Claude Code's
    // adapter passed with 2.1.309 (§13 H1 item 3's table), so 2.1.309 and the package's
    // `2.1.309-1` pass and 2.1.308 does not.
    #[test]
    fn the_passed_version_and_later_ones_pass() {
        for (harness, version) in [
            (Harness::ClaudeCode, "2.1.309"),
            (Harness::ClaudeCode, "2.1.309-1"),
            (Harness::ClaudeCode, "2.1.309-r1"),
            (Harness::ClaudeCode, "2.1.310"),
            (Harness::ClaudeCode, "2.1.311"),
            (Harness::ClaudeCode, "2.2.0"),
            (Harness::ClaudeCode, "3.0.0"),
            (Harness::Codex, "0.163.0"),
            (Harness::Codex, "0.164.0-x86_64-unknown-linux-musl"),
            (Harness::Codex, "0.163.0-linux-x64"),
            (Harness::OpenClaw, "2026.10.7"),
            (Harness::OpenClaw, "2026.10.8"),
            (Harness::OpenClaw, "2027.1.1"),
            (Harness::Hermes, "9976f4857b72439d79016354d7b6e1a3ee952e2f"),
            (
                Harness::Hermes,
                "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003",
            ),
            (
                Harness::Hermes,
                "0123456789abcdef0123456789abcdef01234567@1793038004",
            ),
        ] {
            assert!(
                passes_with(&PASSED_WITH, harness, version),
                "{harness:?} {version}"
            );
        }
        for (harness, version) in [
            (Harness::ClaudeCode, "2.1.308"),
            (Harness::ClaudeCode, "2.1.308-1"),
            (Harness::ClaudeCode, "2.0.999"),
            (Harness::ClaudeCode, "unknown"),
            (Harness::ClaudeCode, ""),
            (Harness::Codex, "0.162.9"),
            (Harness::OpenClaw, "2026.10.6"),
            (Harness::OpenClaw, "2026.9.30"),
            (Harness::OpenClaw, "2026"),
            (Harness::Hermes, "9976f48"),
            (
                Harness::Hermes,
                "0123456789abcdef0123456789abcdef01234567@1793038002",
            ),
            (Harness::Hermes, "0123456789abcdef0123456789abcdef01234567"),
            (Harness::Hermes, "2026.10.7"),
            (Harness::Hermes, ""),
            (Harness::Hermes, "not-a-commit@1793038004"),
            (
                Harness::Hermes,
                "9976f4857b72439d79016354d7b6e1a3ee952e2f@not-a-date",
            ),
            (Harness::Hermes, "9976f4857b72439d79016354d7b6e1a3ee952e2f@"),
            (Harness::Hermes, "@1793038004"),
        ] {
            assert!(
                !passes_with(&PASSED_WITH, harness, version),
                "{harness:?} {version}"
            );
        }
    }

    // §13::2 (a2), §13::14 (3): a harness whose adapter has not passed (missing from the
    // list of passing adapters) is not passed at any release, the stated one and later ones
    // included; with it in the list, the same releases pass.
    #[test]
    fn a_harness_whose_adapter_has_not_passed_passes_at_no_release() {
        let without: Vec<(Harness, &str)> = PASSED_WITH
            .iter()
            .copied()
            .filter(|(h, _)| *h != Harness::OpenClaw && *h != Harness::Hermes)
            .collect();
        for (harness, version) in [
            (Harness::OpenClaw, "2026.10.7"),
            (Harness::OpenClaw, "2027.1.1"),
            (Harness::Hermes, "9976f4857b72439d79016354d7b6e1a3ee952e2f"),
            (
                Harness::Hermes,
                "0123456789abcdef0123456789abcdef01234567@1893038004",
            ),
        ] {
            assert!(
                !passes_with(&without, harness, version),
                "{harness:?} {version}"
            );
            assert!(
                passes_with(&PASSED_WITH, harness, version),
                "{harness:?} {version}"
            );
        }
    }

    // H1.2: a harness's release is read from its own install: its install's path, or its own
    // package's `package.json` above its program; another program's path or package gives none.
    #[test]
    fn versions_are_read_from_the_harness_own_install() {
        let claude = Harness::ClaudeCode;
        let codex = Harness::Codex;
        assert_eq!(
            version_from_path(
                claude,
                Path::new("/home/u/.local/share/claude/versions/2.1.310")
            ),
            Some("2.1.310".to_owned())
        );
        let daemon = Path::new(
            "/home/u/.codex/packages/app-server-daemon/releases/0.164.0-x86_64-unknown-linux-musl/bin/codex",
        );
        assert_eq!(
            version_from_path(codex, daemon),
            Some("0.164.0-x86_64-unknown-linux-musl".to_owned())
        );
        // Another harness's install, or a look-alike folder, is not this harness's release.
        assert_eq!(version_from_path(claude, daemon), None);
        for other in [
            "/tmp/x/versions/9.9.9",
            "/tmp/x/claude-other/versions/9.9.9",
            "/tmp/releases/9.9.9-x/bin/codex",
        ] {
            assert_eq!(version_from_path(claude, Path::new(other)), None, "{other}");
            assert_eq!(version_from_path(codex, Path::new(other)), None, "{other}");
        }
        let root = crate::test_target()
            .join("version-unit")
            .join(std::process::id().to_string());
        let codex_bin = root.join("node_modules/@openai/codex/bin");
        let other_bin = root.join("node_modules/other/bin");
        let _ = std::fs::create_dir_all(&codex_bin);
        let _ = std::fs::create_dir_all(&other_bin);
        let _ = std::fs::write(
            root.join("node_modules/@openai/codex/package.json"),
            r#"{"name": "@openai/codex", "version": "0.163.0"}"#,
        );
        let _ = std::fs::write(
            root.join("node_modules/other/package.json"),
            r#"{"name": "other", "version": "99.0.0"}"#,
        );
        assert_eq!(
            version_from_path(codex, &codex_bin.join("codex.js")),
            Some("0.163.0".to_owned())
        );
        assert_eq!(version_from_path(claude, &codex_bin.join("codex.js")), None);
        assert_eq!(version_from_path(codex, &other_bin.join("x.js")), None);
        // A look-alike package name gives no release.
        let fake = root.join("node_modules/@anthropic-ai/claude-code-other");
        let _ = std::fs::create_dir_all(fake.join("bin"));
        let _ = std::fs::write(
            fake.join("package.json"),
            r#"{"name": "@anthropic-ai/claude-code-other", "version": "99.0.0"}"#,
        );
        assert_eq!(version_from_path(claude, &fake.join("bin/cli.js")), None);
        assert_eq!(version_from_path(claude, Path::new("/usr/bin/sleep")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // R3.1 (OpenClaw): the configuration file is found where OpenClaw finds it: the gateway's
    // environment first, then the state folder's `.env`, then `~/.openclaw/.env`.
    #[test]
    fn openclaw_config_is_found_where_openclaw_finds_it() {
        let home = crate::test_target()
            .join("openclaw-config-unit")
            .join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(home.join(".openclaw"));
        let _ = std::fs::create_dir_all(home.join("state"));
        let none = |_: &str| None;
        assert_eq!(
            openclaw_config_from(&none, &home),
            home.join(".openclaw/openclaw.json")
        );
        let _ = std::fs::write(home.join(".openclaw/.env"), "OPENCLAW_STATE_DIR=~/state\n");
        assert_eq!(
            openclaw_config_from(&none, &home),
            home.join("state/openclaw.json")
        );
        let _ = std::fs::write(
            home.join("state/.env"),
            "export OPENCLAW_CONFIG_PATH=\"~/state/custom.json\"\n",
        );
        assert_eq!(
            openclaw_config_from(&none, &home),
            home.join("state/custom.json")
        );
        let env = |k: &str| (k == "OPENCLAW_CONFIG_PATH").then(|| "/etc/oc.json".to_owned());
        assert_eq!(
            openclaw_config_from(&env, &home),
            PathBuf::from("/etc/oc.json")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // R3.1 (Hermes): the plugin lists of `config.yaml`, block and flow style, with comments and
    // quotes; a disable wins; a `plugins` section in a form not read gives nothing.
    #[test]
    fn hermes_plugin_lists_read_both_yaml_styles() {
        let block = "model: x\nplugins:\n  # ours\n  enabled:\n    - lunchroom\n    - 'other'\n  disabled: []\nnext: 1\n";
        assert_eq!(
            hermes_plugin_lists(block),
            Some((vec!["lunchroom".to_owned(), "other".to_owned()], Vec::new()))
        );
        let flow = "plugins:\n  enabled: [\"lunchroom\", other]  # set by hermes\n  disabled: [lunchroom]\n";
        assert_eq!(
            hermes_plugin_lists(flow),
            Some((
                vec!["lunchroom".to_owned(), "other".to_owned()],
                vec!["lunchroom".to_owned()]
            ))
        );
        assert_eq!(
            hermes_plugin_lists("model: x\n"),
            Some((Vec::new(), Vec::new()))
        );
        assert_eq!(
            hermes_plugin_lists("plugins: {enabled: [lunchroom]}\n"),
            None
        );
        assert_eq!(
            hermes_plugin_lists("plugins:\n  enabled: lunchroom\n"),
            None
        );
    }

    // §13::2 (a2), §13::14 (3) through the evidence itself: a live OpenClaw session in
    // this process, its plugin file running a Lunchroom program, `openclaw.json` enabling the plugin
    // with conversation access, its hook having reported release R. With OpenClaw among the passing
    // adapters it is guarded at the stated release and a later one; with OpenClaw's adapter not
    // among them it is UNGUARDED at both (so the listener and delivery, which act only on a guarded
    // session, give it nothing and its messages stay QUEUED).
    #[test]
    fn a_session_whose_adapter_has_not_passed_is_unguarded_at_any_release() {
        let root = crate::test_target()
            .join("evidence-unit")
            .join(std::process::id().to_string());
        let home = root.join("home");
        let _ = std::fs::create_dir_all(home.join(".openclaw"));
        let _ = std::fs::write(
            home.join(".openclaw/openclaw.json"),
            "{plugins: {entries: {lunchroom: {enabled: true, hooks: {allowConversationAccess: true}}}}}",
        );
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let plugin = root.join("index.mjs");
        let _ = std::fs::write(
            &plugin,
            format!("const PROGRAM = \"{}\";\n", bin.join("lunchroom").display()),
        );
        let process = crate::process::key(std::process::id()).unwrap_or_default();
        let who = Identity::new(Harness::OpenClaw, "agent:main:x");
        let without: Vec<(Harness, &str)> = PASSED_WITH
            .iter()
            .copied()
            .filter(|(h, _)| *h != Harness::OpenClaw)
            .collect();
        for release in ["2026.10.7", "2026.11.0"] {
            let mut mail = Mail::default();
            mail.observe_process(&process, &root, &who);
            mail.record_guard_fingerprint(&who, Some(plugin_fingerprint(&plugin, release)));
            assert_eq!(
                assess_with(&PASSED_WITH, &mail, &who, &home).evidence(),
                Evidence::Guarded,
                "{release}"
            );
            assert_eq!(
                assess_with(&without, &mail, &who, &home),
                Assessment::Unguarded(Why::NoAdapter(Harness::OpenClaw)),
                "{release}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A live plugin-harness session in this test's own process, for the R3.4 cases: its home, its
    /// plugin file and the program that file runs.
    struct PluginSession {
        root: PathBuf,
        home: PathBuf,
        plugin: PathBuf,
        who: Identity,
    }

    impl PluginSession {
        fn new(harness: Harness) -> Self {
            let root = crate::test_target()
                .join("reason-unit")
                .join(format!("{}-{harness:?}", std::process::id()));
            let home = root.join("home");
            let bin = root.join("bin");
            let _ = std::fs::create_dir_all(&bin);
            let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
            let plugin = root.join("plugin");
            let _ = std::fs::write(
                &plugin,
                format!("PROGRAM = \"{}\"\n", bin.join("lunchroom").display()),
            );
            let who = Identity::new(harness, "s1");
            Self {
                root,
                home,
                plugin,
                who,
            }
        }

        /// The harness's settings enabling Lunchroom's plugin, or not.
        fn enable(&self, on: bool) {
            let (file, text) = if self.who.harness == Harness::OpenClaw {
                (
                    self.home.join(".openclaw/openclaw.json"),
                    format!(
                        "{{plugins: {{entries: {{lunchroom: {{enabled: {on}, hooks: \
                         {{allowConversationAccess: true}}}}}}}}}}"
                    ),
                )
            } else {
                (
                    self.home.join(".hermes/config.yaml"),
                    format!(
                        "plugins:\n  enabled: [{}]\n",
                        if on { "lunchroom" } else { "" }
                    ),
                )
            };
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, text);
        }

        /// The judgment with the guard hook having recorded `release` (none: no hook recorded).
        fn assess(&self, release: Option<&str>) -> Assessment {
            let mut mail = Mail::default();
            let process = crate::process::key(std::process::id()).unwrap_or_default();
            mail.observe_process(&process, &self.root, &self.who);
            mail.record_guard_fingerprint(
                &self.who,
                release.map(|r| plugin_fingerprint(&self.plugin, r)),
            );
            assess(&mail, &self.who, &self.home)
        }
    }

    impl Drop for PluginSession {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    // §4::9 (R3.4, R3.5 tests; OpenClaw, Hermes): each R3.4 case gives its own reason, the
    // first the evidence lacks, in R3.4's order: the settings not enabling the plugin (naming the
    // harness and its settings file) before a missing hook record; the plugin file no longer
    // running the guard (naming the file); no guard hook recorded; a release that cannot be read
    // (naming the program's path); an older release (naming both). Each text says what the user
    // does. With everything in place the session is guarded and has no reason.
    #[test]
    fn each_unguarded_case_of_a_plugin_harness_gives_its_own_reason() {
        for (harness, passed, older, unreadable) in [
            (Harness::OpenClaw, "2026.10.7", "2026.10.6", "openclaw"),
            (
                Harness::Hermes,
                "9976f4857b72439d79016354d7b6e1a3ee952e2f@1793038003",
                "0123456789abcdef0123456789abcdef01234567@1793038002",
                "0123456789abcdef0123456789abcdef01234567",
            ),
        ] {
            let s = PluginSession::new(harness);
            let name = harness_name(harness);
            let reason = |a: Assessment| match a {
                Assessment::Unguarded(why) => Some(why),
                Assessment::NotRunning | Assessment::Guarded => None,
            };
            let text_of = |why: Option<&Why>| why.map(Why::text).unwrap_or_default();
            // Item 1 first: the settings do not enable the plugin, and no hook has run either.
            s.enable(false);
            let why = reason(s.assess(None));
            assert!(
                matches!(why, Some(Why::Configuration(_))),
                "{name}: {why:?}"
            );
            let text = text_of(why.as_ref());
            assert!(text.starts_with(&format!("{name}'s settings (")), "{text}");
            assert!(text.contains("lunchroom"), "{text}");
            // The plugin file the hook recorded runs no Lunchroom program.
            s.enable(true);
            let _ = std::fs::write(&s.plugin, "nothing here\n");
            let text = text_of(reason(s.assess(Some(passed))).as_ref());
            assert!(
                text.contains(&s.plugin.display().to_string())
                    && text.contains(&format!("lunchroom install {}", install_word(harness))),
                "{text}"
            );
            let _ = std::fs::write(
                &s.plugin,
                format!("PROGRAM = \"{}\"\n", s.root.join("bin/lunchroom").display()),
            );
            // Item 2: no guard hook recorded in this session.
            assert_eq!(reason(s.assess(None)), Some(Why::NoGuardRan(harness)));
            assert!(Why::NoGuardRan(harness).text().contains(&format!(
                "No Lunchroom guard hook has run in this {name} session"
            )));
            // Item 3: a release that cannot be read names the program's path.
            let exe = std::fs::read_link("/proc/self/exe").unwrap_or_default();
            for unread in ["", unreadable] {
                let why = reason(s.assess(Some(unread)));
                assert!(
                    matches!(&why, Some(Why::ReleaseUnread { harness: h, .. }) if *h == harness),
                    "{name} {unread:?}: {why:?}"
                );
                let text = text_of(why.as_ref());
                assert!(text.contains(&exe.display().to_string()), "{text}");
            }
            // Item 4: an older release names both releases.
            let text = text_of(reason(s.assess(Some(older))).as_ref());
            assert!(text.contains(older), "{text}");
            let stated = PASSED_WITH
                .iter()
                .find(|(h, _)| *h == harness)
                .map(|(_, v)| *v)
                .unwrap_or_default();
            assert!(
                text.contains(stated) && text.contains("older than"),
                "{text}"
            );
            // All in place: guarded, no reason.
            assert_eq!(s.assess(Some(passed)), Assessment::Guarded, "{name}");
        }
    }

    // §4::9 (R3.4; Codex): the setting or hook at fault is named: `unified_exec_tty` not
    // false; a profile or project turning it on; the guard set missing from the hooks file
    // (naming it); one Lunchroom hook not trusted or disabled (naming its event and file). Each
    // text says what the user does.
    #[test]
    fn a_codex_configuration_fault_names_the_setting_or_hook() {
        let root = crate::test_target()
            .join("codex-fault-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let program = bin.join("lunchroom").display().to_string();
        let file = codex_home.join("hooks.json");
        let fault = || codex_guard_in(&codex_home, std::process::id(), &root).err();
        let _ = std::fs::write(codex_home.join("config.toml"), "");
        assert_eq!(fault(), Some(CodexFault::TtyNotOff));
        assert!(
            CodexFault::TtyNotOff
                .text()
                .contains("unified_exec_tty is false")
        );
        let off = "[features]\nunified_exec_tty = false\n";
        let _ = std::fs::write(
            codex_home.join("config.toml"),
            format!("{off}[profiles.p.features]\nunified_exec_tty = true\n"),
        );
        assert_eq!(fault(), Some(CodexFault::TtyTurnedOn));
        assert!(
            CodexFault::TtyTurnedOn
                .text()
                .contains("turns unified_exec_tty on")
        );
        let _ = std::fs::write(codex_home.join("config.toml"), off);
        assert_eq!(fault(), Some(CodexFault::NoGuardSet(file.clone())));
        let text = CodexFault::NoGuardSet(file.clone()).text();
        assert!(
            text.contains(&file.display().to_string()) && text.contains("lunchroom install codex"),
            "{text}"
        );
        let hook = |mode: &str| json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}]);
        let _ = std::fs::write(
            &file,
            json!({"hooks": {
                "PreToolUse": hook("pretool"),
                "PostToolUse": hook("posttool"),
                "UserPromptSubmit": hook("prompt"),
                "SessionStart": hook("start"),
            }})
            .to_string(),
        );
        let trust = |state: &str, extra: &str| {
            format!(
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"\n{extra}",
                file.display()
            )
        };
        // PreToolUse trusted, PostToolUse not: PostToolUse is named.
        let _ = std::fs::write(
            codex_home.join("config.toml"),
            format!("{off}{}", trust("pre_tool_use", "")),
        );
        let untrusted = CodexFault::Untrusted {
            file: file.clone(),
            event: "PostToolUse",
        };
        assert_eq!(fault(), Some(untrusted.clone()));
        let text = untrusted.text();
        assert!(
            text.contains("PostToolUse")
                && text.contains("/hooks")
                && text.contains(&file.display().to_string()),
            "{text}"
        );
        // Every hook trusted but one disabled: that one is named.
        let all: String = ["pre_tool_use", "post_tool_use", "user_prompt_submit"]
            .iter()
            .map(|s| trust(s, ""))
            .chain(std::iter::once(trust("session_start", "enabled = false\n")))
            .collect();
        let _ = std::fs::write(codex_home.join("config.toml"), format!("{off}{all}"));
        assert_eq!(
            fault(),
            Some(CodexFault::Untrusted {
                file: file.clone(),
                event: "SessionStart",
            })
        );
        let all: String = [
            "pre_tool_use",
            "post_tool_use",
            "user_prompt_submit",
            "session_start",
        ]
        .iter()
        .map(|s| trust(s, ""))
        .collect();
        let _ = std::fs::write(codex_home.join("config.toml"), format!("{off}{all}"));
        assert_eq!(fault(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // §4::9 (R3.4 items 3 and 4), §13::2: a release that cannot be read is item 3 (naming
    // the program and the installs read); a readable older one is item 4 (naming both); the
    // adapter's release and later ones give no reason; a harness with no passing adapter says so.
    #[test]
    fn a_release_fault_is_unread_or_older_naming_what_it_read() {
        let unread = version_fault(
            &PASSED_WITH,
            Harness::ClaudeCode,
            "",
            "/usr/bin/claude".to_owned(),
        );
        assert_eq!(
            unread,
            Some(Why::ReleaseUnread {
                harness: Harness::ClaudeCode,
                program: "/usr/bin/claude".to_owned()
            })
        );
        let text = unread.map(|w| w.text()).unwrap_or_default();
        for part in [
            "/usr/bin/claude",
            "…/claude/versions/<release>",
            "@anthropic-ai/claude-code",
            "claude-code",
            "dpkg, apk or rpm",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        let older = version_fault(
            &PASSED_WITH,
            Harness::ClaudeCode,
            "2.1.308",
            "/x".to_owned(),
        );
        let text = older.map(|w| w.text()).unwrap_or_default();
        assert!(
            text.contains("2.1.308") && text.contains("2.1.309") && text.contains("older than"),
            "{text}"
        );
        assert_eq!(
            version_fault(
                &PASSED_WITH,
                Harness::ClaudeCode,
                "2.1.309",
                "/x".to_owned()
            ),
            None
        );
        let codex = version_fault(&PASSED_WITH, Harness::Codex, "0.162.0", "/x".to_owned());
        assert!(
            codex.is_some_and(|w| w.text().contains("0.162.0") && w.text().contains("0.163.0"))
        );
        let codex = version_fault(&PASSED_WITH, Harness::Codex, "x", "/bin/codex".to_owned());
        assert!(
            codex.is_some_and(
                |w| w.text().contains("@openai/codex") && w.text().contains("/bin/codex")
            )
        );
        let without: Vec<(Harness, &str)> = PASSED_WITH
            .iter()
            .copied()
            .filter(|(h, _)| *h != Harness::Codex)
            .collect();
        assert_eq!(
            version_fault(&without, Harness::Codex, "0.163.0", "/x".to_owned()),
            Some(Why::NoAdapter(Harness::Codex))
        );
    }

    /// A scratch root holding dpkg's or apk's records, for the package readings.
    struct Records(PathBuf);

    impl Records {
        fn new(name: &str) -> Self {
            let root = crate::test_target()
                .join("package-unit")
                .join(format!("{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::create_dir_all(root.join("var/lib/dpkg/info"));
            let _ = std::fs::create_dir_all(root.join("lib/apk/db"));
            Self(root)
        }

        fn write(&self, relative: &str, text: &str) {
            let _ = std::fs::write(self.0.join(relative), text);
        }
    }

    impl Drop for Records {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A dpkg status stanza.
    fn stanza(package: &str, status: &str, version: &str) -> String {
        format!(
            "Package: {package}\nStatus: {status}\nPriority: optional\nArchitecture: amd64\n\
             Version: {version}\nDescription: x\n continued line\n .\n more\n"
        )
    }

    // §13::19 (H1.2, dpkg): the version of the installed package named exactly
    // `claude-code` whose `.list` names the program's path. Failure classes: another package owns
    // the path; the package is not installed (`deinstall ok config-files`, `install ok unpacked`,
    // `install reinstreq installed`); an epoch; a multi-arch list name; a held package; missing
    // files; malformed text; a look-alike package name; a path only as a prefix of a listed line.
    #[test]
    fn dpkg_records_give_the_claude_code_package_owning_the_path() {
        let claude = Path::new("/usr/bin/claude");
        let r = Records::new("dpkg");
        // Missing records: nothing.
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            None
        );
        let status = [
            stanza("coreutils", "install ok installed", "9.4-3ubuntu6.3"),
            stanza("claude-code", "install ok installed", "2.1.309-1"),
        ]
        .join("\n");
        r.write("var/lib/dpkg/status", &status);
        // A list missing: nothing, though the status names the package.
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            None
        );
        r.write(
            "var/lib/dpkg/info/claude-code.list",
            "/.\n/usr\n/usr/bin\n/usr/bin/claude\n",
        );
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            Some("2.1.309-1".to_owned())
        );
        // Only Claude Code's program, and only for Claude Code.
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, Path::new("/usr/bin/claud"), &r.0),
            None
        );
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, Path::new("/usr/bin/claude2"), &r.0),
            None
        );
        assert_eq!(version_from_path_in(Harness::Codex, claude, &r.0), None);
        // Another package owns the path: nothing.
        let _ = std::fs::remove_file(r.0.join("var/lib/dpkg/info/claude-code.list"));
        r.write("var/lib/dpkg/info/coreutils.list", "/usr/bin/claude\n");
        r.write(
            "var/lib/dpkg/info/claude-code-extra.list",
            "/usr/bin/claude\n",
        );
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            None
        );
        // A multi-arch list name counts.
        r.write(
            "var/lib/dpkg/info/claude-code:amd64.list",
            "/usr/bin/claude\n",
        );
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            Some("2.1.309-1".to_owned())
        );
        // Status: installed only as `<install|hold> ok installed`; the epoch is left out.
        for (state, version, read) in [
            ("hold ok installed", "2.1.309-1", Some("2.1.309-1")),
            ("install ok installed", "1:2.1.309-1", Some("2.1.309-1")),
            ("deinstall ok config-files", "2.1.309-1", None),
            ("install ok unpacked", "2.1.309-1", None),
            ("install ok half-configured", "2.1.309-1", None),
            ("install reinstreq installed", "2.1.309-1", None),
            ("install ok", "2.1.309-1", None),
            ("install ok installed", "", None),
            ("install ok installed", "not-a-release", None),
        ] {
            r.write(
                "var/lib/dpkg/status",
                &stanza("claude-code", state, version),
            );
            assert_eq!(
                version_from_path_in(Harness::ClaudeCode, claude, &r.0).as_deref(),
                read,
                "{state} {version}"
            );
        }
        // Malformed text: no stanza of the package reads as installed.
        r.write(
            "var/lib/dpkg/status",
            "Package claude-code\nStatus install ok installed\n",
        );
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            None
        );
        assert_eq!(without_epoch("12:3.0"), "3.0");
        assert_eq!(without_epoch("x:3.0"), "x:3.0");
        assert_eq!(without_epoch(":3.0"), ":3.0");
        assert!(!dpkg_list_has(
            "/usr/bin/claude-old\n /usr/bin/claude\n",
            "/usr/bin/claude"
        ));
    }

    // §13::19 (H1.2, apk): the version of package `claude-code` whose record (`F:` folder
    // then `R:` file) names the path. Failure classes: another package owns it; the file named in
    // another folder; a look-alike name; a record without a version; missing file; malformed lines.
    #[test]
    fn apk_records_give_the_claude_code_package_owning_the_path() {
        let claude = Path::new("/usr/bin/claude");
        let r = Records::new("apk");
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            None
        );
        let record = |name: &str, version: &str, dir: &str, file: &str| {
            format!(
                "C:Q1abc=\nP:{name}\nV:{version}\nA:x86_64\nT:Claude Code\nF:{dir}\nR:{file}\nZ:Q1x=\n"
            )
        };
        let db = [
            record("busybox", "1.36.1-r29", "bin", "busybox"),
            record("claude-code", "2.1.309-r1", "usr/bin", "claude"),
        ]
        .join("\n");
        r.write("lib/apk/db/installed", &db);
        assert_eq!(
            version_from_path_in(Harness::ClaudeCode, claude, &r.0),
            Some("2.1.309-r1".to_owned())
        );
        for (what, db) in [
            (
                "another package",
                record("other", "9.9.9-r0", "usr/bin", "claude"),
            ),
            (
                "look-alike name",
                record("claude-code-bin", "2.1.309-r1", "usr/bin", "claude"),
            ),
            (
                "another folder",
                record("claude-code", "2.1.309-r1", "usr/local/bin", "claude"),
            ),
            (
                "another file",
                record("claude-code", "2.1.309-r1", "usr/bin", "claude2"),
            ),
            ("no version", record("claude-code", "", "usr/bin", "claude")),
            (
                "malformed",
                "Pclaude-code\nV 2.1.309\nusr/bin/claude\n".to_owned(),
            ),
        ] {
            r.write("lib/apk/db/installed", &db);
            assert_eq!(
                version_from_path_in(Harness::ClaudeCode, claude, &r.0),
                None,
                "{what}"
            );
        }
        // The owner is the record whose folder precedes the file: a file of the same name under
        // the package's other folder is not the program.
        let two = "P:claude-code\nV:2.1.309-r1\nF:usr/share/claude\nR:claude\nF:usr/bin\nR:other\n";
        assert_eq!(
            apk_owner_version(two, "claude-code", "/usr/bin/claude"),
            None
        );
        assert_eq!(
            apk_owner_version(two, "claude-code", "/usr/share/claude/claude"),
            Some("2.1.309-r1".to_owned())
        );
        assert_eq!(
            apk_owner_version(two, "claude-code", "usr/bin/claude"),
            None
        );
    }

    // §13::19 (H1.2, rpm): `rpm -qf --queryformat '%{NAME}\t%{VERSION}\n'` output gives
    // the version only on a line of the package named exactly `claude-code`. Failure classes:
    // another owner; several owners; "not owned"; malformed lines; an empty version.
    #[test]
    fn rpm_output_gives_the_claude_code_package_owning_the_path() {
        assert_eq!(
            rpm_owner_version("claude-code\t2.1.309\n", "claude-code"),
            Some("2.1.309".to_owned())
        );
        assert_eq!(
            rpm_owner_version("other\t1.0\nclaude-code\t2.1.309\n", "claude-code"),
            Some("2.1.309".to_owned())
        );
        for output in [
            "",
            "other\t2.1.309\n",
            "claude-code-bin\t2.1.309\n",
            "file /usr/bin/claude is not owned by any package\n",
            "claude-code 2.1.309\n",
            "claude-code\t\n",
            "claude-code\t2.1.309\textra\n",
        ] {
            assert_eq!(rpm_owner_version(output, "claude-code"), None, "{output:?}");
        }
        // rpm runs only for the real root: a scratch root never runs it.
        let r = Records::new("rpm");
        assert_eq!(package_version(Path::new("/usr/bin/claude"), &r.0), None);
    }

    // R3.1 (Codex): the session is guarded only while the user has trusted every Lunchroom hook in
    // `hooks.json`, all five events; one declined (no trust entry, or `enabled = false`) leaves it
    // unguarded, and the fingerprint carries every hook's trust.
    #[test]
    fn a_lunchroom_hook_is_known_by_its_program_quoted_or_not() {
        for command in [
            "/home/u/.lunchroom/bin/lunchroom guard pretool",
            "'/home/u v/.lunchroom/bin/lunchroom' guard pretool",
        ] {
            assert!(is_lunchroom_hook(&json!({"command": command})), "{command}");
        }
        assert!(!is_lunchroom_hook(
            &json!({"command": "/x/other guard pretool"})
        ));
    }

    // R3.1, review finding 14: the guard's program is read from the command as `install` writes
    // it, quoted or not; a path with a space is one program; only exactly `guard <mode>` is a
    // guard; a program not named `lunchroom` is no guard.
    #[test]
    fn a_guard_command_is_read_as_install_writes_it() {
        let hook = |c: &str| json!({"command": c});
        for (command, mode, program) in [
            (
                "/h/.lunchroom/bin/lunchroom guard pretool",
                "pretool",
                "/h/.lunchroom/bin/lunchroom",
            ),
            (
                "'/h/my home/.lunchroom/bin/lunchroom' guard posttool",
                "posttool",
                "/h/my home/.lunchroom/bin/lunchroom",
            ),
            (
                "'/h/my home/lunchroom'   guard prompt",
                "prompt",
                "/h/my home/lunchroom",
            ),
            ("lunchroom guard pretool", "pretool", "lunchroom"),
        ] {
            assert_eq!(
                guard_program(&hook(command), mode),
                Some(program),
                "{command}"
            );
        }
        for (command, mode) in [
            ("/h/my home/lunchroom guard pretool", "pretool"),
            ("'/h/lunchroom' guard pretool", "posttool"),
            ("'/h/lunchroom' guard pretool --x", "pretool"),
            ("'/h/lunchroom' guard", "pretool"),
            ("'/h/lunchroom'guard pretool", "pretool"),
            ("'/h/other' guard pretool", "pretool"),
            ("'/h/my home/lunchroom guard pretool", "pretool"),
            ("", "pretool"),
        ] {
            assert_eq!(guard_program(&hook(command), mode), None, "{command}");
        }
        assert_eq!(guard_program(&json!({}), "pretool"), None);
        assert_eq!(guard_program(&json!({"command": 7}), "pretool"), None);
    }

    /// A scratch folder with a program named `lunchroom` in a folder whose name has a space, and
    /// the hook groups for `hooks` (event, matcher, mode) that run it.
    struct Claude {
        root: PathBuf,
        program: String,
    }

    impl Claude {
        fn new(name: &str) -> Self {
            let root = crate::test_target()
                .join("claude-evidence-unit")
                .join(format!("{}-{name}", std::process::id()));
            let bin = root.join("my bin");
            let _ = std::fs::create_dir_all(&bin);
            let _ = std::fs::create_dir_all(root.join("project/.claude"));
            let _ = std::fs::create_dir_all(root.join("config"));
            let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
            let program = bin.join("lunchroom").display().to_string();
            Self { root, program }
        }

        /// The settings of the hooks `events` (event, mode), each in a group with `matcher`.
        fn settings(&self, events: &[(&str, &str)], matcher: Option<&str>) -> Value {
            let mut hooks = serde_json::Map::new();
            for (event, mode) in events {
                let mut group = json!({"hooks": [{
                    "type": "command", "command": format!("'{}' guard {mode}", self.program)
                }]});
                if let Some(m) = matcher {
                    group["matcher"] = json!(m);
                }
                hooks.insert((*event).to_owned(), json!([group]));
            }
            json!({"hooks": hooks})
        }

        fn write(&self, relative: &str, value: &Value) {
            let _ = std::fs::write(self.root.join(relative), value.to_string());
        }

        /// `configured_with` for a process given `args`, against this folder's settings.
        fn guarded(&self, args: &[&str]) -> bool {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            configured_with(
                &args,
                &self.root.join("config"),
                &self.root.join("managed.json"),
                std::process::id(),
                &self.root.join("project"),
            )
        }

        fn clear(&self) {
            for file in [
                "config/settings.json",
                "project/.claude/settings.json",
                "project/.claude/settings.local.json",
                "managed.json",
            ] {
                let _ = std::fs::remove_file(self.root.join(file));
            }
        }
    }

    impl Drop for Claude {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const FULL: [(&str, &str); 4] = [
        ("PreToolUse", "pretool"),
        ("PostToolUse", "posttool"),
        ("PostToolUseFailure", "posttool"),
        ("UserPromptSubmit", "prompt"),
    ];

    // R3.1, review finding 5 and 14: a session is guarded only when the complete guard set runs,
    // each hook covering every call. The guard's program is under a path with a space, quoted as
    // `install` writes it. Failure classes: one event of the set missing (each in turn); a
    // matcher that covers some tools only (a name, a list, a pattern); an `if` filter; a hook
    // that runs in the background or is not a command; the set spread over several settings
    // files (the levels merge); an event listed but with nothing in it.
    #[test]
    fn claude_is_guarded_only_by_the_complete_guard_set_covering_every_call() {
        let c = Claude::new("set");
        c.write("config/settings.json", &c.settings(&FULL, None));
        assert!(
            c.guarded(&[]),
            "complete set, program under a path with a space"
        );
        for missing in FULL {
            let rest: Vec<_> = FULL.iter().copied().filter(|e| *e != missing).collect();
            c.write("config/settings.json", &c.settings(&rest, None));
            assert!(!c.guarded(&[]), "without {missing:?}");
        }
        // The set spread over the user's and the project's settings is the merged set.
        c.write("config/settings.json", &c.settings(&FULL[..2], None));
        c.write(
            "project/.claude/settings.json",
            &c.settings(&FULL[2..], None),
        );
        assert!(c.guarded(&[]), "merged levels");
        c.clear();
        // Matchers: absent, empty and `*` cover every tool; anything else does not.
        for (matcher, covers) in [
            (None, true),
            (Some(""), true),
            (Some("*"), true),
            (Some("Bash"), false),
            (Some("Bash|Edit"), false),
            (Some("^Notebook"), false),
            (Some("mcp__.*"), false),
        ] {
            c.write("config/settings.json", &c.settings(&FULL, matcher));
            // `UserPromptSubmit` has no matcher; the three tool events do.
            assert_eq!(c.guarded(&[]), covers, "matcher {matcher:?}");
        }
        // Only the `PreToolUse` matcher narrowed: still not guarded; only `UserPromptSubmit`
        // given one: guarded, since Claude Code ignores a matcher there.
        let mut narrowed = c.settings(&FULL, None);
        narrowed["hooks"]["PostToolUseFailure"][0]["matcher"] = json!("Bash");
        c.write("config/settings.json", &narrowed);
        assert!(!c.guarded(&[]), "PostToolUseFailure for Bash only");
        let mut ignored = c.settings(&FULL, None);
        ignored["hooks"]["UserPromptSubmit"][0]["matcher"] = json!("Bash");
        c.write("config/settings.json", &ignored);
        assert!(
            c.guarded(&[]),
            "a UserPromptSubmit matcher is ignored by Claude Code"
        );
        for (field, value) in [
            ("if", json!("Bash(rm *)")),
            ("async", json!(true)),
            ("asyncRewake", json!(true)),
            ("type", json!("http")),
        ] {
            for event in [
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "UserPromptSubmit",
            ] {
                let mut v = c.settings(&FULL, None);
                v["hooks"][event][0]["hooks"][0][field] = value.clone();
                c.write("config/settings.json", &v);
                assert!(!c.guarded(&[]), "{event} with {field}");
            }
        }
        let mut empty = c.settings(&FULL, None);
        empty["hooks"]["PostToolUse"] = json!([{"hooks": []}]);
        c.write("config/settings.json", &empty);
        assert!(!c.guarded(&[]), "an empty group");
        // The same set with an unquoted path with a space is not one program: not guarded.
        let mut unquoted = c.settings(&FULL, None);
        for event in [
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "UserPromptSubmit",
        ] {
            let mode = if event == "UserPromptSubmit" {
                "prompt"
            } else if event == "PreToolUse" {
                "pretool"
            } else {
                "posttool"
            };
            unquoted["hooks"][event][0]["hooks"][0]["command"] =
                json!(format!("{} guard {mode}", c.program));
        }
        c.write("config/settings.json", &unquoted);
        assert!(!c.guarded(&[]), "unquoted path with a space");
    }

    // R3.1, review finding 5: the guarded state follows the configuration Claude Code actually
    // loads. Failure classes: `--bare` (no hooks); `--setting-sources` leaving out the level that
    // holds the set; `disableAllHooks` (precedence, and managed hooks surviving it); a managed
    // `disableAllHooks` or `allowManagedHooksOnly`; `--settings` as a file or as text, spaced or
    // joined by `=`.
    #[test]
    fn claude_is_guarded_only_in_the_configuration_it_loads() {
        let c = Claude::new("loaded");
        let full = c.settings(&FULL, None);
        c.write("config/settings.json", &full);
        assert!(c.guarded(&["claude"]));
        assert!(!c.guarded(&["claude", "--bare"]), "--bare runs no hooks");
        // The level that holds the set must be one the process loads.
        assert!(c.guarded(&["claude", "--setting-sources", "user,project"]));
        assert!(c.guarded(&["claude", "--setting-sources=local, user"]));
        assert!(!c.guarded(&["claude", "--setting-sources", "project,local"]));
        assert!(!c.guarded(&["claude", "--setting-sources="]));
        c.clear();
        c.write("project/.claude/settings.local.json", &full);
        assert!(!c.guarded(&["claude", "--setting-sources", "user,project"]));
        assert!(c.guarded(&["claude", "--setting-sources", "local"]));
        c.clear();
        // `--settings`, as text and as a file (absolute, or relative to the session's folder).
        let text = full.to_string();
        assert!(c.guarded(&["claude", "--settings", &text]));
        let joined = format!("--settings={text}");
        assert!(c.guarded(&[joined.as_str()]));
        c.write("flag.json", &full);
        let file = c.root.join("flag.json").display().to_string();
        assert!(c.guarded(&["claude", "--settings", &file]));
        c.write("project/rel.json", &full);
        assert!(c.guarded(&["claude", "--settings", "rel.json"]));
        assert!(!c.guarded(&["claude", "--settings", "{not json"]));
        c.clear();
        // disableAllHooks: the value left after precedence decides.
        let mut off = full.clone();
        off["disableAllHooks"] = json!(true);
        c.write("config/settings.json", &off);
        assert!(!c.guarded(&[]), "user disableAllHooks");
        c.write(
            "project/.claude/settings.json",
            &json!({"disableAllHooks": false}),
        );
        assert!(c.guarded(&[]), "the project's false is the value left");
        c.write(
            "project/.claude/settings.local.json",
            &json!({"disableAllHooks": true}),
        );
        assert!(!c.guarded(&[]), "the local true is the value left");
        let flag_false = json!({"disableAllHooks": false}).to_string();
        assert!(
            c.guarded(&["--settings", &flag_false]),
            "--settings over local"
        );
        let flag_true = json!({"disableAllHooks": true}).to_string();
        assert!(!c.guarded(&["--settings", &flag_true]));
        c.clear();
        // Managed hooks survive a lower level's disableAllHooks, and only managed settings stop
        // them; `allowManagedHooksOnly` drops every other level's hooks.
        c.write("config/settings.json", &json!({"disableAllHooks": true}));
        c.write("managed.json", &full);
        assert!(
            c.guarded(&[]),
            "managed set survives the user's disableAllHooks"
        );
        let mut managed_off = full.clone();
        managed_off["disableAllHooks"] = json!(true);
        c.write("managed.json", &managed_off);
        assert!(!c.guarded(&[]), "managed disableAllHooks");
        c.write("config/settings.json", &full);
        assert!(
            !c.guarded(&[]),
            "managed disableAllHooks over a full user set"
        );
        let mut only = json!({"allowManagedHooksOnly": true});
        c.write("managed.json", &only);
        assert!(
            !c.guarded(&[]),
            "the user's set is dropped under allowManagedHooksOnly"
        );
        only["hooks"] = full["hooks"].clone();
        c.write("managed.json", &only);
        assert!(
            c.guarded(&[]),
            "the managed set runs under allowManagedHooksOnly"
        );
        // Part of the set managed, the rest the user's: merged.
        c.clear();
        c.write("managed.json", &c.settings(&FULL[..1], None));
        c.write("config/settings.json", &c.settings(&FULL[1..], None));
        assert!(c.guarded(&[]), "managed and user merged");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, each way the evidence can fail checked on it
    fn codex_is_guarded_only_when_every_lunchroom_hook_is_trusted() {
        let root = crate::test_target()
            .join("codex-trust-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let program = bin.join("lunchroom").display().to_string();
        let hook = |mode: &str| json!([{"hooks": [{"type": "command", "command": format!("{program} {mode}")}]}]);
        let file = codex_home.join("hooks.json");
        let _ = std::fs::write(
            &file,
            json!({"hooks": {
                "PreToolUse": hook("guard pretool"),
                "PostToolUse": hook("guard posttool"),
                "UserPromptSubmit": hook("guard prompt"),
                "SessionStart": hook("guard start"),
                "Stop": hook("listen --once"),
                "PermissionRequest": [{"hooks": [{"type": "command", "command": "/x/other"}]}],
            }})
            .to_string(),
        );
        let events = [
            "pre_tool_use",
            "post_tool_use",
            "user_prompt_submit",
            "session_start",
            "stop",
        ];
        let entry = |event: &str, extra: &str| {
            format!(
                "[hooks.state.\"{}:{event}:0:0\"]\ntrusted_hash = \"sha256:{event}\"\n{extra}\n",
                file.display()
            )
        };
        let fingerprint = |config: &str| {
            let config = format!("[features]\nunified_exec_tty = false\n{config}");
            let _ = std::fs::write(codex_home.join("config.toml"), config);
            codex_fingerprint_in(&codex_home, std::process::id(), &root)
        };
        let all: String = events.iter().map(|e| entry(e, "enabled = true")).collect();
        let full = fingerprint(&all);
        // H1.1: with Codex's terminals on, or a project turning them on, no session is guarded.
        for (home, project) in [
            ("[features]\nunified_exec_tty = true\n", ""),
            (
                "[features]\nunified_exec_tty = false\n[profiles.p.features]\nunified_exec_tty = true\n",
                "",
            ),
            ("", ""),
            (
                "notes = \"\"\"\n[features]\nunified_exec_tty = false\n\"\"\"\n",
                "",
            ),
            (
                "[features]\nunified_exec_tty = false\n",
                "[features]\nunified_exec_tty = true\n",
            ),
            (
                "[features]\nunified_exec_tty = false\n",
                "features.unified_exec_tty = true\n",
            ),
            (
                "[features]\nunified_exec_tty = false\n",
                "features = { unified_exec_tty = true }\n",
            ),
            ("[features]\nunified_exec_tty = false\n", "[not toml\n"),
        ] {
            let _ = std::fs::write(codex_home.join("config.toml"), format!("{home}{all}"));
            let _ = std::fs::create_dir_all(root.join(".codex"));
            let _ = std::fs::write(root.join(".codex/config.toml"), project);
            let _ = std::fs::create_dir_all(root.join("a/b"));
            // A project file counts from the folder it is in and from every folder below it.
            for cwd in [root.clone(), root.join("a/b")] {
                assert_eq!(
                    codex_fingerprint_in(&codex_home, std::process::id(), &cwd),
                    None,
                    "{home:?} {project:?} {}",
                    cwd.display()
                );
            }
        }
        let _ = std::fs::remove_file(root.join(".codex/config.toml"));
        assert!(
            full.as_ref()
                .is_some_and(|f| events.iter().all(|e| f.contains(&format!("sha256:{e}"))))
        );
        for declined in events {
            let without: String = events
                .iter()
                .filter(|e| **e != declined)
                .map(|e| entry(e, "enabled = true"))
                .collect();
            assert_eq!(fingerprint(&without), None, "{declined} has no entry");
            let disabled: String = events
                .iter()
                .map(|e| {
                    entry(
                        e,
                        if *e == declined {
                            "enabled = false"
                        } else {
                            ""
                        },
                    )
                })
                .collect();
            assert_eq!(fingerprint(&disabled), None, "{declined} is disabled");
        }
        // A changed trust hash changes the fingerprint.
        let rehashed = all.replace("sha256:stop", "sha256:other");
        assert!(fingerprint(&rehashed).is_some_and(|f| Some(&f) != full.as_ref()));
        let _ = std::fs::remove_dir_all(&root);
    }

    // R3.1 (Codex), review finding 6: Codex counts as guarded only with the whole guard set
    // `install` writes for it (pretool, posttool, prompt, start), each in a group whose matcher
    // covers every tool, as Claude Code does (`CLAUDE_GUARD_HOOKS`): a trusted `PreToolUse` alone,
    // a missing event, a narrowed matcher, a wrong mode or an asynchronous hook is unguarded.
    #[test]
    fn codex_is_guarded_only_with_the_whole_guard_hook_set() {
        let root = crate::test_target()
            .join("codex-set-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let copied = std::fs::copy("/bin/true", bin.join("lunchroom"));
        assert!(copied.is_ok(), "{copied:?}");
        let program = bin.join("lunchroom").display().to_string();
        let file = codex_home.join("hooks.json");
        let events = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let tty = "[features]\nunified_exec_tty = false\n".to_owned();
        let config = events.iter().fold(tty, |mut s, (_, state, _)| {
            use std::fmt::Write as _;
            let _ = write!(
                s,
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"\n",
                file.display()
            );
            s
        });
        let _ = std::fs::write(codex_home.join("config.toml"), config);
        // The hooks file with `change` applied to the group of `event` (None drops the event).
        let guarded = |event: &str, change: &dyn Fn(&mut Value)| {
            let mut hooks = serde_json::Map::new();
            for (name, _, args) in events {
                let mut group = json!({"hooks": [{
                    "type": "command", "command": format!("{program} {args}"),
                }]});
                if name == event {
                    change(&mut group);
                }
                if !group.is_null() {
                    hooks.insert(name.to_owned(), json!([group]));
                }
            }
            let _ = std::fs::write(&file, json!({"hooks": hooks}).to_string());
            codex_fingerprint_in(&codex_home, std::process::id(), &root).is_some()
        };
        assert!(guarded("", &|_| {}), "the whole set, trusted");
        for (event, _, _) in events {
            assert!(!guarded(event, &|g| *g = Value::Null), "{event} missing");
            for matcher in ["Bash", "Bash|Edit", "^.*$", "mcp__.*"] {
                let narrowed = guarded(event, &|g| g["matcher"] = json!(matcher));
                // `UserPromptSubmit` has no matcher in Codex either.
                assert_eq!(narrowed, event == "UserPromptSubmit", "{event} {matcher}");
            }
            for matcher in ["", "*"] {
                assert!(
                    guarded(event, &|g| g["matcher"] = json!(matcher)),
                    "{event} {matcher:?}"
                );
            }
            assert!(
                !guarded(event, &|g| g["hooks"][0]["async"] = json!(true)),
                "{event} async"
            );
            assert!(
                !guarded(event, &|g| g["hooks"][0]["if"] = json!("Bash(ls)")),
                "{event} if"
            );
            assert!(
                !guarded(event, &|g| g["hooks"][0]["command"] =
                    json!(format!("{program} listen --once"))),
                "{event} runs no guard"
            );
        }
        // Every event runs the guard of another event's mode: no event is satisfied by a sibling.
        assert!(!guarded("UserPromptSubmit", &|g| {
            g["hooks"][0]["command"] = json!(format!("{program} guard pretool"));
        }));
        let _ = std::fs::remove_dir_all(&root);
    }

    // R3.1 (OpenClaw, Hermes): the program a plugin file runs is read from its `PROGRAM` line.
    #[test]
    fn plugin_program_reads_the_installed_path() {
        assert_eq!(
            plugin_program("// x\nconst PROGRAM = \"/h/.lunchroom/bin/lunchroom\";\n"),
            Some("/h/.lunchroom/bin/lunchroom")
        );
        assert_eq!(
            plugin_program("import os\nPROGRAM = \"/h/.lunchroom/bin/lunchroom\"\n"),
            Some("/h/.lunchroom/bin/lunchroom")
        );
        assert_eq!(plugin_program("PROGRAMS = 1\n"), None);
        let recorded = "/nonexistent/plugin.py\n9976f4857b72439d79016354d7b6e1a3ee952e2f";
        assert!(!plugin_runs_guard(Some(recorded)));
        assert!(!plugin_runs_guard(None));
        assert!(!plugin_runs_guard(Some("/nonexistent/plugin.py")));
        assert_eq!(
            plugin_release(Some(recorded)),
            "9976f4857b72439d79016354d7b6e1a3ee952e2f"
        );
        assert_eq!(plugin_release(Some("/x/plugin.py")), "");
        assert_eq!(plugin_release(None), "");
    }

    /// A file `name` under `dir` holding `text`, executable when `run`.
    fn script(dir: &Path, name: &str, text: &str, run: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::create_dir_all(dir);
        let file = dir.join(name);
        let _ = std::fs::write(&file, text);
        let mode = if run { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode));
        file
    }

    // G3-1: after apt, dnf or apk upgrades the program, the system shows the running session's
    // program as `/usr/bin/claude (deleted)`. The suffix is taken off, the program is marked
    // replaced, and the reason says so and what the user does (full text).
    #[test]
    fn a_program_replaced_under_the_session_is_named_in_the_reason() {
        let found = programs_of(
            Some(PathBuf::from("/usr/bin/claude (deleted)")),
            b"claude\0",
            None,
            &|_| None,
        );
        assert_eq!(
            found,
            Programs {
                paths: vec![PathBuf::from("/usr/bin/claude")],
                replaced: true
            }
        );
        let kept = programs_of(
            Some(PathBuf::from("/usr/bin/claude")),
            b"claude\0",
            None,
            &|_| None,
        );
        assert!(!kept.replaced);
        assert_eq!(
            Why::ProgramReplaced {
                harness: Harness::ClaudeCode,
                program: "/usr/bin/claude".to_owned()
            }
            .text(),
            "Claude Code's program (/usr/bin/claude) was replaced while this session was \
             running, most likely by an upgrade, so Lunchroom cannot tell which release the \
             session still runs: start the Claude Code session anew, so it runs the program now \
             installed."
        );
    }

    // G3-1, against the system's own `/proc`: a copy of a program, started and then deleted, reads
    // as replaced, with its original path.
    #[test]
    fn a_deleted_program_of_a_running_process_reads_as_replaced() {
        let r = Records::new("deleted");
        let Some(real) = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        else {
            unreachable!("this test needs a sleep program");
        };
        let copy = r.0.join("sleeper");
        assert!(std::fs::copy(real, &copy).is_ok());
        let child = std::process::Command::new(&copy).arg("5").spawn();
        assert!(child.is_ok(), "{child:?}");
        let Ok(mut child) = child else { return };
        let _ = std::fs::remove_file(&copy);
        let found = process_programs(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.replaced, "{found:?}");
        assert_eq!(found.paths.first(), Some(&copy), "{found:?}");
    }

    // G3-2: a script a runtime runs is judged by where its links lead (nvm and Volta link
    // `bin/claude` into the package), and a relative first argument counts only when it is a file.
    #[test]
    fn a_linked_script_is_read_from_the_package_it_links_to() {
        let seen = |p: &Path| {
            (p == Path::new("/h/.nvm/bin/claude"))
                .then(|| PathBuf::from("/h/.nvm/lib/node_modules/@anthropic-ai/claude-code/cli.js"))
        };
        let found = programs_of(
            Some(PathBuf::from("/h/.nvm/bin/node")),
            b"node\0/h/.nvm/bin/claude\0",
            None,
            &seen,
        );
        assert_eq!(
            found.paths,
            vec![
                PathBuf::from("/h/.nvm/bin/node"),
                PathBuf::from("/h/.nvm/lib/node_modules/@anthropic-ai/claude-code/cli.js")
            ]
        );
        // A prompt given as the first argument is no script.
        let prompt = programs_of(
            Some(PathBuf::from("/usr/bin/claude")),
            b"claude\0fix it\0",
            Some(Path::new("/p")),
            &|_| None,
        );
        assert_eq!(prompt.paths, vec![PathBuf::from("/usr/bin/claude")]);
        let option = programs_of(
            Some(PathBuf::from("/usr/bin/claude")),
            b"claude\0--version\0",
            Some(Path::new("/p")),
            &|_| Some(PathBuf::from("/never")),
        );
        assert_eq!(option.paths, vec![PathBuf::from("/usr/bin/claude")]);
        // On the real file system: `bin/claude` is a link into the package, whose `package.json`
        // carries the release.
        let r = Records::new("nvm");
        let package = r.0.join("lib/node_modules/@anthropic-ai/claude-code");
        script(&package, "cli.js", "", false);
        script(
            &package,
            "package.json",
            r#"{"name":"@anthropic-ai/claude-code","version":"2.1.309"}"#,
            false,
        );
        let _ = std::fs::create_dir_all(r.0.join("bin"));
        assert!(std::os::unix::fs::symlink(package.join("cli.js"), r.0.join("bin/claude")).is_ok());
        let real = std::fs::canonicalize(r.0.join("bin/claude")).ok();
        assert_eq!(
            real.as_deref()
                .and_then(|p| version_from_path(Harness::ClaudeCode, p)),
            Some("2.1.309".to_owned())
        );
    }

    // G3-2: Claude Code's npm platform packages are read the way the main package is (names from
    // the npm registry's metadata for @anthropic-ai/claude-code 2.1.316).
    #[test]
    fn claude_code_platform_packages_give_their_release() {
        let r = Records::new("platform");
        for (k, name) in CLAUDE_PACKAGES.iter().enumerate() {
            let package = r.0.join(format!("node_modules/{name}"));
            let program = script(&package, "claude", "", true);
            script(
                &package,
                "package.json",
                &format!(r#"{{"name":"{name}","version":"2.1.30{k}"}}"#),
                false,
            );
            assert_eq!(
                version_from_path(Harness::ClaudeCode, &program),
                Some(format!("2.1.30{k}")),
                "{name}"
            );
        }
        // Another package of that scope is no Claude Code.
        let other = r.0.join("node_modules/@anthropic-ai/sdk");
        let program = script(&other, "claude", "", true);
        script(
            &other,
            "package.json",
            r#"{"name":"@anthropic-ai/sdk","version":"2.1.309"}"#,
            false,
        );
        assert_eq!(version_from_path(Harness::ClaudeCode, &program), None);
    }

    // G3-3: Homebrew's cask keeps the program at `<Caskroom>/claude-code/<release>/claude`.
    #[test]
    fn homebrews_claude_code_cask_gives_its_release() {
        let read = |p: &str| version_from_path(Harness::ClaudeCode, Path::new(p));
        assert_eq!(
            read("/home/linuxbrew/.linuxbrew/Caskroom/claude-code/2.1.309/claude"),
            Some("2.1.309".to_owned())
        );
        assert_eq!(
            read("/opt/homebrew/Caskroom/claude-code@latest/2.1.310/claude"),
            Some("2.1.310".to_owned())
        );
        for not in [
            "/x/claude-code/2.1.309/claude",
            "/opt/homebrew/Caskroom/other/2.1.309/claude",
            "/opt/homebrew/Caskroom/claude-code/2.1.309/other",
            "/opt/homebrew/Caskroom/claude-code/latest/claude",
        ] {
            assert_eq!(read(not), None, "{not}");
        }
    }

    // G3-3: a pnpm-style wrapper (`exec node "$basedir/…/openclaw.mjs"`) is followed to the
    // program; a script that names none gets a reason that names the script; Volta's shim gets its
    // own reason (full texts).
    #[test]
    fn doctor_follows_a_wrapper_and_names_one_it_cannot_follow() {
        let r = Records::new("wrapper");
        let package =
            r.0.join("global/5/.pnpm/openclaw@2026.10.7/node_modules/openclaw");
        script(&package, "openclaw.mjs", "", false);
        script(
            &package,
            "package.json",
            r#"{"name":"openclaw","version":"2026.10.7"}"#,
            false,
        );
        let shim = script(
            &r.0.join("pnpm"),
            "openclaw",
            "#!/bin/sh\nbasedir=$(dirname \"$0\")\nif [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/../global/5/.pnpm/openclaw@2026.10.7/node_modules/openclaw/openclaw.mjs\" \"$@\"\nelse\n  exec node  \"$basedir/../global/5/.pnpm/openclaw@2026.10.7/node_modules/openclaw/openclaw.mjs\" \"$@\"\nfi\n",
            true,
        );
        let followed = std::fs::canonicalize(package.join("openclaw.mjs")).unwrap_or_default();
        assert_eq!(
            path_release(Harness::OpenClaw, Some(&shim), Path::new("/")),
            Ok(format!("2026.10.7, read from {}", followed.display()))
        );
        // A wrapper of Claude Code that execs a native install.
        let native = script(
            &r.0.join("home/.local/share/claude/versions"),
            "2.1.309",
            "",
            true,
        );
        let wrapper = script(
            &r.0.join("bin"),
            "claude",
            &format!(
                "#!/bin/bash\nexport A=1\nexec {} \"$@\" 2>/dev/null\n",
                native.display()
            ),
            true,
        );
        let followed = std::fs::canonicalize(&native).unwrap_or_default();
        assert_eq!(
            path_release(Harness::ClaudeCode, Some(&wrapper), Path::new("/")),
            Ok(format!("2.1.309, read from {}", followed.display()))
        );
        // A script that runs no program by path.
        let lost = script(&r.0.join("lost"), "claude", "#!/bin/sh\necho hello\n", true);
        assert_eq!(
            path_release(Harness::ClaudeCode, Some(&lost), Path::new("/")),
            Err(format!(
                "the claude on your PATH, {}, is a script, not Claude Code's program, and \
                 Lunchroom could not follow it to the program it runs (it follows a line that \
                 starts with exec and names the program's path): put Claude Code's own claude \
                 program on your PATH, or end the script with exec and the path of that program.",
                lost.display()
            ))
        );
        // Volta's shim.
        let volta = script(&r.0.join(".volta/bin"), "claude", "", true);
        assert_eq!(
            path_release(Harness::ClaudeCode, Some(&volta), Path::new("/")),
            Err(format!(
                "the claude on your PATH, {}, is Volta's shim, which runs whichever Claude Code \
                 install Volta picks for the folder, so no release can be read from it: run volta \
                 which claude to see the install it runs, and put that install's folder first on \
                 your PATH. A Claude Code session that is running is read from the program it \
                 runs, whatever your PATH holds.",
                volta.display()
            ))
        );
    }

    // G3-4: a stand-in `rpm` records its arguments and environment. rpm is run with `-qf`, rpm's
    // default database folder when it exists, the query format, `--` and the path; with no `HOME`
    // (no user macro file) and `LC_ALL=C`; its answer is read from the output.
    #[test]
    fn rpm_is_asked_with_pinned_arguments_and_an_empty_environment() {
        let r = Records::new("rpm-args");
        let dir = r.0.join("stand-in");
        let asked = dir.join("args");
        let seen = dir.join("env");
        let rpm = script(
            &dir,
            "rpm",
            &format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf '%s,%s' \"${{HOME-unset}}\" \"$LC_ALL\" > {}\nprintf 'claude-code\\t2.1.309\\n'\n",
                asked.display(),
                seen.display()
            ),
            true,
        );
        let rpm = rpm.to_string_lossy().into_owned();
        let limit = std::time::Duration::from_secs(5);
        assert_eq!(
            ask_rpm(&[rpm.as_str()], "/usr/bin/claude", Some(RPM_DBPATH), limit),
            RpmAnswer::Claude("2.1.309".to_owned())
        );
        let text = std::fs::read_to_string(&asked).unwrap_or_default();
        let args: Vec<&str> = text.split('\0').filter(|a| !a.is_empty()).collect();
        assert_eq!(
            args,
            [
                "-qf",
                "--dbpath",
                "/var/lib/rpm",
                "--queryformat",
                "%{NAME}\t%{VERSION}\n",
                "--",
                "/usr/bin/claude"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&seen).unwrap_or_default(),
            "unset,C"
        );
        // No database folder: no --dbpath.
        assert_eq!(
            rpm_args("/usr/bin/claude", None),
            [
                "-qf",
                "--queryformat",
                "%{NAME}\t%{VERSION}\n",
                "--",
                "/usr/bin/claude"
            ]
        );
    }

    // G3-4: what rpm says is read as three answers: claude-code at a version, another package or
    // no package ("is not owned by any package"), or none (rpm missing, failed, past its limit).
    #[test]
    fn rpm_answers_are_told_apart_and_a_slow_rpm_is_killed() {
        let r = Records::new("rpm-answers");
        let dir = r.0.join("stand-in");
        let limit = std::time::Duration::from_secs(5);
        let ask = |rpm: &Path, limit| {
            ask_rpm(
                &[rpm.to_str().unwrap_or_default()],
                "/usr/bin/claude",
                None,
                limit,
            )
        };
        let other = script(&dir, "other", "#!/bin/sh\nprintf 'other\\t1.0\\n'\n", true);
        assert_eq!(ask(&other, limit), RpmAnswer::NotClaude);
        let unowned = script(
            &dir,
            "unowned",
            "#!/bin/sh\necho 'file /usr/bin/claude is not owned by any package' >&2\nexit 1\n",
            true,
        );
        assert_eq!(ask(&unowned, limit), RpmAnswer::NotClaude);
        let broken = script(
            &dir,
            "broken",
            "#!/bin/sh\necho 'error: cannot open Packages database' >&2\nexit 1\n",
            true,
        );
        assert_eq!(ask(&broken, limit), RpmAnswer::Unanswered);
        assert_eq!(
            ask_rpm(&["/nonexistent/rpm"], "/usr/bin/claude", None, limit),
            RpmAnswer::Unanswered
        );
        // A slow rpm is stopped at the limit, and its process is gone.
        let pidfile = dir.join("pid");
        let slow = script(
            &dir,
            "slow",
            &format!(
                "#!/bin/sh\necho $$ > {}\nexec /bin/sleep 30\n",
                pidfile.display()
            ),
            true,
        );
        let began = std::time::Instant::now();
        assert_eq!(
            ask(&slow, std::time::Duration::from_millis(1500)),
            RpmAnswer::Unanswered
        );
        assert!(began.elapsed() < std::time::Duration::from_secs(10));
        let pid = std::fs::read_to_string(&pidfile).unwrap_or_default();
        assert_ne!(pid.trim(), "");
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "rpm {pid} still runs"
        );
    }

    // G3-4: rpm's answers are kept by the file's identity (path, inode, change time): an answer is
    // found again for the same file and not for a changed one; a new answer replaces the earlier
    // one for the path; the newest 32 are kept; the file is written for the user alone.
    #[test]
    fn rpm_answers_are_cached_by_the_files_identity() {
        use std::os::unix::fs::PermissionsExt as _;
        let r = Records::new("rpm-cache");
        let program = script(&r.0, "claude", "one", true);
        let path = program.to_string_lossy().into_owned();
        let identity = file_identity(&path);
        assert!(identity.is_some());
        let identity = identity.unwrap_or_default();
        let text = cache_store("", &identity, Some("2.1.309"));
        assert_eq!(
            cache_lookup(&text, &identity),
            Some(Some("2.1.309".to_owned()))
        );
        // The file changes: another inode or change time, so no answer.
        let _ = std::fs::remove_file(&program);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let changed = script(&r.0, "claude", "two", true);
        let now = file_identity(&changed.to_string_lossy());
        assert_ne!(now.as_deref(), Some(identity.as_str()));
        assert_eq!(
            cache_lookup(&text, now.as_deref().unwrap_or_default()),
            None
        );
        // A package that is not claude-code is kept as `-`, and replaces the earlier answer.
        let newer = cache_store(&text, now.as_deref().unwrap_or_default(), None);
        assert_eq!(
            cache_lookup(&newer, now.as_deref().unwrap_or_default()),
            Some(None)
        );
        assert_eq!(newer.lines().count(), 1);
        // Only the newest are kept.
        let mut many = String::new();
        for k in 0..40 {
            many = cache_store(&many, &format!("/p{k}\t1\t2.3"), Some("1.0"));
        }
        assert_eq!(many.lines().count(), RPM_CACHE_KEEP);
        assert_eq!(cache_lookup(&many, "/p0\t1\t2.3"), None);
        assert_eq!(
            cache_lookup(&many, "/p39\t1\t2.3"),
            Some(Some("1.0".to_owned()))
        );
        // The file is the user's alone.
        let file = r.0.join("rpm-owner.cache");
        write_cache(&file, &newer);
        assert_eq!(
            std::fs::read_to_string(&file).ok().as_deref(),
            Some(newer.as_str())
        );
        assert_eq!(
            std::fs::metadata(&file)
                .map(|m| m.permissions().mode() & 0o777)
                .ok(),
            Some(0o600)
        );
    }

    // G3-4: an rpm database on the system with no rpm program says so, in the doctor line and the
    // reason (full text); with the program there, or no database, the generic reason stays.
    #[test]
    fn an_rpm_database_without_the_rpm_program_is_named() {
        let r = Records::new("rpm-missing");
        assert!(!rpm_database_without_rpm(&r.0));
        let _ = std::fs::create_dir_all(r.0.join("var/lib/rpm"));
        assert!(rpm_database_without_rpm(&r.0));
        let program = r.0.join("usr/bin/claude");
        assert_eq!(
            path_release(Harness::ClaudeCode, Some(&program), &r.0),
            Err(format!(
                "Claude Code's release cannot be read from the install of its program ({}): rpm \
                 is not installed, so Lunchroom cannot read the rpm database that may hold it. To \
                 fix it, install the rpm package, then start the session anew.",
                program.display()
            ))
        );
        script(&r.0.join("usr/bin"), "rpm", "", true);
        assert!(!rpm_database_without_rpm(&r.0));
        assert!(
            path_release(Harness::ClaudeCode, Some(&program), &r.0).is_err_and(|e| e
                .contains("cannot be read from the install of its program")
                && !e.contains("rpm is not installed"))
        );
    }

    // G3-6: the record a plugin's guard hook leaves carries the time; the release is read as before
    // from a record with or without it.
    #[test]
    fn the_plugin_record_carries_its_time() {
        let at = Path::new("/h/.lunchroom/plugins/p.py");
        let made = plugin_fingerprint_at(at, "2026.10.7", 1_793_038_003);
        assert_eq!(made, "/h/.lunchroom/plugins/p.py\n2026.10.7\n1793038003");
        assert_eq!(plugin_release(Some(&made)), "2026.10.7");
        assert_eq!(plugin_reported_at(Some(&made)), Some(1_793_038_003));
        let old = "/h/.lunchroom/plugins/p.py\n2026.10.7";
        assert_eq!(plugin_release(Some(old)), "2026.10.7");
        assert_eq!(plugin_reported_at(Some(old)), None);
        assert_eq!(plugin_reported_at(None), None);
    }

    // G3-6: a plugin's report is told with when it was reported; a report with no time says so.
    #[test]
    fn a_reported_release_is_told_with_its_time() {
        let t = 1_793_038_003;
        assert_eq!(
            reported_when(Some(t), t + 3 * 3600 + 5 * 60),
            "on 2026-10-26 at 18:06 UTC, 3 h 5 min ago"
        );
        assert_eq!(reported_when(Some(t), t - 5), "on 2026-10-26 at 18:06 UTC");
        assert_eq!(reported_when(None, t), "at a time Lunchroom did not record");
    }

    // G3-9: the reasons in plain English (full text), and the agent's form names no path.
    #[test]
    fn the_reasons_are_plain_and_the_agents_form_has_no_path() {
        let older = Why::ReleaseOlder {
            harness: Harness::Codex,
            have: "0.162.0".to_owned(),
            passed: "0.163.0".to_owned(),
        };
        assert_eq!(
            older.text(),
            "Codex 0.162.0 is older than 0.163.0, the oldest release Lunchroom was tested with: \
             update Codex to 0.163.0 or later, then start the session anew."
        );
        assert_eq!(
            Why::NoAdapter(Harness::Hermes).text(),
            "This Lunchroom has not been tested with any release of Hermes, so none is guarded: \
             install a Lunchroom release that has been tested with Hermes."
        );
        // R3.4 (2026-11-03, hermes4.txt): a Hermes checkout that a user other than root and the
        // session's own user owns is refused by git; the reason says what the user does then.
        let hermes = Why::ReleaseUnread {
            harness: Harness::Hermes,
            program: "/usr/local/bin/python3.14".to_owned(),
        }
        .text();
        assert!(
            hermes.contains(
                "if Hermes runs from a git checkout that belongs to a user other than root and \
                 the user who runs Hermes, git refuses to read it: give the checkout to the user \
                 who runs Hermes or to root, then start Hermes anew"
            ),
            "{hermes}"
        );
        for why in [
            Why::Configuration("Codex's /home/u/.codex/config.toml sets x".to_owned()),
            Why::NoGuardRan(Harness::Codex),
            Why::ReleaseUnread {
                harness: Harness::ClaudeCode,
                program: "/home/u/bin/claude".to_owned(),
            },
            older,
            Why::NoAdapter(Harness::Hermes),
            Why::ProgramReplaced {
                harness: Harness::ClaudeCode,
                program: "/usr/bin/claude".to_owned(),
            },
            Why::RpmNotInstalled {
                program: "/usr/bin/claude".to_owned(),
            },
        ] {
            let agent = why.text_for_agent();
            assert!(!agent.contains('/'), "{agent}");
            assert!(!agent.contains("adapter"), "{agent}");
            assert!(!why.text().contains("adapter"), "{}", why.text());
        }
        assert_eq!(
            Why::Configuration("x /home/u/y".to_owned()).text_for_agent(),
            "Its harness configuration does not run Lunchroom's whole guard: the user can see \
             which setting in a terminal of their own, with `lunchroom doctor`."
        );
    }
}
