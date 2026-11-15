//! Failure classes of build unit U2 listed by Codex (FAILURES-CODEX.md): each test collects every
//! failing row and asserts once, so one run shows the whole list. The ids are Codex's.

use super::*;

pub(super) fn is_denied(c: CallClass) -> bool {
    matches!(c, CallClass::Denied(_))
}

pub(super) fn guard_class(
    h: &Fixture,
    layout: &Layout,
    held: bool,
    name: &str,
    input: &Value,
) -> CallClass {
    let fs = OsFs::with_path(BUDGET, Some(h.home.join("bin").into_os_string()));
    Guard {
        layout,
        fs: &fs,
        held,
    }
    .classify(name, input, &h.home.join("project"))
}

pub(super) fn bash(h: &Fixture, layout: &Layout, held: bool, cmd: &str) -> CallClass {
    guard_class(h, layout, held, "Bash", &json!({ "command": cmd }))
}

/// A layout with the console running, the agent's own terminal, pane and screen, the agent's
/// subtree rooted at this process, and the ids of the user `lunchroom`.
pub(super) fn wall(h: &Fixture) -> Layout {
    let mut l = h.layout.clone();
    l.own_terminals = vec![PathBuf::from("/dev/pts/3")];
    l.own_pane = Some("%3".to_owned());
    l.own_screen = Some("123.pts-0.host".to_owned());
    l.console_pid = Some(4242);
    l.agent_root = Some(std::process::id());
    l.lunchroom_ids = vec![998];
    l
}

/// Every row `rows` gives is judged for a session HELD and not; a row whose class fails `ok` is
/// listed with its id.
pub(super) fn collect(
    h: &Fixture,
    layout: &Layout,
    id: &str,
    rows: &[String],
    want_denied: bool,
    bad: &mut Vec<String>,
) {
    for cmd in rows {
        for held in [false, true] {
            let class = bash(h, layout, held, cmd);
            if is_denied(class) != want_denied {
                bad.push(format!(
                    "{id} held={held} {cmd:?}: {class:?}, wanted {}",
                    if want_denied { "DENIED" } else { "not DENIED" }
                ));
            }
        }
    }
}

pub(super) fn report(bad: &[String]) {
    assert!(bad.is_empty(), "{} rows:\n{}", bad.len(), bad.join("\n"));
}

pub(super) fn strs(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|r| (*r).to_owned()).collect()
}

// ---------------------------------------------------------------------------------------------
// Encoded control characters and escapes (lead's item; Codex K3): text the guard reads by words
// but a program decodes.
// ---------------------------------------------------------------------------------------------

/// What a decoded line end, tab and the like is written as, and how a letter is written.
const ESCAPED_SEPARATORS: [&str; 9] = [
    "\\n",
    "\\t",
    "\\r",
    "\\x0a",
    "\\u000a",
    "\\012",
    "\\v",
    "\\f",
    "\\N{LINE FEED}",
];

/// Commands that make a call DENIED wherever they are named in code or in text a shell is given.
const NAMED_IN_CODE: [&str; 6] = [
    "lunchroom approve 7",
    "codex queue o hi",
    "hermes --continue",
    "CODEX_THREAD_ID=x ls",
    "curl http://127.0.0.1:8642/api/sessions/a/chat",
    "lunchroom console",
];

