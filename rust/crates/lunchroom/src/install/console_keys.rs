//! The console's Ed25519 key pair (§17.9): the private key is a 32-byte seed, kept as one line of
//! standard base64; the public key is the 32-byte public key, one line of standard base64.

use ed25519_dalek::SigningKey;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in standard base64 (RFC 4648 section 4), with padding.
#[must_use]
pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let at = |shift: u32| char::from(ALPHABET[((n >> shift) & 63) as usize]);
        out.push(at(18));
        out.push(at(12));
        out.push(if chunk.len() > 1 { at(6) } else { '=' });
        out.push(if chunk.len() > 2 { at(0) } else { '=' });
    }
    out
}

/// The bytes of `text`, standard base64 with padding; `None` for any other form (a character
/// outside the alphabet, a wrong length, padding anywhere but the end).
#[must_use]
pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let raw = text.as_bytes();
    if raw.is_empty() || !raw.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| ALPHABET.iter().position(|a| *a == c);
    let mut out = Vec::with_capacity(raw.len() / 4 * 3);
    let last = raw.len() / 4 - 1;
    for (i, quad) in raw.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && i != last) {
            return None;
        }
        let mut n = 0u32;
        for c in &quad[..4 - pad] {
            n = (n << 6) | u32::try_from(value(*c)?).ok()?;
        }
        n <<= 6 * u32::try_from(pad).ok()?;
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..4 - pad]);
    }
    Some(out)
}

/// The private key's file text: the seed as one line of base64.
#[cfg(test)]
#[must_use]
pub fn private_text(seed: &[u8; 32]) -> String {
    format!("{}\n", b64_encode(seed))
}

/// The public key's file text for `seed`: the 32-byte public key as one line of base64.
#[must_use]
pub fn public_text(seed: &[u8; 32]) -> String {
    let key = SigningKey::from_bytes(seed).verifying_key();
    format!("{}\n", b64_encode(key.as_bytes()))
}

/// The seed in a private key file's text; `None` where the text is not one line of base64 that
/// holds 32 bytes.
#[must_use]
pub fn seed_from(text: &str) -> Option<[u8; 32]> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains(['\n', '\r', ' ']) {
        return None;
    }
    b64_decode(line)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4648 section 10 test vectors, both ways.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64_encode(plain.as_bytes()), coded);
            if !coded.is_empty() {
                assert_eq!(b64_decode(coded).as_deref(), Some(plain.as_bytes()));
            }
        }
    }

    // A form that is not standard base64 with padding decodes to nothing.
    #[test]
    fn other_forms_do_not_decode() {
        for bad in [
            "", "Zg", "Zg=", "Z===", "Zm9v=A==", "Zm9_", "Zm9v\n", "Zm 9", "=m9v",
        ] {
            assert_eq!(b64_decode(bad), None, "{bad:?}");
        }
    }

    // RFC 8032 section 7.1, test 1: the seed's public key.
    #[test]
    fn the_public_key_is_the_rfc_8032_one() {
        let seed: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let want: [u8; 32] = [
            0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64,
            0x07, 0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68,
            0xf7, 0x07, 0x51, 0x1a,
        ];
        assert_eq!(public_text(&seed), format!("{}\n", b64_encode(&want)));
        assert_eq!(seed_from(&private_text(&seed)), Some(seed));
    }

    // A private key file that is not one line holding 32 bytes gives no seed.
    #[test]
    fn a_damaged_private_key_gives_no_seed() {
        let seed = [7u8; 32];
        assert_eq!(seed_from(&private_text(&seed)), Some(seed));
        assert_eq!(seed_from(&b64_encode(&seed)), Some(seed));
        for bad in [
            String::new(),
            b64_encode(&[1u8; 31]),
            b64_encode(&[1u8; 33]),
            format!("{}\n{}\n", b64_encode(&seed), b64_encode(&seed)),
            "not base64!\n".to_owned(),
        ] {
            assert_eq!(seed_from(&bad), None, "{bad:?}");
        }
    }
}
