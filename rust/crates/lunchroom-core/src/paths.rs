//! Which of Lunchroom's path rules a path falls under.
//!
//! Spec: §11 L1.1 (paths a HELD session may not read, credential paths), §6 P1.4 (writing Lunchroom's
//! stores is DENIED; writing Lunchroom's program or the harness configuration that runs the guard is
//! PROTECTED). The rules match path segments as well as the user's own home, so a path written
//! inside code (`HOME/.ssh/x`) or another home is judged the same way.

use std::path::{Component, Path, PathBuf};

/// Where Lunchroom and the harnesses keep their files.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The user's home folder.
    pub home: PathBuf,
    /// Lunchroom's own folder: its stores, records and configuration (`~/.lunchroom`).
    pub lunchroom_home: PathBuf,
    /// Where Lunchroom's program lives (`~/.lunchroom/bin`).
    pub program_dir: PathBuf,
    /// Other places Lunchroom's program is installed.
    pub programs: Vec<PathBuf>,
    /// Transcript and session stores a harness keeps outside the usual places (`CODEX_HOME`,
    /// `HERMES_HOME`, `OPENCLAW_STATE_DIR`, `CLAUDE_CONFIG_DIR`): folders or files, each with
    /// everything below it.
    pub transcript_roots: Vec<PathBuf>,
    /// Harness configuration that runs the guard, kept outside the usual places: the same
    /// variables, `OPENCLAW_CONFIG_PATH` and a Claude Code `--settings` file.
    pub guard_config_roots: Vec<PathBuf>,
    /// The terminal devices that are the agent's own (1.1): the paths its controlling terminal has
    /// (`/dev/pts/3`). Every other terminal device is another process's.
    pub own_terminals: Vec<PathBuf>,
    /// The harness process the agent's commands run under (1.1): the processes in its subtree are
    /// the agent's own; no other process's `/proc` secrets may be read.
    pub agent_root: Option<u32>,
    /// The console's process id, from its liveness file (1.1), when it runs.
    pub console_pid: Option<u32>,
    /// The agent's own tmux pane (`TMUX_PANE`, `%3`) and screen session (`STY`), when it has them.
    pub own_pane: Option<String>,
    pub own_screen: Option<String>,
}

impl Layout {
    /// The default layout for a home folder.
    #[must_use]
    pub fn for_home(home: &Path) -> Self {
        let lunchroom_home = home.join(".lunchroom");
        Self {
            home: home.to_path_buf(),
            program_dir: lunchroom_home.join("bin"),
            lunchroom_home,
            programs: Vec::new(),
            transcript_roots: Vec::new(),
            guard_config_roots: Vec::new(),
            own_terminals: Vec::new(),
            agent_root: None,
            console_pid: None,
            own_pane: None,
            own_screen: None,
        }
    }

    /// Codex's folder when it is not `~/.codex` (`CODEX_HOME`): its session stores and history are
    /// transcripts, and the folder (`hooks.json`, `config.toml`) is the configuration that runs
    /// the guard.
    pub fn add_codex_home(&mut self, dir: &Path) {
        for part in ["sessions", "archived_sessions", "history.jsonl"] {
            push_unique(&mut self.transcript_roots, dir.join(part));
        }
        push_unique(&mut self.guard_config_roots, dir.to_path_buf());
    }

    /// Hermes's folder when it is not `~/.hermes` (`HERMES_HOME`), as `~/.hermes` is judged.
    pub fn add_hermes_home(&mut self, dir: &Path) {
        push_unique(&mut self.transcript_roots, dir.to_path_buf());
        push_unique(&mut self.guard_config_roots, dir.to_path_buf());
    }

    /// OpenClaw's state folder when it is not `~/.openclaw` (`OPENCLAW_STATE_DIR`), as
    /// `~/.openclaw` is judged.
    pub fn add_openclaw_state(&mut self, dir: &Path) {
        push_unique(&mut self.transcript_roots, dir.to_path_buf());
        push_unique(&mut self.guard_config_roots, dir.to_path_buf());
    }

