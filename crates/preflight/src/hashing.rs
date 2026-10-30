/// Stable lowercase hex for persisted hashes and protocol identifiers.
pub fn hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .as_ref()
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 15) as usize] as char,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::hex;
    use sha2::{Digest, Sha256};

    #[test]
    fn persisted_digest_format_is_unchanged() {
        assert_eq!(hex([]), "");
        assert_eq!(hex([0, 1, 15, 16, 127, 128, 255]), "00010f107f80ff");
        assert_eq!(
            hex(Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut hash = Sha256::new();
        hash.update(b"a");
        hash.update(b"bc");
        assert_eq!(hex(hash.finalize()), hex(Sha256::digest(b"abc")));
    }
}
