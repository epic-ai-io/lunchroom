//! Tests of the console's wall (1.1): the guard rules the kernel leaves open. Each rule has a call
//! it DENIES and the neighbour it allows.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::Guard;
use crate::fs::{BUDGET, OsFs};
use crate::paths::Layout;
use crate::policy::{CallClass, DenyReason, ProtectedKind};

const CONFIG: CallClass = CallClass::Denied(DenyReason::Configuration);
const DENIED_PATH: CallClass = CallClass::Denied(DenyReason::DeniedPath);
const OTHER_SESSION: CallClass = CallClass::Protected(ProtectedKind::OtherSession);

/// A layout for a user `u` whose agent's harness is this test process, with a terminal, a tmux pane
/// and a screen session of its own, and a console that runs as process 4242.
fn layout() -> Layout {
    let mut l = Layout::for_home(Path::new("/home/u"));
    l.own_terminals = vec![PathBuf::from("/dev/pts/3")];
    l.agent_root = Some(std::process::id());
    l.console_pid = Some(4242);
    l.own_pane = Some("%3".to_owned());
    l.own_screen = Some("123.pts-0.host".to_owned());
    l
}

fn class(held: bool, tool: &str, input: &Value) -> CallClass {
    let l = layout();
    let fs = OsFs::new(BUDGET);
    Guard {
        layout: &l,
        fs: &fs,
        held,
    }
    .classify(tool, input, Path::new("/tmp"))
}

fn bash(held: bool, command: &str) -> CallClass {
    class(held, "Bash", &json!({ "command": command }))
}

/// The part of the call at fault, as the DENIED message shows it.
fn fault(command: &str) -> Option<String> {
    let l = layout();
    let fs = OsFs::new(BUDGET);
    Guard {
        layout: &l,
        fs: &fs,
        held: false,
    }
    .classify_explained("Bash", &json!({ "command": command }), Path::new("/tmp"))
    .1
}

fn denied(command: &str) {
    for held in [false, true] {
        assert!(
            matches!(bash(held, command), CallClass::Denied(_)),
            "held={held}: {command} is {:?}",
            bash(held, command)
        );
    }
}

fn allowed(command: &str) {
    assert!(
        !matches!(bash(false, command), CallClass::Denied(_)),
        "{command} is {:?}",
        bash(false, command)
    );
}

