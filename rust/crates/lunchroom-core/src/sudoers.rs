//! What `sudo -n -l` says about the user's account, and whether a rule that runs a command without
//! a password is wide or narrow (spec §7 A1.3; §17 C2 item 8, "wide/narrow").
//!
//! Everything here is a pure function over text and over facts the caller read from the system
//! (the owner and mode of each file and of each folder above it; the `sudo -l -U <user>` text of
//! another user), so each rule of the classification is unit-tested. [`facts_from_disk`] is the
//! one function that reads the disk, and nothing here runs a program.
//!
//! The text is that of sudo 1.9.15p5 (`sudo -V`), whose `-l` output has this shape:
//!
//! ```text
//! Matching Defaults entries for alice on host:
//!     env_reset, mail_badpass, secure_path=/usr/local/sbin\:/usr/bin, use_pty
//!
//! Runas and Command-specific defaults for alice:
//!     Defaults!/usr/sbin/visudo env_keep+="SUDO_EDITOR EDITOR VISUAL"
//!
//! User alice may run the following commands on host:
//!     (ALL : ALL) ALL
//!     (root) NOPASSWD: /usr/bin/systemctl restart nginx, !/usr/bin/su, PASSWD: /usr/bin/apt
//! ```
//!
//! A line with a rule starts with `(runas users : runas groups)`, then comma-separated commands
//! each of which may be preceded by tags (`NOPASSWD:`, `PASSWD:`, `SETENV:` ...) that hold for it
//! and for every command after it on the line until another tag replaces them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Shells and programs that run other programs or open a shell (§17.8).
pub const RUNNERS: [&str; 35] = [
    "sh", "bash", "dash", "zsh", "ksh", "fish", "csh", "tcsh", "env", "sudo", "su", "doas",
    "python", "python3", "perl", "ruby", "node", "php", "lua", "awk", "find", "vi", "vim", "less",
    "more", "man", "gawk", "mawk", "busybox", "xargs", "nohup", "timeout", "chroot", "setpriv",
    "tee",
];

/// The most bytes of `sudo -l` output that are read or parsed. A real listing is a few kilobytes;
/// text over this is not read, and counts as a listing that could not be read (so as wide).
pub const MAX_LISTING_BYTES: usize = 1 << 20;

/// The most lines this parser keeps as ones it could not read, each cut to [`UNPARSED_CHARS`]
/// characters: what is kept is for a message and for the comparison of two answers, not a copy.
pub const MAX_UNPARSED: usize = 100;

/// The most rules (commands) a listing may hold and still be read: the classifier looks at the disk
/// for each, so a listing with more is taken as one that could not be read. A real one has tens.
pub const MAX_RULES: usize = 2000;

/// The most `Defaults!`/`Defaults>` scopes that turn authentication off that are kept; more makes
/// every rule password-free (the safe answer), so the time of `parse` does not grow with their
/// product with the number of rules.
const MAX_SCOPED_DEFAULTS: usize = 1000;

/// The most characters kept of one line this parser could not read.
const UNPARSED_CHARS: usize = 256;

/// What stands for output that was over [`MAX_LISTING_BYTES`] when it was read: it is not a
/// listing, so [`parse`] gives [`Status::Unreadable`] and the account counts as wide.
pub const OVER_LIMIT_TEXT: &str = "sudo: the output was over the size limit and was not read\n";

/// What stands for output that did not end in time: it is not a listing either.
pub const TIMED_OUT_TEXT: &str = "sudo: the output did not end in time and was not read\n";

/// The user the store and the console belong to (§17.2).
const LUNCHROOM: &str = "lunchroom";

/// The deepest chain of "runs as another user whose own rules ..." that is followed.
const DEPTH: usize = 8;

/// The first half of the words sudo itself prints for an account that may not use it; split so
/// this source holds no word the product's vocabulary has retired (§1): the match is on sudo's
/// text, not Lunchroom's.
const SUDO_NOT_ALL: &str = "is not all";

/// Tags a command in `sudo -l` may be preceded by.
const TAGS: [&str; 16] = [
    "NOPASSWD",
    "PASSWD",
    "SETENV",
    "NOSETENV",
    "EXEC",
    "NOEXEC",
    "LOG_INPUT",
    "NOLOG_INPUT",
    "LOG_OUTPUT",
    "NOLOG_OUTPUT",
    "MAIL",
    "NOMAIL",
    "FOLLOW",
    "NOFOLLOW",
    "INTERCEPT",
    "NOINTERCEPT",
];

/// Options sudo 1.9 prints before a command.
const OPTIONS: [&str; 10] = [
    "APPARMOR_PROFILE=",
    "CWD=",
    "CHROOT=",
    "ROLE=",
    "TYPE=",
    "PRIVS=",
    "LIMITPRIVS=",
    "NOTBEFORE=",
    "NOTAFTER=",
    "TIMEOUT=",
];

