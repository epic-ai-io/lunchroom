//! What a command writes, read from its words, and the files one call writes and then runs.
//!
//! `§6::55`, `§6::56` (P1.4): "A call that writes a file and then runs it (a heredoc,
//! `printf` or `echo` redirected into a file, then that file run, in one call) is judged on the text
//! it writes; where that text cannot be read from the call, the call is PROTECTED while the
//! session is HELD." [`scan`] finds the writes of a call's text; the classifier judges the text of
//! each one that a later command of the call runs.
//!
//! `§5::44` (T1.8, with U5): [`Access`] is what a call reads, searches and writes, as
//! the guard judged it before the call ran.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::shell::{self, Parsed, Word};

/// The paths one call reads, searches and writes, as the guard judged the call before it ran
/// (T1.8: the read side holds a session by the ids recorded for a path it reads or a folder it
/// searches; the write side records the paths a call writes).
///
/// Each set holds every spelling the guard resolved a word to: the file the operating system opens
/// (links followed), the path as written made absolute and lexically normal, and its real path, so a
/// recorded path matches whichever of them it was recorded under. A word that is not a path
/// (an option, a number, a pattern) may appear in `reads` as the path it would be in the working
/// folder; the sets over-approximate, and never leave out a path a rule judged.
///
/// * `reads`: every path-like word of a command that the command does not write, the file a
///   `Read`-like tool opens, a script a shell or interpreter runs, and the target of `<`.
///   The words of `echo`/`printf`, a search pattern and a commit message are text, not paths.
/// * `searches`: the folders a search enters (`Grep`, `Glob`, `rg`, `grep -r`, `find`, `ls -R`,
///   `du`, `tar` and the other programs that read everything below a folder), each resolved folder
///   once; a search given no folder searches the working folder.
/// * `writes`: the paths a call writes: the file tools' paths, the target of `>`, `>>` and a
///   heredoc, `tee`, the destination of `cp`, `mv`, `install`, `ln`, `rsync` and `scp`, the files
///   `touch`, `mkdir`, `rm`, `chmod`, `chown` and the like, the files `sed -i` and `perl -i` edit, `dd of=`,
///   and the file `curl -o` or `wget -O` saves. A path a program writes by a means the words do not
///   show (code it runs) is not in the set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    /// Paths the call reads.
    pub reads: BTreeSet<PathBuf>,
    /// Folders the call searches.
    pub searches: BTreeSet<PathBuf>,
    /// Paths the call writes.
    pub writes: BTreeSet<PathBuf>,
}

impl Access {
    /// Adds everything in `other`.
    pub fn extend(&mut self, other: Self) {
        self.reads.extend(other.reads);
        self.searches.extend(other.searches);
        self.writes.extend(other.writes);
    }
}

/// A file one call writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The file, as the call spells it.
    pub target: String,
    /// The text written, where the call shows it whole; `None` where it does not (a variable, a
    /// substitution, another program's output, an input the call does not hold).
    pub body: Option<String>,
    /// The call appends (`>>`, `tee -a`): the file's text before is kept.
    pub append: bool,
}

/// Programs that write the text of their words to standard output.
const ECHOES: [&str; 2] = ["echo", "printf"];

/// The text each heredoc of `text` holds, in the order the heredocs open: `None` for one the shell
/// expands before it is written (an unquoted body with `$` or a backquote).
#[must_use]
pub fn heredoc_bodies(text: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        for (word, quoted, tabs) in heredocs(line) {
            let mut body = String::new();
            while i < lines.len() {
                let l = lines[i];
                i += 1;
                let closing = if tabs { l.trim_start_matches('\t') } else { l };
                if closing == word {
                    break;
                }
                body.push_str(l);
                body.push('\n');
            }
            // An unquoted heredoc is expanded by the shell before it is written.
            let readable = quoted || !body.contains(['$', '`']);
            out.push(readable.then_some(body));
        }
    }
    out
}

