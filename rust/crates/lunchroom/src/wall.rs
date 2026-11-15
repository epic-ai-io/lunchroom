//! The store wall and the console as `lunchroom doctor` finds them (C1.4, `§14::30-35`,
//! §17.2, §17.8, §17.9).
//!
//! The facts are read from disk below a root (`/` in the product, a scratch folder in the tests)
//! and judged by [`lines`], which gives one line for each part of the wall, so each broken part is
//! a FAIL of its own: the group `lunchroom`, the program setgid `lunchroom`, the store's folder,
//! the verifier, and the user not being in that group. Nothing here writes, and a `stat` needs no
//! permission on the file, so the facts are read by the user's own process.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use lunchroom_core::setgid;

use crate::install::console_state::State;

/// The owner, group and mode of a file or folder as it is on disk (a link is not followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub owner: u32,
    pub group: u32,
    /// The permission bits with the set-id and sticky bits.
    pub mode: u32,
    pub dir: bool,
    pub file: bool,
}

/// What the wall and the console stand on, as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The user's id; `None` where it cannot be read.
    pub uid: Option<u32>,
    /// The user's account name, from the account database.
    pub account: Option<String>,
    /// The user's primary group.
    pub primary_gid: Option<u32>,
    /// The id of the group `lunchroom`; `None` where there is none.
    pub group: Option<u32>,
    /// The accounts the group database lists in the group `lunchroom`.
    pub members: Vec<String>,
    /// The program, `/usr/local/lib/lunchroom/lunchroom`; the reason when it cannot be read.
    pub program: Result<Node, String>,
    /// The user's store folder, `/var/lib/lunchroom/<uid>`.
    pub folder: Result<Node, String>,
    /// The user's verifier, `/etc/lunchroom/<uid>.verifier`.
    pub verifier: Result<Node, String>,
    /// The folder the verifier is in.
    pub verifier_dir: Result<Node, String>,
    /// The console's public key is there: the console is installed (§17.9).
    pub console_installed: bool,
    /// The record of why the console was not set up (§17.8): `None` where there is none.
    pub console_state: Option<Result<State, String>>,
}

fn under(root: &Path, absolute: &Path) -> PathBuf {
    root.join(absolute.strip_prefix("/").unwrap_or(absolute))
}

fn node(path: &Path) -> Result<Node, String> {
    std::fs::symlink_metadata(path)
        .map(|m| Node {
            owner: m.uid(),
            group: m.gid(),
            mode: m.mode() & 0o7777,
            dir: m.file_type().is_dir(),
            file: m.file_type().is_file(),
        })
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "it is not there".to_owned()
            } else {
                format!("it cannot be read: {e}")
            }
        })
}

