//! Shell variables that hold a literal (spec §12 F1.2, §6 P1.4).
//!
//! The shell reader cannot read a word with a variable in it, so a command such as `X=build; rm -r
//! $X` is "a command the guard cannot parse". Where the variable was given a literal value earlier
//! in the same text, the word is no longer unknown: this module returns the text with each such use
//! replaced by the value, which the reader then reads. A variable the text does not give a literal
//! value stays as it is, and the command stays unparsed: one from the environment, from `read`,
//! from a command substitution, from an argument (`$1`), or assigned in a way that may not have
//! happened before the use.
//!
//! What counts as known, by the last assignment before the use in the text:
//! * `NAME=value`, `export NAME=value`, `readonly NAME=value`, `local NAME=value` and `declare
//!   NAME=value` (and `typeset`) as a statement of their own, with a value of plain characters
//!   (letters, digits and `_ . / : @ % + , = ~ -`), bare or in quotes;
//! * `$NAME` and `${NAME}`, bare or in double quotes, not in single quotes and not behind a
//!   backslash;
//! * a use in the value of a later assignment (`B=$A/x`).
//!
//! What makes it unknown from there on: an assignment that is not a plain literal (`$(…)`, a
//! variable that is unknown, spaces, a wildcard), `read`, `unset`, `getopts`, `mapfile`, `printf
//! -v`, `let`, `eval`, `source`, `declare -i`, `-n`; an assignment inside an `if`, loop or function
//! (the variable is unknown from the start of the outermost construct that holds it, since the
//! construct may run or repeat), after `&&`, `||`, `|` or `&`, or as the variable of a `for`; and
//! any use inside a function body. A text with a subshell, an array, a `case`, or other grouping the
//! module does not follow is returned unchanged.

use std::collections::{HashMap, HashSet};

/// The characters a value may hold to be substituted as it stands.
fn plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:@%+,=~-".contains(c)
}

/// One statement of the text: its byte range, the separator before it, and what it does to the
/// constructs around it.
struct Stmt {
    start: usize,
    end: usize,
    /// The separator before it (`;`, newline, `&&`, `||`, `|`, `&`), `\n` for the first.
    sep: &'static str,
    /// It is followed by `&` or `|`: it runs in a subshell or in the background, and its
    /// assignments are lost.
    bg: bool,
}

/// The text with each use of a variable that holds a literal replaced by the value; `None` when
/// nothing is replaced (or the text has grouping this module does not follow).
#[must_use]
pub fn substitute(text: &str) -> Option<String> {
    if !text.contains('$') {
        return None;
    }
    let stmts = split(text)?;
    let facts = facts(text, &stmts)?;
    let mut env: HashMap<String, String> = HashMap::new();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut changed = false;
    for (k, s) in stmts.iter().enumerate() {
        out.push_str(&text[at..s.start]);
        at = s.end;
        let piece = &text[s.start..s.end];
        let f = &facts[k];
        let known = |name: &str| -> Option<&String> {
            if f.in_function || f.blocked.contains(name) {
                None
            } else {
                env.get(name)
            }
        };
        let replaced = replace_uses(piece, &known);
        if replaced != piece {
            changed = true;
        }
        // What this statement does to the variables that follow.
        apply(&replaced, f, &mut env);
        out.push_str(&replaced);
    }
    out.push_str(&text[at..]);
    changed.then_some(out)
}

/// What is known about one statement before the variables are followed.
struct Fact {
    /// Inside a function body.
    in_function: bool,
    /// Variables unknown from here on: the ones an assignment makes uncertain at or before this
    /// statement ([`facts`]).
    blocked: HashSet<String>,
    /// The statement runs at the top level, in order (not in a construct, not after `&&`, `||`,
    /// `|` or `&`): its assignments are certain.
    certain: bool,
}