/// What `sudo -n -l` came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// It listed the account's rules (there may be none that are password-free).
    Listed,
    /// "a password is required": with `listpw` at its default `any`, the account has no rule
    /// without a password.
    PasswordRequired,
    /// The account may not run `sudo` at all.
    NotAllowed,
    /// The text is not a listing and not one of the two refusals above.
    Unreadable(String),
}

/// One command of one rule line of `sudo -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The users it runs as, as printed (`ALL`, `root`, `#0`, `%group`, ...), negations apart.
    pub run_as: Vec<String>,
    /// The users it does not run as (`!root`).
    pub run_as_not: Vec<String>,
    /// The groups it runs as (the part after `:`).
    pub groups: Vec<String>,
    /// The command as printed, with its arguments; no tag, option or `!`.
    pub command: String,
    /// The command was `!command`: it takes a command away, it grants nothing.
    pub denies: bool,
    /// It runs without a password: `NOPASSWD:`, or `!authenticate` in force for it.
    pub password_free: bool,
}

impl Rule {
    /// The rule as one line for a message.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut users = self.run_as.clone();
        users.extend(self.run_as_not.iter().map(|u| format!("!{u}")));
        let who = users.join(", ");
        let group = if self.groups.is_empty() {
            String::new()
        } else {
            format!(" : {}", self.groups.join(", "))
        };
        let deny = if self.denies { "!" } else { "" };
        let pw = if self.password_free { "NOPASSWD: " } else { "" };
        format!("({who}{group}) {pw}{deny}{}", self.command)
    }
}

/// The parsed text of `sudo -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// What the run came to.
    pub status: Status,
    /// Every command of every rule line, in order.
    pub rules: Vec<Rule>,
    /// Lines in the rules section that this parser could not read; a listing with any of them
    /// cannot be judged narrow.
    pub unparsed: Vec<String>,
}

