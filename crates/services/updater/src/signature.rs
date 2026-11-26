//! The Ed25519 signature of the `.sha256` file of a release. The release publishes
//! the signature next to the other files as `<checksum>.sig`; it holds the raw 64
//! bytes of an Ed25519 signature over the bytes of the checksum file.
//!
//! Until the project decides where the private key lives, [`PUBLIC_KEY`] is empty and
//! the hash alone is checked, as before. With a key set, an update whose signature is
//! missing or wrong is refused.

/// The Ed25519 public key that signs the `.sha256` of a release, as 64 hex digits.
/// Empty while the updater trusts the hash alone.
pub const PUBLIC_KEY: &str = "";

/// Whether [`PUBLIC_KEY`] turns the check of a signature on.
pub fn required() -> bool {
    !PUBLIC_KEY.is_empty()
}

/// Whether `signature` is a valid Ed25519 signature of `message` by [`PUBLIC_KEY`].
pub fn verify(message: &[u8], signature: &[u8]) -> bool {
    verify_with(PUBLIC_KEY, message, signature)
}

/// [`verify`] against the key `key`, which the tests of the updater pass in themselves.
pub fn verify_with(key: &str, message: &[u8], signature: &[u8]) -> bool {
    let Some(key) = decode_key(key) else {
        return false;
    };
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(message, signature)
        .is_ok()
}

/// The 32 bytes of a key written as 64 hex digits.
fn decode_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        key[index] = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Some(key)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
