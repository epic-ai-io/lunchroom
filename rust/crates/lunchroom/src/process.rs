//! Processes as the system reports them in `/proc`: parents and start times.
//!
//! A process is named by its pid and its start time (clock ticks since boot, field 22 of
//! `/proc/<pid>/stat`), so a pid the system reuses never names an earlier process.

/// The fields of `/proc/<pid>/stat` after the command name, which may itself hold spaces and `)`.
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    Some(after.split_whitespace().map(str::to_owned).collect())
}

/// The parent of `pid`.
#[must_use]
pub fn parent(pid: u32) -> Option<u32> {
    // After the command name: state (field 3), then the parent's pid (field 4).
    stat_fields(pid)?.get(1)?.parse().ok()
}

/// The controlling terminal of `pid` as the system numbers it (field 7 of `/proc/<pid>/stat`); 0 is
/// none.
#[must_use]
pub fn tty(pid: u32) -> Option<u64> {
    // After the command name: state, ppid, pgrp, session, then tty_nr.
    stat_fields(pid)?.get(4)?.parse().ok()
}

/// The program name of `pid`: the last part of its first argument.
#[must_use]
pub fn name(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let first = raw.split(|b| *b == 0).next()?;
    let text = String::from_utf8_lossy(first);
    Some(text.rsplit('/').next().unwrap_or_default().to_owned())
}

/// `<pid>:<start time>`, naming one process for as long as it runs. A process that has exited has
/// none, also while it is a zombie (`Z`, its parent has not collected it) or dead (`X`): its
/// `/proc` entry keeps the start time, but nothing runs there (F112-AC1).
#[must_use]
pub fn key(pid: u32) -> Option<String> {
    let fields = stat_fields(pid)?;
    // Field 3, the state, is the first field after the command name.
    if matches!(fields.first().map(String::as_str), Some("Z" | "X")) {
        return None;
    }
    // Field 22, the start time, is the 20th field after the command name.
    let start = fields.get(19)?.clone();
    Some(format!("{pid}:{start}"))
}

/// Now, on the clock of a process's start time: a thread started this moment reads its own.
#[must_use]
pub fn ticks_now() -> Option<u64> {
    let read = || {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
        let after = stat.rsplit_once(')')?.1;
        after.split_whitespace().nth(19)?.parse().ok()
    };
    std::thread::spawn(read).join().ok()?
}

/// This boot of the system: a process's start time counts from it.
#[must_use]
pub fn boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_owned())
}

/// `pid` and each process above it, nearest first, up to (not including) pid 1.
#[must_use]
pub fn ancestors(pid: u32) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut next = Some(pid);
    while let Some(p) = next.filter(|p| *p > 1 && chain.len() < 256) {
        chain.push(p);
        next = parent(p);
    }
    chain
}

/// The processes of user `uid` that are `program` running `listen`: a Lunchroom listener of the
/// earlier program (§17.2). `proc_root` is `/proc`, or a folder laid out like it. A process whose
/// program file was deleted since it started (`/proc/<pid>/exe` ends in ` (deleted)`) counts.
#[must_use]
pub fn listeners_in(proc_root: &std::path::Path, uid: u32, program: &std::path::Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let me = std::process::id();
    let mut found: Vec<u32> = entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            let dir = proc_root.join(pid.to_string());
            let owner = std::fs::read_to_string(dir.join("status"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("Uid:"))?
                        .split_whitespace()
                        .next()?
                        .parse::<u32>()
                        .ok()
                });
            if owner != Some(uid) {
                return false;
            }
            let exe = std::fs::read_link(dir.join("exe")).ok();
            let exe_text = exe.as_deref().and_then(std::path::Path::to_str);
            let same = exe_text.is_some_and(|t| {
                std::path::Path::new(t.strip_suffix(" (deleted)").unwrap_or(t)) == program
            });
            let listens = std::fs::read(dir.join("cmdline"))
                .is_ok_and(|raw| raw.split(|b| *b == 0).nth(1) == Some(b"listen".as_slice()));
            same && listens
        })
        .collect();
    found.sort_unstable();
    found
}

/// Whether `pid` runs: it has a folder in `proc_root` and is not a zombie.
#[must_use]
pub fn running_in(proc_root: &std::path::Path, pid: u32) -> bool {
    std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat"))
        .ok()
        .and_then(|stat| Some(stat.rsplit_once(')')?.1.trim_start().chars().next()? != 'Z'))
        .unwrap_or(false)
}

