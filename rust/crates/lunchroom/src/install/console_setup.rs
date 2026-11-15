//! The store wall and the console (1.1, §17.2, §17.8, §17.9): `lunchroom install` puts the store
//! and the program out of reach of the user's agents; `lunchroom install console` also lets the
//! user start the console and the Agent Execution Controller's approvals; `lunchroom uninstall
//! console` takes the controller's signal away.
//!
//! Every step runs as root through the program A1.3 names, and each is shown to the person before
//! any is done:
//!
//! The wall (`lunchroom install`):
//! 1. the system group and user `lunchroom`, when absent;
//! 2. `/usr/local/lib/lunchroom` (root's, 0755) and, in it, the program: root and group
//!    `lunchroom`, mode 2755 (set-group-id);
//! 3. `/var/lib/lunchroom/<uid>`: root and group `lunchroom`, mode 2770, so only a process with
//!    the group can enter it; the developer is not in that group;
//! 4. the old store copied in once (`cp`, then ownership and mode), when the new one is not there.
//!
//! The console (`lunchroom install console`), after the wall and before anything else a `sudo`
//! rule that runs a command without a password is dealt with (§17.8):
//! 5. the delivery folder of the controller's approvals and its `requests/` folder;
//! 6. a sudoers drop-in `/etc/sudoers.d/lunchroom` that lets the developer, and no one else, run
//!    the console as `lunchroom`, written under a name `sudo` ignores, checked with
//!    `visudo -c -f`, then renamed into place;
//! 7. the key pair (an existing pair is kept), the public key last.
//!
//! Then the new program's own `install` points the hooks at it, the `~/.local/bin` link is pointed
//! at it, and the old store is renamed to `store.moved-<stamp>` beside the old one (nothing is
//! deleted).

use std::fmt::Write as _;
use std::fs;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

use lunchroom_core::mail::Mail;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;
use lunchroom_core::sudoers::{self, Verdict};

use super::console_keys;
use super::console_state::{Kind, State};
use super::{installed_program, stamp};
use crate::cli::Outcome;
use crate::home::Env;
use crate::terminal::{
    SudoProbe, System, check, confirm, confirm_or_begin, read_sudo_answer, system_path,
};

/// The programs the store wall's steps run, found in the system folders. `visudo` is not among
/// them: a system with `doas`, `run0` or `su` and no `sudo` has none, and the wall needs no rule.
/// The programs that make the system group and user (`groupadd` and `useradd`, or `BusyBox`'s
/// `addgroup` and `adduser`) are checked apart, by [`accounts_here`].
const WALL_TOOLS: [&str; 11] = [
    "install", "cp", "chown", "chmod", "mv", "rm", "timeout", "du", "env", "find", "test",
];

/// Which programs make the system group and user here (2026-11-14 real-world kit `packages`, apk:
/// Alpine has `BusyBox`'s `addgroup` and `adduser`, no `groupadd` or `useradd`, and the install
/// stopped before doing anything).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accounts {
    /// `groupadd` and `useradd`.
    Shadow,
    /// `BusyBox`'s `addgroup` and `adduser`.
    Busybox,
}

/// The account programs this system has, `groupadd` and `useradd` first; `None` where it has
/// neither pair whole.
#[must_use]
pub fn accounts_here() -> Option<Accounts> {
    accounts_from(&|name| system_path(name).is_some())
}

/// [`accounts_here`], told which programs are there.
#[must_use]
pub fn accounts_from(there: &dyn Fn(&str) -> bool) -> Option<Accounts> {
    if there("groupadd") && there("useradd") {
        Some(Accounts::Shadow)
    } else if there("addgroup") && there("adduser") {
        Some(Accounts::Busybox)
    } else {
        None
    }
}

/// What the console's steps run besides the wall's: the rule files, the key made as root.
const CONSOLE_TOOLS: [&str; 5] = ["visudo", "cat", "sh", "dd", "base64"];

/// The program that gives the sha-256 of the installed program, run as root.
const HASH_TOOL: &str = "sha256sum";

/// The programs `what` runs: the wall's, and with `console` the console's too.
#[must_use]
pub fn required_tools(console: bool) -> Vec<&'static str> {
    let mut all: Vec<&str> = WALL_TOOLS.to_vec();
    all.push(HASH_TOOL);
    if console {
        all.extend(CONSOLE_TOOLS);
    }
    all
}

/// The launch rule's name while it is unchecked: `sudo` ignores a name with a `.` in it.
const LAUNCH_RULE_UNCHECKED: &str = "/etc/sudoers.d/lunchroom.unchecked";
/// The launch rule's name once `visudo` has passed it.
pub(crate) const LAUNCH_RULE: &str = "/etc/sudoers.d/lunchroom";

/// What a step's input says where the text is the public key of the existing private key, which
/// only root can read: it is read when the step runs.
pub const PUBLIC_KEY_LATER: &str = "(the public key of the private key already there, one line)";

/// What the install says about the sessions that are open (§17.2).
pub(crate) const SESSIONS_ANEW: &str = "Start every session of your agents anew: a session that is open \
                             runs the program it started with.\n";

/// What is checked, as root, after a step has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verify {
    /// The file at `path` has this sha-256.
    Hash { path: String, sha: String },
    /// The file at `path` holds exactly the base64 of 32 bytes and a newline (a console key).
    KeyFile(String),
    /// The tree at `path` is at most `max` bytes (`du -sb`, read as root): a copy that came out
    /// larger than what was measured is not renamed into place.
    MaxBytes { path: String, max: u64 },
    /// The tree at `path` holds exactly `expected` entries that are not folders (`find ! -type d`,
    /// read as root): a file added to the copy, or a link, is not taken for the store.
    Entries { path: String, expected: u64 },
}

/// One command the set-up runs as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootStep {
    /// What it does and why, in plain words, as the person is shown it.
    pub why: String,
    /// The program (an absolute path) and its arguments.
    pub argv: Vec<String>,
    /// Text for its standard input.
    pub input: Option<String>,
    /// A command to run if this one fails (removing a file this set-up made).
    pub on_failure: Option<Vec<String>>,
    /// The file this step installs and the sha-256 it must have afterwards, read as root: where
    /// it differs, `on_failure` runs and the set-up stops (§17.2: the program in place is the
    /// one that was running).
    pub verify: Option<Verify>,
}

/// Which of the console's two keys are on the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// Both: the pair is kept.
    Both,
    /// The private key only (the public key was removed by `lunchroom uninstall`).
    PrivateOnly,
    /// The public key only: a pair with no private key is of no use, so a new pair is made.
    PublicOnly,
    /// Neither.
    Neither,
}

/// What the key steps do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPlan {
    /// Both keys are there: nothing is written.
    Keep,
    /// A new pair: the seed is made by the root step (never in this process), and the public key
    /// is written from the private key it made.
    Make,
    /// The public key is written from the private key that is there.
    Derive,
}

/// What the steps depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent facts about the computer
pub struct Facts {
    pub uid: u32,
    pub developer: String,
    /// The developer's primary group, the group of the delivery folder (§17.9).
    pub primary_group: String,
    /// Where the root step reads the program to install from: a file this process holds open on
    /// its own image (an in-memory copy made before any question, sealed against change), never
    /// a path the user's processes can replace (§17.2).
    pub source: PathBuf,
    /// The sha-256 of that program, to be read again from the installed one.
    pub source_sha256: Option<String>,
    /// Whether the program that is running is the installed one already.
    pub running_is_installed: bool,
    pub group_exists: bool,
    pub user_exists: bool,
    /// Which programs make the system group and user: `groupadd` and `useradd` (shadow-utils), or
    /// `BusyBox`'s `addgroup` and `adduser` where the system has those alone (Alpine).
    pub accounts: Accounts,
    /// The old store folder, records file and records folder, each when it exists and the new one
    /// is not there yet.
    pub old_store: Option<PathBuf>,
    pub old_records: Option<PathBuf>,
    pub old_records_dir: Option<PathBuf>,
    pub keys: Keys,
    /// A record of why an earlier `install console` set nothing up is there.
    pub state_exists: bool,
    /// The delivery folder and the launch rule were there before this run (a failure of this run
    /// removes only what it made).
    pub approvals_there: bool,
    pub launch_rule_there: bool,
    /// Temporary files of an earlier try that are in the way: the install stops and names them.
    pub leftovers: Vec<PathBuf>,
    /// The user's passphrase verifier, when there. 1.1.2 and earlier put it at mode 0644, group
    /// root, which this build refuses (A1.3: root's, group `lunchroom`, mode 0640); the wall
    /// gives it that owner, group and mode (F2-AC4).
    pub verifier: Option<PathBuf>,
}

/// The longest line that can be a `yes`: 16 bytes, newline and spaces included.
const MAX_YES_BYTES: usize = 16;

/// What a copy may exceed what was measured before it by: 1 MiB (folder sizes, lock files).
const COPY_SLACK_BYTES: u64 = 1024 * 1024;

