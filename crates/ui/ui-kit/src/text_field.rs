//! A one-line edit: text, a caret and a selection. Tool windows and the find bar use it,
//! so Ctrl+A, the shift keys, Backspace and the clipboard behave the same everywhere.

use std::ops::Range;

use gpui_kit::prelude::*;
use gpui_kit::*;

/// Typed text with a caret and an anchor. Offsets are bytes on character boundaries.
#[derive(Clone, Debug, Default)]
pub struct TextField {
    pub text: String,
    caret: usize,
    /// The other end of the selection; equal to `caret` when nothing is selected.
    anchor: usize,
}

impl TextField {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let caret = text.len();
        Self {
            text,
            caret,
            anchor: caret,
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
                        if !multiline {
                            let line = text.lines().next().unwrap_or("");
                            self.insert_text(line);
                        } else {
                            let text = text.replace("\r\n", "\n").replace('\r', "\n");
                            self.insert_text(&text);
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
        self.key(event, multiline)
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

/// The parts of a field for drawing: its text split at the selection, with the caret bar
/// between them. `text_color` is the colour of the unselected text.
pub fn text_field_children(
    field: &TextField,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
    caret_color: Hsla,
    caret_height: f32,
    show_caret: bool,
) -> Vec<AnyElement> {
    let (start, end) = {
        let selection = field.selection();
        (selection.start, selection.end)
    };
    let mut parts: Vec<(Range<usize>, bool)> = Vec::new();
    if start > 0 {
        parts.push((0..start, false));
    }
    if start < end {
        parts.push((start..end, true));
    }
    if end < field.text.len() {
        parts.push((end..field.text.len(), false));
    }
    let mut children = Vec::new();
    let mut caret_placed = false;
    for (range, selected) in parts {
        let caret_here =
            show_caret && !caret_placed && field.caret >= range.start && field.caret <= range.end;
        let split = if caret_here { field.caret } else { range.end };
        if split > range.start {
            children.push(segment(
                &field.text[range.start..split],
                selected,
                text_color,
                selected_color,
                selection_color,
            ));
        }
        if caret_here {
            children.push(caret(caret_color, caret_height));
            caret_placed = true;
            if range.end > split {
                children.push(segment(
                    &field.text[split..range.end],
                    selected,
                    text_color,
                    selected_color,
                    selection_color,
                ));
            }
        }
    }
    if show_caret && !caret_placed {
        children.push(caret(caret_color, caret_height));
    }
    children
}

fn segment(
    text: &str,
    selected: bool,
    text_color: Hsla,
    selected_color: Hsla,
    selection_color: Hsla,
) -> AnyElement {
    div()
        .whitespace_nowrap()
        .text_color(if selected { selected_color } else { text_color })
        .when(selected, |this| this.bg(selection_color))
        .child(text.to_owned())
        .into_any_element()
}

fn caret(color: Hsla, height: f32) -> AnyElement {
    div()
        .flex_none()
        .w(px(1.5))
        .h(px(height))
        .bg(color)
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
}
