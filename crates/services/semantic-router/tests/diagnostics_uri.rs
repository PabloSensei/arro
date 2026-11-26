//! Diagnostics of a file whose URI the server spells differently from Arro.

use std::str::FromStr;

use lsp_client::ServerEvents;
use lsp_client::lsp_types::Uri;
use semantic_router::Published;
use serde_json::json;

fn publication(uri: &str) -> serde_json::Value {
    json!({
        "uri": uri,
        "diagnostics": [{
            "range": {
                "start": {"line": 1, "character": 2},
                "end": {"line": 1, "character": 5},
            },
            "severity": 1,
            "message": "cannot find symbol",
        }],
    })
}

#[cfg(windows)]
#[test]
fn a_server_uri_and_an_arro_uri_find_the_same_diagnostics() {
    let server = "file:/C:/Users/\u{041f}\u{0430}\u{0432}\u{0435}\u{043b} (1)/proj+spec@x/A B.txt";
    let arro = Uri::from_str(
        "file:///C:/Users/%D0%9F%D0%B0%D0%B2%D0%B5%D0%BB%20%281%29/proj%2Bspec%40x/A%20B.txt",
    )
    .unwrap();
    let published = Published::new().0;

    published.notification("textDocument/publishDiagnostics", publication(server));

    let found = published.diagnostics(&arro);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message, "cannot find symbol");

    let all = published.all_diagnostics();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].0.as_str(),
        arro.as_str(),
        "the stored URI is the spelling Arro sends"
    );
    assert_eq!(all[0].1.len(), 1);
}

#[test]
fn an_empty_list_clears_the_diagnostics_of_the_document() {
    let uri = "file:///C:/project/A.txt";
    let published = Published::new().0;
    published.notification("textDocument/publishDiagnostics", publication(uri));
    assert_eq!(published.diagnostics(&Uri::from_str(uri).unwrap()).len(), 1);

    published.notification(
        "textDocument/publishDiagnostics",
        json!({"uri": uri, "diagnostics": []}),
    );
    assert!(
        published
            .diagnostics(&Uri::from_str(uri).unwrap())
            .is_empty()
    );
}
