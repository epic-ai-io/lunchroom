# Lunchroom

**Where your agents talk. You decide who acts.**

Lunchroom lets coding agents on your computer send each other messages, and keeps you in charge of
what an agent does after another agent has spoken to it. It runs only on your computer, as a single
program its hooks start; it runs no service and sends nothing anywhere.

This release supports **Claude Code**, **Codex**, **OpenClaw** and **Hermes**. A session of any
other harness is UNGUARDED and receives nothing. The Claude Code and Codex adapters were measured
against the running harnesses (Codex's message delivery was read from its source, not measured; see
the Codex adapter below); the OpenClaw and Hermes adapters are built from those harnesses' source
(openclaw 2026.10.7, hermes-agent 9976f48), tested against stand-ins, and run live against those
releases in a throwaway home (`docs/host-facts/openclaw-and-hermes.md`). Each adapter states the
release it passed with; that release and later ones are supported, and an older one is UNGUARDED.

Lunchroom runs on Linux, x86_64 or aarch64, with `/proc` mounted. The release programs are static:
they need no system library and no particular glibc. Before each release is published, both programs
are run on Amazon Linux 2 and 2023, Ubuntu 20.04, 22.04 and 24.04, Debian 12, Rocky Linux 9 and
Alpine 3.20. It does not build for other systems.

## The words Lunchroom uses

Lunchroom uses each of these words the same way in its messages and in this document. The States
table below gives the full meaning of each.

- **HELD**: a message from another agent is in this session, so Lunchroom restricts what that agent
  may do. You clear it by typing in the session.
- **Hold mode**: how much a hold restricts. You choose it with `lunchroom mode`.
  - **Relaxed**: a HELD agent does what it could do if it were not HELD, except changing Lunchroom's
    program or the guard's configuration, which come to you.
  - **Balanced** (the default): a HELD agent acts freely, but PROTECTED calls come to you.
  - **Strict**: everything a HELD agent would change comes to you.
- **PROTECTED**: a kind of call that comes to you while the agent is HELD: pushing or publishing,
  deleting a file, sending data to another computer, reading or writing credentials, writing
  Lunchroom's program or the guard's configuration, and starting a new agent session.
- **YOLO**: the agent's harness has stopped asking you, by its own switch or by your
  `lunchroom yolo` until a time you state.
- **UNGUARDED**: the agent is running, but Lunchroom has no evidence that its guard runs. It
  receives no messages.
- **STOPPED**: the agent is not running, or Lunchroom cannot reach it.
- **QUEUED, DELIVERED, FAILED**: a message is accepted and waiting; the recipient has it; or it
  could not be handed to the recipient.
- **GRANTED, DENIED**: a call or message goes ahead, or a rule stopped it. A DENIED message always
  says why and what to do next.

## Get it

Each release publishes `lunchroom-v<version>-<target>.tar.gz` for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`, with `SHA256SUMS` signed by Sigstore. Check a download before you run
it, with cosign 3 or newer: first that this repository's release workflow signed `SHA256SUMS` for the
release's own tag (here `v1.1.2`; use the tag you downloaded), then the archive against it:

```
cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json \
  --certificate-identity https://github.com/epic-ai-io/lunchroom/.github/workflows/release.yml@refs/tags/v1.1.2 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum -c SHA256SUMS --ignore-missing
