//! The misses a spelling check found in the text of an editor, and what the menu of a
//! miss offers: a replacement, saving the word, or turning the check off.

use std::ops::Range;

use lang_api::{DiagnosticEntry, Severity, TextSpan};
use ui_kit::gpui::Context;

use crate::view::EditorView;

/// One misspelled word of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellingMark {
    pub range: Range<usize>,
    pub word: String,
    /// The replacements the check offers, nearest first.
    pub suggestions: Vec<String>,
    /// The word is a piece of a declaration name, so a replacement renames the symbol.
    pub declaration: bool,
}

impl EditorView {
    /// The buffer version the marks on screen were checked at.
    pub fn spelling_version(&self) -> Option<i32> {
        self.spelling_version
    }

    /// Claims the check of `version` for one background run. False when this version is
    /// checked already or on its way.
    pub fn claim_spelling(&mut self, version: i32) -> bool {
        if self.spelling_version == Some(version) || self.spelling_asked == Some(version) {
            return false;
        }
        self.spelling_asked = Some(version);
        true
    }

    /// Forgets the marks and the runs of this text, so that the next check asks again.
    pub fn forget_spelling(&mut self) {
        self.spelling_version = None;
        self.spelling_asked = None;
    }

    /// Puts the marks of a background check into the editor. An answer about an older
    /// text is dropped.
    pub fn set_spelling_marks(
        &mut self,
        version: i32,
        marks: Vec<SpellingMark>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.state else {
            return;
        };
        if state.version() as i32 != version {
            return;
        }
        let store = state.store();
        let entries: Vec<DiagnosticEntry> = marks
            .iter()
            .map(|mark| DiagnosticEntry {
                span: TextSpan {
                    start: crate::semantic::point_at(&store, mark.range.start),
                    end: crate::semantic::point_at(&store, mark.range.end),
                },
                severity: Severity::Hint,
                message: format!("Typo: In word '{}'", mark.word),
                unnecessary: false,
                code: Some(lang_api::SPELLING_INSPECTION.to_owned()),
            })
            .collect();
        self.spelling_marks = marks;
        self.spelling_version = Some(version);
        self.spelling_asked = None;
        self.set_native_marks_from("spelling", entries, cx);
    }

    /// The bytes the viewport shows, for a check that answers the visible part first.
    pub fn visible_range(&self) -> Option<Range<usize>> {
        let state = self.state.as_ref()?;
        let line_height = self.style.line_height.max(1.);
        let rows = (f32::from(self.viewport.height) / line_height)
            .floor()
            .max(1.) as usize;
        let first = (self.scroll_top / line_height).floor().max(0.) as usize;
        let display = state.display();
        let last = (first + rows).min(display.row_count().saturating_sub(1));
        let start = display.to_offset(
            display_map::DisplayPoint {
                row: first,
                column: 0,
            },
            display_map::Bias::Left,
        );
        let end = display.to_offset(
            display_map::DisplayPoint {
                row: last,
                column: 0,
            },
            display_map::Bias::Left,
        );
        Some(start..end.max(start))
    }

    /// The miss whose word covers `caret`, when the check that found it is on.
    pub(crate) fn spelling_mark_at(&self, caret: usize) -> Option<&SpellingMark> {
        let lsp = self.lsp.as_ref()?;
        let found = lsp.marks.iter().any(|mark| {
            mark.code.as_deref() == Some(lang_api::SPELLING_INSPECTION)
                && mark.range.start <= caret
                && caret <= mark.range.end
        });
        if !found {
            return None;
        }
        self.spelling_marks
            .iter()
            .find(|mark| mark.range.start <= caret && caret <= mark.range.end)
    }
}
