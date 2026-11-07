#!/bin/bash
# Proof 2 (upgrade): in an Ubuntu container with Anthropic's apt package claude-code, a real
# session runs while the package is upgraded (and, separately, reinstalled). Lunchroom 1.0.2 must
# say "program replaced while running", not "release cannot be read".
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/upgrade/prove.sh | tee upgrade.txt

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context upgrade) || exit 1
run_image lunchroom-realworld-upgrade Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
