# Changelog

## 1.2.0

Lunchroom 1.2 adds the console, the store wall, a check of your `sudo` rules and approvals for the Agent Execution Controller, and a run-time test that no command or hook opens a network connection. 1.1.3 is superseded by 1.2.0.

Upgrade: download and unpack the new release, then run its program by its path, in your own terminal, from the unpacked folder: `./lunchroom install`. Its last step sets up the console; it also sets your passphrase check to root's, group `lunchroom`, mode 0640. If that step stops, `lunchroom install console` runs it again. Then start each agent session anew.

### The console

- `lunchroom install` sets the console up as its last step (`lunchroom install console` runs only
  that step again), and `lunchroom console` starts it. It is a
  full-screen terminal screen (built with Ratatui) that runs as the user `lunchroom`, in a terminal
  of your own that you own. It shows the agents, the QUEUED and FAILED messages with their reasons,
  the notices for you (including those OpenClaw and Hermes cannot show), the calls waiting for
  approval with their age, the counts of DENIED decisions, the guard's timeouts and slow decisions,
  and the checks of `lunchroom doctor`.
- An alert strip of six cells stays at the top of the screen: `CALLS WAIT`, `PROBING`,
  `FAILED MSG`, `UNGUARDED`, `GUARD SLOW` and `CHECKS`. Each carries its count or word, so colour is
  never the only sign, and it stays drawn and live while the console is locked.
- The main screen shows one line per item. Select an item, by a click or by the arrow keys and
  Enter, and a panel opens with its whole text; Esc, or a click outside the panel, closes it. Text
  an agent chose is drawn with each control character and direction mark replaced by a visible
  mark, so an agent cannot change the screen's layout.
- From a panel you can approve or refuse a waiting call, release a hold, resend or discard a failed
  message, hold or unregister an agent, dismiss a probing alert, see the command that repairs an
  UNGUARDED or STOPPED agent, and clear a marked path. Every action has a key and every key a click.
- The console asks your passphrase when it starts, and again for every `approve`, every loosening
  action (`mode balanced`, `mode relaxed`, `yolo`) and every approval for the controller, a second
  `approve` a minute after the first included. After 15 minutes without a key it locks, and asks
  the passphrase again before it does or shows anything but the alert strip. An `unregister`
  within those 15 minutes rides on the passphrase last typed.
- The guard now keeps each DENIED decision (time, session, reason word, and the call with its
  secrets replaced, cut at 64 KiB and marked `cut`), up to 1,000 beside the 200 records of undecided or slow calls. Five or more DENIED decisions in
  10 minutes from one agent raise a probing alert, shown on the console and given to you as a
  notice.
- New guard rules, so an agent cannot reach the console or the store by the side: a read or write
  of `/dev/pts/*`, `/dev/tty*` or `/dev/vcs*` other than the agent's own terminal is DENIED; `kill`,
  `pkill` or `killall` naming the console or `lunchroom` is DENIED; `gdb`, `strace` and `ltrace`
  given a process id, and reads of `/proc/<pid>/mem`, `environ`, `fd` and `maps`, are DENIED unless
  the process is written in the call and is in the agent's own subtree; ptrace through `python` or
  `perl` is DENIED; the keystroke, pane-capture and pane-pipe commands of `tmux`, and the `stuff`
  and `hardcopy` commands of `screen`, are PROTECTED while the agent is HELD and DENIED when the
  target is not its own pane; `sudo`, `su`, `runuser`, `machinectl shell`, `pkexec`, `doas` and
  `run0` to the user `lunchroom`, and any write to Lunchroom's store, program, `/etc/lunchroom` or
  the `sudo` rule file, are DENIED; running the program with `console` is DENIED.
