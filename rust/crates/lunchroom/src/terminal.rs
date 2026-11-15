//! The user's confirmation, read from a terminal no guarded session owns (A1.3).
//!
//! Spec: §7 A1.3 (node `§7::12`): Lunchroom carries out `lunchroom approve`, `lunchroom yolo` and the
//! commands that change its configuration only after reading the user's confirmation from a
//! controlling terminal that no guarded harness process owns; a process with no controlling
//! terminal, inside a guarded harness's terminal, or with a guarded session in its parent chain is
//! denied. The confirmation is the user's passphrase, checked against a verifier that only root can
//! write (`/etc/lunchroom/<uid>.verifier`): a process the user's account runs, detached or not,
//! can open a pseudo-terminal of its own and type `yes`, but it cannot make a verifier that passes
//! or learn a passphrase it was not given. The checks run on a [`System`], so the rules are tested
//! with a fake one.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use lunchroom_core::mail::Mail;
use lunchroom_core::passphrase::{self, Verifier};
use lunchroom_core::sudoers::{self, Kept};
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};

use crate::process;

/// The fewest PBKDF2 iterations a verifier may name, and the number a new one uses. Tests use one,
/// so a roll-call does not spend minutes in hashing; the product always uses the full number.
pub const ITERATIONS: u32 = if cfg!(test) {
    1
} else {
    passphrase::ITERATIONS
};

/// The folder the verifiers are in, owned by root.
const VERIFIER_DIR: &str = "/etc/lunchroom";

/// What [`System::verifier`] reports when the user has no verifier yet.
pub const NO_VERIFIER: &str = "no passphrase is set; run `lunchroom passphrase` in a terminal of \
                               your own to set one";

/// What the user is told when the passphrase could not be read: nothing is read unless the typed
/// characters can be hidden (`tcgetattr` and `tcsetattr` on the terminal), so a terminal whose
/// modes cannot be set ends here as well as a missing terminal.
pub const NO_SECRET_READ: &str = "this command has no controlling terminal to read the passphrase \
                                  from, or the typed characters cannot be hidden on it";

/// The only folders a system program is looked for in, in this order. The user's `PATH` is never
/// searched: the program found runs with the user's terminal, and for `sudo` with their password.
const SYSTEM_FOLDERS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// The first of `folders` that holds an executable regular file named `name`.
fn program_in(folders: &[&str], name: &str) -> Option<PathBuf> {
    folders.iter().map(|f| Path::new(f).join(name)).find(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// `name` in the first of [`SYSTEM_FOLDERS`] that has it.
pub(crate) fn system_program(name: &str) -> Option<PathBuf> {
    program_in(&SYSTEM_FOLDERS, name)
}

/// What `sudo -n -l` prints for the user's account, standard output and error together; a refusal
/// (`a password is required`, exit status 1) is an answer too. Read through [`sudo_list_with`]:
/// bounded in bytes and time, in the English locale, with the program's group dropped.
fn sudo_n_l() -> Result<String, String> {
    let sudo = system_program("sudo").ok_or("sudo was not found in the system folders")?;
    sudo_list_with(&sudo)
}

/// What the user is told when none of the four programs that can run the step that writes as
/// root is present (A1.3): the install says so, sets no passphrase and fails.
fn no_root_program(target: &str) -> String {
    format!(
        "none of sudo, doas, run0 (where `run0 --help` lists -K and -v) and su was found in {}, \
         so Lunchroom cannot put the passphrase check at {target}, which only root may write, and \
         sets no passphrase; install one of them (your account needs the right to use it), then \
         run `lunchroom passphrase`",
        SYSTEM_FOLDERS.join(", ")
    )
}

/// The four programs that can run the step that writes as root, in the order they are tried
/// (A1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootProgram {
    /// `sudo`
    Sudo,
    /// `doas`
    Doas,
    /// `run0`, used only where `run0 --help` lists its `-K` and `-v` options.
    Run0,
    /// `su -c`, which asks root's password at every run.
    Su,
}

impl RootProgram {
    /// The four, in the order they are tried.
    pub const ORDER: [Self; 4] = [Self::Sudo, Self::Doas, Self::Run0, Self::Su];

    /// The program's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sudo => "sudo",
            Self::Doas => "doas",
            Self::Run0 => "run0",
            Self::Su => "su",
        }
    }

    /// The command line that runs `command` as root through the program found at `program`.
    /// `su -c` takes one string: the words of `command`, each quoted for the shell.
    #[must_use]
    pub fn argv(self, program: &Path, command: &[String]) -> Vec<String> {
        let program = program.display().to_string();
        match self {
            Self::Sudo | Self::Doas | Self::Run0 => {
                let mut argv = vec![program];
                argv.extend(command.iter().cloned());
                argv
            }
            Self::Su => {
                let line: Vec<String> = command.iter().map(|w| shell_quote(w)).collect();
                vec![program, "-c".to_owned(), line.join(" "), "root".to_owned()]
            }
        }
    }

    /// What is run as the user, on their terminal, before the install writes anything: the
    /// program's own way of clearing what it remembers, then its own password prompt (A1.3).
    /// `sudo -k` then `sudo -v`; `doas -L` then the `doas` command (`true`, which does nothing
    /// but needs the authorization); `run0 -K` then `run0 -v`; `su -c`, which asks root's
    /// password at every run, runs `true` as root once.
    #[must_use]
    pub fn confirmation(self, program: &Path, true_program: &Path) -> Vec<Vec<String>> {
        let program = program.display().to_string();
        let words = |w: &[&str]| -> Vec<String> {
            std::iter::once(program.clone())
                .chain(w.iter().map(|s| (*s).to_owned()))
                .collect()
        };
        match self {
            Self::Sudo => vec![words(&["-k"]), words(&["-v"])],
            Self::Doas => {
                let truth = true_program.display().to_string();
                vec![words(&["-L"]), words(&[truth.as_str()])]
            }
            Self::Run0 => vec![words(&["-K"]), words(&["-v"])],
            Self::Su => vec![self.argv(Path::new(&program), &[true_program.display().to_string()])],
        }
    }
}

/// `word` quoted for a POSIX shell: in single quotes, a single quote written as `'\''`.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Whether the text of `run0 --help` lists both the `-K` and the `-v` option.
#[must_use]
pub fn run0_lists_k_and_v(help: &str) -> bool {
    let words: Vec<&str> = help
        .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
        .collect();
    words.contains(&"-K") && words.contains(&"-v")
}

/// The first of `sudo`, `doas`, `run0` and `su` that is in one of `folders` (and, for `run0`,
/// whose help `help_of` gives lists `-K` and `-v`), with where it is.
fn choose_root_program(
    folders: &[&str],
    help_of: &dyn Fn(&Path) -> Option<String>,
) -> Option<(RootProgram, PathBuf)> {
    RootProgram::ORDER.iter().find_map(|&which| {
        let found = program_in(folders, which.name())?;
        let usable = which != RootProgram::Run0
            || help_of(&found).is_some_and(|help| run0_lists_k_and_v(&help));
        usable.then_some((which, found))
    })
}

/// Makes `command` print in sudo's and run0's own English, which the parsers read: a translated
/// listing would read as no rules at all.
fn c_locale(command: &mut std::process::Command) -> &mut std::process::Command {
    command
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .env("LANG", "C")
}

/// The words that put the English locale on a command the root program runs: the root program
/// resets or keeps the environment as it likes, so `env` sets it inside.
fn c_locale_words_in(folders: &[&str]) -> Result<Vec<String>, String> {
    let env = program_in(folders, "env").ok_or_else(|| {
        format!(
            "`env` was not found in {}, so what the root step prints cannot be read in English; \
             nothing was run",
            folders.join(", ")
        )
    })?;
    // `LC_ALL` overrides `LANG` and `LANGUAGE` (glibc ignores `LANGUAGE` in the C locale), so it is
    // the one name the program the root step runs is given besides what the root program has.
    Ok(vec![env.display().to_string(), "LC_ALL=C".to_owned()])
}

/// The environment a root program is started with: only `PATH`, `LC_ALL` and `TERM`. `su` without
/// `-` keeps the user's own, so a variable like `BASH_ENV` set in a file the user's processes can
/// edit would be read by root's shell; `sudo`, `doas` and `run0` clean their own, but this does not
/// rest on that.
fn clear_environment(command: &mut std::process::Command) -> &mut std::process::Command {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C");
    if let Some(term) = std::env::var_os("TERM") {
        command.env("TERM", term);
    }
    command
}

/// The text of `run0 --help` run as the user (no group of Lunchroom's), or `None`.
fn run0_help(run0: &Path) -> Option<String> {
    let mut command = std::process::Command::new(run0);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c_locale(&mut command);
    lunchroom_core::setgid::drop_group(&mut command);
    let output = command.output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The program that runs the step that writes as root on this system, taken from the system
/// folders only.
#[must_use]
pub fn root_program() -> Option<(RootProgram, PathBuf)> {
    choose_root_program(&SYSTEM_FOLDERS, &run0_help)
}

/// Where the verifier of user `uid` is.
#[must_use]
pub fn verifier_path(uid: u32) -> PathBuf {
    Path::new(VERIFIER_DIR).join(format!("{uid}.verifier"))
}

/// The path of the verifier as the user is told it, `<uid>` when the system does not give the
/// user id.
#[must_use]
pub fn verifier_shown(sys: &dyn System) -> String {
    sys.uid().map_or_else(
        || format!("{VERIFIER_DIR}/<uid>.verifier"),
        |uid| verifier_path(uid).display().to_string(),
    )
}

/// What the check needs from the system.
pub trait System {
    /// For each process above this one, nearest first: its key (`<pid>:<start time>`) and its
    /// program name.
    fn ancestors(&self) -> Vec<(Option<String>, String)>;
    /// This process's controlling terminal number; 0 when it has none.
    fn own_tty(&self) -> u64;
    /// The controlling terminal number of the harness process `key`, when that process still
    /// runs under that key.
    fn tty_of(&self, key: &str) -> Option<u64>;
    /// Writes `question` to the controlling terminal and reads one line from it; `None` when there
    /// is no controlling terminal to use.
    fn ask(&mut self, question: &str) -> Option<String>;
    /// The real user id of this process.
    fn uid(&self) -> Option<u32>;
    /// The text of the user's verifier, when it can be trusted: a regular file in a folder, both
    /// owned by root and writable by no one else. [`NO_VERIFIER`] when there is none; any other
    /// error says why the one there cannot be trusted.
    ///
    /// # Errors
    /// The reason there is no verifier to check against.
    fn verifier(&self) -> Result<String, String>;
    /// Writes `prompt` to the controlling terminal and reads one line from it with the typed
    /// characters not shown, without its newline; `None` when there is no controlling terminal
    /// to use.
    fn ask_secret(&mut self, prompt: &str) -> Option<String>;
    /// A salt of fresh random bytes for a new verifier.
    ///
    /// # Errors
    /// Why the system gave none.
    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String>;
    /// Makes `line` the user's verifier, owned by root, which takes the user's own authority over
    /// the terminal (`sudo`); the folder is made too when it is not there.
    ///
    /// # Errors
    /// Why the verifier was not put in place.
    fn install_verifier(&mut self, line: &str) -> Result<(), String>;
    /// Why [`System::install_verifier`] cannot work on this system, found before the user is asked
    /// for a passphrase; `None` when nothing is known to stand in its way.
    fn verifier_blocker(&self) -> Option<String> {
        None
    }
    /// Confirms the install that sets the first passphrase, before it writes anything, by the
    /// root program's own password prompt with what it remembers cleared first (A1.3): `question`
    /// is shown first. A typed `yes` confirms nothing; the default, which every test system
    /// has, confirms nothing either.
    ///
    /// # Errors
    /// Why the confirmation was not given.
    fn confirm_root(&mut self, question: &str) -> Result<(), String> {
        let _ = question;
        Err("this system cannot ask a root program for its password".to_owned())
    }
    /// The text `sudo -n -l` prints for the user's account, standard output and error together
    /// (A1.3, §17.8).
    ///
    /// # Errors
    /// Why `sudo` could not be run.
    fn sudo_list(&mut self) -> Result<String, String> {
        Err("this system cannot run sudo".to_owned())
    }
    /// [`System::sudo_list`] for a caller that holds the system by shared reference, as `lunchroom
    /// doctor` does (it changes nothing; `sudo -n -l` only reads).
    ///
    /// # Errors
    /// Why `sudo` could not be run.
    fn sudo_list_shared(&self) -> Result<String, String> {
        Err("this system cannot run sudo".to_owned())
    }
    /// The text `sudo -l -U <user>` prints, read in the step that runs as root (§17.8).
    ///
    /// # Errors
    /// Why the root step did not run.
    fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
        let _ = user;
        Err("this system cannot run a command as root".to_owned())
    }
    /// Keeps the answer `sudo -n -l` gave (A1.3: "and keeps the answer"). The default keeps
    /// nothing; a system with a store keeps it there.
    fn keep_sudo_answer(&mut self, kept: &Kept) {
        let _ = kept;
    }
    /// Why [`System::run_root`] cannot work here; `None` when it can. A system that cannot run a
    /// command as root (the default, and every test system) has no console set-up to offer.
    /// 1.1 only (feature `console`).
    #[cfg(feature = "console")]
    fn root_blocker(&self) -> Option<String> {
        Some("this system cannot run a command as root".to_owned())
    }
    /// Runs `argv` (an absolute program and its arguments) as root through `sudo`, with `input`
    /// on its standard input when there is some. `what` names the step in an error. 1.1 only
    /// (feature `console`).
    ///
    /// # Errors
    /// Why the command did not run or did not succeed.
    #[cfg(feature = "console")]
    fn run_root(&mut self, what: &str, argv: &[String], input: Option<&str>) -> Result<(), String> {
        let _ = (argv, input);
        Err(format!("{what}: this system cannot run a command as root"))
    }
    /// Runs `program` with `args` as the user, on the user's terminal; its exit code (-1 when a
    /// signal ended it). 1.1 only (feature `console`).
    ///
    /// # Errors
    /// Why it could not be started.
    #[cfg(feature = "console")]
    fn run_user(&mut self, program: &Path, args: &[&str]) -> Result<i32, String> {
        let _ = args;
        Err(format!("{} cannot be run from here", program.display()))
    }
    /// Whether `lunchroom install` sets the store wall up on this system (§17.2: the user and
    /// group `lunchroom`, the setgid program, the store in `/var/lib/lunchroom`). The real system
    /// does; a system that cannot run a command as root, which is every stand-in unless it says
    /// otherwise, leaves the install as it was in 1.0.x.
    #[cfg(feature = "console")]
    fn sets_up_wall(&self) -> bool {
        false
    }
    /// The text of the file at `path`; `None` where it cannot be read.
    #[cfg(feature = "console")]
    fn read_text(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    /// Whether anything is at `path`, a link not followed.
    #[cfg(feature = "console")]
    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }
    /// Whether the file system answers that there is no such file at `path` (`NotFound`, a link
    /// not followed). A refused look (`PermissionDenied`) is not missing: F2 design, "Store wall"
    /// (the console is set up). The default is `false`, a stand-in that cannot tell.
    #[cfg(feature = "console")]
    fn missing(&self, _path: &Path) -> bool {
        false
    }
    /// Runs `argv` as root as [`System::run_root`] does and returns what it printed on its
    /// standard output (the console's private key, which only root and `lunchroom` can read).
    ///
    /// # Errors
    /// Why the command did not run or did not succeed.
    #[cfg(feature = "console")]
    fn read_root(&mut self, what: &str, argv: &[String]) -> Result<String, String> {
        let _ = argv;
        Err(format!("{what}: this system cannot run a command as root"))
    }
    /// Clears the remembered authorization of the password program and runs its no-prompt test
    /// command as the user (§17.8): the rule is in force only where that is refused for want of
    /// a password.
    ///
    /// # Errors
    /// Why the program could not be run.
    #[cfg(feature = "console")]
    fn sudo_probe(&mut self) -> Result<SudoProbe, String> {
        Err("this system cannot run the password program".to_owned())
    }
    /// Stops the Lunchroom listeners of user `uid` whose program is one of `programs`, the earlier one
    /// (§17.2: a 1.0.1 listener runs up to 23 hours and would write the old folder), and waits for
    /// them to end.
    ///
    /// # Errors
    /// The process that did not stop, named.
    #[cfg(feature = "console")]
    fn stop_listeners(&mut self, programs: &[PathBuf], uid: u32) -> Result<(), String> {
        let _ = (programs, uid);
        Ok(())
    }
    /// Where the root step reads the program to install from (§17.2). The default, which every
    /// stand-in has, is this program's path; the real system makes a sealed in-memory copy of its
    /// own image before any question is asked, so no file the user's processes can replace is
    /// ever installed as the setgid program.
    ///
    /// # Errors
    /// Why the image could not be copied.
    #[cfg(feature = "console")]
    fn program_source(&mut self) -> Result<ProgramSource, String> {
        let path = std::env::current_exe().map_err(|e| format!("this program's path: {e}"))?;
        Ok(ProgramSource {
            is_installed: path == Path::new(lunchroom_core::setgid::PROGRAM),
            path,
            sha256: None,
        })
    }
    /// Makes this process's memory unreadable to the user's other processes (not dumpable: no
    /// `ptrace`, no `/proc/<pid>/mem`), before a question or a key is handled.
    ///
    /// # Errors
    /// Why the process could not be made so: the install stops, nothing changed.
    #[cfg(feature = "console")]
    fn protect_memory(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// The program the root step installs and what it must hash to (§17.2).
#[cfg(feature = "console")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramSource {
    /// Where root reads it from.
    pub path: PathBuf,
    /// Its sha-256 in lowercase hexadecimal, where it was taken.
    pub sha256: Option<String>,
    /// Whether the running program is the installed one already.
    pub is_installed: bool,
}

/// What the no-prompt test commands did after the remembered authorization was cleared (§17.8).
#[cfg(feature = "console")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SudoProbe {
    /// It ran: no password was asked.
    Ran,
    /// A later rule lets the account run a command as the user `lunchroom` without a password.
    RunsAsLunchroom,
    /// Both were refused because a password is required.
    PasswordRequired,
    /// It was refused for another reason, given.
    Other(String),
}

/// Program names of the harnesses Lunchroom guards: a process under one of them is inside a
/// guarded session, whether or not a hook has recorded it yet.
const HARNESSES: [&str; 5] = ["claude", "codex", "opencode", "openclaw", "hermes"];

/// Checks that this command runs where the user's answer can be trusted: no guarded session in the
/// parent chain, a controlling terminal, and not the terminal of a guarded session (A1.3).
///
/// # Errors
/// The reason the command is denied.
pub fn check(mail: &Mail, sys: &dyn System) -> Result<(), String> {
    for (key, name) in sys.ancestors() {
        let recorded = key
            .as_deref()
            .is_some_and(|k| mail.process_session(k).is_some());
        if recorded || HARNESSES.contains(&name.as_str()) {
            return Err(format!(
                "this command runs under a guarded session ({name}); only the user, in a \
                 terminal outside every guarded session, can run it"
            ));
        }
    }
    let tty = sys.own_tty();
    if tty == 0 {
        return Err(
            "this command has no controlling terminal to read the user's answer from".to_owned(),
        );
    }
    if mail.harness_processes().any(|k| sys.tty_of(k) == Some(tty)) {
        return Err("this command runs in the terminal of a guarded session".to_owned());
    }
    Ok(())
}

/// Shows `question` and succeeds only when every A1.3 condition holds and the user types the
/// passphrase the verifier was made from. The passphrase is read with the typed characters not
/// shown.
///
/// # Errors
/// The reason the command is denied.
pub fn confirm(mail: &Mail, sys: &mut dyn System, question: &str) -> Result<(), String> {
    check(mail, sys)?;
    prove(sys, question)
}

/// As [`confirm`], except that a user who has no verifier yet is confirmed by the root program's
/// own password prompt, with what it remembers cleared first (`sudo -k` then `sudo -v`, and so
/// on: [`RootProgram::confirmation`]): the install that sets the first passphrase has nothing to
/// check a passphrase against, and a typed `yes` confirms nothing (A1.3). A verifier that is there
/// but cannot be trusted is never passed over.
///
/// # Errors
/// The reason the command is denied.
pub fn confirm_or_begin(mail: &Mail, sys: &mut dyn System, question: &str) -> Result<(), String> {
    check(mail, sys)?;
    // `sudo -n -l` is read at every install, and the answer kept; where it shows a rule without
    // a password the user is told, before being asked, that this confirmation proves no person
    // is present (A1.3, K1.10).
    let question = match read_sudo_answer(sys, "", false) {
        Ok(kept) => {
            sys.keep_sudo_answer(&kept);
            let listing = sudoers::parse(&kept.own);
            if listing.has_password_free() {
                format!("{}\n{question}", sudoers::statement(&listing))
            } else {
                question.to_owned()
            }
        }
        Err(_) => question.to_owned(),
    };
    if sys.verifier().is_err_and(|reason| reason == NO_VERIFIER) {
        return sys.confirm_root(&question);
    }
    prove(sys, &question)
}

