//! The line the user types into a console, and the lines sent there before. The Run and
//! Debug consoles share it.

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

/// What a key of the input line asks the console to do.
pub(crate) enum ConsoleKey {
    /// The key is not a key of the console input.
    Ignored,
    /// The line changed; the console only repaints.
    Edited,
    /// Complete lines for the program, in order. The text after the last line break stays
    /// in the line.
    Lines(Vec<String>),
    /// Send EOF.
    Eof,
}

/// The line the user types into a console, with the history of the lines sent to it.
pub(crate) struct ConsoleInput {
    field: TextField,
    history: Vec<String>,
    /// Where the walk through the history stands; `None` while the line is the live one.
    history_at: Option<usize>,
    /// The program was told that its input ended.
    closed: bool,
}

impl ConsoleInput {
    pub fn new() -> Self {
        Self {
            field: TextField::default(),
            history: Vec::new(),
            history_at: None,
            closed: false,
        }
    }

    /// A new program of the console: an empty line and no history.
    pub fn restart(&mut self) {
        self.field = TextField::default();
        self.history.clear();
        self.history_at = None;
        self.closed = false;
    }

    /// Whether more can be typed: the input of the program did not end.
    pub fn is_open(&self) -> bool {
        !self.closed
    }

    /// The text of the line, for tests.
    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.field.text
    }

    /// Enter: the line goes to the program and the line is empty for the next one.
    pub fn take_line(&mut self) -> String {
        let line = self.field.text.clone();
        self.field = TextField::default();
        self.history_at = None;
        line
    }

    /// Keeps a line for Up and Down, unless it repeats the last one.
    pub fn remember(&mut self, line: &str) {
        if !line.is_empty() && self.history.last().is_none_or(|last| last != line) {
            self.history.push(line.to_owned());
        }
    }

    /// Up and Down: the lines sent before, the newest first. True when the line changed.
    pub fn browse_history(&mut self, older: bool) -> bool {
        let len = self.history.len();
        if len == 0 {
            return false;
        }
        let next = match self.history_at {
            None if !older => return false,
            None => Some(len - 1),
            Some(index) if older => index.checked_sub(1),
            Some(index) if index + 1 < len => Some(index + 1),
            Some(_) => None,
        };
        self.history_at = next;
        let text = next.map_or("", |index| self.history[index].as_str());
        self.field = TextField::from_text(text);
        true
    }

    /// Send EOF: nothing more is typed into this console.
    pub fn close(&mut self) {
        self.closed = true;
        self.field = TextField::default();
        self.history_at = None;
    }

    /// A key of the line, as the Run console takes it: Enter sends the line, Up and Down
    /// walk the history, Ctrl+V sends the complete lines of the clipboard one after
    /// another, and Ctrl+D is Send EOF.
    pub fn key(&mut self, event: &KeyDownEvent, cx: &mut App) -> ConsoleKey {
        if self.closed {
            return ConsoleKey::Ignored;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let primary = modifiers.control || modifiers.platform;
        if primary && !modifiers.alt && !modifiers.shift {
            match key {
                "d" => return ConsoleKey::Eof,
                "v" => {
                    let lines = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .map(|text| self.paste(text))
                        .unwrap_or_default();
                    return if lines.is_empty() {
                        ConsoleKey::Edited
                    } else {
                        ConsoleKey::Lines(lines)
                    };
                }
                _ => {}
            }
        }
        if !modifiers.alt && !modifiers.shift && matches!(key, "up" | "down") {
            self.browse_history(key == "up");
            return ConsoleKey::Edited;
        }
        if !modifiers.alt && !modifiers.shift && key == "enter" {
            return ConsoleKey::Lines(vec![self.take_line()]);
        }
        if self.field.key_with_clipboard(event, false, cx) {
            if key == "backspace" || key == "delete" || event.keystroke.key_char.is_some() {
                self.history_at = None;
            }
            return ConsoleKey::Edited;
        }
        ConsoleKey::Ignored
    }

    /// The line with the caret after it, at the end of the console output.
    pub fn element(&self, selector: &'static str, theme: &Theme) -> AnyElement {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .h(px(CODE_LINE_HEIGHT))
            .px(px(SPACE_L))
            .whitespace_nowrap()
            .text_color(theme.console_input)
            .children(text_field_children_named(
                "console-input-field",
                &self.field,
                theme.console_input,
                theme.console_input,
                theme.selection,
                theme.console_input,
                CODE_SIZE,
                true,
            ))
            .into_any_element()
    }

    /// Ctrl+V: the complete lines go to the program one after another; what follows the
    /// last line break stays in the line.
    fn paste(&mut self, text: String) -> Vec<String> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let Some(at) = text.rfind('\n') else {
            self.field.insert_text(&text);
            self.history_at = None;
            return Vec::new();
        };
        let lines = text[..at].split('\n').map(str::to_owned).collect();
        // The complete lines replace the line, as sending them one after another does.
        self.field = TextField::default();
        let rest = &text[at + 1..];
        if !rest.is_empty() {
            self.field.insert_text(rest);
        }
        self.history_at = None;
        lines
    }
}
