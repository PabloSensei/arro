//! A URI as a language server writes it becomes the spelling Arro uses.

#[cfg(windows)]
use std::path::PathBuf;
use std::str::FromStr;

use lsp_client::lsp_types::Uri;
#[cfg(windows)]
use lsp_client::uri::file_path;
use lsp_client::uri::{server_uri, server_uri_key};

#[cfg(windows)]
#[test]
fn a_server_spelling_of_a_path_has_the_arro_uri_and_one_key() {
    let server = "file:/C:/Users/\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)/proj+spec@x/A B.txt";
    let arro =
        "file:///C:/Users/%D0%9F%D0%B0%D0%B2%D0%B5%D0%BB%20%281%29/proj%2Bspec%40x/A%20B.txt";

    let uri = server_uri(server).expect("the raw non-ASCII URI is read");
    assert_eq!(
        uri.as_str(),
        arro,
        "the stored spelling is the one Arro sends"
    );
    assert_eq!(
        file_path(&uri),
        Some(PathBuf::from(
            "C:\\Users\\\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)\\proj+spec@x\\A B.txt"
        ))
    );
    assert_eq!(server_uri_key(server), server_uri_key(arro));
    assert_eq!(server_uri_key(arro), server_uri_key(uri.as_str()));
}

#[test]
fn a_uri_of_another_scheme_keeps_its_text() {
    let uri = "custom://contents/A.class";
    assert_eq!(server_uri_key(uri), uri);
    assert_eq!(
        server_uri(uri).map(|uri| uri.as_str().to_owned()),
        Some(uri.to_owned())
    );
}

#[test]
fn text_that_is_not_a_uri_has_no_server_uri() {
    assert_eq!(server_uri("not a uri"), None);
    assert_eq!(server_uri_key("not a uri"), "not a uri");
    assert!(Uri::from_str("not a uri").is_err());
}
