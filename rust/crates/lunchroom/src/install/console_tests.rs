//! Tests of the store wall and console install (§17.2, §17.8, §17.9) on a stand-in for the root
//! step: it takes the commands the install runs as root and carries them out on a model of the
//! computer (owners, groups, modes, text), in order, so what is checked is the result and the
//! order, with no root and no change to this computer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use lunchroom_core::setgid;
use lunchroom_core::sudoers::Kept;

use super::console_keys;
use super::console_setup::{
    Accounts, Facts, KeyPlan, Keys, Limits, Listing, OldStoreLock, RootStep, Source, Sources,
    console_is_set_up, console_steps, copy_store_steps, copy_store_steps_from, install_console,
    install_with_wall, list_file, list_folder, measure_tree, move_store_with, pub_steps,
    required_tools, rule_steps, rule_text, steps_text, sudoers_text, uninstall_console,
    valid_account, wall_steps,
};
use super::console_state::{Kind, State};
use crate::home::Env;
use crate::terminal::{SudoProbe, System, fake};

const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\ndev:x:1000:1000:Dev:/home/dev:/bin/bash\n";
const GROUP: &str = "root:x:0:\ndev:x:1000:\n";
const NARROW: &str =
    "User dev may run the following commands on host:\n    (root) NOPASSWD: /usr/bin/id\n";
const NO_RULES: &str = "sudo: a password is required\n";
const WIDE: &str = "User dev may run the following commands on host:\n    (ALL) NOPASSWD: ALL\n";

fn tool(name: &str) -> String {
    format!("/usr/bin/{name}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    owner: String,
    group: String,
    mode: u32,
    text: Option<String>,
}

/// The stand-in for the root step and the computer around it.
#[allow(clippy::struct_excessive_bools)]
struct Sim {
    tree: BTreeMap<String, Node>,
    /// Every event, in order: each root command, and `stop-listeners`, `run-user`.
    log: Vec<String>,
    files: BTreeMap<String, String>,
    answers: Vec<Option<String>>,
    asked: Vec<String>,
    secret: Option<String>,
    ancestors: Vec<(Option<String>, String)>,
    tty: u64,
    harness_tty: u64,
    sudo_text: String,
    probe: Result<SudoProbe, String>,
    visudo_file_fails: bool,
    visudo_all_fails: bool,
    stop_error: Option<String>,
    user_install_ok: bool,
    /// The installed program's exit code, where a test sets one (6: hooks done, console not set
    /// up); otherwise 0 or 1 from `user_install_ok`.
    user_install_code: Option<i32>,
    seed: u8,
    kept: Vec<Kept>,
    wall: bool,
    /// What `-l -U <user>` prints, by user.
    others: BTreeMap<String, String>,
    /// The file `visudo -c -f` was given, as it was on disk at that moment.
    checked: Vec<Node>,
    /// The text given to each root command on its standard input.
    inputs: Vec<String>,
    /// `protect_memory` was called.
    protected: bool,
    /// `protect_memory` fails.
    protect_fails: bool,
    /// The verifier (none yet: `NO_VERIFIER`) and what the root program's own prompt answers.
    verifier: Result<String, String>,
    root_confirms: Result<(), String>,
    /// The questions the root program's prompt was shown.
    root_questions: Vec<String>,
    /// Replace the old store by a link to a decoy at the first step of the copy.
    swap_store_on_copy: bool,
    /// What `cp` read through its source argument (the store's `marker`).
    copied_marker: Option<String>,
    /// Grow the old store by this many bytes when `cp` runs.
    grow_on_cp: u64,
    /// Where the root step is told to read the program from, and what it hashes to.
    source: PathBuf,
    source_sha: String,
    /// The running program is the installed one already.
    running_is_installed: bool,
    /// The program that lands in place is not the one that was running.
    tamper: bool,
    /// Words: the first root command naming each fails once.
    fail_once: Vec<String>,
    /// Lunchroom's old folder, whose locks `cp` is checked against.
    lock_home: Option<PathBuf>,
    /// For each `cp`, whether the old store's lock was held by someone else.
    cp_locked: Vec<bool>,
    /// For each `env -C <folder> sha256sum -c` the stand-in carried out: the folder and how many
    /// lines of the list it was given (the copy is taken to hold exactly those files).
    listed: BTreeMap<String, u64>,
    /// Files the copy holds that the list lacks (an added file): what `find` reads back as more.
    extra_in_copy: u64,
    /// What the key script leaves in the file instead of a whole key (a short or cut read).
    key_text: Option<String>,
    /// Replace the old store by a symbolic link at the first listener stop.
    swap_store_on_stop: bool,
    /// Whether the old store was already moved aside when the public key was written.
    old_store_gone_at_pub: Option<bool>,
}

impl Sim {
    fn new() -> Self {
        let mut files = BTreeMap::new();
        files.insert("/etc/passwd".to_owned(), PASSWD.to_owned());
        files.insert("/etc/group".to_owned(), GROUP.to_owned());
        Self {
            tree: BTreeMap::new(),
            log: Vec::new(),
            files,
            answers: Vec::new(),
            asked: Vec::new(),
            secret: Some(fake::PASSPHRASE.to_owned()),
            ancestors: vec![(Some("100:5".to_owned()), "bash".to_owned())],
            tty: 34816,
            harness_tty: 34817,
            sudo_text: NO_RULES.to_owned(),
            probe: Ok(SudoProbe::PasswordRequired),
            visudo_file_fails: false,
            visudo_all_fails: false,
            stop_error: None,
            user_install_ok: true,
            user_install_code: None,
            seed: 1,
            kept: Vec::new(),
            wall: true,
            others: BTreeMap::new(),
            checked: Vec::new(),
            inputs: Vec::new(),
            protected: false,
            protect_fails: false,
            verifier: Ok(fake::verifier()),
            root_confirms: Ok(()),
            root_questions: Vec::new(),
            swap_store_on_copy: false,
            copied_marker: None,
            grow_on_cp: 0,
            source: PathBuf::from("/proc/4242/fd/9"),
            source_sha: "ab".repeat(32),
            running_is_installed: false,
            tamper: false,
            fail_once: Vec::new(),
            lock_home: None,
            cp_locked: Vec::new(),
            listed: BTreeMap::new(),
            extra_in_copy: 0,
            key_text: None,
            swap_store_on_stop: false,
            old_store_gone_at_pub: None,
        }
    }

    fn with_sudo(mut self, text: &str) -> Self {
        text.clone_into(&mut self.sudo_text);
        self
    }

    fn answering(mut self, answers: &[&str]) -> Self {
        self.answers = answers.iter().map(|a| Some((*a).to_owned())).collect();
        self
    }

    fn node(&self, path: &str) -> Option<&Node> {
        self.tree.get(path)
    }

    fn commands(&self) -> Vec<&String> {
        self.log.iter().filter(|l| l.starts_with('/')).collect()
    }

    fn position(&self, needle: &str) -> Option<usize> {
        self.log.iter().position(|l| l.contains(needle))
    }

    fn put(&mut self, path: &str, owner: &str, group: &str, mode: u32, text: Option<&str>) {
        self.tree.insert(
            path.to_owned(),
            Node {
                owner: owner.to_owned(),
                group: group.to_owned(),
                mode,
                text: text.map(str::to_owned),
            },
        );
    }

    /// Carries out one command of the root step on the model.
    #[allow(clippy::too_many_lines)] // one arm for each command the install runs
    fn execute(&mut self, argv: &[String], input: Option<&str>) -> Result<(), String> {
        // `timeout <seconds> <command...>` runs the command.
        if argv[0].ends_with("/timeout") {
            return self.execute(&argv[2..], input);
        }
        let program = argv[0].rsplit('/').next().unwrap_or_default().to_owned();
        let rest = &argv[1..];
        match program.as_str() {
            "groupadd" => {
                self.files
                    .entry("/etc/group".to_owned())
                    .or_default()
                    .push_str("lunchroom:x:990:\n");
                Ok(())
            }
            "useradd" => {
                self.files
                    .entry("/etc/passwd".to_owned())
                    .or_default()
                    .push_str("lunchroom:x:990:990::/nonexistent:/usr/sbin/nologin\n");
                Ok(())
            }
            "install" => {
                let (mut dir, mut owner, mut group, mut mode) =
                    (false, "root".to_owned(), "root".to_owned(), 0o755u32);
                let mut positional = Vec::new();
                let mut it = rest.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-d" => dir = true,
                        "-o" => owner = it.next().cloned().unwrap_or_default(),
                        "-g" => group = it.next().cloned().unwrap_or_default(),
                        "-m" => {
                            mode = u32::from_str_radix(it.next().map_or("", String::as_str), 8)
                                .map_err(|e| e.to_string())?;
                        }
                        other => positional.push(other.to_owned()),
                    }
                }
                if dir {
                    let path = positional.first().cloned().unwrap_or_default();
                    self.tree.insert(
                        path,
                        Node {
                            owner,
                            group,
                            mode,
                            text: None,
                        },
                    );
                    return Ok(());
                }
                let [src, dst] = positional.as_slice() else {
                    return Err("install: wrong number of paths".to_owned());
                };
                if src == "/dev/stdin" && input.is_none() {
                    return Err("install: no input".to_owned());
                }
                let text = if src == "/dev/stdin" {
                    input.map(str::to_owned)
                } else if *src == self.source.display().to_string() {
                    // The program as it was running (or, tampered with, another).
                    let sha = if self.tamper {
                        "ee".repeat(32)
                    } else {
                        self.source_sha.clone()
                    };
                    Some(format!("program:{sha}"))
                } else {
                    Some(format!("(a file from {src})"))
                };
                self.tree.insert(
                    dst.clone(),
                    Node {
                        owner,
                        group,
                        mode,
                        text,
                    },
                );
                Ok(())
            }
            "sh" => {
                // The key made as root from the random source: the script's `$0` is the file.
                let (Some(script), Some(path)) = (rest.get(1), rest.get(2)) else {
                    return Err("sh: no script".to_owned());
                };
                if !script.contains("/dev/urandom") {
                    return Err(format!("the stand-in does not know the script {script}"));
                }
                self.seed = self.seed.wrapping_add(1);
                let text = self
                    .key_text
                    .clone()
                    .unwrap_or_else(|| console_keys::private_text(&[self.seed; 32]));
                self.tree.insert(
                    path.clone(),
                    Node {
                        owner: "root".to_owned(),
                        group: "root".to_owned(),
                        mode: 0o600,
                        text: Some(text),
                    },
                );
                Ok(())
            }
            // `env -C <folder> sha256sum -c ... /dev/stdin`: the list is the input.
            "env" => {
                let folder = rest.get(1).cloned().unwrap_or_default();
                if rest.first().map(String::as_str) == Some("-C")
                    && rest.iter().any(|a| a.ends_with("/sha256sum"))
                {
                    // What `cp` read as the store's `marker` is what the copy holds: the list's
                    // line for `marker` must be that text's sha-256, as `sha256sum -c` finds.
                    if let (Some(marker), Some(text)) = (&self.copied_marker, input)
                        && let Some(line) = text.lines().find(|l| l.ends_with("  marker"))
                        && !line.starts_with(&lunchroom_core::jcs::sha256_hex(marker.as_bytes()))
                    {
                        return Err("sha256sum: marker: FAILED".to_owned());
                    }
                    let lines = input.map_or(0, |t| t.lines().count() as u64);
                    self.listed.insert(folder, lines);
                    Ok(())
                } else {
                    Err(format!("the stand-in does not know env {rest:?}"))
                }
            }
            "test" => Ok(()),
            "mv" => {
                let [from, to] = rest else {
                    return Err("mv: wrong number of paths".to_owned());
                };
                let node = self
                    .tree
                    .remove(from)
                    .ok_or(format!("mv: {from}: no such file"))?;
                self.tree.insert(to.clone(), node);
                Ok(())
            }
            "rm" => {
                let recursive = rest.iter().any(|a| a.starts_with('-') && a.contains('r'));
                for p in rest.iter().filter(|a| !a.starts_with('-')) {
                    self.tree.remove(p);
                    if recursive {
                        let below = format!("{p}/");
                        self.tree.retain(|k, _| !k.starts_with(&below));
                    }
                }
                Ok(())
            }
            "visudo" => {
                if rest.iter().any(|a| a == "-f") {
                    if self.visudo_file_fails {
                        return Err("visudo: syntax error".to_owned());
                    }
                    let path = rest.last().cloned().unwrap_or_default();
                    let Some(node) = self.tree.get(&path) else {
                        return Err(format!("visudo: {path}: no such file"));
                    };
                    self.checked.push(node.clone());
                    Ok(())
                } else if self.visudo_all_fails {
                    Err("visudo: syntax error in the whole configuration".to_owned())
                } else {
                    Ok(())
                }
            }
            "cp" => {
                let dst = rest.last().cloned().unwrap_or_default();
                let src = rest[rest.len() - 2].clone();
                self.tree.insert(
                    dst,
                    Node {
                        owner: "root".to_owned(),
                        group: "root".to_owned(),
                        mode: 0o644,
                        text: Some(format!("(a copy of {src})")),
                    },
                );
                Ok(())
            }
            "chown" => {
                let spec = rest
                    .iter()
                    .find(|a| a.contains(':'))
                    .cloned()
                    .unwrap_or_default();
                let (o, g) = spec.split_once(':').unwrap_or_default();
                let path = rest.last().cloned().unwrap_or_default();
                if let Some(n) = self.tree.get_mut(&path) {
                    o.clone_into(&mut n.owner);
                    g.clone_into(&mut n.group);
                }
                Ok(())
            }
            "chmod" => {
                let path = rest.last().cloned().unwrap_or_default();
                if let (Some(n), Ok(m)) =
                    (self.tree.get_mut(&path), u32::from_str_radix(&rest[0], 8))
                {
                    n.mode = m;
                }
                Ok(())
            }
            other => Err(format!("the stand-in does not know {other}")),
        }
    }
}