/// The files each piece of shell text writes, piece by piece in order (a piece is a part of the
/// text the shell runs as a command of its own, as the classifier splits it). `bodies` are the
/// heredoc bodies of the whole text ([`heredoc_bodies`]), handed to the pieces that open them.
#[must_use]
pub fn scan_pieces(pieces: &[&str], bodies: Vec<Option<String>>) -> Vec<Vec<Written>> {
    let mut bodies = bodies.into_iter();
    pieces
        .iter()
        .map(|piece| {
            let opened = heredocs(piece).len();
            let mine: Vec<Option<String>> = bodies.by_ref().take(opened).collect();
            let targets = destinations(piece);
            if targets.is_empty() {
                return Vec::new();
            }
            let body = match mine.first() {
                Some(b) => b.clone(),
                None => echoed(piece),
            };
            let several = targets.len() > 1;
            targets
                .into_iter()
                .map(|(target, append)| Written {
                    target,
                    // One piece with several targets writes each the same text only for `tee`;
                    // for anything else the text is not known to be the same.
                    body: if several && !is_tee(piece) {
                        None
                    } else {
                        body.clone()
                    },
                    append,
                })
                .collect()
        })
        .collect()
}

/// The files one command the reader could read writes: the targets of its redirections, and what
/// the programs that put content in a file write ([`CONTENT_WRITERS`]). The text is known for
/// `echo` and `printf` redirected into one file.
#[must_use]
pub fn command_writes(words: &[Word]) -> Vec<Written> {
    let mut targets: Vec<(String, bool)> = words
        .iter()
        .filter(|w| w.redirect && w.write)
        .map(|w| (w.text.clone(), w.append))
        .collect();
    let texts: Vec<&str> = words
        .iter()
        .filter(|w| !w.redirect)
        .map(|w| w.text.as_str())
        .collect();
    let program = texts
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or_default();
    if CONTENT_WRITERS.contains(&program) {
        let append = program == "tee" && texts.iter().any(|a| matches!(*a, "-a" | "--append"));
        for (k, _) in operand_writes(program, &texts[1..]) {
            let t = texts[1 + k];
            targets.push((t.strip_prefix("of=").unwrap_or(t).to_owned(), append));
        }
    }
    let body = if targets.len() == 1 {
        echoed_words(words)
    } else {
        None
    };
    targets
        .into_iter()
        .map(|(target, append)| Written {
            target,
            body: body.clone(),
            append,
        })
        .collect()
}

fn is_tee(piece: &str) -> bool {
    program_of(piece).as_deref() == Some("tee")
}

fn program_of(piece: &str) -> Option<String> {
    let Parsed::Commands(cmds) = shell::parse(piece, "/") else {
        return piece
            .split_whitespace()
            .find(|w| !w.contains('='))
            .map(|w| w.rsplit('/').next().unwrap_or(w).to_owned());
    };
    cmds.first()
        .and_then(|c| c.words.iter().find(|w| !w.redirect))
        .map(|w| w.text.rsplit('/').next().unwrap_or(&w.text).to_owned())
}

/// The heredocs a line opens: the closing word, whether the word was quoted (no expansion), and
/// whether it is `<<-` (leading tabs stripped).
fn heredocs(line: &str) -> Vec<(String, bool, bool)> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(k) = rest.find("<<") {
        rest = &rest[k + 2..];
        if rest.starts_with('<') {
            rest = &rest[1..];
            continue;
        }
        let tabs = rest.starts_with('-');
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
            out.push((word.clone(), word.len() != raw.len(), tabs));
        }
    }
    out
}

/// The files one piece of shell text writes: the targets of `>`, `>>`, `>|`, `&>` and `<>`, the
/// operands of `tee`, the destination of `cp`, `mv`, `install` and `ln`, `dd`'s `of=`, and what
/// `curl -o` and `wget -O` save.
fn destinations(piece: &str) -> Vec<(String, bool)> {
    let mut out = redirect_targets(piece);
    // The reader is only needed where a program that puts content in a file may be named.
    if out.is_empty()
        && !piece.split_whitespace().any(|w| {
            CONTENT_WRITERS.contains(&w.rsplit('/').next().unwrap_or(w)) || w.starts_with("of=")
        })
    {
        return out;
    }
    let words = shell::parse(piece, "/");
    let Parsed::Commands(cmds) = words else {
        // Text the reader cannot read: `tee` and `dd` are found by their words.
        let toks: Vec<&str> = piece.split_whitespace().collect();
        if let Some(k) = toks.iter().position(|t| *t == "tee") {
            let append = toks[k + 1..]
                .iter()
                .any(|t| matches!(*t, "-a" | "--append"));
            out.extend(
                toks[k + 1..]
                    .iter()
                    .filter(|t| !t.starts_with(['-', '<', '>', '&', '|']))
                    .map(|t| (unquote(t), append)),
            );
        }
        return dedup(out);
    };
    for c in &cmds {
        let texts: Vec<&str> = c
            .words
            .iter()
            .filter(|w| !w.redirect)
            .map(|w| w.text.as_str())
            .collect();
        let Some(p) = texts.first() else { continue };
        let name = p.rsplit('/').next().unwrap_or(p);
        // `touch`, `rm`, `chmod` and the like write no content a later command could run.
        if !CONTENT_WRITERS.contains(&name) {
            continue;
        }
        let append = name == "tee" && texts.iter().any(|a| matches!(*a, "-a" | "--append"));
        for (k, _) in operand_writes(name, &texts[1..]) {
            let t = texts[1 + k];
            out.push((t.strip_prefix("of=").unwrap_or(t).to_owned(), append));
        }
    }
    dedup(out)
}