    /// OpenClaw's configuration file when it is not in the state folder (`OPENCLAW_CONFIG_PATH`).
    pub fn add_openclaw_config(&mut self, file: &Path) {
        push_unique(&mut self.guard_config_roots, file.to_path_buf());
    }

    /// Claude Code's folder when it is not `~/.claude` (`CLAUDE_CONFIG_DIR`): its projects and
    /// history are transcripts, and the folder (`settings.json`, `.claude.json`) is configuration.
    pub fn add_claude_config_dir(&mut self, dir: &Path) {
        for part in ["projects", "history.jsonl"] {
            push_unique(&mut self.transcript_roots, dir.join(part));
        }
        push_unique(&mut self.guard_config_roots, dir.to_path_buf());
    }

    /// A settings file given to Claude Code with `--settings`.
    pub fn add_claude_settings(&mut self, file: &Path) {
        push_unique(&mut self.guard_config_roots, file.to_path_buf());
    }

    /// The facts about one absolute path.
    #[must_use]
    pub fn facts(&self, p: &Path) -> Facts {
        let segs = segments(p);
        let in_lunchroom = p.starts_with(&self.lunchroom_home);
        let in_program = p.starts_with(&self.program_dir) || self.programs.iter().any(|q| p == q);
        let (seg_store, seg_program) = lunchroom_segments(&segs);
        // The console's wall holds every path in its system folders but the setgid program itself
        // (which `system_store` leaves out): a file planted beside the program is the store's
        // kind, DENIED, even where that folder is also the program folder of this layout.
        // `/var/lib/lunchroom` and the program folder are 1.1 only (feature `console`): 1.1.0
        // ships the console off, and they are not Lunchroom's stores there. `/etc/lunchroom`, the
        // passphrase checks, is a store in every build (L1.1, K1.11).
        let system = system_store(p);
        Facts {
            store: (in_lunchroom && !in_program) || seg_store || system,
            program: in_program || seg_program || p == Path::new(crate::setgid::PROGRAM),
            terminal: terminal_device(p) && !self.own_terminals.iter().any(|t| t == p),
            secret_pid: process_secret_pid(p, &segs),
            transcript: in_process_folder(p, &segs)
                || TRANSCRIPT_SEQS.iter().any(|s| has_seq(&segs, s))
                || self.transcript_roots.iter().any(|r| p.starts_with(r)),
            credential: is_credential(&segs),
            harness_program: is_harness_program(&segs),
            guard_config: GUARD_CONFIG_HOME
                .iter()
                .any(|r| p.starts_with(self.home.join(r)))
                || p.starts_with("/etc/claude-code")
                || self.guard_config_roots.iter().any(|r| p.starts_with(r))
                || segs.iter().any(|s| s == ".codex")
                || segs.windows(2).any(|w| {
                    w[0] == ".claude" && (w[1] == "settings.json" || w[1] == "settings.local.json")
                }),
        }
    }

    /// A search from `dir` would reach a path a HELD session may not read (L1.1).
    #[must_use]
    pub fn denied_below(&self, dir: &Path) -> bool {
        let mut roots: Vec<PathBuf> = TRANSCRIPT_SEQS
            .iter()
            .map(|s| s.iter().fold(self.home.clone(), |acc, part| acc.join(part)))
            .collect();
        roots.push(PathBuf::from("/proc"));
        roots.push(self.lunchroom_home.clone());
        roots.extend(self.transcript_roots.iter().cloned());
        roots.iter().any(|r| r.starts_with(dir))
    }

    /// A search from `dir` would reach one of the user's credential folders (L1.1).
    #[must_use]
    pub fn credential_root_below(&self, dir: &Path) -> bool {
        CREDENTIAL_HOME_ROOTS
            .iter()
            .any(|r| self.home.join(r).starts_with(dir))
    }
}