/// Keeps `kept` in the store in `dir`, under the store's lock, in the second file (A1.3, D1.2).
///
/// # Errors
/// Why the store could not be opened or written.
pub fn keep_answer_in(dir: &Path, kept: &Kept) -> Result<(), String> {
    lunchroom_core::store::Store::<Mail>::open(dir)
        .and_then(|store| store.update(|m| m.keep_sudo_answer(kept)))
        .map_err(|e| e.to_string())
}

/// Reads `sudo -n -l` for the user's account `account`, and with `others` also `sudo -l -U <user>`
/// for each other user a password-free rule runs as (in the step that runs as root), and returns
/// the answer to keep (A1.3, §17.8).
///
/// # Errors
/// Why `sudo -n -l` could not be run.
pub fn read_sudo_answer(sys: &mut dyn System, account: &str, others: bool) -> Result<Kept, String> {
    let own = sys.sudo_list()?;
    let mut kept = Kept {
        read_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        own,
        others: std::collections::BTreeMap::new(),
    };
    if others {
        for user in sudoers::users_to_read(&sudoers::parse(&kept.own), account) {
            // A user whose rules could not be read has no text kept, so the classifier takes it
            // as wide.
            if let Ok(text) = sys.sudo_list_user(&user) {
                kept.others.insert(user, text);
            }
        }
    }
    Ok(kept)
}

/// The verifier the user's check is made against.
///
/// # Errors
/// The reason there is none that can be used.
pub fn trusted_verifier(sys: &dyn System) -> Result<Verifier, String> {
    let line = sys.verifier()?;
    Verifier::parse(&line, ITERATIONS).map_err(|why| {
        format!(
            "the passphrase check at {} cannot be used ({why}); remove it with `sudo rm {}` and \
             run `lunchroom passphrase` to set it again",
            verifier_shown(sys),
            verifier_shown(sys)
        )
    })
}

/// Shows `question`, asks for the passphrase and checks it.
fn prove(sys: &mut dyn System, question: &str) -> Result<(), String> {
    let verifier = trusted_verifier(sys)?;
    let Some(typed) = sys.ask_secret(&format!("{question}Passphrase: ")) else {
        return Err(NO_SECRET_READ.to_owned());
    };
    if verifier.matches(&typed) {
        Ok(())
    } else {
        Err("the passphrase was wrong; nothing was changed".to_owned())
    }
}

const NOT_ACTING: u32 = u32::MAX;

/// The user the console acts for. The console runs as the system user `lunchroom`, but its
/// passphrase check is the developer's: `/etc/lunchroom/<developer uid>.verifier`.
static ACTING_FOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(NOT_ACTING);

/// Makes [`OsSystem`] answer `uid` (and look for `uid`'s verifier) instead of this process's own
/// real user id; used by the console alone (1.1, feature `console`).
#[cfg(feature = "console")]
pub fn act_for(uid: u32) {
    ACTING_FOR.store(uid, std::sync::atomic::Ordering::SeqCst);
}

/// The real system: `/proc` and `/dev/tty`.
#[derive(Debug, Default)]
pub struct OsSystem;

impl System for OsSystem {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        process::ancestors(std::process::id())
            .into_iter()
            .skip(1)
            .map(|pid| (process::key(pid), process::name(pid).unwrap_or_default()))
            .collect()
    }

    fn own_tty(&self) -> u64 {
        process::tty(std::process::id()).unwrap_or(0)
    }

    fn tty_of(&self, key: &str) -> Option<u64> {
        let pid: u32 = key.split(':').next()?.parse().ok()?;
        (process::key(pid).as_deref() == Some(key))
            .then(|| process::tty(pid))
            .flatten()
    }

    fn ask(&mut self, question: &str) -> Option<String> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        tty.write_all(question.as_bytes()).ok()?;
        tty.flush().ok()?;
        read_line_capped(tty, MAX_ANSWER_BYTES)
    }

    fn uid(&self) -> Option<u32> {
        let acting = ACTING_FOR.load(std::sync::atomic::Ordering::SeqCst);
        if acting != NOT_ACTING {
            return Some(acting);
        }
        real_uid(&std::fs::read_to_string("/proc/self/status").ok()?)
    }

    fn verifier(&self) -> Result<String, String> {
        let uid = self
            .uid()
            .ok_or("the user id of this process cannot be read from /proc")?;
        let file = verifier_path(uid);
        let untrusted = |why: &str| {
            format!(
                "the passphrase check at {} cannot be trusted ({why}); `sudo rm {}` removes it, \
                 and `lunchroom passphrase` sets a new one",
                file.display(),
                file.display()
            )
        };
        // The folder, then the file as it is on disk: a link is never followed.
        match std::fs::symlink_metadata(VERIFIER_DIR) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(NO_VERIFIER.to_owned());
            }
            Err(e) => return Err(untrusted(&format!("{VERIFIER_DIR} cannot be read: {e}"))),
            Ok(m) if !folder_trusted(m.file_type(), m.uid(), m.mode()) => {
                return Err(untrusted(&format!(
                    "{VERIFIER_DIR} is not a folder owned by root and writable by no one else"
                )));
            }
            Ok(_) => {}
        }
        match std::fs::symlink_metadata(&file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(NO_VERIFIER.to_owned());
            }
            Err(e) => return Err(untrusted(&format!("it cannot be read: {e}"))),
            Ok(m) if !regular_file(m.file_type()) => {
                return Err(untrusted("it is not a regular file"));
            }
            Ok(_) => {}
        }
        // The facts are read again from the open file, so a file swapped in between is judged as
        // it is.
        let mut opened = std::fs::File::open(&file)
            .map_err(|e| untrusted(&format!("it cannot be read: {e}")))?;
        let facts = opened
            .metadata()
            .map_err(|e| untrusted(&format!("it cannot be read: {e}")))?;
        let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let group =
            lunchroom_core::setgid::gid_from_group(&groups, lunchroom_core::setgid::SYSTEM_USER);
        if !file_trusted(
            facts.file_type(),
            facts.uid(),
            facts.gid(),
            facts.mode(),
            group,
        ) {
            return Err(untrusted(
                "it is not a regular file owned by root, group lunchroom, mode 0640",
            ));
        }
        let mut text = String::new();
        (&mut opened)
            .take(4096)
            .read_to_string(&mut text)
            .map_err(|e| untrusted(&format!("it cannot be read: {e}")))?;
        Ok(text)
    }

    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        tty.write_all(prompt.as_bytes()).ok()?;
        tty.flush().ok()?;
        // Nothing is read while the typed characters would show, and they show again whatever
        // happens.
        let line = read_hidden_line(&tty);
        let _ = tty.write_all(b"\n");
        line.map(|mut line| {
            if line.ends_with('\n') {
                line.pop();
            }
            line
        })
    }

    fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
        let mut salt = [0u8; passphrase::SALT_BYTES];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut salt))
            .map_err(|e| format!("random bytes cannot be read: {e}"))?;
        Ok(salt)
    }

    fn install_verifier(&mut self, line: &str) -> Result<(), String> {
        let uid = self
            .uid()
            .ok_or("the user id of this process cannot be read from /proc")?;
        let target = verifier_path(uid).display().to_string();
        let (root, install_program) = root_programs(&target)?;
        // The root program asks for a password on the terminal, and the verifier reaches `install`
        // from a pipe (or, for `su`, a sealed in-memory file only root can open): there is no file
        // of the user's own that another process of the account could swap while the password is
        // being typed.
        let text = format!("{line}\n");
        let install_program = install_program.display().to_string();
        let [folder, file] = install_steps(&install_program, &target);
        let words =
            |step: &[&str]| -> Vec<String> { step.iter().map(|w| (*w).to_owned()).collect() };
        root_run(
            &root,
            &format!("making {VERIFIER_DIR}"),
            &words(&folder),
            None,
            false,
        )?;
        ensure_group(&root)?;
        root_run(
            &root,
            &format!("installing {target}"),
            &words(&file),
            Some(text.as_str()),
            false,
        )
        .map(|_| ())
    }

    fn verifier_blocker(&self) -> Option<String> {
        root_programs(&verifier_shown(self)).err()
    }

    fn confirm_root(&mut self, question: &str) -> Result<(), String> {
        let target = verifier_shown(self);
        let (which, program) = root_program().ok_or_else(|| no_root_program(&target))?;
        let truth = system_program("true").unwrap_or_else(|| PathBuf::from("/usr/bin/true"));
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|e| format!("the terminal cannot be opened: {e}"))?;
        let ask = match which {
            RootProgram::Su => "root's password",
            _ => "your password",
        };
        let _ = writeln!(
            tty,
            "{question}No passphrase is set yet, so {} confirms this install: it will ask for \
             {ask}. A typed yes confirms nothing.",
            which.name()
        );
        for step in which.confirmation(&program, &truth) {
            run_on_terminal(&tty, &step)?;
        }
        Ok(())
    }

    fn sudo_list(&mut self) -> Result<String, String> {
        sudo_n_l()
    }

    fn sudo_list_shared(&self) -> Result<String, String> {
        sudo_n_l()
    }

    fn keep_sudo_answer(&mut self, kept: &Kept) {
        // The answer is kept in Lunchroom's store of this process's environment (D1.2: in the
        // second file). A store that cannot be written is said, never silently passed over.
        let result =
            crate::home::Env::from_process().and_then(|env| keep_answer_in(&env.store_dir(), kept));
        if let Err(why) = result {
            let _ = writeln!(
                std::io::stderr(),
                "Lunchroom: the answer of `sudo -n -l` was not kept: {why}"
            );
        }
    }

    fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
        let sudo = system_program("sudo").ok_or("sudo was not found in the system folders")?;
        let root = root_program().ok_or("no program to run a command as root was found")?;
        let command = list_user_command(&sudo, user)?;
        root_run(
            &root,
            &format!("reading the sudo rules of {user}"),
            &command,
            None,
            true,
        )
    }

    #[cfg(feature = "console")]
    fn root_blocker(&self) -> Option<String> {
        root_program()
            .is_none()
            .then(|| no_root_program("/etc/lunchroom"))
    }

    #[cfg(feature = "console")]
    fn run_root(&mut self, what: &str, argv: &[String], input: Option<&str>) -> Result<(), String> {
        let root = root_program().ok_or("no program to run a command as root was found")?;
        root_run(&root, what, argv, input, false).map(|_| ())
    }

    #[cfg(feature = "console")]
    fn sets_up_wall(&self) -> bool {
        true
    }

    #[cfg(feature = "console")]
    fn read_root(&mut self, what: &str, argv: &[String]) -> Result<String, String> {
        let root = root_program().ok_or("no program to run a command as root was found")?;
        root_run(&root, what, argv, None, true)
    }

    #[cfg(feature = "console")]
    fn sudo_probe(&mut self) -> Result<SudoProbe, String> {
        let program = system_program("sudo").ok_or("sudo was not found in the system folders")?;
        probe_with(&program)
    }

    #[cfg(feature = "console")]
    fn program_source(&mut self) -> Result<ProgramSource, String> {
        let running = std::fs::read_link("/proc/self/exe")
            .map_err(|e| format!("this program's image cannot be found: {e}"))?;
        if running == Path::new(lunchroom_core::setgid::PROGRAM) {
            return Ok(ProgramSource {
                path: running,
                sha256: None,
                is_installed: true,
            });
        }
        capture_image()
    }

    #[cfg(feature = "console")]
    fn protect_memory(&mut self) -> Result<(), String> {
        make_undumpable(|| {
            rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        })
    }

    #[cfg(feature = "console")]
    fn stop_listeners(&mut self, programs: &[PathBuf], uid: u32) -> Result<(), String> {
        let proc = Path::new("/proc");
        let mut found: Vec<u32> = programs
            .iter()
            .flat_map(|program| process::listeners_in(proc, uid, program))
            .collect();
        found.sort_unstable();
        found.dedup();
        if found.is_empty() {
            return Ok(());
        }
        let left = process::terminate(proc, &found, std::time::Duration::from_secs(10));
        match left.first() {
            None => Ok(()),
            Some(pid) => Err(format!(
                "process {pid} (an earlier Lunchroom's `listen`, started by you) did not stop \
                 within 10 seconds of SIGTERM"
            )),
        }
    }

    #[cfg(feature = "console")]
    fn missing(&self, path: &Path) -> bool {
        matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    }

    #[cfg(feature = "console")]
    fn run_user(&mut self, program: &Path, args: &[&str]) -> Result<i32, String> {
        // The program is the installed setgid program: it gets its group on exec. Its exit code
        // is given whole (F2 design: 6 is "hooks done, console not set up"); one ended by a signal
        // is -1, never success.
        std::process::Command::new(program)
            .args(args)
            .status()
            .map(|s| s.code().unwrap_or(-1))
            .map_err(|e| format!("{} cannot be started: {e}", program.display()))
    }
}

/// Runs `sudo -n -l` with `sudo` at the given path, in the English locale, and gives what it
/// printed, standard output and error together.
fn sudo_list_with(sudo: &Path) -> Result<String, String> {
    sudo_list_within(sudo, &SUDO_READ)
}

/// What is read of a child's output, and for how long.
#[derive(Clone, Copy, Debug)]
struct ReadLimit {
    /// The most bytes of each stream.
    bytes: usize,
    /// The time the child has to close its streams and exit.
    within: std::time::Duration,
}

/// What is read of `sudo -n -l`: it prompts for nothing, so a few seconds are plenty.
const SUDO_READ: ReadLimit = ReadLimit {
    bytes: sudoers::MAX_LISTING_BYTES,
    within: std::time::Duration::from_secs(20),
};

/// What is read of a captured root step: the root program may ask for a password, and the user types
/// it, so this is long; it still ends a step that never does.
const ROOT_READ: ReadLimit = ReadLimit {
    bytes: sudoers::MAX_LISTING_BYTES,
    within: std::time::Duration::from_secs(600),
};

/// What came of reading a child's output.
enum Read {
    /// Both streams ended within the limits and the child exited.
    Done {
        out: Vec<u8>,
        err: Vec<u8>,
        status: std::process::ExitStatus,
    },
    /// A stream went over its byte limit; the child was killed.
    Over,
    /// The streams or the child did not end in time; the child was killed.
    TimedOut,
}

impl Read {
    /// The text that stands for output that was not read, or `None` where it was.
    fn unread_text(&self) -> Option<&'static str> {
        match self {
            Self::Done { .. } => None,
            Self::Over => Some(sudoers::OVER_LIMIT_TEXT),
            Self::TimedOut => Some(sudoers::TIMED_OUT_TEXT),
        }
    }
}

/// How a child that must be ended is killed with what it started.
#[derive(Clone, Copy, Debug)]
enum Kill {
    /// The child leads a process group of its own (it needs no terminal): the group is killed, so a
    /// descendant that holds a pipe, one that was started and left, is gone too.
    Group,
    /// The child shares the terminal's process group (a root program that asks for a password reads
    /// the terminal, which a group of its own would not be let to): the child and everything below
    /// it in `/proc` is killed.
    Tree,
}