/// Opens `path` once, without following a link (`O_NOFOLLOW`), as a folder or a file: the handle
/// names the thing that was looked at.
///
/// # Errors
/// That `path` is a link, or is not a plain folder or file, or cannot be opened.
fn hold(path: &Path, want: Want) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut flags = rustix::fs::OFlags::NOFOLLOW;
    if matches!(want, Want::Folder) {
        flags |= rustix::fs::OFlags::DIRECTORY;
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(i32::try_from(flags.bits()).unwrap_or(0))
        .open(path)
        .map_err(|e| {
            format!(
                "{} is a symbolic link, or not a plain {}, or cannot be opened ({e}); nothing \
                 was moved or changed. Replace it by the real {} and run the command again",
                path.display(),
                if matches!(want, Want::Folder) {
                    "folder"
                } else {
                    "file"
                },
                if matches!(want, Want::Folder) {
                    "folder"
                } else {
                    "file"
                },
            )
        })
}

/// What a path must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    File,
    Folder,
}

/// Whether `path` is a real file or folder as `want` says, read as it is on disk (a link is not
/// followed). A symbolic link is an error: the old store is never copied through one (§17.2).
///
/// # Errors
/// That `path` is a symbolic link.
fn is_real(path: &Path, want: Want) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(format!(
            "{} is a symbolic link, which Lunchroom does not copy through; nothing was moved or \
             changed. Replace it by the real folder or file and run the command again",
            path.display()
        )),
        Ok(m) => Ok(match want {
            Want::File => m.is_file(),
            Want::Folder => m.is_dir(),
        }),
        Err(_) => Ok(false),
    }
}

/// An account name that can stand in the comment of a rule file: no control character.
#[must_use]
pub fn valid_account(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// The launch rule: the developer, by user id, may run the console as the user `lunchroom`,
/// after typing their own sudo password (no `NOPASSWD`). The account is named by its user id, as
/// the rule of §17.8 names it, so a login with a `.` or any other character is no obstacle.
#[must_use]
pub fn sudoers_text(developer: &str, uid: u32) -> String {
    format!(
        "# Written by `lunchroom install console`: lets {developer} start the Lunchroom console as \
         the user {user}.\n# It asks for {developer}'s sudo password.\n\
         #{uid} ALL=({user}) {program} console\n",
        user = setgid::SYSTEM_USER,
        program = setgid::PROGRAM
    )
}

/// The one rule of §17.8: the account, by its user id, asks for its password for everything.
#[must_use]
pub fn rule_text(uid: u32) -> String {
    format!("#{uid} ALL=(ALL:ALL) PASSWD: ALL\n")
}

/// The name the rule file has while it is unchecked: it holds a `.`, so `sudo` skips it.
#[must_use]
pub fn rule_unchecked(uid: u32) -> String {
    format!("{}.new", setgid::rule_file(uid).display())
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn step(why: &str, argv: Vec<String>) -> RootStep {
    RootStep {
        why: why.to_owned(),
        argv,
        input: None,
        on_failure: None,
        verify: None,
    }
}

/// `install -d` for a folder with owner, group and mode.
fn folder(
    why: &str,
    tool: &dyn Fn(&str) -> String,
    owner: &str,
    group: &str,
    mode: &str,
    path: &str,
) -> RootStep {
    step(
        why,
        words(&[
            &tool("install"),
            "-d",
            "-o",
            owner,
            "-g",
            group,
            "-m",
            mode,
            path,
        ]),
    )
}

/// The steps that write a file under a name `sudo` ignores, check it with `visudo -c -f`, and
/// rename it into place (§17.8): a failed check removes the unchecked file.
fn checked_rule_file(
    tool: &dyn Fn(&str) -> String,
    why: &str,
    unchecked: &str,
    finished: &str,
    text: String,
) -> Vec<RootStep> {
    let remove = words(&[&tool("rm"), "-f", unchecked]);
    vec![
        RootStep {
            why: format!("{why}; it is not active until it passes the next step"),
            argv: words(&[
                &tool("install"),
                "-o",
                "root",
                "-g",
                "root",
                "-m",
                "0440",
                "/dev/stdin",
                unchecked,
            ]),
            input: Some(text),
            on_failure: Some(remove.clone()),
            verify: None,
        },
        RootStep {
            why: "check that rule with visudo".to_owned(),
            argv: words(&[&tool("visudo"), "-c", "-f", unchecked]),
            input: None,
            on_failure: Some(remove.clone()),
            verify: None,
        },
        RootStep {
            why: "put the checked rule in place".to_owned(),
            argv: words(&[&tool("mv"), unchecked, finished]),
            input: None,
            on_failure: Some(remove),
            verify: None,
        },
    ]
}

/// The steps that write the rule file of §17.8 and check it: the temporary file, `visudo -c -f`
/// on it, the rename, and `visudo -c` on the whole configuration, which removes the file again
/// where it fails.
#[must_use]
pub fn rule_steps(uid: u32, tool: &dyn Fn(&str) -> String) -> Vec<RootStep> {
    let finished = setgid::rule_file(uid).display().to_string();
    let mut steps = checked_rule_file(
        tool,
        "write the one sudo rule that makes sudo ask your account for its password",
        &rule_unchecked(uid),
        &finished,
        rule_text(uid),
    );
    steps.push(RootStep {
        why: "check the whole sudo configuration with visudo".to_owned(),
        argv: words(&[&tool("visudo"), "-c"]),
        input: None,
        on_failure: Some(words(&[&tool("rm"), "-f", &finished])),
        verify: None,
    });
    steps
}

/// The system group and user `lunchroom`, each where it is not there yet, made with the programs
/// the system has ([`Accounts`]).
fn account_steps(f: &Facts, tool: &dyn Fn(&str) -> String) -> Vec<RootStep> {
    let user = setgid::SYSTEM_USER;
    let mut steps = Vec::new();
    if !f.group_exists {
        let argv = match f.accounts {
            Accounts::Shadow => words(&[&tool("groupadd"), "--system", user]),
            // BusyBox: `addgroup -S` makes a system group.
            Accounts::Busybox => words(&[&tool("addgroup"), "-S", user]),
        };
        steps.push(step("make the system group lunchroom", argv));
    }
    if !f.user_exists {
        let argv = match f.accounts {
            Accounts::Shadow => words(&[
                &tool("useradd"),
                "--system",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                "--gid",
                user,
                user,
            ]),
            // BusyBox: `adduser -S` system user, `-D` no password, `-H` no home folder, `-s` its
            // shell (Alpine keeps `nologin` in /sbin), `-G` its group.
            Accounts::Busybox => words(&[
                &tool("adduser"),
                "-S",
                "-D",
                "-H",
                "-s",
                "/sbin/nologin",
                "-G",
                user,
                user,
            ]),
        };
        steps.push(step(
            "make the system user lunchroom, which cannot log in",
            argv,
        ));
    }
    steps
}

/// The steps of the store wall for `f` before the store is copied (§17.2); `tool` gives the
/// absolute path of a program.
#[must_use]
pub fn wall_steps(f: &Facts, tool: &dyn Fn(&str) -> String) -> Vec<RootStep> {
    let user = setgid::SYSTEM_USER;
    let home = setgid::store_home(f.uid);
    let home_s = home.display().to_string();
    let mut steps = account_steps(f, tool);
    steps.push(folder(
        "make the folder for the program, root's alone to change",
        tool,
        "root",
        "root",
        "755",
        setgid::PROGRAM_DIR,
    ));
    steps.push(folder(
        "make the folder for the stores",
        tool,
        "root",
        "root",
        "755",
        setgid::STORE_ROOT,
    ));
    steps.push(folder(
        "make your store folder, which only the group lunchroom can enter (you are not in it)",
        tool,
        "root",
        user,
        "2770",
        &home_s,
    ));
    // F2-AC4, A1.3: a verifier an earlier release wrote (mode 0644, group root) is given to the
    // group `lunchroom` and made readable by no one else; its contents, the passphrase's check,
    // stay as they are.
    if let Some(verifier) = &f.verifier {
        let verifier = verifier.display().to_string();
        steps.push(step(
            "give your passphrase verifier to the group lunchroom (you are not in it)",
            words(&[&tool("chown"), &format!("root:{user}"), &verifier]),
        ));
        steps.push(step(
            "make your passphrase verifier readable by root and the group lunchroom alone",
            words(&[&tool("chmod"), "0640", &verifier]),
        ));
    }
    if !f.running_is_installed {
        let mut install = step(
            "install this program, as it is running now, as the Lunchroom program: root's, group \
             lunchroom, with the set-group-id bit",
            words(&[
                &tool("install"),
                "-o",
                "root",
                "-g",
                user,
                "-m",
                "2755",
                &f.source.display().to_string(),
                setgid::PROGRAM,
            ]),
        );
        if let Some(hash) = &f.source_sha256 {
            install.verify = Some(Verify::Hash {
                path: setgid::PROGRAM.to_owned(),
                sha: hash.clone(),
            });
            install.on_failure = Some(words(&[&tool("rm"), "-f", setgid::PROGRAM]));
        }
        steps.push(install);
    }
    steps
}

/// What this process saw in the tree it holds open (F2 design, "Store wall", "How root reads what
/// this process holds"): each regular file's sha-256 and its path relative to the tree, and how many
/// other entries (links and the like, never folders) there are. A copy is the tree that was looked at
/// where root's check of it against this passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// (sha-256 hex, relative path) of every regular file. For a single file the path is empty.
    pub files: Vec<(String, String)>,
    /// Entries that are neither folders nor regular files.
    pub others: u64,
}

impl Listing {
    /// How many entries that are not folders the copy must hold: the files and the others.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.files.len() as u64 + self.others
    }

    /// The list in the format `sha256sum -c` reads, one `<sha-256 hex>  <path>` line for each
    /// file, in path order; a name with a backslash or a newline is written as `sha256sum` writes
    /// it (a leading `\`, the backslash as `\\`, the newline as `\n`). `only` names the file of a
    /// single-file listing.
    #[must_use]
    pub fn text(&self, only: Option<&str>) -> String {
        let mut lines: Vec<String> = self
            .files
            .iter()
            .map(|(sha, path)| {
                let name = only.unwrap_or(path);
                if name.contains(['\\', '\n']) {
                    let name = name.replace('\\', "\\\\").replace('\n', "\\n");
                    format!("\\{sha}  {name}\n")
                } else {
                    format!("{sha}  {name}\n")
                }
            })
            .collect();
        lines.sort();
        lines.concat()
    }
}