/// The facts for user `uid`, read below `root`.
#[must_use]
pub fn read(root: &Path, uid: Option<u32>) -> Facts {
    let text =
        |path: &str| std::fs::read_to_string(under(root, Path::new(path))).unwrap_or_default();
    let (passwd, group_db) = (text("/etc/passwd"), text("/etc/group"));
    let primary_gid = uid.and_then(|u| {
        passwd.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.len() >= 4 && fields[2].parse::<u32>().ok() == Some(u))
                .then(|| fields[3].parse().ok())
                .flatten()
        })
    });
    let members = group_db
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.len() >= 4 && fields[0] == setgid::SYSTEM_USER).then(|| {
                fields[3]
                    .split(',')
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    let at = |p: PathBuf| under(root, &p);
    let (folder, verifier, state, public) = match uid {
        Some(u) => (
            node(&at(setgid::store_home(u))),
            node(&at(crate::terminal::verifier_path(u))),
            std::fs::read_to_string(at(setgid::console_state(u))),
            at(setgid::console_pub(u)).exists(),
        ),
        None => (
            Err("the user id cannot be read".to_owned()),
            Err("the user id cannot be read".to_owned()),
            Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            false,
        ),
    };
    let console_state = match state {
        Ok(text) => Some(State::from_json(&text).ok_or_else(|| {
            "it is not a record Lunchroom wrote (the file is root's; a program of yours cannot \
             have changed it)"
                .to_owned()
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(format!("it cannot be read: {e}"))),
    };
    Facts {
        uid,
        account: uid.and_then(|u| setgid::name_from_passwd(&passwd, u)),
        primary_gid,
        group: setgid::gid_from_group(&group_db, setgid::SYSTEM_USER),
        members,
        program: node(&at(PathBuf::from(setgid::PROGRAM))),
        folder,
        verifier,
        verifier_dir: node(&at(PathBuf::from(setgid::KEY_DIR))),
        console_installed: public,
        console_state,
    }
}

const GROUP: &str = setgid::SYSTEM_USER;

fn install_fix() -> String {
    "in a terminal of your own, run `lunchroom install` again, which puts it right".to_owned()
}

/// One part of the wall: the group.
fn group_line(f: &Facts) -> Result<String, String> {
    f.group
        .map(|_| format!("the group {GROUP} exists"))
        .ok_or_else(|| {
            format!(
                "the store wall: there is no group {GROUP}: {}",
                install_fix()
            )
        })
}

/// One part of the wall: the program, setgid `lunchroom`, root's and writable by no one else.
fn program_line(f: &Facts) -> Result<String, String> {
    let shown = setgid::PROGRAM;
    let n = f.program.as_ref().map_err(|why| {
        format!(
            "the store wall: the program {shown}: {why}: {}",
            install_fix()
        )
    })?;
    let wrong = |what: &str| {
        Err(format!(
            "the store wall: the program {shown} {what}: {}",
            install_fix()
        ))
    };
    if !n.file {
        return wrong("is not a regular file");
    }
    if n.owner != 0 {
        return wrong("is not owned by root");
    }
    if f.group != Some(n.group) || f.group.is_none() {
        return wrong(&format!("is not in group {GROUP}"));
    }
    if n.mode & 0o2000 == 0 {
        return wrong("does not have the set-group-id bit");
    }
    if n.mode & 0o022 != 0 {
        return wrong("can be changed by a user other than root");
    }
    if n.mode & 0o111 == 0 {
        return wrong("cannot be run");
    }
    Ok(format!(
        "the program {shown} is setgid {GROUP}, owned by root and changeable by no one else"
    ))
}

/// One part of the wall: the user's store folder, group `lunchroom`, mode 2770, root's.
fn folder_line(f: &Facts) -> Result<String, String> {
    let shown = f.uid.map_or_else(
        || format!("{}/<uid>", setgid::STORE_ROOT),
        |u| setgid::store_home(u).display().to_string(),
    );
    let n = f.folder.as_ref().map_err(|why| {
        format!(
            "the store wall: the store's folder {shown}: {why}: {}",
            install_fix()
        )
    })?;
    let wrong = |what: &str| {
        Err(format!(
            "the store wall: the store's folder {shown} {what}: {}",
            install_fix()
        ))
    };
    if !n.dir {
        return wrong("is not a folder");
    }
    if n.owner != 0 {
        return wrong("is not owned by root, so its owner can change who may enter it");
    }
    if f.group != Some(n.group) || f.group.is_none() {
        return wrong(&format!("is not in group {GROUP}"));
    }
    if n.mode != 0o2770 {
        return wrong(&format!(
            "has mode {:04o}, not 2770 (group {GROUP} only, and set-group-id)",
            n.mode
        ));
    }
    Ok(format!(
        "the store's folder {shown} is group {GROUP}, mode 2770, root's: only that group can enter it"
    ))
}

/// One part of the wall: the verifier, root's, group `lunchroom`, mode 0640, a regular file (not a
/// link), in a folder root's and writable by no group or other user (A1.3).
fn verifier_line(f: &Facts) -> Result<String, String> {
    let shown = f.uid.map_or_else(
        || format!("{}/<uid>.verifier", setgid::KEY_DIR),
        |u| crate::terminal::verifier_path(u).display().to_string(),
    );
    let n = f.verifier.as_ref().map_err(|why| {
        format!("the store wall: the passphrase verifier {shown}: {why}: run `lunchroom passphrase` in a terminal of your own")
    })?;
    let wrong = |what: &str| {
        Err(format!(
            "the store wall: the passphrase verifier {shown} {what}: {}",
            install_fix()
        ))
    };
    if !n.file {
        return wrong("is not a regular file (a link is not followed)");
    }
    if n.owner != 0 {
        return wrong("is not owned by root");
    }
    if f.group != Some(n.group) || f.group.is_none() {
        return wrong(&format!(
            "is not in group {GROUP}, so the program cannot read it"
        ));
    }
    if n.mode != 0o640 {
        return wrong(&format!(
            "has mode {:04o}, not 0640 (readable by group {GROUP} only)",
            n.mode
        ));
    }
    match &f.verifier_dir {
        Ok(d) if d.dir && d.owner == 0 && d.mode & 0o022 == 0 => {}
        Ok(_) => {
            return Err(format!(
                "the store wall: the folder of the verifier, {}, is not a folder owned by root \
                 and writable by no group or other user: {}",
                setgid::KEY_DIR,
                install_fix()
            ));
        }
        Err(why) => {
            return Err(format!(
                "the store wall: the folder of the verifier, {}: {why}: {}",
                setgid::KEY_DIR,
                install_fix()
            ));
        }
    }
    Ok(format!(
        "the passphrase verifier {shown} is root's, group {GROUP}, mode 0640, in a folder only root can change"
    ))
}

/// One part of the wall: the user is not in the group `lunchroom`.
fn member_line(f: &Facts) -> Result<String, String> {
    let (Some(account), Some(group)) = (f.account.as_deref(), f.group) else {
        return if f.group.is_none() {
            Err(format!(
                "the store wall: there is no group {GROUP}, so no one is kept out of the store: {}",
                install_fix()
            ))
        } else {
            Err(
                "the store wall: your account name cannot be read from the account database, so \
                 it is not shown that you are outside the group lunchroom"
                    .to_owned(),
            )
        };
    };
    if f.members.iter().any(|m| m == account) || f.primary_gid == Some(group) {
        Err(format!(
            "the store wall: your account {account} is in the group {GROUP}, so a process of yours \
             can enter the store: in a terminal of your own, run `sudo gpasswd -d {account} {GROUP}` \
             (and, if {GROUP} is your primary group, `sudo usermod -g <another group> {account}`), \
             then log in again"
        ))
    } else {
        Ok(format!(
            "your account {account} is not in the group {GROUP}"
        ))
    }
}

/// The lines of the store wall, one for each part (`§14::30`): the group, the program, the store's
/// folder, the verifier and the user's membership.
#[must_use]
pub fn lines(f: &Facts) -> Vec<Result<String, String>> {
    vec![
        group_line(f),
        program_line(f),
        folder_line(f),
        verifier_line(f),
        member_line(f),
    ]
}

/// What `lunchroom doctor` says of the console (`§14::33-35`, `§17::98`, `§17::104`): the installed
/// console is a PASS; one that was not set up because the user answered anything but `yes`, or
/// because the rule could not be put in force, is a FAIL that gives the recorded reason and says
/// that running `lunchroom install console` again asks once more; otherwise a note that says how to
/// install it and leaves the exit code.
#[must_use]
pub fn console_line(f: &Facts) -> ConsoleLine {
    match (&f.console_state, f.console_installed) {
        (Some(Ok(state)), _) => ConsoleLine::Fail(format!(
            "the console is not set up: {}{} Running `lunchroom install console` again asks once \
             more.",
            state.reason,
            if state.reason.ends_with(['.', '!', '?']) {
                ""
            } else {
                "."
            },
        )),
        (Some(Err(why)), _) => ConsoleLine::Fail(format!(
            "the console: the record of why it was not set up, {}, cannot be used: {why}. \
             Running `lunchroom install console` again asks once more.",
            f.uid.map_or_else(
                || format!("{}/<uid>.console.state", setgid::KEY_DIR),
                |u| setgid::console_state(u).display().to_string()
            )
        )),
        (None, true) => ConsoleLine::Pass(
            "the console is installed: `lunchroom console` starts it in a terminal of your own"
                .to_owned(),
        ),
        (None, false) => ConsoleLine::Note(
            "the console is not installed: `lunchroom install console` sets it up".to_owned(),
        ),
    }
}

/// The result of [`console_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleLine {
    Pass(String),
    Fail(String),
    Note(String),
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::install::console_state::Kind;

    /// The wall as `lunchroom install` makes it, for user 1000 (account `dev`, group 1000).
    pub(crate) fn good() -> Facts {
        let node = |owner, group, mode, dir, file| {
            Ok(Node {
                owner,
                group,
                mode,
                dir,
                file,
            })
        };
        Facts {
            uid: Some(1000),
            account: Some("dev".to_owned()),
            primary_gid: Some(1000),
            group: Some(990),
            members: vec!["someone".to_owned()],
            program: node(0, 990, 0o2755, false, true),
            folder: node(0, 990, 0o2770, true, false),
            verifier: node(0, 990, 0o640, false, true),
            verifier_dir: node(0, 0, 0o755, true, false),
            console_installed: false,
            console_state: None,
        }
    }

    fn fails(f: &Facts) -> Vec<String> {
        lines(f).into_iter().filter_map(Result::err).collect()
    }

    // §14::30: a wall as the install makes it has five parts, each a PASS.
    #[test]
    fn a_wall_as_installed_passes_each_part() {
        let out = lines(&good());
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(Result::is_ok), "{out:?}");
    }

    // §14::30: each broken part gives its own FAIL, and only that one.
    #[test]
    fn each_broken_part_is_its_own_fail() {
        /// Changes the node when there is one.
        fn tweak(n: &mut Result<Node, String>, change: impl FnOnce(&mut Node)) {
            if let Ok(node) = n {
                change(node);
            }
        }
        type Case = (&'static str, fn(&mut Facts), &'static str, usize);
        let cases: [Case; 12] = [
            (
                "group",
                |f| f.group = None,
                "there is no group lunchroom",
                5,
            ),
            (
                "program missing",
                |f| f.program = Err("it is not there".to_owned()),
                "the program /usr/local/lib/lunchroom/lunchroom: it is not there",
                1,
            ),
            (
                "program not setgid",
                |f| tweak(&mut f.program, |n| n.mode = 0o755),
                "does not have the set-group-id bit",
                1,
            ),
            (
                "program in another group",
                |f| tweak(&mut f.program, |n| n.group = 1000),
                "is not in group lunchroom",
                1,
            ),
            (
                "program writable",
                |f| tweak(&mut f.program, |n| n.mode = 0o2775),
                "can be changed by a user other than root",
                1,
            ),
            (
                "folder open to others",
                |f| tweak(&mut f.folder, |n| n.mode = 0o2775),
                "has mode 2775, not 2770",
                1,
            ),
            (
                "folder the user's own",
                |f| tweak(&mut f.folder, |n| n.owner = 1000),
                "is not owned by root",
                1,
            ),
            (
                "folder missing",
                |f| f.folder = Err("it is not there".to_owned()),
                "the store's folder /var/lib/lunchroom/1000: it is not there",
                1,
            ),
            (
                "verifier world readable",
                |f| tweak(&mut f.verifier, |n| n.mode = 0o644),
                "has mode 0644, not 0640",
                1,
            ),
            (
                "verifier root group",
                |f| tweak(&mut f.verifier, |n| n.group = 0),
                "is not in group lunchroom",
                1,
            ),
            (
                "verifier folder writable",
                |f| tweak(&mut f.verifier_dir, |n| n.mode = 0o775),
                "the folder of the verifier",
                1,
            ),
            (
                "user in the group",
                |f| f.members.push("dev".to_owned()),
                "your account dev is in the group lunchroom",
                1,
            ),
        ];
        for (name, break_it, said, count) in cases {
            let mut f = good();
            break_it(&mut f);
            let broken = fails(&f);
            assert_eq!(broken.len(), count, "{name}: {broken:?}");
            assert!(
                broken.iter().any(|l| l.contains(said)),
                "{name}: {said} in {broken:?}"
            );
            assert!(
                broken.iter().all(|l| l.starts_with("the store wall")),
                "{broken:?}"
            );
        }
        // The primary group counts as membership too.
        let mut f = good();
        f.primary_gid = f.group;
        assert_eq!(fails(&f).len(), 1, "{:?}", fails(&f));
    }

    // §14::33-35, §17::104: the console line for an installed console, an answer of no,
    // a rule not in force, a record that cannot be used, and nothing.
    #[test]
    fn the_console_line_follows_the_record() {
        let mut f = good();
        assert!(
            matches!(console_line(&f), ConsoleLine::Note(t) if t.contains("`lunchroom install console`"))
        );
        f.console_installed = true;
        assert!(matches!(console_line(&f), ConsoleLine::Pass(_)));
        for (kind, reason) in [
            (
                Kind::Declined,
                "you answered no to the rule that makes sudo ask for your password",
            ),
            (Kind::NotInForce, "a later rule in /etc/sudoers wins"),
        ] {
            f.console_state = Some(Ok(State {
                kind,
                reason: reason.to_owned(),
                at: 1,
            }));
            let line = console_line(&f);
            assert!(
                matches!(&line, ConsoleLine::Fail(text)
                    if text.contains(reason) && text.contains("again asks once more")),
                "{line:?}"
            );
        }
        f.console_state = Some(Err("it is not a record".to_owned()));
        assert!(
            matches!(console_line(&f), ConsoleLine::Fail(t) if t.contains("again asks once more"))
        );
    }

    // The facts are read from disk below a root: the account database, the group database, the
    // modes of the files, and the console's record.
    #[test]
    fn the_facts_are_read_from_disk_below_a_root() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::test_target()
            .join("wall-facts")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&root);
        let put = |path: &str, text: &str, mode: u32| {
            let file = root.join(path);
            let _ = std::fs::create_dir_all(file.parent().unwrap_or(&root));
            let _ = std::fs::write(&file, text);
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode));
        };
        put(
            "etc/passwd",
            "dev:x:1000:1000:Dev:/home/dev:/bin/bash\n",
            0o644,
        );
        put(
            "etc/group",
            "dev:x:1000:\nlunchroom:x:990:alice,bob\n",
            0o644,
        );
        put("etc/lunchroom/1000.verifier", "v", 0o640);
        put("etc/lunchroom/1000.console.pub", "k", 0o644);
        put(
            "etc/lunchroom/1000.console.state",
            &State {
                kind: Kind::Declined,
                reason: "answered no".to_owned(),
                at: 5,
            }
            .to_json(),
            0o644,
        );
        put("usr/local/lib/lunchroom/lunchroom", "x", 0o2755);
        let _ = std::fs::create_dir_all(root.join("var/lib/lunchroom/1000"));
        let f = read(&root, Some(1000));
        assert_eq!(f.account.as_deref(), Some("dev"));
        assert_eq!(f.primary_gid, Some(1000));
        assert_eq!(f.group, Some(990));
        assert_eq!(f.members, ["alice", "bob"]);
        assert_eq!(f.verifier.map(|n| n.mode), Ok(0o640));
        assert!(f.console_installed);
        assert_eq!(f.program.map(|n| n.file), Ok(true));
        assert!(f.folder.is_ok_and(|n| n.dir));
        assert!(f.verifier_dir.is_ok_and(|n| n.dir));
        assert!(matches!(f.console_state, Some(Ok(ref s)) if s.reason == "answered no"));
        // A record that is not one reads as a record that cannot be used; a missing one as none.
        put("etc/lunchroom/1000.console.state", "not json", 0o644);
        assert!(matches!(
            read(&root, Some(1000)).console_state,
            Some(Err(_))
        ));
        let _ = std::fs::remove_file(root.join("etc/lunchroom/1000.console.state"));
        assert_eq!(read(&root, Some(1000)).console_state, None);
        // Nothing there: every part is missing, and that is a FAIL for each, not a panic.
        let empty = read(&root.join("none"), Some(1000));
        assert_eq!(empty.group, None);
        assert!(empty.program.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
