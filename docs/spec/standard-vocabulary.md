# Lunchroom standard vocabulary

One word per meaning, the same in the code, log events, messages, the README, the instructions Lunchroom gives agents,
the website and the handout cards. Requirement V1 in `docs/spec/lunchroom-spec.md` makes this
binding on the product.

## States, verdicts and parties

| Applies to | Word | Meaning |
|---|---|---|
| Agent | AVAILABLE | Its guard is running and no other agent's text is in its context; it works normally. |
| Agent | HELD | Another agent's text is in its context. What it may still do depends on the hold mode (`lunchroom mode`): in Strict, only what its permission slip allows, or, while a `lunchroom yolo` covers it, any call that is not PROTECTED; in Balanced, any call that is not PROTECTED and, unless a `lunchroom yolo` covers it, not GATED; in Relaxed, any call except a change to Lunchroom's program or the guard's configuration and, unless a `lunchroom yolo` covers it, a GATED call. DENIED and HALTED rules always apply. |
| Agent | YOLO | The agent's harness is set to stop asking the user: by its own switch, or by the user's `lunchroom yolo` until a stated time. Under a `lunchroom yolo` a HELD agent acts without asking; the harness's own switch alone does not free a HELD agent in Strict. In Strict and Balanced, PROTECTED calls still come to the user; DENIED and HALTED rules always apply. |
| Agent | UNGUARDED | It is running, but Lunchroom has no evidence that its guard runs, so it delivers nothing to it. |
| Agent | STOPPED | It is not running, or Lunchroom cannot reach it. |
| Conversation | HALTED | Lunchroom stopped the conversation between two agents (reason: loop, rate limit). |
| Message | QUEUED | Accepted and waiting for the recipient. |
| Message | DELIVERED | The recipient has it. |
| Message | FAILED | It could not be handed to the recipient for a technical reason, and was not sent. |
| Call or send | GRANTED | It goes ahead (approved by the user, or allowed by the rules). |
| Call or send | DENIED | A rule stopped it; the reason is always shown (for example: leak, fence, loop, rate limit). |
| Call | PROTECTED | A kind of call that is never inside a permission slip and that a HELD agent makes only with the user, even under YOLO, in the Strict and Balanced hold modes (push or publish, delete, send off the computer, read or write credentials, write Lunchroom's program or guard configuration, unparseable commands, a tool Lunchroom does not know, text sent to another agent's session outside `lunchroom send`, starting a new agent session; spec P1.4). |
| Call | GATED | A merge, a deploy or a migration, or a call that runs something Lunchroom cannot read to the end (spec P1.4). It is never inside a permission slip, and a HELD agent makes it only with the user in every hold mode, unless a `lunchroom yolo` covers the agent. It is not PROTECTED. A call that is both GATED and PROTECTED goes to the user in every hold mode and under every YOLO. |
| Marked part | loosens | A part of a call that would widen what an agent or Lunchroom's rules allow: it writes Lunchroom's program or the guard's configuration, changes a slip, registration, rate limit, hold mode or YOLO, writes Lunchroom's stores, reads or writes credentials, sends text to another agent's session, or starts a new session (spec §17.10). |
| Marked part | destroys | A part of a call that deletes any file, pushes or publishes, merges, deploys, migrates, sends data off the computer, or patches more than 200 files (spec §17.10). |
| Marked part | not read | A part that the guard could not read to the end: an unparseable command, a tool it does not know, a path it could not resolve, a program not on its list, a file it cannot read, or a command built at run time (spec §17.10). |
| Marked part | reply | The reply of a HELD agent in the Strict hold mode, marked as a whole (spec §17.10). |
| Person | the user | The person at the terminal. Never "the human" in product text. |

A reason is a small label beside the word, never a different word: `DENIED · leak`,
`HALTED · loop`.

## Words that are retired

| Retired | Use instead | Where it would otherwise appear |
|---|---|---|
| bus | Lunchroom | the program's name, its folder, message envelopes and labels |
| locked, lock (of a session) | HELD | guard messages and comments |
| tainted, `TAINTED` | HELD | `status` output |
| refused, refusing, `REFUSED <id>` | DENIED | send and guard messages |
| NOPE | DENIED | the printed cards |
| allowed, allow | GRANTED | log fields such as `allowed=` |
| pending | QUEUED | `status` output `pending=` |
| human | the user | messages and the envelope |
| AUTO | YOLO | the printed cards |
| LOOPING | HALTED · loop | the printed cards |
| down, off, inop | UNGUARDED or STOPPED | rejected as ambiguous |

The product takes these words from one table (`rust/crates/lunchroom-core/src/vocab.rs`, which also
lists the retired words), and its test suite fails on a retired word in anything it prints. Internal
identifiers that never reach a person (function names, file names, log event names stored under
older names) may keep their names. Anything printed, logged for the user or written in documentation
uses this table.
Names that belong to another program (a harness's hook fields, settings and decisions, such as
OpenClaw's `requireApproval` or Claude Code's `permissionDecision: "allow"`) are quoted as that
program spells them, in code type, and that program's documentation is quoted as written, in
quotation marks; the words around them follow this table.

## Writing rules for product text

- A model or agent is named the way its maker writes it: Claude, Codex, Kimi, GLM, DeepSeek,
  Mistral.
- Models speak plainly in examples; no slang.
- No vendor logos, colours or fonts.
