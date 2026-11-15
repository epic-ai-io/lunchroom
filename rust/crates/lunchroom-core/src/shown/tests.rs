//! C3: how agent text is shown (§17.10) and how a DENIED call is kept (§17 item 4).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)] // test code

use super::*;
use serde_json::json;

// §17.10: every class the clause names is replaced by its code; the code of a backslash is `\u{5c}`.
#[test]
fn each_class_of_the_clause_is_a_visible_mark() {
    let samples: [(char, &str); 22] = [
        ('\u{1b}', "\\u{1b}"),       // control
        ('\n', "\\u{a}"),            // control: line feed
        ('\r', "\\u{d}"),            // control: carriage return
        ('\t', "\\u{9}"),            // control: tab
        ('\u{0}', "\\u{0}"),         // NUL
        ('\u{7f}', "\\u{7f}"),       // DEL
        ('\u{85}', "\\u{85}"),       // C1 control
        ('\u{202e}', "\\u{202e}"),   // direction override
        ('\u{200f}', "\\u{200f}"),   // direction mark
        ('\u{2067}', "\\u{2067}"),   // isolate
        ('\u{200b}', "\\u{200b}"),   // zero width space
        ('\u{200d}', "\\u{200d}"),   // zero width joiner
        ('\u{feff}', "\\u{feff}"),   // format: byte order mark
        ('\u{ad}', "\\u{ad}"),       // format: soft hyphen
        ('\u{2028}', "\\u{2028}"),   // line separator
        ('\u{2029}', "\\u{2029}"),   // paragraph separator
        ('\u{a0}', "\\u{a0}"),       // no-break space
        ('\u{3000}', "\\u{3000}"),   // ideographic space
        ('\u{2003}', "\\u{2003}"),   // em space
        ('\u{202f}', "\\u{202f}"),   // narrow no-break space
        ('\\', "\\u{5c}"),           // backslash
        ('\u{e0041}', "\\u{e0041}"), // tag character
    ];
    for (c, want) in samples {
        assert!(is_mark(c), "{:x}", c as u32);
        assert_eq!(
            shown(&format!("a{c}b")),
            format!("a{want}b"),
            "{:x}",
            c as u32
        );
    }
}

// §17.10: the ordinary space and ordinary text are left as they are.
#[test]
fn the_ordinary_space_and_text_are_not_marked() {
    assert_eq!(shown("ls -la /tmp é 漢字 😀"), "ls -la /tmp é 漢字 😀");
    assert!(!is_mark(' '));
    assert_eq!(shown(""), "");
}

// §17.10: text an agent typed cannot look like a mark: a typed `\u{1b}` and an escape differ.
#[test]
fn typed_mark_text_differs_from_a_real_mark() {
    let typed = shown("\\u{1b}");
    let real = shown("\u{1b}");
    assert_eq!(real, "\\u{1b}");
    assert_eq!(typed, "\\u{5c}u{1b}");
    assert_ne!(typed, real);
}

// §17 item 4: a call of exactly 64 KiB is whole, one byte more is cut with the mark `cut`.
#[test]
fn the_kept_call_is_cut_at_64_kib_exactly() {
    let exact = "a".repeat(CALL_KEPT_BYTES);
    assert_eq!(kept_call(&exact), exact);
    let over = "a".repeat(CALL_KEPT_BYTES + 1);
    let kept = kept_call(&over);
    assert_eq!(kept.len(), CALL_KEPT_BYTES + CUT_MARK.len());
    assert!(kept.ends_with("cut"));
    assert!(kept.starts_with(&exact));
    let under = "a".repeat(CALL_KEPT_BYTES - 1);
    assert_eq!(kept_call(&under), under);
}

// §17 item 4: the cut never lands inside a mark or a character.
#[test]
fn the_cut_never_splits_a_mark_or_a_character() {
    // A mark is 6 bytes (`\u{1b}`): fill so that the next one would pass the limit.
    let text = format!("{}\u{1b}tail", "a".repeat(CALL_KEPT_BYTES - 3));
    let kept = kept_call(&text);
    assert_eq!(
        kept,
        format!("{}{CUT_MARK}", "a".repeat(CALL_KEPT_BYTES - 3))
    );
    // A 3-byte character at the limit.
    let text = format!("{}漢", "a".repeat(CALL_KEPT_BYTES - 2));
    let kept = kept_call(&text);
    assert!(kept.ends_with(CUT_MARK));
    assert!(kept.is_char_boundary(kept.len() - CUT_MARK.len()));
    assert_eq!(kept.len(), CALL_KEPT_BYTES - 2 + CUT_MARK.len());
    // A mark that fits exactly is kept, not cut.
    let text = format!("{}\u{1b}", "a".repeat(CALL_KEPT_BYTES - 6));
    let kept = kept_call(&text);
    assert_eq!(kept.len(), CALL_KEPT_BYTES);
    assert!(kept.ends_with("\\u{1b}"));
}

// §17 item 4: the secrets are replaced before the cut, so a secret across the limit is not left in
// part.
#[test]
fn a_secret_across_the_cut_is_replaced_before_cutting() {
    let secret = format!("ghp_{}", "a".repeat(36));
    let pad = "b ".repeat((CALL_KEPT_BYTES - 20) / 2);
    let text = format!("{pad}{secret} and more");
    let kept = kept_call(&text);
    assert!(
        !kept.contains("ghp_"),
        "{}",
        &kept[kept.len().saturating_sub(80)..]
    );
    assert!(!kept.contains("aaaa"));
    assert!(kept.contains(secrets::REMOVED));
    // A secret wholly before the limit is replaced and the call is whole.
    let kept = kept_call(&format!("curl -H {secret} https://example.com"));
    assert_eq!(
        kept,
        format!("curl -H {} https://example.com", secrets::REMOVED)
    );
}