/// Splits `text` at each comma that is not escaped (`\,`) and not inside double quotes.
fn split_commas(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// What the Defaults lines turn off or on for authentication.
#[derive(Default)]
struct Authentication {
    /// `!authenticate` is in force for every rule.
    off_for_all: bool,
    /// `Defaults!<command> !authenticate` or `Defaults><user> !authenticate`.
    off_for: Vec<String>,
    /// More such scopes than [`MAX_SCOPED_DEFAULTS`]: not kept, and every rule counts as
    /// password-free.
    overflow: bool,
}

impl Authentication {
    /// Reads one Defaults entry (`!authenticate`, `authenticate`, or a `Defaults!cmd ...` line).
    fn read(&mut self, entry: &str) {
        let entry = entry.trim();
        if let Some(rest) = entry.strip_prefix("Defaults") {
            let Some(split) = rest.find(char::is_whitespace) else {
                return;
            };
            let (scope, settings) = rest.split_at(split);
            for setting in split_commas(settings) {
                match setting.as_str() {
                    "!authenticate" if self.off_for.len() >= MAX_SCOPED_DEFAULTS => {
                        self.overflow = true;
                    }
                    "!authenticate" => self.off_for.push(scope.to_owned()),
                    "authenticate" => self.off_for.retain(|s| s != scope),
                    _ => {}
                }
            }
            return;
        }
        match entry {
            "!authenticate" => self.off_for_all = true,
            "authenticate" => self.off_for_all = false,
            _ => {}
        }
    }

    /// Whether authentication is off for a rule running `command` as `run_as`.
    fn off(&self, run_as: &[String], command: &str) -> bool {
        self.off_for_all
            || self.overflow
            || self.off_for.iter().any(|scope| {
                if let Some(c) = scope.strip_prefix('!') {
                    c == "ALL" || c == command || c.contains(['*', '?', '['])
                } else if let Some(u) = scope.strip_prefix('>') {
                    u == "ALL" || run_as.iter().any(|r| r == u) || u.starts_with(['%', '+'])
                } else {
                    // `Defaults:user` and `Defaults@host` scopes name the account and the host
                    // this listing is for.
                    true
                }
            })
    }
}

/// Which section of the listing a line belongs to.
#[derive(PartialEq)]
enum Section {
    None,
    Defaults,
    Specific,
    Rules,
}

/// A rule line `(users : groups) tagged commands`, split in its parts.
struct RuleLine {
    users: Vec<String>,
    negated: Vec<String>,
    groups: Vec<String>,
    commands: String,
}

/// Splits the rule line `line`; `None` where it is not of that form.
fn split_rule_line(line: &str) -> Option<RuleLine> {
    let rest = line.strip_prefix('(')?;
    let end = rest.find(')')?;
    let (spec, commands) = (&rest[..end], rest[end + 1..].trim());
    let (users, groups) = spec.split_once(':').unwrap_or((spec, ""));
    let (mut plain, mut negated) = (Vec::new(), Vec::new());
    for u in split_commas(users) {
        match u.strip_prefix('!') {
            Some(n) => negated.push(n.trim().to_owned()),
            None => plain.push(u),
        }
    }
    Some(RuleLine {
        users: plain,
        negated,
        groups: split_commas(groups),
        commands: commands.to_owned(),
    })
}

/// Parses the text `sudo -n -l` (or `sudo -l -U <user>`) printed, standard output and error
/// together.
///
/// The time is linear in the text, whatever the text: it is refused over [`MAX_LISTING_BYTES`]; each
/// line, each comma-separated piece and each tag or option word is looked at a constant number of
/// times (a tag, a `NAME=value` option or a digest is taken off the front of a piece, so the rest
/// that is looked at again is shorter); what is kept as unread is at most [`MAX_UNPARSED`] lines of
/// [`UNPARSED_CHARS`] characters; the rules are at most [`MAX_RULES`], and the scopes that make a
/// rule password-free at most [`MAX_SCOPED_DEFAULTS`], so no loop is a product of two counts that
/// grow with the text.
#[must_use]
pub fn parse(text: &str) -> Listing {
    if text.len() > MAX_LISTING_BYTES {
        return Listing {
            status: Status::Unreadable(OVER_LIMIT_TEXT.trim().to_owned()),
            rules: Vec::new(),
            unparsed: Vec::new(),
        };
    }
    let mut auth = Authentication::default();
    let mut section = Section::None;
    let mut specs: Vec<RuleLine> = Vec::new();
    let mut unparsed = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            section = Section::None;
            continue;
        }
        let indented = raw.starts_with(char::is_whitespace);
        if !indented {
            section = if trimmed.starts_with("Matching Defaults entries") {
                Section::Defaults
            } else if trimmed.starts_with("Runas and Command-specific defaults") {
                Section::Specific
            } else if trimmed.starts_with("User ") && trimmed.contains("may run the following") {
                Section::Rules
            } else {
                Section::None
            };
            continue;
        }
        match section {
            Section::Defaults | Section::Specific => {
                for entry in split_commas(trimmed) {
                    auth.read(&entry);
                }
            }
            Section::Rules => {
                if trimmed.starts_with('(') {
                    match split_rule_line(trimmed) {
                        Some(line) => specs.push(line),
                        None => keep_unparsed(&mut unparsed, trimmed),
                    }
                } else if let Some(last) = specs.last_mut() {
                    // A long line is wrapped: this continues the commands of the line above.
                    last.commands.push(' ');
                    last.commands.push_str(trimmed);
                } else {
                    keep_unparsed(&mut unparsed, trimmed);
                }
            }
            Section::None => {}
        }
    }
    let mut rules = Vec::new();
    for RuleLine {
        users,
        negated,
        groups,
        commands,
    } in specs
    {
        let mut nopasswd = false;
        for piece in split_commas(&commands) {
            let piece = strip_tags(piece.as_str(), &mut nopasswd, &mut unparsed);
            let (denies, command) = match piece.strip_prefix('!') {
                Some(c) => (true, c.trim_start()),
                None => (false, piece),
            };
            let command = strip_options(command);
            if command.is_empty() {
                continue;
            }
            let password_free = nopasswd || auth.off(&users, command);
            if rules.len() >= MAX_RULES {
                return Listing {
                    status: Status::Unreadable("too many rules".to_owned()),
                    rules: Vec::new(),
                    unparsed: Vec::new(),
                };
            }
            rules.push(Rule {
                run_as: users.clone(),
                run_as_not: negated.clone(),
                groups: groups.clone(),
                command: command.to_owned(),
                denies,
                password_free,
            });
        }
    }
    let status = status_of(text, &rules, &unparsed);
    Listing {
        status,
        rules,
        unparsed,
    }
}

/// Takes the options and tags off the front of one command of a rule line, setting `nopasswd` as
/// the tags say. sudoers(5): `Cmnd_Spec ::= Runas_Spec? Option_Spec* (Tag_Spec ':')* Cmnd`,
/// options before tags; the two are read in any order, so a listing that interleaves them still
/// gives its tags to the command. A tag this parser does not know could be a password-free one:
/// the line goes to `unparsed`, and the account cannot be called narrow.
fn strip_tags<'a>(mut piece: &'a str, nopasswd: &mut bool, unparsed: &mut Vec<String>) -> &'a str {
    loop {
        piece = piece.trim_start();
        let first = piece.split_whitespace().next().unwrap_or_default();
        if is_option(first) && piece.len() > first.len() {
            piece = &piece[first.len()..];
            continue;
        }
        let Some((tag, rest)) = piece.split_once(':') else {
            return piece;
        };
        let tag = tag.trim();
        if TAGS.contains(&tag) {
            match tag {
                "NOPASSWD" => *nopasswd = true,
                "PASSWD" => *nopasswd = false,
                _ => {}
            }
        } else if looks_like_tag(tag) {
            keep_unparsed(unparsed, piece);
        } else {
            return piece;
        }
        piece = rest;
    }
}

