//! Codex classes K (cross-products) of build unit U2: every protected operation under every
//! shell construct and wrapper, every odd input, aggregation and size.

use super::u2_enum::{bash, collect, guard_class, is_denied, report, strs, wall};
use super::*;

/// The operations that are DENIED in every session (the classes A, B, C, G, H of the list).
const OPERATIONS: [&str; 10] = [
    "sudo -u lunchroom ls",
    "pkill lunchroom",
    "cat /dev/pts/7",
    "echo x > /var/lib/lunchroom/1000/f",
    "strace -p 1",
    "tmux send-keys -t %9 x",
    "lunchroom approve 7",
    "lunchroom console",
    "cat /proc/1/environ",
    "screen -S other -X stuff x",
];

/// Constructs around a command; `{}` is the command. Each runs the command.
const RUNNERS: [&str; 58] = [
    "{}",
    "true; {}",
    "true && {}",
    "false || {}",
    "true | {}",
    "{} &",
    "{} ; true",
    "{}\ntrue",
    "true\n{}",
    "( {} )",
    "{{ {}; }}",
    "echo $({})",
    "echo `{}`",
    "echo \"$({})\"",
    "if true; then {}; fi",
    "if {}; then true; fi",
    "for i in 1; do {}; done",
    "while true; do {}; break; done",
    "until false; do {}; break; done",
    "case x in x) {} ;; esac",
    "f() {{ {}; }}; f",
    "bash -c '{}'",
    "sh -c '{}'",
    "dash -c '{}'",
    "zsh -c '{}'",
    "bash -lc '{}'",
    "eval '{}'",
    "env A=1 {}",
    "env -i {}",
    "timeout 5 {}",
    "nohup {}",
    "setsid {}",
    "nice -n 5 {}",
    "ionice -c3 {}",
    "stdbuf -o0 {}",
    "time {}",
    "command {}",
    "exec {}",
    "builtin {}",
    "! {}",
    "xargs -I@ {} <<< x",
    "echo x | xargs {}",
    "find /etc/hostname -maxdepth 0 -exec {} \\;",
    "watch -n1 {}",
    "flock /tmp/l {}",
    "flock -n /tmp/l -c '{}'",
    "flock /tmp/l --command '{}'",
    "flock /tmp/l --command='{}'",
    "script -c '{}' /dev/null",
    "script --command '{}' /dev/null",
    "script --command='{}' /dev/null",
    "script -qc '{}' /dev/null",
    "su -c '{}' bob",
    "sudo {}",
    "doas {}",
    "ssh localhost '{}'",
    "at now <<< '{}'",
    "bash <<< '{}'",
];

// K2, K19 — every operation of the list is DENIED under every construct that runs it, held or
// not, and the same constructs around a harmless command are not.
#[test]
fn k2_k19_a_denied_operation_is_denied_under_every_construct_and_wrapper() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    let mut rows = Vec::new();
    for op in OPERATIONS {
        for runner in RUNNERS {
            rows.push(
                runner
                    .replace("{}", op)
                    .replace("{{", "{")
                    .replace("}}", "}"),
            );
        }
    }
    collect(&h, &layout, "K2/K19", &rows, true, &mut bad);
    // The same constructs around a harmless command. Not among them: `env -i`, which clears the
    // whole environment, and a here-string given to a shell, text the guard cannot read, which
    // are DENIED for what they do (I1.2, F1.2); a search of the project folder while HELD, whose
    // links lead to Lunchroom's stores in this fixture.
    let mut near = Vec::new();
    for runner in RUNNERS {
        if matches!(runner, "env -i {}" | "bash <<< '{}'") || runner.starts_with("find ") {
            continue;
        }
        near.push(
            runner
                .replace("{}", "ls /etc/hostname")
                .replace("{{", "{")
                .replace("}}", "}"),
        );
    }
    collect(&h, &layout, "K2-near", &near, false, &mut bad);
    report(&bad);
}

