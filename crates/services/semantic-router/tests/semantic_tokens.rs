//! The semantic tokens of a server: the legend turns its numbers into meanings, the
//! position of a token follows the one before it, and an answer about an old text is
//! dropped.

use std::str::FromStr;

use lang_api::{SemanticTokenKind, SemanticTokenModifiers};
use lsp_client::SemanticTokensCapability;
use lsp_client::lsp_types::{Position, SemanticToken, Uri};
use semantic_router::{Place, RouteError, Router, semantic_tokens_from_lsp};
use sidecar::{State, Status};
use tokio::sync::watch;

fn status() -> Status {
    Status {
        state: State::Ready,
        generation: 1,
        alive: false,
        client: None,
        process_id: None,
        crashes: 0,
        last_error: None,
    }
}

fn uri() -> Uri {
    Uri::from_str("file:///C:/project/A.txt").unwrap()
}

fn place(version: i32) -> Place {
    Place {
        uri: uri(),
        version,
        position: Position::new(0, 0),
    }
}

fn capability() -> SemanticTokensCapability {
    SemanticTokensCapability {
        token_types: vec![
            "class".to_owned(),
            "property".to_owned(),
            "parameter".to_owned(),
        ],
        token_modifiers: vec![
            "static".to_owned(),
            "readonly".to_owned(),
            "deprecated".to_owned(),
        ],
        range: true,
        full: true,
    }
}

#[test]
fn the_legend_and_the_deltas_give_each_token_its_place_and_meaning() {
    let data = [
        // class, at line 0, column 0, five characters long.
        SemanticToken {
            delta_line: 0,
            delta_start: 0,
            length: 5,
            token_type: 0,
            token_modifiers_bitset: 0,
        },
        // property, on the next line at column 4, static and readonly: a constant.
        SemanticToken {
            delta_line: 1,
            delta_start: 4,
            length: 4,
            token_type: 1,
            token_modifiers_bitset: 0b011,
        },
        // parameter, on the same line, ten columns further right.
        SemanticToken {
            delta_line: 0,
            delta_start: 10,
            length: 1,
            token_type: 2,
            token_modifiers_bitset: 0,
        },
    ];
    let tokens = semantic_tokens_from_lsp(&data, &capability());
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].kind, SemanticTokenKind::Type);
    assert_eq!(
        (tokens[0].span.start.line, tokens[0].span.start.character),
        (0, 0)
    );
    assert_eq!(
        (tokens[0].span.end.line, tokens[0].span.end.character),
        (0, 5)
    );

    assert_eq!(tokens[1].kind, SemanticTokenKind::Property);
    assert_eq!(
        (tokens[1].span.start.line, tokens[1].span.start.character),
        (1, 4)
    );
    assert!(tokens[1].modifiers.static_member);
    assert!(tokens[1].modifiers.readonly);
    assert!(tokens[1].modifiers.is_constant());
    assert_eq!(tokens[1].modifiers, {
        SemanticTokenModifiers {
            static_member: true,
            readonly: true,
            ..SemanticTokenModifiers::default()
        }
    });

    assert_eq!(tokens[2].kind, SemanticTokenKind::Parameter);
    assert_eq!(
        (tokens[2].span.start.line, tokens[2].span.start.character),
        (1, 14)
    );
    assert_eq!(tokens[2].span.end.character, 15);
}

#[test]
fn a_type_the_editor_does_not_colour_is_left_out() {
    let capability = SemanticTokensCapability {
        token_types: vec!["unknownToTheEditor".to_owned()],
        token_modifiers: Vec::new(),
        range: false,
        full: true,
    };
    let data = [SemanticToken {
        delta_line: 0,
        delta_start: 0,
        length: 3,
        token_type: 0,
        token_modifiers_bitset: 0,
    }];
    assert!(semantic_tokens_from_lsp(&data, &capability).is_empty());
}

#[tokio::test]
async fn a_question_about_an_old_text_gets_no_answer() {
    let (_sender, receiver) = watch::channel(status());
    let router = Router::new(receiver);
    router.open(uri(), "text", 1, "class A {}".into());
    router.change(&uri(), 2, "class B {}".into(), Vec::new());

    let error = router
        .semantic_tokens(place(1), None)
        .await
        .expect_err("the document changed");
    assert!(matches!(error, RouteError::Stale));
}

#[tokio::test]
async fn a_question_about_a_closed_document_gets_no_answer() {
    let (_sender, receiver) = watch::channel(status());
    let router = Router::new(receiver);
    let error = router
        .semantic_tokens(place(1), None)
        .await
        .expect_err("nothing is open");
    assert!(matches!(error, RouteError::Stale));
}
