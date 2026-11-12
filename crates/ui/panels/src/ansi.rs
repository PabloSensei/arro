//! Console output into lines of styled text. Knows SGR colors and bold, drops every other
//! escape sequence, and a lone `\r` starts the line over, as a progress bar expects.

use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// One of the 16 terminal colors.
    pub color: Option<u8>,
    pub bold: bool,
    /// Came from stderr.
    pub error: bool,
    /// The user typed it into the console, not the program.
    pub input: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

impl Line {
    fn push(&mut self, ch: char, style: Style) {
        let start = self.text.len();
        self.text.push(ch);
        let end = self.text.len();
        match self.spans.last_mut() {
            Some((range, last)) if *last == style && range.end == start => range.end = end,
            _ => self.spans.push((start..end, style)),
        }
    }

    pub fn plain(text: impl Into<String>, style: Style) -> Self {
        let text = text.into();
        let spans = vec![(0..text.len(), style)];
        Self { text, spans }
    }
}

#[derive(Default)]
enum Escape {
    #[default]
    None,
    /// Saw ESC.
    Start,
    /// Inside `ESC [`, collecting parameters.
    Csi(String),
}

#[derive(Default)]
pub struct AnsiParser {
    style: Style,
    current: Line,
    escape: Escape,
    carriage: bool,
}

impl AnsiParser {
    /// Feeds a piece of output. Lines it completes go to `done`.
    pub fn push(&mut self, text: &str, error: bool, done: &mut Vec<Line>) {
        for ch in text.chars() {
            match std::mem::take(&mut self.escape) {
                Escape::Start if ch == '[' => self.escape = Escape::Csi(String::new()),
                Escape::Start => {}
                Escape::Csi(mut params) => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        if ch == 'm' {
                            self.apply(&params);
                        }
                    } else {
                        params.push(ch);
                        self.escape = Escape::Csi(params);
                    }
                }
                Escape::None => self.char(ch, error, done),
            }
        }
    }

    /// The line that has not ended yet.
    pub fn current(&self) -> &Line {
        &self.current
    }

    /// The program is gone: what it left unfinished is a line too.
    pub fn finish(&mut self, done: &mut Vec<Line>) {
        if !self.current.text.is_empty() {
            done.push(std::mem::take(&mut self.current));
        }
        *self = Self::default();
    }

    fn char(&mut self, ch: char, error: bool, done: &mut Vec<Line>) {
        let carriage = std::mem::take(&mut self.carriage);
        match ch {
            '\u{1b}' => self.escape = Escape::Start,
            '\n' => done.push(std::mem::take(&mut self.current)),
            '\r' => self.carriage = true,
            '\t' => self.put(ch, error, carriage),
            ch if ch.is_control() => {}
            ch => self.put(ch, error, carriage),
        }
    }

    fn put(&mut self, ch: char, error: bool, carriage: bool) {
        if carriage {
            self.current = Line::default();
        }
        let style = Style {
            error,
            ..self.style
        };
        self.current.push(ch, style);
    }

    fn apply(&mut self, params: &str) {
        let mut codes = params
            .split(';')
            .map(|code| code.parse::<u16>().unwrap_or(0));
        while let Some(code) = codes.next() {
            match code {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                22 => self.style.bold = false,
                30..=37 => self.style.color = Some((code - 30) as u8),
                90..=97 => self.style.color = Some((code - 90 + 8) as u8),
                39 => self.style.color = None,
                38 | 48 => match codes.next() {
                    Some(5) => {
                        let index = codes.next();
                        if code == 38 {
                            self.style.color = index.filter(|index| *index < 16).map(|i| i as u8);
                        }
                    }
                    Some(2) => {
                        codes.nth(2);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AnsiParser, Line, Style};

    fn lines(text: &str) -> (Vec<Line>, Line) {
        let mut parser = AnsiParser::default();
        let mut done = Vec::new();
        parser.push(text, false, &mut done);
        (done, parser.current().clone())
    }

    #[test]
    fn plain_text_splits_into_lines() {
        let (done, current) = lines("one\ntwo\nthr");
        let texts: Vec<_> = done.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert_eq!(current.text, "thr");
    }

    #[test]
    fn colors_and_bold_become_spans() {
        let (done, _) = lines("a\u{1b}[1;31mbc\u{1b}[0md\n");
        let line = &done[0];
        assert_eq!(line.text, "abcd");
        let red = Style {
            color: Some(1),
            bold: true,
            error: false,
            input: false,
        };
        assert_eq!(
            line.spans,
            [
                (0..1, Style::default()),
                (1..3, red),
                (3..4, Style::default())
            ]
        );
    }

    #[test]
    fn bright_and_indexed_colors_are_known() {
        let (_, current) = lines("\u{1b}[92mx\u{1b}[38;5;4my\u{1b}[38;2;1;2;3mz");
        let colors: Vec<_> = current.spans.iter().map(|(_, style)| style.color).collect();
        // A true color is not one of the 16, so the last color stays.
        assert_eq!(current.text, "xyz");
        assert_eq!(colors, [Some(10), Some(4)]);
    }

    #[test]
    fn other_sequences_are_dropped() {
        let (_, current) = lines("\u{1b}[2Ka\u{1b}[?25lb\u{1b}7c");
        assert_eq!(current.text, "abc");
    }

    #[test]
    fn a_lone_carriage_return_starts_the_line_over() {
        let (done, current) = lines("10%\r20%\r\nnext\r");
        assert_eq!(done[0].text, "20%");
        assert_eq!(current.text, "next");
    }

    #[test]
    fn a_sequence_split_between_pieces_still_applies() {
        let mut parser = AnsiParser::default();
        let mut done = Vec::new();
        parser.push("\u{1b}[3", false, &mut done);
        parser.push("4mx", true, &mut done);
        let style = parser.current().spans[0].1;
        assert_eq!((style.color, style.error), (Some(4), true));
    }
}
