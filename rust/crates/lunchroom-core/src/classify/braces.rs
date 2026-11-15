//! Brace expansion (`/etc/{lunchroom,x}/f`, `f{1..3}`), which a shell makes before it opens a
//! path and which the reader of a command cannot read as a word (K2): the words it makes are
//! judged as paths of their own.

/// The most words one expansion makes; a word that would make more is read as far as this.
const MAX_WORDS: usize = 256;
/// The deepest nesting of braces read.
const MAX_DEPTH: usize = 6;
/// The longest run `{a..z}` or `{1..100}` read.
const MAX_RUN: i64 = 128;

/// The words a shell makes of `word` by brace expansion: `word` itself when it has nothing to
/// expand. Quotes are not read, so a quoted brace is expanded as well, which only judges more.
#[must_use]
pub fn expand(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    expand_into(word, 0, &mut out);
    if out.is_empty() {
        out.push(word.to_owned());
    }
    out
}

fn expand_into(word: &str, depth: usize, out: &mut Vec<String>) {
    let Some((open, close)) = first_group(word) else {
        out.push(word.to_owned());
        return;
    };
    let (head, body, tail) = (&word[..open], &word[open + 1..close], &word[close + 1..]);
    let alternatives = alternatives(body);
    if depth >= MAX_DEPTH || alternatives.is_none() {
        // Not a group the shell expands (`{x}`, `{}`): its braces stay, and the rest is read.
        let mut rest = Vec::new();
        expand_into(tail, depth + 1, &mut rest);
        for r in rest {
            out.push(format!("{head}{{{body}}}{r}"));
            if out.len() >= MAX_WORDS {
                return;
            }
        }
        return;
    }
    for alt in alternatives.unwrap_or_default() {
        let mut inner = Vec::new();
        expand_into(&format!("{head}{alt}{tail}"), depth + 1, &mut inner);
        for w in inner {
            out.push(w);
            if out.len() >= MAX_WORDS {
                return;
            }
        }
    }
}

/// The byte positions of the first `{` that has a matching `}`.
fn first_group(word: &str) -> Option<(usize, usize)> {
    let bytes = word.as_bytes();
    let mut k = 0;
    while k < bytes.len() {
        if bytes[k] == b'\\' {
            k += 2;
            continue;
        }
        if bytes[k] == b'{' {
            let mut depth = 0usize;
            let mut j = k;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 1,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((k, j));
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            return None;
        }
        k += 1;
    }
    None
}

/// What a group's body expands to: its top-level comma parts, or a numeric or letter run; `None`
/// for a body with neither (the shell leaves such a group as it is).
fn alternatives(body: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    let bytes = body.as_bytes();
    let mut k = 0;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => k += 1,
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(body[start..k].to_owned());
                start = k + 1;
            }
            _ => {}
        }
        k += 1;
    }
    if !parts.is_empty() {
        parts.push(body[start..].to_owned());
        return Some(parts);
    }
    run(body)
}

/// `1..5`, `5..1`, `a..e`: the run, up to [`MAX_RUN`] long.
fn run(body: &str) -> Option<Vec<String>> {
    let (from, to) = body.split_once("..")?;
    if let (Ok(a), Ok(b)) = (from.parse::<i64>(), to.parse::<i64>()) {
        let step = if a <= b { 1 } else { -1 };
        let count = (b - a).abs().min(MAX_RUN);
        return Some((0..=count).map(|n| (a + n * step).to_string()).collect());
    }
    let (mut f, mut t) = (from.chars(), to.chars());
    match (f.next(), f.next(), t.next(), t.next()) {
        (Some(a), None, Some(b), None) if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
            let (a, b) = (a as u8, b as u8);
            let list: Vec<u8> = if a <= b {
                (a..=b).collect()
            } else {
                (b..=a).rev().collect()
            };
            Some(
                list.into_iter()
                    .map(|c| char::from(c).to_string())
                    .collect(),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::expand;

    #[test]
    fn a_word_is_expanded_as_a_shell_expands_it() {
        let rows: [(&str, &[&str]); 12] = [
            ("plain", &["plain"]),
            ("a{b,c}d", &["abd", "acd"]),
            ("/etc/{lunchroom,x}/f", &["/etc/lunchroom/f", "/etc/x/f"]),
            ("f{1..3}", &["f1", "f2", "f3"]),
            ("f{3..1}", &["f3", "f2", "f1"]),
            ("{a..c}", &["a", "b", "c"]),
            ("{a,b}{1,2}", &["a1", "a2", "b1", "b2"]),
            ("{a,{b,c}}", &["a", "b", "c"]),
            ("{x}", &["{x}"]),
            ("{}", &["{}"]),
            ("a\\{b,c}", &["a\\{b,c}"]),
            ("{a,b", &["{a,b"]),
        ];
        let mut bad = Vec::new();
        for (word, want) in rows {
            let got = expand(word);
            if got != want {
                bad.push(format!("{word:?}: {got:?}, wanted {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        // A run too long is cut, and a nest too wide makes no more than the cap.
        assert!(expand("{1..100000}").len() <= 129);
        let wide = "{a,b}".repeat(12);
        assert!(expand(&wide).len() <= 256);
    }
}
