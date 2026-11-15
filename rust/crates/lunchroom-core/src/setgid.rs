//! Where the store is, and how files are made, when the program runs setgid `lunchroom` (1.1).
//!
//! The installed program (`/usr/local/lib/lunchroom/lunchroom`) is owned by group `lunchroom` with
//! the set-group-id bit, and the store folder (`/var/lib/lunchroom/<uid>/`) is readable by that
//! group alone. A process the program starts has the developer's real group and the group
//! `lunchroom` as its effective group, so only the program's own code can open the store; the
//! developer's other processes cannot.
//!
//! Such a process must treat its environment as hostile, because the developer (or an agent
//! running as the developer) chose it:
//! - the store folder is the fixed `/var/lib/lunchroom/<real uid>/`, whatever `LUNCHROOM_HOME`
//!   says;
//! - the home folder comes from the account database for the real uid, not from `HOME`;
//! - the effective group is dropped before any child program starts, so a child never inherits
//!   access to the store;
//! - files and folders the program makes are readable and writable by the group, whatever the
//!   caller's `umask` says, so the console (which runs as the user `lunchroom`) can read and write
//!   them.
//!
//! Without the set-group-id bit (tests, a user-only install) none of this applies: the program
//! behaves as 1.0.x.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The folder under which each user's store folder is made.
pub const STORE_ROOT: &str = "/var/lib/lunchroom";

/// The folder under which each user's delivery folder for controller approvals is made (§17.9).
pub const APPROVALS_ROOT: &str = "/var/lib/lunchroom-approvals";

/// The folder of the `sudo` rule file of §17.8, and the start of its name
/// (`/etc/sudoers.d/zz-lunchroom-<uid>`).
pub const SUDOERS_DIR: &str = "/etc/sudoers.d";

/// The start of the name of the rule file of §17.8.
pub const SUDOERS_PREFIX: &str = "zz-lunchroom-";

/// The folder the installed program is in.
pub const PROGRAM_DIR: &str = "/usr/local/lib/lunchroom";

/// The installed program.
pub const PROGRAM: &str = "/usr/local/lib/lunchroom/lunchroom";

/// The system user and group the store and the console belong to.
pub const SYSTEM_USER: &str = "lunchroom";

/// The ids of a process: real and effective user, real and effective group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    pub euid: u32,
    pub gid: u32,
    pub egid: u32,
}

/// The ids in the text of `/proc/<pid>/status`: the first two numbers of its `Uid:` and `Gid:`
/// lines.
#[must_use]
pub fn parse_status(status: &str) -> Option<Ids> {
    let pair = |key: &str| -> Option<(u32, u32)> {
        let line = status.lines().find_map(|l| l.strip_prefix(key))?;
        let mut numbers = line.split_whitespace();
        let real = numbers.next()?.parse().ok()?;
        let effective = numbers.next()?.parse().ok()?;
        Some((real, effective))
    };
    let (user_real, user_eff) = pair("Uid:")?;
    let (group_real, group_eff) = pair("Gid:")?;
    Some(Ids {
        uid: user_real,
        euid: user_eff,
        gid: group_real,
        egid: group_eff,
    })
}

/// Whether a process with these ids runs setgid: its effective group is not its real group.
#[must_use]
pub const fn is_setgid(ids: Ids) -> bool {
    ids.egid != ids.gid
}

/// The ids of this process, from the system calls `getuid`, `geteuid`, `getgid` and `getegid`: no
/// `/proc` is needed, so a process with none still knows it runs setgid (a `LUNCHROOM_HOME` it is
/// given is then still ignored). Always `Some`.
#[must_use]
pub fn current() -> Option<Ids> {
    static IDS: OnceLock<Ids> = OnceLock::new();
    Some(*IDS.get_or_init(|| Ids {
        uid: rustix::process::getuid().as_raw(),
        euid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
        egid: rustix::process::getegid().as_raw(),
    }))
}

/// This process runs setgid.
#[must_use]
pub fn active() -> bool {
    current().is_some_and(is_setgid)
}

