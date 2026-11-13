//! Views of one document: the text, the undo history and the table of line ends are
//! shared, while the carets belong to the view.

use text_core::{
    Buffer, Edit, EditKind, EditSource, Eol, FileFormat, Followed, RopeStore, decode, encode,
};

fn insert(buffer: &mut Buffer<RopeStore>, at: usize, text: &str) {
    buffer.apply(
        vec![Edit::insert(at, text)],
        None,
        EditSource::User,
        EditKind::Command,
    );
}

#[test]
fn views_share_the_table_of_mixed_line_ends_through_edits_and_undo() {
    let decoded = decode(b"a\r\nb\nc\r\n");
    let mut first: Buffer<RopeStore> = Buffer::new(&decoded.text, decoded.format);
    let mut second = first.fork();
    assert!(first.shares_document(&second));

    insert(&mut first, 2, "x\n");
    assert!(matches!(second.follow(), Followed::Changes(_)));
    assert_eq!(
        encode(&second.text(), &second.format()),
        b"a\r\nx\r\nb\nc\r\n",
        "the new line takes the most common line end of the file"
    );

    // Undo in the other view restores the line ends the edit replaced.
    second.undo();
    assert!(matches!(first.follow(), Followed::Changes(_)));
    assert_eq!(encode(&first.text(), &first.format()), b"a\r\nb\nc\r\n");

    second.redo();
    assert!(matches!(first.follow(), Followed::Changes(_)));
    assert_eq!(
        encode(&first.text(), &first.format()),
        b"a\r\nx\r\nb\nc\r\n"
    );

    // Undo and redo in the first view as well.
    first.undo();
    assert!(matches!(second.follow(), Followed::Changes(_)));
    assert_eq!(encode(&second.text(), &second.format()), b"a\r\nb\nc\r\n");
    first.redo();
    assert!(matches!(second.follow(), Followed::Changes(_)));
    assert_eq!(
        encode(&second.text(), &second.format()),
        b"a\r\nx\r\nb\nc\r\n"
    );
}

#[test]
fn a_format_picked_in_one_view_is_the_format_of_the_document() {
    let decoded = decode(b"a\r\nb\nc\r\n");
    let mut first: Buffer<RopeStore> = Buffer::new(&decoded.text, decoded.format);
    let second = first.fork();

    first.set_format(first.format().with_eol(Eol::Lf));
    assert_eq!(
        second.format(),
        first.format(),
        "the line ends of one view are the line ends of the other"
    );
    assert_eq!(encode(&second.text(), &second.format()), b"a\nb\nc\n");
}

#[test]
fn a_view_that_missed_the_changes_is_told_so_and_keeps_the_shared_text() {
    let mut first: Buffer<RopeStore> = Buffer::new("x\n", FileFormat::default());
    let mut second = first.fork();
    let mut text = String::from("x\n");
    for _ in 0..70 {
        insert(&mut first, 0, "y");
        text.insert(0, 'y');
    }

    assert!(matches!(second.follow(), Followed::Missed));
    assert_eq!(second.text(), text);
    assert_eq!(
        second.format(),
        first.format(),
        "the format is not replayed with the changes"
    );

    // An edit of a view that missed the changes still goes into the current text.
    insert(&mut second, 0, "z");
    assert_eq!(first.text(), format!("z{text}"));
}
