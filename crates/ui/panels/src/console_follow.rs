//! Where a console follows its output. A new line keeps the view on the last line while
//! the reader stands at the end of the console; a reader who scrolled up keeps the place
//! they read until they come back to the end. Both console windows share this rule.

use ui_kit::*;

/// How close to the last line still counts as reading the end. The row heights are
/// rounded to whole pixels, so an exact match of the offsets is not reliable.
const END_MARGIN: f32 = 32.;

/// The scroll handle of a console and the rule that decides when it follows its output.
#[derive(Clone, Debug, Default)]
pub(crate) struct ConsoleFollow {
    scroll: UniformListScrollHandle,
}

impl ConsoleFollow {
    pub(crate) fn new() -> Self {
        Self {
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The handle the list of the console tracks and the bars read.
    pub(crate) fn handle(&self) -> UniformListScrollHandle {
        self.scroll.clone()
    }

    /// The handle itself, for the list that tracks it.
    pub(crate) fn list_handle(&self) -> &UniformListScrollHandle {
        &self.scroll
    }

    /// Whether the view stands at the end of the console, so new output belongs on
    /// screen. A console that fits is always at its end.
    pub(crate) fn at_end(&self) -> bool {
        let handle = self.handle();
        let state = handle.0.borrow();
        let max = state.base_handle.max_offset().y;
        max <= px(0.) || state.base_handle.offset().y <= -max + px(END_MARGIN)
    }

    /// Scrolls to the last line when the reader stands at the end, and leaves the view
    /// alone otherwise. Returns whether the view follows.
    pub(crate) fn follow_end(&self, last: usize) -> bool {
        if !self.at_end() {
            return false;
        }
        self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
        true
    }

    /// Moves the view to a line the user asked for, such as a match of the search bar.
    pub(crate) fn scroll_to(&self, index: usize, strategy: ScrollStrategy) {
        self.scroll.scroll_to_item(index, strategy);
    }
}