- A read of `/etc/lunchroom`, where the passphrase checks are, is DENIED in every build.
- The console runs as the user `lunchroom`, which cannot read your home folder or your agents'
  processes. It tells a STOPPED agent itself and shows each other agent's state as your own
  Lunchroom program last found it: each guard hook of a named agent has its session judged again,
  after it has answered, at most every 30 seconds. An agent shows AVAILABLE only from a finding of
  the last 2 minutes; an older one shows as not known, with the time it was last found guarded and
  `lunchroom status` to judge it now. A STOPPED agent's panel shows the command that repairs it.
- The alerts box lists what you act on first (PROBING alerts, QUEUED and FAILED messages, failing
  checks, marked paths); on a full box the newest notices stay on the screen, with a line counting
  the rest. A HELD agent's panel shows the text of an arrival Lunchroom did not deliver (an OpenClaw
  or Hermes prompt you did not type, a harness's own cross-session message), kept with every secret
  replaced while it holds the session.
- A HELD agent's `gdb` that starts a program of its own runs when it takes `-nx` and only `run`,
  `r`, `bt`, `backtrace`, `continue`, `c`, `quit` or `q` as `-ex`, then `--args` and the program;
  any other `gdb` call goes to you (stated limit 27).

### The store wall

- `lunchroom install` creates the system user and group `lunchroom`. The store moves to
  `/var/lib/lunchroom/<uid>/` (group `lunchroom`, mode 2770) and the program is installed at
  `/usr/local/lib/lunchroom/lunchroom`, setgid `lunchroom`. Your user is not in that group, so no
  process of yours, an agent's included, can enter the store or read the passphrase check; only the
  program can, through its own logic. That program ignores `LUNCHROOM_HOME` and every other
  variable that sets a path, and drops the group before it runs any other program.
- The install moves an existing store into the new folder, every QUEUED message with it, writes
  hooks that run the program at its new path, and says which sessions to start anew. Before it
  moves the store it stops your running 1.0.1 listeners; where one does not stop, it moves nothing,
  names that process and exits with a failure.
- `lunchroom install` with no harness named and none found changes nothing, the store wall
  included, and says that a harness's folder is made the first time it runs: start it once, or name
  it (`lunchroom install claude-code`).
- On an account with neither `~/.local/bin` nor `~/bin`, the install makes `~/.local/bin` and links
  `lunchroom` there, and says when that folder is not on your PATH yet.
- `lunchroom doctor` and the console fail when `/usr/local/lib/lunchroom` or the program in it can
  be written by anyone but root, and an error about a store that cannot be reached names the folder
  or file.

### The wide and narrow `sudo` check

- The store wall and the console's wall hold only where your account needs a password for `sudo`.
  `lunchroom install`, `lunchroom install console` and `lunchroom doctor` read `sudo -n -l` and
  sort each rule that runs without a password as wide or narrow. A rule is wide when it runs as
  root or any user with any command, runs as the user or group `lunchroom`, names a command with a
  wildcard, names a shell or a program that runs other programs, names a file or a folder that a
  user other than root can change, or runs as another user whose own rules are wide. Any other rule
  is narrow, and leaves the walls standing.
- With a wide rule, `lunchroom install` still sets the store wall up and says plainly that it does
  not hold on this account until the rule is removed; `lunchroom doctor` says the same on a line
  that leaves its exit code as it was. Setting the console up (the install's last step, or
  `lunchroom install console`) stops before it sets anything up and asks whether to make `sudo` ask your account for its password. On a typed `yes` it writes the
  one rule `#<uid> ALL=(ALL:ALL) PASSWD: ALL` as `/etc/sudoers.d/zz-lunchroom-<uid>`, checks it with
  `visudo`, and keeps it only when `sudo` then asks for a password; otherwise it removes the file,
  sets nothing up, says why, and `lunchroom doctor` shows a FAIL. From then on every `sudo` command
  of your account asks that password, an agent's included. The guard still DENIES only an agent's
  command that runs as `lunchroom` or names Lunchroom's paths.
- `lunchroom uninstall console` leaves that rule file in place and names it, since removing it
  would give your account its password-free `sudo` back.

### Approvals for the Agent Execution Controller

