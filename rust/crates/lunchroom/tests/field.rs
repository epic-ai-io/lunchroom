//! Field replays (2026-11-02): commands the installed guard DENIED as `unparsed` in sessions that
//! were not HELD, run through the real guard program with real files. A unit test of the
//! classifier passed them with paths that did not exist, which is how the defect hid: a missing
//! file reads as unreadable, and only a real file over the 1 MiB script limit is DENIED.
//!
//! Spec: F1.2 (docs/spec/lunchroom-spec.md §12 item 2): "A command that runs a shell script the
//! guard cannot find or read … or any script larger than the 1 MiB it searches, is DENIED in every
//! session." The defect is which word the guard takes for the script, not the rule.

mod common;

use std::path::Path;

use serde_json::json;

use common::{Run, Sandbox};

fn sandbox() -> Sandbox {
    common::declare_test_release();
    Sandbox::new("field-tests")
}

fn interactive() -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_PID", std::process::id().to_string()),
        ("CLAUDE_CODE_SESSION_ATTENDED", "1".to_owned()),
        ("CLAUDE_CODE_ENTRYPOINT", "cli".to_owned()),
    ]
}

fn write(p: &Path, bytes: &[u8]) {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, bytes);
}

/// The guard's answer to a Bash call `cmd` from a session that is not HELD, working in `cwd`.
fn pretool(sb: &Sandbox, cwd: &Path, cmd: &str) -> Run {
    let input = json!({
        "session_id": "field-1",
        "transcript_path": "",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "permission_mode": "default",
        "tool_name": "Bash",
        "tool_input": { "command": cmd },
    });
    sb.run(&["guard", "pretool"], &input.to_string(), &interactive())
}

fn denied(run: &Run) -> bool {
    run.code == Some(2) || run.stderr.contains("DENIED") || run.stdout.contains("\"deny\"")
}

// Python's `-I` (isolated mode) takes no value: the script is the next word. Taking `-I` as an
// option with a value (as ruby's and perl's `-I dir` are) made the data file after the script the
// "script", and a data file over 1 MiB was DENIED in every session.
#[test]
fn python_isolated_mode_runs_the_script_it_names() {
    let sb = sandbox();
    let root = sb.root.clone();
    write(
        &root.join("os_by_size.py"),
        b"import sys\nprint(len(sys.stdin.read()))\n",
    );
    write(&root.join("so/results.csv"), &vec![b'x'; 2 * 1024 * 1024]);
    let script = root.join("os_by_size.py");
    let data = root.join("so/results.csv");
    for cmd in [
        format!("python3 -I {} {}", script.display(), data.display()),
        format!("python3 -I {} < {}", script.display(), data.display()),
        format!("python3 {} < {}", script.display(), data.display()),
    ] {
        let run = pretool(&sb, &root, &cmd);
        assert!(!denied(&run), "{cmd}\n{}\n{}", run.stdout, run.stderr);
    }
    // A script over 1 MiB is still DENIED, with `-I` before it too (F1.2).
    write(&root.join("huge.py"), &vec![b'#'; 2 * 1024 * 1024]);
    let huge = format!("python3 -I {}", root.join("huge.py").display());
    assert!(denied(&pretool(&sb, &root, &huge)), "{huge}");
}

// Python's `-E` is a flag too. Taken as inline code (as perl's `-E` is), it hid the script from the
// guard, so a script that puts a turn into another session ran unread. It is read and DENIED.
#[test]
fn python_ignore_environment_flag_still_reads_the_script() {
    let sb = sandbox();
    let root = sb.root.clone();
    write(
        &root.join("relay.py"),
        b"import subprocess\nsubprocess.run(['codex', 'queue', 'other', 'hi'])\n",
    );
    for cmd in [
        format!("python3 {}", root.join("relay.py").display()),
        format!("python3 -E {}", root.join("relay.py").display()),
        format!("python3 -I -E {}", root.join("relay.py").display()),
    ] {
        let run = pretool(&sb, &root, &cmd);
        assert!(denied(&run), "{cmd}\n{}\n{}", run.stdout, run.stderr);
    }
}

// The quote-aware split of text the parser cannot read must not hide what the shell runs inside
// double quotes (`` `…` `` and `$(…)`), nor a quoted program word: each is still DENIED in every
// session when it puts a turn into another session.
#[test]
fn quoted_text_still_shows_what_the_shell_runs_inside_it() {
    let sb = sandbox();
    let root = sb.root.clone();
    for cmd in [
        "f=$(find . -name x); echo \"$(codex queue other hi)\"".to_owned(),
        "f=$(find . -name x); echo \"`codex queue other hi`\"".to_owned(),
        "f=$(find . -name x); \"codex\" queue other hi".to_owned(),
    ] {
        let run = pretool(&sb, &root, &cmd);
        assert!(denied(&run), "{cmd}\n{}\n{}", run.stdout, run.stderr);
    }
}

