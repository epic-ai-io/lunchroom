#!/bin/bash
# Usage: LUNCHROOM_11=<static program> rust/tests/realworld/cards/run-cards-1.1.sh [CARD ...]
#
# Plays the six printed cards of Lunchroom 1.1 end to end with live sessions of Claude Code, Codex,
# OpenClaw and Hermes (spec section 16, item 22) and prints one PASS or FAIL total. Run it in a terminal of
# your own: the installed guard blocks these kits inside an agent's session. The cards are 1 to 6; with
# none named, all six run, one container at a time.
#
# Environment:
#   LUNCHROOM_11        the static program to test (required): the one `cargo build --release --target
#                       x86_64-unknown-linux-musl --package=lunchroom` makes.
#   CARD_CELLS=base     play only the cell the spec's test line names for each card (default: every cell
#                       of item 22's grid: the hearing agent on each of the four harnesses).
#   CARD_HARNESSES      a list (`"cc cx"`) narrowing the grid to those harnesses (cc Claude Code, cx
#                       Codex, oc OpenClaw, hm Hermes).
#   PODMAN              the podman to use (default: podman).
#   CARD_WORK           where the build context is made (default: $TMPDIR, else /tmp).
#   CARD_LOGS           where each card's log goes (default: logs-cards-1.1 beside this script).
#   CARD_MIN_FREE_GIB   the free space the work folder must have before an image is built (default 5).
#   CARD_MAX_LOAD       the 1-minute load above which a container waits (default 6).
#
# Preflight, before any image is built: every script passes `bash -n`, every Python file parses, the
# helpers' self-test passes. A card whose log prints no RESULT line is a FAIL.

set -u

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REAL=$(cd "$HERE/.." && pwd -P)
PODMAN=${PODMAN:-podman}
WORK=${CARD_WORK:-${TMPDIR:-/tmp}}
LOGS=${CARD_LOGS:-$HERE/logs-cards-1.1}
MIN_FREE_GIB=${CARD_MIN_FREE_GIB:-5}
MAX_LOAD=${CARD_MAX_LOAD:-6}
IMAGE=lunchroom-cards-1.1

say() { printf '%s\n' "$*"; }

case "${1:-}" in
    -h | --help)
        sed -n 2,4p "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
esac

CARDS=("$@")
[ "${#CARDS[@]}" -gt 0 ] || CARDS=(1 2 3 4 5 6)
for c in "${CARDS[@]}"; do
    case "$c" in 1 | 2 | 3 | 4 | 5 | 6) ;; *) say "REFUSED: '$c' is not a card (1 to 6)"; exit 2 ;; esac
done

say "== preflight"
if [ -z "${LUNCHROOM_11:-}" ] || [ ! -x "$LUNCHROOM_11" ]; then
    say "REFUSED: LUNCHROOM_11 must name the static program to test (an executable file); it is '${LUNCHROOM_11:-unset}'"
    exit 3
fi
command -v "$PODMAN" >/dev/null 2>&1 || { say "REFUSED: $PODMAN is not installed"; exit 3; }
mkdir -p "$WORK" "$LOGS" || { say "REFUSED: cannot make $WORK or $LOGS"; exit 3; }