- The console approves a phase of a controller run: you give the run and the phase, the console
  reads the plan hash from the approval request the controller wrote, shows the run, the phase and
  the hash, and asks `y/N` and then your passphrase. It writes one signed record, bound to that run,
  that phase and that plan hash, to `/var/lib/lunchroom-approvals/<uid>/`. The signature is Ed25519
  (RFC 8032). `lunchroom install console` makes the key pair once for each developer; the private
  key is readable by the user `lunchroom` only, and the public key, whose presence tells the
  controller that the console is installed, is written last and removed by
  `lunchroom uninstall console`.

### Limits fixed in 1.2

The limits below were listed for 1.0.1 and are rewritten or fixed in 1.1; the full list is in
`rust/README.md`, "Stated limits".

- Fixed: limits 11, 13, 19, 23 and 24 of the stated limits.
- Rewritten to say what Lunchroom does and does not do now: limits 2, 3, 4, 7, 8, 9, 10, 12, 14,
  15, 16, 17, 18, 20, 21, 22 and 26. Among them, limit 9 names the two cases in which the harness
  runs a call although the guard failed, and limit 10 says the store wall and the passphrase check
  do not hold where your account has a `sudo` rule that needs no password.
- Every release now publishes these limits in its documentation, together with the console's own
  (a person who walks away from an unlocked console for under 15 minutes leaves `unregister` and
  what it shows open to whoever is at its keyboard; the developer's outer terminal is owned by the
  developer, and a process that opens it can race to read keystrokes, the passphrase included).

### Commands

- `lunchroom read <id>` prints the envelope of a message addressed to the session that runs it, and
  DENIES any other id.
- `lunchroom clear <path>` clears a path Lunchroom recorded as written by a HELD agent. You type it
  in a terminal of your own, and an agent's `lunchroom clear` is DENIED.
- `lunchroom uninstall console` removes the console's public key, the controller's sign that the
  console is installed, and leaves the `sudo` rule file in place (see above).

### New dependencies and their licences

- `ratatui` (MIT) and `crossterm` (MIT): the console's screen.
- `signal-hook` (MIT or Apache-2.0).
- `ed25519-dalek` (BSD-3-Clause): the signature of controller approvals. Its dependencies
  `curve25519-dalek` and `subtle` are also BSD-3-Clause.
- `rustix` (Apache-2.0 or MIT).

### No network connection, tested at run time

- Every command and hook of Lunchroom's is now run under a trace of network system calls in the test suite, which fails if any is made; the existing check of the source and dependencies stays.

## 1.1.3

Fixes to 1.1.2. 1.1.2 is superseded by 1.1.3.

Upgrade: download and unpack the new release, then run its program by its path, in your own terminal,
from the unpacked folder: `./lunchroom install`. `lunchroom install` typed by name runs the program
already installed, so it reinstalls the build you already have and upgrades nothing.

- A command only you may run (such as `lunchroom yolo` or `lunchroom approve`) is now DENIED to an
  agent that runs it through a hard link to Lunchroom's program under another name and folder, as it
  is through the program's own path. Before, such a link was not recognised as Lunchroom's program.

## 1.1.2

Fixes to 1.1.1, found by upgrading to it. 1.1.1 is superseded by 1.1.2.

Upgrade: download and unpack the new release, then run its program by its path, in your own terminal,
from the unpacked folder: `./lunchroom install`. `lunchroom install` typed by name runs the program
already installed, so it reinstalls the build you already have and upgrades nothing.

- A session whose agent has exited now shows STOPPED. Before, an agent that had exited but whose
  parent had not yet collected it (a zombie process) was shown UNGUARDED, running.
- `lunchroom install` with no harness named, once a passphrase is set, asks the passphrase once, then
  asks yes or no for each harness found, and installs only those you answer yes to. Before, it asked
  the passphrase once for each harness. A no shows as "not installed (you answered no)". The first
  install, before a passphrase exists, still asks a yes for each harness.
