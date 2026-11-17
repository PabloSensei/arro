//! The popups of the editor and the rules that keep them from covering each other.
//!
//! The completion list, Alt+Enter, Parameter Info and the note under the caret are
//! windows over the same place of the text, so they follow one order: a newer popup
//! hides the older one that would stand in its way, Escape closes the topmost, and
//! Parameter Info keeps to the other side of the line than the completion list. The
//! pointer popup of the documentation never opens over a list. These rules live here
//! rather than at every place that opens a popup.

use text_core::TextStore;
use ui_kit::metrics::CODE_LINE_HEIGHT;
use ui_kit::*;

use crate::element::TEXT_INSET;
use crate::semantic::is_word;
use crate::view::EditorView;

/// A popup of the editor that stands over the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Popup {
    /// The completion list.
    Completion,
    /// The overloads of the call around the caret, Ctrl+P.
    ParameterInfo,
    /// Alt+Enter: the intentions and the quick fixes of the place.
    Actions,
    /// Ctrl+B found several places: the list to pick one from.
    Navigation,
    /// The documentation of a name, from Ctrl+Q or the pointer.
    QuickDoc,
    /// The note under the caret: the problem of the place or a failed navigation.
    Note,
}

/// The `(` of the call around `at`, when it is on the same line: Parameter Info stands
/// at the opening bracket of the call rather than where the caret happens to be.
fn call_open_paren(store: &text_core::RopeStore, at: usize) -> Option<usize> {
    let mut depth = 0;
    let mut offset = at.min(store.len());
    while offset > 0 {
        let previous = store.prev_boundary(offset);
        let character = store.char_at(previous)?;
        match character {
            ')' | ']' | '}' => depth += 1,
            '(' => {
                if depth == 0 {
                    return Some(previous);
                }
                depth -= 1;
            }
            '\n' | ';' | '{' => return None,
            _ => {}
        }
        offset = previous;
    }
    None
}

/// Whether two rectangles of the window share at least a pixel.
fn boxes_overlap(left: Bounds<Pixels>, right: Bounds<Pixels>) -> bool {
    left.right() > right.left()
        && right.right() > left.left()
        && left.bottom() > right.top()
        && right.bottom() > left.top()
}

/// The word around `offset`: a caret inside that range belongs to the same place.
fn word_span(store: &text_core::RopeStore, offset: usize) -> std::ops::Range<usize> {
    let start = crate::semantic::word_start(store, offset);
    let mut end = start;
    while end < store.len() {
        let Some(character) = store.char_at(end) else {
            break;
        };
        if !is_word(character) {
            break;
        }
        end = store.next_boundary(end);
    }
    start..end
}

impl EditorView {
    /// Whether the popup is on screen.
    pub(crate) fn popup_open(&self, popup: Popup) -> bool {
        let Some(lsp) = &self.lsp else {
            return false;
        };
        match popup {
            Popup::Completion => lsp.completion.is_some(),
            Popup::ParameterInfo => lsp.signatures.is_some(),
            Popup::Actions => lsp.actions.is_some(),
            Popup::Navigation => lsp.navigation_list.is_some(),
            Popup::QuickDoc => lsp.quick.is_some(),
            Popup::Note => lsp.note.is_some(),
        }
    }

    /// Whether a popup that holds the focus of the editor is open: the lists and the
    /// note go away with the focus, the pointer popup stays as it is.
    pub(crate) fn list_popups_open(&self) -> bool {
        [
            Popup::Completion,
            Popup::ParameterInfo,
            Popup::Actions,
            Popup::Navigation,
            Popup::Note,
        ]
        .into_iter()
        .any(|popup| self.popup_open(popup))
    }

    /// Takes the completion list and everything its request brought away: the
    /// documentation of a row and the questions that are still in flight. A stale answer
    /// must not open the list again, and the documentation goes with its list.
    pub(crate) fn close_completion(&mut self) {
        if let Some(lsp) = &mut self.lsp {
            lsp.completion = None;
            lsp.completion_doc = None;
        }
        self.completion_task = None;
        self.completion_doc_task = None;
    }

    /// Alt+Enter takes the screen: the completion list, Parameter Info, the note under
    /// the caret, the list of navigation places and the documentation popup would cover
    /// the rows of its menu.
    pub(crate) fn before_actions_popup(&mut self, cx: &mut Context<Self>) {
        let open = self.popup_open(Popup::Completion)
            || self.popup_open(Popup::ParameterInfo)
            || self.popup_open(Popup::Navigation)
            || self.popup_open(Popup::Note);
        let quick = self.popup_open(Popup::QuickDoc);
        if let Some(lsp) = &mut self.lsp {
            lsp.signatures = None;
            lsp.parameter_walk = None;
            lsp.navigation_list = None;
            lsp.note = None;
        }
        self.close_completion();
        if quick {
            self.close_quick_doc(cx);
        } else if open {
            cx.notify();
        }
    }