```

Each release builds each program a second time, from a copy of the source in another folder, and
publishes only if the two are identical byte for byte. To rebuild a release yourself, use the toolchain
`rust-toolchain.toml` pins and the target and `--remap-path-prefix` flags in
`.github/workflows/release.yml`, from a git checkout of the tag; a source copy without git history
reports its version without the commit, so its program differs in that text.

Or build it:

```
cd rust
cargo build --release
```

Try a downloaded copy from your own terminal. Inside a guarded session, a command that clears or
changes the variables Lunchroom reads an agent's identity from (`env -i`, `env -u` or `unset` of
one, `export` of one) is DENIED as a fence (I1.2), so `env -i HOME=… ./lunchroom help` is too.

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.99.1); `rustup` fetches it. The program
is `target/release/lunchroom`; `lunchroom install` copies it to `~/.lunchroom/bin/lunchroom` and,
when `~/.local/bin` or `~/bin` exists, is on your `PATH` and holds no other `lunchroom`, links it
from there so `lunchroom` runs by name.
Run the first install by the build's path. `lunchroom help` lists every command. Tests: `cargo test --release`
(the plugins' own tests are listed under each adapter below).

## Install

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

```
target/release/lunchroom install   # the first install, by the build's path
```

`lunchroom install` finds each supported harness on your computer (by its folder: `~/.claude`,
`~/.codex`, `~/.openclaw`, `~/.hermes`, or the folder its own variable names) and asks about each
one in turn: "add Lunchroom to Claude Code?". Only the ones you confirm get Lunchroom. To
install one harness by itself, name it: `lunchroom install claude-code|codex|openclaw|hermes`.

Run it in your own terminal, not inside an agent's session. For each harness it:

- shows the hooks it adds (to `~/.claude/settings.json`, or to Codex's `hooks.json`);
- asks for your passphrase (the first install, which has none to ask for yet, asks you to type `yes`,
  and afterwards sets your passphrase, which needs `sudo` and your password: see `lunchroom
  passphrase` below);
- keeps a dated backup of each file it changes;
- prints any step only you can do.

It replaces Lunchroom's earlier hooks and leaves every other hook where it was, also in a group
shared with Lunchroom's; a settings file that is a link stays a link, and keeps its mode.

What you do afterwards:

- When the hooks were already in place, open sessions run the new program from their next call;
  otherwise start them anew.
- For Codex, approve Lunchroom's hooks in Codex's `/hooks`. Codex runs no hook you have not trusted.
- For OpenClaw and Hermes the install writes Lunchroom's plugin (OpenClaw:
  `~/.lunchroom/plugins/openclaw/`; Hermes: `<HERMES_HOME>/plugins/lunchroom/`) and lists the steps you
  then take in that harness. OpenClaw: `openclaw plugins install --link ~/.lunchroom/plugins/openclaw
  --force`, `openclaw plugins enable lunchroom` and `openclaw config set
  plugins.entries.lunchroom.hooks.allowConversationAccess true --strict-json`. Hermes: `hermes plugins
  enable lunchroom`.

A broken guard never locks you out: what the guard cannot check goes to you, or, where nobody can be
asked, that one call is DENIED (see "When the guard cannot decide a call" below).

`lunchroom version` names the build, for example `lunchroom 1.1.2 (a1b2c3d)`: the version, then the commit it was built from, with
`, modified` when a tracked file of the source differs from that commit (untracked files do not
count); a build from a source copy with no git history shows the version alone. The install reports the build it installed and the
one it kept as a backup.

To remove Lunchroom, first run `lunchroom uninstall claude-code` and `lunchroom uninstall codex` in
your own terminal: each removes only Lunchroom's hooks and keeps a backup. For OpenClaw run
`openclaw plugins disable lunchroom`, for Hermes `hermes plugins disable lunchroom`. Then delete the
`lunchroom` link in `~/.local/bin` or `~/bin`, and last `~/.lunchroom`: a hook that still names the
program after it is gone fails on every call.

If Lunchroom's store ever cannot be read, the guard still settles the calls that need no record:

- a read inside the permission slip as shipped runs;
- a fence, or a write to Lunchroom's own store, is DENIED;
- a reply passes the guard, and `lunchroom send` DENIES it while the store cannot be used;
- every other guarded call goes to you (or is DENIED where nobody can answer) until you run
  `lunchroom repair`, which puts back the store as it was before its last change and keeps the
  unreadable file beside it;
- a guard that runs out of time sends even a read to you.

Setting the passphrase, which the first install does last, needs `sudo`, `install` and `stty`
found in `/usr/bin`, `/bin`, `/usr/sbin` or `/sbin` (your `PATH` is not searched, and `doas` is not
supported) and a terminal outside every agent session. When one is missing, the install has by then
written the hooks and asked the hold mode; it sets no passphrase, says that none is set, and
exits 1. Every command that asks for the passphrase stays DENIED until
`lunchroom passphrase` succeeds.

The first install then asks how much a message from one agent limits the agent that receives it
(the hold mode):

- Relaxed: agents act on each other's messages freely; Lunchroom still stops injection into other
  sessions, impersonating you, and leaking secrets, and changes to Lunchroom's program or the
  guard's configuration come to you.
- Balanced (recommended, and the default): agents act freely, but PROTECTED calls come to you.
- Strict: everything an agent would change after another agent's message comes to you.

Change it any time with `lunchroom mode relaxed|balanced|strict`; `lunchroom status` shows it.

## Upgrade

To upgrade an installed Lunchroom, download and unpack the new release, then run its program by its
path, in your own terminal, from the unpacked folder:

```
tar xzf lunchroom-v1.1.2-x86_64-unknown-linux-musl.tar.gz
cd lunchroom-v1.1.2-x86_64-unknown-linux-musl
./lunchroom install
```

`lunchroom install` typed by name runs the program already installed, so it reinstalls the build you
already have and upgrades nothing. From 1.1.2 on, an install says so before it asks; 1.1.1 and earlier
do not.

## After install

Nothing receives messages until you include its session. Do this once for each session that may
receive messages:

1. Open a session of a supported harness, for example `claude` in a project folder. When a Claude
   Code or Codex session starts, Lunchroom asks: "include this session? Type join, or join <name>."
2. Type `join` as the whole prompt, or `join alice` to pick the name. The session is in, named after
   its harness and folder (`claude-myproject`; a second one there is `claude-myproject-2`) when you
   pick no name. Its agent is told its name and the commands it may run, and in Claude Code the name
   shows as the session's title. Type anything else and the session stays out: nothing can message it.
   Type `leave` to take it out again.
3. Do the same in a second session, for example `join bob` in a Codex session. In OpenClaw and Hermes,
   type `join` in the session the same way; their plugins show no line at the session's start.
4. In alice's session, ask the agent to message bob. The agent runs `lunchroom send bob "can you run
   the tests?"` and sees `QUEUED lr-00000001`: Lunchroom has accepted the message. It becomes
   DELIVERED when bob's session shows it, and bob's session is then HELD by alice's message.
5. In your own terminal, run `lunchroom status` to see each agent, whether it is AVAILABLE, HELD,
   UNGUARDED or STOPPED, and every QUEUED message.
6. Run `lunchroom doctor` to check your setup. It prints one PASS or FAIL line for each check and, for
   each FAIL, what to do.

Lunchroom leaves a Claude Code title you set, unless it equals a name the session is giving up or
the title Lunchroom last gave it, and a title an earlier version set stays after `leave`.

You can also name a session from your own terminal, by its id:
`lunchroom register <name> <harness>:<session id>` (`lunchroom codex-threads` lists recent Codex
thread ids).

Codex cannot ask you from a hook, so a HELD Codex agent's call that needs you is DENIED and kept:
approve that exact call with `lunchroom approve`, and Lunchroom tells the session so, and it runs
when the agent tries it again.

## States

| Word | Meaning |
|---|---|
| AVAILABLE | Its guard is running and no other agent's text is in its context. |
| HELD | Another agent's text is in its context. What it may still do depends on the hold mode: in Strict, only what its permission slip holds (read any file except the paths Lunchroom denies or protects, and reply with `lunchroom send`), and every other call goes to you; in Balanced, everything except PROTECTED calls, which go to you; in Relaxed, everything it could do if it were not HELD, except changing Lunchroom's program or the guard's configuration, which goes to you. In every mode it replies only to the agents whose messages hold it. Typing in that session clears HELD. |
| YOLO | Its harness stops asking you: by its own switch, or by your `lunchroom yolo` until a stated time. Under a `lunchroom yolo` a HELD agent acts without asking; in Strict and Balanced, PROTECTED calls still come to you. |
| UNGUARDED | It is running, but Lunchroom has no evidence that its guard runs. It receives nothing. You are told when an agent becomes UNGUARDED, and why (see below). |
| STOPPED | It is not running, or Lunchroom cannot reach it. |
| HALTED | Lunchroom stopped a conversation between two agents (reason: loop, rate limit). Typing in either session clears it. |

