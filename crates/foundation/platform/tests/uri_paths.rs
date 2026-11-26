//! The spellings of a `file:` URI that arrive from a language server.

#[cfg(windows)]
use platform::{path_from_uri, path_key, path_to_uri};
#[cfg(windows)]
use std::path::{Path, PathBuf};

/// A path with Cyrillic, parentheses, `+`, `@` and a space, as both URI classes write
/// it: `File.toURI` keeps the characters and uses one slash, Arro percent-encodes them
/// and uses three.
#[cfg(windows)]
#[test]
fn a_path_from_a_server_uri_and_from_an_arro_uri_is_one_file() {
    let server = "file:/C:/Users/\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)/proj+spec@x/A B.txt";
    let arro =
        "file:///C:/Users/%D0%9F%D0%B0%D0%B2%D0%B5%D0%BB%20%281%29/proj%2Bspec%40x/A%20B.txt";

    let expected = PathBuf::from(
        "C:\\Users\\\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)\\proj+spec@x\\A B.txt",
    );
    assert_eq!(path_from_uri(server), Some(expected.clone()), "File.toURI");
    assert_eq!(path_from_uri(arro), Some(expected.clone()), "Arro");
    let from_server = path_key(&path_from_uri(server).unwrap());
    let from_arro = path_key(&path_from_uri(arro).unwrap());
    assert_eq!(from_server, from_arro);
    assert_eq!(from_server, path_key(Path::new(&expected)));
}

#[cfg(windows)]
#[test]
fn a_path_comes_back_as_the_arro_spelling() {
    let path = PathBuf::from(
        "C:\\Users\\\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)\\proj+spec@x\\A B.txt",
    );
    let uri = path_to_uri(&path);
    assert_eq!(
        uri,
        "file:///C:/Users/%D0%9F%D0%B0%D0%B2%D0%B5%D0%BB%20%281%29/proj%2Bspec%40x/A%20B.txt"
    );
    assert_eq!(path_from_uri(&uri), Some(path));
}

#[cfg(windows)]
#[test]
fn one_slash_and_three_slashes_name_the_same_drive_path() {
    assert_eq!(
        path_from_uri("file:/C:/dir/a.txt"),
        path_from_uri("file:///C:/dir/a.txt")
    );
}