fn push_unique(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
}

/// What one path is, for Lunchroom's rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent facts about one path, read separately
pub struct Facts {
    /// Lunchroom's folder, its stores and records: everything in it but its program.
    pub store: bool,
    /// Lunchroom's program.
    pub program: bool,
    /// A terminal device (`/dev/pts/N`, `/dev/ttyN`, `/dev/vcs*`) that is not the agent's own
    /// controlling terminal (1.1). `/dev/tty` names the caller's own and is never one.
    pub terminal: bool,
    /// `/proc/<pid>/{mem,environ,fd,maps}` (or below): the process id, which the caller compares
    /// with the agent's own subtree (1.1). `/proc/self/...` is the command's own process and none.
    pub secret_pid: Option<u32>,
    /// A harness transcript or session store, or a process's folder in `/proc` (L1.1).
    pub transcript: bool,
    /// A credential path (L1.1).
    pub credential: bool,
    /// Harness configuration that runs the guard (P1.4, R3.1).
    pub guard_config: bool,
    /// A harness's own program: its executable, version folder or package folder. It decides what
    /// runs the guard, so writing it is guard configuration (P1.4, R3.1); running it is not.
    pub harness_program: bool,
}

/// Harness transcript and session stores, as segment sequences (L1.1; the spec's list plus the
/// other transcript files the same harnesses keep).
const TRANSCRIPT_SEQS: [&[&str]; 8] = [
    &[".claude", "projects"],
    &[".claude", "history.jsonl"],
    &[".codex", "sessions"],
    &[".codex", "archived_sessions"],
    &[".codex", "history.jsonl"],
    &[".openclaw"],
    &[".hermes"],
    &[".local", "share", "opencode"],
];

/// Home folders and files that configure a harness (the guard's hooks, plugins, trust records) or
/// decide what its name runs: the shell's startup files, which the name is looked up through.
const GUARD_CONFIG_HOME: [&str; 14] = [
    ".claude",
    ".claude.json",
    ".codex",
    ".openclaw",
    ".hermes",
    ".config/opencode",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshenv",
    ".zshrc",
    ".zprofile",
    // The whole folder: `functions/claude.fish` and `conf.d/` change what a name runs as
    // `config.fish` does.
    ".config/fish",
];

/// The harnesses' program names, and the runtimes they run under (OpenClaw, an npm Claude Code and
/// Codex's launcher under `node`, Hermes under `python3`).
const HARNESS_PROGRAMS: [&str; 8] = [
    "claude", "codex", "openclaw", "hermes", "opencode", "node", "python", "python3",
];

/// The harnesses' npm packages (Codex, Claude Code, OpenClaw and `opencode`), as the segments
/// below a `node_modules` folder.
const HARNESS_PACKAGES: [&[&str]; 5] = [
    &["@openai", "codex"],
    &["@anthropic-ai", "claude-code"],
    &["openclaw"],
    &["opencode-ai"],
    &["opencode"],
];

/// A harness's own program, wherever it is installed: a file named for it in a `bin` folder
/// (`~/.local/bin/claude`, `~/.npm-global/bin/codex`), its package folder below a `node_modules`
/// folder, or Claude Code's versions folder (`~/.local/share/claude`, where the link in `bin`
/// ends).
fn is_harness_program(segs: &[String]) -> bool {
    segs.windows(2)
        .any(|w| w[0] == "bin" && HARNESS_PROGRAMS.contains(&w[1].as_str()))
        || has_seq(segs, &[".local", "share", "claude"])
        || segs.iter().enumerate().any(|(k, s)| {
            s == "node_modules"
                && HARNESS_PACKAGES.iter().any(|pkg| {
                    segs.get(k + 1..k + 1 + pkg.len())
                        .is_some_and(|w| w.iter().zip(*pkg).all(|(a, b)| a == b))
                })
        })
}