/// Runs `read` with this thread's effective group (and so its filesystem group) set back to the
/// real group, and sets it back to the effective group after, a panic in `read` included. A setgid
/// program cannot read another process's `/proc/<pid>/exe`, `cwd` or `environ` as it is:
/// `proc_pid_exe(5)` puts each behind "a ptrace access mode `PTRACE_MODE_READ_FSCREDS` check", and
/// `ptrace(2)` rule 3 denies it unless "the real, effective, and saved-set group IDs of the target
/// match the caller's group ID" (the filesystem group, for FSCREDS). With the group `lunchroom`
/// they never match a process of the user's (2026-11-14 real-world kit: every Codex session was
/// UNGUARDED, its program path unreadable). The saved group stays `lunchroom`, so the switch back
/// is allowed; the change is the calling thread's alone (Linux keeps group ids per thread). Where
/// the switch back fails the process stops at once rather than go on with a group it did not
/// choose. A process that does not run setgid runs `read` as it is.
pub fn as_own_group<T>(read: impl FnOnce() -> T) -> T {
    struct Back(rustix::process::Gid);
    impl Drop for Back {
        fn drop(&mut self) {
            if rustix::thread::set_thread_res_gid(None, self.0, None).is_err() {
                std::process::abort();
            }
        }
    }
    let Some(ids) = current().filter(|ids| is_setgid(*ids)) else {
        return read();
    };
    let own_group = rustix::process::Gid::from_raw(ids.gid);
    if rustix::thread::set_thread_res_gid(None, own_group, None).is_err() {
        return read();
    }
    let _back = Back(rustix::process::Gid::from_raw(ids.egid));
    read()
}

/// The store folder of user `uid` in setgid mode: `/var/lib/lunchroom/<uid>`.
#[must_use]
pub fn store_home(uid: u32) -> PathBuf {
    Path::new(STORE_ROOT).join(uid.to_string())
}

/// Lunchroom's folder for a process with `ids`: the fixed folder of the real user when the process
/// runs setgid (`user_choice` is ignored), else `user_choice` (the value of `LUNCHROOM_HOME`) or
/// `home/.lunchroom`.
#[must_use]
pub fn lunchroom_home(ids: Option<Ids>, user_choice: Option<PathBuf>, home: &Path) -> PathBuf {
    match ids {
        Some(ids) if is_setgid(ids) => store_home(ids.uid),
        _ => user_choice.unwrap_or_else(|| home.join(".lunchroom")),
    }
}

/// The home folder of `uid` in the text of an account database (`/etc/passwd`).
#[must_use]
pub fn home_from_passwd(passwd: &str, uid: u32) -> Option<PathBuf> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid) && !fields[5].is_empty())
            .then(|| PathBuf::from(fields[5]))
    })
}

/// The user id of the account `name` in the text of an account database.
#[must_use]
pub fn uid_from_passwd(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 6 && fields[0] == name)
            .then(|| fields[2].parse().ok())
            .flatten()
    })
}

/// The account name of user id `uid` in the text of an account database.
#[must_use]
pub fn name_from_passwd(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid) && !fields[0].is_empty())
            .then(|| fields[0].to_owned())
    })
}

/// The group id of the group `name` in the text of a group database (`/etc/group`).
#[must_use]
pub fn gid_from_group(group: &str, name: &str) -> Option<u32> {
    group.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 3 && fields[0] == name)
            .then(|| fields[2].parse().ok())
            .flatten()
    })
}

/// The name of the primary group of user `uid`: the group number on its line of the account
/// database, named by the group database.
#[must_use]
pub fn primary_group_name(passwd: &str, group: &str, uid: u32) -> Option<String> {
    let gid = passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid))
            .then(|| fields[3].parse::<u32>().ok())
            .flatten()
    })?;
    group.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 3 && fields[2].parse::<u32>().ok() == Some(gid) && !fields[0].is_empty())
            .then(|| fields[0].to_owned())
    })
}

/// The folder of the verifiers and of the console's keys (A1.3, §17.9).
pub const KEY_DIR: &str = "/etc/lunchroom";

/// The console's private key for user `uid` (§17.9): `/etc/lunchroom/<uid>.console.key`.
#[must_use]
pub fn console_key(uid: u32) -> PathBuf {
    Path::new(KEY_DIR).join(format!("{uid}.console.key"))
}

/// The console's public key for user `uid` (§17.9): `/etc/lunchroom/<uid>.console.pub`. Its
/// presence is the controller's signal that the console is installed.
#[must_use]
pub fn console_pub(uid: u32) -> PathBuf {
    Path::new(KEY_DIR).join(format!("{uid}.console.pub"))
}

/// The record of why the console was not set up for user `uid`, which `lunchroom doctor` reads
/// (§17.8): `/etc/lunchroom/<uid>.console.state`, root's, so no process of the user's can change
/// it.
#[must_use]
pub fn console_state(uid: u32) -> PathBuf {
    Path::new(KEY_DIR).join(format!("{uid}.console.state"))
}

/// The delivery folder for controller approvals of user `uid` (§17.9).
#[must_use]
pub fn approvals_home(uid: u32) -> PathBuf {
    Path::new(APPROVALS_ROOT).join(uid.to_string())
}

/// The rule file of §17.8 for user `uid`: `/etc/sudoers.d/zz-lunchroom-<uid>`.
#[must_use]
pub fn rule_file(uid: u32) -> PathBuf {
    Path::new(SUDOERS_DIR).join(format!("{SUDOERS_PREFIX}{uid}"))
}

