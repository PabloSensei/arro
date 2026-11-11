//! Bytes on disk survive opening, editing somewhere else and saving.

use encoding_rs::WINDOWS_1251;
use proptest::prelude::*;
use text_core::{
    Buffer, Edit, EditKind, EditSource, Eol, EolStyle, FileUpdate, RopeStore, TextEncoding, decode,
    encode, read_file, write_file, write_files,
};

fn assert_round_trip(bytes: &[u8]) {
    let decoded = decode(bytes);
    assert!(decoded.lossless, "not lossless: {:?}", decoded.format);
    assert!(!decoded.text.contains('\r'));
    assert_eq!(encode(&decoded.text, &decoded.format), bytes);
}

#[test]
fn lf_stays_lf() {
    assert_round_trip(b"class A {\n}\n");
    assert_eq!(decode(b"a\nb\n").format.eol, EolStyle::Uniform(Eol::Lf));
}

#[test]
fn crlf_stays_crlf() {
    assert_round_trip(b"class A {\r\n}\r\n");
    assert_eq!(
        decode(b"a\r\nb\r\n").format.eol,
        EolStyle::Uniform(Eol::CrLf)
    );
}

#[test]
fn lone_cr_stays_cr() {
    assert_round_trip(b"a\rb\r");
    assert_round_trip(b"a\r\r\nb");
}

#[test]
fn mixed_line_ends_are_not_normalized() {
    let bytes = b"one\r\ntwo\nthree\rfour\r\nfive";
    assert_round_trip(bytes);
    assert!(matches!(
        decode(bytes).format.eol,
        EolStyle::Mixed {
            default: Eol::CrLf,
            ..
        }
    ));
}

#[test]
fn no_line_end_at_the_end_of_the_file() {
    assert_round_trip(b"class A {}");
    assert_round_trip(b"");
}

#[test]
fn utf8_bom_is_kept() {
    let bytes = "\u{feff}class Ж {}\r\n".as_bytes();
    assert_round_trip(bytes);
    let decoded = decode(bytes);
    assert!(decoded.format.bom);
    assert_eq!(decoded.text, "class Ж {}\n");
}

#[test]
fn utf16_with_bom_is_kept() {
    let mut le = vec![0xFF, 0xFE];
    le.extend("a\r\nЖ".encode_utf16().flat_map(u16::to_le_bytes));
    assert_round_trip(&le);
    assert_eq!(decode(&le).format.encoding, TextEncoding::Utf16Le);

    let mut be = vec![0xFE, 0xFF];
    be.extend("a\nЖ".encode_utf16().flat_map(u16::to_be_bytes));
    assert_round_trip(&be);
    assert_eq!(decode(&be).format.encoding, TextEncoding::Utf16Be);
}

#[test]
fn windows_1251_is_detected_and_kept() {
    let text = "// Этот класс хранит заказы покупателей и считает их итоговую стоимость.\r\n\
                public class Заказ {\r\n    // Сумма заказа в рублях\r\n}\r\n";
    let (bytes, _, unmappable) = WINDOWS_1251.encode(text);
    assert!(!unmappable);
    assert_round_trip(&bytes);
    let decoded = decode(&bytes);
    assert_eq!(decoded.format.encoding, TextEncoding::Legacy(WINDOWS_1251));
    assert!(decoded.text.contains("Сумма заказа"));
}

#[test]
fn bytes_that_cannot_come_back_open_read_only() {
    let mut bytes = "\u{feff}a".as_bytes().to_vec();
    bytes.push(0xFF);
    assert!(!decode(&bytes).lossless);
    assert!(!decode(b"a\0b").lossless);
}

#[test]
fn an_edit_in_a_mixed_file_leaves_other_lines_alone() {
    let decoded = decode(b"one\r\ntwo\nthree\r\nfour\n");
    let mut buffer: Buffer<RopeStore> = Buffer::new(&decoded.text, decoded.format);
    buffer.apply(
        vec![Edit::insert(4, "2\n")],
        None,
        EditSource::User,
        EditKind::Command,
    );
    assert_eq!(
        encode(&buffer.text(), buffer.format()),
        b"one\r\n2\r\ntwo\nthree\r\nfour\n"
    );

    buffer.undo();
    assert_eq!(
        encode(&buffer.text(), buffer.format()),
        b"one\r\ntwo\nthree\r\nfour\n"
    );
    buffer.redo();
    assert_eq!(
        encode(&buffer.text(), buffer.format()),
        b"one\r\n2\r\ntwo\nthree\r\nfour\n"
    );
}