impl System for Sim {
    fn ancestors(&self) -> Vec<(Option<String>, String)> {
        self.ancestors.clone()
    }
    fn own_tty(&self) -> u64 {
        self.tty
    }
    fn tty_of(&self, key: &str) -> Option<u64> {
        (key == "200:7").then_some(self.harness_tty)
    }
    fn ask(&mut self, question: &str) -> Option<String> {
        self.asked.push(question.to_owned());
        if self.answers.is_empty() {
            None
        } else {
            self.answers.remove(0)
        }
    }
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn verifier(&self) -> Result<String, String> {
        self.verifier.clone()
    }
    fn confirm_root(&mut self, question: &str) -> Result<(), String> {
        self.root_questions.push(question.to_owned());
        self.root_confirms.clone()
    }
    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        self.asked.push(prompt.to_owned());
        self.secret.clone()
    }
    fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
        self.seed += 1;
        Ok([self.seed; lunchroom_core::passphrase::SALT_BYTES])
    }
    fn install_verifier(&mut self, _line: &str) -> Result<(), String> {
        Ok(())
    }
    fn sudo_list(&mut self) -> Result<String, String> {
        // As sudo shows it: once the console's launch rule is in place, `sudo -n -l` lists it.
        let mut text = self.sudo_text.clone();
        if self.tree.contains_key("/etc/sudoers.d/lunchroom") {
            text.push_str("    (lunchroom) /usr/local/lib/lunchroom/lunchroom console\n");
        }
        Ok(text)
    }
    fn keep_sudo_answer(&mut self, kept: &Kept) {
        self.kept.push(kept.clone());
    }
    fn sudo_list_user(&mut self, user: &str) -> Result<String, String> {
        self.log.push(format!("list-user {user}"));
        self.others
            .get(user)
            .cloned()
            .ok_or_else(|| format!("no rules for {user}"))
    }
    fn root_blocker(&self) -> Option<String> {
        None
    }
    fn run_root(
        &mut self,
        _what: &str,
        argv: &[String],
        input: Option<&str>,
    ) -> Result<(), String> {
        let line = argv.join(" ");
        self.log.push(line.clone());
        if let Some(text) = input {
            self.inputs.push(text.to_owned());
        }
        if let Some(at) = self
            .fail_once
            .iter()
            .position(|w| line.contains(w.as_str()))
        {
            self.fail_once.remove(at);
            return Err(format!("stopped on purpose: {line}"));
        }
        if line.contains("console.pub.new")
            && argv[0].ends_with("/install")
            && let Some(home) = &self.lock_home
        {
            self.old_store_gone_at_pub = Some(!home.join("store").exists());
        }
        if self.swap_store_on_copy
            && line.contains("rm -rf")
            && line.contains("store.new")
            && let Some(home) = self.lock_home.clone()
        {
            // A decoy takes the old store's place after it was opened and looked at.
            self.swap_store_on_copy = false;
            let decoy = home.join("decoy");
            let _ = fs::create_dir_all(&decoy);
            let _ = fs::write(decoy.join("marker"), "decoy");
            let aside = home.join("store-aside");
            let _ = fs::rename(home.join("store"), &aside);
            let _ = std::os::unix::fs::symlink(&decoy, home.join("store"));
        }
        if line.contains("/cp ") {
            let src = argv[argv.len() - 2].clone();
            if let Ok(text) = fs::read_to_string(format!("{src}/marker")) {
                self.copied_marker = Some(text);
            }
            if self.grow_on_cp > 0
                && let Some(home) = self.lock_home.clone()
                && let Ok(mut f) = fs::OpenOptions::new()
                    .append(true)
                    .open(home.join("store/marker"))
            {
                use std::io::Write as _;
                let _ = f.write_all(&vec![b'x'; usize::try_from(self.grow_on_cp).unwrap_or(0)]);
            }
        }
        if line.contains("/cp ")
            && let Some(home) = self.lock_home.clone()
        {
            // Held by this process when the install took it: a second handle cannot lock it.
            let held =
                fs::File::open(home.join("store/state.flock")).is_ok_and(|f| f.try_lock().is_err());
            self.cp_locked.push(held);
        }
        self.execute(argv, input)
    }
    fn program_source(&mut self) -> Result<crate::terminal::ProgramSource, String> {
        Ok(crate::terminal::ProgramSource {
            path: self.source.clone(),
            sha256: Some(self.source_sha.clone()),
            is_installed: self.running_is_installed,
        })
    }
    fn protect_memory(&mut self) -> Result<(), String> {
        if self.protect_fails {
            return Err("this process could not be made unreadable (injected)".to_owned());
        }
        self.protected = true;
        Ok(())
    }
    fn run_user(&mut self, program: &Path, args: &[&str]) -> Result<i32, String> {
        self.log
            .push(format!("run-user {} {}", program.display(), args.join(" ")));
        Ok(self
            .user_install_code
            .unwrap_or(i32::from(!self.user_install_ok)))
    }
    fn sets_up_wall(&self) -> bool {
        self.wall
    }
    fn read_text(&self, path: &Path) -> Option<String> {
        self.files.get(path.to_str()?).cloned()
    }
    fn exists(&self, path: &Path) -> bool {
        path.to_str().is_some_and(|p| self.tree.contains_key(p))
    }
    fn missing(&self, path: &Path) -> bool {
        !self.exists(path)
    }
    fn read_root(&mut self, _what: &str, argv: &[String]) -> Result<String, String> {
        self.log.push(format!("read-root {}", argv.join(" ")));
        let path = argv.last().cloned().unwrap_or_default();
        if argv[0].ends_with("/du") {
            // The size of what the copy came out at: the old store as it is now.
            let bytes = self
                .lock_home
                .as_ref()
                .and_then(|h| {
                    super::console_setup::measure_tree(
                        &h.join("store"),
                        &super::console_setup::LIMITS,
                    )
                    .ok()
                })
                .map_or(100, |(b, _)| b);
            return Ok(format!("{bytes}\t{path}\n"));
        }
        if argv[0].ends_with("/find") {
            // One `.` for each entry that is not a folder: what the list had for that copy (a
            // single file's copy is checked in its folder, which the list names), and what a test
            // added to the copy.
            let parent = Path::new(&argv[1]).parent().and_then(Path::to_str);
            let named = self.listed.get(&argv[1]).copied();
            let in_folder = parent.and_then(|p| self.listed.get(p)).copied();
            let n = named.or(in_folder).unwrap_or(0) + self.extra_in_copy;
            return Ok(".".repeat(usize::try_from(n).unwrap_or(0)));
        }
        if argv[0].ends_with("/sha256sum") {
            let text = self.tree.get(&path).and_then(|n| n.text.clone());
            return text
                .and_then(|t| t.strip_prefix("program:").map(str::to_owned))
                .map(|sha| format!("{sha}  {path}\n"))
                .ok_or(format!("sha256sum: {path}: no such file"));
        }
        self.tree
            .get(&path)
            .and_then(|n| n.text.clone())
            .ok_or(format!("cat: {path}: no such file"))
    }
    fn sudo_probe(&mut self) -> Result<SudoProbe, String> {
        self.log.push("sudo-probe".to_owned());
        self.probe.clone()
    }
    fn stop_listeners(&mut self, programs: &[PathBuf], _uid: u32) -> Result<(), String> {
        self.log.push(format!("stop-listeners {}", programs.len()));
        // A symbolic link takes the old store's place while the listeners are being stopped.
        if let (true, Some(home)) = (self.swap_store_on_stop, self.lock_home.clone()) {
            self.swap_store_on_stop = false;
            let aside = home.join("store-aside");
            let _ = fs::rename(home.join("store"), &aside);
            let _ = std::os::unix::fs::symlink(&aside, home.join("store"));
        }
        self.stop_error.clone().map_or(Ok(()), Err)
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// An environment with an old store (one QUEUED message in it) and records, in a scratch home.
struct Home {
    root: PathBuf,
    env: Env,
}

impl Home {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = crate::test_target()
            .join("console-wall-tests")
            .join(format!("{}-{n}", std::process::id()));
        let home = root.join("home");
        let lunchroom_home = home.join(".lunchroom");
        let _ = fs::create_dir_all(lunchroom_home.join("store"));
        let _ = fs::write(lunchroom_home.join("store/marker"), "one queued message");
        let _ = fs::write(lunchroom_home.join("guard-records.jsonl"), "");
        let _ = fs::create_dir_all(lunchroom_home.join("guard-records.d"));
        let _ = fs::write(lunchroom_home.join("guard-records.d/waiting"), "{}");
        // A harness that has run once: `lunchroom install` with no harness named finds it.
        let _ = fs::create_dir_all(home.join(".claude"));
        let env = Env {
            lunchroom_home,
            claude_home: home.join(".claude"),
            codex_home: home.join(".codex"),
            hermes_home: home.join(".hermes"),
            home,
            claude_pid: None,
            attended: None,
            entrypoint: None,
            claude_session: None,
            codex_thread: None,
            hermes_session: None,
            lunchroom_session: None,
            now: 1_793_023_200,
        };
        Self { root, env }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn facts() -> Facts {
    Facts {
        uid: 1000,
        developer: "dev".to_owned(),
        primary_group: "dev".to_owned(),
        source: PathBuf::from("/proc/4242/fd/9"),
        source_sha256: Some("ab".repeat(32)),
        running_is_installed: false,
        group_exists: false,
        user_exists: false,
        accounts: Accounts::Shadow,
        old_store: Some(PathBuf::from("/home/u/.lunchroom/store")),
        old_records: Some(PathBuf::from("/home/u/.lunchroom/guard-records.jsonl")),
        old_records_dir: Some(PathBuf::from("/home/u/.lunchroom/guard-records.d")),
        keys: Keys::Neither,
        state_exists: false,
        approvals_there: false,
        launch_rule_there: false,
        leftovers: Vec::new(),
        verifier: None,
    }
}

fn lines(steps: &[RootStep]) -> Vec<String> {
    steps.iter().map(|s| s.argv.join(" ")).collect()
}

fn make_keys() -> KeyPlan {
    KeyPlan::Make
}

fn mode_of(sim: &Sim, path: &str) -> Option<(String, String, u32)> {
    sim.node(path)
        .map(|n| (n.owner.clone(), n.group.clone(), n.mode))
}

// §17.2: the wall's steps, in full and in order, for a computer with nothing set up and an old
// store (A1.3 verifier group, the store folder 2770, the program 2755).
#[test]
fn the_wall_steps_for_a_new_setup_in_full() {
    assert_eq!(
        lines(&wall_steps(&facts(), &tool)),
        vec![
            "/usr/bin/groupadd --system lunchroom",
            "/usr/bin/useradd --system --no-create-home --shell /usr/sbin/nologin --gid \
             lunchroom lunchroom",
            "/usr/bin/install -d -o root -g root -m 755 /usr/local/lib/lunchroom",
            "/usr/bin/install -d -o root -g root -m 755 /var/lib/lunchroom",
            "/usr/bin/install -d -o root -g lunchroom -m 2770 /var/lib/lunchroom/1000",
            "/usr/bin/install -o root -g lunchroom -m 2755 /proc/4242/fd/9 \
             /usr/local/lib/lunchroom/lunchroom",
        ]
    );
}

// §17.2 (Fable H4): the program root installs is read from the file the install process holds on
// its own image, never from the path of the program in the user's folder, and its hash is read
// back as root and compared; a mismatch removes it.
#[test]
fn the_installed_program_comes_from_the_held_image_and_is_checked_by_hash() {
    let steps = wall_steps(&facts(), &tool);
    let install = steps
        .iter()
        .find(|s| s.argv.iter().any(|a| a == "2755"))
        .map(|s| (s.argv.clone(), s.verify.clone(), s.on_failure.clone()));
    let Some((argv, verify, on_failure)) = install else {
        return;
    };
    assert_eq!(argv[argv.len() - 2], "/proc/4242/fd/9");
    assert!(
        argv.iter().all(|a| !a.contains("/.lunchroom/bin/")),
        "{argv:?}"
    );
    assert_eq!(
        verify,
        Some(super::console_setup::Verify::Hash {
            path: setgid::PROGRAM.to_owned(),
            sha: "ab".repeat(32)
        })
    );
    assert_eq!(
        on_failure.map(|c| c.join(" ")),
        Some(format!("/usr/bin/rm -f {}", setgid::PROGRAM))
    );
}

// §17.2 (Fable M7; F2 design, "Store wall"): the store is copied by root from its real path beside
// its place, given to the user, checked against the list of the files this process holds open and
// renamed in last; a half made copy of an earlier try goes first; the records file
// and the folder of waiting records are copied too.
#[test]
fn the_store_copy_is_made_beside_compared_and_renamed_in_last() {
    assert_eq!(
        lines(&copy_store_steps(&facts(), &tool)),
        vec![
            "/usr/bin/rm -rf /var/lib/lunchroom/1000/store.new",
            "/usr/bin/timeout 600 /usr/bin/cp -R -P /home/u/.lunchroom/store \
             /var/lib/lunchroom/1000/store.new",
            "/usr/bin/chown -R 1000:lunchroom /var/lib/lunchroom/1000/store.new",
            "/usr/bin/chmod -R u=rwX,g=rwX,o= /var/lib/lunchroom/1000/store.new",
            "/usr/bin/timeout 600 /usr/bin/env -C /var/lib/lunchroom/1000/store.new \
             /usr/bin/sha256sum -c --strict --quiet /dev/stdin",
            "/usr/bin/test -e /var/lib/lunchroom/1000/store.new",
            "/usr/bin/mv /var/lib/lunchroom/1000/store.new /var/lib/lunchroom/1000/store",
            "/usr/bin/rm -rf /var/lib/lunchroom/1000/guard-records.jsonl.new",
            "/usr/bin/timeout 600 /usr/bin/cp -P /home/u/.lunchroom/guard-records.jsonl \
             /var/lib/lunchroom/1000/guard-records.jsonl.new",
            "/usr/bin/chown 1000:lunchroom /var/lib/lunchroom/1000/guard-records.jsonl.new",
            "/usr/bin/chmod 660 /var/lib/lunchroom/1000/guard-records.jsonl.new",
            "/usr/bin/timeout 600 /usr/bin/env -C /var/lib/lunchroom/1000 \
             /usr/bin/sha256sum -c --strict --quiet /dev/stdin",
            "/usr/bin/test -e /var/lib/lunchroom/1000/guard-records.jsonl.new",
            "/usr/bin/mv /var/lib/lunchroom/1000/guard-records.jsonl.new \
             /var/lib/lunchroom/1000/guard-records.jsonl",
            "/usr/bin/rm -rf /var/lib/lunchroom/1000/guard-records.d.new",
            "/usr/bin/timeout 600 /usr/bin/cp -R -P /home/u/.lunchroom/guard-records.d \
             /var/lib/lunchroom/1000/guard-records.d.new",
            "/usr/bin/chown -R 1000:lunchroom /var/lib/lunchroom/1000/guard-records.d.new",
            "/usr/bin/chmod -R u=rwX,g=rwX,o= /var/lib/lunchroom/1000/guard-records.d.new",
            "/usr/bin/timeout 600 /usr/bin/env -C /var/lib/lunchroom/1000/guard-records.d.new \
             /usr/bin/sha256sum -c --strict --quiet /dev/stdin",
            "/usr/bin/test -e /var/lib/lunchroom/1000/guard-records.d.new",
            "/usr/bin/mv /var/lib/lunchroom/1000/guard-records.d.new \
             /var/lib/lunchroom/1000/guard-records.d",
        ]
    );
}

// Existing accounts and no old store leave those steps out; the setgid program already running
// is not copied over itself.
#[test]
fn existing_accounts_no_old_store_and_the_running_program_leave_steps_out() {
    let mut f = facts();
    f.group_exists = true;
    f.user_exists = true;
    f.old_store = None;
    f.old_records = None;
    f.old_records_dir = None;
    f.running_is_installed = true;
    let all = lines(&wall_steps(&f, &tool)).join("\n");
    for gone in ["groupadd", "useradd", " cp ", "chown", "chmod", "-m 2755"] {
        assert!(!all.contains(gone), "{gone} in {all}");
    }
    assert_eq!(lines(&copy_store_steps(&f, &tool)).len(), 0);
    assert!(all.contains("install -d -o root -g lunchroom -m 2770 /var/lib/lunchroom/1000"));
}

// §17.9: the console's steps, in full for a new pair, the public key last and by rename.
#[test]
fn the_console_steps_for_a_new_pair_in_full() {
    let all = lines(&console_steps(&facts(), &tool, &make_keys()));
    assert_eq!(
        all,
        vec![
            "/usr/bin/install -d -o root -g root -m 755 /etc/lunchroom",
            "/usr/bin/install -d -o root -g root -m 755 /var/lib/lunchroom-approvals",
            "/usr/bin/install -d -o lunchroom -g dev -m 2750 /var/lib/lunchroom-approvals/1000",
            "/usr/bin/install -d -o 1000 -g lunchroom -m 2750 \
             /var/lib/lunchroom-approvals/1000/requests",
            "/usr/bin/install -o root -g root -m 0440 /dev/stdin \
             /etc/sudoers.d/lunchroom.unchecked",
            "/usr/bin/visudo -c -f /etc/sudoers.d/lunchroom.unchecked",
            "/usr/bin/mv /etc/sudoers.d/lunchroom.unchecked /etc/sudoers.d/lunchroom",
            "/usr/bin/sh -c umask 077 && /usr/bin/dd if=/dev/urandom of=\"$1\" bs=32 count=1 \
             iflag=fullblock status=none && /usr/bin/base64 -w0 \"$1\" > \"$0\" && echo >> \
             \"$0\" && /usr/bin/rm -f \"$1\" /etc/lunchroom/1000.console.key.new \
             /etc/lunchroom/1000.console.key.raw",
            "/usr/bin/chown lunchroom:lunchroom /etc/lunchroom/1000.console.key.new",
            "/usr/bin/chmod 0400 /etc/lunchroom/1000.console.key.new",
            "/usr/bin/mv /etc/lunchroom/1000.console.key.new /etc/lunchroom/1000.console.key",
        ]
    );
    // The public key is the last of all: its own steps, written beside and renamed.
    assert_eq!(
        lines(&pub_steps(&facts(), &tool, &make_keys())),
        vec![
            "/usr/bin/install -o root -g root -m 0644 /dev/stdin \
             /etc/lunchroom/1000.console.pub.new",
            "/usr/bin/mv /etc/lunchroom/1000.console.pub.new /etc/lunchroom/1000.console.pub",
        ]
    );
    assert_eq!(pub_steps(&facts(), &tool, &KeyPlan::Keep).len(), 0);
}

// §17.8: the rule file's steps: a temporary file whose name holds a `.`, mode 0440 and root's,
// `visudo -c -f` on it, the rename, `visudo -c` on the whole configuration; a failed check removes
// the file it checked.
#[test]
fn the_rule_file_is_written_checked_and_renamed_in_that_order() {
    let steps = rule_steps(1000, &tool);
    assert_eq!(
        lines(&steps),
        vec![
            "/usr/bin/install -o root -g root -m 0440 /dev/stdin \
             /etc/sudoers.d/zz-lunchroom-1000.new",
            "/usr/bin/visudo -c -f /etc/sudoers.d/zz-lunchroom-1000.new",
            "/usr/bin/mv /etc/sudoers.d/zz-lunchroom-1000.new /etc/sudoers.d/zz-lunchroom-1000",
            "/usr/bin/visudo -c",
        ]
    );
    assert_eq!(
        steps[0].input.as_deref(),
        Some("#1000 ALL=(ALL:ALL) PASSWD: ALL\n")
    );
    assert_eq!(rule_text(1000), "#1000 ALL=(ALL:ALL) PASSWD: ALL\n");
    for step in &steps[..3] {
        assert_eq!(
            step.on_failure.as_ref().map(|c| c.join(" ")),
            Some("/usr/bin/rm -f /etc/sudoers.d/zz-lunchroom-1000.new".to_owned())
        );
    }
    assert_eq!(
        steps[3].on_failure.as_ref().map(|c| c.join(" ")),
        Some("/usr/bin/rm -f /etc/sudoers.d/zz-lunchroom-1000".to_owned())
    );
    // `sudo` skips a name with a `.`: the temporary name has one, the final name has none.
    assert!(steps[0].argv[8].contains('.'));
    assert!(
        !steps[2].argv[2]
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .contains('.')
    );
}

// An account name is taken into a rule only when sudoers reads it as it is written.
#[test]
fn only_a_plain_account_name_goes_into_a_rule() {
    // The account stands in a rule by its user id, so a login with a `.` (sudoers takes it) or
    // any other printable character is no obstacle; only a control character, which could end
    // the comment line, is refused (Fable L7).
    for ok in [
        "dev",
        "a_b-c",
        "user1",
        "_x",
        "john.smith",
        "j.smith-2",
        "dev#x",
    ] {
        assert!(valid_account(ok), "{ok}");
    }
    for bad in ["", "dev\nroot", "dev\rroot", "a\u{0}b"] {
        assert!(!valid_account(bad), "{bad:?}");
    }
    let text = sudoers_text("john.smith", 1000);
    assert!(
        text.ends_with("#1000 ALL=(lunchroom) /usr/local/lib/lunchroom/lunchroom console\n"),
        "{text}"
    );
    assert!(!text.contains("NOPASSWD"));
    assert!(!text.lines().any(|l| l.starts_with("john.smith")), "{text}");
}

// Fable L7: the store wall needs no `visudo`: a system with `doas`, `run0` or `su` and no `sudo`
// has none. The console's rule files do.
// 2026-11-14 real-world kit `packages`, apk: Alpine has BusyBox's `addgroup` and `adduser` and no
// `groupadd` or `useradd`; the install stopped ("these programs were not found ... groupadd,
// useradd"). Classes: a system with BusyBox alone is refused; BusyBox steps that are not BusyBox's
// options; a system with both pairs that does not use `groupadd`; a system with neither pair let
// through. Fails before the change: `Accounts` and `accounts_from` do not exist.
#[test]
fn the_group_and_user_are_made_with_the_programs_the_system_has() {
    use super::console_setup::accounts_from;
    let with = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
    assert_eq!(
        accounts_from(&with(&["groupadd", "useradd"])),
        Some(Accounts::Shadow)
    );
    assert_eq!(
        accounts_from(&with(&["groupadd", "useradd", "addgroup", "adduser"])),
        Some(Accounts::Shadow)
    );
    assert_eq!(
        accounts_from(&with(&["addgroup", "adduser"])),
        Some(Accounts::Busybox)
    );
    assert_eq!(accounts_from(&with(&["groupadd", "adduser"])), None);
    assert_eq!(accounts_from(&with(&[])), None);
    let mut f = facts();
    f.accounts = Accounts::Busybox;
    let steps = lines(&wall_steps(&f, &|t: &str| format!("/usr/sbin/{t}")));
    assert!(
        steps.contains(&"/usr/sbin/addgroup -S lunchroom".to_owned()),
        "{steps:?}"
    );
    assert!(
        steps.contains(
            &"/usr/sbin/adduser -S -D -H -s /sbin/nologin -G lunchroom lunchroom".to_owned()
        ),
        "{steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|s| s.contains("groupadd") || s.contains("useradd")),
        "{steps:?}"
    );
    f.accounts = Accounts::Shadow;
    let steps = lines(&wall_steps(&f, &|t: &str| format!("/usr/sbin/{t}")));
    assert!(
        steps.contains(&"/usr/sbin/groupadd --system lunchroom".to_owned()),
        "{steps:?}"
    );
}

#[test]
fn the_wall_does_not_need_visudo_and_the_console_does() {
    let wall = required_tools(false);
    assert!(!wall.contains(&"visudo"), "{wall:?}");
    assert!(wall.contains(&"install") && wall.contains(&"env") && wall.contains(&"sha256sum"));
    assert!(wall.contains(&"find") && wall.contains(&"test"), "{wall:?}");
    let console = required_tools(true);
    for tool in ["visudo", "sh", "dd", "base64", "cat", "install"] {
        assert!(console.contains(&tool), "{tool}");
    }
}

// The person is shown every step before any is done, each rule's text, and never the private key.
#[test]
fn the_person_is_shown_every_step_and_never_the_private_key() {
    let text = steps_text(&console_steps(&facts(), &tool, &make_keys()));
    assert!(
        text.contains("     | #1000 ALL=(lunchroom) /usr/local/lib/lunchroom/lunchroom console\n"),
        "{text}"
    );
    assert!(
        text.contains("/usr/bin/visudo -c -f /etc/sudoers.d/lunchroom.unchecked"),
        "{text}"
    );
    // The seed is made by the root step: there is no key text in anything shown or written.
    assert!(text.contains("/dev/urandom"), "{text}");
    assert!(
        console_steps(&facts(), &tool, &make_keys())
            .iter()
            .all(|s| s
                .input
                .as_deref()
                .is_none_or(|i| console_keys::seed_from(i).is_none()))
    );
}

// F2-AC4, A1.3: a verifier written by 1.1.2 or earlier (root's, group root, mode 0644, readable by
// every process of the user's) is given the owner, group and mode this build requires (root's,
// group lunchroom, mode 0640) by the wall, after the group exists, with its contents unchanged.
#[test]
fn an_earlier_releases_verifier_is_given_to_the_group_lunchroom_at_mode_0640() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(NARROW);
    sim.put(
        "/etc/lunchroom/1000.verifier",
        "root",
        "root",
        0o644,
        Some("the check"),
    );
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(
        mode_of(&sim, "/etc/lunchroom/1000.verifier"),
        Some(("root".to_owned(), "lunchroom".to_owned(), 0o640))
    );
    let group = sim.position("groupadd");
    let chown = sim.position("chown root:lunchroom /etc/lunchroom/1000.verifier");
    assert!(
        group.is_some() && chown.is_some() && group < chown,
        "{group:?} {chown:?}"
    );
    // Without a verifier the wall has no such step.
    let steps = lines(&wall_steps(&facts(), &tool)).join("\n");
    assert!(!steps.contains("verifier"), "{steps}");
    let mut with = facts();
    with.verifier = Some(PathBuf::from("/etc/lunchroom/1000.verifier"));
    let steps = lines(&wall_steps(&with, &tool)).join("\n");
    assert!(
        steps.contains("/usr/bin/chown root:lunchroom /etc/lunchroom/1000.verifier")
            && steps.contains("/usr/bin/chmod 0640 /etc/lunchroom/1000.verifier"),
        "{steps}"
    );
}

// §17.9, §17.2, A1.3: after `install console` on an account whose password-free rules are all
// narrow, nothing is asked about `sudo`, and every path has the owner, group and mode the spec
// states.
#[test]
fn install_console_on_a_narrow_account_sets_every_path_as_the_spec_says() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(NARROW);
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    // No question about a rule: only the passphrase was asked.
    assert!(
        sim.asked.iter().all(|q| q.ends_with("Passphrase: ")),
        "{:?}",
        sim.asked
    );
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    let want = [
        ("/usr/local/lib/lunchroom", "root", "root", 0o755),
        (
            "/usr/local/lib/lunchroom/lunchroom",
            "root",
            "lunchroom",
            0o2755,
        ),
        ("/var/lib/lunchroom", "root", "root", 0o755),
        ("/var/lib/lunchroom/1000", "root", "lunchroom", 0o2770),
        ("/var/lib/lunchroom/1000/store", "1000", "lunchroom", 0o2770),
        ("/etc/lunchroom", "root", "root", 0o755),
        (
            "/etc/lunchroom/1000.console.key",
            "lunchroom",
            "lunchroom",
            0o400,
        ),
        ("/etc/lunchroom/1000.console.pub", "root", "root", 0o644),
        ("/var/lib/lunchroom-approvals", "root", "root", 0o755),
        (
            "/var/lib/lunchroom-approvals/1000",
            "lunchroom",
            "dev",
            0o2750,
        ),
        (
            "/var/lib/lunchroom-approvals/1000/requests",
            "1000",
            "lunchroom",
            0o2750,
        ),
        ("/etc/sudoers.d/lunchroom", "root", "root", 0o440),
    ];
    for (path, owner, group, mode) in want {
        let got = mode_of(&sim, path);
        // The store copy is made by `cp`, then given to the user and the group and its mode set
        // by `chown -R` and `chmod -R u=rwX,g=rwX,o=`: those two are checked as commands.
        if path.ends_with("/store") {
            assert!(
                sim.position("chown -R 1000:lunchroom /var/lib/lunchroom/1000/store")
                    .is_some()
            );
            assert!(
                sim.position("chmod -R u=rwX,g=rwX,o= /var/lib/lunchroom/1000/store")
                    .is_some()
            );
            continue;
        }
        assert_eq!(
            got,
            Some((owner.to_owned(), group.to_owned(), mode)),
            "{path}"
        );
    }
    // The public key is one line of base64 holding the 32 bytes of the key the private key makes.
    let private = sim
        .node("/etc/lunchroom/1000.console.key")
        .and_then(|n| n.text.clone());
    let public = sim
        .node("/etc/lunchroom/1000.console.pub")
        .and_then(|n| n.text.clone());
    let seed = private.as_deref().and_then(console_keys::seed_from);
    assert!(seed.is_some(), "{private:?}");
    assert_eq!(
        public.as_deref(),
        seed.as_ref().map(console_keys::public_text).as_deref()
    );
    let line = public.unwrap_or_default();
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "{line:?}"
    );
    assert_eq!(
        console_keys::b64_decode(line.trim_end()).map(|b| b.len()),
        Some(32)
    );
    // The hooks were written by the installed program, the old store moved aside, and the
    // sessions to start anew named.
    assert!(
        sim.position("run-user /usr/local/lib/lunchroom/lunchroom install")
            .is_some()
    );
    assert!(!home.env.lunchroom_home.join("store").exists());
    assert!(
        out.stdout.contains("The console is set up."),
        "{}",
        out.stdout
    );
    // The answer `sudo -n -l` gave was kept.
    assert_ne!(sim.kept.len(), 0);
}

