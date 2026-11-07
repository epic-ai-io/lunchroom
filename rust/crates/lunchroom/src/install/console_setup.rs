//! `lunchroom install console` (1.1): puts the store and the program out of reach of the user's
//! agents, and lets the user start the console.
//!
//! The steps, each shown to the person before any is done and run through the `sudo` that
//! `lunchroom passphrase` already uses for `/etc/lunchroom`:
//! 1. the system group and user `lunchroom`, when absent;
//! 2. `/usr/local/lib/lunchroom` (root's, 0755) and, in it, the program: root and group `lunchroom`,
//!    mode 2755 (set-group-id);
//! 3. `/var/lib/lunchroom/<uid>`: root and group `lunchroom`, mode 2770, so only a process with
//!    the group can enter it; the developer is not in that group;
//! 4. the old store copied in once (`cp`, then ownership and mode), when the new one is not there;
//! 5. a sudoers drop-in `/etc/sudoers.d/lunchroom` that lets the developer, and no one else, run
//!    the console as `lunchroom` (it asks for the developer's sudo password: no `NOPASSWD`), written
//!    under a name `sudo` ignores, checked with `visudo -cf`, then renamed into place.
//!
//! Then the new program's own `install` points the hooks at it, the `~/.local/bin` link is pointed
//! at it, and the old store is renamed to `store.moved-<stamp>` beside the old one (nothing is
//! deleted).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use lunchroom_core::mail::Mail;
use lunchroom_core::setgid;
use lunchroom_core::store::Store;

use super::{installed_program, stamp};
use crate::cli::Outcome;
use crate::home::Env;
use crate::terminal::{System, confirm, system_path};

/// The programs the steps run, found in the system folders.
const TOOLS: [&str; 9] = [
    "groupadd", "useradd", "install", "cp", "chown", "chmod", "visudo", "mv", "rm",
];

/// The sudoers file's name while it is unchecked: `sudo` ignores a name with a `.` in it.
const SUDOERS_UNCHECKED: &str = "/etc/sudoers.d/lunchroom.unchecked";
/// The sudoers file's name once `visudo` has passed it.
const SUDOERS: &str = "/etc/sudoers.d/lunchroom";

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
}

/// What the steps depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub uid: u32,
    pub developer: String,
    /// The program that is running now, which becomes the installed one.
    pub exe: PathBuf,
    pub group_exists: bool,
    pub user_exists: bool,
    /// The old store folder and records file, when they exist and the new store is not there yet.
    pub old_store: Option<PathBuf>,
    pub old_records: Option<PathBuf>,
}

