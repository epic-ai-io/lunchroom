//! The terminal channel and the signals (spec §12 F1.4, §17 item 5): tmux and screen commands that
//! type into or read another terminal, and the patterns of `pkill` and `killall` that name the
//! console.
//!
//! The command table is `tmux list-commands` of tmux 3.4; screen's commands are `stuff` and
//! `hardcopy` and the commands that carry them (`eval`, `source`).

use std::path::Path;

use crate::shell::{self, Parsed};

use super::{Acc, CallClass, DenyReason, Guard, MAX_NESTING, ProtectedKind, Script, regex};

/// tmux's commands and their aliases (`tmux list-commands`, tmux 3.4).
// protocol-words:begin (tmux's own command names)
pub(super) const TMUX_COMMANDS: &[(&str, &str)] = &[
    ("attach-session", "attach"),
    ("bind-key", "bind"),
    ("break-pane", "breakp"),
    ("capture-pane", "capturep"),
    ("choose-buffer", ""),
    ("choose-client", ""),
    ("choose-tree", ""),
    ("clear-history", "clearhist"),
    ("clear-prompt-history", "clearphist"),
    ("clock-mode", ""),
    ("command-prompt", ""),
    ("confirm-before", "confirm"),
    ("copy-mode", ""),
    ("customize-mode", ""),
    ("delete-buffer", "deleteb"),
    ("detach-client", "detach"),
    ("display-menu", "menu"),
    ("display-message", "display"),
    ("display-popup", "popup"),
    ("display-panes", "displayp"),
    ("find-window", "findw"),
    ("has-session", "has"),
    ("if-shell", "if"),
    ("join-pane", "joinp"),
    ("kill-pane", "killp"),
    ("kill-server", ""),
    ("kill-session", ""),
    ("kill-window", "killw"),
    ("last-pane", "lastp"),
    ("last-window", "last"),
    ("link-window", "linkw"),
    ("list-buffers", "lsb"),
    ("list-clients", "lsc"),
    ("list-commands", "lscm"),
    ("list-keys", "lsk"),
    ("list-panes", "lsp"),
    ("list-sessions", "ls"),
    ("list-windows", "lsw"),
    ("load-buffer", "loadb"),
    ("lock-client", "lockc"),
    ("lock-server", "lock"),
    ("lock-session", "locks"),
    ("move-pane", "movep"),
    ("move-window", "movew"),
    ("new-session", "new"),
    ("new-window", "neww"),
    ("next-layout", "nextl"),
    ("next-window", "next"),
    ("paste-buffer", "pasteb"),
    ("pipe-pane", "pipep"),
    ("previous-layout", "prevl"),
    ("previous-window", "prev"),
    ("refresh-client", "refresh"),
    ("rename-session", "rename"),
    ("rename-window", "renamew"),
    ("resize-pane", "resizep"),
    ("resize-window", "resizew"),
    ("respawn-pane", "respawnp"),
    ("respawn-window", "respawnw"),
    ("rotate-window", "rotatew"),
    ("run-shell", "run"),
    ("save-buffer", "saveb"),
    ("select-layout", "selectl"),
    ("select-pane", "selectp"),
    ("select-window", "selectw"),
    ("send-keys", "send"),
    ("send-prefix", ""),
    ("server-access", ""),
    ("set-buffer", "setb"),
    ("set-environment", "setenv"),
    ("set-hook", ""),
    ("set-option", "set"),
    ("set-window-option", "setw"),
    ("show-buffer", "showb"),
    ("show-environment", "showenv"),
    ("show-hooks", ""),
    ("show-messages", "showmsgs"),
    ("show-options", "show"),
    ("show-prompt-history", "showphist"),
    ("show-window-options", "showw"),
    ("source-file", "source"),
    ("split-window", "splitw"),
    ("start-server", "start"),
    ("suspend-client", "suspendc"),
    ("swap-pane", "swapp"),
    ("swap-window", "swapw"),
    ("switch-client", "switchc"),
    ("unbind-key", "unbind"),
    ("unlink-window", "unlinkw"),
    ("wait-for", "wait"),
];
// protocol-words:end

