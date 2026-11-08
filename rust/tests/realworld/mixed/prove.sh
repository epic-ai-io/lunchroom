#!/bin/bash
# Proof 3 (mixed): the public 1.0.1 release program and the 1.1.0 program on one store.
#
#   LUNCHROOM_102=<static program> rust/tests/realworld/mixed/prove.sh | tee mixed.txt
#
# Downloads lunchroom-v1.0.1-x86_64-unknown-linux-musl.tar.gz and SHA256SUMS from the GitHub
# release epic-ai-io/lunchroom v1.0.1 with `gh release download` (needs gh and its login), checks
# the checksum, and runs the proof in a container.

. "$(dirname "$0")/../common/host.sh"

REL=$(mktemp -d)
ASSET=lunchroom-v1.0.1-x86_64-unknown-linux-musl.tar.gz
if gh release download v1.0.1 --repo epic-ai-io/lunchroom --pattern "$ASSET" --pattern SHA256SUMS --dir "$REL" >"$REL/gh.out" 2>&1 \
    && [ -s "$REL/$ASSET" ] && [ -s "$REL/SHA256SUMS" ]; then
    host_pass "gh release download: $ASSET and SHA256SUMS of epic-ai-io/lunchroom v1.0.1"
else
    host_fail "gh release download of epic-ai-io/lunchroom v1.0.1 failed: $(head -c 300 "$REL/gh.out")"
    echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
    exit 1
fi
if (cd "$REL" && sha256sum -c --ignore-missing SHA256SUMS 2>&1 | grep -q "^$ASSET: OK$"); then
    host_pass "sha256sum -c SHA256SUMS: $ASSET: OK ($(sha256sum "$REL/$ASSET" | cut -d' ' -f1))"
else
    host_fail "the checksum of $ASSET does not match SHA256SUMS"
    echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
    exit 1
fi

CTX=$(make_context mixed) || exit 1
cp "$REL/$ASSET" "$REL/SHA256SUMS" "$CTX/"
run_image lunchroom-realworld-mixed Containerfile "$CTX"
rm -rf "$CTX" "$REL"

echo
echo "RESULT: $HOST_PASSES PASS, $HOST_FAILS FAIL"
[ "$HOST_FAILS" -eq 0 ]
