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
use lunchroom_core::mail::{Evidence, Found, Judged, Mail};

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
pub(crate) const fn install_word(harness: Harness) -> &'static str {
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

/// The session's record and its harness process, while that very process runs: STOPPED
/// otherwise (R3.3). It reads only `/proc/<pid>/stat`, which every user may read, so the console,
/// which runs as the user `lunchroom`, can tell this much itself.
fn running_record<'m>(
    mail: &'m Mail,
    who: &Identity,
) -> Option<(&'m lunchroom_core::mail::SessionRecord, u32)> {
    let record = mail.session(who)?;
    let pid = record
        .process
        .split(':')
        .next()
        .and_then(|p| p.parse::<u32>().ok())?;
    (process::key(pid).as_deref() == Some(record.process.as_str())).then_some((record, pid))
}

/// Whether `who` is not running: no session record, or its harness process has ended (R3.3).
#[must_use]
pub fn not_running(mail: &Mail, who: &Identity) -> bool {
    running_record(mail, who).is_none()
}

/// What `judged` found of `who` at `now`, as the store keeps it for the console.
#[must_use]
pub fn kept(mail: &Mail, who: &Identity, judged: &Assessment, now: u64) -> Judged {
    let (found, why) = match judged {
        Assessment::NotRunning => (Found::NotRunning, String::new()),
        Assessment::Unguarded(why) => (Found::Unguarded, why.text()),
        Assessment::Guarded => (Found::Guarded, String::new()),
    };
    Judged {
        found,
        why,
        process: mail.session(who).map(|r| r.process.clone()),
        at: now,
    }
}

/// `assess` against the list `passed` of passing adapters and their releases. Each harness's
/// checks run in R3.4's order, configuration first, so the first one that fails is the reason;
/// a session passing every check is guarded.
fn assess_with(passed: &[(Harness, &str)], mail: &Mail, who: &Identity, home: &Path) -> Assessment {
    // H1.7: the step named if the guard's time runs out here.
    let _step = lunchroom_core::steps::begin(format!("working out whether {who} is guarded"));
    let Some((record, pid)) = running_record(mail, who) else {
        return Assessment::NotRunning;
    };
    let why = match who.harness {
        // Item 2 for Claude Code: the session's record names this very process only because a
        // Lunchroom hook ran in it, and a record of another process is STOPPED above; so a
        // running Claude Code session never lacks item 2 here.
        Harness::ClaudeCode => {
            match configured(pid, &record.cwd, home) {
                ClaudeGuard::Guarded => release_fault(passed, who.harness, pid),
                // H1.5: the settings hold an earlier install's tool-call hook, which does not
                // block a call when the guard fails.
                ClaudeGuard::OlderHook => Some(Why::Configuration(CLAUDE_OLD_HOOK.to_owned())),
                ClaudeGuard::NotConfigured => {
                    Some(Why::Configuration(CLAUDE_NOT_CONFIGURED.to_owned()))
                }
            }
        }
        Harness::Codex => {
            let current = codex_guard_in(&codex_home_of(pid, home), pid, &record.cwd);
            let opened_before =
                mail.opened_before_codex_setting(who, process::boot_id().as_deref());
            // H1.4: the settings the session's first guard hook recorded for this process; either
            // on keeps the session UNGUARDED until it is started anew.
            let recorded = mail
                .guard_fingerprint(who)
                .and_then(CodexSettings::parse)
                .filter(|s| s.on() && s.process == record.process);
            match current {
                // The setting first, as `lunchroom status` always named it.
                Err(
                    fault @ (CodexFault::TtyNotOff
                    | CodexFault::TtyTurnedOn
                    | CodexFault::WebNotOff
                    | CodexFault::WebTurnedOn
                    | CodexFault::CommandTty(_)
                    | CodexFault::CommandWeb(_)
                    | CodexFault::SharedServer(_)),
                ) => Some(Why::Configuration(fault.text())),
                _ if recorded.is_some() => recorded.map(|s| {
                    Why::Configuration(
                        CodexFault::Recorded {
                            tty: s.tty,
                            web: s.web,
                        }
                        .text(),
                    )
                }),
                _ if opened_before => Some(Why::Configuration(CODEX_OPENED_BEFORE.to_owned())),
                Err(fault) => Some(Why::Configuration(fault.text())),
                // R3.1 (Codex): the trusted guard now in place is the one a guard hook recorded
                // when it ran in this session; else none has run with it.
                Ok(current)
                    if mail.guard_fingerprint(who).map(recorded_hooks)
                        != Some(current.as_str()) =>
                {
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
                let program = lunchroom_core::setgid::as_own_group(|| {
                    std::fs::read_link(format!("/proc/{pid}/exe"))
                })
                .map_or_else(|_| format!("/proc/{pid}/exe"), |p| p.display().to_string());
                version_fault(passed, who.harness, plugin_release(recorded), program)
            }
        }
    };
    why.map_or(Assessment::Guarded, Assessment::Unguarded)
}

/// What the user is told when Claude Code's tool-call hook is an earlier install's: it does not
/// block a call when the guard fails (H1.5).
pub(crate) const CLAUDE_OLD_HOOK: &str = "Claude Code's tool-call hook for Lunchroom's guard is from \
     an earlier install: it does not block a call when the guard fails (no failure branch, or no \
     \"onFailure\": \"block\"). Reinstall: run lunchroom install claude-code in a terminal of your \
     own, then start the session anew.";

/// What the user is told when Codex's tool-call hook is an earlier install's (H1.5).
pub(crate) const CODEX_OLD_HOOK: &str = "Codex's tool-call hook for Lunchroom's guard is from an \
     earlier install: it does not block a call when the guard fails. Reinstall: run lunchroom \
     install codex in a terminal of your own, trust Lunchroom's hooks in Codex's /hooks, then \
     start the session anew.";

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

/// A Codex thread that has a session record and no guard hook record (H1.4).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))] // `lunchroom status` and `doctor` print it (spec C1.4)
pub struct UnrecordedThread {
    /// The thread's id, as Codex names the session.
    pub thread: String,
    /// The session record, `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
    pub file: PathBuf,
}

/// The id a rollout file's name ends with: `rollout-<time>-<thread id>.jsonl`, or the same
/// compressed by Codex's maintenance (`.jsonl.zst`). The thread id is a UUID.
#[cfg_attr(not(test), allow(dead_code))] // used by `unrecorded_codex_threads`
fn rollout_thread(name: &str) -> Option<&str> {
    let stem = name
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")
        .or_else(|| name.strip_suffix(".jsonl.zst")?.strip_prefix("rollout-"))?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let shape = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = id.split('-').collect();
    (parts.len() == shape.len()
        && parts
            .iter()
            .zip(shape)
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit())))
    .then_some(id)
}

