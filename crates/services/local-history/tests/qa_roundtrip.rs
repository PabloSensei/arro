//! Checks from qa: the byte round trip Local History relies on when it puts a
//! version back into a closed file (`decode` the current file for its format, `encode` the text).

use text_core::{decode, encode};

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let win1251: Vec<u8> = "привет, мир\r\nвторая строка\r\n"
        .chars()
        .map(|c| match c {
            'а'..='я' => (c as u32 - 'а' as u32 + 0xE0) as u8,
            ' ' | ',' | '\r' | '\n' => c as u8,
            _ => b'?',
        })
        .collect();
    let mut utf16le = vec![0xFF, 0xFE];
    for u in "строка\r\nline 2\r\n".encode_utf16() {
        utf16le.extend_from_slice(&u.to_le_bytes());
    }
    let mut utf16be = vec![0xFE, 0xFF];
    for u in "строка\nline 2\n".encode_utf16() {
        utf16be.extend_from_slice(&u.to_be_bytes());
    }
    vec![
        ("empty", vec![]),
        ("lf", b"a\nb\n".to_vec()),
        ("crlf", b"a\r\nb\r\n".to_vec()),
        ("cr only", b"a\rb\r".to_vec()),
        ("mixed", b"a\r\nb\nc\r\nd\n\r\ne".to_vec()),
        ("no final newline", b"a\r\nb".to_vec()),
        (
            "utf8 bom crlf",
            [&[0xEF, 0xBB, 0xBF][..], "\u{43f}\u{440}\r\nx".as_bytes()].concat(),
        ),
        (
            "utf8 cyrillic emoji",
            "привет 😀\nмир\n".as_bytes().to_vec(),
        ),
        ("windows-1251 crlf", win1251),
        ("utf16 le bom crlf", utf16le),
        ("utf16 be bom lf", utf16be),
        (
            "invalid utf8",
            vec![b'a', 0xFF, 0xFE, b'\n', 0xC3, 0x28, b'\n'],
        ),
        ("nul bytes", b"a\0b\r\n".to_vec()),
    ]
}

#[test]
fn byte_round_trip_or_flagged_lossy() {
    let mut bad = Vec::new();
    for (name, bytes) in samples() {
        let decoded = decode(&bytes);
        let again = encode(&decoded.text, &decoded.format);
        let same = again == bytes;
        eprintln!("{name:22} lossless={} round-trip={same}", decoded.lossless);
        if decoded.lossless && !same {
            bad.push(format!("{name}: flagged lossless but the bytes differ"));
        }
        if !decoded.lossless && same {
            // fine: flagged but identical
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}
