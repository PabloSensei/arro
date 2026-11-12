//! Colours and metrics of the editor. A plain struct: whoever owns the theme fills it.

use lang_api::{SemanticTokenKind, SemanticTokenModifiers};
use syntax::HighlightKind;
use ui_kit::metrics::{CODE_LINE_HEIGHT, CODE_SIZE, MONO_FONT};
use ui_kit::*;

#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub dark: bool,
    pub background: Hsla,
    pub code: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub comment: Hsla,
    pub doc: Hsla,
    pub doc_tag: Hsla,
    pub field: Hsla,
    pub function: Hsla,
    pub annotation: Hsla,
    /// Text a server marked as something that can go.
    pub unused: Hsla,
    pub semantic_field: Hsla,
    pub semantic_parameter: Hsla,
    pub semantic_local: Hsla,
    pub semantic_method: Hsla,
    pub semantic_type: Hsla,
    pub semantic_type_parameter: Hsla,
    pub semantic_constant: Hsla,
    pub semantic_annotation: Hsla,
    pub caret: Hsla,
    pub caret_row: Hsla,
    pub selection: Hsla,
    pub line_number: Hsla,
    pub line_number_current: Hsla,
    pub indent_guide: Hsla,
    pub inlay: Hsla,
    pub inlay_background: Hsla,
    /// The labels above declarations, as the interface font draws them.
    pub code_vision: Hsla,
    pub fold_background: Hsla,
    pub fold_text: Hsla,
    pub error_mark: Hsla,
    pub warning_mark: Hsla,
    pub panel: Hsla,
    pub panel_border: Hsla,
    pub accent: Hsla,
    pub error: Hsla,
    pub occurrence: Hsla,
    pub bracket: Hsla,
    pub find_match: Hsla,
    pub find_current: Hsla,
    pub scrollbar_thumb: Hsla,
    pub scrollbar_thumb_hover: Hsla,
    pub scrollbar_track: Hsla,
    pub stripe_error: Hsla,
    pub stripe_warning: Hsla,
    pub stripe_find: Hsla,
    pub stripe_caret: Hsla,
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
}

/// How a semantic token is drawn: its colour and the marks the modifiers add.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SemanticLook {
    pub color: Hsla,
    pub italic: bool,
    pub strikethrough: bool,
}

impl EditorStyle {
    /// Colours from the theme tokens, the metrics from the theme crate.
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            dark: theme.dark,
            background: theme.editor_bg,
            code: theme.code,
            keyword: theme.keyword,
            string: theme.string,
            number: theme.number,
            comment: theme.comment,
            doc: theme.doc,
            doc_tag: theme.doc_tag,
            field: theme.field,
            function: theme.function,
            annotation: theme.annotation,
            unused: theme.unused,
            semantic_field: theme.semantic_field,
            semantic_parameter: theme.semantic_parameter,
            semantic_local: theme.semantic_local,
            semantic_method: theme.semantic_method,
            semantic_type: theme.semantic_type,
            semantic_type_parameter: theme.semantic_type_parameter,
            semantic_constant: theme.semantic_constant,
            semantic_annotation: theme.semantic_annotation,
            caret: theme.caret,
            caret_row: theme.caret_row,
            selection: theme.selection,
            line_number: theme.line_number,
            line_number_current: theme.line_number_current,
            indent_guide: theme.indent_guide,
            inlay: theme.inlay,
            inlay_background: theme.inlay_bg,
            code_vision: theme.text_muted,
            fold_background: theme.fold_bg,
            fold_text: theme.fold_text,
            // No theme token yet: the red of an IDEA error stripe.
            error_mark: rgb(0xe55757).into(),
            warning_mark: theme.warning_mark,
            panel: theme.popup_bg,
            panel_border: theme.popup_border,
            accent: theme.accent,
            error: if theme.dark {
                rgb(0xf75464).into()
            } else {
                rgb(0xc7222d).into()
            },
            occurrence: theme.selection_muted,
            bracket: theme.selection_inactive,
            find_match: Hsla {
                a: 0.55,
                ..theme.match_bg
            },
            find_current: Hsla {
                a: 0.7,
                ..theme.accent
            },
            scrollbar_thumb: theme.scrollbar_thumb,
            scrollbar_thumb_hover: theme.scrollbar_thumb_hover,
            scrollbar_track: theme.scrollbar_track,
            stripe_error: theme.stripe_error,
            stripe_warning: theme.stripe_warning,
            stripe_find: theme.stripe_find,
            stripe_caret: theme.stripe_caret,
            font_family: MONO_FONT.into(),
            font_size: CODE_SIZE,
            line_height: CODE_LINE_HEIGHT,
        }
    }

    pub fn dark() -> Self {
        Self::from_theme(&Theme::dark())
    }

    pub fn light() -> Self {
        Self::from_theme(&Theme::light())
    }

    pub(crate) fn color_of(&self, kind: HighlightKind) -> Hsla {
        match kind {
            HighlightKind::Keyword => self.keyword,
            HighlightKind::String => self.string,
            HighlightKind::Number => self.number,
            HighlightKind::Comment => self.comment,
            HighlightKind::DocComment => self.doc,
            HighlightKind::DocTag => self.doc_tag,
            HighlightKind::Field | HighlightKind::Constant => self.field,
            HighlightKind::Function => self.function,
            HighlightKind::Annotation => self.annotation,
        }
    }

    pub(crate) fn is_italic(&self, kind: HighlightKind) -> bool {
        matches!(
            kind,
            HighlightKind::DocComment | HighlightKind::DocTag | HighlightKind::Constant
        ) || (kind == HighlightKind::Comment && !self.dark)
    }

    /// How the semantic token of `kind` with `modifiers` is drawn. `None` leaves the
    /// syntax colour of the same range as it is.
    pub(crate) fn token_style(
        &self,
        kind: SemanticTokenKind,
        modifiers: SemanticTokenModifiers,
    ) -> Option<SemanticLook> {
        let color = match kind {
            SemanticTokenKind::Unknown => return None,
            SemanticTokenKind::Namespace => self.doc,
            SemanticTokenKind::Type => self.semantic_type,
            SemanticTokenKind::TypeParameter => self.semantic_type_parameter,
            SemanticTokenKind::Parameter => self.semantic_parameter,
            SemanticTokenKind::Variable => self.semantic_local,
            SemanticTokenKind::Property => self.semantic_field,
            SemanticTokenKind::EnumMember => self.semantic_constant,
            SemanticTokenKind::Function | SemanticTokenKind::Method => self.semantic_method,
            SemanticTokenKind::Keyword | SemanticTokenKind::Modifier => self.keyword,
            SemanticTokenKind::Comment => self.comment,
            SemanticTokenKind::String => self.string,
            SemanticTokenKind::Number => self.number,
            SemanticTokenKind::Operator => self.code,
            SemanticTokenKind::Annotation => self.semantic_annotation,
        };
        // A constant is a static field that never changes: the constant colour wins over
        // the plain field one.
        let color = if modifiers.is_constant() {
            self.semantic_constant
        } else {
            color
        };
        Some(SemanticLook {
            color,
            italic: modifiers.static_member || modifiers.abstract_member,
            strikethrough: modifiers.deprecated,
        })
    }
}
