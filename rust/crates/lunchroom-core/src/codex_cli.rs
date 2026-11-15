//! The options of the `codex` command before its first word (spec §6 P1.4, §10 D1.8, §13 H1.4).
//!
//! `§6` P1.4: "for `codex`, the guard passes over each option before the first word and,
//! for an option that takes a value, its value, as the option table of the Codex release its
//! adapter passed with defines them, so `codex -m <model> exec` is recognised, and an option that
//! table does not hold makes the command count as starting a session here and as a program's
//! prompt in D1.8".
//!
//! The release is Codex 0.163.0 (tag `rust-v0.163.0`, read from a shallow clone). The command line
//! of `codex` is the `MultitoolCli` struct, `codex-rs/cli/src/main.rs:129-144`, whose options are
//! the flattened [`CliConfigOverrides`](codex-rs/utils/cli/src/config_override.rs), `FeatureToggles`
//! (`main.rs:951-960`), `InteractiveRemoteOptions` (`main.rs:962-974`) and the interactive `Cli`
//! (`codex-rs/tui/src/cli.rs:9-93`), which flattens `SharedCliOptions`
//! (`codex-rs/utils/cli/src/shared_options.rs:9-77`). Codex parses with `clap` 4.5.58
//! (`codex-rs/Cargo.lock`), and the rules below for attached values, short-option clusters, `--`
//! and an option with several values are those of its `parser.rs`, cited at each rule. Codex sets
//! none of `infer_long_args`, `allow_hyphen_values` or `allow_negative_numbers` on these options
//! (searched in `main.rs`, `tui/src/cli.rs` and `utils/cli/src`), so a long option is matched by
//! its whole name only and a value that begins with `-` is not read as a value by clap; this parser
//! takes the next argument as the value regardless, which passes over the same words Codex would.
//!
//! H1.4 (§13 item 4) reads `-c`/`--config` options "as Codex does": "Codex trims spaces around a key
//! and splits at the first `=`" ([`config_override`]).

/// What takes the place of the first word of a `codex` command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstWord {
    /// The argument at this index of the given arguments is the first word: a subcommand, or the
    /// prompt.
    Word(usize),
    /// An option before the first word is not in Codex 0.163.0's table. P1.4: the command counts as
    /// starting a session, and as a program's prompt in D1.8.
    UnknownOption,
    /// The arguments end before any word (only options, or an option lacking its value).
    None,
}

/// One option read from before the first word, by its long name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt<'a> {
    /// The option's long name without the dashes, as the table gives it (`config` for `-c`).
    pub name: &'static str,
    /// Its values as written, an attached value without its `=`; empty for an option with none.
    pub values: Vec<&'a str>,
    /// The index of the argument that holds the option itself.
    pub index: usize,
}

/// The result of reading the arguments after the `codex` word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed<'a> {
    /// The options before the first word, in order, up to an unknown one (which is not listed).
    pub options: Vec<Opt<'a>>,
    /// The first word, or why there is none.
    pub first: FirstWord,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Takes no value.
    Flag,
    /// Takes one value: attached, or the next argument.
    One,
    /// Takes one or more values (`num_args = 1..`): an attached value is the only one; otherwise
    /// every following argument up to the next option or `--` is a value.
    Many,
}

struct Row {
    long: &'static str,
    /// Other long names, `alias = ` in the source.
    aliases: &'static [&'static str],
    short: Option<char>,
    kind: Kind,
}

const fn row(
    long: &'static str,
    aliases: &'static [&'static str],
    short: Option<char>,
    kind: Kind,
) -> Row {
    Row {
        long,
        aliases,
        short,
        kind,
    }
}

