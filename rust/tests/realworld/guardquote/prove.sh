#!/bin/bash
# Proof 16 (guardquote): the guard over-match fix (task #48). A script that only quotes Lunchroom's
# commands, or names the GitHub repository epic-ai-io/lunchroom after an option, must run; a script
# that quotes a user-only command is still DENIED.
#
#   LUNCHROOM_11=<static program> rust/tests/realworld/guardquote/prove.sh | tee guardquote.txt
#
# Evidence for the fix: the installed 1.0.1 guard on the build host DENIED exactly such scripts (the
# mixed-store proof's own script, and `bash -n` of the console proof's scripts, as `configuration`
# and `fence`); run against 1.1 every ordinary script below must run.

. "$(dirname "$0")/../common/host.sh"

CTX=$(make_context guardquote) || exit 1
run_image lunchroom-realworld-guardquote Containerfile "$CTX"
rm -rf "$CTX"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