/// The command a tmux word names: an alias or a full name exactly, else the one full name the word
/// is a prefix of (tmux's `cmd_find`); an ambiguous or unknown word names none.
pub(super) fn tmux_command(word: &str) -> Option<&'static str> {
    if let Some((n, _)) = TMUX_COMMANDS
        .iter()
        .find(|(n, a)| *n == word || (!a.is_empty() && *a == word))
    {
        return Some(n);
    }
    let mut hits = TMUX_COMMANDS
        .iter()
        .filter(|(n, _)| !word.is_empty() && n.starts_with(word));
    match (hits.next(), hits.next()) {
        (Some((n, _)), None) => Some(n),
        _ => None,
    }
}

/// The panes a tmux command names with `-t`: read out of any cluster of options (`-lt %5`,
/// `-pt%5`), where `with_value` are the other one-letter options that take a value; and, whatever
/// the options, any word `-t` or beginning `-t` (tmux's own reading ends at the first operand, so
/// this reads more, never less).
fn tmux_targets<'a>(rest: &[&'a str], with_value: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut k = 0;
    while let Some(a) = rest.get(k).copied() {
        k += 1;
        if a == "--" || !a.starts_with('-') || a.len() == 1 {
            break;
        }
        let cluster = &a[1..];
        for (at, c) in cluster.char_indices() {
            if c == 't' || with_value.contains(c) {
                let attached = &cluster[at + c.len_utf8()..];
                let value = if attached.is_empty() {
                    let v = rest.get(k).copied();
                    k += 1;
                    v
                } else {
                    Some(attached)
                };
                if c == 't' {
                    out.extend(value);
                }
                break;
            }
        }
    }
    for (i, a) in rest.iter().enumerate() {
        if *a == "-t" {
            out.extend(rest.get(i + 1).copied());
        } else if let Some(t) = a.strip_prefix("-t").filter(|t| !t.is_empty()) {
            out.push(t);
        }
    }
    out
}

/// The operands of a tmux command: the words after its options, where `with_value` are the
/// one-letter options that take a value.
fn tmux_operands<'a>(rest: &[&'a str], with_value: &str) -> Vec<&'a str> {
    let mut k = 0;
    while let Some(a) = rest.get(k).copied() {
        if a == "--" {
            k += 1;
            break;
        }
        if !a.starts_with('-') || a.len() == 1 {
            break;
        }
        k += 1;
        let cluster = &a[1..];
        if let Some((at, c)) = cluster
            .char_indices()
            .find(|(_, c)| with_value.contains(*c))
            && cluster[at + c.len_utf8()..].is_empty()
        {
            k += 1;
        }
    }
    rest.get(k..).unwrap_or_default().to_vec()
}

/// screen's options, read as it reads them (clusters such as `-xS name` and `-dX` included): the
/// index of the word that holds `-X`, and the session named by `-S`, or by `-r` or `-x` with a
/// value of its own.
fn screen_options<'a>(args: &[&'a str]) -> (Option<usize>, Option<&'a str>) {
    let (mut x_at, mut target) = (None, None);
    let mut k = 0;
    while let Some(a) = args.get(k).copied() {
        k += 1;
        if a.starts_with("--") || !a.starts_with('-') || a.len() == 1 {
            continue;
        }
        let cluster = &a[1..];
        for (at, c) in cluster.char_indices() {
            let attached = &cluster[at + c.len_utf8()..];
            match c {
                'X' => {
                    x_at.get_or_insert(k - 1);
                }
                'S' => {
                    let v = if attached.is_empty() {
                        let v = args.get(k).copied();
                        k += 1;
                        v
                    } else {
                        Some(attached)
                    };
                    target = v.or(target);
                    break;
                }
                // Options with a value of their own.
                'c' | 'e' | 'h' | 'p' | 's' | 'T' | 't' | 'P' | 'Q' => {
                    if attached.is_empty() {
                        k += 1;
                    }
                    break;
                }
                // Options with an optional session name.
                'r' | 'x' if attached.is_empty() => {
                    if let Some(v) = args.get(k).filter(|v| !v.starts_with('-')) {
                        target = Some(v);
                        k += 1;
                    }
                }
                _ => {}
            }
        }
    }
    (x_at, target)
}

