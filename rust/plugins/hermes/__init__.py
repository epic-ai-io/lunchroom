"""Lunchroom's Hermes plugin (spec §13 H1; host facts docs/host-facts/openclaw-and-hermes.md).

It hands each Hermes event to Lunchroom's guard program and turns the guard's answer into
Hermes's: a call that goes ahead returns ``None``; one that goes to the user returns ``approve``
with a rule key unique to the call, so a "session" or "always" answer never covers another call
(A1.1); a DENIED call returns ``block``. Before each call the plugin reads Hermes's own approval
gate and tells the guard whether a person would be asked (A1.2). ``pre_llm_call`` tells the guard
whether the turn's text is the user's own typing (T1), then hands the next waiting message to the
turn; ``pre_api_request`` passes the turn's request to the guard, which confirms the message when
the request carries it (D1.1). A guard that cannot decide, its program missing included, sends the
call to the user, or DENIES it where nobody can answer (H1.5); the user's own typing is never
blocked.
``pre_llm_call`` cannot block a turn, so when the guard could not judge a turn that is not the
user's typing, the plugin itself sends every later call of that session to the user, or DENIES
it where nobody can answer, until the guard has judged a turn the user typed (T1, H1.5).
``lunchroom install hermes`` writes this file with the program's path in place of the marker.
"""

import importlib
import json
import os
import stat
import re
import subprocess
import threading
import time
import uuid

PROGRAM = "__LUNCHROOM_PROGRAM__"
# The guard answers within its own 10-second deadline (H1.5); this limit is reached only when the
# system stops the guard before it answers.
DEADLINE_SECONDS = 12
PLUGIN_FILE = os.path.abspath(__file__)

# How long Hermes lets a ``pre_llm_call`` callback run: ``plugins.hook_callback_timeout``, 30 seconds
# unless the user set another (NousResearch/hermes-agent at 9976f4857b72, ``hermes_cli/plugins_dispatch.py:183``
# ``_HOOK_CALLBACK_TIMEOUT_SECS = 30.0``; ``pre_llm_call`` is in the bounded set, line 44; the setting is
# read by ``hermes_cli/plugins.py:1049-1071`` ``_resolve_hook_callback_timeout``). Past it Hermes
# abandons the callback and the turn goes on with nothing from it (fail open), so the guard's
# answer and the next message together must fit under it, with a margin.
HOOK_LIMIT_SECONDS = 30.0
HOOK_MARGIN_SECONDS = 5.0
# The listener is not started with less than this left of the budget; the next message then waits
# for the next turn.
MIN_LISTEN_SECONDS = 2.0

# Hermes surfaces where the turn's text is typed by the person at this computer.
TYPED_PLATFORMS = frozenset({"cli", "tui", "desktop"})

# The line that ends a Lunchroom envelope, and the labels only an envelope carries (T1).
ENVELOPE_END = "[END LUNCHROOM MESSAGE "
ENVELOPE_LABELS = ("[LUNCHROOM MESSAGE", ENVELOPE_END.rstrip(), "[lr:")

# What the rules that need no record DENY in every session (F1.1, F1.2, I1.2, P1.4), for a call
# the guard could not judge: a call whose text names Lunchroom, a harness program, a session chat
# API or an identity variable is blocked, never asked. Quotes, backslashes and backticks are removed
# before the match, so a split spelling is read as the word it spells.
NO_RECORD_DENY = re.compile(
    r"\b(lunchroom|hermes|codex|claude|opencode|openclaw|acp|sessions_send|CLAUDE_CODE_SESSION_ID|"
    r"CODEX_THREAD_ID|HERMES_SESSION_ID|LUNCHROOM_SESSION|LUNCHROOM_HOME)\b|/api/sessions/",
    # ASCII word characters, as the OpenClaw plugin's pattern reads them: a letter beside the word
    # does not hide it in either plugin.
    re.IGNORECASE | re.ASCII,
)
SPELLING = re.compile(r"[\\\"'`]")