#[allow(clippy::too_many_lines)] // one arm per character that matters
/// Splits `text` into its statements. `None` for a text with a subshell, an array, a `case` or a
/// word the module does not follow. Heredoc bodies are left out (they are in no statement).
fn split(text: &str) -> Option<Vec<Stmt>> {
    let b = text.as_bytes();
    let mut stmts = Vec::new();
    let mut start = 0;
    let mut sep: &'static str = "\n";
    let (mut quote, mut cmd, mut tick) = (0u8, 0usize, false);
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    let push = |stmts: &mut Vec<Stmt>, start: usize, end: usize, sep: &'static str| {
        if !text[start..end].trim().is_empty() {
            stmts.push(Stmt {
                start,
                end,
                sep,
                bg: false,
            });
        }
    };
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' && quote == b'"' {
                i += 1;
            } else if c == quote {
                quote = 0;
            } else if quote == b'"' && c == b'$' && b.get(i + 1) == Some(&b'(') {
                cmd += 1;
                i += 1;
            } else if quote == b'"' && c == b')' && cmd > 0 {
                cmd -= 1;
            }
            i += 1;
            continue;
        }
        match c {
            b'\\' => i += 1,
            b'\'' | b'"' => quote = c,
            b'`' => tick = !tick,
            b'$' if b.get(i + 1) == Some(&b'(') => {
                cmd += 1;
                i += 1;
            }
            b'(' if cmd > 0 => cmd += 1,
            b')' if cmd > 0 => cmd -= 1,
            // A function header `name()` is the only grouping followed.
            b'(' if cmd == 0 && b.get(i + 1) == Some(&b')') => i += 1,
            b'(' | b')' if cmd == 0 && !tick => return None,
            b'<' if cmd == 0 && !tick && b.get(i + 1) == Some(&b'<') => {
                if b.get(i + 2) == Some(&b'<') {
                    i += 2;
                } else {
                    let rest = &text[i + 2..];
                    let tabs = rest.starts_with('-');
                    let raw: String = rest
                        .trim_start_matches('-')
                        .trim_start()
                        .chars()
                        .take_while(|c| !c.is_whitespace() && !";&|<>()".contains(*c))
                        .collect();
                    let word: String = raw.chars().filter(|c| !"'\"\\".contains(*c)).collect();
                    if !word.is_empty() {
                        pending.push((word, tabs));
                    }
                    i += 1;
                }
            }
            b';' | b'&' | b'|' | b'\n' if cmd == 0 && !tick => {
                let before = stmts.len();
                push(&mut stmts, start, i, sep);
                let two = matches!((c, b.get(i + 1)), (b'&', Some(b'&')) | (b'|', Some(b'|')));
                let amp_gt = c == b'&' && b.get(i + 1) == Some(&b'>');
                let pipe_amp = c == b'|' && b.get(i + 1) == Some(&b'&');
                if stmts.len() > before
                    && !amp_gt
                    && matches!(c, b'&' | b'|')
                    && !two
                    && let Some(last) = stmts.last_mut()
                {
                    last.bg = true;
                }
                if pipe_amp {
                    i += 1;
                }
                if amp_gt {
                    // `&>file` is a redirection, not a separator.
                    i += 1;
                    i += 1;
                    continue;
                }
                sep = match (c, two) {
                    (b'&', true) => "&&",
                    (b'|', true) => "||",
                    (b'&', false) => "&",
                    (b'|', false) => "|",
                    (b'\n', _) => "\n",
                    _ => ";",
                };
                i += usize::from(two);
                if c == b'\n' && !pending.is_empty() {
                    // The bodies of the heredocs this line opened are in no statement.
                    let mut at = i + 1;
                    for (word, tabs) in pending.drain(..) {
                        loop {
                            if at >= b.len() {
                                break;
                            }
                            let end = text[at..].find('\n').map_or(b.len(), |n| at + n);
                            let line = &text[at..end];
                            let line = if tabs {
                                line.trim_start_matches('\t')
                            } else {
                                line
                            };
                            at = (end + 1).min(b.len());
                            if line == word {
                                break;
                            }
                        }
                    }
                    start = at;
                    i = at;
                    continue;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if quote != 0 || cmd != 0 || tick {
        return None;
    }
    push(&mut stmts, start, b.len(), sep);
    Some(stmts)
}

/// The words of a statement: runs of text between unquoted blanks.
fn words(stmt: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut quote, mut start, mut esc) = (None::<char>, None::<usize>, false);
    for (i, c) in stmt.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match (quote, c) {
            (None, ' ' | '\t') => {
                if let Some(s) = start.take() {
                    out.push(&stmt[s..i]);
                }
            }
            (None | Some('"'), '\\') => {
                start.get_or_insert(i);
                esc = true;
            }
            (None, '\'' | '"') => {
                start.get_or_insert(i);
                quote = Some(c);
            }
            (Some(q), _) if c == q => quote = None,
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&stmt[s..]);
    }
    out
}