/// Sends SIGTERM to each of `pids` and waits up to `wait` for them to end; the ones still
/// running, which did not stop.
#[must_use]
pub fn terminate(proc_root: &std::path::Path, pids: &[u32], wait: std::time::Duration) -> Vec<u32> {
    for pid in pids {
        if let Some(target) = i32::try_from(*pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(target, rustix::process::Signal::TERM);
        }
    }
    let end = std::time::Instant::now() + wait;
    loop {
        let alive: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|p| running_in(proc_root, *p))
            .collect();
        if alive.is_empty() || std::time::Instant::now() >= end {
            return alive;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_stable_key_and_a_parent() {
        let me = std::process::id();
        assert!(key(me).is_some());
        assert_eq!(key(me), key(me));
        assert!(parent(me).is_some());
        let chain = ancestors(me);
        assert_eq!(chain.first(), Some(&me));
        assert!(chain.len() >= 2);
        assert_eq!(key(u32::MAX), None);
    }

    // §13 H1: `ticks_now` reads the clock a process key's start time is on, and a process
    // started after it has a later start time.
    #[test]
    fn now_is_on_the_clock_of_a_process_start_time() {
        let start = |pid: u32| {
            let key = key(pid).unwrap_or_default();
            key.split(':').nth(1).and_then(|s| s.parse::<u64>().ok())
        };
        let now = ticks_now();
        assert!(now.is_some() && now >= start(std::process::id()));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let child = std::process::Command::new("sleep").arg("1").spawn();
        assert!(child.is_ok());
        let Ok(mut child) = child else { return };
        assert!(start(child.id()) > now);
        let _ = child.kill();
        let _ = child.wait();
    }

    // F112-AC1: a session's process is running while it runs; once it has exited it is not, both
    // while it is a zombie (exited, its parent, this test, has not collected it) and after it is
    // collected. Found by the 1.1.1 dogfood upgrade: an uncollected Codex was shown UNGUARDED.
    #[test]
    fn f112_ac1_an_exited_process_has_no_key_whether_or_not_it_is_collected() {
        let state = |pid: u32| stat_fields(pid).and_then(|f| f.first().cloned());
        let child = std::process::Command::new("sleep").arg("30").spawn();
        assert!(child.is_ok());
        let Ok(mut child) = child else { return };
        let pid = child.id();
        assert!(key(pid).is_some(), "a running process has a key");
        let _ = child.kill();
        // Not waited for yet: the child is a zombie until this test collects it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state(pid).as_deref() != Some("Z") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            state(pid).as_deref(),
            Some("Z"),
            "the child did not become a zombie"
        );
        assert_eq!(key(pid), None, "a zombie is not running");
        let _ = child.wait();
        assert_eq!(key(pid), None, "a collected process is not running");
    }

    fn fake_proc(name: &str) -> std::path::PathBuf {
        let dir = crate::test_target()
            .join("process-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn fake_process(proc: &std::path::Path, pid: u32, uid: u32, exe: &str, argv: &[&str]) {
        let dir = proc.join(pid.to_string());
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("status"),
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        );
        let _ = std::os::unix::fs::symlink(exe, dir.join("exe"));
        let _ = std::fs::write(dir.join("cmdline"), argv.join("\0") + "\0");
    }

    // §17.2: the listeners the install stops are the user's processes whose program is the
    // earlier one and whose first argument is `listen`: not another user's, not another program,
    // not another command of the same program, and a program whose file was replaced counts.
    #[test]
    fn only_the_users_listeners_of_the_earlier_program_are_found() {
        let proc = fake_proc("listeners");
        let old = "/home/u/.lunchroom/bin/lunchroom";
        fake_process(&proc, 100, 1000, old, &[old, "listen"]);
        fake_process(
            &proc,
            101,
            1000,
            "/home/u/.lunchroom/bin/lunchroom (deleted)",
            &[old, "listen", "--x"],
        );
        fake_process(&proc, 102, 1001, old, &[old, "listen"]);
        fake_process(&proc, 103, 1000, "/usr/bin/sleep", &[old, "listen"]);
        fake_process(&proc, 104, 1000, old, &[old, "status"]);
        fake_process(&proc, 105, 1000, old, &[old]);
        fake_process(
            &proc,
            106,
            1000,
            "/usr/local/lib/lunchroom/lunchroom",
            &["x", "listen"],
        );
        let _ = std::fs::create_dir_all(proc.join("self"));
        let _ = std::fs::write(proc.join("loadavg"), "0 0 0");
        assert_eq!(
            listeners_in(&proc, 1000, std::path::Path::new(old)),
            vec![100, 101]
        );
        assert_eq!(
            listeners_in(
                &proc,
                1000,
                std::path::Path::new("/usr/local/lib/lunchroom/lunchroom")
            ),
            vec![106]
        );
        assert_eq!(
            listeners_in(&proc, 1000, std::path::Path::new("/nowhere")).len(),
            0
        );
        assert_eq!(
            listeners_in(&proc.join("missing"), 1000, std::path::Path::new(old)).len(),
            0
        );
        let _ = std::fs::remove_dir_all(&proc);
    }

    // §17.2: SIGTERM stops a process that obeys it, and a process that ignores it is reported as
    // still running after the wait.
    #[test]
    fn terminate_stops_one_that_obeys_and_reports_one_that_ignores_sigterm() {
        let proc = std::path::Path::new("/proc");
        let spawn = |script: &str| {
            std::process::Command::new("/bin/sh")
                .args(["-c", script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        };
        let (Ok(mut obeys), Ok(mut ignores)) = (
            spawn("exec sleep 30"),
            spawn("trap '' TERM; while :; do sleep 1; done"),
        ) else {
            return;
        };
        // Let the shell set its trap before the signal comes.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(running_in(proc, obeys.id()) && running_in(proc, ignores.id()));
        let left = terminate(
            proc,
            &[obeys.id(), ignores.id()],
            std::time::Duration::from_millis(1500),
        );
        assert_eq!(left, vec![ignores.id()]);
        assert!(!running_in(proc, obeys.id()));
        let _ = ignores.kill();
        let _ = ignores.wait();
        let _ = obeys.wait();
        assert!(!running_in(proc, u32::MAX));
    }
}
