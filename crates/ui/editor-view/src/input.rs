//! Text input and IME. The platform speaks UTF-16 offsets into the whole document.

use std::ops::Range;

use text_core::TextStore;
use ui_kit::*;

use crate::element::TEXT_INSET;
use crate::view::EditorView;

impl EditorView {
    fn to_utf8(&self, range: &Range<usize>) -> Option<Range<usize>> {
        let store = self.state.as_ref()?.store();
        Some(store.utf16_to_offset(range.start)..store.utf16_to_offset(range.end))
    }

    fn to_utf16(&self, range: &Range<usize>) -> Option<Range<usize>> {
        let store = self.state.as_ref()?.store();
        Some(store.offset_to_utf16(range.start)..store.offset_to_utf16(range.end))
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bytes = self.to_utf8(&range)?;
        adjusted.replace(self.to_utf16(&bytes)?);
        Some(self.state.as_ref()?.store().slice(bytes))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let primary = self.state.as_ref()?.selections().primary();
        Some(UTF16Selection {
            range: self.to_utf16(&primary.range())?,
            reversed: primary.head < primary.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.to_utf16(&self.state.as_ref()?.marked()?)
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.unmark();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.override_typed(text, cx)
            || self.generation_typed(text, cx)
            || self.move_typed(text, cx)
            || self.rename_typed(text, cx)
        {
            return;
        }
        let bytes = range.and_then(|range| self.to_utf8(&range));
        let change = self
            .state
            .as_mut()
            .and_then(|state| state.replace_range(bytes, text, false));
        let typed = change.is_some();
        self.changed(change, cx);
        if typed {
            self.typed(text, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bytes = range.and_then(|range| self.to_utf8(&range));
        let change = self
            .state
            .as_mut()
            .and_then(|state| state.replace_range(bytes, text, true));
        self.changed(change, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = self.to_utf8(&range)?.start;
        let layout = self.layout.as_ref()?;
        let row = self.state.as_ref()?.display().to_display(start);
        let shown = layout
            .rows
            .iter()
            .find(|laid_out| laid_out.row == row.row)?;
        let x = bounds.left()
            + px(crate::element::GUTTER_WIDTH + TEXT_INSET)
            + shown.text.x_for_index(row.column)
            - px(self.scroll_left);
        Some(Bounds::new(
            point(x, shown.y),
            size(px(2.), px(self.style.line_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_at(point)?;
        Some(self.state.as_ref()?.store().offset_to_utf16(offset))
    }
}
