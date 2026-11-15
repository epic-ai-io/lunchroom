#!/bin/bash
# Host-side helpers for the real-world proofs of Lunchroom 1.1 (sourced by each proof's prove.sh).
#
# Variables (every host path comes from one of these; none is hard-coded):
#   LUNCHROOM_TEST_ROOT  the one folder the proofs may write to (default ${TMPDIR:-/tmp}/lunchroom-proof).
#                        TMPDIR becomes $LUNCHROOM_TEST_ROOT/tmp and the podman wrapper is
#                        $LUNCHROOM_TEST_ROOT/bin/podman.
#   LUNCHROOM_BUILD_CMD  the cargo wrapper (default $LUNCHROOM_TEST_ROOT/build11/cargo.sh); named in a message only.
#   LUNCHROOM_TARGET_DIR the shared cargo target folder (default $LUNCHROOM_TEST_ROOT/target-main).
#   LUNCHROOM_11         the 1.1 static program (default $LUNCHROOM_TARGET_DIR/x86_64-unknown-linux-musl/release/lunchroom).
#
# The proofs build container images with podman. The disk rule: everything the proofs write goes under
# $LUNCHROOM_TEST_ROOT and nothing on /. So the free space of that folder is read before each image
# build and the proof stops, as a FAIL, under 5 GiB; temporary files go to TMPDIR=$LUNCHROOM_TEST_ROOT/tmp;
# podman runs through $LUNCHROOM_TEST_ROOT/bin/podman, which keeps its storage there; each image is
# removed when its proof has run. The concurrency budget applies: one build or test roll-call at a
# time, so the load is read first.

PROOF_ROOT=${LUNCHROOM_TEST_ROOT:-${TMPDIR:-/tmp}/lunchroom-proof}
BUILD_CMD=${LUNCHROOM_BUILD_CMD:-$PROOF_ROOT/build11/cargo.sh}
TARGET_DIR=${LUNCHROOM_TARGET_DIR:-$PROOF_ROOT/target-main}
export TMPDIR=$PROOF_ROOT/tmp
mkdir -p "$TMPDIR"
case ":$PATH:" in
    *":$PROOF_ROOT/bin:"*) ;;
    *) PATH=$PROOF_ROOT/bin:$PATH ;;
esac
export PATH

REALWORLD=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUST=$(cd "$REALWORLD/../.." && pwd)
# The 1.1 static program. run-all-1.1.sh builds it with $BUILD_CMD into the one shared target folder
# $TARGET_DIR; LUNCHROOM_11 names another.
LUNCHROOM_11=${LUNCHROOM_11:-$TARGET_DIR/x86_64-unknown-linux-musl/release/lunchroom}
HOST_FAILS=0
HOST_PASSES=0

host_pass() { echo "PASS $*"; HOST_PASSES=$((HOST_PASSES + 1)); }
host_fail() { echo "FAIL $*"; HOST_FAILS=$((HOST_FAILS + 1)); }

# Stops the proof (exit 1, as a FAIL) when free space on $PROOF_ROOT is under 5 GiB.
require_disk() {
    local free_kb
    free_kb=$(df --output=avail -k "$PROOF_ROOT" | tail -1 | tr -d ' ')
    echo "MEASURED free space on $PROOF_ROOT: $((free_kb / 1024)) MB"
    if [ "$free_kb" -lt 5242880 ]; then
        echo "FAIL less than 5 GiB free on $PROOF_ROOT: not building an image"
        echo "RESULT: $HOST_PASSES PASS, $((HOST_FAILS + 1)) FAIL"
        exit 1
    fi
}

# Waits until the 1-minute load is under 6.
wait_load() {
    local load
    while :; do
        load=$(cut -d' ' -f1 /proc/loadavg)
        [ "${load%.*}" -lt 6 ] && break
        echo "load $load is 6 or more: waiting 30 s"
        sleep 30
    done
}

# Makes a build context holding the program and the shared files, the files of the proof's folder
# ($1), and echoes its path. The context is a folder under $TMPDIR (under the proof root).
make_context() {
    local proof=$1 ctx
    if [ ! -x "$LUNCHROOM_11" ]; then
        echo "FAIL the 1.1 program is not at $LUNCHROOM_11 (run-all-1.1.sh builds it: $BUILD_CMD build --release --target x86_64-unknown-linux-musl -p lunchroom)" >&2
        return 1
    fi
    ctx=$(mktemp -d "$TMPDIR/ctx.XXXXXX")
    cp "$LUNCHROOM_11" "$ctx/lunchroom"
    cp "$REALWORLD"/common/* "$ctx/"
    cp "$REALWORLD/$proof"/* "$ctx/"
    echo "$ctx"
}

# run_image TAG CONTAINERFILE CONTEXT [PODMAN RUN ARGUMENT ...]: builds, runs, prints the run's
# output, counts its PASS and FAIL lines into HOST_PASSES and HOST_FAILS, and removes the image.
# A build or run that fails to start is a FAIL.
run_image() {
    local tag=$1 file=$2 ctx=$3 out status
    shift 3
    require_disk
    wait_load
    echo "== building $tag from $file"
    if ! podman build -q -t "$tag" -f "$ctx/$file" "$ctx" >"$ctx/build.log" 2>&1; then
        host_fail "$tag: the image did not build"
        tail -20 "$ctx/build.log"
        podman rmi -f "$tag" >/dev/null 2>&1
        return 1
    fi
    echo "MEASURED image $tag: $(podman image inspect "$tag" --format '{{.Size}}' | awk '{printf "%d MB", $1/1048576}')"
    out="$ctx/run.out"
    podman run --rm "$@" "$tag" >"$out" 2>&1
    status=$?
    cat "$out"
    HOST_PASSES=$((HOST_PASSES + $(grep -c '^PASS ' "$out")))
    HOST_FAILS=$((HOST_FAILS + $(grep -c '^FAIL ' "$out")))
    if [ "$status" -ne 0 ] && ! grep -q '^FAIL ' "$out"; then
        host_fail "$tag: the container exited $status with no FAIL line"
    fi
    podman rmi -f "$tag" >/dev/null 2>&1 && echo "MEASURED image $tag removed"
}
