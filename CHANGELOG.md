# Changelog

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