// §17.9: the public key is written last, after every other step, the hooks and the old store set
// aside included, and a failure of any earlier step leaves none.
#[test]
fn the_public_key_is_written_last() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    // The last event of the whole run is the rename of the public key into place.
    assert_eq!(
        sim.log.last().map(String::as_str),
        Some("/usr/bin/mv /etc/lunchroom/1000.console.pub.new /etc/lunchroom/1000.console.pub")
    );
    let pub_write = sim.position("/etc/lunchroom/1000.console.pub.new");
    for other in [
        "groupadd",
        "install -d -o 1000",
        "visudo -c -f",
        "console.key",
        "lunchroom/lunchroom",
        "stop-listeners",
        "sha256sum -c",
        "run-user",
    ] {
        let at = sim.position(other);
        assert!(
            at.is_some() && at < pub_write,
            "{other}: {at:?} before {pub_write:?}"
        );
    }
    // The old store was already set aside when the public key was written.
    assert_eq!(sim.old_store_gone_at_pub, Some(true));
    // A failure of any step, the hooks' install included, leaves no public key.
    for broken in [
        "install -d -o 1000",
        "lunchroom.unchecked",
        "chmod 0400",
        "sha256sum -c",
        "2755",
    ] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.fail_once.push(broken.to_owned());
        let out = install_console(&home.env, &mut sim);
        assert_ne!(out.code, 0, "{broken}");
        assert!(
            sim.node("/etc/lunchroom/1000.console.pub").is_none(),
            "{broken}"
        );
    }
    let home = Home::new();
    let mut sim = Sim::new();
    sim.user_install_ok = false;
    let out = install_console(&home.env, &mut sim);
    assert_ne!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_none());
    assert!(sim.node("/etc/lunchroom/1000.console.key").is_some());
}

