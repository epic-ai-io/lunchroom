#!/bin/bash
# Proof 5 (clock skew and lock contention): stamps from a clock that stepped back, and a store lock
# another Lunchroom process holds.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/clocklock/prove.sh | tee clocklock.txt

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context clocklock) || exit 1
run_image lunchroom-realworld-clocklock Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
