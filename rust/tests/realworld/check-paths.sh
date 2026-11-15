#!/bin/bash
# Usage: rust/tests/realworld/check-paths.sh   (no arguments; builds nothing, starts no container)
#
# Checks that the host paths of the real-world proofs come from variables with neutral defaults:
#   LUNCHROOM_TEST_ROOT  LUNCHROOM_BUILD_CMD  LUNCHROOM_TARGET_DIR  LUNCHROOM_11   (see common/host.sh)
# and that run-all-1.1.sh hands them on. Optional:
#   CHECK_ROOT  a root to prove the "old value gives the old paths" class with (default: a scratch folder).
#               Nothing is created under it: mkdir is stubbed for the host.sh cases.
#   CHECK_TMP   the scratch folder's parent (default ${TMPDIR:-/tmp}); needs 5 GiB free for the run-all cases.
# Prints PASS or FAIL per check and one RESULT line; exit 0 only with no FAIL.

set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
RUST=$(cd "$HERE/../.." && pwd -P)
REPO=$(cd "$RUST/.." && pwd -P)
SCRATCH=$(mktemp -d "${CHECK_TMP:-${TMPDIR:-/tmp}}/check-paths.XXXXXX") || exit 2
PASSES=0
FAILS=0
ROOT_X=${CHECK_ROOT:-$SCRATCH/root}
SUFFIX=x86_64-unknown-linux-musl/release/lunchroom

ok() { echo "PASS $*"; PASSES=$((PASSES + 1)); }
bad() { echo "FAIL $*"; FAILS=$((FAILS + 1)); }
# expect NAME ACTUAL EXPECTED
expect() {
    if [ "$2" = "$3" ]; then ok "$1"; else bad "$1: got [$2] want [$3]"; fi
}

# host NAME=VALUE ...: sources common/host.sh in a clean environment (mkdir stubbed) and prints
# TMPDIR, the first PATH entry and LUNCHROOM_11, one per line.
host() {
    env -i PATH="$PATH" HOME="$SCRATCH" "$@" bash -c '
        mkdir() { :; }
        . "$1/common/host.sh"
        printf "%s\n%s\n%s\n" "$TMPDIR" "${PATH%%:*}" "$LUNCHROOM_11"
    ' _ "$HERE"
}
field() { echo "$1" | sed -n "${2}p"; }

# --- defaults -----------------------------------------------------------------------------------
out=$(host TMPDIR="$SCRATCH")
expect "default root, TMPDIR set: TMPDIR" "$(field "$out" 1)" "$SCRATCH/lunchroom-proof/tmp"
expect "default root, TMPDIR set: PATH head" "$(field "$out" 2)" "$SCRATCH/lunchroom-proof/bin"
expect "default target: program" "$(field "$out" 3)" "$SCRATCH/lunchroom-proof/target-main/$SUFFIX"
out=$(host)
expect "default root, TMPDIR unset: TMPDIR" "$(field "$out" 1)" "/tmp/lunchroom-proof/tmp"
expect "default root, TMPDIR unset: program" "$(field "$out" 3)" "/tmp/lunchroom-proof/target-main/$SUFFIX"

# --- a variable set to a given value gives the paths under that value ---------------------------
out=$(host LUNCHROOM_TEST_ROOT="$ROOT_X")
expect "root set: TMPDIR" "$(field "$out" 1)" "$ROOT_X/tmp"
expect "root set: PATH head" "$(field "$out" 2)" "$ROOT_X/bin"
expect "root set: program" "$(field "$out" 3)" "$ROOT_X/target-main/$SUFFIX"
out=$(host LUNCHROOM_TEST_ROOT="$ROOT_X" LUNCHROOM_TARGET_DIR=/elsewhere/target)
expect "target set: program" "$(field "$out" 3)" "/elsewhere/target/$SUFFIX"
out=$(host LUNCHROOM_TEST_ROOT="$ROOT_X" LUNCHROOM_TARGET_DIR=/elsewhere/target LUNCHROOM_11=/given/program)
expect "LUNCHROOM_11 set wins over target" "$(field "$out" 3)" "/given/program"
msg=$(env -i PATH="$PATH" HOME="$SCRATCH" LUNCHROOM_TEST_ROOT="$ROOT_X" LUNCHROOM_BUILD_CMD=/my/build.sh \
    LUNCHROOM_11="$SCRATCH/absent" bash -c 'mkdir() { :; }; . "$1/common/host.sh"; make_context x' _ "$HERE" 2>&1 >/dev/null)
case "$msg" in *"/my/build.sh build"*) ok "build command set: named in the missing-program message" ;; *) bad "build command set: message [$msg]" ;; esac
msg=$(env -i PATH="$PATH" HOME="$SCRATCH" LUNCHROOM_TEST_ROOT="$ROOT_X" LUNCHROOM_11="$SCRATCH/absent" \
    bash -c 'mkdir() { :; }; . "$1/common/host.sh"; make_context x' _ "$HERE" 2>&1 >/dev/null)
case "$msg" in *"$ROOT_X/build11/cargo.sh build"*) ok "build command default: under the root" ;; *) bad "build command default: message [$msg]" ;; esac

# --- an empty variable is the same as an unset one ----------------------------------------------
out=$(host TMPDIR="$SCRATCH" LUNCHROOM_TEST_ROOT= LUNCHROOM_TARGET_DIR= LUNCHROOM_BUILD_CMD=)
expect "empty variables: TMPDIR" "$(field "$out" 1)" "$SCRATCH/lunchroom-proof/tmp"
expect "empty variables: program" "$(field "$out" 3)" "$SCRATCH/lunchroom-proof/target-main/$SUFFIX"

