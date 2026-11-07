# Codex `apply_patch`: what the guard's hook receives (from Codex's source)

For the Codex adapter (spec §13 H1) and the classifier (spec §6 P1.4, §11 L1.1). Read from the
source of OpenAI's Codex, repository `openai/codex`, tag `rust-v0.166.0` (commit
`593389380740f7b4762771573c447ccfdc59742f`, the release `codex --version` printed on the machine
this was written on). The hook payload code is also the same at tag `rust-v0.163.0` (commit
`129b8ec7ee6f4511557a4856279f529025c25af4`), the release the other Codex facts
(`docs/host-facts/codex.md`) were measured on.

This is source, not a live measurement: no probe in this file ran a Codex session. A live capture of
an `apply_patch` hook input is still to be made and compared with section 1.

## 1. The hook input

Codex edits files with a tool it calls `apply_patch`. When the model calls it, Codex builds the
`PreToolUse` hook input like this
(`codex-rs/core/src/tools/handlers/apply_patch.rs`, lines 370-375,
<https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/core/src/tools/handlers/apply_patch.rs#L370-L375>):

```rust
fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
    apply_patch_payload_command(&invocation.payload).map(|command| PreToolUsePayload {
        tool_name: HookToolName::apply_patch(),
        tool_input: serde_json::json!({ "command": command }),
    })
}
```

- `tool_name` is `apply_patch` (`codex-rs/core/src/tools/hook_names.rs`, lines 34-38,
  <https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/core/src/tools/hook_names.rs#L34-L38>).
  `Write` and `Edit` are only aliases a hook's `matcher` accepts (`matcher_aliases`); the hook's
  input carries `apply_patch`, so the guard receives the tool name `apply_patch`.
- `tool_input` is an object with one field, `command`: the whole patch text, as the model wrote it
  (lines 279-286, `apply_patch_payload_command`, which returns the `input` of a
  `ToolPayload::Custom`). No path is a field of its own: the paths are inside the text.
- Codex passes `tool_name` and `tool_input` to the hook unchanged
  (`codex-rs/hooks/src/events/pre_tool_use.rs`, lines 176-194: "the canonical `tool_name` so audit
  logs and downstream policy decisions stay stable").
- A call to the tool in the other form Codex has (a function call with JSON arguments) returns
  `None` from `apply_patch_payload_command`, so Codex runs no `PreToolUse` hook for it. The guard
  cannot see such a call; it is Codex's choice, not Lunchroom's.
- The hook group Lunchroom installs for `PreToolUse` has no `matcher`
  (`rust/crates/lunchroom/src/install.rs`, `lunchroom_hooks`), so it runs for `apply_patch` as it
  does for every tool.

## 2. The patch text

The format is Codex's own (`codex-rs/apply-patch/src/parser.rs`, lines 1-25 and 37-45,
<https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/apply-patch/src/parser.rs#L1-L45>):

```
*** Begin Patch
*** Add File: <path>
+<line of the new file>
*** Delete File: <path>
*** Update File: <path>
*** Move to: <path>
@@ <heading>
 <context line>
-<removed line>
+<added line>
*** End Patch
```

- The markers are `*** Add File: `, `*** Delete File: `, `*** Update File: ` and `*** Move to: `
  (`parser.rs` lines 39-42).
- Codex reads a header from the trimmed line: `trimmed.strip_prefix(ADD_FILE_MARKER)`,
  `DELETE_FILE_MARKER`, `UPDATE_FILE_MARKER` (`codex-rs/apply-patch/src/streaming_parser.rs`, lines
  107-132) and, inside an update, `update_line.strip_prefix(MOVE_TO_MARKER)` (line 255). The rest of
  the line is the path, as written, relative to the session's working folder. `parser.rs` lines
  24-25: "The parser below is a little more lenient than the explicit spec and allows for
  leading/trailing whitespace around patch markers."
- Every other line is file contents or a hunk heading and starts with `+`, `-`, a space or `@@`; a
  line of contents that holds the text of a header (`+*** Add File: x`) is not a header, because it
  does not begin with the marker once trimmed.

## 3. What the guard does with it

`apply_patch` is a file-write tool, like Claude Code's `Write` and `Edit`. The classifier judges only
the paths on header lines (each once, at most 200 of them) as writes (P1.4): a path in Lunchroom's
stores is DENIED in every session; a path in another session's transcripts is DENIED while the agent
is HELD; credentials and the guard's configuration are PROTECTED while it is HELD. Nothing else in the
patch is read as a path: the text of a patch can be thousands of lines holding wildcards, `~` and
`/` that name nothing, and reading it as paths is what kept the guard past its 10 seconds before.

A patch that Codex would apply by running the `apply_patch` program from a shell command (a heredoc
given to `apply_patch` in a Bash call) is a Bash call, judged as the shell text it is.