/// The processes below `root` (not `root` itself), found through the parent ids in `/proc`.
fn descendants(root: u32) -> Vec<rustix::process::Pid> {
    let mut parents: Vec<(u32, u32)> = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (name) state ppid ...`: the name may hold spaces and parentheses, so the fields are
        // counted from the last `)`.
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
        if let Some(parent) = parent {
            parents.push((pid, parent));
        }
    }
    let mut found = vec![root];
    let mut next = 0;
    while next < found.len() {
        let current = found[next];
        for &(pid, parent) in &parents {
            if parent == current && !found.contains(&pid) {
                found.push(pid);
            }
        }
        next += 1;
    }
    found
        .into_iter()
        .skip(1)
        .filter_map(|p| rustix::process::Pid::from_raw(i32::try_from(p).ok()?))
        .collect()
}

/// Reads up to `limit` bytes in all from `reader`, counted with the other stream's in `total`;
/// whether the two together went over.
fn read_counted(
    mut reader: impl std::io::Read,
    total: &std::sync::atomic::AtomicUsize,
    limit: usize,
) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return (bytes, false),
            Ok(n) => n,
        };
        let before = total.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        if before + n > limit {
            return (bytes, true);
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
}

/// How long the streams of a child that has exited get to end once the process group it led has
/// been stopped: a descendant that left the group and holds a pipe cannot hold the read up longer.
const STREAM_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How often the child and the readers are looked at.
const POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// Reads `stream` on a thread of its own, counted with the other stream's in `total`; what it read
/// and whether it went over come back on `sender`, tagged `is_err`.
fn spawn_reader(
    stream: impl std::io::Read + Send + 'static,
    is_err: bool,
    sender: std::sync::mpsc::Sender<(bool, Vec<u8>, bool)>,
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bytes: usize,
) {
    std::thread::spawn(move || {
        let (data, over) = read_counted(stream, &total, bytes);
        let _ = sender.send((is_err, data, over));
    });
}

/// Stops the process group that `child` leads, and with it every descendant that was started in it.
fn stop_group(child: &std::process::Child) {
    let pid = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// Ends `child` as `kill` says (the group, or everything below it in `/proc`), kills and waits for
/// the child itself, and gives `result`.
fn end_child(child: &mut std::process::Child, kill: Kill, result: Read) -> Read {
    match kill {
        Kill::Group => stop_group(child),
        Kill::Tree => {
            for pid in descendants(child.id()) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Reads the piped standard output and error of `child` within `limit`, and waits for it. The two
/// streams are read on a thread each, and together may give up to the byte limit; the child is
/// killed (by this thread, which owns it, as `kill` says) when they go over, and when the streams or
/// the child have not ended by the time limit, so a child that stops at the limit, says nothing,
/// closes its streams and stays, or never exits cannot hold this up. Where the child leads a
/// process group of its own ([`Kill::Group`]) the group is stopped as soon as the child has
/// exited, so a descendant that holds a pipe open does not hold the read up either: the streams
/// then get [`STREAM_GRACE`] to end, and what has not ended by then is counted as not read. A reader
/// that is still blocked is left to end by itself.
fn read_child(child: &mut std::process::Child, limit: &ReadLimit, kill: Kill) -> Read {
    let mut cutoff = std::time::Instant::now() + limit.within;
    let total = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (sender, receiver) = std::sync::mpsc::channel::<(bool, Vec<u8>, bool)>();
    let mut pending = 0;
    if let Some(out) = child.stdout.take() {
        pending += 1;
        spawn_reader(out, false, sender.clone(), total.clone(), limit.bytes);
    }
    if let Some(err) = child.stderr.take() {
        pending += 1;
        spawn_reader(err, true, sender.clone(), total.clone(), limit.bytes);
    }
    drop(sender);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut exited = None;
    loop {
        if exited.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exited = Some(status);
            if matches!(kill, Kill::Group) {
                stop_group(child);
                cutoff = cutoff.min(std::time::Instant::now() + STREAM_GRACE);
            }
        }
        if let (0, Some(status)) = (pending, exited) {
            return Read::Done { out, err, status };
        }
        let left = cutoff.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return end_child(child, kill, Read::TimedOut);
        }
        if pending == 0 {
            std::thread::sleep(left.min(POLL));
            continue;
        }
        match receiver.recv_timeout(left.min(POLL)) {
            Ok((_, _, true)) => return end_child(child, kill, Read::Over),
            Ok((is_err, data, false)) => {
                if is_err {
                    err = data;
                } else {
                    out = data;
                }
                pending -= 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => pending = 0,
        }
    }
}

/// Runs `program` with `args` as the user, with no terminal and in a process group of its own, in
/// the English locale and with the group of Lunchroom dropped, and reads what it prints within
/// `limit`. The group is stopped when the program has exited, or when the read gives up.
fn run_bounded(program: &Path, args: &[&str], limit: &ReadLimit) -> Result<Read, String> {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    c_locale(&mut command);
    lunchroom_core::setgid::drop_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("{} cannot be started: {e}", program.display()))?;
    Ok(read_child(&mut child, limit, Kill::Group))
}

/// Runs `sudo -n -l` with `sudo` at the given path, in the English locale, and gives what it
/// printed, standard output and error together, read within `limit`; output that was over the byte
/// limit or did not end in time stands for text that is not a listing.
fn sudo_list_within(sudo: &Path, limit: &ReadLimit) -> Result<String, String> {
    let read = run_bounded(sudo, &["-n", "-l"], limit)
        .map_err(|e| format!("sudo cannot be started: {e}"))?;
    if let Some(text) = read.unread_text() {
        return Ok(text.to_owned());
    }
    let Read::Done { out, err, .. } = read else {
        return Err("sudo's output cannot be read".to_owned());
    };
    // A refusal (`a password is required`, exit status 1) is an answer too.
    let mut text = String::from_utf8_lossy(&out).into_owned();
    text.push_str(&String::from_utf8_lossy(&err));
    Ok(text)
}

/// The command the root step runs to read the rules of `user`: `sudo -l -U <user>`.
///
/// # Errors
/// `user` is not a login name or a number ([`sudoers::valid_user_name`]): nothing is built, so a
/// name that is empty, has white space, begins with `-` or holds shell characters is never passed on.
fn list_user_command(sudo: &Path, user: &str) -> Result<Vec<String>, String> {
    if !sudoers::valid_user_name(user) {
        return Err(format!(
            "{user:?} is not a user name that is passed to sudo; its rules were not read"
        ));
    }
    Ok(vec![
        sudo.display().to_string(),
        "-l".to_owned(),
        "-U".to_owned(),
        user.to_owned(),
    ])
}

/// The most bytes read from a probe call, standard output and error together: 4 KiB.
#[cfg(feature = "console")]
pub const MAX_PROBE_BYTES: usize = 4096;

/// How long one probe call may take before it is stopped: 20 seconds.
#[cfg(feature = "console")]
pub const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// What is read of a probe call: [`MAX_PROBE_BYTES`] in all, within [`PROBE_WAIT`].
#[cfg(feature = "console")]
const PROBE_READ: ReadLimit = ReadLimit {
    bytes: MAX_PROBE_BYTES,
    within: PROBE_WAIT,
};

/// What a probe call refused with, read from its standard error exactly (sudo 1.9.15p5, C locale).
#[cfg(feature = "console")]
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// `sudo: a password is required`, and nothing else.
    Password,
    /// `sudo: unknown user lunchroom` or `sudo: unknown group lunchroom` (the line, as asked for).
    NotMade,
    /// Anything else, in any language or form: not proof of a password asked for.
    Unknown,
}

/// The refusal in `text`: the exact C-locale forms only. The password line must be the whole
/// text; the unknown user or group line must be its first line.
#[cfg(feature = "console")]
fn refusal(text: &str, not_made: &str) -> Refusal {
    if text.trim_end_matches('\n') == "sudo: a password is required" {
        Refusal::Password
    } else if text.lines().next() == Some(not_made) {
        Refusal::NotMade
    } else {
        Refusal::Unknown
    }
}

/// Makes the process not dumpable by `set`; a failure is an error (fail closed).
///
/// # Errors
/// Why `set` failed.
#[cfg(feature = "console")]
pub fn make_undumpable(set: impl FnOnce() -> Result<(), rustix::io::Errno>) -> Result<(), String> {
    set().map_err(|e| {
        format!(
            "this process could not be made unreadable to your other processes ({e}), so the \
             set-up stopped"
        )
    })
}

/// One probe call: `program args` run by [`run_bounded`] (the C locale, no terminal, a process
/// group of its own that is stopped with it, what it prints capped and its time bounded by
/// `limit`). `Ok((succeeded, text))`, where the text is its standard error; a call whose output went
/// over the cap or did not end in time did not succeed, and its text says so, which is never one
/// of the C-locale refusals.
#[cfg(feature = "console")]
fn probe_call(program: &Path, args: &[&str], limit: &ReadLimit) -> Result<(bool, String), String> {
    Ok(match run_bounded(program, args, limit)? {
        Read::Done { err, status, .. } => {
            (status.success(), String::from_utf8_lossy(&err).into_owned())
        }
        Read::Over => (
            false,
            format!(
                "the output of the call was longer than {} bytes and it was stopped",
                limit.bytes
            ),
        ),
        Read::TimedOut => (
            false,
            format!(
                "the call did not finish within {} seconds and was stopped",
                limit.within.as_secs()
            ),
        ),
    })
}

/// Runs `program -k`, then `program -n true`, as the user, and says what it did (§17.8). Where
/// root's probe is refused for want of a password, it also tries a command as the user
/// `lunchroom`, as the group `lunchroom`, and as root with the group `lunchroom`: a later rule
/// can grant any of them without a password. Every call runs through [`run_bounded`]: the C locale
/// (`LC_ALL`, `LANGUAGE`, `LANG`), no terminal, the group of Lunchroom dropped, a process group of
/// its own that is stopped with the call, what it prints capped at [`MAX_PROBE_BYTES`] (standard
/// output and error together) and its time bounded to [`PROBE_WAIT`]; a refusal whose text is not
/// exactly one of the C-locale forms is [`SudoProbe::Other`], never taken as a password asked for.
#[cfg(feature = "console")]
fn probe_with(program: &Path) -> Result<SudoProbe, String> {
    probe_within(program, &PROBE_READ)
}

/// [`probe_with`], with what is read of each call bounded by `limit`.
#[cfg(feature = "console")]
fn probe_within(program: &Path, limit: &ReadLimit) -> Result<SudoProbe, String> {
    let name = lunchroom_core::setgid::SYSTEM_USER;
    probe_call(program, &["-k"], limit)?;
    let (ran, text) = probe_call(program, &["-n", "true"], limit)?;
    if ran {
        return Ok(SudoProbe::Ran);
    }
    if refusal(&text, "") != Refusal::Password {
        return Ok(SudoProbe::Other(text.trim().to_owned()));
    }
    // Where the user or the group is not made yet, the program says so, and the account cannot
    // run a command as it.
    let unknown_user = format!("sudo: unknown user {name}");
    let unknown_group = format!("sudo: unknown group {name}");
    let probes: [(&[&str], &str); 3] = [
        (&["-n", "-u", name, "true"], &unknown_user),
        (&["-n", "-g", name, "true"], &unknown_group),
        (&["-n", "-u", "root", "-g", name, "true"], &unknown_group),
    ];
    for (args, not_made) in probes {
        let (ran, text) = probe_call(program, args, limit)?;
        if ran {
            return Ok(SudoProbe::RunsAsLunchroom);
        }
        if refusal(&text, not_made) == Refusal::Unknown {
            return Ok(SudoProbe::Other(text.trim().to_owned()));
        }
    }
    Ok(SudoProbe::PasswordRequired)
}

/// The longest answer read from the terminal to a question: 4 KiB. A longer line is cut there.
pub const MAX_ANSWER_BYTES: u64 = 4096;

/// One line from `reader`, at most `max` bytes of it (the rest of a longer line is left unread);
/// `None` where nothing could be read.
fn read_line_capped(reader: impl std::io::Read, max: u64) -> Option<String> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take(max))
        .read_until(b'\n', &mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The largest image this program copies: 256 MiB. A larger one stops the install (fail closed).
#[cfg(feature = "console")]
pub const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// The sealed in-memory copy of this program's image: the file, the path root reads it by, and its
/// sha-256. Only a copy that was made whole is kept; a failed capture is never kept, so a retry
/// makes it again.
#[cfg(feature = "console")]
type Image = (std::fs::File, String, String);

#[cfg(feature = "console")]
static IMAGE: std::sync::OnceLock<Image> = std::sync::OnceLock::new();

/// The bytes of `reader`, at most `max` of them; more than that is an error, and no more than
/// `max` and one byte are ever read.
///
/// # Errors
/// Why it could not be read, or that it holds more than `max` bytes.
#[cfg(feature = "console")]
pub fn read_capped(reader: impl std::io::Read, max: u64) -> Result<Vec<u8>, String> {
    let mut image = Vec::new();
    reader
        .take(max + 1)
        .read_to_end(&mut image)
        .map_err(|e| format!("this program's image cannot be read: {e}"))?;
    if image.len() as u64 > max {
        return Err(format!(
            "this program's image is larger than {max} bytes, which is more than Lunchroom \
             copies; nothing was changed"
        ));
    }
    Ok(image)
}

/// How long reading this program's image may take: 30 seconds.
#[cfg(feature = "console")]
pub const IMAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// [`read_capped`] on its own thread, waited for at most `wait`: a reader that blocks stops the
/// install (fail closed) instead of holding it for ever.
///
/// # Errors
/// As [`read_capped`], or that it did not finish within `wait`.
#[cfg(feature = "console")]
pub fn read_capped_within(
    reader: impl std::io::Read + Send + 'static,
    max: u64,
    wait: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(read_capped(reader, max));
    });
    receive.recv_timeout(wait).map_err(|_| {
        format!(
            "reading this program's image did not finish within {} seconds; nothing was changed",
            wait.as_secs()
        )
    })?
}

/// The bytes of the file at `path`, at most `max` of them; more than that is an error.
///
/// # Errors
/// Why the file could not be read, or that it is larger than `max`.
#[cfg(feature = "console")]
pub fn read_image(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("this program's image cannot be read: {e}"))?;
    read_capped_within(file, max, IMAGE_WAIT)
}

/// Where `words` name the image kept in `cache` by its `/proc/<pid>/fd` path, that word becomes
/// `/dev/stdin` and the result is the image, read from its first byte, for the command's standard
/// input. `None` when no word names it (the words are unchanged).
#[cfg(feature = "console")]
pub(crate) fn image_as_stdin(
    cache: &std::sync::OnceLock<Image>,
    words: &mut [String],
) -> Option<std::fs::File> {
    use std::io::{Seek as _, SeekFrom};
    let (file, path, _) = cache.get()?;
    let word = words.iter_mut().find(|w| *w == path)?;
    let mut image = file.try_clone().ok()?;
    image.seek(SeekFrom::Start(0)).ok()?;
    "/dev/stdin".clone_into(word);
    Some(image)
}

/// How the program image reaches a root command (F2 design, "Store wall": no root command is given
/// a `/proc/<pid>/fd` path).
#[cfg(feature = "console")]
#[derive(Debug)]
pub(crate) enum ImagePass {
    /// No word names the image: the words are unchanged.
    None,
    /// The word is `/dev/stdin` and this is the stream (`sudo`, `doas`, `run0`).
    Stdin(std::fs::File),
    /// The word is the path of this file (`su`, whose standard input carries the password).
    File(ImageFile),
}

/// The image written to a new file in a new folder of mode 0700, for `su`; the file and the folder
/// are removed when this is dropped.
#[cfg(feature = "console")]
#[derive(Debug)]
pub(crate) struct ImageFile {
    dir: PathBuf,
    /// The file the word names.
    pub(crate) path: PathBuf,
}

#[cfg(feature = "console")]
impl Drop for ImageFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// The folder a new image folder is made in: `XDG_RUNTIME_DIR` where it names a folder, else the
/// temporary folder.
#[cfg(feature = "console")]
fn image_parent() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Decides how the image kept in `cache` reaches the command `words` run through `which`. Where a
/// word names the image by its `/proc/<pid>/fd` path: for `su` the image is written to a new file
/// (mode 0600) in a new folder of mode 0700 made in `parent`, and the word becomes that file's
/// path; for the others the word becomes `/dev/stdin` ([`image_as_stdin`]). No word names it:
/// [`ImagePass::None`].
///
/// # Errors
/// Why the folder or the file could not be made, or the image not read.
#[cfg(feature = "console")]
pub(crate) fn pass_image(
    cache: &std::sync::OnceLock<Image>,
    which: RootProgram,
    words: &mut [String],
    parent: &Path,
) -> Result<ImagePass, String> {
    use std::io::{Seek as _, SeekFrom};
    if which != RootProgram::Su {
        return Ok(image_as_stdin(cache, words).map_or(ImagePass::None, ImagePass::Stdin));
    }
    let Some((file, path, _)) = cache.get() else {
        return Ok(ImagePass::None);
    };
    let Some(word) = words.iter_mut().find(|w| *w == path) else {
        return Ok(ImagePass::None);
    };
    let mut image = file
        .try_clone()
        .map_err(|e| format!("the program image cannot be read: {e}"))?;
    image
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("the program image cannot be read: {e}"))?;
    let mut bytes = Vec::new();
    image
        .read_to_end(&mut bytes)
        .map_err(|e| format!("the program image cannot be read: {e}"))?;
    let kept = private_file(parent, "lunchroom", &bytes)?;
    *word = kept.path.display().to_string();
    Ok(ImagePass::File(kept))
}

/// Writes `bytes` to the file `name` (mode 0600) in a new folder of mode 0700 made in `parent` for
/// this run; the returned guard removes the file and the folder when dropped, on every way out.
///
/// # Errors
/// Why the folder or the file could not be made.
#[cfg(feature = "console")]
pub(crate) fn private_file(parent: &Path, name: &str, bytes: &[u8]) -> Result<ImageFile, String> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = parent.join(format!(
        "lunchroom-image-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("the private folder cannot be made: {e}"))?;
    let made = dir.join(name);
    let kept = ImageFile {
        dir,
        path: made.clone(),
    };
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&made)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| format!("the private file cannot be written: {e}"))?;
    Ok(kept)
}

/// Copies the program image at `source` into a sealed in-memory file kept in `cache`, checking that
/// it hashes the same when read again. A cached copy is reused; a failure leaves `cache` empty.
///
/// # Errors
/// Why the image could not be copied, was larger than `max`, or did not hash the same twice.
#[cfg(feature = "console")]
pub fn capture_into(
    cache: &std::sync::OnceLock<Image>,
    source: &Path,
    max: u64,
) -> Result<ProgramSource, String> {
    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
    if cache.get().is_none() {
        let failed = |e: rustix::io::Errno| format!("the in-memory file cannot be made: {e}");
        rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
            .map_err(failed)?;
        let image = read_image(source, max)?;
        let sha = lunchroom_core::jcs::sha256_hex(&image);
        let mut file = std::fs::File::from(
            memfd_create("lunchroom-program", MemfdFlags::ALLOW_SEALING).map_err(failed)?,
        );
        file.write_all(&image)
            .map_err(|e| format!("the in-memory file cannot be written: {e}"))?;
        fcntl_add_seals(
            &file,
            SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
        )
        .map_err(failed)?;
        // The running image must be what was copied.
        if lunchroom_core::jcs::sha256_hex(&read_image(source, max)?) != sha {
            return Err("this program's image changed while it was copied".to_owned());
        }
        let path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
        let _ = cache.set((file, path, sha));
    }
    match cache.get() {
        Some((_, path, sha)) => Ok(ProgramSource {
            path: PathBuf::from(path),
            sha256: Some(sha.clone()),
            is_installed: false,
        }),
        None => Err("the image copy was not kept".to_owned()),
    }
}

/// Copies this program's own image (`/proc/self/exe`, the inode that is running, which no one can
/// swap by replacing the file at its path) into a sealed in-memory file, checks that the image
/// still hashes the same, and names the copy by `/proc/<pid>/fd/<n>`, which the root step reads
/// (§17.2). Made before any question is asked; the file stays open.
///
/// # Errors
/// Why the image could not be copied or did not hash the same twice.
#[cfg(feature = "console")]
pub fn capture_image() -> Result<ProgramSource, String> {
    capture_into(&IMAGE, Path::new("/proc/self/exe"), MAX_IMAGE_BYTES)
}

/// Runs `program` with `step` (its whole command line, the program first) on the user's terminal;
/// whether it succeeded is the result. Nothing of Lunchroom's group goes to it.
fn run_on_terminal(tty: &std::fs::File, step: &[String]) -> Result<(), String> {
    let own = |tty: &std::fs::File| {
        tty.try_clone()
            .map_err(|e| format!("the terminal cannot be used: {e}"))
    };
    let (head, tail) = step.split_first().ok_or("nothing to run")?;
    let mut command = std::process::Command::new(head);
    command
        .args(tail)
        .stdin(Stdio::from(own(tty)?))
        .stdout(Stdio::from(own(tty)?))
        .stderr(Stdio::from(own(tty)?));
    clear_environment(&mut command);
    lunchroom_core::setgid::drop_group(&mut command);
    let status = command
        .status()
        .map_err(|e| format!("{head} cannot be started: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} did not succeed ({status}); nothing was written",
            step.join(" ")
        ))
    }
}

/// An in-memory file holding `text`, sealed against any change, and the path root opens it by
/// (`/proc/<this process>/fd/<n>`). The process is made not dumpable first, so no other process
/// of the user's can open that path; root can. `su -c` reads its password from standard input, so
/// the text cannot reach `install` through a pipe there.
fn sealed_copy(text: &str) -> Result<(std::fs::File, String), String> {
    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
    let failed = |e: rustix::io::Errno| format!("the in-memory file cannot be made: {e}");
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .map_err(failed)?;
    let mut file =
        std::fs::File::from(memfd_create("lunchroom", MemfdFlags::ALLOW_SEALING).map_err(failed)?);
    file.write_all(text.as_bytes())
        .map_err(|e| format!("the in-memory file cannot be written: {e}"))?;
    fcntl_add_seals(
        &file,
        SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
    )
    .map_err(failed)?;
    let path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
    Ok((file, path))
}

/// Runs `command` (an absolute program and its arguments) as root through the program `root`,
/// with the user's terminal for its questions and `input`, when there is some, on its standard
/// input from a pipe (for `su -c`, from a sealed in-memory file named `/dev/stdin` in `command`);
/// `what` names the step in an error. With `capture`, the standard output is the result. The one
/// way Lunchroom runs anything as root. The child has no access to the store
/// ([`lunchroom_core::setgid`]).
fn root_run(
    root: &(RootProgram, PathBuf),
    what: &str,
    command: &[String],
    input: Option<&str>,
    capture: bool,
) -> Result<String, String> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|e| format!("the terminal cannot be opened: {e}"))?;
    root_run_on(tty, root, what, command, input, capture)
}

/// As [`root_run`], on the terminal `tty`.
fn root_run_on(
    tty: std::fs::File,
    root: &(RootProgram, PathBuf),
    what: &str,
    command: &[String],
    input: Option<&str>,
    capture: bool,
) -> Result<String, String> {
    root_run_in(
        tty,
        root,
        what,
        command,
        input,
        capture,
        &Settings {
            env_folders: &SYSTEM_FOLDERS,
            read: &ROOT_READ,
        },
    )
}

/// Where `env` is looked for and how a captured output is read, for [`root_run_in`].
struct Settings<'a> {
    env_folders: &'a [&'a str],
    read: &'a ReadLimit,
}

/// As [`root_run_on`], with `env` taken from `settings.env_folders`. Where a captured run needs `env` (to put
/// the English locale on what the root program runs) and there is none, nothing is run.
fn root_run_in(
    tty: std::fs::File,
    root: &(RootProgram, PathBuf),
    what: &str,
    command: &[String],
    input: Option<&str>,
    capture: bool,
    settings: &Settings<'_>,
) -> Result<String, String> {
    let (which, program) = root;
    let own = |tty: &std::fs::File| {
        tty.try_clone()
            .map_err(|e| format!("the terminal cannot be used: {e}"))
    };
    let mut words = command.to_vec();
    // §17.2: the program image is given to the root command as its standard input, never by its
    // `/proc/<pid>/fd` path. This process is not dumpable while it holds the image, and reading
    // another process's fd link needs CAP_SYS_PTRACE when the target is not dumpable (ptrace(2),
    // rule 4; proc(5): a PTRACE_MODE_READ_FSCREDS check), which root in a container lacks
    // (2026-11-14 console proof: `install: cannot stat '/proc/57/fd/3': Permission denied`).
    // Through `su`, whose standard input carries the password, the image is a file in a new folder
    // of mode 0700 (F2 design, "Store wall"), removed when `kept` goes out of scope.
    let passed = if input.is_none() {
        pass_image(&IMAGE, *which, &mut words, &image_parent())
            .map_err(|e| format!("{what}: {e}"))?
    } else {
        ImagePass::None
    };
    let (image, _kept) = match passed {
        ImagePass::Stdin(file) => (Some(file), None),
        ImagePass::File(kept) => (None, Some(kept)),
        ImagePass::None => (None, None),
    };
    // `su` text inputs keep the sealed in-memory copy (a `/proc/<pid>/fd` path), because a file
    // the developer's processes can reach would let one of them change what root writes (a sudoers
    // rule, say), and unlike the image no hash check covers it. Root reached through `su` on a
    // host holds CAP_SYS_PTRACE; where it does not (a container) the step fails and says so.
    let mut sealed = None;
    let mut piped = input;
    if let (RootProgram::Su, Some(text)) = (which, input) {
        let (file, path) = sealed_copy(text).map_err(|e| format!("{what}: {e}"))?;
        for word in &mut words {
            if word == "/dev/stdin" {
                word.clone_from(&path);
            }
        }
        sealed = Some(file);
        piped = None;
    }
    // What is read back as text is read in English, whatever the root program does to the
    // environment (`su` gets a cleared one below).
    if capture && *which != RootProgram::Su {
        let mut with_locale =
            c_locale_words_in(settings.env_folders).map_err(|e| format!("{what}: {e}"))?;
        with_locale.extend(words);
        words = with_locale;
    }
    let argv = which.argv(program, &words);
    let (head, tail) = argv.split_first().ok_or("nothing to run")?;
    let mut child_command = std::process::Command::new(head);
    child_command
        .args(tail)
        .stdin(match (piped, image) {
            (Some(_), _) => Stdio::piped(),
            (None, Some(file)) => Stdio::from(file),
            (None, None) => Stdio::from(own(&tty)?),
        })
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::from(own(&tty)?)
        })
        .stderr(Stdio::from(tty));
    clear_environment(&mut child_command);
    lunchroom_core::setgid::drop_group(&mut child_command);
    let name = which.name();
    let mut child = child_command
        .spawn()
        .map_err(|e| format!("{name} cannot be started: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (piped, child.stdin.take()) {
        pipe.write_all(text.as_bytes())
            .map_err(|e| format!("{what}: {name} did not take its input: {e}"))?;
    }
    let (output, status) = if capture {
        let read = read_child(&mut child, settings.read, Kill::Tree);
        if let Some(text) = read.unread_text() {
            return Ok(text.to_owned());
        }
        match read {
            Read::Done { out, status, .. } => (out, status),
            _ => return Err(format!("{what}: the output of {name} cannot be read")),
        }
    } else {
        let status = child
            .wait()
            .map_err(|e| format!("{what}: {name} cannot be waited for: {e}"))?;
        (Vec::new(), status)
    };
    drop(sealed);
    if status.success() {
        Ok(String::from_utf8_lossy(&output).into_owned())
    } else {
        Err(format!("{what} did not succeed ({status})"))
    }
}

/// Makes the system group `lunchroom` as root where `/etc/group` has none (A1.3: the verifier is
/// group `lunchroom`).
fn ensure_group(root: &(RootProgram, PathBuf)) -> Result<(), String> {
    let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
    if lunchroom_core::setgid::gid_from_group(&groups, lunchroom_core::setgid::SYSTEM_USER)
        .is_some()
    {
        return Ok(());
    }
    // `groupadd --system`, or BusyBox's `addgroup -S` where the system has that alone (Alpine).
    let (program, flag) = system_program("groupadd")
        .map(|p| (p, "--system"))
        .or_else(|| system_program("addgroup").map(|p| (p, "-S")))
        .ok_or_else(|| {
            format!(
                "the group {} is not there and neither `groupadd` nor `addgroup` was found in {}",
                lunchroom_core::setgid::SYSTEM_USER,
                SYSTEM_FOLDERS.join(", ")
            )
        })?;
    let command = vec![
        program.display().to_string(),
        flag.to_owned(),
        lunchroom_core::setgid::SYSTEM_USER.to_owned(),
    ];
    root_run(root, "making the group lunchroom", &command, None, false).map(|_| ())
}

/// `name` in a system folder, as an absolute path: the console set-up runs these as root.
#[must_use]
#[cfg(feature = "console")]
pub fn system_path(name: &str) -> Option<PathBuf> {
    system_program(name)
}

/// The program that runs the root step and `install`, as the system has them, or why the
/// verifier cannot be put in place.
fn root_programs(target: &str) -> Result<((RootProgram, PathBuf), PathBuf), String> {
    let root = root_program().ok_or_else(|| no_root_program(target))?;
    let install = system_program("install").ok_or_else(|| {
        format!(
            "`install` was not found in {}, so Lunchroom cannot put the passphrase check at \
             {target}",
            SYSTEM_FOLDERS.join(", ")
        )
    })?;
    Ok((root, install))
}

/// The two commands the root step runs to put the verifier at `target` (A1.3): its folder, then
/// the file. The folder is owned by root and writable by no group or other user, and every user
/// can enter it; the file is owned by root, group `lunchroom`, mode 0640, so only Lunchroom's
/// program, which runs with group `lunchroom` (§17.2), reads it. `install` is the program that
/// does it, found in a system folder.
fn install_steps<'a>(install: &'a str, target: &'a str) -> [Vec<&'a str>; 2] {
    [
        vec![
            install,
            "-d",
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "755",
            VERIFIER_DIR,
        ],
        vec![
            install,
            "-o",
            "root",
            "-g",
            lunchroom_core::setgid::SYSTEM_USER,
            "-m",
            "640",
            "/dev/stdin",
            target,
        ],
    ]
}

/// The real user id in the text of `/proc/<pid>/status`: the first number on its `Uid:` line.
fn real_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Whether a file or folder is owned by root and has no write permission for its group or others.
fn root_only(owner: u32, mode: u32) -> bool {
    owner == 0 && mode & 0o022 == 0
}

/// Whether a folder, as it is on disk (a link is not followed), is root's alone.
fn folder_trusted(kind: std::fs::FileType, owner: u32, mode: u32) -> bool {
    kind.is_dir() && root_only(owner, mode)
}

/// Whether what is on disk is a regular file: a link, a folder or a device is not.
fn regular_file(kind: std::fs::FileType) -> bool {
    kind.is_file()
}

/// Whether a regular file is the verifier of A1.3: owned by root, in the group `lunchroom`
/// (`group` is that group's number, `None` where it does not exist), mode 0640 and no other bit.
fn file_trusted(
    kind: std::fs::FileType,
    owner: u32,
    file_group: u32,
    mode: u32,
    group: Option<u32>,
) -> bool {
    regular_file(kind) && owner == 0 && mode & 0o7777 == 0o640 && group == Some(file_group)
}

/// A terminal with echo turned off by this program itself (`tcgetattr` and `tcsetattr` through
/// `rustix::termios`, no `stty`), and turned back on when this goes out of scope, on every return
/// path and when the process is interrupted: SIGINT (Ctrl-C), SIGTERM, SIGHUP and SIGQUIT are
/// caught (`signal-hook`), echo is restored, and the signal's default action then ends the
/// process as it would have. SIGKILL, which no program can catch, is met by a watchdog process
/// started first ([`crate::echo`]): it turns echo back on when this process is gone.
///
/// A program that has put the terminal in another state (the console's alternate screen and mouse
/// capture) gives the bytes that undo it to [`EchoOff::begin_with`]; they are written on a signal
/// before the modes are restored, so an interrupted console leaves the terminal as it found it.
pub(crate) struct EchoOff {
    tty: std::fs::File,
    saved: Termios,
    watcher: Option<(signal_hook::iterator::Handle, std::thread::JoinHandle<()>)>,
    /// Told that echo is restored once it is, when this goes out of scope.
    watchdog: Option<crate::echo::Watchdog>,
}

impl std::fmt::Debug for EchoOff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoOff").finish_non_exhaustive()
    }
}

impl EchoOff {
    /// Turns echo off on `tty`; `None` when the terminal's modes cannot be read or set, in which
    /// case nothing was changed and nothing may be read from it.
    pub(crate) fn begin(tty: &std::fs::File) -> Option<Self> {
        Self::begin_with(tty, &[])
    }

    /// [`EchoOff::begin`], and on a signal `leave` is written to the terminal before the modes are
    /// restored.
    pub(crate) fn begin_with(tty: &std::fs::File, leave: &[u8]) -> Option<Self> {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
        let own = tty.try_clone().ok()?;
        let saved = tcgetattr(&own).ok()?;
        // The watchdog is running before echo goes off, so there is no moment without one; where
        // it cannot be started echo stays on and nothing is read.
        let watchdog = crate::echo::start(
            &own,
            saved.local_modes.contains(LocalModes::ECHO),
            saved.local_modes.contains(LocalModes::ECHONL),
        )
        .ok()?;
        let mut hidden = saved.clone();
        hidden
            .local_modes
            .remove(LocalModes::ECHO | LocalModes::ECHONL);
        // The signals are caught before echo goes off, so there is no moment without a restore.
        let mut signals =
            signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT]).ok()?;
        let handle = signals.handle();
        let (watch_tty, watch_saved) = (own.try_clone().ok()?, saved.clone());
        let leave = leave.to_vec();
        let thread = std::thread::Builder::new()
            .name("echo-restore".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    let _ = (&watch_tty).write_all(&leave);
                    let _ = tcsetattr(&watch_tty, OptionalActions::Now, &watch_saved);
                    // Ends the process as the signal would have done without this handler.
                    if signal_hook::low_level::emulate_default_handler(signal).is_err() {
                        std::process::exit(128 + signal);
                    }
                }
            })
            .ok();
        let this = Self {
            tty: own,
            saved,
            watcher: thread.map(|t| (handle, t)),
            watchdog,
        };
        if this.watcher.is_none() || tcsetattr(&this.tty, OptionalActions::Drain, &hidden).is_err()
        {
            // Dropped: the handler is taken off and the (unchanged) modes are set again.
            return None;
        }
        Some(this)
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.tty, OptionalActions::Now, &self.saved);
        if let Some((handle, thread)) = self.watcher.take() {
            handle.close();
            let _ = thread.join();
        }
        // Echo is restored: the watchdog is told so and ends without touching the terminal.
        drop(self.watchdog.take());
    }
}

/// Reads one line from `tty` with the typed characters hidden. Nothing is read unless echo could
/// be turned off, and echo is on again when this returns, whatever the read gave.
pub(crate) fn read_hidden_line(tty: &std::fs::File) -> Option<String> {
    let _hidden = EchoOff::begin(tty)?;
    let mut line = String::new();
    let read = BufReader::new(tty.try_clone().ok()?).read_line(&mut line);
    read.ok().filter(|n| *n > 0).map(|_| line)
}

/// What the tests of this crate share about a user who has set a passphrase.
#[cfg(test)]
pub mod fake {
    use lunchroom_core::passphrase::Verifier;

    /// The passphrase the fake user set.
    pub const PASSPHRASE: &str = "correct horse battery";

    /// The user's verifier, as the system would give it.
    pub fn verifier() -> String {
        Verifier::new(PASSPHRASE, [7; 16], super::ITERATIONS).line()
    }

    /// What the fake user types for a test that gives `answer`: the passphrase for a yes, and a
    /// passphrase that is not theirs for anything else.
    pub fn typed(answer: &str) -> String {
        if answer.trim().eq_ignore_ascii_case("yes") {
            PASSPHRASE.to_owned()
        } else {
            format!("not {PASSPHRASE}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run(program)` for a program a test has just written. Linux refuses to start a file that
    /// another process still holds open for writing (ETXTBSY, "Text file busy"), and a test thread
    /// that forks while this test writes its script holds the written file's descriptor until
    /// that child execs; the start is tried again, briefly, until no one holds it.
    fn when_written<T>(run: impl Fn() -> Result<T, String>) -> Result<T, String> {
        for _ in 0..100 {
            match run() {
                Err(e) if e.contains("Text file busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                done => return done,
            }
        }
        run()
    }
    use lunchroom_core::identity::{Harness, Identity};
    use std::os::unix::ffi::OsStrExt as _;

    struct Fake {
        ancestors: Vec<(Option<String>, String)>,
        tty: u64,
        harness_ttys: Vec<(String, u64)>,
        answer: Option<String>,
        secret: Option<String>,
        verifier: Result<String, String>,
        /// Whether anything at all was asked of the user.
        asked: bool,
        /// Whether the passphrase was asked for.
        secret_asked: bool,
        prompts: Vec<String>,
        /// What the root program's own prompt answers.
        root_confirms: Result<(), String>,
        /// The questions it was given.
        root_questions: Vec<String>,
        /// What `sudo -n -l` printed.
        sudo: Result<String, String>,
        /// Why the verifier cannot be put in place on this system.
        blocker: Option<String>,
        /// How many times a verifier was put in place.
        installed: usize,
        /// What was kept.
        kept: Vec<Kept>,
    }

    impl System for Fake {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            self.ancestors.clone()
        }
        fn own_tty(&self) -> u64 {
            self.tty
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            self.harness_ttys
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, t)| *t)
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.asked = true;
            self.prompts.push(question.to_owned());
            self.answer.clone()
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            self.verifier.clone()
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.asked = true;
            self.secret_asked = true;
            self.prompts.push(prompt.to_owned());
            self.secret.clone()
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([5; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            self.installed += 1;
            Ok(())
        }
        fn verifier_blocker(&self) -> Option<String> {
            self.blocker.clone()
        }
        fn confirm_root(&mut self, question: &str) -> Result<(), String> {
            self.root_questions.push(question.to_owned());
            self.root_confirms.clone()
        }
        fn sudo_list(&mut self) -> Result<String, String> {
            self.sudo.clone()
        }
        fn keep_sudo_answer(&mut self, kept: &Kept) {
            self.kept.push(kept.clone());
        }
    }

    fn user_shell() -> Fake {
        Fake {
            ancestors: vec![
                (Some("100:5".to_owned()), "bash".to_owned()),
                (Some("90:1".to_owned()), "sshd".to_owned()),
            ],
            tty: 34816,
            harness_ttys: vec![("200:7".to_owned(), 34817)],
            answer: Some("yes\n".to_owned()),
            secret: Some(fake::PASSPHRASE.to_owned()),
            verifier: Ok(fake::verifier()),
            asked: false,
            secret_asked: false,
            prompts: Vec::new(),
            root_confirms: Ok(()),
            root_questions: Vec::new(),
            sudo: Err("sudo is not here".to_owned()),
            blocker: None,
            installed: 0,
            kept: Vec::new(),
        }
    }

    fn mail_with_harness() -> Mail {
        let mut m = Mail::default();
        m.observe_process(
            "200:7",
            std::path::Path::new("/"),
            &Identity::new(Harness::ClaudeCode, "s1"),
        );
        m
    }

    // §7::12 — the user, in a terminal of their own, typing the passphrase: confirmed. The
    // question is shown with the prompt for the passphrase, and nothing else is asked.
    #[test]
    fn user_terminal_and_the_passphrase_confirms() {
        let mut sys = user_shell();
        assert_eq!(
            confirm(&mail_with_harness(), &mut sys, "Set the mode?\n"),
            Ok(())
        );
        assert!(sys.secret_asked);
        assert_eq!(sys.prompts, ["Set the mode?\nPassphrase: "]);
    }

    // §7::12 — anything but the passphrase changes nothing: not yes, not the passphrase in other
    // capitals or with a space, not part of it, not nothing. A yes is no confirmation.
    #[test]
    fn any_other_passphrase_is_refused() {
        for typed in [
            "",
            "yes",
            "YES",
            "no",
            "Correct horse battery",
            " correct horse battery",
            "correct horse battery ",
            "correct horse batter",
            "correct horse battery!",
        ] {
            let mut sys = user_shell();
            sys.secret = Some(typed.to_owned());
            assert_eq!(
                confirm(&mail_with_harness(), &mut sys, "q"),
                Err("the passphrase was wrong; nothing was changed".to_owned()),
                "{typed:?}"
            );
            assert!(sys.secret_asked, "{typed:?}");
        }
    }

    // §7::12 — a guarded session in the parent chain, recorded or known by its program name, is
    // denied before anything is asked, a passphrase least of all.
    #[test]
    fn guarded_session_in_parent_chain_is_refused() {
        for (key, name) in [
            ("200:7", "node"),
            ("300:9", "claude"),
            ("301:9", "codex"),
            ("302:9", "hermes"),
        ] {
            let mut sys = user_shell();
            sys.ancestors
                .insert(1, (Some(key.to_owned()), name.to_owned()));
            assert!(
                confirm(&mail_with_harness(), &mut sys, "q").is_err(),
                "{name}"
            );
            assert!(!sys.asked, "{name}");
            let mut sys = user_shell();
            sys.ancestors
                .insert(1, (Some(key.to_owned()), name.to_owned()));
            assert!(
                confirm_or_begin(&mail_with_harness(), &mut sys, "q").is_err(),
                "{name}"
            );
            assert!(!sys.asked, "{name}");
        }
    }

    // §7::12 — no controlling terminal, or the terminal of a guarded session, is denied before
    // anything is asked; a terminal that gives no passphrase is denied too.
    #[test]
    fn no_terminal_or_a_guarded_sessions_terminal_is_refused() {
        let mut sys = user_shell();
        sys.tty = 0;
        assert!(confirm(&mail_with_harness(), &mut sys, "q").is_err());
        assert!(!sys.asked);
        let mut sys = user_shell();
        sys.tty = 34817;
        assert!(confirm(&mail_with_harness(), &mut sys, "q").is_err());
        assert!(!sys.asked);
        let mut sys = user_shell();
        sys.secret = None;
        assert!(confirm(&mail_with_harness(), &mut sys, "q").is_err());
        assert!(sys.secret_asked);
    }

    // §7::12 — with no passphrase set, a command only the user may run is denied, with the
    // command that sets one, and neither the passphrase nor a yes is asked for.
    #[test]
    fn no_verifier_is_refused_with_the_command_that_sets_one() {
        let mut sys = user_shell();
        sys.verifier = Err(NO_VERIFIER.to_owned());
        let reason = confirm(&mail_with_harness(), &mut sys, "q")
            .err()
            .unwrap_or_default();
        assert!(reason.contains("`lunchroom passphrase`"), "{reason}");
        assert!(!sys.asked);
    }

    // §7::12 — a verifier the system reports as not root's alone (or one that cannot be read) is
    // denied as it is reported, and nothing is asked: a file the user's account could have written
    // proves nothing.
    #[test]
    fn a_verifier_not_owned_by_root_is_refused() {
        let reported = "the passphrase check at /etc/lunchroom/1000.verifier cannot be trusted \
                        (it is not a regular file owned by root and writable by no one else)";
        let mut sys = user_shell();
        sys.verifier = Err(reported.to_owned());
        assert_eq!(
            confirm(&mail_with_harness(), &mut sys, "q"),
            Err(reported.to_owned())
        );
        assert!(!sys.asked);
        let mut sys = user_shell();
        sys.verifier = Err(reported.to_owned());
        assert_eq!(
            confirm_or_begin(&mail_with_harness(), &mut sys, "q"),
            Err(reported.to_owned())
        );
        assert!(!sys.asked);
    }

    // §7::12 — a verifier line that is not the format (a different scheme, too few iterations, a
    // hash of the wrong length, two lines) is denied before the passphrase is asked.
    #[test]
    fn a_malformed_verifier_is_refused() {
        let good = fake::verifier();
        for line in [
            String::new(),
            "garbage".to_owned(),
            good.replace("pbkdf2-sha256", "pbkdf2-sha1"),
            good.replace("$1$", "$0$"),
            good[..good.len() - 2].to_owned(),
            format!("{good}\n{good}"),
            format!("{good}$x"),
        ] {
            let mut sys = user_shell();
            sys.verifier = Ok(line.clone());
            let reason = confirm(&mail_with_harness(), &mut sys, "q")
                .err()
                .unwrap_or_default();
            assert!(reason.contains("cannot be used"), "{line:?}: {reason}");
            assert!(
                reason.contains("`lunchroom passphrase`"),
                "{line:?}: {reason}"
            );
            assert!(!sys.asked, "{line:?}");
        }
    }

    // §7::54, §7::39 — the install that sets the first passphrase is confirmed by the
    // root program's own prompt, and by nothing a person is not needed for: a typed `yes` confirms
    // nothing and is never asked for; a refusal by the root program writes nothing; with a
    // verifier the passphrase is asked for; a verifier that cannot be trusted is not passed over.
    #[test]
    fn only_the_first_install_confirms_with_the_root_programs_prompt() {
        let mail = mail_with_harness();
        let mut sys = user_shell();
        sys.verifier = Err(NO_VERIFIER.to_owned());
        assert_eq!(confirm_or_begin(&mail, &mut sys, "q\n"), Ok(()));
        assert_eq!(sys.root_questions, ["q\n"]);
        assert!(!sys.asked, "a yes was asked for: {:?}", sys.prompts);
        assert!(!sys.secret_asked);
        // The root program refuses (`sudo -v` after `sudo -k` fails): nothing is confirmed, whatever
        // the user types, and no `yes` is asked.
        for answer in ["yes\n", "YES\n", "y\n", ""] {
            let mut sys = user_shell();
            sys.verifier = Err(NO_VERIFIER.to_owned());
            sys.answer = Some(answer.to_owned());
            sys.root_confirms = Err("sudo said no".to_owned());
            assert_eq!(
                confirm_or_begin(&mail, &mut sys, "q"),
                Err("sudo said no".to_owned()),
                "{answer:?}"
            );
            assert!(!sys.asked, "{answer:?}");
        }
        // With a verifier the passphrase is asked for, and the root program is not.
        let mut sys = user_shell();
        sys.secret = Some("yes".to_owned());
        assert!(confirm_or_begin(&mail, &mut sys, "q").is_err());
        assert!(sys.secret_asked);
        assert_eq!(sys.prompts, ["qPassphrase: "]);
        assert_eq!(sys.root_questions.len(), 0);
        let mut sys = user_shell();
        assert_eq!(confirm_or_begin(&mail, &mut sys, "q"), Ok(()));
        // A verifier that cannot be trusted is no verifier to begin with.
        let mut sys = user_shell();
        sys.verifier = Err("the passphrase check cannot be trusted".to_owned());
        assert!(confirm_or_begin(&mail, &mut sys, "q").is_err());
        assert!(!sys.asked);
        assert_eq!(sys.root_questions.len(), 0);
    }

    // §7::58, §7::67, §7::68, §17::64 — every install reads `sudo -n -l` and keeps the
    // answer; where a rule runs a command without a password the user is told, in the question
    // shown before the confirmation, that it proves no person is present and that the passphrase
    // check does not hold on the account; where there is none, the question is as it was.
    #[test]
    fn the_install_reads_sudo_keeps_the_answer_and_says_what_a_free_rule_means() {
        let mail = mail_with_harness();
        let free =
            "User dev may run the following commands on h:\n    (root) NOPASSWD: /usr/bin/id\n";
        let mut sys = user_shell();
        sys.verifier = Err(NO_VERIFIER.to_owned());
        sys.sudo = Ok(free.to_owned());
        assert_eq!(confirm_or_begin(&mail, &mut sys, "q\n"), Ok(()));
        assert_eq!(sys.kept.len(), 1);
        assert_eq!(sys.kept[0].own, free);
        let question = &sys.root_questions[0];
        for part in [
            "(root) NOPASSWD: /usr/bin/id",
            "proves no person is present",
            "does not hold on this account",
            "K1.10",
            "q\n",
        ] {
            assert!(question.contains(part), "{part}: {question}");
        }
        // The same statement before the passphrase prompt of an install that has a verifier.
        let mut sys = user_shell();
        sys.sudo = Ok(free.to_owned());
        assert_eq!(confirm_or_begin(&mail, &mut sys, "q\n"), Ok(()));
        assert!(sys.prompts[0].contains("proves no person is present"));
        assert_eq!(sys.kept.len(), 1);
        // An account whose rules all ask for a password, and one where sudo cannot be read.
        for text in [
            Ok("User dev may run the following commands on h:\n    (ALL : ALL) ALL\n".to_owned()),
            Ok("sudo: a password is required\n".to_owned()),
            Err("sudo is not here".to_owned()),
        ] {
            let mut sys = user_shell();
            sys.verifier = Err(NO_VERIFIER.to_owned());
            sys.sudo = text.clone();
            assert_eq!(confirm_or_begin(&mail, &mut sys, "q\n"), Ok(()));
            assert_eq!(sys.root_questions, ["q\n"], "{text:?}");
            assert_eq!(sys.kept.len(), usize::from(text.is_ok()), "{text:?}");
        }
    }

    // §7::12 — root's alone: the owner is root and neither the group nor others may write.
    #[test]
    fn a_file_is_root_only_when_root_owns_it_and_no_one_else_writes() {
        for (owner, mode, expected) in [
            (0, 0o100_644, true),
            (0, 0o040_755, true),
            (0, 0o100_600, true),
            (0, 0o100_664, false),
            (0, 0o100_646, false),
            (0, 0o100_666, false),
            (0, 0o040_775, false),
            (0, 0o040_757, false),
            (1000, 0o100_644, false),
            (1000, 0o100_600, false),
            (1, 0o040_755, false),
        ] {
            assert_eq!(root_only(owner, mode), expected, "{owner} {mode:o}");
        }
    }

    /// A pseudo-terminal: the controlling side, and the terminal side a program reads from.
    fn pty() -> std::io::Result<(std::fs::File, std::fs::File)> {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(std::ffi::OsStr::from_bytes(name.as_bytes()))?;
        Ok((std::fs::File::from(master), slave))
    }

    fn echo_on(tty: &std::fs::File) -> bool {
        tcgetattr(tty).is_ok_and(|m| m.local_modes.contains(LocalModes::ECHO))
    }

    /// Waits until echo is off on `tty`.
    fn wait_for_echo_off(tty: &std::fs::File) -> std::io::Result<()> {
        for _ in 0..500 {
            if !echo_on(tty) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(std::io::Error::other("echo was never turned off"))
    }

    // §7::46, §7::47, §7::39 — on a pseudo-terminal, with no `stty` anywhere on the
    // path searched: echo is off while the passphrase is typed (read from the terminal's own
    // modes), the typed line comes back, and echo is on again after the read; also when the
    // read gives nothing because the input ended.
    #[test]
    fn the_passphrase_is_read_with_echo_off_and_echo_is_restored() -> std::io::Result<()> {
        let (master, slave) = pty()?;
        assert!(echo_on(&slave));
        let tty = slave.try_clone()?;
        let reader = std::thread::spawn(move || read_hidden_line(&tty));
        wait_for_echo_off(&slave)?;
        let mut typing = master.try_clone()?;
        typing.write_all(b"correct horse battery\n")?;
        let got = reader.join().unwrap_or_default();
        assert_eq!(got.as_deref(), Some("correct horse battery\n"));
        assert!(echo_on(&slave), "echo was left off");
        // The read gives nothing (end of input, Ctrl-D): echo is on again all the same.
        let tty = slave.try_clone()?;
        let reader = std::thread::spawn(move || read_hidden_line(&tty));
        wait_for_echo_off(&slave)?;
        typing.write_all(&[4])?;
        assert_eq!(reader.join().unwrap_or_default(), None);
        assert!(echo_on(&slave), "echo was left off after the end of input");
        drop(typing);
        drop(master);
        Ok(())
    }

    // §7::46 — a terminal whose modes cannot be read (a file, not a terminal) is never
    // read from: nothing is read unless echo could be turned off.
    #[test]
    fn nothing_is_read_where_echo_cannot_be_turned_off() -> std::io::Result<()> {
        let dir = scratch("no-tty");
        let file = dir.join("input");
        std::fs::write(&file, "secret\n")?;
        let opened = std::fs::File::open(&file)?;
        assert_eq!(read_hidden_line(&opened), None);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// A scratch folder of this test run.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lunchroom-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        dir
    }

    // §7::47 — echo is on again after the program is interrupted. The test runs this
    // test binary again with its terminal side on a pseudo-terminal, waits until echo is off, and
    // sends SIGINT (what Ctrl-C sends), SIGTERM, SIGHUP or SIGQUIT; the child ends by that signal,
    // as it would have without the handler, and echo is on.
    #[test]
    fn echo_is_restored_when_the_program_is_interrupted() -> std::io::Result<()> {
        use rustix::process::{Pid, Resource, Rlimit, Signal, kill_process, setrlimit};
        use std::os::unix::process::ExitStatusExt as _;
        // SIGQUIT's default action dumps core: the child is given no room for one.
        let no_core = Rlimit {
            current: Some(0),
            maximum: Some(0),
        };
        setrlimit(Resource::Core, no_core)?;
        for (signal, number) in [
            (Signal::INT, 2),
            (Signal::TERM, 15),
            (Signal::HUP, 1),
            (Signal::QUIT, 3),
        ] {
            let (master, slave) = pty()?;
            let mut child = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "terminal::tests::echo_child_reads_a_hidden_line",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("LUNCHROOM_ECHO_CHILD", "1")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            wait_for_echo_off(&slave)?;
            let pid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(0));
            assert!(pid.is_some());
            if let Some(pid) = pid {
                kill_process(pid, signal)?;
            }
            let status = child.wait()?;
            assert_eq!(status.signal(), Some(number), "{signal:?}: {status:?}");
            assert!(echo_on(&slave), "echo was left off after {signal:?}");
            drop(master);
        }
        Ok(())
    }

    // The child of the test above: reads a hidden line from its standard input (a terminal) when
    // told to, and does nothing in an ordinary run.
    #[test]
    fn echo_child_reads_a_hidden_line() {
        if std::env::var_os("LUNCHROOM_ECHO_CHILD").is_none() {
            return;
        }
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/stdin");
        if let Ok(tty) = tty {
            let _ = read_hidden_line(&tty);
        }
    }

    // §7::19, §7::24 — sudo, install and stty are looked for only in the fixed system
    // folders, in order, and only as executable regular files: an earlier folder wins, a folder
    // with no such file, a non-executable file or a folder is passed over, and nothing found is
    // `None` (never a search of the user's PATH).
    #[test]
    fn system_programs_are_found_only_in_the_fixed_folders_in_order() {
        let root = std::env::temp_dir().join(format!("lunchroom-programs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = ["a", "b", "c"].map(|d| root.join(d));
        for d in &dirs {
            assert!(std::fs::create_dir_all(d).is_ok());
        }
        let put = |dir: &Path, name: &str, mode: u32| {
            let file = dir.join(name);
            assert!(std::fs::write(&file, "#!/bin/sh\n").is_ok());
            assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).is_ok());
        };
        put(&dirs[0], "plain", 0o644);
        assert!(std::fs::create_dir_all(dirs[0].join("folder")).is_ok());
        put(&dirs[1], "plain", 0o755);
        put(&dirs[1], "both", 0o755);
        put(&dirs[2], "both", 0o755);
        let names: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        let folders: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(program_in(&folders, "both"), Some(dirs[1].join("both")));
        assert_eq!(program_in(&folders, "plain"), Some(dirs[1].join("plain")));
        assert_eq!(program_in(&folders, "folder"), None);
        assert_eq!(program_in(&folders, "absent"), None);
        assert_eq!(program_in(&[], "both"), None);
        let _ = std::fs::remove_dir_all(&root);
        // The folders are the four system ones; the user's PATH is not among them.
        assert_eq!(SYSTEM_FOLDERS, ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
    }

    // §7::51, §7::52, §7::53 — a system with none of the four is told that none is
    // present, that no passphrase is set, and what to do; the passphrase is not read when it
    // cannot be hidden. (The failure exit status and the absence of a verifier are in
    // `none_of_the_four_present_sets_no_passphrase_and_fails`.)
    #[test]
    fn a_system_with_no_root_program_is_told_what_to_do() {
        let text = no_root_program("/etc/lunchroom/1000.verifier");
        for part in [
            "none of sudo, doas, run0",
            "was found in /usr/bin, /bin, /usr/sbin, /sbin",
            "sets no passphrase",
            "/etc/lunchroom/1000.verifier",
            "`lunchroom passphrase`",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        assert!(!NO_SECRET_READ.contains("stty"));
    }

    // §7::41, §7::42, §7::43, §17::14 — the verifier is put in place owned by root, group
    // `lunchroom`, mode 0640, so no process of the user's reads it; its folder is owned by root and
    // writable by no group or other user, and every user can enter it.
    #[test]
    fn the_verifier_is_installed_root_and_lunchroom_s_alone_in_a_root_only_folder() {
        let [folder, file] = install_steps("/bin/install", "/etc/lunchroom/1000.verifier");
        assert_eq!((folder[0], file[0]), ("/bin/install", "/bin/install"));
        let after = |step: &[&str], flag: &str| {
            step.iter()
                .position(|a| *a == flag)
                .and_then(|k| step.get(k + 1))
                .map(|v| (*v).to_owned())
        };
        assert_eq!(file.last(), Some(&"/etc/lunchroom/1000.verifier"));
        assert_eq!(folder.last(), Some(&VERIFIER_DIR));
        // The file: root, group lunchroom, 0640.
        assert_eq!(after(&file, "-o").as_deref(), Some("root"));
        assert_eq!(after(&file, "-g").as_deref(), Some("lunchroom"));
        assert_eq!(after(&file, "-m").as_deref(), Some("640"));
        // The folder: root, no group or other write, entered by every user.
        assert_eq!(after(&folder, "-o").as_deref(), Some("root"));
        let folder_mode = after(&folder, "-m").and_then(|m| u32::from_str_radix(&m, 8).ok());
        assert_eq!(folder_mode.map(|m| m & 0o555), Some(0o555));
        assert!(folder_mode.is_some_and(|m| root_only(0, m)));
        // The file's mode gives others nothing and the group no write.
        assert!(!file.contains(&"644") && !file.contains(&"root,root"));
        let file_mode = after(&file, "-m").and_then(|m| u32::from_str_radix(&m, 8).ok());
        assert_eq!(file_mode.map(|m| m & 0o027), Some(0));
        assert!(file_mode.is_some_and(|m| root_only(0, m)));
    }

    // §7::20 — the verifier's folder and the verifier are each root's alone: owned by
    // root and writable by neither group nor others. Each failure refuses on its own; a link, a
    // folder or anything but a regular file is no verifier.
    #[test]
    fn the_verifier_and_its_folder_must_each_be_root_s_alone() {
        let scratch = std::env::temp_dir().join(format!("lunchroom-kinds-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap_or_default();
        let file = scratch.join("file");
        std::fs::write(&file, "x").unwrap_or_default();
        let link = scratch.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap_or_default();
        let kind = |p: &std::path::Path| std::fs::symlink_metadata(p).map(|m| m.file_type()).ok();
        let kinds = (kind(&scratch), kind(&file), kind(&link));
        assert!(
            kinds.0.is_some() && kinds.1.is_some() && kinds.2.is_some(),
            "{kinds:?}"
        );
        let (Some(dir_kind), Some(file_kind), Some(link_kind)) = kinds else {
            return;
        };
        // The positive cases.
        assert!(folder_trusted(dir_kind, 0, 0o040_755));
        assert!(file_trusted(file_kind, 0, 990, 0o100_640, Some(990)));
        // The folder: not owned by root; group-writable; other-writable; not a folder.
        assert!(!folder_trusted(dir_kind, 1000, 0o040_755));
        assert!(!folder_trusted(dir_kind, 0, 0o040_775));
        assert!(!folder_trusted(dir_kind, 0, 0o040_757));
        assert!(!folder_trusted(file_kind, 0, 0o040_755));
        assert!(!folder_trusted(link_kind, 0, 0o040_755));
        // The file (7::41, 7::43): root's, group `lunchroom`, mode 0640 and nothing
        // else: not 0644, 0600, 0660, 0750 or with a setuid bit, not another owner, not another
        // group, not when the group does not exist.
        for (owner, group, mode, want) in [
            (1000, 990, 0o100_640, Some(990)),
            (0, 0, 0o100_640, Some(990)),
            (0, 1000, 0o100_640, Some(990)),
            (0, 990, 0o100_644, Some(990)),
            (0, 990, 0o100_600, Some(990)),
            (0, 990, 0o100_660, Some(990)),
            (0, 990, 0o100_664, Some(990)),
            (0, 990, 0o100_646, Some(990)),
            (0, 990, 0o100_750, Some(990)),
            (0, 990, 0o104_640, Some(990)),
            (0, 990, 0o100_640, None),
        ] {
            assert!(
                !file_trusted(file_kind, owner, group, mode, want),
                "{owner} {group} {mode:o} {want:?}"
            );
        }
        // The file as a symbolic link (read as it is on disk, never followed), or a folder.
        assert!(!file_trusted(link_kind, 0, 990, 0o100_640, Some(990)));
        assert!(!regular_file(link_kind));
        assert!(!file_trusted(dir_kind, 0, 990, 0o100_640, Some(990)));
        std::fs::remove_dir_all(&scratch).unwrap_or_default();
    }

    // The real user id is the first number on the `Uid:` line of `/proc/<pid>/status` (the real
    // one, before the effective, saved and file-system ids), and the user's verifier is named by it.
    #[test]
    fn the_real_user_id_is_the_first_on_the_uid_line() {
        let status =
            "Name:\tlunchroom\nUmask:\t0022\nUid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(real_uid(status), Some(1000));
        assert_eq!(real_uid("Uid:\t0\t1000\t1000\t1000\n"), Some(0));
        assert_eq!(real_uid("Gid:\t1000\n"), None);
        assert_eq!(real_uid("Uid:\n"), None);
        assert_eq!(real_uid("Uid:\tx\n"), None);
        assert_eq!(real_uid(""), None);
        assert_eq!(
            verifier_path(1000),
            std::path::PathBuf::from("/etc/lunchroom/1000.verifier")
        );
        // This process reports its own.
        assert!(OsSystem.uid().is_some());
        assert_eq!(
            OsSystem.uid(),
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| real_uid(&s))
        );
    }

    /// The real system's view of processes, with the terminal facts of a user's shell and the
    /// user's passphrase given.
    struct RealProcesses {
        tty: u64,
    }

    impl System for RealProcesses {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            user_shell().ancestors
        }
        fn own_tty(&self) -> u64 {
            self.tty
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            OsSystem.tty_of(key)
        }
        fn ask(&mut self, _question: &str) -> Option<String> {
            Some("yes\n".to_owned())
        }
        fn uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn verifier(&self) -> Result<String, String> {
            Ok(fake::verifier())
        }
        fn ask_secret(&mut self, _prompt: &str) -> Option<String> {
            Some(fake::PASSPHRASE.to_owned())
        }
        fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
            Ok([5; passphrase::SALT_BYTES])
        }
        fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
    }

    // §7::12 (A1.3), `process.rs`: a process is named by its pid and its start time, so a pid the
    // system reuses never names an earlier process. A key with this process's pid and any other
    // start time (later, earlier, none, not a number) is not this process: its terminal is not
    // read, and a process recorded under it is not taken for one running now; a pid nothing runs
    // under is no process at all.
    #[test]
    fn a_reused_pid_is_not_the_recorded_process() {
        let me = std::process::id();
        let key = process::key(me).unwrap_or_default();
        let (pid, start) = key.split_once(':').unwrap_or_default();
        assert_eq!(pid, me.to_string());
        let start: u64 = start.parse().unwrap_or_default();
        assert_ne!(start, 0, "{key}");
        // The process's own key names it.
        assert_eq!(OsSystem.tty_of(&key), process::tty(me));
        for reused in [
            format!("{me}:{}", start + 1),
            format!("{me}:{}", start - 1),
            format!("{me}:0"),
            format!("{me}:"),
            format!("{me}:x"),
            me.to_string(),
            format!("{me}:{start}:{start}"),
            format!("{me}: {start}"),
        ] {
            assert_eq!(OsSystem.tty_of(&reused), None, "{reused}");
        }
        assert_eq!(OsSystem.tty_of(&format!("{}:{start}", u32::MAX)), None);
        // A recorded harness process whose pid is this process's but whose start time is not: its
        // terminal is none, so it does not share the user's terminal. Recorded under the right
        // key, and in the terminal the user answers from, it does.
        let own = process::tty(me).unwrap_or_default();
        let mut mail = Mail::default();
        let who = Identity::new(Harness::ClaudeCode, "s1");
        mail.observe_process(
            &format!("{me}:{}", start + 1),
            std::path::Path::new("/"),
            &who,
        );
        let shared = own.max(34816);
        assert_eq!(
            confirm(&mail, &mut RealProcesses { tty: shared }, "q"),
            Ok(())
        );
        if own != 0 {
            let mut mail = Mail::default();
            mail.observe_process(&key, std::path::Path::new("/"), &who);
            assert!(confirm(&mail, &mut RealProcesses { tty: own }, "q").is_err());
        }
    }

    // §7::12 (A1.3): a harness is recognised in the parent chain by the key a hook recorded for it,
    // pid and start time together. The same pid under another start time is a different process:
    // it is not the recorded session, however the ancestors are named.
    #[test]
    fn a_pid_recorded_with_another_start_time_is_not_a_guarded_ancestor() {
        let mail = mail_with_harness();
        for (key, refused) in [
            ("200:7", true),
            ("200:8", false),
            ("200:6", false),
            ("201:7", false),
        ] {
            let mut sys = user_shell();
            sys.ancestors
                .insert(1, (Some(key.to_owned()), "node".to_owned()));
            assert_eq!(confirm(&mail, &mut sys, "q").is_err(), refused, "{key}");
            assert_eq!(sys.asked, !refused, "{key}");
        }
        // A harness process under its recorded key in the user's terminal refuses; the same pid
        // reused (another start time) in that terminal is not a guarded session's terminal.
        let mut sys = user_shell();
        sys.tty = 34817;
        assert!(confirm(&mail, &mut sys, "q").is_err());
        let mut sys = user_shell();
        sys.tty = 34817;
        sys.harness_ttys = vec![("200:8".to_owned(), 34817)];
        assert_eq!(confirm(&mail, &mut sys, "q"), Ok(()));
    }

    /// A folder holding stand-in programs `names`, each a script that writes its command line,
    /// one word to a line, to `<name>.argv` beside it, then runs `rest`.
    /// Waits until the script at `program`, just written, can be executed: another thread's `fork`
    /// can still hold the write descriptor for a moment, and `exec` then fails with "Text file
    /// busy" (os error 26). The script is run once with an argument it ignores.
    fn runnable(program: &Path) {
        for _ in 0..500 {
            let run = std::process::Command::new(program)
                .arg("--runnable")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match run {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                // Started: the script is executable. It is not waited for (one may never end).
                Ok(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                Err(_) => return,
            }
        }
    }

    fn stand_ins(dir: &Path, names: &[(&str, &str)]) {
        for (name, rest) in names {
            let file = dir.join(name);
            let log = dir.join(format!("{name}.argv"));
            let text = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > '{}'\n{rest}\n",
                log.display()
            );
            assert!(std::fs::write(&file, text).is_ok());
            assert!(
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).is_ok()
            );
            runnable(&file);
        }
    }

    fn argv_logged(dir: &Path, name: &str) -> Vec<String> {
        std::fs::read_to_string(dir.join(format!("{name}.argv")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    const RUN0_HELP_WITH: &str = "run0 [OPTIONS...] COMMAND [ARGUMENTS...]\n\nElevate privileges interactively.\n\n  -h --help                Show this help\n     --version             Show package version\n  -K                       Revoke all temporary authorizations\n  -v                       Validate authorization\n     --user=USER           Run as system user\n";
    const RUN0_HELP_WITHOUT: &str = "run0 [OPTIONS...] COMMAND [ARGUMENTS...]\n\nElevate privileges interactively.\n\n  -h --help                Show this help\n     --version             Show package version\n     --user=USER           Run as system user\n  -D --chdir=PATH          Set working directory\n";

    // §7::49 — `run0` is read as listing its `-K` and `-v` options only where its help
    // does: both must be there, as options and not as parts of other words.
    #[test]
    fn run0_is_used_only_where_its_help_lists_k_and_v() {
        assert!(run0_lists_k_and_v(RUN0_HELP_WITH));
        assert!(!run0_lists_k_and_v(RUN0_HELP_WITHOUT));
        assert!(!run0_lists_k_and_v("  -K  revoke\n"));
        assert!(!run0_lists_k_and_v("  -v  validate\n"));
        assert!(!run0_lists_k_and_v("  --version  -Kx  -vv\n"));
        assert!(run0_lists_k_and_v("-K, -v"));
        assert!(!run0_lists_k_and_v(""));
    }

    // §7::48, §7::49, §7::50 — the step that writes as root runs the first of sudo,
    // doas, run0, su that is present, in that order, taken only from the folders given (the
    // system's four; never the user's PATH); run0 without -K or -v in its help is passed over.
    #[test]
    fn the_first_present_of_the_four_in_order_is_the_root_program() {
        let dir = scratch("choose");
        let decoy = scratch("choose-decoy");
        stand_ins(&decoy, &[("sudo", "true")]);
        let folders = [dir.display().to_string()];
        let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
        let with = |_: &Path| Some(RUN0_HELP_WITH.to_owned());
        let without = |_: &Path| Some(RUN0_HELP_WITHOUT.to_owned());
        let none = |_: &Path| None;
        let chosen = |help: &dyn Fn(&Path) -> Option<String>| choose_root_program(&folders, help);
        // None present, though a decoy sudo is elsewhere (as it would be on the user's PATH).
        assert_eq!(chosen(&with), None);
        // Only su.
        stand_ins(&dir, &[("su", "true")]);
        assert_eq!(chosen(&with), Some((RootProgram::Su, dir.join("su"))));
        // run0 without the options is passed over; with them it comes before su.
        stand_ins(&dir, &[("run0", "true")]);
        assert_eq!(chosen(&without), Some((RootProgram::Su, dir.join("su"))));
        assert_eq!(chosen(&none), Some((RootProgram::Su, dir.join("su"))));
        assert_eq!(chosen(&with), Some((RootProgram::Run0, dir.join("run0"))));
        // doas comes before run0, sudo before doas.
        stand_ins(&dir, &[("doas", "true")]);
        assert_eq!(chosen(&with), Some((RootProgram::Doas, dir.join("doas"))));
        stand_ins(&dir, &[("sudo", "true")]);
        assert_eq!(chosen(&with), Some((RootProgram::Sudo, dir.join("sudo"))));
        // The folders of the real system are the four, in order, and nothing else.
        assert_eq!(SYSTEM_FOLDERS, ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
        assert_eq!(
            RootProgram::ORDER.map(RootProgram::name),
            ["sudo", "doas", "run0", "su"]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&decoy);
    }

    // §7::51, §7::52, §7::53 — with none of the four present the install says so, sets
    // no passphrase (nothing is asked and no verifier is put in place) and ends with a failure.
    #[test]
    fn none_of_the_four_present_sets_no_passphrase_and_fails() {
        let folders: [&str; 0] = [];
        assert_eq!(choose_root_program(&folders, &|_| None), None);
        let mut sys = user_shell();
        sys.verifier = Err(NO_VERIFIER.to_owned());
        sys.blocker = Some(no_root_program("/etc/lunchroom/1000.verifier"));
        let out = crate::user::set_passphrase(&mut sys);
        assert_ne!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert!(
            out.stderr.contains("none of sudo, doas, run0"),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("sets no passphrase"), "{}", out.stderr);
        assert_eq!(sys.installed, 0);
        assert!(!sys.asked, "{:?}", sys.prompts);
    }

    // §7::48, §7::55, §7::56, §7::57 — what each program is run with: its own way of
    // running a command as root, and, before anything is written, its own clear-then-ask pair.
    #[test]
    fn each_program_has_its_own_command_line_and_confirmation() {
        let command = [
            "/usr/bin/install".to_owned(),
            "-d".to_owned(),
            "/etc/it's".to_owned(),
        ];
        let path = |n: &str| PathBuf::from(format!("/usr/bin/{n}"));
        assert_eq!(
            RootProgram::Sudo.argv(&path("sudo"), &command),
            ["/usr/bin/sudo", "/usr/bin/install", "-d", "/etc/it's"]
        );
        assert_eq!(
            RootProgram::Doas.argv(&path("doas"), &command),
            ["/usr/bin/doas", "/usr/bin/install", "-d", "/etc/it's"]
        );
        assert_eq!(
            RootProgram::Run0.argv(&path("run0"), &command),
            ["/usr/bin/run0", "/usr/bin/install", "-d", "/etc/it's"]
        );
        assert_eq!(
            RootProgram::Su.argv(&path("su"), &command),
            [
                "/usr/bin/su",
                "-c",
                "'/usr/bin/install' '-d' '/etc/it'\\''s'",
                "root"
            ]
        );
        let truth = path("true");
        let steps = |w: RootProgram| w.confirmation(&path(w.name()), &truth);
        assert_eq!(
            steps(RootProgram::Sudo),
            [["/usr/bin/sudo", "-k"], ["/usr/bin/sudo", "-v"]]
        );
        assert_eq!(
            steps(RootProgram::Doas),
            [["/usr/bin/doas", "-L"], ["/usr/bin/doas", "/usr/bin/true"]]
        );
        assert_eq!(
            steps(RootProgram::Run0),
            [["/usr/bin/run0", "-K"], ["/usr/bin/run0", "-v"]]
        );
        assert_eq!(
            steps(RootProgram::Su),
            [vec!["/usr/bin/su", "-c", "'/usr/bin/true'", "root"]]
        );
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("'"), ["'", "'\\''", "'"].concat());
    }

    // §7::48 — a command runs as the root program says, for each of the four (stand-ins
    // in a fixture folder): its input reaches the command, its output comes back, a failing
    // command is an error; for `su -c` the input is a sealed in-memory file named in the command.
    #[test]
    fn the_root_step_runs_with_each_of_the_four_programs() -> std::io::Result<()> {
        let dir = scratch("root-run");
        stand_ins(
            &dir,
            &[
                ("sudo", "exec \"$@\""),
                ("doas", "exec \"$@\""),
                ("run0", "exec \"$@\""),
                ("su", "exit 0"),
            ],
        );
        let tty = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
        };
        let cat = vec!["/bin/cat".to_owned(), "/dev/stdin".to_owned()];
        for which in [RootProgram::Sudo, RootProgram::Doas, RootProgram::Run0] {
            let root = (which, dir.join(which.name()));
            let out = root_run_on(tty()?, &root, "cat", &cat, Some("one line\n"), true);
            assert_eq!(out.as_deref(), Ok("one line\n"), "{which:?}");
            // What is read back as text is read in English: `env` sets the locale inside.
            let mut expected = vec![dir.join(which.name()).display().to_string()];
            expected.extend(c_locale_words_in(&SYSTEM_FOLDERS).unwrap_or_default());
            expected.extend(["/bin/cat".to_owned(), "/dev/stdin".to_owned()]);
            assert_eq!(argv_logged(&dir, which.name()), expected);
            let failed = root_run_on(
                tty()?,
                &root,
                "false",
                &["/bin/false".to_owned()],
                None,
                false,
            );
            assert!(failed.is_err(), "{which:?}");
        }
        // su: the command is one quoted string for `-c`, and `/dev/stdin` has become the path of a
        // sealed file of this process.
        let root = (RootProgram::Su, dir.join("su"));
        let out = root_run_on(tty()?, &root, "cat", &cat, Some("one line\n"), false);
        assert_eq!(out.as_deref(), Ok(""));
        let argv = argv_logged(&dir, "su");
        assert_eq!(argv.len(), 4, "{argv:?}");
        assert_eq!((argv[1].as_str(), argv[3].as_str()), ("-c", "root"));
        let prefix = format!("'/bin/cat' '/proc/{}/fd/", std::process::id());
        assert!(argv[2].starts_with(&prefix), "{}", argv[2]);
        assert!(!argv[2].contains("/dev/stdin"), "{}", argv[2]);
        // Without input su gets the command as it is.
        let _ = root_run_on(
            tty()?,
            &root,
            "id",
            &["/usr/bin/id".to_owned()],
            None,
            false,
        );
        assert_eq!(argv_logged(&dir, "su")[2], "'/usr/bin/id'");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // §7::43, §17::14 — the sealed copy `su -c` reads holds the text, cannot be changed,
    // and cannot be opened by another process of the user's (this process is not dumpable, so
    // `/proc/<pid>/fd` is root's; the test runs as a user other than root).
    #[test]
    fn the_sealed_copy_is_unchangeable_and_closed_to_the_users_other_processes() {
        use std::io::Seek as _;
        let made = sealed_copy("$pbkdf2$verifier\n");
        assert!(made.is_ok(), "{made:?}");
        let Ok((mut file, path)) = made else { return };
        assert!(file.rewind().is_ok());
        let mut text = String::new();
        assert!(file.read_to_string(&mut text).is_ok());
        assert_eq!(text, "$pbkdf2$verifier\n");
        assert!(
            file.write_all(b"x").is_err(),
            "the sealed file took a write"
        );
        assert!(path.starts_with("/proc/"), "{path}");
        // Another process of the user's (here `cat`) cannot open it; this one, being the process
        // itself, can.
        let other = std::process::Command::new("/bin/cat")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !rustix::process::getuid().is_root() {
            assert!(other.as_ref().is_ok_and(|s| !s.success()), "{other:?}");
        }
        drop(file);
    }

    // §7::67, §7::68, §17::64, §17::65, §17::74 — the answer of `sudo -n -l` is read, and with
    // the root step also `sudo -l -U <user>` for each other plain user a password-free rule runs as.
    #[test]
    fn the_install_reads_and_keeps_sudos_answer_with_the_others_from_the_root_step() {
        struct Sudo {
            asked: Vec<String>,
        }
        impl System for Sudo {
            fn ancestors(&self) -> Vec<(Option<String>, String)> {
                Vec::new()
            }
            fn own_tty(&self) -> u64 {
                1
            }
            fn tty_of(&self, _: &str) -> Option<u64> {
                None
            }
            fn ask(&mut self, _: &str) -> Option<String> {
                None
            }
            fn uid(&self) -> Option<u32> {
                Some(1000)
            }
            fn verifier(&self) -> Result<String, String> {
                Err(NO_VERIFIER.to_owned())
            }
            fn ask_secret(&mut self, _: &str) -> Option<String> {
                None
            }
            fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
                Err(String::new())
            }
            fn install_verifier(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn sudo_list(&mut self) -> Result<String, String> {
                Ok("User dev may run the following commands on h:\n    (svc) NOPASSWD: ALL\n    (root) NOPASSWD: /usr/bin/id\n    (www-data) PASSWD: ALL\n".to_owned())
            }
            fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
                self.asked.push(user.to_owned());
                Ok(format!(
                    "User {user} may run the following commands on h:\n    (ALL : ALL) ALL\n"
                ))
            }
        }
        let mut sys = Sudo { asked: Vec::new() };
        // Without the root step only the account's own text is read.
        let kept = read_sudo_answer(&mut sys, "dev", false);
        assert!(kept.as_ref().is_ok_and(|k| k.others.is_empty()));
        assert_eq!(sys.asked.len(), 0);
        // With it, `sudo -l -U` is read for each other plain user a free rule runs as: here one.
        let kept = read_sudo_answer(&mut sys, "dev", true);
        assert_eq!(sys.asked, ["svc"]);
        let Ok(kept) = kept else { return };
        assert_eq!(kept.others.keys().collect::<Vec<_>>(), ["svc"]);
        assert!(kept.read_at > 1_700_000_000);
        let verdict = kept.verdict("dev", &|_| {
            Some(sudoers::PathFacts {
                file: Some(sudoers::Entry {
                    owner: 0,
                    mode: 0o755,
                }),
                above: vec![sudoers::Entry {
                    owner: 0,
                    mode: 0o755,
                }],
                names: Vec::new(),
            })
        });
        assert!(!verdict.wide(), "{:?}", verdict.wide_lines());
        // A user whose rules the root step could not read has no text kept, and is taken as wide.
        let mut broken = kept.clone();
        broken.others.clear();
        assert!(
            broken
                .verdict("dev", &|_| Some(sudoers::PathFacts::default()))
                .wide()
        );
    }

    // §17.8 (§17::98, §17::99; Fable M8): the probe clears what the program remembers (`-k`), runs
    // `-n true`, and where that is refused for want of a password also tries a command as the user
    // `lunchroom`, as the group `lunchroom` and as root with that group, in that order; the rule is
    // in force only when each is refused for want of a password (or the user or group is not made
    // yet). Every call has the C locale in `LC_ALL`, `LANGUAGE` and `LANG`; a refusal in another
    // language, or in words the probe does not know, is not proof of a password asked for.
    #[cfg(feature = "console")]
    #[test]
    #[allow(clippy::too_many_lines)] // one table of cases
    fn the_probe_clears_then_tries_and_reads_the_refusal() {
        use std::os::unix::fs::PermissionsExt as _;
        let pw = "echo 'sudo: a password is required' >&2; exit 1";
        let no_user = "echo 'sudo: unknown user lunchroom' >&2; exit 1";
        let no_group = "echo 'sudo: unknown group lunchroom' >&2; exit 1";
        let german = "echo 'sudo: ein Passwort ist erforderlich' >&2; exit 1";
        let boom = "echo 'sudo: boom' >&2; exit 1";
        let dir = scratch("probe");
        let log = dir.join("calls");
        let k = "-k\n-n true\n";
        let u = "-n -u lunchroom true\n";
        let g = "-n -g lunchroom true\n";
        let ug = "-n -u root -g lunchroom true\n";
        // name, bodies for root / -u lunchroom / -g lunchroom / -u root -g lunchroom, result, calls
        let cases: Vec<(&str, [&str; 4], SudoProbe, String)> = vec![
            (
                "ask",
                [pw; 4],
                SudoProbe::PasswordRequired,
                format!("{k}{u}{g}{ug}"),
            ),
            ("run", ["exit 0", pw, pw, pw], SudoProbe::Ran, k.to_owned()),
            (
                "as_user",
                [pw, "exit 0", pw, pw],
                SudoProbe::RunsAsLunchroom,
                format!("{k}{u}"),
            ),
            (
                "as_group",
                [pw, pw, "exit 0", pw],
                SudoProbe::RunsAsLunchroom,
                format!("{k}{u}{g}"),
            ),
            (
                "as_root_group",
                [pw, pw, pw, "exit 0"],
                SudoProbe::RunsAsLunchroom,
                format!("{k}{u}{g}{ug}"),
            ),
            (
                "not_made_yet",
                [pw, no_user, no_group, no_group],
                SudoProbe::PasswordRequired,
                format!("{k}{u}{g}{ug}"),
            ),
            (
                "other_root",
                ["echo 'sudo: unknown user' >&2; exit 1", pw, pw, pw],
                SudoProbe::Other("sudo: unknown user".to_owned()),
                k.to_owned(),
            ),
            (
                "other_user",
                [pw, boom, pw, pw],
                SudoProbe::Other("sudo: boom".to_owned()),
                format!("{k}{u}"),
            ),
            (
                "other_group",
                [pw, pw, boom, pw],
                SudoProbe::Other("sudo: boom".to_owned()),
                format!("{k}{u}{g}"),
            ),
            (
                "other_root_group",
                [pw, pw, pw, boom],
                SudoProbe::Other("sudo: boom".to_owned()),
                format!("{k}{u}{g}{ug}"),
            ),
            // A German refusal is not recognised: not proven refused for want of a password.
            (
                "german_root",
                [german, pw, pw, pw],
                SudoProbe::Other("sudo: ein Passwort ist erforderlich".to_owned()),
                k.to_owned(),
            ),
            (
                "german_group",
                [pw, pw, german, pw],
                SudoProbe::Other("sudo: ein Passwort ist erforderlich".to_owned()),
                format!("{k}{u}{g}"),
            ),
        ];
        for (name, bodies, want, calls) in cases {
            let program = dir.join(name);
            let [root, user, group, both] = bodies;
            let script = format!(
                "#!/bin/sh\necho \"$*|$LC_ALL|$LANGUAGE|$LANG\" >> {}\n\
                 if [ \"$1\" = -k ]; then exit 0; fi\n\
                 case \"$*\" in\n\"-n -u lunchroom true\") {user} ;;\n\
                 \"-n -g lunchroom true\") {group} ;;\n\
                 \"-n -u root -g lunchroom true\") {both} ;;\n*) {root} ;;\nesac\n",
                log.display()
            );
            assert!(std::fs::write(&program, script).is_ok());
            assert!(
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).is_ok()
            );
            runnable(&program);
            let _ = std::fs::remove_file(&log);
            assert_eq!(when_written(|| probe_with(&program)), Ok(want), "{name}");
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            // Every call had the three variables at `C`.
            assert!(
                text.lines().all(|l| l.ends_with("|C|C|C")),
                "{name}: {text}"
            );
            let mut seen = String::new();
            for l in text.lines() {
                seen.push_str(l.trim_end_matches("|C|C|C"));
                seen.push('\n');
            }
            assert_eq!(seen, calls, "{name}");
        }
        assert!(probe_with(&dir.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A script that writes the environment it was started with to its standard output, as
    /// `NAME=value` entries ended by NUL, and nothing else: no shell is between it and the
    /// environment, so it adds no variable of its own. Its arguments are ignored.
    const ENV_DUMP: &str = "import os, sys\nsys.stdout.buffer.write(b'\\0'.join(('%s=%s' % (k, v)).encode() for k, v in os.environ.items()))\n";

    fn env_dump_program(dir: &Path, name: &str) -> PathBuf {
        let file = dir.join(name);
        // `-I`: Python reads none of the variables that change it, and starts with the environment
        // it was given.
        assert!(std::fs::write(&file, format!("#!/usr/bin/python3 -I\n{ENV_DUMP}")).is_ok());
        assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).is_ok());
        runnable(&file);
        file
    }

    /// The environment recorded in `file`, by name.
    fn env_recorded(file: &Path) -> std::collections::BTreeMap<String, String> {
        std::fs::read(file)
            .unwrap_or_default()
            .split(|b| *b == 0)
            .filter_map(|entry| {
                let text = String::from_utf8_lossy(entry).into_owned();
                text.split_once('=')
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
            })
            .collect()
    }

    // Review M6 and the Codex ruling on its test — every root program, and the confirmation step of
    // each, is started with exactly `PATH=/usr/bin:/bin`, `LC_ALL=C` and `TERM` as this process has it
    // (and no `TERM` where it has none): no other name and no other value. The recorder is a Python
    // script with no shell under it, so nothing but the environment it was given is written.
    #[test]
    fn root_programs_are_given_exactly_path_lc_all_and_term() -> std::io::Result<()> {
        let dir = scratch("root-env");
        let mut expected = std::collections::BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]);
        if let Some(term) = std::env::var_os("TERM") {
            expected.insert("TERM".to_owned(), term.to_string_lossy().into_owned());
        }
        for which in RootProgram::ORDER {
            let program = env_dump_program(&dir, which.name());
            let recorded = dir.join(format!("{}.env", which.name()));
            let out = || {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&recorded)
            };
            let root = (which, program.clone());
            let run = root_run_on(
                out()?,
                &root,
                "true",
                &["/usr/bin/true".to_owned()],
                None,
                false,
            );
            assert!(run.is_ok(), "{which:?}: {run:?}");
            assert_eq!(env_recorded(&recorded), expected, "{which:?}");
            // Its confirmation step too.
            let step = which.argv(&program, &["/usr/bin/true".to_owned()]);
            assert!(run_on_terminal(&out()?, &step).is_ok(), "{which:?}");
            assert_eq!(
                env_recorded(&recorded),
                expected,
                "the confirmation of {which:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // Fable H3 — `sudo -n -l` and `run0 --help` are run in the English locale the parsers read. The
    // stand-ins answer in German unless `LC_ALL`, `LANGUAGE` and `LANG` are all `C`.
    #[test]
    fn sudo_and_run0_are_run_in_the_english_locale() {
        let dir = scratch("locale");
        let english = "User alice may run the following commands on h:\\n    (ALL : ALL) ALL\\n";
        let guard = r#"if [ "$LC_ALL" = C ] && [ "$LANGUAGE" = C ] && [ "$LANG" = C ]; then"#;
        stand_ins(
            &dir,
            &[
                (
                    "sudo",
                    &format!(
                        "{guard} printf '{english}'; else printf 'Benutzer alice darf\\n'; fi"
                    ),
                ),
                (
                    "run0",
                    &format!(
                        "{guard} printf ' -K  revoke\\n -v  validate\\n'; else printf 'Hilfe\\n'; fi"
                    ),
                ),
            ],
        );
        let text = sudo_list_with(&dir.join("sudo"));
        assert_eq!(text.as_deref(), Ok(english.replace("\\n", "\n").as_str()));
        let help = run0_help(&dir.join("run0")).unwrap_or_default();
        assert!(run0_lists_k_and_v(&help), "{help:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Fable U8-2 — an install whose `sudo -n -l` gave text that is not a listing says so before the
    // confirmation, and keeps the answer.
    #[test]
    fn an_install_with_an_unreadable_sudo_listing_says_it_cannot_tell() {
        let mail = mail_with_harness();
        let mut sys = user_shell();
        sys.verifier = Err(NO_VERIFIER.to_owned());
        sys.sudo = Ok("Benutzer alice darf die folgenden Befehle ausf\u{fc}hren:\n".to_owned());
        assert_eq!(confirm_or_begin(&mail, &mut sys, "q\n"), Ok(()));
        assert_eq!(sys.kept.len(), 1);
        let question = &sys.root_questions[0];
        assert!(
            question.contains("was not a listing it can read"),
            "{question}"
        );
        assert!(question.contains("K1.10"), "{question}");
    }

    /// The stand-in `sudo` of the tests below: its own listing for `-n -l`; for `-l -U svc` an
    /// English listing of a wide rule where `LC_ALL` is `C`, and a German one where it is not.
    const LISTING_STAND_IN: &str = "case \"$*\" in\n\"-n -l\") printf 'User alice may run the following commands on h:\\n    (svc) NOPASSWD: ALL\\n';;\n\"-l -U svc\") if [ \"$LC_ALL\" = C ]; then printf 'User svc may run the following commands on h:\\n    (root) NOPASSWD: ALL\\n'; else printf 'Benutzer svc darf die folgenden Befehle auf h ausf\\303\\274hren:\\n    (root) NOPASSWD: ALL\\n'; fi;;\nesac";

    // Review H3 and the Codex ruling on its tests — `sudo -l -U <user>` is read in English through
    // each of the four root programs, by the same `read_sudo_answer` that the install, `lunchroom
    // doctor` and a session's first hook call: a stand-in `sudo` answers in German wherever
    // `LC_ALL` is not `C`, so a root step that loses the locale keeps a German text, and the kept
    // answer for the account is then read as wide whichever text it is.
    #[test]
    fn the_root_step_reads_other_users_in_english_through_each_program() -> std::io::Result<()> {
        struct Standins {
            inner: PathBuf,
            root: (RootProgram, PathBuf),
        }
        impl System for Standins {
            fn ancestors(&self) -> Vec<(Option<String>, String)> {
                Vec::new()
            }
            fn own_tty(&self) -> u64 {
                1
            }
            fn tty_of(&self, _: &str) -> Option<u64> {
                None
            }
            fn ask(&mut self, _: &str) -> Option<String> {
                None
            }
            fn uid(&self) -> Option<u32> {
                Some(1000)
            }
            fn verifier(&self) -> Result<String, String> {
                Err(NO_VERIFIER.to_owned())
            }
            fn ask_secret(&mut self, _: &str) -> Option<String> {
                None
            }
            fn salt(&mut self) -> Result<[u8; passphrase::SALT_BYTES], String> {
                Err(String::new())
            }
            fn install_verifier(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn sudo_list(&mut self) -> Result<String, String> {
                sudo_list_with(&self.inner)
            }
            fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
                let null = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/null")
                    .map_err(|e| e.to_string())?;
                root_run_on(
                    null,
                    &self.root,
                    "reading",
                    &list_user_command(&self.inner, user)?,
                    None,
                    true,
                )
            }
        }
        let dir = scratch("root-listing");
        stand_ins(
            &dir,
            &[
                ("listing", LISTING_STAND_IN),
                ("sudo", "exec \"$@\""),
                ("doas", "exec \"$@\""),
                ("run0", "exec \"$@\""),
                ("su", "exec /bin/sh -c \"$2\""),
            ],
        );
        let tidy = |_: &str| {
            Some(sudoers::PathFacts {
                file: Some(sudoers::Entry {
                    owner: 0,
                    mode: 0o755,
                }),
                above: vec![sudoers::Entry {
                    owner: 0,
                    mode: 0o755,
                }],
                names: Vec::new(),
            })
        };
        for which in RootProgram::ORDER {
            let mut sys = Standins {
                inner: dir.join("listing"),
                root: (which, dir.join(which.name())),
            };
            let kept = read_sudo_answer(&mut sys, "alice", true).map_err(std::io::Error::other)?;
            let text = kept.others.get("svc").cloned().unwrap_or_default();
            assert!(text.starts_with("User svc may run"), "{which:?}: {text:?}");
            let verdict = kept.verdict("alice", &tidy);
            assert!(verdict.wide(), "{which:?}");
            // The text of a system that did lose the locale: German, and still wide.
            let mut german = kept.clone();
            german.others.insert(
                "svc".to_owned(),
                "Benutzer svc darf die folgenden Befehle ausf\u{fc}hren:\n    (root) NOPASSWD: ALL\n".to_owned(),
            );
            assert!(german.verdict("alice", &tidy).wide(), "{which:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // Codex ruling, item 4 — where no `env` is in the fixed folders, a captured root step is not run
    // (it would read a translated text), says why, and does not crash; with one it is built with it.
    #[test]
    fn a_captured_root_step_without_env_is_refused_and_not_run() -> std::io::Result<()> {
        let missing = c_locale_words_in(&[]);
        assert!(
            missing
                .as_ref()
                .is_err_and(|e| e.contains("`env` was not found")),
            "{missing:?}"
        );
        let dir = scratch("no-env");
        stand_ins(&dir, &[("sudo", "exec \"$@\"")]);
        let folders = [dir.display().to_string()];
        let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
        let root = (RootProgram::Sudo, dir.join("sudo"));
        let null = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
        };
        let _ = std::fs::remove_file(dir.join("sudo.argv"));
        let refused = root_run_in(
            null()?,
            &root,
            "reading",
            &["/usr/bin/id".to_owned()],
            None,
            true,
            &Settings {
                env_folders: &folders,
                read: &ROOT_READ,
            },
        );
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("`env` was not found") && e.contains("nothing was run")),
            "{refused:?}"
        );
        assert!(!dir.join("sudo.argv").exists(), "the root program was run");
        // A step that is not read back does not need `env`.
        let plain = root_run_in(
            null()?,
            &root,
            "x",
            &["/usr/bin/true".to_owned()],
            None,
            false,
            &Settings {
                env_folders: &folders,
                read: &ROOT_READ,
            },
        );
        assert!(plain.is_ok(), "{plain:?}");
        // With an `env` in the folders it is the one used.
        std::fs::copy("/usr/bin/env", dir.join("env"))?;
        let words = c_locale_words_in(&folders);
        assert_eq!(
            words.as_ref().map(|w| w[0].clone()).ok(),
            Some(dir.join("env").display().to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // Codex ruling, item 5 — what is read of `sudo -n -l` has a size limit: output of exactly the
    // limit is read as it is; output over it, whether on standard output or standard error, and
    // output that never ends, is not read, ends the program and counts as a listing that could not be
    // read, within a time bound.
    #[test]
    fn the_output_of_sudo_is_read_up_to_a_limit_and_then_counts_as_unreadable()
    -> std::io::Result<()> {
        let dir = scratch("sudo-size");
        let limit = sudoers::MAX_LISTING_BYTES;
        let head =
            "User alice may run the following commands on h:\n    (root) NOPASSWD: /usr/bin/id ";
        let mut listing = String::from(head);
        listing.push_str(&"a".repeat(limit - head.len() - 1));
        listing.push('\n');
        assert_eq!(listing.len(), limit);
        let exact = dir.join("exact.txt");
        let over = dir.join("over.txt");
        std::fs::write(&exact, &listing)?;
        std::fs::write(&over, format!("{listing} "))?;
        let started = std::time::Instant::now();
        let cases = [
            ("exact", format!("cat '{}'", exact.display()), false),
            ("over", format!("cat '{}'", over.display()), true),
            ("endless-stdout", "exec yes".to_owned(), true),
            ("endless-stderr", "exec yes >&2".to_owned(), true),
        ];
        for (name, body, over_limit) in cases {
            stand_ins(&dir, &[(name, &body)]);
            let text = sudo_list_with(&dir.join(name)).map_err(std::io::Error::other)?;
            if over_limit {
                assert_eq!(text, sudoers::OVER_LIMIT_TEXT, "{name}");
                assert!(!sudoers::parse(&text).known(), "{name}");
            } else {
                assert_eq!(text.len(), limit, "{name}");
                let parsed = sudoers::parse(&text);
                assert!(parsed.known(), "{name}");
                assert_eq!(parsed.rules.len(), 1, "{name}");
            }
        }
        // The root step's captured output has the same limit.
        stand_ins(&dir, &[("flood", "exec \"$@\"")]);
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?;
        let root = (RootProgram::Sudo, dir.join("flood"));
        let text = root_run_on(
            null,
            &root,
            "flood",
            &["/usr/bin/yes".to_owned()],
            None,
            true,
        )
        .map_err(std::io::Error::other)?;
        assert_eq!(text, sudoers::OVER_LIMIT_TEXT);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "{:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Runs `f` on a thread and waits `secs` for its result; `None` where it did not come, which
    /// fails the test that asked instead of hanging it.
    fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(f());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(secs))
            .ok()
    }

    /// Whether the process whose id `pidfile` holds is still there.
    fn pid_alive(pidfile: &Path) -> bool {
        let pid = std::fs::read_to_string(pidfile).unwrap_or_default();
        !pid.trim().is_empty() && Path::new(&format!("/proc/{}", pid.trim())).exists()
    }

    // Codex rulings 1 and 4 — a hostile child cannot hold the read up, or leave a result that
    // depends on a race. With a limit of 1000 bytes and 2 seconds: a child that prints exactly the
    // limit and then stays silent, one that says nothing and never exits, and one that closes both
    // streams and stays, are each killed at the time bound and counted as not read (TIMED_OUT);
    // one whose standard error floods while its standard output is closed, one whose standard
    // output floods while its standard error is closed, and one whose two streams both flood (both
    // readers see the overflow at once), give the over-limit text every time in repeated runs.
    // A child that was killed is gone.
    #[test]
    fn a_hostile_child_is_killed_at_a_time_bound_and_counts_as_unreadable() -> std::io::Result<()> {
        let dir = scratch("hostile");
        let pidfile = dir.join("pid");
        let record = format!("printf '%s' $$ > '{}'; ", pidfile.display());
        let limit = ReadLimit {
            bytes: 1000,
            within: std::time::Duration::from_secs(2),
        };
        let timed_out = [
            ("exact-then-silent", "printf '%01000d' 0; exec sleep 30"),
            ("silent", "exec sleep 30"),
            ("closed-streams", "exec >&- 2>&-; exec sleep 30"),
        ];
        for (name, body) in timed_out {
            stand_ins(&dir, &[(name, &format!("{record}{body}"))]);
            let program = dir.join(name);
            let started = std::time::Instant::now();
            let text = within(20, move || sudo_list_within(&program, &limit));
            assert_eq!(text, Some(Ok(sudoers::TIMED_OUT_TEXT.to_owned())), "{name}");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(15),
                "{name}"
            );
            assert!(!pid_alive(&pidfile), "{name}: the child was left running");
            assert!(!sudoers::parse(sudoers::TIMED_OUT_TEXT).known());
        }
        let over = [
            ("stderr-over-stdout-closed", "exec 1>&-; exec yes >&2"),
            ("stdout-over-stderr-closed", "exec 2>&-; exec yes"),
            ("both-over", "yes >&2 & exec yes"),
        ];
        for (name, body) in over {
            stand_ins(&dir, &[(name, body)]);
            for round in 0..15 {
                let program = dir.join(name);
                let text = within(20, move || sudo_list_within(&program, &limit));
                assert_eq!(
                    text,
                    Some(Ok(sudoers::OVER_LIMIT_TEXT.to_owned())),
                    "{name} round {round}"
                );
            }
        }
        // A child that ends in time with its output in the limit is read as it is.
        stand_ins(
            &dir,
            &[(
                "fine",
                "printf 'sudo: a password is required\\n' >&2; exit 1",
            )],
        );
        let fine = sudo_list_within(&dir.join("fine"), &limit);
        assert_eq!(fine, Ok("sudo: a password is required\n".to_owned()));
        // The captured root step has the same bounds.
        let null = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
        };
        let capture = |name: &str, expect: &str| -> std::io::Result<()> {
            let root = (RootProgram::Sudo, dir.join(name));
            let tty = null()?;
            let folders = SYSTEM_FOLDERS;
            let run = within(20, move || {
                root_run_in(
                    tty,
                    &root,
                    "reading",
                    &["/usr/bin/true".to_owned()],
                    None,
                    true,
                    &Settings {
                        env_folders: &folders,
                        read: &limit,
                    },
                )
            });
            assert_eq!(run, Some(Ok(expect.to_owned())), "captured {name}");
            Ok(())
        };
        for (name, _) in timed_out {
            capture(name, sudoers::TIMED_OUT_TEXT)?;
        }
        capture("stdout-over-stderr-closed", sudoers::OVER_LIMIT_TEXT)?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // Codex ruling 2 — what the program a captured root step runs is given: the root program has
    // exactly `PATH`, `LC_ALL` and `TERM`, and so does the program it runs once `env` has put the
    // locale on it: `LC_ALL` alone, with no `LANG` and no `LANGUAGE` (it overrides both). The root
    // program here is a Python script that runs its first argument and adds nothing; the program
    // it runs records its environment.
    #[test]
    fn the_program_a_captured_root_step_runs_is_given_only_path_lc_all_and_term()
    -> std::io::Result<()> {
        let dir = scratch("captured-env");
        let runner = "#!/usr/bin/python3 -I\nimport os, sys\nos.execv(sys.argv[1], sys.argv[1:])\n";
        let dump = env_dump_program(&dir, "dump");
        let mut expected = std::collections::BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]);
        if let Some(term) = std::env::var_os("TERM") {
            expected.insert("TERM".to_owned(), term.to_string_lossy().into_owned());
        }
        for which in [RootProgram::Sudo, RootProgram::Doas, RootProgram::Run0] {
            let program = dir.join(which.name());
            std::fs::write(&program, runner)?;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
            runnable(&program);
            let null = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")?;
            let text = root_run_on(
                null,
                &(which, program),
                "reading",
                &[dump.display().to_string()],
                None,
                true,
            )
            .map_err(std::io::Error::other)?;
            let seen: std::collections::BTreeMap<String, String> = text
                .split('\0')
                .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
                .collect();
            assert_eq!(seen, expected, "{which:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Whether the process whose id `pidfile` holds is gone, waiting up to five seconds for it.
    fn pid_gone(pidfile: &Path) -> bool {
        for _ in 0..100 {
            if !pid_alive(pidfile) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    // Codex round 4, item 1 — the byte limit is one limit for standard output and standard error
    // together. With a limit of 1000: 500 and 500, 1000 and 0, and 0 and 1000 are read; 600 and 600,
    // 1000 and 1, and 0 and 1001 are over.
    #[test]
    fn the_byte_limit_holds_for_both_streams_together() {
        let dir = scratch("combined");
        let limit = ReadLimit {
            bytes: 1000,
            within: std::time::Duration::from_secs(10),
        };
        let cases = [
            (500, 500, false),
            (1000, 0, false),
            (0, 1000, false),
            (600, 600, true),
            (1000, 1, true),
            (0, 1001, true),
            (501, 500, true),
        ];
        for (i, (out, err, over)) in cases.into_iter().enumerate() {
            let name = format!("case{i}");
            let body = format!("printf '%0{out}d' 0; printf '%0{err}d' 0 >&2");
            // `%00d` prints one digit: a count of 0 prints nothing.
            let body = body
                .replace("printf '%00d' 0; ", "")
                .replace("; printf '%00d' 0 >&2", "");
            stand_ins(&dir, &[(&name, &body)]);
            let program = dir.join(&name);
            let text = within(20, move || sudo_list_within(&program, &limit));
            if over {
                assert_eq!(
                    text,
                    Some(Ok(sudoers::OVER_LIMIT_TEXT.to_owned())),
                    "{out}+{err}"
                );
            } else {
                let read = text.and_then(Result::ok).unwrap_or_default();
                assert_eq!(read.len(), out + err, "{out}+{err}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex round 4, item 4 — a descendant that holds a pipe is gone with the child: for `sudo -n -l`
    // (the child leads a process group, which is killed) and for a captured root step (the child and
    // everything below it in /proc). The stand-in starts a `sleep` in the background, writes its
    // process id, and stays; after the read has given up, that process is gone.
    #[test]
    fn a_descendant_holding_a_pipe_is_killed_with_the_child() -> std::io::Result<()> {
        let dir = scratch("descendant");
        let grand = dir.join("grand");
        let record = format!("sleep 60 & printf '%s' $! > '{}'; ", grand.display());
        let limit = ReadLimit {
            bytes: 1000,
            within: std::time::Duration::from_secs(2),
        };
        // sudo -n -l: the child stays silent, with the descendant on its pipes.
        stand_ins(&dir, &[("listing", &format!("{record}exec sleep 60"))]);
        let program = dir.join("listing");
        let text = within(20, move || sudo_list_within(&program, &limit));
        assert_eq!(text, Some(Ok(sudoers::TIMED_OUT_TEXT.to_owned())));
        assert!(
            pid_gone(&grand),
            "the descendant of sudo -n -l was left running"
        );
        // sudo -n -l: over the limit with a flooding descendant.
        let _ = std::fs::remove_file(&grand);
        let flood = format!("yes >&2 & printf '%s' $! > '{}'; exec yes", grand.display());
        stand_ins(&dir, &[("flood", &flood)]);
        let program = dir.join("flood");
        let text = within(20, move || sudo_list_within(&program, &limit));
        assert_eq!(text, Some(Ok(sudoers::OVER_LIMIT_TEXT.to_owned())));
        assert!(pid_gone(&grand), "the flooding descendant was left running");
        // A captured root step: the root program has the descendant.
        let _ = std::fs::remove_file(&grand);
        stand_ins(&dir, &[("sudo", &format!("{record}exec sleep 60"))]);
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?;
        let root = (RootProgram::Sudo, dir.join("sudo"));
        let run = within(20, move || {
            root_run_in(
                null,
                &root,
                "reading",
                &["/usr/bin/true".to_owned()],
                None,
                true,
                &Settings {
                    env_folders: &SYSTEM_FOLDERS,
                    read: &limit,
                },
            )
        });
        assert_eq!(run, Some(Ok(sudoers::TIMED_OUT_TEXT.to_owned())));
        assert!(
            pid_gone(&grand),
            "the descendant of the root step was left running"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    // Codex round 4, item 5 — the command that reads another user's rules is not built for a name
    // that is empty, has white space, begins with `-`, holds shell characters, quotes, a backslash
    // or a newline; for a login name and a number it is `sudo -l -U <name>`.
    #[test]
    fn the_command_for_another_users_rules_is_built_for_valid_names_only() {
        let sudo = Path::new("/usr/bin/sudo");
        for good in ["svc", "www-data", "#1000"] {
            assert_eq!(
                list_user_command(sudo, good),
                Ok(vec![
                    "/usr/bin/sudo".to_owned(),
                    "-l".to_owned(),
                    "-U".to_owned(),
                    good.to_owned()
                ])
            );
        }
        for bad in [
            "", " ", "a b", "-x", "--help", "a;b", "$(id)", "`id`", "a'b", "a\"b", "a\\b", "a\nb",
            "a|b", "a&b", "x\0y",
        ] {
            let built = list_user_command(sudo, bad);
            assert!(
                built.as_ref().is_err_and(|e| e.contains("not a user name")),
                "{bad:?}: {built:?}"
            );
        }
    }

    // Codex round 4, item 6 — a translated refusal on standard error is read in English too: the
    // stand-in prints its refusal on standard error only, German unless `LC_ALL` is `C`.
    #[test]
    fn a_refusal_on_standard_error_is_read_in_english() {
        let dir = scratch("stderr-locale");
        let guard = r#"if [ "$LC_ALL" = C ] && [ "$LANGUAGE" = C ] && [ "$LANG" = C ]; then"#;
        stand_ins(
            &dir,
            &[(
                "sudo",
                &format!(
                    "{guard} printf 'sudo: a password is required\\n' >&2; else printf 'sudo: ein Passwort ist erforderlich\\n' >&2; fi; exit 1"
                ),
            )],
        );
        let text = sudo_list_with(&dir.join("sudo"));
        assert_eq!(text.as_deref(), Ok("sudo: a password is required\n"));
        assert_eq!(
            sudoers::parse(text.as_deref().unwrap_or_default()).status,
            sudoers::Status::PasswordRequired
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex round 4, item 7 — the probe of the console install (`sudo -k`, then `sudo -n true`) runs
    // in the English locale: the stand-in refuses in German unless `LC_ALL`, `LANGUAGE` and `LANG`
    // are all `C`, and the probe reads the refusal as a password being required.
    #[cfg(feature = "console")]
    #[test]
    fn the_probe_is_run_in_the_english_locale() {
        let dir = scratch("probe-locale");
        let guard = r#"if [ "$LC_ALL" = C ] && [ "$LANGUAGE" = C ] && [ "$LANG" = C ]; then"#;
        stand_ins(
            &dir,
            &[(
                "sudo",
                &format!(
                    "if [ \"$1\" = -k ]; then exit 0; fi; {guard} printf 'sudo: a password is required\\n' >&2; else printf 'sudo: ein Passwort ist erforderlich\\n' >&2; fi; exit 1"
                ),
            )],
        );
        assert_eq!(
            when_written(|| probe_with(&dir.join("sudo"))),
            Ok(SudoProbe::PasswordRequired)
        );
        // A probe whose output never ends is not waited for.
        stand_ins(
            &dir,
            &[(
                "silent",
                "if [ \"$1\" = -k ]; then exit 0; fi; exec sleep 60",
            )],
        );
        let limit = ReadLimit {
            bytes: 1000,
            within: std::time::Duration::from_secs(2),
        };
        let program = dir.join("silent");
        let probe = within(20, move || probe_within(&program, &limit));
        assert!(matches!(probe, Some(Ok(SudoProbe::Other(_)))), "{probe:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex ruling (M8): the probe reads the C-locale forms exactly: only the whole line `sudo: a
    // password is required` is a password asked for; the unknown user or group line, for the user
    // or group asked, is "not made yet"; any other spelling is not proof, whatever it resembles.
    #[cfg(feature = "console")]
    #[test]
    fn only_the_exact_c_locale_refusals_are_read_as_a_password_asked() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("probe-exact");
        let run = |name: &str, root: &str, user: &str| {
            let program = dir.join(name);
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = -k ]; then exit 0; fi\n\
                 case \"$*\" in\n\"-n -u lunchroom true\") {user} ;;\n\
                 \"-n true\") {root} ;;\n*) echo 'sudo: a password is required' >&2; exit 1 ;;\nesac\n"
            );
            assert!(std::fs::write(&program, script).is_ok());
            assert!(
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).is_ok()
            );
            when_written(|| probe_with(&program))
        };
        let pw = "echo 'sudo: a password is required' >&2; exit 1";
        for (name, spelling) in [
            ("no_prefix", "echo 'a password is required' >&2; exit 1"),
            ("colon", "echo 'sudo: a password is required:' >&2; exit 1"),
            ("upper", "echo 'Sudo: a password is required' >&2; exit 1"),
            (
                "extra",
                "printf 'sudo: a password is required\\nsudo: and more\\n' >&2; exit 1",
            ),
            ("plain", "echo 'password required' >&2; exit 1"),
            ("empty", "exit 1"),
        ] {
            let got = run(name, spelling, pw);
            assert!(matches!(got, Ok(SudoProbe::Other(_))), "{name}: {got:?}");
        }
        // The exact line passes, with a trailing newline or none.
        assert_eq!(run("exact", pw, pw), Ok(SudoProbe::PasswordRequired));
        // The unknown-user line, with the audit line sudo adds after it, is "not made yet".
        let not_made = "printf 'sudo: unknown user lunchroom\\nsudo: error initializing audit plugin\\n' >&2; exit 1";
        assert_eq!(run("made", pw, not_made), Ok(SudoProbe::PasswordRequired));
        // Another name, or the group's wording for the user probe, is not it.
        for (name, text) in [
            (
                "other_name",
                "echo 'sudo: unknown user lunchroomx' >&2; exit 1",
            ),
            (
                "wrong_kind",
                "echo 'sudo: unknown group lunchroom' >&2; exit 1",
            ),
        ] {
            let got = run(name, pw, text);
            assert!(matches!(got, Ok(SudoProbe::Other(_))), "{name}: {got:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex ruling (M8): a probe's output (standard output and error together) is read up to a cap
    // and no further, the probe is stopped when it goes over, and a probe that does not finish is
    // stopped; both give a refusal that is not proof (wide).
    #[cfg(feature = "console")]
    #[test]
    fn a_probe_that_floods_or_hangs_is_cut_off() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("probe-limits");
        let make = |name: &str, body: &str| {
            let program = dir.join(name);
            let script = format!("#!/bin/sh\nif [ \"$1\" = -k ]; then exit 0; fi\n{body}\n");
            assert!(std::fs::write(&program, script).is_ok());
            assert!(
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).is_ok()
            );
            program
        };
        let started = std::time::Instant::now();
        let flood = make(
            "flood",
            "head -c 20000000 /dev/zero | tr '\\0' x >&2; exit 1",
        );
        let cap = ReadLimit {
            bytes: 64,
            within: PROBE_WAIT,
        };
        let got = when_written(|| probe_within(&flood, &cap));
        assert!(
            matches!(&got, Ok(SudoProbe::Other(text)) if text.contains("longer than 64 bytes")),
            "{got:?}"
        );
        let hang = make("hang", "exec sleep 30");
        let short = ReadLimit {
            bytes: MAX_PROBE_BYTES,
            within: std::time::Duration::from_millis(700),
        };
        let got = when_written(|| probe_within(&hang, &short));
        assert!(
            matches!(&got, Ok(SudoProbe::Other(text)) if text.contains("did not finish")),
            "{got:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex ruling 3 (H4): an image read that blocks is given up after the wait, not waited for for
    // ever: the install fails closed. (The stand-in blocks a few seconds, then reports an empty
    // read, so a read that is not bounded returns a different answer, not a hang.)
    #[cfg(feature = "console")]
    #[test]
    fn a_blocking_image_read_is_given_up() {
        struct Blocks;
        impl std::io::Read for Blocks {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(std::time::Duration::from_secs(4));
                Ok(0)
            }
        }
        let started = std::time::Instant::now();
        let got = read_capped_within(Blocks, 1000, std::time::Duration::from_millis(300));
        assert!(got.is_err_and(|e| e.contains("did not finish within")));
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        // A reader that answers in time is read.
        let got = read_capped_within(std::io::Cursor::new(vec![1u8; 10]), 1000, IMAGE_WAIT);
        assert_eq!(got.map(|v| v.len()), Ok(10));
    }

    // Codex ruling 3 (H4): a failure of the call that makes the process not dumpable is an error
    // (fail closed); a success is not.
    #[cfg(feature = "console")]
    #[test]
    fn a_failed_memory_protection_is_an_error() {
        let failed = make_undumpable(|| Err(rustix::io::Errno::PERM));
        assert!(failed.is_err_and(|e| e.contains("could not be made unreadable")));
        assert!(make_undumpable(|| Ok(())).is_ok());
    }

    // Codex ruling 3 (M8): a probe that leaves a descendant holding its standard error open does
    // not hold the probe up: the probe's whole process group is stopped, and the text is waited for
    // only a moment. The refusal it printed is still read.
    #[cfg(feature = "console")]
    #[test]
    fn a_probe_whose_child_holds_the_pipe_open_does_not_hang_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("probe-child");
        let program = dir.join("forks");
        let script = "#!/bin/sh\nif [ \"$1\" = -k ]; then exit 0; fi\n\
                      sleep 6 &\necho 'sudo: a password is required' >&2\nexit 1\n";
        assert!(std::fs::write(&program, script).is_ok());
        assert!(std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).is_ok());
        let started = std::time::Instant::now();
        let got = when_written(|| probe_with(&program));
        assert_eq!(got, Ok(SudoProbe::PasswordRequired));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex ruling (H4): the image read stops at its cap, a failed capture is not kept (a retry
    // makes it again), and a good one is kept.
    #[cfg(feature = "console")]
    #[test]
    #[allow(clippy::items_after_statements)] // the endless reader sits beside its use
    fn the_image_read_is_capped_and_a_failed_capture_is_not_kept() {
        let dir = scratch("image-cap");
        let file = dir.join("program");
        assert!(std::fs::write(&file, vec![7u8; 100]).is_ok());
        assert_eq!(read_image(&file, 100).map(|v| v.len()), Ok(100));
        assert!(read_image(&file, 99).is_err_and(|e| e.contains("larger than 99 bytes")));
        assert!(read_image(&dir.join("absent"), 100).is_err());
        // An endless source is cut at the cap: no more than the cap and one byte is asked for (a
        // reader that is asked for more fails the read, so an uncapped read is not taken for one
        // that stopped).
        struct Flood(u64);
        impl std::io::Read for Flood {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.0 += buf.len() as u64;
                if self.0 > 1000 + 64 * 1024 {
                    return Err(std::io::Error::other("read past the cap"));
                }
                buf.fill(0);
                Ok(buf.len())
            }
        }
        let flood = read_capped(Flood(0), 1000);
        assert!(flood.is_err_and(|e| e.contains("larger than 1000 bytes")));
        let cache = std::sync::OnceLock::new();
        // Too large: an error, and nothing kept.
        assert!(capture_into(&cache, &file, 50).is_err());
        assert!(cache.get().is_none());
        assert!(capture_into(&cache, &dir.join("absent"), 1000).is_err());
        assert!(cache.get().is_none());
        // The retry makes the copy; it is then kept and reused.
        let first = capture_into(&cache, &file, 1000);
        assert!(first.is_ok_and(|s| s.sha256.is_some()));
        assert!(cache.get().is_some());
        let again = capture_into(&cache, &dir.join("absent"), 1);
        assert!(again.is_ok(), "the kept copy is reused: {again:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Codex ruling (M12): a line read from the terminal is cut at the cap; a short line is whole.
    #[test]
    fn an_answer_is_read_up_to_a_cap() {
        let long = vec![b'a'; 1_000_000];
        let got = read_line_capped(std::io::Cursor::new(long), MAX_ANSWER_BYTES);
        assert_eq!(got.map(|s| s.len()), Some(4096));
        let got = read_line_capped(
            std::io::Cursor::new(b"yes\nmore\n".to_vec()),
            MAX_ANSWER_BYTES,
        );
        assert_eq!(got.as_deref(), Some("yes\n"));
        let got = read_line_capped(std::io::Cursor::new(Vec::new()), MAX_ANSWER_BYTES);
        assert_eq!(got.as_deref(), Some(""));
    }

    // §17.2 (Fable H4): the copy of the program the root step reads is made from the running
    // image, named by this process's own file descriptor (not a path the user's processes can
    // replace), holds exactly the image, hashes to the sha-256 given, and cannot be changed.
    #[cfg(feature = "console")]
    #[test]
    fn the_image_copy_is_sealed_and_is_the_running_program() {
        let source = capture_image();
        assert!(source.is_ok(), "{source:?}");
        let Ok(source) = source else { return };
        let path = source.path.display().to_string();
        assert!(
            path.starts_with(&format!("/proc/{}/fd/", std::process::id())),
            "{path}"
        );
        let image = std::fs::read("/proc/self/exe").unwrap_or_default();
        assert_ne!(image.len(), 0);
        assert_eq!(
            std::fs::read(&source.path).ok().as_deref(),
            Some(image.as_slice())
        );
        assert_eq!(
            source.sha256.as_deref(),
            Some(lunchroom_core::jcs::sha256_hex(&image).as_str())
        );
        assert!(!source.is_installed);
        // Sealed: it cannot be opened for writing, whoever asks.
        let opened = std::fs::OpenOptions::new().write(true).open(&source.path);
        assert!(opened.is_err() || opened.is_ok_and(|mut f| f.write_all(b"x").is_err()));
        assert_eq!(
            std::fs::read(&source.path).ok().as_deref(),
            Some(image.as_slice())
        );
        // The same copy every time.
        assert_eq!(capture_image().ok(), Some(source));
    }

    // §17.2, 2026-11-14 console proof: root in a container could not read the image by its
    // `/proc/<pid>/fd` path (`install: cannot stat '/proc/57/fd/3': Permission denied`), because
    // this process is not dumpable. The root command gets the image as its standard input: the word
    // naming the image becomes `/dev/stdin`, and the stream is the whole image from its first byte,
    // a second time too. A command that does not name the image is left alone.
    #[cfg(feature = "console")]
    #[test]
    fn the_root_command_reads_the_image_from_standard_input_not_by_its_proc_path() {
        use std::io::Read as _;
        let dir = std::env::temp_dir().join(format!("lr-image-stdin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("program");
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        assert!(std::fs::write(&file, &bytes).is_ok());
        let cache = std::sync::OnceLock::new();
        let source = capture_into(&cache, &file, 1_000_000);
        assert!(source.is_ok(), "{source:?}");
        let Ok(source) = source else { return };
        let path = source.path.display().to_string();
        for _ in 0..2 {
            let mut words = vec![
                "/usr/bin/install".to_owned(),
                "-m".to_owned(),
                "2755".to_owned(),
                path.clone(),
                "/usr/local/lib/lunchroom/lunchroom".to_owned(),
            ];
            let image = image_as_stdin(&cache, &mut words);
            assert_eq!(words[3], "/dev/stdin", "{words:?}");
            assert!(!words.iter().any(|w| w.starts_with("/proc/")), "{words:?}");
            let mut read = Vec::new();
            assert!(image.is_some_and(|mut f| f.read_to_end(&mut read).is_ok()));
            assert_eq!(read, bytes);
        }
        let mut other = vec![
            "/usr/bin/chmod".to_owned(),
            "0640".to_owned(),
            "/x".to_owned(),
        ];
        assert!(image_as_stdin(&cache, &mut other).is_none());
        assert_eq!(other, ["/usr/bin/chmod", "0640", "/x"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cache holding an image of `bytes`, and the `/proc` path that names it.
    #[cfg(feature = "console")]
    fn cached_image(tag: &str, bytes: &[u8]) -> Option<(std::sync::OnceLock<Image>, String)> {
        let dir = std::env::temp_dir().join(format!("lr-pass-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let file = dir.join("program");
        std::fs::write(&file, bytes).ok()?;
        let cache = std::sync::OnceLock::new();
        let source = capture_into(&cache, &file, 1_000_000).ok()?;
        let _ = std::fs::remove_dir_all(&dir);
        Some((cache, source.path.display().to_string()))
    }

    // F2 design, "Store wall", class 1 (a `/proc/<pid>/fd` path in a root step): through `su` the
    // word naming the image is the path of a file that holds exactly the image, in a new folder of
    // mode 0700; the word is never a `/proc/` path; file and folder are gone when the pass is
    // dropped. Fails before the change: `pass_image` and `ImagePass` do not exist, and `su` kept
    // the `/proc/<pid>/fd` word.
    #[cfg(feature = "console")]
    #[test]
    fn through_su_the_image_is_a_file_in_a_private_folder_never_a_proc_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i % 253) as u8).collect();
        let made = cached_image("su", &bytes);
        assert!(made.is_some());
        let Some((cache, path)) = made else { return };
        let parent = std::env::temp_dir().join(format!("lr-pass-parent-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&parent).is_ok());
        let mut words = vec![
            "/usr/bin/install".to_owned(),
            "-m".to_owned(),
            "2755".to_owned(),
            path,
            "/usr/local/lib/lunchroom/lunchroom".to_owned(),
        ];
        let passed = pass_image(&cache, RootProgram::Su, &mut words, &parent);
        assert!(matches!(passed, Ok(ImagePass::File(_))), "{passed:?}");
        let Ok(ImagePass::File(kept)) = passed else {
            return;
        };
        assert_eq!(words[3], kept.path.display().to_string(), "{words:?}");
        assert!(!words.iter().any(|w| w.contains("/proc/")), "{words:?}");
        assert!(kept.path.starts_with(&parent), "{words:?}");
        assert_eq!(std::fs::read(&kept.path).ok().as_deref(), Some(&bytes[..]));
        let folder = kept
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let folder_mode = std::fs::metadata(&folder).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(folder_mode.ok(), Some(0o700), "{folder:?}");
        drop(kept);
        assert!(!folder.exists(), "the folder was kept");
        // A command that does not name the image is left alone, and makes nothing.
        let mut other = vec!["/usr/bin/chmod".to_owned(), "0640".to_owned()];
        let passed = pass_image(&cache, RootProgram::Su, &mut other, &parent);
        assert!(matches!(passed, Ok(ImagePass::None)), "{passed:?}");
        assert_eq!(other, ["/usr/bin/chmod", "0640"]);
        assert_eq!(
            std::fs::read_dir(&parent).map(Iterator::count).ok(),
            Some(0),
            "a folder was left behind"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    // The same decision for the other root programs: the word is `/dev/stdin`, the stream is the
    // image, nothing is written to a file. Pins that the `su` change did not move them.
    #[cfg(feature = "console")]
    #[test]
    fn through_sudo_doas_and_run0_the_image_is_standard_input() {
        use std::io::Read as _;
        let bytes: Vec<u8> = (0..300u32).map(|i| (i % 7) as u8).collect();
        let made = cached_image("others", &bytes);
        assert!(made.is_some());
        let Some((cache, path)) = made else { return };
        let parent = std::env::temp_dir().join(format!("lr-pass-none-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&parent).is_ok());
        for which in [RootProgram::Sudo, RootProgram::Doas, RootProgram::Run0] {
            let mut words = vec!["/usr/bin/install".to_owned(), path.clone(), "/x".to_owned()];
            let passed = pass_image(&cache, which, &mut words, &parent);
            assert!(matches!(passed, Ok(ImagePass::Stdin(_))), "{which:?}");
            assert_eq!(words[1], "/dev/stdin", "{which:?}");
            let mut read = Vec::new();
            if let Ok(ImagePass::Stdin(mut f)) = passed {
                assert!(f.read_to_end(&mut read).is_ok());
            }
            assert_eq!(read, bytes, "{which:?}");
        }
        assert_eq!(
            std::fs::read_dir(&parent).map(Iterator::count).ok(),
            Some(0)
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    // F2 design, "Store wall" (the console is set up): missing is the file system's NotFound;
    // anything else it answers, including a refused look, is not missing. Fails before the change:
    // `System::missing` does not exist. (A refused look needs a folder the test cannot enter, so it
    // is checked only where the test does not run as root.)
    #[cfg(feature = "console")]
    #[test]
    fn missing_is_not_found_and_nothing_else() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("lr-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(dir.join("locked")).is_ok());
        assert!(std::fs::write(dir.join("locked/rule"), "x").is_ok());
        assert!(std::fs::write(dir.join("there"), "x").is_ok());
        assert!(OsSystem.missing(&dir.join("absent")));
        assert!(!OsSystem.missing(&dir.join("there")));
        if !rustix::process::getuid().is_root() {
            assert!(
                std::fs::set_permissions(
                    dir.join("locked"),
                    std::fs::Permissions::from_mode(0o000)
                )
                .is_ok()
            );
            assert!(
                !OsSystem.missing(&dir.join("locked/rule")),
                "refused is not missing"
            );
            assert!(!OsSystem.missing(&dir.join("locked/absent")));
            let _ = std::fs::set_permissions(
                dir.join("locked"),
                std::fs::Permissions::from_mode(0o700),
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §17.9 (Fable M13): after `protect_memory` this process is not dumpable: the user's other
    // processes can neither `ptrace` it nor read its memory.
    #[cfg(feature = "console")]
    #[test]
    fn protect_memory_makes_the_process_not_dumpable() {
        // Another test may have made the process not dumpable already: start from dumpable.
        let reset =
            rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::Dumpable);
        assert!(reset.is_ok());
        assert_eq!(
            rustix::process::dumpable_behavior().ok(),
            Some(rustix::process::DumpableBehavior::Dumpable)
        );
        assert_eq!(OsSystem.protect_memory(), Ok(()));
        assert_eq!(
            rustix::process::dumpable_behavior().ok(),
            Some(rustix::process::DumpableBehavior::NotDumpable)
        );
    }
}