- An install run with the build already installed now says so before it asks, names the build, and
  says how to install a newer one: run the new program by its path. Before, typing
  `lunchroom install` by name to upgrade reinstalled the old build without a word.
- An install or uninstall now keeps the three newest backups of each file it replaces and removes the
  older ones; a backup it cannot remove is named in its output. Before, every install and uninstall
  added a backup and none was ever removed.
- The READMEs have an Upgrade section: unpack the new release and run `./lunchroom install` from its
  folder. Their download, signature-check and `lunchroom version` examples name 1.1.2.

## 1.1.1

Fixes to 1.1.0.

- `lunchroom send`: when Lunchroom's store cannot be used during a hand-over, the report now also says
  what to do: do not send the message again, `lunchroom status` shows where it stands, and tell the
  user if the store still cannot be used. Before, it said only what happened and why.
- The notice that an attempt to hand a message over failed now names the message's age and its sender,
  as Lunchroom's other notices about a message do.
- The README's `lunchroom yolo` usage now reads `lunchroom yolo <name,name,...|all> till <HH:MM>`, the
  same as `lunchroom help` and the message for a wrong use.
- `lunchroom install console` now says the console comes in a later release; 1.1.0 said it comes in
  release 1.1.
- The documentation (the spec's console section, TESTPLAN, the Claude Code host facts) now names the
  shipped build 1.1.0 where it said 1.0.2, and says the console comes in a later release.
- The READMEs' download, signature-check and `lunchroom version` examples name 1.1.1.

## 1.1.0

Fixes for what failed in real use of 1.0.1, and new checks of your setup.

- Fix: Codex agents' file edits no longer time out. Lunchroom took Codex's `apply_patch` tool for a
  tool it did not know and searched the whole text of the patch, which could run past the 10-second
  limit and block the edit. It now judges an `apply_patch` call by the file names in the patch's
  header lines (`*** Add File:`, `*** Update File:`, `*** Delete File:`, `*** Move to:`), as a write
  to each of those files, and limits the work it does on any one call.
- When the guard cannot decide a call (its own error, a store it cannot use, or no answer within 10
  seconds), the message now says which of the three it was, shows the call, names the step that was
  still running when the time ran out, and gives the ways forward, in plain words. A Codex agent is
  told to try the call once more and then to tell you.
- The guard now records each call it could not decide, and each decision that took longer than 2
  seconds: the time, the session, the call and the slowest step. It keeps the latest 200 records in
  Lunchroom's own folder and sends nothing anywhere. `lunchroom doctor` shows the records of the
  last 24 hours, and shows a FAIL for a call the guard could not decide, naming the agent and the
  time the line clears.
- Claude Code installed with Anthropic's apt, dnf or apk package (`/usr/bin/claude`) is no longer
  UNGUARDED for a release Lunchroom could not read: Lunchroom now reads the release of the system
  package named `claude-code` that installed the program, from dpkg's, apk's or rpm's records.
  A program a package manager upgraded under a running session (its path ends `(deleted)`), and one
  reached through a link such as nvm or Volta makes, are now read by their real path.
- The Claude Code release Lunchroom requires is now 2.1.309, the release Anthropic's stable apt,
  dnf and apk channels served on 2026-11-02 (it was 2.1.310).
- When an agent is UNGUARDED, Lunchroom now says why, and what you do about it: in the notice when
  an agent becomes UNGUARDED, and under the agent in `lunchroom status`. The reason is the first
  of these the agent lacks: a harness configuration that runs the whole guard (for Codex, naming
  the setting or hook at fault), a guard hook run in its current session, a release that can be
  read from its install (naming the program's path), a release no older than the adapter's
  (naming both). The separate Codex notes in `lunchroom status` are now these reasons.
- A DENIED call or message is now explained in full, not by its reason alone. After the reason
  and its next step, it says what was DENIED (the command, path or message, and the part at
  fault, as the guard read it), why (the rule, and for a HELD session the agent and message id
  that hold it), and every way forward that exists for it: what the agent can do instead, and
  what you can do (run it in a terminal of your own, type in the session to clear the hold,
  approve the exact call with `lunchroom approve`, or change the setting at fault). A DENIED
  message's text is never shown in it.
- The first line of a DENIED call now names the part at fault, such as the path, and a DENIED search
  names only the folder it searched, never a file it found there. Messages no longer print rule
  codes such as "(L1.1)", they use plain words, and they explain HELD the first time they use it.
  A DENIED message names "change how strict Lunchroom is" as a way forward without the commands
  that do it, because the agent reads every message its session shows; the commands are in
  `lunchroom help` and the README.
- No message waits without an end. Every Lunchroom program that runs (any guard hook, any
  listener, `lunchroom send`, `lunchroom status`) now checks every QUEUED message: it looks again
  for the recipient's confirmation, counts a hand-over unconfirmed after 10 minutes as a failed
  attempt and offers the message again (after at least 10 seconds), and FAILS a message whose
  recipient took itself out with `leave` or lost its name. Before, a message handed to a Codex
  session that never confirmed it could show "Not confirmed; still QUEUED" for ever, with nobody
  told.
- Fix: a Codex message that FAILED could print DELIVERED. `lunchroom send` now prints DELIVERED only
  for a message the recipient confirmed, and FAILED when the message failed while `send` waited.
- You and the sender are now told, in the next prompt of any guarded session (Codex and OpenClaw
  senders too, in their listeners): when a hand-over has no confirmation for 2 minutes, when an
  attempt fails, when a message is FAILED, when it cannot be handed over, and when it is DELIVERED
  after any of these. Each notice names the message, its recipient, its age, the attempt (n of 3),
  what Lunchroom waits for, when it is offered again, and what you can do. A message that could not
  be handed over because its recipient was UNGUARDED is offered again when that reason clears.
- `lunchroom status` lists every QUEUED message with those facts, including messages to agents
  no longer named, and runs that check first, so it now changes Lunchroom's records. It also
  prints the total number of calls waiting for you.
- `lunchroom approve` shows how long ago each waiting call was made, and a call that has waited as
  long as an approval lasts, 10 minutes, no longer waits.
- Lunchroom keeps at most 50 notices for you and for each sender, and says how many older ones it
  dropped. Its store now carries a schema number and keeps fields it does not know, so a later
  version's records survive a rewrite by an older program.
- `lunchroom send` now reports it when Lunchroom's store could not be used during a hand-over,
  instead of saying nothing, and each of its other messages that stop a send says what happened,
  why, and what to do.
- New: `lunchroom doctor` checks your setup and changes nothing. One PASS or FAIL line per check,
  in plain words, with the fix for each FAIL; it exits 0 only when every check passes. It runs only
  in your own terminal, outside every agent session, and an agent's `lunchroom doctor` is DENIED. It
  reads Claude Code's, Codex's and OpenClaw's releases from the programs your `PATH` runs (OpenClaw's
  from what Lunchroom's plugin reported, when it has), and Hermes's from what Lunchroom's plugin
  reported in Hermes's latest session, with the time it reported it. It checks Claude Code in the
  folder `CLAUDE_CONFIG_DIR` names, and shows an agent that is HELD as HELD, not as PASS or FAIL.
- The `lunchroom yolo` usage is the same in `lunchroom help`, this documentation and the message
  for a wrong use: names separated by commas, or `all`.
- An agent's `lunchroom check` is DENIED, as are the other commands only you may run.
- Records of calls the guard could not decide have control characters removed and secrets hidden,
  and only you can read them.
- Fix: in a Claude Code session started by a program (`claude -p`) or a Codex session started by
  `codex exec`, Lunchroom no longer shows the waiting-message notices meant for a person at the
  prompt.
- OpenClaw and Hermes: notices meant for you are no longer lost. Where the harness lets a plugin show
  text to you, the plugin shows it; where it does not, `lunchroom status` shows it. Hermes's plugin
  reads its harness's output as UTF-8 and stays inside one total time limit.
- Fix (security): an OpenClaw agent's `apply_patch` into Lunchroom's own folder ran. OpenClaw passes
  the patch in a field named `input`, which Lunchroom did not read. Lunchroom now reads it, and any
  file-write call whose input holds no path or patch Lunchroom can read is DENIED in every session.
- Fix: a scheduled or scripted `claude -p` with Lunchroom installed never ran its prompt: Lunchroom's
  listener held it until it was killed. A Claude Code session run as one prompt (`-p`, `--print`,
  `--permission-prompt-tool`) is now asked nothing at its start, and its listener returns at once.
- Fix: in Claude Code, a DENIED call's explanation was folded behind "Ran 1 shell command" until you
  pressed ctrl+o. It now reaches you on screen in full, and still reaches the agent.
- Fix: `lunchroom status` waited up to 30 seconds for a store another Lunchroom process held. It now
  waits 10 seconds in all, then says the store is busy.
- Fix: a running Codex session whose hooks run through a wrapper (`sh -c`, a script) showed STOPPED.
  Lunchroom now finds the session's `codex` process above the hook.
- Fix: UNGUARDED was explained as "nothing it does is checked". UNGUARDED means Lunchroom has no
  evidence that the agent's guard runs, and delivers nothing to it; a session whose program an
  upgrade replaced still runs its hooks.
- Fix: Hermes installed from a git checkout that root owns, run by another user, showed UNGUARDED
  because git refused to read the checkout. Lunchroom's plugin now reads the release with git told
  to trust that checkout, only when root owns it and no other user can write to it; otherwise the
  reason says to give the checkout to the user who runs Hermes or to root.
- Fix: a script that named the repository `epic-ai-io/lunchroom` with an option after it was DENIED
  as a Lunchroom command. Only Lunchroom's user-only commands count now.

## 1.0.1

- Fix: Lunchroom no longer replaces titles you set on Claude Code sessions. A joined session that
  started or resumed was renamed to its Lunchroom name each time, over any title you had set. Now
  Lunchroom sets a title only on a session with no title of yours, and changes only a title it
  gave or one equal to a name the session is giving up. A title already replaced stays as it is:
  set it again with `/rename`.

## 1.0.0

The first release.

- Messages between Claude Code, Codex, OpenClaw and Hermes sessions, with the sender stamped by
  Lunchroom, delivery confirmed by each harness, and honest outcomes (QUEUED, DELIVERED, FAILED).
  A message is at most 64 KiB.
- The guard: a session holding another agent's text is HELD, and the user's hold mode (Strict,
  Balanced, Relaxed) sets what it may still do; fences stop any agent putting a turn into another
  session; reads of other sessions' transcripts and Lunchroom's stores are DENIED. In Strict and
  Balanced, a HELD agent's credential reads, pushes, deletes and network calls go to the user.
- Loop and rate limits, the user's YOLO with an end time, approvals of one exact call in the folder
  it runs in, covering exactly what the user was shown.
- Commands only the user may run (approve, YOLO, the hold mode, install and the rest) are confirmed
  with the user's Lunchroom passphrase, checked against a file only root can change.
- `lunchroom install` finds the supported harnesses on the computer and installs the ones the user
  says yes to; `lunchroom uninstall` removes Lunchroom's hooks; both keep every other hook and a
  backup of each file changed.
- A session receives messages once the user types `join` (or `join <name>`) in it, and stops when
  they type `leave`; its agent is told its name, and Claude Code shows the name as the session's
  title. `lunchroom register` and `unregister` name sessions from the user's terminal.
- Every tool call the guard DENIES says what the agent or the user can do next; `lunchroom status`
  opens with a count of agents by state.
- `lunchroom version` names the commit the program was built from; the install reports the build
  it installed and the one it replaced.
- Static Linux programs for x86_64 and aarch64, run on eight Linux distributions and rebuilt byte
  for byte before release, with checksums signed by Sigstore.
