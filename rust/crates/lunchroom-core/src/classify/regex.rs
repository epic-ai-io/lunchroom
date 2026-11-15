//! A small extended regular expression matcher, for the pattern of `pkill` and `killall -r`
//! (spec §17 item 5: a kill naming the console's process is DENIED).
//!
//! The question it answers is whether a pattern can match the name or command line of the console,
//! so the guard needs a real match, not a search for a word: `lunchr[o]om`, `l.nchroom` and
//! `lun(ch|x)room` name it without spelling it. The syntax is that of `pgrep` (POSIX extended
//! expressions): literals, `.`, bracket sets with ranges and classes, groups, `|`, `*`, `+`, `?`,
//! `{m,n}`, `^`, `$` and backslash escapes. A pattern outside it (a word boundary, a back
//! reference) is `None`: the caller treats it as a match. So is a pattern or a text that needs more
//! than a fixed number of steps or nested attempts: the match is bounded in time and in stack.

use std::cell::Cell;

/// One unit of a pattern.
enum Atom {
    Lit(char),
    Any,
    /// Ranges, and whether the set is negated.
    Set(Vec<(char, char)>, bool),
    /// Alternatives, each a sequence.
    Group(Vec<Vec<Item>>),
    Bol,
    Eol,
}

/// An atom with how many times it may repeat.
struct Item {
    atom: Atom,
    min: usize,
    max: usize,
}

/// The pattern's alternatives, or `None` where its syntax is outside this module's.
fn parse(cs: &[char], i: &mut usize, depth: usize) -> Option<Vec<Vec<Item>>> {
    if depth > MAX_GROUPS {
        return None;
    }
    let mut branches: Vec<Vec<Item>> = vec![Vec::new()];
    while *i < cs.len() {
        let c = cs[*i];
        *i += 1;
        let atom = match c {
            '|' => {
                branches.push(Vec::new());
                continue;
            }
            ')' => {
                if depth == 0 {
                    return None;
                }
                return Some(branches);
            }
            '(' => Atom::Group(parse(cs, i, depth + 1)?),
            '[' => parse_set(cs, i)?,
            '.' => Atom::Any,
            '^' => Atom::Bol,
            '$' => Atom::Eol,
            '\\' => {
                let e = *cs.get(*i)?;
                *i += 1;
                match e {
                    'w' => Atom::Set(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], false),
                    'W' => Atom::Set(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], true),
                    'd' => Atom::Set(vec![('0', '9')], false),
                    'D' => Atom::Set(vec![('0', '9')], true),
                    's' => Atom::Set(vec![(' ', ' '), ('\t', '\t'), ('\n', '\n')], false),
                    'S' => Atom::Set(vec![(' ', ' '), ('\t', '\t'), ('\n', '\n')], true),
                    '<' | '>' | 'b' | 'B' | '1'..='9' => return None,
                    other => Atom::Lit(other),
                }
            }
            '*' | '+' | '?' | '{' => return None,
            other => Atom::Lit(other),
        };
        let (mut min, mut max) = (1, 1);
        match cs.get(*i) {
            Some('*') => (min, max, *i) = (0, usize::MAX, *i + 1),
            Some('+') => (min, max, *i) = (1, usize::MAX, *i + 1),
            Some('?') => (min, max, *i) = (0, 1, *i + 1),
            Some('{') => {
                let close = cs[*i..].iter().position(|c| *c == '}')? + *i;
                let body: String = cs[*i + 1..close].iter().collect();
                let (lo, hi) = match body.split_once(',') {
                    None => {
                        let n = body.parse().ok()?;
                        (n, n)
                    }
                    Some((lo, "")) => (lo.parse().ok()?, usize::MAX),
                    Some((lo, hi)) => (lo.parse().ok()?, hi.parse().ok()?),
                };
                (min, max, *i) = (lo, hi, close + 1);
            }
            _ => {}
        }
        // A repeat of a position (`^*`) is not a pattern this module follows.
        if matches!(atom, Atom::Bol | Atom::Eol) && (min, max) != (1, 1) {
            return None;
        }
        branches.last_mut()?.push(Item { atom, min, max });
    }
    (depth == 0).then_some(branches)
}

