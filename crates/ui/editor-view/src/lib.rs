//! The code editor view. `EditorState` holds the document and every command and knows
//! nothing about the toolkit; `EditorView` turns events into commands and paints.

mod actions;
mod annotate;
mod bar;
mod blocks;
mod bundle;
mod code_lens;
mod decor;
mod delete;
mod element;
mod generate;
mod hierarchy;
mod hippie;
mod input;
mod live;
mod markdown_images;
mod markdown_preview;
mod popups;
mod quick_doc;
mod refactor;
mod regions;
mod rendered_doc;
mod scrollbar;
mod semantic;
mod spelling;
mod state;
mod style;
mod view;

pub use annotate::BlameNote;
pub use bundle::{BundleChanged, BundleHost, BundleMode, BundleModes};
pub use decor::PaneDecor;
pub use element::StripeKind;
#[cfg(any(test, feature = "test-support"))]
pub use element::{reset_widest_row_counts, widest_row_counts};
pub use live::{KeyUse, LiveTemplates, TemplateList};
pub use markdown_preview::PreviewMode;
pub use markdown_preview::PreviewModes;
pub use quick_doc::{
    DocAlign, DocBlock, DocHost, DocImage, DocInline, DocLinkHandler, DocPicture, DocSelectEvent,
    DocSelectHandler, display_text as display_documentation, parse as parse_documentation,
    render_document, split_line_fragment,
};
pub use refactor::FolderMove;
pub use semantic::{
    CompletionCase, CompletionSources, ExternalLinks, ExternalOpener, FileLanguages,
    InspectionProfile, LocalCompletions, Semantic, SymbolLookup, completion_order,
    completion_order_with, completion_parts, expand_snippet, show_location, uri_path,
};
pub use spelling::SpellingMark;
pub use state::{
    ActionsOnSave, Conflict, EditorState, LoadError, SaveOptions, Saved, TrimTrailing,
};
pub use style::{EditorStyle, SemanticLook};
// The language data a caller needs to describe its files to the editor, and to build
// one in a test.
pub use syntax::{FoldStyle, HighlightKind, HighlightSpan, LanguageConfig, StatementRules};
pub use templates::{ANY as TEMPLATE_ANY, Kind as TemplateKind, Template, TemplateLanguage};
pub use view::{
    ActionMark, AiProblem, BookmarkEntry, CodeStyleLanguages, CoverageMark, CoverageState,
    DebugValue, DetailedUsage, DocumentationQuery, EditorEvent, EditorOptions, EditorView,
    EndpointMark, ExtraTemplates, FileBar, GutterKind, GutterMark, InlayPaint, InlineValue,
    InlineWatchRow, Languages, LineDeclaration, OutlineItem, OutlineKind, PasteText, PlaceMark,
    PlaceTarget, PlainSyntax, ProblemSummary, RowPaint, RunFiles, RunMark, RunMarkState, Spot,
    TemplateSettings, Usage, UsagesUpdate, bookmarks, clipboard_history, highlight_text,
    install_keymap,
};