// K1 — the quoting of a word does not change the decision: the same operation with its words in
// single or double quotes, escaped, or cut by empty quotes is DENIED all the same.
#[test]
fn k1_the_quoting_of_a_command_does_not_change_the_decision() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = Vec::new();
    for op in OPERATIONS {
        let words: Vec<&str> = op.split(' ').collect();
        // A word that is syntax (`>`) or an option stays as it is in the forms that quote all.
        let plain = |w: &str| w == ">";
        let all = |q: fn(&str) -> String| {
            words
                .iter()
                .map(|w| if plain(w) { (*w).to_owned() } else { q(w) })
                .collect::<Vec<_>>()
                .join(" ")
        };
        rows.push(all(|w| format!("'{w}'")));
        rows.push(all(|w| format!("\"{w}\"")));
        rows.push(all(|w| format!("\\{w}")));
        rows.push(all(|w| {
            let mut cut = String::new();
            for c in w.chars() {
                cut.push(c);
                cut.push_str("''");
            }
            cut
        }));
        // The program word only.
        let rest = words[1..].join(" ");
        rows.push(format!("'{}' {rest}", words[0]));
        rows.push(format!("\"{}\" {rest}", words[0]));
        rows.push(format!("\\{} {rest}", words[0]));
        rows.push(format!("{}''{} {rest}", &words[0][..1], &words[0][1..]));
        rows.push(format!("{}\"\"{} {rest}", &words[0][..1], &words[0][1..]));
    }
    let mut bad = Vec::new();
    collect(&h, &layout, "K1", &rows, true, &mut bad);
    report(&bad);
}

// K2 — data is not mistaken for a command.
#[test]
fn k2_text_that_only_names_an_operation_is_not_the_operation() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    collect(
        &h,
        &layout,
        "K2-data",
        &strs(&[
            "echo 'sudo -u lunchroom ls'",
            "printf '%s\\n' 'pkill lunchroom'",
            "grep -n 'tmux send-keys' notes.txt",
            "echo \"cat /dev/pts/7\" > /tmp/note",
            "git commit -m 'strace -p 1'",
            "# sudo -u lunchroom ls",
            "ls # pkill lunchroom",
            "echo x | grep 'sudo -u lunchroom'",
            "cat <<'EOF' > /tmp/doc\nsudo -u lunchroom ls\nEOF",
        ]),
        false,
        &mut bad,
    );
    report(&bad);
}

// K3 — a control byte anywhere in a call is judged without a panic; a tab is a blank.
#[test]
fn k3_control_bytes_in_every_position_are_judged_without_a_panic() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    for op in OPERATIONS {
        for c in [
            '\u{0}', '\r', '\n', '\t', '\u{1b}', '\u{7f}', '\u{b}', '\u{c}', '\u{8}',
        ] {
            let chars: Vec<char> = op.chars().collect();
            for at in 0..=chars.len() {
                let mut text: String = chars[..at].iter().collect();
                text.push(c);
                text.extend(&chars[at..]);
                // Only that none of these panics.
                let _ = bash(&h, &layout, false, &text);
                let _ = bash(&h, &layout, true, &text);
            }
        }
        // A tab or a line end between the words of the command is the blank it stands for.
        let tabbed = op.replace(' ', "\t");
        for held in [false, true] {
            let class = bash(&h, &layout, held, &tabbed);
            if !is_denied(class) {
                bad.push(format!(
                    "K3 held={held} {tabbed:?}: {class:?}, wanted DENIED"
                ));
            }
        }
    }
    report(&bad);
}

// K4, K5 — the names are read as they are written: a look-alike, a full-width letter, a
// combining mark, a right-to-left mark or a Turkish letter in a name or a path is another name.
#[test]
fn k4_k5_look_alike_names_are_other_names_and_nothing_depends_on_locale() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    collect(
        &h,
        &layout,
        "K4",
        &strs(&[
            "sudo -u lun\u{441}hroom id",
            "sudo -u lunchro\u{43e}m id",
            "sudo -u ｌｕｎｃｈｒｏｏｍ id",
            "sudo -u lunchroom\u{301} id",
            "sudo -u lun\u{202e}chroom id",
            "sudo -u lunchroom\u{200b} id",
            "sudo -u LUNCHROOM id",
            "sudo -u Lunchroom id",
            "sudo -u lunchr\u{131}om id",
            "pkill lun\u{441}hroom",
            "pkill LUNCHROOM",
            "cat /dev/p\u{442}s/7",
            "cat /dev/PTS/7",
            "echo x > /var/lib/Lunchroom/f",
            "echo x > /var/lib/lunchro\u{43e}m/f",
            "lun\u{441}hroom approve 7",
            "Lunchroom approve 7",
        ]),
        false,
        &mut bad,
    );
    // The one spelling that is the name is DENIED; so are upper-case code names the guard reads
    // without regard to case.
    collect(
        &h,
        &layout,
        "K5",
        &strs(&[
            "python3 -c 'import ctypes; ctypes.CDLL(None).PTRACE(1)'",
            "python3 -c 'x = \"tiocsti\"'",
            "python3 -c 'x = \"TIOCSTI\"'",
            "python3 -c 'x = \"TioCSTI\"'",
        ]),
        true,
        &mut bad,
    );
    report(&bad);
}