/// Credential folders in the user's home (L1.1).
const CREDENTIAL_HOME_ROOTS: [&str; 3] = [".ssh", ".aws", ".config/gcloud"];

/// A process's own folder in `/proc` (`/proc/1234`, `/proc/self`, `/proc/thread-self`) or a path
/// below it: its `environ`, `cmdline`, `fd` and `mem` hold another session's data. `/proc/loadavg`
/// and the other system files do not.
fn in_process_folder(p: &Path, segs: &[String]) -> bool {
    p.starts_with("/proc")
        && segs.get(1).is_some_and(|s| {
            matches!(s.as_str(), "self" | "thread-self")
                || (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// The folders and files of the console's wall: the passphrase checks (`/etc/lunchroom`), in every
/// build (they exist from 1.1.0, A1.3, and a HELD agent's read of one is DENIED, L1.1); and, with
/// the console (1.1, feature `console`) only, the store folders (`/var/lib/lunchroom`) and
/// everything in the program folder except the program itself.
fn system_store(p: &Path) -> bool {
    p.starts_with("/etc/lunchroom")
        || (cfg!(feature = "console")
            && (p.starts_with(crate::setgid::STORE_ROOT)
                || (p.starts_with(crate::setgid::PROGRAM_DIR)
                    && p != Path::new(crate::setgid::PROGRAM))))
}

/// A terminal device other than `/dev/tty` itself: `/dev/pts/N`, `/dev/ttyN`, `/dev/ttySN`,
/// `/dev/vcs`, `/dev/vcsN`, `/dev/vcsaN`. The folder `/dev/pts` alone is not one.
fn terminal_device(p: &Path) -> bool {
    if p.starts_with("/dev/pts") {
        return p != Path::new("/dev/pts");
    }
    p.parent() == Some(Path::new("/dev"))
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| (n.starts_with("tty") && n != "tty") || n.starts_with("vcs"))
}

/// The process id in `/proc/<pid>/{mem,environ,fd,maps}` or below one of them.
fn process_secret_pid(p: &Path, segs: &[String]) -> Option<u32> {
    if !p.starts_with("/proc") {
        return None;
    }
    let pid = segs.get(1)?.parse::<u32>().ok()?;
    segs.get(2)
        .is_some_and(|s| matches!(s.as_str(), "mem" | "environ" | "fd" | "maps"))
        .then_some(pid)
}

/// The paths a controlling terminal with the number `tty_nr` (field 7 of `/proc/<pid>/stat`) has:
/// `/dev/pts/N` for a pseudo-terminal, `/dev/ttyN` for a virtual console, `/dev/ttySN` for a serial
/// line; none for 0 (no terminal) or a device not known here.
#[must_use]
pub fn terminal_paths(tty_nr: u64) -> Vec<PathBuf> {
    if tty_nr == 0 {
        return Vec::new();
    }
    let major = (tty_nr >> 8) & 0xfff;
    let minor = (tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00);
    match major {
        136..=143 => vec![PathBuf::from(format!(
            "/dev/pts/{}",
            (major - 136) * 256 + minor
        ))],
        4 if minor < 64 => vec![PathBuf::from(format!("/dev/tty{minor}"))],
        4 => vec![PathBuf::from(format!("/dev/ttyS{}", minor - 64))],
        _ => Vec::new(),
    }
}

/// The normal components of a path, as text.
fn segments(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn has_seq(segs: &[String], seq: &[&str]) -> bool {
    segs.windows(seq.len())
        .any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
}

/// (store, program) judged on a `.lunchroom` segment anywhere in the path. A folder linked to
/// Lunchroom's under another name is found by its real path.
fn lunchroom_segments(segs: &[String]) -> (bool, bool) {
    let mut store = false;
    let mut program = false;
    for (k, s) in segs.iter().enumerate() {
        if s == ".lunchroom" {
            match segs.get(k + 1) {
                Some(next) if next == "bin" => program = true,
                Some(_) | None => store = true,
            }
        }
    }
    (store, program)
}

/// Endings of a file that stands in for a credential file (`.env.example`) and holds none.
const TEMPLATE_ENDINGS: [&str; 4] = [".example", ".sample", ".template", ".dist"];

/// L1.1's credential paths: `.env*`, `*.pem`, `*.key`, `id_rsa*`, `.npmrc`, and anything in `.ssh`,
/// `.aws` or `.config/gcloud`; a file named as a template of one is not.
#[must_use]
pub fn is_credential_name(name: &str) -> bool {
    let ext = Path::new(name).extension();
    !TEMPLATE_ENDINGS.iter().any(|e| name.ends_with(e))
        && (name.starts_with(".env")
            || ext.is_some_and(|e| e.eq_ignore_ascii_case("pem") || e.eq_ignore_ascii_case("key"))
            || name.starts_with("id_rsa")
            || name == ".npmrc")
}

fn is_credential(segs: &[String]) -> bool {
    segs.last().is_some_and(|n| is_credential_name(n))
        || segs.iter().any(|s| s == ".ssh" || s == ".aws")
        || has_seq(segs, &[".config", "gcloud"])
}

/// Makes `p` absolute without touching its parts: a leading `~` is the home folder and a relative
/// path is taken from `cwd`; `.` and `..` stay where they were written. The operating system
/// resolves `..` after following the links before it, so a check of what a path opens starts from
/// this form and resolves it with the file system (`Fs::resolve`), never from [`normalize`].
#[must_use]
pub fn absolute(p: &str, cwd: &Path, home: &Path) -> PathBuf {
    if p == "~" {
        home.to_path_buf()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else {
        cwd.join(p)
    }
}

/// Makes `p` absolute and lexically normal: as [`absolute`], then `.` is dropped and `..` removes
/// the previous part. Symlinks are not followed, so `link/..` is the folder holding `link`, which
/// is what a shell's `cd` makes of it but not what a file open reaches: use it for a working
/// folder the shell tracks, and `Fs::resolve` for any path whose file is checked.
#[must_use]
pub fn normalize(p: &str, cwd: &Path, home: &Path) -> PathBuf {
    let joined = absolute(p, cwd, home);
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The folders of a wildcard path before its first wildcard: every match lies under it.
#[must_use]
pub fn literal_prefix(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for part in segments(p) {
        if part.contains(['*', '?', '[']) {
            break;
        }
        out.push(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::for_home(Path::new("/home/u"))
    }

    // §11 L1.1: Lunchroom's stores, transcripts and /proc are the denied reads; the program is not.
    #[test]
    fn l1_denied_read_paths() {
        let l = layout();
        for p in [
            "/home/u/.lunchroom/inbox/x",
            "/home/u/.lunchroom/registry.json",
            "/home/other/.lunchroom/delivered/y",
        ] {
            assert!(l.facts(Path::new(p)).store, "{p}");
        }
        for p in [
            "/home/u/.claude/projects/a/b.jsonl",
            "/home/u/.codex/sessions/2026/x.jsonl",
            "/home/u/.openclaw/agents/x",
            "/home/u/.hermes/state.db",
            "/proc/self/environ",
            "/proc/1234/cmdline",
            "/proc/thread-self/fd/0",
            "/proc/42",
            "/srv/HOME/.claude/projects/x",
        ] {
            assert!(l.facts(Path::new(p)).transcript, "{p}");
        }
        // Only a process's own folder holds another session's data.
        for p in [
            "/proc",
            "/proc/loadavg",
            "/proc/meminfo",
            "/proc/cpuinfo",
            "/proc/uptime",
            "/proc/stat",
            "/proc/version",
            "/proc/sys/kernel/hostname",
            "/proc/12a/x",
        ] {
            assert!(!l.facts(Path::new(p)).transcript, "{p}");
        }
        let prog = l.facts(Path::new("/home/u/.lunchroom/bin/lunchroom"));
        assert!(prog.program && !prog.store);
        assert!(l.facts(Path::new("/home/u/.lunchroom")).store);
        assert!(!l.facts(Path::new("/home/u/project/src/main.rs")).transcript);
    }

    // 1.1: the paths a controlling terminal number has, from the kernel's encoding of the device.
    #[test]
    fn terminal_numbers_name_their_devices() {
        let nr = |major: u64, minor: u64| (major << 8) | (minor & 0xff) | ((minor & !0xff) << 12);
        assert_eq!(terminal_paths(0), Vec::<PathBuf>::new());
        assert_eq!(
            terminal_paths(nr(136, 3)),
            vec![PathBuf::from("/dev/pts/3")]
        );
        assert_eq!(
            terminal_paths(nr(136, 255)),
            vec![PathBuf::from("/dev/pts/255")]
        );
        assert_eq!(
            terminal_paths(nr(137, 4)),
            vec![PathBuf::from("/dev/pts/260")]
        );
        assert_eq!(terminal_paths(nr(4, 2)), vec![PathBuf::from("/dev/tty2")]);
        assert_eq!(terminal_paths(nr(4, 65)), vec![PathBuf::from("/dev/ttyS1")]);
        assert_eq!(terminal_paths(nr(5, 0)), Vec::<PathBuf>::new());
    }

    // 1.1: another terminal device, `/proc/<pid>/{mem,environ,fd,maps}`, and the stores and
    // program folder of the console are facts; the agent's own terminal, `/dev/tty`, `/dev/pts`
    // itself, `/proc/<pid>/status` and the program are not (the program is the program).
    #[test]
    fn the_console_wall_facts() {
        let mut l = layout();
        l.own_terminals = vec![PathBuf::from("/dev/pts/3")];
        for p in [
            "/dev/pts/7",
            "/dev/tty1",
            "/dev/ttyS0",
            "/dev/vcs1",
            "/dev/vcsa2",
        ] {
            assert!(l.facts(Path::new(p)).terminal, "{p}");
        }
        for p in [
            "/dev/pts/3",
            "/dev/tty",
            "/dev/pts",
            "/dev/null",
            "/dev/stdout",
        ] {
            assert!(!l.facts(Path::new(p)).terminal, "{p}");
        }
        assert_eq!(l.facts(Path::new("/proc/42/environ")).secret_pid, Some(42));
        assert_eq!(l.facts(Path::new("/proc/42/fd/3")).secret_pid, Some(42));
        assert_eq!(l.facts(Path::new("/proc/42/maps")).secret_pid, Some(42));
        assert_eq!(l.facts(Path::new("/proc/42/mem")).secret_pid, Some(42));
        for p in [
            "/proc/42/status",
            "/proc/self/environ",
            "/proc/loadavg",
            "/proc/42",
        ] {
            assert_eq!(l.facts(Path::new(p)).secret_pid, None, "{p}");
        }
        // DV-09: the passphrase checks in `/etc/lunchroom` are a store in every build (a HELD
        // agent's read of one is DENIED, L1.1, K1.11); the console's other two system folders are
        // stores only with the console (1.1).
        for p in [
            "/etc/lunchroom",
            "/etc/lunchroom/1000.verifier",
            "/etc/lunchroom/1000.verifier.tmp",
        ] {
            assert!(l.facts(Path::new(p)).store, "{p}");
        }
        for p in [
            "/var/lib/lunchroom",
            "/var/lib/lunchroom/1000/store/state.json",
            "/usr/local/lib/lunchroom/other",
        ] {
            assert_eq!(
                l.facts(Path::new(p)).store,
                cfg!(feature = "console"),
                "{p}"
            );
        }
        assert!(!l.facts(Path::new("/etc/lunchrooms/x")).store);
        let program = l.facts(Path::new("/usr/local/lib/lunchroom/lunchroom"));
        assert!(program.program && !program.store);
        assert!(!l.facts(Path::new("/var/lib/lunchrooms")).store);
        assert!(!l.facts(Path::new("/usr/local/lib/lunchroom-other")).store);
    }

    // §11 L1.1: each credential pattern, by name and by folder.
    #[test]
    fn l1_credential_paths() {
        let l = layout();
        for p in [
            "/p/.env",
            "/p/.env.local",
            "/p/cert.pem",
            "/p/server.key",
            "/home/u/.ssh/id_rsa",
            "/p/id_rsa.pub",
            "/home/u/.ssh/config",
            "/home/u/.aws/credentials",
            "/home/u/.config/gcloud/application_default_credentials.json",
            "/p/.npmrc",
        ] {
            assert!(l.facts(Path::new(p)).credential, "{p}");
        }
        // A template of a credential file holds none; the same name in a credential folder is
        // still inside it.
        for p in [
            "/p/.env.example",
            "/p/.env.sample",
            "/p/server.key.template",
            "/p/cert.pem.dist",
        ] {
            assert!(!l.facts(Path::new(p)).credential, "{p}");
        }
        assert!(l.facts(Path::new("/home/u/.ssh/id_rsa.example")).credential);
        for p in ["/p/environment.md", "/p/keys.rs", "/p/src/.envy/x.rs.txt"] {
            assert!(!l.facts(Path::new(p)).credential, "{p}");
        }
    }

    // §6 P1.4: the harness configuration that runs the guard.
    #[test]
    fn p1_guard_config_paths() {
        let l = layout();
        for p in [
            "/home/u/.claude/settings.json",
            "/home/u/.claude.json",
            "/home/u/.codex/config.toml",
            "/home/u/.openclaw/openclaw.json",
            "/home/u/.hermes/config.yaml",
            "/home/u/.config/opencode/opencode.json",
            "/srv/app/.claude/settings.local.json",
            "/srv/app/.codex/config.toml",
            "/etc/claude-code/managed-settings.json",
        ] {
            assert!(l.facts(Path::new(p)).guard_config, "{p}");
        }
        assert!(
            !l.facts(Path::new("/srv/app/.claude/agents/x.md"))
                .guard_config
        );
    }

    // §11 L1.1: a search from a parent folder reaches the denied paths and credential folders.
    #[test]
    fn l1_parent_folder_reaches() {
        let l = layout();
        for d in [
            "/",
            "/home",
            "/home/u",
            "/home/u/.claude",
            "/home/u/.lunchroom",
        ] {
            assert!(l.denied_below(Path::new(d)), "{d}");
        }
        assert!(!l.denied_below(Path::new("/home/u/project")));
        assert!(!l.denied_below(Path::new("/home/u/.lunchroom/bin")));
        assert!(l.credential_root_below(Path::new("/home/u")));
        assert!(l.credential_root_below(Path::new("/home/u/.config")));
        assert!(!l.credential_root_below(Path::new("/home/u/project")));
    }

    // §11 L1.1, §6 P1.4: a harness folder moved by its variable is judged as the usual one is:
    // transcripts denied to read, configuration protected, a search from a parent reaching them.
    #[test]
    fn moved_harness_folders_are_judged_as_the_usual_ones() {
        let mut l = layout();
        l.add_codex_home(Path::new("/srv/codex-data"));
        l.add_hermes_home(Path::new("/srv/hermes-data"));
        l.add_openclaw_state(Path::new("/srv/oc-state"));
        l.add_openclaw_config(Path::new("/etc/oc/custom.json"));
        l.add_claude_config_dir(Path::new("/srv/claude-cfg"));
        l.add_claude_settings(Path::new("/opt/policy/settings-x.json"));
        for p in [
            "/srv/codex-data/sessions/2026/s.jsonl",
            "/srv/codex-data/archived_sessions/s.jsonl",
            "/srv/codex-data/history.jsonl",
            "/srv/hermes-data/sessions/s.json",
            "/srv/oc-state/agents/a.json",
            "/srv/claude-cfg/projects/p/t.jsonl",
            "/srv/claude-cfg/history.jsonl",
        ] {
            assert!(l.facts(Path::new(p)).transcript, "{p}");
        }
        for p in [
            "/srv/codex-data/hooks.json",
            "/srv/codex-data/config.toml",
            "/srv/hermes-data/config.yaml",
            "/srv/oc-state/openclaw.json",
            "/etc/oc/custom.json",
            "/srv/claude-cfg/settings.json",
            "/srv/claude-cfg/.claude.json",
            "/opt/policy/settings-x.json",
        ] {
            assert!(l.facts(Path::new(p)).guard_config, "{p}");
        }
        // A sibling that merely shares a name prefix is not below the folder.
        for p in [
            "/srv/codex-data2/sessions/s.jsonl",
            "/srv/claude-cfg2/projects/t.jsonl",
            "/opt/policy/settings-x.json.bak",
            "/etc/oc/other.json",
        ] {
            let f = l.facts(Path::new(p));
            assert!(!f.transcript && !f.guard_config, "{p}");
        }
        // Inside the moved folder, as inside `~/.claude`: configuration, a transcript only below
        // `projects`.
        let f = l.facts(Path::new("/srv/claude-cfg/projects-x/t.jsonl"));
        assert!(!f.transcript && f.guard_config);
        for d in ["/", "/srv", "/srv/codex-data", "/srv/claude-cfg"] {
            assert!(l.denied_below(Path::new(d)), "{d}");
        }
        assert!(!l.denied_below(Path::new("/srv/other")));
        // Registering a folder twice records it once.
        l.add_codex_home(Path::new("/srv/codex-data"));
        assert_eq!(
            l.guard_config_roots
                .iter()
                .filter(|r| r.as_path() == Path::new("/srv/codex-data"))
                .count(),
            1
        );
    }

    #[test]
    fn normalize_expands_home_and_parents() {
        let home = Path::new("/home/u");
        let cwd = Path::new("/srv/app");
        assert_eq!(normalize("~", cwd, home), PathBuf::from("/home/u"));
        assert_eq!(normalize("~/a/../b", cwd, home), PathBuf::from("/home/u/b"));
        assert_eq!(
            normalize("./x/./y", cwd, home),
            PathBuf::from("/srv/app/x/y")
        );
        assert_eq!(normalize("../../../..", cwd, home), PathBuf::from("/"));
        assert_eq!(
            normalize("/etc/../proc/1", cwd, home),
            PathBuf::from("/proc/1")
        );
        assert_eq!(literal_prefix(Path::new("/a/b*/c")), PathBuf::from("/a"));
    }

    // §11 L1.1 "Paths are resolved (symlinks, wildcards) before checking": `absolute` keeps `..`
    // where it was written, so the file system, not the text, decides which folder it leaves.
    #[test]
    fn absolute_keeps_parent_parts_for_the_file_system_to_resolve() {
        let home = Path::new("/home/u");
        let cwd = Path::new("/srv/app");
        assert_eq!(absolute("~", cwd, home), PathBuf::from("/home/u"));
        assert_eq!(
            absolute("~/a/../b", cwd, home),
            PathBuf::from("/home/u/a/../b")
        );
        assert_eq!(
            absolute("link/../h.jsonl", cwd, home),
            PathBuf::from("/srv/app/link/../h.jsonl")
        );
        assert_eq!(absolute("/x/../y", cwd, home), PathBuf::from("/x/../y"));
        // The lexical form differs exactly where a link could sit before the `..`.
        assert_eq!(
            normalize("link/../h.jsonl", cwd, home),
            PathBuf::from("/srv/app/h.jsonl")
        );
    }
}
