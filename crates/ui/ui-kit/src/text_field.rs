//! A one-line edit: text, a caret and a selection. Tool windows and the find bar use it,
//! so Ctrl+A, the shift keys, Backspace, the clipboard and the mouse behave the same
//! everywhere. The mouse and the menu are in `text_field_mouse`.

use std::ops::Range;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::{CONTROL_HEIGHT, CONTROL_RADIUS, SPACE_M};
use crate::text_field_mouse::{self, FieldMouse};

/// The menu that a right click opened on a field: where it stands, and whether Paste
/// applies to the clipboard as it was at the click.
#[derive(Clone, Debug)]
pub struct FieldMenu {
    pub at: Point<Pixels>,
    pub can_paste: bool,
}

/// Typed text with a caret and an anchor. Offsets are bytes on character boundaries.
#[derive(Clone, Debug, Default)]
pub struct TextField {
    pub text: String,
    caret: usize,
    /// The other end of the selection; equal to `caret` when nothing is selected.
    anchor: usize,
    /// Whether a press of the left button started a selection that a drag still extends.
    dragging: bool,
    /// The menu of a right click, while it is open.
    pub menu: Option<FieldMenu>,
}

impl TextField {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let caret = text.len();
        Self {
            text,
            caret,
            anchor: caret,
            dragging: false,
            menu: None,
        }
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn set_caret(&mut self, caret: usize) {
        let caret = self.boundary(caret);
        self.caret = caret;
        self.anchor = caret;
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Replaces the selection, or puts `text` at the caret, and leaves the caret after it.
    pub fn insert_text(&mut self, text: &str) {
        let range = self.selection();
        self.text.replace_range(range.clone(), text);
        self.caret = range.start + text.len();
        self.anchor = self.caret;
    }

    /// Inserts pasted `text`. A multiline field takes every line, a single-line one the
    /// first line only.
    pub fn paste(&mut self, text: &str, multiline: bool) {
        if multiline {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.insert_text(&text);
        } else {
            self.insert_text(text.lines().next().unwrap_or(""));
        }
    }

    /// Removes the selection, or one character before the caret.
    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let start = if word {
            self.previous_word(self.caret)
        } else {
            self.previous_boundary(self.caret)
        };
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.anchor = start;
    }