// K6, K7, K8, K9 — empty, missing, null and wrongly typed input is judged without a panic; a
// shell command that is not a string is never run on trust, HELD or not.
#[test]
fn k6_k7_k8_k9_empty_missing_null_and_mistyped_input() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    let tools = [
        "Bash",
        "Read",
        "Write",
        "Edit",
        "MultiEdit",
        "NotebookEdit",
        "Grep",
        "Glob",
        "LS",
        "WebFetch",
        "WebSearch",
        "Task",
        "TodoWrite",
        "SendMessage",
        "mcp__x__y",
        "",
        "bash",
    ];
    let inputs = [
        json!({}),
        json!(null),
        json!([]),
        json!("text"),
        json!(5),
        json!(true),
        json!({ "command": null }),
        json!({ "command": 5 }),
        json!({ "command": true }),
        json!({ "command": [] }),
        json!({ "command": ["ls"] }),
        json!({ "command": {} }),
        json!({ "command": { "a": "ls" } }),
        json!({ "command": "" }),
        json!({ "command": " " }),
        json!({ "command": "\n" }),
        json!({ "file_path": null }),
        json!({ "file_path": 5 }),
        json!({ "file_path": [] }),
        json!({ "file_path": "" }),
        json!({ "path": 5, "pattern": [] }),
        json!({ "pattern": null }),
        json!({ "url": 5 }),
        json!({ "edits": null }),
        json!({ "edits": [null, 5, "x", {}] }),
        json!({ "edits": [{ "old_string": 1, "new_string": [] }] }),
        json!({ "notebook_path": 5, "new_source": null }),
        json!({ "content": 5 }),
        json!({ "text": null }),
        json!({ "to": 5, "message": [] }),
        json!({ "a": { "b": { "c": [1, 2, { "d": null }] } } }),
    ];
    for tool in tools {
        for input in &inputs {
            for held in [false, true] {
                // Only that none of these panics.
                let _ = guard_class(&h, &layout, held, tool, input);
            }
        }
    }
    // A shell command that is no string is not a command the guard has judged: HELD, it is not
    // run unseen.
    for input in [
        json!({ "command": null }),
        json!({ "command": 5 }),
        json!({ "command": ["sudo", "-u", "lunchroom", "id"] }),
        json!({ "command": { "a": "ls" } }),
        json!({}),
    ] {
        let class = guard_class(&h, &layout, true, "Bash", &input);
        if class == CallClass::Ordinary || class == CallClass::InSlip {
            bad.push(format!(
                "K9 held Bash {input}: {class:?}, wanted PROTECTED or DENIED"
            ));
        }
    }
    report(&bad);
}

