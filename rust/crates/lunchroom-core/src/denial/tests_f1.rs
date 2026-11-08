//! F1 acceptance criteria F1-AC36 to F1-AC39 (docs/spec/1.1/F1/requirements.md, story 14): a DENIED
//! call or message explained in full.
//!
//! Already covered by `tests::a_denied_path_message_in_full` (the whole path message word for word:
//! first line with the path and the next step, what, why with holder and message id, a way),
//! `tests::other_denials_keep_the_label_and_define_held_once` (a Fence message, HELD defined once;
//! an empty fault keeps the label) and `tests::the_agent_is_not_given_the_commands_that_loosen_the_guard`
//! (one reason, no `lunchroom mode` or `lunchroom yolo`). The tests here cover what those leave out.

use super::*;
use crate::vocab::HELD_MEANING;

/// Every reason of the vocabulary table.
const ALL_REASONS: [Reason; 9] = [
    Reason::Fence,
    Reason::Unparsed,
    Reason::DeniedPath,
    Reason::Configuration,
    Reason::NoApprovalChannel,
    Reason::Leak,
    Reason::Halted,
    Reason::Loop,
    Reason::RateLimit,
];

/// `text` holds a rule code of the specification, such as `L1.1`: a capital letter, a digit, a
/// full stop and a digit, not in the middle of a word.
fn has_rule_code(text: &str) -> bool {
    let b = text.as_bytes();
    b.windows(4).enumerate().any(|(i, w)| {
        w.first().is_some_and(u8::is_ascii_uppercase)
            && w.get(1).is_some_and(u8::is_ascii_digit)
            && w.get(2) == Some(&b'.')
            && w.get(3).is_some_and(u8::is_ascii_digit)
            && (i == 0 || b.get(i - 1).is_none_or(|c| !c.is_ascii_alphanumeric()))
    })
}

/// A denial of `reason` with a holder, a part at fault and one way forward.
fn held_denial(reason: Reason) -> Denial {
    let mut d = Denial::new(reason, "the command `ls`");
    d.fault = Some("ls".to_owned());
    d.held_by = vec![holder("alice", "lr-00000045", true)];
    d.ways = vec!["The agent: do something else.".to_owned()];
    d
}

// F1-AC36: after the reason and its next step, the message says what was DENIED, then why (with
// every holder of a HELD session, agent and message id), then every way forward, one line each,
// in that order. A part left out, or put in another place, fails the order check.
#[test]
fn f1_ac36_what_why_and_every_way_forward_follow_the_reason_in_order() {
    let mut d = Denial::new(Reason::NoApprovalChannel, "the command `git push`");
    d.held_by = vec![
        holder("alice", "lr-00000045", true),
        holder("bob", "lr-00000046", true),
    ];
    d.ways = vec![
        "The agent: one.".to_owned(),
        "The user: two.".to_owned(),
        "The user: three.".to_owned(),
    ];
    let out = d.render();
    let first = format!(
        "DENIED · {}: {}",
        Reason::NoApprovalChannel.word(),
        Reason::NoApprovalChannel.next_step()
    );
    assert_eq!(out.lines().next(), Some(first.as_str()), "{out}");
    let at = |needle: &str| out.find(needle);
    let order = [
        at("\nWhat was DENIED: the command `git push`."),
        at("\nWhy: "),
        at("alice (lr-00000045), bob (lr-00000046)"),
        at("\nWays forward:"),
        at("\n- The agent: one."),
        at("\n- The user: two."),
        at("\n- The user: three."),
    ];
    assert!(order.iter().all(Option::is_some), "{order:?}\n{out}");
    assert!(
        order.windows(2).all(|w| w.first() < w.get(1)),
        "{order:?}\n{out}"
    );
    assert_eq!(out.lines().filter(|l| l.starts_with("- ")).count(), 3);
}