    /// Removes the selection, or one character after the caret.
    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        let end = if word {
            self.next_word(self.caret)
        } else {
            self.next_boundary(self.caret)
        };
        self.text.replace_range(self.caret..end, "");
        self.anchor = self.caret;
    }

    /// A press of the left button at byte `at`. One click puts the caret there, and Shift
    /// keeps the other end of the selection, so the selection grows from it. Two clicks
    /// select the word under `at`, and three select the whole text. A press with one click
    /// starts a drag, which [`TextField::drag_to`] extends until [`TextField::release`].
    pub fn press(&mut self, at: usize, shift: bool, clicks: usize) {
        let at = self.boundary(at);
        self.dragging = false;
        match clicks {
            0 | 1 => {
                if !shift {
                    self.anchor = at;
                }
                self.caret = at;
                self.dragging = true;
            }
            2 => {
                let word = self.word_at(at);
                self.anchor = word.start;
                self.caret = word.end;
            }
            _ => self.select_all(),
        }
    }

    /// Moves the caret of a drag to byte `at`. Returns whether the caret moved; it does
    /// nothing when no drag is going on.
    pub fn drag_to(&mut self, at: usize) -> bool {
        if !self.dragging {
            return false;
        }
        let at = self.boundary(at);
        let moved = at != self.caret;
        self.caret = at;
        moved
    }

    /// Ends a drag. Returns whether one was going on.
    pub fn release(&mut self) -> bool {
        std::mem::take(&mut self.dragging)
    }

    /// The run of word characters, or the run of other characters, that holds byte `at`.
    /// At the end of the text the run before the end is taken.
    pub fn word_at(&self, at: usize) -> Range<usize> {
        let at = self.boundary(at);
        let class = if let Some(next) = self.text[at..].chars().next() {
            Self::word_class(next)
        } else if let Some(last) = self.text[..at].chars().next_back() {
            Self::word_class(last)
        } else {
            return at..at;
        };
        let mut start = at;
        while start > 0 {
            let previous = self.previous_boundary(start);
            let character = self.text[previous..start].chars().next().unwrap_or(' ');
            if Self::word_class(character) != class {
                break;
            }
            start = previous;
        }
        let mut end = at;
        while end < self.text.len() {
            let character = self.text[end..].chars().next().unwrap_or(' ');
            if Self::word_class(character) != class {
                break;
            }
            end = self.next_boundary(end);
        }
        start..end
    }

    fn word_class(character: char) -> u8 {
        if Self::word_char(character) {
            0
        } else if character.is_whitespace() {
            1
        } else {
            2
        }
    }

    /// Removes the selection, if there is one. Returns whether it did.
    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let range = self.selection();
        self.text.replace_range(range.clone(), "");
        self.caret = range.start;
        self.anchor = range.start;
        true
    }

    fn boundary(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    fn previous_boundary(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self, at: usize) -> usize {
        at + self.text[at..].chars().next().map_or(0, char::len_utf8)
    }

    fn word_char(character: char) -> bool {
        character.is_alphanumeric() || character == '_'
    }

    /// The start of the word before `at`, skipping the spaces between words.
    fn previous_word(&self, at: usize) -> usize {
        let mut at = at;
        while at > 0 {
            let previous = self.previous_boundary(at);
            let character = self.text[previous..at].chars().next().unwrap_or(' ');
            if Self::word_char(character) {
                break;
            }
            at = previous;
        }
        while at > 0 {
            let previous = self.previous_boundary(at);
            let character = self.text[previous..at].chars().next().unwrap_or(' ');
            if !Self::word_char(character) {
                break;
            }
            at = previous;
        }
        at
    }

    /// The end of the word after `at`, skipping the characters between words.
    fn next_word(&self, at: usize) -> usize {
        let mut at = at;
        while at < self.text.len() {
            let character = self.text[at..].chars().next().unwrap_or(' ');
            if Self::word_char(character) {
                break;
            }
            at = self.next_boundary(at);
        }
        while at < self.text.len() {
            let character = self.text[at..].chars().next().unwrap_or(' ');
            if !Self::word_char(character) {
                break;
            }
            at = self.next_boundary(at);
        }
        at
    }

    fn move_to(&mut self, to: usize, shift: bool) {
        if !shift {
            self.anchor = to;
        }
        self.caret = to;
    }

    fn move_left(&mut self, word: bool, shift: bool) {
        if !shift && self.has_selection() {
            let start = self.selection().start;
            self.move_to(start, false);
            return;
        }
        let to = if word {
            self.previous_word(self.caret)
        } else {
            self.previous_boundary(self.caret)
        };
        self.move_to(to, shift);
    }

    fn move_right(&mut self, word: bool, shift: bool) {
        if !shift && self.has_selection() {
            let end = self.selection().end;
            self.move_to(end, false);
            return;
        }
        let to = if word {
            self.next_word(self.caret)
        } else {
            self.next_boundary(self.caret)
        };
        self.move_to(to, shift);
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |index| index + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |index| at + index)
    }

    /// Editing keys. `multiline` lets Enter add a line and Up/Down move between lines.
    /// Clipboard keys are in [`TextField::key_with_clipboard`]. Returns whether the key
    /// was taken.
    pub fn key(&mut self, event: &KeyDownEvent, multiline: bool) -> bool {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        let primary = modifiers.control || modifiers.platform;
        let shift = modifiers.shift;
        match stroke.key.as_str() {
            "backspace" if !modifiers.alt => self.backspace(primary),
            "delete" if !modifiers.alt => self.delete(primary),
            "left" if !modifiers.alt => self.move_left(primary, shift),
            "right" if !modifiers.alt => self.move_right(primary, shift),
            "home" if !modifiers.alt => {
                let to = self.line_start(self.caret);
                self.move_to(to, shift);
            }
            "end" if !modifiers.alt => {
                let to = self.line_end(self.caret);
                self.move_to(to, shift);
            }
            "up" if multiline && !primary && !modifiers.alt => {
                self.vertical(-1, shift);
            }
            "down" if multiline && !primary && !modifiers.alt => {
                self.vertical(1, shift);
            }
            "a" if primary && !modifiers.alt => self.select_all(),
            "enter" if multiline && !primary && !modifiers.alt => {
                self.insert_text("\n");
            }
            _ => {
                if primary || modifiers.alt {
                    return false;
                }
                let Some(text) = &stroke.key_char else {
                    return false;
                };
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if text.is_empty() || text.chars().any(char::is_control) && !multiline {
                    return false;
                }
                self.insert_text(&text);
            }
        }
        true
    }

    /// [`TextField::key`] plus Ctrl+C, Ctrl+X and Ctrl+V through the clipboard.
    pub fn key_with_clipboard(
        &mut self,
        event: &KeyDownEvent,
        multiline: bool,
        cx: &mut App,
    ) -> bool {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        if (modifiers.control || modifiers.platform) && !modifiers.alt {
            match stroke.key.as_str() {
                "c" => {
                    if self.has_selection() {
                        let text = self.selected_text().to_owned();
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return true;
                }
                "x" => {
                    if self.has_selection() {
                        let text = self.selected_text().to_owned();
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        self.backspace(false);
                    }
                    return true;
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.paste(&text, multiline);
                    }
                    return true;
                }
                _ => {}
            }
        }
        self.key(event, multiline)
    }

    /// [`key_with_clipboard`](Self::key_with_clipboard) for a password: Copy and Cut do
    /// nothing, so the secret never reaches the clipboard; Paste and the other keys work.
    pub fn key_secret(&mut self, event: &KeyDownEvent, cx: &mut App) -> bool {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        if (modifiers.control || modifiers.platform)
            && !modifiers.alt
            && matches!(stroke.key.as_str(), "c" | "x")
        {
            return true;
        }
        self.key_with_clipboard(event, false, cx)
    }

    /// The keys of a field that cannot be changed. The caret moves and the selection grows as in
    /// [`TextField::key`], Ctrl+A selects all and Ctrl+C copies the selection. A key that would
    /// edit the text is taken and changes nothing. Enter, Escape and Tab are left to the owner.
    /// Returns whether the key was taken.
    pub fn key_read_only(&mut self, event: &KeyDownEvent, cx: &mut App) -> bool {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        let primary = modifiers.control || modifiers.platform;
        if modifiers.alt {
            return false;
        }
        match stroke.key.as_str() {
            "c" if primary => {
                if self.has_selection() {
                    let text = self.selected_text().to_owned();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                true
            }
            "a" if primary => {
                self.select_all();
                true
            }
            "x" | "v" if primary => true,
            "enter" | "escape" | "tab" => false,
            "left" | "right" | "home" | "end" => {
                self.key(event, false);
                true
            }
            "backspace" | "delete" => true,
            _ if primary => false,
            _ => stroke.key_char.is_some(),
        }
    }

    fn vertical(&mut self, step: isize, shift: bool) {
        let line = self.text[..self.caret]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let column = self.caret - self.line_start(self.caret);
        let target_line = line as isize + step;
        if target_line < 0 {
            return;
        }
        let mut start = 0;
        for _ in 0..target_line {
            match self.text[start..].find('\n') {
                Some(index) => start += index + 1,
                None => return,
            }
        }
        let end = self.line_end(start);
        let mut to = start + column.min(end - start);
        while to > start && !self.text.is_char_boundary(to) {
            to -= 1;
        }
        self.move_to(to, shift);
    }
}