/// The Codex threads with a session record in `<codex_home>/sessions` that no guard hook has
/// recorded in `mail`, oldest first (H1.4): `lunchroom status` and `lunchroom doctor` show each as
/// UNGUARDED with the reason [`CODEX_NO_HOOK_RAN`]. A thread a hook recorded has a session in
/// `mail`, whatever its state.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))] // `lunchroom status` and `doctor` print it (spec C1.4)
pub fn unrecorded_codex_threads(mail: &Mail, codex_home: &Path) -> Vec<UnrecordedThread> {
    let mut found: Vec<UnrecordedThread> = Vec::new();
    let mut dirs = vec![(codex_home.join("sessions"), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 4 {
                    dirs.push((path, depth + 1));
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(thread) = rollout_thread(&name) else {
                continue;
            };
            let who = Identity::new(Harness::Codex, thread);
            if mail.session(&who).is_none() && !found.iter().any(|f| f.thread == thread) {
                found.push(UnrecordedThread {
                    thread: thread.to_owned(),
                    file: path,
                });
            }
        }
    }
    found.sort_by(|a, b| a.file.file_name().cmp(&b.file.file_name()));
    found
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
///
/// H1.4: the string recorded also holds, on its first line, the tty and web search settings in force
/// for the session's `codex` process, read from the configuration files and from that process's
/// command line at the session's first hook ([`CodexSettings`]). `previous` is the string recorded
/// at the session's earlier hook: while it was recorded for the same process, its settings are kept
/// as they were, so a session recorded with either on stays UNGUARDED across later hooks (until it
/// is started anew, which is a new process).
#[must_use]
pub fn codex_guard_fingerprint(
    pid: u32,
    cwd: &Path,
    home: &Path,
    previous: Option<&str>,
) -> Option<String> {
    let codex_home = codex_home_of(pid, home);
    let key = process::key(pid)?;
    let settings = previous
        .and_then(CodexSettings::parse)
        .filter(|s| s.process == key)
        .unwrap_or_else(|| CodexSettings::read(&key, &codex_home, pid, cwd));
    let hooks = codex_hooks_in(&codex_home, pid, cwd).ok();
    if hooks.is_none() && !settings.on() {
        return None;
    }
    Some(format!(
        "{}\n{}",
        settings.line(),
        hooks.unwrap_or_default()
    ))
}

/// The hooks part of a recorded fingerprint: what follows the settings line.
fn recorded_hooks(recorded: &str) -> &str {
    recorded.split_once('\n').map_or("", |(_, hooks)| hooks)
}

#[cfg(test)]
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
    /// `config.toml` does not set [`CODEX_WEB`] to `"disabled"` at its top level (or is not TOML).
    WebNotOff,
    /// A profile, or a project's `.codex/config.toml`, sets [`CODEX_WEB`] to another value, or is
    /// not TOML.
    WebTurnedOn,
    /// The session's `codex` process was started with an option that turns the tty setting on (H1.4).
    CommandTty(String),
    /// The session's `codex` process was started with an option that turns web search on (H1.4).
    CommandWeb(String),
    /// The record made at the session's first hook shows the tty setting on, the web search
    /// setting on, or both (H1.4): the session stays UNGUARDED until it is started anew.
    Recorded { tty: String, web: String },
    /// The session runs in a shared server, and a running `codex` process of the user was started
    /// with an option that turns the tty setting or web search on (H1.4).
    SharedServer(String),
    /// The hooks file `file` holds the whole guard set, but its tool-call hook is an earlier
    /// install's, which does not block a call when the guard fails (H1.5).
    OldToolHook(PathBuf),
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
/// What the user is told when [`CODEX_WEB`] is not `"disabled"`.
const CODEX_WEB_NOT_OFF: &str = "Codex sessions are UNGUARDED until web_search is \"disabled\" at \
     the top of Codex's config.toml: run lunchroom install codex in a terminal of your own, then \
     start them anew.";
/// What the user is told when a profile or a project sets [`CODEX_WEB`] to another value.
const CODEX_WEB_TURNED_ON: &str = "A Codex config.toml sets web_search to a value other than \
     \"disabled\", in a project or a profile, or is not TOML: the Codex sessions it reaches are \
     UNGUARDED until you change that file.";
/// What the user is told of a Codex session open when the install set [`CODEX_TTY`] and
/// [`CODEX_WEB`].
pub(crate) const CODEX_OPENED_BEFORE: &str = "A Codex session started before unified_exec_tty and \
     web_search were set is UNGUARDED: start a new session; resuming this one under the same Codex \
     server keeps it UNGUARDED.";
/// Why a Codex thread with a session record and no hook record is UNGUARDED (H1.4, R3.4).
#[cfg_attr(not(test), allow(dead_code))] // `lunchroom status` and `doctor` print it (spec C1.4)
pub(crate) const CODEX_NO_HOOK_RAN: &str = "no guard hook has run in it";

impl CodexFault {
    /// The fault as the user reads it, naming the setting or the hook, and what to do.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::TtyNotOff => CODEX_TTY_NOT_OFF.to_owned(),
            Self::TtyTurnedOn => CODEX_TTY_TURNED_ON.to_owned(),
            Self::WebNotOff => CODEX_WEB_NOT_OFF.to_owned(),
            Self::WebTurnedOn => CODEX_WEB_TURNED_ON.to_owned(),
            Self::CommandTty(by) => format!(
                "This Codex session was started with {by}, which turns unified_exec_tty on: it is \
                 UNGUARDED until you start it anew without that option."
            ),
            Self::CommandWeb(by) => format!(
                "This Codex session was started with {by}, which turns web search on: it is \
                 UNGUARDED until you start it anew without that option."
            ),
            Self::Recorded { tty, web } => {
                let named: Vec<String> = [("unified_exec_tty", tty), ("web search", web)]
                    .into_iter()
                    .filter(|(_, by)| !by.is_empty())
                    .map(|(what, by)| format!("{what} on by {by}"))
                    .collect();
                format!(
                    "This Codex session was started with {}, as its first guard hook read it: it \
                     is UNGUARDED until you start it anew.",
                    named.join(" and ")
                )
            }
            Self::SharedServer(by) => format!(
                "A Codex window of yours was started with {by}, which turns unified_exec_tty or \
                 web search on, and Codex's shared server runs this session: every session in that \
                 server is UNGUARDED until that window ends."
            ),
            Self::OldToolHook(file) => format!("{CODEX_OLD_HOOK} (in {})", file.display()),
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
    if let Some(fault) = codex_settings_fault(codex_home, pid, cwd) {
        return Err(fault);
    }
    codex_hooks_in(codex_home, pid, cwd)
}

/// The first of what in the configuration files and on the command line of the `codex` process
/// `pid` turns the tty setting or web search on (H1.1, H1.4), or `None`. The files first, then the
/// command line, then, for a session in a shared server, the command lines of the user's other
/// `codex` processes.
fn codex_settings_fault(codex_home: &Path, pid: u32, cwd: &Path) -> Option<CodexFault> {
    let config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap_or_default();
    // Codex runs no hook for `write_stdin`: with terminals on, a HELD agent could type into a shell
    // it opened before the hold, so a session is guarded only with them off (H1.1).
    if codex_tty_setting(&config) != Some(false) {
        return Some(CodexFault::TtyNotOff);
    }
    if turns_tty_on(&config) || codex_project_tty(cwd) {
        return Some(CodexFault::TtyTurnedOn);
    }
    // Codex's hosted web search reaches no hook either (H1.4): guarded only where it is `disabled`,
    // by the same files and profiles.
    if !codex_web_is_off(&config) {
        return Some(CodexFault::WebNotOff);
    }
    if turns_web_on(&config) || codex_project_web(cwd) {
        return Some(CodexFault::WebTurnedOn);
    }
    let mut files = vec![config];
    files.extend(project_configs(cwd));
    let line = CommandLine::read(&cmdline_args(pid), &files);
    if let Some(by) = line.tty.first() {
        return Some(CodexFault::CommandTty(by.clone()));
    }
    if let Some(by) = line.web.first() {
        return Some(CodexFault::CommandWeb(by.clone()));
    }
    shared_server_fault(pid)
}

/// The hooks Codex runs for the session in process `pid`, as a fingerprint of the trusted guard set,
/// or the first fault in it (the settings are [`codex_settings_fault`]'s).
fn codex_hooks_in(codex_home: &Path, pid: u32, cwd: &Path) -> Result<String, CodexFault> {
    codex_hooks_with(
        &read_file,
        &|hook| program_present(hook, pid, cwd),
        codex_home,
        cwd,
    )
}

/// [`codex_hooks_in`] reading each file through `read`, once: both forms of the tool-call hook are
/// judged from the one reading of each hooks file, and the trust from one reading of `config.toml`.
fn codex_hooks_with(
    read: Reader<'_>,
    present: Present<'_>,
    codex_home: &Path,
    cwd: &Path,
) -> Result<String, CodexFault> {
    let config = read(&codex_home.join("config.toml")).unwrap_or_default();
    let mut fault = CodexFault::NoGuardSet(codex_home.join("hooks.json"));
    for file in [codex_home.join("hooks.json"), cwd.join(".codex/hooks.json")] {
        let Some(hooks) = json_from(read, &file) else {
            continue;
        };
        let holds = |form: PreTool| {
            CODEX_GUARD_HOOKS.iter().all(|(event, mode)| {
                has_guard_hook(&hooks, event, mode, &present, form, Some(&file))
            })
        };
        if !holds(PreTool::Codex) {
            // H1.5: a set whose tool-call hook is an earlier install's is named for what it is.
            if holds(PreTool::Older) && matches!(fault, CodexFault::NoGuardSet(_)) {
                fault = CodexFault::OldToolHook(file.clone());
            }
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

/// Codex's hosted web search mode: `disabled`, `cached`, `indexed` or `live` (Codex 0.163.0,
/// `codex-rs/config/src/config_toml.rs` lines 475-476), a top-level key of `config.toml`.
pub(crate) const CODEX_WEB: &str = "web_search";
/// The value of [`CODEX_WEB`] that turns web search off.
pub(crate) const CODEX_WEB_DISABLED: &str = "disabled";

/// `config` sets [`CODEX_WEB`] to `"disabled"` at its top level.
pub(crate) fn codex_web_is_off(config: &str) -> bool {
    config
        .parse::<DocumentMut>()
        .is_ok_and(|c| c.get(CODEX_WEB).is_some_and(web_value_is_off))
}

/// A `web_search` value that turns web search off: the string `disabled` (H1.4: any other value,
/// of any type, counts as on).
fn web_value_is_off(item: &toml_edit::Item) -> bool {
    item.as_str() == Some(CODEX_WEB_DISABLED)
}

/// A `unified_exec_tty` value that turns the terminals off: the boolean `false` (H1.4: any other
/// value, of any type, counts as on).
fn tty_value_is_off(item: &toml_edit::Item) -> bool {
    item.as_bool() == Some(false)
}

/// The tty setting a table of settings (a profile, or the top level) holds, and whether it is on.
fn table_tty_on(table: &toml_edit::Item) -> bool {
    table
        .get("features")
        .and_then(|f| f.get(CODEX_TTY))
        .is_some_and(|v| !tty_value_is_off(v))
}

/// The web search setting a table of settings holds, and whether it is on.
fn table_web_on(table: &toml_edit::Item) -> bool {
    table.get(CODEX_WEB).is_some_and(|v| !web_value_is_off(v))
}

/// `text` turns [`CODEX_TTY`] on, in its `[features]` or in any profile, with any value but
/// `false`, or is not TOML.
fn turns_tty_on(text: &str) -> bool {
    turns_on(text, table_tty_on)
}

/// `text` sets [`CODEX_WEB`] to any value but `"disabled"` at its top level or in any profile, or
/// is not TOML. A top level with no `web_search` is not this: it is the file that sets none
/// ([`codex_web_is_off`]).
fn turns_web_on(text: &str) -> bool {
    turns_on(text, table_web_on)
}

fn turns_on(text: &str, on: fn(&toml_edit::Item) -> bool) -> bool {
    text.parse::<DocumentMut>().map_or(true, |config| {
        on(config.as_item())
            || config
                .get("profiles")
                .and_then(toml_edit::Item::as_table_like)
                .is_some_and(|all| all.iter().any(|(_, p)| on(p)))
    })
}

/// The `.codex/config.toml` files Codex reads for a session in `cwd`: those in `cwd` and every
/// folder above it. Codex reads each of these in a trusted project; trust is not asked here.
fn project_configs(cwd: &Path) -> Vec<String> {
    cwd.ancestors()
        .filter_map(|folder| std::fs::read_to_string(folder.join(".codex/config.toml")).ok())
        .collect()
}

/// A `.codex/config.toml` in `cwd` or a folder above it turns [`CODEX_TTY`] on.
fn codex_project_tty(cwd: &Path) -> bool {
    project_configs(cwd).iter().any(|text| turns_tty_on(text))
}

/// A `.codex/config.toml` in `cwd` or a folder above it sets [`CODEX_WEB`] to another value.
fn codex_project_web(cwd: &Path) -> bool {
    project_configs(cwd).iter().any(|text| turns_web_on(text))
}

/// What the command line of a `codex` process turns on (H1.4), as the options it carries, each in
/// the spelling it was written in. Read from Codex 0.163.0: `-c` or `--config` (also `-c<value>`,
/// `-c=<value>` and `--config=<value>`, which `clap` reads alike) with the key
/// `features.unified_exec_tty` or the whole table `features`, and with the key `web_search`;
/// `--enable unified_exec_tty` (also `--enable=unified_exec_tty`), which is
/// `-c features.unified_exec_tty=true`; `--search`, which is `-c web_search="live"`; and `-p` or
/// `--profile` (also `-p<value>` and `--profile=<value>`) naming a profile a configuration file
/// holds that sets either on. `--enable web_search_request` turns nothing on: a `web_search` value
/// wins over Codex's older web search features (`codex-rs/core/src/config/mod.rs` lines 2736-2746).
/// Options are read wherever they stand up to a `--`: `-c`, `--enable` and `--disable` are global
/// to the subcommands (`global = true`), so `codex exec -c web_search=live` carries one too.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CommandLine {
    /// The options that turn the tty setting on.
    pub(crate) tty: Vec<String>,
    /// The options that turn web search on.
    pub(crate) web: Vec<String>,
}

/// The value of a `-c` override as Codex reads it (`codex-rs/utils/cli/src/config_override.rs`
/// lines 49-81): TOML when it parses, else the text with its quotes taken off as a string.
fn override_value(raw: &str) -> toml_edit::Item {
    format!("_x_ = {raw}")
        .parse::<DocumentMut>()
        .ok()
        .and_then(|d| d.get("_x_").cloned())
        .unwrap_or_else(|| toml_edit::value(raw.trim().trim_matches(|c| c == '"' || c == '\'')))
}

impl CommandLine {
    /// What `args`, the arguments of a `codex` process, turn on; `configs` are the configuration
    /// files' texts, in which a profile named by `-p` is looked up.
    pub(crate) fn read(args: &[String], configs: &[String]) -> Self {
        let mut line = Self::default();
        let mut at = 0;
        while at < args.len() {
            let arg = args[at].as_str();
            if arg == "--" {
                break;
            }
            at += 1;
            if arg == "--search" {
                line.web.push(arg.to_owned());
                continue;
            }
            let Some((option, value, took)) = option_of(arg, args.get(at)) else {
                continue;
            };
            at += took;
            let spelled = format!("{option} {value}");
            match option {
                "config" => line.override_on(&value, &spelled),
                "enable" => {
                    if value == CODEX_TTY {
                        line.tty.push(spelled);
                    }
                }
                "profile" => {
                    let sets = |on: fn(&toml_edit::Item) -> bool| {
                        configs.iter().any(|text| {
                            text.parse::<DocumentMut>().is_ok_and(|c| {
                                c.get("profiles")
                                    .and_then(|p| p.get(value.as_str()))
                                    .is_some_and(on)
                            })
                        })
                    };
                    if sets(table_tty_on) {
                        line.tty.push(spelled.clone());
                    }
                    if sets(table_web_on) {
                        line.web.push(spelled);
                    }
                }
                _ => {}
            }
        }
        line
    }

    /// The effect of one `-c key=value` override.
    fn override_on(&mut self, raw: &str, spelled: &str) {
        let Some((key, value)) = lunchroom_core::codex_cli::config_override(raw) else {
            return;
        };
        let value = override_value(&value);
        let tty_key = format!("features.{CODEX_TTY}");
        let profile_key = |tail: &str| {
            key.strip_prefix("profiles.")
                .and_then(|rest| rest.split_once('.'))
                .is_some_and(|(_, rest)| rest == tail)
        };
        if key == tty_key || profile_key(&tty_key) {
            if !tty_value_is_off(&value) {
                self.tty.push(spelled.to_owned());
            }
        } else if key == "features" {
            // The whole table: its `unified_exec_tty`, when it holds one (a table merges over the
            // file's, `codex-rs/config/src/merge.rs`); a value that is no table counts as on unless
            // it is `false`.
            let on = match value.as_table_like() {
                Some(table) => table.get(CODEX_TTY).is_some_and(|v| !tty_value_is_off(v)),
                None => !tty_value_is_off(&value),
            };
            if on {
                self.tty.push(spelled.to_owned());
            }
        } else if (key == CODEX_WEB || profile_key(CODEX_WEB)) && !web_value_is_off(&value) {
            self.web.push(spelled.to_owned());
        }
    }
}

/// The option `arg` is, as `clap` reads it for `codex` (the six this reads are
/// [`lunchroom_core::codex_cli`]'s `config`, `enable`, `disable`, `profile`, `search`): its name,
/// its value and how many of the following arguments it took (`next` is the argument after it).
/// A short option takes the rest of its argument as its value, a leading `=` dropped
/// (`parser.rs:979`); a long one takes what follows `=`, else the next argument.
fn option_of(arg: &str, next: Option<&String>) -> Option<(&'static str, String, usize)> {
    let long = |name: &'static str| -> Option<(&'static str, String, usize)> {
        let rest = arg.strip_prefix("--")?.strip_prefix(name)?;
        if let Some(value) = rest.strip_prefix('=') {
            return Some((name, value.to_owned(), 0));
        }
        rest.is_empty()
            .then(|| next.map(|v| (name, v.clone(), 1)))
            .flatten()
    };
    let short = |letter: char, name: &'static str| -> Option<(&'static str, String, usize)> {
        let rest = arg.strip_prefix('-')?.strip_prefix(letter)?;
        if rest.is_empty() {
            return next.map(|v| (name, v.clone(), 1));
        }
        Some((name, rest.strip_prefix('=').unwrap_or(rest).to_owned(), 0))
    };
    if arg.starts_with("--") {
        long("config")
            .or_else(|| long("enable"))
            .or_else(|| long("disable"))
            .or_else(|| long("profile"))
    } else {
        short('c', "config").or_else(|| short('p', "profile"))
    }
}

/// What a `codex app-server` process (the server Codex's windows share) is called on its command
/// line.
const CODEX_SERVER_WORD: &str = "app-server";

/// H1.4: a session that runs in a shared server (its process is the server) is UNGUARDED while any
/// running `codex` process of the user carries an option that turns the tty setting or web search
/// on, since the options a window was started with are on that window's own process, not on the
/// server's (codex-cli 0.166.0, measured 2026-10-31). Read from the `codex` processes now running,
/// so it ends with the window.
fn shared_server_fault(pid: u32) -> Option<CodexFault> {
    use std::os::unix::fs::MetadataExt as _;
    if !cmdline_args(pid).iter().any(|a| a == CODEX_SERVER_WORD) {
        return None;
    }
    let user = std::fs::metadata(format!("/proc/{pid}")).ok()?.uid();
    let own_files = |other: u32| {
        let home = process_env(other, "CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| process_env(other, "HOME").map(|h| Path::new(&h).join(".codex")));
        home.and_then(|h| std::fs::read_to_string(h.join("config.toml")).ok())
            .into_iter()
            .collect::<Vec<_>>()
    };
    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|other| *other != pid)
        .filter(|other| {
            std::fs::metadata(format!("/proc/{other}")).is_ok_and(|m| m.uid() == user)
                && is_codex_process(*other)
        })
        .find_map(|other| {
            let line = CommandLine::read(&cmdline_args(other), &own_files(other));
            line.tty
                .into_iter()
                .chain(line.web)
                .next()
                .map(|by| CodexFault::SharedServer(format!("{by} (process {other})")))
        })
}

/// Process `pid` is a `codex` process: its program, as its command line gives it, is named `codex`.
fn is_codex_process(pid: u32) -> bool {
    cmdline_args(pid)
        .first()
        .and_then(|program| program.rsplit('/').next().map(str::to_owned))
        .is_some_and(|name| name == "codex")
}

/// The tty and web search settings in force for a Codex session's `codex` process, as its first
/// guard hook read them from the configuration files and the process's command line (H1.4),
/// recorded as the first line of the session's guard fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSettings {
    /// The process the settings were read for: `<pid>:<start time>`.
    pub(crate) process: String,
    /// What turned the tty setting on (an option, or the file's setting); empty when it is off.
    pub(crate) tty: String,
    /// What turned web search on; empty when it is off.
    pub(crate) web: String,
}

/// Starts the settings line of a recorded fingerprint.
const SETTINGS_LINE: &str = "codex-settings";

impl CodexSettings {
    pub(crate) fn read(key: &str, codex_home: &Path, pid: u32, cwd: &Path) -> Self {
        let config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap_or_default();
        let mut files = vec![config.clone()];
        files.extend(project_configs(cwd));
        let line = CommandLine::read(&cmdline_args(pid), &files);
        // Each setting on its own, the files first: the one the user fixes in a file, then the
        // option the process was started with.
        let tty = if codex_tty_setting(&config) != Some(false) {
            "config.toml".to_owned()
        } else if turns_tty_on(&config) || codex_project_tty(cwd) {
            "a profile or project file".to_owned()
        } else {
            line.tty.first().cloned().unwrap_or_default()
        };
        let web = if !codex_web_is_off(&config) {
            "config.toml".to_owned()
        } else if turns_web_on(&config) || codex_project_web(cwd) {
            "a profile or project file".to_owned()
        } else {
            line.web.first().cloned().unwrap_or_default()
        };
        Self {
            process: key.to_owned(),
            tty: clean(&tty),
            web: clean(&web),
        }
    }

    /// Either setting is on.
    pub(crate) fn on(&self) -> bool {
        !self.tty.is_empty() || !self.web.is_empty()
    }

    /// The settings as the first line of a recorded fingerprint.
    pub(crate) fn line(&self) -> String {
        format!("{SETTINGS_LINE}|{}|{}|{}", self.process, self.tty, self.web)
    }

    /// The settings of a recorded fingerprint, or `None` for a record that has none.
    pub(crate) fn parse(recorded: &str) -> Option<Self> {
        let first = recorded.lines().next()?;
        let mut parts = first.splitn(4, '|');
        (parts.next()? == SETTINGS_LINE).then_some(())?;
        Some(Self {
            process: parts.next()?.to_owned(),
            tty: parts.next()?.to_owned(),
            web: parts.next()?.to_owned(),
        })
    }
}

/// `text` as one field of the settings line: no separator and no line break.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c == '|' || c.is_control() { ' ' } else { c })
        .collect()
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
pub(crate) fn guard_program<'a>(
    hook: &'a Value,
    mode: &str,
    form: PreTool,
    source: Option<&Path>,
) -> Option<&'a str> {
    let command = hook["command"].as_str().unwrap_or_default();
    // A command past the bound is not Lunchroom's line in any form, and is read no further.
    if command.len() > crate::install::MAX_HOOK_COMMAND {
        return None;
    }
    let (program, args) = lunchroom_command(hook)?;
    if mode != "pretool" {
        return (args == format!("guard {mode}")).then_some(program);
    }
    // H1.5: the tool-call hook is `guard pretool` followed by the failure branch the install
    // writes, byte for byte, and its line names the configuration file the hook was read from; in
    // Claude Code it also carries `"onFailure": "block"`. An install of 1.0.x wrote `guard
    // pretool` alone, which fails open when the guard fails; only a diagnosis ([`PreTool::Older`])
    // takes it. A hook read from no file (settings given as text) cannot be the install's.
    // One ASCII space, nothing else, between the program and `guard` (the shell splits a command
    // at spaces and tabs only, and a hook is run as written).
    let before = &command[..command.len().saturating_sub(args.len())];
    let one_space = before
        .strip_suffix(' ')
        .is_some_and(|p| !p.ends_with(char::is_whitespace));
    let written =
        source.is_some_and(|file| one_space && crate::install::is_fail_closed_for(args, file));
    let accepted = match form {
        PreTool::Codex => written,
        PreTool::Claude => written && hook["onFailure"] == "block",
        // The diagnosis "reinstall" is for the hook as the install writes it (here without
        // `onFailure`) and for the one form a released installer wrote: `guard pretool` alone
        // (1.0.1, commit 276b58f, install.rs line 346). Any other text is no Lunchroom form: the
        // line of another file's, an alias, an altered or whitespace-altered line, a bare
        // `|| exit 2` (no release wrote it), or settings given as text.
        PreTool::Older => {
            source.is_some() && (written || one_space && crate::install::is_pre_1_1_pretool(args))
        }
    };
    accepted.then_some(program)
}