/// What the console's name or command line is searched for in: `pgrep` matches a pattern against
/// the process name, or with `-f` the whole command line.
fn console_lines() -> [String; 4] {
    const VERB: &str = concat!("con", "sole");
    const NAME: &str = "lunch-room";
    let name = NAME.replace('-', "");
    [
        name.clone(),
        format!("{name} {VERB}"),
        format!("/usr/local/lib/{name}/{name}"),
        format!("/usr/local/lib/{name}/{name} {VERB}"),
    ]
}

/// `pkill` and `killall -r` take a pattern: one that can match the console's name or command line
/// is a kill naming it, whatever its spelling (`lunchr[o]om`, `l.nchroom`). A pattern the matcher
/// does not follow counts as a match.
pub(super) fn kill_pattern_matches(name: &str, args: &[&str]) -> bool {
    let regex = match name {
        "pkill" => true,
        "killall" => args.iter().any(|a| {
            matches!(*a, "-r" | "--regexp")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains('r'))
        }),
        _ => false,
    };
    let lines = console_lines();
    regex
        && args.iter().filter(|a| !a.starts_with('-')).any(|a| {
            lines
                .iter()
                .any(|line| regex::search(a, line).unwrap_or(true))
        })
}

/// The long options of `pkill`, `pgrep` and `killall`: name, and whether it takes a value
/// (pgrep(1), killall(1)).
const KILL_LONG: &[(&str, bool)] = &[
    ("euid", true),
    ("uid", true),
    ("group", true),
    ("user", true),
    ("pgroup", true),
    ("process-group", true),
    ("session", true),
    ("terminal", true),
    ("parent", true),
    ("pidfile", true),
    ("older", true),
    ("older-than", true),
    ("younger-than", true),
    ("signal", true),
    ("delimiter", true),
    ("runstates", true),
    ("queue", true),
    ("ns", true),
    ("nslist", true),
    ("cgroup", true),
    ("env", true),
    ("context", true),
    ("inverse", false),
    ("lightweight", false),
    ("count", false),
    ("full", false),
    ("exact", false),
    ("newest", false),
    ("oldest", false),
    ("ignore-case", false),
    ("list-name", false),
    ("list-full", false),
    ("echo", false),
    ("require-handler", false),
    ("logpidfile", false),
    ("regexp", false),
    ("wait", false),
    ("quiet", false),
    ("verbose", false),
    ("interactive", false),
    ("help", false),
    ("version", false),
];

