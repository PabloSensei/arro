//! The code editor view. `EditorState` holds the document and every command and knows
//! nothing about the toolkit; `EditorView` turns events into commands and paints.

mod actions;
mod annotate;
mod bar;
mod blocks;
mod code_lens;
mod decor;
mod delete;
mod element;
mod generate;
mod hierarchy;
mod hippie;
mod input;
mod live;
mod markdown_preview;
mod quick_doc;
mod refactor;
mod regions;
mod rendered_doc;
mod scrollbar;
mod semantic;
mod state;
mod style;
mod view;

pub use annotate::BlameNote;
pub use decor::PaneDecor;
pub use element::StripeKind;
pub use live::{KeyUse, LiveTemplates, TemplateList};
pub use markdown_preview::PreviewMode;
pub use quick_doc::{
    DocAlign, DocBlock, DocHost, DocInline, DocLinkHandler, DocSelectEvent, DocSelectHandler,
    display_text as display_documentation, parse as parse_documentation, render_document,
    split_line_fragment,
};
pub use refactor::FolderMove;
pub use semantic::{
    CompletionCase, CompletionSources, ExternalLinks, ExternalOpener, LocalCompletions, Semantic,
    completion_order, completion_order_with, completion_parts, expand_snippet, show_location,
    uri_path,
};
pub use state::{Conflict, EditorState, LoadError, SaveOptions, Saved, TrimTrailing};
pub use style::{EditorStyle, SemanticLook};
// The language data a caller needs to describe its files to the editor, and to build
// one in a test.
pub use syntax::{FoldStyle, HighlightKind, HighlightSpan, LanguageConfig, StatementRules};
pub use templates::{ANY as TEMPLATE_ANY, Kind as TemplateKind, Template, TemplateLanguage};
pub use view::{
    BookmarkEntry, DetailedUsage, DocumentationQuery, EditorEvent, EditorOptions, EditorView,
    GutterKind, GutterMark, InlayPaint, Languages, LineDeclaration, OutlineItem, OutlineKind,
    ProblemSummary, RowPaint, RunMark, RunMarkState, Spot, TemplateSettings, Usage, UsagesUpdate,
    bookmarks, clipboard_history, highlight_text, install_keymap,
};