// F1-AC37: a search names only the folder searched in the first line, never a file found there:
// the caller passes the folder as the part at fault, and nothing else about the folder reaches
// the message. (Which part a call's denial names is chosen where the call is judged, in the
// lunchroom crate.)
#[test]
fn f1_ac37_a_search_names_only_its_folder_in_the_first_line() {
    let folder = "/home/u/.claude/projects";
    let mut d = Denial::new(Reason::DeniedPath, format!("the Grep of {folder}"));
    d.fault = Some(folder.to_owned());
    d.ways = vec!["The agent: search a folder that holds none.".to_owned()];
    let out = d.render();
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.starts_with(&format!("DENIED · {folder}: ")),
        "{first}"
    );
    assert!(
        !out.contains(".jsonl") && !out.contains("transcript.json"),
        "{out}"
    );
    assert_eq!(out.matches(folder).count(), 3, "{out}");
}

// F1-AC38: no message prints a rule code such as "(L1.1)": not the reason's label, next step or
// rule, and not the whole message of any reason. The detector is first shown to find a code.
#[test]
fn f1_ac38_no_reason_prints_a_rule_code() {
    assert!(has_rule_code("a path (L1.1) is"));
    assert!(has_rule_code("L1.1"));
    assert!(!has_rule_code("Codex 0.163.0 and TLS1.2"));
    for reason in ALL_REASONS {
        for text in [
            reason.word().to_owned(),
            reason.next_step(),
            reason.rule(),
            held_denial(reason).render(),
            Denial::new(reason, "the message to Kimi").render(),
        ] {
            assert!(!has_rule_code(&text), "{reason:?}: {text}");
        }
    }
}

// F1-AC38: HELD is explained the first time a message uses it, once, on the line of that first
// use: by the reason's own rule where it uses the word, else after the holders. A message with
// no holder that uses the word (a denied path) explains it too.
#[test]
fn f1_ac38_held_is_explained_once_where_a_message_first_uses_it() {
    let held = AgentState::Held.word();
    for reason in ALL_REASONS {
        let out = held_denial(reason).render();
        assert_eq!(out.matches(HELD_MEANING).count(), 1, "{reason:?}: {out}");
        let (use_at, meaning_at) = (out.find(held), out.find(HELD_MEANING));
        assert!(use_at < meaning_at && use_at.is_some(), "{reason:?}: {out}");
        let between = out.get(use_at.unwrap_or(0)..meaning_at.unwrap_or(0));
        assert!(
            between.is_some_and(|b| !b.contains('\n')),
            "{reason:?}: {out}"
        );
    }
    let alone = Denial::new(Reason::DeniedPath, "the Read of x").render();
    assert_eq!(alone.matches(HELD_MEANING).count(), 1, "{alone}");
}

// F1-AC39: the way forward that names changing how strict Lunchroom is names no command: no
// program, no code span, none of the words of the commands that do it.
#[test]
fn f1_ac39_the_strictness_way_names_the_change_without_a_command() {
    let way = strictness_way();
    assert!(way.contains("change how strict Lunchroom is"), "{way}");
    for command in ["`", "lunchroom ", "mode", "yolo"] {
        assert!(!way.contains(command), "{command:?}: {way}");
    }
}

// F1-AC39: for every reason, a message that carries the strictness way, with a holder, names it
// and none of the commands that loosen the guard, in the message or in the reason's own text.
#[test]
fn f1_ac39_no_reason_hands_the_agent_a_command_that_loosens_the_guard() {
    for reason in ALL_REASONS {
        let mut d = held_denial(reason);
        d.ways.push(strictness_way());
        let out = d.render();
        assert!(out.ends_with(&format!("\n- {}", strictness_way())), "{out}");
        for text in [out, reason.next_step(), reason.rule()] {
            for command in ["lunchroom mode", "lunchroom yolo"] {
                assert!(!text.contains(command), "{reason:?} {command}: {text}");
            }
        }
    }
}