/// Where one copy reads from: the argument of `cp` (the real path of the old folder or file, the
/// one the plan shows, never a `/proc` path), whether `cp` follows it, the most bytes the copy may
/// come out at, and what this process saw in the tree it holds open (none: the plan as it is shown
/// before anything is looked at).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub copy: String,
    pub follow: bool,
    pub max_bytes: Option<u64>,
    pub listing: Option<Listing>,
}

impl Source {
    /// The path as it is, for what is shown to the person before anything is done.
    fn of(path: &Path) -> Self {
        let text = path.display().to_string();
        Self {
            copy: text,
            follow: false,
            max_bytes: None,
            listing: None,
        }
    }
}

/// The three sources of a store move.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    pub store: Option<Source>,
    pub records: Option<Source>,
    pub records_dir: Option<Source>,
}

/// One folder or file copied beside its place by root from the real path of the original, given to
/// the user and the group, checked against the list this process made of the tree it holds open,
/// and renamed into place last, so a copy that stops half way, or is not the tree that was looked
/// at, is never taken for the store (§17.2; F2 design, "Store wall").
///
/// The check replaces a `diff -r` of the copy with the original's path: that reads the path again,
/// where the tree that was looked at is the one held open, so it can neither prove the copy is
/// that tree nor stay silent when the path changed after the look; the list can.
fn copy_steps(
    tool: &dyn Fn(&str) -> String,
    what: &str,
    src: &Source,
    new: &Path,
    folder: bool,
    owner: &str,
) -> Vec<RootStep> {
    let beside = format!("{}.new", new.display());
    let new_s = new.display().to_string();
    let mut copy = vec![tool("timeout"), LIMITS.seconds.to_string(), tool("cp")];
    if folder {
        copy.extend(words(&["-R"]));
    }
    copy.extend(words(&[
        if src.follow { "-L" } else { "-P" },
        &src.copy,
        &beside,
    ]));
    let mut chown = vec![tool("chown")];
    let mut chmod = vec![tool("chmod")];
    if folder {
        chown.push("-R".to_owned());
        chmod.push("-R".to_owned());
    }
    chown.extend(words(&[owner, &beside]));
    chmod.extend(words(&[
        if folder { "u=rwX,g=rwX,o=" } else { "660" },
        &beside,
    ]));
    // `sha256sum -c` runs in the copy's folder (for a file, the folder it is in) and reads the list
    // as its standard input.
    let (run_in, only) = if folder {
        (beside.clone(), None)
    } else {
        let path = Path::new(&beside);
        (
            path.parent()
                .map_or_else(|| ".".to_owned(), |d| d.display().to_string()),
            path.file_name().and_then(|n| n.to_str()),
        )
    };
    let remove = words(&[&tool("rm"), "-rf", &beside]);
    let with_cleanup = |mut s: RootStep| {
        s.on_failure = Some(remove.clone());
        s
    };
    let mut steps = vec![
        step(
            &format!("remove a copy of {what} that an earlier try left half made"),
            words(&[&tool("rm"), "-rf", &beside]),
        ),
        with_cleanup(step(
            &format!("copy {what} to the new folder, beside its place (the old one is kept)"),
            copy,
        )),
        with_cleanup(step(
            "give the copy to you, with the group lunchroom",
            chown,
        )),
        with_cleanup(step(
            "let the group read and write the copy, and no one else",
            chmod,
        )),
    ];
    // A tree with no regular file has no line to check (`sha256sum -c` refuses an empty list); the
    // count below still holds it to what was seen.
    if src.listing.as_ref().is_none_or(|l| !l.files.is_empty()) {
        let mut check = step(
            "check every file of the copy against the list of the files this process holds open \
             and their sha-256",
            words(&[
                &tool("timeout"),
                &LIMITS.seconds.to_string(),
                &tool("env"),
                "-C",
                &run_in,
                &tool(HASH_TOOL),
                "-c",
                "--strict",
                "--quiet",
                "/dev/stdin",
            ]),
        );
        check.input = src.listing.as_ref().map(|l| l.text(only));
        // A copy larger than what was measured is never taken for the store.
        check.verify = src.max_bytes.map(|max| Verify::MaxBytes {
            path: beside.clone(),
            max,
        });
        steps.push(with_cleanup(check));
    }
    // A file the list lacks (added, or a link) is not seen by the check above: the copy holds
    // exactly as many entries that are not folders as the list has.
    let mut count = step(
        "check that the copy holds no file besides those of the list",
        words(&[&tool("test"), "-e", &beside]),
    );
    count.verify = src.listing.as_ref().map(|l| Verify::Entries {
        path: beside.clone(),
        expected: l.entries(),
    });
    steps.push(with_cleanup(count));
    steps.push(with_cleanup(step(
        &format!("put the checked copy of {what} in place"),
        words(&[&tool("mv"), &beside, &new_s]),
    )));
    steps
}

/// The steps that move the old store, its records and the folder of its waiting records into the
/// new folder (§17.2), each only when it is there and the new one is not.
#[must_use]
pub fn copy_store_steps(f: &Facts, tool: &dyn Fn(&str) -> String) -> Vec<RootStep> {
    let shown = Sources {
        store: f.old_store.as_deref().map(Source::of),
        records: f.old_records.as_deref().map(Source::of),
        records_dir: f.old_records_dir.as_deref().map(Source::of),
    };
    copy_store_steps_from(f, tool, &shown)
}

/// As [`copy_store_steps`], reading from `sources` (the descriptors the install holds open).
#[must_use]
pub fn copy_store_steps_from(
    f: &Facts,
    tool: &dyn Fn(&str) -> String,
    sources: &Sources,
) -> Vec<RootStep> {
    let home = setgid::store_home(f.uid);
    let owner = format!("{}:{}", f.uid, setgid::SYSTEM_USER);
    let mut steps = Vec::new();
    if let Some(src) = &sources.store {
        steps.extend(copy_steps(
            tool,
            "your store, every queued message with it",
            src,
            &home.join("store"),
            true,
            &owner,
        ));
    }
    if let Some(src) = &sources.records {
        steps.extend(copy_steps(
            tool,
            "the guard's records",
            src,
            &home.join("guard-records.jsonl"),
            false,
            &owner,
        ));
    }
    if let Some(src) = &sources.records_dir {
        steps.extend(copy_steps(
            tool,
            "the guard's waiting records",
            src,
            &home.join("guard-records.d"),
            true,
            &owner,
        ));
    }
    steps
}

/// The steps of the console for `f` (§17.9, §17.8) before the public key. `keys` says what the
/// key steps do.
#[must_use]
#[allow(clippy::too_many_lines)] // the steps, in order, one after another
pub fn console_steps(f: &Facts, tool: &dyn Fn(&str) -> String, keys: &KeyPlan) -> Vec<RootStep> {
    let user = setgid::SYSTEM_USER;
    let approvals = setgid::approvals_home(f.uid);
    let private_path = setgid::console_key(f.uid).display().to_string();
    let public_path = setgid::console_pub(f.uid).display().to_string();
    let mut steps = vec![
        folder(
            "make the folder of the passphrase checks and the console's keys, root's alone to change",
            tool,
            "root",
            "root",
            "755",
            setgid::KEY_DIR,
        ),
        folder(
            "make the folder for the delivery folders of the controller's approvals",
            tool,
            "root",
            "root",
            "755",
            setgid::APPROVALS_ROOT,
        ),
        folder(
            "make your delivery folder: the console writes approvals there, you can read them",
            tool,
            user,
            &f.primary_group,
            "2750",
            &approvals.display().to_string(),
        ),
        folder(
            "make the folder where you (or the controller, as you) leave approval requests for the \
             console to read",
            tool,
            &f.uid.to_string(),
            user,
            "2750",
            &approvals.join("requests").display().to_string(),
        ),
    ];
    steps.extend(checked_rule_file(
        tool,
        &format!(
            "write the sudo rule that lets you, and no one else, start the console as {user} (it \
             asks for your sudo password)"
        ),
        LAUNCH_RULE_UNCHECKED,
        LAUNCH_RULE,
        sudoers_text(&f.developer, f.uid),
    ));
    if matches!(f.keys, Keys::PublicOnly) {
        steps.push(step(
            "remove the public key that has no private key with it",
            words(&[&tool("rm"), "-f", &public_path]),
        ));
    }
    if matches!(keys, KeyPlan::Make) {
        // The seed is made here, as root, from the system's random source, and never reaches this
        // process: the file is written to a name of its own, given to the user `lunchroom`, made
        // read-only, and renamed into place.
        let beside = format!("{private_path}.new");
        let raw = format!("{private_path}.raw");
        let remove = words(&[&tool("rm"), "-f", &beside, &raw]);
        // No pipeline: a failed read ends the chain with a failure, a short read is refused by
        // `iflag=fullblock`, and the file is checked afterwards in any case.
        let script = format!(
            "umask 077 && {dd} if=/dev/urandom of=\"$1\" bs=32 count=1 iflag=fullblock \
             status=none && {base64} -w0 \"$1\" > \"$0\" && echo >> \"$0\" && {rm} -f \"$1\"",
            dd = tool("dd"),
            base64 = tool("base64"),
            rm = tool("rm")
        );
        let mut make = step(
            "make the console's private key from the system's random source, as root",
            vec![tool("sh"), "-c".to_owned(), script, beside.clone(), raw],
        );
        make.on_failure = Some(remove.clone());
        make.verify = Some(Verify::KeyFile(beside.clone()));
        steps.push(make);
        let owner = format!("{user}:{user}");
        let mut give = step(
            "give the private key to the user lunchroom",
            words(&[&tool("chown"), &owner, &beside]),
        );
        give.on_failure = Some(remove.clone());
        steps.push(give);
        let mut lock = step(
            "make the private key readable by lunchroom alone (mode 0400)",
            words(&[&tool("chmod"), "0400", &beside]),
        );
        lock.on_failure = Some(remove.clone());
        steps.push(lock);
        let mut place = step(
            "put the private key in place",
            words(&[&tool("mv"), &beside, &private_path]),
        );
        place.on_failure = Some(remove);
        steps.push(place);
    }
    if f.state_exists {
        steps.push(step(
            "remove the record of why an earlier set-up stopped",
            words(&[
                &tool("rm"),
                "-f",
                &setgid::console_state(f.uid).display().to_string(),
            ]),
        ));
    }
    steps
}