const CONTROL: [&str; 6] = ["then", "do", "else", "elif", "time", "!"];
const OPENERS: [&str; 5] = ["if", "while", "until", "for", "select"];
const DECLARERS: [&str; 5] = ["export", "readonly", "local", "declare", "typeset"];

/// The statement without the control words that lead it (`then`, `do`, `else`, `elif`).
fn body(stmt: &str) -> &str {
    let mut s = stmt.trim_start();
    loop {
        let first = s.split_whitespace().next().unwrap_or_default();
        if CONTROL.contains(&first) {
            s = s[first.len()..].trim_start();
        } else {
            return s;
        }
    }
}

/// A name that may be assigned.
fn is_name(n: &str) -> bool {
    let mut cs = n.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `NAME=value` → (`NAME`, `value`).
fn assignment(word: &str) -> Option<(&str, &str)> {
    let (n, _) = word.split_once('=')?;
    let n = n.strip_suffix('+').unwrap_or(n);
    is_name(n).then(|| (n, &word[word.find('=').map_or(0, |k| k + 1)..]))
}

/// What a statement does to variables: names it makes unknown, names it gives a value (with the
/// value word), and whether it makes everything unknown.
#[derive(Default)]
struct Effect<'a> {
    unknown: Vec<&'a str>,
    set: Vec<(&'a str, &'a str)>,
    everything: bool,
}

fn effect(stmt: &str) -> Effect<'_> {
    let ws = words(body(stmt));
    let mut e = Effect::default();
    let Some(&first) = ws.first() else { return e };
    // `NAME=value … ` alone: the variables are set for the rest of the text. With a command after
    // them they are set for that command only.
    if assignment(first).is_some() {
        let mut k = 0;
        while let Some((n, v)) = ws.get(k).and_then(|w| assignment(w)) {
            if ws[k].contains("+=") {
                e.unknown.push(n);
            } else {
                e.set.push((n, v));
            }
            k += 1;
        }
        if k < ws.len() {
            e.set.clear();
        }
        return e;
    }
    let rest = &ws[1..];
    match first {
        f if DECLARERS.contains(&f) => {
            let opts: String = rest
                .iter()
                .filter(|w| w.starts_with('-') && w.len() > 1)
                .flat_map(|w| w[1..].chars())
                .collect();
            let by_value = opts.contains(['i', 'n', 'a', 'A']);
            for w in rest.iter().filter(|w| !w.starts_with('-')) {
                if let Some((n, v)) = assignment(w) {
                    if by_value || w.contains("+=") {
                        e.unknown.push(n);
                    } else {
                        e.set.push((n, v));
                    }
                } else if is_name(w) && matches!(f, "local" | "declare" | "typeset") {
                    e.unknown.push(w);
                }
            }
        }
        "read" | "mapfile" | "readarray" | "getopts" | "unset" | "let" => {
            for w in rest.iter().filter(|w| !w.starts_with('-')) {
                let n = w.split('=').next().unwrap_or(w);
                if is_name(n) {
                    e.unknown.push(n);
                }
            }
            // `read` with no name sets REPLY.
            e.unknown.push("REPLY");
        }
        "printf" => {
            if let Some(k) = rest.iter().position(|w| *w == "-v")
                && let Some(n) = rest.get(k + 1)
            {
                e.unknown.push(n);
            }
        }
        "eval" | "source" | "." | "exec" | "trap" | "alias" => e.everything = true,
        _ => {}
    }
    e
}

/// Where one statement sits.
struct Row {
    depth_zero: bool,
    in_function: bool,
    start: usize,
    conditional: bool,
}

