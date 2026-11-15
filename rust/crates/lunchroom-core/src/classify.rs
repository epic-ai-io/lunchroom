//! Classifies one Claude Code tool call against Lunchroom's fixed rules.
//!
//! Spec: §6 P1.1 (a slip holds only reads), P1.4 (the PROTECTED kinds; an agent's write to
//! Lunchroom's stores is DENIED), §11 L1.1 (denied and credential paths, resolved, by direct path,
//! symlink, wildcard and parent-folder search), §12 F1.1–F1.2 (fences; unparsed commands), §7 A1.3
//! (an agent's `lunchroom approve`, `yolo` and configuration commands are DENIED), §9 I1.2 (no
//! command sets, unsets or clears the identity variables).
//!
//! Rules that hold in every session are always checked. The rest change nothing for an agent that
//! is not HELD (P1.2), so they are checked only while it is HELD, which keeps the guard's
//! file-system work off every call such an agent makes. What a call reads, searches and writes
//! ([`Access`]) is recorded in every session, since a session that is not HELD is held by what it
//! then reads (§5 T1.8).
//!
//! What the guard judges is the call itself (spec §15 K1.8) and what it runs. A script the call
//! runs is judged as a command of its own would be, with the session's own state (§6 P1.4): while
//! the session is HELD each part of it that would be PROTECTED makes the call PROTECTED, and each
//! part that would be DENIED makes it DENIED; so is a file the call writes and then runs (a heredoc,
//! `printf` or `echo` redirected into it), judged on the text written. Code handed to a program
//! such as `python -c` is searched for the rules that are DENIED in every session (fences,
//! identity, Lunchroom commands, terminals, `ptrace`), for paths of Lunchroom's and for commands of
//! a PROTECTED kind that it spells out in strings.

use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::fs::{Fs, Script, WalkEnd, fnmatch};
use crate::paths::{Layout, absolute, literal_prefix, normalize, proc_secret_folder};
use crate::policy::{Call, CallClass, DenyReason, ProtectedKind, ProtectedKinds};
use crate::shell::{self, Parsed, Word};

/// The guard's view of one agent at the moment of a call.
#[derive(Clone, Copy)]
pub struct Guard<'a> {
    /// Where Lunchroom and the harnesses keep their files.
    pub layout: &'a Layout,
    /// The file system.
    pub fs: &'a dyn Fs,
    /// Another agent's text is in this agent's context.
    pub held: bool,
}

impl std::fmt::Debug for Guard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("layout", self.layout)
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

/// A judged call: the class and kinds ([`Call`]), what the rule behind a DENIED class found at
/// fault (V1.4), and the paths the call reads, searches and writes ([`Access`], T1.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// The class shown and every PROTECTED kind raised.
    pub call: Call,
    /// The path, program, word or text at fault for a DENIED class.
    pub fault: Option<String>,
    /// What the call reads, searches and writes.
    pub access: Access,
}

/// Tools that only read Lunchroom-internal or harness-internal lists.
const SLIP_TOOLS: [&str; 4] = ["ToolSearch", "TaskList", "TaskGet", "ListAgents"];

/// Tools that change the session's own state or start work, and send nothing off the computer.
const ORDINARY_TOOLS: [&str; 12] = [
    "Agent",
    "Task",
    "TodoWrite",
    "TaskCreate",
    "TaskUpdate",
    "TaskStop",
    "AskUserQuestion",
    "Skill",
    "EnterPlanMode",
    "ExitPlanMode",
    "SendMessage",
    "ReadNotifications",
];

/// Tools that send data off the computer.
const NETWORK_TOOLS: [&str; 2] = ["WebFetch", "WebSearch"];

/// Programs that put a turn into another agent's session (F1.1).
const FENCE_PROGRAMS: [&str; 5] = ["codex", "claude", "opencode", "openclaw", "hermes"];

/// Commands of `claude` and `codex` that take no prompt: a first word that is none of these starts
/// a new session on that prompt (P1.4).
const NOT_A_PROMPT: [&str; 41] = [
    // claude
    "agents",
    "attach",
    "config",
    "migrate-installer",
    "auth",
    // protocol-words:begin (Claude Code's own command name)
    "auto-mode",
    // protocol-words:end
    "doctor",
    "gateway",
    "import",
    "install",
    "logs",
    "mcp",
    "plugin",
    "plugins",
    "purge",
    "respawn",
    "rm",
    "setup-token",
    "stop",
    "kill",
    "ultrareview",
    "update",
    "upgrade",
    // codex
    "login",
    "logout",
    "app-server",
    "exec-server",
    "migrate-rollouts",
    "migrate-nfs-db",
    "remote-control",
    "completion",
    "sandbox",
    "debug",
    "apply",
    "a",
    "archive",
    "delete",
    "unarchive",
    "features",
    "help",
    "cloud",
];

/// The name of Lunchroom's program. A link under another name is found by its real path.
const LUNCHROOM_NAMES: [&str; 1] = ["lunchroom"];

/// Lunchroom commands an agent may run; every other one is the user's alone (A1.3).
const LUNCHROOM_OPEN: [&str; 9] = [
    "send",
    // P1.1, D1.1: `lunchroom read <id>` prints a message addressed to the session; it is a read.
    "read",
    "status",
    "codex-threads",
    "help",
    "version",
    "--help",
    "-h",
    "--version",
];

/// Variables that carry an agent's identity (I1.2).
const IDENTITY_VARS: [&str; 5] = [
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "HERMES_SESSION_ID",
    "LUNCHROOM_SESSION",
    "LUNCHROOM_HOME",
];

const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

/// Programs that run code given as an argument.
const INTERPRETERS: [&str; 19] = [
    "python",
    "python2",
    "python3",
    "node",
    "nodejs",
    "perl",
    "ruby",
    "php",
    "deno",
    "bun",
    "lua",
    "Rscript",
    "osascript",
    "pwsh",
    "tclsh",
    "awk",
    "gawk",
    "mawk",
    "sed",
];

const NETWORK_PROGRAMS: [&str; 31] = [
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "telnet",
    "ftp",
    "lftp",
    "gh",
    "aws",
    "gcloud",
    "az",
    "http",
    "https",
    "xh",
    "rclone",
    "s3cmd",
    "mail",
    "mailx",
    "sendmail",
    "mutt",
    "dig",
    "nslookup",
    "host",
    "ping",
    "traceroute",
];

const GIT_NETWORK: [&str; 8] = [
    "fetch",
    "pull",
    "clone",
    "ls-remote",
    "send-email",
    "request-pull",
    "submodule",
    "svn",
];

const PUBLISHERS: [&str; 17] = [
    "npm", "pnpm", "yarn", "bun", "cargo", "gem", "twine", "poetry", "flit", "hatch", "docker",
    "podman", "helm", "vsce", "ovsx", "dotnet", "nuget",
];

const DELETERS: [&str; 7] = [
    "rm",
    "rmdir",
    "unlink",
    "shred",
    "srm",
    "trash",
    "trash-put",
];

/// Programs that change everything below a folder they are given.
const RECURSIVE_WRITE: [&str; 9] = [
    "rm", "rmdir", "shred", "chmod", "chown", "chgrp", "chattr", "setfacl", "mv",
];

/// `find` actions that change or run something.
const FIND_ACTIONS: [&str; 5] = ["-delete", "-exec", "-execdir", "-ok", "-okdir"];

/// Programs that search below the working folder when given no folder.
const SEARCHERS: [&str; 8] = ["rg", "ag", "ack", "fd", "fdfind", "tree", "du", "find"];

/// Programs that search below a folder only when told to recurse (`-r`, `-R`, `--recursive`).
const RECURSE_ON_FLAG: [&str; 5] = ["grep", "egrep", "fgrep", "zgrep", "ls"];

/// The `grep` family: the first word that is not an option is the pattern, unless `-e` or `-f`
/// gives it.
const GREPS: [&str; 4] = ["grep", "egrep", "fgrep", "zgrep"];

/// Programs that read everything below a folder they are given, and nothing when given none.
const COPIERS: [&str; 5] = ["tar", "zip", "cp", "rsync", "scp"];

/// Lunchroom commands that only the user may run, as they are spotted in code (A1.3).
const LUNCHROOM_USER_ONLY: [&str; 19] = [
    // 1.1: the console is the user's own screen; an agent that ran it would see and press its keys.
    "console",
    // T1.8: forgets the paths recorded as holding messages; an agent that ran it would clear its
    // own hold (§5 T1.8, A1.3).
    "clear",
    "approve",
    // Runs the check of the QUEUED messages (D1.7), which counts hand-overs failed and offers
    // messages again through the recipients' own programs. The hooks start it; an agent that ran
    // it could make another session's message fail or be offered again.
    "check",
    "repair",
    "mode",
    "yolo",
    "register",
    "unregister",
    "slip",
    "rate",
    // Sets the passphrase that every command of this list asks for (A1.3).
    "passphrase",
    "guard",
    // Delivery runs only from the harness's own hook: an agent that ran it could take another
    // session's message (D1.1, I1.2).
    "listen",
    "rename",
    "config",
    // Write the program and the harness configuration that runs the guard (H1.4, A1.3).
    "install",
    "uninstall",
    // C1.4: "It is the user's command: an agent's attempt to run it is DENIED."
    "doctor",
];

/// Programs that signal processes by id or by name (1.1, rule 2).
const KILLERS: [&str; 3] = ["kill", "pkill", "killall"];

/// `word` is one of the words of `text`, where a word is a run of letters, digits, `_` and `-`
/// (so `/usr/local/lib/lunchroom/lunchroom` names it, `my-lunchroom-app` does not).
fn names_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .any(|w| w == word)
}

/// `name args...` runs a program as the user or the group `lunchroom` (1.1, rule 5): `sudo`,
/// `doas`, `runuser` and `pkexec` with the user or group option naming it, `su` and
/// `machinectl shell` with it as an operand. A word `lunchroom` that is no user (a package name)
/// is not one.
///
/// §17.5, §7 A1.3: `sudo`, `su`, `runuser`, `machinectl shell`, `pkexec`, `doas` and `run0` to the
/// user `lunchroom`. The user is named by its name or by its numeric id (`-u '#998'`, `ids`: the
/// ids the layout knows for the user and group `lunchroom`). Options are read as the programs read
/// them: a cluster (`-nu lunchroom`, `-Hulunchroom`), a long option with `=` or a separate value,
/// and options end at the command, so `sudo ls -u lunchroom` gives `ls` its own `-u`.
fn becomes_lunchroom(name: &str, args: &[&str], ids: &[u32]) -> bool {
    const WHO: &str = "lunchroom";
    let names = |v: &str| {
        v == WHO
            || v.strip_prefix('#')
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| ids.contains(&n))
    };
    // The one-letter options that take a value, which of them name a user or a group, the long
    // options that name one, and every long option the program has (`true`: it takes a value).
    // From `sudo --help` and `runuser --help` of this host, and the manuals of run0 and pkexec.
    let (value_letters, who_letters, who_long, longs): (&str, &str, &[&str], &[(&str, bool)]) =
        match name {
            "sudo" => ("ughpCDrtTRU", "ug", &["user", "group"], SUDO_LONG),
            "doas" => ("uC", "u", &[], &[]),
            "run0" => ("ugD", "ug", &["user", "group"], RUN0_LONG),
            "pkexec" => ("", "", &["user"], PKEXEC_LONG),
            "runuser" => (
                "ugGcsw",
                "ugG",
                &["user", "group", "supp-group"],
                RUNUSER_LONG,
            ),
            "su" | "machinectl" => {
                // `su` and `machinectl shell` take the user as an operand, anywhere, or as the
                // value of `--uid=`, `--group=` and the like.
                return args.iter().enumerate().any(|(k, a)| {
                    names(a)
                        || a.starts_with("lunchroom@")
                        || a.split_once('=')
                            .is_some_and(|(o, v)| o.starts_with("--") && names(v))
                        || (name == "su"
                            && matches!(*a, "-g" | "--group" | "-G" | "--supp-group")
                            && args.get(k + 1).is_some_and(|v| names(v)))
                });
            }
            _ => return false,
        };
    let mut operand = 0usize;
    // An option this module does not know may take the next word: the words after it are read for
    // the user too, wherever the command starts.
    let mut unknown = false;
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        if a == "--" {
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (opt, attached) = rest
                .split_once('=')
                .map_or((rest, None), |(o, v)| (o, Some(v)));
            // getopt_long takes any unambiguous prefix of a long option.
            let Some((full, takes)) = resolve_long(opt, longs) else {
                unknown = true;
                continue;
            };
            let value = if attached.is_some() || !takes {
                attached
            } else {
                let v = args.get(k).copied();
                k += 1;
                v
            };
            if who_long.contains(&full) && value.is_some_and(names) {
                return true;
            }
            // `run0 --property=User=lunchroom` (or `Group=`).
            if full == "property"
                && value
                    .and_then(|v| v.split_once('='))
                    .is_some_and(|(key, v)| matches!(key, "User" | "Group") && names(v))
            {
                return true;
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            let cluster = &a[1..];
            for (at, c) in cluster.char_indices() {
                if value_letters.contains(c) {
                    let rest = &cluster[at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        let v = args.get(k).copied();
                        k += 1;
                        v
                    } else {
                        Some(rest)
                    };
                    if who_letters.contains(c) && value.is_some_and(names) {
                        return true;
                    }
                    break;
                }
            }
            continue;
        }
        // A variable assignment before the command (`sudo FOO=1 cmd`) is not the command.
        if a.contains('=') && matches!(name, "sudo") {
            continue;
        }
        // `runuser [options] [-] [user [argument...]]`: the first operand is the user.
        if name == "runuser" && operand == 0 && a != "-" {
            operand += 1;
            if names(a) {
                return true;
            }
            continue;
        }
        break;
    }
    unknown && args[k.min(args.len())..].iter().any(|a| names(a))
}

/// The long options of `sudo`: name, and whether it takes a value (`sudo --help`).
const SUDO_LONG: &[(&str, bool)] = &[
    ("askpass", false),
    ("background", false),
    ("bell", false),
    ("close-from", true),
    ("chdir", true),
    ("preserve-env", false),
    ("edit", false),
    ("group", true),
    ("set-home", false),
    ("help", false),
    ("host", true),
    ("login", false),
    ("remove-timestamp", false),
    ("reset-timestamp", false),
    ("list", false),
    ("non-interactive", false),
    ("preserve-groups", false),
    ("prompt", true),
    ("chroot", true),
    ("role", true),
    ("stdin", false),
    ("shell", false),
    ("type", true),
    ("command-timeout", true),
    ("other-user", true),
    ("user", true),
    ("version", false),
    ("validate", false),
];

/// The long options of `runuser` (`runuser --help`).
const RUNUSER_LONG: &[(&str, bool)] = &[
    ("user", true),
    ("group", true),
    ("supp-group", true),
    ("preserve-environment", false),
    ("whitelist-environment", true),
    ("login", false),
    ("command", true),
    ("session-command", true),
    ("fast", false),
    ("shell", true),
    ("pty", false),
    ("help", false),
    ("version", false),
];

/// The long options of `run0` (run0(1)); `--background` and `--lightweight` take their value
/// only after `=`.
const RUN0_LONG: &[(&str, bool)] = &[
    ("user", true),
    ("group", true),
    ("unit", true),
    ("property", true),
    ("description", true),
    ("slice", true),
    ("slice-inherit", false),
    ("nice", true),
    ("chdir", true),
    ("setenv", true),
    ("background", false),
    ("machine", true),
    ("shell-prompt-prefix", true),
    ("area", true),
    ("lightweight", false),
    ("pty", false),
    ("pipe", false),
    ("no-ask-password", false),
    ("help", false),
    ("version", false),
];

/// The long options of `su` and `runuser`: name, and whether it takes a value (su(1), `runuser
/// --help`).
const SU_LONG: &[(&str, bool)] = &[
    ("command", true),
    ("session-command", true),
    ("fast", false),
    ("login", false),
    ("preserve-environment", false),
    ("shell", true),
    ("group", true),
    ("supp-group", true),
    ("whitelist-environment", true),
    ("user", true),
    ("pty", false),
    ("help", false),
    ("version", false),
];

/// The long options of `script` (script(1)).
const SCRIPT_LONG: &[(&str, bool)] = &[
    ("append", false),
    ("command", true),
    ("echo", true),
    ("return", false),
    ("flush", false),
    ("force", false),
    ("log-in", true),
    ("log-out", true),
    ("log-io", true),
    ("log-timing", true),
    ("logging-format", true),
    ("output-limit", true),
    ("quiet", false),
    ("timing", false),
    ("version", false),
    ("help", false),
];

/// The long options of `flock` (flock(1)).
const FLOCK_LONG: &[(&str, bool)] = &[
    ("shared", false),
    ("exclusive", false),
    ("unlock", false),
    ("nonblock", false),
    ("no-fork", false),
    ("timeout", true),
    ("wait", true),
    ("conflict-exit-code", true),
    ("close", false),
    ("command", true),
    ("verbose", false),
    ("help", false),
    ("version", false),
];

/// The texts `su` and `runuser` run through the shell: the value of `-c`, `--command` and
/// `--session-command`, written as the programs read them: separate, attached (`-ccmd`), after
/// `=`, in a cluster of options (`-lc cmd`), a unique prefix of a long name (`--comm`), before or
/// after the user operand, and after `--` (the arguments there are the shell's).
fn su_commands<'a>(args: &[&'a str]) -> Vec<&'a str> {
    command_option_values(
        args,
        "cgGswu",
        SU_LONG,
        'c',
        &["command", "session-command"],
    )
}

/// The command texts a program is given through an option, read as getopt reads them: `short`
/// is the one-letter option that carries one and `valued` every one-letter option that takes a
/// value (so `-qc cmd` is `-q` and `-c cmd`, and `-ccmd` has the value attached); `longs` are the
/// long options (name, takes a value) and `command` the long names that carry one, each by a
/// unique prefix, with the value after `=` or as the next word.
fn command_option_values<'a>(
    args: &[&'a str],
    valued: &str,
    longs: &[(&str, bool)],
    short: char,
    command: &[&str],
) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        // After `--` the arguments go to the user's shell, which reads `-c` as su does.
        if a == "--" {
            continue;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (opt, attached) = rest
                .split_once('=')
                .map_or((rest, None), |(o, v)| (o, Some(v)));
            let Some((full, takes)) = resolve_long(opt, longs) else {
                continue;
            };
            let value = if attached.is_some() || !takes {
                attached
            } else {
                let v = args.get(k).copied();
                k += 1;
                v
            };
            if command.contains(&full) {
                out.extend(value);
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            let cluster = &a[1..];
            for (at, c) in cluster.char_indices() {
                if valued.contains(c) {
                    let rest = &cluster[at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        let v = args.get(k).copied();
                        k += 1;
                        v
                    } else {
                        Some(rest)
                    };
                    if c == short {
                        out.extend(value);
                    }
                    break;
                }
            }
        }
    }
    out
}

/// The long options of `pkexec` (pkexec(1)).
const PKEXEC_LONG: &[(&str, bool)] = &[
    ("user", true),
    ("keep-cwd", false),
    ("disable-internal-agent", false),
    ("help", false),
    ("version", false),
];