/// The steps that put the public key in place: the last of the set-up, after every other step
/// and the hooks (§17.9). None where the pair is kept.
#[must_use]
pub fn pub_steps(f: &Facts, tool: &dyn Fn(&str) -> String, keys: &KeyPlan) -> Vec<RootStep> {
    if matches!(keys, KeyPlan::Keep) {
        return Vec::new();
    }
    let public_path = setgid::console_pub(f.uid).display().to_string();
    let unchecked = format!("{public_path}.new");
    let remove = words(&[&tool("rm"), "-f", &unchecked]);
    let mut write = step(
        "write the console's public key beside the private one: root's, readable by all",
        words(&[
            &tool("install"),
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "0644",
            "/dev/stdin",
            &unchecked,
        ]),
    );
    write.input = Some(PUBLIC_KEY_LATER.to_owned());
    write.on_failure = Some(remove.clone());
    let mut place = step(
        "put the public key in place, last: its presence tells the controller the console is \
         installed",
        words(&[&tool("mv"), &unchecked, &public_path]),
    );
    place.on_failure = Some(remove);
    vec![write, place]
}

/// The steps as the person reads them before any is done.
#[must_use]
pub fn steps_text(steps: &[RootStep]) -> String {
    let mut out = String::new();
    for (n, s) in steps.iter().enumerate() {
        let _ = writeln!(out, "{}. {}\n     {}", n + 1, s.why, s.argv.join(" "));
        if let Some(input) = &s.input {
            for line in input.lines() {
                let _ = writeln!(out, "     | {line}");
            }
        }
    }
    out
}

/// Points each link `~/.local/bin/lunchroom` and `~/bin/lunchroom` that leads to `old` at `new`,
/// by making a new link beside it and renaming it over; the lines of what was done.
#[must_use]
pub fn repoint_links(home: &Path, old: &Path, new: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for dir in [home.join(".local/bin"), home.join("bin")] {
        let link = dir.join("lunchroom");
        if fs::read_link(&link).ok().as_deref() != Some(old) {
            continue;
        }
        let beside = dir.join(format!("lunchroom.lunchroom-new-{}", std::process::id()));
        let _ = fs::remove_file(&beside);
        let made =
            std::os::unix::fs::symlink(new, &beside).and_then(|()| fs::rename(&beside, &link));
        lines.push(match made {
            Ok(()) => format!("linked {} to {}\n", link.display(), new.display()),
            Err(e) => {
                let _ = fs::remove_file(&beside);
                format!(
                    "{} was not re-linked ({e}); run Lunchroom by its path, {}\n",
                    link.display(),
                    new.display()
                )
            }
        });
    }
    lines
}

/// Renames the old store and records beside themselves to `<name>.moved-<stamp>`; the lines of
/// what was done. Nothing is deleted.
#[must_use]
pub fn move_old_aside(old_home: &Path, now: u64) -> Vec<String> {
    let mut lines = Vec::new();
    for name in ["store", "guard-records.jsonl", "guard-records.d"] {
        let from = old_home.join(name);
        if fs::symlink_metadata(&from).is_err() {
            continue;
        }
        let to = old_home.join(format!("{name}.moved-{}", stamp(now)));
        lines.push(match fs::rename(&from, &to) {
            Ok(()) => format!(
                "moved {} to {} (kept, not deleted)\n",
                from.display(),
                to.display()
            ),
            Err(e) => format!(
                "{} was not moved ({e}); move it aside yourself\n",
                from.display()
            ),
        });
    }
    lines
}

/// What the set-up depends on, read from the computer through `sys`.
fn gather(env: &Env, sys: &mut dyn System, uid: u32) -> Result<Facts, String> {
    let passwd = sys
        .read_text(Path::new("/etc/passwd"))
        .ok_or("/etc/passwd cannot be read")?;
    let group = sys
        .read_text(Path::new("/etc/group"))
        .ok_or("/etc/group cannot be read")?;
    let developer = setgid::name_from_passwd(&passwd, uid)
        .ok_or_else(|| format!("user {uid} has no name in /etc/passwd"))?;
    if !valid_account(&developer) {
        return Err(format!(
            "the account name {developer:?} has a control character; nothing was changed"
        ));
    }
    let primary_group = setgid::primary_group_name(&passwd, &group, uid)
        .ok_or_else(|| format!("the primary group of {developer} has no name in /etc/group"))?;
    let source = sys.program_source()?;
    let new_home = setgid::store_home(uid);
    let new_store_there = sys.exists(&new_home.join("store"));
    let old_store = env.store_dir();
    let old_records = crate::records::file(&env.lunchroom_home);
    let old_records_dir = env.lunchroom_home.join("guard-records.d");
    // Files an earlier try left, which this set-up neither uses nor removes.
    let key = setgid::console_key(uid).display().to_string();
    let leftovers: Vec<PathBuf> = [
        format!("{key}.new"),
        format!("{key}.raw"),
        format!("{}.new", setgid::console_pub(uid).display()),
        LAUNCH_RULE_UNCHECKED.to_owned(),
        rule_unchecked(uid),
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|p| sys.exists(p))
    .collect();
    let keys = match (
        sys.exists(&setgid::console_key(uid)),
        sys.exists(&setgid::console_pub(uid)),
    ) {
        (true, true) => Keys::Both,
        (true, false) => Keys::PrivateOnly,
        (false, true) => Keys::PublicOnly,
        (false, false) => Keys::Neither,
    };
    Ok(Facts {
        uid,
        developer,
        primary_group,
        source: source.path,
        source_sha256: source.sha256,
        running_is_installed: source.is_installed,
        group_exists: setgid::gid_from_group(&group, setgid::SYSTEM_USER).is_some(),
        user_exists: setgid::uid_from_passwd(&passwd, setgid::SYSTEM_USER).is_some(),
        accounts: accounts_here().unwrap_or(Accounts::Shadow),
        old_store: (is_real(&old_store, Want::Folder)? && !new_store_there).then_some(old_store),
        old_records: (is_real(&old_records, Want::File)?
            && !sys.exists(&new_home.join("guard-records.jsonl")))
        .then_some(old_records),
        old_records_dir: (is_real(&old_records_dir, Want::Folder)?
            && !sys.exists(&new_home.join("guard-records.d")))
        .then_some(old_records_dir),
        leftovers,
        verifier: {
            let verifier = crate::terminal::verifier_path(uid);
            sys.exists(&verifier).then_some(verifier)
        },
        keys,
        state_exists: sys.exists(&setgid::console_state(uid)),
        approvals_there: sys.exists(&setgid::approvals_home(uid)),
        launch_rule_there: sys.exists(Path::new(LAUNCH_RULE)),
    })
}

/// The programs an earlier Lunchroom's listeners run: the installed program of this folder, and
/// where the `lunchroom` links of the user lead.
fn earlier_programs(env: &Env, old: &Path) -> Vec<PathBuf> {
    let mut found = vec![old.to_path_buf()];
    let mut more = Vec::new();
    for candidate in [old.to_path_buf()].into_iter().chain([
        env.home.join(".local/bin/lunchroom"),
        env.home.join("bin/lunchroom"),
    ]) {
        if let Ok(real) = fs::canonicalize(&candidate) {
            more.push(real);
        }
    }
    found.extend(more);
    found.sort();
    found.dedup();
    found
}