// §17.9: an existing pair is kept: a second install writes neither key.
#[test]
fn an_existing_key_pair_is_kept() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let before = (
        sim.node("/etc/lunchroom/1000.console.key").cloned(),
        sim.node("/etc/lunchroom/1000.console.pub").cloned(),
    );
    sim.log.clear();
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(sim.position("console.key").is_none(), "{:?}", sim.log);
    assert!(sim.position("console.pub").is_none(), "{:?}", sim.log);
    assert_eq!(
        before,
        (
            sim.node("/etc/lunchroom/1000.console.key").cloned(),
            sim.node("/etc/lunchroom/1000.console.pub").cloned(),
        )
    );
}

// §17.9: the public key was removed by uninstall and the private key stayed: the next install
// writes the public key of that private key, and no new key.
#[test]
fn a_private_key_alone_gets_its_own_public_key_back() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let private = sim.node("/etc/lunchroom/1000.console.key").cloned();
    let public = sim.node("/etc/lunchroom/1000.console.pub").cloned();
    sim.tree.remove("/etc/lunchroom/1000.console.pub");
    sim.log.clear();
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(
        sim.node("/etc/lunchroom/1000.console.key").cloned(),
        private
    );
    assert_eq!(sim.node("/etc/lunchroom/1000.console.pub").cloned(), public);
    assert!(
        sim.position("read-root /usr/bin/cat /etc/lunchroom/1000.console.key")
            .is_some()
    );
    assert!(
        !sim.log.iter().any(|l| l.contains("/dev/urandom")),
        "no new private key: {:?}",
        sim.log
    );
}

// A private key that is not one this program made stops the install before any public key is
// written, and names the file.
#[test]
fn a_damaged_private_key_stops_before_a_public_key_is_written() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.put(
        "/etc/lunchroom/1000.console.key",
        "lunchroom",
        "lunchroom",
        0o400,
        Some("junk\n"),
    );
    let out = install_console(&home.env, &mut sim);
    assert_ne!(out.code, 0);
    assert!(
        out.stderr.contains("/etc/lunchroom/1000.console.key"),
        "{}",
        out.stderr
    );
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_none());
    assert!(sim.node("/etc/lunchroom/1000.console.pub.new").is_none());
}

// A public key with no private key is of no use: it is removed first, then a new pair is made.
#[test]
fn a_public_key_alone_is_removed_first_and_a_new_pair_made() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.put(
        "/etc/lunchroom/1000.console.pub",
        "root",
        "root",
        0o644,
        Some("old\n"),
    );
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let removed = sim.position("rm -f /etc/lunchroom/1000.console.pub");
    let key = sim.position("/dev/urandom");
    assert!(removed.is_some() && removed < key, "{:?}", sim.log);
    let public = sim
        .node("/etc/lunchroom/1000.console.pub")
        .and_then(|n| n.text.clone());
    assert_ne!(public.as_deref(), Some("old\n"));
}

// §17.8: a wide rule and `yes`: the install stops, asks, writes the one rule into a file that is
// root's with mode 0440, checks it, and then (the probe refused for want of a password) sets the
// console up; the question names the rule and says the wall does not hold.
#[test]
fn a_wide_rule_and_yes_writes_the_one_rule_and_goes_on() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let node = sim.node("/etc/sudoers.d/zz-lunchroom-1000");
    assert_eq!(
        node.map(|n| (
            n.owner.as_str(),
            n.group.as_str(),
            n.mode,
            n.text.as_deref()
        )),
        Some((
            "root",
            "root",
            0o440,
            Some("#1000 ALL=(ALL:ALL) PASSWD: ALL\n")
        ))
    );
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000.new").is_none());
    // The question came before any root command, and said what the spec says.
    let question = &sim.asked[0];
    assert!(
        question.contains("the console's wall does not hold on this account"),
        "{question}"
    );
    assert!(question.contains("(ALL) NOPASSWD: ALL"), "{question}");
    assert!(
        question.contains("#1000 ALL=(ALL:ALL) PASSWD: ALL"),
        "{question}"
    );
    // The order: write, check the file, rename, check everything, probe, then the console.
    let order: Vec<Option<usize>> = [
        "install -o root -g root -m 0440 /dev/stdin /etc/sudoers.d/zz-lunchroom-1000.new",
        "visudo -c -f /etc/sudoers.d/zz-lunchroom-1000.new",
        "mv /etc/sudoers.d/zz-lunchroom-1000.new /etc/sudoers.d/zz-lunchroom-1000",
        "visudo -c\u{0}",
        "sudo-probe",
        "groupadd",
    ]
    .iter()
    .map(|w| match w.strip_suffix('\u{0}') {
        Some(exact) => sim.log.iter().position(|l| l.ends_with(exact)),
        None => sim.position(w),
    })
    .collect();
    assert!(order.iter().all(Option::is_some), "{order:?} {:?}", sim.log);
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // The console is set up and no record of a refusal remains.
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_some());
    assert!(sim.node("/etc/lunchroom/1000.console.state").is_none());
}

// §17.8: any answer but yes (no, empty, `y`, a sentence) writes no rule, sets nothing up, exits
// with a failure, and records the reason for `lunchroom doctor`.
#[test]
fn any_answer_but_yes_writes_nothing_and_records_the_reason() {
    for typed in ["no\n", "\n", "", "y\n", "yes please\n", "n"] {
        let home = Home::new();
        let mut sim = Sim::new().with_sudo(WIDE).answering(&[typed]);
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 3, "{typed:?}: {}{}", out.stdout, out.stderr);
        assert!(
            sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none(),
            "{typed:?}"
        );
        for word in [
            "groupadd",
            "useradd",
            "lunchroom/lunchroom",
            "console.key",
            "console.pub",
            "visudo",
        ] {
            assert!(
                sim.position(word).is_none(),
                "{typed:?} {word}: {:?}",
                sim.log
            );
        }
        let state = sim
            .node("/etc/lunchroom/1000.console.state")
            .and_then(|n| n.text.as_deref())
            .and_then(State::from_json);
        assert_eq!(
            state.as_ref().map(|s| s.kind),
            Some(Kind::Declined),
            "{typed:?}"
        );
        assert!(
            state.is_some_and(|s| s.reason.contains("not yes")),
            "{typed:?}"
        );
        assert!(out.stderr.contains("does not hold"), "{}", out.stderr);
        // The store was not moved.
        assert!(home.env.lunchroom_home.join("store").exists());
    }
}

// A1.3, §17.8: a `yes` typed by an agent's process, by a process with no controlling terminal, or
// in the terminal of a guarded session confirms nothing: nothing is asked, nothing is written.
#[test]
fn a_yes_from_an_agent_or_without_a_terminal_confirms_nothing() {
    type Setup = fn(&mut Sim);
    let cases: [(&str, Setup); 4] = [
        ("an agent above it", |s| {
            s.ancestors = vec![(Some("200:7".to_owned()), "claude".to_owned())];
        }),
        ("no controlling terminal", |s| s.tty = 0),
        ("the terminal of a guarded session", |s| {
            s.harness_tty = s.tty;
        }),
        ("a harness by name", |s| {
            s.ancestors = vec![(None, "codex".to_owned())];
        }),
    ];
    for (what, set) in cases {
        let home = Home::new();
        // The store knows the guarded session's process, as a hook records it.
        let store =
            lunchroom_core::store::Store::<lunchroom_core::mail::Mail>::open(&home.env.store_dir());
        let who = lunchroom_core::identity::Identity::new(
            lunchroom_core::identity::Harness::ClaudeCode,
            "s1",
        );
        assert!(
            store.is_ok_and(|s| s
                .update(|m| m.observe_process("200:7", Path::new("/"), &who))
                .is_ok()),
            "{what}"
        );
        let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
        set(&mut sim);
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 3, "{what}: {}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("DENIED"), "{what}: {}", out.stderr);
        assert!(sim.log.is_empty(), "{what}: {:?}", sim.log);
        assert!(sim.asked.is_empty(), "{what}: {:?}", sim.asked);
        assert!(
            sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none(),
            "{what}"
        );
    }
}

// §17.8: a terminal that gives no answer at all (no line read) is no yes.
#[test]
fn no_line_read_is_no_yes() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE);
    sim.answers = vec![None];
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(sim.log.is_empty(), "{:?}", sim.log);
}

// §17.8: a rule file that fails `visudo -c -f` is never put in place, nothing is set up, and the
// file it checked is removed.
#[test]
fn a_rule_file_that_fails_visudo_is_never_put_in_place() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.visudo_file_fails = true;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("visudo: syntax error"),
        "{}",
        out.stderr
    );
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000.new").is_none());
    assert!(
        sim.position("mv /etc/sudoers.d/zz-lunchroom-1000")
            .is_none()
    );
    for word in [
        "groupadd",
        "lunchroom/lunchroom",
        "console.key",
        "console.pub",
    ] {
        assert!(sim.position(word).is_none(), "{word}: {:?}", sim.log);
    }
    let state = sim
        .node("/etc/lunchroom/1000.console.state")
        .and_then(|n| n.text.as_deref())
        .and_then(State::from_json);
    assert_eq!(state.map(|s| s.kind), Some(Kind::NotInForce));
}

// §17.8: a check of the whole configuration that fails removes the file again.
#[test]
fn a_failed_check_of_the_whole_configuration_removes_the_file_again() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.visudo_all_fails = true;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(sim.position("sudo-probe").is_none(), "{:?}", sim.log);
    assert!(sim.position("groupadd").is_none());
    let state = sim
        .node("/etc/lunchroom/1000.console.state")
        .and_then(|n| n.text.as_deref())
        .and_then(State::from_json);
    assert_eq!(state.map(|s| s.kind), Some(Kind::NotInForce));
}

// §17.8 (§17::105-107): a password-free rule that a later file or /etc/sudoers holds wins: the
// install removes its file, sets nothing up, shows that rule as `sudo -n -l` prints it, says the
// change is the user's own to make in /etc/sudoers (remove the rule, or give it PASSWD:), and what
// else on the account uses that rule stops running unattended; doctor's record says why.
#[test]
fn a_later_rule_that_wins_leaves_nothing_set_up_and_is_shown() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.probe = Ok(SudoProbe::Ran);
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(
        sim.position("rm -f /etc/sudoers.d/zz-lunchroom-1000")
            .is_some()
    );
    for word in [
        "groupadd",
        "lunchroom/lunchroom",
        "console.key",
        "console.pub",
    ] {
        assert!(sim.position(word).is_none(), "{word}: {:?}", sim.log);
    }
    let text = &out.stderr;
    assert!(text.contains("(ALL) NOPASSWD: ALL"), "{text}");
    assert!(text.contains("/etc/sudoers"), "{text}");
    assert!(
        text.contains("remove that rule, or give it PASSWD:"),
        "{text}"
    );
    assert!(text.contains("stops running unattended"), "{text}");
    assert!(text.contains("Nothing was set up"), "{text}");
    let state = sim
        .node("/etc/lunchroom/1000.console.state")
        .and_then(|n| n.text.as_deref())
        .and_then(State::from_json);
    assert_eq!(state.as_ref().map(|s| s.kind), Some(Kind::NotInForce));
    assert!(state.is_some_and(|s| s.reason.contains("later")));
}

// §17.8: `sudo -n true` refused for any other reason is not a rule in force either.
#[test]
fn a_probe_refused_for_another_reason_is_not_a_rule_in_force() {
    for probe in [
        Ok(SudoProbe::Other("sudo: unknown user".to_owned())),
        Err("sudo cannot be started".to_owned()),
    ] {
        let home = Home::new();
        let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
        sim.probe = probe;
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
        assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
        assert!(sim.position("groupadd").is_none());
    }
}

// §17.2 (Fable M7a): the earlier program's listeners are stopped after every question and after
// the first root commands, immediately before the copy, and looked for once more after it; one
// that does not stop leaves the store where it was and the install names it and fails.
#[test]
fn listeners_stop_just_before_the_copy_and_again_after_it() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let stops: Vec<usize> = sim
        .log
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("stop-listeners"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(stops.len(), 2, "{:?}", sim.log);
    let first_root = sim.log.iter().position(|l| l.starts_with('/'));
    let first_copy = sim.position("cp -R -P");
    let last_store_rename = sim
        .log
        .iter()
        .rposition(|l| l.contains("mv /var/lib/lunchroom/1000/"));
    // After the first root command (the question and the password came before), before the copy.
    assert!(
        first_root < Some(stops[0]) && Some(stops[0]) < first_copy,
        "{:?}",
        sim.log
    );
    // Again after the last rename of the copy.
    assert!(Some(stops[1]) > last_store_rename, "{:?}", sim.log);
    // One that does not stop: nothing is copied, the old store stays, the process is named.
    for plain in [false, true] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.stop_error =
            Some("process 4242 (an earlier Lunchroom's `listen`) did not stop".to_owned());
        let out = if plain {
            install_with_wall(&[], &home.env, &mut sim)
        } else {
            install_console(&home.env, &mut sim)
        };
        assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("process 4242"), "{}", out.stderr);
        assert!(sim.position("cp ").is_none(), "{:?}", sim.log);
        assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
        assert!(home.env.lunchroom_home.join("store/marker").exists());
        assert!(sim.position("run-user").is_none());
    }
}

// §17.2 (Fable M7b): the old store's lock is held by the install while the copy is made, so a
// hook or a message that writes the old folder waits; it is let go afterwards.
#[test]
fn the_old_store_is_locked_while_it_is_copied() {
    let home = Home::new();
    // The lock file is there once the store has been read.
    let read =
        lunchroom_core::store::Store::<lunchroom_core::mail::Mail>::open(&home.env.store_dir())
            .and_then(|s| s.read());
    assert!(read.is_ok());
    // The helper itself: held until dropped, a second handle cannot take it.
    let flock = home.env.lunchroom_home.join("store/state.flock");
    let lock = OldStoreLock::take(&home.env.lunchroom_home);
    assert!(lock.is_ok());
    assert!(fs::File::open(&flock).is_ok_and(|f| f.try_lock().is_err()));
    drop(lock);
    assert!(fs::File::open(&flock).is_ok_and(|f| f.try_lock().is_ok()));
    // The install holds it while `cp` runs, and lets it go afterwards.
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_ne!(sim.cp_locked.len(), 0);
    assert!(
        sim.cp_locked.iter().all(|held| *held),
        "{:?}",
        sim.cp_locked
    );
}

// §17.2 (Fable M7c): a copy that stops half way (the `chown` fails) is never taken for the store: the
// new store is not in place, the old one is not set aside, and the rerun removes the half made
// copy and makes it again.
#[test]
fn a_copy_that_stops_half_way_is_redone_by_a_rerun() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.fail_once
        .push("chown -R 1000:lunchroom /var/lib/lunchroom/1000/store.new".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    // The half made copy was removed when the step failed.
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
    assert!(home.env.lunchroom_home.join("store/marker").exists());
    assert!(sim.position("run-user").is_none());
    // The rerun copies again and finishes.
    sim.log.clear();
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_some());
    assert!(
        sim.position("rm -rf /var/lib/lunchroom/1000/store.new")
            .is_some()
    );
    assert!(!home.env.lunchroom_home.join("store").exists());
}