/// The option table of Codex 0.163.0. Source of each row (paths under `codex-rs/`, from the clone
/// of tag `rust-v0.163.0`):
///
/// - `config`, `-c`, one value: `utils/cli/src/config_override.rs:24-34` (`short = 'c'`,
///   `long = "config"`, `action = ArgAction::Append`, `global = true`), flattened at
///   `cli/src/main.rs:130-131`.
/// - `enable`, one value: `cli/src/main.rs:954`; `disable`, one value: `cli/src/main.rs:958`
///   (both `action = Append`, `global = true`), flattened at `main.rs:133-134`.
/// - `remote`, one value: `cli/src/main.rs:967`; `remote-auth-token-env`, one value:
///   `cli/src/main.rs:972`; flattened at `main.rs:136-137`.
/// - `strict-config`, flag: `tui/src/cli.rs:21`.
/// - `ask-for-approval`, `-a`, one value: `tui/src/cli.rs:74`.
/// - `search`, flag: `tui/src/cli.rs:78`.
/// - `no-alt-screen`, flag: `tui/src/cli.rs:84`.
/// - `no-daemon`, flag: `tui/src/cli.rs:88` (`#[arg(long)]`).
/// - `image`, `-i`, one or more values: `utils/cli/src/shared_options.rs:12-19`
///   (`num_args = 1..`, `value_delimiter = ','`).
/// - `model`, `-m`, one value: `shared_options.rs:22`.
/// - `oss`, flag: `shared_options.rs:26`.
/// - `local-provider`, one value: `shared_options.rs:31`.
/// - `profile`, `-p`, one value: `shared_options.rs:35`.
/// - `sandbox`, `-s`, one value: `shared_options.rs:40`.
/// - `approve-for-me` (alias `not-so-yolo`), flag: `shared_options.rs:44-50`.
/// - `dangerously-bypass-approvals-and-sandbox` (alias `yolo`), flag: `shared_options.rs:54-59`.
/// - `dangerously-bypass-hook-trust`, flag: `shared_options.rs:63`.
/// - `cd`, `-C`, one value: `shared_options.rs:67`.
/// - `worktree`, flag: `shared_options.rs:71`.
/// - `add-dir`, one value per occurrence: `shared_options.rs:75`.
/// - `help` (`-h`) and `version` (`-V`), flags: added by `clap` for the `author, version` command
///   (`cli/src/main.rs:118-120`; `tui/src/cli.rs:10` `#[command(version)]`).
const TABLE: &[Row] = &[
    row("config", &[], Some('c'), Kind::One),
    row("enable", &[], None, Kind::One),
    row("disable", &[], None, Kind::One),
    row("remote", &[], None, Kind::One),
    row("remote-auth-token-env", &[], None, Kind::One),
    row("strict-config", &[], None, Kind::Flag),
    row("ask-for-approval", &[], Some('a'), Kind::One),
    row("search", &[], None, Kind::Flag),
    row("no-alt-screen", &[], None, Kind::Flag),
    row("no-daemon", &[], None, Kind::Flag),
    row("image", &[], Some('i'), Kind::Many),
    row("model", &[], Some('m'), Kind::One),
    row("oss", &[], None, Kind::Flag),
    row("local-provider", &[], None, Kind::One),
    row("profile", &[], Some('p'), Kind::One),
    row("sandbox", &[], Some('s'), Kind::One),
    row("approve-for-me", &["not-so-yolo"], None, Kind::Flag),
    row(
        "dangerously-bypass-approvals-and-sandbox",
        &["yolo"],
        None,
        Kind::Flag,
    ),
    row("dangerously-bypass-hook-trust", &[], None, Kind::Flag),
    row("cd", &[], Some('C'), Kind::One),
    row("worktree", &[], None, Kind::Flag),
    row("add-dir", &[], None, Kind::One),
    row("help", &[], Some('h'), Kind::Flag),
    row("version", &[], Some('V'), Kind::Flag),
];

fn by_long(name: &str) -> Option<&'static Row> {
    TABLE
        .iter()
        .find(|r| r.long == name || r.aliases.contains(&name))
}

fn by_short(c: char) -> Option<&'static Row> {
    TABLE.iter().find(|r| r.short == Some(c))
}

/// An argument that clap reads as an option (it begins with `-` and is more than `-`, which is a
/// positional value).
fn is_option_like(a: &str) -> bool {
    a.starts_with('-') && a != "-"
}

/// What reading one option argument gave.
enum Step<'a> {
    /// The option (or the options of a cluster), and how many arguments they took.
    Took(Vec<Opt<'a>>, usize),
    /// The option is not in the table.
    Unknown,
    /// The option lacks its value: the arguments end.
    Missing(Vec<Opt<'a>>),
}

/// Reads `args[at]`, which begins with `--` and is not `--`.
fn long_option<'a>(args: &[&'a str], at: usize) -> Step<'a> {
    let long = args[at].strip_prefix("--").unwrap_or_default();
    // `--name` or `--name=value` (clap splits at the first `=`).
    let (name, attached) = match long.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (long, None),
    };
    let Some(row) = by_long(name) else {
        return Step::Unknown;
    };
    let one = |values: Vec<&'a str>| Opt {
        name: row.long,
        values,
        index: at,
    };
    match (row.kind, attached) {
        // A flag given a value is a clap error: Codex would not run it.
        (Kind::Flag, Some(_)) => Step::Unknown,
        (Kind::Flag, None) => Step::Took(vec![one(Vec::new())], 1),
        // clap_builder 4.5.58 `parser.rs:1060-1073` `parse_opt_value`: an attached value is
        // "Attached are always done", so it is the option's only value, whatever its `num_args`.
        (_, Some(value)) => Step::Took(vec![one(vec![value])], 1),
        (Kind::One, None) => match args.get(at + 1) {
            Some(value) => Step::Took(vec![one(vec![value])], 2),
            None => Step::Missing(vec![one(Vec::new())]),
        },
        (Kind::Many, None) => {
            let (values, next) = many_values(args, at + 1);
            Step::Took(vec![one(values)], next - at)
        }
    }
}