/// Programs that put content in the file they write (what a later command may run).
const CONTENT_WRITERS: [&str; 12] = [
    "tee", "cp", "install", "ln", "mv", "dd", "curl", "wget", "rsync", "scp", "sed", "perl",
];

fn dedup(v: Vec<(String, bool)>) -> Vec<(String, bool)> {
    let mut seen = BTreeSet::new();
    v.into_iter().filter(|t| seen.insert(t.clone())).collect()
}

/// Which operands (indexes into `args`) of the program `name` it writes, and whether it also reads
/// them. The programs are those named in [`Access`].
#[must_use]
pub fn operand_writes(name: &str, args: &[&str]) -> Vec<(usize, bool)> {
    let operands: Vec<usize> = operand_indexes(name, args);
    let last = operands.last().copied();
    match name {
        "tee" => operands.iter().map(|&k| (k, false)).collect(),
        "cp" | "install" | "ln" | "rsync" | "scp" => {
            // `-t DIR` / `--target-directory=DIR` names the destination; else the last operand.
            let target = args.iter().enumerate().find_map(|(k, a)| {
                if matches!(*a, "-t" | "--target-directory") {
                    args.get(k + 1).map(|_| k + 1)
                } else {
                    a.strip_prefix("--target-directory=").map(|_| k)
                }
            });
            match (target, last) {
                (Some(k), _) => vec![(k, false)],
                (None, Some(k)) if operands.len() > 1 => vec![(k, false)],
                _ => Vec::new(),
            }
        }
        // The sources of `mv` are written too: they are removed.
        "mv" => {
            let target = args
                .iter()
                .position(|a| matches!(*a, "-t" | "--target-directory"))
                .and_then(|k| args.get(k + 1).map(|_| k + 1));
            let mut out: Vec<(usize, bool)> = operands.iter().map(|&k| (k, true)).collect();
            if let Some(t) = target {
                out.push((t, false));
            }
            out
        }
        "touch" | "mkdir" | "rmdir" | "rm" | "unlink" | "shred" | "mkfifo" | "mknod" | "trash"
        | "trash-put" | "srm" => operands.iter().map(|&k| (k, false)).collect(),
        "truncate" => operands.iter().map(|&k| (k, false)).collect(),
        // The first operand is the mode, the owner or the group, the rest are the files.
        "chmod" | "chown" | "chgrp" | "setfacl" | "chattr" => operands
            .iter()
            .skip(usize::from(!args.iter().any(|a| {
                a.starts_with("--reference") || a.starts_with("--from")
            })))
            .map(|&k| (k, false))
            .collect(),
        "sed" => {
            let in_place = args.iter().any(|a| {
                *a == "--in-place"
                    || a.starts_with("--in-place=")
                    || (a.starts_with('-')
                        && !a.starts_with("--")
                        && a[1..].starts_with(|c: char| c.is_ascii_alphabetic())
                        && a[1..].contains('i'))
            });
            if !in_place {
                return Vec::new();
            }
            // The script is the first operand, unless `-e` or `-f` gives it.
            let given = args.iter().any(|a| {
                matches!(*a, "-e" | "-f" | "--expression" | "--file")
                    || is_short_with(a, 'e')
                    || is_short_with(a, 'f')
            });
            operands
                .iter()
                .skip(usize::from(!given))
                .map(|&k| (k, true))
                .collect()
        }
        "perl" => {
            let in_place = args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('i'));
            if !in_place {
                return Vec::new();
            }
            let given = args.iter().any(|a| *a == "-e" || is_short_with(a, 'e'));
            operands
                .iter()
                .skip(usize::from(!given))
                .map(|&k| (k, true))
                .collect()
        }
        "dd" => args
            .iter()
            .enumerate()
            .filter(|(_, a)| a.starts_with("of="))
            .map(|(k, _)| (k, false))
            .collect(),
        "curl" => option_value_index(args, &["-o", "--output"]),
        "wget" => option_value_index(args, &["-O", "--output-document"]),
        "find" => {
            if args.contains(&"-delete") {
                // The folders before the first expression.
                args.iter()
                    .enumerate()
                    .take_while(|(_, a)| !a.starts_with('-') && !matches!(**a, "(" | "!"))
                    .map(|(k, _)| (k, false))
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// A cluster of short options holding `letter` (`-ne`), as a program reads it.
fn is_short_with(a: &str, letter: char) -> bool {
    a.starts_with('-') && !a.starts_with("--") && a.len() > 1 && a[1..].contains(letter)
}

/// The indexes of the operands of `args`: the words that are not options or their values. `--` ends
/// the options. The options that take a value are those of the programs in [`operand_writes`].
fn operand_indexes(name: &str, args: &[&str]) -> Vec<usize> {
    let value_opts: &[&str] = match name {
        "cp" | "mv" | "install" | "ln" => &["-t", "-S", "--target-directory", "--suffix"],
        "sed" => &["-e", "-f", "-l", "--expression", "--file", "--line-length"],
        "perl" => &["-e", "-E", "-I", "-M", "-m", "-x"],
        "truncate" => &["-s", "-r", "--size", "--reference"],
        "chmod" | "chown" | "chgrp" => &["--reference", "--from"],
        "mkdir" => &["-m", "--mode"],
        "touch" => &["-d", "-r", "-t", "--date", "--reference"],
        "rsync" => &["-e", "--rsh", "--exclude", "--include", "--filter"],
        "scp" => &["-P", "-i", "-o", "-F", "-c", "-l"],
        "shred" => &["-n", "-s", "--iterations", "--size"],
        _ => &[],
    };
    let mut out = Vec::new();
    let mut k = 0;
    let mut ended = false;
    while let Some(a) = args.get(k).copied() {
        if !ended && a == "--" {
            ended = true;
        } else if !ended && a.starts_with('-') && a.len() > 1 && !is_mode(name, a) {
            if value_opts.contains(&a) {
                k += 1;
            }
        } else {
            out.push(k);
        }
        k += 1;
    }
    out
}

/// A mode that starts with a dash (`chmod -x f`, `chmod -w,go-r f`): an operand, not an option.
fn is_mode(name: &str, a: &str) -> bool {
    name == "chmod"
        && a.len() > 1
        && a[1..].chars().all(|c| "rwxXstugo=+,-".contains(c))
        && !a[1..].chars().all(|c| "Rcfv".contains(c))
}

/// The index of the value of the first of `flags` in `args`.
fn option_value_index(args: &[&str], flags: &[&str]) -> Vec<(usize, bool)> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| flags.contains(a))
        .filter_map(|(k, _)| args.get(k + 1).map(|_| (k + 1, false)))
        .collect()
}

/// The text `piece` writes when it is `echo` or `printf` (alone), whole; `None` for anything else
/// or for words the reader cannot settle (a variable, a substitution).
fn echoed(piece: &str) -> Option<String> {
    let Parsed::Commands(cmds) = shell::parse(piece, "/") else {
        return None;
    };
    let [c] = cmds.as_slice() else { return None };
    echoed_words(&c.words)
}

/// [`echoed`] for the words of one command.
fn echoed_words(command: &[Word]) -> Option<String> {
    let words: Vec<&str> = command
        .iter()
        .filter(|w| !w.redirect)
        .map(|w| w.text.as_str())
        .collect();
    let name = words.first().map(|w| w.rsplit('/').next().unwrap_or(w))?;
    if !ECHOES.contains(&name) {
        return None;
    }
    // A glob character in an unquoted word is expanded by the shell: not the text written.
    if command.iter().any(|w| w.glob) {
        return None;
    }
    let args: Vec<&str> = words[1..]
        .iter()
        .copied()
        .skip_while(|a| name == "echo" && matches!(*a, "-n" | "-e" | "-E" | "-ne" | "-en"))
        .skip_while(|a| name == "printf" && *a == "--")
        .collect();
    let mut text = String::new();
    for a in &args {
        text.push_str(&unescape(a));
        text.push('\n');
    }
    Some(text)
}

/// Backslash escapes that make a line end, as `echo -e` and `printf` read them. Reading them in a
/// word `echo` would print as typed only adds lines to judge.
fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\r", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
}