#[test]
fn deleting_a_line_end_and_undo_restore_a_mixed_file() {
    let bytes = b"a\nb\r\nc\rd";
    let decoded = decode(bytes);
    let mut buffer: Buffer<RopeStore> = Buffer::new(&decoded.text, decoded.format);
    buffer.apply(
        vec![Edit::delete(1..5)],
        None,
        EditSource::User,
        EditKind::Command,
    );
    buffer.undo();
    assert_eq!(encode(&buffer.text(), buffer.format()), bytes);
}

#[test]
fn saved_file_has_the_same_bytes() {
    let directory = std::env::temp_dir().join(format!("text-core-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Sample.txt");
    let bytes = b"one\r\ntwo\nthree";
    std::fs::write(&path, bytes).unwrap();

    let decoded = read_file(&path).unwrap();
    write_file(&path, &decoded.text, &decoded.format).unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let leftovers = std::fs::read_dir(&directory).unwrap().count();
    assert_eq!(leftovers, 1);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn a_batch_of_files_is_written_all_or_none() {
    let directory = std::env::temp_dir().join(format!("text-core-batch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let first = directory.join("a.txt");
    let second = directory.join("b.txt");
    std::fs::write(&first, b"one\r\none").unwrap();
    std::fs::write(&second, b"two\n").unwrap();
    let first_format = read_file(&first).unwrap().format;
    let second_format = read_file(&second).unwrap().format;

    let updates = [
        FileUpdate {
            path: &first,
            text: "1\n1",
            format: &first_format,
        },
        FileUpdate {
            path: &second,
            text: "2\n",
            format: &second_format,
        },
    ];
    write_files(&updates).unwrap();
    assert_eq!(std::fs::read(&first).unwrap(), b"1\r\n1");
    assert_eq!(std::fs::read(&second).unwrap(), b"2\n");

    let mut permissions = std::fs::metadata(&second).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&second, permissions).unwrap();
    let updates = [
        FileUpdate {
            path: &first,
            text: "x",
            format: &first_format,
        },
        FileUpdate {
            path: &second,
            text: "y",
            format: &second_format,
        },
    ];
    let failed = write_files(&updates).unwrap_err();
    assert!(failed.to_string().contains("b.txt"), "{failed}");
    assert_eq!(std::fs::read(&first).unwrap(), b"1\r\n1");
    assert_eq!(std::fs::read(&second).unwrap(), b"2\n");
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);

    let mut permissions = std::fs::metadata(&second).unwrap().permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&second, permissions).unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
}

proptest! {
    #[test]
    fn any_utf8_text_with_any_line_ends_round_trips(
        pieces in prop::collection::vec(("[a-zа-я ]{0,8}", prop::sample::select(vec!["\n", "\r\n", "\r", ""])), 0..24)
    ) {
        let bytes: String = pieces.iter().flat_map(|(text, eol)| [text.as_str(), *eol]).collect();
        assert_round_trip(bytes.as_bytes());
    }

    #[test]
    fn any_bytes_either_round_trip_or_open_read_only(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let decoded = decode(&bytes);
        if decoded.lossless {
            prop_assert_eq!(encode(&decoded.text, &decoded.format), bytes);
        }
    }
}

#[test]
fn a_format_picked_by_the_user_is_written_and_marks_the_buffer_edited() {
    let decoded = decode(b"a\nb\n");
    let mut buffer: Buffer<RopeStore> = Buffer::new(&decoded.text, decoded.format.clone());
    assert!(!buffer.is_dirty());

    buffer.set_format(buffer.format().with_eol(Eol::CrLf));
    assert!(buffer.is_dirty());
    assert_eq!(encode(&buffer.text(), buffer.format()), b"a\r\nb\r\n");

    let format = buffer.format().with_encoding_name("UTF-8 BOM").unwrap();
    assert_eq!(encode("é", &format), b"\xEF\xBB\xBF\xC3\xA9");
    let legacy = format.with_encoding_name("windows-1251").unwrap();
    assert_eq!(legacy.encoding, TextEncoding::Legacy(WINDOWS_1251));
    assert!(format.with_encoding_name("no-such-encoding").is_none());
}