def names_no_record_rule(tool_input):
    """Whether a call's input names what the rules that need no record DENY; input that cannot be
    read cannot be shown free of them."""
    try:
        strings = []
        _collect_strings(tool_input, strings, 0)
    except (TypeError, ValueError, RecursionError):
        return True
    # The match runs on the decoded text, never on its JSON form: a tab or newline in the call is
    # one character there, not a backslash and a letter that would join it to the next word. Each
    # string stands alone and all of them stand together, one to a line. A literal backslash-n,
    # -t or -r, which a shell turns into a separator, is read as one as well (`$'a\\nlunchroom'`).
    decoded = "\n".join(strings)
    for text in (decoded, _unescape(decoded), *strings):
        if NO_RECORD_DENY.search(text) or NO_RECORD_DENY.search(SPELLING.sub("", text)):
            return True
    return False


ESCAPES = re.compile(r"\\(?:([ntr])|u([0-9a-fA-F]{4}))")


def _unescape(text):
    """Backslash-n, -t and -r as a space, and backslash-uXXXX as its character: the text a shell or
    a JSON string inside a string would read."""
    def one(m):
        return " " if m.group(1) else chr(int(m.group(2), 16))
    return ESCAPES.sub(one, text)


def _collect_strings(value, out, depth):
    """Every string in a call's input, keys and values, at any depth; other values as text."""
    if depth > 64:
        raise ValueError("input nested too deeply to read")
    if isinstance(value, str):
        out.append(value)
    elif isinstance(value, dict):
        for key, item in value.items():
            out.append(key if isinstance(key, str) else str(key))
            _collect_strings(item, out, depth + 1)
    elif isinstance(value, (list, tuple, set, frozenset)):
        for item in value:
            _collect_strings(item, out, depth + 1)
    elif value is not None:
        out.append(str(value))

# Sessions another agent's text reached that the guard could not judge: a turn that was not the
# user's typing, or a tool's output with Lunchroom's labels.
UNJUDGED = set()

# The messages handed to each turn and not yet confirmed: session id -> {turn id: message ids}.
HANDED = {}

# The input judged for each tool call, by call id, until its output is reported: a ``cd`` changes the
# session's folder, so the report must carry the input the call was judged and approved with (A2.1).
JUDGED = {}

# The guard program's subcommand for the end of a call that Hermes's own prompt answered (D1.8). One
# place, so a name changes in one edit.
CALL_END_ARGS = ["guard", "callend"]

# The calls the plugin sent to Hermes's prompt and has not reported, by (session id, call id):
# ``{"session", "id"}``. ``post_tool_call`` runs on worker threads, so every use is under
# ``ASKED_LOCK``; an entry is taken out by whoever reports it, so a call is reported once.
ASKED = {}
ASKED_LOCK = threading.Lock()


def harness_version():
    """Hermes's version for Lunchroom (H1.2): ``<commit>@<commit date>`` from its version
    information (``hermes_cli/version_info.py`` ``get_version_info``), the commit alone when it has
    no date, else its base version; empty when it cannot be read, which Lunchroom counts as a
    version it cannot read (UNGUARDED)."""
    try:
        from hermes_cli.version_info import get_version_info

        info = get_version_info()
        if info.commit and info.commit_date:
            return f"{info.commit}@{info.commit_date}"
        if not info.commit:
            # Read once per process, as Hermes reads its own version once: the walk of the
            # checkout and the two git runs stay out of every later call's time.
            if "release" not in CHECKOUT_READ:
                CHECKOUT_READ["release"] = root_checkout_release()
            if CHECKOUT_READ["release"]:
                return CHECKOUT_READ["release"]
        return str(info.commit or info.base_version or "")
    except Exception:
        return ""


# The owner a checkout must have to be trusted (H1.2); a test stands in its own uid.
ROOT_UID = 0

# What `root_checkout_release` found, once per process (key "release").
CHECKOUT_READ = {}

# The most entries a checkout may hold for the plugin to check it; a larger one is not trusted.
CHECKOUT_LIMIT = 200000


