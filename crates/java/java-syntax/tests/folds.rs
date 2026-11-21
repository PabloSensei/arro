//! The regions a source file offers to the folding keys, read through the same thread
//! the editor uses.

use std::sync::mpsc;
use std::time::Duration;

use syntax::{DocumentId, SyntaxService, SyntaxSnapshot};
use text_core::{Change, RopeStore, TextStore};

/// Members with a short signature, an annotation above it, a signature that wraps and a
/// long one that stays on a line, one body holding a block.
const SAMPLE: &str = r#"package com.example;

class OwnerController {
    @InitBinder
    void setAllowedFields(Object binder) {
        binder.hashCode();
    }

    String processCreationForm(Object owner,
            Object result) {
        if (result != null) {
            return "form";
        }
        return "redirect";
    }

    String findPaginatedForOwnersLastName(int page, String lastname) {
        return lastname;
    }
}
"#;

fn parse(text: &str) -> (SyntaxService, mpsc::Receiver<SyntaxSnapshot>) {
    let service = SyntaxService::new();
    let (sender, receiver) = mpsc::channel();
    service.open(DocumentId(1), java_syntax::config(), move |snapshot| {
        let _ = sender.send(snapshot);
    });
    let store = RopeStore::from_text(text);
    service.update(DocumentId(1), 1, store, &Change::default());
    (service, receiver)
}

#[test]
fn every_body_of_a_member_is_a_region() {
    let (_service, receiver) = parse(SAMPLE);
    let snapshot = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the query compiles and the parse ends");
    assert!(!snapshot.tree.root_node().has_error());

    // Every `{` of the sample opens a body of a class, a method or a block, and the
    // wrapped signature and the annotation above a method take none away.
    for (at, _) in SAMPLE.match_indices('{') {
        assert!(
            snapshot
                .folds
                .iter()
                .any(|fold| fold.range.start == at && fold.placeholder == "{...}"),
            "a body starts at {at}"
        );
    }
}
