//! The process ids a debugger or tracer is given (spec §17 item 5, F1.4).
//!
//! `§17::53`: "When `gdb`, `strace` or `ltrace` is given a process id, … and the process
//! id is not written in the call or that process is not in the agent's own subtree, Lunchroom shall
//! DENY it." `§17::54`: the same call with the id written in the call and the process in
//! the agent's own subtree is not DENIED under this rule. `§17::55`: a debugger that
//! starts a program of its own runs.
//!
//! This module reads the options of each tool as the tool reads them (option tables below, from
//! `strace -h` of strace 6.x, the `ltrace(1)`, `gdb(1)`, `lldb(1)` and `gdbserver(1)` manuals) and
//! returns every process id the call gives, written or not. The caller decides which of them are
//! in the agent's subtree. Options end at the program the tool starts (`strace ls -p x` gives `ls`
//! the option `-p`), except for `gdb`, which reads its options anywhere before `--args`.

/// A process id a debugger is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pid {
    /// The id is written in the call (all digits).
    Written(u32),
    /// The id is not written in the call: a variable, a substitution, a name, a command the tool
    /// reads from its input or from a file the guard cannot settle.
    Unwritten,
}

/// The programs this module reads.
pub const DEBUGGERS: [&str; 5] = ["gdb", "gdbserver", "lldb", "strace", "ltrace"];

/// `v` as a process id list (`1234`, `12,34`): each piece written when it is all digits.
fn pid_list(v: &str) -> Vec<Pid> {
    let mut out: Vec<Pid> = v
        .split([',', ' '])
        .filter(|p| !p.is_empty())
        .map(one)
        .collect();
    if out.is_empty() {
        out.push(Pid::Unwritten);
    }
    out
}

fn one(v: &str) -> Pid {
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        v.parse::<u32>().map_or(Pid::Unwritten, Pid::Written)
    } else {
        Pid::Unwritten
    }
}

/// Every process id `name args…` is given. `fed`: the tool's input comes from a pipe, a redirection
/// or a heredoc in the same call, so commands it reads there are not written in the call's words.
/// `read` gives the text of a command file the tool is told to run (`gdb -x`), or `None` where the
/// guard cannot read it.
#[must_use]
pub fn target_pids(
    name: &str,
    args: &[&str],
    fed: bool,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<Pid> {
    match name {
        // strace 6.x: `strace -h` and strace(1).
        "strace" => tracer(
            args,
            "IbeaosXOSPpUEu",
            &[
                "attach",
                "env",
                "user",
                "argv0",
                "detach-on",
                "interruptible",
                "trace",
                "abbrev",
                "verbose",
                "raw",
                "signal",
                "read",
                "write",
                "fault",
                "inject",
                "status",
                "kvm",
                "decode-fds",
                "trace-fds",
                "output",
                "string-limit",
                "columns",
                "summary-sort-by",
                "trace-path",
                "syscall-limit",
            ],
            &["attach", "pid"],
        ),
        // ltrace(1).
        "ltrace" => tracer(
            args,
            "aADeFlnopsuwx",
            &["pid", "debug", "library", "output"],
            &["pid"],
        ),
        "gdb" => gdb(args, fed, read),
        "lldb" => lldb(args, fed, read),
        "gdbserver" => gdbserver(args),
        _ => Vec::new(),
    }
}

/// `strace` and `ltrace`: options end at the program they start. `short` are the one-letter options
/// that take a value, `long` the long ones, `pid_long` the long names of `-p`.
fn tracer(args: &[&str], short: &str, long: &[&str], pid_long: &[&str]) -> Vec<Pid> {
    let mut pids = Vec::new();
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        if a == "--" || !a.starts_with('-') || a == "-" {
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (name, attached) = rest
                .split_once('=')
                .map_or((rest, None), |(n, v)| (n, Some(v)));
            let takes = long.contains(&name);
            let value = if attached.is_some() {
                attached
            } else if takes {
                let v = args.get(k).copied();
                k += 1;
                v
            } else {
                None
            };
            if pid_long.contains(&name) {
                pids.extend(value.map_or_else(|| vec![Pid::Unwritten], pid_list));
            }
            continue;
        }
        // A cluster of short options: the first that takes a value ends it, the value being the
        // rest of the word or the next word.
        let cluster = &a[1..];
        for (at, c) in cluster.char_indices() {
            if short.contains(c) {
                let rest = &cluster[at + c.len_utf8()..];
                let value = if rest.is_empty() {
                    let v = args.get(k).copied();
                    k += 1;
                    v
                } else {
                    Some(rest)
                };
                if c == 'p' {
                    pids.extend(value.map_or_else(|| vec![Pid::Unwritten], pid_list));
                }
                break;
            }
        }
    }
    pids
}