/// The colours and the size of the parts of a field.
#[derive(Clone, Copy)]
struct Look {
    text: Hsla,
    selected: Hsla,
    selection: Hsla,
    caret: Hsla,
    caret_height: f32,
    show_caret: bool,
}

/// The grey text of an empty field, such as `Commit Message`. It stands after the caret of the
/// first line, so the caret and the hint share the place where the text will start, and it goes
/// as soon as the field holds a character. A field with a name gets the selector
/// `{name}-placeholder` on the hint.
#[derive(Clone, Copy, Debug)]
pub struct FieldHint {
    pub text: &'static str,
    pub color: Hsla,
}

/// The parts of a field for drawing: its text split at the selection, with the caret bar
/// between them. `text_color` is the colour of the unselected text. `mouse` connects the
/// field to the pointer and to its menu; a field whose owner is not connected passes
/// [`FieldMouse::off`].
#[allow(clippy::too_many_arguments)]
pub fn text_field_children(
    mouse: FieldMouse,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
) -> Vec<AnyElement> {
    field_children(
        mouse,
        None,
        field,
        false,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
    )
}

/// [`text_field_children`] with the caret and the selected text named in the render tree
/// as `<name>-caret` and `<name>-selection`, so a test of a window sees the caret of the
/// field it focused.
#[allow(clippy::too_many_arguments)]
pub fn text_field_children_named(
    mouse: FieldMouse,
    name: &'static str,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
) -> Vec<AnyElement> {
    field_children(
        mouse,
        Some(name),
        field,
        false,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
    )
}