// §17.2 (Fable M7d): the folder of waiting records is copied with the store, and a copy that
// differs from the original is never renamed in or set aside for.
#[test]
fn the_waiting_records_are_copied_and_a_copy_that_differs_is_not_taken() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    assert!(
        sim.node("/var/lib/lunchroom/1000/guard-records.d")
            .is_some()
    );
    assert!(
        sim.node("/var/lib/lunchroom/1000/guard-records.jsonl")
            .is_some()
    );
    let home = Home::new();
    let mut sim = Sim::new();
    sim.fail_once.push("sha256sum -c".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
    assert!(home.env.lunchroom_home.join("store/marker").exists());
    assert!(
        home.env
            .lunchroom_home
            .join("guard-records.d/waiting")
            .exists()
    );
}

// §17.2 (Fable H4): the install reads the program from the held image, hashes it back as root and
// stops, removing it, where the hash is not the running program's; the memory is protected before
// the first question.
#[test]
fn the_program_installed_is_the_one_that_was_running() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    assert!(sim.protected);
    assert!(
        sim.position("-m 2755 /proc/4242/fd/9 /usr/local/lib/lunchroom/lunchroom")
            .is_some()
    );
    assert!(
        sim.log
            .iter()
            .all(|l| !l.contains("/.lunchroom/bin/lunchroom ")
                && !l.contains(
                    &*std::env::current_exe()
                        .unwrap_or_default()
                        .to_string_lossy()
                )),
        "{:?}",
        sim.log
    );
    let hashed = sim.position("read-root /usr/bin/sha256sum /usr/local/lib/lunchroom/lunchroom");
    assert!(
        hashed.is_some() && hashed < sim.position("run-user"),
        "{:?}",
        sim.log
    );
    // Another file in its place: removed, nothing after it runs.
    let home = Home::new();
    let mut sim = Sim::new();
    sim.tamper = true;
    let out = install_with_wall(&[], &home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("is not the program that was running"),
        "{}",
        out.stderr
    );
    assert!(sim.node("/usr/local/lib/lunchroom/lunchroom").is_none());
    assert!(
        sim.position("cp ").is_none() && sim.position("run-user").is_none(),
        "{:?}",
        sim.log
    );
    assert!(home.env.lunchroom_home.join("store/marker").exists());
}

// §17.9 (Fable M13): the seed of a new key is made by the root step from the random source and
// never passes through this process; the public key is read from the private key by the root step.
#[test]
fn the_console_key_is_made_by_the_root_step() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let made = sim.log.iter().find(|l| l.contains("/dev/urandom")).cloned();
    assert!(made.is_some_and(|l| l.contains("/sh -c ")));
    // No text given to a root command is a key, and the key was read back only by root's `cat`.
    let private_text = sim
        .node("/etc/lunchroom/1000.console.key")
        .and_then(|n| n.text.clone());
    assert!(private_text.is_some());
    assert!(
        sim.inputs.iter().all(|i| Some(i) != private_text.as_ref()),
        "{:?}",
        sim.inputs
    );
    assert!(
        sim.position("read-root /usr/bin/cat /etc/lunchroom/1000.console.key")
            .is_some()
    );
    assert!(sim.protected);
    let private = sim
        .node("/etc/lunchroom/1000.console.key")
        .and_then(|n| n.text.clone());
    let public = sim
        .node("/etc/lunchroom/1000.console.pub")
        .and_then(|n| n.text.clone());
    let seed = private.as_deref().and_then(console_keys::seed_from);
    assert_eq!(
        public.as_deref(),
        seed.as_ref().map(console_keys::public_text).as_deref()
    );
    // Two installs on two computers make two different keys.
    let home = Home::new();
    let mut other = Sim::new();
    other.seed = 40;
    assert_eq!(install_console(&home.env, &mut other).code, 0);
    assert_ne!(
        other
            .node("/etc/lunchroom/1000.console.key")
            .and_then(|n| n.text.clone()),
        private
    );
}

// §17.8 (Fable M12): a typed `yes` is not enough: the A1.3 confirmation follows (the passphrase),
// and a wrong one writes no rule.
#[test]
fn a_yes_without_the_passphrase_writes_no_rule() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.secret = Some("not the passphrase".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("not confirmed"), "{}", out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(sim.position("groupadd").is_none());
    let state = sim
        .node("/etc/lunchroom/1000.console.state")
        .and_then(|n| n.text.as_deref())
        .and_then(State::from_json);
    assert_eq!(state.map(|s| s.kind), Some(Kind::Declined));
    // And no passphrase typed at all.
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.secret = None;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
}

// §17.8 (Fable M8): a later rule that lets the account run commands as the user `lunchroom`
// without a password leaves the rule not in force, though root's probe was refused.
#[test]
fn a_later_rule_for_the_user_lunchroom_is_not_a_rule_in_force() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.probe = Ok(SudoProbe::RunsAsLunchroom);
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr
            .contains("as the user or the group lunchroom without a password"),
        "{}",
        out.stderr
    );
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(sim.position("groupadd").is_none());
    let state = sim
        .node("/etc/lunchroom/1000.console.state")
        .and_then(|n| n.text.as_deref())
        .and_then(State::from_json);
    assert_eq!(state.map(|s| s.kind), Some(Kind::NotInForce));
}

// Codex ruling, item 4: the key is made without a pipeline and checked after it: a failing `dd`, a
// short read and a truncated file each stop the install, remove what was made and leave no key,
// no raw seed, no public key and the store where it was.
#[test]
fn a_failed_short_or_cut_key_read_stops_the_install_and_leaves_nothing() {
    let short = console_keys::b64_encode(&[7u8; 20]) + "\n";
    let cut = {
        let whole = console_keys::private_text(&[9u8; 32]);
        whole[..whole.len() - 3].to_owned()
    };
    let no_newline = console_keys::b64_encode(&[7u8; 32]);
    let two_lines = format!("{}{}", console_keys::private_text(&[1u8; 32]), "\n");
    let cases: [(&str, Option<&str>, Option<String>); 5] = [
        ("a failing dd", Some("dd if=/dev/urandom"), None),
        ("a short read", None, Some(short)),
        ("a truncated file", None, Some(cut)),
        ("no newline", None, Some(no_newline)),
        ("an extra line", None, Some(two_lines)),
    ];
    for (what, fail, text) in cases {
        let home = Home::new();
        let mut sim = Sim::new();
        if let Some(word) = fail {
            sim.fail_once.push(word.to_owned());
        }
        sim.key_text = text;
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 1, "{what}: {}{}", out.stdout, out.stderr);
        for gone in [
            "/etc/lunchroom/1000.console.key",
            "/etc/lunchroom/1000.console.key.new",
            "/etc/lunchroom/1000.console.key.raw",
            "/etc/lunchroom/1000.console.pub",
        ] {
            assert!(sim.node(gone).is_none(), "{what}: {gone}");
        }
        assert!(
            home.env.lunchroom_home.join("store/marker").exists(),
            "{what}"
        );
        assert!(
            sim.node("/var/lib/lunchroom/1000/store").is_none(),
            "{what}"
        );
        assert!(sim.position("run-user").is_none(), "{what}");
    }
    // A whole key passes.
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
}

// Codex ruling, item 3 (§17.8: "removes its file, sets nothing up"): the store moves only after
// every console root step has passed. A failed `visudo` check, a failed rule install and a failed
// key step each leave the store where it was, no console part behind, no public key and no hooks.
#[test]
fn a_console_step_that_fails_leaves_the_store_unmoved_and_no_console_part() {
    for (what, word) in [
        (
            "visudo on the launch rule",
            "visudo -c -f /etc/sudoers.d/lunchroom.unchecked",
        ),
        (
            "installing the launch rule",
            "install -o root -g root -m 0440 /dev/stdin /etc/sudoers.d/lunchroom.unchecked",
        ),
        (
            "the delivery folder",
            "install -d -o lunchroom -g dev -m 2750",
        ),
        (
            "the key step",
            "chmod 0400 /etc/lunchroom/1000.console.key.new",
        ),
        ("the key rename", "mv /etc/lunchroom/1000.console.key.new"),
    ] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.fail_once.push(word.to_owned());
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 1, "{what}: {}{}", out.stdout, out.stderr);
        // The store was never touched: not stopped for, not copied, not renamed, not set aside.
        assert!(
            home.env.lunchroom_home.join("store/marker").exists(),
            "{what}"
        );
        assert!(
            sim.position("stop-listeners").is_none(),
            "{what}: {:?}",
            sim.log
        );
        assert!(sim.position("cp ").is_none(), "{what}");
        assert!(
            sim.node("/var/lib/lunchroom/1000/store").is_none(),
            "{what}"
        );
        assert!(sim.position("run-user").is_none(), "{what}");
        // No console part is left.
        for part in [
            "/etc/sudoers.d/lunchroom",
            "/etc/sudoers.d/lunchroom.unchecked",
            "/etc/lunchroom/1000.console.key",
            "/etc/lunchroom/1000.console.key.new",
            "/etc/lunchroom/1000.console.pub",
            "/var/lib/lunchroom-approvals/1000",
            "/var/lib/lunchroom-approvals/1000/requests",
        ] {
            assert!(sim.node(part).is_none(), "{what}: {part} is left");
        }
        assert!(
            out.stdout.contains("store was not moved")
                || out.stderr.contains("store was not moved"),
            "{what}"
        );
    }
    // A pair and a delivery folder that were there before are never removed by a failed rerun.
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let key = sim.node("/etc/lunchroom/1000.console.key").cloned();
    sim.fail_once
        .push("visudo -c -f /etc/sudoers.d/lunchroom.unchecked".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert_eq!(sim.node("/etc/lunchroom/1000.console.key").cloned(), key);
    assert!(sim.node("/var/lib/lunchroom-approvals/1000").is_some());
    assert!(sim.node("/etc/sudoers.d/lunchroom").is_some());
}

// Codex ruling (M7): a symbolic link in place of the old store, its records file or its folder of
// waiting records is never copied through: the install stops at discovery, naming it, before any
// root command, for both entry points.
#[test]
fn a_symbolic_link_for_the_old_store_or_its_records_stops_the_install() {
    for what in ["store", "guard-records.jsonl", "guard-records.d"] {
        for plain in [false, true] {
            let home = Home::new();
            let target = home.root.join("elsewhere");
            let _ = fs::create_dir_all(&target);
            let path = home.env.lunchroom_home.join(what);
            let _ = fs::remove_dir_all(&path);
            let _ = fs::remove_file(&path);
            assert!(std::os::unix::fs::symlink(&target, &path).is_ok());
            let mut sim = Sim::new();
            let out = if plain {
                install_with_wall(&[], &home.env, &mut sim)
            } else {
                install_console(&home.env, &mut sim)
            };
            assert_eq!(out.code, 1, "{what} {plain}: {}{}", out.stdout, out.stderr);
            assert!(
                out.stderr.contains("is a symbolic link"),
                "{what}: {}",
                out.stderr
            );
            assert!(out.stderr.contains(what), "{what}: {}", out.stderr);
            assert_eq!(sim.commands().len(), 0, "{what} {plain}: {:?}", sim.log);
        }
    }
}

// Codex ruling (M7): a link that takes the store's place after discovery (while the listeners are
// stopped, under the locks) is found by the second look: nothing is copied.
#[test]
fn a_store_replaced_by_a_link_after_discovery_is_not_copied() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    sim.swap_store_on_stop = true;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("is a symbolic link"), "{}", out.stderr);
    assert!(sim.position("cp ").is_none(), "{:?}", sim.log);
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.position("run-user").is_none());
}

// Codex ruling (M7): a lock file that is not there is made and locked (never skipped); one that is
// a symbolic link, or whose folder is not a real folder, is refused or left out.
#[test]
fn a_missing_lock_file_is_made_and_locked_and_a_linked_one_is_refused() {
    let home = Home::new();
    let flock = home.env.lunchroom_home.join("store/state.flock");
    let records = home.env.lunchroom_home.join("guard-records.flock");
    let _ = fs::remove_file(&flock);
    let _ = fs::remove_file(&records);
    let lock = OldStoreLock::take(&home.env.lunchroom_home);
    assert!(lock.is_ok());
    for file in [&flock, &records] {
        assert!(file.exists(), "{}", file.display());
        assert!(fs::File::open(file).is_ok_and(|f| f.try_lock().is_err()));
    }
    drop(lock);
    // A symbolic link in the lock's place.
    let _ = fs::remove_file(&flock);
    let other = home.root.join("other.flock");
    let _ = fs::write(&other, "");
    assert!(std::os::unix::fs::symlink(&other, &flock).is_ok());
    let refused = OldStoreLock::take(&home.env.lunchroom_home);
    assert!(refused.is_err_and(|e| e.contains("state.flock")));
}

// Codex ruling (M7): the copy is bounded: more bytes, more entries or more depth than the limits
// is an error, a link counts as one entry and is not followed, and an install over the limits
// copies nothing.
#[test]
fn the_size_of_the_old_store_is_bounded() {
    let home = Home::new();
    let tree = home.root.join("tree");
    let _ = fs::create_dir_all(tree.join("a/b/c"));
    let _ = fs::write(tree.join("a/one"), vec![0u8; 100]);
    let _ = fs::write(tree.join("a/b/two"), vec![0u8; 100]);
    let roomy = Limits {
        bytes: 1 << 20,
        entries: 1000,
        depth: 8,
        seconds: 1,
    };
    let measured = measure_tree(&tree, &roomy);
    assert!(measured.is_ok_and(|(bytes, entries)| bytes >= 200 && entries == 6));
    let few = Limits {
        entries: 5,
        ..roomy
    };
    assert!(measure_tree(&tree, &few).is_err_and(|e| e.contains("files and folders")));
    let small = Limits {
        bytes: 150,
        ..roomy
    };
    assert!(measure_tree(&tree, &small).is_err_and(|e| e.contains("bytes")));
    let shallow = Limits { depth: 2, ..roomy };
    assert!(measure_tree(&tree, &shallow).is_err_and(|e| e.contains("deeper")));
    // A link to something huge is one entry; it is not walked.
    let link = home.root.join("link-tree");
    let _ = fs::create_dir_all(&link);
    assert!(std::os::unix::fs::symlink(&home.root, link.join("loop")).is_ok());
    assert!(measure_tree(&link, &roomy).is_ok_and(|(_, entries)| entries == 2));
    // The install over the limits copies nothing and stops the move.
    let mut sim = Sim::new();
    let facts = Facts {
        old_store: Some(home.env.store_dir()),
        old_records: None,
        old_records_dir: None,
        ..facts()
    };
    let tiny = Limits {
        entries: 1,
        ..roomy
    };
    let moved = move_store_with(&home.env, &mut sim, &facts, Path::new("/x"), &tiny);
    assert!(moved.is_err_and(|o| o.code == 1 && o.stderr.contains("more than")));
    assert!(sim.position("cp ").is_none(), "{:?}", sim.log);
    assert!(home.env.lunchroom_home.join("store/marker").exists());
}

// Codex ruling (M7): a `cp` and a `diff` that run without end are stopped by `timeout`.
#[test]
fn every_copy_and_compare_runs_under_a_time_limit() {
    let steps = copy_store_steps(&facts(), &tool);
    let bounded: Vec<&RootStep> = steps
        .iter()
        .filter(|s| {
            s.argv
                .iter()
                .any(|a| a.ends_with("/cp") || a.ends_with("/env"))
        })
        .collect();
    assert_eq!(bounded.len(), 6);
    for s in bounded {
        assert_eq!(&s.argv[..2], ["/usr/bin/timeout", "600"], "{:?}", s.argv);
    }
}