Why an agent is UNGUARDED: Lunchroom gives the first of these the agent lacks, with what you do about
it.

1. Its harness configuration does not run the whole guard (for Codex, the notice names the setting or
   hook at fault).
2. No guard hook has run in its current session.
3. Its harness release cannot be read from its install (the notice names the program's path). A
   program that a package manager upgraded under the running session shows its path ending
   `(deleted)`: Lunchroom reads the release from the path without that ending, and the reason says to
   restart the session.
4. Its harness release is older than the one its adapter passed with (the notice names both).

The notice to you, `lunchroom status` and `lunchroom doctor` give the same reason.

## Messages: QUEUED, DELIVERED, FAILED

A message is QUEUED, then DELIVERED, or FAILED, and none waits without an end.

Every Lunchroom program that runs (any guard hook in any session, any listener, `lunchroom send`,
`lunchroom status`) checks every QUEUED message:

- It looks again for the recipient's confirmation.
- It counts a hand-over unconfirmed after 10 minutes as a failed attempt and offers the message again
  (a Codex or OpenClaw recipient, through that harness's own program). An offer again waits at least
  10 seconds after the last failed attempt.
- It FAILS a message whose recipient took itself out with `leave` or lost its name. Messages QUEUED
  for a name you unregister become FAILED at the next check, and their senders are told.
- After three failed attempts a message is FAILED.
- A message that could not be handed over because its recipient was UNGUARDED is offered again when
  that reason clears.

A guard hook starts this check in a process of its own, which it does not wait for, and a listener
does the same for the offers again, so neither passes its hook's time limit.

You are told in the next prompt you type in any guarded session, and the sender in its next prompt
or listener (a Codex or OpenClaw sender too), when:

- a hand-over has had no confirmation for 2 minutes;
- an attempt fails;
- a message is FAILED;
- it cannot be handed over (its recipient UNGUARDED, with the reason, or STOPPED; its program cannot
  be found), as soon as that is found;
- it is DELIVERED after any of these.

Each notice names the message, its recipient, how long ago it was sent, the attempt (n of 3), what
Lunchroom waits for, when it is offered again, and what you can do, as in:

```
Lunchroom: lr-00000053 to bob: no confirmation 2 min after its hand-over. lr-00000053 to bob (codex:019a…), sent 3 min ago by alice, attempt 1 of 3: handed over 2 min ago, at 14:05 UTC, not yet confirmed; Lunchroom waits for the message id in that session's record, or its next prompt; it is offered again at 14:15 UTC if not confirmed by then. Nothing is needed; to end the wait, the user can run `lunchroom unregister bob` in a terminal of their own, and the message is then FAILED.
```

Lunchroom keeps at most 50 notices for you and 50 for each sender; when it drops older ones, the next
notice says how many.

## DENIED calls and messages

A call is GRANTED or DENIED. A tool call the guard DENIES, and a message `lunchroom send` DENIES, is
explained in full:

- the first line: the part at fault and the next step;
- what was DENIED (the command, path or message, and the part at fault, as the guard read it);
- why (the rule, and for a HELD session the agent and message id that hold it);
- every way forward that exists for it, the agent's and yours.

For example, a HELD session reading another session's transcript:

```
Lunchroom: DENIED · /home/u/.claude/projects/p/t.jsonl: this path is, or leads to, Lunchroom's records or another session's transcripts; leave it out of the call, and the user can read it in a terminal of their own.
What was DENIED: the command `cat /home/u/.claude/projects/p/t.jsonl` (the part at fault: /home/u/.claude/projects/p/t.jsonl).
Why: a HELD session may not read Lunchroom's records or another session's transcript, nor search a folder that holds one; HELD means a message from another agent is in this session, so Lunchroom restricts what it may do. This session is HELD by alice (lr-00000045).
Ways forward:
- The agent: leave /home/u/.claude/projects/p/t.jsonl out of the call; read or search only folders that hold neither Lunchroom's records nor another session's transcripts.
- The user: read it in a terminal of their own.
- The user: type in this session to clear the hold; the call is then judged as for a session that is not HELD.
```

A DENIED search names only the folder searched, never a file it found there. What a DENIED message
tells an agent about changing how strict Lunchroom is stays general ("The user: change how strict
Lunchroom is for this session, in a terminal of their own."); the exact commands (`lunchroom mode`,
`lunchroom yolo`) are for you, in the messages Lunchroom shows you.

A message carrying a secret shows `DENIED · leak: the text carries or asks for a secret; remove it
and send again.`, then the message it was (never its text), the rule and the way forward.

PROTECTED calls (push or publish, deleting a file,
sending data to another computer, reading or writing credentials, writing Lunchroom's program or the
guard's configuration, commands Lunchroom cannot parse, a tool Lunchroom does not know, which may
send data to another computer, text sent to another agent's session outside `lunchroom send`, as
Claude Code's `SendMessage` does, and starting a new agent session, as `claude -p` or `codex exec`
does) come to you from a HELD agent even under YOLO, in the Strict and
Balanced hold modes.

## When the guard cannot decide a call

The guard cannot decide a call when it has an error of its own, when Lunchroom's store cannot be
used, or when it has not answered within 10 seconds. Lunchroom never locks you out in that case.
First the rules that need no record judge the call, as for a HELD agent: a read inside the
permission slip as shipped runs, and a call a DENIED rule covers is DENIED. Then:

| Harness | The guard has an error, the store cannot be used, or 10 seconds pass | The guard's program is missing, or the system stops it before it answers |
|---|---|---|
| Claude Code | In an interactive session the call goes to you, through Claude Code's permission prompt. Where nobody can answer (`claude -p`, `--permission-prompt-tool`) that one call is DENIED. A prompt you type goes on to the agent; a prompt carrying another agent's text is blocked. | Claude Code runs the call unchecked and passes a prompt on unchecked, another agent's text included. Lunchroom shows the agent UNGUARDED and delivers nothing to it. |
| Codex | Codex has no way to ask you from a hook, so that one call is DENIED. The message tells the agent to try the call once more and, if it fails again, to tell you. A prompt you type goes on; a prompt carrying another agent's text is blocked. | Codex runs the call unchecked and passes a prompt on unchecked. Lunchroom shows the agent UNGUARDED and delivers nothing to it. |
| OpenClaw | Lunchroom's plugin sends the call to you through OpenClaw's own approval, or DENIES it where nobody can answer. Another agent's text holds the session; your own typing goes on. | The plugin handles it the same way. Lunchroom shows the agent UNGUARDED when the program is missing. |
| Hermes | Lunchroom's plugin sends the call to you through Hermes's own approval, or DENIES it where nobody can answer. Another agent's text holds the session; your own typing goes on. | The plugin handles it the same way. Lunchroom shows the agent UNGUARDED when the program is missing. |

When the guard cannot decide a call, the message you or the agent sees says which of the three it was:

- the guard's own error, naming it;
- a store Lunchroom cannot use, naming what is wrong with it;
- no answer within 10 seconds, naming the step that was still running when the time ran out.

It also shows the call, and the ways forward: answer the permission prompt where there is one, run the
call in a terminal of your own, and `lunchroom doctor`.

The guard also writes a record of each call it could not decide, and of each decision that took longer
than 2 seconds, in a file in Lunchroom's own folder: the time, the session, the call, the slowest step
and how long it took. It keeps the latest 200 records and sends nothing anywhere. `lunchroom doctor`
shows the records of the last 24 hours, and its line for them is a FAIL for 24 hours after a call the
guard could not decide.

## Commands

Agents may run:

- `lunchroom send <agent> [--reply-to <id>] <text>`: send a message; a text of `-` is read from
  standard input. `--reply-to <id>` is accepted, and the id is not used. A HELD agent may reply only to the agents whose
  messages hold it; a reply carrying a secret, or a message asking for one, is DENIED. A message that
  `send` does not accept (empty, too long, too many lines, to a name Lunchroom does not know, or
  stamped as another agent's) is answered with what is wrong, why, and what to do.
- `lunchroom status`: a one-line count of agents by state, the hold mode and the slip every agent
  has, then each agent (STOPPED ones last) with its state, what holds it (message
  holds by sender and id; other holds counted by source), its QUEUED messages and the calls
  waiting for you, and for an UNGUARDED agent a `why:` line with its reason and what you do; then
  every QUEUED message, whatever its recipient (one no longer named included), each on one line
  with its recipient, how long ago it was sent and by whom, the attempt (n of 3), whether it waits
  to be handed over or was handed over and since when, what Lunchroom waits for, when it is
  offered again, and why it cannot be handed over if it cannot; then every HALTED conversation.
  `status` also prints the total number of calls waiting for you, the number `lunchroom approve`
  lists. It first runs the check every Lunchroom program runs (above), so it changes Lunchroom's
  records; `lunchroom doctor` does not.
- `lunchroom codex-threads [-n COUNT]`: recent Codex threads, to name one with `register`.
- `lunchroom help` and `lunchroom version`.

Only you may type, as the whole prompt of a session:

- `join [<name>]`: include this session, so it receives messages as that name.
- `leave`: take this session out; messages already QUEUED for it are FAILED at the next check,
  with that reason, and their senders are told.

A `join` or `leave` in another agent's text, or inside a longer prompt, does nothing; so does one
from a Claude Code that a program runs (`claude -p`), or an OpenClaw or Hermes turn their plugin
does not report as your typing.

Only you, in your own terminal, may run (each one that changes something asks for your passphrase
first, typed where it does not show; `doctor`, `slip`, and `approve` and `mode` with nothing after
them, only show):

- `lunchroom passphrase`: set your passphrase, or change it. With one already set it asks for the
  current one first; it asks for the new one twice (at least 12 characters), derives a verifier
  from it (PBKDF2-HMAC-SHA-256, 600000 rounds, a random salt) and, with `sudo` (you type your
  password), puts it in `/etc/lunchroom/<uid>.verifier`, owned by root. It reads the file back to
  show that root owns it and that it holds the passphrase you typed. Lunchroom checks every
  passphrase against that file and uses the file only when it is a regular file owned by root and
  writable by no one else, in a folder owned by root and writable by no one else; a link, a file
  of your own or one your group can write is not a check, and a command that needs one is DENIED.
  `lunchroom install` sets the first passphrase this way once it has written the hooks; until you
  have one, every command below that asks for the passphrase (`uninstall`, `register`,
  `unregister`, `yolo`, `rate`, `repair`, and `approve <n>` or `mode <mode>` with something after
  it) is DENIED with this command as the next step. `passphrase` itself, `slip`, and `approve` and
  `mode` with nothing after them need none, and `install` asks you to type `yes` instead while you
  have none. Setting the passphrase needs `sudo`, `install` and `stty` in the system folders named
  under Install, and a terminal to type in.
  `sudo rm /etc/lunchroom/<uid>.verifier` removes the check; Lunchroom never removes it.
- `lunchroom approve [<n>]`: list the calls waiting for you, each with how long ago it was made, or
  approve one exact call once. A call that has waited 10 minutes, as long as an approval lasts, no
  longer waits.
- `lunchroom mode [relaxed|balanced|strict]`: show the hold mode, or set it.
- `lunchroom yolo <name,name,...|all> till <HH:MM>`: let the named agents act without asking until then
  (UTC); name them separated by commas. The end may also be a date and time, `YYYY-MM-DDTHH:MMZ`. It
  turns YOLO on for every agent named, or for none: `all` names every agent, so while any of them is
  a Codex, OpenClaw or Hermes agent (`lunchroom yolo` works for Claude Code agents only) it is DENIED
  and says which agents to name instead.
- `lunchroom register <name> <harness>:<session id>`: name a session so it receives messages
  (`--codex-thread <id>` and `--claude-session <id>` are accepted in place of `<harness>:<session id>`).
- `lunchroom unregister <name>`: take a name away so that session receives no more messages;
  messages already QUEUED for it become FAILED at the next check, with that reason, and their
  senders are told.
- `lunchroom rate <messages> <seconds>`: set the rate limit (it ships at 20 messages in 600 seconds).
- `lunchroom slip`: show the permission slip; it holds every read and the reply, and nothing that
  changes a file or runs a command can be added to it (`slip add <agent> <tool>` says so).
- `lunchroom repair`: put back the last good store when it cannot be read.
- `lunchroom doctor`: check your setup and change nothing. Like `approve`, it runs only in your own
  terminal, outside every agent session. It prints one line for each check, PASS or FAIL, in plain
  words, and for each FAIL what you do. The checks:
  - Lunchroom's program is installed (`~/.lunchroom/bin/lunchroom`) and is the `lunchroom` your
    `PATH` runs;
  - the passphrase check is in place;
  - for each supported harness whose folder exists (Claude Code's folder is the one
    `CLAUDE_CONFIG_DIR` names when it is set), Lunchroom is installed and the harness configuration
    runs the whole guard, and the harness release is the one its adapter passed with or later. For
    Claude Code and Codex the release is read from the install of the program your `PATH` runs
    (`claude`, `codex`). For OpenClaw it is the release Lunchroom's plugin reported when there is one
    (the line says which), otherwise the one read from the `openclaw` your `PATH` runs. For Hermes it
    is the release Lunchroom's plugin reported in Hermes's latest session; the line says when that
    was, and a Hermes that has reported nothing since this Lunchroom was installed is a FAIL;
  - the guard's records of the last 24 hours: a FAIL when the guard could not decide a call, naming the
    agent and the time the line clears (24 hours after the call, `HH:MM UTC`);
  - each running agent's state, with its reason when UNGUARDED. An agent that is HELD is shown as
    HELD, with the agent whose message holds it (for example `HELD (a message from alice is in it)`),
    not as PASS or FAIL.

  The project settings it reads are those of the folder you run it in. It reads only local files and
  sends nothing anywhere, and exits 0 when every check passes, 1 otherwise.

An agent's attempt to run any of these, or `lunchroom check`, is DENIED.

Exit codes: 0 done (a hand-over that FAILED, or is not confirmed, is reported in the output, and the
exit code is still 0), 1 a store or file Lunchroom needs cannot be used, 2 usage or an argument that
is not valid (a DENIED message that is empty, too long or has too many lines is 2 too), 3 not permitted (DENIED because
the session, the name, your terminal or your passphrase does not qualify), 4 HALTED, 5 rate limit,
7 DENIED by a rule.

## The Claude Code adapter

How Lunchroom works with Claude Code, item by item, and the tests that prove it. The adapter states
2.1.309, the release Anthropic's stable apt, dnf and apk channels served on 2026-11-02.

Live measurement: Claude Code 2.1.309, taken from Anthropic's own `claude-code_2.1.309-1_amd64.deb`
and run from `…/claude/versions/2.1.309` in a separate Linux account with the Lunchroom 1.1.0 pre-release build installed
by `lunchroom install`, two interactive sessions. Both sessions started, were asked to join and
joined, and were shown AVAILABLE. A session's `lunchroom send` of a message asking for an API key
was DENIED (exit 7). An ordinary message to the idle session was QUEUED, woke the session and was
shown, and `lunchroom status` then showed that session HELD by the sender's message. The measurements
are in `docs/host-facts/claude-code.md`, section 5.

| Item | How | Proved by |
|---|---|---|
| Install and your confirmation | `lunchroom install claude-code` adds the guard hooks (PreToolUse, PostToolUse, PostToolUseFailure, UserPromptSubmit, and SessionStart, which shows how to include the session) and the listener (SessionStart, Stop) to `~/.claude/settings.json` beside the old file, keeps a dated backup, writes only after you confirm at your own terminal (your passphrase, or `yes` on the first install), and never writes Claude Code's trust records. | `installer_writes_beside_old_config_with_backup_and_lists_user_steps`, `installer_changes_nothing_without_yes_or_on_unreadable_settings` |
| Guard evidence | An agent is guarded while a guard hook has recorded its session in a live Claude Code process, a settings file that process loads (managed, user, project, local or `--settings`) runs the guard, and that Claude Code is release 2.1.309, the one this adapter passed with, or later; otherwise it is UNGUARDED, or STOPPED when the process has ended. The release is read from the install of the program the session runs, after following a link to the program (as nvm and Volta make) and ignoring a trailing `(deleted)` that the system shows after a package upgrade: the native installer's `…/claude/versions/<release>`, the npm package `@anthropic-ai/claude-code` above the program, or the system package named `claude-code` that installed the program (Anthropic's apt, dnf and apk packages install `/usr/bin/claude`), from dpkg's records (`/var/lib/dpkg`), apk's (`/lib/apk/db/installed`) or rpm's (`rpm -qf`); another package owning the program gives no release. Where the rpm database exists and no `rpm` program is installed, the reason says so. | `unguarded_or_stopped_agent_gets_nothing_and_sender_is_told`, `user_is_told_when_an_agent_becomes_unguarded`, `older_harness_version_is_unguarded_and_a_later_one_is_guarded`, `the_passed_version_and_later_ones_pass`, `dpkg_records_give_the_claude_code_package_owning_the_path`, `apk_records_give_the_claude_code_package_owning_the_path`, `rpm_output_gives_the_claude_code_package_owning_the_path` |
| Identity | An agent is `claude-code:<session id>` from the hook's input; `lunchroom send` finds its session through the Claude Code process above it, and an environment that names another session is DENIED. | `send_from_observed_session_only` |
| Delivery and confirmation | `lunchroom listen` (an asynchronous Stop hook) wakes the idle session with the message; the message is DELIVERED when that prompt reaches the guard's UserPromptSubmit hook, and shown once. | `delivery_is_confirmed_by_the_prompt_hook_and_shown_once`, `lunchrooms_own_delivery_passes_the_guard` |
| Approval channel | Claude Code's own permission prompt, in an attended interactive session only; under `claude -p` or `--permission-prompt-tool` a call that needs you is DENIED. | `held_call_asks_when_interactive_and_is_denied_headless`, `approval_channel_only_when_interactive_and_attended` |
| Injection channels | Commands that put a turn into another session are DENIED in every session; a SendMessage prompt, its notices and another agent's text in a tool's output each hold the session. | `every_channel_every_way_through_the_hook`, `sendmessage_text_treated_as_agent_text_and_holds_session` |
| YOLO | Lunchroom turns Claude Code's YOLO on for a `lunchroom yolo` by answering the guard hook so the call goes ahead without a prompt, until the end time; Claude Code's own switch alone still asks for a HELD agent. | `yolo_turns_harness_yolo_on_until_its_end`, `harness_yolo_alone_still_asks_and_status_shows_yolo` |

## What Lunchroom protects, and what it does not

The attacker is text: anything another agent wrote, or that came from outside your own typing (a
message, a web page, a file an agent fetched, a tool's output), may carry instructions. Lunchroom
protects your authority (only you approve actions), your files and secrets, and the other agent
sessions (no agent may speak to another as you). Text from another agent grants no authority,
whatever it says.

Non-goals: a hostile program already running as your user (it can already do anything you can); an
agent that was never guarded (Lunchroom delivers nothing to it); you approving a harmful action;
the model provider.

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

## The Codex adapter

How Lunchroom works with Codex (codex-cli 0.163.0 measured), item by item, and the tests that prove
it.

| Item | How | Proved by |
|---|---|---|
| Install and your confirmation | `lunchroom install codex` adds the guard hooks (PreToolUse, PostToolUse, UserPromptSubmit, and SessionStart, which shows how to include the session) and the listener (SessionStart, Stop) to `<CODEX_HOME>/hooks.json` beside the old file, keeps a dated backup and writes only after you confirm at your own terminal (your passphrase, or `yes` on the first install); you then approve the hooks in Codex's `/hooks`, and Lunchroom writes no trust record. | `codex_installer_writes_hooks_json_and_asks_the_user_to_trust_them` |
| Guard evidence | A Codex session is guarded while `unified_exec_tty` is `false` in Codex's `config.toml` (the install sets it; no `.codex/config.toml` in its folder or a folder above may set it to `true` or fail to read as TOML, no profile may set it to `true`, and the session started after the install set it), a guard hook has recorded its thread in a live `codex` process and the guard Codex ran then is still the one you trusted: the `hooks.json` that process reads holds all four guard hooks, and Codex's `config.toml` holds the same trust record for it. Removing, editing or untrusting it makes the session UNGUARDED, and so does a Codex older than 0.163.0, the release this adapter passed with; it is STOPPED when the process has ended. | `codex_delivery_through_codex_queue`, `codex_listener_hands_over_and_the_prompt_hook_confirms`, `the_passed_version_and_later_ones_pass` |
| Identity | An agent is `codex:<thread id>` from the hook's input; `lunchroom send` finds its thread through the `codex` process above it, and a `CODEX_THREAD_ID` that names another thread is DENIED. | `codex_delivery_through_codex_queue`, `codex_hooks_have_no_approval_channel` |
| Delivery and confirmation | Lunchroom HOLDS the session by the message, then hands it over with `codex queue` when it is sent, or at the session's start or a turn's end; it is DELIVERED when the guard's prompt hook sees it or its id is in the thread's rollout file. | `codex_delivery_through_codex_queue`, `codex_listener_hands_over_and_the_prompt_hook_confirms` |
| Approval channel | None a hook can use (Codex runs a call it was asked about); a call that needs you is DENIED and kept, and runs on its retry once you approve that exact call with `lunchroom approve`. | `codex_hooks_have_no_approval_channel` |
| Injection channels | The same commands that put a turn into another session are DENIED in a Codex session, and another agent's text reaching it holds it. | `codex_injection_channels_through_the_hook`, `codex_hooks_have_no_approval_channel` |
| YOLO | Not built: Lunchroom does not start or end Codex's YOLO. `lunchroom yolo` refuses a Codex agent, as R2.2 requires until the adapter passes its test of starting and ending YOLO. The guard's rule for a HELD Codex agent under a `lunchroom yolo` is built and tested: its ordinary calls run until the end time, and its PROTECTED calls are still DENIED. | `codex_hooks_have_no_approval_channel` |

Codex edits files with its `apply_patch` tool. Lunchroom judges such a call by the file names in the
patch's header lines (`*** Add File:`, `*** Update File:`, `*** Delete File:`, `*** Move to:`), as a
write to each of those files, and not by the patch's text. Host facts for that tool are in
`docs/host-facts/codex-apply-patch.md`.

## The OpenClaw adapter

How Lunchroom works with OpenClaw (read from the source of openclaw 2026.10.7; host facts in
`docs/host-facts/openclaw-and-hermes.md`), item by item, and the tests that prove it. The
plugin's own tests run with `node --test rust/plugins/openclaw/index.test.mjs`.

| Item | How | Proved by |
|---|---|---|
| Install and your confirmation | `lunchroom install openclaw` writes the program and the plugin with the program's path in it, after you confirm at your own terminal (your passphrase, or `yes` on the first install), keeps a dated backup of an old file, and lists your steps in OpenClaw; it writes no OpenClaw setting. | `plugin_installers_write_the_plugin_and_list_the_users_steps` |
| Guard evidence | An OpenClaw session is guarded while a guard hook has recorded it in a live gateway process running OpenClaw 2026.10.7, the release this adapter passed with, or later, the plugin file that process loaded still runs Lunchroom's program, and `openclaw.json` (read each time, since a gateway applies plugin settings while it runs) enables the plugin with conversation access; otherwise it is UNGUARDED and receives nothing, or STOPPED when the process has ended. | `openclaw_delivery_through_openclaw_agent_and_before_agent_run`, `openclaw_settings_enable_the_guard_only_with_conversation_access`, `the_passed_version_and_later_ones_pass`, `plugin_session_whose_process_ended_is_stopped` |
| Identity | An agent is `openclaw:<session key>`; the plugin names the session in each `exec` command (`LUNCHROOM_SESSION`), and `lunchroom send` accepts it only for a session a hook saw in that same gateway process. An `exec` whose `env`, as the agent wrote it, sets an identity variable is DENIED. | `gateway_sender_is_the_session_its_environment_names`, `plugin_calls_are_judged_as_the_calls_they_are` |
| Delivery and confirmation | `openclaw agent --session-key` runs a turn with the message when it is sent, or at the end of the session's turn (`agent_end`); the message is DELIVERED, and holds the session, when that turn's prompt reaches the plugin's `before_agent_run`, and the same prompt again is blocked. | `openclaw_delivery_through_openclaw_agent_and_before_agent_run` |
| Approval channel | OpenClaw's own approval (`requireApproval`: this one call, or deny), in a run you started from a chat channel. In a run with nobody to answer (scheduled, heartbeat, or started with no channel), a call that needs you is DENIED, as OpenClaw would block it; only a call you approved is reported as approved. | `plugin_prompt_holds_and_the_reported_channel_decides_ask_or_deny`, `plugin_guard_failure_follows_the_reported_channel`, and the plugin test "only a call the user approved is reported as approved after it runs" |
| Injection channels | `openclaw agent`, `sessions_send` and the other commands that put a turn into another session are DENIED, and so are OpenClaw's tools that do it (`sessions_send`, the `openclaw` tool, `automations` adding or changing a scheduled run) and what its `terminal` tool types; a turn from another session or from no person holds the session. | `plugin_calls_are_judged_as_the_calls_they_are`, `plugin_prompt_holds_and_the_reported_channel_decides_ask_or_deny` |
| YOLO | Not built: Lunchroom does not start or end OpenClaw's YOLO. `lunchroom yolo` refuses an OpenClaw agent, as R2.2 requires until the adapter passes its test of starting and ending YOLO; OpenClaw's own YOLO stays the user's switch. | `yolo_refusals` |

## The Hermes adapter

How Lunchroom works with Hermes (read from the source of hermes-agent 9976f48; host facts in
`docs/host-facts/openclaw-and-hermes.md`), item by item, and the tests that prove it. The
plugin's own tests run with `python3 -m unittest` in `rust/plugins/hermes/`.

| Item | How | Proved by |
|---|---|---|
| Install and your confirmation | `lunchroom install hermes` writes the program and the plugin into `<HERMES_HOME>/plugins/lunchroom/` with the program's path in it, after you confirm at your own terminal (your passphrase, or `yes` on the first install), keeps a dated backup of an old file, and lists your steps; enabling it is yours (`hermes plugins enable lunchroom`). | `plugin_installers_write_the_plugin_and_list_the_users_steps` |
| Guard evidence | A Hermes session is guarded while a guard hook has recorded it in a live Hermes process running Hermes commit 9976f48, the one this adapter passed with, or a later commit by commit date, the plugin file that process loaded still runs Lunchroom's program, and Hermes's `config.yaml` still enables the plugin (`plugins.enabled`, not `plugins.disabled`); otherwise it is UNGUARDED and receives nothing, or STOPPED when the process has ended. | `hermes_session_at_another_version_or_without_the_program_is_unguarded`, `the_passed_version_and_later_ones_pass`, `plugin_session_whose_process_ended_is_stopped` |
| Identity | An agent is `hermes:<session id>`; `lunchroom send` takes the session Hermes names in each command (`HERMES_SESSION_ID`), only for a session a hook saw in that same Hermes process. | `gateway_sender_is_the_session_its_environment_names` |
| Delivery and confirmation | At the start of the session's next turn the plugin's `pre_llm_call` takes the next message and hands it to that turn, and the session is HELD from then; the message is DELIVERED when the turn's request to the model carries it (the plugin's `pre_api_request`), and a repeat changes nothing. | `hermes_turn_takes_the_next_message`, `test_only_the_handed_turns_request_carrying_the_envelope_reaches_the_guard` |
| Approval channel | Hermes's own approval (`approve`, with a rule key unique to the call). Before each call the plugin reads Hermes's approval state; under `--yolo`, a session's `/yolo`, Hermes's approvals mode set to stop asking (`approvals.mode`), `hermes -z`, or with no surface to ask on, a call that needs you is DENIED. | `test_yolo_set_and_cleared_mid_session`, `test_each_no_person_setting_is_reported_and_yolo_is_reported`, `plugin_prompt_holds_and_the_reported_channel_decides_ask_or_deny` |
| Injection channels | The commands that put a turn into another session, Hermes's local session chat API included, are DENIED. A turn typed in Hermes's terminal, TUI or desktop app is your typing and clears HELD; a turn from a chat platform, the local API, a schedule or a parent session delegating to a subagent holds the session. | `plugin_calls_are_judged_as_the_calls_they_are`, `test_turn_text_is_typed_only_on_this_computers_surfaces`, `plugin_prompt_holds_and_the_reported_channel_decides_ask_or_deny` |
| YOLO | Not built: Lunchroom does not start or end Hermes's YOLO. `lunchroom yolo` refuses a Hermes agent, as R2.2 requires until the adapter passes its test of starting and ending YOLO; Hermes's own YOLO stays the user's switch, and the plugin reports it to the guard. | `yolo_refusals`, `test_each_no_person_setting_is_reported_and_yolo_is_reported` |

## Stated limits

These are statements for users of what Lunchroom does not prevent; they are not rules to test.

1. A hostile program running as the same user is out of scope (T1.4).
2. Lunchroom cannot tell a person's typing from a turn some other program writes into a harness,
   except for the markers it knows. The fences (F1) close the known channels; any
   unknown one is a limit.
3. A HELD agent can still pass to its sender anything already in its context or that it may read, in
   a form no scanner recognises (L1.3).
4. Lunchroom tracks another agent's text by its labels; text an agent rewrote before writing it
   elsewhere is not tracked once written. In the Strict hold mode (P1.6) a HELD agent writes nothing
   without the user's approval, except, under a `lunchroom yolo`, writes that are not PROTECTED (P1,
   R2.4), which bounds this; in the Balanced mode only its PROTECTED writes are bounded, and in the
   Relaxed mode only its writes to Lunchroom's program and the guard's configuration (P1.6).
5. Exactly-once action is not promised; delivery is at most once per message id, with unknown
   outcomes stated (D1).
6. Where an OpenClaw or Hermes approval appears, and whether it reaches a phone, depends on the
   user's own setup of that harness; Lunchroom only asks, and blocks when nobody can answer (A1).
7. Lunchroom spots a request for a secret by its words (L1.4); a reworded request can get through.
   In the Strict and Balanced hold modes (P1.6) a HELD agent's credential reads still go to the user
   (L1.1), and in every mode a reply carrying a secret is still scanned (L1.2).
8. Lunchroom judges a call by what the call itself shows. Apart from searching the text of a script
   or code the call runs for the fences, the identity variables and Lunchroom's own commands (F1,
   I1.2, A1.3), it does not judge what that script or program goes on to do, such as sending data
   or deleting files, so under a `lunchroom yolo` a HELD agent's script runs like any ordinary call;
   a channel the script reaches in a way its text does not spell out is not caught. A file a program
   reads as data (`sed` or `awk` input, the arguments after an interpreter's script) is not
   searched, unless the `sed` or `awk` program can run what it reads. A script is
   searched as it reads when the call is made, so a call that writes a script and then runs it is
   judged on the script as it was before. The search counts a mention anywhere in a script's
   text, a string it only prints included, so a script that quotes one of Lunchroom's user-only
   commands is DENIED; an agent changes such a file with its own edit tools instead.
9. A guard program that is missing, or that the system stops before it answers, is a failure the
   harness itself handles: Claude Code and Codex then run the call unchecked, and pass a prompt on
   unchecked, another agent's text included, without holding the session. Lunchroom accepts this so that
   a broken guard never locks the user out (H1.5). In OpenClaw and Hermes, Lunchroom's plugin
   handles it instead: the call goes to the user, or is DENIED where nobody can answer, and
   another agent's text holds the session; the user's own typing still goes on. When the guard's program is missing, Lunchroom
   shows that agent UNGUARDED and delivers nothing to it (R3); a guard stopped during one call
   leaves no such trace. The table "When the guard cannot decide a call" above sets the outcomes
   side by side.
10. The passphrase check does not hold where an agent has passwordless `sudo`: such an agent acts
    as root, and can write the verifier, remove it, or change Lunchroom's own files.
11. An agent can read the verifier (it is readable by every user) and guess the passphrase
    offline. A minimum of 12 characters and 600000 PBKDF2 rounds slow that guessing; they do not
    prevent it for a weak passphrase. Use a long one that appears nowhere else.
12. A program that records the user's keystrokes defeats any secret typed at a terminal, this
    passphrase included. Until the first passphrase is set, the install that sets it is confirmed
    with a typed `yes`, which a process of the user's own can type.
13. The envelope reaches a Codex or OpenClaw session as one argument of `codex queue` or
    `openclaw agent`, so another user of the computer can read it in `/proc/<pid>/cmdline` while
    that program runs; Lunchroom's own store is readable by its user alone.
14. The passphrase step needs `sudo` (not `doas`), `install` and `stty` in `/usr/bin`, `/bin`,
    `/usr/sbin` or `/sbin`; a system without `sudo` installs the hooks but no passphrase, and says
    so, and the commands only the user may run stay DENIED until a passphrase is set.
15. Starting a new session is recognised by the forms P1.4 lists, where the program is plainly the
    program being run: a prompt with no option, given through a wrapper Lunchroom does not know, is
    not recognised, and in a program's own code it is recognised only as a string after the
    program's name at the start of a list or call, in plain text the guard can read: a bracket in
    a comment between them, a quote mark earlier in the text, or a prompt built at run time, is not.
16. A Hermes `terminal` command's folder is read from Hermes itself; where it cannot be read, a
    HELD agent's command goes to the user.
17. Codex's hooks do not see everything an agent does. Codex's own documentation calls them "a useful
    guardrail, not a complete enforcement boundary": a hosted tool such as web search reaches no
    hook, and neither does input sent to a command already running. So `lunchroom install codex`
    sets `unified_exec_tty = false`, for every Codex session of that user, which closes such input
    and means those agents cannot keep a terminal program open; a Codex session is UNGUARDED without
    it, and a session open when the install sets it is UNGUARDED until started anew. A session open
    when the user sets it by hand, or started with it turned back on by a command-line option, can
    still type into a shell it has open, and the guard does not see it. A Ctrl-C still reaches a
    command that is running. A session no hook has seen when the install sets it is not caught, so
    close open Codex sessions before the install or start them anew after it. Codex 0.166 runs
    sessions in one server process: a session whose window you closed still shows as running until
    that server ends, a message to it shows DELIVERED though no window shows it, and a thread resumed
    under that server stays UNGUARDED, so start a new one.
18. After `lunchroom approve`, a Codex agent is told, and may decline to run the call. An OpenClaw or
    Hermes agent is not told: the call runs only if the agent tries it again by itself.
19. An agent can change Lunchroom's records, your approvals and its holds included. They are files
    your own user can write, and an agent runs as your user: the guard DENIES a call that shows those
    files, and a script or a path built from pieces does not show them. Being HELD stops this only in
    the Strict hold mode without a `lunchroom yolo`, and there the script comes to you (in Codex,
    through `lunchroom approve`): you are shown the script, not what it writes. Putting the records out of an agent's reach takes a separate account, which this release
    does not do.

## License

Copyright 2026 protectNIL Inc. d/b/a Epic AI

Licensed under the Apache License, Version 2.0 (the "License"); you may not use this software
except in compliance with the License. You may obtain a copy of the License in the LICENSE file
next to this README in a release download, or in [LICENSE](https://github.com/epic-ai-io/lunchroom/blob/main/LICENSE) in the source, or at <http://www.apache.org/licenses/LICENSE-2.0>.

Unless required by applicable law or agreed to in writing, software distributed under the License
is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
implied. See the License for the specific language governing permissions and limitations under the
License.