// Ruby's and perl's `-I` does take a value: the script is the word after it.
#[test]
fn ruby_include_option_takes_its_folder() {
    let sb = sandbox();
    let root = sb.root.clone();
    write(&root.join("app.rb"), b"puts 1\n");
    write(&root.join("lib/huge.rb"), &vec![b'#'; 2 * 1024 * 1024]);
    let cmd = format!("ruby -I lib {}", root.join("app.rb").display());
    let run = pretool(&sb, &root, &cmd);
    assert!(!denied(&run), "{cmd}\n{}", run.stderr);
}

// The real-world mixed-store proof (2026-11-03) was DENIED as `configuration`: its script, which
// the parser cannot read (`if`, `$REL`), named the GitHub repository `epic-ai-io/lunchroom` with
// `--pattern` after it. A1.3 (spec §7 item 3) denies a command that runs one of Lunchroom's
// user-only commands, and §15 limit 8 a script that quotes one; a word after Lunchroom's name that
// is an option and not one of those commands runs nothing (Lunchroom takes its command from its
// first argument and prints its usage for any other, main.rs).
#[test]
fn a_script_naming_the_lunchroom_repository_with_options_is_ordinary() {
    let sb = sandbox();
    let root = sb.root.clone();
    let ordinary = [
        "REL=$(mktemp -d)\nif gh release download v1.0.1 --repo epic-ai-io/lunchroom --pattern \"$ASSET\" --dir \"$REL\"; then echo ok; fi\n",
        "gh release view --repo epic-ai-io/lunchroom --json tagName \"$X\"\n",
        "if true; then tools/lunchroom --pattern x; fi\n",
    ];
    for (i, body) in ordinary.iter().enumerate() {
        let f = root.join(format!("ordinary{i}.sh"));
        write(&f, format!("#!/bin/bash\n{body}").as_bytes());
        let cmd = format!("bash {}", f.display());
        let run = pretool(&sb, &root, &cmd);
        assert!(!denied(&run), "{body}\n{}\n{}", run.stdout, run.stderr);
    }
    // A user-only command in such a script is still DENIED, after `help`-type words too.
    let user_only = [
        "if true; then lunchroom approve 3; fi\n",
        "if [ -n \"$X\" ]; then lunchroom --help approve 3; fi\n",
        "X=$(date); epic-ai-io/lunchroom yolo all till 14:00\n",
        "if true; then lunchroom --version register x; fi\n",
    ];
    for (i, body) in user_only.iter().enumerate() {
        let f = root.join(format!("user_only{i}.sh"));
        write(&f, format!("#!/bin/bash\n{body}").as_bytes());
        let cmd = format!("bash {}", f.display());
        let run = pretool(&sb, &root, &cmd);
        assert!(
            run.stderr.contains("configuration"),
            "{body}\n{}\n{}",
            run.stdout,
            run.stderr
        );
    }
}

// The other commands DENIED as `unparsed` on 2026-11-02, with real files where they name one.
#[test]
fn field_commands_of_2026_11_02_are_ordinary_in_a_session_that_is_not_held() {
    let sb = sandbox();
    let root = sb.root.clone();
    write(&root.join("a/graph_run.sh"), b"#!/bin/bash\necho hi\n");
    write(&root.join("a.txt"), b"one\ntwo\n");
    write(&root.join("s/make.py"), b"print(1)\n");
    write(&root.join("v/x.rs"), b"fn bash(&self) {}\n");
    let r = root.display();
    for cmd in [
        format!("install -m 644 {r}/a/graph_run.sh {r}/b/graph_run.sh"),
        format!("S={r}/s; B={r}/b; mkdir -p $B; cd $S; python3 make.py"),
        format!("cd {r}; sed -n 1,5p a.txt; f=$(find . -name '*.dxf'); echo $f"),
        format!(
            "D={r}/v; mkdir -p $D && cd {r} && git archive HEAD rust | tar -x -C $D && ls $D && grep -n \"fn bash(&self\" $D/x.rs | head -2"
        ),
    ] {
        let run = pretool(&sb, &root, &cmd);
        assert!(!denied(&run), "{cmd}\n{}\n{}", run.stdout, run.stderr);
    }
}