/// An account name `sudoers` takes as it is written: letters, digits, `_` and `-`, not starting
/// with `-`.
#[must_use]
pub fn valid_account(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The sudoers drop-in: the developer may run the console as the user `lunchroom`, after typing
/// their own sudo password (no `NOPASSWD`).
#[must_use]
pub fn sudoers_text(developer: &str) -> String {
    format!(
        "# Written by `lunchroom install console`: lets {developer} start the Lunchroom console as \
         the user {user}.\n# It asks for {developer}'s sudo password.\n\
         {developer} ALL=({user}) {program} console\n",
        user = setgid::SYSTEM_USER,
        program = setgid::PROGRAM
    )
}

/// The steps for `facts`; `tool` gives the absolute path of a program by name.
#[must_use]
#[allow(clippy::too_many_lines)] // the steps, in order, one after another
pub fn root_steps(f: &Facts, tool: &dyn Fn(&str) -> String) -> Vec<RootStep> {
    let user = setgid::SYSTEM_USER;
    let home = setgid::store_home(f.uid);
    let home_s = home.display().to_string();
    let owner = format!("{}:{user}", f.uid);
    let step = |why: &str, argv: Vec<String>| RootStep {
        why: why.to_owned(),
        argv,
        input: None,
        on_failure: None,
    };
    let v = |parts: &[&str]| -> Vec<String> { parts.iter().map(|p| (*p).to_owned()).collect() };
    let mut steps = Vec::new();
    if !f.group_exists {
        steps.push(step(
            "make the system group lunchroom",
            v(&[&tool("groupadd"), "--system", user]),
        ));
    }
    if !f.user_exists {
        steps.push(step(
            "make the system user lunchroom, which cannot log in",
            v(&[
                &tool("useradd"),
                "--system",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                "--gid",
                user,
                user,
            ]),
        ));
    }
    steps.push(step(
        "make the folder for the program, root's alone to change",
        v(&[
            &tool("install"),
            "-d",
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "755",
            setgid::PROGRAM_DIR,
        ]),
    ));
    steps.push(step(
        "make the folder for the stores",
        v(&[
            &tool("install"),
            "-d",
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "755",
            setgid::STORE_ROOT,
        ]),
    ));
    steps.push(step(
        "make your store folder, which only the group lunchroom can enter (you are not in it)",
        v(&[
            &tool("install"),
            "-d",
            "-o",
            "root",
            "-g",
            user,
            "-m",
            "2770",
            &home_s,
        ]),
    ));
    steps.push(step(
        "install this program as the Lunchroom program: root's, group lunchroom, with the \
         set-group-id bit",
        v(&[
            &tool("install"),
            "-o",
            "root",
            "-g",
            user,
            "-m",
            "2755",
            &f.exe.display().to_string(),
            setgid::PROGRAM,
        ]),
    ));
    if let Some(old) = &f.old_store {
        let new_store = home.join("store").display().to_string();
        steps.push(step(
            "copy your store to the new folder (once; the old one is kept)",
            v(&[
                &tool("cp"),
                "-R",
                "-P",
                &old.display().to_string(),
                &new_store,
            ]),
        ));
        steps.push(step(
            "give the copy to you, with the group lunchroom",
            v(&[&tool("chown"), "-R", &owner, &new_store]),
        ));
        steps.push(step(
            "let the group read and write the copy, and no one else",
            v(&[&tool("chmod"), "-R", "u=rwX,g=rwX,o=", &new_store]),
        ));
    }
    if let Some(old) = &f.old_records {
        let new_records = home.join("guard-records.jsonl").display().to_string();
        steps.push(step(
            "copy the guard's records to the new folder",
            v(&[&tool("cp"), "-P", &old.display().to_string(), &new_records]),
        ));
        steps.push(step(
            "give the copy to you, with the group lunchroom",
            v(&[&tool("chown"), &owner, &new_records]),
        ));
        steps.push(step(
            "let the group read and write the copy, and no one else",
            v(&[&tool("chmod"), "660", &new_records]),
        ));
    }
    steps.push(RootStep {
        why: format!(
            "write the sudoers rule that lets you, and no one else, start the console as {user} \
             (it asks for your sudo password); it is not active until it passes the next step"
        ),
        argv: v(&[
            &tool("install"),
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "0440",
            "/dev/stdin",
            SUDOERS_UNCHECKED,
        ]),
        input: Some(sudoers_text(&f.developer)),
        on_failure: None,
    });
    steps.push(RootStep {
        why: "check the sudoers rule with visudo".to_owned(),
        argv: v(&[&tool("visudo"), "-cf", SUDOERS_UNCHECKED]),
        input: None,
        on_failure: Some(v(&[&tool("rm"), "-f", SUDOERS_UNCHECKED])),
    });
    steps.push(RootStep {
        why: "put the checked rule in place".to_owned(),
        argv: v(&[&tool("mv"), SUDOERS_UNCHECKED, SUDOERS]),
        input: None,
        on_failure: Some(v(&[&tool("rm"), "-f", SUDOERS_UNCHECKED])),
    });
    steps
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

/// What the set-up depends on, read from the computer.
fn gather(env: &Env, uid: u32) -> Result<Facts, String> {
    let passwd = fs::read_to_string("/etc/passwd").map_err(|e| format!("/etc/passwd: {e}"))?;
    let group = fs::read_to_string("/etc/group").map_err(|e| format!("/etc/group: {e}"))?;
    let developer = setgid::name_from_passwd(&passwd, uid)
        .ok_or_else(|| format!("user {uid} has no name in /etc/passwd"))?;
    if !valid_account(&developer) {
        return Err(format!(
            "the account name {developer:?} has characters the sudoers rule cannot take as they \
             are; nothing was changed"
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("this program's path: {e}"))?;
    let new_home = setgid::store_home(uid);
    let new_store_there = new_home.join("store").exists();
    let old_store = env.store_dir();
    let old_records = crate::records::file(&env.lunchroom_home);
    Ok(Facts {
        uid,
        developer,
        exe,
        group_exists: setgid::gid_from_group(&group, setgid::SYSTEM_USER).is_some(),
        user_exists: setgid::uid_from_passwd(&passwd, setgid::SYSTEM_USER).is_some(),
        old_store: (old_store.is_dir() && !new_store_there).then_some(old_store),
        old_records: (old_records.is_file() && !new_home.join("guard-records.jsonl").exists())
            .then_some(old_records),
    })
}

/// `lunchroom install console`.
#[must_use]
#[allow(clippy::too_many_lines)] // one transaction: check, show, confirm, run, report
pub fn install_console(env: &Env, sys: &mut dyn System) -> Outcome {
    if setgid::active() {
        return Outcome::ok(format!(
            "The console is already set up: this Lunchroom runs as {} with the store at {}.\n\
             Start the console in a terminal of your own with:\n  {}\n",
            setgid::PROGRAM,
            env.lunchroom_home.display(),
            crate::console::start_command()
        ));
    }
    if let Some(why) = sys.root_blocker() {
        return Outcome::failed(
            &format!("the console cannot be set up here: {why}; nothing was changed"),
            3,
        );
    }
    let Some(uid) = sys.uid() else {
        return Outcome::failed(
            "the user id of this process cannot be read; nothing was changed",
            1,
        );
    };
    let missing: Vec<&str> = TOOLS
        .iter()
        .copied()
        .filter(|t| system_path(t).is_none())
        .collect();
    if !missing.is_empty() {
        return Outcome::failed(
            &format!(
                "these programs were not found in the system folders: {}; nothing was changed",
                missing.join(", ")
            ),
            3,
        );
    }
    let facts = match gather(env, uid) {
        Ok(f) => f,
        Err(why) => return Outcome::failed(&why, 1),
    };
    let tool =
        |name: &str| system_path(name).map_or_else(|| name.to_owned(), |p| p.display().to_string());
    let steps = root_steps(&facts, &tool);
    let mail = match Store::<Mail>::open(&env.store_dir()).and_then(|s| s.read()) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(&crate::cli::unread(e), 1),
    };
    let old = installed_program(env);
    let question = format!(
        "Lunchroom: set up the console? This puts Lunchroom's store and program where no process \
         of yours can reach them, and lets you start a console that your agents cannot see or \
         press keys in.\nIt runs these commands as root with sudo, which asks for your sudo \
         password. Nothing is deleted.\n{}\nThen it runs `{} install` so the hooks of your agents \
         run the new program, points your `lunchroom` link at it, and renames your old store to \
         store.moved-{}.\n",
        steps_text(&steps),
        setgid::PROGRAM,
        stamp(env.now)
    );
    if let Err(reason) = confirm(&mail, sys, &question) {
        return Outcome::denied(&reason, 3);
    }
    let mut report = String::new();
    for (n, step) in steps.iter().enumerate() {
        match sys.run_root(&step.why, &step.argv, step.input.as_deref()) {
            Ok(()) => {
                let _ = writeln!(report, "done {}/{}: {}", n + 1, steps.len(), step.why);
            }
            Err(e) => {
                if let Some(cleanup) = &step.on_failure {
                    let _ = sys.run_root("remove the rule that did not pass", cleanup, None);
                }
                let _ = writeln!(
                    report,
                    "stopped at step {} ({}): {e}\nNothing after it was done. Your store and hooks \
                     are as they were; run `lunchroom install console` again after fixing that.",
                    n + 1,
                    step.why
                );
                return Outcome::failed(&report, 1);
            }
        }
    }
    let program = Path::new(setgid::PROGRAM);
    if let Ok(true) = sys.run_user(program, &["install"]) {
        let _ = writeln!(report, "the hooks now run {}", setgid::PROGRAM);
    } else {
        let _ = writeln!(
            report,
            "`{} install` did not finish, so the hooks still run the old program and your \
             old store was left where it is. Run `{} install` in a terminal of your own, then \
             `lunchroom install console` again to move the old store aside.",
            setgid::PROGRAM,
            setgid::PROGRAM
        );
        return Outcome::failed(&report, 1);
    }
    for line in repoint_links(&env.home, &old, program) {
        report.push_str(&line);
    }
    for line in move_old_aside(&env.lunchroom_home, env.now) {
        report.push_str(&line);
    }
    let _ = write!(
        report,
        "The console is set up. Start it in a terminal of your own (not inside an agent session):\n  {}\n",
        crate::console::start_command()
    );
    Outcome::ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> String {
        format!("/usr/bin/{name}")
    }

    fn facts() -> Facts {
        Facts {
            uid: 1000,
            developer: "dev".to_owned(),
            exe: PathBuf::from("/home/u/.lunchroom/bin/lunchroom"),
            group_exists: false,
            user_exists: false,
            old_store: Some(PathBuf::from("/home/u/.lunchroom/store")),
            old_records: Some(PathBuf::from("/home/u/.lunchroom/guard-records.jsonl")),
        }
    }

    fn lines(steps: &[RootStep]) -> Vec<String> {
        steps.iter().map(|s| s.argv.join(" ")).collect()
    }

    // The steps, in full, for a computer with nothing set up and an old store.
    #[test]
    fn the_steps_for_a_new_setup_in_full() {
        let steps = root_steps(&facts(), &tool);
        assert_eq!(
            lines(&steps),
            vec![
                "/usr/bin/groupadd --system lunchroom",
                "/usr/bin/useradd --system --no-create-home --shell /usr/sbin/nologin --gid \
                 lunchroom lunchroom",
                "/usr/bin/install -d -o root -g root -m 755 /usr/local/lib/lunchroom",
                "/usr/bin/install -d -o root -g root -m 755 /var/lib/lunchroom",
                "/usr/bin/install -d -o root -g lunchroom -m 2770 /var/lib/lunchroom/1000",
                "/usr/bin/install -o root -g lunchroom -m 2755 /home/u/.lunchroom/bin/lunchroom \
                 /usr/local/lib/lunchroom/lunchroom",
                "/usr/bin/cp -R -P /home/u/.lunchroom/store /var/lib/lunchroom/1000/store",
                "/usr/bin/chown -R 1000:lunchroom /var/lib/lunchroom/1000/store",
                "/usr/bin/chmod -R u=rwX,g=rwX,o= /var/lib/lunchroom/1000/store",
                "/usr/bin/cp -P /home/u/.lunchroom/guard-records.jsonl \
                 /var/lib/lunchroom/1000/guard-records.jsonl",
                "/usr/bin/chown 1000:lunchroom /var/lib/lunchroom/1000/guard-records.jsonl",
                "/usr/bin/chmod 660 /var/lib/lunchroom/1000/guard-records.jsonl",
                "/usr/bin/install -o root -g root -m 0440 /dev/stdin \
                 /etc/sudoers.d/lunchroom.unchecked",
                "/usr/bin/visudo -cf /etc/sudoers.d/lunchroom.unchecked",
                "/usr/bin/mv /etc/sudoers.d/lunchroom.unchecked /etc/sudoers.d/lunchroom",
            ]
        );
    }

    // A computer that already has the user and group, and no old store, gets neither of those
    // steps nor the copy.
    #[test]
    fn existing_accounts_and_no_old_store_leave_those_steps_out() {
        let mut f = facts();
        f.group_exists = true;
        f.user_exists = true;
        f.old_store = None;
        f.old_records = None;
        let all = lines(&root_steps(&f, &tool)).join("\n");
        for gone in ["groupadd", "useradd", " cp ", "chown", "chmod -R"] {
            assert!(!all.contains(gone), "{gone} in {all}");
        }
        assert!(all.contains("install -d -o root -g lunchroom -m 2770 /var/lib/lunchroom/1000"));
    }

    // The sudoers rule names the developer, the console program with its one argument, and no
    // NOPASSWD; it is written at mode 0440 under a name sudo ignores, checked with visudo -cf
    // before it is renamed, and a failed check removes the unchecked file.
    #[test]
    fn the_sudoers_rule_asks_for_the_password_and_is_checked_before_it_counts() {
        assert_eq!(
            sudoers_text("dev"),
            "# Written by `lunchroom install console`: lets dev start the Lunchroom console as the \
             user lunchroom.\n# It asks for dev's sudo password.\ndev ALL=(lunchroom) \
             /usr/local/lib/lunchroom/lunchroom console\n"
        );
        assert!(!sudoers_text("dev").contains("NOPASSWD"));
        let steps = root_steps(&facts(), &tool);
        let write = steps.iter().find(|s| s.argv.contains(&"0440".to_owned()));
        assert_eq!(
            write.and_then(|s| s.input.clone()),
            Some(sudoers_text("dev"))
        );
        let check = steps.iter().find(|s| s.argv.contains(&"-cf".to_owned()));
        assert_eq!(
            check.and_then(|s| s.on_failure.clone()),
            Some(vec![
                "/usr/bin/rm".to_owned(),
                "-f".to_owned(),
                SUDOERS_UNCHECKED.to_owned()
            ])
        );
        // The unchecked name has a `.`, which sudo skips when it reads the folder.
        assert!(SUDOERS_UNCHECKED.contains('.'));
        assert!(!SUDOERS.rsplit('/').next().unwrap_or_default().contains('.'));
    }

    // An account name is taken into the rule only when it is one sudoers reads as it is written.
    #[test]
    fn only_a_plain_account_name_goes_into_the_rule() {
        for ok in ["dev", "a_b-c", "user1", "_x"] {
            assert!(valid_account(ok), "{ok}");
        }
        for bad in [
            "",
            "-dev",
            "de v",
            "dev,root",
            "dev\nroot",
            "dev#x",
            "a:b",
            "ALL ALL",
        ] {
            assert!(!valid_account(bad), "{bad:?}");
        }
        assert!(!valid_account(&"a".repeat(33)));
    }

    // The set-up is shown in full before anything is done: every step, its command, and the rule
    // text with each of its lines.
    #[test]
    fn the_person_is_shown_every_step_and_the_rule() {
        let mut f = facts();
        f.group_exists = true;
        f.user_exists = true;
        f.old_store = None;
        f.old_records = None;
        let text = steps_text(&root_steps(&f, &tool));
        assert!(
            text.starts_with(
                "1. make the folder for the program, root's alone to change\n     \
             /usr/bin/install -d -o root -g root -m 755 /usr/local/lib/lunchroom\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "     | dev ALL=(lunchroom) /usr/local/lib/lunchroom/lunchroom console\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("check the sudoers rule with visudo\n     /usr/bin/visudo -cf"),
            "{text}"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::test_target()
            .join("console-setup")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    // The link that led to the old program leads to the new one; a link to anything else, and a
    // file that is not a link, are left alone.
    #[test]
    fn only_a_link_to_the_old_program_is_repointed() {
        let home = scratch("links");
        let old = home.join(".lunchroom/bin/lunchroom");
        let new = Path::new("/usr/local/lib/lunchroom/lunchroom");
        let local = home.join(".local/bin");
        let bin = home.join("bin");
        assert!(fs::create_dir_all(&local).is_ok());
        assert!(fs::create_dir_all(&bin).is_ok());
        assert!(std::os::unix::fs::symlink(&old, local.join("lunchroom")).is_ok());
        assert!(std::os::unix::fs::symlink("/somewhere/else", bin.join("lunchroom")).is_ok());
        let lines = repoint_links(&home, &old, new);
        assert_eq!(
            lines,
            vec![format!(
                "linked {} to {}\n",
                local.join("lunchroom").display(),
                new.display()
            )]
        );
        assert_eq!(
            fs::read_link(local.join("lunchroom")).ok().as_deref(),
            Some(new)
        );
        assert_eq!(
            fs::read_link(bin.join("lunchroom")).ok(),
            Some(PathBuf::from("/somewhere/else"))
        );
        // Run again: nothing leads to the old program now.
        assert_eq!(
            repoint_links(&home, &old, new),
            [] as [std::string::String; 0]
        );
        let _ = fs::remove_dir_all(&home);
    }

    // The old store and records are renamed beside themselves with a stamp, never deleted; what is
    // not there is not mentioned.
    #[test]
    fn the_old_store_is_moved_aside_and_kept() {
        let home = scratch("aside");
        assert!(fs::create_dir_all(home.join("store")).is_ok());
        assert!(fs::write(home.join("store/state.json"), "{}").is_ok());
        assert!(fs::write(home.join("guard-records.jsonl"), "").is_ok());
        let lines = move_old_aside(&home, 1_700_000_000);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("moved "), "{lines:?}");
        assert!(!home.join("store").exists());
        assert_eq!(
            fs::read_to_string(home.join("store.moved-20231114T221320Z/state.json"))
                .ok()
                .as_deref(),
            Some("{}")
        );
        assert!(
            home.join("guard-records.jsonl.moved-20231114T221320Z")
                .is_file()
        );
        assert_eq!(
            move_old_aside(&home, 1_700_000_000),
            [] as [std::string::String; 0]
        );
        let _ = fs::remove_dir_all(&home);
    }
}