    /// A list takes the screen: the menu of Alt+Enter, the note, the list of navigation
    /// places and the documentation give way. Parameter Info stays, it stands on the
    /// other side of the line.
    pub(crate) fn before_completion_popup(&mut self, cx: &mut Context<Self>) {
        let open = self.popup_open(Popup::Actions)
            || self.popup_open(Popup::Navigation)
            || self.popup_open(Popup::Note);
        let quick = self.popup_open(Popup::QuickDoc);
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
            lsp.navigation_list = None;
            lsp.note = None;
        }
        if quick {
            self.close_quick_doc(cx);
        } else if open {
            cx.notify();
        }
    }

    /// Ctrl+Q takes the screen: the lists and the note give way.
    pub(crate) fn before_quick_doc_popup(&mut self, cx: &mut Context<Self>) {
        self.dismiss_list_popups(cx);
    }

    /// Parameter Info takes its place beside the completion list: the menu of Alt+Enter,
    /// the list of navigation places and the documentation give way.
    pub(crate) fn before_parameter_info_popup(&mut self, cx: &mut Context<Self>) {
        let open = self.popup_open(Popup::Actions) || self.popup_open(Popup::Navigation);
        let quick = self.popup_open(Popup::QuickDoc);
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
            lsp.navigation_list = None;
        }
        if quick {
            self.close_quick_doc(cx);
        } else if open {
            cx.notify();
        }
    }

    /// The list of places of Ctrl+B takes the screen: the completion list, Parameter
    /// Info, the note and the documentation would cover it.
    pub(crate) fn before_navigation_popup(&mut self, cx: &mut Context<Self>) {
        let quick = self.popup_open(Popup::QuickDoc);
        self.dismiss_list_popups(cx);
        if quick {
            self.close_quick_doc(cx);
        }
    }

    /// The note under the caret takes the screen: the lists would cover it.
    pub(crate) fn before_note_popup(&mut self, cx: &mut Context<Self>) {
        self.dismiss_list_popups(cx);
    }

    /// Hides the completion list, Parameter Info, Alt+Enter, the list of navigation
    /// places and the note. A completion question that is still on its way is dropped
    /// too: its answer belongs to a list nobody waits for now.
    pub(crate) fn dismiss_list_popups(&mut self, cx: &mut Context<Self>) {
        let open = self.list_popups_open();
        self.close_completion();
        if !open {
            return;
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.signatures = None;
            lsp.parameter_walk = None;
            lsp.actions = None;
            lsp.navigation_list = None;
            lsp.note = None;
        }
        cx.notify();
    }

    /// A click in the editor closes the list of entries in the way, as IDEA does.
    pub(crate) fn dismiss_click_popups(&mut self, cx: &mut Context<Self>) {
        let open = self.popup_open(Popup::Completion)
            || self.popup_open(Popup::Actions)
            || self.popup_open(Popup::Navigation)
            || self.popup_open(Popup::Note);
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
            lsp.navigation_list = None;
            lsp.note = None;
        }
        self.close_completion();
        if open {
            cx.notify();
        }
    }

    /// Every popup goes away: the tab leaves the screen or the window loses the focus.
    pub fn dismiss_popups(&mut self, cx: &mut Context<Self>) {
        self.dismiss_list_popups(cx);
        if self.popup_open(Popup::QuickDoc) {
            self.close_quick_doc(cx);
        }
    }

    /// Escape closes the topmost popup and answers whether one was on screen. The menu
    /// of Alt+Enter stands over the note, the note over Parameter Info and the list of
    /// navigation places, and those over the completion list. The documentation popup
    /// closes on any key before this runs.
    pub(crate) fn close_top_popup(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(lsp) = &self.lsp else {
            return false;
        };
        let top = if lsp.actions.is_some() {
            Popup::Actions
        } else if lsp.note.is_some() {
            Popup::Note
        } else if lsp.signatures.is_some() {
            Popup::ParameterInfo
        } else if lsp.navigation_list.is_some() {
            Popup::Navigation
        } else if lsp.completion.is_some() {
            Popup::Completion
        } else {
            return false;
        };
        match top {
            Popup::Actions => {
                if let Some(lsp) = &mut self.lsp {
                    lsp.actions = None;
                }
            }
            Popup::Note => {
                if let Some(lsp) = &mut self.lsp {
                    lsp.note = None;
                }
            }
            Popup::ParameterInfo => {
                if let Some(lsp) = &mut self.lsp {
                    lsp.signatures = None;
                    lsp.parameter_walk = None;
                }
            }
            Popup::Navigation => {
                if let Some(lsp) = &mut self.lsp {
                    lsp.navigation_list = None;
                }
            }
            Popup::Completion => self.close_completion(),
            // The documentation closes before this runs; the list never stands alone.
            Popup::QuickDoc => {}
        }
        cx.notify();
        true
    }

    /// The text changed: the menu of Alt+Enter belongs to the text it opened on.
    pub(crate) fn dismiss_actions_popup(&mut self, cx: &mut Context<Self>) {
        if !self.popup_open(Popup::Actions) {
            return;
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
        }
        cx.notify();
    }

    /// The caret moved: the menu of Alt+Enter belongs to the word it opened on and
    /// closes when the caret leaves that word.
    pub(crate) fn follow_actions_popup(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.actions.as_ref())
            .map(|menu| menu.at)
        else {
            return;
        };
        let Some(state) = &self.state else {
            return;
        };
        let head = state.head();
        let store = state.store();
        let word = word_span(&store, at);
        if word.contains(&head) {
            return;
        }
        if let Some(lsp) = &mut self.lsp {
            lsp.actions = None;
        }
        cx.notify();
    }

    /// The pointer must not open the documentation over a list that holds the screen.
    pub(crate) fn hover_popup_blocked(&self) -> bool {
        self.popup_open(Popup::Completion)
            || self.popup_open(Popup::Actions)
            || self.popup_open(Popup::Navigation)
    }

    /// The height of the completion list as it stands, its border included. The rows of
    /// the list are shaped at the size of the code font, which may be taller than a row
    /// of the text when the line spacing of the editor is small.
    pub(crate) fn completion_height(&self) -> Option<f32> {
        let lsp = self.lsp.as_ref()?;
        let completion = lsp.completion.as_ref()?;
        let rows = completion.window().len();
        if rows == 0 {
            return None;
        }
        let row = self.style.line_height.max(CODE_LINE_HEIGHT);
        let mut height = rows as f32 * row + 8.;
        // The documentation under the rows belongs to the same box: the rules that put
        // Parameter Info and the note beside the list must know its whole height.
        if let Some(doc) = &lsp.completion_doc
            && completion.shown.get(completion.selected) == Some(&doc.entry)
        {
            height += crate::semantic::completion_doc_line_count(doc) as f32 * row + 12.;
        }
        Some(height)
    }

    /// The x of a popup at `offset` and the top of its line, as the last frame put
    /// them.
    fn popup_anchor(&self, offset: usize) -> Option<(Pixels, Pixels)> {
        let (layout, state) = (self.layout.as_ref()?, self.state.as_ref()?);
        let at = state.display().to_display(offset);
        let shown = layout.rows.iter().find(|shown| shown.row == at.row)?;
        let x = layout.bounds.left() + px(self.gutter_room() + TEXT_INSET + self.center_inset)
            - px(self.scroll_left)
            + shown.text.x_for_index(at.column);
        Some((x, shown.y))
    }

    /// The place the completion list takes, when it is open.
    fn completion_bounds(&self) -> Option<Bounds<Pixels>> {
        let completion = self.lsp.as_ref()?.completion.as_ref()?;
        let height = self.completion_height()?;
        let (position, anchor) = self.popup_position(completion.start, height)?;
        let top = match anchor {
            Anchor::BottomLeft => position.y - px(height),
            _ => position.y,
        };
        Some(Bounds::new(
            point(position.x, top),
            size(px(1.), px(height)),
        ))
    }

    /// The place of Parameter Info. It stands over the line of the call, at its opening
    /// bracket, and keeps that place while the completion list is open: the list opens
    /// under the line, so the two do not meet. A line with no room above it puts the
    /// window under the line, clear of the list.
    pub(crate) fn parameter_info_position(
        &self,
        at: usize,
        height: f32,
    ) -> Option<(Point<Pixels>, Anchor)> {
        let (layout, state) = (self.layout.as_ref()?, self.state.as_ref()?);
        let anchor = call_open_paren(&state.store(), at).unwrap_or(at);
        let (x, line_top) = self.popup_anchor(anchor)?;
        let editor = layout.bounds;
        let gap = px(2.);
        let limit = px(4.);
        let line_bottom = line_top + px(self.style.line_height);
        let list = self.completion_bounds();
        let clear_of_list =
            |bounds: Bounds<Pixels>| list.is_none_or(|list| !boxes_overlap(bounds, list));
        let above = line_top - gap;
        let above_bounds = Bounds::new(point(x, above - px(height)), size(px(1.), px(height)));
        if above - px(height) >= editor.top() + limit && clear_of_list(above_bounds) {
            return Some((point(x, above), Anchor::BottomLeft));
        }
        let below = line_bottom + gap;
        let below_bounds = Bounds::new(point(x, below), size(px(1.), px(height)));
        if below + px(height) <= editor.bottom() - limit && clear_of_list(below_bounds) {
            return Some((point(x, below), Anchor::TopLeft));
        }
        // Neither side of the line is clear: the list holds the room under it, so the
        // window stands under or over the list rather than covering it.
        if let Some(list) = list {
            let under_list = list.bottom() + gap;
            if under_list + px(height) <= editor.bottom() - limit {
                return Some((point(x, under_list), Anchor::TopLeft));
            }
            let over_list = list.top() - gap;
            if over_list - px(height) >= editor.top() + limit {
                return Some((point(x, over_list), Anchor::BottomLeft));
            }
        }
        let top = below.min((editor.bottom() - limit - px(height)).max(editor.top() + limit));
        Some((point(x, top), Anchor::TopLeft))
    }
}
