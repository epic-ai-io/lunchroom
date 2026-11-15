#!/bin/bash
# Usage: rust/tests/realworld/run-all-1.1.sh   (no arguments; in the terminal of the person running the kit; logs go to rust/tests/realworld/logs-1.1/)
#
# Variables (every host path comes from one of these; none is hard-coded):
#   LUNCHROOM_TEST_ROOT  the one folder the proofs may write to (default ${TMPDIR:-/tmp}/lunchroom-proof).
#                        It holds tmp/ (TMPDIR), bin/ (the podman wrapper), build11/ (the cargo wrapper)
#                        and target-main/ (the cargo target folder) by default. This checkout must itself
#                        be under it.
#   LUNCHROOM_BUILD_CMD  the cargo wrapper that builds into the shared target folder (default
#                        $LUNCHROOM_TEST_ROOT/build11/cargo.sh).
#   LUNCHROOM_TARGET_DIR the shared cargo target folder (default $LUNCHROOM_TEST_ROOT/target-main).
#   LUNCHROOM_11         set by this script to the built program; common/host.sh reads it.
#
# Builds the Lunchroom 1.1 static program, runs every real-world kit and the console proof, writes each
# log to logs-1.1/, and prints one PASS or FAIL total.
#
#   1. Preflight, before anything is built: it refuses to start with under 5 GiB free on
#      $LUNCHROOM_TEST_ROOT (the disk rule: everything goes under that folder, nothing on /); it checks
#      every script with `bash -n` and every Python file with the parser (the installed 1.0.1 guard
#      blocks `bash -n` of scripts that quote Lunchroom's commands in an agent's session, so this runs
#      here, in the terminal of the person running the kit); it checks the podman wrapper
#      $LUNCHROOM_TEST_ROOT/bin/podman.
#   2. Build: $LUNCHROOM_BUILD_CMD build --release --target x86_64-unknown-linux-musl -p
#      lunchroom, into $LUNCHROOM_TARGET_DIR.
#   3. Kits, one container at a time, through $LUNCHROOM_TEST_ROOT/bin/podman (which sets --root and
#      --runroot under $LUNCHROOM_TEST_ROOT): packages, upgrade, hooktext, hookfields, headless, permmodes,
#      hermes, openclaw, openclawlink, openclawws, codex, codexodd, codexstopped, clocklock, mixed,
#      guardquote, echokill. Then the console proof: prove.sh (an account whose sudo asks for a
#      password; it leaves the console alone for 15 minutes, so it runs about 25 minutes), and
#      wide.sh for password-free sudo (answer yes, answer no, a later rule wins).
#   4. The total counts every PASS and FAIL line of every log; a kit that prints no RESULT line is a FAIL.
#
# TMPDIR is $LUNCHROOM_TEST_ROOT/tmp. Nothing is written outside $LUNCHROOM_TEST_ROOT except the logs and
# the build in this checkout, which must itself be under $LUNCHROOM_TEST_ROOT.

set -u

PROOF_ROOT=${LUNCHROOM_TEST_ROOT:-${TMPDIR:-/tmp}/lunchroom-proof}
export LUNCHROOM_TEST_ROOT=$PROOF_ROOT
BUILD_CMD=${LUNCHROOM_BUILD_CMD:-$PROOF_ROOT/build11/cargo.sh}
TARGET_DIR=${LUNCHROOM_TARGET_DIR:-$PROOF_ROOT/target-main}
export LUNCHROOM_BUILD_CMD=$BUILD_CMD LUNCHROOM_TARGET_DIR=$TARGET_DIR
case "${1:-}" in
    -h | --help)
        sed -n 2p "$0" | sed 's/^# //'
        exit 0
        ;;
esac

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
RUST=$(cd "$HERE/../.." && pwd -P)
LOGS=$HERE/logs-1.1
export TMPDIR=$PROOF_ROOT/tmp
export PATH=$PROOF_ROOT/bin:$PATH
mkdir -p "$TMPDIR"

say() { printf '%s\n' "$*"; }