def checkout_is_roots(repo):
    """H1.2: what the two git commands the plugin runs (`rev-parse HEAD`, `log -1`) read is the
    checkout's `.git`, so that is what must be root's: the checkout folder itself (whoever can
    write it can replace `.git`) and every entry in `.git`, owned by root, writable by no group or
    other user, with no symbolic link in `.git` (a link could lead git to a file someone else
    writes). The working tree is read by neither command, so a file there that others can write,
    such as the `.venv/.lock` that `uv` leaves writable by all, does not matter. A `.git` with more
    than ``CHECKOUT_LIMIT`` entries, or one that cannot be read, is not trusted."""
    git_dir = os.path.join(repo, ".git")
    try:
        top = os.lstat(repo)
    except OSError:
        return False
    if top.st_uid != ROOT_UID or top.st_mode & 0o022 or stat.S_ISLNK(top.st_mode):
        return False
    seen = 0
    # A folder in `.git` that cannot be listed is one whose entries are not examined: the walk
    # fails, and the checkout is not trusted (os.walk skips such a folder unless told otherwise).
    unread = []
    for root, dirs, files in os.walk(git_dir, onerror=unread.append):
        for name in [None, *dirs, *files]:
            path = root if name is None else os.path.join(root, name)
            seen += 1
            if seen > CHECKOUT_LIMIT:
                return False
            try:
                st = os.lstat(path)
            except OSError:
                return False
            if stat.S_ISLNK(st.st_mode) or st.st_uid != ROOT_UID or st.st_mode & 0o022:
                return False
    return seen > 0 and not unread


def root_checkout_release():
    """H1.2: where Hermes runs from a git checkout another user owns, git refuses to read it
    ("dubious ownership") and Hermes's version information has no commit. The commit and its date
    are then read with git told to trust that one checkout, only when root owns the checkout and
    its ``.git`` and no group or other user can write to either; otherwise nothing is read."""
    try:
        import hermes_cli

        repo = os.path.dirname(os.path.dirname(os.path.realpath(hermes_cli.__file__)))
        if not os.path.isdir(os.path.join(repo, ".git")) or not checkout_is_roots(repo):
            return ""

        def git(*args):
            run = subprocess.run(["git", "-c", f"safe.directory={repo}", *args], cwd=repo,
                                 capture_output=True, text=True, timeout=3)
            return run.stdout.strip() if run.returncode == 0 else ""

        commit = git("rev-parse", "HEAD")
        date = git("log", "-1", "--format=%ct", "HEAD")
        if commit and date.isdigit():
            return f"{commit}@{date}"
        return commit
    except Exception:
        return ""


def hook_budget():
    """The seconds a ``pre_llm_call`` callback may use in all: Hermes's own limit for it, less a
    margin. Hermes's limit is the user's ``plugins.hook_callback_timeout`` when it is a positive
    number, else 30 seconds (``_resolve_hook_callback_timeout``; a value of 0 or less turns the
    bound off, and the plugin then keeps to the default)."""
    limit = HOOK_LIMIT_SECONDS
    try:
        from hermes_cli.plugins import _resolve_hook_callback_timeout

        value = float(_resolve_hook_callback_timeout())
        if value > 0:
            limit = value
    except Exception:  # noqa: BLE001 -- another Hermes: the default limit
        pass
    return max(limit - HOOK_MARGIN_SECONDS, MIN_LISTEN_SECONDS)


