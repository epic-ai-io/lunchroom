#!/bin/bash
# Proof 10 (hookfields): which hook output field Claude Code shows by default.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/hookfields/prove.sh | tee hookfields.txt

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context headless) || exit 1
run_image lunchroom-realworld-headless Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
