#!/bin/bash
# Proof 17 (echokill): the terminal's echo is on again after the program is killed (SIGKILL), or
# terminated (SIGTERM, SIGHUP), while the passphrase is typed with echo off.
#
#   LUNCHROOM_11=<static program> rust/tests/realworld/echokill/prove.sh | tee echokill.txt

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context echokill) || exit 1
run_image lunchroom-realworld-echokill Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