def run_guard(args, payload, program=None, timeout=None):
    """One guard run: its exit code and output, ``missing`` or ``failed``. ``timeout`` is the most
    seconds it may take (never more than ``DEADLINE_SECONDS``). Its output is read as UTF-8, with a
    replacement for any byte that is not, so output that is not text never raises."""
    data = dict(payload, harness="hermes", plugin_file=PLUGIN_FILE, harness_version=harness_version())
    limit = DEADLINE_SECONDS if timeout is None else max(0.1, min(DEADLINE_SECONDS, timeout))
    try:
        run = subprocess.run(
            [program or PROGRAM, *args],
            input=json.dumps(data, default=str),
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            timeout=limit,
            check=False,
        )
    except FileNotFoundError:
        return {"missing": True}
    except subprocess.TimeoutExpired:
        return {"failed": "no answer in time"}
    except Exception as exc:  # noqa: BLE001 -- input that cannot be written out is a guard that could not decide
        try:
            return {"failed": str(exc)}
        except Exception:  # noqa: BLE001
            return {"failed": "unreadable input"}
    if run.returncode < 0:
        return {"failed": f"stopped by signal {-run.returncode}"}
    # The guard answers with exit 0 or 2 (H1.5); any other exit is a guard that could not decide.
    if run.returncode not in (0, 2):
        return {"failed": f"exit {run.returncode}"}
    return {"status": run.returncode, "stdout": run.stdout or "", "stderr": run.stderr or ""}


def _approval():
    from tools import approval  # Hermes's own approval gate, in this process

    return approval


def person_would_be_asked():
    """Whether Hermes's approval gate would ask a person for this call now, decided as
    ``tools/approval.py`` ``_run_approval_gate`` decides it (9976f48, lines 968-1038): no person
    under ``_yolo_active()`` (``--yolo``, a session's ``/yolo``, ``hermes -z``) or
    ``approvals.mode: off``; otherwise a person exactly when ``_presence()`` finds a CLI, gateway or
    ask surface. Hermes resolves the cron, single-query and unattended modes (deny, or approve
    without a person) only when ``_presence()`` finds none, and ``_presence()`` itself clears every
    surface for a single-query or cron context and the ask surface for an unattended platform
    with nobody to answer (lines 933-966), so a mode set to approve never applies while a surface
    is found. Anything that cannot be read counts as no person (A1.2)."""
    try:
        gate = _approval()
        if gate._yolo_active() or gate.approval_context._get_approval_mode() == "off":
            return False
        _, cli, gateway, ask = gate._presence(None)
        return bool(cli or gateway or ask)
    except Exception:
        return False


def harness_yolo():
    """Hermes's own YOLO for this session (R2.1): ``--yolo``, a session's ``/yolo`` or
    ``approvals.mode: off``."""
    try:
        gate = _approval()
        return bool(gate.is_approval_bypass_active_for_session(gate.get_current_session_key(default="")))
    except Exception:
        return False


def _decision(stdout):
    try:
        hook = json.loads(stdout.strip() or "{}").get("hookSpecificOutput") or {}
        return hook.get("permissionDecision") or "", hook.get("permissionDecisionReason") or ""
    except (ValueError, AttributeError):
        return None, ""  # an answer that cannot be read is a guard that could not decide (H1.5)


def _context(stdout):
    """What the guard's prompt hook adds to the turn: its ``additionalContext`` (the session's name
    after ``join``, a sender's notices). The guard's ``systemMessage`` is text for the user and is
    never added: Hermes shows the user nothing from ``pre_llm_call`` (it reads only ``context`` from
    a callback's result, ``agent/turn_context.py:838-841`` at 9976f4857b72, and adds it to the turn's
    user message, ``:806-808``), so the plugin tells the guard it cannot show the user text
    (``can_show_user: false``) and the guard leaves the user's notices where ``lunchroom status``
    shows them. ``PluginContext.post_message`` (``hermes_cli/plugins.py:607``) posts text to the
    user only through a live gateway (``hermes_cli/plugin_messages.py:5``, ``:99-111``), which the
    terminal surfaces this plugin serves do not have."""
    try:
        hook = json.loads(stdout.strip() or "{}").get("hookSpecificOutput") or {}
        text = hook.get("additionalContext")
        return text if isinstance(text, str) else ""
    except (ValueError, AttributeError):
        return ""