// §17 item 4: a control character or direction mark at the cut shows as its whole mark or as the
// cut, never as the raw character.
#[test]
fn a_control_character_at_the_cut_is_never_raw() {
    for c in ['\u{1b}', '\u{202e}', '\n'] {
        for back in 0..8 {
            let text = format!("{}{c}{c}{c}", "a".repeat(CALL_KEPT_BYTES - back));
            let kept = kept_call(&text);
            assert!(!kept.contains(c), "{:x} back {back}", c as u32);
            assert!(kept.len() <= CALL_KEPT_BYTES + CUT_MARK.len());
        }
    }
}

// §17.10 "Marked parts": a shell command is shown as its command line; any other call as its
// canonical JSON, with sorted keys and no spaces; a call with no command is the JSON.
#[test]
fn the_call_text_is_the_command_line_or_the_canonical_json() {
    assert_eq!(call_text("Bash", &json!({"command": "ls -la"})), "ls -la");
    assert_eq!(
        call_text("Write", &json!({"path": "/a", "content": "x"})),
        r#"{"content":"x","path":"/a"}"#
    );
    assert_eq!(call_text("Bash", &json!({})), "{}");
    assert_eq!(
        call_text("Bash", &json!({"command": 5})),
        r#"{"command":5}"#
    );
    assert_eq!(call_text("Mystery", &json!(null)), "null");
    assert_eq!(
        call_text("Mystery", &json!({"b": [1, {"z": 1, "a": 2}], "a": ""})),
        r#"{"a":"","b":[1,{"a":2,"z":1}]}"#
    );
    // The text that A2.1 hashes is the one shown, byte for byte.
    let v = json!({"k": "é\n"});
    assert_eq!(call_text("T", &v), crate::jcs::canonical(&v));
}

// ---- C3 (Codex's list) ----

// C3-040, C3-058: every C0 control, DEL and C1 control is a lowercase mark.
#[test]
fn c3_c0_controls_drawn_as_marks() {
    for c in (0u32..=0x1f).chain([0x7f]).chain(0x80..=0x9f) {
        let ch = char::from_u32(c).unwrap();
        assert_eq!(shown(&ch.to_string()), format!("\\u{{{c:x}}}"), "{c:x}");
    }
}

// C3-041: every direction mark, override and isolate.
#[test]
fn c3_direction_marks_drawn_as_marks() {
    for c in [
        0x61c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068,
        0x2069,
    ] {
        let ch = char::from_u32(c).unwrap();
        assert_eq!(shown(&ch.to_string()), format!("\\u{{{c:x}}}"));
    }
}

// C3-044: spaces other than the ordinary one: all of U+2000..U+200A, NBSP, narrow, medium, ideographic.
#[test]
fn c3_non_ordinary_spaces_are_marks() {
    for c in (0x2000u32..=0x200a).chain([0xa0, 0x1680, 0x202f, 0x205f, 0x3000]) {
        assert!(is_mark(char::from_u32(c).unwrap()), "{c:x}");
    }
}

// C3-053: an emoji ZWJ sequence keeps its emoji and marks the joiner.
#[test]
fn c3_zwj_sequence_marks_the_joiner() {
    assert_eq!(shown("👩\u{200d}💻"), "👩\\u{200d}💻");
}

// C3-070: a call cut inside a run of marks never leaves part of one.
#[test]
fn c3_cut_inside_a_run_of_marks_leaves_whole_marks() {
    let text = "\u{1b}".repeat(CALL_KEPT_BYTES);
    let kept = kept_call(&text);
    assert!(kept.ends_with(CUT_MARK));
    let body = &kept[..kept.len() - CUT_MARK.len()];
    assert!(body.len() <= CALL_KEPT_BYTES);
    assert_eq!(body.len() % "\\u{1b}".len(), 0);
    assert!(
        body.as_bytes()
            .chunks("\\u{1b}".len())
            .all(|c| c == "\\u{1b}".as_bytes())
    );
}

// §17.10: every format character (Unicode category Cf), every separator (Zl, Zp) and every space
// but the ordinary one (Zs) is a mark. The table is Unicode 15.0's, generated with Python's
// `unicodedata` (category in {Cf, Zs, Zl, Zp}, U+0020 left out), not typed by hand.
#[test]
fn every_format_separator_and_odd_space_of_unicode_15_is_a_mark() {
    const UNICODE_15: [(u32, u32); 24] = [
        (0x00A0, 0x00A0),
        (0x00AD, 0x00AD),
        (0x0600, 0x0605),
        (0x061C, 0x061C),
        (0x06DD, 0x06DD),
        (0x070F, 0x070F),
        (0x0890, 0x0891),
        (0x08E2, 0x08E2),
        (0x1680, 0x1680),
        (0x180E, 0x180E),
        (0x2000, 0x200F),
        (0x2028, 0x202F),
        (0x205F, 0x2064),
        (0x2066, 0x206F),
        (0x3000, 0x3000),
        (0xFEFF, 0xFEFF),
        (0xFFF9, 0xFFFB),
        (0x110BD, 0x110BD),
        (0x110CD, 0x110CD),
        (0x13430, 0x1343F),
        (0x1BCA0, 0x1BCA3),
        (0x1D173, 0x1D17A),
        (0xE0001, 0xE0001),
        (0xE0020, 0xE007F),
    ];
    for (first, last) in UNICODE_15 {
        for cp in first..=last {
            let c = char::from_u32(cp).unwrap();
            assert!(is_mark(c), "U+{cp:04X} is not a mark");
            assert_eq!(shown(&c.to_string()), format!("\\u{{{cp:x}}}"));
        }
    }
}