// K10, K12, K13 — a large call, a large file and a deep nest are judged in bounded time, and a
// protected operation among them is still found or the call is DENIED as unreadable.
#[test]
#[allow(clippy::too_many_lines)] // one case per kind of size
fn k10_k12_k13_large_deep_and_recursive_input_is_bounded() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    let started = std::time::Instant::now();
    let mut big = Vec::new();
    // A million-byte argument, and thousands of options and statements.
    big.push(format!(
        "echo {}; sudo -u lunchroom ls",
        "a".repeat(1_000_000)
    ));
    big.push(format!("sudo -u lunchroom ls {}", "a".repeat(1_000_000)));
    big.push(format!("ls {}; sudo -u lunchroom ls", "-a ".repeat(20_000)));
    big.push(format!("{}sudo -u lunchroom ls", "true; ".repeat(20_000)));
    big.push(format!("{}sudo -u lunchroom ls", "true\n".repeat(20_000)));
    big.push(format!("echo '{}'; pkill lunchroom", "x".repeat(300_000)));
    // Nesting beyond what is read.
    let mut nested = "sudo -u lunchroom ls".to_owned();
    // Each level of quoting makes the text four times longer: eight levels are past what is read.
    for _ in 0..8 {
        nested = format!("bash -c '{}'", nested.replace('\'', "'\\''"));
    }
    big.push(nested);
    let mut sub = "sudo -u lunchroom ls".to_owned();
    for _ in 0..500 {
        sub = format!("echo $({sub})");
    }
    big.push(sub);
    big.push("(".repeat(5_000) + "sudo -u lunchroom ls" + &")".repeat(5_000));
    big.push("{ ".repeat(3_000) + "sudo -u lunchroom ls" + &"; }".repeat(3_000));
    big.push(format!("{}x", "if true; then ".repeat(2_000)));
    big.push(format!("echo {}", "$(".repeat(2_000)));
    big.push(format!("echo {}", "`".repeat(5_001)));
    big.push(format!("echo {}", "\"".repeat(100_001)));
    big.push(format!("echo {}", "'".repeat(100_001)));
    big.push(format!("echo {}", "\\".repeat(100_001)));
    big.push(format!("echo {}", "{".repeat(50_000)));
    big.push(format!("echo {}", "{a,".repeat(5_000)));
    for cmd in &big {
        for held in [false, true] {
            let begun = std::time::Instant::now();
            let class = bash(&h, &layout, held, cmd);
            if begun.elapsed() > std::time::Duration::from_secs(20) {
                bad.push(format!(
                    "K10 held={held} {:.40?}: took {:?}",
                    cmd,
                    begun.elapsed()
                ));
            }
            // A call that holds the operation, or cannot be read, is DENIED; a call that holds
            // only the shape (no operation) is judged however it is.
            if cmd.contains("lunchroom") && !is_denied(class) {
                bad.push(format!(
                    "K10 held={held} {cmd:.60?}: {class:?}, wanted DENIED"
                ));
            }
        }
    }
    // A script file larger than the reader takes, and one with a protected command at its end.
    write(
        &h.home.join("project/huge.sh"),
        &format!("{}sudo -u lunchroom ls\n", "true\n".repeat(2_000_000)),
    );
    write(
        &h.home.join("project/long.sh"),
        &format!("echo {}\nsudo -u lunchroom ls\n", "a".repeat(3_000_000)),
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        for f in ["huge.sh", "long.sh"] {
            let p = h.home.join("project").join(f);
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }
    for cmd in [
        "sh huge.sh",
        "bash ./huge.sh",
        "source long.sh",
        ". ./long.sh",
        "./huge.sh",
        "./long.sh",
    ] {
        for held in [false, true] {
            let class = bash(&h, &layout, held, cmd);
            if !is_denied(class) {
                bad.push(format!("K12 held={held} {cmd}: {class:?}, wanted DENIED"));
            }
        }
    }
    // K13: files that carry commands to tmux or screen and refer to themselves.
    write(&h.home.join("project/loop.tmux"), "source-file loop.tmux\n");
    write(
        &h.home.join("project/deep.tmux"),
        &format!("{}send-keys -t %9 x\n", "if-shell true '".repeat(0)),
    );
    // A screen file that sources another, which types into a terminal (a chain), and one that
    // sources itself (names nothing to type: not DENIED).
    write(&h.home.join("project/a.scr"), "source b.scr\n");
    write(&h.home.join("project/b.scr"), "source c.scr\n");
    write(&h.home.join("project/c.scr"), "stuff x\n");
    write(&h.home.join("project/loop.scr"), "source loop.scr\n");
    let mut chain = "send-keys -t %9 x".to_owned();
    for _ in 0..8 {
        chain = format!("if-shell true '{}'", chain.replace('\'', "'\\''"));
    }
    write(&h.home.join("project/chain.tmux"), &format!("{chain}\n"));
    for cmd in [
        "tmux source-file loop.tmux",
        "screen -S other -X source a.scr",
        "screen -S other -X source loop.scr",
        "tmux source-file chain.tmux",
        "tmux source-file deep.tmux",
        &format!("tmux {chain}"),
    ] {
        for held in [false, true] {
            let class = bash(&h, &layout, held, cmd);
            if !is_denied(class) {
                bad.push(format!(
                    "K13 held={held} {cmd:.60}: {class:?}, wanted DENIED"
                ));
            }
        }
    }
    if started.elapsed() > std::time::Duration::from_secs(300) {
        bad.push(format!("K10 the whole took {:?}", started.elapsed()));
    }
    report(&bad);
}

// K14 — the same call judged from several threads at once gives the same answer each time.
#[test]
fn k14_classification_is_the_same_on_every_thread() {
    let h = Fixture::new();
    let layout = wall(&h);
    let calls: Vec<String> = OPERATIONS
        .iter()
        .map(|s| (*s).to_owned())
        .chain(strs(&[
            "ls /tmp",
            "cat /proc/self/maps",
            "echo hi > /dev/null",
        ]))
        .collect();
    let want: Vec<CallClass> = calls.iter().map(|c| bash(&h, &layout, true, c)).collect();
    let bad = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for t in 0..8 {
            let (h, layout, calls, want, bad) = (&h, &layout, &calls, &want, &bad);
            s.spawn(move || {
                for round in 0..20 {
                    for (c, w) in calls.iter().zip(want) {
                        let got = bash(h, layout, true, c);
                        if got != *w
                            && let Ok(mut b) = bad.lock()
                        {
                            b.push(format!(
                                "K14 thread {t} round {round} {c}: {got:?}, once {w:?}"
                            ));
                        }
                    }
                }
            });
        }
    });
    report(&bad.into_inner().unwrap_or_default());
}