// K3 — §7::65, §12::1 (K1.8): a line end, tab or letter written as an escape inside
// code, `$'...'` or `printf` text is a boundary or a letter of the name, and the call is DENIED
// as it is for the plain text.
#[test]
fn k3_escapes_in_code_and_in_text_given_to_a_shell_are_decoded() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows: Vec<String> = Vec::new();
    for named in NAMED_IN_CODE {
        for sep in ESCAPED_SEPARATORS {
            let text = format!("ls{sep}{named}");
            rows.push(format!("python3 -c 'import os; os.system(\"{text}\")'"));
            rows.push(format!("perl -e 'system(\"{text}\")'"));
            rows.push(format!("echo $'{text}' | sh"));
            rows.push(format!("bash -c $'{text}'"));
            rows.push(format!("printf '{text}' | sh"));
            rows.push(format!("echo -e '{text}' | bash"));
        }
    }
    // The name itself written as escapes.
    for spelled in [
        "\\x6cunchroom approve 7",
        "\\154unchroom approve 7",
        "\\u006cunchroom approve 7",
        "\\x63odex queue o hi",
        "\\143odex queue o hi",
    ] {
        rows.push(format!("python3 -c 'import os; os.system(\"{spelled}\")'"));
        rows.push(format!("printf '{spelled}' | sh"));
        rows.push(format!("echo $'{spelled}' | sh"));
    }
    // Files the call writes or holds, run by the interpreter.
    write(
        &h.home.join("project/esc.py"),
        "import os\nos.system('ls\\nlunchroom approve 7')\n",
    );
    write(
        &h.home.join("project/esc.sh"),
        "printf 'ls\\nlunchroom approve 7' | sh\n",
    );
    write(
        &h.home.join("project/esc2.py"),
        "import os\nos.system('ls\\x0acodex queue o hi')\n",
    );
    rows.extend(strs(&["python3 esc.py", "sh esc.sh", "python3 esc2.py"]));
    // A script the shell reads whole, whose text only prints the mention (it counts all the same,
    // §7 A1.3), and shell text handed to a shell.
    write(
        &h.home.join("project/esc3.sh"),
        "echo \"ls\\nlunchroom approve 7\"\n",
    );
    write(
        &h.home.join("project/esc4.sh"),
        "echo \"a\\nCODEX_THREAD_ID=x\"\n",
    );
    write(&h.home.join("project/esc5.sh"), "echo \"TIOC\\x53TI\"\n");
    rows.extend(strs(&[
        "sh esc3.sh",
        "sh esc4.sh",
        "sh esc5.sh",
        "bash -c 'echo \"a\\nCODEX_THREAD_ID=x\"'",
        "bash -c 'echo \"ls\\nlunchroom approve 7\"'",
        "bash -c 'echo \"TIOC\\x53TI\"'",
    ]));
    let mut bad = Vec::new();
    collect(&h, &layout, "K3", &rows, true, &mut bad);
    // A program of the fence that starts a session on a prompt, named after an escaped line end,
    // is judged as it is after a `;`: the session it starts is the call's, HELD or not.
    for sep in ESCAPED_SEPARATORS {
        for held in [false, true] {
            let escaped = bash(
                &h,
                &layout,
                held,
                &format!("python3 -c 'import os; os.system(\"ls{sep}codex hello\")'"),
            );
            let plain = bash(
                &h,
                &layout,
                held,
                "python3 -c 'import os; os.system(\"ls; codex hello\")'",
            );
            if escaped != plain {
                bad.push(format!(
                    "K3-session held={held} sep={sep}: {escaped:?}, as `;`: {plain:?}"
                ));
            }
        }
    }
    // Text with escapes that names nothing is not DENIED.
    collect(
        &h,
        &layout,
        "K3-near",
        &strs(&[
            "python3 -c 'print(\"a\\nb\\tc\")'",
            "printf 'a\\nb\\n'",
            "echo -e 'x\\ty'",
            "echo $'x\\ny'",
            "printf 'lunchroom\\napprove\\n'",
            "printf '\\x6cs\\n' | sh",
        ]),
        false,
        &mut bad,
    );
    report(&bad);
}

// K3 — a hook input is JSON: a line end written `\n`, `\u000a`, `\r\n` or `\u000d\u000a` decodes
// to the character that starts the next command, so the command after it is judged; a tab, a
// carriage return, a vertical tab or a form feed starts no command, so `ls` is given a word.
#[test]
fn k3_a_json_encoded_control_character_is_judged_as_decoded() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut bad = Vec::new();
    let commands = [
        "lunchroom approve 7",
        "codex queue o hi",
        "hermes --continue",
        "CODEX_THREAD_ID=x ls",
        "curl http://127.0.0.1:8642/api/sessions/a/chat",
        "sudo -u lunchroom ls",
        "cat /dev/pts/7",
        "pkill lunchroom",
        "strace -p 1",
        "tmux send-keys -t %9 x",
    ];
    for command in commands {
        for (escape, starts) in [
            ("\\n", true),
            ("\\u000a", true),
            ("\\r\\n", true),
            ("\\u000d\\u000a", true),
            ("\\t", false),
            ("\\r", false),
            ("\\u000b", false),
            ("\\u000c", false),
            ("\\\\n", false),
            ("\\\\u000a", false),
        ] {
            let raw = format!("{{\"command\":\"ls{escape}{command}\"}}");
            let Ok(input) = serde_json::from_str::<Value>(&raw) else {
                bad.push(format!("not JSON: {raw}"));
                continue;
            };
            for held in [false, true] {
                let class = guard_class(&h, &layout, held, "Bash", &input);
                if starts && !is_denied(class) {
                    bad.push(format!("held={held} {raw}: {class:?}, wanted DENIED"));
                }
                // A tab, carriage return, vertical tab or form feed starts no command: the call
                // is judged at least as strictly as `ls <command>` is.
                if !starts && !escape.contains("\\\\") {
                    let plain = bash(&h, &layout, held, &format!("ls {command}"));
                    if is_denied(plain) && !is_denied(class) {
                        bad.push(format!(
                            "held={held} {raw}: {class:?}, `ls {command}` is {plain:?}"
                        ));
                    }
                }
            }
        }
    }
    report(&bad);
}