/// The checks every set-up starts with: a root program, the user id, the programs the steps run,
/// the facts. `Err` is the outcome to return.
fn start(
    env: &Env,
    sys: &mut dyn System,
    doing: &str,
    console: bool,
) -> Result<(u32, Facts), Outcome> {
    // Before any question or key is handled: no process of the user's can read this one's memory.
    if let Err(why) = sys.protect_memory() {
        return Err(Outcome::failed(&format!("{why}; nothing was changed"), 1));
    }
    if let Some(why) = sys.root_blocker() {
        return Err(Outcome::failed(
            &format!("{doing} cannot be done here: {why}; nothing was changed"),
            3,
        ));
    }
    let Some(uid) = sys.uid() else {
        return Err(Outcome::failed(
            "the user id of this process cannot be read; nothing was changed",
            1,
        ));
    };
    let mut missing: Vec<&str> = required_tools(console)
        .into_iter()
        .filter(|t| system_path(t).is_none())
        .collect();
    if accounts_here().is_none() {
        missing.push("groupadd and useradd (or addgroup and adduser)");
    }
    if !missing.is_empty() {
        return Err(Outcome::failed(
            &format!(
                "these programs were not found in the system folders: {}; nothing was changed",
                missing.join(", ")
            ),
            3,
        ));
    }
    let facts = gather(env, sys, uid).map_err(|why| Outcome::failed(&why, 1))?;
    Ok((uid, facts))
}

fn tool(name: &str) -> String {
    system_path(name).map_or_else(|| name.to_owned(), |p| p.display().to_string())
}

/// Runs `steps` as root, one after another; the lines of what was done. `Err` holds the lines up
/// to the step that failed and what to do.
fn run_steps(sys: &mut dyn System, facts: &Facts, steps: &[RootStep]) -> Result<String, String> {
    let mut report = String::new();
    for (n, s) in steps.iter().enumerate() {
        let fail = |report: &mut String, sys: &mut dyn System, e: &str| {
            if let Some(cleanup) = &s.on_failure {
                let _ = sys.run_root("remove the file that did not pass", cleanup, None);
            }
            let _ = writeln!(
                report,
                "stopped at step {} ({}): {e}\nNothing after it was done. Run the command again \
                 after fixing that.",
                n + 1,
                s.why
            );
        };
        let input = if s.input.as_deref() == Some(PUBLIC_KEY_LATER) {
            match public_from_private(sys, facts.uid) {
                Ok(text) => Some(text),
                Err(e) => {
                    fail(&mut report, sys, &e);
                    return Err(report);
                }
            }
        } else {
            s.input.clone()
        };
        let ran = sys
            .run_root(&s.why, &s.argv, input.as_deref())
            .and_then(|()| match &s.verify {
                Some(Verify::Hash { path, sha }) => verify_installed(sys, path, sha),
                Some(Verify::KeyFile(path)) => verify_key_file(sys, path),
                Some(Verify::MaxBytes { path, max }) => verify_max_bytes(sys, path, *max),
                Some(Verify::Entries { path, expected }) => verify_entries(sys, path, *expected),
                None => Ok(()),
            });
        match ran {
            Ok(()) => {
                let _ = writeln!(report, "done {}/{}: {}", n + 1, steps.len(), s.why);
            }
            Err(e) => {
                fail(&mut report, sys, &e);
                return Err(report);
            }
        }
    }
    Ok(report)
}

/// Reads the sha-256 of `path` as root and compares it with `sha`: the program installed is the
/// program that was running.
fn verify_installed(sys: &mut dyn System, path: &str, sha: &str) -> Result<(), String> {
    let out = sys.read_root(
        "read the sha-256 of the installed program",
        &words(&[&tool(HASH_TOOL), path]),
    )?;
    let got = out.split_whitespace().next().unwrap_or_default();
    if got.eq_ignore_ascii_case(sha) {
        Ok(())
    } else {
        Err(format!(
            "{path} is not the program that was running (its sha-256 is {got:?}, the running \
             program's is {sha}); it was removed"
        ))
    }
}

/// Reads, as root, one `.` for each entry of the tree at `path` that is not a folder (`find
/// -printf`, so no name is read) and compares their number with `expected`: the copy holds no
/// file the list lacks.
fn verify_entries(sys: &mut dyn System, path: &str, expected: u64) -> Result<(), String> {
    let out = sys.read_root(
        "count the files of the copy",
        &words(&[&tool("find"), path, "!", "-type", "d", "-printf", "."]),
    )?;
    let got = out.chars().count() as u64;
    if got == expected && out.chars().all(|c| c == '.') {
        Ok(())
    } else {
        Err(format!(
            "the copy {path} holds {got} files, the list of what was held open has {expected}: a \
             file was added or removed while the store was copied; it was removed and nothing was \
             moved"
        ))
    }
}

/// Reads the size of the tree at `path` as root (`du -sb`) and compares it with `max`.
fn verify_max_bytes(sys: &mut dyn System, path: &str, max: u64) -> Result<(), String> {
    let out = sys.read_root(
        "read the size of the copy",
        &words(&[&tool("du"), "-sb", path]),
    )?;
    let got: u64 = out
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("the size of {path} could not be read ({out:?})"))?;
    if got <= max {
        Ok(())
    } else {
        Err(format!(
            "the copy {path} is {got} bytes, more than the {max} measured before it was made: the \
             old store changed while it was copied; it was removed and nothing was moved"
        ))
    }
}

/// Reads the key file at `path` as root: it must be exactly the base64 of 32 bytes (44
/// characters) and a newline, nothing else (a failed or short read, a truncated file).
fn verify_key_file(sys: &mut dyn System, path: &str) -> Result<(), String> {
    let text = sys.read_root("read the new private key", &words(&[&tool("cat"), path]))?;
    if text.len() == 45 && console_keys::seed_from(&text).is_some() {
        Ok(())
    } else {
        Err(format!(
            "{path} does not hold exactly the base64 of 32 bytes and a newline ({} characters \
             were read); it was removed",
            text.len()
        ))
    }
}

/// The public key's text for the private key that is on the computer, read as root.
fn public_from_private(sys: &mut dyn System, uid: u32) -> Result<String, String> {
    let key = setgid::console_key(uid).display().to_string();
    let text = sys.read_root(
        "read the console's private key",
        &words(&[&tool("cat"), &key]),
    )?;
    let seed = console_keys::seed_from(&text).ok_or_else(|| {
        format!(
            "{key} is not a private key this program made (one line of base64 holding 32 bytes); \
             remove it as root and run the command again to make a new pair"
        )
    })?;
    Ok(console_keys::public_text(&seed))
}

/// Writes the record of why the console was not set up, as root; the line to add to the answer
/// where that failed.
fn record_state(sys: &mut dyn System, uid: u32, kind: Kind, reason: &str, now: u64) -> String {
    let state = State {
        kind,
        reason: reason.to_owned(),
        at: now,
    };
    let path = setgid::console_state(uid).display().to_string();
    let made = sys.run_root(
        "make the folder of the passphrase checks",
        &words(&[
            &tool("install"),
            "-d",
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "755",
            setgid::KEY_DIR,
        ]),
        None,
    );
    let written = made.and_then(|()| {
        sys.run_root(
            "record why the console was not set up, for `lunchroom doctor`",
            &words(&[
                &tool("install"),
                "-o",
                "root",
                "-g",
                "root",
                "-m",
                "0644",
                "/dev/stdin",
                &path,
            ]),
            Some(&state.to_json()),
        )
    });
    match written {
        Ok(()) => String::new(),
        Err(e) => format!(
            "The reason could not be recorded for `lunchroom doctor` ({e}); run `lunchroom install \
             console` again to see it.\n"
        ),
    }
}

/// The lines of `sudo -n -l` that grant a command without a password, as `sudo` prints them.
fn password_free_lines(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("NOPASSWD"))
        .map(str::to_owned)
        .collect()
}