/// How much of H1.5 a tool-call hook must have to count as the guard's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreTool {
    /// Codex: `guard pretool` and the failure branch.
    Codex,
    /// Claude Code: the failure branch and `"onFailure": "block"`.
    Claude,
    /// An older install's hook is accepted too (to tell "reinstall" from "no guard at all").
    Older,
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
    // As the user's own group: a setgid program cannot read another process's exe or cwd link
    // (`lunchroom_core::setgid::as_own_group`).
    lunchroom_core::setgid::as_own_group(|| process_programs_read(pid))
}

fn process_programs_read(pid: u32) -> Programs {
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
    let raw =
        lunchroom_core::setgid::as_own_group(|| std::fs::read(format!("/proc/{pid}/environ")))
            .ok()?;
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
fn configured(pid: u32, cwd: &Path, home: &Path) -> ClaudeGuard {
    let config_dir =
        process_env(pid, "CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    claude_guard_with(
        &read_file,
        &|hook| program_present(hook, pid, cwd),
        &cmdline_args(pid),
        &config_dir,
        Path::new(MANAGED_SETTINGS),
        cwd,
    )
}

/// What Claude Code's settings hold of Lunchroom's guard (H1.5), judged from one reading of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeGuard {
    /// The whole guard set, the tool-call hook as the install writes it.
    Guarded,
    /// The whole guard set, but the tool-call hook is an earlier install's: reinstall.
    OlderHook,
    /// Not the whole guard set, or no hooks run.
    NotConfigured,
}

/// How a file's text is read: the real one, or a test's.
type Reader<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// Whether a hook's program is there to run (R3.1): the real one asks the file system; a test's
/// records what it is asked.
type Present<'a> = &'a dyn Fn(&Value) -> bool;

fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The JSON in the file `path`, read with `read`.
fn json_from(read: Reader<'_>, path: &Path) -> Option<Value> {
    serde_json::from_str(&read(path)?).ok()
}