def _approve(message, tool_call_id, session_id=""):
    # A rule key no other call has, even one with the same call id (A1.1).
    if session_id and tool_call_id:
        with ASKED_LOCK:
            # A second request for a call still waiting shares its record: neither overwrites the other.
            ASKED.setdefault((session_id, tool_call_id), {"session": session_id, "id": tool_call_id})
    return {"action": "approve", "message": message, "rule_key": f"lunchroom:{tool_call_id}:{uuid.uuid4().hex}"}


def _take_asked(session_id, tool_call_id):
    """The asked call under (session, id), taken out of the store: whoever takes it reports it."""
    with ASKED_LOCK:
        return ASKED.pop((session_id, tool_call_id), None)


def _take_one_asked_of(session_id):
    """One asked call of a session, taken out of the store; None when it has none."""
    with ASKED_LOCK:
        for key, value in ASKED.items():
            if value["session"] == session_id:
                return ASKED.pop(key)
        return None


def report_end(entry, outcome, program=None, timeout=None):
    """Tell the guard how a call Hermes's prompt answered ended (D1.8): ``granted`` or ``denied``.
    Never raises and never changes what Hermes does with the call."""
    try:
        run_guard(CALL_END_ARGS, {
            "hook_event_name": "CallEnd",
            "session_id": entry["session"],
            "tool_use_id": entry["id"],
            "outcome": outcome,
        }, program, timeout=timeout)
    except Exception:  # noqa: BLE001 -- a report that fails leaves the call's end to the next turn
        pass


def _base(session_id, event):
    return {"hook_event_name": event, "session_id": session_id, "cwd": os.getcwd()}


def session_folder(task_id=""):
    """The folder Hermes runs a ``terminal`` command in when the call names no ``workdir``, read as
    Hermes reads it (``tools/terminal_tool.py`` at 9976f48): "explicit ``workdir`` > the session's
    own cwd record > ``default_cwd``" (line 993). The record is ``get_session_cwd(session_key)``
    (line 277), written after every completed command of the session (lines 247-252), with
    ``session_key = get_current_session_key(default="") or (task_id or "")`` (lines 1494-1496); the
    default is ``overrides.get("cwd") or get_session_cwd(task_id) or config["cwd"]`` with
    ``overrides = resolve_task_overrides(task_id)`` (lines 1183-1187). Empty when any of that cannot
    be read (another Hermes), or the result is not an absolute folder."""
    try:
        terminal = importlib.import_module("tools.terminal_tool")
        key = _approval().get_current_session_key(default="") or (task_id or "")
        folder = (
            terminal.get_session_cwd(key)
            or (terminal.resolve_task_overrides(task_id) or {}).get("cwd")
            or terminal.get_session_cwd(task_id)
            or terminal._get_env_config()["cwd"]
        )
        folder = os.path.expanduser(folder) if isinstance(folder, str) else ""
        return os.path.normpath(folder) if os.path.isabs(folder) else ""
    except Exception:
        return ""


def judged_input(tool_name, args, task_id=""):
    """The tool input the guard judges. A ``terminal`` command runs in its own ``workdir``
    (``tools/terminal_tool.py:1707``, ``workdir=args.get("workdir")``), else in the session's folder
    (``session_folder``). The input sent carries the absolute directory the command runs in (``~``
    is the user's home; a relative ``workdir`` is read in the session's folder): the guard
    classifies against it and an approval, which covers the exact input (A2.1), covers that
    directory too. Only when the folder cannot be read does the input carry ``workdir_unknown:
    true`` and no ``workdir``, so the guard treats a relative path in the command as a folder it
    cannot resolve."""
    given = args if isinstance(args, dict) else {}
    if tool_name != "terminal":
        return given
    workdir = given.get("workdir")
    workdir = os.path.expanduser(workdir.strip()) if isinstance(workdir, str) else ""
    rest = {k: v for k, v in given.items() if k not in ("workdir", "workdir_unknown")}
    if not (workdir and os.path.isabs(workdir)):
        folder = session_folder(task_id)
        if not folder:
            return dict(rest, workdir_unknown=True)
        workdir = os.path.join(folder, workdir) if workdir else folder
    return dict(rest, workdir=os.path.normpath(workdir))