/// §17.8: on a wide rule, stops before anything is set up, asks whether to make `sudo` ask this
/// account for its password, and on a typed `yes` writes the rule and proves it is in force.
#[allow(clippy::too_many_lines)] // one flow: ask, write, prove, undo
fn wide_rule(
    env: &Env,
    sys: &mut dyn System,
    facts: &Facts,
    verdict: &Verdict,
) -> Result<(), Outcome> {
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Err(Outcome::failed(&crate::cli::unread(e), 1)),
    };
    // The answer is read as A1.3 reads a confirmation: from a controlling terminal that no guarded
    // harness process owns, with no guarded session above this command.
    check(&mail, sys).map_err(|why| Outcome::denied(&why, 3))?;
    let rule = rule_text(facts.uid);
    let file = setgid::rule_file(facts.uid);
    let question = format!(
        "Lunchroom: the console's wall does not hold on this account. `sudo -n -l` shows a rule \
         that lets it run a command without a password, and a process of yours (an agent's \
         included) can then act as root and as the user {user}, read the store, kill the console \
         and read its terminal, around the guard's rules:\n{wide}\nMake `sudo` ask this account \
         for its password? Lunchroom would write this one rule, as root, in the new file {file}, \
         and check it with visudo before it counts:\n  {rule}Whatever else on the account runs \
         `sudo` without a password (a script or a scheduled job) would then stop running \
         unattended.\n",
        user = setgid::SYSTEM_USER,
        wide = verdict
            .wide_lines()
            .iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        file = file.display(),
        rule = rule,
    );
    // The A1.3 confirmation comes first, with the question shown: the passphrase where there is
    // one (before the first passphrase, the root program's own prompt). A process that only has a
    // terminal of its own never reaches the `yes` line below.
    if let Err(why) = confirm_or_begin(&mail, sys, &question) {
        let reason = format!(
            "the answer was not confirmed ({why}), so no rule was written and the console was not \
             set up"
        );
        let note = record_state(sys, facts.uid, Kind::Declined, &reason, env.now);
        return Err(Outcome::failed(&format!("{reason}.\n{note}"), 3));
    }
    let answer = sys
        .ask("Type yes to write the rule; anything else writes nothing and sets nothing up: ")
        .ok_or_else(|| {
            Outcome::denied(
                "this command has no controlling terminal to read the user's answer from",
                3,
            )
        })?;
    // A line longer than a word is no `yes`, however much of it is spaces.
    if answer.len() > MAX_YES_BYTES || !answer.trim().eq_ignore_ascii_case("yes") {
        let reason = "the answer to making sudo ask this account for its password was not yes, \
                      so no rule was written and the console was not set up";
        let note = record_state(sys, facts.uid, Kind::Declined, reason, env.now);
        return Err(Outcome::failed(
            &format!(
                "{reason}. The console's wall does not hold on this account until a rule that \
                 runs a command without a password is removed from sudo's configuration, or the \
                 question is answered yes.\n{note}"
            ),
            3,
        ));
    }
    let steps = rule_steps(facts.uid, &tool);
    let mut shown = String::new();
    if let Err(report) = run_steps(sys, facts, &steps) {
        let reason = "the sudo rule could not be put in place; it was removed again";
        let note = record_state(sys, facts.uid, Kind::NotInForce, reason, env.now);
        return Err(Outcome::failed(&format!("{report}{note}"), 1));
    }
    let _ = writeln!(shown, "wrote {}", file.display());
    // With the file in place: forget what sudo remembers and try a command without a password.
    let probe = sys.sudo_probe();
    if matches!(probe, Ok(SudoProbe::PasswordRequired)) {
        return Ok(());
    }
    // Not in force: the file is removed again and nothing is set up.
    let removed = sys.run_root(
        "remove the rule that is not in force",
        &words(&[&tool("rm"), "-f", &file.display().to_string()]),
        None,
    );
    let later = sys
        .sudo_list()
        .map(|t| password_free_lines(&t))
        .unwrap_or_default();
    let shown_rule = if later.is_empty() {
        "  (`sudo -n -l` shows no line of that kind now; see it with `sudo -n -l`)".to_owned()
    } else {
        later
            .iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let why = match &probe {
        Ok(SudoProbe::RunsAsLunchroom) => "a rule `sudo` reads later lets the account run a \
                                           command as the user or the group lunchroom without a \
                                           password"
            .to_owned(),
        Ok(SudoProbe::Other(text)) => {
            format!("`sudo -n true` was not proven to ask for a password (it printed: {text})")
        }
        Err(e) => format!("`sudo -n true` could not be run: {e}"),
        _ => "a rule `sudo` reads later still lets the account run a command without a password"
            .to_owned(),
    };
    let reason = format!("{why}; the rule file was removed and the console was not set up");
    let note = record_state(sys, facts.uid, Kind::NotInForce, &reason, env.now);
    let cleanup = match removed {
        Ok(()) => format!("{shown}and removed it again.\n"),
        Err(e) => format!("{shown}and could not remove it again ({e}); remove it as root.\n"),
    };
    Err(Outcome::failed(
        &format!(
            "{cleanup}{why}. The rule still in force:\n{shown_rule}\nThe change is yours to make \
             in /etc/sudoers (or the file in /etc/sudoers.d that holds it): remove that rule, or \
             give it PASSWD:. Whatever else on this account uses that rule stops running \
             unattended once you do. Then run `lunchroom install console` again. Nothing was set \
             up.\n{note}"
        ),
        1,
    ))
}

/// `sudo -n -l` read for the developer, with the other users' rules read in the root step, kept,
/// and classified; `None` where `sudo` could not be run.
fn read_verdict(sys: &mut dyn System, facts: &Facts) -> Option<Verdict> {
    let kept = read_sudo_answer(sys, &facts.developer, true).ok()?;
    sys.keep_sudo_answer(&kept);
    Some(kept.verdict(&facts.developer, &sudoers::facts_from_disk))
}

/// What the install says where a rule that runs a command without a password is wide.
fn wall_does_not_hold(verdict: &Verdict) -> String {
    let lines: Vec<String> = verdict
        .wide_lines()
        .iter()
        .map(|l| format!("  {l}"))
        .collect();
    format!(
        "\nThe store wall does not hold on this account until that rule is removed: `sudo -n -l` \
         shows a rule that lets it run a command without a password, wide because of:\n{}\n\
         `lunchroom install console` can make `sudo` ask for the password.\n",
        lines.join("\n")
    )
}

/// The locks of the old store and of the old records, held while the store is copied: a hook or
/// a message that writes the old folder waits, so the files are copied at one instant and nothing
/// written after the copy is lost into the folder moved aside (§17.2). A lock file that is not
/// there is made and locked (a listener cannot make it after the check and use it), and one that
/// is a symbolic link is refused.
#[derive(Debug)]
pub struct OldStoreLock {
    /// The files whose locks are held until this is dropped.
    #[allow(dead_code)]
    held: Vec<fs::File>,
}

impl OldStoreLock {
    /// The locks in `home` (Lunchroom's old folder), each waited for up to 30 seconds; `Err` names
    /// the one that could not be taken.
    ///
    /// # Errors
    /// The lock that could not be made or taken, or that is a symbolic link.
    pub fn take(home: &Path) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut held = Vec::new();
        for name in ["store/state.flock", "guard-records.flock"] {
            let path = home.join(name);
            // Nothing to race where the folder the lock lives in is not there.
            if !path
                .parent()
                .is_some_and(|dir| matches!(is_real(dir, Want::Folder), Ok(true)))
            {
                continue;
            }
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0))
                .open(&path)
                .map_err(|e| {
                    format!(
                        "{} cannot be made or opened as a plain file ({e}), so the store was not \
                         moved",
                        path.display()
                    )
                })?;
            let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match file.try_lock() {
                    Ok(()) => break,
                    Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < end => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(e) => {
                        return Err(format!(
                            "{} stayed in the hands of another process ({e}), so the store was \
                             not moved",
                            path.display()
                        ));
                    }
                }
            }
            held.push(file);
        }
        Ok(Self { held })
    }
}

/// How much of the old store Lunchroom copies, and for how long: more than this stops the install
/// before anything is moved (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in all, links counted as nothing.
    pub bytes: u64,
    /// Files, folders and links in all.
    pub entries: u64,
    /// Folders below the one named.
    pub depth: usize,
    /// Seconds one `cp` or one `diff` may run before it is stopped.
    pub seconds: u64,
}

/// The limits of an install: 2 GiB, 500,000 entries, 64 levels, 10 minutes a command.
pub const LIMITS: Limits = Limits {
    bytes: 2 * 1024 * 1024 * 1024,
    entries: 500_000,
    depth: 64,
    seconds: 600,
};

/// The bytes and entries under `path` (a file, or a folder walked without following a link), or
/// the reason it is over `limits`.
///
/// # Errors
/// That a limit is passed, or that something could not be read.
pub fn measure_tree(path: &Path, limits: &Limits) -> Result<(u64, u64), String> {
    let (mut bytes, mut entries) = (0u64, 0u64);
    let mut todo = vec![(path.to_path_buf(), 0usize)];
    while let Some((next, depth)) = todo.pop() {
        let meta = fs::symlink_metadata(&next)
            .map_err(|e| format!("{} cannot be read: {e}", next.display()))?;
        entries += 1;
        if entries > limits.entries {
            return Err(format!(
                "{} holds more than {} files and folders, which is more than Lunchroom copies",
                path.display(),
                limits.entries
            ));
        }
        if meta.file_type().is_symlink() {
            continue;
        }
        bytes = bytes.saturating_add(meta.len());
        if bytes > limits.bytes {
            return Err(format!(
                "{} holds more than {} bytes, which is more than Lunchroom copies",
                path.display(),
                limits.bytes
            ));
        }
        if meta.is_dir() {
            if depth >= limits.depth {
                return Err(format!(
                    "{} is nested deeper than {} folders, which is more than Lunchroom copies",
                    path.display(),
                    limits.depth
                ));
            }
            let dir = fs::read_dir(&next)
                .map_err(|e| format!("{} cannot be read: {e}", next.display()))?;
            for entry in dir {
                let entry = entry.map_err(|e| format!("{} cannot be read: {e}", next.display()))?;
                todo.push((entry.path(), depth + 1));
            }
        }
    }
    Ok((bytes, entries))
}

/// The sha-256 of the regular file at `path`, read in blocks, counting its size into `bytes`
/// against `limits`. With `nofollow` a link at `path` is an error, not followed.
fn hash_of(
    path: &Path,
    nofollow: bool,
    limits: &Limits,
    bytes: &mut u64,
) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut open = fs::OpenOptions::new();
    open.read(true);
    if nofollow {
        open.custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0));
    }
    let mut file = open
        .open(path)
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a plain file", path.display()));
    }
    *bytes = bytes.saturating_add(meta.len());
    if *bytes > limits.bytes {
        return Err(format!(
            "{} holds more than {} bytes, which is more than Lunchroom copies",
            path.display(),
            limits.bytes
        ));
    }
    lunchroom_core::jcs::sha256_hex_of(&mut file)
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))
}

