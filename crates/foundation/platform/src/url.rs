//! The parts of a URL the platform layer reads: the path of a `file://` URL.

/// The part of a `file:` URI after the scheme and a `localhost` authority, still
/// percent-encoded. Accepts both `file:///C:/dir` and the `file:/C:/dir` spelling of
/// `File.toURI`. `None` for any other scheme.
pub(crate) fn file_rest(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("file:")?;
    let Some(after) = rest.strip_prefix("//") else {
        // `File.toURI` writes one slash before the path.
        return Some(rest);
    };
    match after.strip_prefix("localhost") {
        Some(local) if local.is_empty() || local.starts_with('/') => Some(local),
        _ => Some(after),
    }
}

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
