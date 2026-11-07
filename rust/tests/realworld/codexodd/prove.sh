#!/bin/bash
# Proof 7 (codex): the real Codex CLI 0.166.0, its real hooks, a real apply_patch call, and what the
# user sees of Lunchroom's multi-line hook text.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/codex/prove.sh | tee codex.txt
#
# Needs podman and network access (docker.io node:22, the npm registry).

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context codexodd) || exit 1
run_image lunchroom-realworld-codexodd Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
