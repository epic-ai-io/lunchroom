#!/usr/bin/env python3
"""A small pure-Python Ed25519 verifier (RFC 8032, section 6's reference code), for the controller
approval proof where python's `cryptography` is not in the container.

    ed25519_verify.py --selftest                       RFC 8032 test 1; exit 0 when it verifies
    ed25519_verify.py PUBKEY_B64 SIGNATURE_B64 FILE    exit 0 when SIGNATURE verifies over FILE's bytes

Spec section 17 item 9: the signature "verifies against the public key over the canonical bytes
with an Ed25519 verifier other than Lunchroom's". This is slow (milliseconds per check) and is
used for one record at a time.
"""

import base64
import hashlib
import sys

P = 2**255 - 19
D = -121665 * pow(121666, P - 2, P) % P
Q = 2**252 + 27742317777372353535851937790883648493


def inv(x):
    return pow(x, P - 2, P)


def recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * inv(D * y * y + 1) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P != 0:
        x = x * pow(2, (P - 1) // 4, P) % P
    if (x * x - x2) % P != 0:
        return None
    if (x & 1) != sign:
        x = P - x
    return x


GY = 4 * inv(5) % P
GX = recover_x(GY, 0)
G = (GX, GY, 1, GX * GY % P)


def add(a, b):
    aa = (a[1] - a[0]) * (b[1] - b[0]) % P
    bb = (a[1] + a[0]) * (b[1] + b[0]) % P
    cc = 2 * a[3] * b[3] * D % P
    dd = 2 * a[2] * b[2] % P
    e, f, g, h = bb - aa, dd - cc, dd + cc, bb + aa
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def mul(s, point):
    q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            q = add(q, point)
        point = add(point, point)
        s >>= 1
    return q


def same(a, b):
    if (a[0] * b[2] - b[0] * a[2]) % P != 0:
        return False
    return (a[1] * b[2] - b[1] * a[2]) % P == 0


def decompress(s):
    if len(s) != 32:
        return None
    y = int.from_bytes(s, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % P)


def verify(public, message, signature):
    if len(public) != 32 or len(signature) != 64:
        return False
    a = decompress(public)
    r = decompress(signature[:32])
    if a is None or r is None:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= Q:
        return False
    h = int.from_bytes(hashlib.sha512(signature[:32] + public + message).digest(), "little") % Q
    return same(mul(s, G), add(r, mul(h, a)))


def selftest():
    # RFC 8032 section 7.1, TEST 1 (empty message).
    public = bytes.fromhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
    signature = bytes.fromhex(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
        "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    )
    return verify(public, b"", signature) and not verify(public, b"x", signature)


def main() -> int:
    if sys.argv[1:] == ["--selftest"]:
        return 0 if selftest() else 1
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    public = base64.b64decode(sys.argv[1], validate=True)
    signature = base64.b64decode(sys.argv[2], validate=True)
    with open(sys.argv[3], "rb") as f:
        message = f.read()
    return 0 if verify(public, message, signature) else 1


if __name__ == "__main__":
    sys.exit(main())