// Codex ruling (M12): the passphrase confirmation comes first, so a process that only has a
// terminal of its own never reaches the `yes` line; a `yes` followed by pages of spaces is no yes.
#[test]
fn the_confirmation_comes_before_the_yes_line_and_a_long_yes_is_no_yes() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.secret = Some("not the passphrase".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(
        sim.asked.iter().all(|q| !q.contains("Type yes")),
        "{:?}",
        sim.asked
    );
    // With the passphrase, the yes line is asked after it.
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let pass = sim.asked.iter().position(|q| q.ends_with("Passphrase: "));
    let yes = sim.asked.iter().position(|q| q.contains("Type yes"));
    assert!(
        pass.is_some() && yes.is_some() && pass < yes,
        "{:?}",
        sim.asked
    );
    // A long answer.
    let long = format!("yes{}\n", " ".repeat(10_000));
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&[long.as_str()]);
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
}

// Codex ruling (M13): files of an earlier try that are in the way (`.new`, `.raw`, the unchecked
// rule, the unchecked public key) are named and never touched or removed: the install stops
// before any command.
#[test]
fn leftovers_of_an_earlier_try_are_named_and_left_alone() {
    for leftover in [
        "/etc/lunchroom/1000.console.key.new",
        "/etc/lunchroom/1000.console.key.raw",
        "/etc/lunchroom/1000.console.pub.new",
        "/etc/sudoers.d/lunchroom.unchecked",
        "/etc/sudoers.d/zz-lunchroom-1000.new",
    ] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.put(leftover, "root", "root", 0o600, Some("earlier"));
        let out = install_console(&home.env, &mut sim);
        assert_eq!(out.code, 1, "{leftover}: {}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains(leftover), "{leftover}: {}", out.stderr);
        assert_eq!(sim.commands().len(), 0, "{leftover}: {:?}", sim.log);
        assert_eq!(
            sim.node(leftover).and_then(|n| n.text.clone()).as_deref(),
            Some("earlier")
        );
        assert!(home.env.lunchroom_home.join("store/marker").exists());
    }
}

// Codex ruling 3 (H4): a memory protection that fails stops the install before any question, key or
// command (fail closed), and says so.
#[test]
fn a_memory_protection_that_fails_stops_the_install() {
    for plain in [false, true] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.protect_fails = true;
        let out = if plain {
            install_with_wall(&[], &home.env, &mut sim)
        } else {
            install_console(&home.env, &mut sim)
        };
        assert_eq!(out.code, 1, "{plain}: {}{}", out.stdout, out.stderr);
        assert!(
            out.stderr.contains("could not be made unreadable"),
            "{}",
            out.stderr
        );
        assert_eq!(sim.log.len(), 0, "{:?}", sim.log);
        assert_eq!(sim.asked.len(), 0, "{:?}", sim.asked);
        assert!(home.env.lunchroom_home.join("store/marker").exists());
    }
}

// Codex ruling 3 (M7): a file that grows between the measure and the copy is caught: the copy is
// measured as root before it is renamed in, and one larger than what was measured (plus a small
// allowance) is removed and stops the move.
#[test]
fn a_store_that_grows_after_it_was_measured_is_not_taken() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    sim.grow_on_cp = 10 * 1024 * 1024;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("more than the"), "{}", out.stderr);
    assert!(
        out.stderr.contains("measured before it was made"),
        "{}",
        out.stderr
    );
    // The copy was never renamed in, and is gone.
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
    assert!(sim.position("run-user").is_none());
    // Not grown: the same run passes.
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_some());
}

// F2 design, "Store wall", class 3 (a copy that differs from the tree held open and is put in
// place), replacing Codex ruling 3 (M7), whose copy through the descriptor path is class 1: root
// copies from the real path, so a path swapped after the look IS copied (the decoy), and the check
// against the list this process made of the tree it held stops the install before the move.
// Fails before the change: `cp` was given the descriptor path, copied the held tree, and the
// install went through (code 0).
#[test]
fn a_store_swapped_after_the_look_is_copied_from_its_path_and_refused_by_the_list() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    sim.swap_store_on_copy = true;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    // The decoy was copied: root read the path, not the descriptor.
    assert_eq!(sim.copied_marker.as_deref(), Some("decoy"), "{:?}", sim.log);
    let store = home.env.lunchroom_home.join("store").display().to_string();
    assert!(
        sim.log.iter().any(|l| l.contains(&format!(
            "/cp -R -P {store} /var/lib/lunchroom/1000/store.new"
        ))),
        "{:?}",
        sim.log
    );
    // And the check against the list refused it: nothing was put in place, the copy is gone.
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
    assert!(sim.position("run-user").is_none(), "{:?}", sim.log);
}

/// A completed install's log, run with the old store on disk in `home`.
fn finished_install() -> (Home, Sim) {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    (home, sim)
}

// F2 design, "Store wall", class 1: no root step is given a `/proc/<pid>/fd` path. The one word
// that names the program image (`Sim::source`) is replaced where the step runs
// (`terminal::pass_image`), so it is the only `/proc` word the plan may hold. Fails before the
// change: the `cp` of the store, of the records and of the waiting records named
// `/proc/<pid>/fd/<n>`.
#[test]
fn no_root_step_is_given_a_proc_fd_path_but_the_image_word() {
    let (_home, sim) = finished_install();
    let image = sim.source.display().to_string();
    let offending: Vec<&String> = sim
        .log
        .iter()
        .filter(|l| l.contains("/proc/") && !l.contains(&image))
        .collect();
    assert!(offending.is_empty(), "{offending:?}");
}

// F2 design, "Store wall", item 2: the old store and records are copied from their own paths, the
// ones the plan shows (`copy_store_steps`), nothing else. Fails before the change: the sources
// were descriptor paths (`/proc/<pid>/fd/<n>/.`, `-L /proc/<pid>/fd/<n>`).
#[test]
fn root_copies_from_the_real_paths_the_plan_shows() {
    let (home, sim) = finished_install();
    let lh = home.env.lunchroom_home.display().to_string();
    let run: Vec<&String> = sim.log.iter().filter(|l| l.contains("/cp ")).collect();
    assert_eq!(run.len(), 3, "{run:?}");
    for (copy, to) in [
        (format!("/cp -R -P {lh}/store"), "/store.new"),
        (
            format!("/cp -P {lh}/guard-records.jsonl"),
            "/guard-records.jsonl.new",
        ),
        (
            format!("/cp -R -P {lh}/guard-records.d"),
            "/guard-records.d.new",
        ),
    ] {
        assert!(
            run.iter().any(|l| l.contains(&copy) && l.ends_with(to)),
            "{copy}: {run:?}"
        );
    }
    // The plan the person is shown names the same sources.
    let shown = steps_text(&copy_store_steps(
        &Facts {
            old_store: Some(home.env.lunchroom_home.join("store")),
            old_records: Some(home.env.lunchroom_home.join("guard-records.jsonl")),
            old_records_dir: Some(home.env.lunchroom_home.join("guard-records.d")),
            ..facts()
        },
        &tool,
    ));
    for word in [
        format!("cp -R -P {lh}/store "),
        format!("cp -P {lh}/guard-records.jsonl "),
        format!("cp -R -P {lh}/guard-records.d "),
    ] {
        assert!(shown.contains(&word), "{word}: {shown}");
    }
}

// F2 design, "Store wall", item 2 and class 3: after each copy a root step runs `sha256sum -c
// --strict --quiet` in the copy's folder with the list this process made as its standard input
// (`<sha-256>  <path>`), and the copy is put in place only after it and the count of files. Fails
// before the change: there was no list and no such step (the copy was compared with `diff`).
#[test]
fn each_copy_is_checked_against_the_list_before_it_is_put_in_place() {
    let (_home, sim) = finished_install();
    let sha = |bytes: &[u8]| lunchroom_core::jcs::sha256_hex(bytes);
    let store = sim.position("/cp -R -P").unwrap_or(usize::MAX);
    let check = sim
        .position("env -C /var/lib/lunchroom/1000/store.new /usr/bin/sha256sum -c --strict --quiet /dev/stdin")
        .unwrap_or(usize::MAX);
    let count = sim
        .position("read-root /usr/bin/find /var/lib/lunchroom/1000/store.new ! -type d -printf .")
        .unwrap_or(usize::MAX);
    let put = sim
        .position("/usr/bin/mv /var/lib/lunchroom/1000/store.new /var/lib/lunchroom/1000/store")
        .unwrap_or(usize::MAX);
    assert!(
        store < check && check < count && count < put,
        "{store} {check} {count} {put}: {:?}",
        sim.log
    );
    // The lists, one per copy, as `sha256sum -c` reads them: hash, two spaces, relative path; a
    // single file is named as its copy is, beside its place.
    for line in [
        // The store's list holds the lock file too: the install makes `state.flock` when it locks
        // the store for the copy, so it is in the tree held open and in the copy alike.
        format!(
            "{}  marker\n{}  state.flock\n",
            sha(b"one queued message"),
            sha(b"")
        ),
        format!("{}  guard-records.jsonl.new\n", sha(b"")),
        format!("{}  waiting\n", sha(b"{}")),
    ] {
        assert!(
            sim.inputs.contains(&line),
            "{line:?} not in {:?}",
            sim.inputs
        );
    }
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_some());
}

// F2 design, "Store wall", class 3 (a file different from the list): the list check failing stops
// the install before the copy is put in place; the copy is removed; nothing after it runs. Fails
// before the change: no step runs `sha256sum -c`, so the install ended with code 0.
#[test]
fn a_list_that_does_not_match_the_copy_stops_before_the_move_into_place() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    sim.fail_once.push("sha256sum -c".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("stopped at step"), "{}", out.stderr);
    assert!(
        sim.position("/usr/bin/mv /var/lib/lunchroom/1000/store.new")
            .is_none(),
        "{:?}",
        sim.log
    );
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
    assert!(sim.position("run-user").is_none());
    assert!(home.env.lunchroom_home.join("store/marker").exists());
}

// F2 design, "Store wall", class 3 (a file added): the list check cannot see a file the list lacks,
// so root counts the copy's entries that are not folders and the install stops where there is one
// more than the list. Fails before the change: nothing counted the copy.
#[test]
fn a_file_added_to_the_copy_stops_the_install_before_the_move_into_place() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.lock_home = Some(home.env.lunchroom_home.clone());
    sim.extra_in_copy = 1;
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr
            .contains("files, the list of what was held open has"),
        "{}",
        out.stderr
    );
    assert!(
        sim.position("/usr/bin/mv /var/lib/lunchroom/1000/store.new")
            .is_none(),
        "{:?}",
        sim.log
    );
    assert!(sim.node("/var/lib/lunchroom/1000/store").is_none());
    assert!(sim.node("/var/lib/lunchroom/1000/store.new").is_none());
}

// F2 design, "Store wall", item 2 (what the list holds, from the held tree): every regular file
// at any depth, by relative path, with its sha-256; a link counts as another entry, a folder as
// nothing; a name with a newline or backslash is written as `sha256sum` reads it; an empty tree
// has an empty list, which gets no `sha256sum -c` step (it refuses an empty list) but still the
// count, of 0. Fails before the change: `list_folder`, `list_file`, `Listing` do not exist.
#[test]
fn the_list_is_every_regular_file_of_the_held_tree_with_its_sha_256() {
    let sha = |bytes: &[u8]| lunchroom_core::jcs::sha256_hex(bytes);
    let home = Home::new();
    let tree = home.root.join("tree");
    let _ = fs::create_dir_all(tree.join("a/b"));
    let _ = fs::write(tree.join("top"), "t");
    let _ = fs::write(tree.join("a/b/deep"), "d");
    let _ = fs::write(tree.join("a/odd\\name"), "o");
    let _ = fs::write(tree.join("a/new\nline"), "n");
    let _ = std::os::unix::fs::symlink("/etc/passwd", tree.join("link"));
    let listing = list_folder(&tree, &super::console_setup::LIMITS);
    assert!(listing.is_ok(), "{listing:?}");
    let Ok(listing) = listing else { return };
    assert_eq!(listing.others, 1, "the link");
    assert_eq!(listing.entries(), 5);
    let mut want = [
        format!("{}  top\n", sha(b"t")),
        format!("{}  a/b/deep\n", sha(b"d")),
        format!("\\{}  a/odd\\\\name\n", sha(b"o")),
        format!("\\{}  a/new\\nline\n", sha(b"n")),
    ];
    want.sort();
    assert_eq!(listing.text(None), want.concat());
    // A single file: named as its copy is.
    let _ = fs::write(home.root.join("one"), "x");
    let one = list_file(&home.root.join("one"), &super::console_setup::LIMITS);
    assert_eq!(
        one.map(|l| l.text(Some("one.new"))),
        Ok(format!("{}  one.new\n", sha(b"x")))
    );
    // The steps: the list is the check's input, the count is the list's.
    let source = Source {
        copy: "/o/store".to_owned(),
        follow: false,
        max_bytes: None,
        listing: Some(listing.clone()),
    };
    let steps = copy_store_steps_from(
        &Facts {
            old_records: None,
            old_records_dir: None,
            ..facts()
        },
        &tool,
        &Sources {
            store: Some(source.clone()),
            ..Sources::default()
        },
    );
    let check = steps
        .iter()
        .find(|s| s.argv.iter().any(|a| a.ends_with("/sha256sum")));
    assert_eq!(
        check.and_then(|s| s.input.clone()),
        Some(listing.text(None))
    );
    let count = steps.iter().find_map(|s| {
        s.verify
            .clone()
            .filter(|v| matches!(v, super::console_setup::Verify::Entries { .. }))
    });
    assert_eq!(
        count,
        Some(super::console_setup::Verify::Entries {
            path: "/var/lib/lunchroom/1000/store.new".to_owned(),
            expected: 5
        })
    );
    // Empty: no `sha256sum -c`, count of 0.
    let empty = Source {
        listing: Some(Listing::default()),
        ..source
    };
    let steps = copy_store_steps_from(
        &Facts {
            old_records: None,
            old_records_dir: None,
            ..facts()
        },
        &tool,
        &Sources {
            store: Some(empty),
            ..Sources::default()
        },
    );
    assert!(
        steps
            .iter()
            .all(|s| !s.argv.iter().any(|a| a.ends_with("/sha256sum"))),
        "{steps:?}"
    );
    assert!(steps.iter().any(|s| s.verify
        == Some(super::console_setup::Verify::Entries {
            path: "/var/lib/lunchroom/1000/store.new".to_owned(),
            expected: 0
        })));
}

