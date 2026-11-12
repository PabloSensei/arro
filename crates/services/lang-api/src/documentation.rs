use std::ops::Range;

/// A run of a documentation line. The editor draws each kind with its own look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentationInline {
    Text(String),
    Code(String),
    Strong(Vec<DocumentationInline>),
    Emphasis(Vec<DocumentationInline>),
    /// A reference of the language, such as `Type#member`, with the text shown for it.
    Link {
        text: String,
        target: String,
    },
}

/// One block of a documentation comment. A language maps its own markup onto these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentationBlock {
    Paragraph(Vec<DocumentationInline>),
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<DocumentationInline>>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    /// A block tag such as `@param`, with the subject it names when it has one.
    Tag {
        name: String,
        subject: Option<String>,
        description: Vec<DocumentationInline>,
    },
}

/// A documentation comment of a text, with the blocks it renders to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentationComment {
    /// Byte offsets of the whole comment in the text it came from.
    pub range: Range<usize>,
    pub blocks: Vec<DocumentationBlock>,
}

/// The documentation comments of a file. Blocks, so callers run it off the UI thread.
pub trait DocumentationCommentProvider: Send + Sync {
    fn documentation_comments(&self, text: &str) -> Vec<DocumentationComment>;
}