/// For each statement: where it sits, and which variables are unknown at it.
fn facts(text: &str, stmts: &[Stmt]) -> Option<Vec<Fact>> {
    // Start of the outermost open construct, and whether any open construct is a function.
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut header = false;
    // Where each variable becomes unknown (a construct that may or may not run, or repeat, holds an
    // assignment to it), and where a statement that runs for certain gives it a value again.
    let mut from: HashMap<String, Vec<usize>> = HashMap::new();
    let mut resets: HashMap<String, Vec<usize>> = HashMap::new();
    let mut rows = Vec::new();
    for s in stmts {
        let piece = &text[s.start..s.end];
        let b = body(piece);
        let first = b.split_whitespace().next().unwrap_or_default();
        if first == "case" || first == "coproc" {
            return None;
        }
        let conditional = matches!(s.sep, "&&" | "||" | "|" | "&") || s.bg;
        // A closer ends a construct before anything else in the statement.
        if matches!(first, "fi" | "done" | "esac" | "}") {
            open.pop();
        }
        let in_function = open.iter().any(|(_, f)| *f);
        rows.push(Row {
            depth_zero: open.is_empty(),
            in_function,
            start: s.start,
            conditional,
        });
        let outer = open.first().map_or(s.start, |(o, _)| *o);
        // Assignments made here.
        let e = effect(piece);
        let certain = open.is_empty() && !conditional;
        for n in e
            .unknown
            .iter()
            .copied()
            .chain(e.set.iter().map(|(n, _)| *n))
        {
            if !certain {
                let at = if open.is_empty() { s.start } else { outer };
                from.entry(n.to_owned()).or_default().push(at);
            }
        }
        if certain {
            for (n, _) in &e.set {
                resets.entry((*n).to_owned()).or_default().push(s.start);
            }
        }
        // `for NAME in …` sets NAME in a construct that repeats.
        if matches!(first, "for" | "select")
            && let Some(n) = b.split_whitespace().nth(1)
        {
            from.entry(n.to_owned()).or_default().push(s.start);
        }
        // A function header: `name()`, `name() {`, `function name`.
        let is_header = first == "function"
            || piece.contains("()") && b.split("()").next().is_some_and(|n| is_name(n.trim()));
        if is_header {
            header = true;
        }
        let opens_brace = first == "{" || b.contains("() {") || b.ends_with('{');
        if OPENERS.contains(&first) {
            open.push((s.start, false));
        } else if opens_brace {
            open.push((s.start, header));
            header = false;
        }
    }
    if !open.is_empty() {
        return None;
    }
    Some(
        rows.iter()
            .map(|r| Fact {
                in_function: r.in_function,
                blocked: from
                    .iter()
                    .filter(|(n, starts)| {
                        // The latest start before here, unless a statement that runs for certain
                        // has given the variable a value since.
                        starts
                            .iter()
                            .copied()
                            .filter(|a| *a <= r.start)
                            .max()
                            .is_some_and(|a| {
                                !resets
                                    .get(*n)
                                    .is_some_and(|ps| ps.iter().any(|p| *p > a && *p < r.start))
                            })
                    })
                    .map(|(n, _)| n.clone())
                    .collect(),
                certain: r.depth_zero && !r.conditional,
            })
            .collect(),
    )
}

/// What a statement does to the variables that follow it, after its uses were replaced.
fn apply(stmt: &str, f: &Fact, env: &mut HashMap<String, String>) {
    let e = effect(stmt);
    if e.everything {
        env.clear();
    }
    for n in &e.unknown {
        env.remove(*n);
    }
    for (n, v) in &e.set {
        match (f.certain, literal(v)) {
            (true, Some(lit)) => {
                env.insert((*n).to_owned(), lit);
            }
            _ => {
                env.remove(*n);
            }
        }
    }
}

/// The value a word spells, when it is plain text in quotes or not.
fn literal(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut quote = None::<char>;
    for c in word.chars() {
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            _ if plain(c) => out.push(c),
            _ => return None,
        }
    }
    quote.is_none().then_some(out)
}

