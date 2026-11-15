//! Tests of the marked parts (build unit C2, `Guard::marked`; spec §17.10 "Marked parts"). Failure
//! classes: every P1.4 kind marked with its range; a DENIED reason carried; ranges on character
//! boundaries and equal to the words they name; scripts; a call whose text cannot be read; a tool
//! call's canonical JSON; a call with nothing to mark; hostile size.

use super::*;
use crate::classify::{Mark, MarkKind, MarkSource, Marked};

fn marked(h: &Fixture, tool: &str, input: &Value) -> Marked {
    let fs = OsFs::with_path(BUDGET, Some(h.home.join("bin").into_os_string()));
    Guard {
        layout: &h.layout,
        fs: &fs,
        held: true,
    }
    .marked(tool, input, &h.home.join("project"))
}

fn bash(h: &Fixture, cmd: &str) -> Marked {
    marked(h, "Bash", &json!({ "command": cmd }))
}

/// The mark whose words are `words`, in the call's own text.
fn part<'a>(m: &'a Marked, words: &str) -> Option<&'a Mark> {
    m.marks
        .iter()
        .find(|k| k.source == MarkSource::Call && k.words == words)
}

/// Every range is on character boundaries, inside its text, and equals the words it names.
fn consistent(m: &Marked) {
    for k in &m.marks {
        let text = match k.source {
            MarkSource::Call => &m.text,
            MarkSource::Script(i) => &m.scripts[i],
        };
        assert!(k.start <= k.end && k.end <= text.len(), "{k:?}");
        assert!(
            text.is_char_boundary(k.start) && text.is_char_boundary(k.end),
            "{k:?}"
        );
        assert_eq!(&text[k.start..k.end], k.words, "{k:?}");
        assert!(!k.kinds.is_empty() || k.reason.is_some(), "{k:?}");
    }
}

// Each kind of P1.4 is marked at the command that raised it, with the kind beside it.
#[test]
fn every_kind_is_marked_at_its_command() {
    let h = Fixture::new();
    for (cmd, part_words, kind) in [
        ("ls; rm -rf x; ls", "rm -rf x", MarkKind::Destroys),
        (
            "ls; git push origin feature",
            "git push origin feature",
            MarkKind::Destroys,
        ),
        (
            "ls && curl http://example.com",
            "curl http://example.com",
            MarkKind::Destroys,
        ),
        ("ls; git merge x", "git merge x", MarkKind::Destroys),
        ("ls; terraform apply", "terraform apply", MarkKind::Destroys),
        ("ls; dbmate up", "dbmate up", MarkKind::Destroys),
        (
            "ls; cat ~/.ssh/id_rsa",
            "cat ~/.ssh/id_rsa",
            MarkKind::Loosens,
        ),
        ("ls; claude -p hi", "claude -p hi", MarkKind::Loosens),
        ("ls; frobnicate x", "frobnicate x", MarkKind::NotRead),
        ("ls; $(echo deploy)", "$(echo deploy)", MarkKind::NotRead),
        ("ls; eval \"$c\"", "eval \"$c\"", MarkKind::NotRead),
    ] {
        let m = bash(&h, cmd);
        consistent(&m);
        let k = part(&m, part_words);
        assert!(
            k.is_some_and(|k| k.kinds.contains(&kind)),
            "{cmd}: {:?}",
            m.marks
        );
        // The part is a range of the call's text.
        let k = k.map_or((0, 0), |k| (k.start, k.end));
        assert_eq!(&m.text[k.0..k.1], part_words, "{cmd}");
        // No other command is marked.
        assert!(part(&m, "ls").is_none(), "{cmd}");
    }
    // A write to the guard's configuration is marked with its redirection.
    let m = bash(&h, "ls; echo x > ~/.claude/settings.json");
    consistent(&m);
    let k = part(&m, "echo x > ~/.claude/settings.json");
    assert!(
        k.is_some_and(|k| k.kinds.contains(&MarkKind::Loosens)),
        "{:?}",
        m.marks
    );
}

