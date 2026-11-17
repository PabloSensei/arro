//! Geometry of the scroll bars and of the marks on the vertical one. Plain numbers, so
//! the view and its tests can work without a frame.

use display_map::DisplaySnapshot;
use text_core::TextStore;

/// The bar at rest and under the pointer: the thin and the wide look.
pub(crate) const WIDTH: f32 = 6.;
pub(crate) const HOVER_WIDTH: f32 = 12.;
/// The space kept free between the bar and the right edge of the editor.
pub(crate) const MARGIN: f32 = 2.;
/// The width the text leaves for the bar, so the wide look never covers the text.
pub(crate) const BAND: f32 = HOVER_WIDTH + MARGIN;
pub(crate) const HORIZONTAL_HEIGHT: f32 = 6.;
pub(crate) const HORIZONTAL_HOVER_HEIGHT: f32 = 12.;
/// A thumb never becomes shorter than this, however long the file is.
pub(crate) const MIN_THUMB: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

/// Where the thumb starts and how long it is, from the start of the track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Thumb {
    pub offset: f32,
    pub len: f32,
}

/// One bar: the track and the content that scrolls in it, in window coordinate units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bar {
    pub track_start: f32,
    pub track_len: f32,
    pub content: f32,
    pub viewport: f32,
}

impl Bar {
    /// The thumb for `scroll`, `None` when the whole content is in sight.
    pub fn thumb(self, scroll: f32) -> Option<Thumb> {
        if self.track_len <= 0. || self.content <= self.viewport + 0.5 {
            return None;
        }
        let ratio = (self.viewport / self.content).clamp(0., 1.);
        let len = (self.track_len * ratio).max(MIN_THUMB.min(self.track_len));
        let available = (self.track_len - len).max(0.);
        let max_scroll = self.content - self.viewport;
        let offset = available * (scroll / max_scroll).clamp(0., 1.);
        Some(Thumb { offset, len })
    }

    /// The scroll position that puts the grabbed part of the thumb under `pointer`.
    pub fn scroll_at(self, pointer: f32, grab: f32) -> f32 {
        if self.content <= self.viewport + 0.5 {
            return 0.;
        }
        let len = self.thumb(0.).map_or(0., |thumb| thumb.len);
        let available = (self.track_len - len).max(0.);
        if available <= 0. {
            return 0.;
        }
        let local = (pointer - self.track_start - grab).clamp(0., available);
        local / available * (self.content - self.viewport)
    }
}

/// A thumb held by the pointer: the axis and the part of the thumb under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Drag {
    pub axis: Axis,
    pub grab: f32,
}

/// Where the line stands in the whole file, from zero to one, with folds taken from
/// the display map: a hidden line stands where its fold does.
pub(crate) fn stripe_fraction<S: TextStore>(display: &DisplaySnapshot<'_, S>, line: usize) -> f32 {
    let rows = display.row_count();
    if rows == 0 {
        return 0.;
    }
    let row = display.line_to_row(line).min(rows - 1);
    (row as f32 + 0.5) / rows as f32
}

#[cfg(test)]
mod checks {
    use super::{Bar, MIN_THUMB, stripe_fraction};
    use crate::state::EditorState;
    use keymap::editor as actions;
    use text_core::FileFormat;

    fn bar(track_len: f32, content: f32, viewport: f32) -> Bar {
        Bar {
            track_start: 0.,
            track_len,
            content,
            viewport,
        }
    }

    #[test]
    fn the_thumb_is_the_share_of_the_view_in_the_track() {
        let thumb = bar(100., 200., 100.).thumb(0.).unwrap();
        assert_eq!(thumb.offset, 0.);
        assert_eq!(thumb.len, 50.);

        let thumb = bar(100., 1000., 100.).thumb(450.).unwrap();
        assert_eq!(thumb.len, MIN_THUMB, "a long file keeps the minimum");
        let max = 100. - MIN_THUMB;
        assert!((thumb.offset - max * 0.5).abs() < 1e-3, "{thumb:?}");
    }

    #[test]
    fn a_file_that_fits_has_no_thumb() {
        assert_eq!(bar(100., 90., 100.).thumb(0.), None);
        assert_eq!(bar(100., 100., 100.).thumb(0.), None);
        assert_eq!(bar(100., 90., 100.).scroll_at(50., 0.), 0.);
    }

    #[test]
    fn a_click_on_the_track_puts_the_middle_of_the_thumb_there() {
        let bar = bar(100., 200., 100.);
        let thumb = bar.thumb(0.).unwrap();
        let scroll = bar.scroll_at(50., thumb.len / 2.);
        assert_eq!(
            scroll, 50.,
            "the middle of the content comes to the pointer"
        );
    }

    #[test]
    fn a_dragged_thumb_moves_the_scroll() {
        let bar = bar(100., 200., 100.);
        let thumb = bar.thumb(0.).unwrap();
        let grab = 5.;
        assert_eq!(
            bar.scroll_at(bar.track_start + thumb.offset + grab, grab),
            0.
        );
        let moved = bar.scroll_at(50., grab);
        assert!(moved > 0., "{moved}");
        assert_eq!(moved, 90., "45 of the free 50 is 90 of the 100 hidden");
    }

    #[test]
    fn a_stripe_of_a_hidden_line_stands_on_its_fold() {
        let text = "//region R\none\ntwo\nthree\n//endregion\nlast\n";
        let mut state = EditorState::new(text, FileFormat::default(), None, false);
        let before_rows = state.display().row_count();
        let before = stripe_fraction(&state.display(), 5);
        state.select_range(0..0);
        state.perform(actions::FOLD, 10);
        let after_rows = state.display().row_count();
        assert!(after_rows < before_rows, "the region is folded");

        let after = stripe_fraction(&state.display(), 5);
        assert!(after < before, "the last line moves up with the fold");
        assert_eq!(
            stripe_fraction(&state.display(), 2),
            stripe_fraction(&state.display(), 0),
            "a hidden line stands where the fold does"
        );
    }
}