/// The option `opt` names among `longs`: the exact name, else the one name it is a prefix of.
fn resolve_long<'a>(opt: &str, longs: &'a [(&'a str, bool)]) -> Option<(&'a str, bool)> {
    if let Some(&(n, v)) = longs.iter().find(|(n, _)| *n == opt) {
        return Some((n, v));
    }
    let mut hits = longs
        .iter()
        .filter(|(n, _)| !opt.is_empty() && n.starts_with(opt));
    match (hits.next(), hits.next()) {
        (Some(&(n, v)), None) => Some((n, v)),
        _ => None,
    }
}

/// Options whose value the program runs as a command, from the programs' manuals (2026-10-27): their
/// values are judged as code, written separately, after `=`, or (one-letter options) attached. Any
/// other option's value is data. `less`'s `+` command and the words after `git bisect run` and
/// `git submodule foreach` are judged the same way; `sed` is judged as an interpreter (its
/// script, `e` command included).
const COMMAND_OPTIONS: [(&str, &[&str]); 10] = [
    (
        "git",
        &[
            // A configuration value such as `core.pager` is run.
            "-c",
            "-x",
            "--exec",
            "--extcmd",
            "--upload-pack",
            "--receive-pack",
            "--tree-filter",
            "--env-filter",
            "--index-filter",
            "--msg-filter",
            "--commit-filter",
            "--parent-filter",
            "--tag-name-filter",
        ],
    ),
    ("rg", &["--pre", "--hostname-bin"]),
    (
        "tar",
        &[
            "-I",
            "--use-compress-program",
            "--to-command",
            "-F",
            "--info-script",
            "--new-volume-script",
            "--checkpoint-action",
            "--rsh-command",
            "--rmt-command",
        ],
    ),
    ("sort", &["--compress-program"]),
    ("man", &["-P", "--pager", "-H", "--html"]),
    ("wget", &["-e", "--execute"]),
    ("cargo", &["--config"]),
    // `-o ProxyCommand=…` runs a command.
    ("ssh", &["-o"]),
    ("scp", &["-o"]),
    ("sftp", &["-o"]),
];

/// Programs that run none of their arguments as commands, except through [`COMMAND_OPTIONS`]. After
/// any other program, the words that follow are judged as if that program could run them (an
/// unknown wrapper).
const NEVER_RUNS: &[&str] = &[
    "rg",
    "git",
    "sed",
    "sort",
    "man",
    "less",
    "wget",
    "cargo",
    "tar",
    "echo",
    "printf",
    "cat",
    "ls",
    "grep",
    "egrep",
    "fgrep",
    "wc",
    "head",
    "tail",
    "uniq",
    "cut",
    "tr",
    "test",
    "[",
    "true",
    "false",
    "touch",
    "mkdir",
    "file",
    "stat",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "which",
    "type",
    "more",
    "jq",
    "diff",
    "cmp",
    "sha1sum",
    "sha224sum",
    "sha256sum",
    "sha384sum",
    "sha512sum",
    "md5sum",
    "b2sum",
    "cksum",
    "du",
    "readelf",
    "objdump",
    "nm",
    "strings",
    "xxd",
    "od",
    "hexdump",
    "nl",
    "tac",
    "rev",
    "fold",
    "tee",
    "mv",
    "cp",
    "ln",
    "chmod",
    "chown",
    "curl",
    "cd",
    "pwd",
    "date",
    "rm",
    "gh",
    "lunchroom",
    // Their command is the text of `-c`, judged where it is read (`nested_at`).
    "runuser",
    "su",
];

/// `word` is longer than the system takes as a path, or holds a name longer than it takes.
fn unspellable(word: &str) -> bool {
    word.len() >= 4096 || word.split('/').any(|name| name.len() > 255)
}

/// How deep `sh -c` strings and scripts are followed.
const MAX_NESTING: usize = 4;

/// The most severe class seen so far (the one shown), and every PROTECTED kind seen: the policy
/// follows the kinds, so choosing a class to show never discards a restriction (P1.6, R3.1).
#[allow(clippy::struct_excessive_bools)] // independent facts about one call, read separately
struct Acc {
    class: CallClass,
    kinds: ProtectedKinds,
    /// A command, or text a script or code would run, starts a new agent session
    /// ([`starts_session`]). Kept apart from `kinds` because a script is classified as an agent
    /// that is not HELD would be (K1.8): it is PROTECTED only for a HELD agent, decided at the end
    /// of the call.
    new_session: bool,
    /// The call is a reply (§6 item 1): its words are text to send, which it opens as no file.
    reply: bool,
    /// V1.4: the path, program, word or text at fault for the DENIED class shown, as the guard
    /// read it; set by the rule that made the class shown.
    fault: Option<String>,
    /// How many path-like strings this call has had resolved against the file system
    /// ([`MAX_RESOLVED`]).
    resolved: usize,
    /// What the call reads, searches and writes ([`Access`]).
    access: Access,
    /// The call has more path-like strings than the guard resolves ([`MAX_RESOLVED`]) and is not a
    /// reply: DENIED at the end of the call unless a narrower rule already denied it.
    too_many: bool,
    /// The statements of a script the reader could not read whole are being judged one by one: the
    /// folder each runs in is not known (a `cd` may sit in a construct the reader skipped), so a
    /// program named by a relative path is not required to exist.
    lenient: bool,
    /// The files this call writes with text it shows (P1.4: a file written and then run in one
    /// call is judged on the text written).
    written: Vec<WrittenFile>,
}

/// How a command uses the path a word names ([`Access`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Read,
    Write,
    Both,
    /// Judged by the path rules, but no path the call reads or writes: the name of a program the
    /// system finds through `PATH`.
    Skip,
}

impl Acc {
    /// Records `paths` in the call's access, as read, written or both.
    fn note(&mut self, dir: Dir, paths: &[PathBuf]) {
        for p in paths {
            if matches!(dir, Dir::Read | Dir::Both) {
                self.access.reads.insert(p.clone());
            }
            if matches!(dir, Dir::Write | Dir::Both) {
                self.access.writes.insert(p.clone());
            }
        }
    }
}

/// A file one call writes, for the commands of the call that run it.
struct WrittenFile {
    /// The file as the call spells it.
    literal: String,
    /// The file, made absolute and lexically normal from the call's working folder.
    path: PathBuf,
    /// The text written, where the call shows it whole.
    body: Option<String>,
    /// The text is being judged now (a file that runs itself is judged once).
    active: bool,
}

/// The words of one piece of text that run a file: a program named by a path, the script a shell or
/// an interpreter is given, and the file `source` reads, each with how it is run.
fn run_targets(t: &[&str]) -> Vec<(usize, RunAs)> {
    let positions = program_positions(t);
    let runnable = runnable_positions(t, &positions);
    let mut out: Vec<(usize, RunAs)> = positions
        .iter()
        .filter(|&&p| t[p].contains('/'))
        .map(|&p| (p, RunAs::Auto))
        .collect();
    for k in script_positions(t, &runnable) {
        let runner = runnable
            .iter()
            .rev()
            .find(|&&p| p < k)
            .map(|&p| basename(t[p]));
        let how = if runner.is_some_and(|n| SHELLS.contains(&n)) {
            RunAs::Shell
        } else {
            RunAs::Code
        };
        out.push((k, how));
    }
    for &p in &positions {
        if matches!(basename(t[p]), "source" | ".") && p + 1 < t.len() {
            out.push((p + 1, RunAs::Shell));
        }
    }
    out
}

/// How a command runs a file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunAs {
    /// By its path or its `#!` line, or its name's ending.
    Auto,
    /// As a script of a shell.
    Shell,
    /// As code of an interpreter.
    Code,
}

/// Text written to a file is code of an interpreter, not a shell script: it starts with a `#!`
/// line naming one that is no shell, or (with no such line) its name ends as a program of one does.
fn script_is_code(text: &str, name: &str) -> bool {
    if let Some(line) = text.strip_prefix("#!") {
        let first = line.lines().next().unwrap_or_default();
        let mut words = first.split_whitespace();
        let mut prog = words.next().map(basename);
        if prog == Some("env") {
            prog = words.find(|a| !a.starts_with('-') && !is_assignment(a));
        }
        return !prog.is_some_and(|n| SHELLS.contains(&n));
    }
    Path::new(name).extension().is_some_and(|e| {
        matches!(
            e.to_str().unwrap_or_default(),
            "py" | "pl" | "rb" | "js" | "mjs" | "cjs" | "php" | "lua" | "r" | "ts" | "tcl" | "awk"
        )
    })
}

/// One statement of a shell script, ready to be judged as a command of its own: the control words
/// of the shell off its front, a function's header off, `for x in …` as the words it lists, a `[[ … ]]`
/// test as the words in it; `None` for nothing left (a comment, `fi`, `done`, `{`).
fn script_piece(raw: &str) -> Option<String> {
    const CONTROL: [&str; 17] = [
        "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
        "esac", ";;", "in", "function",
    ];
    let mut s = raw.trim();
    loop {
        let before = s;
        // `name() {`, `name () {`
        if let Some(open) = s.find("()") {
            let name = s[..open].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
            {
                s = s[open + 2..].trim_start();
            }
        }
        let first = s.split_whitespace().next().unwrap_or_default();
        if CONTROL.contains(&first) {
            s = s[first.len()..].trim_start();
        }
        if s == before {
            break;
        }
    }
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    // `for x in a b c` / `select x in a b c`: the words listed are data the loop hands on.
    if let Some(rest) = s.strip_prefix("for ").or_else(|| s.strip_prefix("select ")) {
        return Some(match rest.split_once(" in ") {
            Some((_, list)) => format!(": {}", list.trim()),
            None => ":".to_owned(),
        });
    }
    if let Some(inner) = s.strip_prefix("[[") {
        return Some(format!(": {}", inner.trim().trim_end_matches("]]").trim()));
    }
    Some(s.to_owned())
}

/// What `text` writes and reads, per word, for the words of one command: a redirect target that
/// opens its file for writing, and the operands the command's program writes ([`Access`]).
fn word_dirs(words: &[Word], texts: &[&str], positions: &[usize]) -> Vec<Dir> {
    let mut dirs: Vec<Dir> = words
        .iter()
        .enumerate()
        .map(|(k, w)| {
            if w.write {
                Dir::Write
            } else if positions.contains(&k) && !w.text.contains('/') && !w.redirect {
                Dir::Skip
            } else {
                Dir::Read
            }
        })
        .collect();
    for &p in positions {
        let name = basename(texts[p]);
        let idx: Vec<usize> = (p + 1..texts.len())
            .filter(|&k| !words[k].redirect)
            .collect();
        let args: Vec<&str> = idx.iter().map(|&k| texts[k]).collect();
        for (i, both) in writes::operand_writes(name, &args) {
            dirs[idx[i]] = if both { Dir::Both } else { Dir::Write };
        }
    }
    dirs
}

