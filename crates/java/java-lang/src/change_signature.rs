//! Change Signature: the visibility levels that the declaration at the caret may take. The
//! server describes the signature as before; the tree of the open file says which levels
//! the place of the declaration allows, so the form does not offer a method of an
//! interface as protected.

use std::str::FromStr;
use std::sync::Arc;

use base::Cancel;
use futures_util::StreamExt;
use lang_api::{ChangeSignatureInfo, DocumentSpot, GraphVisibility, Provider, Reply};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use semantic_router::{LspProviders, Router};
use tokio::runtime::Handle;
use tree_sitter::{Node, Parser, Tree};

/// Asks the server for the signature under the caret and adds the levels that the place of
/// the declaration allows.
pub(crate) struct ChangeSignatureSupport {
    router: Arc<Router>,
    providers: Arc<LspProviders>,
    runtime: Handle,
}

impl ChangeSignatureSupport {
    pub(crate) fn new(router: Arc<Router>, providers: Arc<LspProviders>, runtime: Handle) -> Self {
        Self {
            router,
            providers,
            runtime,
        }
    }
}

impl Provider<DocumentSpot, ChangeSignatureInfo> for ChangeSignatureSupport {
    fn start(&self, spot: DocumentSpot, cancel: Cancel, mut reply: Reply<ChangeSignatureInfo>) {
        let router = self.router.clone();
        let providers = self.providers.clone();
        self.runtime.spawn(async move {
            // The server answers as it does without this support; an answer with a
            // signature also gets the levels that its place allows.
            let (inner_reply, mut replies) = lang_api::channel::<ChangeSignatureInfo>();
            (providers.as_ref() as &dyn Provider<DocumentSpot, ChangeSignatureInfo>).start(
                spot.clone(),
                cancel.clone(),
                inner_reply,
            );
            while let Some(mut answer) = replies.next().await {
                if cancel.is_cancelled() || reply.is_cancelled() {
                    return;
                }
                if answer.value.signature.is_some() {
                    answer.value.allowed_visibility = allowed_at(&router, &spot);
                }
                let _ = reply.send(answer);
            }
        });
    }
}

/// The levels of the declaration at a place, read from the text of its file: the open
/// editor's text, or the file on disk.
fn allowed_at(router: &Router, spot: &DocumentSpot) -> Vec<GraphVisibility> {
    let Ok(uri) = Uri::from_str(&spot.uri) else {
        return Vec::new();
    };
    let Some(path) = file_path(&uri) else {
        return Vec::new();
    };
    let text = router
        .text(&uri)
        .map(|text| text.to_string())
        .or_else(|| std::fs::read_to_string(&path).ok());
    let Some(text) = text else {
        return Vec::new();
    };
    allowed_visibility(&text, lang_api::offset_in(&text, spot.point))
}

/// The levels that the declaration around `offset` may take, or none when the place allows
/// every level. Only the header of a method or a constructor counts: a caret in its body
/// is at a use of some method, and that method may be declared somewhere else.
fn allowed_visibility(text: &str, offset: usize) -> Vec<GraphVisibility> {
    let Some(tree) = parse(text) else {
        return Vec::new();
    };
    let offset = offset.min(text.len());
    let Some(mut node) = tree.root_node().descendant_for_byte_range(offset, offset) else {
        return Vec::new();
    };
    while !matches!(
        node.kind(),
        "method_declaration" | "constructor_declaration"
    ) {
        let Some(parent) = node.parent() else {
            return Vec::new();
        };
        node = parent;
    }
    let body_start = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    if offset > body_start {
        return Vec::new();
    }
    let Some(place) = place_of(node) else {
        return Vec::new();
    };
    match (node.kind(), place.kind()) {
        ("method_declaration", "interface_body") if is_private(node, text) => {
            vec![GraphVisibility::Public, GraphVisibility::Private]
        }
        ("method_declaration", "interface_body") => vec![GraphVisibility::Public],
        ("constructor_declaration", "enum_body") => vec![GraphVisibility::Private],
        _ => Vec::new(),
    }
}

fn parse(text: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&java_syntax::config().grammar).ok()?;
    parser.parse(text, None)
}

/// The body that holds a declaration. The members of an enum sit in a group of their own.
fn place_of(declaration: Node<'_>) -> Option<Node<'_>> {
    let parent = declaration.parent()?;
    if parent.kind() == "enum_body_declarations" {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// Whether the modifiers of a declaration spell the keyword `private`.
fn is_private(declaration: Node<'_>, text: &str) -> bool {
    let mut cursor = declaration.walk();
    let Some(modifiers) = declaration
        .children(&mut cursor)
        .find(|child| child.kind() == "modifiers")
    else {
        return false;
    };
    let mut cursor = modifiers.walk();
    modifiers
        .children(&mut cursor)
        .any(|keyword| text.get(keyword.byte_range()) == Some("private"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"interface Shapes {
    double area();
    default String label() { return "shape"; }
    static Shapes unit() { return null; }
    private int scale() { return 2; }
}
enum Mode {
    FAST;
    Mode() { }
    void run() { }
}
class Plain {
    Plain() { }
    void go() { }
}
"#;

    /// The offset `inside` bytes into the first `marker` of the source.
    fn at(marker: &str, inside: usize) -> usize {
        SOURCE.find(marker).expect("the marker is in the source") + inside
    }

    #[test]
    fn allowed_visibility_of_an_interface_method_is_public_only() {
        assert_eq!(
            allowed_visibility(SOURCE, at("area()", 1)),
            [GraphVisibility::Public]
        );
        assert_eq!(
            allowed_visibility(SOURCE, at("label()", 1)),
            [GraphVisibility::Public],
            "a default method"
        );
        assert_eq!(
            allowed_visibility(SOURCE, at("unit()", 1)),
            [GraphVisibility::Public],
            "a static method"
        );
    }

    #[test]
    fn allowed_visibility_of_a_private_interface_method_is_public_or_private() {
        assert_eq!(
            allowed_visibility(SOURCE, at("scale()", 1)),
            [GraphVisibility::Public, GraphVisibility::Private]
        );
    }

    #[test]
    fn allowed_visibility_of_an_enum_constructor_is_private_only() {
        assert_eq!(
            allowed_visibility(SOURCE, at("Mode() { }", 1)),
            [GraphVisibility::Private]
        );
    }

    #[test]
    fn allowed_visibility_of_a_class_member_is_every_level() {
        assert!(allowed_visibility(SOURCE, at("Plain() { }", 1)).is_empty());
        assert!(allowed_visibility(SOURCE, at("go()", 1)).is_empty());
        assert!(allowed_visibility(SOURCE, at("run()", 1)).is_empty());
    }

    #[test]
    fn allowed_visibility_of_a_caret_in_a_body_is_every_level() {
        assert!(allowed_visibility(SOURCE, at("\"shape\"", 2)).is_empty());
    }
}
