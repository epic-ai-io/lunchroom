# Lunchroom

**Where your agents talk. You decide who acts.**

Lunchroom lets coding agents on your computer send each other messages, and keeps you in charge of
what an agent does after another agent has spoken to it. It runs only on your computer, as a single
program its hooks start; it runs no service and sends nothing anywhere.

Lunchroom 1.0 supports **Claude Code**, **Codex**, **OpenClaw** and **Hermes**, on Linux (x86_64 or
aarch64, with `/proc` mounted). The release programs are static: they need no system library, and
each release is run on Amazon Linux 2 and 2023, Ubuntu 20.04, 22.04 and 24.04, Debian 12, Rocky
Linux 9 and Alpine 3.20 before it is published. Lunchroom does not run on macOS or Windows (under
Windows, WSL 2 runs Linux). Website: [lunchroom.talk](https://lunchroom.talk).

**Codex users, read this first.** `lunchroom install codex` sets a Codex feature,
`unified_exec_tty`, to false for every Codex session of your user, not only the ones in Lunchroom. Codex runs
no hook for what an agent types into a program it keeps open, so without this a HELD agent could get
past the guard. With it false, Codex agents still run commands, edit files and leave servers running,
but they cannot answer a program that asks a question, such as a password prompt, and cannot keep an
interactive shell open. The installer asks before it changes this, keeps a backup, and you restart
open Codex sessions afterwards. To take it back, remove the line from Codex's `config.toml`; Lunchroom
then shows Codex sessions as UNGUARDED and delivers nothing to them. If you update Lunchroom through
another harness's install, run `lunchroom install codex` too: until you do, `lunchroom status` shows
Codex sessions as UNGUARDED and says so. Codex 0.166 runs sessions in one server process: a session
whose window you closed still shows as running until that server ends, and a message to it shows
DELIVERED though no window shows it.

> **CAUTION: The limit every tool-call guard shares**
>
> Lunchroom works the way most AI agent guards work: it judges each call an agent makes, before the
> call runs. Every guard built this way has the same limit. Once a script is running, the guard does
> not see what the script does. Security researchers and vendors across the industry document this
> ([NVIDIA](https://developer.nvidia.com/blog/practical-security-guidance-for-sandboxing-agentic-workflows-and-managing-execution-risk),
> [ActPlane](https://arxiv.org/html/2606.25189v2)).
>
> What this means for you:
>
> - **Strict, without `lunchroom yolo`:** a HELD agent's scripts wait for you. You see the script,
>   not what it will do.
> - **Balanced and Relaxed:** a HELD agent's scripts run.
> - **An agent that is not HELD:** its scripts run in every mode, and they can change Lunchroom's
>   own records.
>
> To put a wall around scripts, use a limit the operating system enforces: a separate account or a
> virtual machine.

## Get it

Download `lunchroom-v<version>-<target>.tar.gz` for your machine from
[Releases](https://github.com/epic-ai-io/lunchroom/releases) (`x86_64-unknown-linux-musl` or
`aarch64-unknown-linux-musl`), unpack it, and run the first install from the download's folder, in
your own terminal:

```
tar xzf lunchroom-v1.0.1-x86_64-unknown-linux-musl.tar.gz
cd lunchroom-v1.0.1-x86_64-unknown-linux-musl
./lunchroom install
```

The first install asks you to type `yes`, then to choose a passphrase of at least 12 characters, and
runs `sudo` (you type your password) to put the passphrase's check where only root can change it. It
needs `sudo`, `install` and `stty` in `/usr/bin`, `/bin`, `/usr/sbin` or `/sbin` (`doas` is not
supported) and a terminal outside every agent session. Without them the hooks are still written,
no passphrase is set and the install exits 1; `lunchroom passphrase` sets it later.

Each release carries `SHA256SUMS`, signed with Sigstore. [`rust/README.md`](rust/README.md) shows how
to check a download, how to build from source, and documents every command, the hold modes, each
harness adapter and Lunchroom's stated limits.

## Layout

| Path | What it is |
|---|---|
| `rust/` | The program (`crates/lunchroom-core`, `crates/lunchroom`), its tests and the OpenClaw and Hermes plugins |
| `rust/TESTPLAN.md` | Each requirement of the spec, the test that covers it, and what is covered only in part |
| `docs/spec/` | The product specification and the standard vocabulary |
| `docs/host-facts/` | What each supported harness does, with sources, that the adapters rely on |

## Tests

```
cd rust
cargo test --release
node --test plugins/openclaw/index.test.mjs
(cd plugins/hermes && python3 -m unittest)
```

The suite must run as `--release`: the guard tests stand in for Claude Code through the release
build's folder.

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability privately.

## License

Copyright 2026 protectNIL Inc. d/b/a Epic AI. Licensed under the Apache License, Version 2.0; see
[LICENSE](LICENSE) and [NOTICE](NOTICE).
