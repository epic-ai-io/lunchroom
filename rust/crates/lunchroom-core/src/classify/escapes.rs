//! Backslash escapes in text the guard reads as words but a program decodes (K1.8).
//!
//! Code a call runs (`python3 -c 'os.system("ls\nlunchroom approve 7")'`), the text of `$'...'`, of
//! `printf` and of `echo -e` carries line ends, tabs and even letters as escapes. The guard reads
//! such text by its words, so `\nlunchroom` looked like the word `nlunchroom` and `\x6cunchroom` like
//! `x6cunchroom`. Text that holds an escape is therefore read twice, as written and as decoded, and
//! a rule that fires on either one fires.

/// The text with its escapes replaced by the characters they stand for: `None` when it holds none.
///
/// Read: `\n \t \r \v \f \a \b \e \E`, octal (`\012`, `\0`), `\xHH`, `\uHHHH`, `\UHHHHHHHH`,
/// `\N{NAME}` (the line and space names; any other name stands for a space, a boundary) and
/// `\\ \' \"`. Any other backslash stays as written (`\c` is the shell's quoted `c`, already read
/// as such).
#[must_use]
pub fn decode(text: &str) -> Option<String> {
    if !text.contains('\\') {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&n) = chars.get(i + 1) else {
            out.push(c);
            break;
        };
        let simple = match n {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'v' => Some('\u{b}'),
            'f' => Some('\u{c}'),
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'e' | 'E' => Some('\u{1b}'),
            '\\' | '\'' | '"' => Some(n),
            _ => None,
        };
        if let Some(d) = simple {
            out.push(d);
            i += 2;
            continue;
        }
        let (radix, max, skip) = match n {
            '0'..='7' => (8, 3, 1),
            'x' => (16, 2, 2),
            'u' => (16, 4, 2),
            'U' => (16, 8, 2),
            _ => (0, 0, 0),
        };
        if radix != 0 {
            let digits: String = chars[i + skip..]
                .iter()
                .take(max)
                .take_while(|d| d.is_digit(radix))
                .collect();
            if !digits.is_empty() {
                let code = u32::from_str_radix(&digits, radix).unwrap_or(0xfffd);
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                i += skip + digits.len();
                continue;
            }
        }
        if n == 'N' && chars.get(i + 2) == Some(&'{') {
            let close = chars[i + 3..].iter().position(|d| *d == '}');
            if let Some(len) = close {
                let name: String = chars[i + 3..i + 3 + len].iter().collect();
                out.push(named(&name));
                i += 4 + len;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    (out != text).then_some(out)
}

/// The text with the pieces of a string joined: `'pt' + 'race'`, `"pt" "race"` and `'pt'.'race'` are
/// one literal, and `chr(112) + chr(116)` is the characters it makes. `None` when nothing joins.
#[must_use]
pub fn fold(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    let list = text.contains(".join(");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some((ch, len)) = chr_call(&chars[i..]) {
            out.push(ch);
            i += len;
            // `+` or `.` between two `chr(…)` calls joins them.
            let mut j = i;
            while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                j += 1;
            }
            if matches!(chars.get(j), Some('+' | '.')) {
                let mut k = j + 1;
                while chars.get(k).is_some_and(|c| c.is_whitespace()) {
                    k += 1;
                }
                if chr_call(&chars[k.min(chars.len())..]).is_some() {
                    i = k;
                }
            }
            continue;
        }
        match quote {
            Some(_) if c == '\\' => {
                out.push(c);
                if let Some(n) = chars.get(i + 1) {
                    out.push(*n);
                }
                i += 2;
                continue;
            }
            Some(q) if c == q => {
                // A closing quote, then an optional `+` or `.`, then the same quote opening:
                // one literal.
                let mut j = i + 1;
                while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                // A comma joins as well in `''.join(['pt', 'race'])`.
                if matches!(chars.get(j), Some('+' | '.')) || (list && chars.get(j) == Some(&',')) {
                    j += 1;
                    while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                        j += 1;
                    }
                }
                if chars.get(j) == Some(&q) {
                    i = j + 1;
                    continue;
                }
                quote = None;
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    (out != text).then_some(out)
}

/// `chr(NNN)` at the start of `chars`: the character and the length of the call.
fn chr_call(chars: &[char]) -> Option<(char, usize)> {
    let rest: String = chars.iter().take(16).collect();
    let inner = rest.strip_prefix("chr(")?;
    let digits: String = inner.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !inner[digits.len()..].starts_with(')') {
        return None;
    }
    let ch = char::from_u32(digits.parse().ok()?)?;
    Some((ch, "chr(".len() + digits.len() + 1))
}

/// The character a `\N{NAME}` escape stands for: the line and tab names it names, else a space.
fn named(name: &str) -> char {
    match name.to_ascii_uppercase().as_str() {
        "LINE FEED" | "NEW LINE" | "LF" | "NEXT LINE" | "LINE SEPARATOR" => '\n',
        "CARRIAGE RETURN" | "CR" => '\r',
        "CHARACTER TABULATION" | "TAB" | "HORIZONTAL TABULATION" => '\t',
        "LINE TABULATION" | "VERTICAL TABULATION" => '\u{b}',
        "FORM FEED" | "FF" => '\u{c}',
        _ => ' ',
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, fold};

    #[test]
    fn the_pieces_of_a_string_are_joined() {
        let rows: [(&str, Option<&str>); 11] = [
            ("plain", None),
            ("'a' 'b'", Some("'ab'")),
            ("'a' + 'b'", Some("'ab'")),
            ("'a'.'b'", Some("'ab'")),
            ("\"a\" . \"b\"", Some("\"ab\"")),
            ("'a', 'b'", None),
            ("'a' + \"b\"", None),
            ("chr(112)", Some("p")),
            ("chr(112) + chr(116)", Some("pt")),
            ("chr(112).chr(116).chr(114)", Some("ptr")),
            ("chr(x)", None),
        ];
        let mut bad = Vec::new();
        for (text, want) in rows {
            let got = fold(text);
            if got.as_deref() != want {
                bad.push(format!("{text:?}: {got:?}, wanted {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn escapes_are_decoded_and_plain_text_is_not() {
        let rows: [(&str, Option<&str>); 22] = [
            ("ls", None),
            ("a\\", None),
            ("a\\c", None),
            ("a\\nb", Some("a\nb")),
            ("a\\tb", Some("a\tb")),
            ("a\\rb", Some("a\rb")),
            ("a\\vb", Some("a\u{b}b")),
            ("a\\fb", Some("a\u{c}b")),
            ("a\\x0ab", Some("a\nb")),
            ("a\\x6cb", Some("alb")),
            ("a\\u000ab", Some("a\nb")),
            ("a\\U0000000ab", Some("a\nb")),
            ("a\\012b", Some("a\nb")),
            ("a\\0b", Some("a\0b")),
            ("a\\N{LINE FEED}b", Some("a\nb")),
            ("a\\N{no such name}b", Some("a b")),
            ("a\\\\nb", Some("a\\nb")),
            ("a\\'b", Some("a'b")),
            ("a\\xzzb", None),
            ("a\\ud800b", Some("a\u{fffd}b")),
            ("\\x", None),
            ("a\\N{LINE FEED", None),
        ];
        let mut bad = Vec::new();
        for (text, want) in rows {
            let got = decode(text);
            if got.as_deref() != want {
                bad.push(format!("{text:?}: {got:?}, wanted {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