/// `pkill` and `killall` select processes by user or group: `-u`, `-U`, `-G` (also inside a cluster
/// and with the value attached: `-9u name`), `--euid`, `--uid`, `--group` and `--user`, or a
/// unique prefix of one (`--eu`), by name or by id; naming the user or group of the console is a
/// kill naming it. A list may hold several (`bob,998`).
pub(super) fn kill_names_user(args: &[&str], ids: &[u32]) -> bool {
    let who = "lunch-room".replace('-', "");
    let names = |list: &str| {
        list.split(',').any(|v| {
            v == who
                || v.trim_start_matches('#')
                    .parse::<u32>()
                    .is_ok_and(|n| ids.contains(&n))
        })
    };
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
            // A prefix that two options share (`--u`: `--uid` of pkill, `--user` of killall) is
            // read as each of them.
            let candidates: Vec<&(&str, bool)> = KILL_LONG
                .iter()
                .filter(|(n, _)| *n == opt || (!opt.is_empty() && n.starts_with(opt)))
                .collect();
            let takes = candidates.iter().any(|(_, t)| *t);
            let value = if attached.is_some() || !takes {
                attached
            } else {
                let v = args.get(k).copied();
                k += 1;
                v
            };
            let selects = candidates
                .iter()
                .any(|(n, _)| matches!(*n, "euid" | "uid" | "group" | "user"));
            if selects && value.is_some_and(names) {
                return true;
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            let cluster = &a[1..];
            for (at, c) in cluster.char_indices() {
                // The one-letter options that take a value.
                if "uUGgsPtdFOqr".contains(c) {
                    let rest = &cluster[at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        let v = args.get(k).copied();
                        k += 1;
                        v
                    } else {
                        Some(rest)
                    };
                    if "uUG".contains(c) && value.is_some_and(names) {
                        return true;
                    }
                    break;
                }
            }
        }
    }
    // Whatever the option reading made of the words: `-u`, `-U`, `-G` as words of their own.
    args.windows(2)
        .any(|w| matches!(w[0], "-u" | "-U" | "-G") && names(w[1]))
}