// Codex ruling 3 (§17.8, A1.3): before the first passphrase there is no verifier, and the
// confirmation is the root program's own prompt (A1.3: "confirmed, before it writes anything, by
// that program's own password prompt with any authorization it remembers cleared first"), read
// after the A1.3 terminal check and before the `yes` line. Where that confirmation fails, the
// `yes` line is never asked; where an agent's process runs it, nothing is asked at all.
#[test]
fn before_the_first_passphrase_the_root_programs_prompt_comes_before_the_yes() {
    use crate::terminal::NO_VERIFIER;
    // The prompt refuses: no `yes` line, no rule, nothing set up.
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.verifier = Err(NO_VERIFIER.to_owned());
    sim.root_confirms = Err("the password was not given".to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert_eq!(sim.root_questions.len(), 1, "{:?}", sim.root_questions);
    assert!(
        sim.root_questions[0].contains("proves no person is present"),
        "{}",
        sim.root_questions[0]
    );
    assert!(
        sim.root_questions[0].contains("PASSWD: ALL"),
        "{}",
        sim.root_questions[0]
    );
    assert!(
        sim.asked.iter().all(|q| !q.contains("Type yes")),
        "{:?}",
        sim.asked
    );
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert!(
        sim.position("groupadd").is_none() && sim.position("visudo").is_none(),
        "{:?}",
        sim.log
    );
    // The prompt passes and `yes` is typed: the rule is written (the case A1.3 itself states).
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.verifier = Err(NO_VERIFIER.to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_some());
    let prompt = sim.root_questions.len();
    assert!(prompt >= 1);
    // An agent's process: the terminal check refuses first; the root prompt is never shown.
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.verifier = Err(NO_VERIFIER.to_owned());
    sim.ancestors = vec![(None, "claude".to_owned())];
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert_eq!(sim.root_questions.len(), 0);
    assert_eq!(sim.asked.len(), 0);
}

// Fable L7: an uninstall with no public key says it was not there, and does not say it removed it.
#[test]
fn uninstall_does_not_claim_to_remove_a_key_that_was_not_there() {
    let home = Home::new();
    let mut sim = Sim::new();
    let out = uninstall_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(!out.stdout.contains("removed "), "{}", out.stdout);
    assert!(out.stdout.contains("was not there"), "{}", out.stdout);
    assert_eq!(sim.commands().len(), 0);
}

// Fable L7: a login with a `.` goes through the whole install, and the rules name it by its id.
#[test]
fn a_login_with_a_dot_installs_and_is_named_by_its_id() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim.files.insert(
        "/etc/passwd".to_owned(),
        "john.smith:x:1000:1000::/home/u:/bin/bash\n".to_owned(),
    );
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let launch = sim
        .node("/etc/sudoers.d/lunchroom")
        .and_then(|n| n.text.clone());
    assert!(
        launch.is_some_and(|t| t.contains("\n#1000 ALL=(lunchroom) ")),
        "{:?}",
        sim.tree.get("/etc/sudoers.d/lunchroom")
    );
}

// §17.2: plain `lunchroom install` sets the wall up (group, user, program, store), hands the hooks to
// the installed program with the same arguments, moves the old store aside, and names the sessions
// to start anew; it makes no console key, no delivery folder and no rule file.
#[test]
fn plain_install_sets_the_wall_up_and_hands_the_hooks_to_the_program() {
    let home = Home::new();
    let mut sim = Sim::new();
    let args = ["claude-code".to_owned()];
    let out = install_with_wall(&args, &home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    for (path, owner, group, mode) in [
        (
            "/usr/local/lib/lunchroom/lunchroom",
            "root",
            "lunchroom",
            0o2755,
        ),
        ("/var/lib/lunchroom/1000", "root", "lunchroom", 0o2770),
    ] {
        assert_eq!(
            mode_of(&sim, path),
            Some((owner.to_owned(), group.to_owned(), mode)),
            "{path}"
        );
    }
    for absent in ["console.key", "console.pub", "approvals", "sudoers"] {
        assert!(sim.position(absent).is_none(), "{absent}: {:?}", sim.log);
    }
    assert!(
        sim.log.contains(
            &"run-user /usr/local/lib/lunchroom/lunchroom install claude-code".to_owned()
        ),
        "{:?}",
        sim.log
    );
    assert!(
        out.stdout
            .contains("Start every session of your agents anew"),
        "{}",
        out.stdout
    );
    assert!(
        home.env
            .lunchroom_home
            .join("store.moved-20261026T000000Z")
            .exists()
            || {
                // the stamp is of `now`; find any moved folder
                fs::read_dir(&home.env.lunchroom_home).is_ok_and(|d| {
                    d.flatten()
                        .any(|e| e.file_name().to_string_lossy().starts_with("store.moved-"))
                })
            }
    );
    assert!(!home.env.lunchroom_home.join("store").exists());
    // The hooks' install failing leaves the old store where it is.
    let home = Home::new();
    let mut sim = Sim::new();
    sim.user_install_ok = false;
    let out = install_with_wall(&args, &home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(home.env.lunchroom_home.join("store/marker").exists());
}

// §17.8: `lunchroom install` on an account with a wide rule sets the wall up anyway and says
// plainly that the wall does not hold until the rule is removed; on a narrow account it says
// nothing of the kind.
#[test]
fn plain_install_on_a_wide_account_sets_the_wall_up_and_prints_its_line() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE);
    let out = install_with_wall(&[], &home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout
            .contains("The store wall does not hold on this account until that rule is removed"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("(ALL) NOPASSWD: ALL"), "{}", out.stdout);
    assert!(sim.node("/var/lib/lunchroom/1000").is_some());
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert_ne!(sim.kept.len(), 0);
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(NARROW);
    let out = install_with_wall(&[], &home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(!out.stdout.contains("does not hold"), "{}", out.stdout);
}

// F2 design, "Finding and starting the console" item 1. The console setup that ends `install` is
// run by the installed (setgid) program's own install (`end_with_console`), the child that
// `finish_wall` starts, so the unpacked parent runs only that child, once, and never `install
// console` itself (a second run). Failure class: a console setup run twice by one install. Fails
// before this change: the parent's log holds a `run-user ... install console` entry. Not
// testable here: the in-process step needs `setgid::active()`, true only in the setgid program,
// which a stand-in cannot fake; it is covered by the pure decision and `console_is_set_up` tests.
#[test]
fn the_unpacked_parent_runs_the_hooks_child_and_not_the_console_setup() {
    for args in [vec![], vec!["claude-code".to_owned()]] {
        let home = Home::new();
        let mut sim = Sim::new().with_sudo(WIDE);
        let out = super::install(&args, &home.env, &mut sim);
        assert_eq!(out.code, 0, "{args:?}: {}{}", out.stdout, out.stderr);
        let child = format!(
            "run-user /usr/local/lib/lunchroom/lunchroom install {}",
            args.join(" ")
        );
        let runs: Vec<&String> = sim
            .log
            .iter()
            .filter(|l| l.starts_with("run-user "))
            .collect();
        assert_eq!(runs, vec![&child.trim_end().to_owned()], "{args:?}");
        // The wall and F2-AC30's line stay as before.
        assert!(sim.node("/var/lib/lunchroom/1000").is_some());
        assert!(
            out.stdout.contains("does not hold"),
            "{args:?}: {}",
            out.stdout
        );
    }
    // The wall's own install failing: the same, and a failure.
    let home = Home::new();
    let mut sim = Sim::new();
    sim.user_install_ok = false;
    let out = super::install(&[], &home.env, &mut sim);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
}

// §13::4 (cold first-time install, 2026-11-14): `lunchroom install` with no harness named and none
// found changes nothing, the store wall included, and says how to name one; with a harness named it
// goes on to the wall as before.
#[test]
fn an_install_with_no_harness_found_sets_nothing_up() {
    let home = Home::new();
    let _ = fs::remove_dir_all(home.env.home.join(".claude"));
    let mut sim = Sim::new();
    let out = super::install(&[], &home.env, &mut sim);
    assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("so nothing was changed"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("lunchroom install claude-code"),
        "{}",
        out.stderr
    );
    assert!(sim.log.is_empty(), "{:?}", sim.log);
    assert!(sim.node("/var/lib/lunchroom/1000").is_none());
    // Named: the wall comes first, as before.
    let mut sim = Sim::new();
    let _ = super::install(&["claude-code".to_owned()], &home.env, &mut sim);
    assert!(
        sim.node("/var/lib/lunchroom/1000").is_some(),
        "{:?}",
        sim.log
    );
}

// F2-AC38 (cold first-time install, 2026-11-15): the `sudo -n -l` answer kept at the end of a
// console set-up holds the console's own launch rule, so doctor sees no change right after it.
#[test]
fn the_kept_sudo_answer_holds_the_consoles_own_launch_rule() {
    let home = Home::new();
    let mut sim = Sim::new();
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let last = sim.kept.last().map(|k| k.own.clone()).unwrap_or_default();
    assert!(
        last.contains("(lunchroom) /usr/local/lib/lunchroom/lunchroom console"),
        "{:?}",
        sim.kept
    );
    // What doctor compares: the answer now and the one kept are the same rules.
    let now = sim.sudo_list().unwrap_or_default();
    assert!(sim.kept.last().is_some_and(|k| k.same_rules_as(&now)));
}

// F2 design item 1, exit codes (classes: a stopped console setup reported as exit 0; a stopped
// setup reported as "the hooks still run the old program", with the link and the old store left as
// they were; an install that failed for another reason reported as 6). The installed program exits
// 6 where its hooks are written but its console setup stopped: the unpacked program finishes as on
// 0 (the hooks, the start-anew line, F2-AC30's line) and exits 6; any other code is a failure, 1.
// Fails before the change: 6 was read as a failure ("did not finish", exit 1).
#[test]
fn a_console_that_stopped_gives_exit_6_and_the_install_still_finishes() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE);
    sim.user_install_code = Some(6);
    let out = super::install(&[], &home.env, &mut sim);
    let all = format!("{}{}", out.stdout, out.stderr);
    assert_eq!(out.code, 6, "{all}");
    assert!(
        all.contains("the hooks now run /usr/local/lib/lunchroom/lunchroom"),
        "{all}"
    );
    assert!(!all.contains("did not finish"), "{all}");
    assert!(
        all.contains(super::console_setup::SESSIONS_ANEW.trim()),
        "{all}"
    );
    assert!(all.contains("does not hold"), "{all}");
    for other in [1, 5, -1] {
        let home = Home::new();
        let mut sim = Sim::new();
        sim.user_install_code = Some(other);
        let out = super::install(&[], &home.env, &mut sim);
        let all = format!("{}{}", out.stdout, out.stderr);
        assert_eq!(out.code, 1, "{other}: {all}");
        assert!(all.contains("did not finish"), "{other}: {all}");
    }
}

// F2 design item 1, the decision at the end of the install (classes: a console not set up that is
// not set up by the install; a console already set up that is set up again). Fails before the
// change: `after_install` and `AfterInstall` do not exist.
#[test]
fn the_end_of_an_install_sets_the_console_up_only_where_it_is_not() {
    use super::{AfterInstall, after_install};
    assert_eq!(after_install(false), AfterInstall::RunSetup);
    assert_eq!(after_install(true), AfterInstall::SayOpen);
}

// F2 design item 1, the fact the end of the install rests on (class: a rerun that does not set the
// console up, or sets it up again). The console is set up exactly where both keys are there, read
// as the set-up reads them; one key, none, or the launch rule file alone is not. Fails before the
// change: `console_is_set_up` does not exist.
#[test]
fn the_console_is_set_up_only_where_both_keys_are_there() {
    let key = setgid::console_key(1000).display().to_string();
    let public = setgid::console_pub(1000).display().to_string();
    let home = Home::new();
    let mut sim = Sim::new();
    assert!(!console_is_set_up(&home.env, &mut sim));
    sim.put("/etc/sudoers.d/lunchroom", "root", "root", 0o440, None);
    assert!(
        !console_is_set_up(&home.env, &mut sim),
        "the rule file is not the sign"
    );
    sim.put(&key, "root", "lunchroom", 0o640, None);
    assert!(
        !console_is_set_up(&home.env, &mut sim),
        "a private key alone"
    );
    sim.put(&public, "root", "root", 0o644, None);
    assert!(console_is_set_up(&home.env, &mut sim));
    let mut sim = Sim::new();
    sim.put(&public, "root", "root", 0o644, None);
    assert!(
        !console_is_set_up(&home.env, &mut sim),
        "a public key alone"
    );
    // F2 design, "Store wall", "The console is set up": both keys and the launch rule MISSING
    // (the file system answers no such file) is not set up. Fails before the change: only the
    // keys were read, so this was true.
    let mut sim = Sim::new();
    sim.put(&key, "root", "lunchroom", 0o640, None);
    sim.put(&public, "root", "root", 0o644, None);
    assert!(
        !console_is_set_up(&home.env, &mut sim),
        "both keys, the launch rule missing"
    );
    sim.put("/etc/sudoers.d/lunchroom", "root", "root", 0o440, None);
    assert!(console_is_set_up(&home.env, &mut sim), "the rule put back");
}

// F2 design, "Store wall", "The console is set up": a refused look (the folder is 0750 root's) is
// not missing, so the keys decide. A stand-in that cannot tell (the default `missing`) never says
// missing. Fails before the change: `System::missing` does not exist, and the rule was not read.
#[test]
fn a_refused_look_at_the_launch_rule_leaves_the_keys_to_decide() {
    struct Refused(Sim);
    impl System for Refused {
        fn ancestors(&self) -> Vec<(Option<String>, String)> {
            self.0.ancestors()
        }
        fn own_tty(&self) -> u64 {
            self.0.own_tty()
        }
        fn tty_of(&self, key: &str) -> Option<u64> {
            self.0.tty_of(key)
        }
        fn uid(&self) -> Option<u32> {
            self.0.uid()
        }
        fn verifier(&self) -> Result<String, String> {
            self.0.verifier()
        }
        fn ask(&mut self, question: &str) -> Option<String> {
            self.0.ask(question)
        }
        fn ask_secret(&mut self, prompt: &str) -> Option<String> {
            self.0.ask_secret(prompt)
        }
        fn salt(&mut self) -> Result<[u8; lunchroom_core::passphrase::SALT_BYTES], String> {
            self.0.salt()
        }
        fn install_verifier(&mut self, line: &str) -> Result<(), String> {
            self.0.install_verifier(line)
        }
        fn program_source(&mut self) -> Result<crate::terminal::ProgramSource, String> {
            self.0.program_source()
        }
        fn read_text(&self, path: &Path) -> Option<String> {
            self.0.read_text(path)
        }
        fn exists(&self, path: &Path) -> bool {
            self.0.exists(path)
        }
        // `missing` is not given: the default is false, as for a look that was refused.
    }
    let key = setgid::console_key(1000).display().to_string();
    let public = setgid::console_pub(1000).display().to_string();
    let home = Home::new();
    let mut sim = Sim::new();
    sim.put(&key, "root", "lunchroom", 0o640, None);
    sim.put(&public, "root", "root", 0o644, None);
    assert!(console_is_set_up(&home.env, &mut Refused(sim)));
}

// §17.9, §17.8: uninstall removes the public key before any other part of the console, leaves the
// rule file and names it; the wrong passphrase removes nothing; an agent's process confirms
// nothing.
#[test]
fn uninstall_removes_the_public_key_first_and_names_the_rule_file() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_some());
    sim.log.clear();
    let out = uninstall_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(
        sim.commands(),
        vec!["/usr/bin/rm -f /etc/lunchroom/1000.console.pub"]
    );
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_none());
    assert!(sim.node("/etc/lunchroom/1000.console.key").is_some());
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_some());
    assert!(
        out.stdout
            .contains("/etc/sudoers.d/zz-lunchroom-1000 was left in place"),
        "{}",
        out.stdout
    );
    // A wrong passphrase removes nothing.
    let mut sim = Sim::new();
    sim.put(
        "/etc/lunchroom/1000.console.pub",
        "root",
        "root",
        0o644,
        Some("k\n"),
    );
    sim.secret = Some("not the passphrase".to_owned());
    let out = uninstall_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_some());
    assert_eq!(sim.commands().len(), 0);
    // An agent's process.
    let mut sim = Sim::new();
    sim.put(
        "/etc/lunchroom/1000.console.pub",
        "root",
        "root",
        0o644,
        Some("k\n"),
    );
    sim.ancestors = vec![(None, "claude".to_owned())];
    let out = uninstall_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    assert!(sim.node("/etc/lunchroom/1000.console.pub").is_some());
}

