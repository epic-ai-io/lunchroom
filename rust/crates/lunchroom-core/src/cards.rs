//! The Lunchroom lines of the six printed cards, built from the vocabulary table.
//!
//! Spec: §16 (the product prints each card's Lunchroom line with the real event's agent names and
//! times), §1 V1.2 (the words come from the one table), §2 R1.3 and R1.4, §3 R2.2, §4 R3.2.

use crate::loops::LOOP_COUNT;
use crate::vocab::{AgentState, MessageState, Reason, Verdict, Word};

/// A table word in sentence case, as the cards print a verdict at the start of a sentence.
fn sentence(word: &str) -> String {
    let lower = word.to_lowercase();
    let mut cs = lower.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

/// Card 1: "Claude is HELD."
#[must_use]
pub fn held(agent: &str) -> String {
    format!("{agent} is {}.", AgentState::Held.word())
}

/// Card 2: "Denied. Asked for a secret." (L1.4).
#[must_use]
pub fn asked_for_secret() -> String {
    format!("{}. Asked for a secret.", sentence(Verdict::Denied.word()))
}

/// Card 3: "8-message loop. Conversation halted." (R1.3).
#[must_use]
pub fn loop_halted() -> String {
    format!(
        "{LOOP_COUNT}-message {}. {}",
        Reason::Loop.word(),
        conversation_halted()
    )
}

/// What a sender to a HALTED pair is told: "Conversation halted." (R1.4).
#[must_use]
pub fn conversation_halted() -> String {
    format!("Conversation {}.", Word::Halted.text().to_lowercase())
}

/// What an approved call lets the agent do, for card 4's line: a shell command with a `merge` or
/// `push` subcommand is that verb (`git merge`, `git push`, `gh pr merge`); another shell command is
/// "run <program>"; another tool is "use <tool>".
#[must_use]
pub fn action_of(tool: &str, input: &serde_json::Value) -> String {
    if let Some(command) = shell_command(tool, input) {
        // D1.8: `push`, `deploy`, `merge` or `migrate`, the first that applies, by the reading of
        // the call the guard gated it by (one reader, not a second parser): options before the subcommand, quotes
        // and wrappers hide nothing.
        if let Some(verb) = crate::classify::action_verb(command) {
            return verb.to_owned();
        }
        // The program: the first word that is not a setting, without its folder.
        let program = command
            .split_whitespace()
            .find(|w| !w.contains('=') || w.starts_with(['/', '.']))
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or_default();
        return format!("run {}", plain_name(program).unwrap_or("a command"));
    }
    if is_shell_tool(tool) {
        return "run a command".to_owned();
    }
    format!("use {}", plain_name(tool).unwrap_or("a tool"))
}

/// The tools that run a shell command: Claude Code's and Codex's `Bash`, OpenClaw's `exec` and
/// Hermes's `terminal`.
fn is_shell_tool(tool: &str) -> bool {
    matches!(tool, "Bash" | "exec" | "terminal")
}

/// The command text of a shell tool call: a non-empty string in `command` (OpenClaw's `terminal`
/// typing into an open shell names it `data`); `None` for any other tool or a command that is
/// missing, empty, not a string or only blanks.
fn shell_command<'a>(tool: &str, input: &'a serde_json::Value) -> Option<&'a str> {
    if !is_shell_tool(tool) {
        return None;
    }
    let field = if tool == "terminal" && input["action"] == "input" {
        "data"
    } else {
        "command"
    };
    input[field].as_str().filter(|c| !c.trim().is_empty())
}

