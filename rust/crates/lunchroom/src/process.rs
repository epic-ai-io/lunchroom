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
}