/// `piece` with each use of a known variable replaced by its value.
fn replace_uses<'a>(piece: &str, known: &dyn Fn(&str) -> Option<&'a String>) -> String {
    let cs: Vec<char> = piece.chars().collect();
    let mut out = String::with_capacity(piece.len());
    let (mut quote, mut i) = (None::<char>, 0);
    while i < cs.len() {
        let c = cs[i];
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'') => quote = Some('\''),
            (None, '"') => quote = Some('"'),
            (q, '\\') if q != Some('\'') => {
                out.push(c);
                if let Some(n) = cs.get(i + 1) {
                    out.push(*n);
                }
                i += 2;
                continue;
            }
            (q, '$') if q != Some('\'') => {
                let braced = cs.get(i + 1) == Some(&'{');
                let from = i + 1 + usize::from(braced);
                let mut j = from;
                while cs
                    .get(j)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    j += 1;
                }
                let name: String = cs[from..j].iter().collect();
                let closed = !braced || cs.get(j) == Some(&'}');
                if is_name(&name)
                    && closed
                    && let Some(v) = known(&name)
                {
                    out.push_str(v);
                    i = j + usize::from(braced);
                    continue;
                }
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::substitute;

    fn s(text: &str) -> String {
        substitute(text).unwrap_or_else(|| text.to_owned())
    }

    // The last assignment before the use decides; both spellings of a use are replaced.
    #[test]
    fn literal_variables_are_replaced_by_the_last_assignment_before_the_use() {
        for (text, want) in [
            ("X=build; rm -r $X", "X=build; rm -r build"),
            ("X=build; rm -r ${X}/out", "X=build; rm -r build/out"),
            ("X=build\nrm -r \"$X\"", "X=build\nrm -r \"build\""),
            ("X=a; X=b; rm $X", "X=a; X=b; rm b"),
            ("export X=a; rm $X", "export X=a; rm a"),
            ("readonly X=a; rm $X", "readonly X=a; rm a"),
            ("declare -x X=a; rm $X", "declare -x X=a; rm a"),
            ("local X=a; rm $X", "local X=a; rm a"),
            ("X='a'; rm $X", "X='a'; rm a"),
            ("X=\"a\"; rm $X", "X=\"a\"; rm a"),
            ("A=/tmp; B=$A/x; rm $B", "A=/tmp; B=/tmp/x; rm /tmp/x"),
            ("X=a; Y=b; rm $X $Y", "X=a; Y=b; rm a b"),
            ("X=a Y=b\nrm $X$Y", "X=a Y=b\nrm ab"),
            (
                "X=a; if true; then rm $X; fi",
                "X=a; if true; then rm a; fi",
            ),
            ("X=a; echo $(echo $X)", "X=a; echo $(echo a)"),
            ("X=a; rm $XY", "X=a; rm $XY"),
            ("X=; rm -r y$X", "X=; rm -r y"),
            ("X=a && rm $X; rm $X", "X=a && rm a; rm a"),
            (
                "if c; then X=a; fi; X=b; rm $X",
                "if c; then X=a; fi; X=b; rm b",
            ),
            (
                "while c; do X=a; done; X=b; rm $X",
                "while c; do X=a; done; X=b; rm b",
            ),
            ("true || X=a; X=b; rm $X", "true || X=a; X=b; rm b"),
            ("X=a; X=b && rm $X", "X=a; X=b && rm b"),
        ] {
            assert_eq!(s(text), want, "{text}");
        }
    }

    // A variable the text does not give a literal stays as it is.
    #[test]
    fn other_variables_are_left_alone() {
        // The exact results where nothing may be replaced.
        for text in [
            "rm $X",
            "rm ${X:-b}",
            "X=$(date); rm $X",
            "X='a b'; rm $X",
            "read X; rm $X",
            "X=a; read X; rm $X",
            "X=a; unset X; rm $X",
            "X=a; eval foo; rm $X",
            "X=a cmd; rm $X",
            "X=a; echo '$X'",
            "X=a & rm $X",
            "X=a | cat; rm $X",
            "true || X=a; rm $X",
            "echo hi | read X; rm $X",
            "X=a; while c; do rm $X; X=b; done",
            "X=a; for X in a b; do :; done; rm $X",
            "f() {\n X=a\n}\nf\nrm $X",
            "X=a; f() {\n rm $X\n}\nf",
            "X=a; getopts ab X; rm $X",
            "X=a; printf -v X %s b; rm $X",
            "X=a; mapfile X < f; rm $X",
            "X=a; declare -i X=2; rm $X",
            "X=a; X+=b; rm $X",
            "X=a*; rm $X",
            "X=`date`; rm $X",
            "X=$Y; rm $X",
            "X=a; source f; rm $X",
            "X=a; echo \\$X",
            "arr=(a b); rm $arr",
            "X=a; if c; then X=b; fi; rm $X",
            "rm $1",
            "if c; then X=a; fi; rm $X",
            "for X in a b; do rm $X; done",
            "X=a; (rm $X)",
            "case x in a) X=b;; esac; rm $X",
        ] {
            assert_eq!(s(text), text, "{text}");
        }
    }

    #[test]
    fn heredoc_bodies_and_quotes_are_left_as_written() {
        let t = "X=a\ncat > f <<'E'\nrm $X\nE\nrm $X\n";
        assert_eq!(s(t), "X=a\ncat > f <<'E'\nrm $X\nE\nrm a\n");
        assert_eq!(s("X=a; echo \"'$X'\""), "X=a; echo \"'a'\"");
        assert_eq!(s("X=a; echo '\"$X\"'"), "X=a; echo '\"$X\"'");
        assert_eq!(substitute(""), None);
        assert_eq!(substitute("echo hi"), None);
    }
}