/// The lines of a multiline field: one row per line, with the caret on its own line. A
/// line is `caret_height` tall, so a caret and the text next to it share the row.
#[allow(clippy::too_many_arguments)]
pub fn text_field_lines(
    mouse: FieldMouse,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
) -> Vec<AnyElement> {
    field_children(
        mouse,
        None,
        field,
        true,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
    )
}

/// [`text_field_lines`] with the named parts of [`text_field_children_named`].
#[allow(clippy::too_many_arguments)]
pub fn text_field_lines_named(
    mouse: FieldMouse,
    name: &'static str,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
) -> Vec<AnyElement> {
    field_children(
        mouse,
        Some(name),
        field,
        true,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
    )
}

/// [`text_field_children_named`] with a hint that shows while the field is empty.
#[allow(clippy::too_many_arguments)]
pub fn text_field_children_hinted(
    mouse: FieldMouse,
    name: &'static str,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
    hint: FieldHint,
) -> Vec<AnyElement> {
    field_children_hinted(
        mouse,
        Some(name),
        field,
        false,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
        Some(hint),
    )
}

/// [`text_field_lines_named`] with a hint that shows while the field is empty. The hint stands
/// after the caret of the first line.
#[allow(clippy::too_many_arguments)]
pub fn text_field_lines_hinted(
    mouse: FieldMouse,
    name: &'static str,
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
    hint: FieldHint,
) -> Vec<AnyElement> {
    field_children_hinted(
        mouse,
        Some(name),
        field,
        true,
        Look {
            text: text_color,
            selected: selected_color,
            selection: selection_color,
            caret: caret_color,
            caret_height,
            show_caret,
        },
        Some(hint),
    )
}

/// A field whose text can be selected and copied but not changed. It is drawn as a field, with
/// a frame and a background a little dimmer than the window, and it shows no caret. `mouse`
/// should be [`FieldMouse::read_only`]; the keys of its owner go to [`TextField::key_read_only`].
pub fn read_only_field(
    id: impl Into<ElementId>,
    selector: String,
    name: &'static str,
    mouse: FieldMouse,
    field: &TextField,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .min_w(px(0.))
        .flex_1()
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .bg(theme.main_bg)
        .overflow_hidden()
        .cursor_text()
        .text_color(theme.text)
        .children(text_field_children_named(
            mouse,
            name,
            field,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            16.,
            false,
        ))
}

/// The rows of a field, one per line, then the element that covers the field for the
/// pointer, and the menu when a right click opened one. A row of a multiline field is
/// `caret_height` tall. Each row also measures its text when the field has a mouse.
fn field_children(
    mouse: FieldMouse,
    name: Option<&'static str>,
    field: &TextField,
    multiline: bool,
    look: Look,
) -> Vec<AnyElement> {
    field_children_hinted(mouse, name, field, multiline, look, None)
}

/// [`field_children`] with the hint of an empty field, which stands after the caret of the first
/// line.
fn field_children_hinted(
    mouse: FieldMouse,
    name: Option<&'static str>,
    field: &TextField,
    multiline: bool,
    look: Look,
    hint: Option<FieldHint>,
) -> Vec<AnyElement> {
    let ranges = line_ranges(field, multiline);
    let lines = mouse.is_on().then(|| text_field_mouse::lines(ranges.len()));
    let mut children = Vec::new();
    for (index, range) in ranges.iter().enumerate() {
        let mut row = div().flex().items_center().flex_none().whitespace_nowrap();
        if multiline {
            row = row.h(px(look.caret_height));
        }
        row = row.children(range_children(name, field, range.clone(), look));
        if let Some(hint) = hint.filter(|_| index == 0 && field.text.is_empty()) {
            row = row.child(hint_text(name, hint));
        }
        if let Some(lines) = &lines {
            let text = &field.text[range.clone()];
            row = row.child(text_field_mouse::measure_line(
                lines,
                index,
                range.clone(),
                text,
            ));
        }
        children.push(row.into_any_element());
    }
    if let Some(lines) = lines {
        children.push(text_field_mouse::hit_area(&mouse, lines));
    }
    children.extend(text_field_mouse::menu(&mouse, field, multiline));
    children
}