// §17.9: after uninstall then install, the console is back, with the pair that was kept.
#[test]
fn install_after_uninstall_brings_the_same_pair_back() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let public = sim.node("/etc/lunchroom/1000.console.pub").cloned();
    assert_eq!(uninstall_console(&home.env, &mut sim).code, 0);
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    assert_eq!(sim.node("/etc/lunchroom/1000.console.pub").cloned(), public);
}

// A successful install removes the record of an earlier refusal, before the public key.
#[test]
fn a_successful_install_removes_the_record_of_an_earlier_refusal() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["no\n"]);
    assert_eq!(install_console(&home.env, &mut sim).code, 3);
    assert!(sim.node("/etc/lunchroom/1000.console.state").is_some());
    let mut sim2 = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    sim2.tree = sim.tree.clone();
    sim2.files = sim.files.clone();
    let out = install_console(&home.env, &mut sim2);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(sim2.node("/etc/lunchroom/1000.console.state").is_none());
    let removed = sim2.position("rm -f /etc/lunchroom/1000.console.state");
    let public = sim2.position("1000.console.pub.new");
    assert!(removed.is_some() && removed < public, "{:?}", sim2.log);
}

// §17.5, §17.9, §17.8, A1.3: an agent's command that names any path the install makes or writes is
// DENIED, as a read, a write, a removal and as root; the program itself is the one path an agent
// may run.
#[test]
fn an_agents_command_naming_any_path_the_install_makes_is_denied() {
    use lunchroom_core::classify::Guard;
    use lunchroom_core::fs::OsFs;
    use lunchroom_core::paths::Layout;
    use lunchroom_core::policy::CallClass;
    let home = Home::new();
    let layout = Layout::for_home(&home.env.home);
    let fs = OsFs::new(std::time::Duration::from_secs(5));
    let mut paths: Vec<String> = Vec::new();
    let key = make_keys();
    let mut all = wall_steps(&facts(), &tool);
    all.extend(console_steps(&facts(), &tool, &key));
    all.extend(rule_steps(1000, &tool));
    for s in &all {
        paths.extend(
            s.argv
                .iter()
                .filter(|a| a.starts_with('/') && !a.starts_with("/usr/bin/"))
                .cloned(),
        );
    }
    for p in [
        "/etc/lunchroom/1000.verifier",
        "/etc/lunchroom/1000.console.state",
        "/var/lib/lunchroom-approvals/1000/requests/run-0123456789abcdef0123456789abcdef.plan.json",
        "/var/lib/lunchroom-approvals/1000/run-0123456789abcdef0123456789abcdef.plan.json",
    ] {
        paths.push(p.to_owned());
    }
    // Only Lunchroom's paths: not `/dev/stdin`, the old store (the user's own folder), the shell
    // of the system user, or the program, the one path an agent may run.
    paths.retain(|p| {
        p != setgid::PROGRAM
            && [
                "/etc/lunchroom",
                "/var/lib/lunchroom",
                "/usr/local/lib/lunchroom",
                "/etc/sudoers.d/",
            ]
            .iter()
            .any(|root| p.starts_with(root))
    });
    paths.sort();
    paths.dedup();
    assert!(paths.len() >= 15, "{paths:?}");
    let guard = |held| Guard {
        layout: &layout,
        fs: &fs,
        held,
    };
    for p in &paths {
        for cmd in [
            format!("cat {p}"),
            format!("rm -f {p}"),
            format!("echo x > {p}"),
            format!("sudo cat {p}"),
            format!("sudo rm {p}"),
            format!("cp /bin/sh {p}"),
        ] {
            for held in [false, true] {
                let judged = guard(held).judge(
                    "Bash",
                    &serde_json::json!({ "command": cmd }),
                    &home.env.home.join("project"),
                );
                assert!(
                    matches!(judged.call.folded(), CallClass::Denied(_)),
                    "held={held}: {cmd}: {:?}",
                    judged.call.folded()
                );
            }
        }
        // The file tools too.
        for (tool, held) in [("Read", true), ("Write", false), ("Edit", false)] {
            let judged = guard(held).judge(
                tool,
                &serde_json::json!({ "file_path": p, "content": "x", "old_string": "a", "new_string": "b" }),
                &home.env.home.join("project"),
            );
            let denied = matches!(judged.call.folded(), CallClass::Denied(_));
            // A read of a path outside the store folders is not DENIED unless held; the store
            // paths are, in every state.
            assert!(denied, "{tool} {p}: {:?}", judged.call.folded());
        }
    }
}

// The primary group of the account is the group of the delivery folder, by name from /etc/group.
#[test]
fn the_delivery_folder_takes_the_developers_primary_group() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.files.insert(
        "/etc/passwd".to_owned(),
        "dev:x:1000:2000:Dev:/home/dev:/bin/bash\n".to_owned(),
    );
    sim.files.insert(
        "/etc/group".to_owned(),
        "staff:x:2000:\ndev:x:1000:\n".to_owned(),
    );
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(
        mode_of(&sim, "/var/lib/lunchroom-approvals/1000"),
        Some(("lunchroom".to_owned(), "staff".to_owned(), 0o2750))
    );
}

// 17::12: nothing the install runs puts anyone in the group `lunchroom`; the developer
// has no membership in it after the install, and the system user is made with it as its group only.
#[test]
fn nothing_puts_the_user_in_the_group_lunchroom() {
    let home = Home::new();
    let mut sim = Sim::new();
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    for line in &sim.log {
        for banned in [
            "usermod", "gpasswd", "adduser", "addgroup", "-G ", "--groups",
        ] {
            assert!(!line.contains(banned), "{banned}: {line}");
        }
    }
    let group = sim.files.get("/etc/group").cloned().unwrap_or_default();
    let members = group
        .lines()
        .find(|l| l.starts_with("lunchroom:"))
        .and_then(|l| l.split(':').nth(3))
        .unwrap_or("x");
    assert!(members.is_empty(), "{group}");
}

// 17::26: the install that moves the store names the sessions to start anew, both
// `install` and `install console`.
#[test]
fn both_installs_name_the_sessions_to_start_anew() {
    let home = Home::new();
    let mut sim = Sim::new();
    let out = install_console(&home.env, &mut sim);
    assert!(
        out.stdout
            .contains("Start every session of your agents anew"),
        "{}",
        out.stdout
    );
    let home = Home::new();
    let mut sim = Sim::new();
    let out = install_with_wall(&[], &home.env, &mut sim);
    assert!(
        out.stdout
            .contains("Start every session of your agents anew"),
        "{}",
        out.stdout
    );
}

// 17::75: the kept answer holds the developer's `sudo -n -l` of that time together with
// the rules read, in the root step, for the other user a password-free rule runs as; and that
// other user's wide rule makes the developer's rule wide (the question is asked), a narrow one not.
#[test]
fn the_kept_answer_holds_the_other_users_rules_with_the_developers() {
    let own = "User dev may run the following commands on host:\n    (svc) NOPASSWD: ALL\n";
    let narrow =
        "User svc may run the following commands on host:\n    (root) NOPASSWD: /usr/bin/id\n";
    let wide = "User svc may run the following commands on host:\n    (root) NOPASSWD: /usr/bin/systemctl is-active *\n";
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(own);
    sim.others.insert("svc".to_owned(), narrow.to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        sim.asked.iter().all(|q| q.ends_with("Passphrase: ")),
        "{:?}",
        sim.asked
    );
    let kept = sim.kept.first().cloned().unwrap_or_default();
    assert_eq!(kept.own, own);
    assert_eq!(kept.others.get("svc").map(String::as_str), Some(narrow));
    assert!(sim.position("list-user svc").is_some());
    // The same account, the service account holding a wide rule: the question is asked.
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(own).answering(&["no"]);
    sim.others.insert("svc".to_owned(), wide.to_owned());
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 3, "{}{}", out.stdout, out.stderr);
    let kept = sim.kept.first().cloned().unwrap_or_default();
    assert_eq!(kept.others.get("svc").map(String::as_str), Some(wide));
}

// 17::90, 17::91, 17::92, 17::93: the rule file is new, its name holds no `.` and starts
// with `zz-`, and while `visudo -c -f` checks the temporary file that file is root's, mode 0440,
// holds the one rule and has a `.` in its name.
#[test]
fn the_rule_file_is_new_named_without_a_dot_and_checked_as_root_0440() {
    let home = Home::new();
    let mut sim = Sim::new().with_sudo(WIDE).answering(&["yes"]);
    assert!(sim.node("/etc/sudoers.d/zz-lunchroom-1000").is_none());
    assert_eq!(install_console(&home.env, &mut sim).code, 0);
    let name = setgid::rule_file(1000);
    let name = name
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    assert!(!name.contains('.') && name.starts_with("zz-"), "{name}");
    let first = sim.checked.first().cloned();
    assert_eq!(
        first.map(|n| (n.owner, n.group, n.mode, n.text)),
        Some((
            "root".to_owned(),
            "root".to_owned(),
            0o440,
            Some("#1000 ALL=(ALL:ALL) PASSWD: ALL\n".to_owned())
        ))
    );
    assert!(super::console_setup::rule_unchecked(1000).contains(".new"));
}

// An account whose name sudoers cannot take as written, or whose files cannot be read, stops the
// install before anything is done.
#[test]
fn a_bad_account_name_or_unreadable_account_files_stop_the_install() {
    let home = Home::new();
    let mut sim = Sim::new();
    sim.files.insert(
        "/etc/passwd".to_owned(),
        "de\u{7}v:x:1000:1000::/h:/bin/sh\n".to_owned(),
    );
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1);
    assert_eq!(sim.log.len(), 0);
    let mut sim = Sim::new();
    sim.files.remove("/etc/group");
    let out = install_console(&home.env, &mut sim);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(sim.log.len(), 0);
}

/// Every file and folder under `dir`: path relative to `dir`, whether it is a folder, its bytes
/// (files) and its modification time (files).
fn old_folder_state(
    dir: &Path,
) -> BTreeMap<String, (bool, Vec<u8>, Option<std::time::SystemTime>)> {
    fn walk(
        base: &Path,
        at: &Path,
        out: &mut BTreeMap<String, (bool, Vec<u8>, Option<std::time::SystemTime>)>,
    ) {
        for entry in fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .map_or_else(|_| String::new(), |p| p.to_string_lossy().into_owned());
            let meta = fs::symlink_metadata(&path).ok();
            let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
            if is_dir {
                out.insert(rel, (true, Vec::new(), None));
                walk(base, &path, out);
            } else {
                let mtime = meta.and_then(|m| m.modified().ok());
                out.insert(rel, (false, fs::read(&path).unwrap_or_default(), mtime));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

// F2-AC9: "When the install has finished, Lunchroom shall write nothing in the old folder."
// Observed: the real old folder (`~/.lunchroom` under the test home) is read file by file (path,
// bytes, mtime) once the install has returned and again after a pause. The Sim does not run the
// setgid program's own `install`, so this shows what the wall set-up does to the old folder, not
// what the hooks install does. Asserted: (1) the folder holds only the three names moved aside
// and the records lock the install made while it held the old store (no `store`, records file or
// records folder made again, nothing else new); (2) every file that was there before is, under
// its moved name, byte for byte and mtime for mtime what it was (nothing in it was rewritten);
// (3) a second reading after a pause is identical (nothing writes once the install returned).
#[test]
fn nothing_is_written_in_the_old_folder_once_the_install_has_finished() {
    let home = Home::new();
    let old = home.env.lunchroom_home.clone();
    let before = old_folder_state(&old);
    assert!(before.contains_key("store/marker"), "{before:?}");
    let mut sim = Sim::new();
    let args = ["claude-code".to_owned()];
    let out = install_with_wall(&args, &home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let after = old_folder_state(&old);
    let top: Vec<&str> = after
        .keys()
        .filter(|k| !k.contains('/'))
        .map(String::as_str)
        .collect();
    for name in &top {
        assert!(
            name.starts_with("store.moved-")
                || name.starts_with("guard-records.jsonl.moved-")
                || name.starts_with("guard-records.d.moved-")
                || *name == "guard-records.flock",
            "unexpected {name:?} in the old folder: {top:?}"
        );
    }
    for gone in ["store", "guard-records.jsonl", "guard-records.d"] {
        assert!(!after.contains_key(gone), "{gone} was made again: {top:?}");
    }
    for (path, (is_dir, bytes, mtime)) in &before {
        let (head, tail) = path
            .split_once('/')
            .map_or((path.as_str(), ""), |(h, t)| (h, t));
        let moved = after
            .keys()
            .find(|k| {
                k.split('/')
                    .next()
                    .is_some_and(|first| first.starts_with(&format!("{head}.moved-")))
            })
            .and_then(|k| k.split('/').next())
            .unwrap_or_default()
            .to_owned();
        let at = if tail.is_empty() {
            moved
        } else {
            format!("{moved}/{tail}")
        };
        let got = after.get(&at);
        assert_eq!(
            got.map(|g| (g.0, &g.1, g.2)),
            Some((*is_dir, bytes, *mtime)),
            "{path} -> {at} changed"
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        old_folder_state(&old),
        after,
        "the old folder changed after the install"
    );
}

// F2-AC10: "While the store is under the wall, Lunchroom shall carry out `lunchroom send`, the
// guard and `lunchroom status` through the program." The guard runs from the hooks (tested in
// install.rs, `hooks_for_the_installed_program_run_it_at_its_new_path`); `lunchroom send` and
// `lunchroom status` are run by name, so they reach the program through the `lunchroom` link in
// `~/.local/bin` and `~/bin`. Asserted: after the install both links that led to the earlier
// program lead to the setgid program, and a link that led elsewhere is left alone. That the
// kernel then gives the process its group needs a real setgid install (Demonstration).
#[test]
fn the_lunchroom_links_lead_to_the_program_once_the_wall_is_up() {
    use std::os::unix::fs::symlink;
    let home = Home::new();
    let earlier = home.env.lunchroom_home.join("bin/lunchroom");
    let program = PathBuf::from(setgid::PROGRAM);
    for dir in [".local/bin", "bin"] {
        let _ = fs::create_dir_all(home.env.home.join(dir));
    }
    let local = home.env.home.join(".local/bin/lunchroom");
    let plain = home.env.home.join("bin/lunchroom");
    assert!(symlink(&earlier, &local).is_ok());
    assert!(symlink("/opt/elsewhere/lunchroom", &plain).is_ok());
    let mut sim = Sim::new();
    let args = ["claude-code".to_owned()];
    let out = install_with_wall(&args, &home.env, &mut sim);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(fs::read_link(&local).ok(), Some(program.clone()));
    assert_eq!(
        fs::read_link(&plain).ok(),
        Some(PathBuf::from("/opt/elsewhere/lunchroom"))
    );
    assert!(
        out.stdout.contains(&format!(
            "linked {} to {}",
            local.display(),
            program.display()
        )),
        "{}",
        out.stdout
    );
}
