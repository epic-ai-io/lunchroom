#!/bin/bash
# Proof 9 (hermes): the real Hermes Agent, Lunchroom's plugin loaded in it, and real tool calls.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/hermes/prove.sh | tee hermes.txt
#
# Needs podman and network access (docker.io python:3.14, github.com, PyPI).

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context hermes) || exit 1
run_image lunchroom-realworld-hermes Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
