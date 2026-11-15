//! Codex classes H (other processes) and I (terminals) of build unit U2.

use std::io::BufRead as _;
use std::process::{Child, Command, Stdio};

use super::u2_enum::{bash, collect, guard_class, is_denied, report, strs, wall};
use super::*;

/// A sleeping child of this process and a sleeping child of that child.
struct Tree {
    child: Child,
    grandchild: u32,
}

impl Tree {
    fn start() -> Option<Self> {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let grandchild = line.trim().parse().ok()?;
        Some(Self { child, grandchild })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg(self.grandchild.to_string())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A sleeping child of this process.
fn sleeper() -> Option<Child> {
    Command::new("sleep").arg("60").spawn().ok()
}

// H4, H5, H17 — §17::53, §17::54: a debugger given a process id is DENIED unless the id
// is written in the call and the process is in the agent's own subtree: a child, a deeper
// descendant; not the root's parent, a sibling, the init process, nor a number no process has.
#[test]
#[allow(clippy::too_many_lines)] // one row per spelling
fn h4_h5_h17_a_debugger_is_judged_by_the_process_the_id_names() {
    let h = Fixture::new();
    let Some(tree) = Tree::start() else { return };
    let Some(mut sibling) = sleeper() else { return };
    let mut layout = wall(&h);
    // The agent's subtree is rooted at `tree.child`.
    layout.agent_root = Some(tree.pid());
    let (own, deep, sib, parent) = (
        tree.pid(),
        tree.grandchild,
        sibling.id(),
        std::os::unix::process::parent_id(),
    );
    let mut bad = Vec::new();
    for spelling in [
        "strace -p {}",
        "strace -p{}",
        "strace -fp {}",
        "strace -f -p {}",
        "strace --attach={}",
        "strace --attach {}",
        "ltrace -p {}",
        "ltrace -p{}",
        "ltrace --pid {}",
        "ltrace --pid={}",
        "gdb -p {}",
        "gdb --pid {}",
        "gdb --pid={}",
        "gdb -batch -p {}",
        "gdb --batch -ex 'attach {}'",
        "gdb ./prog {}",
        "sudo strace -p {}",
        "env A=1 gdb -p {}",
    ] {
        for (pid, inside) in [
            (own, true),
            (deep, true),
            (sib, false),
            (parent, false),
            (1, false),
            (4242, false),
        ] {
            let cmd = spelling.replace("{}", &pid.to_string());
            for held in [false, true] {
                let class = bash(&h, &layout, held, &cmd);
                if is_denied(class) == inside {
                    bad.push(format!(
                        "H4/H17 held={held} {cmd:?}: {class:?}, wanted {}",
                        if inside { "not DENIED" } else { "DENIED" }
                    ));
                }
            }
        }
    }
    // A list or a second `-p` with one id outside the subtree.
    for cmd in [
        format!("strace -p {own} -p 1"),
        format!("strace -p 1 -p {own}"),
        format!("strace -p {own},1"),
        format!("strace -p '{own} 1'"),
        format!("gdb -p {own} -p 1"),
        format!("ltrace -p {own} -p 1"),
    ] {
        for held in [false, true] {
            let class = bash(&h, &layout, held, &cmd);
            if !is_denied(class) {
                bad.push(format!("H4 held={held} {cmd:?}: {class:?}, wanted DENIED"));
            }
        }
    }
    // H5: an id that names no process, or none the way a program reads it, is never allowed.
    for id in [
        "0",
        "-1",
        "4294967295",
        "4294967296",
        "99999999999999999999",
        "00001",
        "+1",
        "' 1'",
        "'1 '",
        "1x",
        "0x1",
        "1.0",
        "''",
        "$((1))",
        "$(echo 1)",
    ] {
        for tool in ["strace -p", "gdb -p", "ltrace -p"] {
            let cmd = format!("{tool} {id}");
            for held in [false, true] {
                let class = bash(&h, &layout, held, &cmd);
                if !is_denied(class) {
                    bad.push(format!("H5 held={held} {cmd:?}: {class:?}, wanted DENIED"));
                }
            }
        }
    }
    for cmd in [
        "strace -p",
        "gdb -p",
        "ltrace -p",
        "gdb --pid=",
        "strace --attach=",
    ] {
        for held in [false, true] {
            let class = bash(&h, &layout, held, cmd);
            if !is_denied(class) {
                bad.push(format!("H5 held={held} {cmd:?}: {class:?}, wanted DENIED"));
            }
        }
    }
    let _ = sibling.kill();
    let _ = sibling.wait();
    report(&bad);
}

// H7, H8, H9 — §17::53: a read of `/proc/<pid>/{mem,environ,fd,maps}` of a process
// outside the agent's subtree is DENIED however the path is spelled and whatever reads it; the
// agent's own (`self`, its children) and the other files of /proc are not.
#[test]
#[allow(clippy::too_many_lines)] // one row per spelling
fn h7_h8_h9_proc_paths_and_readers() {
    let h = Fixture::new();
    let Some(tree) = Tree::start() else { return };
    let mut layout = wall(&h);
    layout.agent_root = Some(tree.pid());
    std::os::unix::fs::symlink("/proc/1", h.home.join("project/lnk-p1")).unwrap_or_default();
    std::os::unix::fs::symlink("/proc", h.home.join("project/lnk-proc")).unwrap_or_default();
    let (own, deep) = (tree.pid(), tree.grandchild);
    let mut denied = Vec::new();
    for file in ["mem", "environ", "fd", "maps"] {
        for p in [
            format!("/proc/1/{file}"),
            format!("/proc/01/{file}"),
            format!("/proc/0001/{file}"),
            format!("/proc//1/{file}"),
            format!("/proc/./1/{file}"),
            format!("//proc/1/{file}"),
            format!("/proc/1/../1/{file}"),
            format!("/proc/{own}/../1/{file}"),
            format!("/proc/1/task/1/{file}"),
            format!("/proc/1/root/proc/1/{file}"),
            format!("lnk-p1/{file}"),
            format!("lnk-proc/1/{file}"),
            format!("/proc/1/{file}/"),
        ] {
            denied.push(format!("cat {p}"));
        }
        denied.push(format!("cd /proc/1 && cat {file}"));
        denied.push(format!("cd /proc && cat 1/{file}"));
        denied.push(format!("cd /proc/1/task && cat 1/{file}"));
        // Whatever reads it.
        for reader in [
            "cat",
            "head -c 10",
            "tail -n 1",
            "less",
            "more",
            "strings",
            "xxd",
            "od -c",
            "hexdump -C",
            "grep -a x",
            "grep -r x",
            "awk 1",
            "sed -n p",
            "cut -c1-5",
            "base64",
            "tr a b <",
            "wc -c",
            "sort",
        ] {
            denied.push(format!("{reader} /proc/1/{file}"));
        }
        denied.push(format!("dd if=/proc/1/{file} of=/tmp/x"));
        denied.push(format!("cp /proc/1/{file} /tmp/x"));
        denied.push(format!("rsync /proc/1/{file} /tmp/x"));
        denied.push(format!("cat < /proc/1/{file}"));
        denied.push(format!("exec 3< /proc/1/{file}"));
        denied.push(format!(
            "python3 -c \"print(open('/proc/1/{file}').read())\""
        ));
        denied.push(format!("perl -e 'open(F, \"/proc/1/{file}\"); print <F>'"));
        denied.push(format!("ruby -e 'puts File.read(\"/proc/1/{file}\")'"));
        denied.push(format!("sh -c 'cat /proc/1/{file}'"));
        denied.push(format!("sudo cat /proc/1/{file}"));
        denied.push(format!("env A=1 cat /proc/1/{file}"));
        denied.push(format!("timeout 5 cat /proc/1/{file}"));
        denied.push(format!("tar -cf /tmp/t.tar /proc/1/{file}"));
        denied.push(format!("find /proc/1 -name {file} -exec cat {{}} ;"));
    }
    let allowed: Vec<String> = [
        "cat /proc/loadavg",
        "cat /proc/1/status",
        "cat /proc/1/cmdline",
        "cat /proc/1/stat",
        "cat /proc/self/maps",
        "cat /proc/self/environ",
        "cat /proc/thread-self/maps",
        "ls /proc",
        "ls /proc/1",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain([
        format!("cat /proc/{own}/maps"),
        format!("cat /proc/{own}/environ"),
        format!("cat /proc/{deep}/maps"),
        format!("cat /proc/{deep}/fd"),
        format!("cat /proc/{own}/task/{own}/maps"),
    ])
    .collect();
    let mut bad = Vec::new();
    collect(&h, &layout, "H7/H8", &denied, true, &mut bad);
    // The other files of /proc and the agent's own: a HELD session may read only what the slip
    // lets it (L1.1), so these are judged for a session that is not HELD.
    for cmd in &allowed {
        let class = bash(&h, &layout, false, cmd);
        if is_denied(class) {
            bad.push(format!("H9 {cmd:?}: {class:?}, wanted not DENIED"));
        }
    }
    // Names only, no content: not a read of the secret files.
    for cmd in strs(&[
        "ls -R /proc/1",
        "du -a /proc/1",
        "tree /proc/1",
        "find /proc/1 -type f",
        "find /proc -maxdepth 2 -name environ",
        "grep -r x /proc/self",
        "ls -R /proc",
    ]) {
        let class = bash(&h, &layout, false, &cmd);
        if is_denied(class) {
            bad.push(format!("H9 {cmd:?}: {class:?}, wanted not DENIED"));
        }
    }
    // Everything below /proc or a process's folder, read: the secret files are read.
    let mut recursive = Vec::new();
    for dir in [
        "/proc",
        "/proc/1",
        "/proc/1/",
        "/proc/1/task",
        "/proc/1/task/1",
        "/proc//1",
        "/proc/../proc/1",
    ] {
        for cmd in [
            "grep -r x {}",
            "grep -R x {}",
            "grep --recursive x {}",
            "grep -rn x {}",
            "egrep -r x {}",
            "rg x {}",
            "ag x {}",
            "ack x {}",
            "cp -r {} /tmp/x",
            "cp -a {} /tmp/x",
            "tar cf /tmp/t.tar {}",
            "rsync -a {} /tmp/x",
            "zip -r /tmp/z.zip {}",
            "scp -r {} host:/tmp",
            "find {} -exec cat {} +",
            "find {} -name x -execdir cat {} ;",
            "find {} -ok cat {} ;",
            "fd -x cat . {}",
            "sudo grep -r x {}",
            "sh -c 'grep -r x {}'",
        ] {
            recursive.push(cmd.replace("{}", dir));
        }
    }
    collect(&h, &layout, "H7/H8-recursive", &recursive, true, &mut bad);
    // The agent's own folder is its own.
    for cmd in [
        format!("grep -r x /proc/{own}"),
        format!("cp -r /proc/{deep} /tmp/x"),
        format!("find /proc/{own}/task -exec cat {{}} +"),
    ] {
        let class = bash(&h, &layout, false, &cmd);
        if is_denied(class) {
            bad.push(format!("H9 {cmd:?}: {class:?}, wanted not DENIED"));
        }
    }
    report(&bad);
}

// H11, H12 — §17::56: ptrace through python or perl is DENIED by the text of the code:
// by name, by system call number in any base, and where the number or the name is not written
// as one literal (the target is set when the code runs, and the guard judges code by its text).
#[test]
fn h11_h12_ptrace_in_code_by_number_and_by_computed_name() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = Vec::new();
    for call in [
        "libc.syscall(101, 16, 1, 0, 0)",
        "libc.syscall(0x65, 16, 1, 0, 0)",
        "libc.syscall(0o145, 16, 1, 0, 0)",
        "libc.syscall(0b1100101, 16, 1, 0, 0)",
        "libc.syscall(26, 16, 1, 0, 0)",
        "libc.syscall(117, 16, 1, 0, 0)",
        "libc.syscall(101 + 0, 16, 1, 0, 0)",
        "libc.syscall(100 + 1, 16, 1, 0, 0)",
        "libc.syscall(n, 16, 1, 0, 0)",
        "libc.syscall(NR, 16, 1, 0, 0)",
        "libc.syscall(int('65', 16), 16, 1, 0, 0)",
        "libc.syscall(ord('e'), 16, 1, 0, 0)",
        "libc.syscall(*args)",
        "libc.syscall (101, 16, 1, 0, 0)",
        "libc.syscall(\n101, 16, 1, 0, 0)",
        "libc.syscall(  0x65,16,1,0,0)",
        "getattr(libc, 'pt' + 'race')(16, 1, 0, 0)",
        "getattr(libc, 'pt' 'race')(16, 1, 0, 0)",
        "getattr(libc, 'ptr' + 'ace')(16, 1, 0, 0)",
        "getattr(libc, ''.join(['pt', 'race']))(16, 1, 0, 0)",
        "getattr(libc, chr(112) + chr(116) + chr(114) + chr(97) + chr(99) + chr(101))(16)",
        "getattr(libc, 'PTRACE'.lower())(16)",
        "getattr(libc, 'ecartp'[::-1])(16)",
        "getattr(libc, codecs.decode('cgenpr', 'rot13'))(16)",
        "getattr(libc, base64.b64decode('cHRyYWNl').decode())(16)",
        "getattr(libc, base64.b64decode('UFRSQUNF').decode().lower())(16)",
        "getattr(libc, bytes.fromhex('707472616365').decode())(16)",
        "getattr(libc, '\\x70trace')(16)",
        "getattr(libc, 'p\\164race')(16)",
        "libc.PTRACE_ATTACH",
        "libc.ptrace(16, 1, 0, 0)",
    ] {
        rows.push(format!(
            "python3 -c \"import ctypes; libc = ctypes.CDLL(None); {}\"",
            call.replace('\n', " ")
        ));
    }
    for call in [
        "syscall(101, 16, 1, 0, 0)",
        "syscall(0x65, 16, 1, 0, 0)",
        "syscall(0145, 16, 1, 0, 0)",
        "syscall($n, 16, 1, 0, 0)",
        "syscall(&SYS_ptrace, 16, 1, 0, 0)",
        "syscall(100 + 1, 16, 1, 0, 0)",
        "syscall(26, 16, 1, 0, 0)",
        "syscall (117, 16, 1, 0, 0)",
        "&{\"sys\" . \"call\"}(101, 16, 1)",
        "syscall(eval '101', 16, 1, 0, 0)",
    ] {
        rows.push(format!("perl -e '{call}'"));
    }
    let mut bad = Vec::new();
    collect(&h, &layout, "H11/H12", &rows, true, &mut bad);
    // Calls that are no ptrace.
    collect(
        &h,
        &layout,
        "H11-near",
        &strs(&[
            "perl -e 'syscall()'",
            "python3 -c 'import ctypes; ctypes.CDLL(None).syscall()'",
            "perl -e 'syscall(39)'",
            "perl -e 'syscall(0x27)'",
            "python3 -c 'import ctypes; ctypes.CDLL(None).syscall(39)'",
            "python3 -c 'import ctypes; ctypes.CDLL(None).syscall(0x27, 0)'",
            "python3 -c 'print(101)'",
            "python3 -c 'print(\"syscall\")'",
            "python3 -c 'x = 26'",
            "perl -e 'print 101'",
        ]),
        false,
        &mut bad,
    );
    report(&bad);
}

// H13 — gdb's commands given on its command line that name another process or run a shell.
#[test]
fn h13_a_debuggers_own_commands_are_judged() {
    let h = Fixture::new();
    let layout = wall(&h);
    let mut rows = Vec::new();
    for ex in [
        "attach 1",
        "att 1",
        "attach $P",
        "attach $(pgrep sshd)",
        "call (int)ptrace(16, 1, 0, 0)",
        "call (int)PTRACE_ATTACH",
        "call (int)syscall(n, 16, 1, 0, 0)",
        "call (long)syscall(101, 16, 1, 0, 0)",
        "print ptrace(16, 1, 0, 0)",
        "shell cat /proc/1/mem",
        "shell id",
        "! id",
        "python import os",
        "pi import os",
        "py print(1)",
        "source cmds.gdb",
        "core-file /proc/1/mem",
        "detach",
    ] {
        for flag in [
            "-ex",
            "-iex",
            "--eval-command",
            "--init-eval-command",
            "-eval-command",
        ] {
            if flag.contains("eval") && flag.starts_with("--") {
                rows.push(format!("gdb --batch {flag}='{ex}'"));
            }
            rows.push(format!("gdb --batch {flag} '{ex}' ./prog"));
        }
    }
    // lldb's own options for a command: the program is its own, the command ptraces another.
    for flag in ["-o", "-O", "--one-line", "--one-line-before-file"] {
        rows.push(format!(
            "lldb -b {flag} 'expr (int)ptrace(16, 1, 0, 0)' ./prog"
        ));
        rows.push(format!(
            "lldb -b {flag}='expr (int)ptrace(16, 1, 0, 0)' ./prog"
        ));
        rows.push(format!(
            "lldb -b {flag} 'expr (long)syscall(n, 16, 1)' ./prog"
        ));
    }
    let mut bad = Vec::new();
    // `detach` names no process and runs none; every other command is judged.
    let (detach, rest): (Vec<String>, Vec<String>) =
        rows.into_iter().partition(|r| r.contains("'detach'"));
    collect(&h, &layout, "H13", &rest, true, &mut bad);
    collect(&h, &layout, "H13-near", &detach, false, &mut bad);
    report(&bad);
}

// I2, I3, I7, I8, I9 — §12::19, §17::50: a terminal device other than the agent's own is
// DENIED however it is opened, linked or spelled; the agent's own and the ordinary devices are
// not; a missing, empty or odd operand is judged without a panic.
#[test]
fn i2_i3_i7_i8_i9_other_terminals_by_every_reader_and_spelling() {
    let h = Fixture::new();
    let layout = wall(&h);
    std::os::unix::fs::symlink("/dev/pts/7", h.home.join("project/lnk-tty")).unwrap_or_default();
    std::os::unix::fs::symlink("lnk-tty", h.home.join("project/lnk-tty2")).unwrap_or_default();
    std::os::unix::fs::symlink("/dev", h.home.join("project/lnk-dev")).unwrap_or_default();
    let mut denied = Vec::new();
    for p in [
        "/dev/pts/7",
        "/dev/pts/7/",
        "/dev//pts/7",
        "/dev/./pts/7",
        "/dev/pts/../pts/7",
        "//dev/pts/7",
        "/dev/tty1",
        "/dev/ttyS0",
        "/dev/vcs1",
        "/dev/vcsa2",
        "/dev/vcsu3",
        "/dev/pts/03",
        "/dev/pts/0009",
        "/dev/pts/30",
        "lnk-tty",
        "lnk-tty2",
        "lnk-dev/pts/7",
        "lnk-dev/tty1",
        "/dev/pts/*",
        "/dev/pts/[0-9]",
        "/dev/tty[1-3]",
        "/dev/tty?",
    ] {
        for reader in [
            "cat {}",
            "head -c 1 {}",
            "tail -f {}",
            "dd if={} of=/tmp/x",
            "tee {}",
            "tee -a {}",
            "script -q {}",
            "script -qc ls {}",
            "stty -a -F {}",
            "stty < {}",
            "echo hi > {}",
            "echo hi >> {}",
            "cat < {}",
            "read x < {}",
            "exec 3< {}",
            "exec 3<> {}",
            "python3 -c \"open('{}').read()\"",
            "python3 -c \"open('{}', 'w').write('x')\"",
            "perl -e 'open(F, \"{}\")'",
            "ruby -e 'File.read(\"{}\")'",
            "sh -c 'cat {}'",
            "sudo cat {}",
            "env A=1 cat {}",
            "timeout 5 cat {}",
            "ls -l {}",
            "readlink -f {}",
            "cp {} /tmp/x",
        ] {
            denied.push(reader.replace("{}", p));
        }
    }
    denied.push("cd /dev && cat pts/7".to_owned());
    denied.push("cd /dev/pts && cat 7".to_owned());
    denied.push("cd /dev && echo x > tty1".to_owned());
    let allowed = strs(&[
        "cat /dev/pts/3",
        "echo hi > /dev/pts/3",
        "echo hi > /dev/tty",
        "cat /dev/tty",
        "cat /dev/null",
        "echo hi > /dev/null",
        "cat /dev/zero | head -c 1",
        "dd if=/dev/urandom bs=1 count=1",
        "cat /dev/stdin",
        "ls /dev/pts",
        "ls /dev",
        "ls -l /dev/pts/",
        "cat \"\"",
        "cat /dev/pts/",
        "cd /dev/pts && ls",
        "stty -a",
        "tty",
    ]);
    let mut bad = Vec::new();
    collect(&h, &layout, "I2/I3/I7/I8", &denied, true, &mut bad);
    collect(&h, &layout, "I7/I9-near", &allowed, false, &mut bad);
    // I9: a path that is no device, no path at all, or holds a NUL: judged without a panic, and
    // a NUL after a terminal device name does not hide it.
    for held in [false, true] {
        for p in [
            "/dev/pts/7\u{0}x",
            "/dev/tty1\u{0}",
            "/dev/pts/7\u{0}/dev/null",
        ] {
            let class = guard_class(&h, &layout, held, "Read", &json!({ "file_path": p }));
            if !is_denied(class) {
                bad.push(format!(
                    "I9 held={held} Read {p:?}: {class:?}, wanted DENIED"
                ));
            }
        }
        for p in [
            "",
            " ",
            "/dev/pts/x",
            "/dev/pts/-1",
            "/dev/pts/99999999999999999999",
        ] {
            let _ = guard_class(&h, &layout, held, "Read", &json!({ "file_path": p }));
            let _ = bash(&h, &layout, held, &format!("cat '{p}'"));
        }
    }
    report(&bad);
}
