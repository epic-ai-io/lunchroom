#!/bin/bash
# Proof 6 (hooktext): what the user sees of Lunchroom's multi-line hook texts in Claude Code (and,
# when the Codex part is built, in Codex).
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/hooktext/prove.sh | tee hooktext.txt

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context hooktext) || exit 1
run_image lunchroom-realworld-hooktext Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
