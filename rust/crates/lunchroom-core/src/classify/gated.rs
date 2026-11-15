//! GATED calls (spec §6 P1.4): a merge, a deploy, a migration, and a call that runs something the
//! guard cannot read to the end.
//!
//! The text is read here by a reader of its own (not the strict reader of [`crate::shell`], whose
//! job is to refuse what it cannot read): it takes the words of each command after quote removal,
//! drops each redirection's operator and operand, reads a command substitution as the commands it
//! holds, and keeps a word that held an expansion the guard cannot resolve with that expansion
//! removed (`d${E}eploy` is `deploy`) and marked. [`command_kinds`] then applies P1.4's steps to
//! one command: the subcommand tables, the generic `deploy` and `migrate` words, the exemptions.
//! What needs the file system (a compiled program, a script, a Makefile, `package.json`, a
//! justfile) is in [`Guard::gated_text`] and in `recipes`.

use std::collections::HashSet;
use std::path::Path;

use super::{
    Acc, CallClass, Guard, INTERPRETERS, NEVER_RUNS, SHELLS, basename, git_subcommand,
    is_assignment, program_positions,
};
use crate::paths::{absolute, normalize};
use crate::policy::ProtectedKind;

/// The longest nesting of `$( )`, backquotes and handed-on commands the reader follows; one
/// deeper is something the guard cannot read to the end.
pub(super) const MAX_DEPTH: usize = 24;

/// What `$HOME` and a leading `~` stand for in the reader's words: a path either way.
const HOME: &str = "~";

/// One word of a command, after quote removal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct W {
    /// The text, each expansion the guard cannot resolve removed.
    pub t: String,
    /// An expansion the guard cannot resolve was removed from it.
    pub rt: bool,
}

/// The commands of a text, in order, a command substitution's commands among them.
#[derive(Debug, Default)]
pub(super) struct Reading {
    pub cmds: Vec<Vec<W>>,
    /// The byte range of each command in the text read, redirections included.
    pub spans: Vec<(usize, usize)>,
    /// A command substitution, a backquote or a process substitution stands in the text.
    pub subst: bool,
    /// The text has shell syntax this reader does not follow (a group, a function, a control
    /// word, a here-document, brace expansion, `$'...'`): the strict reader's refusal of it stays.
    pub structural: bool,
    /// A quote or a substitution is not closed, or the text nests deeper than [`MAX_DEPTH`].
    pub broken: bool,
    /// Names the text defines as functions.
    pub funcs: Vec<String>,
}

const CONTROL: [&str; 17] = [
    "if", "then", "else", "elif", "fi", "for", "select", "while", "until", "do", "done", "case",
    "esac", "in", "function", "!", "[[",
];

struct Lx {
    s: Vec<char>,
    i: usize,
    out: Reading,
    heredocs: Vec<(String, bool)>,
    /// The byte offset of each character, and of the end.
    offs: Vec<usize>,
    /// Where the command being read begins and ends, as character places.
    cs: Option<usize>,
    ce: usize,
    /// Where the word being read begins.
    wstart: usize,
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\r'
}

