#!/bin/bash
# Proof 1 (packages): Claude Code from Anthropic's real apt, dnf and apk repositories, a real
# session running /usr/bin/claude, and what Lunchroom 1.0.2 says about it.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/packages/prove.sh | tee packages.txt
#
# Needs podman, network access to downloads.claude.ai, and 2 GB free on /. Prints PASS, FAIL or
# MEASURED for each check; exits non-zero on any FAIL.

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context packages) || exit 1
for distro in ${DISTROS:-apt dnf apk}; do
    echo
    echo "######## $distro"
    run_image "lunchroom-realworld-packages-$distro" "Containerfile.$distro" "$CTX"
done
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