/// A bracket set; `*i` is just after the `[`.
fn parse_set(cs: &[char], i: &mut usize) -> Option<Atom> {
    let negated = cs.get(*i) == Some(&'^');
    *i += usize::from(negated);
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let c = *cs.get(*i)?;
        *i += 1;
        if c == ']' && !first {
            return Some(Atom::Set(ranges, negated));
        }
        first = false;
        if c == '[' && cs.get(*i) == Some(&':') {
            let close = cs[*i..].windows(2).position(|w| w == [':', ']'])? + *i;
            let name: String = cs[*i + 1..close].iter().collect();
            *i = close + 2;
            match name.as_str() {
                "alpha" => ranges.extend([('a', 'z'), ('A', 'Z')]),
                "digit" => ranges.push(('0', '9')),
                "alnum" => ranges.extend([('a', 'z'), ('A', 'Z'), ('0', '9')]),
                "upper" => ranges.push(('A', 'Z')),
                "lower" => ranges.push(('a', 'z')),
                "space" => ranges.extend([(' ', ' '), ('\t', '\r')]),
                "punct" => ranges.extend([('!', '/'), (':', '@'), ('[', '`'), ('{', '~')]),
                "xdigit" => ranges.extend([('0', '9'), ('a', 'f'), ('A', 'F')]),
                _ => return None,
            }
            continue;
        }
        if cs.get(*i) == Some(&'-') && cs.get(*i + 1).is_some_and(|n| *n != ']') {
            let hi = cs[*i + 1];
            *i += 2;
            ranges.push((c, hi));
        } else {
            ranges.push((c, c));
        }
    }
}

/// What a match may spend: steps (every atom tried) and depth (nested attempts). A pattern or a text
/// that needs more is not decided here, and the caller treats it as a match.
const MAX_STEPS: usize = 1_000_000;
const MAX_DEPTH: usize = 200;
/// The longest pattern read, and the deepest nesting of groups in it.
const MAX_PATTERN: usize = 4096;
const MAX_GROUPS: usize = 64;

/// The budget of one search.
struct Cx {
    steps: Cell<usize>,
    depth: Cell<usize>,
    over: Cell<bool>,
}

impl Cx {
    /// One more step, one level deeper; `false` once either budget is spent.
    fn enter(&self) -> bool {
        if self.over.get() {
            return false;
        }
        let (s, d) = (self.steps.get() + 1, self.depth.get() + 1);
        if s > MAX_STEPS || d > MAX_DEPTH {
            self.over.set(true);
            return false;
        }
        self.steps.set(s);
        self.depth.set(d);
        true
    }

    fn leave(&self) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }

    fn tick(&self) -> bool {
        let s = self.steps.get() + 1;
        if self.over.get() || s > MAX_STEPS {
            self.over.set(true);
            return false;
        }
        self.steps.set(s);
        true
    }
}

/// Whether `atom` matches at `pos`, handing each end position to `k`.
fn atom(a: &Atom, t: &[char], pos: usize, cx: &Cx, k: &mut dyn FnMut(usize) -> bool) -> bool {
    if !cx.tick() {
        return false;
    }
    match a {
        Atom::Lit(c) => t.get(pos) == Some(c) && k(pos + 1),
        Atom::Any => pos < t.len() && k(pos + 1),
        Atom::Set(ranges, negated) => {
            t.get(pos)
                .is_some_and(|c| ranges.iter().any(|(lo, hi)| lo <= c && c <= hi) != *negated)
                && k(pos + 1)
        }
        Atom::Group(branches) => branches.iter().any(|b| seq(b, 0, t, pos, cx, k)),
        Atom::Bol => pos == 0 && k(pos),
        Atom::Eol => pos == t.len() && k(pos),
    }
}

/// Whether the sequence from `idx` matches at `pos`, handing the end position to `k`.
fn seq(
    items: &[Item],
    idx: usize,
    t: &[char],
    pos: usize,
    cx: &Cx,
    k: &mut dyn FnMut(usize) -> bool,
) -> bool {
    let Some(it) = items.get(idx) else {
        return k(pos);
    };
    repeat(items, idx, it, 0, t, pos, cx, k)
}

#[allow(clippy::too_many_arguments)] // the state of one match, passed down
fn repeat(
    items: &[Item],
    idx: usize,
    it: &Item,
    count: usize,
    t: &[char],
    pos: usize,
    cx: &Cx,
    k: &mut dyn FnMut(usize) -> bool,
) -> bool {
    if !cx.enter() {
        return false;
    }
    // One more, as many as there may be first.
    let more = count < it.max
        && atom(&it.atom, t, pos, cx, &mut |p| {
            // A repeat that consumed nothing cannot go on: it would not end.
            (p != pos || count < it.min) && repeat(items, idx, it, count + 1, t, p, cx, k)
        });
    let found = more || (count >= it.min && seq(items, idx + 1, t, pos, cx, k));
    cx.leave();
    found
}