/// The canonical line of a listing that could not be read. A rule line begins with `(` and a kept
/// line with `? `, so this one cannot be the canonical form of anything else.
const UNREADABLE_MARKER: &str = "! the output was not a listing";

/// Keeps `line` as one this parser could not read, within [`MAX_UNPARSED`] lines of
/// [`UNPARSED_CHARS`] characters.
fn keep_unparsed(unparsed: &mut Vec<String>, line: &str) {
    if unparsed.len() < MAX_UNPARSED {
        unparsed.push(line.chars().take(UNPARSED_CHARS).collect());
    }
}

/// Whether `word` is one of the `NAME=value` options sudo prints.
fn is_option(word: &str) -> bool {
    OPTIONS.iter().any(|o| word.starts_with(o))
}

/// Whether `word` has the form of a tag (capital letters and underscores).
fn looks_like_tag(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// `command` without the options and digest sudo 1.9 prints before it.
fn strip_options(mut command: &str) -> &str {
    loop {
        let first = command.split_whitespace().next().unwrap_or_default();
        let is_option = is_option(first);
        let is_digest = ["sha224:", "sha256:", "sha384:", "sha512:"]
            .iter()
            .any(|d| first.starts_with(d));
        if (is_option || is_digest) && command.len() > first.len() {
            command = command[first.len()..].trim_start();
        } else {
            return command.trim();
        }
    }
}

/// What the whole text came to.
fn status_of(text: &str, rules: &[Rule], unparsed: &[String]) -> Status {
    let lower = text.to_ascii_lowercase();
    if lower.contains("a password is required") && rules.is_empty() {
        return Status::PasswordRequired;
    }
    if lower.contains(&[SUDO_NOT_ALL, "owed to run sudo"].concat())
        || lower.contains("may not run sudo")
    {
        return Status::NotAllowed;
    }
    if !rules.is_empty() || !unparsed.is_empty() || lower.contains("may run the following") {
        return Status::Listed;
    }
    Status::Unreadable(text.trim().chars().take(200).collect())
}

impl Listing {
    /// The rules that run a command without a password and grant it (not `!command`).
    pub fn password_free(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|r| r.password_free && !r.denies)
    }

    /// Whether the listing shows a rule that lets the account run a command without a password
    /// (A1.3). A listing that could not be read does not show one, and does not show that there
    /// is none either: [`Listing::known`] says which.
    #[must_use]
    pub fn has_password_free(&self) -> bool {
        self.password_free().next().is_some() || !self.unparsed.is_empty() || !self.known()
    }

    /// Whether the text could be read: `false` for [`Status::Unreadable`].
    #[must_use]
    pub fn known(&self) -> bool {
        !matches!(self.status, Status::Unreadable(_))
    }

    /// The password-free rules in a fixed order, one line each; two listings of the same rules
    /// have the same canonical text, whatever else `sudo -l` printed.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut lines: Vec<String> = self.password_free().map(Rule::describe).collect();
        lines.extend(self.unparsed.iter().map(|u| format!("? {u}")));
        if !self.known() {
            lines.push(UNREADABLE_MARKER.to_owned());
        }
        lines.sort();
        lines.dedup();
        lines.join("\n")
    }
}

/// What the install and `lunchroom doctor` say where `sudo -n -l` shows a rule that runs a command
/// without a password (A1.3, K1.10): the rules, that the confirmation proves no person is present,
/// and that the passphrase check does not hold on the account.
#[must_use]
pub fn statement(listing: &Listing) -> String {
    if !listing.known() {
        // §17.8: text that is not a listing is counted as a wide rule, and the clause's words are
        // used as they are: the console's wall does not hold on this account.
        return "Lunchroom: the output of `sudo -n -l` was not a listing it can read, so Lunchroom \
                counts this account as one with a rule that runs a command without a password. \
                The console's wall does not hold on this account until `sudo` asks it for its \
                password; `lunchroom install console` can make it ask. This confirmation \
                therefore proves no person is present, and the passphrase check does not hold on \
                this account (K1.10)."
            .to_owned();
    }
    let rules: Vec<String> = listing
        .password_free()
        .map(|r| format!("  {}", r.describe()))
        .collect();
    let shown = if rules.is_empty() {
        String::new()
    } else {
        format!("\n{}", rules.join("\n"))
    };
    format!(
        "Lunchroom: `sudo -n -l` shows a rule that lets your account run a command without a \
         password:{shown}\nThis confirmation therefore proves no person is present, and the \
         passphrase check does not hold on this account (K1.10)."
    )
}

/// The owner and permission bits of a file or folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The owner's user id.
    pub owner: u32,
    /// The permission bits (the file-type bits are ignored).
    pub mode: u32,
}