/// What this process sees in the file it holds open at `path` (its own descriptor,
/// `/proc/self/fd/<n>`, which it may read): its sha-256, as a listing of one file.
///
/// # Errors
/// That it cannot be read, or is over `limits`.
pub fn list_file(path: &Path, limits: &Limits) -> Result<Listing, String> {
    let mut bytes = 0;
    let sha = hash_of(path, false, limits, &mut bytes)?;
    Ok(Listing {
        files: vec![(sha, String::new())],
        others: 0,
    })
}

/// What this process sees in the tree it holds open, walked from `root` (its own descriptor,
/// `/proc/self/fd/<n>/.`, which it may read) without following a link: the sha-256 and relative
/// path of every regular file, and the number of entries that are neither folders nor regular
/// files. A name that is not text is an error, never skipped.
///
/// # Errors
/// That something cannot be read, a name is not text, or a limit of `limits` is passed.
pub fn list_folder(root: &Path, limits: &Limits) -> Result<Listing, String> {
    let mut listing = Listing::default();
    let (mut bytes, mut entries) = (0u64, 0u64);
    let mut todo = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = todo.pop() {
        if depth > limits.depth {
            return Err(format!(
                "{} is nested deeper than {} folders, which is more than Lunchroom copies",
                root.display(),
                limits.depth
            ));
        }
        let read =
            fs::read_dir(&dir).map_err(|e| format!("{} cannot be read: {e}", dir.display()))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("{} cannot be read: {e}", dir.display()))?;
            entries += 1;
            if entries > limits.entries {
                return Err(format!(
                    "{} holds more than {} files and folders, which is more than Lunchroom copies",
                    root.display(),
                    limits.entries
                ));
            }
            let file_name = entry.file_name();
            let name = file_name
                .to_str()
                .ok_or_else(|| format!("{} holds a name that is not text", dir.display()))?;
            let rel_name = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
            if meta.is_dir() {
                todo.push((path, rel_name, depth + 1));
            } else if meta.is_file() {
                let sha = hash_of(&path, true, limits, &mut bytes)?;
                listing.files.push((sha, rel_name));
            } else {
                listing.others += 1;
            }
        }
    }
    Ok(listing)
}

/// Moves the old store into the new folder (§17.2); see [`move_store_with`].
fn move_store(
    env: &Env,
    sys: &mut dyn System,
    facts: &Facts,
    old: &Path,
) -> Result<String, Outcome> {
    move_store_with(env, sys, facts, old, &LIMITS)
}

/// Moves the old store into the new folder (§17.2): the earlier program's listeners are stopped
/// now, after every question has been answered, the old store's locks are taken, the old store,
/// records and waiting records are looked at again (a symbolic link in place of any of them, or
/// more than `limits`, stops the install before anything is copied), the copy is made beside its
/// place, compared and renamed in, and the listeners are looked for once more before the locks
/// are let go.
///
/// # Errors
/// The outcome to return where the store was not moved.
pub fn move_store_with(
    env: &Env,
    sys: &mut dyn System,
    facts: &Facts,
    old: &Path,
    limits: &Limits,
) -> Result<String, Outcome> {
    if copy_store_steps(facts, &tool).is_empty() {
        return Ok(String::new());
    }
    let programs = earlier_programs(env, old);
    let stopped = |why: String| {
        Outcome::failed(
            &format!("{why}; the store was not moved. Stop that process and run the command again"),
            1,
        )
    };
    let lock = OldStoreLock::take(&env.lunchroom_home).map_err(stopped)?;
    sys.stop_listeners(&programs, facts.uid).map_err(stopped)?;
    // Each old folder or file is opened once, here, without following a link, and held open until
    // the copy is done: it is measured and copied through that descriptor, so the tree that was
    // looked at is the tree that is copied, whatever happens to the path after this.
    let mut held: Vec<fs::File> = Vec::new();
    let mut sources = Sources::default();
    for (path, want, slot) in [
        (facts.old_store.as_ref(), Want::Folder, &mut sources.store),
        (facts.old_records.as_ref(), Want::File, &mut sources.records),
        (
            facts.old_records_dir.as_ref(),
            Want::Folder,
            &mut sources.records_dir,
        ),
    ] {
        let Some(path) = path else { continue };
        let file = hold(path, want).map_err(stopped)?;
        let fd = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
        let bytes = match want {
            Want::Folder => {
                let (bytes, _) = measure_tree(Path::new(&format!("{fd}/.")), limits)
                    .map_err(|e| e.replace(&fd, &path.display().to_string()))
                    .map_err(stopped)?;
                bytes
            }
            Want::File => {
                let len = file
                    .metadata()
                    .map_err(|e| format!("{} cannot be read: {e}", path.display()))
                    .map_err(stopped)?
                    .len();
                if len > limits.bytes {
                    return Err(stopped(format!(
                        "{} holds more than {} bytes, which is more than Lunchroom copies",
                        path.display(),
                        limits.bytes
                    )));
                }
                len
            }
        };
        // F2 design, "Store wall": this process lists every file of what it holds open, through
        // its own descriptor, with its sha-256. Root copies from the real path (the one the plan
        // shows, never a `/proc/<pid>/fd` path) and checks the copy against this list.
        let listing = match want {
            Want::Folder => list_folder(Path::new(&format!("{fd}/.")), limits),
            Want::File => list_file(Path::new(&fd), limits),
        }
        .map_err(|e| e.replace(&fd, &path.display().to_string()))
        .map_err(stopped)?;
        *slot = Some(Source {
            copy: path.display().to_string(),
            follow: false,
            max_bytes: Some(bytes + COPY_SLACK_BYTES),
            listing: Some(listing),
        });
        held.push(file);
    }
    let steps = copy_store_steps_from(facts, &tool, &sources);
    let report = run_steps(sys, facts, &steps).map_err(|report| Outcome::failed(&report, 1))?;
    sys.stop_listeners(&programs, facts.uid).map_err(stopped)?;
    drop(held);
    drop(lock);
    Ok(report)
}

/// The program's own `install`, the links and the old store set aside, after the wall: the lines
/// of what was done, or the outcome where the hooks were not written.
fn finish_wall(
    env: &Env,
    sys: &mut dyn System,
    args: &[String],
    old: &Path,
    mut report: String,
) -> Result<String, Outcome> {
    let program = Path::new(setgid::PROGRAM);
    let mut shown: Vec<&str> = vec!["install"];
    shown.extend(args.iter().map(String::as_str));
    // F2 design, item 1, exit codes: 0, and 6 (the hooks are written but the console setup
    // stopped), both mean the hooks run the new program; any other code means they do not.
    let code = sys.run_user(program, &shown);
    let console_stopped = code == Ok(i32::from(CONSOLE_NOT_SET_UP));
    if code == Ok(0) || console_stopped {
        let _ = writeln!(report, "the hooks now run {}", setgid::PROGRAM);
    } else {
        let _ = writeln!(
            report,
            "`{} install` did not finish, so the hooks still run the old program and your old \
             store was left where it is. Run `{} install` in a terminal of your own to finish.",
            setgid::PROGRAM,
            setgid::PROGRAM
        );
        return Err(Outcome::failed(&report, 1));
    }
    for line in repoint_links(&env.home, old, program) {
        report.push_str(&line);
    }
    for line in move_old_aside(&env.lunchroom_home, env.now) {
        report.push_str(&line);
    }
    report.push_str(SESSIONS_ANEW);
    if console_stopped {
        // The installed program printed why the console setup stopped and how to finish; this
        // install ends with the same code, so a caller sees that the console is not set up.
        return Err(Outcome {
            stdout: report,
            stderr: String::new(),
            code: CONSOLE_NOT_SET_UP,
        });
    }
    Ok(report)
}

/// The install's exit code where the hooks are written but the console setup stopped (F2 design,
/// "Finding and starting the console", item 1): a code no other command uses.
pub const CONSOLE_NOT_SET_UP: u8 = 6;