# ---------------------------------------------------------------------------------------------
say "== preflight"
case "$RUST" in
    $PROOF_ROOT/*) ;;
    *) say "REFUSED: this checkout ($RUST) is not under $PROOF_ROOT; the build and the logs must not go on /"; exit 3 ;;
esac
FREE_KIB=$(df --output=avail -k "$PROOF_ROOT" | tail -1 | tr -d ' ')
say "free space on $PROOF_ROOT: $((FREE_KIB / 1024)) MiB"
if [ "$FREE_KIB" -lt $((5 * 1024 * 1024)) ]; then
    say "REFUSED: $PROOF_ROOT has $((FREE_KIB / 1024)) MiB free, under 5 GiB. Nothing was started."
    exit 4
fi
[ -x "$PROOF_ROOT/bin/podman" ] || { say "REFUSED: $PROOF_ROOT/bin/podman (the wrapper that keeps podman's storage under the proof root) is not there"; exit 5; }
[ -x "$BUILD_CMD" ] || { say "REFUSED: $BUILD_CMD is not there"; exit 5; }
command -v "$PROOF_ROOT/bin/podman" >/dev/null && say "podman: $(podman --version 2>&1 | head -1) (storage on $PROOF_ROOT/containers)"

SYNTAX_BAD=0
for f in "$HERE"/run-all-1.1.sh "$HERE"/common/*.sh "$HERE"/*/*.sh "$HERE"/../console-proof/*.sh; do
    [ -f "$f" ] || continue
    if ! bash -n "$f" 2>"$TMPDIR/syntax.err"; then
        say "SYNTAX ERROR in $f:"
        sed 's/^/    /' "$TMPDIR/syntax.err"
        SYNTAX_BAD=1
    fi
done
for f in "$HERE"/common/*.py "$HERE"/*/*.py "$HERE"/../console-proof/*.py; do
    [ -f "$f" ] || continue
    if ! python3 -I -c 'import ast, sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$f" 2>"$TMPDIR/syntax.err"; then
        say "SYNTAX ERROR in $f:"
        sed 's/^/    /' "$TMPDIR/syntax.err"
        SYNTAX_BAD=1
    fi
done
if [ "$SYNTAX_BAD" -ne 0 ]; then
    say "REFUSED: a script does not parse. Nothing was built."
    exit 6
fi
say "every script passes bash -n and every Python file parses"

# ---------------------------------------------------------------------------------------------
say
say "== build: the static 1.1 program (release, x86_64-unknown-linux-musl) through $BUILD_CMD"
cd "$RUST" || exit 3
if ! "$BUILD_CMD" build --release --target x86_64-unknown-linux-musl -p lunchroom; then
    say "FAIL the build failed"
    exit 7
fi
export LUNCHROOM_11=$TARGET_DIR/x86_64-unknown-linux-musl/release/lunchroom
[ -x "$LUNCHROOM_11" ] || { say "FAIL the built program is not at $LUNCHROOM_11"; exit 7; }
say "built: $("$LUNCHROOM_11" --version 2>&1 | head -1)  sha256 $(sha256sum "$LUNCHROOM_11" | cut -d' ' -f1)"

mkdir -p "$LOGS"
TOTAL_PASS=0
TOTAL_FAIL=0
SUMMARY=()

# record NAME LOGFILE: counts the PASS and FAIL lines of a log into the total; a log with no RESULT
# line is a FAIL.
record() {
    local name=$1 log=$2 p f
    p=$(grep -c '^PASS ' "$log")
    f=$(grep -c '^FAIL ' "$log")
    if ! grep -q '^RESULT: ' "$log"; then
        say "FAIL $name printed no RESULT line"
        f=$((f + 1))
    fi
    TOTAL_PASS=$((TOTAL_PASS + p))
    TOTAL_FAIL=$((TOTAL_FAIL + f))
    SUMMARY+=("$(printf '%-24s %4s PASS %3s FAIL' "$name" "$p" "$f")")
    say "== $name: $p PASS, $f FAIL (log: $log)"
}

# ---------------------------------------------------------------------------------------------
say
say "== the kits"
for kit in packages upgrade hooktext hookfields headless permmodes hermes openclaw openclawlink openclawws \
    codex codexodd codexstopped clocklock mixed guardquote echokill; do
    say
    say "######## $kit"
    log=$LOGS/$kit.txt
    bash "$HERE/$kit/prove.sh" 2>&1 | tee "$log"
    record "$kit" "$log"
done

# ---------------------------------------------------------------------------------------------
say
say "######## console proof"
. "$HERE/common/host.sh"
CTX=$(mktemp -d "$TMPDIR/ctx.console.XXXXXX")
cp "$LUNCHROOM_11" "$CTX/lunchroom"
cp "$HERE"/../console-proof/* "$CTX/"
cp "$HERE/common/hold.py" "$HERE/common/plaintext.py" "$CTX/"
IMAGE=lunchroom-console-proof
require_disk
wait_load
if podman build -q -t "$IMAGE" -f "$CTX/Containerfile" "$CTX" >"$CTX/build.log" 2>&1; then
    say "built image $IMAGE"
    for run in "prove|prove.sh|" "wide-yes|wide.sh|WIDE=yes" "wide-no|wide.sh|WIDE=no" "wide-later|wide.sh|WIDE=later"; do
        IFS='|' read -r tag script envvar <<< "$run"
        log=$LOGS/console-$tag.txt
        say
        say "######## console proof: $script ${envvar:+($envvar)}"
        require_disk
        wait_load
        podman run --rm ${envvar:+-e "$envvar"} "$IMAGE" "/opt/proof/$script" 2>&1 | tee "$log"
        record "console-$tag" "$log"
    done
    podman rmi -f "$IMAGE" >/dev/null 2>&1
else
    say "FAIL the console proof's image did not build"
    tail -20 "$CTX/build.log"
    TOTAL_FAIL=$((TOTAL_FAIL + 1))
    SUMMARY+=("console-proof             image did not build")
fi
rm -rf "$CTX"

# ---------------------------------------------------------------------------------------------
say
say "== summary"
for line in "${SUMMARY[@]}"; do say "$line"; done
say "TOTAL: $TOTAL_PASS PASS, $TOTAL_FAIL FAIL"
if [ "$TOTAL_FAIL" -eq 0 ]; then
    say "RUN-ALL 1.1: PASS"
    exit 0
fi
say "RUN-ALL 1.1: FAIL"
exit 1