/// The end of the path segment that begins `s`: its first `/`, white space, quote or separator
/// outside a `$(…)`, a `${…}` and a pair of backquotes.
fn segment_end(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut depth, mut tick, mut i) = (0usize, false, 0);
    while i < b.len() {
        match b[i] {
            b'`' => tick = !tick,
            b'$' if matches!(b.get(i + 1), Some(b'(' | b'{')) => {
                depth += 1;
                i += 1;
            }
            b'(' | b'{' if depth > 0 => depth += 1,
            b')' | b'}' if depth > 0 => depth -= 1,
            c if depth == 0
                && !tick
                && (c == b'/'
                    || c.is_ascii_whitespace()
                    || matches!(c, b'"' | b'\'' | b';' | b'&' | b'|')) =>
            {
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// A segment that is not written out: a variable, a substitution, a wildcard, `$$`.
fn unwritten_segment(seg: &str) -> bool {
    seg.contains(['$', '`', '{', '(', '*', '?', '['])
}

/// §17.5: a path `/proc/<pid>/{mem,environ,fd,maps}` (or `/proc/<pid>/task/<tid>/…`) in text the
/// guard cannot read in full, whose process id (or thread id) is not written: a variable, a
/// substitution, `$$`. The path as the call wrote it.
fn proc_secret_unwritten(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(k) = rest.find("/proc/") {
        let after = &rest[k + "/proc/".len()..];
        let (seg, tail) = after.split_at(segment_end(after));
        if let Some(next) = tail.strip_prefix('/') {
            let (mut name, mut after_name) = next.split_at(segment_end(next));
            let mut unwritten = unwritten_segment(seg);
            // `/proc/<pid>/task/<tid>/environ`
            if name == "task"
                && let Some(t) = after_name.strip_prefix('/')
            {
                let (tid, tail) = t.split_at(segment_end(t));
                unwritten |= unwritten_segment(tid);
                let u = tail.strip_prefix('/').unwrap_or_default();
                (name, after_name) = u.split_at(segment_end(u));
            }
            let _ = after_name;
            if unwritten && matches!(name, "mem" | "environ" | "fd" | "maps") {
                return Some(format!("/proc/{seg}/{name}"));
            }
        }
        rest = after;
    }
    None
}

const fn rank(c: CallClass) -> u8 {
    match c {
        CallClass::InSlip => 0,
        CallClass::Ordinary => 1,
        // F1.2: when text in the command could not be read, `unparsed` is the reason shown within
        // its class, whichever rule ran first. A DENIED call stays above every PROTECTED one; a
        // denial under another rule (P1.4's store writes) keeps its own reason.
        CallClass::Protected(ProtectedKind::Unparsed) => 3,
        CallClass::Protected(_) => 2,
        CallClass::Denied(DenyReason::Unparsed) => 5,
        CallClass::Denied(_) => 4,
    }
}

impl Acc {
    const fn new(class: CallClass) -> Self {
        let call = Call::of(class);
        Self {
            class: call.class,
            kinds: call.kinds,
            new_session: false,
            reply: false,
            fault: None,
            resolved: 0,
            access: Access {
                reads: BTreeSet::new(),
                searches: BTreeSet::new(),
                writes: BTreeSet::new(),
            },
            written: Vec::new(),
            too_many: false,
            lenient: false,
        }
    }

    /// Takes what a part of the call (a script it runs) found: DENIED with what it found at
    /// fault, every PROTECTED kind and class, a new session, the paths resolved and the access.
    fn absorb(&mut self, sub: Self) {
        // The reason a rule gave first stays the one shown (F1.2: `unparsed` is the reason within
        // its class only where no rule named a narrower one).
        if !self.denied() {
            self.deny_from(&sub);
        }
        self.kinds = self.kinds.union(sub.kinds);
        if let CallClass::Protected(_) = sub.class {
            self.raise(sub.class);
        }
        self.new_session |= sub.new_session;
        self.too_many |= sub.too_many;
        self.resolved = sub.resolved;
        self.access.extend(sub.access);
        self.written = sub.written;
    }

    /// The file written by this call under the spelling `f`, from `here`.
    fn written_as(&self, f: &str, here: &Path, home: &Path) -> Option<&WrittenFile> {
        let path = normalize(f, here, home);
        self.written
            .iter()
            .find(|w| w.path == path || w.literal == f)
    }

    /// Raises DENIED for `r`, noting `what` (V1.4) when this rule's class is the one shown and no
    /// earlier rule of that class noted its own.
    fn deny(&mut self, r: DenyReason, what: &str) {
        let c = CallClass::Denied(r);
        let before = self.class;
        self.raise(c);
        if self.class == c && (before != c || self.fault.is_none()) {
            self.fault = Some(what.to_owned());
        }
    }

    /// Takes a DENIED class `sub` reached on text this call runs, with what it found at fault.
    fn deny_from(&mut self, sub: &Self) {
        if let CallClass::Denied(r) = sub.class {
            self.deny(r, sub.fault.as_deref().unwrap_or_default());
            if self.fault.as_deref() == Some("") {
                self.fault = None;
            }
        }
    }

    fn raise(&mut self, c: CallClass) {
        // Every PROTECTED kind is remembered, whichever class ends up shown.
        if let CallClass::Protected(kind) = c {
            self.kinds = self.kinds.with(kind);
        }
        if rank(c) > rank(self.class) {
            self.class = c;
        }
    }

    const fn denied(&self) -> bool {
        matches!(self.class, CallClass::Denied(_))
    }

    const fn call(&self) -> Call {
        // P1.4: a command the guard cannot parse is PROTECTED (F1.2) and, since Lunchroom cannot
        // read it to the end, also GATED: the call carries both kinds.
        let kinds = if self.kinds.contains(ProtectedKind::Unparsed) {
            self.kinds.with(ProtectedKind::Gated)
        } else {
            self.kinds
        };
        Call {
            class: self.class,
            kinds,
        }
    }
}

/// The paths one word may mean (see [`Guard::forms`]).
#[derive(Default)]
struct Forms {
    /// Every path, once.
    paths: Vec<PathBuf>,
    /// The folder to search from when a wildcard's matching was cut short.
    capped: Option<PathBuf>,
    /// The file could not be settled: it may be anywhere.
    unresolved: bool,
    /// It was left unsettled because a look at it was cut short by the time (L1.1: one Lunchroom
    /// could not check), not because it cannot be followed. Known per path, never from the call's
    /// other paths.
    timed_out: bool,
}

impl Forms {
    fn push(&mut self, p: PathBuf) {
        if !self.paths.contains(&p) {
            self.paths.push(p);
        }
    }
}

/// A path split before its first part with a wildcard: the folders before it, and that part with
/// all that follows.
fn split_wild(p: &Path) -> (PathBuf, PathBuf) {
    let (mut head, mut tail) = (PathBuf::new(), PathBuf::new());
    let mut wild = false;
    for c in p.components() {
        wild |= matches!(c, Component::Normal(s) if s.to_string_lossy().contains(['*', '?', '[']));
        if wild {
            tail.push(c.as_os_str());
        } else {
            head.push(c.as_os_str());
        }
    }
    (head, tail)
}

fn basename(t: &str) -> &str {
    Path::new(t)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(t)
}

/// `NAME=value`, or Bash's append form `NAME+=value`.
/// The word at a program's place in a piece of shell text, past any `NAME=value` (a quoted value
/// whole, spaces included), without its quotes.
fn program_word(piece: &str) -> Option<&str> {
    // The length of the shell word `s` starts with: to the first space outside quotes.
    let word_len = |s: &str| {
        let mut quote = None;
        let mut cs = s.char_indices();
        while let Some((k, c)) = cs.next() {
            match (quote, c) {
                (None, ' ' | '\t') => return k,
                (None, '"' | '\'') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (Some('"') | None, '\\') => {
                    cs.next();
                }
                _ => {}
            }
        }
        s.len()
    };
    let mut rest = piece.trim_start();
    while !rest.is_empty() {
        let n = word_len(rest);
        let word = &rest[..n];
        if !is_assignment(word) {
            return Some(word.trim_matches(['"', '\'']));
        }
        rest = rest[n..].trim_start();
    }
    None
}

fn is_assignment(w: &str) -> bool {
    let Some((name, _)) = w.split_once('=') else {
        return false;
    };
    let name = name.strip_suffix('+').unwrap_or(name);
    let mut cs = name.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_identity(t: &str) -> bool {
    IDENTITY_VARS.iter().any(|v| {
        t.strip_prefix(v)
            .is_some_and(|r| r.is_empty() || r.starts_with('=') || r.starts_with("+="))
    })
}

/// A local agent session chat API: `/api/sessions/<id>/chat` (F1.1).
fn names_chat_api(t: &str) -> bool {
    const KEY: &str = "/api/sessions/";
    let mut rest = t;
    while let Some(k) = rest.find(KEY) {
        let after = &rest[k + KEY.len()..];
        let id_len = after
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(after.len());
        if id_len > 0 && after[id_len..].starts_with("/chat") {
            return true;
        }
        rest = after;
    }
    false
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "._/~+@%,=-*?[]".contains(c)
}

fn is_path_char(c: char) -> bool {
    c != '=' && is_word_char(c)
}

/// The whole words of a text the shell reader could not read (F1.2: "as a whole word").
fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !is_word_char(c))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The most path-like strings one call has looked up in the file system (links, wildcards,
/// folders). Past it a string is judged by its text alone, which still finds a store or transcript
/// path written out in full, and the call is DENIED in every session (§11 L1.1) with a message that
/// says to split it, unless it is a reply, whose words are text to send, not files: a HELD reply is
/// PROTECTED as unparsed for the strings left unread.
const MAX_RESOLVED: usize = 1_000;

/// What a call with more than [`MAX_RESOLVED`] path-like strings is DENIED for, as the message
/// shows it (V1.4, the part at fault). The reason is `unparsed`: the guard cannot read the call in
/// full.
pub const TOO_MANY_PATHS: &str =
    "more than 1,000 path-like strings in one call; split it into smaller calls";

/// The path-like strings in one word: the word, its parts, and each part from its first `/` or `~`;
/// each once (a set, not a search of the list: a text of thousands of words took time in
/// proportion to the square of their number).
fn candidates(text: &str) -> Vec<&str> {
    fn add<'a>(t: &'a str, seen: &mut HashSet<&'a str>, out: &mut Vec<&'a str>) {
        if !t.is_empty() && !t.starts_with('-') && seen.insert(t) {
            out.push(t);
        }
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    add(text, &mut seen, &mut out);
    for part in text.split(|c: char| !is_path_char(c)) {
        add(part, &mut seen, &mut out);
        if let Some(k) = part.find(['/', '~']) {
            add(&part[k..], &mut seen, &mut out);
        }
    }
    out
}

/// Every name a word may run as: its base name, or for a wildcard the known names it matches.
fn prog_names(w: &Word) -> Vec<&str> {
    let base = basename(&w.text);
    if !w.glob {
        return vec![base];
    }
    FENCE_PROGRAMS
        .iter()
        .chain(&LUNCHROOM_NAMES)
        .chain(&SHELLS)
        .chain(&INTERPRETERS)
        .chain(&NETWORK_PROGRAMS)
        .chain(&PUBLISHERS)
        .chain(&DELETERS)
        .chain(&RECURSIVE_WRITE)
        .chain(&SEARCHERS)
        .chain(&RECURSE_ON_FLAG)
        .chain(&COPIERS)
        .chain(&["git", "env", "printenv", "eval", "source"])
        .copied()
        .filter(|n| fnmatch(base, n))
        .collect()
}

/// One of `set` is among the options (`--name` or `--name=value`).
fn has_flag(args: &[&str], set: &[&str]) -> bool {
    args.iter()
        .any(|a| set.contains(&a.split('=').next().unwrap_or(a)))
}

/// P1.4: Claude Code is started without the user's settings, which hold the guard's hooks:
/// `--bare`, or `--setting-sources` with a list (comma-separated) that does not name `user`.
fn without_user_settings(args: &[&str]) -> bool {
    has_flag(args, &["--bare"])
        || args.iter().enumerate().any(|(k, a)| {
            let list = if *a == "--setting-sources" {
                Some(args.get(k + 1).copied().unwrap_or_default())
            } else {
                a.strip_prefix("--setting-sources=")
            };
            list.is_some_and(|l| !l.split(',').any(|s| s.trim() == "user"))
        })
}

/// One of `letters` is in a cluster of short options (`-rc`).
fn has_cluster(args: &[&str], letters: &[char]) -> bool {
    args.iter().any(|a| {
        a.strip_prefix('-')
            .is_some_and(|r| !r.starts_with('-') && r.chars().any(|c| letters.contains(&c)))
    })
}

/// One of the long options in `names` (each with the shortest prefix of it that counts) is among
/// the options, in any spelling `argparse` accepts (`--res`, `--resume=x`). Hermes does not turn
/// prefixes off.
fn has_abbrev(args: &[&str], names: &[(&str, usize)]) -> bool {
    args.iter().any(|a| {
        let name = a.split('=').next().unwrap_or(a);
        name.starts_with("--")
            && names
                .iter()
                .any(|(full, min)| name.len() >= *min && full.starts_with(name))
    })
}

/// A command that puts a turn into another agent's session (F1.1).
fn fence_args(prog: &str, args: &[&str]) -> bool {
    let flag = |set: &[&str]| has_flag(args, set);
    let cluster = |letters: &[char]| has_cluster(args, letters);
    match prog {
        "codex" => args.contains(&"queue") || args.contains(&"resume"),
        "claude" => flag(&["--resume", "--continue"]) || cluster(&['r', 'c']),
        // `--resume`/`-r <id|title|latest>` and `--continue`/`-c [name]` run the query of `chat -q`
        // or top-level `-z` in that session (`hermes_cli/oneshot.py:437-465`,
        // `hermes_cli/main.py:1403-1470,1511-1560`, hermes-agent 9976f48). No other long option
        // starts `--res` or `--co`.
        "hermes" => has_abbrev(args, &[("--resume", 5), ("--continue", 4)]) || cluster(&['r', 'c']),
        // `run`, the default interface (`--prompt`) and `attach` all take `-s`/`--session` and
        // `-c`/`--continue` to reach an existing session (`opencode --help`, 2026-10-26).
        "opencode" => flag(&["--session", "--continue"]) || cluster(&['s', 'c']),
        // F1.1: a command naming `agent` (`openclaw agent`, `gateway call agent`), `sessions_send`
        // or `acp` (which bridges prompts into a session), and `cron add`/`edit` (scheduled turns).
        // Word presence, not position: an option's value would shift positions.
        "openclaw" => {
            args.contains(&"agent")
                || args.contains(&"sessions_send")
                || args.contains(&"acp")
                || (args.contains(&"cron") && (args.contains(&"add") || args.contains(&"edit")))
        }
        _ => false,
    }
}

/// A command that starts a new agent session on a prompt of its own (P1.4): a harness's
/// non-interactive forms (`claude -p`, `codex exec`, `hermes chat -q`, `opencode run`) and a prompt
/// given on the command line. That session is guarded but not HELD, so it would run what its parent
/// may not. Resuming or continuing an existing one is a fence ([`fence_args`]), which ranks above.
///
/// `bare`: a prompt given with no option or command counts. It does for a command; it does not for
/// the words of a script or code, where a name followed by any word is prose (`claude is good`).
#[must_use]
pub fn starts_session(prog: &str, args: &[&str], bare: bool) -> bool {
    let first = args.iter().find(|a| !a.starts_with('-'));
    match prog {
        // `claude [options] [command] [prompt]`: `-p`/`--print`, `--bg`, or a first word that is
        // not one of its commands. `--help` and `--version` start nothing. P1.4: Claude Code
        // started without the user's settings, which hold the guard's hooks, is one as well.
        "claude" => {
            !has_flag(args, &["--help", "-h", "--version", "-v"])
                && (has_flag(args, &["--print", "--bg", "--background"])
                    || has_cluster(args, &['p'])
                    || without_user_settings(args)
                    || (bare && first.is_some_and(|a| !NOT_A_PROMPT.contains(a))))
        }
        // `codex [OPTIONS] [PROMPT]`; `exec`, `e`, `review` and `fork` are not in `NOT_A_PROMPT`.
        // P1.4: each option before the first word is passed over as the option table of Codex
        // 0.163.0 defines it (`codex_cli`), so `codex -m <model> exec` is recognised; an option
        // that table does not hold makes the command count as starting a session.
        "codex" => {
            !has_flag(args, &["--help", "-h", "--version", "-V"])
                && match crate::codex_cli::first_word(args) {
                    crate::codex_cli::FirstWord::UnknownOption => true,
                    crate::codex_cli::FirstWord::None => false,
                    crate::codex_cli::FirstWord::Word(at) => {
                        let a = args[at];
                        if bare {
                            !NOT_A_PROMPT.contains(&a)
                        } else {
                            matches!(a, "exec" | "e" | "review" | "fork")
                        }
                    }
                }
        }
        // `opencode run [message..]`, `opencode pr <n>`, and the default interface's `--prompt`.
        "opencode" => has_flag(args, &["--prompt"]) || matches!(first, Some(&("run" | "pr"))),
        // `chat -q|--query|--query-file`, top-level `-z|--oneshot` (`hermes_cli/_parser.py:148,
        // 233-247`, hermes-agent 9976f48).
        "hermes" => {
            has_cluster(args, &['q', 'z'])
                || has_abbrev(
                    args,
                    &[("--query", 5), ("--query-file", 5), ("--oneshot", 3)],
                )
        }
        _ => false,
    }
}

/// What script or code text runs, as far as its words show it (F1, I1.2, A1.3; spec §15 K1.8),
/// judged statement by statement: an identity variable named or the environment cleared
/// ([`changes_identity`]), a channel's name followed later in the statement by its injecting word
/// (`codex` … `queue`), a Lunchroom command only the user may run, or a local session chat API. A
/// name alone (a comment, a message) is not a match.
fn code_rules(text: &str) -> Option<DenyReason> {
    code_rules_as_written(text)
        .or_else(|| escapes::decode(text).and_then(|d| code_rules_as_written(&d)))
}

/// [`code_rules`] on the text as it stands.
fn code_rules_as_written(text: &str) -> Option<DenyReason> {
    // I1.2 first: an identity variable named, or the environment cleared, in any statement.
    if mentions_identity(text) {
        return Some(DenyReason::Fence);
    }
    // F1.4: `TIOCSTI` or its number types into a terminal's input.
    if names_tiocsti(text) {
        return Some(DenyReason::Configuration);
    }
    for stmt in statements(text) {
        // Words with a letter or digit in them; quotes, commas and brackets between are skipped.
        let words: Vec<&str> = tokens(stmt)
            .into_iter()
            .filter(|t| t.chars().any(char::is_alphanumeric))
            .collect();
        for (k, t) in words.iter().enumerate() {
            let name = basename(t);
            let after = words.get(k + 1..).unwrap_or_default();
            if FENCE_PROGRAMS.contains(&name) && fence_args(name, after) {
                return Some(DenyReason::Fence);
            }
            if names_user_only_command(name, after) {
                return Some(DenyReason::Configuration);
            }
        }
    }
    names_chat_api(text).then_some(DenyReason::Fence)
}

/// A Lunchroom command only the user may run (A1.3; §15 limit 8: a mention counts): `name` is
/// Lunchroom's, and `after` begins with one of [`LUNCHROOM_USER_ONLY`]. After help (`--help
/// approve`) a user-only command still counts. Any other word after the name runs nothing:
/// Lunchroom takes its command from its first argument and prints its usage for anything else
/// (main.rs), so `--repo epic-ai-io/lunchroom --pattern x` is not a Lunchroom command.
fn names_user_only_command(name: &str, after: &[&str]) -> bool {
    let lead = after
        .iter()
        .take_while(|n| matches!(**n, "help" | "--help" | "-h" | "--version"))
        .count();
    LUNCHROOM_NAMES.contains(&name)
        && after
            .get(lead)
            .is_some_and(|n| LUNCHROOM_USER_ONLY.contains(n))
}

/// §7 A1.3: script or code text, other than Lunchroom's own program, mentions a Lunchroom command
/// only the user may run, anywhere in its text, a string it only prints included.
fn lunchroom_mention(text: &str) -> Option<String> {
    lunchroom_mention_as_written(text)
        .or_else(|| escapes::decode(text).and_then(|d| lunchroom_mention_as_written(&d)))
}

/// [`lunchroom_mention`] on the text as it stands.
fn lunchroom_mention_as_written(text: &str) -> Option<String> {
    statements(text).into_iter().find_map(|stmt| {
        let words: Vec<&str> = tokens(stmt)
            .into_iter()
            .filter(|t| t.chars().any(char::is_alphanumeric))
            .collect();
        words.iter().enumerate().find_map(|(k, t)| {
            let after = words.get(k + 1..).unwrap_or_default();
            names_user_only_command(basename(t), after).then(|| {
                let lead = after
                    .iter()
                    .take_while(|n| matches!(**n, "help" | "--help" | "-h" | "--version"))
                    .count();
                format!("{t} {}", after[lead])
            })
        })
    })
}

/// Code names `ptrace`, or calls `syscall` with the number of `ptrace` (26 on 32-bit x86 and Arm,
/// 101 on x86-64, 117 on 64-bit Arm).
///
/// The target of a call is set when the code runs, and the guard judges code by its text (K1.8), so
/// the text is read as written, with its escapes decoded and with the pieces of a string joined
/// (`'pt' + 'race'`, `chr(112) + …`); a system call whose number is not one integer literal
/// (`syscall(n, …)`, `syscall(100 + 1, …)`) and a name written reversed or encoded (rot13,
/// base64, hex) count as well, since no text shows which call they make.
fn names_ptrace(text: &str) -> bool {
    let mut views = vec![text.to_owned()];
    views.extend(escapes::decode(text));
    for k in 0..views.len() {
        views.extend(escapes::fold(&views[k]));
    }
    views.iter().any(|v| names_ptrace_as_written(v))
}

/// [`names_ptrace`] on the text as it stands.
fn names_ptrace_as_written(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    // `ptrace` as it is, reversed, in rot13, in base64 (`cHRyYWNl`) and in hex.
    lower.contains("ptrace")
        || lower.contains("ecartp")
        || lower.contains("cgenpr")
        || text.contains("cHRyYWNl")
        || text.contains("UFRSQUNF")
        || lower.contains("707472616365")
        || syscall_number_unwritten(&lower)
        || statements(text).into_iter().any(|stmt| {
            let words = number_words(stmt);
            stmt.to_ascii_lowercase().contains("syscall")
                && words.iter().any(|n| matches!(n, 26 | 101 | 117))
        })
}

/// The commands a debugger is given on its command line: the value of `-ex`, `-iex`,
/// `--eval-command`, `--init-eval-command` (gdb) and `-o`, `-O`, `--one-line`,
/// `--one-line-before-file` (lldb), after a space or an `=`.
fn debugger_payloads<'a>(args: &[&'a str]) -> Vec<&'a str> {
    const OPTIONS: [&str; 8] = [
        "-ex",
        "-iex",
        "-eval-command",
        "-init-eval-command",
        "-o",
        "-O",
        "-one-line",
        "-one-line-before-file",
    ];
    let mut out = Vec::new();
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        let plain = a.trim_start_matches('-');
        let (name, attached) = match plain.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (plain, None),
        };
        if a.starts_with('-') && OPTIONS.iter().any(|o| o.trim_start_matches('-') == name) {
            if let Some(v) = attached {
                out.push(v);
            } else if let Some(v) = args.get(k).copied() {
                out.push(v);
                k += 1;
            }
        }
    }
    out
}

/// Code calls `syscall(` with a first argument that is not one integer literal: its number is a
/// variable or an expression, which may be that of `ptrace`.
fn syscall_number_unwritten(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(k) = rest.find("syscall") {
        let after = rest[k + "syscall".len()..].trim_start();
        rest = &rest[k + "syscall".len()..];
        let Some(args) = after.strip_prefix('(') else {
            continue;
        };
        let mut depth = 0usize;
        let end = args
            .char_indices()
            .find(|(_, c)| match c {
                '(' | '[' | '{' => {
                    depth += 1;
                    false
                }
                ')' | ']' | '}' => {
                    if depth == 0 {
                        return true;
                    }
                    depth -= 1;
                    false
                }
                ',' => depth == 0,
                _ => false,
            })
            .map_or(args.len(), |(i, _)| i);
        let first = args[..end].trim();
        let literal = first.starts_with(|c: char| c.is_ascii_digit())
            && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && number_words(first).len() == 1;
        if !first.is_empty() && !literal {
            return true;
        }
    }
    false
}

/// The values of the integer literals in `text` (decimal, `0x`, `0o`, `0b`, a leading `0` for
/// octal), whatever the base they are written in. A literal too large for 64 bits is left out.
fn number_words(text: &str) -> Vec<u64> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter_map(|w| {
            let w = w.replace('_', "");
            let lower = w.to_ascii_lowercase();
            let (digits, radix) = if let Some(h) = lower.strip_prefix("0x") {
                (h.to_owned(), 16)
            } else if let Some(o) = lower.strip_prefix("0o") {
                (o.to_owned(), 8)
            } else if let Some(b) = lower.strip_prefix("0b") {
                (b.to_owned(), 2)
            } else if lower.len() > 1 && lower.starts_with('0') {
                (lower[1..].to_owned(), 8)
            } else {
                (lower, 10)
            };
            // A suffix such as `L` or `UL` on a C-style literal.
            let digits = digits.trim_end_matches(['l', 'u']);
            u64::from_str_radix(digits, radix).ok()
        })
        .collect()
}

/// §12 F1.4: code names `TIOCSTI`, the ioctl that types into a terminal's input, or its number
/// (`0x5412`, `21522`, or the same value in any other base).
fn names_tiocsti(text: &str) -> bool {
    names_tiocsti_as_written(text)
        || escapes::decode(text).is_some_and(|d| names_tiocsti_as_written(&d))
}

/// [`names_tiocsti`] on the text as it stands.
fn names_tiocsti_as_written(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("tiocsti"))
        || number_words(text).contains(&0x5412)
}

/// Script or code text names a program that starts a new agent session, in a statement, as
/// [`code_rules`] reads one ([`starts_session`] without a bare prompt).
fn code_starts_session(text: &str) -> bool {
    statements(text).into_iter().any(|stmt| {
        let words: Vec<&str> = tokens(stmt)
            .into_iter()
            .filter(|t| t.chars().any(char::is_alphanumeric))
            .collect();
        words.iter().enumerate().any(|(k, t)| {
            let name = basename(t);
            FENCE_PROGRAMS.contains(&name)
                && starts_session(name, words.get(k + 1..).unwrap_or_default(), false)
        })
    }) || passes_a_prompt(text)
}

/// The spans of the quoted strings in code, each from its opening quote to its closing one.
fn quoted_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut open = None;
    for (i, c) in text.char_indices() {
        match open {
            None if matches!(c, '\'' | '"') => open = Some((c, i)),
            Some((q, start)) if c == q => {
                spans.push((start, i));
                open = None;
            }
            _ => {}
        }
    }
    spans
}

/// Code runs `claude` or `codex` on a prompt given as a string: the name is the first string of a
/// list or call and the first string after it in that list or call that is no option is no
/// command of theirs (`run(['claude', 'do it'])`, `spawn("codex", [/* x */ "do it"])`). Prose in
/// a string names no list.
fn passes_a_prompt(text: &str) -> bool {
    let spans = quoted_spans(text);
    spans.iter().enumerate().any(|(k, (start, end))| {
        let name = basename(&text[start + 1..*end]);
        if !matches!(name, "claude" | "codex") || !text[..*start].trim_end().ends_with(['[', '(']) {
            return false;
        }
        let mut from = *end;
        let mut after_option = false;
        for (s, e) in &spans[k + 1..] {
            let between = &text[from + 1..*s];
            if !between.contains(',') || between.contains([')', ']', ';']) {
                return false;
            }
            let word = &text[s + 1..*e];
            // A word after an option may be that option's value (`--cd config`), so it decides
            // nothing unless it can only be a prompt.
            let command = NOT_A_PROMPT.contains(&word) || FENCE_PROGRAMS.contains(&word);
            let option = word.starts_with('-');
            let value = after_option && command;
            if !option && !value {
                return !word.is_empty() && !command;
            }
            after_option = option;
            from = *e;
        }
        false
    })
}

