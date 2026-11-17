//! The parts of a URL the platform layer reads: the path of a `file://` URL.

/// The bytes a `%XX` sequence stands for. `None` when a percent is not followed by two
/// hexadecimal digits.
pub(crate) fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let (digits, tail) = rest.split_at_checked(2)?;
        let digits = std::str::from_utf8(digits).ok()?;
        bytes.push(u8::from_str_radix(digits, 16).ok()?);
        rest = tail;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn a_percent_sequence_becomes_a_byte() {
        assert_eq!(percent_decode("a%20b").unwrap(), b"a b");
        assert_eq!(percent_decode("%D0%BF%D1%80").unwrap(), "пр".as_bytes());
    }

    #[test]
    fn a_broken_percent_sequence_is_not_a_url() {
        assert!(percent_decode("a%2").is_none());
        assert!(percent_decode("a%zz").is_none());
    }
}