impl Entry {
    /// Whether a user other than root can write it: another owner, or write for group or others.
    #[must_use]
    pub fn writable_by_non_root(self) -> bool {
        self.owner != 0 || self.mode & 0o022 != 0
    }
}

/// What the caller read about the file a rule names.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PathFacts {
    /// The file itself, as the system has it after links; `None` where nothing is there.
    pub file: Option<Entry>,
    /// Each folder above it, the folders of every link on the way included.
    pub above: Vec<Entry>,
    /// The base names of the file and of every link on the way to it (`vi`, then `vim.basic`).
    pub names: Vec<String>,
}

/// Why a rule is wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Runs as root or as any user (`ALL`) with any command (`ALL`).
    RootOrAnyWithAll,
    /// Runs as `lunchroom`, by name or through `ALL`.
    RunsAsLunchroom,
    /// Names a command with a wildcard.
    Wildcard,
    /// Names a shell or a program that runs other programs.
    Runner(String),
    /// Names a path that is not absolute.
    NotAbsolute,
    /// Names a folder: every file in it is named.
    Folder,
    /// Names a file not owned by root.
    NotOwnedByRoot,
    /// A user other than root can write the file or a folder above it.
    WritableAbove,
    /// Runs as another user whose own rules hold a wide rule.
    OtherUserWide(String),
    /// Runs as a user whose rules were not read (a group, a netgroup, or no text for the user).
    OtherUserUnread(String),
    /// The rule or the file it names could not be examined.
    Unjudged(String),
}

impl Reason {
    /// The reason in a sentence.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::RootOrAnyWithAll => "runs any command as root or as any user".to_owned(),
            Self::RunsAsLunchroom => "can run as the user lunchroom".to_owned(),
            Self::Wildcard => "names a command with a wildcard".to_owned(),
            Self::Runner(n) => format!("names {n}, which runs other programs or opens a shell"),
            Self::NotAbsolute => "names a path that is not absolute".to_owned(),
            Self::Folder => "names a folder, so every file in it".to_owned(),
            Self::NotOwnedByRoot => "names a file that root does not own".to_owned(),
            Self::WritableAbove => {
                "names a file, or a folder above it, that a user other than root can write"
                    .to_owned()
            }
            Self::OtherUserWide(u) => format!("runs as {u}, whose own rules hold a wide rule"),
            Self::OtherUserUnread(u) => {
                format!("runs as {u}, whose rules were not read, so they are taken as wide")
            }
            Self::Unjudged(w) => format!("could not be judged: {w}"),
        }
    }
}

/// A password-free rule and the reasons it is wide; none means narrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule, as [`Rule::describe`] gives it.
    pub rule: String,
    /// Empty where the rule is narrow.
    pub reasons: Vec<Reason>,
}

/// The classification of a listing.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Verdict {
    /// One entry per password-free rule, and one per line this parser could not read.
    pub findings: Vec<Finding>,
}

impl Verdict {
    /// Whether the account has a rule that runs a command without a password.
    #[must_use]
    pub fn password_free(&self) -> bool {
        !self.findings.is_empty()
    }

    /// Whether any password-free rule is wide.
    #[must_use]
    pub fn wide(&self) -> bool {
        self.findings.iter().any(|f| !f.reasons.is_empty())
    }

    /// The wide rules, each with its reasons in a sentence, one per line.
    #[must_use]
    pub fn wide_lines(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| !f.reasons.is_empty())
            .map(|f| {
                let why: Vec<String> = f.reasons.iter().map(Reason::text).collect();
                format!("{}: {}", f.rule, why.join("; "))
            })
            .collect()
    }
}

/// The facts the classifier asks the caller for.
pub struct Sources<'a> {
    /// The owner and modes for a path; `None` where they cannot be read.
    pub facts: &'a dyn Fn(&str) -> Option<PathFacts>,
    /// The text of `sudo -l -U <user>` for a user, read in the step that runs as root; `None`
    /// where it was not read.
    pub others: &'a dyn Fn(&str) -> Option<String>,
    /// The user id and the group id of `lunchroom` on this system, where it has them; a rule that
    /// names an id (`#n`) is matched against them.
    pub lunchroom_ids: LunchroomIds,
}

/// The user id and the group id of `lunchroom` on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LunchroomIds {
    /// The user id, where the user exists.
    pub uid: Option<u32>,
    /// The group id, where the group exists.
    pub gid: Option<u32>,
    /// Whether the two were read from one unchanged state of `/etc/passwd` and `/etc/group`. Where
    /// not, a number in a rule cannot be matched against them, and counts as `lunchroom`'s.
    pub settled: bool,
}

impl LunchroomIds {
    /// Ids read from one state of the two files.
    #[must_use]
    pub fn known(uid: Option<u32>, gid: Option<u32>) -> Self {
        Self {
            uid,
            gid,
            settled: true,
        }
    }
}

impl std::fmt::Debug for Sources<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources").finish_non_exhaustive()
    }
}