/// The ids a `gdb` command gives: `attach <pid>`, and any command that can run anything.
fn gdb_command(cmd: &str) -> Vec<Pid> {
    let mut words = cmd.split_whitespace();
    let Some(first) = words.next() else {
        return Vec::new();
    };
    let first = first.to_ascii_lowercase();
    // gdb takes a unique prefix of a command: `att` is `attach`.
    if first.len() >= 3 && "attach".starts_with(&first) {
        return vec![words.next().map_or(Pid::Unwritten, one)];
    }
    // Commands that run other commands, programs or code, whose own text the guard does not read.
    if matches!(
        first.as_str(),
        "python"
            | "py"
            | "python-interactive"
            | "pi"
            | "shell"
            | "!"
            | "pipe"
            | "|"
            | "source"
            | "so"
            | "define"
            | "commands"
            | "guile"
            | "gu"
            | "compile"
            | "expression"
    ) || first.starts_with('!')
    {
        return vec![Pid::Unwritten];
    }
    Vec::new()
}

fn gdb_script(file: Option<&str>, read: &dyn Fn(&str) -> Option<String>) -> Vec<Pid> {
    match file.and_then(read) {
        Some(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .flat_map(gdb_command)
            .collect(),
        None => vec![Pid::Unwritten],
    }
}

/// `gdb`: `-p`/`--pid`, `attach` in a command, `gdb <program> <pid>`; options may be written with
/// one dash or two.
fn gdb(args: &[&str], fed: bool, read: &dyn Fn(&str) -> Option<String>) -> Vec<Pid> {
    const VALUE: [&str; 26] = [
        "ex",
        "iex",
        "eval-command",
        "init-eval-command",
        "x",
        "ix",
        "command",
        "init-command",
        "d",
        "directory",
        "s",
        "symbols",
        "e",
        "exec",
        "se",
        "c",
        "core",
        "p",
        "pid",
        "cd",
        "b",
        "l",
        "tty",
        "D",
        "data-directory",
        "interpreter",
    ];
    let mut pids = Vec::new();
    // The tool reads commands from its input when it has any.
    if fed {
        pids.push(Pid::Unwritten);
    }
    let mut positional = 0;
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        if a == "--args" || a == "-args" {
            break;
        }
        let name = a.trim_start_matches('-');
        if a.starts_with('-') && a.len() > 1 {
            let (name, attached) = name
                .split_once('=')
                .map_or((name, None), |(n, v)| (n, Some(v)));
            // `-p1234`: the short option with its value attached.
            let (name, attached) = match name.strip_prefix('p') {
                Some(rest)
                    if name != "p"
                        && !rest.is_empty()
                        && attached.is_none()
                        && rest.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    ("p", Some(rest))
                }
                _ => (name, attached),
            };
            if VALUE.contains(&name) {
                let value = attached.or_else(|| {
                    let v = args.get(k).copied();
                    k += 1;
                    v
                });
                match name {
                    "p" | "pid" => {
                        pids.extend(value.map_or_else(|| vec![Pid::Unwritten], pid_list));
                    }
                    "ex" | "iex" | "eval-command" | "init-eval-command" => {
                        pids.extend(value.map_or_else(Vec::new, gdb_command));
                    }
                    "x" | "ix" | "command" | "init-command" => {
                        pids.extend(gdb_script(value, read));
                    }
                    _ => {}
                }
            }
            continue;
        }
        positional += 1;
        // `gdb <program> <pid or core file>`.
        if positional == 2 && a.bytes().all(|b| b.is_ascii_digit()) {
            pids.push(one(a));
        }
    }
    pids
}