// A part that is more than one kind carries each; a push to `main` is a deploy and a push.
#[test]
fn a_part_can_be_several_kinds() {
    let h = Fixture::new();
    let m = bash(&h, "git push origin main");
    let k = part(&m, "git push origin main");
    assert!(
        k.is_some_and(|k| k.kinds == vec![MarkKind::Destroys]),
        "{:?}",
        m.marks
    );
    let m = bash(&h, "rm $(echo x)");
    let k = part(&m, "rm $(echo x)");
    assert!(
        k.is_some_and(
            |k| k.kinds.contains(&MarkKind::Destroys) && k.kinds.contains(&MarkKind::NotRead)
        ),
        "{:?}",
        m.marks
    );
}

// A range that made the call DENIED carries its reason word.
#[test]
fn a_denied_range_carries_its_reason() {
    let h = Fixture::new();
    let m = bash(&h, "ls; codex queue other hi");
    consistent(&m);
    let k = part(&m, "codex queue other hi");
    assert!(
        k.is_some_and(|k| k.reason == Some(DenyReason::Fence)),
        "{:?}",
        m.marks
    );
    let m = bash(&h, "cat ~/.lunchroom/registry.json");
    assert!(m.marks.iter().any(|k| k.reason.is_some()), "{:?}", m.marks);
    // A call that is not DENIED carries no reason anywhere.
    let m = bash(&h, "rm x");
    assert!(m.marks.iter().all(|k| k.reason.is_none()));
}

// The reply is marked as a whole.
#[test]
fn the_reply_is_marked_as_a_whole() {
    let h = Fixture::new();
    let cmd = format!("{} send codex hi", h.path(".lunchroom/bin/lunchroom"));
    let m = bash(&h, &cmd);
    consistent(&m);
    assert!(
        m.marks
            .iter()
            .any(|k| k.kinds == vec![MarkKind::Reply] && k.start == 0 && k.end == cmd.len()),
        "{:?}",
        m.marks
    );
    assert!(
        !bash(&h, "ls")
            .marks
            .iter()
            .any(|k| k.kinds.contains(&MarkKind::Reply))
    );
}

// Ranges are byte ranges on character boundaries, whatever the characters.
#[test]
fn ranges_are_on_character_boundaries() {
    let h = Fixture::new();
    for cmd in [
        "echo é; rm ü",
        "echo 日本語 && git push origin feature ü",
        "echo \u{1F600}; rm \u{1F600}\u{1F600}",
        "rm `echo é`",
        "echo é > ~/.claude/settings.json",
    ] {
        let m = bash(&h, cmd);
        consistent(&m);
        assert!(!m.marks.is_empty(), "{cmd}");
    }
}

// A script the call runs is shown and marked in its own text.
#[test]
fn a_script_is_shown_and_marked_in_its_own_text() {
    let h = Fixture::new();
    write(
        &h.home.join("project/ps.sh"),
        "echo hi\ngit push origin feature\nls\n",
    );
    let m = bash(&h, "bash ps.sh");
    consistent(&m);
    assert_eq!(
        m.scripts,
        vec!["echo hi\ngit push origin feature\nls\n".to_owned()]
    );
    let k = m
        .marks
        .iter()
        .find(|k| k.source == MarkSource::Script(0) && k.words == "git push origin feature");
    assert!(
        k.is_some_and(|k| k.kinds.contains(&MarkKind::Destroys)),
        "{:?}",
        m.marks
    );
    // A script that is read and holds nothing to mark marks nothing in itself.
    write(&h.home.join("project/ok.sh"), "echo hi\nls\n");
    let m = bash(&h, "bash ok.sh");
    assert_eq!(m.scripts.len(), 1);
    assert!(
        m.marks.iter().all(|k| k.source == MarkSource::Call),
        "{:?}",
        m.marks
    );
    // A script run by its path.
    write(&h.home.join("project/rm.sh"), "#!/bin/sh\nrm -r build\n");
    let m = bash(&h, "./rm.sh");
    assert_eq!(m.scripts.len(), 1);
}