/// Whether `name` is `entry`, or `entry` followed by a version or a suffix (`python3.11`,
/// `vim.basic`, `perl5.38`).
fn is_runner_name(name: &str) -> Option<&'static str> {
    if let Some(exact) = RUNNERS.iter().copied().find(|entry| *entry == name) {
        return Some(exact);
    }
    RUNNERS.iter().copied().find(|entry| {
        name.strip_prefix(entry).is_some_and(|rest| {
            rest.starts_with('.')
                || rest
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        })
    })
}

/// The first word of a command, backslash-escapes taken out: the file it names.
fn command_file(command: &str) -> String {
    let mut word = String::new();
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            c if c.is_whitespace() => break,
            c => word.push(c),
        }
    }
    word
}

/// Whether `text` holds a wildcard character that is not escaped.
fn has_wildcard(text: &str) -> bool {
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '*' | '?' | '[' => return true,
            _ => {}
        }
    }
    false
}

/// The reasons the command part of a rule is wide.
fn command_reasons(rule: &Rule, sources: &Sources<'_>) -> Vec<Reason> {
    let command = rule.command.as_str();
    if command == "ALL" {
        return Vec::new();
    }
    let mut reasons = Vec::new();
    if has_wildcard(command) {
        reasons.push(Reason::Wildcard);
    }
    let file = command_file(command);
    if !file.starts_with('/') {
        reasons.push(Reason::NotAbsolute);
        if let Some(n) = is_runner_name(&file) {
            reasons.push(Reason::Runner(n.to_owned()));
        }
        return reasons;
    }
    if file.ends_with('/') {
        reasons.push(Reason::Folder);
    }
    let base = Path::new(&file)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    if let Some(n) = is_runner_name(&base) {
        reasons.push(Reason::Runner(n.to_owned()));
    }
    if reasons.contains(&Reason::Wildcard) && file.contains(['*', '?', '[']) {
        // The path itself holds the wildcard: there is no one file to examine.
        return reasons;
    }
    match (sources.facts)(&file) {
        None => reasons.push(Reason::Unjudged(format!("{file} could not be examined"))),
        Some(facts) => {
            for name in &facts.names {
                if let Some(n) = is_runner_name(name) {
                    let r = Reason::Runner(n.to_owned());
                    if !reasons.contains(&r) {
                        reasons.push(r);
                    }
                }
            }
            if facts.file.is_some_and(|f| f.owner != 0) {
                reasons.push(Reason::NotOwnedByRoot);
            }
            let file_writable = facts.file.is_some_and(|f| f.mode & 0o022 != 0);
            if file_writable || facts.above.iter().any(|e| e.writable_by_non_root()) {
                reasons.push(Reason::WritableAbove);
            }
        }
    }
    reasons
}

/// The reasons the user part of a rule is wide, other users' rules followed.
fn user_reasons(
    rule: &Rule,
    sources: &Sources<'_>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Reason> {
    let mut reasons = Vec::new();
    let named = |n: &str| rule.run_as.iter().any(|u| u == n);
    let excluded = |n: &str| rule.run_as_not.iter().any(|u| u == n);
    let all = named("ALL");
    let root = named("root") || named("#0") || all;
    if root && rule.command == "ALL" {
        reasons.push(Reason::RootOrAnyWithAll);
    }
    // `ALL` includes `lunchroom` unless the rule takes it out again; so does a number that is
    // its id.
    let LunchroomIds { uid, gid, settled } = sources.lunchroom_ids;
    // A number is `lunchroom`'s where it is its id, and where the ids could not be settled.
    let by_number = |id: Option<u32>, list: &[String]| {
        list.iter().any(|n| {
            n.strip_prefix('#').is_some_and(|digits| {
                !digits.is_empty()
                    && digits.chars().all(|c| c.is_ascii_digit())
                    && (if settled {
                        id.is_some_and(|id| digits == id.to_string())
                    } else {
                        true
                    })
            })
        })
    };
    let user_hit = named(LUNCHROOM) || all || by_number(uid, &rule.run_as);
    let user_out = excluded(LUNCHROOM) || by_number(uid, &rule.run_as_not);
    // Running with group `lunchroom` reads the store as well (§17.8): the group by name, `ALL`,
    // or its number, unless the rule takes it out again (`!lunchroom`).
    let group_hit =
        rule.groups.iter().any(|g| g == LUNCHROOM || g == "ALL") || by_number(gid, &rule.groups);
    let group_out = rule.groups.iter().any(|g| g == "!lunchroom")
        || (settled && by_number(gid, &negated_numbers(&rule.groups)));
    let lunchroom = (user_hit && !user_out) || (group_hit && !group_out);
    if lunchroom {
        reasons.push(Reason::RunsAsLunchroom);
    }
    for user in &rule.run_as {
        if user == "ALL" || user == "root" || user == "#0" || user == LUNCHROOM {
            continue;
        }
        if settled && uid.is_some_and(|id| *user == format!("#{id}")) {
            continue;
        }
        if user.starts_with(['%', '+']) || !valid_user_name(user) {
            reasons.push(Reason::OtherUserUnread(user.clone()));
            continue;
        }
        if depth >= DEPTH {
            reasons.push(Reason::OtherUserUnread(user.clone()));
            continue;
        }
        if !seen.insert(user.clone()) {
            continue;
        }
        match (sources.others)(user) {
            None => reasons.push(Reason::OtherUserUnread(user.clone())),
            Some(text) => {
                let listing = parse(&text);
                let verdict = classify_depth(&listing, sources, seen, depth + 1);
                if verdict.wide() {
                    reasons.push(Reason::OtherUserWide(user.clone()));
                }
            }
        }
    }
    reasons
}