/// The PROTECTED kinds ([`protected_kind`]) of the commands that code spells out as strings: each
/// quoted string read as a command (`os.system('git push')`), and each list of strings as the
/// words of one (`run(['git', 'push'])`). Code builds a command in ways no text shows, which only
/// fewer kinds are found in; a string that reads as a command of one of the kinds counts, whatever
/// the code does with it.
fn code_kinds(text: &str) -> Vec<ProtectedKind> {
    let mut out: Vec<ProtectedKind> = Vec::new();
    let mut judge = |command: &str| {
        let Parsed::Commands(cmds) = shell::parse(command, "/") else {
            return;
        };
        for c in &cmds {
            let t: Vec<&str> = c.words.iter().map(|w| w.text.as_str()).collect();
            for p in program_positions(&t) {
                if let Some(kind) = protected_kind(basename(t[p]), &t[p + 1..])
                    && !out.contains(&kind)
                {
                    out.push(kind);
                }
            }
        }
    };
    let spans = quoted_spans(text);
    for (start, end) in &spans {
        judge(&text[start + 1..*end]);
    }
    // A list or a call's arguments: strings separated by commas only, after `[` or `(`.
    for (k, (start, end)) in spans.iter().enumerate() {
        if !text[..*start].trim_end().ends_with(['[', '(']) {
            continue;
        }
        let mut words = vec![&text[start + 1..*end]];
        let mut from = *end;
        for (s, e) in &spans[k + 1..] {
            let between = &text[from + 1..*s];
            if !between.contains(',') || between.contains([')', ']', ';']) {
                break;
            }
            words.push(&text[s + 1..*e]);
            from = *e;
        }
        if words.len() > 1 {
            judge(&words.join(" "));
        }
    }
    out
}

/// The most files one `apply_patch` call is judged by. A patch naming more is judged by the first
/// this many and the HELD call is then PROTECTED as unparsed.
const MAX_PATCH_PATHS: usize = 200;

/// The files a Codex `apply_patch` patch writes, each once, in order, and whether more than
/// [`MAX_PATCH_PATHS`] were named. Codex's own parser takes a line, trimmed, that begins `*** Add
/// File: `, `*** Delete File: `, `*** Update File: ` or `*** Move to: ` and reads the rest as the
/// path (openai/codex tag `rust-v0.166.0`, `codex-rs/apply-patch/src/streaming_parser.rs` lines
/// 107-132 and 255, markers in `codex-rs/apply-patch/src/parser.rs` lines 39-42). Every other line
/// is file content, which this reads for no path.
fn patch_paths(patch: &str) -> (Vec<&str>, bool) {
    const MARKERS: [&str; 4] = [
        "*** Add File: ",
        "*** Delete File: ",
        "*** Update File: ",
        "*** Move to: ",
    ];
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for line in patch.lines() {
        let line = line.trim();
        let Some(named) = MARKERS.iter().find_map(|m| line.strip_prefix(m)) else {
            continue;
        };
        if named.is_empty() || !seen.insert(named) {
            continue;
        }
        if out.len() >= MAX_PATCH_PATHS {
            return (out, true);
        }
        out.push(named);
    }
    (out, false)
}

/// The files a Codex `apply_patch` patch writes, as the guard judges them ([`patch_paths`]), for a
/// message that says what a call was (V1.4).
#[must_use]
pub fn patch_files(patch: &str) -> Vec<&str> {
    patch_paths(patch).0
}

/// Every string in a tool's input, at any depth.
fn string_leaves<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|i| string_leaves(i, out)),
        Value::Object(map) => map.values().for_each(|i| string_leaves(i, out)),
        _ => {}
    }
}

/// Text the shell parser could not read, split where a command can start (`;`, `&`, `|`, a
/// newline, a backquote), but never at such a character inside single or double quotes or after a
/// backslash: quoted text is one word (field, 2026-11-02: `grep "fn bash(&self" x.rs` split at the
/// `&` made a program `self" x.rs`, missing, DENIED in every session). A quote never closed runs
/// to the end. Each piece is the original text, quotes and all.
pub(super) fn split_unquoted(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            // Inside double quotes the shell still runs `` `…` `` and `$(…)` (marked by a newline
            // before this split): a command starts there, so the text splits there too.
            Some('"') if c == '`' || c == '\n' => {
                pieces.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            Some(q) => {
                if q == '"' && c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(c),
                ';' | '&' | '|' | '\n' | '`' => {
                    pieces.push(&text[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    pieces.push(&text[start..]);
    pieces
}

/// Shell text with each heredoc's body lines removed: after a line holding `<<WORD` (`<<-`,
/// quoted or not), the lines up to the one that is `WORD` alone. A heredoc never closed takes the
/// rest of the text. A line of an unquoted body holding a substitution stays: the shell runs it.
pub(super) fn without_heredoc_bodies(text: &str) -> String {
    let mut out = String::new();
    let mut closing: Vec<(String, bool)> = Vec::new();
    for line in text.lines() {
        if let Some((word, quoted)) = closing.first() {
            if line.trim_start_matches('\t') == word {
                closing.remove(0);
            } else if !quoted && (line.contains("$(") || line.contains('`')) {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        let mut rest = line;
        while let Some(k) = rest.find("<<") {
            rest = &rest[k + 2..];
            if rest.starts_with('<') {
                // `<<<` is a here-string, not a heredoc.
                rest = &rest[1..];
                continue;
            }
            let raw: String = rest
                .trim_start_matches('-')
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace() && !";&|<>()".contains(*c))
                .collect();
            let word: String = raw
                .chars()
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect();
            if !word.is_empty() {
                closing.push((word.clone(), word.len() != raw.len()));
            }
        }
    }
    out
}

/// Programs that run the text they are handed.
fn runs_text(name: &str) -> bool {
    SHELLS.contains(&name)
        || INTERPRETERS.contains(&name)
        || name.starts_with("python")
        || DEFERRED_RUNNERS.contains(&name)
        || matches!(name, "eval" | "source" | "." | "ssh" | "su" | "watch")
}

/// Programs that start a job to run later or in a session of its own, with text the call hands them
/// (§7 A1.3 and item 61: "a detached process … a scheduled job"). `setsid`, `nohup` and `xargs`
/// run their command at once, and are wrappers ([`wrapper_inner`]).
const DEFERRED_RUNNERS: [&str; 10] = [
    "at",
    "batch",
    "crontab",
    "systemd-run",
    "tmux",
    "screen",
    "dtach",
    "abduco",
    "daemonize",
    "start-stop-daemon",
];

/// A channel's name, or a Lunchroom command only the user may run, at the `live` positions.
fn names_channel(toks: &[&str], live: &[usize]) -> bool {
    live.iter().any(|&k| {
        let name = basename(toks[k]);
        FENCE_PROGRAMS.contains(&name)
            || (LUNCHROOM_NAMES.contains(&name)
                && toks.get(k + 1).is_some_and(|n| !LUNCHROOM_OPEN.contains(n)))
    })
}

/// The program file named at `args[k]` of `sed` or `awk`: the value of `-f` (and gawk's `-E` and
/// `-i`), written separately, after `=` or attached.
fn script_option_file<'a>(name: &str, args: &[&'a str], k: usize) -> Option<&'a str> {
    let options: &[&str] = if name == "sed" {
        &["-f", "--file"]
    } else {
        &["-f", "--file", "-E", "--exec", "-i", "--include"]
    };
    let a = args[k];
    if k > 0 && options.contains(&args[k - 1]) {
        return Some(a);
    }
    options.iter().find_map(|o| {
        if o.starts_with("--") {
            a.strip_prefix(*o).and_then(|r| r.strip_prefix('='))
        } else {
            a.strip_prefix(*o).filter(|r| !r.is_empty())
        }
    })
}

/// Whether a `sed` script holds the `e` command or the `e` flag of `s`, which run text. The
/// `/re/` addresses and the parts of `s` are skipped; text of `a`, `i`, `c` or `y` may show a
/// false `e`, which only makes the files searched.
fn sed_runs_text(script: &str) -> bool {
    // Reads up to the next unescaped `d`; false if there is none.
    fn skip(cs: &mut std::str::Chars, d: char) -> bool {
        while let Some(c) = cs.next() {
            if c == '\\' {
                cs.next();
            } else if c == d {
                return true;
            }
        }
        false
    }
    let mut cs = script.chars();
    while let Some(c) = cs.next() {
        match c {
            'e' => return true,
            '/' => {
                skip(&mut cs, '/');
            }
            's' => {
                let Some(d) = cs.next() else { return true };
                if !(skip(&mut cs, d) && skip(&mut cs, d)) {
                    return true;
                }
                let flags: String = cs
                    .by_ref()
                    .take_while(|f| !f.is_whitespace() && !matches!(f, ';' | '}'))
                    .collect();
                if flags.contains('e') {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Whether an `awk` program calls `system`, pipes (`print | "cmd"`, `"cmd" | getline`, `|&`) or
/// loads an extension. Text in strings is ignored; a `|` in a regex counts, which only makes the
/// files searched.
fn awk_runs_text(script: &str) -> bool {
    let code: String = script.split('"').step_by(2).collect();
    let code = code.replace("||", "").replace(char::is_whitespace, "");
    code.contains("system(") || code.contains("@load") || code.contains('|')
}

/// The argument an interpreter runs as its script: for `awk` and `sed` the file given to `-f`;
/// otherwise the first word that is not an option or an option's value (after `run` for `deno`
/// and `bun`), unless inline code (`-c`, `-e`, `-m` …) comes first.
fn interpreter_script<'a>(name: &str, args: &[&'a str]) -> Option<&'a str> {
    if name == "sed" {
        return option_value(args, &["-f", "--file"]);
    }
    // gawk's `-E` (`--exec`) names the program file as `-f` does.
    if matches!(name, "awk" | "gawk" | "mawk") {
        return option_value(args, &["-f", "--file", "-E", "--exec"]);
    }
    // Python's `-I` (isolated mode) and `-E` (ignore the environment) are flags with no value; the
    // script is the next word (field, 2026-11-02: `python3 -I run.py data.csv` took the data file
    // for the script). Ruby's and perl's `-I dir` takes a folder, and perl's `-E` is inline code.
    let python = name.starts_with("python") || name.starts_with("pypy");
    let mut k = 0;
    while let Some(a) = args.get(k) {
        match *a {
            "-I" | "-E" if python => k += 1,
            "-c" | "-e" | "-E" | "-m" | "-p" | "-r" | "--eval" | "--print" | "-Command" => {
                return None;
            }
            "-W" | "-X" | "--require" | "--import" | "--loader" | "-I" => k += 2,
            "run" if matches!(name, "deno" | "bun") => k += 1,
            a if a.starts_with('-') => k += 1,
            a => return Some(a),
        }
    }
    None
}

/// The positions of the script files the shells and interpreters at `runnable` run: a shell's
/// first word that is not an option (none with `-c`), an interpreter's [`interpreter_script`].
/// Where a shell takes its commands from, read as the shell reads its arguments: options end at
/// the first operand, so a word after a script's name (`bash build.sh -clean`) is the script's.
enum ShellInput {
    /// `-c`: the operand at this index is the command text (none given: nothing runs).
    Text(Option<usize>),
    /// The script file at this index.
    Script(usize),
    /// No operand: commands come from standard input.
    Stdin,
}

fn shell_input(args: &[&str]) -> ShellInput {
    let mut dash_c = false;
    let mut k = 0;
    let operand = loop {
        match args.get(k) {
            Some(&("-o" | "-O" | "+o" | "+O" | "--rcfile" | "--init-file")) => k += 2,
            Some(&"--") => break args.get(k + 1).map(|_| k + 1),
            // `--command <text>`: the next word is the command text, whatever it starts with.
            Some(&"--command") => {
                dash_c = true;
                break args.get(k + 1).map(|_| k + 1);
            }
            Some(a) if a.starts_with('-') || a.starts_with('+') => {
                dash_c |= a
                    .strip_prefix('-')
                    .is_some_and(|r| !r.starts_with('-') && r.contains('c'));
                k += 1;
            }
            Some(_) => break Some(k),
            None => break None,
        }
    };
    match (dash_c, operand) {
        (true, k) => ShellInput::Text(k),
        (false, Some(k)) => ShellInput::Script(k),
        (false, None) => ShellInput::Stdin,
    }
}

fn script_positions(texts: &[&str], runnable: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    for &p in runnable {
        let name = basename(texts[p]);
        let args = &texts[p + 1..];
        let script = if SHELLS.contains(&name) {
            match shell_input(args) {
                ShellInput::Script(k) => Some(k),
                ShellInput::Text(_) | ShellInput::Stdin => None,
            }
        } else if INTERPRETERS.contains(&name) || name.starts_with("python") {
            interpreter_script(name, args).and_then(|s| args.iter().position(|a| *a == s))
        } else {
            None
        };
        out.extend(script.map(|k| p + 1 + k));
    }
    out
}

/// I1.2 in script or code text: some statement names an identity variable or clears the
/// environment ([`changes_identity`]).
fn mentions_identity(text: &str) -> bool {
    let named = |t: &str| statements(t).into_iter().any(changes_identity);
    named(text) || escapes::decode(text).is_some_and(|d| named(&d))
}

/// I1.2 in one statement of script or code: it names an identity variable, or it clears the whole
/// environment (`os.environ.clear()`, `clearenv`). Any mention counts, a read included: no list of
/// the ways a language can set a variable is complete, so the guard does not try to tell a read
/// from a change in code it does not run as shell.
fn changes_identity(stmt: &str) -> bool {
    let toks = tokens(stmt);
    let parts: Vec<&str> = toks
        .iter()
        .flat_map(|t| t.split(['.', '=', '[', ']']))
        .collect();
    IDENTITY_VARS.iter().any(|v| parts.contains(v))
        || parts.contains(&"clearenv")
        || parts
            .windows(2)
            .any(|w| matches!(w[0], "environ" | "env") && w[1] == "clear")
}

/// The statements of script or code text: split at a line end or `;` outside brackets, where the
/// line end is not escaped. A call spelled across lines inside brackets stays one statement.
fn statements(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\n' | ';' if depth == 0 && prev != '\\' => {
                out.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
        prev = c;
    }
    out.push(&text[start..]);
    out
}

/// A1.3: Lunchroom's arguments run only an open command. Every option before the command must be
/// an open one (`--help`, `--version`); the command must be open; and with no command, or `help`,
/// no word after it may be a command only the user may run (`lunchroom --help approve`). The words
/// after `send` and the other open commands are their own arguments (a message's text).
fn lunchroom_args_open(args: &[&str]) -> bool {
    let cmd_at = args.iter().position(|a| !a.starts_with('-'));
    let options = &args[..cmd_at.unwrap_or(args.len())];
    if !options.iter().all(|o| LUNCHROOM_OPEN.contains(o)) {
        return false;
    }
    match cmd_at.map(|k| args[k]) {
        Some(cmd) if !LUNCHROOM_OPEN.contains(&cmd) => false,
        None | Some("help") => !args.iter().any(|a| LUNCHROOM_USER_ONLY.contains(a)),
        Some(_) => true,
    }
}

/// The paths a `grep` is given: its arguments that are not options, their values or the pattern.
/// The pattern is the first such word unless `-e`/`-f` (or `--regexp`/`--file`) gives it.
fn grep_path_operands<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut operands = Vec::new();
    let mut pattern_given = false;
    let mut skip_value = false;
    let mut ended = false;
    for &a in args {
        if skip_value {
            skip_value = false;
        } else if ended || a == "-" || !a.starts_with('-') {
            operands.push(a);
        } else if a == "--" {
            ended = true;
        } else if let Some(long) = a.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, false), |(n, _)| (n, true));
            pattern_given |= matches!(name, "regexp" | "file");
            skip_value = !attached
                && matches!(
                    name,
                    "regexp"
                        | "file"
                        | "after-context"
                        | "before-context"
                        | "context"
                        | "max-count"
                        | "directories"
                        | "devices"
                        | "include"
                        | "exclude"
                        | "exclude-from"
                        | "exclude-dir"
                        | "label"
                        | "binary-files"
                );
        } else {
            // A cluster of short options: the first one that takes a value ends it, the value being
            // the rest of the word or the next word.
            let cluster = &a[1..];
            if let Some(k) = cluster.find(|c| "efABCmdD".contains(c)) {
                pattern_given |= matches!(&cluster[k..=k], "e" | "f");
                skip_value = k + 1 == cluster.len();
            }
        }
    }
    if !pattern_given && !operands.is_empty() {
        operands.remove(0);
    }
    operands
}

/// The first `git` argument that is not an option: its subcommand.
fn git_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut k = 0;
    while let Some(a) = args.get(k) {
        if matches!(
            *a,
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            k += 2;
        } else if a.starts_with('-') {
            k += 1;
        } else {
            return Some(a);
        }
    }
    None
}

/// The PROTECTED kind of a program run with these arguments (P1.4), judged on its name.
fn protected_kind(prog: &str, args: &[&str]) -> Option<ProtectedKind> {
    if NETWORK_PROGRAMS.contains(&prog) {
        return Some(ProtectedKind::Network);
    }
    if prog == "git" {
        return match git_subcommand(args) {
            Some("push") => Some(ProtectedKind::PushOrPublish),
            Some("rm" | "clean") => Some(ProtectedKind::Delete),
            Some(s) if GIT_NETWORK.contains(&s) => Some(ProtectedKind::Network),
            _ => None,
        };
    }
    if PUBLISHERS.contains(&prog)
        && args
            .iter()
            .any(|a| matches!(*a, "publish" | "push" | "upload"))
    {
        return Some(ProtectedKind::PushOrPublish);
    }
    if DELETERS.contains(&prog)
        || (prog == "find" && args.contains(&"-delete"))
        || (prog == "gio" && matches!(args.first(), Some(&("trash" | "remove"))))
    {
        return Some(ProtectedKind::Delete);
    }
    // Printing the environment prints any token in it.
    let only_options = args.iter().all(|a| a.starts_with('-'));
    if prog == "printenv"
        || (matches!(prog, "export" | "declare" | "typeset") && only_options)
        || (prog == "set" && args.is_empty())
    {
        return Some(ProtectedKind::Credential);
    }
    None
}

/// Where a wrapper (`sudo`, `env`, `timeout`, ...) at `i` starts the program it runs.
fn wrapper_inner(texts: &[&str], i: usize) -> Option<usize> {
    wrapper_scan(texts, i).map(|(j, _)| j)
}

/// The `-ex` commands of a gdb that starts a program of its own, each of which only runs, shows or
/// ends that program.
const GDB_COMMANDS: [&str; 8] = ["run", "r", "bt", "backtrace", "continue", "c", "quit", "q"];

/// §17 item 5 (decision of 2026-11-14, "Follow spec, narrowly", and "Your pick, as chosen"): the gdb at
/// `i` starts a program of its own in the chosen form: `-nx` (`--nx`, `-n`: no `.gdbinit`
/// is read), then only `--batch`/`-batch` and `-q`/`-quiet`/`-silent` (each with one or two
/// dashes, none of which runs a command) and `-ex`/`--eval-command` options whose value is one of
/// [`GDB_COMMANDS`], ending with `--args` and the program. The position of the program and of the
/// `-ex` values; `None` for any other gdb call (an `-ex` not on the list, `-x`, an init file),
/// which stays unreadable. Stated limit (chosen with this known): gdb's `run` starts the
/// program through `$SHELL` by default (GDB manual, "Starting"), which the guard does not read.
fn gdb_starts_its_own(texts: &[&str], i: usize) -> Option<(usize, Vec<usize>)> {
    let mut nx = false;
    let mut values = Vec::new();
    let mut j = i + 1;
    while let Some(a) = texts.get(j) {
        let bare = a.strip_prefix("--").or_else(|| a.strip_prefix('-'));
        match bare {
            Some("args") => {
                return (nx && j + 1 < texts.len()).then_some((j + 1, values));
            }
            Some("nx" | "n") => nx = true,
            Some("batch" | "q" | "quiet" | "silent") => {}
            Some("ex" | "eval-command") => {
                if !GDB_COMMANDS.contains(texts.get(j + 1)?) {
                    return None;
                }
                values.push(j + 1);
                j += 1;
            }
            _ => return None,
        }
        j += 1;
    }
    None
}