/// Reads `args[at]`, a cluster of short options (`-hm`, `-mgpt`, `-m=gpt`, `-m`), as `clap_builder`
/// 4.5.58 `parser.rs:932-987` `parse_short_arg` does: a flag letter is taken and the next letter
/// read; a letter that takes a value takes the rest of the argument as it (a leading `=` dropped,
/// `parser.rs:979`), or the next argument if nothing is left.
fn short_cluster<'a>(args: &[&'a str], at: usize) -> Step<'a> {
    let arg = args[at];
    let mut found = Vec::new();
    for (offset, letter) in arg.char_indices().skip(1) {
        let Some(row) = by_short(letter) else {
            return Step::Unknown;
        };
        let opt = |values: Vec<&'a str>| Opt {
            name: row.long,
            values,
            index: at,
        };
        if row.kind == Kind::Flag {
            found.push(opt(Vec::new()));
            continue;
        }
        let rest = &arg[offset + letter.len_utf8()..];
        if !rest.is_empty() {
            let value = rest.strip_prefix('=').unwrap_or(rest);
            found.push(opt(vec![value]));
            return Step::Took(found, 1);
        }
        if row.kind == Kind::One {
            return if let Some(value) = args.get(at + 1) {
                found.push(opt(vec![value]));
                Step::Took(found, 2)
            } else {
                found.push(opt(Vec::new()));
                Step::Missing(found)
            };
        }
        let (values, next) = many_values(args, at + 1);
        found.push(opt(values));
        return Step::Took(found, next - at);
    }
    Step::Took(found, 1)
}

/// Reads the arguments that follow the `codex` word, up to its first word.
#[must_use]
pub fn parse<'a>(args: &[&'a str]) -> Parsed<'a> {
    let mut options = Vec::new();
    let mut at = 0;
    while at < args.len() {
        let arg = args[at];
        // `--` ends the options; the argument after it is a positional.
        if arg == "--" {
            let first = if at + 1 < args.len() {
                FirstWord::Word(at + 1)
            } else {
                FirstWord::None
            };
            return Parsed { options, first };
        }
        if !is_option_like(arg) {
            return Parsed {
                options,
                first: FirstWord::Word(at),
            };
        }
        let step = if arg.starts_with("--") {
            long_option(args, at)
        } else {
            short_cluster(args, at)
        };
        match step {
            Step::Took(found, used) => {
                options.extend(found);
                at += used;
            }
            Step::Unknown => {
                return Parsed {
                    options,
                    first: FirstWord::UnknownOption,
                };
            }
            Step::Missing(found) => {
                options.extend(found);
                return Parsed {
                    options,
                    first: FirstWord::None,
                };
            }
        }
    }
    Parsed {
        options,
        first: FirstWord::None,
    }
}

/// The values of an option taking one or more: each argument from `from` up to the next option or
/// `--`. Returns them and the index after the last.
fn many_values<'a>(args: &[&'a str], from: usize) -> (Vec<&'a str>, usize) {
    let mut next = from;
    let mut values = Vec::new();
    while next < args.len() && !is_option_like(args[next]) {
        values.push(args[next]);
        next += 1;
    }
    (values, next)
}

/// The first word of a `codex` command line, passing over each option and its value as Codex
/// 0.163.0's table defines them (P1.4). `args` are the arguments after the `codex` word.
#[must_use]
pub fn first_word(args: &[&str]) -> FirstWord {
    parse(args).first
}