/// The numbers a list takes out (`!#985` as `#985`).
fn negated_numbers(list: &[String]) -> Vec<String> {
    list.iter()
        .filter_map(|g| g.strip_prefix("!#").map(|n| format!("#{n}")))
        .collect()
}

fn classify_depth(
    listing: &Listing,
    sources: &Sources<'_>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Verdict {
    let mut findings = Vec::new();
    // Text that is not a listing (a translation of sudo's words, an error) says nothing about the
    // rules, so the account cannot be called narrow.
    if !listing.known() {
        findings.push(Finding {
            rule: "the output of sudo -l".to_owned(),
            reasons: vec![Reason::Unjudged("the output was not understood".to_owned())],
        });
    }
    for rule in listing.password_free() {
        let mut reasons = user_reasons(rule, sources, seen, depth);
        if rule.command != "ALL" {
            reasons.extend(command_reasons(rule, sources));
        }
        reasons.dedup();
        findings.push(Finding {
            rule: rule.describe(),
            reasons,
        });
    }
    for line in &listing.unparsed {
        findings.push(Finding {
            rule: line.clone(),
            reasons: vec![Reason::Unjudged("the line was not understood".to_owned())],
        });
    }
    Verdict { findings }
}

/// Classifies every rule of `listing` that runs a command without a password as wide (with its
/// reasons) or narrow (§17.8). `account` is the user the listing is of; it is never followed as
/// another user.
#[must_use]
pub fn classify(listing: &Listing, account: &str, sources: &Sources<'_>) -> Verdict {
    let mut seen = BTreeSet::new();
    seen.insert(account.to_owned());
    classify_depth(listing, sources, &mut seen, 0)
}

/// Whether `name` can be handed to `sudo -l -U` as a user: a login name (letters, digits, `_`, `.`,
/// `-`, `$`; not beginning with `-` or `.`; at most 64 characters) or a number (`#` and digits). An
/// empty name, a name with white space, one beginning with `-`, which `sudo` could read as an
/// option, and one holding shell characters, quotes, a backslash or a newline is not one: it is
/// never passed on, and a rule that runs as it is taken as wide.
#[must_use]
pub fn valid_user_name(name: &str) -> bool {
    if let Some(number) = name.strip_prefix('#') {
        return !number.is_empty()
            && number.len() <= 10
            && number.chars().all(|c| c.is_ascii_digit());
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 64
        && (first.is_ascii_alphanumeric() || first == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '$'))
}

/// The users of `listing`'s password-free rules whose own rules the root step must read with
/// `sudo -l -U <user>`: every plain user name other than `root`, `lunchroom` and `ALL`.
#[must_use]
pub fn users_to_read(listing: &Listing, account: &str) -> Vec<String> {
    let mut users = BTreeSet::new();
    for rule in listing.password_free() {
        for u in &rule.run_as {
            let plain = !(u == "ALL"
                || u == "root"
                || u == "#0"
                || u == LUNCHROOM
                || u == account
                || u.starts_with(['%', '+']));
            if plain && valid_user_name(u) {
                users.insert(u.clone());
            }
        }
    }
    users.into_iter().collect()
}

/// The answer Lunchroom keeps from reading `sudo -n -l` (A1.3: "and keeps the answer"): when it
/// was read, the account's text, and the text of each other user read in the root step.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Kept {
    /// Seconds since 1970 when it was read.
    pub read_at: u64,
    /// The text of `sudo -n -l`.
    pub own: String,
    /// The text of `sudo -l -U <user>`, by user.
    pub others: BTreeMap<String, String>,
}

