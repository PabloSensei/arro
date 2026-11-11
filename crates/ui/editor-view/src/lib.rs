//! The code editor view. `EditorState` holds the document and every command and knows
//! nothing about the toolkit; `EditorView` turns events into commands and paints.

mod actions;
mod bar;
mod blocks;
mod delete;
mod element;
mod generate;
mod hierarchy;
mod hippie;
mod input;
mod live;
mod refactor;
mod regions;
mod scrollbar;
mod semantic;
mod state;
mod style;
mod view;

pub use element::StripeKind;
pub use live::{KeyUse, LiveTemplates, TemplateList};
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
pub use syntax::{FoldStyle, LanguageConfig, StatementRules};
pub use templates::{ANY as TEMPLATE_ANY, Kind as TemplateKind, Template, TemplateLanguage};
pub use view::{
    BookmarkEntry, DetailedUsage, EditorEvent, EditorOptions, EditorView, GutterKind, GutterMark,
    Languages, OutlineItem, OutlineKind, ProblemSummary, RunMark, RunMarkState, Spot, Usage,
    UsagesUpdate, bookmarks, clipboard_history, install_keymap,
};