/// H1.4: "Codex trims spaces around a key and splits at the first `=`". Returns the key and the
/// value as Codex reads a `-c`/`--config` value: split at the first `=`, each half trimmed
/// (`codex-rs/utils/cli/src/config_override.rs`, `parse_overrides`: `splitn(2, '=')`, `k.trim()`,
/// `.trim()`). `None` where Codex rejects the override: no `=` ("Invalid override (missing '=')")
/// or an empty key ("Empty key in override").
#[must_use]
pub fn config_override(v: &str) -> Option<(String, String)> {
    let (k, val) = v.split_once('=')?;
    let key = k.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), val.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fw(args: &[&str]) -> FirstWord {
        first_word(args)
    }

    // Node §6::26 acceptance: `codex -m <model> exec "…"` is passed over; each option of the table,
    // with and without a value, is passed over.
    #[test]
    fn model_then_exec_is_exec() {
        assert_eq!(fw(&["-m", "gpt", "exec", "x"]), FirstWord::Word(2));
    }

    #[test]
    fn global_value_options_pass_over_their_value() {
        assert_eq!(fw(&["-c", "k=v", "e"]), FirstWord::Word(2));
        assert_eq!(fw(&["--profile", "p", "review"]), FirstWord::Word(2));
        for (long, short) in [
            ("config", Some("-c")),
            ("enable", None),
            ("disable", None),
            ("remote", None),
            ("remote-auth-token-env", None),
            ("ask-for-approval", Some("-a")),
            ("model", Some("-m")),
            ("local-provider", None),
            ("profile", Some("-p")),
            ("sandbox", Some("-s")),
            ("cd", Some("-C")),
            ("add-dir", None),
        ] {
            let l = format!("--{long}");
            assert_eq!(fw(&[&l, "VAL", "exec"]), FirstWord::Word(2), "{l}");
            let attached = format!("--{long}=VAL");
            assert_eq!(fw(&[&attached, "exec"]), FirstWord::Word(1), "{attached}");
            if let Some(s) = short {
                assert_eq!(fw(&[s, "VAL", "exec"]), FirstWord::Word(2), "{s}");
                let glued = format!("{s}VAL");
                assert_eq!(fw(&[&glued, "exec"]), FirstWord::Word(1), "{glued}");
                let eq = format!("{s}=VAL");
                assert_eq!(fw(&[&eq, "exec"]), FirstWord::Word(1), "{eq}");
            }
        }
    }

    #[test]
    fn flags_pass_over_no_value() {
        for f in [
            "--strict-config",
            "--search",
            "--no-alt-screen",
            "--no-daemon",
            "--oss",
            "--approve-for-me",
            "--not-so-yolo",
            "--dangerously-bypass-approvals-and-sandbox",
            "--yolo",
            "--dangerously-bypass-hook-trust",
            "--worktree",
            "--help",
            "-h",
            "--version",
            "-V",
        ] {
            assert_eq!(fw(&[f, "exec"]), FirstWord::Word(1), "{f}");
        }
        // `codex --search` then a prompt: the flag takes no value, the prompt is the word.
        assert_eq!(fw(&["--search", "fix the bug"]), FirstWord::Word(1));
    }

    #[test]
    fn flag_given_a_value_is_unknown() {
        assert_eq!(fw(&["--search=yes", "exec"]), FirstWord::UnknownOption);
    }

    // Node §6::27 and §6::28 acceptance: an option the table does not hold.
    #[test]
    fn unknown_option_is_reported() {
        assert_eq!(fw(&["--frobnicate", "exec"]), FirstWord::UnknownOption);
        assert_eq!(fw(&["--unknown", "x"]), FirstWord::UnknownOption);
        assert_eq!(fw(&["-z", "exec"]), FirstWord::UnknownOption);
        assert_eq!(fw(&["-"]), FirstWord::Word(0));
        // An unknown option after a known one with a value.
        assert_eq!(
            fw(&["-m", "gpt", "--nope", "exec"]),
            FirstWord::UnknownOption
        );
        // A prefix of a long name is not the name (no `infer_long_args`).
        assert_eq!(fw(&["--mod", "gpt", "exec"]), FirstWord::UnknownOption);
        assert_eq!(fw(&["--modelx=1", "exec"]), FirstWord::UnknownOption);
        // The empty long option name.
        assert_eq!(fw(&["--=x", "exec"]), FirstWord::UnknownOption);
    }

    #[test]
    fn a_value_is_not_the_first_word() {
        // The word `exec` here is the value of `--model`, so the word is the next one.
        assert_eq!(fw(&["--model", "exec", "review"]), FirstWord::Word(2));
        assert_eq!(
            fw(&["-m", "gpt", "-c", "a=b", "-p", "q", "e"]),
            FirstWord::Word(6)
        );
    }

    #[test]
    fn short_clusters() {
        // `-ma` is `-m` with value `a`; `-hV` are two flags; `-hm x` is a flag then `-m x`.
        assert_eq!(fw(&["-ma", "exec"]), FirstWord::Word(1));
        assert_eq!(fw(&["-hV", "exec"]), FirstWord::Word(1));
        assert_eq!(fw(&["-hm", "x", "exec"]), FirstWord::Word(2));
        // The rest of a cluster after a value letter is that value, not more letters.
        assert_eq!(fw(&["-mzz", "exec"]), FirstWord::Word(1));
        // A cluster with an unknown letter.
        assert_eq!(fw(&["-hz", "exec"]), FirstWord::UnknownOption);
    }

    #[test]
    fn image_takes_every_value_up_to_an_option() {
        // `num_args = 1..`: `exec` is a value of `-i`, so no word follows.
        assert_eq!(fw(&["-i", "a.png", "exec"]), FirstWord::None);
        assert_eq!(
            fw(&["--image", "a.png", "b.png", "--search", "exec"]),
            FirstWord::Word(4)
        );
        // An attached value is the only one (`parser.rs:1072`).
        assert_eq!(fw(&["--image=a.png", "exec"]), FirstWord::Word(1));
        assert_eq!(fw(&["-ia.png", "exec"]), FirstWord::Word(1));
        // `--` ends the values.
        assert_eq!(fw(&["-i", "a.png", "--", "exec"]), FirstWord::Word(3));
    }

    #[test]
    fn image_values_are_recorded() {
        let p = parse(&["-i", "a.png,b.png", "c.png", "-m", "x", "exec"]);
        assert_eq!(p.first, FirstWord::Word(5));
        assert_eq!(p.options[0].name, "image");
        assert_eq!(p.options[0].values, vec!["a.png,b.png", "c.png"]);
        assert_eq!(p.options[1].name, "model");
        assert_eq!(p.options[1].values, vec!["x"]);
    }

    #[test]
    fn empty_and_missing() {
        assert_eq!(fw(&[]), FirstWord::None);
        assert_eq!(fw(&["--search"]), FirstWord::None);
        // An option lacking its value at the end of the arguments.
        assert_eq!(fw(&["-m"]), FirstWord::None);
        assert_eq!(fw(&["--model"]), FirstWord::None);
        assert_eq!(fw(&["--model="]), FirstWord::None);
        assert_eq!(fw(&["-m", ""]), FirstWord::None);
        // An empty argument is a word (a positional, the empty prompt).
        assert_eq!(fw(&[""]), FirstWord::Word(0));
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(fw(&["--", "exec"]), FirstWord::Word(1));
        assert_eq!(fw(&["--"]), FirstWord::None);
        assert_eq!(fw(&["-m", "x", "--", "--weird"]), FirstWord::Word(3));
    }

    #[test]
    fn options_are_listed_by_long_name_and_alias() {
        let p = parse(&[
            "--yolo",
            "-c",
            "a=b",
            "-p",
            "q",
            "--config=web_search=\"x\"",
            "exec",
        ]);
        let names: Vec<_> = p.options.iter().map(|o| o.name).collect();
        assert_eq!(
            names,
            vec![
                "dangerously-bypass-approvals-and-sandbox",
                "config",
                "profile",
                "config"
            ]
        );
        assert_eq!(p.options[3].values, vec!["web_search=\"x\""]);
        assert_eq!(p.options[3].index, 5);
        assert_eq!(p.first, FirstWord::Word(6));
    }

    #[test]
    fn unknown_keeps_the_options_before_it() {
        let p = parse(&["-m", "x", "--nope", "-p", "q"]);
        assert_eq!(p.first, FirstWord::UnknownOption);
        assert_eq!(p.options.len(), 1);
    }

    // Node §13::129 acceptance and H1.4: key trimmed, split at the first `=`.
    #[test]
    fn config_override_trims_and_splits_at_first_equals() {
        assert_eq!(
            config_override(" web_search = \"live\""),
            Some(("web_search".into(), "\"live\"".into()))
        );
        assert_eq!(config_override("a=b=c"), Some(("a".into(), "b=c".into())));
        assert_eq!(
            config_override(" features.unified_exec_tty =true"),
            Some(("features.unified_exec_tty".into(), "true".into()))
        );
        assert_eq!(
            config_override("web_search=\"a=b\""),
            Some(("web_search".into(), "\"a=b\"".into()))
        );
    }

    #[test]
    fn config_override_rejects_what_codex_rejects() {
        assert_eq!(config_override(""), None);
        assert_eq!(config_override("novalue"), None);
        assert_eq!(config_override("=v"), None);
        assert_eq!(config_override("  =v"), None);
        assert_eq!(config_override("k="), Some(("k".into(), String::new())));
    }
}