impl Guard<'_> {
    /// The words of a command line that tmux or screen reads as its own (a line of a file, a command
    /// handed to `eval`, `if-shell` or `bind-key`), with its quotes taken off as the classifier
    /// takes them off a shell word (`"stuff" 'x y'` is `stuff`, `x y`); `;` stays as a word where it
    /// separates two commands. Text the shell reader cannot read is split at blanks, quotes
    /// trimmed.
    fn carried(&self, text: &str) -> Vec<String> {
        match shell::parse(text, &self.home()) {
            Parsed::Commands(cmds) => {
                let mut out = Vec::new();
                for (i, c) in cmds.iter().enumerate() {
                    if i > 0 {
                        out.push(";".to_owned());
                    }
                    out.extend(c.words.iter().map(|w| w.text.clone()));
                }
                out
            }
            Parsed::Unparsed => text
                .split_whitespace()
                .map(|w| w.trim_matches(['"', '\'']).to_owned())
                .collect(),
        }
    }

    /// Rule 4 for tmux: `send-keys`, `send-prefix`, `capture-pane`, `pipe-pane` and `paste-buffer` type into or
    /// read a pane, and `source-file`, `if-shell`, `run-shell`, `bind-key`, `set-hook` and
    /// `confirm-before` carry tmux commands to run. The agent's own pane (`TMUX_PANE`, named by
    /// `-t` or by none) is PROTECTED while HELD; any other target, or another tmux server (`-L`,
    /// `-S`), is DENIED. A command is named by any unambiguous prefix of its name, as tmux reads
    /// it, and `-t` is read inside a cluster of options (`-lt %5`).
    pub(super) fn tmux_keys(&self, args: &[&str], here: &Path, what: &str, acc: &mut Acc) {
        let mut other_server = false;
        let mut k = 0;
        // Global options before the subcommand; `-L`, `-S`, `-f`, `-c`, `-T` take a value, also
        // inside a cluster (`-uL name`).
        while let Some(a) = args.get(k).filter(|a| a.starts_with('-') && a.len() > 1) {
            k += 1;
            for (at, c) in a[1..].char_indices() {
                if matches!(c, 'L' | 'S' | 'f' | 'c' | 'T') {
                    other_server |= matches!(c, 'L' | 'S');
                    if a[1 + at + 1..].is_empty() {
                        k += 1;
                    }
                    break;
                }
            }
        }
        let rest = args.get(k..).unwrap_or_default();
        self.tmux_commands(rest, other_server, here, what, 0, acc);
    }

    /// The tmux commands in `words`, separated by `;`, each judged.
    #[allow(clippy::too_many_lines)] // one arm per command that types or carries commands
    fn tmux_commands(
        &self,
        words: &[&str],
        other_server: bool,
        here: &Path,
        what: &str,
        depth: usize,
        acc: &mut Acc,
    ) {
        if depth > MAX_NESTING {
            acc.deny(DenyReason::Unparsed, what);
            return;
        }
        for command in words.split(|w| *w == ";") {
            // A command given as one word (`tmux 'send-keys -t %9 x'`) is read as its words.
            let owned: Vec<String>;
            let flat: Vec<&str>;
            let command = match command.first() {
                Some(first) if first.contains(char::is_whitespace) => {
                    owned = self.carried(first);
                    flat = owned
                        .iter()
                        .map(String::as_str)
                        .chain(command[1..].iter().copied())
                        .collect();
                    &flat[..]
                }
                _ => command,
            };
            let Some(first) = command.first() else {
                continue;
            };
            let Some(name) = tmux_command(first) else {
                continue;
            };
            let rest = &command[1..];
            let judge = |targets: Vec<&str>, acc: &mut Acc| {
                let own = !other_server
                    && targets
                        .iter()
                        .all(|t| self.layout.own_pane.as_deref() == Some(*t));
                if own {
                    self.when_held(acc, CallClass::Protected(ProtectedKind::OtherSession));
                } else {
                    acc.deny(DenyReason::Configuration, what);
                }
            };
            match name {
                "send-keys" => judge(tmux_targets(rest, "cN"), acc),
                "capture-pane" => judge(tmux_targets(rest, "bES"), acc),
                "pipe-pane" | "send-prefix" => judge(tmux_targets(rest, ""), acc),
                "paste-buffer" => judge(tmux_targets(rest, "bs"), acc),
                // Commands the file holds, one a line; a file the guard cannot read is DENIED.
                "source-file" => {
                    // An empty operand names no file: nothing is sourced.
                    for f in rest.iter().filter(|w| !w.starts_with('-') && !w.is_empty()) {
                        match self.fs.script(&self.physical(f, here)) {
                            Script::Text(text) => {
                                for line in text.lines() {
                                    let line = line.trim();
                                    if line.is_empty() || line.starts_with('#') {
                                        continue;
                                    }
                                    let owned = self.carried(line);
                                    let ws: Vec<&str> = owned.iter().map(String::as_str).collect();
                                    self.tmux_commands(
                                        &ws,
                                        other_server,
                                        here,
                                        what,
                                        depth + 1,
                                        acc,
                                    );
                                }
                            }
                            Script::TooLarge | Script::Unreadable => {
                                acc.deny(DenyReason::Configuration, what);
                            }
                        }
                    }
                }
                // `if-shell [-bF] [-t target] condition command [command]`.
                "if-shell" => {
                    let operands = tmux_operands(rest, "t");
                    if let Some((cond, commands)) = operands.split_first() {
                        if !rest
                            .iter()
                            .any(|w| w.starts_with('-') && w[1..].contains('F'))
                        {
                            self.shell_text(cond, here, depth + 1, acc);
                        }
                        for c in commands {
                            let owned = self.carried(c);
                            let ws: Vec<&str> = owned.iter().map(String::as_str).collect();
                            self.tmux_commands(&ws, other_server, here, what, depth + 1, acc);
                        }
                    }
                }
                // `run-shell [-bC] [-d delay] [-t target] command`: a shell command, or with `-C`
                // tmux ones.
                "run-shell" => {
                    let with_c = rest
                        .iter()
                        .any(|w| w.starts_with('-') && w[1..].contains('C'));
                    for c in tmux_operands(rest, "cdt") {
                        if with_c {
                            let owned = self.carried(c);
                            let ws: Vec<&str> = owned.iter().map(String::as_str).collect();
                            self.tmux_commands(&ws, other_server, here, what, depth + 1, acc);
                        } else {
                            self.shell_text(c, here, depth + 1, acc);
                        }
                    }
                }
                // `bind-key [-nr] [-T table] [-N note] key command …`, `set-hook [-agpRuw] [-t
                // target] hook command`, `confirm-before [-p prompt] [-t target] command`.
                "bind-key" | "set-hook" | "confirm-before" => {
                    let with_value = match name {
                        "bind-key" => "TN",
                        "set-hook" => "t",
                        _ => "cpt",
                    };
                    let operands = tmux_operands(rest, with_value);
                    let skip = usize::from(name != "confirm-before");
                    // No command after the key, the hook or the prompt: nothing is carried.
                    let owned: Vec<String> = operands
                        .iter()
                        .skip(skip)
                        .flat_map(|w| self.carried(w))
                        .collect();
                    let command: Vec<&str> = owned.iter().map(String::as_str).collect();
                    self.tmux_commands(&command, other_server, here, what, depth + 1, acc);
                }
                _ => {}
            }
        }
    }

    /// Whether the words of a screen command type into a terminal: `stuff`, `hardcopy` or `paste`
    /// among them, in an `eval`'s commands, or in the lines of a file it `source`s, which may
    /// `source` another in turn. A file the guard cannot read, or one nested beyond
    /// [`MAX_NESTING`], sets `unreadable`.
    fn screen_words_hit(
        &self,
        words: &[&str],
        here: &Path,
        depth: usize,
        unreadable: &mut bool,
    ) -> bool {
        let mut hit = words
            .iter()
            .any(|a| matches!(*a, "stuff" | "hardcopy" | "paste"));
        // `eval` runs each word after it as a command; `source` runs the commands of a file.
        if let Some(e) = words.iter().position(|a| *a == "eval") {
            for c in &words[e + 1..] {
                let owned = self.carried(c);
                let w: Vec<&str> = owned.iter().map(String::as_str).collect();
                hit |= self.screen_words_hit(&w, here, depth + 1, unreadable);
            }
        }
        // An empty operand names no file: nothing is sourced.
        if let Some(s) = words.iter().position(|a| *a == "source")
            && let Some(f) = words.get(s + 1).filter(|f| !f.is_empty())
        {
            if depth >= MAX_NESTING {
                *unreadable = true;
                return hit;
            }
            match self.fs.script(&self.physical(f, here)) {
                Script::Text(text) => {
                    for l in text.lines() {
                        let l = l.trim();
                        if l.starts_with('#') {
                            continue;
                        }
                        let owned = self.carried(l);
                        let w: Vec<&str> = owned.iter().map(String::as_str).collect();
                        hit |= self.screen_words_hit(&w, here, depth + 1, unreadable);
                    }
                }
                Script::TooLarge | Script::Unreadable => *unreadable = true,
            }
        }
        hit
    }

    /// Rule 4 for screen: `-X stuff` and `-X paste` type into a session and `-X hardcopy` reads one; `-X eval`
    /// and `-X source` carry those commands. The agent's own session (`STY`, named by `-S` or by
    /// none) is PROTECTED while HELD; any other is DENIED, and so is a file of commands the guard
    /// cannot read.
    pub(super) fn screen_keys(&self, args: &[&str], here: &Path, what: &str, acc: &mut Acc) {
        let (x_at, target) = screen_options(args);
        let Some(x) = x_at else {
            return;
        };
        // The command follows -X; options may come after it too (`screen -X -S name stuff x`).
        let after = &args[x + 1..];
        let mut unreadable = false;
        let hit = self.screen_words_hit(after, here, 0, &mut unreadable);
        if !hit && !unreadable {
            return;
        }
        let own = target.is_none_or(|t| {
            self.layout
                .own_screen
                .as_deref()
                .is_some_and(|sty| sty == t || sty.split('.').nth(1) == Some(t))
        });
        // A file the guard cannot read may hold either command: it is DENIED for the agent's own
        // session as for another.
        if own && !unreadable {
            self.when_held(acc, CallClass::Protected(ProtectedKind::OtherSession));
        } else {
            acc.deny(DenyReason::Configuration, what);
        }
    }
}