# --- run-all-1.1.sh: a copy of the kit tree in a scratch checkout ------------------------------
# make_tree DIR: DIR/rust/tests/{realworld,console-proof} as they are now.
make_tree() {
    mkdir -p "$1/rust/tests"
    cp -r "$HERE" "$1/rust/tests/realworld"
    cp -r "$RUST/tests/console-proof" "$1/rust/tests/console-proof"
}
# A root that does not hold the checkout: refused (exit 3) before anything is built.
OUTSIDE=$SCRATCH/outside
make_tree "$OUTSIDE/checkout"
OTHER_ROOT=$SCRATCH/other-root
msg=$(env LUNCHROOM_TEST_ROOT="$OTHER_ROOT" bash "$OUTSIDE/checkout/rust/tests/realworld/run-all-1.1.sh" 2>&1)
rc=$?
expect "root without the checkout: exit code" "$rc" "3"
case "$msg" in *"REFUSED: this checkout ("*") is not under $OTHER_ROOT;"*) ok "root without the checkout: message names the root" ;; *) bad "root without the checkout: message [$msg]" ;; esac
# A root that holds the checkout: preflight passes, and the build command sees the hand-off.
INSIDE=$SCRATCH/inside
mkdir -p "$INSIDE/bin"
printf '#!/bin/sh\necho podman stub\n' >"$INSIDE/bin/podman"
printf '#!/bin/sh\necho "HANDOFF root=$LUNCHROOM_TEST_ROOT cmd=$LUNCHROOM_BUILD_CMD target=$LUNCHROOM_TARGET_DIR tmp=$TMPDIR"\nexit 1\n' >"$INSIDE/build.sh"
chmod +x "$INSIDE/bin/podman" "$INSIDE/build.sh"
make_tree "$INSIDE/checkout"
msg=$(env -u LUNCHROOM_TARGET_DIR -u LUNCHROOM_11 LUNCHROOM_TEST_ROOT="$INSIDE" LUNCHROOM_BUILD_CMD="$INSIDE/build.sh" \
    bash "$INSIDE/checkout/rust/tests/realworld/run-all-1.1.sh" 2>&1)
rc=$?
expect "root holding the checkout: stops at the build (exit 7)" "$rc" "7"
case "$msg" in
    *"HANDOFF root=$INSIDE cmd=$INSIDE/build.sh target=$INSIDE/target-main tmp=$INSIDE/tmp"*) ok "run-all hands root, build command, target and TMPDIR to the build" ;;
    *) bad "run-all hand-off: output [$(echo "$msg" | tail -5)]" ;;
esac
# Root unset: the default root (under TMPDIR) is derived once and handed on, with the default build
# command and target under it.
DFLT_TMP=$SCRATCH/dflt
DFLT=$DFLT_TMP/lunchroom-proof
mkdir -p "$DFLT/bin" "$DFLT/build11"
cp "$INSIDE/bin/podman" "$DFLT/bin/podman"
printf '#!/bin/sh\necho "HANDOFF root=$LUNCHROOM_TEST_ROOT cmd=$LUNCHROOM_BUILD_CMD target=$LUNCHROOM_TARGET_DIR tmp=$TMPDIR"\nexit 1\n' >"$DFLT/build11/cargo.sh"
chmod +x "$DFLT/bin/podman" "$DFLT/build11/cargo.sh"
make_tree "$DFLT/checkout"
msg=$(env -u LUNCHROOM_TEST_ROOT -u LUNCHROOM_BUILD_CMD -u LUNCHROOM_TARGET_DIR -u LUNCHROOM_11 TMPDIR="$DFLT_TMP" \
    bash "$DFLT/checkout/rust/tests/realworld/run-all-1.1.sh" 2>&1)
rc=$?
expect "root unset: stops at the build (exit 7)" "$rc" "7"
case "$msg" in
    *"HANDOFF root=$DFLT cmd=$DFLT/build11/cargo.sh target=$DFLT/target-main tmp=$DFLT/tmp"*) ok "root unset: default root, build command and target handed on" ;;
    *) bad "root unset hand-off: output [$(echo "$msg" | tail -5)]" ;;
esac
# With the build command unset it defaults under the root: the preflight names the missing file.
msg=$(env -u LUNCHROOM_BUILD_CMD -u LUNCHROOM_TARGET_DIR LUNCHROOM_TEST_ROOT="$INSIDE" \
    bash "$INSIDE/checkout/rust/tests/realworld/run-all-1.1.sh" 2>&1)
rc=$?
expect "build command unset: exit 5" "$rc" "5"
case "$msg" in *"REFUSED: $INSIDE/build11/cargo.sh is not there"*) ok "build command default: under the root" ;; *) bad "build command unset: message [$msg]" ;; esac

# --- the public export finds nothing under rust/tests/realworld --------------------------------
if [ -f "$REPO/tools/export_public.py" ]; then
    hits=$(python3 "$REPO/tools/export_public.py" "$SCRATCH/export" 2>&1 | grep -c '^rust/tests/realworld')
    expect "export hits under rust/tests/realworld" "$hits" "0"
else
    bad "tools/export_public.py is not there: the export class is not checked"
fi

echo "RESULT: $PASSES PASS, $FAILS FAIL"
[ "$FAILS" -eq 0 ]