/// Whether `pattern`, searched for anywhere in `text`, matches; `None` for a pattern outside the
/// syntax this module follows, and for one (or a text) that needs more than the match may spend:
/// a pattern of more than [`MAX_PATTERN`] characters, groups nested more than [`MAX_GROUPS`] deep,
/// more than [`MAX_STEPS`] steps or [`MAX_DEPTH`] nested attempts.
#[must_use]
pub fn search(pattern: &str, text: &str) -> Option<bool> {
    let cs: Vec<char> = pattern.chars().collect();
    if cs.len() > MAX_PATTERN {
        return None;
    }
    let mut i = 0;
    let branches = parse(&cs, &mut i, 0)?;
    let t: Vec<char> = text.chars().collect();
    let cx = Cx {
        steps: Cell::new(0),
        depth: Cell::new(0),
        over: Cell::new(false),
    };
    let found = (0..=t.len()).any(|start| {
        !cx.over.get()
            && branches
                .iter()
                .any(|b| seq(b, 0, &t, start, &cx, &mut |_| true))
    });
    (!cx.over.get()).then_some(found)
}

#[cfg(test)]
mod tests {
    use super::search;

    #[test]
    fn patterns_match_as_pgrep_reads_them() {
        let yes = |p: &str, t: &str| assert_eq!(search(p, t), Some(true), "{p} in {t}");
        let no = |p: &str, t: &str| assert_eq!(search(p, t), Some(false), "{p} in {t}");
        yes("lunch", "lunchroom");
        yes("lunchroom", "lunchroom");
        yes("lunchr[o]om", "lunchroom");
        yes("l.nchroom", "lunchroom");
        yes("lunch.*", "lunchroom");
        yes("l.*m$", "lunchroom");
        yes("^lunch", "lunchroom");
        yes("lunchro+m", "lunchroom");
        yes("lunchroo?m", "lunchroom");
        yes("(lunch)room", "lunchroom");
        yes("lun(ch|x)room", "lunchroom");
        yes("lun[a-z]*oom", "lunchroom");
        yes("lun[[:alpha:]]+room", "lunchroom");
        yes("lunch\\w+", "lunchroom");
        yes("lunch.{4}$", "lunchroom");
        yes("lunch.{2,}", "lunchroom");
        yes("l\\.*unchroom", "lunchroom");
        yes(
            "^/usr/local/lib/lunchroom/lunch",
            "/usr/local/lib/lunchroom/lunchroom console",
        );
        yes("(a|b)*lunch", "lunchroom");
        yes("x*", "lunchroom");
        no("lunchroomx", "lunchroom");
        no("^lunch", "xlunchroom");
        no("room$", "roomx");
        no("^room", "lunchroom");
        no("my-lunchroom-app", "lunchroom");
        no("^node", "lunchroom");
        no("l[u]nx", "lunchroom");
        no("python.*server", "lunchroom");
        no("run.*job", "lunchroom console");
        no("^lunchroom$", "lunchroom console");
        no("lunch.{5}$", "lunchroom");
        no("[^l]unchroom", "lunchroom");
        no("lun(ch|x)rom$", "lunchroom");
        no("lunch[0-9]", "lunchroom");
        no("\\dunchroom", "lunchroom x");
        yes("\\Dunchroom", "lunchroom x");
    }

    // The match is bounded in time and stack, and a pattern or a text that needs more is
    // not decided (None), which the caller counts as a match.
    #[test]
    fn a_match_is_bounded_in_time_and_in_stack() {
        let started = std::time::Instant::now();
        let text = "a".repeat(64 * 1024);
        // A pathological pattern: exponential backtracking on a text that cannot match.
        assert_eq!(search("(a*)*b", &text), None);
        assert_eq!(search("(a|aa)+$b", &text), None);
        assert_eq!(
            search("(a*)*b", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None
        );
        // A repeat as long as the text would nest as deep.
        assert_eq!(search("a*b", &text), None);
        assert_eq!(search(".*.*.*.*x", &text), None);
        // Nesting too deep to parse, and a pattern too long to read.
        let deep = format!("{}a{}", "(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(search(&deep, "a"), None);
        let nested = format!("{}a{}", "(".repeat(65), ")".repeat(65));
        assert_eq!(search(&nested, "a"), None);
        let long = "a".repeat(5_000);
        assert_eq!(search(&long, "a"), None);
        // Within the limits it still decides.
        let ok = format!("{}a{}", "(".repeat(60), ")".repeat(60));
        assert_eq!(search(&ok, "a"), Some(true));
        assert_eq!(search("a*b", "aaaaaaaaaaaaaaaaaaaaab"), Some(true));
        assert_eq!(search("(a*)*b", "aaab"), Some(true));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn syntax_outside_the_matcher_is_none() {
        for p in [
            "(lunch",
            "lunch)",
            "*lunch",
            "lunch\\<",
            "(a)\\1",
            "[abc",
            "lunch{x}",
            "a{2",
            "^*",
            "[[:bogus:]]",
        ] {
            assert_eq!(search(p, "lunchroom"), None, "{p}");
        }
    }
}
