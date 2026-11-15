//! Whether a command reads the folder it runs in (spec §11 L1.1, Hermes `workdir_unknown`).
//!
//! `§11::16`: "Where a Hermes `terminal` command is judged with no folder, for a HELD
//! session Lunchroom shall PROTECT it only when it names a relative path, or runs, with no path
//! named, a program that reads the folder it runs in when given none (the guard's table: a search
//! such as `grep -r`, `rg`, `find`, `ls`, `du` or `tree`, and `git`, `make`, `cargo`, `npm`, `pnpm`,
//! `yarn`, `pip`, `python -m`, `pytest`, `go` and `tar` with no file named)." `§11::17`:
//! otherwise the command is judged as any other command.
//!
//! A word names a relative path when it is no option, no absolute or home path, no assignment and
//! no number, and it looks like a path (a `/`, a leading `.`, a wildcard or a file extension), or
//! the program it is given to takes file names (`cat notes`, `cd sub`): a bare word given to a
//! program that takes names, hosts or users (`ssh host`, `id bob`) names none.

use super::{
    GREPS, INTERPRETERS, RECURSE_ON_FLAG, SEARCHERS, SHELLS, basename, grep_path_operands,
    is_assignment, program_positions, text_positions,
};
use crate::shell::{self, Parsed};

/// Programs whose own words after the program are subcommands or targets (`git status`,
/// `make test`), not paths: a bare word after them names no relative path.
const SUBCOMMANDS: [&str; 11] = [
    "git", "make", "cargo", "npm", "pnpm", "yarn", "pip", "pip3", "pytest", "go", "tar",
];

/// Programs that take file names: a bare word given to one names a relative path.
const FILE_READERS: &[&str] = &[
    "cat",
    "tac",
    "head",
    "tail",
    "less",
    "more",
    "nl",
    "wc",
    "sort",
    "uniq",
    "cut",
    "paste",
    "sed",
    "awk",
    "gawk",
    "mawk",
    "grep",
    "egrep",
    "fgrep",
    "zgrep",
    "rg",
    "ag",
    "ack",
    "xxd",
    "od",
    "hexdump",
    "strings",
    "file",
    "stat",
    "readlink",
    "realpath",
    "cp",
    "mv",
    "rm",
    "rmdir",
    "ln",
    "touch",
    "mkdir",
    "chmod",
    "chown",
    "chgrp",
    "truncate",
    "diff",
    "cmp",
    "zip",
    "unzip",
    "gzip",
    "gunzip",
    "cksum",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "source",
    ".",
    "cd",
    "pushd",
    "tee",
    "dd",
    "install",
    "jq",
    "bat",
];

/// The command `text`, which a session ran with no folder the guard could read, names a relative
/// path, or runs a program of the guard's table with no path named, so that what it reads depends
/// on a folder the guard does not know. Text the reader cannot read counts: it is PROTECTED while
/// HELD in any case (F1.2).
#[must_use]
pub fn reads_its_folder(text: &str) -> bool {
    let Parsed::Commands(cmds) = shell::parse(text, "/") else {
        return true;
    };
    // After `cd` to an absolute folder, the working folder is known to the commands after it.
    let mut known = false;
    for c in &cmds {
        let words = &c.words;
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        let positions = program_positions(&texts);
        if !known && command_reads_its_folder(words, &texts, &positions) {
            return true;
        }
        if let Some(&p) = positions.first()
            && matches!(basename(texts[p]), "cd" | "pushd")
        {
            let target = texts[p + 1..].iter().find(|a| !a.starts_with('-'));
            known |= target.is_none_or(|t| t.starts_with('/'));
        }
    }
    false
}

/// `find` predicates whose next word is their value (a pattern, a type, a number), not a path.
const FIND_VALUES: [&str; 22] = [
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-regex",
    "-iregex",
    "-type",
    "-size",
    "-perm",
    "-user",
    "-group",
    "-newer",
    "-mtime",
    "-atime",
    "-ctime",
    "-mmin",
    "-amin",
    "-cmin",
    "-maxdepth",
    "-mindepth",
    "-wholename",
    "-lname",
];

fn command_reads_its_folder(words: &[shell::Word], texts: &[&str], positions: &[usize]) -> bool {
    let mut skip = text_positions(words, texts, positions);
    // The words that are code, a pattern or a value, and no path.
    for &p in positions {
        let name = basename(texts[p]);
        for k in p + 1..texts.len() {
            let after_code_flag = matches!(texts[k - 1], "-c" | "-e")
                && (SHELLS.contains(&name) || INTERPRETERS.contains(&name));
            let after_find_value = name == "find" && FIND_VALUES.contains(&texts[k - 1]);
            if after_code_flag || after_find_value {
                skip.push(k);
            }
        }
        // `sh -c 'cat x'`: the text is a command, judged as one.
        if SHELLS.contains(&name)
            && let Some(k) = texts[p + 1..].iter().position(|a| *a == "-c")
            && let Some(code) = texts.get(p + 2 + k)
            && reads_its_folder(code)
        {
            return true;
        }
    }
    for (k, w) in words.iter().enumerate() {
        if skip.contains(&k) || w.text.is_empty() {
            continue;
        }
        if positions.contains(&k) {
            // A program run by a path: `./run.sh`.
            if w.text.contains('/') && !w.text.starts_with('/') {
                return true;
            }
            continue;
        }
        // A redirection's target is a path whatever it looks like.
        if w.redirect && !w.text.starts_with('/') {
            return true;
        }
        if names_relative_path(&w.text, positions, texts, k) {
            return true;
        }
    }
    for &p in positions {
        let name = basename(texts[p]);
        let all: Vec<&str> = (p + 1..texts.len())
            .filter(|&k| !words[k].redirect)
            .map(|k| texts[k])
            .collect();
        let args: Vec<&str> = (p + 1..texts.len())
            .filter(|&k| !words[k].redirect && !skip.contains(&k))
            .map(|k| texts[k])
            .collect();
        let absolute = args.iter().any(|a| a.starts_with('/'));
        let reads = if GREPS.contains(&name) {
            // `grep -r pattern`: the pattern is the first word that is no option, and nothing else
            // names a path.
            recursive(&all) && grep_path_operands(&all).is_empty()
        } else if RECURSE_ON_FLAG.contains(&name) {
            // `ls`: the folder it lists when given none.
            args.iter().all(|a| a.starts_with('-'))
        } else if SEARCHERS.contains(&name) || SUBCOMMANDS.contains(&name) {
            !absolute
        } else if name.starts_with("python") {
            args.contains(&"-m") && !absolute
        } else {
            false
        };
        if reads {
            return true;
        }
    }
    false
}