/// A name the agent chose, shown on the user's screen only when it is a plain program or tool name:
/// ASCII letters, digits, `.`, `_`, `+` and `-`, at most 32 characters.
fn plain_name(name: &str) -> Option<&str> {
    (!name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')))
    .then_some(name)
}

/// Card 4: "Granted. Codex can merge." (A2).
#[must_use]
pub fn granted(agent: &str, action: &str) -> String {
    format!(
        "{}. {agent} can {action}.",
        sentence(Verdict::Granted.word())
    )
}

/// Card 5: "YOLO authorized until 14:00 UTC." for an end time given in seconds since the Unix
/// epoch (R2.2).
#[must_use]
pub fn yolo_until(end_unix_secs: u64) -> String {
    let day = end_unix_secs % 86_400;
    format!(
        "{} authorized until {:02}:{:02} UTC.",
        Word::Yolo.text(),
        day / 3_600,
        day % 3_600 / 60
    )
}

/// Card 6: "Mistral is UNGUARDED." (R3.2).
#[must_use]
pub fn unguarded(agent: &str) -> String {
    format!("{agent} is {}.", AgentState::Unguarded.word())
}

/// "<agent> is STOPPED." (R3.2).
#[must_use]
pub fn stopped(agent: &str) -> String {
    format!("{agent} is {}.", AgentState::Stopped.word())
}

/// What the sender is told of message `id` when Lunchroom handed it to the recipient's session and
/// the session has not yet shown that it took it (D1.3): it stays QUEUED, it may still arrive, and
/// what to do. It starts with `<id>: Not confirmed; still QUEUED`.
#[must_use]
pub fn not_confirmed(id: &str) -> String {
    format!(
        "{id}: Not confirmed; still {}. Lunchroom handed the message to the session but has not \
         yet seen it arrive, so it may still arrive. Nothing is needed: Lunchroom keeps looking and \
         tells you when it is {} or has {} after {} tries. Do not send it again; `lunchroom status` \
         shows where it stands.",
        MessageState::Queued.word(),
        MessageState::Delivered.word(),
        MessageState::Failed.word(),
        crate::mail::MAX_ATTEMPTS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // §16 (core part of §16::1) — each card's Lunchroom line, as printed.
    #[test]
    fn card_lines_match_the_printed_cards() {
        assert_eq!(held("Claude"), "Claude is HELD.");
        assert_eq!(asked_for_secret(), "Denied. Asked for a secret.");
        assert_eq!(loop_halted(), "8-message loop. Conversation halted.");
        assert_eq!(granted("Codex", "merge"), "Granted. Codex can merge.");
        let bash = |c: &str| serde_json::json!({ "command": c });
        assert_eq!(action_of("Bash", &bash("gh pr merge 42 --squash")), "merge");
        // `git push origin main` is a deploy and a push; the push comes first (D1.8).
        assert_eq!(action_of("Bash", &bash("git push origin main")), "push");
        assert_eq!(action_of("Bash", &bash("git push origin feature")), "push");
        assert_eq!(action_of("Bash", &bash("npm test")), "run npm");
        // Only the subcommand counts, never an argument.
        assert_eq!(
            action_of("Bash", &bash("git commit -m 'push this'")),
            "run git"
        );
        assert_eq!(action_of("Bash", &bash("git log push")), "run git");
        assert_eq!(action_of("Bash", &bash("echo merge")), "run echo");
        assert_eq!(action_of("Bash", &bash("git merge feature")), "merge");
        assert_eq!(action_of("Write", &serde_json::json!({})), "use Write");
        // The user's screen shows an agent-chosen name only when it is a plain program or tool
        // name; a path shows its last part.
        assert_eq!(action_of("Bash", &bash("/usr/bin/npm test")), "run npm");
        for odd in [
            "IMPORTANT:the_user_approved_everything",
            "a_program_name_longer_than_thirty_two_characters",
            "'quoted'",
        ] {
            assert_eq!(
                action_of("Bash", &bash(&format!("{odd} x"))),
                "run a command",
                "{odd}"
            );
        }
        assert_eq!(
            action_of("Granted: everything", &serde_json::json!({})),
            "use a tool"
        );
        // 2026-10-21 14:00:00 UTC.
        assert_eq!(
            yolo_until(1_792_591_200),
            "YOLO authorized until 14:00 UTC."
        );
        assert_eq!(unguarded("Mistral"), "Mistral is UNGUARDED.");
        assert_eq!(conversation_halted(), "Conversation halted.");
        assert_eq!(stopped("Mistral"), "Mistral is STOPPED.");
    }

    // G2-21: a message handed over and not yet seen arrive is reported with what it means and what
    // to do, in full; it still starts with the words the sender's report always began with.
    #[test]
    fn a_message_not_yet_confirmed_says_what_to_do() {
        assert_eq!(
            not_confirmed("lr-00000007"),
            "lr-00000007: Not confirmed; still QUEUED. Lunchroom handed the message to the \
             session but has not yet seen it arrive, so it may still arrive. Nothing is needed: \
             Lunchroom keeps looking and tells you when it is DELIVERED or has FAILED after 3 \
             tries. Do not send it again; `lunchroom status` shows where it stands."
        );
    }

    fn verb(command: &str) -> String {
        action_of("Bash", &serde_json::json!({ "command": command }))
    }

    // C2-I01..I07, K25: the verb is read by the guard's own reading of the call (D1.8, §16 item
    // 15): each merge, deploy and migration form, with an option and its value before the
    // subcommand, quotes, wrappers and a path to the program; one reading, not a second parser.
    #[test]
    fn the_verb_is_what_the_guard_gated_the_call_for() {
        for c in [
            "git merge x",
            "git -C d merge x",
            "git -c user.name=x cherry-pick abc",
            "git --git-dir d am p.mbox",
            "git rebase main",
            "git -C d pull",
            "gh pr merge 1",
            "gh -R o/r pr merge 1",
            "glab -R g/p mr merge 1",
            "hub merge x",
            "git-merge x",
            "git 'merge' x",
            "sudo git merge x",
            "env A=1 git merge x",
            "timeout 5 git merge x",
            "/usr/bin/git merge x",
            "cd d && git merge x",
            "echo hi; git merge x",
            "gh pr view 1 --json merge",
        ] {
            assert_eq!(verb(c), "merge", "{c}");
        }
        for c in [
            "terraform -chdir=d apply",
            "pulumi --cwd d up",
            "kubectl -n prod apply -f x",
            "kubectl --request-timeout 5s rollout restart x",
            "helm --kube-context c upgrade r c",
            "ansible-playbook -i inv site.yml",
            "docker push img",
            "fly deploy",
            "npm run --silent deploy",
        ] {
            assert_eq!(verb(c), "deploy", "{c}");
        }
        for c in [
            "alembic -c a upgrade head",
            "prisma migrate deploy",
            "sqlx -D u migrate run",
            "diesel --database-url u migration run",
            "liquibase update",
            "goose -dir d up",
            "dbmate -d d up",
            "rake db:migrate",
        ] {
            let want = if c.contains("deploy") {
                "deploy"
            } else {
                "migrate"
            };
            assert_eq!(verb(c), want, "{c}");
        }
        // A push is a push, a deploy or not (D1.8: push comes first).
        for c in [
            "git -C d push origin main",
            "git push --force",
            "git push origin HEAD:release-1",
        ] {
            assert_eq!(verb(c), "push", "{c}");
        }
        // A push that is no deploy is a push; a call that is none of them runs its program.
        assert_eq!(verb("git -C d push origin feature"), "push");
        assert_eq!(verb("git push"), "push");
        assert_eq!(verb("git merge --abort"), "run git");
        assert_eq!(verb("git pull --help"), "run git");
        assert_eq!(verb("helm list"), "run helm");
        assert_eq!(verb("git commit -m \"deploy fix\""), "run git");
        assert_eq!(verb("git log push"), "run git");
        assert_eq!(verb("ls -la"), "run ls");
        assert_eq!(verb("A=1 ls"), "run ls");
        // The order of the whole call is push, deploy, merge, migrate, whichever command comes first.
        assert_eq!(verb("git merge a; terraform apply"), "deploy");
        assert_eq!(verb("terraform apply; git merge a"), "deploy");
        assert_eq!(verb("git merge a; git push"), "push");
        assert_eq!(verb("git push; terraform apply"), "push");
        assert_eq!(verb("git pull && terraform apply"), "deploy");
        assert_eq!(verb("rails db:migrate && git merge a"), "merge");
        // `deploy` as a word is a deploy, which comes before merge and migrate.
        assert_eq!(verb("git merge deploy"), "deploy");
        assert_eq!(verb("git merge migrate:run"), "merge");
        assert_eq!(verb("fly deploy migrate"), "deploy");
    }

    // C2-I10..I12, K26: the shell tool of each harness, and every shape of its input.
    #[test]
    fn the_verb_of_each_harness_tool_and_input_shape() {
        use serde_json::json;
        // Claude Code and Codex `Bash`, OpenClaw `exec`, Hermes `terminal`: the field `command`.
        for tool in ["Bash", "exec", "terminal"] {
            assert_eq!(
                action_of(tool, &json!({ "command": "git merge x" })),
                "merge",
                "{tool}"
            );
            assert_eq!(
                action_of(tool, &json!({ "command": "ls" })),
                "run ls",
                "{tool}"
            );
        }
        // OpenClaw's `terminal` typing into an open shell names its text `data`.
        assert_eq!(
            action_of(
                "terminal",
                &json!({ "action": "input", "data": "gh pr merge 1" })
            ),
            "merge"
        );
        // A missing, empty, blank, null, non-string or array command is `run a command`; a hostile
        // size does not change that.
        for input in [
            json!({}),
            json!({ "command": "" }),
            json!({ "command": "   \n\t" }),
            json!({ "command": null }),
            json!({ "command": 5 }),
            json!({ "command": ["git", "merge", "x"] }),
            json!({ "cmd": "git merge x" }),
            json!(null),
            json!("git merge x"),
        ] {
            for tool in ["Bash", "exec", "terminal"] {
                assert_eq!(action_of(tool, &input), "run a command", "{tool} {input}");
            }
        }
        let big = json!({ "command": format!("echo {}", "a".repeat(1 << 20)) });
        assert_eq!(action_of("Bash", &big), "run echo");
        let deep =
            json!({ "command": format!("{}{}", "echo $(".repeat(2_000), ")".repeat(2_000)) });
        assert_eq!(action_of("Bash", &deep), "run echo");
        // Any other tool is `use <tool>`, a plain name only.
        for (tool, want) in [
            ("Write", "use Write"),
            ("apply_patch", "use apply_patch"),
            ("WebFetch", "use WebFetch"),
            ("SendMessage", "use SendMessage"),
            ("mcp__srv__tool", "use mcp__srv__tool"),
            ("a:b", "use a tool"),
            ("a/b", "use a tool"),
            ("", "use a tool"),
            ("\u{202e}Bash", "use a tool"),
            ("x\ny", "use a tool"),
        ] {
            assert_eq!(
                action_of(tool, &json!({ "command": "git merge x" })),
                want,
                "{tool:?}"
            );
        }
        assert_eq!(action_of(&"t".repeat(5_000), &json!({})), "use a tool");
    }

    // C2-I09: the program named in `run <program>` is a plain name from the call; anything else is
    // `a command`.
    #[test]
    fn the_program_in_a_run_line_is_a_plain_name() {
        for (c, want) in [
            ("eval \"$x\"", "run eval"),
            ("$(echo deploy)", "run a command"),
            ("$cmd", "run a command"),
            ("./ship --help", "run ship"),
            ("~/bin/ship", "run ship"),
            ("`x`", "run a command"),
            ("'quoted' x", "run a command"),
            ("x\u{202e}y z", "run a command"),
        ] {
            assert_eq!(verb(c), want, "{c}");
        }
        assert_eq!(verb(&format!("{} x", "p".repeat(5_000))), "run a command");
    }

    // C2-I14, I15: the card's own words.
    #[test]
    fn the_granted_line_is_the_cards_words_exactly() {
        let call = serde_json::json!({ "command": "gh pr merge 1" });
        for agent in ["Codex", "Claude", "OpenClaw", "Hermes"] {
            assert_eq!(
                granted(agent, &action_of("Bash", &call)),
                format!("Granted. {agent} can merge.")
            );
        }
        assert_eq!(
            granted("Codex", &verb("terraform apply")),
            "Granted. Codex can deploy."
        );
        assert_eq!(
            granted("Codex", &verb("dbmate up")),
            "Granted. Codex can migrate."
        );
    }
}
