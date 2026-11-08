//! F1-AC34 for the Claude Code and Codex harnesses (docs/spec/1.1/F1/requirements.md, story 13):
//! when an agent becomes UNGUARDED, the reason given is the first one its evidence lacks, in the
//! order R3.4 lists them.
//!
//! The notice (`claude.rs`, which tells `why.text()`) and the line under the agent in `lunchroom
//! status` (`cli.rs`, which prints `why.text()` or `why.text_for_agent()` after `why: `) both print
//! the `Why` that `assess` returns, so these tests judge that `Why` and its two texts. Already
//! covered by `tests::each_unguarded_case_of_a_plugin_harness_gives_its_own_reason` (OpenClaw and
//! Hermes, the same order through `assess`) and
//! `tests::a_codex_configuration_fault_names_the_setting_or_hook` (each Codex fault of
//! `codex_guard_in`, not through `assess`).
//!
//! Each session is this test's own process, recorded as a guard hook would record it. A session
//! whose harness folder the process environment redirects (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`) or
//! that has managed Claude Code settings is not built, so a test never writes into a real
//! configuration, and its test then returns without asserting.

use serde_json::json;

use super::*;

/// A live session of `harness` in this process, with an empty home.
struct Live {
    root: PathBuf,
    home: PathBuf,
    who: Identity,
    mail: Mail,
}