/// [`wrapper_inner`], with the positions of the words that are the values of the wrapper's options.
#[allow(clippy::too_many_lines)] // the option table of each wrapper
fn wrapper_scan(texts: &[&str], i: usize) -> Option<(usize, Vec<usize>)> {
    let name = basename(texts[i]);
    // §17 item 5: gdb that starts a program of its own runs, read through to that program, in the
    // one form whose every command the guard reads (F2 design, "Guard rules"); any other gdb
    // call wraps nothing the guard can read.
    if name == "gdb" {
        return gdb_starts_its_own(texts, i);
    }
    let with_value: &[&str] = match name {
        "sudo" => &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-r",
            "-t",
            "-U",
            "-T",
            "-R",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--chdir",
            "--chroot",
            "--close-from",
            "--command-timeout",
            "--role",
            "--type",
            "--other-user",
        ],
        "doas" => &["-u", "-C"],
        "pkexec" => &["--user"],
        // run0(1): the options that take a value.
        "run0" => &[
            "-u",
            "-g",
            "-D",
            "--user",
            "--group",
            "--chdir",
            "--unit",
            "--property",
            "--description",
            "--slice",
            "--nice",
            "--setenv",
            "--machine",
            "--shell-prompt-prefix",
            "--area",
        ],
        // `runuser -u user [--] command` runs a command; without `-u` the first operand is the
        // user and the command is the text of `-c`, which is judged where it is read.
        "runuser"
            if texts[i + 1..].iter().any(|a| {
                matches!(*a, "-u" | "--user")
                    || a.starts_with("--user=")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('u'))
            }) =>
        {
            &[
                "-u",
                "-g",
                "-G",
                "-s",
                "-w",
                "-c",
                "--user",
                "--group",
                "--supp-group",
                "--shell",
                "--whitelist-environment",
                "--command",
                "--session-command",
            ]
        }
        "env" => &["-u", "-C", "--unset", "--chdir"],
        "timeout" => &["-k", "-s", "--kill-after", "--signal"],
        "nice" => &["-n", "--adjustment"],
        "ionice" => &["-c", "-n", "-p", "--class", "--classdata", "--pid"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "xargs" => &[
            "-I",
            "-L",
            "-n",
            "-P",
            "-s",
            "-d",
            "-E",
            "-a",
            "--max-args",
            "--max-procs",
            "--delimiter",
            "--arg-file",
            "--replace",
            "--max-lines",
            "--max-chars",
            "--eof",
        ],
        "exec" => &["-a"],
        "flock" => &["-w", "--timeout", "-E", "--conflict-exit-code"],
        "strace" => &[
            "-o", "-p", "-e", "-s", "-u", "-E", "-P", "-S", "-X", "-I", "-a", "-b",
        ],
        "ltrace" => &["-o", "-p", "-e", "-s", "-u", "-n", "-a"],
        "proxychains" | "proxychains4" => &["-f"],
        "nohup" | "setsid" | "time" | "command" | "builtin" | "chronic" | "unbuffer"
        | "caffeinate" | "taskset" | "busybox" | "valgrind" | "torsocks" | "firejail"
        | "unshare" | "dbus-run-session" | "chroot" => &[],
        _ => return None,
    };
    let mut j = i + 1;
    let mut values = Vec::new();
    while let Some(a) = texts.get(j) {
        if *a == "--" {
            j += 1;
            break;
        }
        if name == "env" && is_assignment(a) {
            j += 1;
            continue;
        }
        if !a.starts_with('-') {
            break;
        }
        if with_value.contains(a) {
            values.push(j + 1);
            j += 2;
        } else {
            j += 1;
        }
    }
    if matches!(name, "timeout" | "taskset" | "flock" | "chroot") {
        j += 1; // the duration, CPU mask, lock file or new root before the program
    }
    // `flock FILE -c 'text'` runs the text through a shell: no program follows the lock file.
    if name == "flock"
        && !command_option_values(&texts[i + 1..], "wEc", FLOCK_LONG, 'c', &["command"]).is_empty()
    {
        j = texts.len();
    }
    Some((j, values))
}

/// The words that are the values of the options of the wrapper at `i` (`sudo --prompt X`).
fn wrapper_values(texts: &[&str], i: usize) -> Vec<usize> {
    wrapper_scan(texts, i).map_or_else(Vec::new, |(_, values)| values)
}

/// The positions in a command that hold the program it runs, through any wrappers.
/// The positions in a command that hold a program it runs: the command's own program, the program
/// each wrapper (`sudo`, `env`, `timeout`, ...) runs, and the programs `find -exec` runs. Sorted.
fn program_positions(texts: &[&str]) -> Vec<usize> {
    let mut out = Vec::new();
    let Some(start) = texts.iter().position(|t| !is_assignment(t)) else {
        return out;
    };
    let mut starts = vec![start];
    while let Some(mut i) = starts.pop() {
        while !out.contains(&i) {
            out.push(i);
            if basename(texts[i]) == "find" {
                for k in i + 1..texts.len() {
                    if matches!(texts[k], "-exec" | "-execdir" | "-ok" | "-okdir")
                        && k + 1 < texts.len()
                    {
                        starts.push(k + 1);
                    }
                }
            }
            match wrapper_inner(texts, i) {
                Some(j) if j < texts.len() => i = j,
                _ => break,
            }
        }
    }
    out.sort_unstable();
    out
}

/// The positions the always-DENIED rules judge as programs: the known program positions, and every
/// later word of a program that is not known never to run its arguments (`mywrap codex queue`).
fn runnable_positions(texts: &[&str], positions: &[usize]) -> Vec<usize> {
    // The value of a wrapper's option (`sudo --prompt X`, `timeout -s KILL`) is data, not a program.
    let values: Vec<usize> = positions
        .iter()
        .flat_map(|&p| wrapper_values(texts, p))
        .collect();
    (0..texts.len())
        .filter(|&i| !values.contains(&i))
        .filter(|&i| {
            positions.contains(&i)
                || positions
                    .iter()
                    .rev()
                    .find(|&&p| p < i)
                    .is_some_and(|&p| !NEVER_RUNS.contains(&basename(texts[p])))
        })
        .collect()
}

/// Whether a command is a build or an install, through any wrapper: a program run by a path that
/// does not exist yet may be what it makes.
fn builds(texts: &[&str]) -> bool {
    program_positions(texts).into_iter().any(|p| {
        let args = &texts[p + 1..];
        // The first word that is not an option: the subcommand.
        let sub = args.iter().find(|a| !a.starts_with('-')).copied();
        match basename(texts[p]) {
            // Compilers and build tools make their output whatever they are asked.
            "make" | "ninja" | "tsc" | "gcc" | "g++" | "cc" | "c++" | "clang" | "clang++"
            | "rustc" => true,
            "cmake" => args.contains(&"--build"),
            "cargo" => matches!(sub, Some("build" | "b" | "install")),
            "go" => matches!(sub, Some("build" | "install")),
            "npm" | "pnpm" | "yarn" => {
                matches!(
                    sub,
                    None | Some("install" | "i" | "ci" | "add" | "build" | "run")
                )
            }
            "pip" | "pip3" => sub == Some("install"),
            name if name.starts_with("python") => {
                args.windows(2)
                    .any(|w| matches!(w, ["-m", "venv" | "virtualenv"]))
                    || args
                        .windows(3)
                        .any(|w| matches!(w, ["-m", "pip", "install"]))
            }
            _ => false,
        }
    })
}

/// The positions of the words that are text, not paths: the pattern a searcher is given, what
/// `echo` and `printf` print (a redirection's target is still a path) and a commit's message.
fn text_positions(words: &[Word], texts: &[&str], positions: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    for &p in positions {
        let args = &texts[p + 1..];
        let text = match basename(texts[p]) {
            "echo" | "printf" => {
                out.extend((p + 1..texts.len()).filter(|&k| !words[k].redirect));
                None
            }
            "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" => {
                option_value(args, &["-e", "--regexp"]).or_else(|| {
                    // Else the first operand, unless an option may carry the pattern or a file of them.
                    let other = args.iter().any(|a| {
                        matches!(*a, "--file" | "--files")
                            || (a.starts_with('-')
                                && !a.starts_with("--")
                                && a.contains(['e', 'f']))
                    });
                    args.iter().find(|a| !other && !a.starts_with('-')).copied()
                })
            }
            "git" if git_subcommand(args) == Some("commit") => {
                option_value(args, &["-m", "--message"])
            }
            _ => None,
        };
        // The first word that is that text (or `--opt=text`): the pattern or message itself.
        let at = text.and_then(|t| {
            args.iter()
                .position(|a| *a == t || a.strip_suffix(t).is_some_and(|r| r.ends_with('=')))
        });
        out.extend(at.map(|k| p + 1 + k));
    }
    out
}