/// A `grep` is told to recurse: `-r`, `-R`, `--recursive` or a cluster holding either.
fn recursive(args: &[&str]) -> bool {
    args.iter().any(|a| {
        *a == "--recursive"
            || a.strip_prefix('-')
                .is_some_and(|r| !r.starts_with('-') && r.contains(['r', 'R']))
    })
}

/// `word`, a word of a command other than its program, names a relative path.
fn names_relative_path(word: &str, positions: &[usize], texts: &[&str], k: usize) -> bool {
    if word.starts_with(['-', '/', '+']) || is_assignment(word) {
        return false;
    }
    if word.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let path_looking = word.contains(['/', '*', '?', '['])
        || word.starts_with('.')
        || std::path::Path::new(word).extension().is_some();
    let program = positions
        .iter()
        .rev()
        .find(|&&p| p < k)
        .map(|&p| basename(texts[p]));
    match program {
        // `git status`, `make test`: a subcommand or a target, unless it looks like a path.
        Some(p) if SUBCOMMANDS.contains(&p) => path_looking,
        Some(p) => path_looking || takes_names(p),
        None => path_looking,
    }
}

/// A bare word given to this program names a file.
fn takes_names(program: &str) -> bool {
    FILE_READERS.contains(&program)
        || SHELLS.contains(&program)
        || INTERPRETERS.contains(&program)
        || program.starts_with("python")
}

#[cfg(test)]
mod tests {
    use super::reads_its_folder;

    // §11::16 — a command that names a relative path, or runs a program of the table
    // with no path named, depends on the folder it runs in.
    #[test]
    fn a_command_that_depends_on_its_folder() {
        for cmd in [
            "cat ./notes.txt",
            "cat notes.txt",
            "cat ../x",
            "cat notes",
            "rg secret",
            "rg -n secret",
            "grep -r secret",
            "grep -rn secret",
            "grep -R secret",
            "grep --recursive secret",
            "find",
            "find -name x",
            "ls",
            "ls -la",
            "du",
            "du -sh",
            "tree",
            "git status",
            "git log -3",
            "git commit -m message",
            "make",
            "make test",
            "cargo build",
            "npm install",
            "pnpm i",
            "yarn",
            "pip install -r r.txt",
            "python3 -m pytest",
            "python -m http.server",
            "pytest",
            "pytest -x",
            "go test",
            "tar xf",
            "./run.sh",
            "bash run.sh",
            "echo hi > out.txt",
            "echo hi > out",
            "sudo cat notes.txt",
            "env X=1 ls",
            "timeout 5 rg x",
            "cat *.txt",
            "cd sub && ls /tmp",
            "git add ./x",
            "git add src/x.rs",
            "cat /tmp/x ; cat y",
            "sh -c 'cat x'",
            "head -n 5 data.csv",
            "wc -l file",
            "python3 script.py",
            "cp /tmp/a b",
        ] {
            assert!(reads_its_folder(cmd), "{cmd}");
        }
    }

    // §11::16, §11::17 — absolute paths, no path, and programs that read nothing there.
    #[test]
    fn a_command_that_does_not_depend_on_its_folder() {
        for cmd in [
            "cat /tmp/x",
            "cat ~/x",
            "date",
            "date +%s",
            "pwd",
            "whoami",
            "echo hello",
            "echo hello world",
            "printf '%s' x",
            "sleep 5",
            "rg secret /tmp",
            "rg -n secret /var/log",
            "grep secret /tmp/x",
            "grep -r secret /tmp",
            "grep -r secret ~/src",
            "find /tmp -name x",
            "find /tmp -type f -name '*.log'",
            "ls /tmp",
            "ls -la /var",
            "du -sh /var",
            "tree /tmp",
            "git -C /srv/repo status",
            "tar xf /tmp/a.tar",
            "python3 /tmp/x.py",
            "python3 -c 'print(1)'",
            "sh -c date",
            "sh -c 'cat /tmp/x'",
            "ssh host",
            "id bob",
            "cd /tmp && cat x",
            "cd /tmp; ls",
            "cd && cat x",
            "sudo cat /tmp/x",
            "env X=1 date",
            "nproc",
            "uname -a",
            "id",
        ] {
            assert!(!reads_its_folder(cmd), "{cmd}");
        }
    }

    // Text the reader cannot read is PROTECTED anyway; the table says it depends on the folder.
    #[test]
    fn text_the_reader_cannot_read_counts() {
        assert!(reads_its_folder("cat $X"));
        assert!(reads_its_folder("cat <<E\nx\nE\n"));
        assert!(reads_its_folder("echo $(ls)"));
        assert!(!reads_its_folder(""));
    }
}