impl Live {
    fn new(harness: Harness, name: &str) -> Option<Self> {
        let pid = std::process::id();
        let redirected = match harness {
            Harness::Codex => process_env(pid, "CODEX_HOME").is_some(),
            _ => {
                process_env(pid, "CLAUDE_CONFIG_DIR").is_some()
                    || Path::new(MANAGED_SETTINGS).exists()
            }
        };
        if redirected {
            return None;
        }
        let root = crate::test_target()
            .join("f1-ac34-unit")
            .join(format!("{pid}-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let _ = std::fs::create_dir_all(&home);
        let _ = std::fs::create_dir_all(root.join("bin"));
        let _ = std::fs::copy("/bin/true", root.join("bin/lunchroom"));
        let who = Identity::new(harness, "f1-ac34-session");
        let mut mail = Mail::default();
        let process = crate::process::key(pid).unwrap_or_default();
        mail.observe_process(&process, &root, &who);
        Some(Self {
            root,
            home,
            who,
            mail,
        })
    }

    /// The reason the session is UNGUARDED, or `None` when it is not.
    fn why(&self) -> Option<Why> {
        unguarded_reason(&self.mail, &self.who, &self.home)
    }

    fn hook(&self, mode: &str) -> serde_json::Value {
        let program = self.root.join("bin/lunchroom").display().to_string();
        json!([{"hooks": [{"type": "command", "command": format!("{program} guard {mode}")}]}])
    }

    /// Claude Code's user settings holding the whole guard.
    fn write_claude_guard(&self) {
        let dir = self.home.join(".claude");
        let _ = std::fs::create_dir_all(&dir);
        let settings = json!({"hooks": {
            "PreToolUse": self.hook("pretool"),
            "PostToolUse": self.hook("posttool"),
            "PostToolUseFailure": self.hook("posttool"),
            "UserPromptSubmit": self.hook("prompt"),
        }});
        let _ = std::fs::write(dir.join("settings.json"), settings.to_string());
    }

    fn codex_home(&self) -> PathBuf {
        self.home.join(".codex")
    }

    /// Codex's `config.toml` with terminals off, the setting a guarded session needs.
    fn write_codex_terminals_off(&self) {
        let _ = std::fs::create_dir_all(self.codex_home());
        let off = "[features]\nunified_exec_tty = false\n";
        let _ = std::fs::write(self.codex_home().join("config.toml"), off);
    }

    /// Codex's whole guard: terminals off, the four hooks, each trusted.
    fn write_codex_guard(&self) {
        self.write_codex_terminals_off();
        let file = self.codex_home().join("hooks.json");
        let hooks = json!({"hooks": {
            "PreToolUse": self.hook("pretool"),
            "PostToolUse": self.hook("posttool"),
            "UserPromptSubmit": self.hook("prompt"),
            "SessionStart": self.hook("start"),
        }});
        let _ = std::fs::write(&file, hooks.to_string());
        let trusted: Vec<String> = [
            "pre_tool_use",
            "post_tool_use",
            "user_prompt_submit",
            "session_start",
        ]
        .iter()
        .map(|state| {
            format!(
                "[hooks.state.\"{}:{state}:0:0\"]\ntrusted_hash = \"sha256:{state}\"\n",
                file.display()
            )
        })
        .collect();
        let trusted = trusted.concat();
        let config = format!("[features]\nunified_exec_tty = false\n{trusted}");
        let _ = std::fs::write(self.codex_home().join("config.toml"), config);
    }

    /// A guard hook ran in the session with the configuration now in place.
    fn record_codex_guard_ran(&mut self) {
        let fingerprint = codex_guard_in(&self.codex_home(), std::process::id(), &self.root).ok();
        self.mail.record_guard_fingerprint(&self.who, fingerprint);
    }

    /// The install set `unified_exec_tty` after this session was open.
    fn codex_setting_set_after_it_opened(&mut self) {
        let boot = process::boot_id().unwrap_or_default();
        self.mail.codex_setting_changed(&boot, u64::MAX);
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// F1-AC34 (Claude Code): a session whose settings do not run the whole guard is UNGUARDED for
// that, naming the harness and the install command; its release is unreadable too, and is not
// the reason given (see the next test). For the agent the reason is the short form.
#[test]
fn f1_ac34_claude_code_without_the_whole_guard_gets_the_configuration_reason() {
    let Some(s) = Live::new(Harness::ClaudeCode, "claude-config") else {
        return;
    };
    let why = s.why();
    assert_eq!(
        why,
        Some(Why::Configuration(CLAUDE_NOT_CONFIGURED.to_owned()))
    );
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(
        text.starts_with("Claude Code's settings")
            && text.contains("lunchroom install claude-code"),
        "{text}"
    );
    let agent = why.as_ref().map(Why::text_for_agent).unwrap_or_default();
    assert!(
        agent.contains("does not run Lunchroom's whole guard") && !agent.contains('/'),
        "{agent}"
    );
}

// F1-AC34 (Codex): the setting at fault comes first. With terminals not off and the session also
// open before the setting was made, the reason is the setting (naming `unified_exec_tty` and the
// install command), not the opened-before one, nor the missing hooks.
#[test]
fn f1_ac34_codex_with_terminals_not_off_gets_the_setting_reason_first() {
    let Some(mut s) = Live::new(Harness::Codex, "codex-setting") else {
        return;
    };
    s.codex_setting_set_after_it_opened();
    let why = s.why();
    assert_eq!(why, Some(Why::Configuration(CodexFault::TtyNotOff.text())));
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(
        text.contains("unified_exec_tty") && text.contains("lunchroom install codex"),
        "{text}"
    );
    assert_ne!(text, CODEX_OPENED_BEFORE);
}

// F1-AC34 (Codex): with terminals off, a session that was open before the setting was made is
// UNGUARDED for that, before any hook fault: it says to start a new session.
#[test]
fn f1_ac34_codex_open_before_the_setting_gets_the_opened_before_reason() {
    let Some(mut s) = Live::new(Harness::Codex, "codex-opened-before") else {
        return;
    };
    s.write_codex_terminals_off();
    s.codex_setting_set_after_it_opened();
    let why = s.why();
    assert_eq!(
        why,
        Some(Why::Configuration(CODEX_OPENED_BEFORE.to_owned()))
    );
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(text.contains("start a new session"), "{text}");
}

// F1-AC34 (Codex): with terminals off and the session not open before, a hooks file without the
// whole guard is the reason, naming the file, the hooks and where the user trusts them.
#[test]
fn f1_ac34_codex_without_the_guard_hooks_names_the_hooks_file() {
    let Some(s) = Live::new(Harness::Codex, "codex-hooks") else {
        return;
    };
    s.write_codex_terminals_off();
    let file = s.codex_home().join("hooks.json");
    let why = s.why();
    assert_eq!(
        why,
        Some(Why::Configuration(
            CodexFault::NoGuardSet(file.clone()).text()
        ))
    );
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(
        text.contains(&file.display().to_string())
            && text.contains("PreToolUse")
            && text.contains("/hooks"),
        "{text}"
    );
}

// F1-AC34 (Codex): with the whole guard configured and trusted but no guard hook run in the
// session since, the reason is that none has run, and it says to type a prompt; the agent's form
// says the same without a path.
#[test]
fn f1_ac34_codex_configured_but_no_hook_run_gets_the_no_guard_ran_reason() {
    let Some(s) = Live::new(Harness::Codex, "codex-no-hook") else {
        return;
    };
    s.write_codex_guard();
    let why = s.why();
    assert_eq!(why, Some(Why::NoGuardRan(Harness::Codex)));
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(
        text.contains("No Lunchroom guard hook has run in this Codex session")
            && text.contains("type a prompt in the session"),
        "{text}"
    );
    let agent = why.as_ref().map(Why::text_for_agent).unwrap_or_default();
    assert!(
        agent.contains("a prompt typed in the session makes its guard hook run")
            && !agent.contains('/'),
        "{agent}"
    );
}

// F1-AC34 (Codex): once a guard hook has run with the configuration in place, the reason moves on
// to the release, which cannot be read from this test's program; it names the Codex installs
// Lunchroom reads and says to start the session anew.
#[test]
fn f1_ac34_codex_whose_hook_ran_gets_the_release_reason() {
    let Some(mut s) = Live::new(Harness::Codex, "codex-release") else {
        return;
    };
    s.write_codex_guard();
    s.record_codex_guard_ran();
    let why = s.why();
    let release_reason = matches!(
        &why,
        Some(Why::ReleaseUnread { harness, .. }) if *harness == Harness::Codex
    );
    assert!(release_reason, "{why:?}");
    let text = why.as_ref().map(Why::text).unwrap_or_default();
    assert!(
        text.starts_with("Codex's release cannot be read")
            && text.contains("@openai/codex")
            && text.contains("start the session anew"),
        "{text}"
    );
}

// F1-AC34 (Claude Code): with the whole guard in its settings, the configuration is no longer the
// reason; the reason moves on to the release item. The judgment is made against an empty adapter
// table, so what the host's package records say of this test's own program cannot make it pass:
// the reason is that no adapter covers the release (or, on a host with an rpm database and no rpm
// program, that rpm is not installed), never the configuration.
#[test]
fn f1_ac34_claude_code_with_the_whole_guard_moves_on_to_the_release_reason() {
    let Some(s) = Live::new(Harness::ClaudeCode, "claude-release") else {
        return;
    };
    assert_eq!(
        s.why(),
        Some(Why::Configuration(CLAUDE_NOT_CONFIGURED.to_owned()))
    );
    s.write_claude_guard();
    let judged = assess_with(&[], &s.mail, &s.who, &s.home);
    let release_reason = matches!(
        &judged,
        Assessment::Unguarded(Why::NoAdapter(Harness::ClaudeCode) | Why::RpmNotInstalled { .. })
    );
    assert!(release_reason, "{judged:?}");
}