fn ends_word(c: char) -> bool {
    is_blank(c) || matches!(c, '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

/// Reads `text`.
pub(super) fn read(text: &str) -> Reading {
    let mut offs: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    offs.push(text.len());
    let mut lx = Lx {
        s: text.chars().collect(),
        i: 0,
        out: Reading::default(),
        heredocs: Vec::new(),
        offs,
        cs: None,
        ce: 0,
        wstart: 0,
    };
    lx.commands(None, 0);
    lx.out
}

impl Lx {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    /// Reads commands until the end of the text, or until the `)` that closes a substitution; the
    /// command being read outside it keeps its place.
    fn commands(&mut self, close: Option<char>, depth: usize) {
        let saved = (self.cs.take(), self.ce);
        self.commands_inner(close, depth);
        (self.cs, self.ce) = saved;
    }

    #[allow(clippy::too_many_lines)] // one arm per kind of character at a command's place
    fn commands_inner(&mut self, close: Option<char>, depth: usize) {
        if depth > MAX_DEPTH {
            self.out.broken = true;
            self.out.structural = true;
            self.i = self.s.len();
            return;
        }
        let mut cur: Vec<W> = Vec::new();
        let mut fn_next = false;
        loop {
            while let Some(c) = self.peek() {
                if is_blank(c) {
                    self.i += 1;
                } else if c == '\\' && self.at(1) == Some('\n') {
                    self.i += 2;
                } else {
                    break;
                }
            }
            let Some(c) = self.peek() else {
                if close.is_some() {
                    self.out.broken = true;
                }
                break;
            };
            match c {
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\n' => {
                    self.i += 1;
                    self.skip_heredoc_bodies();
                    self.end(&mut cur);
                }
                ';' | '&' | '|' => {
                    self.i += 1;
                    if self
                        .peek()
                        .is_some_and(|n| n == c || (c == '|' && n == '&') || (c == ';' && n == '&'))
                    {
                        self.i += 1;
                    }
                    self.end(&mut cur);
                }
                '(' => {
                    // `name()` defines a function; any other `(` opens a group.
                    self.i += 1;
                    self.out.structural = true;
                    if cur.len() == 1 && self.peek() == Some(')') {
                        self.i += 1;
                        let name = cur.pop().unwrap_or_default().t;
                        self.out.funcs.push(name);
                    } else {
                        self.end(&mut cur);
                    }
                }
                ')' => {
                    self.i += 1;
                    self.end(&mut cur);
                    if close == Some(')') {
                        return;
                    }
                    self.out.structural = true;
                }
                '<' | '>' => {
                    self.cs.get_or_insert(self.i);
                    self.redirection(depth);
                    self.ce = self.i;
                }
                _ => {
                    let at_command = cur.iter().all(|w| is_assignment(&w.t));
                    let start = self.i;
                    let (w, plain) = self.word(depth);
                    if self.i == start {
                        self.i += 1;
                        continue;
                    }
                    // A number before `<` or `>` is the redirected descriptor.
                    if plain
                        && !w.t.is_empty()
                        && w.t.chars().all(|c| c.is_ascii_digit())
                        && matches!(self.peek(), Some('<' | '>'))
                    {
                        continue;
                    }
                    if at_command && plain {
                        if w.t == "{" || w.t == "}" || CONTROL.contains(&w.t.as_str()) {
                            self.out.structural = true;
                            fn_next = w.t == "function";
                            continue;
                        }
                        if fn_next {
                            fn_next = false;
                            self.out.funcs.push(w.t);
                            continue;
                        }
                    }
                    cur.push(w);
                    self.cs.get_or_insert(start);
                    self.ce = self.i;
                }
            }
        }
        self.end(&mut cur);
    }

    fn end(&mut self, cur: &mut Vec<W>) {
        if cur.is_empty() {
            self.cs = None;
        } else {
            let from = self.cs.take().unwrap_or(0).min(self.offs.len() - 1);
            let to = self.ce.clamp(from, self.offs.len() - 1);
            self.out.spans.push((self.offs[from], self.offs[to]));
            self.out.cmds.push(std::mem::take(cur));
        }
    }

    /// A redirection: the operator and its operand are not words. A here-document's body is
    /// skipped; `<(` and `>(` are process substitutions.
    fn redirection(&mut self, depth: usize) {
        let c = self.s[self.i];
        if self.at(1) == Some('(') {
            self.i += 2;
            self.out.subst = true;
            self.commands(Some(')'), depth + 1);
            return;
        }
        self.i += 1;
        let mut heredoc = false;
        let mut strip = false;
        match (c, self.peek()) {
            ('<', Some('<')) => {
                self.i += 1;
                if self.peek() == Some('<') {
                    self.i += 1;
                } else {
                    heredoc = true;
                    self.out.structural = true;
                    if self.peek() == Some('-') {
                        self.i += 1;
                        strip = true;
                    }
                }
            }
            (_, Some('>' | '|' | '&')) => self.i += 1,
            _ => {}
        }
        while self.peek().is_some_and(is_blank) {
            self.i += 1;
        }
        if self.peek().is_some_and(|c| !ends_word(c)) {
            let (w, _) = self.word(depth);
            if heredoc {
                self.heredocs.push((w.t, strip));
            }
        }
    }

    fn skip_heredoc_bodies(&mut self) {
        for (delim, strip) in std::mem::take(&mut self.heredocs) {
            while self.i < self.s.len() {
                let end = self.s[self.i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(self.s.len(), |p| self.i + p);
                let line: String = self.s[self.i..end].iter().collect();
                self.i = (end + 1).min(self.s.len());
                let line = if strip {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if line == delim {
                    break;
                }
            }
        }
    }

    /// One word and whether it held no quote, escape or expansion.
    fn word(&mut self, depth: usize) -> (W, bool) {
        let mut w = W::default();
        let mut plain = true;
        let start = self.i;
        self.wstart = start;
        while let Some(c) = self.peek() {
            if ends_word(c) {
                break;
            }
            self.i += 1;
            match c {
                '\\' => {
                    plain = false;
                    match self.peek() {
                        None => break,
                        Some('\n') => self.i += 1,
                        Some(n) => {
                            w.t.push(n);
                            self.i += 1;
                        }
                    }
                }
                '\'' => {
                    plain = false;
                    loop {
                        match self.peek() {
                            None => {
                                self.out.broken = true;
                                break;
                            }
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(n) => {
                                w.t.push(n);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    plain = false;
                    self.double(&mut w, depth);
                }
                '$' => {
                    plain = false;
                    self.dollar(&mut w, depth, false);
                }
                '`' => {
                    plain = false;
                    self.backquote(&mut w, depth);
                }
                '}' if w.t.contains('{') && (w.t.contains(',') || w.t.contains("..")) => {
                    // Brace expansion is not followed.
                    self.out.structural = true;
                    w.t.push(c);
                }
                '~' if self.i - 1 == start => match self.peek() {
                    None | Some('/') => w.t.push_str(HOME),
                    Some(n) if ends_word(n) => w.t.push_str(HOME),
                    Some(_) => {
                        self.out.structural = true;
                        w.t.push('~');
                    }
                },
                _ => w.t.push(c),
            }
        }
        (w, plain)
    }

    fn double(&mut self, w: &mut W, depth: usize) {
        loop {
            let Some(c) = self.peek() else {
                self.out.broken = true;
                return;
            };
            self.i += 1;
            match c {
                '"' => return,
                '\\' => match self.peek() {
                    None => {
                        self.out.broken = true;
                        return;
                    }
                    Some('\n') => self.i += 1,
                    Some(n @ ('$' | '`' | '"' | '\\')) => {
                        w.t.push(n);
                        self.i += 1;
                    }
                    Some(_) => w.t.push('\\'),
                },
                '$' => self.dollar(w, depth, true),
                '`' => self.backquote(w, depth),
                _ => w.t.push(c),
            }
        }
    }

    fn backquote(&mut self, w: &mut W, depth: usize) {
        let mut body = String::new();
        loop {
            match self.peek() {
                None => {
                    self.out.broken = true;
                    break;
                }
                Some('`') => {
                    self.i += 1;
                    break;
                }
                Some('\\') if matches!(self.at(1), Some('`' | '\\' | '$')) => {
                    body.push(self.s[self.i + 1]);
                    self.i += 2;
                }
                Some(c) => {
                    body.push(c);
                    self.i += 1;
                }
            }
        }
        self.out.subst = true;
        w.rt = true;
        if depth + 1 > MAX_DEPTH {
            self.out.broken = true;
            self.out.structural = true;
            return;
        }
        let inner = read(&body);
        self.out.structural |= inner.structural;
        self.out.broken |= inner.broken;
        self.out.funcs.extend(inner.funcs);
        // The backquoted text is shown as written: each of its commands stands on the backquotes.
        let shown = (
            self.offs[self.wstart.min(self.offs.len() - 1)],
            self.offs[self.i.min(self.offs.len() - 1)],
        );
        self.out.spans.extend(inner.cmds.iter().map(|_| shown));
        self.out.cmds.extend(inner.cmds);
    }

    /// After a `$`. `quoted`: inside double quotes.
    fn dollar(&mut self, w: &mut W, depth: usize, quoted: bool) {
        match self.peek() {
            Some('\'') if !quoted => {
                // `$'...'`: ANSI-C quoting, resolved.
                self.i += 1;
                self.out.structural = true;
                self.ansi(w);
            }
            Some('"') if !quoted => {
                self.i += 1;
                self.out.structural = true;
                self.double(w, depth);
            }
            Some('(') if self.at(1) == Some('(') => {
                // Arithmetic: not a command substitution; the text up to `))` is dropped.
                self.i += 2;
                let mut open = 2_usize;
                while let Some(c) = self.peek() {
                    self.i += 1;
                    match c {
                        '(' => open += 1,
                        ')' => {
                            open -= 1;
                            if open == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                self.out.structural = true;
                if open != 0 {
                    self.out.broken = true;
                }
            }
            Some('(') => {
                self.i += 1;
                self.out.subst = true;
                w.rt = true;
                self.commands(Some(')'), depth + 1);
            }
            Some('{') => {
                self.i += 1;
                let mut name = String::new();
                let mut open = 1_usize;
                while let Some(c) = self.peek() {
                    self.i += 1;
                    match c {
                        '{' => open += 1,
                        '}' => {
                            open -= 1;
                            if open == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    name.push(c);
                }
                if open != 0 {
                    self.out.broken = true;
                }
                if name == "HOME" {
                    w.t.push_str(HOME);
                } else {
                    w.rt = true;
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(n) = self
                    .peek()
                    .filter(|n| n.is_ascii_alphanumeric() || *n == '_')
                {
                    name.push(n);
                    self.i += 1;
                }
                if name == "HOME" {
                    w.t.push_str(HOME);
                } else {
                    w.rt = true;
                }
            }
            Some(c)
                if c.is_ascii_digit() || matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') =>
            {
                self.i += 1;
                w.rt = true;
            }
            _ => w.t.push('$'),
        }
    }

    /// The text of `$'...'`, escapes resolved.
    #[allow(clippy::many_single_char_names)] // the letters of the escapes are the letters
    fn ansi(&mut self, w: &mut W) {
        loop {
            let Some(c) = self.peek() else {
                self.out.broken = true;
                return;
            };
            self.i += 1;
            match c {
                '\'' => return,
                '\\' => {
                    let Some(n) = self.peek() else {
                        self.out.broken = true;
                        return;
                    };
                    self.i += 1;
                    match n {
                        'n' => w.t.push('\n'),
                        't' => w.t.push('\t'),
                        'r' => w.t.push('\r'),
                        'a' | 'b' | 'e' | 'E' | 'f' | 'v' => {}
                        'x' | 'u' | 'U' => {
                            let max = match n {
                                'x' => 2,
                                'u' => 4,
                                _ => 8,
                            };
                            let mut v = 0_u32;
                            let mut k = 0;
                            while k < max
                                && let Some(d) = self.peek().and_then(|d| d.to_digit(16))
                            {
                                v = v.saturating_mul(16).saturating_add(d);
                                self.i += 1;
                                k += 1;
                            }
                            if let Some(ch) = char::from_u32(v) {
                                w.t.push(ch);
                            }
                        }
                        '0'..='7' => {
                            let mut v = n.to_digit(8).unwrap_or(0);
                            let mut k = 1;
                            while k < 3
                                && let Some(d) = self.peek().and_then(|d| d.to_digit(8))
                            {
                                v = v * 8 + d;
                                self.i += 1;
                                k += 1;
                            }
                            if let Some(ch) = char::from_u32(v) {
                                w.t.push(ch);
                            }
                        }
                        other => w.t.push(other),
                    }
                }
                _ => w.t.push(c),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The words of one command (P1.4 steps)
// ---------------------------------------------------------------------------------------------

/// What a text is: the kinds of GATED it raised.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent facts
pub struct Gate {
    /// A merge.
    pub merge: bool,
    /// A deploy.
    pub deploy: bool,
    /// A migration.
    pub migrate: bool,
    /// Something the guard cannot read to the end, as the words alone show it: a command
    /// substitution, `eval`, an expansion it cannot resolve used as a program or as a word of a
    /// command of a listed program.
    pub unread: bool,
}

impl Gate {
    /// A merge, a deploy or a migration.
    #[must_use]
    pub const fn words(self) -> bool {
        self.merge || self.deploy || self.migrate
    }

    /// Everything either found.
    #[must_use]
    pub const fn or(self, o: Self) -> Self {
        Self {
            merge: self.merge || o.merge,
            deploy: self.deploy || o.deploy,
            migrate: self.migrate || o.migrate,
            unread: self.unread || o.unread,
        }
    }
}

/// The programs P1.4 reads by subcommand, taken by name (`docker` for its `push`).
const LISTED: [&str; 16] = [
    "git",
    "gh",
    "glab",
    "hub",
    "kubectl",
    "helm",
    "terraform",
    "pulumi",
    "liquibase",
    "goose",
    "dbmate",
    "alembic",
    "prisma",
    "sqlx",
    "diesel",
    "docker",
];

/// Programs that only print or read: not GATED by a word, by name or by a path in a system folder.
const PRINTERS: [&str; 12] = [
    "echo", "printf", "cat", "grep", "rg", "man", "less", "head", "tail", "ls", "which", "type",
];

/// The folders of the system's programs (P1.4).
const SYSTEM_DIRS: [&str; 5] = ["/bin", "/usr/bin", "/usr/local/bin", "/sbin", "/usr/sbin"];

/// The folders in which a printing program is exempt by a path (P1.4).
const EXEMPT_DIRS: [&str; 3] = ["/bin", "/usr/bin", "/usr/local/bin"];

fn in_dirs(path: &str, dirs: &[&str]) -> bool {
    Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .is_some_and(|d| dirs.contains(&d))
}

/// A word that is a path: it holds `/` or begins with `.` or `~`.
fn is_path(t: &str) -> bool {
    t.contains('/') || t.starts_with(['.', '~'])
}

/// P1.4: `migrate`, `db:migrate`, a word beginning `migrate:` or `migration:`, or ending `.migrate`.
fn migrate_word(t: &str) -> bool {
    t == "migrate"
        || t == "db:migrate"
        || t.starts_with("migrate:")
        || t.starts_with("migration:")
        || t.ends_with(".migrate")
}

/// The listed programs the word at a command's place names: a dashed `git-merge` is `git` (the
/// flag says so), `hub` is read as `git` and as `gh`.
fn names(word: &str) -> Vec<(&'static str, bool)> {
    let b = basename(word);
    if b.strip_prefix("git-").is_some_and(|r| !r.is_empty()) {
        return vec![("git", true)];
    }
    match b {
        "hub" => vec![("git", false), ("gh", false)],
        _ => LISTED
            .iter()
            .find(|l| **l == b)
            .map(|l| vec![(*l, false)])
            .unwrap_or_default(),
    }
}

const KUBECTL: [&str; 8] = [
    "apply", "create", "replace", "patch", "delete", "scale", "set", "rollout",
];
const HELM: [&str; 4] = ["install", "upgrade", "rollback", "uninstall"];

/// Each index of `b` in `ws` that has `a` before it.
fn in_order(ws: &[&str], a: &str, b: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, w) in ws.iter().enumerate() {
        if *w == a
            && let Some(k) = ws[i + 1..].iter().position(|x| *x == b)
        {
            out.push(i + 1 + k);
        }
    }
    out
}

/// The words of a `git push` as P1.4 compares them: each word, an option with an attached value
/// as the option, a cluster of short options as each letter, a refspec as its destination.
fn push_words(ws: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for w in ws {
        out.push((*w).to_owned());
        if let Some(long) = w.strip_prefix("--") {
            if let Some((name, _)) = long.split_once('=') {
                out.push(format!("--{name}"));
            }
        } else if let Some(short) = w.strip_prefix('-') {
            for c in short.chars() {
                out.push(format!("-{c}"));
            }
        } else {
            let dest = w.strip_prefix('+').unwrap_or(w);
            let dest = dest.rsplit(':').next().unwrap_or(dest);
            let dest = dest.strip_prefix("refs/heads/").unwrap_or(dest);
            out.push(dest.to_owned());
        }
    }
    out
}

fn deploy_branch(w: &str) -> bool {
    matches!(w, "main" | "master" | "production" | "prod") || w.starts_with("release")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Merge,
    Deploy,
    Migrate,
}

/// The kinds the words of one command raise (P1.4), with the index of the word each stands on,
/// so the help exemption can see what follows it. `ws[0]` is the program.
#[allow(clippy::too_many_lines)] // one arm per program, as the clause lists them
fn triggers(ws: &[&str]) -> Vec<(Kind, usize)> {
    let mut out: Vec<(Kind, usize)> = Vec::new();
    let Some(&first) = ws.first() else {
        return out;
    };
    for (prog, dashed) in names(first) {
        // A dashed `git-merge x` is `git merge x`: its own name is the subcommand.
        let own = [basename(first).strip_prefix("git-").unwrap_or("")];
        let rest: Vec<&str> = if dashed {
            own.iter().copied().chain(ws[1..].iter().copied()).collect()
        } else {
            ws[1..].to_vec()
        };
        // The index in `ws` of `rest[k]`.
        let at = |k: usize| if dashed { k } else { k + 1 };
        for (k, w) in rest.iter().enumerate() {
            let only = |set: &[&str]| rest.len() == k + 2 && set.contains(&rest[k + 1]);
            match (prog, *w) {
                ("git", "merge" | "cherry-pick" | "am") if !only(&["--abort", "--quit"]) => {
                    out.push((Kind::Merge, at(k)));
                }
                ("git", "rebase") if !only(&["--abort", "--quit", "--skip"]) => {
                    out.push((Kind::Merge, at(k)));
                }
                ("git", "pull") => out.push((Kind::Merge, at(k))),
                ("git", "push") => {
                    if push_words(&rest).iter().any(|x| {
                        deploy_branch(x)
                            || matches!(x.as_str(), "--force" | "-f" | "--force-with-lease")
                    }) {
                        out.push((Kind::Deploy, at(k)));
                    }
                }
                ("terraform", "apply" | "destroy") | ("pulumi", "up" | "destroy") => {
                    out.push((Kind::Deploy, at(k)));
                }
                ("kubectl", x) if KUBECTL.contains(&x) => out.push((Kind::Deploy, at(k))),
                ("helm", x) if HELM.contains(&x) => out.push((Kind::Deploy, at(k))),
                ("docker", "push") => out.push((Kind::Deploy, at(k))),
                ("alembic", "upgrade")
                | ("prisma", "migrate")
                | ("liquibase", "update")
                | ("goose" | "dbmate", "up") => out.push((Kind::Migrate, at(k))),
                _ => {}
            }
        }
        let pairs: &[(&str, &str, &str, Kind)] = &[
            ("gh", "pr", "merge", Kind::Merge),
            ("glab", "mr", "merge", Kind::Merge),
            ("sqlx", "migrate", "run", Kind::Migrate),
            ("diesel", "migration", "run", Kind::Migrate),
        ];
        for (p, a, b, kind) in pairs {
            if prog == *p {
                for i in in_order(&rest, a, b) {
                    out.push((*kind, at(i)));
                }
            }
        }
    }
    if basename(first) == "ansible-playbook" {
        out.push((Kind::Deploy, 0));
    }
    // For any program: `deploy`, and a migration word, after quote removal and not a path.
    for (i, w) in ws.iter().enumerate() {
        if is_path(w) {
            continue;
        }
        if *w == "deploy" {
            out.push((Kind::Deploy, i));
        } else if migrate_word(w) {
            out.push((Kind::Migrate, i));
        }
    }
    out
}

/// The words before the first bare `--` of the command: it belongs to the command's own program, or
/// to a wrapper (`sudo`, `env`, `timeout`, `nohup`, `xargs`, `kubectl exec`), in which case the
/// words after it are the wrapped command, read as a command of its own.
fn own_words<'a>(ws: &'a [&'a str]) -> &'a [&'a str] {
    ws.iter().position(|w| *w == "--").map_or(ws, |k| &ws[..k])
}

/// The kinds the words of one command raise, exemptions applied; `ws[0]` is its program.
fn command_kinds(ws: &[&str]) -> Vec<Kind> {
    let ws = own_words(ws);
    let Some(&first) = ws.first() else {
        return Vec::new();
    };
    // A program that only prints or reads, by name or by a path in a system folder.
    if PRINTERS.contains(&basename(first)) && (!first.contains('/') || in_dirs(first, &EXEMPT_DIRS))
    {
        return Vec::new();
    }
    // `--version` is the only word after the program's name.
    if ws.len() == 2 && ws[1] == "--version" {
        return Vec::new();
    }
    // Each kind holds unless the word after what raised it (its subcommand, or the program's own
    // name where it has none) is `--help` or `-h`; `--help` elsewhere is a word like any other.
    let mut kinds: Vec<Kind> = Vec::new();
    for (kind, i) in triggers(ws) {
        if matches!(ws.get(i + 1), Some(&("--help" | "-h"))) {
            continue;
        }
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The first non-option word at or after `from`: the subcommand of `npm`, `cargo`, `docker`.
pub(super) fn subcommand<'a>(ws: &[&'a str], from: usize) -> Option<(usize, &'a str)> {
    // Options that take a separate value.
    const VALUED: [&str; 12] = [
        "--prefix",
        "-C",
        "--cwd",
        "--dir",
        "-w",
        "--workspace",
        "--filter",
        "-F",
        "--userconfig",
        "--registry",
        "--cache",
        "-f",
    ];
    let mut i = from;
    while i < ws.len() {
        let w = ws[i];
        if w == "--" {
            return None;
        }
        if w.starts_with('-') {
            i += if VALUED.contains(&w) { 2 } else { 1 };
            continue;
        }
        return Some((i, w));
    }
    None
}

/// The kinds one command raises through every program it runs: its own program, the programs its
/// wrappers run (`sudo git merge`), and the commands handed on (`ssh host 'git merge'`).
fn command_gate(words: &[W], depth: usize) -> Gate {
    let mut g = Gate::default();
    if depth > MAX_DEPTH {
        g.unread = true;
        return g;
    }
    let texts: Vec<&str> = words.iter().map(|w| w.t.as_str()).collect();
    let positions = program_positions(&texts);
    let mut starts: Vec<usize> = positions.clone();
    // The command a runner hands on is a command of its own.
    for &p in &positions {
        if let Some(q) = handed_start(&texts, p) {
            starts.push(q);
        }
        // A wrapper's own `--` (`timeout 5 -- git merge x`) ends its options; the command follows.
        if texts[p] == "--" && p + 1 < texts.len() {
            starts.push(p + 1);
        }
    }
    starts.sort_unstable();
    starts.dedup();
    for &p in &starts {
        for kind in command_kinds(&texts[p..]) {
            match kind {
                Kind::Merge => g.merge = true,
                Kind::Deploy => g.deploy = true,
                Kind::Migrate => g.migrate = true,
            }
        }
        let name = basename(texts[p]);
        // `eval`, a program built at run time, and an expansion in a listed program's words.
        let listed = !names(texts[p]).is_empty();
        if words[p].rt || name == "eval" || (listed && words[p..].iter().any(|w| w.rt)) {
            g.unread = true;
        }
        // A handed-on word that holds a space is shell text.
        if let Some(q) = handed_start(&texts, p)
            && words[q..].len() == 1
            && words[q].t.contains(char::is_whitespace)
        {
            g = g.or(scan_reading(&read(&words[q].t), depth + 1));
        }
        // `sh -c 'text'`, `su -c`, `env -S`: the text is a command of its own.
        if SHELLS.contains(&name) || matches!(name, "su" | "runuser") {
            if let Some(text) = shell_text_arg(&texts[p + 1..]) {
                g = g.or(scan_reading(&read(text), depth + 1));
            }
        } else if name == "env"
            && let Some(k) = texts[p + 1..]
                .iter()
                .position(|t| *t == "-S" || *t == "--split-string")
            && let Some(text) = texts.get(p + k + 2)
        {
            g = g.or(scan_reading(&read(text), depth + 1));
        }
    }
    g
}

/// The text a shell is given with `-c` (a cluster of options ending in `c` counts).
fn shell_text_arg<'a>(args: &[&'a str]) -> Option<&'a str> {
    for (i, a) in args.iter().enumerate() {
        if *a == "--command" || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('c'))
        {
            return args.get(i + 1).copied();
        }
        if !a.starts_with('-') {
            return None;
        }
    }
    None
}

/// `ssh` options that take a separate value.
const SSH_VALUED: [&str; 18] = [
    "-p", "-i", "-o", "-l", "-F", "-J", "-L", "-R", "-D", "-b", "-c", "-e", "-m", "-O", "-Q", "-S",
    "-W", "-w",
];

/// `docker exec` and `docker run` options that take a separate value.
const DOCKER_VALUED: [&str; 16] = [
    "-e",
    "-w",
    "-u",
    "-v",
    "-p",
    "--name",
    "--env",
    "--workdir",
    "--user",
    "--volume",
    "--entrypoint",
    "--network",
    "-l",
    "--label",
    "--mount",
    "-h",
];

/// Where the command a runner hands on begins, for the runners P1.4 lists after `ssh`,
/// `docker exec`, `docker run`, `kubectl exec`, `watch` and `at`; `None` for any other word.
fn handed_start(t: &[&str], p: usize) -> Option<usize> {
    let name = basename(t[p]);
    let after = &t[p + 1..];
    let skip = |valued: &[&str], from: usize| {
        let mut i = from;
        while i < after.len() && after[i].starts_with('-') {
            i += if valued.contains(&after[i]) { 2 } else { 1 };
        }
        i
    };
    let rel = match name {
        // The host is one word; the command follows it.
        "ssh" => skip(&SSH_VALUED, 0) + 1,
        "docker" | "podman" => {
            let (k, sub) = subcommand(after, 0)?;
            if !matches!(sub, "exec" | "run") {
                return None;
            }
            // The container or image is one word; the command follows it.
            skip(&DOCKER_VALUED, k + 1) + 1
        }
        "kubectl" => {
            let (_, sub) = subcommand(after, 0)?;
            if sub != "exec" {
                return None;
            }
            after.iter().position(|w| *w == "--")? + 1
        }
        "watch" => skip(&["-n", "--interval"], 0),
        "at" => skip(&["-q", "-f"], 0),
        _ => return None,
    };
    let q = p + 1 + rel;
    (q < t.len()).then_some(q)
}

/// The kinds the commands of a reading raise, by their words (no file is read).
pub(super) fn scan_reading(rd: &Reading, depth: usize) -> Gate {
    let mut g = Gate {
        unread: rd.subst || rd.broken,
        ..Gate::default()
    };
    for c in &rd.cmds {
        g = g.or(command_gate(c, depth));
        // An alias defined in the text is the command it names.
        if c.first().is_some_and(|w| w.t == "alias") {
            for a in &c[1..] {
                if let Some((_, v)) = a.t.split_once('=') {
                    g = g.or(scan_reading(&read(v), depth + 1));
                }
            }
        }
    }
    g
}

/// The kinds a text raises by its words alone.
#[cfg(test)]
pub(super) fn scan_text(text: &str) -> Gate {
    scan_reading(&read(text), 0)
}

/// The verb of a shell command for the Granted line (D1.8): of the kinds the whole call is, the
/// first that applies in the order `push`, `deploy`, `merge`, `migrate` (spec D1.8, "Granted.
/// <agent> can <action>."; a `git pull && terraform apply` is `deploy`, a `git push origin main` is
/// `push`, though it is also a deploy). The same reading as the classifier's, so the line names
/// what the guard gated.
#[must_use]
pub fn action_verb(text: &str) -> Option<&'static str> {
    let rd = read(text);
    let (mut push, mut deploy, mut merge, mut migrate) = (false, false, false, false);
    for c in &rd.cmds {
        let g = command_gate(c, 0);
        merge |= g.merge;
        deploy |= g.deploy;
        migrate |= g.migrate;
        let texts: Vec<&str> = c.iter().map(|w| w.t.as_str()).collect();
        for p in program_positions(&texts) {
            let ws = own_words(&texts[p..]);
            let pushes = names(ws[0]).iter().any(|(n, _)| *n == "git")
                && if basename(ws[0]) == "git-push" {
                    true
                } else {
                    git_subcommand(&ws[1..]) == Some("push")
                };
            if pushes && !matches!(ws.get(1), Some(&("--help" | "-h"))) {
                push = true;
            }
        }
    }
    if push {
        Some("push")
    } else if deploy {
        Some("deploy")
    } else if merge {
        Some("merge")
    } else if migrate {
        Some("migrate")
    } else {
        None
    }
}

/// The kinds the string literals of code in another language raise: the literals of one list or
/// call that starts a program are the words of one command, and a literal that holds a space is
/// shell text (P1.4).
pub(super) fn scan_code(text: &str) -> Gate {
    let mut g = Gate::default();
    let mut groups: Vec<Vec<W>> = Vec::new();
    let mut between = String::new();
    let mut lits: Vec<String> = Vec::new();
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    let flush = |groups: &mut Vec<Vec<W>>, lits: &mut Vec<String>| {
        if !lits.is_empty() {
            groups.push(
                std::mem::take(lits)
                    .into_iter()
                    .map(|t| W { t, rt: false })
                    .collect(),
            );
        }
    };
    while i < cs.len() {
        let c = cs[i];
        if c == '"' || c == '\'' {
            let mut lit = String::new();
            i += 1;
            while i < cs.len() && cs[i] != c {
                if cs[i] == '\\' && i + 1 < cs.len() {
                    i += 1;
                }
                lit.push(cs[i]);
                i += 1;
            }
            i += 1;
            // The text between two literals: only separators of one list or call joins them.
            let joined = !lits.is_empty()
                && between
                    .replace("args", "")
                    .replace("arg", "")
                    .replace("new", "")
                    .chars()
                    .all(|b| {
                        b.is_whitespace() || matches!(b, ',' | '(' | ')' | '[' | ']' | '.' | ':')
                    });
            if !joined {
                flush(&mut groups, &mut lits);
            }
            between.clear();
            if lit.contains(char::is_whitespace) {
                g = g.or(scan_reading(&read(&lit), 1));
            }
            lits.push(lit);
        } else {
            between.push(c);
            i += 1;
        }
    }
    flush(&mut groups, &mut lits);
    for group in &groups {
        g = g.or(command_gate(group, 1));
    }
    // A program started from a value built at run time is not read here.
    Gate { unread: false, ..g }
}

/// Programs the guard classifies by name (P1.4: "the list of known programs"): the shells,
/// interpreters, wrappers and runners it reads through (F1.2), the programs that run none of their
/// arguments as commands, and the programs P1.4 and L1.1 name. Published with each release.
pub const KNOWN_PROGRAMS: &[&str] = &[
    // The shells.
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "ash",
    "fish",
    // The interpreters.
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
    // The wrappers and runners it reads through.
    "sudo",
    "doas",
    "run0",
    "pkexec",
    "su",
    "runuser",
    "env",
    "nohup",
    "timeout",
    "xargs",
    "nice",
    "ionice",
    "setsid",
    "stdbuf",
    "chroot",
    "setpriv",
    "busybox",
    "time",
    "command",
    "builtin",
    "exec",
    "watch",
    "flock",
    "strace",
    "ltrace",
    "proxychains",
    "proxychains4",
    "chronic",
    "unbuffer",
    "caffeinate",
    "taskset",
    "valgrind",
    "torsocks",
    "firejail",
    "unshare",
    "dbus-run-session",
    "script",
    "at",
    "ssh",
    "find",
    "tmux",
    "screen",
    "parallel",
    // The programs P1.4 and L1.1 name.
    "git",
    "gh",
    "glab",
    "hub",
    "kubectl",
    "helm",
    "terraform",
    "pulumi",
    "liquibase",
    "goose",
    "dbmate",
    "alembic",
    "prisma",
    "sqlx",
    "diesel",
    "docker",
    "podman",
    "ansible-playbook",
    "make",
    "just",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "bunx",
    "cargo",
    "go",
    "lunchroom",
    "codex",
    "claude",
    "opencode",
    "openclaw",
    "hermes",
    // Builtins and the programs that run none of their arguments.
    "cd",
    "pushd",
    "popd",
    "pwd",
    "export",
    "unset",
    "set",
    "declare",
    "typeset",
    "local",
    "readonly",
    "read",
    "alias",
    "unalias",
    "source",
    ".",
    ":",
    "[",
    "]",
    "true",
    "false",
    "exit",
    "return",
    "shift",
    "wait",
    "trap",
    "ulimit",
    "umask",
    "jobs",
    "fg",
    "bg",
    "hash",
    "let",
    "mapfile",
    "readarray",
    "shopt",
    "getopts",
    "eval",
    "kill",
    "pkill",
    "killall",
    "printenv",
    "gio",
    "zgrep",
    "ag",
    "ack",
    "fd",
    "fdfind",
    "tree",
    "rmdir",
    "unlink",
    "shred",
    "srm",
    "trash",
    "trash-put",
    "chgrp",
    "chattr",
    "setfacl",
    "rsync",
    "scp",
    "sftp",
    "zip",
    "unzip",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "telnet",
    "ftp",
    "lftp",
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
    "gem",
    "twine",
    "poetry",
    "flit",
    "hatch",
    "vsce",
    "ovsx",
    "dotnet",
    "nuget",
    "pip",
    "pip3",
    "pytest",
    "sleep",
    "mktemp",
];

/// A bare name (or a path's last part) is on the list of known programs.
pub(super) fn known(name: &str) -> bool {
    KNOWN_PROGRAMS.contains(&name)
        || NEVER_RUNS.contains(&name)
        || SHELLS.contains(&name)
        || INTERPRETERS.contains(&name)
}

/// The whole table, sorted, each name once: what Lunchroom publishes with each release.
#[must_use]
pub fn known_programs() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = KNOWN_PROGRAMS
        .iter()
        .chain(NEVER_RUNS.iter())
        .chain(SHELLS.iter())
        .chain(INTERPRETERS.iter())
        .copied()
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

// ---------------------------------------------------------------------------------------------
// The file-system part
// ---------------------------------------------------------------------------------------------

/// What the guard has seen while reading recipes, so a cycle ends and the work is bounded.
#[derive(Default)]
pub(super) struct Seen {
    pub keys: HashSet<String>,
    pub files: usize,
}

impl Guard<'_> {
    /// P1.4: the GATED kinds a text raises, and, while HELD, whether it runs something the guard
    /// cannot read to the end. `here` is the folder the text runs in.
    pub(super) fn gated_text(&self, text: &str, here: &Path, depth: usize, acc: &mut Acc) {
        // P1.1: a call inside the slip (a reply, a read of Lunchroom's own message) is never made
        // GATED by a word in it.
        if depth == 0 && (acc.reply || self.is_read(text, here)) {
            return;
        }
        let mut seen = Seen::default();
        let g = self.gated_judge(text, here, 0, &mut seen);
        if g.words() || (self.held && g.unread) {
            Self::gate(acc);
        }
    }

    /// P1.4: code in another language the call runs (an interpreter's string, a script's text).
    pub(super) fn gated_code(text: &str, acc: &mut Acc) {
        if scan_code(text).words() {
            Self::gate(acc);
        }
    }

    /// Raises GATED: a call that is GATED is never inside a slip (P1.4), and the kind stays with
    /// the call's kinds for the policy and the console.
    pub(super) fn gate(acc: &mut Acc) {
        acc.raise(CallClass::Ordinary);
        acc.kinds = acc.kinds.with(ProtectedKind::Gated);
    }

    /// The kinds a text raises: its words, then what the file system shows about each program.
    pub(super) fn gated_judge(
        &self,
        text: &str,
        here: &Path,
        depth: usize,
        seen: &mut Seen,
    ) -> Gate {
        let literal = super::literals::substitute(text);
        let text = literal.as_deref().unwrap_or(text);
        let rd = read(text);
        let mut g = scan_reading(&rd, depth);
        if depth > MAX_DEPTH {
            g.unread = true;
            return g;
        }
        // Names the text defines (functions, aliases) are read from the text.
        let mut defined = rd.funcs.clone();
        for c in &rd.cmds {
            if c.first().is_some_and(|w| w.t == "alias") {
                defined.extend(
                    c[1..]
                        .iter()
                        .filter_map(|a| a.t.split_once('=').map(|(n, _)| n.to_owned())),
                );
            }
        }
        let mut here = here.to_path_buf();
        for c in &rd.cmds {
            let t: Vec<&str> = c.iter().map(|w| w.t.as_str()).collect();
            let positions = program_positions(&t);
            for &p in &positions {
                if self.cannot_read_program(&t, p, &here, &defined, depth, seen, &mut g) {
                    g.unread = true;
                }
            }
            if let Some(&p) = positions.first()
                && matches!(t[p], "cd" | "pushd")
                && let Some(d) = t.get(p + 1).filter(|d| !d.starts_with('-'))
            {
                here = normalize(d, &here, &self.layout.home);
            }
        }
        g
    }

    /// The program at `p` runs something the guard cannot read to the end: a compiled program not
    /// on the list (or one of a listed name outside a system folder), a name it cannot find, a
    /// program that runs code it fetches or builds, a recipe, script or target it cannot read.
    /// What a recipe, script or target it reads holds is added to `g`.
    #[allow(clippy::too_many_arguments)] // the state one command is judged with
    fn cannot_read_program(
        &self,
        t: &[&str],
        p: usize,
        here: &Path,
        defined: &[String],
        depth: usize,
        seen: &mut Seen,
        g: &mut Gate,
    ) -> bool {
        let word = t[p];
        let name = basename(word);
        if word.is_empty() || defined.iter().any(|f| f == word) {
            return false;
        }
        // A program that runs code it fetches or builds.
        if super::recipes::fetches(t, p) {
            return true;
        }
        // `make`, `npm run`, `just`: the text they run is read.
        match self.recipe(t, p, here, depth, seen) {
            super::recipes::Recipe::Not => {}
            super::recipes::Recipe::Unread => return true,
            super::recipes::Recipe::Read(gate) => {
                *g = g.or(gate);
                return gate.unread;
            }
        }
        // §17 item 5: gdb in the form whose every command the guard reads is known, as strace
        // is (super::gdb_starts_its_own); any other gdb is not on the list.
        let listed = known(name) || (name == "gdb" && super::gdb_starts_its_own(t, p).is_some());
        if word.contains('/') {
            let physical = self.physical(word, here);
            // F1.2 DENIES a program by a path that does not exist.
            if !self.fs.exists(&physical) || self.fs.runs_as_script(&physical) {
                return false;
            }
            // A compiled program: known only by a listed name in a system folder.
            let shown = absolute(word, here, &self.layout.home);
            // Lunchroom's own program is judged by its own rules (A1.3), wherever it is installed.
            return !(listed
                && (name == "lunchroom" || in_dirs(&normalize_lexical(&shown), &SYSTEM_DIRS)));
        }
        if listed {
            // A program of a listed name that PATH finds outside the system's folders is not it.
            return self.fs.which(word).is_some_and(|f| {
                let s = f.to_string_lossy();
                !self.fs.runs_as_script(&f)
                    && !in_dirs(&s, &SYSTEM_DIRS)
                    && ![
                        "/usr/",
                        "/opt/",
                        "/nix/",
                        "/snap/",
                        "/home/linuxbrew/",
                        "/lib",
                    ]
                    .iter()
                    .any(|r| s.starts_with(r))
            });
        }
        match self.fs.which(word) {
            Some(f) => !self.fs.runs_as_script(&f),
            None => true,
        }
    }
}

fn normalize_lexical(p: &Path) -> String {
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out.to_string_lossy().into_owned()
}

/// P1.4: the commands of `text` when the strict reader's refusal of it is only for what the text
/// builds at run time (a command substitution, an expansion): the text is then read, not
/// unparsable. `None` when the text has syntax this reader does not follow either.
pub(super) fn run_time_commands(text: &str) -> Option<Vec<Vec<String>>> {
    let rd = read(text);
    // What P1.4 lists as built at run time: a command substitution, `eval`, an expansion used as
    // the program or as a word of a command of a listed program. An expansion in any other word
    // (`cat $P`) is text the strict reader refuses and the guard does not read further.
    (!rd.structural && !rd.broken && scan_reading(&rd, 0).unread).then(|| {
        rd.cmds
            .into_iter()
            .map(|c| c.into_iter().map(|w| w.t).collect())
            .collect()
    })
}