// Text the guard cannot parse is marked "not read", and the whole call is.
#[test]
fn a_call_whose_text_cannot_be_read_is_not_read_as_a_whole() {
    let h = Fixture::new();
    for cmd in [
        "cat <<E\nx\nE",
        "echo {a,b}",
        "echo $PATH",
        "echo 'unclosed",
    ] {
        let m = bash(&h, cmd);
        assert!(m.whole_call_not_read, "{cmd}");
        assert!(
            m.marks.iter().any(|k| k.kinds.contains(&MarkKind::NotRead)
                && k.start == 0
                && k.end == cmd.len()),
            "{cmd}: {:?}",
            m.marks
        );
        consistent(&m);
    }
    assert!(!bash(&h, "rm x").whole_call_not_read);
    assert!(!bash(&h, "ls").whole_call_not_read);
}

// A tool call's text is its canonical JSON; the part is the path it names, or the whole.
#[test]
fn a_tool_call_is_marked_in_its_canonical_json() {
    let h = Fixture::new();
    let input = json!({ "file_path": h.path(".ssh/id_rsa"), "content": "x" });
    let m = marked(&h, "Write", &input);
    assert_eq!(m.text, crate::jcs::canonical(&input));
    consistent(&m);
    let quoted = crate::jcs::canonical(&json!(h.path(".ssh/id_rsa")));
    assert!(
        m.marks
            .iter()
            .any(|k| k.words == quoted && !k.kinds.is_empty()),
        "{:?}",
        m.marks
    );
    // A tool the guard does not know sends data off the computer and is not read.
    let m = marked(&h, "MysteryTool", &json!({ "q": "x" }));
    consistent(&m);
    assert_eq!(m.marks.len(), 1);
    assert!(m.marks[0].kinds.contains(&MarkKind::Destroys));
    assert!(m.marks[0].kinds.contains(&MarkKind::NotRead));
    assert_eq!((m.marks[0].start, m.marks[0].end), (0, m.text.len()));
    // A write with no path the guard can read is DENIED, with its reason, over the whole text.
    let m = marked(&h, "Write", &json!({ "content": "x" }));
    assert!(
        m.marks
            .iter()
            .any(|k| k.reason == Some(DenyReason::Unparsed)),
        "{:?}",
        m.marks
    );
    // A Bash call with no command field is read as its JSON.
    let m = marked(&h, "Bash", &json!({}));
    assert_eq!(m.text, "{}");
    assert!(m.whole_call_not_read);
}

// A call with nothing to mark has no mark.
#[test]
fn a_call_with_nothing_to_mark_has_none() {
    let h = Fixture::new();
    for cmd in ["ls", "git status", "cd /tmp && ls -la", "echo hi"] {
        let m = bash(&h, cmd);
        assert!(m.marks.is_empty(), "{cmd}: {:?}", m.marks);
        assert_eq!(m.scripts.len(), 0);
        assert!(!m.whole_call_not_read);
        assert_eq!(m.text, cmd);
    }
    let m = marked(
        &h,
        "Read",
        &json!({ "file_path": h.path("project/src/main.rs") }),
    );
    assert!(m.marks.is_empty(), "{:?}", m.marks);
}

// Hostile size: a long text, many commands and deep nesting are marked in bounded work, never
// panic, and a call with more commands than are marked one by one is not read as a whole.
#[test]
fn hostile_calls_are_bounded() {
    let h = Fixture::new();
    let many = vec!["rm x"; 300].join("; ");
    let m = bash(&h, &many);
    assert!(m.whole_call_not_read);
    assert!(m.marks.len() <= 260);
    consistent(&m);
    let long = format!("echo {}", "é".repeat(1 << 18));
    let m = bash(&h, &long);
    assert_eq!(m.marks.len(), 0);
    let deep = format!("{}{}", "echo $(".repeat(2_000), ")".repeat(2_000));
    let m = bash(&h, &deep);
    consistent(&m);
    assert!(m.whole_call_not_read || !m.marks.is_empty());
}
