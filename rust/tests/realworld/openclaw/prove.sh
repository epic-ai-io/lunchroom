#!/bin/bash
# Proof 8 (openclaw): the real OpenClaw gateway, Lunchroom's plugin loaded in it, and real tool calls.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/openclaw/prove.sh | tee openclaw.txt
#
# Needs podman and network access (docker.io node:24, the npm registry).

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context openclaw) || exit 1
run_image lunchroom-realworld-openclaw Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