/// [`claude_guard_with`] as the boolean for one form of the tool-call hook: whether the settings
/// hold the whole guard set with the tool-call hook in the form `form` (or, for
/// [`PreTool::Older`], in either).
#[cfg(test)]
fn configured_with(
    args: &[String],
    config_dir: &Path,
    managed: &Path,
    pid: u32,
    cwd: &Path,
    form: PreTool,
) -> bool {
    match claude_guard_with(
        &read_file,
        &|hook| program_present(hook, pid, cwd),
        args,
        config_dir,
        managed,
        cwd,
    ) {
        ClaudeGuard::Guarded => true,
        ClaudeGuard::OlderHook => form == PreTool::Older,
        ClaudeGuard::NotConfigured => false,
    }
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
///
/// Each settings file is read once, through `read`, and both forms of the tool-call hook are judged
/// from that one reading: a file replaced between two readings cannot be judged guarded by one
/// and older by the other.
fn claude_guard_with(
    read: Reader<'_>,
    present: Present<'_>,
    args: &[String],
    config_dir: &Path,
    managed: &Path,
    cwd: &Path,
) -> ClaudeGuard {
    if args.iter().any(|a| a == "--bare") {
        return ClaudeGuard::NotConfigured;
    }
    let managed_json = json_from(read, managed);
    let managed_flag = |key: &str| managed_json.as_ref().and_then(|m| m[key].as_bool());
    let managed_only = managed_flag("allowManagedHooksOnly");
    if managed_flag("disableAllHooks") == Some(true) {
        return ClaudeGuard::NotConfigured;
    }
    let loaded = setting_sources(args);
    // Each settings value with the file it was read from (none for settings given as text).
    let mut levels: Vec<(Option<PathBuf>, Value)> = [
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
    .filter_map(|(_, path)| json_from(read, &path).map(|v| (Some(path), v)))
    .collect();
    levels.extend(settings_flag(read, args, cwd));
    let others_off = levels
        .iter()
        .rev()
        .find_map(|(_, v)| v["disableAllHooks"].as_bool())
        == Some(true);
    let mut running: Vec<(Option<PathBuf>, Value)> = managed_json
        .into_iter()
        .map(|v| (Some(managed.to_path_buf()), v))
        .collect();
    if !others_off && managed_only != Some(true) {
        running.extend(levels);
    }
    let holds = |form: PreTool| {
        CLAUDE_GUARD_HOOKS.iter().all(|(event, mode)| {
            running
                .iter()
                .any(|(file, v)| has_guard_hook(v, event, mode, &present, form, file.as_deref()))
        })
    };
    if holds(PreTool::Claude) {
        ClaudeGuard::Guarded
    } else if holds(PreTool::Older) {
        ClaudeGuard::OlderHook
    } else {
        ClaudeGuard::NotConfigured
    }
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
fn settings_flag(read: Reader<'_>, args: &[String], cwd: &Path) -> Vec<(Option<PathBuf>, Value)> {
    option_values(args, "--settings")
        .filter_map(|given| {
            if given.trim_start().starts_with('{') {
                serde_json::from_str(given).ok().map(|v| (None, v))
            } else {
                let path = PathBuf::from(given);
                let path = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
                json_from(read, &path).map(|v| (Some(path), v))
            }
        })
        .collect()
}

/// `settings` holds a hook of `event` that runs Lunchroom's `guard <mode>` on every occurrence of
/// the event: its group's matcher is absent, empty or `*` (any other matcher is a tool name or a
/// pattern, which leaves the calls it does not match unchecked; `UserPromptSubmit` has no
/// matcher), and the hook is a synchronous command with no `if` filter whose program is there to
/// run.
fn has_guard_hook(
    settings: &Value,
    event: &str,
    mode: &str,
    present: &dyn Fn(&Value) -> bool,
    form: PreTool,
    source: Option<&Path>,
) -> bool {
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
            guard_program(h, mode, form, source).is_some()
                && h["type"] == "command"
                && h["if"].is_null()
                && h["async"].as_bool() != Some(true)
                && h["asyncRewake"].as_bool() != Some(true)
                && present(h)
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
    claude_setup_fault_with(
        &read_file,
        &|hook| program_present(hook, pid, cwd),
        config_dir,
        managed,
        cwd,
    )
}

/// [`claude_setup_fault`] reading the settings through `read`, once.
fn claude_setup_fault_with(
    read: Reader<'_>,
    present: Present<'_>,
    config_dir: &Path,
    managed: &Path,
    cwd: &Path,
) -> Option<String> {
    match claude_guard_with(read, present, &[], config_dir, managed, cwd) {
        ClaudeGuard::Guarded => None,
        ClaudeGuard::OlderHook => Some(format!(
            "{} (settings in {}, and those of the project in {})",
            CLAUDE_OLD_HOOK,
            config_dir.join("settings.json").display(),
            cwd.display()
        )),
        ClaudeGuard::NotConfigured => Some(format!(
            "Claude Code's settings ({}, and those of the project in {}) do not run Lunchroom's \
             whole guard (PreToolUse, PostToolUse, PostToolUseFailure and UserPromptSubmit, each \
             on every call, its program there to run), or stop every hook (disableAllHooks): run \
             lunchroom install claude-code in a terminal of your own.",
            config_dir.join("settings.json").display(),
            cwd.display()
        )),
    }
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
mod tests_f1;

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
        let off = "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n";
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
        let hook = |mode: &str| json!([{"hooks": [{"type": "command", "command": written(&program, &format!("guard {mode}"), &file)}]}]);
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

    /// A Lunchroom hook's command as the install writes it: the tool-call hook (`guard pretool`)
    /// with H1.5's failure branch, the others as they are.
    fn written(program: &str, rest: &str, file: &Path) -> String {
        if rest == "guard pretool" {
            crate::install::pretool_command(program, file)
        } else {
            format!("{program} {rest}")
        }
    }

    /// Stands for the settings file a Claude Code fixture is written to: [`Claude::write`] puts the
    /// file's own name in its place, since the line of the tool-call hook names the file it is in.
    const HERE: &str = "/x/here.json";

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    // §13::68-§13::75 (H1.4): each command-line spelling that turns the tty setting or
    // web search on is read as Codex reads it, and each that does not is not: the key spellings
    // (`-c`, `--config`, `-c<value>`, `-c=<value>`, `--config=<value>`), spaces around the key, a
    // value of any type but `false` (tty) or `"disabled"` (web search), the whole table
    // `features`, `--enable` and `--enable=`, `--search`, a profile named by `-p` or `--profile`,
    // options after a subcommand, and nothing after `--`.
    #[test]
    #[allow(clippy::too_many_lines)] // one table of spellings
    fn command_line_spellings_turn_the_settings_on_as_codex_reads_them() {
        let tty = |a: &[&str]| CommandLine::read(&words(a), &[]).tty.len();
        let web = |a: &[&str]| CommandLine::read(&words(a), &[]).web.len();
        for on in [
            &["-c", "features.unified_exec_tty=true"][..],
            &["-c", "features.unified_exec_tty=1"],
            &["-c", "features.unified_exec_tty=\"x\""],
            &["-c", "features.unified_exec_tty="],
            &["-c", "features.unified_exec_tty=\"false\""],
            &["-c", " features.unified_exec_tty = true"],
            &["--config", "features.unified_exec_tty=true"],
            &["-cfeatures.unified_exec_tty=true"],
            &["-c=features.unified_exec_tty=true"],
            &["--config=features.unified_exec_tty=true"],
            &["-c", "features={unified_exec_tty=true}"],
            &["-c", "features={ unified_exec_tty = \"x\" }"],
            &["-c", "features=true"],
            &["-c", "profiles.p.features.unified_exec_tty=true"],
            &["--enable", "unified_exec_tty"],
            &["--enable=unified_exec_tty"],
            &["-m", "gpt", "-c", "features.unified_exec_tty=true"],
            &["exec", "-c", "features.unified_exec_tty=true", "a prompt"],
            &[
                "-c",
                "features.unified_exec_tty=false",
                "--enable",
                "unified_exec_tty",
            ],
        ] {
            assert_eq!(tty(on), 1, "{on:?}");
            assert_eq!(web(on), 0, "{on:?}");
        }
        for off in [
            &["-c", "features.unified_exec_tty=false"][..],
            &["--config=features.unified_exec_tty=false"],
            &["-c", "features={unified_exec_tty=false}"],
            &["-c", "features={hooks=true}"],
            &["-c", "features=false"],
            &["--disable", "unified_exec_tty"],
            &["--enable", "web_search_request"],
            &["--enable", "hooks"],
            &["-c", "features.unified_exec_tty"],
            &["--", "-c", "features.unified_exec_tty=true"],
            &["exec", "--", "--enable", "unified_exec_tty"],
            &["-m", "unified_exec_tty"],
            &[],
        ] {
            assert_eq!(tty(off), 0, "{off:?}");
        }
        for on in [
            &["-c", "web_search=\"live\""][..],
            &["-c", "web_search=live"],
            &["-c", "web_search=\"cached\""],
            &["-c", "web_search=\"indexed\""],
            &["-c", "web_search="],
            &["-c", "web_search=\"\""],
            &["-c", "web_search=1"],
            &["-c", " web_search = \"live\""],
            &["--config", "web_search=\"live\""],
            &["-cweb_search=\"live\""],
            &["-c=web_search=\"live\""],
            &["--config=web_search=\"live\""],
            &["-c", "profiles.p.web_search=\"live\""],
            &["--search"],
            &["-m", "gpt", "--search", "a prompt"],
            &["exec", "-c", "web_search=\"live\"", "a prompt"],
            &["-c", "web_search=\"disabled\"", "--search"],
        ] {
            assert_eq!(web(on), 1, "{on:?}");
            assert_eq!(tty(on), 0, "{on:?}");
        }
        for off in [
            &["-c", "web_search=\"disabled\""][..],
            &["-c", "web_search=disabled"],
            &["-c", "web_search='disabled'"],
            &["--config=web_search=\"disabled\""],
            &["-cweb_search=\"disabled\""],
            &["--enable", "web_search_request"],
            &[
                "--enable=web_search_request",
                "-c",
                "web_search=\"disabled\"",
            ],
            &["--", "--search"],
            &["-c", "web_search"],
            &["--search=yes"],
        ] {
            assert_eq!(web(off), 0, "{off:?}");
        }
        // `-p` or `--profile` naming a profile that sets either on.
        let config = "[profiles.t.features]\nunified_exec_tty = true\n\
                      [profiles.w]\nweb_search = \"live\"\n\
                      [profiles.ok]\nweb_search = \"disabled\"\n[profiles.ok.features]\nunified_exec_tty = false\n"
            .to_owned();
        let read = |a: &[&str]| CommandLine::read(&words(a), std::slice::from_ref(&config));
        for (args, tty_on, web_on) in [
            (&["-p", "t"][..], 1, 0),
            (&["--profile", "t"], 1, 0),
            (&["--profile=t"], 1, 0),
            (&["-pt"], 1, 0),
            (&["-p", "w"], 0, 1),
            (&["--profile=w"], 0, 1),
            (&["-p", "ok"], 0, 0),
            (&["-p", "missing"], 0, 0),
        ] {
            let got = read(args);
            assert_eq!((got.tty.len(), got.web.len()), (tty_on, web_on), "{args:?}");
        }
        // The spelling is kept, for the user to read.
        let got = CommandLine::read(&words(&["--config=web_search=\"live\""]), &[]);
        assert_eq!(got.web, ["config web_search=\"live\"".to_owned()]);
    }

    // §13::65, §13::73, §13::74 (H1.4): web search is guarded only where `web_search` is
    // `"disabled"` at the top of `config.toml`, by the same files and profiles as the tty setting:
    // unset, `live`, `cached`, `indexed`, an empty value and a value of another type each count as
    // on, in `config.toml`, in a profile and in a project's `.codex/config.toml`; the tty values
    // `true`, `1`, an empty string and `"x"` count as on, `false` does not.
    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, each file the setting can come from
    fn web_search_and_tty_values_are_read_from_every_file_and_profile() {
        let root = crate::test_target()
            .join("codex-web-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let project = root.join("project");
        let _ = std::fs::create_dir_all(&codex_home);
        let _ = std::fs::create_dir_all(project.join(".codex"));
        let fault = |home: &str, project_config: &str| {
            let _ = std::fs::write(codex_home.join("config.toml"), home);
            let _ = std::fs::write(project.join(".codex/config.toml"), project_config);
            codex_settings_fault(&codex_home, std::process::id(), &project)
        };
        let ok = "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n";
        assert_eq!(fault(ok, ""), None);
        assert_eq!(fault(ok, "model = \"x\"\n"), None);
        // `config.toml`: unset, and every other value.
        assert_eq!(
            fault("[features]\nunified_exec_tty = false\n", ""),
            Some(CodexFault::WebNotOff)
        );
        for value in [
            "\"live\"",
            "\"cached\"",
            "\"indexed\"",
            "\"\"",
            "1",
            "true",
            "[]",
        ] {
            let home = format!("web_search = {value}\n[features]\nunified_exec_tty = false\n");
            assert_eq!(fault(&home, ""), Some(CodexFault::WebNotOff), "{value}");
        }
        // The same keys as dotted or quoted TOML.
        for spelling in [
            "\"web_search\" = \"disabled\"\n",
            "web_search = 'disabled'\n",
        ] {
            let home = format!("{spelling}[features]\nunified_exec_tty = false\n");
            assert_eq!(fault(&home, ""), None, "{spelling}");
        }
        // A profile, and a project's file.
        for value in ["\"live\"", "\"cached\"", "\"indexed\"", "\"\"", "1"] {
            let profile = format!("{ok}[profiles.p]\nweb_search = {value}\n");
            assert_eq!(
                fault(&profile, ""),
                Some(CodexFault::WebTurnedOn),
                "{value}"
            );
            let project_config = format!("web_search = {value}\n");
            assert_eq!(
                fault(ok, &project_config),
                Some(CodexFault::WebTurnedOn),
                "{value}"
            );
        }
        assert_eq!(
            fault(ok, "[profiles.q]\nweb_search = \"live\"\n"),
            Some(CodexFault::WebTurnedOn)
        );
        assert_eq!(fault(ok, "web_search = \"disabled\"\n"), None);
        assert_eq!(fault(ok, "[not toml\n"), Some(CodexFault::TtyTurnedOn));
        // The tty values.
        for (value, on) in [
            ("true", true),
            ("1", true),
            ("\"\"", true),
            ("\"x\"", true),
            ("false", false),
        ] {
            let home =
                format!("web_search = \"disabled\"\n[features]\nunified_exec_tty = {value}\n");
            let expected = (on).then_some(CodexFault::TtyNotOff);
            assert_eq!(fault(&home, ""), expected, "home {value}");
            let project_config = format!("[features]\nunified_exec_tty = {value}\n");
            let expected = (on).then_some(CodexFault::TtyTurnedOn);
            assert_eq!(fault(ok, &project_config), expected, "project {value}");
            let profile = format!("{ok}[profiles.p.features]\nunified_exec_tty = {value}\n");
            assert_eq!(fault(&profile, ""), expected, "profile {value}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Held by each test that runs stand-in `codex` processes: a window with an option makes a
    /// shared-server session UNGUARDED (H1.4), so the tests must not overlap.
    static WINDOWS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stand-in `codex` process whose command line carries `options`; it is ended on drop.
    struct StandIn(std::process::Child);

    impl StandIn {
        fn start(codex_home: &Path, options: &[&str]) -> Option<Self> {
            use std::os::unix::process::CommandExt as _;
            let child = std::process::Command::new("/bin/sh")
                .arg0("codex")
                .args(["-c", "sleep 30; :", "stand-in"])
                .args(options)
                .env("CODEX_HOME", codex_home)
                .stdin(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stand = Self(child);
            // Until the process has started its program its command line is this test's.
            for _ in 0..200 {
                if cmdline_args(stand.0.id()).first().map(String::as_str) == Some("codex") {
                    return Some(stand);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            None
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // §13::66, §13::67, §13::68-§13::72, §13::75, §4::38, §4::40 (H1.4): the settings in
    // force for a session's `codex` process, read from the files and its real command line and
    // recorded with the guard fingerprint at the first hook; a session recorded with either on
    // stays UNGUARDED across later hooks, whatever the files say then, and is read anew only in
    // another process.
    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, each spelling on a real process
    fn the_first_hook_records_the_settings_of_the_codex_process() {
        let _alone = WINDOWS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = crate::test_target()
            .join("codex-record-unit")
            .join(std::process::id().to_string());
        let home = root.join("home");
        let codex_home = home.join(".codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let ok = "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n\
                  [profiles.live]\nweb_search = \"disabled\"\n";
        let _ = std::fs::write(codex_home.join("config.toml"), ok);
        let read = |stand: &StandIn| {
            let key = process::key(stand.pid()).unwrap_or_default();
            CodexSettings::read(&key, &codex_home, stand.pid(), &root)
        };
        for (options, tty_on, web_on) in [
            (&["--search"][..], false, true),
            (&["-c", "web_search=\"live\""], false, true),
            (&["--config=web_search=cached"], false, true),
            (&["-c", "features.unified_exec_tty=true"], true, false),
            (&["-cfeatures.unified_exec_tty=1"], true, false),
            (&["--enable", "unified_exec_tty"], true, false),
            (&["--enable=unified_exec_tty"], true, false),
            (&["-c", "features={unified_exec_tty=true}"], true, false),
            (&["-c", "web_search=\"disabled\""], false, false),
            (&["--enable", "web_search_request"], false, false),
            (
                &[
                    "--enable=web_search_request",
                    "-c",
                    "web_search=\"disabled\"",
                ],
                false,
                false,
            ),
            (&["-c", "features.unified_exec_tty=false"], false, false),
            (&[], false, false),
        ] {
            let started = StandIn::start(&codex_home, options);
            assert!(started.is_some(), "{options:?}");
            let Some(stand) = started else { return };
            let settings = read(&stand);
            assert_eq!(
                (!settings.tty.is_empty(), !settings.web.is_empty()),
                (tty_on, web_on),
                "{options:?}: {settings:?}"
            );
            // The live check gives the same answer, naming the option.
            let live = codex_settings_fault(&codex_home, stand.pid(), &root);
            assert_eq!(live.is_some(), tty_on || web_on, "{options:?}: {live:?}");
        }
        // The first hook's record, kept by later hooks while the process stays the same.
        let started = StandIn::start(&codex_home, &[]);
        assert!(started.is_some());
        let Some(stand) = started else { return };
        let _ = std::fs::write(
            codex_home.join("config.toml"),
            "[features]\nunified_exec_tty = true\n",
        );
        let first = codex_guard_fingerprint(stand.pid(), &root, &home, None);
        let recorded = first.as_deref().and_then(CodexSettings::parse);
        assert!(
            recorded
                .as_ref()
                .is_some_and(|s| !s.tty.is_empty() && !s.web.is_empty())
        );
        // The user fixes the files; the process is the same, so the record stays.
        let _ = std::fs::write(codex_home.join("config.toml"), ok);
        let later = codex_guard_fingerprint(stand.pid(), &root, &home, first.as_deref());
        assert_eq!(later, first);
        // With no record (a session started anew in a new process) the files are read again.
        assert_eq!(
            codex_guard_fingerprint(stand.pid(), &root, &home, None),
            None
        );
        // A record made for another process is not this process's.
        let other = first
            .as_deref()
            .map(|f| f.replacen(&process::key(stand.pid()).unwrap_or_default(), "1:1", 1));
        assert_eq!(
            codex_guard_fingerprint(stand.pid(), &root, &home, other.as_deref()),
            None
        );
        assert_eq!(recorded_hooks("codex-settings|1:1||\nhooks"), "hooks");
        assert_eq!(recorded_hooks("codex-settings|1:1||"), "");
        let _ = std::fs::remove_dir_all(&root);
    }

    // §13::76 (H1.4): a session in a shared server is UNGUARDED while any running `codex`
    // process of the user was started with an option that turns the tty setting or web search on,
    // and not made so by windows started only with `-c web_search="disabled"`, nor after the
    // window ends.
    #[test]
    fn a_window_with_such_an_option_makes_the_shared_server_unguarded() {
        let _alone = WINDOWS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = crate::test_target()
            .join("codex-server-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let _ = std::fs::write(
            codex_home.join("config.toml"),
            "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n",
        );
        let started = StandIn::start(&codex_home, &["app-server"]);
        assert!(started.is_some());
        let Some(server) = started else { return };
        if shared_server_fault(server.pid()).is_some() {
            // A real `codex` window of this user with such an option is running: not testable now.
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let quiet = StandIn::start(&codex_home, &["-c", "web_search=\"disabled\""]);
        assert!(quiet.is_some());
        assert_eq!(shared_server_fault(server.pid()), None);
        let window = StandIn::start(&codex_home, &["--search"]);
        let window_pid = window.as_ref().map(StandIn::pid).unwrap_or_default();
        let fault = shared_server_fault(server.pid());
        assert!(
            matches!(&fault, Some(CodexFault::SharedServer(by)) if by.contains("--search") && by.contains(&window_pid.to_string())),
            "{fault:?}"
        );
        assert!(fault.is_some_and(|f| {
            f.text()
                .contains("every session in that server is UNGUARDED")
        }));
        // A session that is not in the server (no `app-server` on its command line) is not made so.
        let own = StandIn::start(&codex_home, &[]);
        assert_eq!(
            own.as_ref().and_then(|s| shared_server_fault(s.pid())),
            None
        );
        drop(window);
        assert_eq!(shared_server_fault(server.pid()), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // §13::77, §13::78, §4::42 (H1.4): a thread with a session record in
    // `<CODEX_HOME>/sessions` that no guard hook recorded is listed, once, whether its record is
    // plain or compressed; one a hook recorded, and files that are not session records, are not.
    #[test]
    fn a_thread_with_a_record_and_no_hook_is_listed() {
        let root = crate::test_target()
            .join("codex-threads-unit")
            .join(std::process::id().to_string());
        let day = root.join("sessions/2026/10/27");
        let _ = std::fs::create_dir_all(&day);
        let (seen, plain, packed) = (
            "11111111-2222-3333-4444-555555555555",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "01234567-89ab-cdef-0123-456789abcdef",
        );
        for name in [
            format!("rollout-2026-10-27T00-00-00-{seen}.jsonl"),
            format!("rollout-2026-10-27T00-00-01-{plain}.jsonl"),
            format!("rollout-2026-10-26T00-00-01-{packed}.jsonl.zst"),
            format!("rollout-2026-10-27T00-00-02-{plain}.jsonl"),
            "rollout-2026-10-27T00-00-03-not-a-uuid.jsonl".to_owned(),
            format!("notes-{plain}.jsonl"),
            format!("rollout-2026-10-27T00-00-04-{plain}.txt"),
        ] {
            let _ = std::fs::write(day.join(name), "{}\n");
        }
        let mut mail = Mail::default();
        // A hook records a session by the process it ran in.
        mail.observe_process("1:1", &root, &Identity::new(Harness::Codex, seen));
        let listed = unrecorded_codex_threads(&mail, &root);
        let ids: Vec<&str> = listed.iter().map(|t| t.thread.as_str()).collect();
        assert_eq!(ids, [packed, plain]);
        assert!(
            listed[0]
                .file
                .ends_with(format!("rollout-2026-10-26T00-00-01-{packed}.jsonl.zst"))
        );
        // Another harness's session of that name is not a Codex thread's record.
        mail.observe_process("2:2", &root, &Identity::new(Harness::ClaudeCode, plain));
        assert_eq!(unrecorded_codex_threads(&mail, &root).len(), 2);
        mail.observe_process("3:3", &root, &Identity::new(Harness::Codex, plain));
        assert_eq!(unrecorded_codex_threads(&mail, &root).len(), 1);
        assert_eq!(unrecorded_codex_threads(&mail, &root.join("none")), []);
        assert_eq!(CODEX_NO_HOOK_RAN, "no guard hook has run in it");
        let _ = std::fs::remove_dir_all(&root);
    }

    // §13::67, §4::38-§4::41 (H1.4, R3.4): the reasons the user reads name the option, the
    // file or the window.
    #[test]
    fn the_reasons_for_the_new_faults_name_what_to_change() {
        let text = |f: CodexFault| f.text();
        assert!(text(CodexFault::WebNotOff).contains("web_search is \"disabled\""));
        assert!(text(CodexFault::WebNotOff).contains("lunchroom install codex"));
        assert!(text(CodexFault::WebTurnedOn).contains("web_search"));
        assert!(
            text(CodexFault::CommandWeb("--search".to_owned())).contains("started with --search")
        );
        assert!(
            text(CodexFault::CommandTty("enable unified_exec_tty".to_owned()))
                .contains("unified_exec_tty")
        );
        let both = text(CodexFault::Recorded {
            tty: "config.toml".to_owned(),
            web: "--search".to_owned(),
        });
        assert!(both.contains("unified_exec_tty on by config.toml and web search on by --search"));
        assert!(both.contains("UNGUARDED until you start it anew"));
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
            // An earlier install's `guard pretool` is read, to be told apart from none.
            assert_eq!(
                guard_program(
                    &hook(command),
                    mode,
                    PreTool::Older,
                    Some(Path::new("/h/s.json"))
                ),
                Some(program),
                "{command}"
            );
            // Read from no file (settings given as text), no older install is told from none.
            assert_eq!(
                guard_program(&hook(command), mode, PreTool::Older, None).is_some(),
                mode != "pretool",
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
            assert_eq!(
                guard_program(
                    &hook(command),
                    mode,
                    PreTool::Older,
                    Some(Path::new("/h/s.json"))
                ),
                None,
                "{command}"
            );
        }
        assert_eq!(
            guard_program(&json!({}), "pretool", PreTool::Older, None),
            None
        );
        assert_eq!(
            guard_program(&json!({"command": 7}), "pretool", PreTool::Older, None),
            None
        );
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
                let quoted = format!("'{}'", self.program);
                let mut hook = json!({
                    "type": "command", "command": written(&quoted, &format!("guard {mode}"), Path::new(HERE))
                });
                // H1.5: Claude Code's tool-call hook also carries `onFailure: block`.
                if *mode == "pretool" {
                    hook["onFailure"] = json!("block");
                }
                let mut group = json!({"hooks": [hook]});
                if let Some(m) = matcher {
                    group["matcher"] = json!(m);
                }
                hooks.insert((*event).to_owned(), json!([group]));
            }
            json!({"hooks": hooks})
        }

        fn write(&self, relative: &str, value: &Value) {
            // The tool-call hook's line names the file it is in.
            let file = self.root.join(relative);
            let text = value.to_string().replace(HERE, &file.display().to_string());
            let _ = std::fs::write(file, text);
        }

        /// `configured_with` for a process given `args`, against this folder's settings.
        fn guarded(&self, args: &[&str]) -> bool {
            self.guarded_as(args, PreTool::Claude)
        }

        /// As [`Self::guarded`], taking the tool-call hook in the form `form`.
        fn guarded_as(&self, args: &[&str], form: PreTool) -> bool {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            configured_with(
                &args,
                &self.root.join("config"),
                &self.root.join("managed.json"),
                std::process::id(),
                &self.root.join("project"),
                form,
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

    // §13::99-102, §13::103 (H1.5), Fable M11: after an upgrade, a tool-call hook of the
    // 1.0.1 form (`guard pretool` alone, commit 276b58f install.rs line 346), a bare `|| exit 2`
    // (no release wrote it; Codex counts an exit 2 with nothing on standard error as a failed hook
    // that lets the call run), a failure branch altered by the user, and, in Claude Code, the
    // written branch without `"onFailure": "block"`, do not make a session guarded; the reason
    // names the reinstall only for the 1.0.1 form and the written branch without `onFailure`.
    #[test]
    fn claude_hooks_of_the_old_form_are_not_the_guard_and_name_the_reinstall() {
        let c = Claude::new("oldform");
        let program = format!("'{}'", c.program);
        let current = written(&program, "guard pretool", Path::new(HERE));
        let altered = current.replace("exit 2; }", "exit 0; }");
        for (what, command, on_failure) in [
            ("1.0.x form", format!("{program} guard pretool"), true),
            (
                "bare exit 2",
                format!("{program} guard pretool || exit 2"),
                true,
            ),
            ("branch without onFailure", current.clone(), false),
            ("altered branch", altered, true),
            ("line endings", format!("{current}\r\n"), true),
            (
                "a line break in the branch",
                current.replace("; exit 2", ";\nexit 2"),
                true,
            ),
        ] {
            let mut settings = c.settings(&FULL, None);
            let hook = &mut settings["hooks"]["PreToolUse"][0]["hooks"][0];
            hook["command"] = json!(command);
            if !on_failure && let Some(h) = hook.as_object_mut() {
                h.remove("onFailure");
            }
            c.write("config/settings.json", &settings);
            assert!(!c.guarded(&[]), "{what}");
            let older = c.guarded_as(&[], PreTool::Older);
            // Only the hook as the install writes it (here without `onFailure`) and the forms an
            // earlier install wrote are told from none: an altered or whitespace-altered line is
            // no Lunchroom form, so it is not an invitation to reinstall.
            assert_eq!(
                older,
                !matches!(
                    what,
                    "altered branch"
                        | "line endings"
                        | "a line break in the branch"
                        | "bare exit 2"
                ),
                "{what}"
            );
            let fault = claude_setup_fault(
                &c.root.join("config"),
                &c.root.join("managed.json"),
                std::process::id(),
                &c.root.join("project"),
            )
            .unwrap_or_default();
            assert!(fault.contains("Reinstall") == older, "{what}: {fault}");
            assert!(
                fault.contains("lunchroom install claude-code"),
                "{what}: {fault}"
            );
        }
        // The hook as the install writes it is the guard.
        c.write("config/settings.json", &c.settings(&FULL, None));
        assert!(c.guarded(&[]));
        assert_eq!(
            claude_setup_fault(
                &c.root.join("config"),
                &c.root.join("managed.json"),
                std::process::id(),
                &c.root.join("project"),
            ),
            None
        );
    }

    // H1.5 failure classes of where the hook stands: an old-form tool-call hook in one settings
    // file does not stand for the written one in another, and two Lunchroom tool-call hooks (an
    // old and a written one) are guarded by the written one alone; the old one by itself is not.
    #[test]
    fn the_tool_call_hook_is_judged_across_settings_levels_and_duplicates() {
        let c = Claude::new("levels");
        let program = format!("'{}'", c.program);
        let old = json!({"hooks": {"PreToolUse": [{"hooks": [
            {"type": "command", "command": format!("{program} guard pretool")}
        ]}]}});
        let rest = &FULL[1..];
        // Old in the user's file, the rest of the set in the project's: not guarded.
        c.write("config/settings.json", &old);
        c.write("project/.claude/settings.json", &c.settings(rest, None));
        assert!(!c.guarded(&[]), "old form only");
        assert!(
            c.guarded_as(&[], PreTool::Older),
            "old form is told from none"
        );
        // The written hook in a third file (the project's local settings) makes it guarded, the
        // old hook in the user's file notwithstanding.
        let pre = c.settings(&FULL[..1], None);
        c.write("project/.claude/settings.local.json", &pre);
        assert!(c.guarded(&[]), "written hook in another settings file");
        c.clear();
        // Two Lunchroom tool-call hooks in one file: an old one before the written one.
        let mut both = c.settings(&FULL, None);
        let written_group = both["hooks"]["PreToolUse"][0].clone();
        both["hooks"]["PreToolUse"] = json!([old["hooks"]["PreToolUse"][0].clone(), written_group]);
        c.write("config/settings.json", &both);
        assert!(c.guarded(&[]), "an old hook and a written one");
        // Two old ones are still no guard.
        let mut twice = c.settings(&FULL, None);
        twice["hooks"]["PreToolUse"] = json!([
            old["hooks"]["PreToolUse"][0].clone(),
            old["hooks"]["PreToolUse"][0].clone()
        ]);
        c.write("config/settings.json", &twice);
        assert!(!c.guarded(&[]), "two old hooks");
    }

    /// A tool-call hook of Claude Code (with `onFailure`) running `command`.
    fn claude_pretool(command: &str) -> Value {
        json!({"type": "command", "command": command, "onFailure": "block"})
    }

    //H1.5: the tool-call hook counts only when it is the install's line byte for byte.
    // A leading, trailing, repeated, tab, line-break or Unicode space (a no-break space looks like
    // a space and the shell does not split at it) is not it; the hook is still read as Lunchroom's,
    // so an install replaces it.
    #[test]
    fn the_tool_call_hook_must_be_the_installs_line_byte_for_byte() {
        let file = Path::new("/h/.claude/settings.json");
        let program = "/h/.lunchroom/bin/lunchroom";
        let current = written(program, "guard pretool", file);
        let check = |command: &str| {
            guard_program(
                &claude_pretool(command),
                "pretool",
                PreTool::Claude,
                Some(file),
            )
            .is_some()
        };
        assert!(check(&current));
        for (what, command) in [
            ("trailing space", format!("{current} ")),
            ("trailing tab", format!("{current}\t")),
            ("trailing no-break space", format!("{current}\u{a0}")),
            ("trailing line break", format!("{current}\n")),
            ("trailing carriage return", format!("{current}\r")),
            (
                "two spaces after the program",
                current.replacen(" guard", "  guard", 1),
            ),
            (
                "a tab after the program",
                current.replacen(" guard", "\tguard", 1),
            ),
            (
                "a no-break space after the program",
                current.replacen(" guard", "\u{a0}guard", 1),
            ),
            (
                "two spaces before pretool",
                current.replacen("guard pretool", "guard  pretool", 1),
            ),
            (
                "two spaces before the branch",
                current.replacen(" || {", "  || {", 1),
            ),
            ("a tab in the branch", current.replacen("{ [", "{\t[", 1)),
            (
                "a no-break space in the branch",
                current.replacen("-eq 2", "-eq\u{a0}2", 1),
            ),
            (
                "two spaces in the line",
                current.replacen("the guard", "the  guard", 1),
            ),
            (
                "a line break before the exit",
                current.replacen(">&2; exit", ">&2;\nexit", 1),
            ),
            ("a leading space", format!(" {current}")),
        ] {
            assert!(!check(&command), "{what}: {command:?}");
        }
    }

    //H1.5: the file the line names is the file the hook was read from, not just any
    // name. A wrong but non-empty name, a name that differs in one character, and a hook read from no
    // file (settings given as text) are not the install's hook; a spelling of the same file is.
    #[test]
    fn the_tool_call_hooks_line_must_name_the_file_it_was_read_from() {
        let root = crate::test_target()
            .join("hook-name-unit")
            .join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(root.join("a"));
        let file = root.join("a/settings.json");
        let other = root.join("a/settings.json.other");
        let _ = std::fs::write(&file, "{}");
        let program = "/h/.lunchroom/bin/lunchroom";
        let check = |named: &Path, read_from: Option<&Path>| {
            let command = written(program, "guard pretool", named);
            guard_program(
                &claude_pretool(&command),
                "pretool",
                PreTool::Claude,
                read_from,
            )
            .is_some()
        };
        assert!(check(&file, Some(&file)));
        assert!(!check(&other, Some(&file)), "another file");
        assert!(
            !check(Path::new("/x/y/settings.json"), Some(&file)),
            "a name that is not it"
        );
        assert!(
            !check(&root.join("a/settings.jsoN"), Some(&file)),
            "one character"
        );
        assert!(!check(&file, None), "read from no file");
        assert!(
            !check(&file.with_file_name("x y"), Some(&file)),
            "a name with a space"
        );
        // The same file by another spelling does not count: the name is the path the install
        // writes, byte for byte. A link, a relative path, `..`, `.` and a doubled slash are each
        // another spelling (and each would be a file-system question the check does not ask).
        let link = root.join("link");
        let _ = std::os::unix::fs::symlink(root.join("a"), &link);
        assert!(
            link.join("settings.json").exists(),
            "the link reaches the file"
        );
        assert!(
            !check(&link.join("settings.json"), Some(&file)),
            "a symlink alias"
        );
        assert!(
            !check(Path::new("a/settings.json"), Some(&file)),
            "a relative alias"
        );
        assert!(
            !check(Path::new("../a/settings.json"), Some(&file)),
            "a relative alias with .."
        );
        for alias in [
            format!("{}/a/../a/settings.json", root.display()),
            format!("{}/a/./settings.json", root.display()),
            format!("{}//a/settings.json", root.display()),
            format!("{}/a//settings.json", root.display()),
            format!("{}/a/settings.json/", root.display()),
        ] {
            assert!(!check(Path::new(&alias), Some(&file)), "{alias}");
        }
        // A quote in the name is escaped as the install writes it.
        let quoted = root.join("it's/settings.json");
        let _ = std::fs::create_dir_all(root.join("it's"));
        let _ = std::fs::write(&quoted, "{}");
        assert!(check(&quoted, Some(&quoted)), "a quote in the name");
        assert!(!check(&quoted, Some(&file)), "a quote, another file");
        let _ = std::fs::remove_dir_all(&root);
    }

    //H1.5 (failure class: an empty or nameless failure line): evidence takes the
    // tool-call hook as the guard only with the install's line, which names the file. An empty line,
    // a line with no file name, a line of other text, and the line with another file's name are not
    // it, for Claude Code and for Codex.
    #[test]
    fn an_empty_or_nameless_failure_line_is_not_the_guard() {
        let file = Path::new("/h/.claude/settings.json");
        let program = "/h/.lunchroom/bin/lunchroom";
        let current = written(program, "guard pretool", file);
        let line = crate::install::guard_failed_line(file);
        assert!(current.contains(&line));
        let counts = |command: &str| {
            let hook = claude_pretool(command);
            [PreTool::Claude, PreTool::Codex]
                .map(|form| guard_program(&hook, "pretool", form, Some(file)).is_some())
        };
        assert_eq!(counts(&current), [true, true]);
        for (what, command) in [
            ("an empty line", current.replacen(&line, "", 1)),
            ("a line of one space", current.replacen(&line, " ", 1)),
            (
                "a line with no file name",
                current.replacen(&file.display().to_string(), "", 1),
            ),
            (
                "a line of other text",
                current.replacen(&line, "the guard is fine", 1),
            ),
            (
                "another file's name",
                current.replacen(&file.display().to_string(), "/h/.claude/other.json", 1),
            ),
        ] {
            assert_eq!(counts(&command), [false, false], "{what}: {command}");
        }
    }

    //H1.5 (failure class: a configuration file path that is not UTF-8): the line names
    // the file in text, so such a path has no spelling in it. The hook the install would write
    // (with the lossy name) is not accepted as naming the file, and the install refuses the path
    // before changing anything; a UTF-8 path with an odd name is accepted when named exactly.
    #[test]
    fn a_configuration_path_that_is_not_utf8_is_never_named_by_a_line() {
        use std::os::unix::ffi::OsStringExt as _;
        let program = "/h/.lunchroom/bin/lunchroom";
        let bytes = b"/tmp/not-text-\xff\xfe/settings.json".to_vec();
        let file = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        assert!(file.to_str().is_none());
        // The install would have written the lossy spelling.
        let command = written(program, "guard pretool", &file);
        assert!(command.contains('\u{fffd}'));
        let hook = claude_pretool(&command);
        for form in [PreTool::Claude, PreTool::Codex] {
            assert!(
                guard_program(&hook, "pretool", form, Some(&file)).is_none(),
                "{form:?}"
            );
        }
        // The lossy name of a real UTF-8 file is not taken for the non-UTF-8 one either way round.
        let lossy = PathBuf::from(file.to_string_lossy().into_owned());
        assert!(guard_program(&hook, "pretool", PreTool::Claude, Some(&lossy)).is_some());
        // A UTF-8 name that holds characters a shell must quote is named exactly.
        let odd = Path::new("/h/it's a \u{e9} dir/settings.json");
        let command = written(program, "guard pretool", odd);
        assert!(
            guard_program(
                &claude_pretool(&command),
                "pretool",
                PreTool::Claude,
                Some(odd)
            )
            .is_some()
        );
    }

    //H1.5: Claude Code settings given as text (`--settings '<json>'`, or `--settings=`)
    // are read from no file, so the line cannot name the file they were read from: they are not the
    // install's hook, and not an older install's either (there is nothing to reinstall: the
    // diagnosis is "no guard"). The same text is the current form, an older form, and a bare
    // `|| exit 2`.
    #[test]
    fn settings_given_as_text_are_no_guard_and_no_older_install() {
        let c = Claude::new("inline");
        let user = c.root.join("config/settings.json");
        let texts = [
            claude_text(&c, &user, true),
            claude_text(&c, &user, false),
            claude_text(&c, &user, false).replace(" guard pretool", " guard pretool || exit 2"),
        ];
        let nothing = |_: &Path| None;
        for text in &texts {
            for args in [
                vec!["claude".to_owned(), "--settings".to_owned(), text.clone()],
                vec!["claude".to_owned(), format!("--settings={text}")],
            ] {
                let got = claude_guard_with(
                    &nothing,
                    &|hook| program_present(hook, std::process::id(), &c.root),
                    &args,
                    &c.root.join("config"),
                    &c.root.join("managed.json"),
                    &c.root.join("project"),
                );
                assert_eq!(got, ClaudeGuard::NotConfigured, "{text}");
            }
        }
        // The same text read from the user's file is guarded, older, or older (bare).
        for (text, expected) in [
            (&texts[0], ClaudeGuard::Guarded),
            (&texts[1], ClaudeGuard::OlderHook),
        ] {
            let reader = |path: &Path| (path == user).then(|| text.clone());
            let got = claude_guard_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            );
            assert_eq!(got, expected);
        }
    }

    //H1.5: a hook that looks like the install's line but names another file, an alias
    // of this one (`..`, `.`, a doubled slash, a relative path), or is altered (Windows line ending,
    // a line break, a trailing space), is neither the guard nor an older install's: for Claude
    // Code the settings are not configured, and for Codex the hooks file holds no guard set. The
    // exact line is the control.
    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, each look-alike judged for both harnesses
    fn a_fail_closed_looking_hook_that_is_not_the_installs_is_no_guard_and_no_older_install() {
        use std::fmt::Write as _;
        let c = Claude::new("lookalike");
        let user = c.root.join("config/settings.json");
        let program = format!("'{}'", c.program);
        let root = c.root.display().to_string();
        let current = written(&program, "guard pretool", &user);
        let variants = [
            (
                "another file",
                written(&program, "guard pretool", &c.root.join("config/other.json")),
            ),
            (
                "a .. alias",
                written(
                    &program,
                    "guard pretool",
                    Path::new(&format!("{root}/config/../config/settings.json")),
                ),
            ),
            (
                "a . alias",
                written(
                    &program,
                    "guard pretool",
                    Path::new(&format!("{root}/config/./settings.json")),
                ),
            ),
            (
                "a doubled slash",
                written(
                    &program,
                    "guard pretool",
                    Path::new(&format!("{root}//config/settings.json")),
                ),
            ),
            (
                "a relative path",
                written(&program, "guard pretool", Path::new("config/settings.json")),
            ),
            ("a Windows line ending", format!("{current}\r\n")),
            ("a line break", current.replacen("; exit 2", ";\nexit 2", 1)),
            ("a trailing space", format!("{current} ")),
        ];
        let claude = |command: &str| {
            let mut settings = c.settings(&FULL, None);
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"] = json!(command);
            let text = settings.to_string();
            let reader = |path: &Path| (path == user).then(|| text.clone());
            claude_guard_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            )
        };
        assert_eq!(claude(&current), ClaudeGuard::Guarded);
        for (what, command) in &variants {
            assert_eq!(
                claude(command),
                ClaudeGuard::NotConfigured,
                "Claude Code, {what}"
            );
        }
        // Codex.
        let codex_home = c.root.join("home/.codex");
        let file = codex_home.join("hooks.json");
        let states = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let mut config =
            String::from("web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n");
        for (_, state, _) in states {
            let _ = writeln!(
                config,
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"",
                file.display()
            );
        }
        let program_bare = program.clone();
        let codex = |pretool: &str| {
            let mut hooks = serde_json::Map::new();
            for (event, _, rest) in states {
                let command = if rest == "guard pretool" {
                    pretool.to_owned()
                } else {
                    format!("{program_bare} {rest}")
                };
                hooks.insert(
                    event.to_owned(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                );
            }
            let text = json!({ "hooks": hooks }).to_string();
            let reader = |path: &Path| {
                if path == file {
                    Some(text.clone())
                } else if path == codex_home.join("config.toml") {
                    Some(config.clone())
                } else {
                    None
                }
            };
            codex_hooks_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &codex_home,
                &c.root,
            )
        };
        let codex_current = written(&program_bare, "guard pretool", &file);
        assert!(codex(&codex_current).is_ok());
        let alias = format!("{root}/home/.codex/../.codex/hooks.json");
        let codex_variants = [
            written(
                &program_bare,
                "guard pretool",
                &codex_home.join("other.json"),
            ),
            written(&program_bare, "guard pretool", Path::new(&alias)),
            format!("{codex_current}\r\n"),
            codex_current.replacen("; exit 2", ";\nexit 2", 1),
            format!("{codex_current} "),
        ];
        for command in &codex_variants {
            assert_eq!(
                codex(command),
                Err(CodexFault::NoGuardSet(file.clone())),
                "{command:?}"
            );
        }
    }

    //H1.5: one spelling is an older install, byte for byte: `<program> guard pretool`,
    // the only form a released installer wrote (the 1.0.1 release, commit 276b58f,
    // `rust/crates/lunchroom/src/install.rs` line 346, `"command": format!("{p} guard {mode}")`).
    // A bare `guard pretool || exit 2` was never written by a release: it is no older install, and
    // not Lunchroom's hook. Any other spelling, with a space, tab, vertical tab, form feed, next line,
    // no-break, ogham, en, em or ideographic space, line separator, line break or carriage return,
    // in front, behind, between the words or around `||`, is no older install.
    #[test]
    fn only_the_one_released_spelling_is_an_older_install() {
        let file = Path::new("/h/.claude/settings.json");
        let program = "/h/.lunchroom/bin/lunchroom";
        let older = |command: &str| {
            guard_program(
                &json!({"command": command}),
                "pretool",
                PreTool::Older,
                Some(file),
            )
            .is_some()
        };
        assert!(older(&format!("{program} guard pretool")));
        assert!(older("'/h/my home/lunchroom' guard pretool"));
        // The bare form no release wrote is none.
        assert!(!older(&format!("{program} guard pretool || exit 2")));
        assert!(!older("'/h/my home/lunchroom' guard pretool || exit 2"));
        let spaces = [
            " ", "  ", "\t", "\u{b}", "\u{c}", "\r", "\n", "\r\n", "\u{85}", "\u{a0}", "\u{1680}",
            "\u{2003}", "\u{2028}", "\u{2029}", "\u{202f}", "\u{205f}", "\u{3000}",
        ];
        let mut altered = Vec::new();
        for ws in spaces {
            for exact in ["guard pretool", "guard pretool || exit 2"] {
                // Leading, trailing, between the program and `guard`, before `pretool`.
                altered.push(format!("{ws}{program} {exact}"));
                altered.push(format!("{program} {exact}{ws}"));
                altered.push(format!("{program} {ws}{exact}"));
                altered.push(format!(
                    "{program} {}",
                    exact.replacen("guard pretool", &format!("guard{ws}pretool"), 1)
                ));
            }
            // Around `||`, and inside `exit 2`.
            altered.push(format!("{program} guard pretool{ws}|| exit 2"));
            altered.push(format!("{program} guard pretool ||{ws}exit 2"));
            altered.push(format!("{program} guard pretool || exit{ws}2"));
            altered.push(format!("{program} guard pretool{ws}||{ws}exit{ws}2"));
        }
        altered.push(format!("{program}\u{2003}guard pretool"));
        altered.push(format!("{program} guard pretool\u{2003}|| exit 2"));
        let spelled = [format!("{program} guard pretool")];
        for command in altered.iter().filter(|c| !spelled.contains(c)) {
            assert!(!older(command), "{command:?}");
        }
        // Through the whole reading: an altered spelling in the user's file is no guard and no
        // invitation to reinstall; the exact spelling is the invitation.
        let c = Claude::new("legacy");
        let user = c.root.join("config/settings.json");
        let verdict = |command: &str| {
            let mut settings = c.settings(&FULL, None);
            let hook = &mut settings["hooks"]["PreToolUse"][0]["hooks"][0];
            hook["command"] = json!(command);
            if let Some(h) = hook.as_object_mut() {
                h.remove("onFailure");
            }
            let text = settings.to_string();
            let reader = |path: &Path| (path == user).then(|| text.clone());
            claude_guard_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            )
        };
        let quoted = format!("'{}'", c.program);
        assert_eq!(
            verdict(&format!("{quoted} guard pretool")),
            ClaudeGuard::OlderHook
        );
        assert_eq!(
            verdict(&format!("{quoted} guard pretool || exit 2")),
            ClaudeGuard::NotConfigured
        );
        for ws in ["  ", "\t", "\u{a0}", "\u{2003}", "\r\n"] {
            for command in [
                format!("{quoted} guard{ws}pretool"),
                format!("{quoted} guard pretool{ws}|| exit 2"),
                format!("{quoted} guard pretool{ws}"),
            ] {
                assert_eq!(verdict(&command), ClaudeGuard::NotConfigured, "{command:?}");
            }
        }
    }

    //H1.5: a hook command over 4096 bytes never reaches the file system: the check that
    // the hook's program is there to run is not asked, in any form (the current one for Claude Code
    // and Codex, and an older one). An injected check records what it is asked: at 4096 bytes it is
    // asked once, at 4097 and above never.
    #[test]
    fn a_command_over_the_bound_never_reaches_the_file_system() {
        use std::cell::Cell;
        let file = Path::new("/h/.claude/settings.json");
        let program = "/h/.lunchroom/bin/lunchroom";
        let fixed = written(program, "guard pretool", Path::new("")).len();
        let calls = Cell::new(0usize);
        let present = |_: &Value| {
            calls.set(calls.get() + 1);
            true
        };
        let holds = |command: &str, form: PreTool| {
            let settings = json!({"hooks": {"PreToolUse": [{"hooks": [claude_pretool(command)]}]}});
            calls.set(0);
            let got = has_guard_hook(
                &settings,
                "PreToolUse",
                "pretool",
                &present,
                form,
                Some(file),
            );
            (got, calls.get())
        };
        for (over, expected) in [(0usize, true), (1, false), (500, false), (4096, false)] {
            let name = format!("/{}", "a".repeat(4096 - fixed - 1 + over));
            let long_file = PathBuf::from(&name);
            let command = written(program, "guard pretool", &long_file);
            assert_eq!(command.len(), 4096 + over);
            let settings =
                json!({"hooks": {"PreToolUse": [{"hooks": [claude_pretool(&command)]}]}});
            for form in [PreTool::Claude, PreTool::Codex, PreTool::Older] {
                calls.set(0);
                let got = has_guard_hook(
                    &settings,
                    "PreToolUse",
                    "pretool",
                    &present,
                    form,
                    Some(&long_file),
                );
                assert_eq!(
                    (got, calls.get()),
                    (expected, usize::from(expected)),
                    "{form:?} at {} bytes",
                    command.len()
                );
            }
        }
        // An older form past the bound (a long program path) is not asked about either.
        for (pad, expected) in [
            (4096 - "/lunchroom guard pretool".len(), true),
            (4097 - "/lunchroom guard pretool".len(), false),
        ] {
            let command = format!("/{}/lunchroom guard pretool", "p".repeat(pad - 1));
            assert_eq!(command.len(), 4096 + usize::from(!expected));
            assert_eq!(
                holds(&command, PreTool::Older),
                (expected, usize::from(expected)),
                "{} bytes",
                command.len()
            );
        }
    }

    //H1.5: a hook command of more than 4096 bytes is not the install's line, and no call
    // is made on the file system for it (the file it names does not exist, and is not asked about).
    #[test]
    fn a_tool_call_hook_command_is_bounded_at_4096_bytes() {
        let program = "/h/.lunchroom/bin/lunchroom";
        // The command's length is this much more than the length of the file's name.
        let fixed = written(program, "guard pretool", Path::new("")).len();
        for (over, expected) in [(0usize, true), (1, false), (4096, false)] {
            let name = format!("/{}", "a".repeat(4096 - fixed - 1 + over));
            let file = PathBuf::from(&name);
            let command = written(program, "guard pretool", &file);
            assert_eq!(command.len(), 4096 + over);
            assert!(!file.exists());
            let hook = claude_pretool(&command);
            let counts =
                |form: PreTool| guard_program(&hook, "pretool", form, Some(&file)).is_some();
            assert_eq!(counts(PreTool::Claude), expected, "{} bytes", command.len());
            assert_eq!(counts(PreTool::Codex), expected, "{} bytes", command.len());
            // Past the bound it is Lunchroom's line in no form, not even an older one.
            assert_eq!(counts(PreTool::Older), expected, "{} bytes", command.len());
        }
    }

    //H1.5: Claude Code's tool-call hook has `"onFailure": "block"`, the string and no
    // other value: not `Block`, a boolean, null, a number, another string, a string with a space, an
    // object, a list, or nothing.
    #[test]
    fn only_the_string_block_is_the_on_failure_that_blocks() {
        let file = Path::new("/h/.claude/settings.json");
        let command = written("/h/.lunchroom/bin/lunchroom", "guard pretool", file);
        let with = |value: Option<Value>| {
            let mut hook = json!({"type": "command", "command": command});
            if let Some(v) = value {
                hook["onFailure"] = v;
            }
            hook
        };
        let claude =
            |hook: &Value| guard_program(hook, "pretool", PreTool::Claude, Some(file)).is_some();
        assert!(claude(&with(Some(json!("block")))));
        for (what, value) in [
            ("Block", Some(json!("Block"))),
            ("BLOCK", Some(json!("BLOCK"))),
            ("block with a space", Some(json!("block "))),
            ("true", Some(json!(true))),
            ("false", Some(json!(false))),
            ("null", Some(json!(null))),
            ("a number", Some(json!(1))),
            ("another string", Some(json!("warn"))),
            ("an empty string", Some(json!(""))),
            ("a list", Some(json!(["block"]))),
            ("an object", Some(json!({"block": true}))),
            ("nothing", None),
        ] {
            assert!(!claude(&with(value.clone())), "{what}");
            // Codex has no such option and ignores it: its tool-call hook needs the line only.
            assert!(
                guard_program(&with(value), "pretool", PreTool::Codex, Some(file)).is_some(),
                "{what} (Codex)"
            );
        }
    }

    /// The text of a settings file for the Claude fixture `c` holding the whole guard set, with the
    /// tool-call hook as the install writes it (`current`) or as 1.0.x did.
    fn claude_text(c: &Claude, file: &Path, current: bool) -> String {
        let mut settings = c.settings(&FULL, None);
        if !current {
            let hook = &mut settings["hooks"]["PreToolUse"][0]["hooks"][0];
            hook["command"] = json!(format!("'{}' guard pretool", c.program));
            if let Some(h) = hook.as_object_mut() {
                h.remove("onFailure");
            }
        }
        settings
            .to_string()
            .replace(HERE, &file.display().to_string())
    }

    //H1.5 (failure class: a `command` that is not a string): a hook whose `command` is
    // absent, null, a boolean, a number, an array, an object or an empty string is not Lunchroom's
    // hook, in every mode and form, for Claude Code and for Codex, and is never asked about on the
    // file system. It is not read as the empty string and it is not read through its JSON text: the
    // array and the object here hold the exact command of the install, as a string inside.
    #[test]
    #[allow(clippy::too_many_lines)] // one table of values, judged four ways
    fn a_command_that_is_not_a_string_is_not_lunchroom_s_hook() {
        use std::fmt::Write as _;
        let file = Path::new("/h/.claude/settings.json");
        let program = "/h/.lunchroom/bin/lunchroom";
        let exact = written(program, "guard pretool", file);
        let values: Vec<(&str, Option<Value>)> = vec![
            ("absent", None),
            ("null", Some(json!(null))),
            ("true", Some(json!(true))),
            ("a number", Some(json!(7))),
            ("an empty string", Some(json!(""))),
            ("an empty array", Some(json!([]))),
            ("an array with the command", Some(json!([exact.clone()]))),
            ("an empty object", Some(json!({}))),
            (
                "an object with the command",
                Some(json!({"command": exact.clone()})),
            ),
        ];
        let calls = std::cell::Cell::new(0usize);
        let present = |_: &Value| {
            calls.set(calls.get() + 1);
            true
        };
        let hook_of = |value: &Option<Value>| {
            let mut hook = json!({"type": "command", "onFailure": "block"});
            if let Some(v) = value {
                hook["command"] = v.clone();
            }
            hook
        };
        for (what, value) in &values {
            let hook = hook_of(value);
            for mode in ["pretool", "posttool", "prompt", "start"] {
                for form in [PreTool::Claude, PreTool::Codex, PreTool::Older] {
                    assert!(
                        guard_program(&hook, mode, form, Some(file)).is_none(),
                        "{what}, {mode}, {form:?}"
                    );
                }
            }
            let settings = json!({"hooks": {"PreToolUse": [{"hooks": [hook]}]}});
            calls.set(0);
            assert!(!has_guard_hook(
                &settings,
                "PreToolUse",
                "pretool",
                &present,
                PreTool::Claude,
                Some(file)
            ));
            assert_eq!(calls.get(), 0, "{what}");
        }
        // The whole reading: Claude Code settings and a Codex hooks file whose tool-call hook has
        // such a command are not configured and hold no guard set, and are not an older install.
        let c = Claude::new("nonstring");
        let user = c.root.join("config/settings.json");
        let quoted = format!("'{}'", c.program);
        let codex_home = c.root.join("home/.codex");
        let hooks_file = codex_home.join("hooks.json");
        let states = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let mut config =
            String::from("web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n");
        for (_, state, _) in states {
            let _ = write!(
                config,
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"\n",
                hooks_file.display()
            );
        }
        for (what, value) in &values {
            let mut settings: Value =
                serde_json::from_str(&claude_text(&c, &user, true)).unwrap_or_default();
            let slot = &mut settings["hooks"]["PreToolUse"][0]["hooks"][0];
            match value {
                Some(v) => slot["command"] = v.clone(),
                None => {
                    if let Some(h) = slot.as_object_mut() {
                        h.remove("command");
                    }
                }
            }
            let text = settings.to_string();
            let reader = |path: &Path| (path == user).then(|| text.clone());
            let got = claude_guard_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            );
            assert_eq!(got, ClaudeGuard::NotConfigured, "Claude Code, {what}");
            // Codex.
            let mut hooks = serde_json::Map::new();
            for (event, _, rest) in states {
                let mut hook = json!({"type": "command"});
                if rest == "guard pretool" {
                    if let Some(v) = value {
                        hook["command"] = v.clone();
                    }
                } else {
                    hook["command"] = json!(format!("{quoted} {rest}"));
                }
                hooks.insert(event.to_owned(), json!([{"hooks": [hook]}]));
            }
            let hooks_text = json!({ "hooks": hooks }).to_string();
            let reader = |path: &Path| {
                if path == hooks_file {
                    Some(hooks_text.clone())
                } else if path == codex_home.join("config.toml") {
                    Some(config.clone())
                } else {
                    None
                }
            };
            let got = codex_hooks_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &codex_home,
                &c.root,
            );
            assert_eq!(
                got,
                Err(CodexFault::NoGuardSet(hooks_file.clone())),
                "Codex, {what}"
            );
        }
    }

    //H1.5: every `onFailure` that is not the string `block` leaves Claude Code's tool-call
    // hook (the install's line in the user's file) out of the guard: the whole reading says reinstall
    // (`OlderHook`), the doctor's text names the reinstall and `onFailure`, and the session is not
    // guarded. The exact string is guarded and the doctor says nothing.
    #[test]
    fn a_wrong_on_failure_is_a_reinstall_in_the_whole_reading_and_the_doctors_words() {
        let c = Claude::new("onfailure");
        let user = c.root.join("config/settings.json");
        let reading = |value: Option<Value>| {
            let mut settings: Value =
                serde_json::from_str(&claude_text(&c, &user, true)).unwrap_or_default();
            let hook = &mut settings["hooks"]["PreToolUse"][0]["hooks"][0];
            match value {
                Some(v) => hook["onFailure"] = v,
                None => {
                    if let Some(h) = hook.as_object_mut() {
                        h.remove("onFailure");
                    }
                }
            }
            let text = settings.to_string();
            let reader = |path: &Path| (path == user).then(|| text.clone());
            let present = |hook: &Value| program_present(hook, std::process::id(), &c.root);
            let got = claude_guard_with(
                &reader,
                &present,
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            );
            let doctor = claude_setup_fault_with(
                &reader,
                &present,
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            );
            (got, doctor)
        };
        assert_eq!(reading(Some(json!("block"))), (ClaudeGuard::Guarded, None));
        for (what, value) in [
            ("Block", Some(json!("Block"))),
            ("BLOCK", Some(json!("BLOCK"))),
            ("block with a space", Some(json!("block "))),
            ("true", Some(json!(true))),
            ("false", Some(json!(false))),
            ("null", Some(json!(null))),
            ("a number", Some(json!(1))),
            ("another string", Some(json!("warn"))),
            ("an empty string", Some(json!(""))),
            ("a list", Some(json!(["block"]))),
            ("an object", Some(json!({"block": true}))),
            ("nothing", None),
        ] {
            let (got, doctor) = reading(value);
            assert_eq!(got, ClaudeGuard::OlderHook, "{what}");
            let text = doctor.unwrap_or_default();
            assert!(
                text.contains("Reinstall") && text.contains("onFailure"),
                "{what}: {text}"
            );
        }
    }

    //H1.5: Claude Code's settings are read once and both forms of the tool-call hook
    // are judged from that one reading. A reader that gives the current form the first time and the
    // 1.0.x form after (a file replaced between two readings) is judged by the first reading; so is
    // the other way round. Each file is read once.
    #[test]
    fn claude_settings_are_read_once_and_judged_from_that_reading() {
        use std::cell::RefCell;
        use std::collections::HashMap;
        let c = Claude::new("snapshot");
        let user = c.root.join("config/settings.json");
        let current = claude_text(&c, &user, true);
        let older = claude_text(&c, &user, false);
        for (first, second, expected) in [
            (&current, &older, ClaudeGuard::Guarded),
            (&older, &current, ClaudeGuard::OlderHook),
            (&current, &current, ClaudeGuard::Guarded),
            (&older, &older, ClaudeGuard::OlderHook),
        ] {
            let reads: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new());
            let reader = |path: &Path| {
                let mut reads = reads.borrow_mut();
                let n = reads.entry(path.to_path_buf()).or_insert(0);
                *n += 1;
                (path == user).then(|| {
                    if *n == 1 {
                        first.clone()
                    } else {
                        second.clone()
                    }
                })
            };
            let got = claude_guard_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &c.root),
                &[],
                &c.root.join("config"),
                &c.root.join("managed.json"),
                &c.root.join("project"),
            );
            assert_eq!(got, expected);
            assert!(
                reads.borrow().values().all(|n| *n == 1),
                "{:?}",
                reads.borrow()
            );
            assert_eq!(reads.borrow().get(&user), Some(&1));
        }
        // The doctor text is judged from one reading too.
        let reads: RefCell<usize> = RefCell::new(0);
        let reader = |path: &Path| {
            (path == user).then(|| {
                *reads.borrow_mut() += 1;
                if *reads.borrow() == 1 {
                    older.clone()
                } else {
                    current.clone()
                }
            })
        };
        let text = claude_setup_fault_with(
            &reader,
            &|hook| program_present(hook, std::process::id(), &c.root),
            &c.root.join("config"),
            &c.root.join("managed.json"),
            &c.root.join("project"),
        );
        assert!(text.is_some_and(|t| t.contains("Reinstall")));
        assert_eq!(*reads.borrow(), 1);
    }

    //H1.5: for Codex too, each hooks file and `config.toml` are read once, and both
    // forms of the tool-call hook are judged from that one reading.
    #[test]
    fn codex_hooks_are_read_once_and_judged_from_that_reading() {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::fmt::Write as _;
        let root = crate::test_target()
            .join("codex-snapshot-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let program = bin.join("lunchroom").display().to_string();
        let file = codex_home.join("hooks.json");
        let states = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let mut config =
            String::from("web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n");
        for (_, state, _) in states {
            let _ = writeln!(
                config,
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"",
                file.display()
            );
        }
        let set = |current: bool| {
            let mut hooks = serde_json::Map::new();
            for (event, _, rest) in states {
                let command = if current {
                    written(&program, rest, &file)
                } else {
                    format!("{program} {rest}")
                };
                hooks.insert(
                    event.to_owned(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                );
            }
            json!({ "hooks": hooks }).to_string()
        };
        let (current, older) = (set(true), set(false));
        for (first, second, guarded) in [(&current, &older, true), (&older, &current, false)] {
            let reads: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new());
            let reader = |path: &Path| {
                let mut reads = reads.borrow_mut();
                let n = reads.entry(path.to_path_buf()).or_insert(0);
                *n += 1;
                if path == file {
                    Some(if *n == 1 {
                        first.clone()
                    } else {
                        second.clone()
                    })
                } else if path == codex_home.join("config.toml") {
                    Some(config.clone())
                } else {
                    None
                }
            };
            let got = codex_hooks_with(
                &reader,
                &|hook| program_present(hook, std::process::id(), &root),
                &codex_home,
                &root,
            );
            assert_eq!(got.is_ok(), guarded, "{got:?}");
            if !guarded {
                assert_eq!(got, Err(CodexFault::OldToolHook(file.clone())));
            }
            assert!(
                reads.borrow().values().all(|n| *n == 1),
                "{:?}",
                reads.borrow()
            );
            assert_eq!(reads.borrow().get(&file), Some(&1));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    // H1.5 failure class "a hook in another settings file", for Codex: a project's
    // `.codex/hooks.json` is read beside `<CODEX_HOME>/hooks.json`. An old-form set in the one does
    // not stand for the written set in the other (the written, trusted one guards), and an old-form
    // set in the project's file alone is told apart from no guard, naming that file.
    #[test]
    fn a_codex_project_hooks_file_is_judged_on_its_own_form() {
        use std::fmt::Write as _;
        let root = crate::test_target()
            .join("codex-project-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let project = root.join("project");
        let _ = std::fs::create_dir_all(&codex_home);
        let _ = std::fs::create_dir_all(project.join(".codex"));
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let program = bin.join("lunchroom").display().to_string();
        let home_file = codex_home.join("hooks.json");
        let project_file = project.join(".codex/hooks.json");
        let states = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let mut config =
            String::from("web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n");
        for file in [&home_file, &project_file] {
            for (_, state, _) in states {
                let _ = writeln!(
                    config,
                    "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"",
                    file.display()
                );
            }
        }
        let _ = std::fs::write(codex_home.join("config.toml"), config);
        let set = |file: &Path, current: bool| {
            let mut hooks = serde_json::Map::new();
            for (event, _, rest) in states {
                let command = if current {
                    written(&program, rest, file)
                } else if rest == "guard pretool" {
                    format!("{program} guard pretool")
                } else {
                    format!("{program} {rest}")
                };
                hooks.insert(
                    event.to_owned(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                );
            }
            let _ = std::fs::write(file, json!({ "hooks": hooks }).to_string());
        };
        let judged = || codex_guard_in(&codex_home, std::process::id(), &project);
        // Old in the user's file, written in the project's: guarded.
        set(&home_file, false);
        set(&project_file, true);
        assert!(judged().is_ok(), "{:?}", judged());
        // Old in both: not guarded, and told apart from none.
        set(&project_file, false);
        assert_eq!(judged(), Err(CodexFault::OldToolHook(home_file.clone())));
        // Old in the project's file alone.
        let _ = std::fs::remove_file(&home_file);
        assert_eq!(judged(), Err(CodexFault::OldToolHook(project_file.clone())));
        // Written in the project's file alone: guarded.
        set(&project_file, true);
        assert!(judged().is_ok(), "{:?}", judged());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_tool_call_hook_of_the_old_form_is_unguarded_and_names_the_reinstall() {
        use std::fmt::Write as _;
        let root = crate::test_target()
            .join("codex-oldform-unit")
            .join(std::process::id().to_string());
        let codex_home = root.join("home/.codex");
        let _ = std::fs::create_dir_all(&codex_home);
        let bin = root.join("bin");
        let _ = std::fs::create_dir_all(&bin);
        let _ = std::fs::copy("/bin/true", bin.join("lunchroom"));
        let program = bin.join("lunchroom").display().to_string();
        let file = codex_home.join("hooks.json");
        let events = [
            ("PreToolUse", "pre_tool_use", "guard pretool"),
            ("PostToolUse", "post_tool_use", "guard posttool"),
            ("UserPromptSubmit", "user_prompt_submit", "guard prompt"),
            ("SessionStart", "session_start", "guard start"),
        ];
        let mut config =
            String::from("web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n");
        for (_, state, _) in events {
            let _ = writeln!(
                config,
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"",
                file.display()
            );
        }
        let _ = std::fs::write(codex_home.join("config.toml"), config);
        let fault = |pretool: &str| {
            let mut hooks = serde_json::Map::new();
            for (event, _, rest) in events {
                let command = if rest == "guard pretool" {
                    pretool.to_owned()
                } else {
                    format!("{program} {rest}")
                };
                hooks.insert(
                    event.to_owned(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                );
            }
            let _ = std::fs::write(&file, json!({ "hooks": hooks }).to_string());
            codex_guard_in(&codex_home, std::process::id(), &root)
        };
        let current = written(&program, "guard pretool", &file);
        for old in [
            format!("{program} guard pretool"),
            format!("{program} guard pretool || exit 2"),
            current.replace("printf", "echo"),
        ] {
            let got = fault(&old);
            let expected = old.ends_with("guard pretool");
            if expected {
                assert_eq!(got, Err(CodexFault::OldToolHook(file.clone())), "{old}");
                let text = CodexFault::OldToolHook(file.clone()).text();
                assert!(
                    text.contains("Reinstall") && text.contains("lunchroom install codex"),
                    "{text}"
                );
            } else {
                // A branch the install did not write is no Lunchroom hook form at all.
                assert_eq!(got, Err(CodexFault::NoGuardSet(file.clone())), "{old}");
            }
            assert_eq!(
                codex_fingerprint_in(&codex_home, std::process::id(), &root),
                None
            );
        }
        assert!(fault(&current).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

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
            unquoted["hooks"][event][0]["hooks"][0]["command"] = json!(written(
                &c.program,
                &format!("guard {mode}"),
                Path::new(HERE)
            ));
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
        // Settings given as text are read from no file, and the tool-call hook's line names the
        // file it was read from: text is not the install's hook (H1.5), whatever it holds.
        let text = full.to_string();
        assert!(!c.guarded(&["claude", "--settings", &text]));
        let joined = format!("--settings={text}");
        assert!(!c.guarded(&[joined.as_str()]));
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
        let file = codex_home.join("hooks.json");
        let hook = |mode: &str| json!([{"hooks": [{"type": "command", "command": written(&program, mode, &file)}]}]);
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
            let config = format!(
                "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n{config}"
            );
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
            // The web search setting is off in each: the fault under test is the tty's.
            let web = "web_search = \"disabled\"\n";
            let _ = std::fs::write(codex_home.join("config.toml"), format!("{web}{home}{all}"));
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
        let tty = "web_search = \"disabled\"\n[features]\nunified_exec_tty = false\n".to_owned();
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
                    "type": "command", "command": written(&program, args, &file),
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
            g["hooks"][0]["command"] = json!(written(&program, "guard pretool", &file));
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
