#!/bin/bash
# Host-side helpers for the real-world proofs of Lunchroom 1.0.2 (sourced by each proof's prove.sh).
#
# The proofs build container images with podman. Disk is tight on the machine that runs them, so
# before each image build the free space of / is read and the proof stops, as a FAIL, under 2 GB;
# each image is removed when its proof has run. The concurrency budget applies: one build or test
# roll-call at a time, so the load is read first.

REALWORLD=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUST=$(cd "$REALWORLD/../.." && pwd)
LUNCHROOM_102=${LUNCHROOM_102:-$RUST/target/x86_64-unknown-linux-musl/release/lunchroom}
HOST_FAILS=0
HOST_PASSES=0

host_pass() { echo "PASS $*"; HOST_PASSES=$((HOST_PASSES + 1)); }
host_fail() { echo "FAIL $*"; HOST_FAILS=$((HOST_FAILS + 1)); }

# Stops the proof (exit 1, as a FAIL) when free space on / is under 2 GB.
require_disk() {
    local free_kb
    free_kb=$(df --output=avail -k / | tail -1 | tr -d ' ')
    echo "MEASURED free space on /: $((free_kb / 1024)) MB"
    if [ "$free_kb" -lt 3145728 ]; then
        echo "FAIL less than 3 GB free on /: not building an image"
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
# ($1), and echoes its path.
make_context() {
    local proof=$1 ctx
    if [ ! -x "$LUNCHROOM_102" ]; then
        echo "FAIL the 1.0.2 program is not at $LUNCHROOM_102 (cargo build --release --target x86_64-unknown-linux-musl -p lunchroom)" >&2
        return 1
    fi
    ctx=$(mktemp -d)
    cp "$LUNCHROOM_102" "$ctx/lunchroom"
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