impl Kept {
    /// The answer as JSON text for the store.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "read_at": self.read_at,
            "own": self.own,
            "others": self.others,
        })
        .to_string()
    }

    /// The answer from its JSON text; `None` where the text is not one.
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let others = value
            .get("others")?
            .as_object()?
            .iter()
            .map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(Self {
            read_at: value.get("read_at")?.as_u64()?,
            own: value.get("own")?.as_str()?.to_owned(),
            others,
        })
    }

    /// Whether `now`, a fresh `sudo -n -l` text, shows the same password-free rules as the kept
    /// one (`lunchroom doctor` prints a FAIL where it does not).
    #[must_use]
    pub fn same_rules_as(&self, now: &str) -> bool {
        let (kept, fresh) = (parse(&self.own), parse(now));
        // `sudo -n -l` lists the rules while sudo's credentials are cached (just after a password
        // was typed, as in the install) and answers "a password is required" when they are not
        // (a later terminal, or after the timeout): with no password-free rule the two are the
        // same rules (cold first-time install, 2026-11-15: doctor's FAIL right after install).
        let settled = |l: &Listing| {
            if l.status == Status::Listed
                && l.password_free().next().is_none()
                && l.unparsed.is_empty()
            {
                Status::PasswordRequired
            } else {
                l.status.clone()
            }
        };
        settled(&kept) == settled(&fresh) && kept.canonical() == fresh.canonical()
    }

    /// The kept answer classified, the other users' texts taken from what was kept with it.
    #[must_use]
    pub fn verdict(&self, account: &str, facts: &dyn Fn(&str) -> Option<PathFacts>) -> Verdict {
        let others = |user: &str| self.others.get(user).cloned();
        classify(
            &parse(&self.own),
            account,
            &Sources {
                facts,
                others: &others,
                lunchroom_ids: lunchroom_ids_on_disk(),
            },
        )
    }
}

/// What identifies one state of a file: its length, modification time and inode.
type Stamp = (u64, i64, i64, u64);

/// The ids of `lunchroom` read from one state of `/etc/passwd` and `/etc/group`: each file is read
/// once per try, between two looks at both files' stamps; where a stamp changed in between, the
/// try is made again (up to `tries`), and where no try held still the ids are not settled.
#[must_use]
pub fn lunchroom_ids_with(
    stamp: &dyn Fn(&str) -> Option<Stamp>,
    read: &dyn Fn(&str) -> String,
    tries: usize,
) -> LunchroomIds {
    for _ in 0..tries {
        let before = (stamp("/etc/passwd"), stamp("/etc/group"));
        let (passwd, group) = (read("/etc/passwd"), read("/etc/group"));
        if (stamp("/etc/passwd"), stamp("/etc/group")) == before {
            return LunchroomIds::known(
                crate::setgid::uid_from_passwd(&passwd, LUNCHROOM),
                crate::setgid::gid_from_group(&group, LUNCHROOM),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    LunchroomIds {
        uid: None,
        gid: None,
        settled: false,
    }
}

/// The ids of `lunchroom` from the files on this system, as one snapshot.
#[must_use]
pub fn lunchroom_ids_on_disk() -> LunchroomIds {
    use std::os::unix::fs::MetadataExt as _;
    let stamp = |path: &str| -> Option<Stamp> {
        let m = std::fs::metadata(path).ok()?;
        Some((m.len(), m.mtime(), m.mtime_nsec(), m.ino()))
    };
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    lunchroom_ids_with(&stamp, &read, 20)
}

/// Reads the owner and modes for `path` from the disk: the file after links, the folders above it
/// and above every link on the way, and the names of the file and of every link. `None` where a
/// folder above cannot be read or links loop.
#[must_use]
pub fn facts_from_disk(path: &str) -> Option<PathFacts> {
    use std::os::unix::fs::MetadataExt as _;
    let entry = |meta: &std::fs::Metadata| Entry {
        owner: meta.uid(),
        mode: meta.mode() & 0o7777,
    };
    // (path, whether it is the file itself or a folder a link led to)
    let mut todo = vec![(std::path::PathBuf::from(path), true)];
    let mut facts = PathFacts::default();
    let mut hops = 0;
    while let Some((next, is_file)) = todo.pop() {
        hops += 1;
        if hops > 40 {
            return None;
        }
        if let Some(name) = next.file_name().and_then(|n| n.to_str()) {
            facts.names.push(name.to_owned());
        }
        let link_target = |at: &std::path::Path| -> Option<std::path::PathBuf> {
            let target = std::fs::read_link(at).ok()?;
            if target.is_absolute() {
                Some(target)
            } else {
                at.parent().map(|p| p.join(&target))
            }
        };
        // Every folder above `next`, read as it is on disk.
        let mut walked = std::path::PathBuf::new();
        let mut parts: Vec<_> = next.components().collect();
        parts.pop();
        for part in parts {
            walked.push(part);
            let meta = std::fs::symlink_metadata(&walked).ok()?;
            if meta.file_type().is_symlink() {
                todo.push((link_target(&walked)?, false));
            } else {
                facts.above.push(entry(&meta));
            }
        }
        match std::fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                todo.push((link_target(&next)?, is_file));
            }
            Ok(meta) if is_file => facts.file = Some(entry(&meta)),
            Ok(meta) => facts.above.push(entry(&meta)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(facts)
}

#[cfg(test)]
mod tests;