// K17 — a tmux command is named by its full name, its alias or the shortest prefix no other
// command shares; an ambiguous or unknown word names none.
#[test]
fn k17_every_prefix_of_a_tmux_command_is_resolved_as_tmux_resolves_it() {
    use crate::classify::terminal::{TMUX_COMMANDS, tmux_command};
    let mut bad = Vec::new();
    for (name, alias) in TMUX_COMMANDS {
        if tmux_command(name) != Some(*name) {
            bad.push(format!(
                "K17 {name}: full name resolves to {:?}",
                tmux_command(name)
            ));
        }
        if !alias.is_empty() && tmux_command(alias) != Some(*name) {
            bad.push(format!(
                "K17 {alias}: alias of {name} resolves to {:?}",
                tmux_command(alias)
            ));
        }
        for len in 1..name.len() {
            let prefix = &name[..len];
            let exact = TMUX_COMMANDS
                .iter()
                .find(|(n, a)| *n == prefix || (!a.is_empty() && *a == prefix));
            let sharing: Vec<&str> = TMUX_COMMANDS
                .iter()
                .filter(|(n, _)| n.starts_with(prefix))
                .map(|(n, _)| *n)
                .collect();
            let want = match (exact, sharing.as_slice()) {
                (Some((n, _)), _) => Some(*n),
                (None, [only]) => Some(*only),
                _ => None,
            };
            if tmux_command(prefix) != want {
                bad.push(format!(
                    "K17 prefix {prefix:?} of {name}: {:?}, wanted {want:?}",
                    tmux_command(prefix)
                ));
            }
        }
    }
    for word in [
        "",
        "-",
        "send-keyz",
        "send-keys-",
        "SEND-KEYS",
        "send keys",
        "x",
        "\u{0}",
    ] {
        if let Some(n) = tmux_command(word) {
            bad.push(format!("K17 {word:?} resolves to {n}"));
        }
    }
    report(&bad);
}

// K20 — a denial or a protection in any part of a call is the call's, whatever else it holds and
// wherever in it the part stands.
#[test]
fn k20_the_outcomes_of_the_parts_of_a_call_are_not_lost() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    let ordinary = ["ls /tmp", "true", "echo hi", "pwd"];
    let denied = ["sudo -u lunchroom ls", "cat /dev/pts/7", "pkill lunchroom"];
    let unparsed = ["echo $(date)", "X=$(date); echo $X", "echo `date`"];
    let protected = ["rm /tmp/x", "git push origin main"];
    for d in denied {
        for sep in [";", "&&", "||", "|", "\n", "&"] {
            for o in ordinary {
                for cmd in [
                    format!("{d} {sep} {o}"),
                    format!("{o} {sep} {d}"),
                    format!("{o} {sep} {o} {sep} {d} {sep} {o}"),
                    format!("{o} {sep} {d} {sep} {o} {sep} {d}"),
                ] {
                    for held in [false, true] {
                        let class = bash(&h, &layout, held, &cmd);
                        if !is_denied(class) {
                            bad.push(format!("K20 held={held} {cmd:?}: {class:?}, wanted DENIED"));
                        }
                    }
                }
            }
            // A denied part beats an unparsed one and a protected one, in either order.
            for u in unparsed.iter().chain(&protected) {
                for cmd in [
                    format!("{u} {sep} {d}"),
                    format!("{d} {sep} {u}"),
                    format!("{u} {sep} {d} {sep} {u}"),
                ] {
                    let class = bash(&h, &layout, true, &cmd);
                    if !is_denied(class) {
                        bad.push(format!("K20 held {cmd:?}: {class:?}, wanted DENIED"));
                    }
                }
            }
        }
    }
    // A protected part, held, is the call's protection however many ordinary parts follow it.
    for p in protected {
        for o in ordinary {
            for cmd in [
                format!("{p}; {o}"),
                format!("{o}; {p}"),
                format!("{o}; {p}; {o}; {o}"),
            ] {
                let class = bash(&h, &layout, true, &cmd);
                if !matches!(class, CallClass::Protected(_) | CallClass::Denied(_)) {
                    bad.push(format!("K20 held {cmd:?}: {class:?}, wanted PROTECTED"));
                }
            }
        }
    }
    report(&bad);
}