/// `mode` with the group given what the owner has: `0o600` becomes `0o660`, `0o700` `0o770`.
#[must_use]
pub const fn shared_mode(mode: u32) -> u32 {
    (mode & !0o070) | ((mode & 0o700) >> 3)
}

/// Gives `path` the group access [`shared_mode`] names, when `setgid` is on. `umask` has already
/// had its say at creation, so the mode is set again here, whatever it was.
pub fn settle_if(setgid: bool, path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if !setgid {
        return;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let now = meta.permissions().mode() & 0o7777;
        let want = shared_mode(now);
        if want != now {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(want));
        }
    }
}

static SHARING: AtomicBool = AtomicBool::new(false);

/// Makes this process give the group access to every file and folder it makes, as a setgid
/// process does. The console calls it: it runs as the user `lunchroom`, not setgid, and what it
/// writes must stay readable and writable by the developer's program, which reaches the store
/// through the group.
pub fn share_with_group() {
    SHARING.store(true, Ordering::SeqCst);
}

/// This process gives the group access to what it makes.
#[must_use]
pub fn sharing() -> bool {
    active() || SHARING.load(Ordering::SeqCst)
}

/// [`settle_if`] for this process.
pub fn settle(path: &Path) {
    settle_if(sharing(), path);
}

/// Makes `command` start with the real group as its effective group when this process runs
/// setgid, so the child has no access to the store. A process that is not setgid is left alone.
pub fn drop_group(command: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt as _;
    match current() {
        Some(ids) if is_setgid(ids) => command.gid(ids.gid),
        _ => command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_SETGID: &str = "Name:\tlunchroom\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t990\t990\t990\nGroups:\t1000\n";
    const STATUS_PLAIN: &str =
        "Name:\tbash\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";

    // The mode follows the ids alone: an effective group that differs from the real group is
    // setgid, and nothing else is.
    #[test]
    fn setgid_is_an_effective_group_that_differs_from_the_real_group() {
        let ids = parse_status(STATUS_SETGID);
        assert_eq!(
            ids,
            Some(Ids {
                uid: 1000,
                euid: 1000,
                gid: 1000,
                egid: 990
            })
        );
        assert!(ids.is_some_and(is_setgid));
        assert!(!parse_status(STATUS_PLAIN).is_some_and(is_setgid));
        // A root process (both groups 0) is not setgid.
        let root = "Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n";
        assert!(!parse_status(root).is_some_and(is_setgid));
        // A status with no Gid line, or a damaged one, decides nothing.
        assert_eq!(parse_status("Uid:\t1000\t1000\n"), None);
        assert_eq!(parse_status("Uid:\t1000\t1000\nGid:\tx\ty\n"), None);
        assert_eq!(parse_status(""), None);
    }

    // The store folder of a setgid process is the fixed folder of its real user: the environment
    // value is not looked at, and neither is the home folder.
    #[test]
    fn a_setgid_process_ignores_lunchroom_home() {
        let ids = parse_status(STATUS_SETGID);
        let chosen = Some(PathBuf::from("/tmp/chosen"));
        assert_eq!(
            lunchroom_home(ids, chosen, Path::new("/home/dev")),
            PathBuf::from("/var/lib/lunchroom/1000")
        );
        assert_eq!(
            lunchroom_home(ids, None, Path::new("/home/dev")),
            PathBuf::from("/var/lib/lunchroom/1000")
        );
    }

    // Without the bit the program is the 1.0.x program: the environment value wins, else the
    // default under the home folder.
    #[test]
    fn a_plain_process_keeps_the_1_0_folders() {
        let ids = parse_status(STATUS_PLAIN);
        assert_eq!(
            lunchroom_home(
                ids,
                Some(PathBuf::from("/tmp/chosen")),
                Path::new("/home/dev")
            ),
            PathBuf::from("/tmp/chosen")
        );
        assert_eq!(
            lunchroom_home(ids, None, Path::new("/home/dev")),
            PathBuf::from("/home/dev/.lunchroom")
        );
        // Ids that cannot be read: the same.
        assert_eq!(
            lunchroom_home(None, None, Path::new("/home/dev")),
            PathBuf::from("/home/dev/.lunchroom")
        );
    }

    // The account database names the home of the real user; a uid it lacks, a short line and an
    // empty home name none.
    #[test]
    fn the_home_comes_from_the_account_database() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      dev:x:1000:1000:Dev,,,:/home/dev:/bin/bash\n\
                      short:x:1001\n\
                      nohome:x:1002:1002:::/bin/sh\n";
        assert_eq!(
            home_from_passwd(passwd, 1000),
            Some(PathBuf::from("/home/dev"))
        );
        assert_eq!(home_from_passwd(passwd, 0), Some(PathBuf::from("/root")));
        assert_eq!(home_from_passwd(passwd, 1001), None);
        assert_eq!(home_from_passwd(passwd, 1002), None);
        assert_eq!(home_from_passwd(passwd, 4242), None);
    }

    // The databases are read by name and by number; a short line, a damaged number and an unknown
    // name give nothing.
    #[test]
    fn the_account_databases_are_read_by_name_and_number() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      lunchroom:x:990:990::/nonexistent:/usr/sbin/nologin\n\
                      short:x:5\n\
                      bad:x:abc:1:::/bin/sh\n";
        assert_eq!(uid_from_passwd(passwd, "lunchroom"), Some(990));
        assert_eq!(uid_from_passwd(passwd, "root"), Some(0));
        assert_eq!(uid_from_passwd(passwd, "short"), None);
        assert_eq!(uid_from_passwd(passwd, "bad"), None);
        assert_eq!(uid_from_passwd(passwd, "nobody"), None);
        assert_eq!(name_from_passwd(passwd, 990).as_deref(), Some("lunchroom"));
        assert_eq!(name_from_passwd(passwd, 77), None);
        let group = "root:x:0:\nlunchroom:x:990:\nbroken\n";
        assert_eq!(gid_from_group(group, "lunchroom"), Some(990));
        assert_eq!(gid_from_group(group, "broken"), None);
        assert_eq!(gid_from_group(group, "absent"), None);
    }

    // Files are 0660 and folders 0770 in setgid mode; a mode the owner cannot use gives the group
    // nothing.
    #[test]
    fn group_access_copies_the_owners() {
        assert_eq!(shared_mode(0o600), 0o660);
        assert_eq!(shared_mode(0o700), 0o770);
        assert_eq!(shared_mode(0o400), 0o440);
        assert_eq!(shared_mode(0o000), 0o000);
        assert_eq!(shared_mode(0o660), 0o660);
    }

    // `settle_if` sets the mode whatever the umask made it, only when setgid is on.
    #[test]
    fn settle_sets_group_access_only_in_setgid_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::test_target().join(format!("setgid-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let file = dir.join("f");
        assert!(std::fs::write(&file, "x").is_ok());
        assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).is_ok());
        settle_if(false, &file);
        let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o7777);
        assert_eq!(mode(&file).ok(), Some(0o640));
        settle_if(true, &file);
        assert_eq!(mode(&file).ok(), Some(0o660));
        // A missing path is left alone.
        settle_if(true, &dir.join("absent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The tests run without the bit, so a child keeps whatever group it would have had.
    #[test]
    fn the_test_process_is_not_setgid() {
        assert!(!active());
    }

    // Fable L7: the ids come from the system calls, not from `/proc`, so they are always known and
    // agree with `/proc/self/status` where that is there.
    #[test]
    fn the_ids_come_from_the_system_calls() {
        let ids = current();
        assert!(ids.is_some());
        if let Some(from_proc) = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| parse_status(&s))
        {
            assert_eq!(ids, Some(from_proc));
        }
    }

    // §17.9: the delivery folder's group is the developer's primary group by name; a user with no
    // line, or a group number with no name, gives none.
    #[test]
    fn the_primary_group_is_named_from_the_two_databases() {
        let passwd = "root:x:0:0::/root:/bin/sh\ndev:x:1000:2000:Dev:/home/dev:/bin/bash\nodd:x:1001:3000::/h:/bin/sh\n";
        let group = "root:x:0:\nstaff:x:2000:dev\ndev:x:1000:\n";
        assert_eq!(
            primary_group_name(passwd, group, 1000).as_deref(),
            Some("staff")
        );
        assert_eq!(
            primary_group_name(passwd, group, 0).as_deref(),
            Some("root")
        );
        assert_eq!(primary_group_name(passwd, group, 1001), None);
        assert_eq!(primary_group_name(passwd, group, 4242), None);
        assert_eq!(primary_group_name("", group, 1000), None);
    }

    // §17.8, §17.9: the paths the console install makes, by user id.
    #[test]
    fn the_console_paths_are_the_spec_ones() {
        assert_eq!(
            console_key(1000),
            Path::new("/etc/lunchroom/1000.console.key")
        );
        assert_eq!(
            console_pub(1000),
            Path::new("/etc/lunchroom/1000.console.pub")
        );
        assert_eq!(
            console_state(7),
            Path::new("/etc/lunchroom/7.console.state")
        );
        assert_eq!(
            approvals_home(1000),
            Path::new("/var/lib/lunchroom-approvals/1000")
        );
        assert_eq!(
            rule_file(1000),
            Path::new("/etc/sudoers.d/zz-lunchroom-1000")
        );
    }
}