/// Removes one layer of quotes and backslashes from a word.
fn unquote(w: &str) -> String {
    let mut out = String::new();
    let mut cs = w.chars();
    while let Some(c) = cs.next() {
        match c {
            '\'' | '"' => {}
            '\\' => out.extend(cs.next()),
            _ => out.push(c),
        }
    }
    out
}

/// The words after `>`, `>>`, `>|`, `&>`, `&>>` and `<>` in `piece`, outside quotes. `>&2`, `2>&1`
/// and `>(…)` name a descriptor or a process, not a file.
#[allow(clippy::many_single_char_names)] // the indexes and characters of one scan
fn redirect_targets(piece: &str) -> Vec<(String, bool)> {
    let cs: Vec<char> = piece.chars().collect();
    let mut out = Vec::new();
    let (mut quote, mut i) = (None::<char>, 0);
    while i < cs.len() {
        let c = cs[i];
        match quote {
            Some(q) => {
                if q == '"' && c == '\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\\' => i += 1,
                '\'' | '"' => quote = Some(c),
                '>' | '<' if c == '>' || cs.get(i + 1) == Some(&'>') => {
                    let append = c == '>' && cs.get(i + 1) == Some(&'>');
                    let mut j = i + 1;
                    if c == '<' {
                        j += 1;
                    }
                    if cs.get(j) == Some(&'>') || cs.get(j) == Some(&'|') {
                        j += 1;
                    }
                    if cs.get(j) == Some(&'&') {
                        // `>&2`, `>&-`: a descriptor, unless a file name follows (`>& file`).
                        let d = cs.get(j + 1);
                        if d.is_some_and(|d| d.is_ascii_digit() || *d == '-') {
                            i = j + 1;
                            continue;
                        }
                        j += 1;
                    }
                    if cs.get(j) == Some(&'(') {
                        i = j;
                        continue;
                    }
                    while cs.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                        j += 1;
                    }
                    let mut w = String::new();
                    let mut q = None::<char>;
                    while let Some(&ch) = cs.get(j) {
                        match q {
                            Some(qc) => {
                                if ch == qc {
                                    q = None;
                                } else {
                                    w.push(ch);
                                }
                            }
                            None => {
                                if ch == '\'' || ch == '"' {
                                    q = Some(ch);
                                } else if ch == '\\' {
                                    j += 1;
                                    w.extend(cs.get(j));
                                } else if " \t;&|<>()\n".contains(ch) {
                                    break;
                                } else {
                                    w.push(ch);
                                }
                            }
                        }
                        j += 1;
                    }
                    if !w.is_empty() {
                        out.push((w, append));
                    }
                    i = j;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Written, command_writes, heredoc_bodies, operand_writes, redirect_targets, scan_pieces,
    };
    use crate::classify::{split_unquoted, without_heredoc_bodies};
    use crate::shell::{self, Parsed};

    /// The writes of every piece of `text`, as the classifier splits it, in order.
    fn scan(text: &str) -> Vec<Written> {
        let head = without_heredoc_bodies(text).replace("$(", "\n");
        let pieces = split_unquoted(&head);
        scan_pieces(&pieces, heredoc_bodies(text))
            .into_iter()
            .flatten()
            .collect()
    }

    fn w(text: &str) -> Vec<(String, Option<String>)> {
        scan(text).into_iter().map(|w| (w.target, w.body)).collect()
    }

    fn targets(text: &str) -> Vec<String> {
        redirect_targets(text).into_iter().map(|t| t.0).collect()
    }

    // The writes of the commands the reader can read, as the classifier reads them.
    #[test]
    fn writes_of_a_command_the_reader_read_and_their_mode() {
        let writes = |text: &str| -> Vec<(String, Option<String>, bool)> {
            let Parsed::Commands(cmds) = shell::parse(text, "/") else {
                return Vec::new();
            };
            cmds.iter()
                .flat_map(|c| command_writes(&c.words))
                .map(|w| (w.target, w.body, w.append))
                .collect()
        };
        assert_eq!(
            writes("echo 'git push' > s.sh"),
            vec![("s.sh".into(), Some("git push\n".into()), false)]
        );
        assert_eq!(
            writes("echo more >> s.sh"),
            vec![("s.sh".into(), Some("more\n".into()), true)]
        );
        assert_eq!(
            writes("printf 'a' &>> s.sh"),
            vec![("s.sh".into(), Some("a\n".into()), true)]
        );
        assert_eq!(writes("cat a > s.sh"), vec![("s.sh".into(), None, false)]);
        assert_eq!(writes("cp a s.sh"), vec![("s.sh".into(), None, false)]);
        assert_eq!(writes("tee -a s.sh"), vec![("s.sh".into(), None, true)]);
        assert_eq!(writes("tee s.sh"), vec![("s.sh".into(), None, false)]);
        assert_eq!(writes("echo x > a > b").len(), 2);
        for text in [
            "echo hi",
            "cat s.sh",
            "touch s.sh",
            "echo x 2>&1",
            "cat < s.sh",
        ] {
            assert_eq!(writes(text), vec![], "{text}");
        }
    }

    // §6::55 — a heredoc, `printf` and `echo` redirected into a file are written files
    // whose text the call shows.
    #[test]
    fn heredoc_printf_and_echo_are_written_with_their_text() {
        let h = w("cat > s.sh <<'E'\ngit push\nE\nbash s.sh");
        assert_eq!(h, vec![("s.sh".to_owned(), Some("git push\n".to_owned()))]);
        let h = w("cat <<EOF > out.sh\nrm -r x\nEOF\n");
        assert_eq!(h, vec![("out.sh".to_owned(), Some("rm -r x\n".to_owned()))]);
        let h = w("tee s.sh <<'E'\nls\nE\n");
        assert_eq!(h, vec![("s.sh".to_owned(), Some("ls\n".to_owned()))]);
        let p = w("printf 'git push' > s; sh s");
        assert_eq!(p, vec![("s".to_owned(), Some("git push\n".to_owned()))]);
        let e = w("echo 'git push' >> s.sh && sh s.sh");
        assert_eq!(e, vec![("s.sh".to_owned(), Some("git push\n".to_owned()))]);
        let e = w("echo -e 'a\\ngit push' > s");
        assert_eq!(e, vec![("s".to_owned(), Some("a\ngit push\n".to_owned()))]);
        let t = w("<<'E' tee -a s.sh\nx\nE\n");
        assert_eq!(t, vec![("s.sh".to_owned(), Some("x\n".to_owned()))]);
    }

    // §6::56 — text the call does not show whole is not read: a variable, a
    // substitution, an unquoted heredoc with an expansion, another program's output.
    #[test]
    fn text_the_call_does_not_show_is_none() {
        assert_eq!(w("echo \"$X\" > s"), vec![("s".to_owned(), None)]);
        assert_eq!(w("echo $(cat y) > s"), vec![("s".to_owned(), None)]);
        assert_eq!(w("cat > s <<E\n$X\nE\n"), vec![("s".to_owned(), None)]);
        assert_eq!(w("cat > s <<E\n`id`\nE\n"), vec![("s".to_owned(), None)]);
        assert_eq!(w("curl -o s http://h/x"), vec![("s".to_owned(), None)]);
        assert_eq!(w("cat a > s"), vec![("s".to_owned(), None)]);
        assert_eq!(w("cp a s"), vec![("s".to_owned(), None)]);
        assert_eq!(w("echo *.sh > s"), vec![("s".to_owned(), None)]);
        // The same heredoc, quoted, is read.
        assert_eq!(
            w("cat > s <<'E'\n$X\nE\n"),
            vec![("s".to_owned(), Some("$X\n".to_owned()))]
        );
    }

    // Near misses: no write.
    #[test]
    fn a_command_that_writes_no_file_has_no_written_file() {
        for text in [
            "echo hi",
            "cat s.sh",
            "echo hi 2>&1",
            "echo hi >&2",
            "ls > /dev/null 2>&1; echo '> not a redirect'",
            "grep '>' file",
            "echo \"a > b\"",
            "sh < s.sh",
            "cat <<'E'\nx\nE\n",
            "diff <(ls) <(ls)",
        ] {
            let got = w(text);
            let ok = got.iter().all(|(t, _)| t == "/dev/null");
            assert!(ok, "{text}: {got:?}");
        }
    }

    #[test]
    fn redirect_targets_in_every_spelling() {
        for (text, want) in [
            ("echo x > a", vec!["a"]),
            ("echo x >a", vec!["a"]),
            ("echo x >> a", vec!["a"]),
            ("echo x &>> a", vec!["a"]),
            ("echo x >| a", vec!["a"]),
            ("echo x &> a", vec!["a"]),
            ("echo x 2> a", vec!["a"]),
            ("echo x > 'a b'", vec!["a b"]),
            ("echo x > \"a b\"", vec!["a b"]),
            ("echo x > a\\ b", vec!["a b"]),
            ("echo x > a > b", vec!["a", "b"]),
            ("echo x <> a", vec!["a"]),
            ("echo x >& a", vec!["a"]),
            ("echo x 2>&1", vec![]),
            ("echo x >&2", vec![]),
            ("echo x < a", vec![]),
            ("echo '>' a", vec![]),
        ] {
            assert_eq!(targets(text), want, "{text}");
        }
    }

    #[test]
    fn redirect_modes() {
        let modes = |t: &str| {
            redirect_targets(t)
                .into_iter()
                .map(|t| t.1)
                .collect::<Vec<_>>()
        };
        assert_eq!(modes("echo x > a"), vec![false]);
        assert_eq!(modes("echo x >> a"), vec![true]);
        assert_eq!(modes("echo x &>> a"), vec![true]);
        assert_eq!(modes("echo x >| a"), vec![false]);
        assert_eq!(modes("echo x > a >> b"), vec![false, true]);
        assert_eq!(modes("tee -a a < b"), Vec::<bool>::new());
    }

    // §5::44 — which operands each program writes.
    #[test]
    fn operand_writes_by_program() {
        let t = |name: &str, line: &str| -> Vec<(usize, bool)> {
            let args: Vec<&str> = line.split_whitespace().collect();
            operand_writes(name, &args)
        };
        assert_eq!(t("tee", "-a f g"), vec![(1, false), (2, false)]);
        assert_eq!(t("cp", "a b"), vec![(1, false)]);
        assert_eq!(t("cp", "-r a b c"), vec![(3, false)]);
        assert_eq!(t("cp", "-t d a b"), vec![(1, false)]);
        assert_eq!(t("cp", "a"), vec![]);
        assert_eq!(t("mv", "a b"), vec![(0, true), (1, true)]);
        assert_eq!(t("touch", "a b"), vec![(0, false), (1, false)]);
        assert_eq!(t("rm", "-rf a"), vec![(1, false)]);
        assert_eq!(t("chmod", "+x a b"), vec![(1, false), (2, false)]);
        assert_eq!(t("chown", "-R me a"), vec![(2, false)]);
        assert_eq!(t("sed", "-i s/a/b/ f"), vec![(2, true)]);
        assert_eq!(t("sed", "-i.bak s/a/b/ f g"), vec![(2, true), (3, true)]);
        assert_eq!(t("sed", "-ni -e p f"), vec![(3, true)]);
        assert_eq!(t("sed", "--in-place s/a/b/ f"), vec![(2, true)]);
        assert_eq!(t("sed", "s/a/b/ f"), vec![]);
        assert_eq!(t("perl", "-pi -e s/a/b/ f"), vec![(3, true)]);
        assert_eq!(t("dd", "if=a of=b"), vec![(1, false)]);
        assert_eq!(t("curl", "-s -o out http://h"), vec![(2, false)]);
        assert_eq!(t("wget", "-O out http://h"), vec![(1, false)]);
        assert_eq!(
            t("find", "d e -name x -delete"),
            vec![(0, false), (1, false)]
        );
        assert_eq!(t("find", "d -name x"), vec![]);
        assert_eq!(t("cat", "a b"), vec![]);
        assert_eq!(t("grep", "x a"), vec![]);
    }
}
