//! The type graph a diagram shows: the types around a place or a folder with the
//! relationships between them, as the language reads them from the sources.

use std::path::PathBuf;

use crate::{DocumentSpot, Provider, SourceSpan};

/// What a type of the diagram is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphTypeKind {
    Class,
    Interface,
    Enum,
    Record,
    Annotation,
}

/// How visible a member is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphVisibility {
    Public,
    Protected,
    Package,
    Private,
}

/// What a member of a type is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphMemberKind {
    Field,
    Method,
}

/// One field or method of a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphMember {
    pub name: String,
    /// The type of a field or the return type of a method, without type arguments.
    pub type_name: String,
    /// The parameter types of a method, as the source spells them.
    pub parameters: Vec<String>,
    pub kind: GraphMemberKind,
    pub visibility: GraphVisibility,
    pub static_member: bool,
    /// A method without a body.
    pub abstract_member: bool,
    /// The name of the member in its source.
    pub place: Option<SourceSpan>,
}

/// One type of the diagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphType {
    pub name: String,
    /// The name with its package and outer types, `demo.Owner.Inner`.
    pub qualified: String,
    pub kind: GraphTypeKind,
    pub abstract_type: bool,
    /// The type is only known by its name: a library type the project does not declare.
    pub external: bool,
    pub members: Vec<GraphMember>,
    /// The name of the type in its source, for a type with a source.
    pub place: Option<SourceSpan>,
}

/// The relationship an edge draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GraphEdgeKind {
    /// A class extends a class, or an interface extends an interface.
    Inheritance,
    /// A class implements an interface.
    Implementation,
    /// A field keeps a value of the type.
    Association,
}

/// One edge between two types of the diagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    /// The qualified name of the type the edge starts at: the subtype or the owner.
    pub from: String,
    /// The qualified name of the type the edge points at.
    pub to: String,
    pub kind: GraphEdgeKind,
    /// `*` for a field that holds a collection of the target type.
    pub multiplicity: Option<String>,
}

/// The types and the edges between them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeGraph {
    pub nodes: Vec<GraphType>,
    pub edges: Vec<GraphEdge>,
}

/// What a diagram is made for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeGraphQuery {
    /// The type at a place and the types it is related to.
    At(DocumentSpot),
    /// Every type of the package in a folder, with their relationships.
    Folder(PathBuf),
    /// One type by its qualified name, for a type added to an open diagram.
    Type(String),
}

pub type TypeGraphProvider = dyn Provider<TypeGraphQuery, TypeGraph>;