BAD=0
for f in "$HERE"/*.sh "$REAL"/common/*.sh; do
    [ -f "$f" ] || continue
    if ! bash -n "$f" 2>"$WORK/syntax.err"; then
        say "SYNTAX ERROR in $f:"
        sed 's/^/    /' "$WORK/syntax.err"
        BAD=1
    fi
done
for f in "$HERE"/*.py "$REAL"/common/*.py; do
    [ -f "$f" ] || continue
    if ! python3 -I -c 'import ast, sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$f" 2>"$WORK/syntax.err"; then
        say "SYNTAX ERROR in $f:"
        sed 's/^/    /' "$WORK/syntax.err"
        BAD=1
    fi
done
[ "$BAD" -eq 0 ] || { say "REFUSED: a script does not parse. Nothing was built."; exit 6; }
say "every script passes bash -n and every Python file parses"
if ! python3 -I "$HERE/selftest.py" >"$WORK/selftest.out" 2>&1; then
    say "REFUSED: the helpers' self-test failed:"
    grep -E '^FAIL|^RESULT' "$WORK/selftest.out"
    exit 6
fi
say "helpers' self-test: $(grep '^RESULT' "$WORK/selftest.out")"

wait_load() {
    local load
    while :; do
        load=$(cut -d' ' -f1 /proc/loadavg)
        [ "${load%.*}" -lt "$MAX_LOAD" ] && break
        say "load $load is $MAX_LOAD or more: waiting 30 s"
        sleep 30
    done
}
require_disk() {
    local free_kb
    free_kb=$(df --output=avail -k "$WORK" | tail -1 | tr -d ' ')
    say "free space on $WORK: $((free_kb / 1024)) MiB"
    if [ "$free_kb" -lt $((MIN_FREE_GIB * 1024 * 1024)) ]; then
        say "REFUSED: $WORK has under $MIN_FREE_GIB GiB free. Nothing was started."
        exit 4
    fi
}

say
say "== build context and image"
require_disk
wait_load
CTX=$(mktemp -d "$WORK/ctx.cards.XXXXXX")
cp "$LUNCHROOM_11" "$CTX/lunchroom"
cp "$REAL"/common/lib.sh "$REAL"/common/ptyrun.py "$REAL"/common/sessionpty.py "$REAL"/common/plaintext.py "$REAL"/common/screen.py "$CTX/"
cp "$HERE"/cardmock.py "$HERE"/cardtool.py "$HERE"/lib-cards.sh "$HERE"/inner.sh "$HERE"/fakeprog.sh "$HERE"/Containerfile "$CTX/"
cp "$HERE"/card[1-6].sh "$CTX/"
# The console proof's pty driver (it has the options the console needs) and screen reader.
cp "$REAL/../console-proof/ptyrun.py" "$CTX/consolepty.py"
cp "$REAL/../console-proof/lastscreen.py" "$CTX/lastscreen.py"
say "program: $("$LUNCHROOM_11" --version 2>&1 | head -1)  sha256 $(sha256sum "$LUNCHROOM_11" | cut -d' ' -f1)"
if ! "$PODMAN" build -q -t "$IMAGE" -f "$CTX/Containerfile" "$CTX" >"$CTX/build.log" 2>&1; then
    say "FAIL the image did not build"
    tail -30 "$CTX/build.log"
    "$PODMAN" rmi -f "$IMAGE" >/dev/null 2>&1
    rm -rf "$CTX"
    exit 7
fi
say "built image $IMAGE ($("$PODMAN" image inspect "$IMAGE" --format '{{.Size}}' | awk '{printf "%d MB", $1/1048576}'))"

TOTAL_PASS=0
TOTAL_FAIL=0
SUMMARY=()
for c in "${CARDS[@]}"; do
    say
    say "######## card $c"
    require_disk
    wait_load
    log=$LOGS/card$c.txt
    "$PODMAN" run --rm -e "CARD=$c" -e "CARD_CELLS=${CARD_CELLS:-all}" -e "CARD_HARNESSES=${CARD_HARNESSES:-}" \
        "$IMAGE" /opt/proof/inner.sh 2>&1 | tee "$log"
    p=$(grep -c '^PASS ' "$log")
    f=$(grep -c '^FAIL ' "$log")
    if ! grep -q '^RESULT: ' "$log"; then
        say "FAIL card $c printed no RESULT line"
        f=$((f + 1))
    fi
    TOTAL_PASS=$((TOTAL_PASS + p))
    TOTAL_FAIL=$((TOTAL_FAIL + f))
    SUMMARY+=("$(printf 'card %s: %4s PASS %3s FAIL (log: %s)' "$c" "$p" "$f" "$log")")
done
"$PODMAN" rmi -f "$IMAGE" >/dev/null 2>&1
rm -rf "$CTX"

say
say "== summary"
for line in "${SUMMARY[@]}"; do say "$line"; done
say "TOTAL: $TOTAL_PASS PASS, $TOTAL_FAIL FAIL"
if [ "$TOTAL_FAIL" -eq 0 ]; then
    say "CARDS 1.1: PASS"
    exit 0
fi
say "CARDS 1.1: FAIL"
exit 1