def shown(tool_name, tool_input):
    """A call in full, set apart as the call's input (A1.4, A2.2)."""
    try:
        body = json.dumps(tool_input, default=str, indent=2)
    except (TypeError, ValueError):
        body = repr(tool_input)
    return f"\n\nThe call's input, in full ({tool_name}):\n{body}"


def _text_id(value):
    """An id as Hermes gives it: text, else none (a number, a list or an object names nothing)."""
    return value if isinstance(value, str) else ""


def on_pre_tool_call(tool_name="", args=None, session_id="", tool_call_id="", task_id="", program=None, **_):
    """``pre_tool_call``: the guard's answer as Hermes's. Any error while reading the call keeps it
    from running (H1.5)."""
    session_id, tool_call_id = _text_id(session_id), _text_id(tool_call_id)
    try:
        return _on_pre_tool_call(tool_name, args, session_id, tool_call_id, task_id, program)
    except Exception:  # noqa: BLE001 -- fail closed
        return {"action": "block", "message": (
            "Lunchroom could not check this call. This call is DENIED, because Lunchroom could not read it.")}


def _on_pre_tool_call(tool_name, args, session_id, tool_call_id, task_id, program):
    channel = person_would_be_asked()
    tool_input = judged_input(tool_name, args, task_id)
    if tool_call_id:
        JUDGED[(session_id, tool_call_id)] = tool_input

    def not_judged(why):
        # A call the guard cannot judge goes to the user where a person can answer, else it is
        # DENIED; one the rules needing no record DENY is DENIED, in every uncertain state (H1.5).
        # The request shows the call in full (A1.4). Each message says what happened, why and
        # what happens next in its first line.
        if names_no_record_rule(tool_input):
            return {"action": "block", "message": (
                f"Lunchroom {why} This call is DENIED, because it names Lunchroom or another "
                "agent's session, which Lunchroom never lets through unchecked.")}
        if not channel:
            return {"action": "block", "message": (
                f"Lunchroom {why} Nobody can be asked in this session, so this call is DENIED.")}
        request = (f"Lunchroom {why} It needs your yes: answer this prompt (yes runs the call once, "
                   "no stops it). Lunchroom did not read any script this call runs.")
        return _approve(f"{request}{shown(tool_name, tool_input)}", tool_call_id, session_id)

    # What the plugin knows that the guard's record may not: no session, or another agent's text
    # that reached the session unjudged. The guard still answers for the call when it can.
    held = ""
    if not session_id:
        held = "could not tell which session this call belongs to."
    elif session_id in UNJUDGED:
        held = "could not check another agent's text that reached this session."
    if held and names_no_record_rule(tool_input):
        return not_judged(held)

    def unable(what):
        return not_judged(f"{held} ({what})" if held else f"could not check this call: {what}.")

    payload = dict(
        _base(session_id, "PreToolUse"),
        tool_name=tool_name,
        tool_input=tool_input,
        tool_use_id=tool_call_id,
        approval_channel=channel,
        harness_yolo=harness_yolo(),
    )
    run = run_guard(["guard", "pretool"], payload, program)
    if "failed" in run or run.get("missing"):
        return unable(run.get("failed") or "its program is missing")
    if run["status"] == 2:
        return {"action": "block", "message": run["stderr"].strip() or "Lunchroom: DENIED"}
    decision, reason = _decision(run["stdout"])
    if decision is None:
        return unable("its answer could not be read")
    if decision == "ask":
        # A1.2: with nobody to ask, a call for the user is DENIED, whatever the guard answered.
        if not channel:
            return {"action": "block", "message": reason or "Lunchroom: DENIED"}
        # A1.4: the guard's request shows the call; one with no text of its own is given it here.
        request = reason or f"Lunchroom: this call needs your yes.{shown(tool_name, tool_input)}"
        return _approve(f"{request}\nLunchroom {held}" if held else request, tool_call_id, session_id)
    # The guard lets it run, but the plugin knows what the guard's record may not: the user decides.
    return not_judged(held) if held else None