/// The byte ranges of the lines of a field: one range for a single-line field, and one per
/// line for a multiline one.
fn line_ranges(field: &TextField, multiline: bool) -> Vec<Range<usize>> {
    if !multiline {
        return std::iter::once(0..field.text.len()).collect();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    loop {
        let end = field.text[start..]
            .find('\n')
            .map_or(field.text.len(), |index| start + index);
        ranges.push(start..end);
        if end == field.text.len() {
            return ranges;
        }
        start = end + 1;
    }
}

/// The parts of the field that lie on one line. The caret is placed only when it belongs
/// to this line, and the fallback at the end keeps it on an empty line.
fn range_children(
    name: Option<&'static str>,
    field: &TextField,
    line: Range<usize>,
    look: Look,
) -> Vec<AnyElement> {
    let selection = field.selection();
    let selected_start = selection.start.clamp(line.start, line.end);
    let selected_end = selection.end.clamp(line.start, line.end);
    let mut parts: Vec<(Range<usize>, bool)> = Vec::new();
    if selected_start > line.start {
        parts.push((line.start..selected_start, false));
    }
    if selected_start < selected_end {
        parts.push((selected_start..selected_end, true));
    }
    if selected_end < line.end {
        parts.push((selected_end..line.end, false));
    }
    let caret_in_line = look.show_caret && field.caret >= line.start && field.caret <= line.end;
    let mut children = Vec::new();
    let mut caret_placed = false;
    for (range, selected) in parts {
        let caret_here = caret_in_line
            && !caret_placed
            && field.caret >= range.start
            && field.caret <= range.end;
        let split = if caret_here { field.caret } else { range.end };
        if split > range.start {
            children.push(segment(
                name,
                &field.text[range.start..split],
                selected,
                look,
            ));
        }
        if caret_here {
            children.push(caret(look.caret, look.caret_height, name));
            caret_placed = true;
            if range.end > split {
                children.push(segment(name, &field.text[split..range.end], selected, look));
            }
        }
    }
    if caret_in_line && !caret_placed {
        children.push(caret(look.caret, look.caret_height, name));
    }
    children
}

fn segment(name: Option<&'static str>, text: &str, selected: bool, look: Look) -> AnyElement {
    div()
        .whitespace_nowrap()
        .text_color(if selected { look.selected } else { look.text })
        .when(selected, |this| this.bg(look.selection))
        .when_some(name.filter(|_| selected), |this, name| {
            this.debug_selector(move || format!("{name}-selection"))
        })
        .child(text.to_owned())
        .into_any_element()
}

/// The text of a hint. A named field marks it with `{name}-placeholder`, and the tests of the
/// window read its label from the frame.
fn hint_text(name: Option<&'static str>, hint: FieldHint) -> AnyElement {
    let text = div()
        .flex_none()
        .whitespace_nowrap()
        .text_color(hint.color)
        .when_some(name, |this, name| {
            this.debug_selector(move || format!("{name}-placeholder"))
        })
        .child(hint.text);
    #[cfg(any(test, feature = "test-support"))]
    let text = match name {
        Some(name) => text.child(crate::bounds_probe(
            format!("{name}-placeholder"),
            Some(hint.text.into()),
        )),
        None => text,
    };
    text.into_any_element()
}

fn caret(color: Hsla, height: f32, name: Option<&'static str>) -> AnyElement {
    div()
        .flex_none()
        .w(px(1.5))
        .h(px(height))
        .bg(color)
        .when_some(name, |this, name| {
            this.debug_selector(move || format!("{name}-caret"))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui_kit::{KeyDownEvent, Keystroke, Modifiers};

    use super::TextField;

    fn press(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.into(),
                key_char: key_char.map(Into::into),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn control() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::default()
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn control_shift() -> Modifiers {
        Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn typing_goes_in_at_the_caret_and_backspace_takes_it_back() {
        let mut field = TextField::default();
        assert!(field.key(&press("a", Some("a"), Modifiers::default()), false));
        assert!(field.key(&press("b", Some("b"), Modifiers::default()), false));
        assert!(field.key(&press("left", None, Modifiers::default()), false));
        assert!(field.key(&press("x", Some("x"), Modifiers::default()), false));
        assert_eq!(field.text, "axb");
        assert!(field.key(&press("backspace", None, Modifiers::default()), false));
        assert_eq!(field.text, "ab");
        assert_eq!(field.caret(), 1);
    }

    #[test]
    fn ctrl_a_selects_all_and_typing_replaces_it() {
        let mut field = TextField::from_text("alpha");
        assert!(field.key(&press("a", Some("a"), control()), false));
        assert_eq!(field.selected_text(), "alpha");
        assert!(field.key(&press("b", Some("b"), Modifiers::default()), false));
        assert_eq!(field.text, "b");
        assert_eq!(field.caret(), 1);
    }

    #[test]
    fn shift_and_ctrl_shift_move_the_end_of_the_selection() {
        let mut field = TextField::from_text("one two");
        assert!(field.key(&press("home", None, Modifiers::default()), false));
        assert!(field.key(&press("right", None, shift()), false));
        assert_eq!(field.selected_text(), "o");
        assert!(field.key(&press("right", None, control_shift()), false));
        assert_eq!(field.selected_text(), "one");
        assert!(field.key(&press("end", None, shift()), false));
        assert_eq!(field.selected_text(), "one two");
    }

    #[test]
    fn shift_and_delete_replace_the_selection() {
        let mut field = TextField::from_text("abcd");
        assert!(field.key(&press("left", None, shift()), false));
        assert!(field.key(&press("backspace", None, Modifiers::default()), false));
        assert_eq!(field.text, "abc");
        assert!(field.key(&press("left", None, shift()), false));
        assert!(field.key(&press("delete", None, Modifiers::default()), false));
        assert_eq!(field.text, "ab");
    }

    #[test]
    fn a_press_places_the_caret_and_a_drag_selects_up_to_the_pointer() {
        let mut field = TextField::from_text("one two");
        field.press(4, false, 1);
        assert_eq!(field.caret(), 4);
        assert!(!field.has_selection());
        assert!(field.drag_to(7));
        assert_eq!(field.selected_text(), "two");
        assert!(field.release());
        assert!(!field.drag_to(0), "a drag ends at the release");
        assert_eq!(field.selected_text(), "two");
    }

    #[test]
    fn shift_click_grows_the_selection_from_the_caret() {
        let mut field = TextField::from_text("one two");
        field.press(0, false, 1);
        field.release();
        field.press(3, true, 1);
        field.release();
        assert_eq!(field.selected_text(), "one");
        field.press(7, true, 1);
        field.release();
        assert_eq!(
            field.selected_text(),
            "one two",
            "the start of the selection stays"
        );
    }

    #[test]
    fn two_clicks_select_a_word_and_three_select_all() {
        let mut field = TextField::from_text("one two");
        field.press(5, false, 2);
        assert_eq!(field.selected_text(), "two");
        assert!(!field.release(), "a double click starts no drag");
        field.press(1, false, 3);
        assert_eq!(field.selected_text(), "one two");
    }

    #[test]
    fn a_word_is_a_run_of_word_characters_or_of_other_characters() {
        let field = TextField::from_text("one  two.");
        assert_eq!(field.word_at(1), 0..3);
        assert_eq!(field.word_at(3), 3..5, "the spaces");
        assert_eq!(field.word_at(5), 5..8, "two");
        assert_eq!(field.word_at(8), 8..9, "the full stop");
        assert_eq!(field.word_at(9), 8..9, "the end takes the run before it");
    }

    #[test]
    fn paste_takes_the_first_line_of_a_single_line_field() {
        let mut field = TextField::from_text("ab");
        field.paste("x\ny", false);
        assert_eq!(field.text, "abx");
        let mut lines = TextField::default();
        lines.paste("p\r\nq", true);
        assert_eq!(lines.text, "p\nq");
    }
}
