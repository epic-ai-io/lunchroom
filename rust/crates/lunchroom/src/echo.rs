//! The echo watchdog (spec §7 A1.3, `§7::46-47`; U8's limit 1): "The program shall turn
//! echo back on before it exits, also when it is interrupted."
//!
//! SIGINT, SIGTERM, SIGHUP and SIGQUIT are caught by [`crate::terminal::EchoOff`], which restores
//! echo before the signal's default action ends the process. SIGKILL cannot be caught, so the
//! process that turns echo off first starts a small watcher: this same program, run as the hidden
//! subcommand `echo-watch`, with the terminal as its standard output and the read end of a pipe as
//! its standard input. The main process holds the write end and nothing else does (a pipe that
//! Rust opens is close-on-exec). When the main process ends the pipe closes, however it ended; the
//! watcher then reads end-of-file and turns echo back on. A main process that restored echo itself
//! first writes one `x` to the pipe, so the watcher exits without touching a terminal the shell
//! may have handed to another program since.
//!
//! The watcher runs with `SIGTTOU` ignored: when the main process was the foreground job and is
//! gone, the shell takes the terminal back, the watcher's group is then a background group, and
//! changing the terminal's modes from one would stop it. Signals that are ignored stay ignored
//! across `exec`, so `sh -c 'trap "" TTOU; exec …'` starts it that way without any `unsafe`.

use std::fs::File;
use std::io::Write as _;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

/// The hidden subcommand the watcher runs as.
pub const SUBCOMMAND: &str = "echo-watch";

/// The byte the main process writes when it has restored echo itself.
const DONE: u8 = b'x';

/// How long the main process waits for the watcher to exit once told it is done.
const WAIT: Duration = Duration::from_secs(2);

/// A running watcher. Dropping it tells the watcher that echo has been restored and waits for it.
#[derive(Debug)]
pub struct Watchdog {
    child: Child,
}

/// Starts the watcher for the terminal `tty`, whose modes before echo went off had `echo` and
/// `echonl` as given. `Ok(None)` where this build starts none (the unit tests of the program
/// itself, whose own executable is the test harness).
///
/// # Errors
/// Why the watcher could not be started: echo must then stay on, since nothing would turn it back
/// on if the program were killed.
pub fn start(tty: &File, echo: bool, echonl: bool) -> Result<Option<Watchdog>, String> {
    if cfg!(test) {
        return Ok(None);
    }
    let program = std::env::current_exe()
        .map_err(|e| format!("this program's own file cannot be found: {e}"))?;
    let tty = tty
        .try_clone()
        .map_err(|e| format!("the terminal cannot be duplicated: {e}"))?;
    let shell = ["/bin/sh", "/usr/bin/sh"]
        .into_iter()
        .map(std::path::Path::new)
        .find(|p| p.is_file())
        .ok_or("no /bin/sh was found to start the echo watcher with")?;
    let mut command = Command::new(shell);
    command
        .args(["-c", "trap '' TTOU; exec \"$0\" \"$@\""])
        .arg(&program)
        .arg(SUBCOMMAND)
        .arg(if echo { "1" } else { "0" })
        .arg(if echonl { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::from(tty))
        .stderr(Stdio::null());
    lunchroom_core::setgid::drop_group(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("the echo watcher cannot be started: {e}"))?;
    Ok(Some(Watchdog { child }))
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(mut pipe) = self.child.stdin.take() {
            let _ = pipe.write_all(&[DONE]);
            let _ = pipe.flush();
        }
        let end = Instant::now() + WAIT;
        loop {
            match self.child.try_wait() {
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
                Ok(Some(_)) | Err(_) => return,
            }
        }
    }
}

/// Turns the `echo` and `echonl` modes that were on back on, on the terminal `tty`.
pub fn restore(tty: impl rustix::fd::AsFd, echo: bool, echonl: bool) -> bool {
    let Ok(mut modes) = tcgetattr(&tty) else {
        return false;
    };
    if echo {
        modes.local_modes.insert(LocalModes::ECHO);
    }
    if echonl {
        modes.local_modes.insert(LocalModes::ECHONL);
    }
    tcsetattr(&tty, OptionalActions::Now, &modes).is_ok()
}

/// What the watcher does when `control` ends: an `x` means the main process restored echo itself;
/// anything else (end of input, an error, another byte) means it is gone, and echo is turned on.
fn main_process_restored(mut control: impl std::io::Read) -> bool {
    let mut byte = [0u8; 1];
    loop {
        match control.read(&mut byte) {
            Ok(1) => return byte[0] == DONE,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // End of input, or any other error.
            Ok(_) | Err(_) => return false,
        }
    }
}

/// `lunchroom echo-watch <echo> <echonl>`: the watcher. Its standard output is the terminal and its
/// standard input the pipe from the main process. It prints nothing.
#[must_use]
pub fn watch(args: &[String]) -> ExitCode {
    let on = |k: usize| args.get(k).is_some_and(|a| a == "1");
    let (echo, echonl) = (on(0), on(1));
    let tty = rustix::stdio::stdout();
    // Standard output that is not a terminal has nothing to restore.
    if tcgetattr(tty).is_err() {
        return ExitCode::from(2);
    }
    if main_process_restored(std::io::stdin()) {
        return ExitCode::SUCCESS;
    }
    if restore(tty, echo, echonl) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The watcher is told the main process restored echo by an `x`, and by nothing else: the end
    // of the pipe (the main process was killed), an empty read and any other byte all mean echo
    // is to be turned on.
    #[test]
    fn only_an_x_means_the_main_process_restored_echo() {
        assert!(main_process_restored(&b"x"[..]));
        assert!(!main_process_restored(&b""[..]));
        assert!(!main_process_restored(&b"y"[..]));
        assert!(!main_process_restored(&b"\n"[..]));
    }

    // The watcher is not started by the unit tests of the program, whose own executable is the
    // test harness; a terminal is not needed to find that out.
    #[test]
    fn no_watcher_is_started_under_the_unit_tests() {
        let file = File::open("/dev/null");
        assert!(file.is_ok());
        let Ok(file) = file else { return };
        assert!(matches!(start(&file, true, false), Ok(None)));
    }
}