/// `lldb`: `-p`/`--attach-pid`; `-n`/`--attach-name` and `-w`/`--wait-for` name no id; an `attach`
/// in a command given with `-o` or in a script file.
fn lldb(args: &[&str], fed: bool, read: &dyn Fn(&str) -> Option<String>) -> Vec<Pid> {
    const VALUE: [&str; 20] = [
        "p",
        "attach-pid",
        "n",
        "attach-name",
        "o",
        "one-line",
        "O",
        "one-line-before-file",
        "k",
        "one-line-on-crash",
        "K",
        "source-on-crash",
        "s",
        "source",
        "S",
        "source-before-file",
        "c",
        "core",
        "f",
        "file",
    ];
    fn command(text: &str) -> Vec<Pid> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let is_attach = |w: &str| w.len() >= 3 && "attach".starts_with(&w.to_ascii_lowercase());
        let runs_anything = |w: &str| {
            matches!(
                w.to_ascii_lowercase().as_str(),
                "script" | "scripting" | "python" | "command" | "platform" | "shell" | "!"
            )
        };
        if words.first().is_some_and(|w| runs_anything(w)) {
            return vec![Pid::Unwritten];
        }
        if words.iter().take(2).any(|w| is_attach(w)) {
            let at = words.iter().position(|w| matches!(*w, "-p" | "--pid"));
            return vec![
                at.and_then(|i| words.get(i + 1))
                    .map_or(Pid::Unwritten, |w| one(w)),
            ];
        }
        Vec::new()
    }
    let mut pids = Vec::new();
    if fed {
        pids.push(Pid::Unwritten);
    }
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        if a == "--" {
            break;
        }
        if !a.starts_with('-') || a.len() == 1 {
            continue;
        }
        let name = a.trim_start_matches('-');
        let (name, attached) = name
            .split_once('=')
            .map_or((name, None), |(n, v)| (n, Some(v)));
        // `-w`/`--wait-for` waits for a process by name: no id is written.
        if matches!(name, "w" | "wait-for") {
            pids.push(Pid::Unwritten);
            continue;
        }
        if VALUE.contains(&name) {
            let value = attached.or_else(|| {
                let v = args.get(k).copied();
                k += 1;
                v
            });
            match name {
                "p" | "attach-pid" => {
                    pids.push(value.map_or(Pid::Unwritten, one));
                }
                "n" | "attach-name" => pids.push(Pid::Unwritten),
                "o" | "one-line" | "O" | "one-line-before-file" | "k" | "one-line-on-crash" => {
                    pids.extend(value.map_or_else(Vec::new, command));
                }
                "s" | "source" | "S" | "source-before-file" | "K" | "source-on-crash" => {
                    match value.and_then(read) {
                        Some(text) => pids.extend(
                            text.lines()
                                .map(str::trim)
                                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                                .flat_map(command),
                        ),
                        None => pids.push(Pid::Unwritten),
                    }
                }
                _ => {}
            }
        }
    }
    pids
}