def on_post_tool_call(tool_name="", args=None, result=None, session_id="", tool_call_id="",
                      status="", task_id="", program=None, **_):
    """``post_tool_call``: another agent's text in a tool's output holds the session (T1). A call
    Hermes blocked or cancelled did not run, so it is not reported. When the guard cannot judge
    an output carrying Lunchroom's labels, the plugin holds the session itself (H1.5). An error
    while reading the call holds the session too, and never reaches Hermes."""
    session_id, tool_call_id = _text_id(session_id), _text_id(tool_call_id)
    try:
        return _on_post_tool_call(tool_name, args, result, session_id, tool_call_id, status, task_id, program)
    except Exception:  # noqa: BLE001 -- fail closed
        if session_id:
            UNJUDGED.add(session_id)
        return None


def _on_post_tool_call(tool_name, args, result, session_id, tool_call_id, status, task_id, program):
    judged = JUDGED.pop((session_id, tool_call_id), None) if tool_call_id else None
    # A call Hermes's prompt answered ends here (D1.8): `ok` or `error` is Granted; `blocked`,
    # `cancelled` and any other status is Denied, since there is no evidence the call ran.
    asked = _take_asked(session_id, tool_call_id) if session_id and tool_call_id else None
    if asked is not None:
        report_end(asked, "granted" if status in ("ok", "error") else "denied", program)
    # A call Hermes blocked or cancelled did not run, so its output is not judged.
    if not session_id or status in ("blocked", "cancelled"):
        return None
    run = run_guard(["guard", "posttool"], dict(
        _base(session_id, "PostToolUse"),
        tool_name=tool_name,
        tool_input=judged if judged is not None else judged_input(tool_name, args, task_id),
        tool_use_id=tool_call_id,
        tool_response=result,
    ), program)
    try:
        text = result if isinstance(result, str) else json.dumps(result, default=str)
    except Exception:  # noqa: BLE001 -- output that cannot be written out cannot be shown free of the labels
        text = ENVELOPE_LABELS[0]
    if "status" not in run and any(label in (text or "") for label in ENVELOPE_LABELS):
        UNJUDGED.add(session_id)
    return None


def end_orphans(session_id, program, started, budget):
    """A call sent to Hermes's prompt with no report when the session's next turn starts is told as
    Denied, since the turn ended with the call not run (D1.8). Every such call is forgotten, so a
    report that comes later changes nothing; one is reported only while the callback's budget has
    time for it, and the guard's own prompt run closes any left."""
    while True:
        left = budget - (time.monotonic() - started)
        # With no time left for one more report, the calls not yet taken stay in the store for a
        # later turn (and the guard's own prompt run closes them in its record).
        if left < 2 * MIN_LISTEN_SECONDS:
            return
        entry = _take_one_asked_of(session_id)
        if entry is None:
            return
        report_end(entry, "denied", program, timeout=min(DEADLINE_SECONDS, left / 2))


def on_pre_llm_call(session_id="", user_message=None, platform="", turn_id="", program=None, **_):
    """``pre_llm_call``: the turn's text reaches the guard, then the next waiting message is handed
    to the turn (D1.1). A subagent's turn is its parent session's text, reaching it through
    Hermes's own delegation: another agent's text, which holds the subagent's session (T1). Any
    error while reading the turn fails open, with the session held (H1.5)."""
    session_id = _text_id(session_id)
    if not session_id:
        return None
    try:
        return _on_pre_llm_call(session_id, user_message, platform, turn_id, program)
    except Exception:  # noqa: BLE001 -- this hook cannot block a turn: hold the session instead
        UNJUDGED.add(session_id)
        return None


