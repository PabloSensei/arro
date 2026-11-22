//! Editor features that a language answers about the structure around a place: the
//! declarations of a file with their signatures, the methods a type can override, the
//! call hierarchy, the source of a definition and the reference to a symbol.

use crate::{DocumentSpot, HierarchyDirection, Provider, SymbolKind, TextSpan, WorkspaceChange};

/// One declaration of a file as the language server describes it. `detail` is the part
/// after the name that the server gives, such as the return type of a method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    pub range: TextSpan,
    pub name_range: TextSpan,
    pub children: Vec<FileSymbol>,
}

/// The declarations of one document, by URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSymbolsQuery(pub String);

/// One method that a type can override or implement. `handle` is the provider's own
/// value for the method and is given back unchanged when it is added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverridableMethod {
    pub name: String,
    /// The class or interface that declares the method.
    pub owner: String,
    pub parameters: Vec<String>,
    /// The type the method returns, when the provider names it.
    pub return_type: Option<String>,
    /// The method has no implementation in the owner yet.
    pub abstract_method: bool,
    pub handle: String,
}

/// The methods of one type that the editor can offer, with the type they belong to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverridableMethods {
    pub owner_type: String,
    pub methods: Vec<OverridableMethod>,
}

/// A type as the editor shows it: without the parts of the package, with the arguments
/// of a generic type shortened the same way, and with wildcards and arrays kept. A text
/// that is not one type comes back trimmed and otherwise unchanged.
pub fn short_type_name(text: &str) -> String {
    let mut rest = text.trim();
    match short_type(&mut rest) {
        Some(name) if rest.trim().is_empty() => name,
        _ => text.trim().to_owned(),
    }
}

/// One type from the front of `rest`: a name, its arguments and its array dimensions.
fn short_type(rest: &mut &str) -> Option<String> {
    *rest = rest.trim_start();
    let mut name = if let Some(after) = rest.strip_prefix('?') {
        *rest = after.trim_start();
        let word_end = rest
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        match &rest[..word_end] {
            "extends" => {
                *rest = rest[word_end..].trim_start();
                format!("? extends {}", short_type(rest)?)
            }
            "super" => {
                *rest = rest[word_end..].trim_start();
                format!("? super {}", short_type(rest)?)
            }
            _ => "?".to_owned(),
        }
    } else {
        let end = rest
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '$' | '.'))
            })
            .unwrap_or(rest.len());
        let qualified = &rest[..end];
        let simple = qualified.rsplit(['.', '$']).next().unwrap_or(qualified);
        if simple.is_empty() {
            return None;
        }
        *rest = rest[end..].trim_start();
        let mut name = simple.to_owned();
        if let Some(after) = rest.strip_prefix('<') {
            *rest = after;
            let mut arguments = Vec::new();
            loop {
                *rest = rest.trim_start();
                if let Some(after) = rest.strip_prefix('>') {
                    *rest = after;
                    break;
                }
                if !arguments.is_empty() {
                    *rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
                }
                arguments.push(short_type(rest)?);
            }
            name.push('<');
            name.push_str(&arguments.join(", "));
            name.push('>');
        }
        name
    };
    loop {
        if let Some(after) = rest.strip_prefix("[]") {
            *rest = after;
            name.push_str("[]");
        } else if let Some(after) = rest.strip_prefix("...") {
            *rest = after;
            name.push_str("...");
        } else {
            break;
        }
    }
    Some(name)
}

/// Ctrl+O and Ctrl+I: the methods at `spot` can override. Implement keeps only the
/// methods without a body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverridableMethodsQuery {
    pub spot: DocumentSpot,
    pub only_abstract: bool,
}

/// Ctrl+O and Ctrl+I after the user picks methods: the edits that add them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddOverridableMethodsRequest {
    pub spot: DocumentSpot,
    pub methods: Vec<OverridableMethod>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallDirection {
    /// Ctrl+Alt+H: the calls that reach this place.
    Incoming,
    Outgoing,
}

/// One node of the call hierarchy. `handle` is the provider's value for the node and is
/// given back when its children are asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallHierarchyItem {
    pub name: String,
    pub detail: Option<String>,
    pub place: Option<crate::SourceSpan>,
    pub handle: String,
}

/// Ctrl+Alt+H: the roots of the hierarchy at a place, or the calls of one node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallHierarchyQuery {
    Roots(DocumentSpot),
    Calls {
        item: CallHierarchyItem,
        direction: CallDirection,
    },
}

/// How a type of the type hierarchy is declared, for its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TypeKind {
    #[default]
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

/// One type of the type hierarchy. `handle` is the provider's own value for the type and
/// is given back unchanged when its supertypes or subtypes are asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeHierarchyItem {
    pub name: String,
    /// The package or the type that holds this one, shown beside the name.
    pub detail: Option<String>,
    pub kind: TypeKind,
    /// The type is declared abstract.
    pub abstract_type: bool,
    pub place: Option<crate::SourceSpan>,
    pub handle: String,
}

/// Ctrl+H: the type at a place, or the types next to one node of the hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeHierarchyQuery {
    Root(DocumentSpot),
    Neighbours {
        item: TypeHierarchyItem,
        direction: HierarchyDirection,
    },
}

/// Ctrl+Shift+I: the source of the definition at `spot`, from the start of the
/// declaration to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionPreviewQuery(pub DocumentSpot);

/// The text of a definition and the place it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefinitionPreview {
    pub text: String,
    pub target: Option<crate::SourceSpan>,
}

/// Ctrl+Alt+Shift+C: the reference to the symbol at `spot`, empty when there is none
/// under the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceQuery(pub DocumentSpot);

/// Where a type of the hierarchy stands in relation to the method under the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodRelation {
    /// The type declares the method.
    Defines,
    /// The type takes the method from a parent.
    Inherits,
    /// The type is abstract and does not have the method yet.
    MustDefine,
}

/// One type of the hierarchy of a method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodHierarchyEntry {
    pub type_name: String,
    pub method: String,
    pub relation: MethodRelation,
    pub place: Option<crate::SourceSpan>,
}

/// Ctrl+Shift+H: the types of the method under `spot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodHierarchyQuery(pub DocumentSpot);

pub type MethodHierarchyProvider = dyn Provider<MethodHierarchyQuery, Vec<MethodHierarchyEntry>>;
pub type FileSymbolsProvider = dyn Provider<FileSymbolsQuery, Vec<FileSymbol>>;
pub type OverridableMethodsProvider = dyn Provider<OverridableMethodsQuery, OverridableMethods>;
pub type AddOverridableMethodsProvider =
    dyn Provider<AddOverridableMethodsRequest, WorkspaceChange>;
pub type CallHierarchyProvider = dyn Provider<CallHierarchyQuery, Vec<CallHierarchyItem>>;
pub type TypeHierarchyProvider = dyn Provider<TypeHierarchyQuery, Vec<TypeHierarchyItem>>;
pub type DefinitionPreviewProvider = dyn Provider<DefinitionPreviewQuery, DefinitionPreview>;
pub type ReferenceStringProvider = dyn Provider<ReferenceQuery, String>;