/// The value of the first of `flags` in `args`, written separately or attached.
fn option_value<'a>(args: &[&'a str], flags: &[&str]) -> Option<&'a str> {
    for (k, a) in args.iter().enumerate() {
        for f in flags {
            if a == f {
                return args.get(k + 1).copied();
            }
            if let Some(v) = a.strip_prefix(f) {
                if f.starts_with("--") {
                    if let Some(v) = v.strip_prefix('=') {
                        return Some(v);
                    }
                } else if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

impl Guard<'_> {
    /// Classifies one Claude Code tool call made from `cwd`, as the one class
    /// [`Call::folded`] gives: a call that also raised the guard's configuration is shown as it.
    /// Prefer [`Guard::classify_call`], which keeps every kind for `decide_call`.
    #[must_use]
    pub fn classify(&self, tool: &str, input: &Value, cwd: &Path) -> CallClass {
        self.classify_call(tool, input, cwd).folded()
    }

    /// Classifies one Claude Code tool call made from `cwd`: the class to show and every PROTECTED
    /// kind the call raised (P1.6: the policy follows the kinds, not the class shown).
    #[must_use]
    pub fn classify_call(&self, tool: &str, input: &Value, cwd: &Path) -> Call {
        self.classify_explained(tool, input, cwd).0
    }

    /// [`Guard::classify_call`], with what the rule behind a DENIED class found at fault: the
    /// path, program, word or text, as the guard read it (V1.4). `None` for a call not DENIED, or
    /// where the rule names nothing narrower than the whole call.
    #[must_use]
    pub fn classify_explained(
        &self,
        tool: &str,
        input: &Value,
        cwd: &Path,
    ) -> (Call, Option<String>) {
        let j = self.judge(tool, input, cwd);
        (j.call, j.fault)
    }

    /// [`Guard::classify_explained`], with the paths the call reads, searches and writes
    /// ([`Access`], T1.8): what the guard judged before the call ran.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one arm per tool
    pub fn judge(&self, tool: &str, input: &Value, cwd: &Path) -> Judged {
        // H1.7: the step named if the guard's time runs out here.
        let _step = crate::steps::begin(format!("judging the {tool} call"));
        let field = |k: &str| input.get(k).and_then(Value::as_str);
        let mut acc = Acc::new(CallClass::InSlip);
        match tool {
            "Read" | "NotebookRead" | "LS" | "Grep" => {
                let key = match tool {
                    "Read" => "file_path",
                    "NotebookRead" => "notebook_path",
                    _ => "path",
                };
                let cwd_text = cwd.to_string_lossy();
                let raw = field(key).or(if tool == "Grep" {
                    Some(&*cwd_text)
                } else {
                    None
                });
                match raw {
                    Some(r) => self.read_path(r, cwd, tool == "Grep", &mut acc),
                    None => self.when_held(&mut acc, CallClass::Protected(ProtectedKind::Unparsed)),
                }
            }
            "Glob" => {
                let root = field("path").map_or_else(
                    || cwd.to_path_buf(),
                    |p| absolute(p, cwd, &self.layout.home),
                );
                match field("pattern") {
                    Some(p) => self.glob_tool(p, &root, &mut acc),
                    None => self.when_held(&mut acc, CallClass::Protected(ProtectedKind::Unparsed)),
                }
            }
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                acc.raise(CallClass::Ordinary);
                let key = if tool == "NotebookEdit" {
                    "notebook_path"
                } else {
                    "file_path"
                };
                // P1.4: a file-write call with no path the guard can read is DENIED in every
                // session, since it may write Lunchroom's stores.
                match field(key).filter(|p| !p.is_empty()) {
                    Some(p) => self.write_path(p, cwd, tool != "Write", &mut acc),
                    None => acc.deny(DenyReason::Unparsed, &format!("{tool} with no path")),
                }
            }
            "apply_patch" => {
                // Codex's and OpenClaw's file-edit tool (docs/host-facts/codex-apply-patch.md; the
                // OpenClaw plugin passes OpenClaw's `input` as `command`): its input is a patch
                // whose header lines name the files it writes; the lines between them are file
                // contents, not paths, and are never judged as paths. A patch with no header line
                // the guard can read is DENIED in every session (P1.4).
                acc.raise(CallClass::Ordinary);
                let (paths, cut) = field("command").map_or((Vec::new(), false), patch_paths);
                if paths.is_empty() {
                    acc.deny(
                        DenyReason::Unparsed,
                        "apply_patch with no file the guard can read",
                    );
                }
                for p in paths {
                    self.write_path(p, cwd, true, &mut acc);
                }
                if cut {
                    self.when_held(&mut acc, CallClass::Protected(ProtectedKind::Unparsed));
                }
            }
            "Bash" => {
                // §6 item 1: a reply, a command that is exactly one `lunchroom send`, is inside
                // every slip; its words are still judged below.
                acc.reply = field("command").is_some_and(|c| self.is_reply(c, cwd));
                // §6 item 1, D1.1: `lunchroom read <id>` is a read, and is inside every slip.
                let read = field("command").is_some_and(|c| self.is_read(c, cwd));
                if !acc.reply && !read {
                    acc.raise(CallClass::Ordinary);
                }
                match field("command") {
                    Some(c) => self.shell_text(c, cwd, 0, &mut acc),
                    None => self.when_held(&mut acc, CallClass::Protected(ProtectedKind::Unparsed)),
                }
                // P1.1: the reply is PROTECTED in the Strict hold mode only, which the policy
                // decides from this kind (the classifier does not know the mode).
                if acc.reply {
                    acc.kinds = acc.kinds.with(ProtectedKind::Reply);
                }
            }
            t if SLIP_TOOLS.contains(&t) => {}
            t if ORDINARY_TOOLS.contains(&t) => acc.raise(CallClass::Ordinary),
            t if NETWORK_TOOLS.contains(&t) => {
                acc.raise(CallClass::Ordinary);
                self.when_held(&mut acc, CallClass::Protected(ProtectedKind::Network));
            }
            _ => {
                acc.raise(CallClass::Ordinary);
                self.when_held(&mut acc, CallClass::Protected(ProtectedKind::UnknownTool));
                // P1.4: an unknown tool given a path in Lunchroom's stores may write it.
                let mut texts = Vec::new();
                string_leaves(input, &mut texts);
                for t in texts {
                    self.word_paths(t, cwd, false, Dir::Read, &mut acc);
                }
            }
        }
        // P1.4: a command, script or code that starts a new agent session.
        if acc.new_session {
            self.when_held(&mut acc, CallClass::Protected(ProtectedKind::OtherSession));
        }
        // §11 L1.1: a call, other than the reply, with more path-like strings than the guard
        // resolves is DENIED in every session; its message says to split it.
        if acc.too_many && !acc.denied() {
            acc.deny(DenyReason::TooManyPaths, TOO_MANY_PATHS);
        }
        let fault = if acc.denied() { acc.fault.take() } else { None };
        Judged {
            call: acc.call(),
            fault,
            access: acc.access,
        }
    }

    fn when_held(&self, acc: &mut Acc, c: CallClass) {
        if self.held {
            acc.raise(c);
        }
    }

    fn home(&self) -> String {
        self.layout.home.to_string_lossy().into_owned()
    }

    /// The file `raw` opens, resolved as the operating system resolves it (L1.1: "Paths are
    /// resolved (symlinks, wildcards) before checking"): part by part, so `..` leaves the folder a
    /// link led to, not the folder holding the link.
    fn physical(&self, raw: &str, here: &Path) -> PathBuf {
        self.fs
            .resolve(&absolute(raw, here, &self.layout.home))
            .path
    }

    /// The paths `raw` may mean: the file it opens, the path as written (made absolute and
    /// lexically normal) and its real path, and for a wildcard every match and its real path. The
    /// wildcard is matched in the folder its literal part resolves to. `capped` is the folder to
    /// search from when the wildcard's matching was cut short; `unresolved` says the file could
    /// not be settled (a link loop, a part the guard may not look at, a `..` after a part that does
    /// not exist or after a wildcard), so it may be anywhere.
    fn forms(&self, raw: &str, cwd: &Path) -> Forms {
        // H1.7: the step named if the guard's time runs out here.
        let _step = crate::steps::begin(format!("looking up the path {raw}"));
        let joined = absolute(raw, cwd, &self.layout.home);
        let lexical = normalize(raw, cwd, &self.layout.home);
        let mut f = Forms::default();
        if raw.contains(['*', '?', '[']) {
            let (head, tail) = split_wild(&joined);
            let base = self.fs.resolve(&head);
            f.unresolved = !base.complete;
            f.timed_out = f.unresolved && self.fs.cut(&head);
            let mut pattern = base.path.clone();
            for c in tail.components() {
                match c {
                    Component::Normal(s) => pattern.push(s),
                    // The folders a wildcard matched are not known here, so the folder above one
                    // is not either: the path is unresolved (fail closed while HELD, P1.2).
                    Component::ParentDir => {
                        pattern.pop();
                        f.unresolved = true;
                    }
                    Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                }
            }
            f.push(self.fs.real(&lexical));
            let mut looked = vec![lexical.clone()];
            f.push(lexical);
            let e = self.fs.expand(&pattern);
            looked.push(pattern.clone());
            for m in e.matches {
                f.push(self.fs.real(&m));
                looked.push(m.clone());
                f.push(m);
            }
            // A `real` look cut short by the time leaves that path unsettled, however it is spelled.
            if looked.iter().any(|p| self.fs.cut(p)) {
                (f.unresolved, f.timed_out) = (true, true);
            }
            if e.capped && f.capped.is_none() {
                f.capped = Some(literal_prefix(&pattern));
            }
        } else {
            let r = self.fs.resolve(&joined);
            f.unresolved = !r.complete;
            f.timed_out = f.unresolved && self.fs.cut(&joined);
            f.push(r.path);
            f.push(self.fs.real(&lexical));
            if self.fs.cut(&lexical) {
                (f.unresolved, f.timed_out) = (true, true);
            }
            f.push(lexical);
        }
        f
    }

    /// A path the guard could not settle may be anywhere. While HELD, one a tool is known to read
    /// is DENIED (a read that cannot be resolved cannot be shown to avoid L1.1's paths, and the
    /// operating system would fail it too); one a shell word names, with no direction, may be
    /// read or written, so it is PROTECTED as everything it could be: unparsed text, a credential,
    /// the guard's configuration. Nothing changes for an agent that is not HELD (P1.2).
    fn unresolved(&self, reads: bool, timed_out: bool, what: &str, acc: &mut Acc) {
        if !self.held {
            return;
        }
        // H1.5, L1.1: a path left unsettled because the guard's time ran out is one Lunchroom could
        // not check, which goes to the user (as a search cut short does); only a path that cannot be
        // followed at all is DENIED.
        if reads && timed_out {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
            return;
        }
        if reads {
            self.denied_read(
                &format!("{what} (a path Lunchroom could not follow to the file it opens)"),
                acc,
            );
        } else {
            acc.raise(CallClass::Protected(ProtectedKind::Unparsed));
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
            acc.raise(CallClass::Protected(ProtectedKind::GuardConfig));
        }
    }

    /// L1.1 for one path a HELD session reads.
    ///
    /// `named` is the path as the call wrote it: the part at fault a DENIED message shows is
    /// always a path the agent itself named, never one the guard found (a link's target, a file in
    /// a folder searched), which would tell a HELD agent where other sessions' files are.
    fn read_facts(&self, p: &Path, named: &str, acc: &mut Acc) {
        let f = self.layout.facts(p);
        // 1.1: another terminal's screen and keys, and another process's memory, environment and
        // descriptors, are not read in any session.
        if f.terminal {
            acc.deny(DenyReason::Configuration, named);
        }
        self.other_process(f.secret_pid, named, acc);
        if f.store || f.transcript {
            self.denied_read(named, acc);
        } else if f.credential {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
        }
    }

    /// A read of a path L1.1 denies is never inside the slip: DENIED while HELD; for an agent that
    /// is not HELD it is an ordinary call, which runs (L1.1 holds only while HELD).
    fn denied_read(&self, what: &str, acc: &mut Acc) {
        if self.held {
            acc.deny(DenyReason::DeniedPath, what);
        } else {
            acc.raise(CallClass::Ordinary);
        }
    }

    fn read_path(&self, raw: &str, cwd: &Path, search: bool, acc: &mut Acc) {
        let forms = self.forms(raw, cwd);
        for f in &forms.paths {
            self.read_facts(f, raw, acc);
        }
        // T1.8: a path the tool opens is read; a folder a search is given is searched.
        let (_, files): (Vec<PathBuf>, Vec<PathBuf>) = forms
            .paths
            .iter()
            .cloned()
            .partition(|f| search && self.fs.is_dir(f));
        acc.note(Dir::Read, &files);
        if search {
            for f in forms.paths.iter().filter(|f| self.fs.is_dir(f)) {
                self.search_from(f, raw, acc);
            }
        }
        if let Some(prefix) = forms.capped {
            self.search_from(&prefix, raw, acc);
        }
        if forms.unresolved {
            self.unresolved(true, forms.timed_out, raw, acc);
        }
    }

    fn glob_tool(&self, pattern: &str, root: &Path, acc: &mut Acc) {
        let joined = absolute(pattern, root, &self.layout.home);
        let (head, tail) = split_wild(&joined);
        let base = self.fs.resolve(&head);
        // The folder the pattern starts in, as written and as the operating system reaches it.
        let start = literal_prefix(&normalize(pattern, root, &self.layout.home));
        // The pattern with the folder the call gave it, as the call wrote them together.
        let named = joined.to_string_lossy();
        self.search_from(&start, &named, acc);
        self.search_from(&base.path, &named, acc);
        // A `..` after a wildcard leaves a folder not yet known.
        if !base.complete || tail.components().any(|c| c == Component::ParentDir) {
            self.unresolved(true, !base.complete && self.fs.cut(&head), pattern, acc);
        }
    }

    /// L1.1 for a search from `dir`: what it is, what lies below it, and (by a bounded walk) any
    /// credential file or link to a denied path inside it. A walk cut short counts as reaching a
    /// credential, so the search goes to the user. For an agent that is not HELD the walk is not
    /// made, because nothing it could find changes what happens to the call (P1.2); the search is
    /// then an ordinary call, not shown to be inside the slip.
    ///
    /// `named` is the folder as the call wrote it. A DENIED search names that folder and nothing
    /// found below it: what the walk found is where other sessions' files are, which a HELD agent
    /// must not learn from the message (L1.1).
    fn search_from(&self, dir: &Path, named: &str, acc: &mut Acc) {
        acc.access.searches.insert(dir.to_path_buf());
        self.read_facts(dir, named, acc);
        let fault = format!("a search of {named}");
        if self.layout.denied_below(dir) {
            self.denied_read(&fault, acc);
        }
        if self.layout.credential_root_below(dir) {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
        }
        if acc.denied() || !self.fs.is_dir(dir) {
            return;
        }
        if !self.held {
            acc.raise(CallClass::Ordinary);
            return;
        }
        let (mut denied, mut credential) = (None::<PathBuf>, false);
        // H1.7: the step named if the guard's time runs out here.
        let _step = crate::steps::begin(format!("searching the folder {named}"));
        let end = self.fs.walk(dir, &mut |p| {
            let f = self.layout.facts(p);
            if denied.is_none() && (f.store || f.transcript) {
                denied = Some(p.to_path_buf());
            }
            credential |= f.credential;
            denied.is_some()
        });
        if denied.is_some() {
            self.denied_read(&fault, acc);
        } else if credential || end == WalkEnd::Capped {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
        }
    }

    /// P1.4 for one path a call writes.
    fn write_path(&self, raw: &str, cwd: &Path, also_read: bool, acc: &mut Acc) {
        let forms = self.forms(raw, cwd);
        for f in &forms.paths {
            self.path_rules(f, raw, false, acc);
        }
        acc.note(if also_read { Dir::Both } else { Dir::Write }, &forms.paths);
        if let Some(prefix) = forms.capped {
            self.capped_rules(&prefix, raw, acc);
        }
        if forms.unresolved {
            self.unresolved(false, forms.timed_out, raw, acc);
        }
    }

    /// The rules for one path a call reads or writes (the shell's words have no direction): a
    /// store is DENIED in every session; while HELD, transcripts are DENIED and credentials, guard
    /// configuration and Lunchroom's program (other than running it) are PROTECTED.
    ///
    /// `named` is the path as the call wrote it, the part at fault a message shows (see
    /// [`Guard::read_facts`]).
    fn path_rules(&self, p: &Path, named: &str, runs_it: bool, acc: &mut Acc) {
        let f = self.layout.facts(p);
        if f.store {
            acc.deny(DenyReason::Configuration, named);
        }
        // 1.1: a terminal device that is not the agent's own controlling terminal, in every
        // session: another terminal's keys and screen are the user's (the console among them).
        if f.terminal {
            acc.deny(DenyReason::Configuration, named);
        }
        // 1.1: the installed program is root's; it is named only to run it.
        if p == Path::new(crate::setgid::PROGRAM) && !runs_it {
            acc.deny(DenyReason::Configuration, named);
        }
        self.other_process(f.secret_pid, named, acc);
        if !self.held {
            return;
        }
        if f.transcript {
            acc.deny(DenyReason::DeniedPath, named);
        }
        if f.credential {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
        }
        // A harness's program decides what runs the guard, as Lunchroom's program does (R3.1).
        if f.guard_config || ((f.program || f.harness_program) && !runs_it) {
            acc.raise(CallClass::Protected(ProtectedKind::GuardConfig));
        }
    }

    /// A wildcard cut short may match anything below its literal prefix.
    fn capped_rules(&self, prefix: &Path, named: &str, acc: &mut Acc) {
        if self.layout.lunchroom_home.starts_with(prefix) || self.layout.facts(prefix).store {
            acc.deny(
                DenyReason::Configuration,
                &format!("the wildcard {named}, which may match anything in Lunchroom's folder"),
            );
        }
        if self.held {
            self.search_from(prefix, named, acc);
        }
    }

    /// The path rules for one string, by its text alone: nothing is looked up in the file system.
    /// A string past [`MAX_RESOLVED`] gets this, so a store or transcript path written out in
    /// full is still found, and a HELD call is PROTECTED as unparsed for the rest (links and
    /// wildcards in it were not looked at), unless it is a reply.
    fn lexical_rules(&self, c: &str, cwd: &Path, acc: &mut Acc) {
        let lexical = normalize(c, cwd, &self.layout.home);
        self.path_rules(&lexical, c, false, acc);
        if !acc.reply {
            acc.note(Dir::Read, &[lexical]);
        }
        // §11 L1.1: "a call with more, other than the reply, is DENIED in every session, and its
        // message says to split it into smaller calls". The reply's words are text to send, which
        // it opens as no file; a HELD reply is PROTECTED as unparsed for the strings left unread.
        if acc.reply {
            self.when_held(acc, CallClass::Protected(ProtectedKind::Unparsed));
        } else {
            acc.too_many = true;
        }
    }

    fn word_paths(&self, text: &str, cwd: &Path, runs_it: bool, dir: Dir, acc: &mut Acc) {
        for c in candidates(text) {
            if acc.resolved >= MAX_RESOLVED {
                self.lexical_rules(c, cwd, acc);
                continue;
            }
            acc.resolved += 1;
            let forms = self.forms(c, cwd);
            for f in &forms.paths {
                self.path_rules(f, c, runs_it && c == text, acc);
            }
            // The words of a reply are text to send, not paths the call opens.
            if !acc.reply {
                acc.note(dir, &forms.paths);
            }
            if let Some(prefix) = forms.capped {
                self.capped_rules(&prefix, c, acc);
            }
            // A long reply's text is too long for the system to take as a path at all.
            if forms.unresolved && !(acc.reply && unspellable(c)) {
                self.unresolved(false, forms.timed_out, c, acc);
            }
        }
    }

    /// K2: a shell expands braces before it opens a path (`/etc/{lunchroom,x}/f`), and a word that
    /// holds one is read by the words it makes; a word ending in a path the call writes is a write.
    fn brace_paths(&self, text: &str, targets: &[String], cwd: &Path, acc: &mut Acc) {
        for w in text.split_whitespace().filter(|w| w.contains('{')) {
            let dir = if targets.iter().any(|t| w.ends_with(t.as_str())) {
                Dir::Write
            } else {
                Dir::Read
            };
            for e in braces::expand(w).iter().filter(|e| *e != w) {
                self.word_paths(e, cwd, false, dir, acc);
            }
        }
    }

    /// F1.2: text the guard cannot read is judged by its words, with the reason `unparsed`, and
    /// while HELD it is PROTECTED. A program runs at the word that begins each piece of the text
    /// (after `;`, `&`, `|`, a line end, `$(` or a backquote, past any `NAME=value`). A channel's
    /// name or a user-only Lunchroom command counts there and after a wrapper, not in prose (a
    /// commit message, a heredoc written to a file); text handed to a shell or an interpreter
    /// counts in full, heredoc bodies included.
    fn unparsed(&self, text: &str, cwd: &Path, acc: &mut Acc) {
        // K1.8: a text that holds escapes (`$'a\nb'`, `printf 'a\x0ab'`) is read as written and as
        // the program decodes it.
        if let Some(decoded) = escapes::decode(text) {
            self.unparsed(&decoded, cwd, acc);
        }
        let home = self.home();
        let text = text.replace("${HOME}", &home).replace("$HOME", &home);
        // Field, 2026-11-02: quoted text is one word, never a place a command starts. A split that
        // ignored the quotes made `grep "fn bash(&self" x.rs` a program `self" x.rs`, missing, and
        // DENIED in every session. The text inside quotes is masked before the split.
        // Only where the text splits is read from the masked copy; every piece is the original text,
        // so a quoted channel word or identity variable is still seen by the rules below.
        let head = without_heredoc_bodies(&text).replace("$(", "\n");
        let raw: Vec<&str> = split_unquoted(&head);
        let pieces: Vec<Vec<&str>> = raw.iter().map(|p| tokens(p)).collect();
        let at: Vec<Vec<usize>> = pieces.iter().map(|t| program_positions(t)).collect();
        let fed = pieces
            .iter()
            .zip(&at)
            .any(|(t, ps)| ps.iter().any(|&p| runs_text(basename(t[p]))));
        let live = if fed { text.as_str() } else { head.as_str() };
        let toks = tokens(live);
        let named = if fed {
            names_channel(&toks, &(0..toks.len()).collect::<Vec<_>>())
        } else {
            (pieces.iter().zip(&at)).any(|(t, ps)| names_channel(t, &runnable_positions(t, ps)))
        };
        // F1.2: a whole word, not a word inside a path (`/tmp/claude-1000/x` is no match).
        if named
            || toks.iter().any(|t| *t == "sessions_send" || is_identity(t))
            || names_chat_api(live)
        {
            // The word at fault where one is a channel's own word or an identity variable; else
            // the text that names the channel.
            let word = toks
                .iter()
                .find(|t| **t == "sessions_send" || is_identity(t) || names_chat_api(t))
                .copied();
            acc.deny(DenyReason::Unparsed, word.unwrap_or(live));
        }
        // 1.1: the console's wall holds for text the guard cannot read in full, at each piece's
        // programs.
        for (t, ps) in pieces.iter().zip(&at) {
            for p in runnable_positions(t, ps) {
                self.console_wall(t, p, &text, cwd, acc);
            }
        }
        // §17.5: `/proc/<pid>/{mem,environ,fd,maps}` of a process whose id the call does not write.
        if let Some(path) = proc_secret_unwritten(&text) {
            acc.deny(DenyReason::DeniedPath, &path);
        }
        // P1.4: a file this call writes and then runs is judged on the text written, in the order
        // of the call: a run counts the writes before it, and a later write replaces an earlier
        // text for the runs after it.
        let per_piece = writes::scan_pieces(&raw, writes::heredoc_bodies(&text));
        let mut written_before = Vec::with_capacity(raw.len());
        let mut targets: Vec<String> = Vec::new();
        for ((piece, t), ws) in raw.iter().zip(&pieces).zip(per_piece) {
            written_before.push(
                program_word(piece)
                    .filter(|w| w.contains('/'))
                    .is_some_and(|w| acc.written_as(w, cwd, &self.layout.home).is_some()),
            );
            for (k, how) in run_targets(t) {
                // The piece that writes a file is not a run of it.
                if !ws.iter().any(|w| w.target == t[k]) {
                    self.run_written(t[k], cwd, how, 1, acc);
                }
            }
            for w in ws {
                targets.push(w.target.clone());
                self.note_written(w, cwd, acc);
            }
        }
        for t in toks {
            let dir = if targets.iter().any(|w| w == t) {
                Dir::Write
            } else {
                Dir::Read
            };
            self.word_paths(t, cwd, false, dir, acc);
        }
        self.brace_paths(&text, &targets, cwd, acc);
        // A path at a program's place that does not exist is DENIED, as in a command the guard
        // can read, unless an earlier piece builds or this call has written it by then.
        let mut built = false;
        for (k, (piece, t)) in raw.iter().zip(&pieces).enumerate() {
            if !built
                && !acc.lenient
                && !written_before[k]
                && let Some(w) = program_word(piece).filter(|w| w.contains('/'))
                && !self.fs.exists(&self.physical(w, cwd))
            {
                acc.deny(
                    DenyReason::Unparsed,
                    &format!("{w} (a program that does not exist)"),
                );
            }
            built |= builds(t);
        }
        // P1.4: text the strict reader refused only for what it builds at run time (a command
        // substitution, an expansion) is read by the GATED reader and is GATED, not PROTECTED as
        // text the guard cannot parse is (F1.2); the PROTECTED kinds of the commands it reads
        // still stand.
        match gated::run_time_commands(&text) {
            Some(cmds) => {
                for c in &cmds {
                    let t: Vec<&str> = c.iter().map(String::as_str).collect();
                    for p in program_positions(&t) {
                        if let Some(kind) = protected_kind(basename(t[p]), &t[p + 1..]) {
                            self.when_held(acc, CallClass::Protected(kind));
                        }
                    }
                }
            }
            None => self.when_held(acc, CallClass::Protected(ProtectedKind::Unparsed)),
        }
    }

    fn shell_text(&self, text: &str, cwd: &Path, depth: usize, acc: &mut Acc) {
        // P1.4: a merge, a deploy or a migration, or something the guard cannot read to the end.
        self.gated_text(text, cwd, depth, acc);
        if depth > MAX_NESTING {
            self.unparsed(text, cwd, acc);
            return;
        }
        // F1.2: a variable the text itself gave a literal is that literal, not an unknown word.
        let literal = literals::substitute(text);
        let text = literal.as_deref().unwrap_or(text);
        let parsed = shell::parse(text, &self.home());
        // I1.2: text the command hands to a shell is code the command runs, where any mention
        // of an identity variable counts as a change; text it cannot parse shows `unparsed`
        // (F1.2).
        if depth > 0 && mentions_identity(text) {
            let reason = if parsed == Parsed::Unparsed {
                DenyReason::Unparsed
            } else {
                DenyReason::Fence
            };
            let word = tokens(text).into_iter().find(|t| is_identity(t));
            acc.deny(reason, word.unwrap_or(text));
        }
        // A1.3, F1.4: code the command hands to a shell is a script it runs, where a mention of a
        // Lunchroom command only the user may run, a string it only prints included, and a
        // mention of `TIOCSTI` or its number make the call DENIED.
        if depth > 0 {
            Self::code_mentions(text, "text a shell is given", acc);
        }
        match parsed {
            Parsed::Unparsed => self.unparsed(text, cwd, acc),
            Parsed::Commands(cmds) => {
                // §7 A1.3: a job started here to run later (`at`, `crontab`, a detached tmux or
                // screen session) runs text the call hands it, which is code the call runs.
                let deferred = |own: bool| {
                    cmds.iter().any(|c| {
                        let t: Vec<&str> = c.words.iter().map(|w| w.text.as_str()).collect();
                        program_positions(&t).iter().any(|&p| {
                            let n = basename(t[p]);
                            DEFERRED_RUNNERS.contains(&n)
                                && (own || !matches!(n, "tmux" | "screen"))
                        })
                    })
                };
                if deferred(true) {
                    self.code_text(text, "text a job started here would run", acc);
                }
                // The text of `at`, `batch`, `crontab` and the like is read by its words, any of
                // which may start a command (the commands `at` reads, the lines of a crontab):
                // the console's wall holds at each of them. (`tmux` and `screen` have their own
                // reading of their commands.)
                if deferred(false) {
                    let toks = tokens(text);
                    for p in 0..toks.len() {
                        self.console_wall(&toks, p, text, cwd, acc);
                    }
                }
                let mut here = cwd.to_path_buf();
                let mut built = false;
                for c in &cmds {
                    let at = here.clone();
                    self.command(&c.words, text, &mut here, depth, &mut built, acc);
                    // P1.4: what this command writes counts for the commands after it.
                    for w in writes::command_writes(&c.words) {
                        self.note_written(w, &at, acc);
                    }
                }
            }
        }
    }

    /// `built`: an earlier command of the same text is a build or an install ([`builds`]); this
    /// command adds itself to it.
    #[allow(clippy::too_many_lines)] // the rules of one command, in the order they are judged
    fn command(
        &self,
        words: &[Word],
        full: &str,
        here: &mut PathBuf,
        depth: usize,
        built: &mut bool,
        acc: &mut Acc,
    ) {
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        let positions = program_positions(&texts);
        let runnable = runnable_positions(&texts, &positions);
        for &p in &runnable {
            Self::program_rules(words, &texts, p, positions.contains(&p), acc);
            // A word an unknown wrapper is given may be a setting it applies or a URL it calls.
            if !positions.contains(&p) && (is_identity(texts[p]) || names_chat_api(texts[p])) {
                acc.deny(DenyReason::Fence, texts[p]);
            }
            // A word an unknown wrapper may run is judged as code, its value too when it is
            // attached to a one-letter option (`-x 'codex queue …'`).
            if !positions.contains(&p) {
                let attached = texts[p]
                    .strip_prefix('-')
                    .filter(|r| !r.starts_with('-'))
                    .and_then(|r| r.get(1..));
                if let Some(r) = code_rules(texts[p]).or_else(|| attached.and_then(code_rules)) {
                    acc.deny(r, texts[p]);
                }
            }
        }
        // 1.1: the console's wall, after the rules above, so a reason they gave stays the one shown.
        for &p in &runnable {
            self.console_wall(&texts, p, full, here, acc);
        }
        // Settings before the program: `CODEX_THREAD_ID=x cmd`, `GIT_SSH_COMMAND=... git`.
        let first = positions.first().copied().unwrap_or(texts.len());
        for a in &texts[..first] {
            if is_identity(a) {
                acc.deny(DenyReason::Fence, a);
            }
            if let Some(r) = a.split_once('=').and_then(|(_, v)| code_rules(v)) {
                acc.deny(r, a);
            }
        }
        for &i in &runnable {
            self.nested_at(words, i, full, here, depth, acc);
        }
        for &p in &runnable {
            // A link to a channel's program under another name.
            let stem = self
                .resolve(texts[p], here)
                .and_then(|r| r.file_stem().map(|s| s.to_string_lossy().into_owned()));
            if stem
                .as_deref()
                .is_some_and(|s| FENCE_PROGRAMS.contains(&s) && fence_args(s, &texts[p + 1..]))
            {
                acc.deny(
                    DenyReason::Fence,
                    &format!(
                        "{} (a link to {})",
                        texts[p..].join(" "),
                        stem.as_deref().unwrap_or_default()
                    ),
                );
            }
            acc.new_session |= stem.as_deref().is_some_and(|s| {
                FENCE_PROGRAMS.contains(&s)
                    && starts_session(s, &texts[p + 1..], positions.contains(&p))
            });
            self.lunchroom_at(&texts, p, here, acc);
            self.recursive_write_at(&texts, p, here, acc);
            // P1.4: a program run by a path this call writes is judged on the text written, and
            // on the file as it is now where there is one (the run may come before the write).
            if texts[p].contains('/') {
                self.run_written(texts[p], here, RunAs::Auto, depth, acc);
            }
            self.shebang_at(&texts, p, here, depth, acc);
        }
        // F1.2: a program run by a path that does not exist can run only if this call makes it.
        // Judged at the command's own program and the programs wrappers the guard knows run
        // (`positions`); a path an unknown program is given (`docker run -v ./data:/d`,
        // `mywrap ./x`) is not known to run, and making every such argument exist would refuse
        // ordinary commands. A build or an install earlier in the text may make it
        // (`cargo build && ./target/release/app`); a redirection or any other command may not
        // (`echo x > s.sh && ./s.sh`).
        for &p in &positions {
            if !*built
                && !acc.lenient
                && texts[p].contains('/')
                && acc.written_as(texts[p], here, &self.layout.home).is_none()
                && !self.fs.exists(&self.physical(texts[p], here))
            {
                acc.deny(
                    DenyReason::Unparsed,
                    &format!("{} (a program that does not exist)", texts[p]),
                );
            }
        }
        *built |= builds(&texts);
        // P1.4: a script a shell or interpreter runs is run, not named
        // (`python3 ~/.lunchroom/bin/lunchroom status`).
        let scripts = script_positions(&texts, &runnable);
        // A1.3: Lunchroom's program run by a shell or interpreter (`python3 …/lunchroom approve`)
        // is judged as if run directly.
        for &k in &scripts {
            self.lunchroom_at(&texts, k, here, acc);
        }
        let text = text_positions(words, &texts, &positions);
        let dirs = word_dirs(words, &texts, &positions);
        for (k, w) in words.iter().enumerate().filter(|(k, _)| !text.contains(k)) {
            // A program named with no `/` is found on the search path, never in the working
            // folder: `cd /dev/pts && echo` opens no file named `/dev/pts/echo`.
            if matches!(dirs[k], Dir::Skip) {
                // It still counts among the strings of the call (L1.1: 1,000 of them).
                acc.resolved += candidates(&w.text).len();
                continue;
            }
            let runs_it = positions.contains(&k) || scripts.contains(&k);
            self.word_paths(&w.text, here, runs_it, dirs[k], acc);
        }
        // L1.1: the folders a search enters. Judged while HELD; recorded in every session, since
        // a session that is not HELD is held by what it then searches (T1.8).
        for &i in &runnable {
            self.recursive_read_at(&texts, i, here, acc);
        }
        if self.held {
            Self::held_rules(words, &texts, &positions, &runnable, acc);
        }
        // `cd` changes where the commands after it run.
        if let Some(&p) = positions.first() {
            match (
                texts[p],
                texts
                    .get(p + 1..)
                    .and_then(|r| r.iter().find(|a| !a.starts_with('-'))),
            ) {
                ("cd" | "pushd", None) => here.clone_from(&self.layout.home),
                ("cd" | "pushd", Some(d)) => *here = normalize(d, here, &self.layout.home),
                ("popd", _) => self.when_held(acc, CallClass::Protected(ProtectedKind::Unparsed)),
                _ => {}
            }
            if texts[p] == "cd" && texts.get(p + 1) == Some(&"-") {
                self.when_held(acc, CallClass::Protected(ProtectedKind::Unparsed));
            }
        }
    }

    /// F1.1, I1.2: the program at position `p` puts a turn into another session, changes an
    /// identity variable, calls a local session chat API, or is given a command as an option value
    /// ([`COMMAND_OPTIONS`]: `git -c core.pager=...`, `ssh -o ProxyCommand=...`).
    fn program_rules(words: &[Word], texts: &[&str], p: usize, known: bool, acc: &mut Acc) {
        let args = &texts[p + 1..];
        let name = basename(texts[p]);
        // V1.4: the program and its arguments, as the guard read this command.
        let what = texts[p..].join(" ");
        let deny = |acc: &mut Acc, r| acc.deny(r, &what);
        if prog_names(&words[p]).iter().any(|n| fence_args(n, args)) {
            deny(acc, DenyReason::Fence);
        }
        // P1.4: judged for every agent and kept only for a HELD one at the end of the call. A bare
        // prompt counts only where the word is sure to be the program: an argument of another
        // program (`grep claude notes`, `tool send codex hi`) starts nothing.
        acc.new_session |= prog_names(&words[p])
            .iter()
            .any(|n| starts_session(n, args, known));
        // Builtins that set a variable named in an argument, a nameref's target
        // (`declare -n r=CODEX_THREAD_ID`) included.
        let sets = matches!(
            name,
            "export"
                | "declare"
                | "typeset"
                | "readonly"
                | "local"
                | "unset"
                | "read"
                | "printf"
                | "mapfile"
                | "readarray"
                | "getopts"
                | "let"
        );
        if sets && args.iter().any(|a| changes_identity(a)) {
            deny(acc, DenyReason::Fence);
        }
        if name == "env" {
            let end = wrapper_inner(texts, p)
                .unwrap_or(texts.len())
                .min(texts.len());
            let clears = |a: &&str| {
                matches!(*a, "-" | "--ignore-environment")
                    || a.strip_prefix('-')
                        .is_some_and(|r| !r.starts_with('-') && r.contains('i'))
            };
            if texts[p + 1..end].iter().any(|a| {
                // `--unset=NAME` and `-uNAME` name the variable in the same word.
                let unset = a
                    .strip_prefix("--unset=")
                    .or_else(|| a.strip_prefix("-u").filter(|r| !r.is_empty()));
                is_identity(a) || unset.is_some_and(is_identity) || clears(a)
            }) {
                deny(acc, DenyReason::Fence);
            }
        }
        if NETWORK_PROGRAMS.contains(&name) && args.iter().any(|a| names_chat_api(a)) {
            deny(acc, DenyReason::Fence);
        }
        let runs = COMMAND_OPTIONS
            .iter()
            .find(|(p, _)| *p == name)
            .map_or(&[][..], |(_, o)| *o);
        for (k, a) in args.iter().enumerate() {
            let after_flag = k > 0 && runs.contains(&args[k - 1]);
            // A one-letter option with its value attached (`tar -Iprog`).
            let attached = runs
                .iter()
                .find(|o| o.len() == 2 && a.len() > 2 && a.starts_with(*o) && !a.starts_with("--"))
                .map(|_| &a[2..]);
            let value = if attached.is_some() {
                attached
            } else if a.starts_with('-') {
                a.split_once('=')
                    .filter(|(o, _)| runs.contains(o))
                    .map(|(_, v)| v)
            } else if after_flag || (name == "less" && a.starts_with('+')) {
                Some(a.split_once('=').map_or(*a, |(_, v)| v))
            } else {
                None
            };
            // An attached value may be a setting (`-oProxyCommand=…`): its text after `=` too.
            let setting = value.and_then(|v| v.split_once('=')).map(|(_, v)| v);
            if let Some(r) = value
                .and_then(code_rules)
                .or_else(|| setting.and_then(code_rules))
            {
                deny(acc, r);
            }
        }
        if name == "git"
            && let Some(i) = args
                .windows(2)
                .position(|w| matches!(w, ["bisect", "run"] | ["submodule", "foreach"]))
            && let Some(r) = code_rules(&args[i + 2..].join(" "))
        {
            deny(acc, r);
        }
    }

    /// P1.4's kinds, and L1.1's searches, judged at the positions that may run a program.
    fn held_rules(
        words: &[Word],
        texts: &[&str],
        positions: &[usize],
        runnable: &[usize],
        acc: &mut Acc,
    ) {
        for &i in runnable {
            let args = &texts[i + 1..];
            for n in prog_names(&words[i]) {
                if let Some(kind) = protected_kind(n, args) {
                    acc.raise(CallClass::Protected(kind));
                }
            }
        }
        // The shell opens a connection for a word naming one of these, a redirection's target too.
        if texts
            .iter()
            .any(|t| t.contains("/dev/tcp/") || t.contains("/dev/udp/"))
        {
            acc.raise(CallClass::Protected(ProtectedKind::Network));
        }
        // `env` with no program after it prints the environment.
        if positions
            .last()
            .is_some_and(|&p| basename(texts[p]) == "env" && wrapper_inner(texts, p).is_some())
        {
            acc.raise(CallClass::Protected(ProtectedKind::Credential));
        }
    }

    /// `sh -c`, `eval`, `env -S`, `su -c`, `watch`, `ssh`, scripts, a shell reading its commands
    /// from a pipe, and code given to an interpreter.
    fn nested_at(
        &self,
        words: &[Word],
        i: usize,
        full: &str,
        here: &Path,
        depth: usize,
        acc: &mut Acc,
    ) {
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        let rest = &texts[i + 1..];
        for name in prog_names(&words[i]) {
            match name {
                n if SHELLS.contains(&n) => {
                    let runs = program_positions(&texts).contains(&i);
                    self.shell_at(rest, runs, full, here, depth, acc);
                }
                // Shell builtins: they run only as the command itself, never as another program's
                // argument (`find . -name x` names a folder).
                "source" | "." if program_positions(&texts).contains(&i) => {
                    if let Some(f) = rest.first() {
                        self.script_file(f, here, depth, acc);
                    }
                }
                "eval" | "ssh" => self.shell_text(&rest.join(" "), here, depth + 1, acc),
                // Their command is the text of `-c` or `--command`, in any spelling they read.
                "su" | "runuser" => {
                    for text in su_commands(rest) {
                        self.shell_text(text, here, depth + 1, acc);
                    }
                }
                // The text of `-c` or `--command`, in any spelling their options are read in.
                "flock" | "script" => {
                    let (valued, longs): (&str, &[(&str, bool)]) = if name == "script" {
                        ("cEIOBoT", SCRIPT_LONG)
                    } else {
                        ("wEc", FLOCK_LONG)
                    };
                    for s in command_option_values(rest, valued, longs, 'c', &["command"]) {
                        self.shell_text(s, here, depth + 1, acc);
                    }
                }
                "env" => {
                    if let Some(s) = option_value(rest, &["--split-string", "-S"]) {
                        self.shell_text(s, here, depth + 1, acc);
                    }
                }
                "watch" => {
                    let from = rest
                        .iter()
                        .position(|a| !a.starts_with('-'))
                        .unwrap_or(rest.len());
                    let skip =
                        usize::from(from > 0 && matches!(rest[from - 1], "-n" | "--interval"));
                    let cmd = rest.get(from + skip..).unwrap_or_default();
                    self.shell_text(&cmd.join(" "), here, depth + 1, acc);
                }
                n if INTERPRETERS.contains(&n) || n.starts_with("python") => {
                    // The script word is itself a runnable position, so `command` also judges it
                    // with `lunchroom_at` (`python3 …/lunchroom approve` is DENIED, A1.3).
                    if matches!(n, "awk" | "gawk" | "mawk" | "sed") {
                        // `sed` and `awk` read their files as data, which runs only when their
                        // program runs it (K1.8): then those files are searched like a script.
                        let data = !self.script_runs_data(n, rest, here);
                        for (k, a) in rest.iter().enumerate() {
                            self.code_text(a, a, acc);
                            let file = if data {
                                script_option_file(n, rest, k)
                            } else {
                                Some(*a)
                            };
                            if let Some(f) = file
                                && let Script::Text(t) = self.fs.script(&self.physical(f, here))
                            {
                                self.code_text(&t, &format!("the script {f}"), acc);
                            }
                        }
                    } else {
                        // Any other interpreter runs inline code (`-c`, `-e`) and its script,
                        // which come first; the words after the script are its arguments, and
                        // the files among them are data (K1.8).
                        let end = interpreter_script(n, rest)
                            .and_then(|s| rest.iter().position(|a| *a == s))
                            .map_or(rest.len(), |k| k + 1);
                        for a in &rest[..end] {
                            self.code_text(a, a, acc);
                        }
                    }
                    // The script it runs, also when attached to its option (`awk -fprog.awk`).
                    // F1.2: one larger than the guard searches is DENIED. A data file is not a
                    // script; a name that is not a file is left to the interpreter, which
                    // resolves names its own way (`node app` loads `app.js`).
                    if let Some(s) = interpreter_script(n, rest)
                        // §7 A1.3: Lunchroom's own program is judged by `lunchroom_at`.
                        && !self.layout.facts(&self.physical(s, here)).program
                    {
                        // P1.4: a script this call writes and then runs is judged on the text
                        // written.
                        self.run_written(s, here, RunAs::Code, depth, acc);
                        match self.fs.script(&self.physical(s, here)) {
                            Script::Text(t) => self.code_text(&t, &format!("the script {s}"), acc),
                            Script::TooLarge => {
                                acc.deny(
                                    DenyReason::Unparsed,
                                    &format!("{s} (a script too large for Lunchroom to read)"),
                                );
                            }
                            Script::Unreadable => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// `runs`: the shell is the program the command runs (not an argument of another program).
    fn shell_at(
        &self,
        rest: &[&str],
        runs: bool,
        full: &str,
        here: &Path,
        depth: usize,
        acc: &mut Acc,
    ) {
        match shell_input(rest) {
            ShellInput::Text(k) => {
                if let Some(s) = k.and_then(|k| rest.get(k)) {
                    self.shell_text(s, here, depth + 1, acc);
                }
            }
            ShellInput::Script(k) => self.script_file(rest[k], here, depth, acc),
            // No -c and no script: the shell reads its commands from its input, which the guard
            // cannot see.
            ShellInput::Stdin => {
                if runs {
                    self.unparsed(full, here, acc);
                }
            }
        }
    }

    /// Whether the `sed` or `awk` program in `args` can run the text it reads ([`sed_runs_text`],
    /// [`awk_runs_text`]): the `-e` texts, else the `-f` file (one the guard cannot read counts as
    /// running), else the first operand, which is not the value of `-F` or `-v`.
    fn script_runs_data(&self, name: &str, args: &[&str], here: &Path) -> bool {
        let exprs: Vec<&str> = (0..args.len())
            .filter_map(|k| {
                let prev = k.checked_sub(1).map(|j| args[j]);
                prev.filter(|p| matches!(*p, "-e" | "--expression" | "--source"))
                    .map(|_| args[k])
                    .or_else(|| args[k].strip_prefix("--expression="))
                    .or_else(|| args[k].strip_prefix("-e").filter(|r| !r.is_empty()))
            })
            .collect();
        let program = if !exprs.is_empty() {
            exprs.join("\n")
        } else if let Some(f) = interpreter_script(name, args) {
            match self.fs.script(&self.physical(f, here)) {
                Script::Text(t) => t,
                Script::TooLarge | Script::Unreadable => return true,
            }
        } else {
            let value = |k: usize| k > 0 && matches!(args[k - 1], "-F" | "-v");
            let operand = args
                .iter()
                .enumerate()
                .find(|&(k, a)| !a.starts_with('-') && !value(k));
            operand.map_or_else(String::new, |(_, a)| (*a).to_owned())
        };
        if name == "sed" {
            sed_runs_text(&program)
        } else {
            awk_runs_text(&program)
        }
    }

    /// A shell script the command runs: checked for the rules that hold in every session. What
    /// else it does is not judged (K1.8).
    /// F1.2: one the guard cannot find, read or search in full is DENIED (`unparsed`), wherever
    /// the shell may run, as channels are judged (`mywrap bash x.sh`). A bare name not in the
    /// working folder is looked up through `PATH`, as the shell does.
    fn script_file(&self, f: &str, here: &Path, depth: usize, acc: &mut Acc) {
        // P1.4: a file this call writes and then runs is judged on the text written; the file as
        // it is now too, where there is one (the run may come before the write).
        let wrote = self.run_written(f, here, RunAs::Shell, depth, acc);
        // §7 A1.3: a mention of a Lunchroom command in Lunchroom's own program is no mention (the
        // command that runs it is judged by `lunchroom_at`).
        if self.layout.facts(&self.physical(f, here)).program {
            return;
        }
        let mut script = self.fs.script(&self.physical(f, here));
        if script == Script::Unreadable && !f.contains('/') {
            match self.fs.which(f) {
                Some(p) => script = self.fs.script(&p),
                // L1.1: a program the search for it was cut short for is one Lunchroom could not
                // check, not one that is not there.
                None if self.fs.cut(Path::new(f)) => {
                    self.unresolved(false, true, f, acc);
                    return;
                }
                None => {}
            }
        }
        match script {
            Script::Text(t) => self.shell_script(&t, &format!("the script {f}"), here, depth, acc),
            Script::TooLarge | Script::Unreadable if wrote => {}
            Script::TooLarge | Script::Unreadable => {
                acc.deny(
                    DenyReason::Unparsed,
                    &format!("{f} (a script Lunchroom cannot find, read or search in full)"),
                );
            }
        }
    }

    /// Records a file this call writes, for the commands after it that run it (P1.4): its text
    /// replaces the text of an earlier write of the same file, or is added to it where the write
    /// appends (an append to text the call does not show leaves none shown).
    fn note_written(&self, w: writes::Written, cwd: &Path, acc: &mut Acc) {
        let path = normalize(&w.target, cwd, &self.layout.home);
        acc.note(Dir::Write, &[self.physical(&w.target, cwd), path.clone()]);
        let prior = acc.written.iter().position(|o| o.path == path);
        let body = match (w.append, prior.map(|k| acc.written[k].body.clone())) {
            (true, Some(Some(old))) => w.body.map(|new| old + &new),
            (true, Some(None)) => None,
            _ => w.body,
        };
        let entry = WrittenFile {
            literal: w.target,
            path,
            body,
            active: false,
        };
        match prior {
            Some(k) => acc.written[k] = entry,
            None => acc.written.push(entry),
        }
    }

    /// The file `f`, which a command of this call runs, was written by this call (P1.4):
    /// judges the text written as the script it is, `how` it is run, and says whether there was
    /// one. Where the call does not show the text it wrote, the call is PROTECTED while the
    /// session is HELD.
    fn run_written(&self, f: &str, here: &Path, how: RunAs, depth: usize, acc: &mut Acc) -> bool {
        let path = normalize(f, here, &self.layout.home);
        // Every write of the file counts: the last one decides what runs, and the call does not
        // tell which that is when a command writes it more than once.
        let matches: Vec<usize> = (0..acc.written.len())
            .filter(|&k| acc.written[k].path == path || acc.written[k].literal == f)
            .collect();
        if matches.is_empty() {
            return false;
        }
        for at in matches {
            // Judging the text of a file that runs itself must end.
            if acc.written[at].active {
                continue;
            }
            let (body, literal) = (
                acc.written[at].body.clone(),
                acc.written[at].literal.clone(),
            );
            let Some(text) = body else {
                self.when_held(acc, CallClass::Protected(ProtectedKind::Unparsed));
                continue;
            };
            let source = format!("the file {literal}, which this call writes");
            let code = match how {
                RunAs::Shell => false,
                RunAs::Code => true,
                RunAs::Auto => script_is_code(&text, &literal),
            };
            acc.written[at].active = true;
            if code {
                self.code_text(&text, &source, acc);
            } else {
                self.shell_script(&text, &source, here, depth, acc);
            }
            if let Some(w) = acc.written.get_mut(at) {
                w.active = false;
            }
        }
        true
    }

    /// A shell script's text, `source` naming it (`the script ./x.sh`), judged with the session's
    /// own state (P1.4, F1.2): while the session is HELD, each part of it that
    /// would be PROTECTED as a command of its own makes the call PROTECTED, and each part that
    /// would be DENIED makes it DENIED. Text the reader can read whole is read as one text; any
    /// other is read piece by piece ([`Guard::script_pieces`]) and as code ([`code_rules`]).
    fn shell_script(&self, text: &str, source: &str, here: &Path, depth: usize, acc: &mut Acc) {
        // F1.2: a variable the script gave a literal is that literal, not an unknown word.
        let literal = literals::substitute(text);
        let text = literal.as_deref().unwrap_or(text);
        // I1.2: any mention of an identity variable in a script counts, parsed or not.
        if mentions_identity(text) {
            let word = tokens(text).into_iter().find(|t| is_identity(t));
            acc.deny(
                DenyReason::Fence,
                &format!("{} in {source}", word.unwrap_or("an identity variable")),
            );
        }
        // A1.3, F1.4: a mention anywhere in the text counts, a string it only prints included.
        Self::code_mentions(text, source, acc);
        let whole = shell::parse(text, &self.home()) != Parsed::Unparsed;
        if !whole {
            self.code_text(text, source, acc);
        }
        let mut sub = Acc::new(CallClass::Ordinary);
        sub.resolved = acc.resolved;
        sub.written = std::mem::take(&mut acc.written);
        if whole {
            self.shell_text(text, here, depth + 1, &mut sub);
        } else {
            self.script_pieces(text, here, depth + 1, &mut sub);
        }
        if sub.denied() {
            sub.fault = Some(match sub.fault.take() {
                Some(f) => format!("{f}, in {source}"),
                None => source.to_owned(),
            });
        }
        acc.absorb(sub);
    }

    /// A script the reader cannot read whole, judged piece by piece: each statement, as a command
    /// of its own, with the control words of the shell (`if`, `then`, `do`, `done`, `fi`, …) and a
    /// function's header taken off it, and `cd` carried from one statement to the next. A piece the
    /// reader still cannot read is judged by its words ([`Guard::unparsed`]).
    fn script_pieces(&self, text: &str, here: &Path, depth: usize, acc: &mut Acc) {
        let body = without_heredoc_bodies(text);
        let mut here = here.to_path_buf();
        let was = std::mem::replace(&mut acc.lenient, true);
        for raw in split_unquoted(&body) {
            let Some(piece) = script_piece(raw) else {
                continue;
            };
            self.shell_text(&piece, &here, depth, acc);
            if let Parsed::Commands(cmds) = shell::parse(&piece, &self.home()) {
                for c in &cmds {
                    let t: Vec<&str> = c.words.iter().map(|w| w.text.as_str()).collect();
                    if let ["cd" | "pushd", d, ..] = t.as_slice() {
                        here = normalize(d, &here, &self.layout.home);
                    }
                }
            }
        }
        acc.lenient = was;
    }

    /// A1.3, F1.4 for script or code text: a mention of a Lunchroom command only the user may run
    /// (a string it only prints included), of `TIOCSTI` or of its number makes the call DENIED.
    /// `what` names the text for the message (V1.4).
    fn code_mentions(text: &str, what: &str, acc: &mut Acc) {
        if let Some(m) = lunchroom_mention(text) {
            acc.deny(DenyReason::Configuration, &format!("{m} (in {what})"));
        }
        if names_tiocsti(text) {
            acc.deny(
                DenyReason::Configuration,
                &format!("TIOCSTI or its number (in {what})"),
            );
        }
    }

    /// Script or code text that is not a shell command (K1.8), `what` naming it for a denial
    /// (V1.4). Every rule of [`code_rules`], identity changes included.
    fn code_text(&self, text: &str, what: &str, acc: &mut Acc) {
        let home = self.home();
        let text = text.replace("${HOME}", &home).replace("$HOME", &home);
        if let Some(r) = code_rules(&text) {
            acc.deny(r, what);
        }
        // §17.5: ptrace through python or perl is DENIED, also with the target process id written
        // in the call and the process in the agent's own subtree (a program in these languages
        // reaches ptrace by a name or a system call number the guard reads only as text).
        if names_ptrace(&text) {
            acc.deny(DenyReason::Configuration, what);
        }
        // F1.4, A1.3, §17.5: a string of the code that names another terminal, a path of
        // Lunchroom's or another process's secrets is judged as the shell judges the path.
        for (start, end) in quoted_spans(&text) {
            for c in candidates(&text[start + 1..end]) {
                if !c.starts_with(['/', '~']) {
                    continue;
                }
                let f = self
                    .layout
                    .facts(&normalize(c, Path::new("/"), &self.layout.home));
                if f.terminal || f.store {
                    acc.deny(DenyReason::Configuration, c);
                }
                self.other_process(f.secret_pid, c, acc);
            }
        }
        // P1.4 (§6::53): while the session is HELD, a part of the code that would be PROTECTED as a
        // command of its own makes the call PROTECTED.
        if self.held {
            for kind in code_kinds(&text) {
                acc.raise(CallClass::Protected(kind));
            }
        }
        acc.new_session |= code_starts_session(&text);
        // P1.4: the string literals of one list or call that starts a program are one command.
        Self::gated_code(&text, acc);
    }

    /// The memory, environment, descriptors and maps of a process outside the agent's own subtree
    /// (`secret_pid`, from the path's facts) are DENIED in every session (§17.5, L1.1; the lesson of
    /// the 2026 Hugging Face incident, where agents dumped other processes' environments for
    /// tokens).
    fn other_process(&self, secret_pid: Option<u32>, named: &str, acc: &mut Acc) {
        if secret_pid.is_none_or(|pid| self.own_process(pid)) {
            return;
        }
        acc.deny(DenyReason::DeniedPath, named);
    }

    /// The process `pid` is in the agent's own subtree: the harness process its commands run
    /// under, or a descendant. The console and every other user's process are not.
    fn own_process(&self, pid: u32) -> bool {
        // The guard itself is what `/proc/self` resolves to, and it runs in the agent's subtree.
        self.layout.console_pid != Some(pid)
            && (pid == std::process::id()
                || self
                    .layout
                    .agent_root
                    .is_some_and(|root| self.fs.descends_from(pid, root)))
    }

    /// 1.1, the console's wall: the rules the kernel leaves open. `p` is the position of a program
    /// word, `full` the whole text of the command.
    fn console_wall(&self, texts: &[&str], p: usize, full: &str, here: &Path, acc: &mut Acc) {
        let name = basename(texts[p]);
        let args = &texts[p + 1..];
        let what = texts[p..].join(" ");
        // Rule 3 (§17.5): a debugger or tracer given a process id that is not written in the call
        // or is not in the agent's own subtree. One that starts a program of its own runs.
        if trace::DEBUGGERS.contains(&name) {
            // gdb and lldb read commands from their input when a pipe or a redirection feeds them.
            let fed = full.contains(['|', '<']);
            let read = |f: &str| match self.fs.script(&self.physical(f, here)) {
                Script::Text(t) => Some(t),
                Script::TooLarge | Script::Unreadable => None,
            };
            let foreign =
                trace::target_pids(name, args, fed, &read)
                    .into_iter()
                    .any(|pid| match pid {
                        trace::Pid::Written(n) => !self.own_process(n),
                        trace::Pid::Unwritten => true,
                    });
            if foreign {
                acc.deny(DenyReason::Configuration, &what);
            }
            // §17.5 rule 3: a command given on the command line that makes the program ptrace
            // (`call (int)ptrace(…)`) reaches any process, as code in python or perl does.
            if debugger_payloads(args).iter().any(|c| names_ptrace(c)) {
                acc.deny(DenyReason::Configuration, &what);
            }
        }
        // Rule 2: signalling the console, or anything called lunchroom.
        if KILLERS.contains(&name) {
            let pid = self.layout.console_pid.map(|p| p.to_string());
            let names_pid = pid.as_deref().is_some_and(|pid| {
                args.iter()
                    .any(|a| *a == pid || a.strip_prefix('-').is_some_and(|r| r == pid))
            });
            if names_pid
                || names_word(full, "lunchroom")
                || terminal::kill_pattern_matches(name, args)
                || terminal::kill_names_user(args, &self.layout.lunchroom_ids)
            {
                acc.deny(DenyReason::Configuration, &what);
            }
        }
        // Rule 5: becoming the user or the group `lunchroom`.
        if becomes_lunchroom(name, args, &self.layout.lunchroom_ids) {
            acc.deny(DenyReason::Configuration, &what);
        }
        // Rule 4: another terminal's keys and screen through tmux or screen.
        match name {
            "tmux" => self.tmux_keys(args, here, &what, acc),
            "screen" => self.screen_keys(args, here, &what, acc),
            _ => {}
        }
    }

    /// The real path of the program a word runs: by its path, or found through `PATH`.
    fn resolve(&self, w: &str, here: &Path) -> Option<PathBuf> {
        if w.contains('/') {
            Some(self.physical(w, here))
        } else {
            self.fs.which(w)
        }
    }

    /// A program run by path or found through `PATH` that is a script (a `#!` line).
    fn shebang_at(&self, texts: &[&str], p: usize, here: &Path, depth: usize, acc: &mut Acc) {
        let found = self.resolve(texts[p], here);
        // L1.1: a program the search for it was cut short for is one Lunchroom could not check.
        if found.is_none() && !texts[p].contains('/') && self.fs.cut(Path::new(texts[p])) {
            self.unresolved(false, true, texts[p], acc);
            return;
        }
        let Some(path) = found.filter(|p| self.fs.runs_as_script(p)) else {
            return;
        };
        // §7 A1.3: a mention of a Lunchroom command in Lunchroom's own program is no mention.
        if self.layout.facts(&path).program {
            return;
        }
        let t = match self.fs.script(&path) {
            Script::Text(t) => t,
            // F1.2: a script it cannot read or search in full.
            Script::TooLarge | Script::Unreadable => {
                acc.deny(
                    DenyReason::Unparsed,
                    &format!(
                        "{} (a script Lunchroom cannot read or search in full)",
                        path.display()
                    ),
                );
                return;
            }
        };
        let source = format!("the script {}", path.display());
        // No `#!` line: the shell that started it runs it as a script of its own.
        if !t.starts_with("#!") {
            self.shell_script(&t, &source, here, depth, acc);
            return;
        }
        {
            let first = t.lines().next().unwrap_or_default();
            let mut interp = first.trim_start_matches("#!").split_whitespace();
            let mut name = interp.next().map(basename);
            if name == Some("env") {
                name = interp.find(|a| !a.starts_with('-') && !is_assignment(a));
            }
            if name.is_some_and(|n| SHELLS.contains(&n)) {
                self.shell_script(&t, &source, here, depth, acc);
            } else {
                self.code_text(&t, &source, acc);
            }
        }
    }

    /// Whether the word names Lunchroom's program, by its name or its real path.
    fn is_lunchroom(&self, w: &str, here: &Path) -> bool {
        LUNCHROOM_NAMES.contains(&basename(w))
            || self
                .resolve(w, here)
                .is_some_and(|r| self.layout.facts(&r).program || self.is_a_program_file(&r))
    }

    /// C6-X09: `r` is one of Lunchroom's program files under any name and folder (a hard link):
    /// the installed program, each other place it is installed, and the setgid program.
    fn is_a_program_file(&self, r: &Path) -> bool {
        // Every place is looked at, whatever the first answer: the guard's first pass lists each
        // look it will need, and answers none of them.
        let id = self.fs.file_id(r);
        let ids: Vec<_> = std::iter::once(self.layout.program_dir.join(LUNCHROOM_NAMES[0]))
            .chain(self.layout.programs.iter().cloned())
            .chain(std::iter::once(PathBuf::from(crate::setgid::PROGRAM)))
            .map(|p| self.fs.file_id(&p))
            .collect();
        id.is_some_and(|id| ids.contains(&Some(id)))
    }

    /// §6 item 1: the text is exactly one `lunchroom send`, with nothing before the program and
    /// no other command, separator or redirection. The program is found by its real path only, and
    /// must be Lunchroom's program itself: a program merely named `lunchroom`, or another program
    /// in Lunchroom's program folder, is not.
    fn is_reply(&self, text: &str, cwd: &Path) -> bool {
        shell::single(text, &self.home()).is_some_and(|c| {
            matches!(c.words.as_slice(), [p, s, ..]
                if s.text == "send" && !p.glob && !s.glob && self.is_real_program(&p.text, cwd))
        })
    }

    /// §6 item 1, D1.1: the text is exactly one `lunchroom read <id>`: Lunchroom's own program
    /// (as [`Guard::is_reply`] finds it), the word `read`, and one id, nothing else.
    fn is_read(&self, text: &str, cwd: &Path) -> bool {
        shell::single(text, &self.home()).is_some_and(|c| {
            matches!(c.words.as_slice(), [p, r, id]
                if r.text == "read"
                    && !p.glob
                    && !r.glob
                    && !id.glob
                    && !id.redirect
                    && !id.text.is_empty()
                    && !id.text.starts_with('-')
                    && self.is_real_program(&p.text, cwd))
        })
    }

    /// The word names Lunchroom's program itself, by its real path: the installed one or one the
    /// layout lists. A program merely named `lunchroom` is not.
    fn is_real_program(&self, w: &str, cwd: &Path) -> bool {
        let installed = self
            .fs
            .real(&self.layout.program_dir.join(LUNCHROOM_NAMES[0]));
        self.resolve(w, cwd)
            .is_some_and(|r| r == installed || self.layout.programs.contains(&r))
    }

    /// A1.3: an agent runs only Lunchroom's open commands.
    fn lunchroom_at(&self, texts: &[&str], p: usize, here: &Path, acc: &mut Acc) {
        // In a statement of a script the reader could not read whole, a program that only has
        // Lunchroom's name (`tools/lunchroom --pattern x`) is judged by the words after it, as the
        // script's text is ([`names_user_only_command`]); Lunchroom's own program by its arguments.
        let is_it = if acc.lenient {
            self.resolve(texts[p], here)
                .is_some_and(|r| self.layout.facts(&r).program)
        } else {
            self.is_lunchroom(texts[p], here)
        };
        if is_it && !lunchroom_args_open(&texts[p + 1..]) {
            acc.deny(DenyReason::Configuration, &texts[p..].join(" "));
        }
    }

    /// P1.4: a program that changes everything below a folder, given a folder holding Lunchroom's
    /// stores, writes them.
    fn recursive_write_at(&self, texts: &[&str], p: usize, here: &Path, acc: &mut Acc) {
        let name = basename(texts[p]);
        let args = &texts[p + 1..];
        let finds = name == "find" && args.iter().any(|a| FIND_ACTIONS.contains(a));
        if !RECURSIVE_WRITE.contains(&name) && !finds {
            return;
        }
        let mut operands: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('-'))
            .collect();
        if finds && operands.is_empty() {
            operands.push(".");
        }
        for a in operands {
            let forms = self.forms(a, here);
            if forms
                .paths
                .iter()
                .any(|f| self.layout.lunchroom_home.starts_with(f))
                || forms
                    .capped
                    .is_some_and(|c| self.layout.lunchroom_home.starts_with(c))
            {
                acc.deny(
                    DenyReason::Configuration,
                    &format!(
                        "{} {a} (it changes everything in {a}, Lunchroom's folder among it)",
                        texts[p]
                    ),
                );
            }
        }
    }

    /// L1.1: a program that reads everything below the folders it is given, or below the working
    /// folder for a search given none.
    fn recursive_read_at(&self, texts: &[&str], i: usize, here: &Path, acc: &mut Acc) {
        let name = basename(texts[i]);
        let args = &texts[i + 1..];
        let recurse_flag = args.iter().any(|a| {
            *a == "--recursive"
                || a.strip_prefix('-').is_some_and(|r| {
                    !r.starts_with('-') && r.contains(if name == "ls" { 'R' } else { 'r' })
                })
                || (name != "ls"
                    && a.strip_prefix('-')
                        .is_some_and(|r| !r.starts_with('-') && r.contains('R')))
        });
        let searcher =
            SEARCHERS.contains(&name) || (RECURSE_ON_FLAG.contains(&name) && recurse_flag);
        if !searcher && !COPIERS.contains(&name) {
            return;
        }
        let greps = GREPS.contains(&name);
        let operands: Vec<&str> = if greps {
            grep_path_operands(args)
        } else {
            args.iter()
                .copied()
                .filter(|a| !a.starts_with('-'))
                .collect()
        };
        // Each folder with the operand that named it (the part at fault a message shows).
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        for a in &operands {
            let forms = self.forms(a, here);
            roots.extend(
                forms
                    .paths
                    .into_iter()
                    .filter(|f| self.fs.is_dir(f))
                    .map(|f| (f, (*a).to_owned())),
            );
        }
        // `grep -r`, `du` and `ls -R` given only files read only those files; a word the shell
        // expands into the working folder (`*`, `$X`) may name its folders, so it still counts as
        // given none. Other searchers take patterns and expressions among their words, so for
        // them only a folder operand counts.
        let given = if greps || matches!(name, "du" | "ls") {
            !operands.is_empty()
                && (!roots.is_empty()
                    || !operands
                        .iter()
                        .any(|a| a.contains(['*', '?', '[', '$', '`', '{'])))
        } else {
            !roots.is_empty()
        };
        if !given && searcher {
            roots.push((here.to_path_buf(), here.to_string_lossy().into_owned()));
        }
        // §17.5: a program that reads the content of everything below `/proc` or a process's
        // folder in it reads `mem`, `environ`, `fd` and `maps` of the processes there, in every
        // session. Listing names (`ls -R`, `du`, `tree`, `find` with no action) reads none.
        let reads_content = match name {
            "find" => args
                .iter()
                .any(|a| FIND_ACTIONS.contains(a) && *a != "-delete"),
            "fd" | "fdfind" => args
                .iter()
                .any(|a| matches!(*a, "-x" | "-X" | "--exec" | "--exec-batch")),
            "tree" | "du" | "ls" => false,
            _ => true,
        };
        if reads_content {
            for (r, named) in &roots {
                match proc_secret_folder(r) {
                    Some(None) => acc.deny(DenyReason::DeniedPath, named),
                    Some(pid) => self.other_process(pid, named, acc),
                    None => {}
                }
            }
        }
        for (r, named) in roots {
            if self.held {
                self.search_from(&r, &named, acc);
            } else {
                acc.access.searches.insert(r);
            }
        }
    }
}

mod braces;
mod escapes;
mod folder;
mod gated;
mod literals;
mod marks;
mod recipes;
mod regex;
mod terminal;
mod trace;
mod writes;

pub use folder::reads_its_folder;
pub use gated::{Gate, KNOWN_PROGRAMS, action_verb, known_programs};
pub use marks::{Mark, MarkKind, MarkSource, Marked};
pub use writes::Access;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "console"))]
mod console_tests;
