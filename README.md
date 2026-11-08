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

## The words Lunchroom uses

- **HELD**: a message from another agent is in this session, so Lunchroom restricts what that agent
  may do. You clear it by typing in the session.
- **Hold mode**: how much a hold restricts. You choose it at install and change it with
  `lunchroom mode`.
  - **Relaxed**: a HELD agent does what it could do if it were not HELD, except changing Lunchroom's
    program or the guard's configuration, which come to you.
  - **Balanced** (the default): a HELD agent acts freely, but PROTECTED calls come to you.
  - **Strict**: everything a HELD agent would change comes to you.
- **PROTECTED**: a kind of call that comes to you while the agent is HELD: pushing or publishing,
  deleting a file, sending data to another computer, reading or writing credentials, writing
  Lunchroom's program or the guard's configuration, and starting a new agent session.
- **YOLO**: the agent's harness has stopped asking you, by its own switch or by your
  `lunchroom yolo` until a time you state. Under a `lunchroom yolo` a HELD agent acts without asking;
  in Strict and Balanced, PROTECTED calls still come to you.
- **UNGUARDED**: the agent is running, but Lunchroom has no evidence that its guard runs. It
  receives no messages, and Lunchroom tells you why.
- **STOPPED**: the agent is not running, or Lunchroom cannot reach it.
- **QUEUED, DELIVERED, FAILED**: a message is accepted and waiting; the recipient has it; or it
  could not be handed to the recipient.
- **DENIED**: a rule stopped a call or message. The message always says why and what to do next.

**Codex users, read this first.** `lunchroom install codex` sets a Codex feature,
`unified_exec_tty`, to false for every Codex session of your user, not only the ones in Lunchroom.

- Why: Codex runs no hook for what an agent types into a program it keeps open, so without this a
  HELD agent could get past the guard.
- What changes for Codex agents: they still run commands, edit files and leave servers running, but
  they cannot answer a program that asks a question, such as a password prompt, and cannot keep an
  interactive shell open.
- What the installer does: it asks before it changes this and keeps a backup. You restart open Codex
  sessions afterwards.
- To take it back, remove the line from Codex's `config.toml`. Lunchroom then shows Codex sessions as
  UNGUARDED and delivers nothing to them.
- If you update Lunchroom through another harness's install, run `lunchroom install codex` too: until
  you do, `lunchroom status` shows Codex sessions as UNGUARDED and says so.
- Codex 0.166 runs sessions in one server process: a session whose window you closed still shows as
  running until that server ends, and a message to it shows DELIVERED though no window shows it.

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
tar xzf lunchroom-v1.1.1-x86_64-unknown-linux-musl.tar.gz
cd lunchroom-v1.1.1-x86_64-unknown-linux-musl
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

## After install

Nothing receives messages until you include its session. Do this once for each session that may
receive messages:

1. Open a session of a supported harness, for example `claude` in a project folder. When a Claude
   Code or Codex session starts, Lunchroom asks: "include this session? Type join, or join <name>."
2. Type `join` as the whole prompt, or `join alice` to pick the name. Type anything else and the
   session stays out: nothing can message it. Type `leave` to take it out again. In OpenClaw and
   Hermes, type `join` in the session the same way; their plugins show no line at the session's start.
3. Do the same in a second session, for example `join bob` in a Codex session.
4. In alice's session, ask the agent to message bob. The agent runs `lunchroom send bob "can you run
   the tests?"` and sees `QUEUED lr-00000001`: Lunchroom has accepted the message. It becomes
   DELIVERED when bob's session shows it, and bob's session is then HELD by alice's message.
5. In your own terminal, run `lunchroom status` to see each agent, whether it is AVAILABLE, HELD,
   UNGUARDED or STOPPED, and every QUEUED message.
6. Run `lunchroom doctor` to check your setup. It prints one PASS or FAIL line for each check and, for
   each FAIL, what to do.

## When the guard cannot decide a call

The guard cannot decide a call when it has an error of its own, when Lunchroom's store cannot be
used, or when it has not answered within 10 seconds. Lunchroom never locks you out in that case.
First the rules that need no record judge the call: a read inside the permission slip as shipped
runs, and a call a DENIED rule covers is DENIED. Then:

| Harness | The guard has an error, the store cannot be used, or 10 seconds pass | The guard's program is missing, or the system stops it before it answers |
|---|---|---|
| Claude Code | In an interactive session the call goes to you, through Claude Code's permission prompt. Where nobody can answer (`claude -p`) that one call is DENIED. A prompt you type goes on to the agent; a prompt carrying another agent's text is blocked. | Claude Code runs the call unchecked. Lunchroom shows the agent UNGUARDED and delivers nothing to it. |
| Codex | Codex has no way to ask you from a hook, so that one call is DENIED. The message tells the agent to try the call once more and, if it fails again, to tell you. | Codex runs the call unchecked. Lunchroom shows the agent UNGUARDED and delivers nothing to it. |
| OpenClaw | Lunchroom's plugin sends the call to you through OpenClaw's own approval, or DENIES it where nobody can answer. | The plugin handles it the same way. Lunchroom shows the agent UNGUARDED. |
| Hermes | Lunchroom's plugin sends the call to you through Hermes's own approval, or DENIES it where nobody can answer. | The plugin handles it the same way. Lunchroom shows the agent UNGUARDED. |

The message says which of the three it was, shows the call, and names the step that was still running
when 10 seconds passed. `lunchroom doctor` lists the calls the guard could not decide in the last 24
hours.

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