def _on_pre_llm_call(session_id, user_message, platform, turn_id, program):
    typed = isinstance(platform, str) and platform in TYPED_PLATFORMS
    # Hermes lets this hook fail open (only pre_tool_call fails closed), so text that is not the
    # user's is unjudged until the guard has judged it: a hook that dies here leaves it so (H1.5).
    was_unjudged = session_id in UNJUDGED
    if not typed:
        UNJUDGED.add(session_id)
    # One budget for the whole callback, under Hermes's limit for it (hook_budget): the calls left
    # with no report first, then the guard's answer, then the next message with what is left.
    started = time.monotonic()
    budget = hook_budget()
    end_orphans(session_id, program, started, budget)
    # A message that cannot be written out is a turn the guard cannot judge: the hook fails open
    # with the session held (on_pre_llm_call).
    text = user_message if isinstance(user_message, str) else json.dumps(user_message, default=str)
    try:
        judged = run_guard(["guard", "prompt"], dict(
            _base(session_id, "UserPromptSubmit"),
            prompt=text or "",
            prompt_id=turn_id,
            user_typed=typed,
            # Hermes shows the user nothing from this hook (see ``_context``).
            can_show_user=False,
        ), program, timeout=budget)
    except Exception:  # noqa: BLE001 -- any failure is a guard that could not judge (H1.5)
        # Not judged: the turn's text stays unjudged, so every later call of this session goes
        # to the user, or is DENIED where nobody can answer; nothing is handed to this turn.
        return None
    if "status" in judged and not typed and not was_unjudged:
        UNJUDGED.discard(session_id)
    elif typed and judged.get("status") == 0:
        UNJUDGED.discard(session_id)
    told = _context(judged.get("stdout", "")) if typed and judged.get("status") == 0 else ""
    left = budget - (time.monotonic() - started)
    if left < MIN_LISTEN_SECONDS:
        # No time left to hand the turn a message: it waits for the next turn.
        return {"context": told} if told else None
    got = run_guard(["listen", "--once"], dict(_base(session_id, "Stop"), can_show_user=False,
                                               user_typed=typed), program, timeout=left)
    if got.get("status") == 2 and got["stderr"].strip():
        context = got["stderr"].strip()
        ids = _envelope_ids(context)
        if ids:
            HANDED.setdefault(session_id, {})[turn_id] = ids
        return {"context": f"{told}\n\n{context}" if told else context}
    return {"context": told} if told else None


def _envelope_ids(text):
    """The ids of the envelopes whose end line stands as a whole line in ``text``."""
    ids = set()
    for line in text.splitlines():
        line = line.rstrip()
        if line.startswith(ENVELOPE_END) and line.endswith("]"):
            ids.add(line[len(ENVELOPE_END):-1])
    return ids


def _text(content):
    """A request message's text: a string, or the text parts of a list of parts."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def on_pre_api_request(session_id="", request_messages=None, api_request_id="", turn_id="",
                       program=None, **_):
    """``pre_api_request``: a request to the model. When it is a request of the turn the plugin
    handed messages to, and its last user message carries those envelopes, that text reaches the
    guard's prompt hook, which confirms them DELIVERED (D1.1); the plugin then forgets them, so a
    later request of the same turn, which sends the same context again, confirms nothing more.
    Hermes's ``pre_api_request`` observes and cannot block a request."""
    turns = HANDED.get(session_id) or {}
    ids = turns.get(turn_id)
    if not ids or not isinstance(request_messages, list):
        return None
    users = [m for m in request_messages if isinstance(m, dict) and m.get("role") == "user"]
    text = _text(users[-1].get("content")) if users else ""
    if not ids <= _envelope_ids(text):
        return None
    run = run_guard(["guard", "prompt"], dict(_base(session_id, "UserPromptSubmit"), prompt=text,
                                              prompt_id=api_request_id), program)
    if "status" in run:
        turns.pop(turn_id, None)
        if not turns:
            HANDED.pop(session_id, None)
    return None


def register(ctx):
    ctx.register_hook("pre_tool_call", on_pre_tool_call)
    ctx.register_hook("post_tool_call", on_post_tool_call)
    ctx.register_hook("pre_llm_call", on_pre_llm_call)
    ctx.register_hook("pre_api_request", on_pre_api_request)
