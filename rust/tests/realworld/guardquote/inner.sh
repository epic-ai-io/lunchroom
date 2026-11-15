#!/bin/bash
# Proof 16 (guardquote), inside the container: the real Claude Code (Anthropic's apt package), Lunchroom
# 1.1 installed by `lunchroom install claude-code`, the API answered by mockapi.py so that a prompt
# `FILETEST <label>` becomes a Bash tool call whose command is the text of /run/sess/cmd-<label>.txt,
# which the real program's real PreToolUse hook judges.
#
# The defect (task #48): "unparsable script naming owner/lunchroom with an option is DENIED as
# configuration". A1.3 (docs/spec/lunchroom-spec.md section 7 item 3) denies a command that runs one
# of Lunchroom's user-only commands, and a script that quotes one, since "there the guard cannot tell
# running a command from naming it" (K1.8); a word after Lunchroom's name that is an option and not
# one of those commands runs nothing (Lunchroom takes its command from its first argument). So:
#
#   ordinary: scripts the shell parser cannot read (`if`, `$REL`), naming the GitHub repository
#     `epic-ai-io/lunchroom` after `--repo`, or a path `tools/lunchroom` with `--pattern`, RUN;
#   user-only: scripts that quote `lunchroom approve`, `yolo`, `register`, even after `--help` or
#     `--version`, are still DENIED, as `configuration`.
#
# Each ordinary script calls a stand-in (a `gh` and a `tools/lunchroom` that write a line to
# /tmp/stub.log), so that "it ran" is a line in a file, not a guess from Claude Code's answer.

set -u
. /opt/proof/lib.sh

heading "facts of this system"
echo "claude: $(/usr/bin/claude --version 2>&1 | head -1)"
echo "lunchroom: $("$PROOF/lunchroom" --version 2>&1 | head -1)"

make_user
export MOCK_API=1
seed_claude
install_lunchroom
# 1.1, spec section 17 item 2: the install is finished when the setgid program and /var/lib/lunchroom/<uid> are in place.
lr_installed && pass "lunchroom install claude-code finished" || { fail "install did not finish"; finish; }

# The stand-ins: what an ordinary script calls.
cat > /usr/local/bin/gh <<'SH'
#!/bin/sh
echo "gh $*" >> /tmp/stub.log
exit 0
SH
chmod 755 /usr/local/bin/gh
mkdir -p "$DEVHOME/tools" "$DEVHOME/q"
cat > "$DEVHOME/tools/lunchroom" <<'SH'
#!/bin/sh
echo "tools/lunchroom $*" >> /tmp/stub.log
exit 0
SH
chmod 755 "$DEVHOME/tools/lunchroom"
: > /tmp/stub.log
chmod 666 /tmp/stub.log
chown -R dev "$DEVHOME/tools" "$DEVHOME/q"

# script NAME BODY: a script file of dev's, run by the tool call `bash <file>`.
script() {
    printf '#!/bin/bash\n%s\n' "$2" > "$DEVHOME/q/$1.sh"
    chown dev "$DEVHOME/q/$1.sh"
    printf 'bash %s/q/%s.sh' "$DEVHOME" "$1" > "$SESS/cmd-$1.txt"
    chmod 644 "$SESS/cmd-$1.txt"
}
mkdir -p "$SESS"
chmod 777 "$SESS"

start_session s1 /usr/bin/claude --permission-mode default --allowedTools Bash
sleep 3
status | grep -q "^s1	" && pass "the session joined as s1" || fail "the session did not join"

heading "0. control: an ordinary command runs"
printf 'echo guardquote-control' > "$SESS/cmd-c0.txt"
chmod 644 "$SESS/cmd-c0.txt"
R=$(probe s1 FILETEST c0)
case "$R" in *guardquote-control*) pass "control: the Bash call ran (its output came back)" ;; *) fail "control: the call did not run: '${R:-nothing}'" ;; esac

heading "1. scripts that only quote Lunchroom's name with options: they run (task #48)"
script o0 'REL=$(mktemp -d)
if gh release download v1.0.1 --repo epic-ai-io/lunchroom --pattern "$ASSET" --dir "$REL"; then echo ok; fi'
script o1 'gh release view --repo epic-ai-io/lunchroom --json tagName "$X"'
script o2 'if true; then tools/lunchroom --pattern x; fi'
script o3 'URL=https://github.com/epic-ai-io/lunchroom/releases; echo "see $URL"; gh release list --repo epic-ai-io/lunchroom --limit 3'
for label in o0 o1 o2 o3; do
    BEFORE=$(wc -l < /tmp/stub.log)
    R=$(probe s1 FILETEST "$label")
    AFTER=$(wc -l < /tmp/stub.log)
    echo "MEASURED $label: Claude Code gave back: $(echo "${R:-nothing}" | head -c 300)"
    case "$R" in
        *DENIED*) fail "$label: a script that only quotes Lunchroom's name with an option was DENIED: $(echo "$R" | head -c 300)" ;;
        "") fail "$label: no result came back" ;;
        *) [ "$AFTER" -gt "$BEFORE" ] \
               && pass "$label: the script ran (its stand-in wrote a line to /tmp/stub.log)" \
               || fail "$label: the script was not DENIED but did not run either: $(echo "$R" | head -c 200)" ;;
    esac
done

heading "2. scripts that quote a user-only command are still DENIED (A1.3, K1.8)"
script u0 'if true; then lunchroom approve 3; fi'
script u1 'if [ -n "$X" ]; then lunchroom --help approve 3; fi'
script u2 'X=$(date); epic-ai-io/lunchroom yolo all till 14:00'
script u3 'if true; then lunchroom --version register x; fi'
for label in u0 u1 u2 u3; do
    R=$(probe s1 FILETEST "$label")
    echo "MEASURED $label: Claude Code gave back: $(echo "${R:-nothing}" | head -c 300)"
    case "$R" in
        *DENIED*configuration*|*configuration*DENIED*) pass "$label: DENIED as configuration" ;;
        *DENIED*) fail "$label: DENIED, but not as configuration: $(echo "$R" | head -c 300)" ;;
        "") fail "$label: no result came back" ;;
        *) fail "$label: a script quoting a user-only command was NOT denied: $(echo "$R" | head -c 300)" ;;
    esac
done
[ "$(wc -l < /tmp/stub.log)" = "$(grep -c . /tmp/stub.log)" ] && measured "stub log: $(wc -l < /tmp/stub.log) lines, written by the ordinary scripts only"

heading "3. the fences stay: a raw codex queue and a sudo to lunchroom are still DENIED"
D=$(probe s1 DENYTEST d1)
case "$D" in *DENIED*) pass "a raw 'codex queue' is still DENIED" ;; "") fail "no result for the raw codex queue" ;; *) fail "a raw codex queue was not denied: $D" ;; esac
printf 'sudo -u lunchroom id' > "$SESS/cmd-s0.txt"
chmod 644 "$SESS/cmd-s0.txt"
S=$(probe s1 FILETEST s0)
case "$S" in *DENIED*) pass "'sudo -u lunchroom id' is still DENIED (item 5)" ;; "") fail "no result for sudo -u lunchroom" ;; *) fail "sudo -u lunchroom was not denied: $S" ;; esac

finish