// Rule 1: a terminal device other than the agent's own controlling terminal is DENIED, to read or
// to write, in every session, with the device as the part at fault; the agent's own terminal,
// `/dev/tty`, the folder `/dev/pts` and a device that is no terminal are not.
#[test]
fn rule_1_another_terminal_is_denied_and_the_agents_own_is_not() {
    for command in [
        "cat /dev/pts/7",
        "echo hi > /dev/pts/7",
        "cat /dev/tty1",
        "cat /dev/ttyS0",
        "cat /dev/vcs1",
        "cat /dev/vcsa",
        "stty -a < /dev/pts/0",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    assert_eq!(fault("cat /dev/pts/7").as_deref(), Some("/dev/pts/7"));
    for command in [
        "cat /dev/pts/3",
        "echo hi > /dev/pts/3",
        "echo hi > /dev/tty",
        "ls /dev/pts",
        "cat /dev/null",
        "echo hi > /dev/stderr",
    ] {
        allowed(command);
    }
    // The file tools are judged the same way.
    for tool in ["Read", "Write"] {
        let denied_call = class(
            false,
            tool,
            &json!({ "file_path": "/dev/pts/9", "content": "x" }),
        );
        assert_eq!(denied_call, CONFIG, "{tool}");
        let own = class(
            false,
            tool,
            &json!({ "file_path": "/dev/pts/3", "content": "x" }),
        );
        assert!(!matches!(own, CallClass::Denied(_)), "{tool}: {own:?}");
    }
}

// Rule 2: kill, pkill and killall that name the console's pid or the word lunchroom are DENIED;
// a signal to another process is not.
#[test]
fn rule_2_signalling_the_console_is_denied() {
    for command in [
        "kill 4242",
        "kill -9 4242",
        "kill -KILL 4242",
        "kill -4242",
        "pkill lunchroom",
        "pkill -f lunchroom",
        "pkill -u lunchroom",
        "killall lunchroom",
        "kill -9 $(pgrep lunchroom)",
        "pkill -f /usr/local/lib/lunchroom/lunchroom",
        "sudo kill -9 4242",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    assert_eq!(fault("kill -9 4242").as_deref(), Some("kill -9 4242"));
    for command in [
        "kill 4243",
        "kill -9 42420",
        "pkill node",
        "killall firefox",
        "pkill -f my-lunchroom-app",
        "kill -l",
    ] {
        allowed(command);
    }
}

// Rule 3: gdb, strace, ltrace and ptrace through python or perl are DENIED, alone or behind a
// wrapper; `/proc/<pid>/{mem,environ,fd,maps}` of a process outside the agent's own subtree is
// DENIED, and one inside it (the harness, its children) is not.
#[test]
fn rule_3_debuggers_and_tracing_are_denied() {
    for command in [
        "gdb -p 1234",
        "strace -p 1",
        "ltrace ls",
        "sudo strace -f ls",
        "env FOO=1 gdb --batch",
        "python3 -c 'import ctypes; ctypes.CDLL(None).ptrace(16, 1234, 0, 0)'",
        "perl -e 'require \"syscall.ph\"; syscall(&SYS_ptrace, 16, 1234, 0, 0)'",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    for command in [
        "python3 -c 'print(1)'",
        "perl -e 'print 1'",
        "ps aux",
        "git diff",
    ] {
        allowed(command);
    }
}

#[test]
fn rule_3_other_processes_secrets_are_denied_and_the_agents_own_are_not() {
    // Process 1 is outside the agent's subtree, whoever the agent is.
    for command in [
        "cat /proc/1/environ",
        "cat /proc/1/mem",
        "ls /proc/1/fd",
        "cat /proc/1/maps",
        "strings /proc/4242/environ",
    ] {
        assert_eq!(bash(false, command), DENIED_PATH, "{command}");
        denied(command);
    }
    let me = std::process::id();
    // The harness itself, and a child of it, are the agent's own.
    let child = std::process::Command::new("sleep").arg("5").spawn();
    let Ok(mut child) = child else {
        return;
    };
    for command in [
        format!("cat /proc/{me}/environ"),
        format!("ls /proc/{}/fd", child.id()),
        "cat /proc/self/environ".to_owned(),
        "cat /proc/loadavg".to_owned(),
        "cat /proc/1/status".to_owned(),
    ] {
        allowed(&command);
    }
    let _ = child.kill();
    let _ = child.wait();
}

// Rule 4: tmux send-keys, capture-pane and pipe-pane, and screen -X stuff and hardcopy, are
// PROTECTED when HELD if they act on the agent's own pane or session, and DENIED for any other.
#[test]
fn rule_4_another_pane_is_denied_and_the_agents_own_is_protected_when_held() {
    for command in [
        "tmux send-keys -t %9 'rm -rf x' Enter",
        "tmux send -t %9 x",
        "tmux capture-pane -p -t %9",
        "tmux capturep -t main:0.1 -p",
        "tmux pipe-pane -t %9 'cat >> /tmp/x'",
        "tmux -L other send-keys -t %3 x",
        "tmux -S /tmp/sock send-keys x",
        "screen -S other -X stuff 'ls'",
        "screen -r other -X hardcopy /tmp/x",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    for command in [
        "tmux send-keys -t %3 'ls' Enter",
        "tmux send-keys x",
        "tmux capture-pane -p",
        "screen -X stuff 'ls'",
        "screen -S pts-0 -X hardcopy /tmp/x",
        "screen -S 123.pts-0.host -X stuff 'ls'",
    ] {
        allowed(command);
        assert_eq!(bash(true, command), OTHER_SESSION, "held: {command}");
    }
    for command in [
        "tmux list-panes",
        "tmux new-session -d",
        "screen -ls",
        "screen -X title x",
    ] {
        allowed(command);
        assert!(
            !matches!(bash(true, command), CallClass::Denied(_)),
            "held: {command}"
        );
    }
}

// Rule 5: sudo, doas, su, runuser, machinectl and pkexec as the user or group lunchroom are
// DENIED, and so is any mention of the stores, the passphrase checks and the program folder; an
// ordinary sudo, and running the installed program, are not.
#[test]
fn rule_5_becoming_lunchroom_is_denied() {
    for command in [
        "sudo -u lunchroom ls",
        "sudo -ulunchroom id",
        "sudo --user=lunchroom id",
        "sudo --user lunchroom id",
        "sudo -g lunchroom cat /x",
        "sudo --group=lunchroom id",
        "su lunchroom",
        "su - lunchroom",
        "runuser -u lunchroom -- id",
        "runuser -l lunchroom",
        "pkexec --user lunchroom id",
        "doas -u lunchroom id",
        "machinectl shell lunchroom@.host",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    // `su` with a shell option is a command the guard cannot read; it is DENIED either way.
    denied("su -s /bin/sh lunchroom");
    for command in [
        "sudo apt update",
        "sudo -u postgres psql",
        "sudo apt install lunchroom",
        "su postgres",
        "runuser -u postgres -- id",
        "machinectl list",
    ] {
        allowed(command);
    }
}

#[test]
fn rule_5_the_stores_and_the_program_folder_are_denied() {
    for command in [
        "cat /var/lib/lunchroom/1000/store/state.json",
        "ls /var/lib/lunchroom",
        "echo x > /var/lib/lunchroom/1000/console.alive",
        "cat /etc/lunchroom/1000.verifier",
        "echo x > /etc/lunchroom/1000.verifier",
        "touch /usr/local/lib/lunchroom/helper",
        "cp /bin/sh /usr/local/lib/lunchroom/lunchroom",
        "cat /usr/local/lib/lunchroom/lunchroom",
    ] {
        assert_eq!(bash(false, command), CONFIG, "{command}");
        denied(command);
    }
    assert_eq!(
        class(
            false,
            "Write",
            &json!({ "file_path": "/var/lib/lunchroom/1000/x", "content": "" })
        ),
        CONFIG
    );
    assert_eq!(
        class(
            false,
            "Write",
            &json!({ "file_path": "/usr/local/lib/lunchroom/lunchroom", "content": "" })
        ),
        CONFIG
    );
    // Running the installed program by its path with a command an agent may run is not refused by
    // these rules. (The program is not installed on the machine that runs this test, so the
    // classifier's rule that a program named by a path must exist refuses it for that reason.)
    for command in [
        "/usr/local/lib/lunchroom/lunchroom send codex hi",
        "/usr/local/lib/lunchroom/lunchroom status",
    ] {
        assert_ne!(bash(false, command), CONFIG, "{command}");
    }
    allowed("ls /usr/local/lib");
    allowed("cat /etc/hostname");
}

// Rule 6: running the Lunchroom program with `console` is DENIED, by name, by the installed path
// and in code; the commands an agent may run are not.
#[test]
fn rule_6_the_console_command_is_the_users_alone() {
    for command in [
        "lunchroom console",
        "/usr/local/lib/lunchroom/lunchroom console",
        "sudo -u lunchroom /usr/local/lib/lunchroom/lunchroom console",
        "python3 -c \"import os; os.system('lunchroom console')\"",
        "sh -c 'lunchroom console'",
    ] {
        assert!(
            matches!(bash(false, command), CallClass::Denied(_)),
            "{command}"
        );
        denied(command);
    }
    assert_eq!(bash(false, "lunchroom console"), CONFIG);
    assert_eq!(
        fault("lunchroom console").as_deref(),
        Some("lunchroom console")
    );
    allowed("lunchroom send codex hi");
    allowed("lunchroom status");
    allowed("lunchroom help");
}