/// `lunchroom install`, where this system sets the store wall up and this process does not run as
/// the setgid program yet (§17.2): the wall, then the setgid program's own `install` for the
/// hooks. `args` are the install's arguments.
#[must_use]
pub fn install_with_wall(args: &[String], env: &Env, sys: &mut dyn System) -> Outcome {
    let (_uid, facts) = match start(env, sys, "the store wall", false) {
        Ok(started) => started,
        Err(out) => return out,
    };
    let verdict = read_verdict(sys, &facts);
    let mut steps = wall_steps(&facts, &tool);
    steps.extend(copy_store_steps(&facts, &tool));
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let old = installed_program(env);
    let question = format!(
        "Lunchroom: put its store and program where no process of yours can reach them? It runs \
         these commands as root, which asks for a password. Nothing is deleted.\n{}\nThen it \
         runs `{} install` so the hooks of your agents run the new program, points your \
         `lunchroom` link at it, and renames your old store to store.moved-{}.\n",
        steps_text(&steps),
        setgid::PROGRAM,
        stamp(env.now)
    );
    if let Err(reason) = confirm_or_begin(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let mut report = match run_steps(sys, &facts, &wall_steps(&facts, &tool)) {
        Ok(r) => r,
        Err(report) => return Outcome::failed(&report, 1),
    };
    match move_store(env, sys, &facts, &old) {
        Ok(more) => report.push_str(&more),
        Err(out) => return out,
    }
    // Exit code 6 (hooks done, console not set up) still carries F2-AC30's warning.
    let (mut report, code) = match finish_wall(env, sys, args, &old, report) {
        Ok(r) => (r, 0),
        Err(out) if out.code == CONSOLE_NOT_SET_UP => (out.stdout, CONSOLE_NOT_SET_UP),
        Err(out) => return out,
    };
    if let Some(v) = verdict.as_ref().filter(|v| v.wide()) {
        report.push_str(&wall_does_not_hold(v));
    }
    Outcome {
        stdout: report,
        stderr: String::new(),
        code,
    }
}

/// The commands that take away what a failed console set-up made this run (§17.8: "sets nothing
/// up"): the delivery folder and the launch rule where they were not there before, and a new key
/// pair. A key that was kept is never removed.
#[must_use]
pub fn rollback_steps(f: &Facts, tool: &dyn Fn(&str) -> String, keys: &KeyPlan) -> Vec<RootStep> {
    let mut steps = Vec::new();
    let rm_f = |paths: &[String]| {
        let mut argv = vec![tool("rm"), "-f".to_owned()];
        argv.extend(paths.iter().cloned());
        argv
    };
    if !f.launch_rule_there {
        steps.push(step(
            "remove the launch rule this set-up wrote",
            rm_f(&[LAUNCH_RULE.to_owned(), LAUNCH_RULE_UNCHECKED.to_owned()]),
        ));
    }
    if !f.approvals_there {
        steps.push(step(
            "remove the delivery folder this set-up made",
            words(&[
                &tool("rm"),
                "-rf",
                &setgid::approvals_home(f.uid).display().to_string(),
            ]),
        ));
    }
    if matches!(keys, KeyPlan::Make) {
        let key = setgid::console_key(f.uid).display().to_string();
        steps.push(step(
            "remove the key this set-up made",
            rm_f(&[format!("{key}.new"), format!("{key}.raw"), key]),
        ));
    }
    steps
}

/// Whether the console is set up (F2 design, "Store wall", "The console is set up"): both keys are
/// there and the launch rule [`LAUNCH_RULE`] is not missing. Missing means the file system answers
/// that there is no such file ([`System::missing`]); where it refuses to look (`/etc/sudoers.d` is
/// commonly root's, mode 0750), the keys decide: the set-up writes the public key last, after the
/// rule.
#[must_use]
pub fn console_is_set_up(env: &Env, sys: &mut dyn System) -> bool {
    sys.uid()
        .and_then(|uid| gather(env, sys, uid).ok())
        .is_some_and(|facts| matches!(facts.keys, Keys::Both))
        && !sys.missing(Path::new(LAUNCH_RULE))
}

/// `lunchroom install console`.
#[must_use]
#[allow(clippy::too_many_lines)] // one transaction: check, ask, show, confirm, run, report
pub fn install_console(env: &Env, sys: &mut dyn System) -> Outcome {
    let (uid, facts) = match start(env, sys, "the console", true) {
        Ok(started) => started,
        Err(out) => return out,
    };
    // Files an earlier try left in the way are neither used nor removed (a failure of this run
    // removes only what this run made); the person removes them and runs the command again.
    if !facts.leftovers.is_empty() {
        let names: Vec<String> = facts
            .leftovers
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        return Outcome::failed(
            &format!(
                "files of an earlier try are in the way: {}; remove them as root and run the \
                 command again; nothing was changed",
                names.join(", ")
            ),
            1,
        );
    }
    let wall_there = setgid::active();
    // §17.8: before anything is set up, a rule that runs a command without a password.
    if let Some(verdict) = read_verdict(sys, &facts).filter(Verdict::wide)
        && let Err(out) = wide_rule(env, sys, &facts, &verdict)
    {
        return out;
    }
    // The seed of a new key is made by the root step, never by this process.
    let keys = match facts.keys {
        Keys::Both => KeyPlan::Keep,
        Keys::PrivateOnly => KeyPlan::Derive,
        Keys::PublicOnly | Keys::Neither => KeyPlan::Make,
    };
    let wall = if wall_there {
        Vec::new()
    } else {
        wall_steps(&facts, &tool)
    };
    let copy = if wall_there {
        Vec::new()
    } else {
        copy_store_steps(&facts, &tool)
    };
    let console = console_steps(&facts, &tool, &keys);
    let last = pub_steps(&facts, &tool, &keys);
    let all: Vec<RootStep> = [wall.clone(), console.clone(), copy, last.clone()].concat();
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let old = installed_program(env);
    let earlier = sys
        .read_text(&setgid::console_state(uid))
        .and_then(|t| State::from_json(&t))
        .map_or_else(String::new, |s| {
            format!("An earlier attempt set nothing up: {}.\n", s.reason)
        });
    let question = format!(
        "{earlier}Lunchroom: set up the console? This puts Lunchroom's store and program where no \
         process of yours can reach them, and lets you start a console that your agents cannot \
         see or press keys in.\nIt runs these commands as root, which asks for a password. \
         Nothing is deleted.\n{}\n{}",
        steps_text(&all),
        if wall_there {
            String::new()
        } else {
            format!(
                "Between the last two groups it runs `{} install` so the hooks of your agents run \
                 the new program, points your `lunchroom` link at it, and renames your old store \
                 to store.moved-{}.\n",
                setgid::PROGRAM,
                stamp(env.now)
            )
        }
    );
    let confirmed = if matches!(facts.keys, Keys::Both) || wall_there {
        confirm(&mail, sys, &question)
    } else {
        confirm_or_begin(&mail, sys, &question)
    };
    if let Err(reason) = confirmed {
        return Outcome::denied(&reason, 3);
    }
    let mut report = match run_steps(sys, &facts, &wall) {
        Ok(r) => r,
        Err(report) => return Outcome::failed(&report, 1),
    };
    // The store moves only after every console step has passed: a rule, a check, a key or a
    // folder that fails leaves the store where it was and no console part behind (§17.8).
    match run_steps(sys, &facts, &console) {
        Ok(r) => report.push_str(&r),
        Err(more) => {
            report.push_str(&more);
            let _ = run_steps(sys, &facts, &rollback_steps(&facts, &tool, &keys));
            report.push_str(
                "The store was not moved, and what this run made of the console was removed.\n",
            );
            return Outcome::failed(&report, 1);
        }
    }
    if !wall_there {
        match move_store(env, sys, &facts, &old) {
            Ok(more) => report.push_str(&more),
            Err(out) => return out,
        }
    }
    // The hooks, the links and the old store come before the public key: it is the last of the
    // set-up, written once every other step has passed (§17.9).
    if !wall_there {
        report = match finish_wall(env, sys, &[], &old, report) {
            Ok(r) => r,
            Err(out) => return out,
        };
    }
    match run_steps(sys, &facts, &last) {
        Ok(r) => report.push_str(&r),
        Err(more) => {
            report.push_str(&more);
            return Outcome::failed(&report, 1);
        }
    }
    // F2-AC38: the answer kept is the one `sudo -n -l` gives with the console's own launch rule in
    // place, so `lunchroom doctor` does not report that rule as a change (cold first-time install,
    // 2026-11-15: doctor's FAIL right after a clean install).
    if let Ok(kept) = crate::terminal::read_sudo_answer(sys, &facts.developer, true) {
        sys.keep_sudo_answer(&kept);
    }
    let _ = write!(
        report,
        "The console is set up. Start it in a terminal of your own (not inside an agent session):\n  {}\n",
        crate::console::start_command()
    );
    Outcome::ok(report)
}

/// `lunchroom uninstall console` (§17.9, §17.8): removes the public key, before any other part of
/// the console, and leaves the rule file, naming it.
#[must_use]
pub fn uninstall_console(env: &Env, sys: &mut dyn System) -> Outcome {
    let (uid, _facts) = match start(env, sys, "the console's removal", true) {
        Ok(started) => started,
        Err(out) => return out,
    };
    let public = setgid::console_pub(uid);
    let rule = setgid::rule_file(uid);
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let question = format!(
        "Lunchroom: this takes the console away from the controller: it removes the public key \
         {} (root's, so it asks for a password), which is the controller's sign that the console \
         is installed. Nothing else is removed: the rule file {} stays, since removing it would \
         give this account its password-free `sudo` back.\n",
        public.display(),
        rule.display()
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let mut report = String::new();
    if sys.exists(&public) {
        let run = sys.run_root(
            "remove the console's public key",
            &words(&[&tool("rm"), "-f", &public.display().to_string()]),
            None,
        );
        if let Err(e) = run {
            return Outcome::failed(
                &format!("the public key {} was not removed: {e}", public.display()),
                1,
            );
        }
        let _ = writeln!(report, "removed {}", public.display());
    } else {
        let _ = writeln!(report, "{} was not there", public.display());
    }
    if sys.exists(&rule) {
        let _ = writeln!(
            report,
            "{} was left in place: it makes `sudo` ask this account for its password, and \
             removing it would give the account its password-free `sudo` back. Remove it yourself \
             as root if that is what you want.",
            rule.display()
        );
    }
    Outcome::ok(report)
}

/// The line `lunchroom uninstall <harness>` adds where the rule file is there.
#[must_use]
pub fn rule_file_note(sys: &dyn System) -> Option<String> {
    let rule = setgid::rule_file(sys.uid()?);
    sys.exists(&rule).then(|| {
        format!(
            "{} is left in place: it makes `sudo` ask this account for its password.\n",
            rule.display()
        )
    })
}