/// `gdbserver --attach <host:port> <pid>`; `--multi` lets a remote gdb attach to any process.
fn gdbserver(args: &[&str]) -> Vec<Pid> {
    let mut pids = Vec::new();
    for (k, a) in args.iter().enumerate() {
        match *a {
            "--attach" => pids.push(args.get(k + 2).copied().map_or(Pid::Unwritten, one)),
            "--multi" => pids.push(Pid::Unwritten),
            "--" => break,
            _ => {}
        }
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::{Pid, Pid::Unwritten, Pid::Written, target_pids};

    fn none(_: &str) -> Option<String> {
        None
    }

    fn pids(name: &str, line: &str) -> Vec<Pid> {
        let args: Vec<&str> = line.split_whitespace().collect();
        target_pids(name, &args, false, &none)
    }

    // §17::53, §17::54 — every spelling of the id option is read, written or not.
    #[test]
    fn strace_and_ltrace_ids_in_every_spelling() {
        for (line, want) in [
            ("-p 123", vec![Written(123)]),
            ("-p123", vec![Written(123)]),
            ("-fp 123", vec![Written(123)]),
            ("-fp123", vec![Written(123)]),
            ("-ff -p 5 -p 6", vec![Written(5), Written(6)]),
            ("-p 5,6", vec![Written(5), Written(6)]),
            ("--attach=77", vec![Written(77)]),
            ("--attach 77", vec![Written(77)]),
            ("-o out.txt -p 9", vec![Written(9)]),
            ("-p $PID", vec![Unwritten]),
            ("-p", vec![Unwritten]),
            ("-p 12x", vec![Unwritten]),
            ("-p +12", vec![Unwritten]),
            ("-p -12", vec![Unwritten]),
        ] {
            assert_eq!(pids("strace", line), want, "strace {line}");
        }
        assert_eq!(pids("ltrace", "-p 4"), vec![Written(4)]);
        assert_eq!(pids("ltrace", "-p4"), vec![Written(4)]);
        assert_eq!(pids("ltrace", "-o x -p $P"), vec![Unwritten]);
    }

    // §17::55 — a program of its own gives no id; its own `-p` is its own.
    #[test]
    fn a_program_of_its_own_gives_no_id() {
        for line in [
            "ls",
            "-f ls -la",
            "-o out ls",
            "-e trace=open -- ls -p 1",
            "-f -- sleep 5",
            "ls -p 1",
            "grep -p 1 x",
            "-u bob ls",
            "-E A=1 env",
        ] {
            assert_eq!(pids("strace", line), vec![], "strace {line}");
        }
        assert_eq!(pids("ltrace", "ls -p 1"), vec![]);
        assert_eq!(pids("ltrace", "ls"), vec![]);
    }

    #[test]
    fn gdb_ids_in_every_spelling() {
        for (line, want) in [
            ("-p 123", vec![Written(123)]),
            ("--pid 123", vec![Written(123)]),
            ("--pid=123", vec![Written(123)]),
            ("-pid 123", vec![Written(123)]),
            ("-p123", vec![Written(123)]),
            ("prog 123", vec![Written(123)]),
            ("-q prog 123", vec![Written(123)]),
            ("-p $P", vec![Unwritten]),
            ("-p", vec![Unwritten]),
        ] {
            assert_eq!(pids("gdb", line), want, "gdb {line}");
        }
        // Commands are one argument each, after the shell has unquoted them.
        let ex = |c: &str| target_pids("gdb", &["--batch", "-ex", c], false, &none);
        assert_eq!(ex("attach 42"), vec![Written(42)]);
        assert_eq!(ex("att 42"), vec![Written(42)]);
        assert_eq!(ex("ATTACH 42"), vec![Written(42)]);
        assert_eq!(ex("attach $pid"), vec![Unwritten]);
        assert_eq!(ex("attach"), vec![Unwritten]);
        assert_eq!(ex("python import os"), vec![Unwritten]);
        assert_eq!(ex("shell strace -p 1"), vec![Unwritten]);
        assert_eq!(ex("!id"), vec![Unwritten]);
        assert_eq!(ex("run"), vec![]);
        assert_eq!(ex("bt"), vec![]);
        assert_eq!(ex("info registers"), vec![]);
        // A program of its own, and its arguments.
        assert_eq!(pids("gdb", "./prog"), vec![]);
        assert_eq!(pids("gdb", "--batch ./prog"), vec![]);
        assert_eq!(pids("gdb", "--args ./prog -p 1"), vec![]);
        assert_eq!(pids("gdb", "./prog core.dump"), vec![]);
    }

    #[test]
    fn gdb_command_files_and_input() {
        let file = |name: &str| -> Option<String> {
            match name {
                "a.gdb" => Some("# attach to it\nbreak main\nattach 77\n".to_owned()),
                "b.gdb" => Some("break main\nrun\n".to_owned()),
                _ => None,
            }
        };
        let run = |args: &[&str], fed| target_pids("gdb", args, fed, &file);
        assert_eq!(run(&["-x", "a.gdb"], false), vec![Written(77)]);
        assert_eq!(run(&["--command=a.gdb"], false), vec![Written(77)]);
        assert_eq!(run(&["-x", "b.gdb"], false), vec![]);
        assert_eq!(run(&["-x", "missing.gdb"], false), vec![Unwritten]);
        assert_eq!(run(&["./prog"], true), vec![Unwritten]);
    }

    #[test]
    fn lldb_and_gdbserver_ids() {
        assert_eq!(pids("lldb", "-p 9"), vec![Written(9)]);
        assert_eq!(pids("lldb", "--attach-pid 9"), vec![Written(9)]);
        assert_eq!(pids("lldb", "--attach-pid=9"), vec![Written(9)]);
        assert_eq!(pids("lldb", "-n sshd"), vec![Unwritten]);
        assert_eq!(pids("lldb", "-w -n sshd"), vec![Unwritten, Unwritten]);
        assert_eq!(pids("lldb", "-p $P"), vec![Unwritten]);
        assert_eq!(pids("lldb", "./prog"), vec![]);
        let o = |c: &str| target_pids("lldb", &["-b", "-o", c], false, &none);
        assert_eq!(o("process attach -p 5"), vec![Written(5)]);
        assert_eq!(o("pro att -p 5"), vec![Written(5)]);
        assert_eq!(o("attach -p 5"), vec![Written(5)]);
        assert_eq!(o("process attach --name sshd"), vec![Unwritten]);
        assert_eq!(o("script import os"), vec![Unwritten]);
        assert_eq!(o("run"), vec![]);
        assert_eq!(pids("gdbserver", "--attach host:1 55"), vec![Written(55)]);
        assert_eq!(pids("gdbserver", "--attach host:1 $P"), vec![Unwritten]);
        assert_eq!(pids("gdbserver", "--multi host:1"), vec![Unwritten]);
        assert_eq!(pids("gdbserver", "host:1 ./prog"), vec![]);
        assert_eq!(pids("cat", "-p 1"), vec![]);
    }
}
